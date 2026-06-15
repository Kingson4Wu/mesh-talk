# Event-Log Store Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the append-only, hash-linked, per-conversation event-log store — the tamper-evident, causally-ordered local data structure every DM and channel is made of — with durable encrypted-file persistence.

**Architecture:** A new `eventlog` module in four layers, each independently testable. (1) `event` defines the canonical content-addressed `Event` (id = hash of its content, Ed25519-signed, hash-linked to parent events, carrying a Lamport clock and a per-author sequence number). (2) `store::EventLog` is a pure in-memory index that validates and appends events (integrity, signature, causal completeness, equivocation), and answers queries (total order, heads, per-author version vector). (3) `persist::LogFile` is an encrypted append-only file (magic + salt header, then length-prefixed AES-256-GCM records, reusing the existing `storage::encryption`). (4) `persist::PersistentEventLog` composes the two: validate → persist → index, and rebuilds its index on open. This plan delivers the STORE only; the reconciliation/sync protocol (Plan 4), DM payload crypto (Plan 5), and full-text search (Phase 2) are out of scope.

**Tech Stack:** Rust; `sha2` (content hashing), `bincode`/`serde` (canonical serialization), the existing `storage::encryption` (PBKDF2 + AES-256-GCM at rest), and the existing `identity::DeviceIdentity` (Ed25519 sign/verify, X25519). No new dependencies.

---

## Background the implementer needs

**Why `eventlog` and not `log`.** The crate depends on the `log` crate (`log = "0.4"` in `Cargo.toml`). A crate-root module named `log` would shadow it inside this crate. The module is therefore named `eventlog`.

**Design spec being implemented (from `docs/superpowers/specs/2026-06-15-mesh-talk-redesign-design.md` §5):**
- A conversation is an append-only **log of events**; kinds are `Message, Edit, Delete, React, ReadMarker, MembershipChange, KeyRotation, FileManifest`.
- Event shape: `{ id = content-hash, conversation_id, author, seq (per-author), parents[] (hash links), lamport_ts, wall_clock, ciphertext, sig }`.
- Hash-linked → tamper-evident and causally ordered. Deterministic total order by `(lamport, id)`. Content-addressed → identical events merge idempotently (split-brain safe).
- Local storage is **encrypted at rest**; plaintext payloads live only inside the local encrypted store. (Here, the per-event `ciphertext` field is opaque to the store — it is the E2E-encrypted payload that Plan 5 produces. The store additionally encrypts the *whole record* at rest.)

**Identity API you build on (`src-tauri/src/identity/device.rs`, already implemented):**
```rust
pub struct PublicIdentity { pub ed25519_pub: [u8; 32], pub x25519_pub: [u8; 32] }
impl PublicIdentity { pub fn user_id_from(ed25519_pub: &[u8; 32]) -> String; }
impl DeviceIdentity {
    pub fn public(&self) -> PublicIdentity;
    pub fn sign(&self, message: &[u8]) -> [u8; 64];
    pub fn verify(ed25519_pub: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool; // associated fn
}
```

**At-rest crypto you reuse (`src-tauri/src/storage/encryption.rs`; see `identity/keystore.rs` for usage):**
```rust
pub const SALT_SIZE: usize;   // 16
pub const NONCE_SIZE: usize;  // 12
pub fn generate_salt() -> [u8; SALT_SIZE];
impl EncryptionKey { pub fn from_password(password: &str, salt: &[u8]) -> Result<EncryptionKey, StorageError>; }
pub fn encrypt_data(plaintext: &[u8], key: &EncryptionKey) -> Result<(Vec<u8> /*ciphertext*/, [u8; NONCE_SIZE] /*nonce*/), StorageError>;
pub fn decrypt_data(ciphertext: &[u8], nonce: &[u8], key: &EncryptionKey) -> Result<Vec<u8>, StorageError>;
```
> Confirm these exact signatures by reading `identity/keystore.rs` (it uses all of them). If `encrypt_data` returns the nonce as a `Vec<u8>` rather than `[u8; NONCE_SIZE]`, the code below still works (`extend_from_slice` and `&nonce` accept both) — just keep using the `NONCE_SIZE` constant for record framing.

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare full `cargo test`/`cargo build`. Always `nice` + throttle, scoped to the module:
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog:: -- --test-threads=2
nice -n 10 cargo fmt
nice -n 10 cargo clippy --lib -- -D warnings
```
The repo's pre-commit hook runs the full health check (fmt/clippy/test/build) on commit — let it run (may take minutes).

**Conventions:** match `src-tauri/src/identity/keystore.rs` and `src-tauri/src/transport/*` — hand-written error enums (NO `thiserror`), `Display` + `std::error::Error` + `From` impls, tests use `tempfile` (a dev-dependency) and `#[test]`/`#[tokio::test]`. rustfmt's `reorder_modules` sorts `pub mod` declarations — keep them alphabetical.

---

### Task 1: `eventlog` module skeleton + the `Event` type

**Files:**
- Create: `src-tauri/src/eventlog/mod.rs`
- Create: `src-tauri/src/eventlog/event.rs`
- Modify: `src-tauri/src/lib.rs` (module declaration)

- [ ] **Step 1: Register the module in `lib.rs`**

In `src-tauri/src/lib.rs`, add `pub mod eventlog;` in alphabetical position — **between** `pub mod error;` and `pub mod events;** (`eventlog` sorts before `events`):
```rust
pub mod error;
pub mod eventlog;
pub mod events;
```

- [ ] **Step 2: Create `eventlog/mod.rs` with `LogError` and the event re-exports**

Create `src-tauri/src/eventlog/mod.rs`:
```rust
//! Append-only, hash-linked, per-conversation event log — the tamper-evident
//! data structure every DM and channel is built from. See
//! `docs/superpowers/specs/2026-06-15-mesh-talk-redesign-design.md` §5.
//!
//! Layers: [`event`] (the content-addressed `Event`), `store` (the in-memory
//! validating index), and `persist` (encrypted append-only file + the durable
//! composition). `store`/`persist` are added in later tasks.

pub mod event;

pub use event::{Author, ConversationId, Event, EventId, EventKind};

/// Errors from the event log.
#[derive(Debug)]
pub enum LogError {
    /// `event.id` does not equal the hash of its content (tampered or corrupt).
    CorruptId,
    /// The author's Ed25519 signature over the event id did not verify.
    BadSignature,
    /// One or more `parents` are not present in the log yet (causally incomplete).
    MissingParents(Vec<EventId>),
    /// The author already has a *different* event at this `seq` (equivocation).
    AuthorEquivocation { author: Author, seq: u64 },
    /// At-rest crypto failure (key derivation / encryption).
    Storage(crate::storage::errors::StorageError),
    /// (De)serialization of an event failed.
    Serialization(String),
    /// I/O failure on the log file.
    Io(std::io::Error),
    /// The on-disk log file is malformed (bad header or undecryptable record).
    CorruptFile(String),
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogError::CorruptId => write!(f, "event id does not match its content"),
            LogError::BadSignature => write!(f, "event signature did not verify"),
            LogError::MissingParents(p) => write!(f, "missing {} parent event(s)", p.len()),
            LogError::AuthorEquivocation { seq, .. } => {
                write!(f, "author equivocation at seq {seq}")
            }
            LogError::Storage(e) => write!(f, "storage error: {e}"),
            LogError::Serialization(m) => write!(f, "serialization error: {m}"),
            LogError::Io(e) => write!(f, "io error: {e}"),
            LogError::CorruptFile(m) => write!(f, "corrupt log file: {m}"),
        }
    }
}

impl std::error::Error for LogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LogError::Storage(e) => Some(e),
            LogError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<crate::storage::errors::StorageError> for LogError {
    fn from(e: crate::storage::errors::StorageError) -> Self {
        LogError::Storage(e)
    }
}

impl From<std::io::Error> for LogError {
    fn from(e: std::io::Error) -> Self {
        LogError::Io(e)
    }
}
```
> If `crate::storage::errors::StorageError` does not implement `std::error::Error`, drop the `Some(e)` arm for `Storage` in `source()` (return `None`) — read `src-tauri/src/storage/errors.rs` to confirm. Everything else is unaffected.

- [ ] **Step 3: Create `eventlog/event.rs` with the `Event` type and its tests**

Create `src-tauri/src/eventlog/event.rs`:
```rust
//! The canonical, content-addressed event. `id` is the SHA-256 hash of the
//! event's content (everything except `id` and `sig`); `sig` is the author's
//! Ed25519 signature over `id`. Hash-linked via `parents`, carrying a Lamport
//! clock and a per-author sequence number.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Domain separator for event-content hashing.
const EVENT_DOMAIN: &[u8] = b"mesh-talk-event-v1";

/// 32-byte content hash identifying an event.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId([u8; 32]);

impl EventId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Debug for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EventId({}…)", &self.to_hex()[..8])
    }
}

/// 32-byte opaque conversation identifier (a DM pair or a channel).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ConversationId([u8; 32]);

impl ConversationId {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An event author, identified by their Ed25519 public key (self-certifying).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Author([u8; 32]);

impl Author {
    pub fn from_ed25519(ed25519_pub: [u8; 32]) -> Self {
        Self(ed25519_pub)
    }
    pub fn ed25519_pub(&self) -> &[u8; 32] {
        &self.0
    }
    /// The stable user-id fingerprint of this author (hex, 32 chars).
    pub fn user_id(&self) -> String {
        PublicIdentity::user_id_from(&self.0)
    }
}

/// The kind of an event. The payload itself lives (encrypted) in `ciphertext`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum EventKind {
    Message,
    Edit,
    Delete,
    React,
    ReadMarker,
    MembershipChange,
    KeyRotation,
    FileManifest,
}

/// A single, content-addressed, signed log event.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub conversation_id: ConversationId,
    pub author: Author,
    pub seq: u64,
    pub parents: Vec<EventId>,
    pub lamport: u64,
    pub wall_clock: u64,
    pub kind: EventKind,
    pub ciphertext: Vec<u8>,
    pub sig: Vec<u8>,
}

/// The hashed content of an event (everything that fixes its identity). Held by
/// reference so we never copy the ciphertext to compute the id.
#[derive(Serialize)]
struct EventContent<'a> {
    conversation_id: &'a ConversationId,
    author: &'a Author,
    seq: u64,
    parents: &'a [EventId],
    lamport: u64,
    wall_clock: u64,
    kind: EventKind,
    ciphertext: &'a [u8],
}

fn hash_content(content: &EventContent) -> EventId {
    let bytes = bincode::serialize(content).expect("event content is serializable");
    let mut hasher = Sha256::new();
    hasher.update(EVENT_DOMAIN);
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut id = [0u8; 32];
    id.copy_from_slice(&digest);
    EventId(id)
}

impl Event {
    /// Build a fresh event: canonicalize parents, compute the content-hash id,
    /// and sign that id with the author's Ed25519 key.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: &DeviceIdentity,
        conversation_id: ConversationId,
        seq: u64,
        mut parents: Vec<EventId>,
        lamport: u64,
        wall_clock: u64,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) -> Self {
        // Canonical parent order so the same logical event always hashes equal.
        parents.sort();
        parents.dedup();
        let author = Author::from_ed25519(identity.public().ed25519_pub);
        let id = hash_content(&EventContent {
            conversation_id: &conversation_id,
            author: &author,
            seq,
            parents: &parents,
            lamport,
            wall_clock,
            kind,
            ciphertext: &ciphertext,
        });
        let sig = identity.sign(id.as_bytes()).to_vec();
        Event {
            id,
            conversation_id,
            author,
            seq,
            parents,
            lamport,
            wall_clock,
            kind,
            ciphertext,
            sig,
        }
    }

    /// Recompute the content-hash id from the event's own fields.
    pub fn recompute_id(&self) -> EventId {
        hash_content(&EventContent {
            conversation_id: &self.conversation_id,
            author: &self.author,
            seq: self.seq,
            parents: &self.parents,
            lamport: self.lamport,
            wall_clock: self.wall_clock,
            kind: self.kind,
            ciphertext: &self.ciphertext,
        })
    }

    /// True if `id` matches the hash of the content (tamper-evidence).
    pub fn verify_integrity(&self) -> bool {
        self.id == self.recompute_id()
    }

    /// True if `sig` is a valid Ed25519 signature over `id` by `author`.
    pub fn verify_signature(&self) -> bool {
        let Ok(sig): Result<[u8; 64], _> = self.sig.as_slice().try_into() else {
            return false;
        };
        DeviceIdentity::verify(self.author.ed25519_pub(), self.id.as_bytes(), &sig)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    #[test]
    fn id_is_deterministic_for_identical_content() {
        let id = DeviceIdentity::generate();
        let a = Event::new(&id, conv(), 1, vec![], 1, 100, EventKind::Message, b"hi".to_vec());
        let b = Event::new(&id, conv(), 1, vec![], 1, 100, EventKind::Message, b"hi".to_vec());
        // Same content (and Ed25519 is deterministic) → identical events.
        assert_eq!(a.id, b.id);
        assert_eq!(a, b);
    }

    #[test]
    fn id_changes_when_content_changes() {
        let id = DeviceIdentity::generate();
        let a = Event::new(&id, conv(), 1, vec![], 1, 100, EventKind::Message, b"hi".to_vec());
        let b = Event::new(&id, conv(), 1, vec![], 1, 100, EventKind::Message, b"bye".to_vec());
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn parents_are_order_independent() {
        let id = DeviceIdentity::generate();
        let p1 = EventId([7u8; 32]);
        let p2 = EventId([9u8; 32]);
        let a = Event::new(&id, conv(), 2, vec![p1, p2], 2, 0, EventKind::Message, b"x".to_vec());
        let b = Event::new(&id, conv(), 2, vec![p2, p1], 2, 0, EventKind::Message, b"x".to_vec());
        assert_eq!(a.id, b.id);
        assert_eq!(a.parents, b.parents); // both sorted to the same order
    }

    #[test]
    fn signature_round_trips_and_rejects_tampering() {
        let id = DeviceIdentity::generate();
        let e = Event::new(&id, conv(), 1, vec![], 1, 0, EventKind::Message, b"hi".to_vec());
        assert!(e.verify_signature());

        let mut tampered = e.clone();
        tampered.sig[0] ^= 0xFF;
        assert!(!tampered.verify_signature());
    }

    #[test]
    fn integrity_detects_content_mutation() {
        let id = DeviceIdentity::generate();
        let e = Event::new(&id, conv(), 1, vec![], 1, 0, EventKind::Message, b"hi".to_vec());
        assert!(e.verify_integrity());

        let mut mutated = e.clone();
        mutated.ciphertext.push(0); // change content without recomputing id
        assert!(!mutated.verify_integrity());
    }

    #[test]
    fn author_user_id_matches_identity() {
        let id = DeviceIdentity::generate();
        let e = Event::new(&id, conv(), 1, vec![], 1, 0, EventKind::Message, b"hi".to_vec());
        assert_eq!(e.author.user_id(), PublicIdentity::user_id_from(&id.public().ed25519_pub));
    }
}
```

- [ ] **Step 4: Run the tests**

Run:
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::event -- --test-threads=2
```
Expected: PASS — 6 tests.

- [ ] **Step 5: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/lib.rs src-tauri/src/eventlog/mod.rs src-tauri/src/eventlog/event.rs
git commit -m "feat(eventlog): content-addressed signed Event type

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: `EventLog` in-memory store — validate & append

**Files:**
- Create: `src-tauri/src/eventlog/store.rs`
- Modify: `src-tauri/src/eventlog/mod.rs` (add module + re-exports)

- [ ] **Step 1: Add the module declaration and re-exports to `mod.rs`**

In `src-tauri/src/eventlog/mod.rs`, add `pub mod store;` (sorted: after `pub mod event;`) and the re-export:
```rust
pub mod event;
pub mod store;

pub use event::{Author, ConversationId, Event, EventId, EventKind};
pub use store::{AppendOutcome, EventLog};
```

- [ ] **Step 2: Create `store.rs` with the index, `validate`, `index_trusted`, `append`, `has`, `get`, and tests**

Create `src-tauri/src/eventlog/store.rs`:
```rust
//! In-memory, append-only event index for all conversations. Validates events
//! (integrity, signature, causal completeness, equivocation) and answers
//! ordering/heads/version queries. Pure data structure — no I/O.

use crate::eventlog::event::{Author, ConversationId, Event, EventId};
use crate::eventlog::LogError;
use std::collections::{HashMap, HashSet};

/// Result of an append: a genuinely new event, or an already-present duplicate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppendOutcome {
    Appended,
    Duplicate,
}

/// The in-memory event log across all conversations.
#[derive(Default)]
pub struct EventLog {
    /// Every known event, by id.
    events: HashMap<EventId, Event>,
    /// Event ids grouped by conversation.
    by_conversation: HashMap<ConversationId, HashSet<EventId>>,
    /// Per-conversation frontier: events not (yet) referenced as a parent.
    heads: HashMap<ConversationId, HashSet<EventId>>,
    /// Per-conversation version vector: author → highest seq seen.
    version: HashMap<ConversationId, HashMap<Author, u64>>,
    /// Per-conversation highest Lamport timestamp seen.
    max_lamport: HashMap<ConversationId, u64>,
    /// (conversation, author, seq) → id, for equivocation detection.
    author_seq: HashMap<(ConversationId, Author, u64), EventId>,
}

impl EventLog {
    /// Validate an event for appending.
    ///
    /// Returns `Ok(true)` if it is new and valid (ready to index), `Ok(false)`
    /// if it is already present (a content-addressed duplicate — safe to skip),
    /// or `Err` if it is corrupt, unsigned, causally incomplete, or an
    /// equivocation. Pure — does not mutate the log.
    pub fn validate(&self, event: &Event) -> Result<bool, LogError> {
        if !event.verify_integrity() {
            return Err(LogError::CorruptId);
        }
        if !event.verify_signature() {
            return Err(LogError::BadSignature);
        }
        if self.events.contains_key(&event.id) {
            return Ok(false);
        }
        let conv = event.conversation_id;
        // Every parent must already be present, in the same conversation.
        let missing: Vec<EventId> = event
            .parents
            .iter()
            .filter(|p| self.events.get(*p).map(|e| e.conversation_id) != Some(conv))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(LogError::MissingParents(missing));
        }
        // The author must not already have a different event at this seq.
        if let Some(existing) = self.author_seq.get(&(conv, event.author, event.seq)) {
            if *existing != event.id {
                return Err(LogError::AuthorEquivocation {
                    author: event.author,
                    seq: event.seq,
                });
            }
        }
        Ok(true)
    }

    /// Insert an event that is already known to be valid (validated here, or
    /// authenticated at rest on reload). Must be called in causal order:
    /// every parent must already be indexed.
    pub(crate) fn index_trusted(&mut self, event: Event) {
        let conv = event.conversation_id;
        let id = event.id;

        let heads = self.heads.entry(conv).or_default();
        for p in &event.parents {
            heads.remove(p);
        }
        heads.insert(id);

        let vv = self.version.entry(conv).or_default();
        let slot = vv.entry(event.author).or_insert(0);
        if event.seq > *slot {
            *slot = event.seq;
        }

        let ml = self.max_lamport.entry(conv).or_insert(0);
        if event.lamport > *ml {
            *ml = event.lamport;
        }

        self.author_seq.insert((conv, event.author, event.seq), id);
        self.by_conversation.entry(conv).or_default().insert(id);
        self.events.insert(id, event);
    }

    /// Validate and append an event.
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.validate(&event)? {
            return Ok(AppendOutcome::Duplicate);
        }
        self.index_trusted(event);
        Ok(AppendOutcome::Appended)
    }

    pub fn has(&self, id: &EventId) -> bool {
        self.events.contains_key(id)
    }

    pub fn get(&self, id: &EventId) -> Option<&Event> {
        self.events.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::EventKind;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    fn mk(
        id: &DeviceIdentity,
        seq: u64,
        parents: Vec<EventId>,
        lamport: u64,
        payload: &[u8],
    ) -> Event {
        Event::new(id, conv(), seq, parents, lamport, 0, EventKind::Message, payload.to_vec())
    }

    #[test]
    fn appends_a_root_then_a_child() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();

        let root = mk(&id, 1, vec![], 1, b"root");
        assert_eq!(log.append(root.clone()).unwrap(), AppendOutcome::Appended);
        assert!(log.has(&root.id));

        let child = mk(&id, 2, vec![root.id], 2, b"child");
        assert_eq!(log.append(child.clone()).unwrap(), AppendOutcome::Appended);
        assert!(log.has(&child.id));
    }

    #[test]
    fn rejects_event_with_missing_parent() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let orphan = mk(&id, 1, vec![EventId([42u8; 32])], 1, b"orphan");
        assert!(matches!(log.append(orphan), Err(LogError::MissingParents(_))));
    }

    #[test]
    fn duplicate_append_is_idempotent() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let e = mk(&id, 1, vec![], 1, b"hi");
        assert_eq!(log.append(e.clone()).unwrap(), AppendOutcome::Appended);
        assert_eq!(log.append(e.clone()).unwrap(), AppendOutcome::Duplicate);
        // Still only one event present.
        assert!(log.has(&e.id));
    }

    #[test]
    fn rejects_bad_signature() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let mut e = mk(&id, 1, vec![], 1, b"hi");
        e.sig[0] ^= 0xFF;
        assert!(matches!(log.append(e), Err(LogError::BadSignature)));
    }

    #[test]
    fn rejects_corrupt_id() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let mut e = mk(&id, 1, vec![], 1, b"hi");
        e.ciphertext.push(0); // mutate content without recomputing id
        assert!(matches!(log.append(e), Err(LogError::CorruptId)));
    }

    #[test]
    fn rejects_author_equivocation() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        // Two different roots from the same author at the same seq.
        let a = mk(&id, 1, vec![], 1, b"one");
        let b = mk(&id, 1, vec![], 1, b"two");
        assert_eq!(log.append(a).unwrap(), AppendOutcome::Appended);
        assert!(matches!(
            log.append(b),
            Err(LogError::AuthorEquivocation { seq: 1, .. })
        ));
    }
}
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::store -- --test-threads=2
```
Expected: PASS — 6 tests.

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/mod.rs src-tauri/src/eventlog/store.rs
git commit -m "feat(eventlog): validating in-memory EventLog append

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `EventLog` queries — total order, heads, version vector, prepare

**Files:**
- Modify: `src-tauri/src/eventlog/store.rs` (add query methods + tests)

- [ ] **Step 1: Add the query methods to the `impl EventLog` block**

In `src-tauri/src/eventlog/store.rs`, add these methods inside `impl EventLog` (after `get`):
```rust
    /// All events in a conversation, in deterministic total order `(lamport, id)`.
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event> {
        let mut events: Vec<&Event> = match self.by_conversation.get(conversation) {
            Some(ids) => ids.iter().filter_map(|id| self.events.get(id)).collect(),
            None => Vec::new(),
        };
        events.sort_by(|a, b| a.lamport.cmp(&b.lamport).then_with(|| a.id.cmp(&b.id)));
        events
    }

    /// The current frontier of a conversation: events not referenced as a
    /// parent by any other event. Sorted for determinism.
    pub fn heads(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut heads: Vec<EventId> = self
            .heads
            .get(conversation)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        heads.sort();
        heads
    }

    /// Per-author highest sequence number seen in a conversation (for sync).
    pub fn version_vector(&self, conversation: &ConversationId) -> HashMap<Author, u64> {
        self.version.get(conversation).cloned().unwrap_or_default()
    }

    /// The `(parents, lamport)` a new local event should use: the current heads
    /// and the next Lamport timestamp (one past the highest seen).
    pub fn prepare(&self, conversation: &ConversationId) -> (Vec<EventId>, u64) {
        let parents = self.heads(conversation);
        let lamport = self.max_lamport.get(conversation).copied().unwrap_or(0) + 1;
        (parents, lamport)
    }
```

- [ ] **Step 2: Add the query tests to the test module**

In the `#[cfg(test)] mod tests` block in `store.rs`, add these tests:
```rust
    #[test]
    fn events_are_returned_in_total_order() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = mk(&id, 1, vec![], 1, b"a");
        let mid = mk(&id, 2, vec![root.id], 2, b"b");
        let tip = mk(&id, 3, vec![mid.id], 3, b"c");
        // Append out of lamport order; query must still sort.
        log.append(root.clone()).unwrap();
        log.append(mid.clone()).unwrap();
        log.append(tip.clone()).unwrap();

        let ordered = log.events(&conv());
        let lamports: Vec<u64> = ordered.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, vec![1, 2, 3]);
        assert_eq!(ordered[0].id, root.id);
        assert_eq!(ordered[2].id, tip.id);
    }

    #[test]
    fn heads_track_the_frontier() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = mk(&id, 1, vec![], 1, b"root");
        log.append(root.clone()).unwrap();
        assert_eq!(log.heads(&conv()), vec![root.id]);

        // A child supersedes the root as the head.
        let child = mk(&id, 2, vec![root.id], 2, b"child");
        log.append(child.clone()).unwrap();
        assert_eq!(log.heads(&conv()), vec![child.id]);
    }

    #[test]
    fn version_vector_tracks_max_seq_per_author() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let a1 = mk(&alice, 1, vec![], 1, b"a1");
        log.append(a1.clone()).unwrap();
        let b1 = Event::new(&bob, conv(), 1, vec![a1.id], 2, 0, EventKind::Message, b"b1".to_vec());
        log.append(b1.clone()).unwrap();
        let a2 = mk(&alice, 2, vec![b1.id], 3, b"a2");
        log.append(a2).unwrap();

        let vv = log.version_vector(&conv());
        assert_eq!(vv.get(&a1.author), Some(&2));
        assert_eq!(vv.get(&b1.author), Some(&1));
    }

    #[test]
    fn prepare_returns_heads_and_next_lamport() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        // Empty conversation: no parents, lamport starts at 1.
        assert_eq!(log.prepare(&conv()), (vec![], 1));

        let root = mk(&id, 1, vec![], 5, b"root");
        log.append(root.clone()).unwrap();
        let (parents, lamport) = log.prepare(&conv());
        assert_eq!(parents, vec![root.id]);
        assert_eq!(lamport, 6); // one past the highest lamport (5)
    }
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::store -- --test-threads=2
```
Expected: PASS — 10 tests (6 from Task 2 + 4 new).

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/store.rs
git commit -m "feat(eventlog): ordering, heads, version-vector queries

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: `LogFile` — encrypted append-only file

**Files:**
- Create: `src-tauri/src/eventlog/persist.rs`
- Modify: `src-tauri/src/eventlog/mod.rs` (add module)

- [ ] **Step 1: Add the module declaration to `mod.rs`**

In `src-tauri/src/eventlog/mod.rs`, add `pub mod persist;` (sorted: `event`, `persist`, `store`) and re-export the durable log (defined in Task 5; declaring the module now is fine because Task 4 defines `LogFile` in the file):
```rust
pub mod event;
pub mod persist;
pub mod store;

pub use event::{Author, ConversationId, Event, EventId, EventKind};
pub use store::{AppendOutcome, EventLog};
```
(The `PersistentEventLog` re-export is added in Task 5.)

- [ ] **Step 2: Create `persist.rs` with `LogFile` and its tests**

Create `src-tauri/src/eventlog/persist.rs`:
```rust
//! Encrypted, append-only on-disk log. Header: a 6-byte magic + a 16-byte salt.
//! Body: a sequence of length-prefixed AES-256-GCM records, one per event:
//! `[u32 be record_len][nonce(12)][ciphertext]`, where `ciphertext` is the
//! AES-GCM encryption of `bincode(Event)`. Reuses `storage::encryption`.

use crate::eventlog::event::Event;
use crate::eventlog::LogError;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTLOG1";

/// An append-only encrypted log file plus the derived key for writing records.
pub struct LogFile {
    file: File,
    key: EncryptionKey,
}

impl LogFile {
    /// Open the log at `path`, creating it (with a fresh random salt) if absent.
    /// Returns the writer plus every event already stored, in file order.
    pub fn open(path: &Path, password: &str) -> Result<(Self, Vec<Event>), LogError> {
        if path.exists() {
            Self::load(path, password)
        } else {
            Self::create(path, password)
        }
    }

    fn create(path: &Path, password: &str) -> Result<(Self, Vec<Event>), LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let salt = generate_salt();
        let key = EncryptionKey::from_password(password, &salt)?;
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        file.write_all(MAGIC)?;
        file.write_all(&salt)?;
        file.flush()?;
        Ok((Self { file, key }, Vec::new()))
    }

    fn load(path: &Path, password: &str) -> Result<(Self, Vec<Event>), LogError> {
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
        let events = Self::parse_records(&rest, &key)?;
        Ok((Self { file, key }, events))
    }

    /// Parse the record region. A torn trailing record (crash mid-write) is
    /// dropped; a record that fails to decrypt (tampering) is an error.
    fn parse_records(mut data: &[u8], key: &EncryptionKey) -> Result<Vec<Event>, LogError> {
        let mut events = Vec::new();
        loop {
            if data.len() < 4 {
                break; // clean end, or a torn length prefix
            }
            let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            data = &data[4..];
            if data.len() < len {
                break; // torn trailing record — drop it
            }
            let record = &data[..len];
            data = &data[len..];

            if record.len() < NONCE_SIZE {
                return Err(LogError::CorruptFile("record shorter than nonce".into()));
            }
            let nonce: [u8; NONCE_SIZE] =
                record[..NONCE_SIZE].try_into().expect("nonce length checked");
            let ciphertext = &record[NONCE_SIZE..];
            let plaintext = decrypt_data(ciphertext, &nonce, key)
                .map_err(|_| LogError::CorruptFile("record failed to decrypt".into()))?;
            let event: Event = bincode::deserialize(&plaintext)
                .map_err(|e| LogError::CorruptFile(format!("record decode: {e}")))?;
            events.push(event);
        }
        Ok(events)
    }

    /// Append one event as an encrypted, length-prefixed record.
    pub fn append_record(&mut self, event: &Event) -> Result<(), LogError> {
        let plaintext =
            bincode::serialize(event).map_err(|e| LogError::Serialization(e.to_string()))?;
        let (ciphertext, nonce) = encrypt_data(&plaintext, &self.key)?;
        let record_len = (NONCE_SIZE + ciphertext.len()) as u32;

        let mut buf = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        buf.extend_from_slice(&record_len.to_be_bytes());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        self.file.write_all(&buf)?;
        self.file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{ConversationId, EventKind};
    use crate::identity::device::DeviceIdentity;

    fn mk(id: &DeviceIdentity, seq: u64, lamport: u64, payload: &[u8]) -> Event {
        Event::new(
            id,
            ConversationId::new([1u8; 32]),
            seq,
            vec![],
            lamport,
            0,
            EventKind::Message,
            payload.to_vec(),
        )
    }

    #[test]
    fn append_then_reopen_returns_records_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        let e1 = mk(&id, 1, 1, b"one");
        let e2 = mk(&id, 2, 2, b"two");

        {
            let (mut f, existing) = LogFile::open(&path, "pw").unwrap();
            assert!(existing.is_empty());
            f.append_record(&e1).unwrap();
            f.append_record(&e2).unwrap();
        }

        let (_f, events) = LogFile::open(&path, "pw").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, e1.id);
        assert_eq!(events[1].id, e2.id);
    }

    #[test]
    fn wrong_password_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        {
            let (mut f, _) = LogFile::open(&path, "right").unwrap();
            f.append_record(&mk(&id, 1, 1, b"secret")).unwrap();
        }
        assert!(matches!(
            LogFile::open(&path, "wrong"),
            Err(LogError::CorruptFile(_))
        ));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        std::fs::write(&path, b"not a mesh-talk log file at all").unwrap();
        assert!(matches!(
            LogFile::open(&path, "pw"),
            Err(LogError::CorruptFile(_))
        ));
    }

    #[test]
    fn torn_trailing_record_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        {
            let (mut f, _) = LogFile::open(&path, "pw").unwrap();
            f.append_record(&mk(&id, 1, 1, b"good")).unwrap();
        }
        // Simulate a crash mid-write: a length prefix claiming more bytes than follow.
        {
            use std::io::Write;
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&100u32.to_be_bytes()).unwrap(); // claims 100 bytes
            file.write_all(&[0u8; 10]).unwrap(); // only 10 present
        }
        let (_f, events) = LogFile::open(&path, "pw").unwrap();
        assert_eq!(events.len(), 1); // the torn record is dropped, the good one survives
    }

    #[test]
    fn tampered_record_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        {
            let (mut f, _) = LogFile::open(&path, "pw").unwrap();
            f.append_record(&mk(&id, 1, 1, b"data")).unwrap();
        }
        // Flip the last byte (inside the AEAD tag region of the only record).
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        assert!(matches!(
            LogFile::open(&path, "pw"),
            Err(LogError::CorruptFile(_))
        ));
    }
}
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::persist -- --test-threads=2
```
Expected: PASS — 5 tests.

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/mod.rs src-tauri/src/eventlog/persist.rs
git commit -m "feat(eventlog): encrypted append-only LogFile

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: `PersistentEventLog` — durable composition

**Files:**
- Modify: `src-tauri/src/eventlog/persist.rs` (add `PersistentEventLog` + tests)
- Modify: `src-tauri/src/eventlog/mod.rs` (re-export `PersistentEventLog`)

- [ ] **Step 1: Re-export `PersistentEventLog` from `mod.rs`**

In `src-tauri/src/eventlog/mod.rs`, extend the persist re-export line:
```rust
pub use persist::PersistentEventLog;
```
(Add it after the `pub use store::{...};` line.)

- [ ] **Step 2: Add `PersistentEventLog` to `persist.rs`**

In `src-tauri/src/eventlog/persist.rs`, add these imports to the top `use` block:
```rust
use crate::eventlog::event::{Author, ConversationId, EventId};
use crate::eventlog::store::{AppendOutcome, EventLog};
use std::collections::HashMap;
```
Then add this `struct` and `impl` ABOVE the `#[cfg(test)] mod tests` block:
```rust
/// A durable event log: an in-memory [`EventLog`] backed by an encrypted,
/// append-only [`LogFile`]. On append it validates, persists, then indexes —
/// so a write failure never leaves an unpersisted event in memory.
pub struct PersistentEventLog {
    log: EventLog,
    file: LogFile,
}

impl PersistentEventLog {
    /// Open (or create) the log at `path`, replaying any stored events.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, events) = LogFile::open(path, password)?;
        let mut log = EventLog::default();
        // Records were validated when first appended and are AES-GCM
        // authenticated at rest, so replay them as trusted, in file (causal) order.
        for event in events {
            log.index_trusted(event);
        }
        Ok(Self { log, file })
    }

    /// Validate, persist, then index an event.
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.log.validate(&event)? {
            return Ok(AppendOutcome::Duplicate);
        }
        self.file.append_record(&event)?;
        self.log.index_trusted(event);
        Ok(AppendOutcome::Appended)
    }

    pub fn has(&self, id: &EventId) -> bool {
        self.log.has(id)
    }
    pub fn get(&self, id: &EventId) -> Option<&Event> {
        self.log.get(id)
    }
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event> {
        self.log.events(conversation)
    }
    pub fn heads(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.log.heads(conversation)
    }
    pub fn version_vector(&self, conversation: &ConversationId) -> HashMap<Author, u64> {
        self.log.version_vector(conversation)
    }
    pub fn prepare(&self, conversation: &ConversationId) -> (Vec<EventId>, u64) {
        self.log.prepare(conversation)
    }
}
```

- [ ] **Step 3: Add `PersistentEventLog` tests to the test module**

In the `#[cfg(test)] mod tests` block in `persist.rs`, add these tests:
```rust
    #[test]
    fn durable_log_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();

        let root = mk(&id, 1, 1, b"root");
        let child = Event::new(
            &id,
            conv,
            2,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            plog.append(root.clone()).unwrap();
            plog.append(child.clone()).unwrap();
            assert_eq!(plog.heads(&conv), vec![child.id]);
        }

        // Reopen: events, heads, and version vector are rebuilt from disk.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        let events = plog.events(&conv);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, root.id);
        assert_eq!(events[1].id, child.id);
        assert_eq!(plog.heads(&conv), vec![child.id]);
        assert_eq!(plog.version_vector(&conv).get(&root.author), Some(&2));
    }

    #[test]
    fn invalid_event_is_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();

        let mut bad = mk(&id, 1, 1, b"bad");
        bad.sig[0] ^= 0xFF; // break the signature
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            assert!(matches!(plog.append(bad), Err(LogError::BadSignature)));
        }
        // Reopen: nothing was written.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.events(&ConversationId::new([1u8; 32])).len(), 0);
    }

    #[test]
    fn duplicate_append_writes_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();
        let e = mk(&id, 1, 1, b"hi");
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            assert_eq!(plog.append(e.clone()).unwrap(), AppendOutcome::Appended);
            assert_eq!(plog.append(e.clone()).unwrap(), AppendOutcome::Duplicate);
        }
        // Reopen: the duplicate was not written a second time.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.events(&conv).len(), 1);
    }
```

- [ ] **Step 4: Run the whole module test suite**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog:: -- --test-threads=2
```
Expected: PASS — all `eventlog` tests (6 event + 10 store + 5 LogFile + 3 PersistentEventLog ≈ 24).

- [ ] **Step 5: Full fmt + clippy gate**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt --check; echo "fmt EXIT: $?"
```
Expected: clippy clean; `fmt EXIT: 0`.

- [ ] **Step 6: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/mod.rs src-tauri/src/eventlog/persist.rs
git commit -m "feat(eventlog): durable PersistentEventLog (validate, persist, index)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next phase

- **What this delivers:** the event-log store primitive — a content-addressed, signed, hash-linked, causally-ordered, equivocation-detecting event log with deterministic total order, heads/version-vector queries, and durable encrypted-at-rest persistence that survives reopen.
- **Deliberately deferred (do NOT add here):** the sync/reconciliation protocol (version-vector exchange + missing-event fetch — Plan 4, which consumes `version_vector`/`heads`/`get`); DM/channel payload encryption that fills `ciphertext` (Plan 5); the decrypted "messages" rendering view and full-text search (Phase 2); wiring into `commands.rs`/`node_service`; and `fsync`/group-commit durability tuning.
- **Security properties:** every event is tamper-evident (`id` = content hash, checked on append) and authenticated (`sig` over `id`, checked on append); content-addressing makes merges idempotent and split-brain-safe; the whole log is encrypted at rest with the existing PBKDF2+AES-256-GCM, and each record is individually AEAD-authenticated (tampering on disk fails to load).
- **Accepted v1 limitations:** the in-memory index holds all events (fine for an office-scale LAN log; a windowed/paged store is a later optimization); `append` requires parents to be present (the sync engine is responsible for delivering events in causal order — orphan buffering lives there, not here); `flush` (not `fsync`) per record — a power cut can lose the last unacknowledged record, which the torn-record reader tolerates.
