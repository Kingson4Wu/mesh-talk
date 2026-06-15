# Sync Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reconcile a conversation's event log between any two peers — "tell me what you have, here are the events you're missing" — so two divergent local logs converge to the union, with the same protocol for peer↔peer and peer↔post-office.

**Architecture:** A new `eventlog::sync` module: serializable message types (`SyncRequest` / `SyncResponse` / `SyncFollowup`), a small `SyncStore` trait (the three data operations sync needs, implemented by both `EventLog` and `PersistentEventLog`), pure handler functions that compute each message and ingest received events with full validation, and an in-process `reconcile` driver. Reconciliation is **set-based over content-addressed event ids** (not a version-vector diff — see the design note), which is correct regardless of per-author sequence gaps. Received events are validated through the store's normal `append` path (signature, integrity, canonical form, causal completeness, equivocation), so sync cannot inject forged history.

**Tech Stack:** Rust; the existing `eventlog` module (Plan 3); `serde`/`bincode` for messages; `rand` (already a dependency) for a seeded randomized convergence test. No new dependencies.

---

## Background the implementer needs

**The event-log store this builds on (`src-tauri/src/eventlog/`, from Plan 3):**
```rust
// event.rs
pub struct EventId([u8; 32]);          // Copy, Eq, Hash, Ord, Serialize, Deserialize
pub struct ConversationId([u8; 32]);   // Copy, Eq, Hash, Serialize, Deserialize
pub struct Event { pub id: EventId, pub conversation_id: ConversationId, pub parents: Vec<EventId>, /* ... */ }
                                       // Event: Clone, PartialEq, Eq, Serialize, Deserialize
// store.rs
pub enum AppendOutcome { Appended, Duplicate }
impl EventLog {
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError>; // FULL validation
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event>;        // total order (lamport, id)
    // also: has, get, heads, version_vector, prepare
}
// persist.rs
impl PersistentEventLog {
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError>; // validate -> persist -> index
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event>;
}
// mod.rs
pub enum LogError { /* CorruptId, BadSignature, MissingParents, NonCanonical, AuthorEquivocation, ... */ }
```
Key facts:
- `events(conv)` returns the conversation's events in **total order `(lamport, id)`**, which is a topological order (parents before children) for honestly-constructed events. A *subset* of that order, taken in order, is still safe to `append` one-by-one: each event's parents are either already in the store or appear earlier in the subset.
- `append` is the single validation gate. Applying a received event through `append` re-checks everything; a duplicate (same content hash) returns `Ok(AppendOutcome::Duplicate)` rather than erroring.

**Design note — why id-sets, not version vectors.** The design spec (§5) describes the sync protocol as "exchange per-author version vectors → you're missing these ids, here they are." However, Plan 3's `EventLog::version_vector` is documented as a per-author **`max(seq)`, not a dense high-water mark**: the store does not enforce gap-free sequences, so `author → 5` does not imply seqs `1..=5` are all present. A naïve version-vector diff would therefore be *unsound* — it cannot detect that a peer is missing a middle event. This plan instead exchanges the **set of event ids** a peer holds for a conversation; the responder returns exactly the events whose ids the requester lacks. This is correct regardless of gaps and still matches the spec's "here are the ids you're missing." A compact version-vector/range summary is a deferred optimization (viable once sequences are made dense or range-encoded); it is **not** in scope here.

**Scope.** This plan delivers the reconciliation *logic* and *serializable messages*. Running sync over the network (serializing these messages across a `transport::SecureChannel`) and the post-office relay are deferred to the node-service integration and Plan 6 — the `reconcile` function here is the in-process driver (also what the two-CLI rig will call once wired). The post office is "just another `SyncStore`," so no protocol change is needed for it.

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare full `cargo test`/`cargo build`. Always `nice` + throttle, scoped:
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::sync -- --test-threads=2
nice -n 10 cargo fmt
nice -n 10 cargo clippy --lib -- -D warnings
```
The pre-commit hook runs the full health check on commit — let it run (may take minutes). The sync tests use the in-memory `EventLog` (no disk, no PBKDF2) so they are fast; only one test touches `PersistentEventLog`.

**Conventions:** match the rest of `eventlog` and `transport` — hand-written types, `#[cfg(test)] mod tests`, `tempfile` (a dev-dependency) for the one persistence test. rustfmt's `reorder_modules` sorts `pub mod` lines — keep them alphabetical.

---

### Task 1: `sync` module — message types and the `SyncStore` trait

**Files:**
- Create: `src-tauri/src/eventlog/sync.rs`
- Modify: `src-tauri/src/eventlog/mod.rs` (add module + re-exports)

- [ ] **Step 1: Add the module declaration and re-exports to `mod.rs`**

In `src-tauri/src/eventlog/mod.rs`, add `pub mod sync;` (sorted: `event`, `persist`, `store`, `sync`) and re-export the public sync API. After the existing `pub use` lines, the module/re-export section should include:
```rust
pub mod sync;
```
and, after the existing `pub use persist::PersistentEventLog;` (or wherever rustfmt places it), add a re-export of the sync **types** (the handler functions stay `pub` in `sync.rs` and are reached via the `eventlog::sync::` path — they are not re-exported here, which keeps this line valid before those functions exist):
```rust
pub use sync::{ApplyReport, SyncFollowup, SyncRequest, SyncResponse, SyncStore};
```
(rustfmt may reorder these alphabetically — that's fine.)

- [ ] **Step 2: Create `src-tauri/src/eventlog/sync.rs` with the messages, trait, impls, and tests**

```rust
//! Event-log reconciliation ("sync"). Two peers converge a conversation's log
//! by exchanging the set of event ids each holds; the responder returns exactly
//! the events the requester is missing, in topological order. Received events
//! are validated through [`SyncStore::ingest`] (the store's normal `append`
//! gate), so sync can never inject forged or out-of-order history.
//!
//! Reconciliation is id-set based, not version-vector based: the store does not
//! guarantee dense per-author sequences, so a version-vector diff would miss a
//! gap. See the module plan for the rationale.

use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::eventlog::store::{AppendOutcome, EventLog};
use crate::eventlog::LogError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Sent by the peer initiating sync: "for this conversation, here are the ids I have."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    pub conversation: ConversationId,
    pub have: Vec<EventId>,
}

/// The responder's reply: the events the requester is missing (topological
/// order), plus the responder's own id-set so the requester can reciprocate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResponse {
    pub conversation: ConversationId,
    pub events: Vec<Event>,
    pub have: Vec<EventId>,
}

/// The requester's final message: the events the responder is missing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncFollowup {
    pub conversation: ConversationId,
    pub events: Vec<Event>,
}

/// Outcome of ingesting a batch of received events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Events newly appended.
    pub applied: usize,
    /// Events already present (content-addressed duplicates).
    pub duplicates: usize,
    /// Events rejected by validation, with the reason. (id, message)
    pub rejected: Vec<(EventId, String)>,
}

/// The data operations sync needs from an event store. Implemented by both the
/// in-memory [`EventLog`] and the durable `PersistentEventLog`.
pub trait SyncStore {
    /// All event ids this store holds for the conversation.
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId>;
    /// The conversation's events whose id is NOT in `have`, cloned, in
    /// topological (`(lamport, id)`) order — safe to ingest one-by-one.
    fn events_excluding(&self, conversation: &ConversationId, have: &HashSet<EventId>) -> Vec<Event>;
    /// Validate and store a received event (the store's normal append gate).
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError>;
}

impl SyncStore for EventLog {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.events(conversation).iter().map(|e| e.id).collect()
    }
    fn events_excluding(&self, conversation: &ConversationId, have: &HashSet<EventId>) -> Vec<Event> {
        self.events(conversation)
            .into_iter()
            .filter(|e| !have.contains(&e.id))
            .cloned()
            .collect()
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append(event)
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

    /// Build a signed event in `conv()`.
    fn ev(
        id: &DeviceIdentity,
        seq: u64,
        parents: Vec<EventId>,
        lamport: u64,
        payload: &[u8],
    ) -> Event {
        Event::new(id, conv(), seq, parents, lamport, 0, EventKind::Message, payload.to_vec())
    }

    #[test]
    fn store_reports_ids_and_excludes_known() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let a = ev(&id, 1, vec![], 1, b"a");
        let b = ev(&id, 2, vec![a.id], 2, b"b");
        log.append(a.clone()).unwrap();
        log.append(b.clone()).unwrap();

        let ids = log.event_ids(&conv());
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));

        // Excluding `a` leaves only `b`.
        let have: HashSet<EventId> = std::iter::once(a.id).collect();
        let missing = log.events_excluding(&conv(), &have);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, b.id);
    }

    #[test]
    fn messages_round_trip_through_bincode() {
        let id = DeviceIdentity::generate();
        let a = ev(&id, 1, vec![], 1, b"a");
        let req = SyncRequest { conversation: conv(), have: vec![a.id] };
        let resp = SyncResponse { conversation: conv(), events: vec![a.clone()], have: vec![a.id] };

        let req2: SyncRequest =
            bincode::deserialize(&bincode::serialize(&req).unwrap()).unwrap();
        let resp2: SyncResponse =
            bincode::deserialize(&bincode::serialize(&resp).unwrap()).unwrap();
        assert_eq!(req2.have, req.have);
        assert_eq!(resp2.events, resp.events);
    }
}
```

- [ ] **Step 3: Implement `SyncStore` for `PersistentEventLog`**

`PersistentEventLog` lives in `src-tauri/src/eventlog/persist.rs`. Add this impl block to `persist.rs` (it needs the trait + a `HashSet`; add the imports if missing). At the top of `persist.rs`, ensure these imports exist (merge with existing `use` lines — do not duplicate):
```rust
use crate::eventlog::store::AppendOutcome;
use crate::eventlog::sync::SyncStore;
use std::collections::HashSet;
```
Then add, ABOVE the `#[cfg(test)] mod tests` block in `persist.rs`:
```rust
impl SyncStore for PersistentEventLog {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.events(conversation).iter().map(|e| e.id).collect()
    }
    fn events_excluding(&self, conversation: &ConversationId, have: &HashSet<EventId>) -> Vec<Event> {
        self.events(conversation)
            .into_iter()
            .filter(|e| !have.contains(&e.id))
            .cloned()
            .collect()
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append(event)
    }
}
```
> Note: `persist.rs` already imports `Event`, `ConversationId`, `EventId`, `LogError`. If `AppendOutcome` was already imported (Plan 3 used it), do not import it twice — run clippy to catch any duplicate/unused import and fix.

- [ ] **Step 4: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::sync -- --test-threads=2
```
Expected: PASS — 2 tests.

- [ ] **Step 5: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/mod.rs src-tauri/src/eventlog/sync.rs src-tauri/src/eventlog/persist.rs
git commit -m "feat(eventlog): sync message types and SyncStore trait

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Request & response handlers

**Files:**
- Modify: `src-tauri/src/eventlog/sync.rs` (add `build_request`, `handle_request` + tests)

- [ ] **Step 1: Add `build_request` and `handle_request`**

In `src-tauri/src/eventlog/sync.rs`, add these functions after the `SyncStore` impl for `EventLog` (before the `#[cfg(test)]` module):
```rust
/// Build the opening request: the ids this store holds for `conversation`.
pub fn build_request(store: &impl SyncStore, conversation: ConversationId) -> SyncRequest {
    SyncRequest { conversation, have: store.event_ids(&conversation) }
}

/// Answer a request: the events the requester is missing (topological order),
/// plus this store's own id-set so the requester can reciprocate.
pub fn handle_request(store: &impl SyncStore, request: &SyncRequest) -> SyncResponse {
    let have: HashSet<EventId> = request.have.iter().copied().collect();
    let events = store.events_excluding(&request.conversation, &have);
    let have = store.event_ids(&request.conversation);
    SyncResponse { conversation: request.conversation, events, have }
}
```

- [ ] **Step 2: Add the handler tests to the test module**

Add to the `#[cfg(test)] mod tests` block in `sync.rs`:
```rust
    #[test]
    fn request_lists_all_local_ids() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let a = ev(&id, 1, vec![], 1, b"a");
        let b = ev(&id, 2, vec![a.id], 2, b"b");
        log.append(a.clone()).unwrap();
        log.append(b.clone()).unwrap();

        let req = build_request(&log, conv());
        assert_eq!(req.conversation, conv());
        assert_eq!(req.have.len(), 2);
    }

    #[test]
    fn response_sends_exactly_the_requesters_missing_events() {
        let id = DeviceIdentity::generate();
        // Responder has root -> a -> b; requester has only root.
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![a.id], 3, b"b");
        responder.append(root.clone()).unwrap();
        responder.append(a.clone()).unwrap();
        responder.append(b.clone()).unwrap();

        let req = SyncRequest { conversation: conv(), have: vec![root.id] };
        let resp = handle_request(&responder, &req);

        // Missing = {a, b}, in topological order (a before b).
        assert_eq!(resp.events.len(), 2);
        assert_eq!(resp.events[0].id, a.id);
        assert_eq!(resp.events[1].id, b.id);
        // Responder advertises all three of its ids.
        assert_eq!(resp.have.len(), 3);
    }

    #[test]
    fn response_is_empty_when_requester_has_everything() {
        let id = DeviceIdentity::generate();
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        responder.append(root.clone()).unwrap();

        let req = SyncRequest { conversation: conv(), have: vec![root.id] };
        let resp = handle_request(&responder, &req);
        assert!(resp.events.is_empty());
    }

    #[test]
    fn response_for_disjoint_branches_sends_the_responders_branch() {
        let id = DeviceIdentity::generate();
        // Shared root, then responder has branch `b`, requester has branch `a`.
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![root.id], 2, b"b");

        let mut responder = EventLog::default();
        responder.append(root.clone()).unwrap();
        responder.append(b.clone()).unwrap();

        // Requester has root + a.
        let req = SyncRequest { conversation: conv(), have: vec![root.id, a.id] };
        let resp = handle_request(&responder, &req);
        // Responder only has root + b; requester is missing b (and has root already).
        assert_eq!(resp.events.len(), 1);
        assert_eq!(resp.events[0].id, b.id);
    }
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::sync -- --test-threads=2
```
Expected: PASS — 6 tests (2 from Task 1 + 4 new).

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/sync.rs
git commit -m "feat(eventlog): sync request/response handlers

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Ingest & reciprocate handlers

**Files:**
- Modify: `src-tauri/src/eventlog/sync.rs` (add `ingest_all`, `handle_response`, `handle_followup` + tests)

- [ ] **Step 1: Add the ingest helper and the response/followup handlers**

In `src-tauri/src/eventlog/sync.rs`, add after `handle_request`:
```rust
/// Ingest a batch of received events through the store's validation gate,
/// tallying outcomes. An invalid event is rejected and recorded, not fatal —
/// the rest of the batch still applies.
fn ingest_all(store: &mut impl SyncStore, events: &[Event]) -> ApplyReport {
    let mut report = ApplyReport::default();
    for event in events {
        match store.ingest(event.clone()) {
            Ok(AppendOutcome::Appended) => report.applied += 1,
            Ok(AppendOutcome::Duplicate) => report.duplicates += 1,
            Err(e) => report.rejected.push((event.id, e.to_string())),
        }
    }
    report
}

/// Apply a responder's reply: ingest the events it sent, then compute the
/// events IT is missing (so the caller can send a [`SyncFollowup`]).
pub fn handle_response(
    store: &mut impl SyncStore,
    response: &SyncResponse,
) -> (ApplyReport, SyncFollowup) {
    let report = ingest_all(store, &response.events);
    let remote_have: HashSet<EventId> = response.have.iter().copied().collect();
    let events = store.events_excluding(&response.conversation, &remote_have);
    (report, SyncFollowup { conversation: response.conversation, events })
}

/// Apply the requester's follow-up: ingest the events it sent.
pub fn handle_followup(store: &mut impl SyncStore, followup: &SyncFollowup) -> ApplyReport {
    ingest_all(store, &followup.events)
}
```

- [ ] **Step 2: Add the tests**

Add to the `#[cfg(test)] mod tests` block:
```rust
    #[test]
    fn handle_response_applies_events_and_builds_followup() {
        let id = DeviceIdentity::generate();
        // Requester has only root; responder sent it a + b.
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![a.id], 3, b"b");
        // Requester also has a local-only event c that the responder lacks.
        let c = ev(&id, 4, vec![root.id], 2, b"c");

        let mut requester = EventLog::default();
        requester.append(root.clone()).unwrap();
        requester.append(c.clone()).unwrap();

        // Responder advertised {root, a, b}; the followup should offer c.
        let response = SyncResponse {
            conversation: conv(),
            events: vec![a.clone(), b.clone()],
            have: vec![root.id, a.id, b.id],
        };
        let (report, followup) = handle_response(&mut requester, &response);

        assert_eq!(report.applied, 2);
        assert!(report.rejected.is_empty());
        assert!(requester.has(&a.id) && requester.has(&b.id));
        // Responder is missing c.
        assert_eq!(followup.events.len(), 1);
        assert_eq!(followup.events[0].id, c.id);
    }

    #[test]
    fn ingest_rejects_a_forged_event_but_keeps_the_rest() {
        let id = DeviceIdentity::generate();
        let mut requester = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        requester.append(root.clone()).unwrap();

        // A valid event a, and a forged event with a broken signature.
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let mut forged = ev(&id, 3, vec![root.id], 2, b"forged");
        forged.sig[0] ^= 0xFF;

        let response = SyncResponse {
            conversation: conv(),
            events: vec![a.clone(), forged.clone()],
            have: vec![],
        };
        let (report, _followup) = handle_response(&mut requester, &response);

        assert_eq!(report.applied, 1); // only `a`
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].0, forged.id);
        assert!(requester.has(&a.id));
        assert!(!requester.has(&forged.id));
    }

    #[test]
    fn ingesting_known_events_counts_as_duplicates() {
        let id = DeviceIdentity::generate();
        let mut requester = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        requester.append(root.clone()).unwrap();

        let response = SyncResponse {
            conversation: conv(),
            events: vec![root.clone()], // requester already has root
            have: vec![root.id],
        };
        let (report, _followup) = handle_response(&mut requester, &response);
        assert_eq!(report.applied, 0);
        assert_eq!(report.duplicates, 1);
    }

    #[test]
    fn handle_followup_applies_events() {
        let id = DeviceIdentity::generate();
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        responder.append(root.clone()).unwrap();

        let c = ev(&id, 2, vec![root.id], 2, b"c");
        let followup = SyncFollowup { conversation: conv(), events: vec![c.clone()] };
        let report = handle_followup(&mut responder, &followup);
        assert_eq!(report.applied, 1);
        assert!(responder.has(&c.id));
    }
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::sync -- --test-threads=2
```
Expected: PASS — 10 tests (6 + 4 new).

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/sync.rs
git commit -m "feat(eventlog): sync ingest and reciprocate handlers

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: The `reconcile` driver and convergence

**Files:**
- Modify: `src-tauri/src/eventlog/sync.rs` (add `reconcile` + convergence tests)

- [ ] **Step 1: Add `reconcile`**

In `src-tauri/src/eventlog/sync.rs`, add after `handle_followup`:
```rust
/// Run a full in-process reconciliation between two stores for one conversation.
/// `a` is the requester, `b` the responder; afterward both hold the union of the
/// two logs. Returns `(report_a, report_b)` — what each side applied. This is the
/// reference driver; over a network, the three messages travel across a channel.
pub fn reconcile<A: SyncStore, B: SyncStore>(
    a: &mut A,
    b: &mut B,
    conversation: ConversationId,
) -> (ApplyReport, ApplyReport) {
    let request = build_request(a, conversation);
    let response = handle_request(b, &request);
    let (report_a, followup) = handle_response(a, &response);
    let report_b = handle_followup(b, &followup);
    (report_a, report_b)
}
```

- [ ] **Step 2: Add the convergence tests**

Add to the `#[cfg(test)] mod tests` block. First add this assertion helper at the top of the `tests` module (right after `ev`):
```rust
    /// Assert two stores hold the same conversation log (same events, same order).
    fn assert_converged(a: &EventLog, b: &EventLog) {
        let ea: Vec<EventId> = a.events(&conv()).iter().map(|e| e.id).collect();
        let eb: Vec<EventId> = b.events(&conv()).iter().map(|e| e.id).collect();
        assert_eq!(ea, eb, "stores did not converge");
    }
```
Then the tests:
```rust
    #[test]
    fn disjoint_branches_converge() {
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a_ev = ev(&id, 2, vec![root.id], 2, b"a");
        let b_ev = ev(&id, 3, vec![root.id], 2, b"b");

        let mut a = EventLog::default();
        a.append(root.clone()).unwrap();
        a.append(a_ev.clone()).unwrap();

        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();
        b.append(b_ev.clone()).unwrap();

        reconcile(&mut a, &mut b, conv());
        assert_converged(&a, &b);
        // Both now hold all three events.
        assert!(a.has(&a_ev.id) && a.has(&b_ev.id));
        assert!(b.has(&a_ev.id) && b.has(&b_ev.id));
    }

    #[test]
    fn subset_peer_catches_up() {
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a_ev = ev(&id, 2, vec![root.id], 2, b"a");
        let b_ev = ev(&id, 3, vec![a_ev.id], 3, b"b");

        // `full` has the whole chain; `partial` has only root.
        let mut full = EventLog::default();
        full.append(root.clone()).unwrap();
        full.append(a_ev.clone()).unwrap();
        full.append(b_ev.clone()).unwrap();

        let mut partial = EventLog::default();
        partial.append(root.clone()).unwrap();

        reconcile(&mut partial, &mut full, conv());
        assert_converged(&partial, &full);
        assert_eq!(partial.events(&conv()).len(), 3);
    }

    #[test]
    fn identical_logs_exchange_nothing() {
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");

        let mut a = EventLog::default();
        a.append(root.clone()).unwrap();
        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();

        let (report_a, report_b) = reconcile(&mut a, &mut b, conv());
        assert_eq!(report_a.applied, 0);
        assert_eq!(report_b.applied, 0);
        assert_converged(&a, &b);
    }

    #[test]
    fn reconcile_is_idempotent() {
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a_ev = ev(&id, 2, vec![root.id], 2, b"a");
        let b_ev = ev(&id, 3, vec![root.id], 2, b"b");

        let mut a = EventLog::default();
        a.append(root.clone()).unwrap();
        a.append(a_ev.clone()).unwrap();
        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();
        b.append(b_ev.clone()).unwrap();

        reconcile(&mut a, &mut b, conv());
        // A second pass applies nothing new.
        let (report_a, report_b) = reconcile(&mut a, &mut b, conv());
        assert_eq!(report_a.applied, 0);
        assert_eq!(report_b.applied, 0);
        assert_converged(&a, &b);
    }

    #[test]
    fn multi_author_concurrent_dag_converges() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let root = ev(&alice, 1, vec![], 1, b"root");
        // Alice's branch and Bob's branch from the shared root, then Alice merges.
        let a1 = ev(&alice, 2, vec![root.id], 2, b"a1");
        let b1 = Event::new(&bob, conv(), 1, vec![root.id], 2, 0, EventKind::Message, b"b1".to_vec());
        let merge = ev(&alice, 3, vec![a1.id, b1.id], 3, b"merge");

        // Start divergent: A has Alice's branch, B has Bob's branch.
        let mut a = EventLog::default();
        a.append(root.clone()).unwrap();
        a.append(a1.clone()).unwrap();

        let mut b = EventLog::default();
        b.append(root.clone()).unwrap();
        b.append(b1.clone()).unwrap();

        // First reconcile so both have {root, a1, b1}, then Alice can author the merge.
        reconcile(&mut a, &mut b, conv());
        assert_converged(&a, &b);
        a.append(merge.clone()).unwrap();

        // Sync again; B catches the merge.
        reconcile(&mut b, &mut a, conv());
        assert_converged(&a, &b);
        assert!(b.has(&merge.id));
    }
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::sync -- --test-threads=2
```
Expected: PASS — 15 tests (10 + 5 new).

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/sync.rs
git commit -m "feat(eventlog): reconcile driver with convergence tests

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Offline catch-up scenario + randomized convergence

**Files:**
- Modify: `src-tauri/src/eventlog/sync.rs` (add scenario + property tests)
- Modify: `src-tauri/src/eventlog/persist.rs` (one durable-store sync test)

- [ ] **Step 1: Add the offline catch-up and randomized convergence tests**

Add to the `#[cfg(test)] mod tests` block in `sync.rs`:
```rust
    #[test]
    fn returning_peer_catches_up_after_offline_activity() {
        // Models Phase-0 offline delivery: A goes offline at the root; B keeps
        // appending; A returns and syncs everything B accumulated.
        let id = DeviceIdentity::generate();
        let root = ev(&id, 1, vec![], 1, b"root");

        let mut online = EventLog::default();
        online.append(root.clone()).unwrap();
        // B (online) appends a chain of 5 while A is away.
        let mut parent = root.id;
        let mut chain = Vec::new();
        for i in 0..5u64 {
            let e = ev(&id, i + 2, vec![parent], i + 2, &[b'm', i as u8]);
            online.append(e.clone()).unwrap();
            parent = e.id;
            chain.push(e);
        }

        let mut returning = EventLog::default();
        returning.append(root.clone()).unwrap();

        let (report, _) = reconcile(&mut returning, &mut online, conv());
        assert_eq!(report.applied, 5);
        assert_converged(&returning, &online);
        for e in &chain {
            assert!(returning.has(&e.id));
        }
    }

    #[test]
    fn random_partitions_converge() {
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};
        use std::collections::HashMap;

        let authors: Vec<DeviceIdentity> = (0..3).map(|_| DeviceIdentity::generate()).collect();

        for seed in 0..8u64 {
            let mut rng = StdRng::seed_from_u64(seed);

            // Build a valid reference DAG of 12 events using a reference log's
            // `prepare` for correct parents/lamport, with dense per-author seqs.
            let mut reference = EventLog::default();
            let mut all: Vec<Event> = Vec::new();
            let mut seqs: HashMap<[u8; 32], u64> = HashMap::new();
            for i in 0..12u64 {
                let author = &authors[rng.gen_range(0..authors.len())];
                let (parents, lamport) = reference.prepare(&conv());
                let pk = author.public().ed25519_pub;
                let seq = {
                    let s = seqs.entry(pk).or_insert(0);
                    *s += 1;
                    *s
                };
                let e = ev(author, seq, parents, lamport, &[i as u8]);
                reference.append(e.clone()).unwrap();
                all.push(e);
            }

            // Causally-closed random partition: only add an event to a side if all
            // its parents are already there, so each side stays append-valid.
            let mut a = EventLog::default();
            let mut b = EventLog::default();
            let (mut a_ids, mut b_ids): (HashSet<EventId>, HashSet<EventId>) =
                (HashSet::new(), HashSet::new());
            for e in &all {
                if rng.gen_bool(0.7) && e.parents.iter().all(|p| a_ids.contains(p)) {
                    a.append(e.clone()).unwrap();
                    a_ids.insert(e.id);
                }
                if rng.gen_bool(0.7) && e.parents.iter().all(|p| b_ids.contains(p)) {
                    b.append(e.clone()).unwrap();
                    b_ids.insert(e.id);
                }
            }

            reconcile(&mut a, &mut b, conv());

            // Both converge to exactly the union of the two partitions.
            let mut got: Vec<EventId> = a.events(&conv()).iter().map(|e| e.id).collect();
            let mut got_b: Vec<EventId> = b.events(&conv()).iter().map(|e| e.id).collect();
            got.sort();
            got_b.sort();
            assert_eq!(got, got_b, "seed {seed}: peers did not converge");

            let mut union: Vec<EventId> = a_ids.union(&b_ids).copied().collect();
            union.sort();
            assert_eq!(got, union, "seed {seed}: converged set != union of partitions");
        }
    }
```
> Property-testing note: this is a seeded, deterministic randomized test (no new dependency). A full `proptest`-based suite (the design spec's "property tests for convergence under reordering and partition") is a worthwhile later enhancement; the seeded generator here exercises varied DAG shapes and partitions deterministically.

- [ ] **Step 2: Add one durable-store sync test to `persist.rs`**

In the `#[cfg(test)] mod tests` block in `src-tauri/src/eventlog/persist.rs`, add a test that the `SyncStore` impl works end-to-end through the durable log and survives reopen. Add the import the test needs at the top of the `tests` module (inside `mod tests`, after the existing `use super::*;`):
```rust
    use crate::eventlog::sync::{reconcile, SyncStore};
```
Then the test (the `mk` helper already exists in `persist.rs` tests; it builds an event in conversation `[1u8;32]` with empty parents):
```rust
    #[test]
    fn persistent_store_syncs_and_persists_received_events() {
        let dir = tempfile::tempdir().unwrap();
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();

        // Source log has a root + child.
        let root = mk(&id, 1, 1, b"root");
        let child = Event::new(&id, conv, 2, vec![root.id], 2, 0, EventKind::Message, b"child".to_vec());

        let src_path = dir.path().join("src.log");
        let mut source = PersistentEventLog::open(&src_path, "pw").unwrap();
        source.append(root.clone()).unwrap();
        source.append(child.clone()).unwrap();

        // Empty destination syncs from the source.
        let dst_path = dir.path().join("dst.log");
        {
            let mut dest = PersistentEventLog::open(&dst_path, "pw").unwrap();
            reconcile(&mut dest, &mut source, conv);
            assert!(dest.has(&root.id) && dest.has(&child.id));
            // Sanity: the SyncStore view agrees.
            assert_eq!(SyncStore::event_ids(&dest, &conv).len(), 2);
        }

        // Received events were persisted — they survive reopen.
        let dest = PersistentEventLog::open(&dst_path, "pw").unwrap();
        assert_eq!(dest.events(&conv).len(), 2);
    }
```

- [ ] **Step 3: Run the whole eventlog suite**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog:: -- --test-threads=2
```
Expected: PASS — all `eventlog` tests, including the 17 sync tests and the new persist test.

- [ ] **Step 4: Full fmt + clippy gate**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt --check; echo "fmt EXIT: $?"
```
Expected: clippy clean; `fmt EXIT: 0`.

- [ ] **Step 5: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/sync.rs src-tauri/src/eventlog/persist.rs
git commit -m "test(eventlog): offline catch-up, randomized convergence, durable sync

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next phase

- **What this delivers:** the reconciliation logic + serializable messages — `build_request` / `handle_request` / `handle_response` / `handle_followup`, the `reconcile` reference driver, the `SyncStore` trait (both stores), and `ApplyReport`. Two divergent logs converge to the union; received events are fully validated (no forged history); duplicates and rejects are tallied.
- **Deliberately deferred (do NOT add here):** running sync over `transport::SecureChannel` (a thin send/recv loop wiring these messages — belongs to the node-service integration / two-CLI rig); the post office (Plan 6 — it is just another `SyncStore`, no protocol change); multi-conversation sweep (a loop over conversations); a compact version-vector/range summary to replace the id-set (needs dense or range-encoded sequences first); and a `proptest` property suite.
- **Correctness argument:** the responder returns every event the requester lacks, in `(lamport, id)` order; because the store's append requires parents-present and content-addresses events, the requester can apply that subset in order (each event's parents precede it or already exist) and reach `A ∪ B`; the symmetric follow-up brings the responder to `A ∪ B` too. Convergence is therefore reached in one round and is idempotent. Id-set reconciliation is sound even when per-author sequences have gaps (unlike a version-vector diff).
- **Accepted v1 limitations:** O(n) id-set per sync (fine at office scale; compact summaries deferred); single-conversation unit; in-process driver (network driver is the integration step).
