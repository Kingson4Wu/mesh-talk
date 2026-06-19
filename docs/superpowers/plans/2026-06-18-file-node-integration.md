# File Sharing in the Node Implementation Plan (File Plan 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire file sharing into the live `Node`: send a file into a DM or channel (chunks as events in a per-file conversation + a sealed `FileManifest` event), surface received files on a stream, and save a received file by reassembling + verifying its chunks — validated by an in-process two-node transfer over loopback TCP.

**Architecture:** Mirrors the channel integration. A `FileBook` (in-memory, like `ChannelBook`) records received manifests keyed by their per-file conversation id + dedups. `process_file_events(conv)` opens any new `FileManifest` events (DM sealed-box for a DM conv, the channel group key for a channel conv) and streams `ReceivedFile`. `send_file_dm`/`send_file_channel` chunk + seal the file into a fresh per-file conversation, seal the manifest into the original conversation, and distribute both. `save_file` gathers the per-file conversation's chunk events and runs `reassemble_and_verify`.

**Tech Stack:** Rust; `file::{crypto,manifest}` (Plan 1), `dm::{seal,open}`, `channel::ChannelState::{seal_message,open_message}`, the node's `request_round`/`dial`/`elected_post_office`/`distribute_channel` machinery. No new dependencies.

---

## Background the implementer needs

This is **File Plan 2** (spec: `docs/superpowers/specs/2026-06-18-file-sharing-design.md`). Plan 1 shipped the pure `file` module:
```rust
crate::file::{FileKey, FileManifest, CHUNK_SIZE, file_checksum, split_chunks, seal_chunk, open_chunk, reassemble_and_verify, FileError};
// FileKey: generate(), from_bytes([u8;32]), as_bytes()->&[u8;32]
// split_chunks(&[u8]) -> Vec<&[u8]>  (>=1 chunk, last may be short; empty -> one empty chunk)
// seal_chunk(&FileKey, &[u8]) -> Result<Vec<u8>, FileError>   (nonce ‖ AES-256-GCM)
// open_chunk(&FileKey, &[u8]) -> Result<Vec<u8>, FileError>
// FileManifest { name: String, size: u64, mime: String, checksum: [u8;32], file_key: [u8;32], file_conv: ConversationId, chunk_count: u32 }
//   ::encode()->Vec<u8>, ::decode(&[u8])->Option<Self>, ::key()->FileKey
// reassemble_and_verify(&FileManifest, &[Vec<u8>]) -> Result<Vec<u8>, FileError>
```

**The `Node` shape (current, verbatim relevant parts):**
```rust
pub struct Node {
    identity: DeviceIdentity,
    log: Mutex<PersistentEventLog>,
    sentlog: Mutex<SentLog>,
    roster: Arc<Mutex<Roster>>,
    incoming: mpsc::UnboundedSender<ReceivedDm>,
    channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
    channels: Mutex<ChannelBook>,
    emitted: Mutex<HashSet<EventId>>,
}
// Node::open(identity, roster, incoming, channel_incoming, log_path, sent_path, password) -> Result<Arc<Self>, LogError>
//   (seeds `emitted` from log.all_event_ids(); seeds `channels` by processing every conversation)
// serve_connection: while Served::Handled(conv) -> emit_new_messages(conv); process_channel(conv)
// drain_from_post_office: per non-PO peer request_round(dm conv)+emit; then per channel_ids() request_round+process_channel
// fn process_channel(&self, conv): snapshot log.events(conv) -> book.process -> send on channel_incoming
// fn append_channel_event(&self, conv, kind: EventKind, ciphertext: Vec<u8>) -> Result<(), NodeError>
//   (seq from version_vector(conv).get(self_author)+1; parents+lamport from log.prepare; Event::new(...); log.append)
// async fn deliver_direct(&self, peer: &PeerRecord, conv) -> Result<(), SessionError>  (dial + request_round)
// async fn replicate_to_post_office(&self, conv) -> Result<bool, SessionError>
// async fn distribute_channel(&self, channel, members: &[PublicIdentity])  (dial each known member + replicate PO)
// pub fn create_channel / send_channel_message / channel_history / list_channels
// NodeError { UnknownPeer(String), Seal(DmError), Log(LogError), Session(SessionError), Channel(String) }
```

**Verified helpers:**
- `crate::node::conversation::{dm_conversation_id(&PublicIdentity,&PublicIdentity)->ConversationId, build_dm_event(...)}`.
- `crate::dm::{seal(&DeviceIdentity, recipient_x25519: &[u8;32], plaintext: &[u8]) -> Result<Vec<u8>, DmError>, open(&DeviceIdentity, sender_x25519: &[u8;32], envelope: &[u8]) -> Result<Vec<u8>, DmError>}`.
- `ChannelState::{seal_message(&[u8])->Result<Vec<u8>,ChannelError>, open_message(&[u8])->Option<Vec<u8>>, members()->&[PublicIdentity]}`; `ChannelBook::{state(&ConversationId)->Option<&ChannelState>}`.
- `EventKind::{Message, FileManifest}` (FileManifest = 7 already exists in the enum). `Event { author: Author, kind: EventKind, ciphertext: Vec<u8>, wall_clock: u64, id: EventId, .. }`. `Author::from_ed25519(self.identity.public().ed25519_pub)`. `Event::new(&DeviceIdentity, conv, seq, parents, lamport, wall_clock, kind, ciphertext)`.
- `PeerRecord { addr, public: PublicIdentity, post_office: bool, name: String }`; `Roster::get(&str)->Option<&PeerRecord>`.

**CPU/test discipline (MANDATORY):** never a bare `cargo test`/`build`. Use `nice -n 10`, one filter per run:
```bash
cd src-tauri && nice -n 10 cargo test --lib node::file -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
Confirm `git status | head -1` == `On branch feat/redesign-phase0` after committing.

---

### Task 1: `FileBook` + `ReceivedFile` + Node plumbing (receive side + constructor migration)

**Files:** Create `src-tauri/src/node/filebook.rs`; modify `src-tauri/src/node/mod.rs`, `node/node.rs`, `node/runtime.rs`, `bin/mesh-talk-node.rs`.

- [ ] **Step 1: `node/filebook.rs` — the in-memory received-file store**
```rust
//! In-memory store of received file manifests (keyed by the per-file conversation id)
//! plus an emitted-event set so a manifest is surfaced once. Mirrors `ChannelBook`.
//! Opening a manifest needs the conversation crypto (roster for a DM, the channel
//! group key for a channel), so the `Node` opens it and calls [`FileBook::record`].

use crate::eventlog::event::{ConversationId, EventId};
use crate::file::FileManifest;
use std::collections::{HashMap, HashSet};

/// A received file announcement, surfaced to the application.
#[derive(Debug, Clone)]
pub struct ReceivedFile {
    /// The DM/channel conversation the manifest arrived in.
    pub conv: ConversationId,
    /// The sender's user-id fingerprint.
    pub from: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// The per-file conversation holding the chunk events (pass to `save_file`).
    pub file_conv: ConversationId,
}

#[derive(Default)]
pub struct FileBook {
    manifests: HashMap<ConversationId, FileManifest>, // keyed by file_conv
    emitted: HashSet<EventId>,
}

impl FileBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// The manifest for a per-file conversation, if received.
    pub fn manifest(&self, file_conv: &ConversationId) -> Option<&FileManifest> {
        self.manifests.get(file_conv)
    }

    /// Record an opened manifest (idempotent — same file_conv overwrites with the
    /// same manifest).
    pub fn record(&mut self, manifest: FileManifest) {
        self.manifests.insert(manifest.file_conv, manifest);
    }

    /// Whether a FileManifest event id was already surfaced (or seeded on open).
    pub fn is_emitted(&self, id: &EventId) -> bool {
        self.emitted.contains(id)
    }

    /// Mark a FileManifest event id surfaced/seeded.
    pub fn mark_emitted(&mut self, id: EventId) {
        self.emitted.insert(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::ConversationId;

    fn conv(n: u8) -> ConversationId {
        ConversationId::new([n; 32])
    }

    #[test]
    fn records_and_dedups() {
        let mut book = FileBook::new();
        assert!(book.manifest(&conv(1)).is_none());
        let m = FileManifest {
            name: "a.txt".into(),
            size: 3,
            mime: "text/plain".into(),
            checksum: [0u8; 32],
            file_key: [1u8; 32],
            file_conv: conv(1),
            chunk_count: 1,
        };
        book.record(m.clone());
        assert_eq!(book.manifest(&conv(1)), Some(&m));

        let id = crate::eventlog::event::EventId::new([9u8; 32]);
        assert!(!book.is_emitted(&id));
        book.mark_emitted(id);
        assert!(book.is_emitted(&id));
    }
}
```
(Verify `EventId::new([u8;32])` exists — it mirrors `ConversationId::new`. If `EventId`'s constructor differs, adapt the test only.)

- [ ] **Step 2: register the module (`node/mod.rs`)**

Add `pub mod filebook;` with the other `pub mod`s, and re-export next to where `ChannelSummary`/`ReceivedChannelMessage`-style types are re-exported:
```rust
pub use filebook::{FileBook, ReceivedFile};
```
(Match the existing `pub use` grouping style in `node/mod.rs`.)

- [ ] **Step 3: `Node` fields + `Node::open` migration + seed (node.rs)**

Add imports: `use crate::node::filebook::{FileBook, ReceivedFile};` and `use crate::file::FileManifest;`.
Add fields after `channels`:
```rust
    file_incoming: mpsc::UnboundedSender<ReceivedFile>,
    files: Mutex<FileBook>,
```
`Node::open` gains `file_incoming` right after `channel_incoming`. After the channel-seeding loop, seed the file book's emitted set so existing manifests aren't re-surfaced after restart (opening them needs the roster/channel crypto which isn't available at open — persisting received manifests across restart is deferred; see notes):
```rust
        let mut files = FileBook::new();
        for conv in log.conversations() {
            for event in log.events(&conv) {
                if event.kind == EventKind::FileManifest {
                    files.mark_emitted(event.id);
                }
            }
        }
```
Add to the struct literal: `file_incoming,` and `files: Mutex::new(files),`.

- [ ] **Step 4: `process_file_events` + wire into the serve loop (node.rs)**

Add this method. It opens new FileManifest events with the right crypto (channel group key if `conv` is a known channel, else the DM sealed-box via the author's roster key), records the manifest, and streams `ReceivedFile`. Snapshot under locks, no lock across the (sync) send:
```rust
    /// Open and surface any new `FileManifest` events in `conv`. Channel manifests
    /// open with the channel group key; DM manifests with the DM sealed-box (the
    /// author's X25519 from the roster). A no-op for conversations with no new
    /// manifest events. Own manifests are skipped (the sender already knows).
    fn process_file_events(&self, conv: ConversationId) {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let manifest_events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| e.kind == EventKind::FileManifest && e.author != self_author)
                .cloned()
                .collect()
        };
        if manifest_events.is_empty() {
            return;
        }
        // Is this a channel conversation? If so, open with its group key.
        let is_channel = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&conv).is_some()
        };
        let mut surfaced: Vec<ReceivedFile> = Vec::new();
        for event in manifest_events {
            {
                let mut files = self.files.lock().expect("files mutex not poisoned");
                if files.is_emitted(&event.id) {
                    continue;
                }
                files.mark_emitted(event.id);
            }
            let plaintext = if is_channel {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                match book.state(&conv).and_then(|s| s.open_message(&event.ciphertext)) {
                    Some(p) => p,
                    None => continue,
                }
            } else {
                let author_uid = event.author.user_id();
                let sender_x25519 = {
                    let roster = self.roster.lock().expect("roster mutex not poisoned");
                    match roster.get(&author_uid) {
                        Some(p) => p.public.x25519_pub,
                        None => continue, // author unknown → can't open yet
                    }
                };
                match crate::dm::open(&self.identity, &sender_x25519, &event.ciphertext) {
                    Ok(p) => p,
                    Err(_) => continue,
                }
            };
            let Some(manifest) = FileManifest::decode(&plaintext) else {
                continue;
            };
            let received = ReceivedFile {
                conv,
                from: event.author.user_id(),
                name: manifest.name.clone(),
                size: manifest.size,
                mime: manifest.mime.clone(),
                file_conv: manifest.file_conv,
            };
            self.files
                .lock()
                .expect("files mutex not poisoned")
                .record(manifest);
            surfaced.push(received);
        }
        for rf in surfaced {
            let _ = self.file_incoming.send(rf);
        }
    }
```
Wire it into `serve_connection` (after `process_channel`):
```rust
    pub async fn serve_connection(&self, mut channel: SecureChannel<TcpStream>) {
        while let Ok(Served::Handled(conv)) = serve_one(&mut channel, &self.log).await {
            self.emit_new_messages(conv);
            self.process_channel(conv);
            self.process_file_events(conv);
        }
    }
```

- [ ] **Step 5: migrate the other `Node::open` call sites**

`grep -rn "Node::open" src-tauri/src`. For each, add a `file_incoming` arg right after `channel_incoming`:
- `node/runtime.rs` `RedesignRuntime::start`: add `let (file_tx, _file_rx) = mpsc::unbounded_channel::<crate::node::filebook::ReceivedFile>();` before the `Node::open` call and pass `file_tx` after `channel_tx`. (The receiver is wired to the GUI in File Plan 3; dropped for now.)
- `bin/mesh-talk-node.rs`: add `let (file_tx, _file_rx) = mpsc::unbounded_channel::<mesh_talk::node::filebook::ReceivedFile>();` and pass it after the channel sender.
- `node/node.rs` `#[cfg(test)] mod tests`: every `Node::open` gains a `file_incoming`. Add `let (file_tx, _file_rx) = mpsc::unbounded_channel();` (distinct names per node, e.g. `a_file_tx`) before each call and insert after the channel sender. The channel loopback rig (`two_nodes_exchange_a_channel_message_over_loopback_tcp`) and all others just need the extra sink.

- [ ] **Step 6: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::filebook -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/filebook.rs src-tauri/src/node/mod.rs src-tauri/src/node/node.rs src-tauri/src/node/runtime.rs src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(node): FileBook + process_file_events; seed manifests on open; constructor migration"
git status | head -1
```

---

### Task 2: send_file + save_file + distribution + the loopback rig

**Files:** Modify `src-tauri/src/node/node.rs`.

- [ ] **Step 1: imports + generalize the append helper**

Add imports: `use crate::file::{file_checksum, seal_chunk, split_chunks, FileError, FileKey, FileManifest};` and `use std::path::Path;` (if not present).

Rename `append_channel_event` to `append_event` (it already sequences + appends an arbitrary `(conv, kind, ciphertext)` — it's generic, the name was channel-specific). Update its 3 call sites in `create_channel` (×2) and `send_channel_message` (×1) to `append_event`. (Pure rename; no behavior change.)

- [ ] **Step 2: `send_file_dm`, `send_file_channel`, the shared chunker, `save_file`**
```rust
    /// Send the file at `path` to a DM peer. Chunks + seals it into a fresh per-file
    /// conversation, seals the manifest to the recipient, posts a `FileManifest`
    /// event into the DM conversation, and distributes both. Returns the per-file
    /// conversation id (the handle for the recipient to save).
    pub async fn send_file_dm(&self, recipient: &str, path: &Path) -> Result<ConversationId, NodeError> {
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;

        let (manifest, file_conv) = self.stage_file(path)?;
        let sealed = crate::dm::seal(&self.identity, &peer.public.x25519_pub, &manifest.encode())
            .map_err(NodeError::Seal)?;
        let dm_conv = dm_conversation_id(&self.identity.public(), &peer.public);
        self.append_event(dm_conv, EventKind::FileManifest, sealed)?;

        self.deliver_direct(&peer, file_conv).await.ok();
        self.replicate_to_post_office(file_conv).await.ok();
        self.deliver_direct(&peer, dm_conv).await.ok();
        self.replicate_to_post_office(dm_conv).await.ok();
        Ok(file_conv)
    }

    /// Send the file at `path` to a channel we hold the key for.
    pub async fn send_file_channel(&self, channel: ConversationId, path: &Path) -> Result<ConversationId, NodeError> {
        let (manifest, file_conv) = self.stage_file(path)?;
        let (sealed, members) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            let sealed = state
                .seal_message(&manifest.encode())
                .map_err(|e| NodeError::Channel(format!("manifest sealing failed: {e}")))?;
            (sealed, state.members().to_vec())
        };
        self.append_event(channel, EventKind::FileManifest, sealed)?;
        self.distribute_channel(file_conv, &members).await;
        self.distribute_channel(channel, &members).await;
        Ok(file_conv)
    }

    /// Read `path`, chunk + seal it into a fresh per-file conversation (appending a
    /// chunk event per piece), and build the (unsealed) manifest. Shared by both
    /// send paths. The caller seals + posts the manifest into the original conv.
    fn stage_file(&self, path: &Path) -> Result<(FileManifest, ConversationId), NodeError> {
        let data = std::fs::read(path)
            .map_err(|e| NodeError::Channel(format!("read file: {e}")))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let key = FileKey::generate();
        let checksum = file_checksum(&data);
        let file_conv = crate::channel::new_channel_id(); // a fresh random ConversationId
        let chunks = split_chunks(&data);
        let chunk_count = chunks.len() as u32;
        for chunk in &chunks {
            let sealed = seal_chunk(&key, chunk)
                .map_err(|e| NodeError::Channel(format!("chunk seal: {e}")))?;
            self.append_event(file_conv, EventKind::Message, sealed)?;
        }
        let manifest = FileManifest {
            name,
            size: data.len() as u64,
            mime: "application/octet-stream".to_string(),
            checksum,
            file_key: *key.as_bytes(),
            file_conv,
            chunk_count,
        };
        Ok((manifest, file_conv))
    }

    /// Save a received file (identified by its per-file conversation id) to `dest`:
    /// gather the chunk events, reassemble, verify the checksum, and write. Errors if
    /// the manifest is unknown, not all chunks have synced yet, or verification fails.
    pub fn save_file(&self, file_conv: ConversationId, dest: &Path) -> Result<(), NodeError> {
        let manifest = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .cloned()
            .ok_or_else(|| NodeError::Channel("unknown file".into()))?;
        let chunks: Vec<Vec<u8>> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&file_conv)
                .into_iter()
                .filter(|e| e.kind == EventKind::Message)
                .map(|e| e.ciphertext.clone())
                .collect()
        };
        if chunks.len() as u32 != manifest.chunk_count {
            return Err(NodeError::Channel(format!(
                "file incomplete: {}/{} chunks",
                chunks.len(),
                manifest.chunk_count
            )));
        }
        let data = reassemble_and_verify(&manifest, &chunks)
            .map_err(|e| NodeError::Channel(format!("reassemble: {e}")))?;
        std::fs::write(dest, data).map_err(|e| NodeError::Channel(format!("write file: {e}")))?;
        Ok(())
    }
```
Add `use crate::file::reassemble_and_verify;` to the imports (or include it in the Step-1 `use crate::file::{...}` group). NOTE: chunk events in `file_conv` are ordered by the log's `(lamport, id)` sort; the sender is the sole author with monotonically increasing seq/lamport, so `events()` returns them in append (chunk) order — reassembly is correct. (If a reviewer worries about ordering, an explicit `sort_by_key(|e| e.seq)` before mapping is a safe belt-and-suspenders; the sole-author invariant makes it unnecessary.)

- [ ] **Step 3: drain per-file conversations from the post office**

In `drain_from_post_office`, after draining channels, also drain the per-file conversations the file book knows (so a recipient pulls chunks the PO holds). Add a `file_convs()` accessor to `FileBook`:
```rust
    /// The per-file conversation ids we have manifests for.
    pub fn file_convs(&self) -> Vec<crate::eventlog::event::ConversationId> {
        self.manifests.keys().copied().collect()
    }
```
And in `drain_from_post_office`, after the channel loop (reusing the dialed `channel`):
```rust
        let file_convs: Vec<ConversationId> = {
            let book = self.files.lock().expect("files mutex not poisoned");
            book.file_convs()
        };
        for fc in file_convs {
            if request_round(&mut channel, &self.log, fc).await.is_err() {
                return;
            }
        }
```
(No `process_*` after — chunks need no surfacing; `save_file` reads them on demand. The manifest itself arrived via the DM/channel conv drain + `process_file_events`, which the existing per-peer/channel drain already triggers — but `process_file_events` is only wired into `serve_connection`, not drain. ALSO add `self.process_file_events(conv)` after `emit_new_messages(conv)` in the per-peer DM drain loop and after `process_channel(cid)` in the channel drain loop, so a manifest pulled from the PO is surfaced.)

- [ ] **Step 4: the loopback-TCP file rig (append in `node.rs` `mod tests`)**
```rust
    #[tokio::test]
    async fn two_nodes_transfer_a_file_over_loopback_tcp() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let alice_pub = alice.public();
        let dir = tempfile::tempdir().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();
        // Bob knows Alice (to open her DM-sealed manifest); Alice knows Bob (to dial).
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
        let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_f, _a_f_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
        let (b_f, mut b_f_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(alice, alice_roster, a_dm, a_ch, a_f,
            &dir.path().join("a.log"), &dir.path().join("a-sent.log"), "pw").unwrap();
        let bob_node = Node::open(bob, bob_roster, b_dm, b_ch, b_f,
            &dir.path().join("b.log"), &dir.path().join("b-sent.log"), "pw").unwrap();
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        // A multi-chunk payload.
        let payload = vec![0xABu8; crate::file::CHUNK_SIZE + 1234];
        let src = dir.path().join("photo.bin");
        std::fs::write(&src, &payload).unwrap();

        let bob_uid = bob_node.user_id();
        let file_conv = alice_node.send_file_dm(&bob_uid, &src).await.unwrap();

        // Bob surfaces the received file.
        let rf = tokio::time::timeout(std::time::Duration::from_secs(5), b_f_r.recv())
            .await
            .expect("bob received a file within 5s")
            .expect("file stream open");
        assert_eq!(rf.name, "photo.bin");
        assert_eq!(rf.size, payload.len() as u64);
        assert_eq!(rf.file_conv, file_conv);
        assert_eq!(rf.from, alice_node.user_id());

        // Bob saves it and the bytes match.
        let dest = dir.path().join("saved.bin");
        bob_node.save_file(rf.file_conv, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
    }
```
(`seed_roster(peer, name, port, self_user_id)` is the existing helper. Bob's roster knows Alice at port `1` — never dialed, only used to resolve Alice's X25519 for opening the manifest.)

- [ ] **Step 5: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs
git commit -m "feat(node): send_file/save_file + distribution + drain; loopback file-transfer rig"
git status | head -1
```
Expected: the file rig + all node tests pass; clippy clean.

---

## Notes for the reviewer / next plan

- **Delivered:** live file sharing in the `Node` — `send_file_dm`/`send_file_channel` (chunk + seal into a per-file conversation, seal the manifest into the original conv, distribute both), `process_file_events` (open + surface received files), `save_file` (gather chunks → reassemble → verify → write). The loopback rig proves a real two-node multi-chunk transfer with end-to-end checksum verification.
- **Deferred to File Plan 3 (app+UI):** the runtime forwards the file stream to a `redesign-file-received` event; IPC `redesign_{send_file,list_files,save_file}`; a file card with picker-send + Save in `/redesign`.
- **Known MVP limitations (note in code, don't fix here):** (a) received-file manifests are in-memory — after a restart `save_file` can't find a manifest for a file received in a prior session (the chunks persist in the log; persisting opened manifests needs a durable sidecar — a later plan); (b) eager chunk distribution (chunks replicate to every member + the PO) — on-demand fetch is the Phase 2.x refinement; (c) `mime` is always `application/octet-stream` (the name carries the extension).
- **Reviewer checks:** no `MutexGuard` held across an `.await` in `send_file_*`/`save_file`/`distribute` (snapshot then drop); the DM-vs-channel manifest open in `process_file_events` picks the right crypto; chunk ordering on reassembly (sole-author seq monotonicity); the constructor migration covers every `Node::open` site; `save_file`'s incomplete-chunk guard; own-file events are skipped (sender not self-notified).
