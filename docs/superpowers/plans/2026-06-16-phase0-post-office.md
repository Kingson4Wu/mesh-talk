# Minimal Post Office Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A minimal "post office" — an elected always-on peer that stores-and-forwards encrypted events so a 1:1 DM reaches its recipient even when sender and recipient are never online at the same time.

**Architecture:** The post office is a peer with two extra hats — **full replica** (a `PersistentEventLog`, Plan 3) and **store-and-forward** (it participates in reconciliation as a `SyncStore`, Plan 4). For DMs it is a *dumb relay*: it holds recipient-sealed envelopes (Plan 5) it can never decrypt — it never holds any conversation key. A small deterministic **election** (lowest identity-fingerprint among the eligible/always-on peers) lets every peer independently agree on who the post office is. Full leader election with standbys and uptime detection is Phase 1; this is the minimal Phase-0 version that delivers offline messages.

**Tech Stack:** Rust; the existing `eventlog` (store + sync), `dm` (sealed envelopes), and `identity` modules. No new dependencies.

---

## Background the implementer needs

**Why the post office is mostly assembly.** Everything it does is already built:
- A regular peer's event store (`eventlog::PersistentEventLog`, Plan 3) holds validly-signed, content-addressed events for a conversation.
- Reconciliation (`eventlog::sync::reconcile`, Plan 4) converges any two `SyncStore`s for a conversation. Plan 4's notes state: "the post office is just another `SyncStore`."
- A DM payload is a recipient-sealed envelope (`dm::seal`/`open`, Plan 5) carried in an event's `ciphertext`; the post office holds the opaque bytes and never has the key.

So the offline-delivery mechanism is: the sender reconciles with the post office (it now holds the event); later the recipient reconciles with the post office (it serves the event). The genuinely-new code in this plan is the **election** and a thin **`PostOffice` role type**; the load-bearing deliverable is the **integration test** proving the end-to-end offline flow and the post office's blindness to content.

**APIs you build on (already implemented):**
```rust
// identity/device.rs
pub struct DeviceIdentity; // generate(), from_secret_bytes([u8;32],[u8;32]), secret_bytes()->([u8;32],[u8;32]), public()
pub struct PublicIdentity { pub ed25519_pub: [u8;32], pub x25519_pub: [u8;32] } // Clone, PartialEq, Eq
impl PublicIdentity { pub fn user_id(&self) -> String; } // hex fingerprint, the basis for election

// eventlog
pub struct ConversationId([u8;32]); // ConversationId::new([u8;32]); Copy
pub struct EventId([u8;32]);          // Copy, Ord
pub struct Event { pub id: EventId, pub ciphertext: Vec<u8>, /* ... */ }
pub enum EventKind { Message, /* ... */ }
impl Event { pub fn new(identity, conversation_id, seq, parents, lamport, wall_clock, kind, ciphertext) -> Event; }
pub struct EventLog;            // Default; append, events, has, get
pub enum AppendOutcome { Appended, Duplicate }
pub enum LogError { /* ... */ }
impl PersistentEventLog {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError>;
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError>; // validate -> persist -> index
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event>;
    pub fn has(&self, id: &EventId) -> bool;
}
pub trait SyncStore { // implemented by EventLog AND PersistentEventLog
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId>;
    fn events_excluding(&self, conversation: &ConversationId, have: &HashSet<EventId>) -> Vec<Event>;
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError>;
}
pub fn reconcile<A: SyncStore, B: SyncStore>(a: &mut A, b: &mut B, conversation: ConversationId) -> (ApplyReport, ApplyReport);

// dm.rs
pub fn seal(sender: &DeviceIdentity, recipient_x25519_pub: &[u8;32], plaintext: &[u8]) -> Result<Vec<u8>, DmError>;
pub fn open(recipient: &DeviceIdentity, sender_x25519_pub: &[u8;32], envelope_bytes: &[u8]) -> Result<Vec<u8>, DmError>;
```

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare full `cargo test`/`cargo build`. Always `nice` + throttle, scoped:
```bash
cd src-tauri && nice -n 10 cargo test --lib postoffice:: -- --test-threads=2
nice -n 10 cargo fmt
nice -n 10 cargo clippy --lib -- -D warnings
```
The pre-commit hook runs the full health check on commit — let it run. The election tests are pure (no disk); the relay/integration tests open `PersistentEventLog`s (one PBKDF2 each) so they are a bit slower — keep `--test-threads=2`.

**Conventions:** match `eventlog`/`dm` — hand-written types, `#[cfg(test)] mod tests`, `tempfile` (a dev-dependency) for the on-disk tests. rustfmt's `reorder_modules` sorts `pub mod` lines — keep them alphabetical.

---

### Task 1: `postoffice` module + election

**Files:**
- Create: `src-tauri/src/postoffice/mod.rs`
- Create: `src-tauri/src/postoffice/election.rs`
- Modify: `src-tauri/src/lib.rs` (module declaration)

- [ ] **Step 1: Register the module in `lib.rs`**

In `src-tauri/src/lib.rs`, add `pub mod postoffice;` in alphabetical position — **between** `pub mod platform;` and `pub mod services;** (`postoffice` sorts after `platform`, before `services`):
```rust
pub mod platform;
pub mod postoffice;
pub mod services;
```

- [ ] **Step 2: Create `postoffice/mod.rs` with the module doc and election re-exports**

```rust
//! The post office: an elected always-on peer that stores-and-forwards encrypted
//! events so a message reaches an offline recipient. It is a peer with two extra
//! hats — a full replica (a [`crate::eventlog::PersistentEventLog`]) and a
//! store-and-forward relay (it reconciles as a [`crate::eventlog::sync::SyncStore`]).
//! For DMs it is a *dumb relay*: it holds recipient-sealed envelopes it can never
//! decrypt, and never holds any conversation key.
//!
//! [`election`] decides — deterministically, so every peer agrees — which eligible
//! (always-on) peer is the post office: the one with the lowest identity
//! fingerprint. Full election with standbys and uptime detection is Phase 1.

pub mod election;

pub use election::{elect, is_post_office};
```

- [ ] **Step 3: Create `postoffice/election.rs` with the election + tests**

```rust
//! Deterministic post-office election. Among the eligible (always-on) peers, the
//! post office is the one with the lowest identity fingerprint (`user_id`).
//! Because the rule is a pure function of the eligible set, every peer computes
//! the same answer without coordination.

use crate::identity::device::PublicIdentity;

/// Elect the post office: the eligible peer with the lowest `user_id` fingerprint,
/// or `None` if no peer is eligible.
pub fn elect(eligible: &[PublicIdentity]) -> Option<PublicIdentity> {
    eligible.iter().min_by_key(|p| p.user_id()).cloned()
}

/// Whether `me` is the elected post office for the given eligible set.
pub fn is_post_office(me: &PublicIdentity, eligible: &[PublicIdentity]) -> bool {
    elect(eligible).as_ref() == Some(me)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    fn identities(n: usize) -> Vec<PublicIdentity> {
        (0..n).map(|_| DeviceIdentity::generate().public()).collect()
    }

    #[test]
    fn elects_lowest_fingerprint_independent_of_order() {
        let ids = identities(5);
        let expected = ids.iter().min_by_key(|p| p.user_id()).unwrap().clone();

        assert_eq!(elect(&ids).unwrap(), expected);

        // Order of the input must not change the result (every peer agrees).
        let mut reversed = ids.clone();
        reversed.reverse();
        assert_eq!(elect(&reversed).unwrap(), expected);
    }

    #[test]
    fn no_eligible_peers_means_no_post_office() {
        assert!(elect(&[]).is_none());
    }

    #[test]
    fn single_eligible_peer_is_elected() {
        let only = DeviceIdentity::generate().public();
        assert_eq!(elect(std::slice::from_ref(&only)), Some(only));
    }

    #[test]
    fn is_post_office_identifies_the_elected_node() {
        let ids = identities(4);
        let elected = elect(&ids).unwrap();
        assert!(is_post_office(&elected, &ids));

        // A different eligible peer is not the post office.
        let other = ids.iter().find(|p| **p != elected).unwrap();
        assert!(!is_post_office(other, &ids));
    }
}
```

- [ ] **Step 4: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib postoffice:: -- --test-threads=2
```
Expected: PASS — 4 tests.

- [ ] **Step 5: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/lib.rs src-tauri/src/postoffice/mod.rs src-tauri/src/postoffice/election.rs
git commit -m "feat(postoffice): deterministic lowest-fingerprint election

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: The `PostOffice` relay type

**Files:**
- Modify: `src-tauri/src/postoffice/mod.rs` (add `PostOffice` + `SyncStore` impl + re-export + tests)

- [ ] **Step 1: Add the imports, `PostOffice` type, and its `SyncStore` impl**

In `src-tauri/src/postoffice/mod.rs`, add these imports at the top (below the module doc, above `pub mod election;`):
```rust
use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::eventlog::persist::PersistentEventLog;
use crate::eventlog::store::AppendOutcome;
use crate::eventlog::sync::SyncStore;
use crate::eventlog::LogError;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use std::collections::HashSet;
use std::path::Path;
```
Add `PostOffice` to the re-export line so it reads:
```rust
pub use election::{elect, is_post_office};
```
(leave that line; `PostOffice` is defined in this file, no re-export needed). Then add, after the `pub use` line:
```rust
/// A post office: a durable replica plus a store-and-forward relay. It accepts
/// validly-signed events (it cannot forge or alter them) and serves them to
/// peers via reconciliation, but it never decrypts a DM — it has no key and no
/// API to do so.
pub struct PostOffice {
    log: PersistentEventLog,
    identity: DeviceIdentity,
}

impl PostOffice {
    /// Open (or create) the post office's replica at `path`, under `identity`.
    pub fn open(path: &Path, password: &str, identity: DeviceIdentity) -> Result<Self, LogError> {
        Ok(Self { log: PersistentEventLog::open(path, password)?, identity })
    }

    /// This post office's public identity (used by the election).
    pub fn public(&self) -> PublicIdentity {
        self.identity.public()
    }

    /// Accept an event for relay: validate (signature, integrity, parents, …) and
    /// persist it. A forged or malformed event is rejected.
    pub fn accept(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.log.append(event)
    }

    /// Whether this post office holds the event with the given id.
    pub fn has(&self, id: &EventId) -> bool {
        self.log.has(id)
    }

    /// The ids this post office holds for a conversation, sorted (for comparison).
    pub fn held_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut ids: Vec<EventId> = self.log.events(conversation).iter().map(|e| e.id).collect();
        ids.sort();
        ids
    }
}

impl SyncStore for PostOffice {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        SyncStore::event_ids(&self.log, conversation)
    }
    fn events_excluding(&self, conversation: &ConversationId, have: &HashSet<EventId>) -> Vec<Event> {
        SyncStore::events_excluding(&self.log, conversation, have)
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        SyncStore::ingest(&mut self.log, event)
    }
}
```

- [ ] **Step 2: Add the `PostOffice` tests**

Add a test module to `src-tauri/src/postoffice/mod.rs` (at the end of the file):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::EventKind;
    use crate::eventlog::store::EventLog;
    use crate::eventlog::sync::reconcile;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    fn message(author: &DeviceIdentity, seq: u64, parents: Vec<EventId>, lamport: u64, body: &[u8]) -> Event {
        Event::new(author, conv(), seq, parents, lamport, 0, EventKind::Message, body.to_vec())
    }

    #[test]
    fn accepts_and_holds_a_valid_event() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let po_id = DeviceIdentity::generate();
        let po_pub = po_id.public();
        let mut po = PostOffice::open(&dir.path().join("po.log"), "pw", po_id).unwrap();
        assert_eq!(po.public(), po_pub); // the post office knows its own identity (for election)

        let ev = message(&alice, 1, vec![], 1, b"x");
        assert_eq!(po.accept(ev.clone()).unwrap(), AppendOutcome::Appended);
        assert!(po.has(&ev.id));
        assert_eq!(po.held_ids(&conv()), vec![ev.id]);
    }

    #[test]
    fn rejects_a_forged_event() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let mut po = PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();

        let mut forged = message(&alice, 1, vec![], 1, b"x");
        forged.sig[0] ^= 0xFF;
        assert!(po.accept(forged).is_err());
    }

    #[test]
    fn a_peer_reconciles_its_events_into_the_post_office() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let mut alice_log = EventLog::default();
        let ev = message(&alice, 1, vec![], 1, b"x");
        alice_log.append(ev.clone()).unwrap();

        let mut po = PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();
        // The post office is just another SyncStore.
        reconcile(&mut alice_log, &mut po, conv());
        assert!(po.has(&ev.id));
    }
}
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib postoffice:: -- --test-threads=2
```
Expected: PASS — 7 tests (4 from Task 1 + 3 new).

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/postoffice/mod.rs
git commit -m "feat(postoffice): PostOffice relay (SyncStore over a durable replica)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Offline-delivery + split-brain integration tests

**Files:**
- Modify: `src-tauri/src/postoffice/mod.rs` (add integration tests)

- [ ] **Step 1: Add the integration tests**

Add these tests to the `#[cfg(test)] mod tests` block in `src-tauri/src/postoffice/mod.rs`:
```rust
    #[test]
    fn offline_dm_delivered_via_post_office() {
        // The headline scenario: Alice sends Bob a DM while Bob is offline; the
        // post office holds it; Bob comes online (Alice now offline) and receives
        // and decrypts it. The post office never could.
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Keep a second handle to the post office's identity to prove it can't decrypt.
        let po_identity = DeviceIdentity::generate();
        let (po_ed, po_x) = po_identity.secret_bytes();
        let po_identity_copy = DeviceIdentity::from_secret_bytes(po_ed, po_x);

        // Alice seals a DM to Bob and wraps it in a Message event.
        let sealed = crate::dm::seal(&alice, &bob.public().x25519_pub, b"meet at 5").unwrap();
        let dm_event = Event::new(&alice, conv(), 1, vec![], 1, 0, EventKind::Message, sealed);

        // Alice pushes it to the post office while Bob is offline.
        let mut alice_log = EventLog::default();
        alice_log.append(dm_event.clone()).unwrap();
        let mut po = PostOffice::open(&dir.path().join("po.log"), "pw", po_identity).unwrap();
        reconcile(&mut alice_log, &mut po, conv());
        assert!(po.has(&dm_event.id));

        // Bob comes online and syncs from the post office (Alice may now be offline).
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut po, conv());
        let received = bob_log.get(&dm_event.id).expect("bob received the event");

        // Bob opens it; the post office's own identity cannot.
        let plaintext = crate::dm::open(&bob, &alice.public().x25519_pub, &received.ciphertext).unwrap();
        assert_eq!(plaintext, b"meet at 5");
        assert!(crate::dm::open(&po_identity_copy, &alice.public().x25519_pub, &received.ciphertext).is_err());
    }

    #[test]
    fn two_post_offices_converge_after_split_brain() {
        // Two post offices each saw a different branch of a conversation; after they
        // reconcile, content-addressed events merge idempotently to identical state.
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();

        let root = message(&alice, 1, vec![], 1, b"root");
        let x = message(&alice, 2, vec![root.id], 2, b"x");
        let y = message(&alice, 3, vec![root.id], 2, b"y");

        let mut po1 = PostOffice::open(&dir.path().join("po1.log"), "pw", DeviceIdentity::generate()).unwrap();
        let mut po2 = PostOffice::open(&dir.path().join("po2.log"), "pw", DeviceIdentity::generate()).unwrap();
        po1.accept(root.clone()).unwrap();
        po1.accept(x.clone()).unwrap();
        po2.accept(root.clone()).unwrap();
        po2.accept(y.clone()).unwrap();

        reconcile(&mut po1, &mut po2, conv());
        assert_eq!(po1.held_ids(&conv()), po2.held_ids(&conv()));
        assert!(po1.has(&x.id) && po1.has(&y.id));
        assert!(po2.has(&x.id) && po2.has(&y.id));
    }
```

- [ ] **Step 2: Run the whole `postoffice` suite**
```bash
cd src-tauri && nice -n 10 cargo test --lib postoffice:: -- --test-threads=2
```
Expected: PASS — 9 tests (7 + 2 new).

- [ ] **Step 3: Full fmt + clippy gate**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt --check; echo "fmt EXIT: $?"
```
Expected: clippy clean; `fmt EXIT: 0`.

- [ ] **Step 4: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/postoffice/mod.rs
git commit -m "test(postoffice): offline DM delivery and split-brain convergence

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next phase

- **What this delivers:** the minimal post office — `election::{elect, is_post_office}` (deterministic lowest-fingerprint choice) and `PostOffice` (a `SyncStore`-backed durable relay). The integration tests prove the headline property: **a DM reaches an offline recipient via the post office, which forwards the sealed envelope without ever decrypting it**, and that two post offices reconcile idempotently after a split.
- **Why it's mostly assembly:** offline delivery = the sender reconciles to the post office, the recipient reconciles from it — both via the existing `reconcile`. The post office's blindness = it only ever holds `Event`s (opaque `ciphertext`) and has no decryption API. Authorization for DMs is "accept any validly-signed event" (the store's `append` is the gate); membership/key-holding is irrelevant because DMs are recipient-sealed.
- **Deliberately deferred (do NOT add here):** full leader election (raft-lite), hot standbys, uptime-based eligibility detection, and the "longest-online peer" fallback when no peer is flagged always-on (Phase 1 — "full post-office election & standbys"); channel (group) membership where the post office is a member holding ciphertext; rate-limiting / anti-spam on `accept`; and running reconciliation over `transport::SecureChannel` (the node-service integration / two-CLI rig that wires transport + sync + DM + post office together — the Phase-0 validation step that follows this plan).
- **Failure-mode coverage (from spec §6):** "post office down → peers reconcile on its return" is the same `reconcile` path (no post-office-specific code). "Split-brain (two post offices) → merge idempotently" is tested directly. "A message is only delayed, never lost" follows from store-and-forward: the event persists in the post office's replica until the recipient syncs.
- **Accepted v1 limitations:** eligibility is an input (a peer is flagged always-on by the node service); the post office accepts any validly-signed event for any conversation (no per-conversation authorization yet); single active post office (no standby replication in this plan).
