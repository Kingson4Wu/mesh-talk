# Persistent Message Log Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a node's conversation history survive a restart: back the node with the durable `PersistentEventLog`, add a local encrypted "sent log" sidecar (so the node can show plaintext of messages it sent, which it can't decrypt from the sealed event log), and add an on-demand `/history <peer> [n]` command.

**Architecture:** `Node.log` becomes `Mutex<PersistentEventLog>` (the same encrypted store the post office uses); a new `node::sentlog::SentLog` reuses the existing `storage::encryption` cipher to persist locally-sent plaintext; `Node::open` seeds the `emitted` set from the loaded log so restored history is not re-streamed; `Node::history` merges decrypted-received + sidecar-sent in time order; the binary opens both files under the keystore password and adds `/history`.

**Tech Stack:** Rust; the existing `eventlog::persist::PersistentEventLog`/`LogFile`, `eventlog::store::EventLog`, `storage::encryption` (AES-256-GCM, password KDF), `node` + `discovery`; `tokio`; `serde`/`bincode`. No new dependencies.

---

## Background the implementer needs

**This is the single plan of the "persistent message log" slice** (spec: `docs/superpowers/specs/2026-06-16-persistent-message-log-design.md`). Prior slices built the online DM path (`node::Node` with an in-memory `EventLog`) and offline post-office delivery. **Why a sidecar:** an outbound DM is sealed to the recipient (`dm::seal`), so the sender cannot decrypt its own sent messages back from the stored event — to show both sides in `/history`, the node keeps a local-only encrypted plaintext copy of what it sent.

**Exact verified APIs you build on:**
```rust
// eventlog::persist (durable event store; SAME methods as the in-memory EventLog + SyncStore)
crate::eventlog::persist::PersistentEventLog::open(path: &Path, password: &str) -> Result<PersistentEventLog, LogError>;
impl PersistentEventLog { pub fn append(&mut self, Event)->Result<AppendOutcome,LogError>; pub fn has(&self,&EventId)->bool;
    pub fn events(&self,&ConversationId)->Vec<&Event>; pub fn heads(..); pub fn version_vector(&self,&ConversationId)->HashMap<Author,u64>;
    pub fn prepare(&self,&ConversationId)->(Vec<EventId>,u64); } // + impl SyncStore
// storage::encryption (REUSE these for the sidecar — exactly as eventlog::persist does)
crate::storage::encryption::{decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE};
EncryptionKey::from_password(password: &str, salt: &[u8; SALT_SIZE]) -> Result<EncryptionKey, _>; // ? into LogError
encrypt_data(plaintext: &[u8], key: &EncryptionKey) -> Result<(Vec<u8>, [u8; NONCE_SIZE]), _>;     // ? into LogError
decrypt_data(ciphertext: &[u8], nonce: &[u8; NONCE_SIZE], key: &EncryptionKey) -> Result<Vec<u8>, _>;
generate_salt() -> [u8; SALT_SIZE];
// eventlog::event
pub struct Event { pub id: EventId, pub author: Author, pub kind: EventKind, pub wall_clock: u64, pub ciphertext: Vec<u8>, /* ... */ } // Clone
pub struct ConversationId([u8;32]); // Copy, Eq, Hash, Serialize, Deserialize
pub struct EventId([u8;32]);         // Copy, Eq, Hash, Ord, Serialize, Deserialize
crate::eventlog::LogError;           // CorruptFile(String), Serialization(String), From<io::Error>, Display + Error
// node internals (you are extending node.rs)
//   Node { identity, log: Mutex<EventLog>, roster, incoming, emitted: Mutex<HashSet<EventId>> }
//   fn emit_new_messages(&self, conv)  // private; PRIVATE items + fields are reachable from node.rs's own #[cfg(test)] mod tests
crate::node::conversation::{dm_conversation_id, build_dm_event, open_dm_event};
crate::identity::device::{DeviceIdentity, PublicIdentity}; // PublicIdentity: Clone, Eq
```

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare `cargo test`/`build`. Always:
```bash
cd src-tauri && nice -n 10 cargo test --lib node::sentlog -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::node eventlog::persist eventlog::store -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
The pre-commit hook runs the full health check — let it run. The Task-5 rig is `#[ignore]`d.

**Conventions:** hand-written errors (reuse `LogError`); `#[cfg(test)] mod tests`; rustfmt sorts `pub mod`/`pub use` lines; locks are `std::sync::Mutex` (snapshot under the guard, drop before any `.await`; std Mutex is NOT reentrant — never lock the same mutex twice on one thread).

---

### Task 1: `node::sentlog` — the encrypted sent-plaintext sidecar

**Files:**
- Create: `src-tauri/src/node/sentlog.rs`
- Modify: `src-tauri/src/node/mod.rs`

- [ ] **Step 1: Register the module in `node/mod.rs`**

Add `pub mod sentlog;`. rustfmt sorts modules; the correct alphabetical order is `conversation, node, postbox, sentlog, session, transport` (note: `sentlog` sorts before `session` because `n` < `s` at index 2). The module section becomes:
```rust
pub mod conversation;
pub mod node;
pub mod postbox;
pub mod sentlog;
pub mod session;
pub mod transport;

pub use node::{Node, NodeError, ReceivedDm};
pub use postbox::elected_post_office;
```
(Keep any existing module doc comment above these lines.)

- [ ] **Step 2: Create `src-tauri/src/node/sentlog.rs`**

```rust
//! A local-only, password-encrypted sidecar recording the plaintext of DMs this
//! node SENT. Outbound DMs are sealed to the recipient, so the sender cannot
//! decrypt its own messages back from the synced event log; this store keeps a
//! private plaintext copy so history can show both sides. It is never synced,
//! never served to peers, and holds no signatures. On-disk framing mirrors
//! [`crate::eventlog::persist`] (magic + salt header, then length-prefixed
//! AES-256-GCM records) but stores [`SentEntry`] records, not events.

use crate::eventlog::event::ConversationId;
use crate::eventlog::LogError;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTSENT";

/// One sent message's local plaintext record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentEntry {
    pub conversation: ConversationId,
    pub seq: u64,
    pub wall_clock: u64,
    pub plaintext: Vec<u8>,
}

/// An append-only, password-encrypted log of locally-sent message plaintext,
/// indexed in memory by conversation.
pub struct SentLog {
    file: File,
    key: EncryptionKey,
    by_conversation: HashMap<ConversationId, Vec<SentEntry>>,
}

impl SentLog {
    /// Open (or create) the sent log at `path`, replaying stored entries.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                let salt = generate_salt();
                let key = EncryptionKey::from_password(password, &salt)?;
                file.write_all(MAGIC)?;
                file.write_all(&salt)?;
                file.flush()?;
                Ok(Self {
                    file,
                    key,
                    by_conversation: HashMap::new(),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Self::load(path, password),
            Err(e) => Err(e.into()),
        }
    }

    fn load(path: &Path, password: &str) -> Result<Self, LogError> {
        let mut file = OpenOptions::new().read(true).append(true).open(path)?;
        let mut magic = [0u8; 6];
        file.read_exact(&mut magic)
            .map_err(|_| LogError::CorruptFile("missing magic".into()))?;
        if &magic != MAGIC {
            return Err(LogError::CorruptFile("bad magic".into()));
        }
        let mut salt = [0u8; SALT_SIZE];
        file.read_exact(&mut salt)
            .map_err(|_| LogError::CorruptFile("missing salt".into()))?;
        let key = EncryptionKey::from_password(password, &salt)?;
        let mut rest = Vec::new();
        file.read_to_end(&mut rest)?;

        let mut by_conversation: HashMap<ConversationId, Vec<SentEntry>> = HashMap::new();
        for entry in Self::parse_records(&rest, &key)? {
            by_conversation
                .entry(entry.conversation)
                .or_default()
                .push(entry);
        }
        Ok(Self {
            file,
            key,
            by_conversation,
        })
    }

    /// Parse the record region. A torn trailing record (crash mid-write) is
    /// dropped; a record that fails to decrypt (tampering) is an error.
    fn parse_records(mut data: &[u8], key: &EncryptionKey) -> Result<Vec<SentEntry>, LogError> {
        let mut entries = Vec::new();
        loop {
            if data.len() < 4 {
                break;
            }
            let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            data = &data[4..];
            if data.len() < len {
                break; // torn trailing record
            }
            let record = &data[..len];
            data = &data[len..];
            if record.len() < NONCE_SIZE {
                return Err(LogError::CorruptFile("record shorter than nonce".into()));
            }
            let nonce: [u8; NONCE_SIZE] = record[..NONCE_SIZE]
                .try_into()
                .expect("nonce length checked");
            let ciphertext = &record[NONCE_SIZE..];
            let plaintext = decrypt_data(ciphertext, &nonce, key)
                .map_err(|_| LogError::CorruptFile("record failed to decrypt".into()))?;
            let entry: SentEntry = bincode::deserialize(&plaintext)
                .map_err(|e| LogError::CorruptFile(format!("record decode: {e}")))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Record a sent message's plaintext: append an encrypted record, then index.
    pub fn record(
        &mut self,
        conversation: ConversationId,
        seq: u64,
        wall_clock: u64,
        plaintext: &[u8],
    ) -> Result<(), LogError> {
        let entry = SentEntry {
            conversation,
            seq,
            wall_clock,
            plaintext: plaintext.to_vec(),
        };
        let bytes =
            bincode::serialize(&entry).map_err(|e| LogError::Serialization(e.to_string()))?;
        let (ciphertext, nonce) = encrypt_data(&bytes, &self.key)?;
        let record_len = u32::try_from(NONCE_SIZE + ciphertext.len())
            .map_err(|_| LogError::Serialization("sent record exceeds u32 length".into()))?;
        let mut buf = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        buf.extend_from_slice(&record_len.to_be_bytes());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        self.file.write_all(&buf)?;
        self.file.flush()?;
        self.by_conversation
            .entry(conversation)
            .or_default()
            .push(entry);
        Ok(())
    }

    /// All sent entries for `conversation`, sorted by `(wall_clock, seq)`.
    pub fn entries(&self, conversation: &ConversationId) -> Vec<SentEntry> {
        let mut v = self
            .by_conversation
            .get(conversation)
            .cloned()
            .unwrap_or_default();
        v.sort_by_key(|e| (e.wall_clock, e.seq));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(n: u8) -> ConversationId {
        ConversationId::new([n; 32])
    }

    #[test]
    fn records_survive_reopen_and_filter_by_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sent.log");
        {
            let mut s = SentLog::open(&path, "pw").unwrap();
            s.record(conv(1), 1, 2000, b"second").unwrap();
            s.record(conv(1), 0, 1000, b"first").unwrap();
            s.record(conv(2), 1, 1500, b"other-conv").unwrap();
        }
        // Reopen from disk: entries are restored, per-conversation, time-ordered.
        let s = SentLog::open(&path, "pw").unwrap();
        let c1 = s.entries(&conv(1));
        assert_eq!(c1.len(), 2);
        assert_eq!(c1[0].plaintext, b"first"); // wall_clock 1000 sorts before 2000
        assert_eq!(c1[1].plaintext, b"second");
        assert_eq!(s.entries(&conv(2)).len(), 1);
        assert!(s.entries(&conv(9)).is_empty());
    }

    #[test]
    fn wrong_password_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sent.log");
        {
            let mut s = SentLog::open(&path, "right").unwrap();
            s.record(conv(1), 1, 1, b"secret").unwrap();
        }
        assert!(matches!(
            SentLog::open(&path, "wrong"),
            Err(LogError::CorruptFile(_))
        ));
    }

    #[test]
    fn header_only_log_reopens_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sent.log");
        {
            let _s = SentLog::open(&path, "pw").unwrap(); // header only, no records
        }
        let s = SentLog::open(&path, "pw").unwrap();
        assert!(s.entries(&conv(1)).is_empty());
    }
}
```

- [ ] **Step 3: Run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::sentlog -- --test-threads=2
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/mod.rs src-tauri/src/node/sentlog.rs
git commit -m "feat(node): encrypted sent-plaintext sidecar (SentLog)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```
Expected: 3 tests pass; clippy clean.

---

### Task 2: Persist the node's event log

**Files:**
- Modify: `src-tauri/src/eventlog/store.rs` (add `all_event_ids`)
- Modify: `src-tauri/src/eventlog/persist.rs` (add `all_event_ids`)
- Modify: `src-tauri/src/node/node.rs` (struct → persistent + sidecar, `Node::open`, `send_dm` records, migrate tests)

- [ ] **Step 1: Add `all_event_ids` to `EventLog` (store.rs)**

`EventLog` has a private `events: HashMap<EventId, Event>`. Add this accessor in `impl EventLog` (e.g. just after `has`):
```rust
    /// All event ids currently held, in arbitrary order.
    pub fn all_event_ids(&self) -> Vec<EventId> {
        self.events.keys().copied().collect()
    }
```

- [ ] **Step 2: Add `all_event_ids` to `PersistentEventLog` (persist.rs)**

In `impl PersistentEventLog` (e.g. after `prepare`):
```rust
    /// All event ids currently held (used to seed an already-emitted set).
    pub fn all_event_ids(&self) -> Vec<EventId> {
        self.log.all_event_ids()
    }
```

- [ ] **Step 3: Run the eventlog tests to confirm nothing broke**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::store eventlog::persist -- --test-threads=2
```
Expected: existing tests still pass (the accessor is additive).

- [ ] **Step 4: Switch `Node` to the persistent log + sidecar (node.rs)**

Edit the imports at the top of `src-tauri/src/node/node.rs`:
- Change `use crate::eventlog::store::EventLog;` to `use crate::eventlog::persist::PersistentEventLog;`
- Add: `use crate::eventlog::LogError;`, `use crate::node::sentlog::SentLog;`, `use std::path::Path;`

Change the `Node` struct's `log` field and add `sentlog`:
```rust
pub struct Node {
    identity: DeviceIdentity,
    log: Mutex<PersistentEventLog>,
    sentlog: Mutex<SentLog>,
    roster: Arc<Mutex<Roster>>,
    incoming: mpsc::UnboundedSender<ReceivedDm>,
    emitted: Mutex<HashSet<EventId>>,
}
```

Replace `Node::new` with `Node::open`:
```rust
    /// Open a node backed by a durable event log at `log_path` and a sent-plaintext
    /// sidecar at `sent_path` (both encrypted with `password`). Restored history is
    /// seeded into the already-emitted set so it is NOT re-streamed — only events
    /// received after open are surfaced live.
    pub fn open(
        identity: DeviceIdentity,
        roster: Arc<Mutex<Roster>>,
        incoming: mpsc::UnboundedSender<ReceivedDm>,
        log_path: &Path,
        sent_path: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        let log = PersistentEventLog::open(log_path, password)?;
        let sentlog = SentLog::open(sent_path, password)?;
        let emitted: HashSet<EventId> = log.all_event_ids().into_iter().collect();
        Ok(Arc::new(Self {
            identity,
            log: Mutex::new(log),
            sentlog: Mutex::new(sentlog),
            roster,
            incoming,
            emitted: Mutex::new(emitted),
        }))
    }
```

- [ ] **Step 5: Record sent plaintext in `send_dm`**

In `send_dm`, capture `wall_clock` and `seq` so they can be recorded in the sidecar, then record after the append block. Replace the local-append block (the `let conv = ...; let self_author = ...; { let mut log ... append ... }` portion) with:
```rust
        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let wall_clock = now_millis();
        let seq;
        {
            let mut log = self.log.lock().expect("log mutex not poisoned");
            let (parents, lamport) = log.prepare(&conv);
            seq = log
                .version_vector(&conv)
                .get(&self_author)
                .copied()
                .unwrap_or(0)
                + 1;
            let event = build_dm_event(
                &self.identity,
                &peer.public,
                conv,
                seq,
                parents,
                lamport,
                wall_clock,
                text,
            )
            .map_err(NodeError::Seal)?;
            log.append(event).map_err(NodeError::Log)?;
        }

        // Keep a local plaintext copy of what we sent (sealed events aren't
        // self-decryptable). Best-effort: a sidecar write error doesn't fail a
        // message that was sealed, appended, and about to be delivered.
        let _ = self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .record(conv, seq, wall_clock, text);
```
The rest of `send_dm` (the `deliver_direct` / `replicate_to_post_office` / `emit_new_messages` / `match`) is UNCHANGED.

- [ ] **Step 6: Migrate the in-crate node tests to `Node::open`**

The `#[cfg(test)] mod tests` block constructs nodes with `Node::new(identity, roster, tx)`. Each call must become a `Node::open(identity, roster, tx, &log_path, &sent_path, "pw").unwrap()` backed by a `tempfile::tempdir()`. Concretely:

- `two_nodes_exchange_a_dm_over_loopback_tcp`: add `let dir = tempfile::tempdir().unwrap();` near the top, then
  ```rust
  let alice_node = Node::open(alice, alice_roster, alice_tx, &dir.path().join("alice.log"), &dir.path().join("alice-sent.log"), "pw").unwrap();
  let bob_node = Node::open(bob, bob_roster, bob_tx, &dir.path().join("bob.log"), &dir.path().join("bob-sent.log"), "pw").unwrap();
  ```
- `send_dm_to_unknown_peer_errors`: add `let dir = tempfile::tempdir().unwrap();`, then
  ```rust
  let node = Node::open(me, roster, tx, &dir.path().join("me.log"), &dir.path().join("me-sent.log"), "pw").unwrap();
  ```
- `offline_dm_delivered_via_post_office_over_loopback`: it already has `let dir = tempfile::tempdir().unwrap();` — reuse it:
  ```rust
  let alice_node = Node::open(alice, alice_roster, alice_tx, &dir.path().join("alice.log"), &dir.path().join("alice-sent.log"), "pw").unwrap();
  let bob_node = Node::open(bob, bob_roster, bob_tx, &dir.path().join("bob.log"), &dir.path().join("bob-sent.log"), "pw").unwrap();
  ```

- [ ] **Step 7: Add the restart no-restream test**

Append this test inside `mod tests` (it pre-seeds a persistent log, reopens as a Node, and asserts the existing event is NOT re-emitted but a new one IS). Note: `emit_new_messages`, `self.log`, and `self.sentlog` are private but reachable from this same-module test block.
```rust
    #[tokio::test]
    async fn reopen_seeds_emitted_so_history_is_not_restreamed() {
        let dir = tempfile::tempdir().unwrap();
        let me = DeviceIdentity::generate();
        let alice = DeviceIdentity::generate();
        let me_pub = me.public();
        let conv = crate::node::conversation::dm_conversation_id(&me_pub, &alice.public());
        let log_path = dir.path().join("me.log");
        let sent_path = dir.path().join("me-sent.log");

        // Prior session: a received DM from Alice is already in the persistent log.
        let old = crate::node::conversation::build_dm_event(
            &alice, &me_pub, conv, 1, vec![], 1, 1000, b"old",
        )
        .unwrap();
        {
            let mut log =
                crate::eventlog::persist::PersistentEventLog::open(&log_path, "pw").unwrap();
            log.append(old.clone()).unwrap();
        }

        // Roster knows Alice (so decryption is possible if it were attempted).
        let roster = Arc::new(Mutex::new(Roster::default()));
        roster.lock().unwrap().update(
            &Announce::new(&alice, "Alice", 4000),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &me_pub.user_id(),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let node = Node::open(me, roster, tx, &log_path, &sent_path, "pw").unwrap();

        // Restored history must NOT be re-streamed.
        node.emit_new_messages(conv);
        assert!(rx.try_recv().is_err(), "restored history was re-streamed");

        // A genuinely-new received event (not present at open) IS surfaced.
        let fresh = crate::node::conversation::build_dm_event(
            &alice, &me_pub, conv, 2, vec![old.id], 2, 2000, b"new",
        )
        .unwrap();
        node.log.lock().unwrap().append(fresh).unwrap();
        node.emit_new_messages(conv);
        let got = rx.try_recv().expect("a new message is emitted");
        assert_eq!(got.text, b"new");
    }
```

- [ ] **Step 8: Run node tests, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/store.rs src-tauri/src/eventlog/persist.rs src-tauri/src/node/node.rs
git commit -m "feat(node): back the node with a durable log + sent sidecar; seed emitted on open

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```
Expected: all node tests pass (migrated + the new restart test); clippy clean.

---

### Task 3: `Node::history` — merged conversation view

**Files:**
- Modify: `src-tauri/src/node/node.rs`

- [ ] **Step 1: Add `HistoryEntry` and the history methods**

Add the `HistoryEntry` struct near `ReceivedDm` (after it):
```rust
/// A merged conversation-history entry (sent or received), for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub from_me: bool,
    pub who: String,
    pub text: Vec<u8>,
    pub wall_clock: u64,
}
```

Add these two methods to `impl Node` (e.g. after `drain_from_post_office`). `history` snapshots the log's events under its lock, drops it, then locks the roster — never two store locks at once, and no lock held across an await (this is sync):
```rust
    /// The last `limit` messages of the DM with `peer`, both directions, in time
    /// order. Convenience wrapper that derives the conversation id.
    pub fn dm_history(&self, peer: &PublicIdentity, limit: usize) -> Vec<HistoryEntry> {
        let conv = dm_conversation_id(&self.identity.public(), peer);
        self.history(conv, limit)
    }

    /// The last `limit` messages of `conversation`, both directions, sorted by
    /// wall-clock: peer-authored events decrypted from the durable log, plus our
    /// own sent plaintext from the local sidecar.
    pub fn history(&self, conversation: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Snapshot the conversation's events, then release the log lock.
        let events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conversation).into_iter().cloned().collect()
        };

        let mut entries: Vec<HistoryEntry> = Vec::new();
        {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            for event in &events {
                if event.kind != EventKind::Message || event.author == self_author {
                    continue; // our own sent events come from the sidecar (below)
                }
                if let Some((_uid, name, text)) = open_dm_event(&self.identity, &roster, event) {
                    entries.push(HistoryEntry {
                        from_me: false,
                        who: name,
                        text,
                        wall_clock: event.wall_clock,
                    });
                }
            }
        }
        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&conversation)
        {
            entries.push(HistoryEntry {
                from_me: true,
                who: "you".to_string(),
                text: sent.plaintext,
                wall_clock: sent.wall_clock,
            });
        }
        entries.sort_by_key(|e| e.wall_clock);
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }
```

Add `PublicIdentity` to the identity import: change `use crate::identity::device::DeviceIdentity;` to `use crate::identity::device::{DeviceIdentity, PublicIdentity};`.

- [ ] **Step 2: Add a history unit test**

Append inside `mod tests`:
```rust
    #[tokio::test]
    async fn history_merges_sent_and_received_in_time_order() {
        let dir = tempfile::tempdir().unwrap();
        let me = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let me_pub = me.public();
        let conv = crate::node::conversation::dm_conversation_id(&me_pub, &bob.public());

        let roster = Arc::new(Mutex::new(Roster::default()));
        roster.lock().unwrap().update(
            &Announce::new(&bob, "Bob", 4000),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &me_pub.user_id(),
        );
        let (tx, _rx) = mpsc::unbounded_channel();
        let node = Node::open(
            me,
            roster,
            tx,
            &dir.path().join("me.log"),
            &dir.path().join("me-sent.log"),
            "pw",
        )
        .unwrap();

        // Bob sent us a message at t=1000 (a received event in our durable log).
        let recv =
            crate::node::conversation::build_dm_event(&bob, &me_pub, conv, 1, vec![], 1, 1000, b"hi from bob")
                .unwrap();
        node.log.lock().unwrap().append(recv).unwrap();
        // We sent one at t=2000 (recorded in the sidecar).
        node.sentlog
            .lock()
            .unwrap()
            .record(conv, 1, 2000, b"hi from me")
            .unwrap();

        let hist = node.dm_history(&bob.public(), 10);
        assert_eq!(hist.len(), 2);
        assert!(!hist[0].from_me && hist[0].text == b"hi from bob"); // t=1000 first
        assert!(hist[1].from_me && hist[1].who == "you" && hist[1].text == b"hi from me");

        // `limit` keeps the most-recent entries.
        let last1 = node.dm_history(&bob.public(), 1);
        assert_eq!(last1.len(), 1);
        assert!(last1[0].from_me); // the newer (t=2000) one
    }
```

- [ ] **Step 3: Run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs
git commit -m "feat(node): Node::history merges received + sent (sidecar) in time order

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Binary wiring + `/history` command

**Files:**
- Modify: `src-tauri/src/bin/mesh-talk-node.rs`

- [ ] **Step 1: Open the node with durable paths**

In the normal-node `main`, replace the node construction `let node = Node::new(identity, Arc::clone(&roster), incoming_tx);` with paths derived from the keystore dir (mirroring how the post office derives `relay.log`):
```rust
    let key_dir = std::path::Path::new(&args.keystore)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let node = Node::open(
        identity,
        Arc::clone(&roster),
        incoming_tx,
        &key_dir.join("messages.log"),
        &key_dir.join("sent.log"),
        &args.password,
    )?;
```
(`Node::open` returns `Result<_, LogError>`; `LogError: Error`, so `?` works under `main`'s `Box<dyn std::error::Error>`. The `announce` is still built from `&identity` BEFORE this line — keep that order.)

- [ ] **Step 2: Add the `/history` REPL branch**

In the REPL `while let Some(line)` chain, add a branch BEFORE the final `else if !line.is_empty()` arm:
```rust
        } else if let Some(rest) = line.strip_prefix("/history ") {
            handle_history(&node, &roster, rest);
```

- [ ] **Step 3: Add the `handle_history` function**

Add next to `handle_msg`:
```rust
/// Parse and dispatch `/history <peer-prefix> [n]`: resolve the peer, then print
/// the last n (default 20) messages of that DM, both directions, oldest first.
fn handle_history(node: &Arc<Node>, roster: &Arc<Mutex<Roster>>, rest: &str) {
    let mut parts = rest.split_whitespace();
    let prefix = parts.next().unwrap_or("");
    let limit: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    if prefix.is_empty() {
        emit("usage: /history <user_id-prefix> [n]");
        return;
    }
    // Resolve the peer under the roster lock, then DROP the lock before calling
    // into the node (which locks the roster again — std Mutex is not reentrant).
    let peer = {
        let roster = roster.lock().expect("roster mutex not poisoned");
        let peers = roster.peers();
        match resolve_recipient(&peers, prefix) {
            Ok(uid) => roster.get(&uid).cloned(),
            Err(ResolveError::NotFound) => {
                emit(&format!("no peer matches prefix '{prefix}'"));
                return;
            }
            Err(ResolveError::Ambiguous(n)) => {
                emit(&format!("'{prefix}' matches {n} peers; be more specific"));
                return;
            }
        }
    };
    let Some(peer) = peer else {
        emit("peer vanished from roster");
        return;
    };
    let entries = node.dm_history(&peer.public, limit);
    if entries.is_empty() {
        emit("(no history)");
        return;
    }
    for e in entries {
        let text = String::from_utf8_lossy(&e.text);
        emit(&format!("[{}] {}: {text}", e.wall_clock, e.who));
    }
}
```
Add `HistoryEntry` is not needed in the bin (we only read fields). `Node`/`dm_history` are already reachable via the existing `use mesh_talk::node::{Node, ReceivedDm};` (which re-exports `Node`).

- [ ] **Step 4: Build, clippy, bin tests**
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo test --bin mesh-talk-node -- --test-threads=2
```
Expected: build OK; clippy clean; the resolver tests still pass.

- [ ] **Step 5: Single-process smoke (startup + clean exit with a keystore dir)**
```bash
cd src-tauri && TMP=$(mktemp -d) && printf '/history nobody\n/quit\n' | nice -n 10 cargo run --quiet --bin mesh-talk-node -- \
  --keystore "$TMP/id.keystore" --password pw --name Smoke 2>&1 | grep -E "^(node|no peer) "
ls "$TMP"   # expect: id.keystore, messages.log, sent.log
```
Expected: a `node <uid> listening ...` line and a `no peer matches prefix 'nobody'` line; the dir contains `messages.log` and `sent.log` (the durable files were created).

- [ ] **Step 6: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(node-cli): durable message log paths + /history command

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Restart integration rig

**Files:**
- Create: `src-tauri/tests/persistent_history.rs`

Proves persistence across a real process restart: Alice and Bob exchange DMs both ways; Alice restarts on the same keystore; her `/history` shows BOTH her sent and the received message (no post office involved — the messages come purely from Alice's persisted log + sidecar).

- [ ] **Step 1: Create `src-tauri/tests/persistent_history.rs`**

```rust
//! Persistence rig: Alice and Bob exchange DMs both directions; Alice restarts on
//! the same keystore; her `/history` shows both her sent message (from the local
//! sidecar) and Bob's reply (decrypted from the durable log) — proving message
//! history survives a real process restart. No post office: history comes purely
//! from disk. `#[ignore]`d (real processes + UDP broadcast). Run explicitly:
//!   cd src-tauri && nice -n 10 cargo test --test persistent_history -- --ignored --test-threads=2

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

struct CliNode {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl CliNode {
    fn spawn(keystore: &std::path::Path, name: &str, discovery_port: u16) -> CliNode {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
            .arg("--keystore")
            .arg(keystore)
            .arg("--password")
            .arg("pw")
            .arg("--name")
            .arg(name)
            .arg("--port")
            .arg("0")
            .arg("--discovery-port")
            .arg(discovery_port.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mesh-talk-node");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        CliNode { child, stdin, lines: rx }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write to child stdin");
        self.stdin.flush().expect("flush child stdin");
    }

    fn wait_for(&self, pred: impl Fn(&str) -> bool, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self.lines.recv_timeout(remaining) {
                Ok(l) => {
                    if pred(&l) {
                        return Some(l);
                    }
                }
                Err(_) => return None,
            }
        }
    }
}

impl Drop for CliNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_uid(node: &CliNode) -> String {
    let line = node
        .wait_for(|l| l.starts_with("node "), Duration::from_secs(20))
        .expect("node startup line");
    line.split_whitespace().nth(1).expect("uid").to_string()
}

fn await_peer(node: &mut CliNode, want_uid: &str, who: &str) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        node.send("/peers");
        if node
            .wait_for(|l| l.contains(want_uid), Duration::from_secs(1))
            .is_some()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!("{who} never discovered {want_uid}");
}

#[test]
#[ignore = "real processes + UDP broadcast; run with --ignored"]
fn message_history_survives_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Perturbed per-run, disjoint from the other rigs' ranges.
    let dp = 48900 + (std::process::id() % 100) as u16;

    let alice_keystore = dir.path().join("alice.keystore");
    let mut alice = CliNode::spawn(&alice_keystore, "Alice", dp);
    let mut bob = CliNode::spawn(&dir.path().join("bob.keystore"), "Bob", dp);
    let alice_uid = read_uid(&alice);
    let bob_uid = read_uid(&bob);

    await_peer(&mut alice, &bob_uid, "Alice→Bob");
    await_peer(&mut bob, &alice_uid, "Bob→Alice");

    // Alice → Bob, then Bob → Alice (both persisted on Alice's side).
    alice.send(&format!("/msg {} ping-from-alice", &bob_uid[..8]));
    bob.wait_for(|l| l.contains("ping-from-alice"), Duration::from_secs(10))
        .expect("Bob received Alice's ping");
    bob.send(&format!("/msg {} pong-from-bob", &alice_uid[..8]));
    alice
        .wait_for(|l| l.contains("pong-from-bob"), Duration::from_secs(10))
        .expect("Alice received Bob's pong");

    // Alice restarts on the same keystore (same identity + same durable files).
    drop(alice);
    let mut alice = CliNode::spawn(&alice_keystore, "Alice", dp);
    let _ = read_uid(&alice);
    await_peer(&mut alice, &bob_uid, "Alice(restarted)→Bob"); // re-discover Bob to resolve + decrypt

    // /history shows BOTH directions, restored from disk.
    alice.send(&format!("/history {} 20", &bob_uid[..8]));
    let sent = alice
        .wait_for(|l| l.contains("ping-from-alice"), Duration::from_secs(10))
        .expect("history shows Alice's sent message after restart");
    assert!(sent.contains("you:"), "sent line attributed to you: {sent}");
    alice
        .wait_for(|l| l.contains("pong-from-bob"), Duration::from_secs(10))
        .expect("history shows Bob's received reply after restart");
}
```

- [ ] **Step 2: Build, then run the rig explicitly (be patient, ~60-90s)**
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo test --test persistent_history -- --ignored --test-threads=2
```
Expected: `test message_history_survives_restart ... ok`. If it fails:
- at an `await_peer`: same-host UDP broadcast didn't converge — retry ONCE.
- at a `/history` assertion: report the exact stage and what `/history` printed; do NOT weaken assertions.

- [ ] **Step 3: Confirm the rig is excluded from the normal sweep**
```bash
cd src-tauri && nice -n 10 cargo test --test persistent_history -- --test-threads=2
```
Expected: `0 passed; 0 failed; 1 ignored`.

- [ ] **Step 4: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/tests/persistent_history.rs
git commit -m "test(node-cli): message history survives a real process restart

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer

- **What this delivers:** node DMs persist across restart. `Node.log` is now the durable `PersistentEventLog`; a local encrypted `SentLog` sidecar holds the plaintext of messages this node sent (it can't decrypt its own sealed events); `Node::open` seeds the `emitted` set from the loaded log so restored history is not re-streamed; `Node::history`/`dm_history` merge received (decrypted) + sent (sidecar) in time order; the binary opens `messages.log` + `sent.log` and adds `/history <peer> [n]`.
- **Security/correctness:** the sidecar reuses the audited `storage::encryption` cipher (AES-256-GCM, password KDF, per-record nonce) with the same framing as `eventlog::persist` — local-only, never synced, no signatures (it is not a synced event). No re-stream on restart is a seeded-`emitted` invariant, unit-tested. No lock-across-await; `history` snapshots events under the log lock then releases it before locking the roster; `handle_history` drops the roster lock before calling the node (std Mutex is not reentrant).
- **Accepted limitations (from the spec):** the sidecar holds your sent plaintext (encrypted at rest) — the cost of sealing-to-recipient; seal-to-self and cross-device sent-history sync are deferred. DMs only (no channel history). No retention/pruning; logs grow unbounded. `/history` is last-N only (no pagination API). The `Node::new → Node::open` change is reflected in the migrated in-crate tests.
- **Reviewer note:** confirm `LogError` exposes `CorruptFile`/`Serialization` and `From<io::Error>` (the sidecar relies on the same conversions `eventlog::persist` uses) and that `LogError: std::error::Error` (so the binary's `?` works).
