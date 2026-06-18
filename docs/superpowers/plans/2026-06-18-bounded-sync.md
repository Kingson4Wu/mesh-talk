# Bounded / Multi-Round Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a conversation sync even when its missing events exceed one transport frame: bound each `SyncResponse`/`SyncFollowup` to a per-frame byte budget and loop `request_round` until the conversation fully reconciles. Then raise the file-size cap.

**Architecture:** Add `cap_events` (longest event prefix fitting a byte budget) + bounded variants `handle_request_bounded`/`handle_response_bounded`; the existing `handle_request`/`handle_response` become `usize::MAX` wrappers so `reconcile` + all current tests are untouched. `serve_one` bounds the Response it builds; `request_round` loops the Request→Response→Followup exchange until a round makes no progress. Raise `MAX_FILE_SIZE` to 8 MB.

**Tech Stack:** Rust; `eventlog::sync`, `node::session`, `bincode` (fixint, for event sizing), `crate::transport::MAX_PLAINTEXT`. No new dependencies.

---

## Background the implementer needs

Spec: `docs/superpowers/specs/2026-06-18-bounded-sync-design.md`. The sync protocol (`src-tauri/src/eventlog/sync.rs`):
```rust
pub trait SyncStore {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId>;
    fn events_excluding(&self, conversation: &ConversationId, have: &HashSet<EventId>) -> Vec<Event>; // topological order
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError>;
}
pub struct SyncRequest { pub conversation: ConversationId, pub have: Vec<EventId> }
pub struct SyncResponse { pub conversation: ConversationId, pub events: Vec<Event>, pub have: Vec<EventId> } // have = RESPONDER's ids
pub struct SyncFollowup { pub conversation: ConversationId, pub events: Vec<Event> }
pub struct ApplyReport { pub applied: usize, pub duplicates: usize, pub rejected: Vec<(EventId, String)> } // Default

pub fn build_request(store, conversation) -> SyncRequest // { conversation, have: store.event_ids(conv) }
pub fn handle_request(store, request) -> SyncResponse     // events = events_excluding(requester_have); have = responder ids
fn ingest_all(store, events) -> ApplyReport               // private; ingests, tallies
pub fn handle_response(store, response) -> (ApplyReport, SyncFollowup) // ingest response.events; followup = events_excluding(response.have)
pub fn handle_followup(store, followup) -> ApplyReport
pub fn reconcile<A,B>(a, b, conversation) -> (ApplyReport, ApplyReport) // in-process: build_request→handle_request→handle_response→handle_followup
```
`node::session` (`src-tauri/src/node/session.rs`):
```rust
fn encode(wire: &SyncWire) -> Result<Vec<u8>, SessionError> // bincode DefaultOptions fixint reject_trailing
async fn request_round<S: SyncStore, IO>(channel, store: &Mutex<S>, conversation) -> Result<ApplyReport, SessionError> {
    // build_request (lock) → send Request → recv Response → handle_response (lock) → send Followup → Ok(report)
}
async fn serve_one<S, IO>(channel, store: &Mutex<S>) -> Result<Served, SessionError> {
    // recv → Request: handle_request (lock) → send Response → Handled(conv)
    //        Followup: handle_followup (lock) → Handled(conv)
}
```
`crate::transport::MAX_PLAINTEXT = 65_519`. The file-size cap `MAX_FILE_SIZE = 56 * 1024` is in `src-tauri/src/node/node.rs`.

**Termination + correctness of the loop:** messages are ordered over TCP, so `Request_{N+1}` is processed after `Followup_N` is ingested → `Response_{N+1}.have` reflects it; no event is resent; both `have` sets grow monotonically each round. Terminate when a round makes no inbound progress AND has nothing to push (`report.applied == 0 && followup.events.is_empty()`), with a `MAX_SYNC_ROUNDS` backstop against a peer that resends non-applying events forever.

**CPU/test discipline (MANDATORY):** `nice -n 10`, one filter per run:
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::sync -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::session -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
Confirm `git status | head -1` == `On branch feat/redesign-phase0` after committing.

---

### Task 1: `cap_events` + bounded handlers (sync.rs)

**Files:** Modify `src-tauri/src/eventlog/sync.rs`.

- [ ] **Step 1: imports + consts + sizing helper**

Add near the top imports:
```rust
use crate::transport::MAX_PLAINTEXT;
use bincode::Options;
```
Add module consts (near the other items):
```rust
/// Bytes reserved per sync frame for the message's non-event framing (enum tag,
/// conversation id, bincode wrappers) on top of the event/id payload.
const SYNC_MARGIN: usize = 256;
```
Add private helpers:
```rust
/// The fixint-bincode encoded size of one event (matches `node::session::encode`).
fn encoded_event_size(event: &Event) -> usize {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialized_size(event)
        .map(|n| n as usize)
        .unwrap_or(usize::MAX)
}

/// The longest prefix of `events` whose summed encoded size fits `budget`. Always
/// returns at least the first event when `events` is non-empty (progress guarantee —
/// a single event is bounded by `CHUNK_SIZE` < a frame by construction).
fn cap_events(events: Vec<Event>, budget: usize) -> Vec<Event> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for event in events {
        let size = encoded_event_size(&event);
        if !out.is_empty() && used.saturating_add(size) > budget {
            break;
        }
        used = used.saturating_add(size);
        out.push(event);
    }
    out
}

/// Approximate encoded size of a `have` id-set (`Vec<EventId>`): 8-byte length
/// prefix + 32 bytes per id (fixint). Used to leave room for `have` in a Response.
fn have_set_size(have: &[EventId]) -> usize {
    8 + 32 * have.len()
}
```

- [ ] **Step 2: bounded handlers + delegating wrappers**

Replace `handle_request` with a bounded version + a delegating wrapper:
```rust
/// Answer a request, bounding the returned events so the encoded `SyncResponse`
/// (events + the responder's `have`) stays within `frame_budget` bytes. The
/// requester pulls the remainder over subsequent rounds (it learns the full set
/// from `have`).
pub fn handle_request_bounded(
    store: &impl SyncStore,
    request: &SyncRequest,
    frame_budget: usize,
) -> SyncResponse {
    let requester_have: HashSet<EventId> = request.have.iter().copied().collect();
    let all_missing = store.events_excluding(&request.conversation, &requester_have);
    let responder_have = store.event_ids(&request.conversation);
    let event_budget = frame_budget.saturating_sub(have_set_size(&responder_have) + SYNC_MARGIN);
    let events = cap_events(all_missing, event_budget);
    SyncResponse {
        conversation: request.conversation,
        events,
        have: responder_have,
    }
}

/// Unbounded answer (in-process / single-round callers). Equivalent to
/// [`handle_request_bounded`] with no frame limit.
pub fn handle_request(store: &impl SyncStore, request: &SyncRequest) -> SyncResponse {
    handle_request_bounded(store, request, usize::MAX)
}
```
Replace `handle_response` similarly (the `SyncFollowup` carries only events, so no `have` reservation):
```rust
/// Apply a responder's reply and build a (bounded) follow-up of events the
/// responder is missing.
pub fn handle_response_bounded(
    store: &mut impl SyncStore,
    response: &SyncResponse,
    frame_budget: usize,
) -> (ApplyReport, SyncFollowup) {
    let report = ingest_all(store, &response.events);
    let remote_have: HashSet<EventId> = response.have.iter().copied().collect();
    let all_missing = store.events_excluding(&response.conversation, &remote_have);
    let events = cap_events(all_missing, frame_budget.saturating_sub(SYNC_MARGIN));
    (
        report,
        SyncFollowup {
            conversation: response.conversation,
            events,
        },
    )
}

pub fn handle_response(
    store: &mut impl SyncStore,
    response: &SyncResponse,
) -> (ApplyReport, SyncFollowup) {
    handle_response_bounded(store, response, usize::MAX)
}
```
(Leave `build_request`, `ingest_all`, `handle_followup`, `reconcile` unchanged. `reconcile` keeps calling the unbounded `handle_request`/`handle_response`, so it stays a single round — correct for in-process use with no frame limit.)

- [ ] **Step 3: tests for cap_events + bounded handlers**

Append in the `#[cfg(test)] mod tests` block. Use the existing test helpers in this file to build a store + events (read the existing tests for the exact builder — e.g. how they make an `EventLog` and append events to a conversation). The tests:
```rust
    #[test]
    fn cap_events_returns_prefix_within_budget_but_always_one() {
        // Build N events in a conversation, then check bounding.
        // (Use the file's existing event-builder helper; pseudo-shown here.)
        let (store, conv) = build_store_with_events(5); // helper: 5 events in `conv`
        let all = store.events_excluding(&conv, &HashSet::new());
        assert_eq!(all.len(), 5);
        let one = encoded_event_size(&all[0]);
        // A budget below one event still yields exactly one (progress guarantee).
        assert_eq!(cap_events(all.clone(), 1).len(), 1);
        // A budget for ~2 events yields 2 or 3 (size-dependent), never all 5.
        let capped = cap_events(all.clone(), one * 2 + 8);
        assert!(capped.len() >= 1 && capped.len() < 5);
        // A huge budget yields all.
        assert_eq!(cap_events(all, usize::MAX).len(), 5);
    }

    #[test]
    fn handle_request_bounded_caps_events_but_reports_full_have() {
        let (store, conv) = build_store_with_events(5);
        let request = SyncRequest { conversation: conv, have: vec![] };
        let one = encoded_event_size(&store.events_excluding(&conv, &HashSet::new())[0]);
        // A small frame budget returns fewer events than the full set...
        let resp = handle_request_bounded(&store, &request, one + 8 + 300);
        assert!(resp.events.len() < 5);
        // ...but always the FULL have id-set, so the requester knows to come back.
        assert_eq!(resp.have.len(), 5);
        // Unbounded returns everything.
        assert_eq!(handle_request(&store, &request).events.len(), 5);
    }
```
If the file has no `build_store_with_events` helper, write a minimal local one in the test module mirroring how existing tests construct events (sign with a generated `DeviceIdentity`, append to an `EventLog` via its append/ingest API, return `(EventLog, ConversationId)`). Keep events small; assert relative bounds (`< 5`, `>= 1`), not exact byte counts.

- [ ] **Step 4: run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib eventlog::sync -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/eventlog/sync.rs
git commit -m "feat(sync): cap_events + bounded handle_request/handle_response (unbounded wrappers preserved)"
git status | head -1
```
Expected: all existing `eventlog::sync` tests STILL pass (the wrappers preserve behavior) + the new bounded tests pass; clippy clean.

---

### Task 2: multi-round `request_round` + bounded `serve_one` (session.rs)

**Files:** Modify `src-tauri/src/node/session.rs`.

- [ ] **Step 1: imports + round backstop**

Add to imports: `use crate::eventlog::sync::{handle_request_bounded, handle_response_bounded};` (keep existing `build_request`/etc imports; remove now-unused `handle_request`/`handle_response` imports if they become unused — clippy will tell you). Add `use crate::transport::MAX_PLAINTEXT;`. Add a const:
```rust
/// Backstop on sync rounds for one conversation (a peer that resends
/// non-applying events forever can't spin us indefinitely). Generous: a round
/// transfers up to ~one frame, so this bounds a single conversation's transfer.
const MAX_SYNC_ROUNDS: usize = 10_000;
```

- [ ] **Step 2: loop `request_round`**

Replace the body of `request_round` with a multi-round loop that accumulates the report and terminates on no-progress:
```rust
pub async fn request_round<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
) -> Result<ApplyReport, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let mut total = ApplyReport::default();
    for _ in 0..MAX_SYNC_ROUNDS {
        let request = {
            let store = store.lock().expect("store mutex not poisoned");
            build_request(&*store, conversation)
        };
        channel.send(&encode(&SyncWire::Request(request))?).await?;

        let response = match decode(&channel.recv().await?)? {
            SyncWire::Response(r) => r,
            _ => return Err(SessionError::UnexpectedMessage),
        };

        let (report, followup) = {
            let mut store = store.lock().expect("store mutex not poisoned");
            handle_response_bounded(&mut *store, &response, MAX_PLAINTEXT)
        };
        let made_progress = report.applied > 0;
        let more_to_push = !followup.events.is_empty();

        channel
            .send(&encode(&SyncWire::Followup(followup))?)
            .await?;

        total.applied += report.applied;
        total.duplicates += report.duplicates;
        total.rejected.extend(report.rejected);

        if !made_progress && !more_to_push {
            break;
        }
    }
    Ok(total)
}
```

- [ ] **Step 3: bound `serve_one`'s Response**

In `serve_one`, change the Request arm to use the bounded handler:
```rust
        SyncWire::Request(request) => {
            let conversation = request.conversation;
            let response = {
                let store = store.lock().expect("store mutex not poisoned");
                handle_request_bounded(&*store, &request, MAX_PLAINTEXT)
            };
            channel
                .send(&encode(&SyncWire::Response(response))?)
                .await?;
            Ok(Served::Handled(conversation))
        }
```
(The Followup arm is unchanged — `handle_followup` ingests whatever it's given.)

- [ ] **Step 4: loopback test — a `>`-frame conversation syncs fully**

Add to `node/session.rs`'s `#[cfg(test)] mod tests` (mirror the existing session loopback tests' setup — an in-memory duplex or `tokio::io::duplex`, two `EventLog`s, a spawned `serve` loop). The test seeds the responder with enough events that their combined size exceeds `MAX_PLAINTEXT` (e.g. 4 events each carrying a ~20 KB ciphertext → ~80 KB total > 64 KB), runs `request_round` once (which now loops internally), and asserts the requester ends up with ALL events:
```rust
    #[tokio::test]
    async fn request_round_syncs_a_conversation_larger_than_one_frame() {
        // Build a responder store holding several large events in `conv` whose
        // total encoded size exceeds MAX_PLAINTEXT, and an empty requester store.
        // Drive request_round (requester) against a spawned serve loop (responder)
        // over tokio::io::duplex, then assert the requester now has every event.
        // Use the existing session-test scaffolding for channel setup + the event
        // builder used elsewhere; make ~4 events of ~20 KB payload each.
        // assert_eq!(requester.event_ids(&conv).len(), responder.event_ids(&conv).len());
    }
```
Follow the EXACT scaffolding of the existing session loopback test in this file (channel construction, store-in-`Mutex`, spawning the serve loop). Build large events using the same event constructor the file already uses; assert the requester's id-set equals the responder's after one `request_round`. If the existing session tests use a `SecureChannel` over `tokio::io::duplex`, reuse that; if they use a TCP pair, reuse that.

- [ ] **Step 5: run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::session -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2   # DM/channel/file rigs still green
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/session.rs
git commit -m "feat(session): multi-round request_round + bounded serve_one; >frame conversations sync"
git status | head -1
```
Expected: the new >frame test passes; ALL existing DM/channel/file loopback rigs still pass (they now do 1-2 rounds); clippy clean.

---

### Task 3: raise `MAX_FILE_SIZE` + a multi-chunk file-transfer rig (node.rs)

**Files:** Modify `src-tauri/src/node/node.rs`.

- [ ] **Step 1: raise the cap**

Change `MAX_FILE_SIZE` and update its doc comment:
```rust
/// Maximum raw file size for `send_file_*`. Bounded multi-round sync transfers a
/// file's chunk events across frames now, so the practical limit is the `have`
/// id-set size (a conversation's full id-set still travels in one frame): ~8 MB of
/// 48 KiB chunks ≈ 180 ids ≈ 7 KB, comfortably within a frame. Lifting further
/// needs id-set compaction (a separate slice).
const MAX_FILE_SIZE: usize = 8 * 1024 * 1024;
```

- [ ] **Step 2: a multi-chunk (multi-round) transfer rig**

Append to `node/node.rs`'s `#[cfg(test)] mod tests`, mirroring `two_nodes_transfer_a_file_over_loopback_tcp` but with a payload spanning several chunks (so the transfer requires multiple sync rounds):
```rust
    #[tokio::test]
    async fn two_nodes_transfer_a_multi_chunk_file_over_loopback_tcp() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
        let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());

        let (a_dm, _a) = mpsc::unbounded_channel();
        let (a_ch, _b) = mpsc::unbounded_channel();
        let (a_f, _c) = mpsc::unbounded_channel();
        let (b_dm, _d) = mpsc::unbounded_channel();
        let (b_ch, _e) = mpsc::unbounded_channel();
        let (b_f, mut b_f_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(alice, alice_roster, a_dm, a_ch, a_f,
            &dir.path().join("a.log"), &dir.path().join("a-sent.log"), "pw").unwrap();
        let bob_node = Node::open(bob, bob_roster, b_dm, b_ch, b_f,
            &dir.path().join("b.log"), &dir.path().join("b-sent.log"), "pw").unwrap();
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        // ~5 chunks → the file_conv batch far exceeds one frame → multiple rounds.
        let payload = vec![0x5Au8; crate::file::CHUNK_SIZE * 4 + 999];
        let src = dir.path().join("big.bin");
        std::fs::write(&src, &payload).unwrap();

        let bob_uid = bob_node.user_id();
        let file_conv = alice_node.send_file_dm(&bob_uid, &src).await.unwrap();

        let rf = tokio::time::timeout(std::time::Duration::from_secs(10), b_f_r.recv())
            .await.expect("received within 10s").expect("stream open");
        assert_eq!(rf.size, payload.len() as u64);

        // Saving may need a brief retry while the file_conv chunks finish syncing.
        let dest = dir.path().join("big-saved.bin");
        let mut saved = false;
        for _ in 0..100 {
            if bob_node.save_file(rf.file_conv, &dest).is_ok() { saved = true; break; }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(saved, "bob saved the multi-chunk file");
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        let _ = file_conv;
    }
```

- [ ] **Step 3: run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs
git commit -m "feat(node): raise file cap to 8 MB now multi-round sync spans frames; multi-chunk rig"
git status | head -1
```

---

## Notes for the reviewer / next plan

- **Delivered:** conversations sync across frames via bounded multi-round reconciliation; files up to 8 MB transfer. The bounded handlers are additive (unbounded wrappers preserve `reconcile` + all prior tests). `serve_one` is unchanged structurally (its loop already serves repeated messages); only the Response is bounded.
- **Reviewer checks:** the loop terminates (no-progress break + `MAX_SYNC_ROUNDS` backstop); no event is resent (Request.have grows each round; ordering over TCP makes `Response_{N+1}.have` reflect `Followup_N`); `cap_events` always returns ≥1 event (progress) and never exceeds the frame once `have`+margin are reserved; no `MutexGuard` across `.await` in `request_round` (store locked only for `build_request`/`handle_response_bounded`, dropped before send/recv); the existing DM/channel/file rigs still pass.
- **Deferred:** the `have` id-set still travels in one frame, so a conversation with a very large id-set (tens of thousands of events) is the next limit — needs id-set compaction / ranged sync. On-demand chunk fetch (vs eager push) also remains. Then File Plan 3 (UI) can handle real files.
