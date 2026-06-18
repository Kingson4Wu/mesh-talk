# Bounded / Multi-Round Sync Design

**Status:** Derived from the master redesign spec (sync protocol §5) + the file-sharing finding. Authored autonomously against the north-star.

## Problem

`eventlog::sync` returns ALL of a conversation's missing events in one `SyncResponse`/`SyncFollowup`, which `node::session` sends as a single transport frame. Frames are capped at `MAX_PLAINTEXT` (65,519 B). So a conversation whose missing-event batch exceeds one frame cannot sync — `channel.send` errors. File transfer is the first feature to hit this (it's currently capped at 56 KiB). There is a standing TODO at `sync.rs:128` to cap batch size (also an unbounded-batch DoS vector).

## Approach: bounded batches + multi-round reconciliation

Cap each `SyncResponse.events` / `SyncFollowup.events` at a byte budget below `MAX_PLAINTEXT`, and loop the Request→Response→Followup exchange until the conversation is fully reconciled. The responder's `serve_one` loop already handles repeated inbound messages on one connection, so only the requester's `request_round` needs to loop; `serve_one` just bounds the Response it builds.

**Why this works (termination + no duplication):** messages travel in order over TCP. Per round the requester sends `Request(have)` then `Followup(events the responder lacks)`; the responder answers each `Request` with a bounded `Response` and ingests each `Followup`. Because `Request_{N+1}` arrives after `Followup_N` is ingested, `Response_{N+1}.have` already reflects `Followup_N` — so the requester never resends the same events, and each round strictly grows both `have` sets. Terminate when a round transfers nothing in either direction (`Response.events` empty AND `Followup.events` empty).

**Budgeting:** the `SyncResponse` carries `events` + the responder's full `have` id-set, so its event budget is `MAX_PLAINTEXT − encoded(have) − margin`. The `SyncFollowup` carries only `events`, so its budget is `MAX_PLAINTEXT − margin`. `cap_events` returns the longest prefix of the (topologically-ordered) missing events whose summed encoded size fits the budget, always ≥ 1 event for progress (a single event always fits because chunk events are bounded by `CHUNK_SIZE` = 48 KiB < frame).

**Known residual limit (out of scope, note in code):** the `have` id-set itself is unbounded — a conversation with tens of thousands of events has a `have` set exceeding a frame, which neither bounded events nor this round-loop fixes (it needs id-set compaction / ranged sync). For files this is fine: raising `MAX_FILE_SIZE` to 8 MB yields ≲ ~180 chunk ids (`have` ≈ 7 KB ≪ frame). Lifting beyond a few MB needs the `have`-set fix.

## Scope

- **In:** `cap_events` + bounded `handle_request`/`handle_response` variants (the existing unbounded fns become `usize::MAX` wrappers, so `reconcile` + existing tests are untouched); `request_round` loops to convergence; `serve_one` bounds its Response; raise `MAX_FILE_SIZE` to 8 MB; tests — a `>`-frame conversation reconciles fully (in-process loop + loopback), and a multi-chunk (multi-round) file transfer.
- **Deferred:** `have`-set compaction / ranged sync for very large conversations; on-demand chunk fetch.

## Decomposition (one plan, three tasks)

1. `cap_events` pure helper + `handle_request_bounded`/`handle_response_bounded` + budget consts (unbounded fns delegate). Tests.
2. `request_round` multi-round loop + `serve_one` uses the bounded responder. Loopback test syncing a `>`-frame conversation.
3. Raise `MAX_FILE_SIZE` to 8 MB; multi-chunk (multi-round) file-transfer loopback rig.
