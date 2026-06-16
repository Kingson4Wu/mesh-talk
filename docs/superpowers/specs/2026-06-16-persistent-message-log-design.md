# Persistent Message Log — Design Spec

Date: 2026-06-16
Status: Approved for planning

## 1. Purpose & scope

Today a redesign node keeps its messages in an in-memory `EventLog`: identity persists (keystore) and the post office persists its relay log, but a normal node's conversation history is **lost on exit**. This slice makes a node's message log durable so history survives a restart, and adds an on-demand `/history` view.

**This slice — "persistent message log":** swap the node's in-memory `EventLog` for the durable `PersistentEventLog` (the primitive the post office already uses), add a local-only encrypted "sent log" sidecar so a node can show the plaintext of messages it sent (which it cannot decrypt back from the sealed event log), and add a `/history <peer> [n]` command. No change to the synced wire format or the E2E crypto.

**Out of scope (later slices):** channels/groups persistence; cross-device sync of sent history; full-text search; retention/pruning/compaction; a paginated history query API for the UI; migrating the Tauri UI (separate slice). Auto-replaying history to stdout on startup is explicitly rejected (`/history` is the on-demand viewer; startup stays quiet).

## 2. The sealing constraint (why a sidecar is needed)

An outbound DM is sealed **to the recipient** (`dm::seal(sender, recipient_x25519, plaintext)`); only the recipient can `dm::open` it. The sender cannot decrypt its own sent message back from the stored (sealed) event — the per-message ephemeral secret is discarded after sealing, and the sender never holds the recipient's static secret. Therefore the persisted event log alone can reconstruct **received** plaintext (decryptable) but not **sent** plaintext.

To show both sides of a conversation, the node keeps a **local-only, password-encrypted sent log** recording the plaintext of messages it sends, keyed by conversation. This never leaves the device and is never served to peers; it is the deliberate cost of E2E sealing-to-recipient. (Alternatives considered and deferred: seal-to-self / dual-recipient sealing, which would change the DM crypto and the recipient's envelope.)

## 3. Components

### 3.1 `node::Node` — durable event log
- `Node.log` changes from `Mutex<EventLog>` to `Mutex<PersistentEventLog>`.
- `Node::new(...)` is replaced by `Node::open(identity, roster, incoming, log_path: &Path, sent_path: &Path, password: &str) -> Result<Arc<Self>, LogError>`. It opens (or creates) the persistent event log and the sent-log sidecar.
- All existing call sites are unchanged: `PersistentEventLog` exposes the same `prepare`/`version_vector`/`append`/`events`/`has` methods and the same `SyncStore` impl, so `send_dm`, `drain_from_post_office`, `serve_connection`/`run_accept_loop`, and `emit_new_messages` compile and behave identically against the durable log.

### 3.2 `node::sentlog` (new) — local sent-plaintext sidecar
- A small append-only, password-encrypted store of records `{ conversation: ConversationId, seq: u64, wall_clock: u64, plaintext: Vec<u8> }`.
- API: `SentLog::open(path: &Path, password: &str) -> Result<SentLog, LogError>`; `record(&mut self, conversation, seq, wall_clock, plaintext: &[u8]) -> Result<(), LogError>` (append + persist); `entries(&self, conversation: &ConversationId) -> Vec<SentEntry>` (in-memory, sorted by `(wall_clock, seq)`).
- **Reuse, don't reinvent:** the implementation reuses the existing encrypted append-only file primitive that backs `PersistentEventLog` (`eventlog::persist`'s `LogFile` / the `storage`+`crypto` password-keyed AES-256-GCM record format), not a new cipher. The exact reuse mechanism is a plan decision; the boundary is: an encrypted file on disk, an in-memory index loaded on open.
- Local-only: never serialized to the wire, never served via `SyncStore`, no signatures (it is not a synced event).

### 3.3 `Node::history` — merged conversation view
- `pub fn history(&self, conversation: ConversationId, limit: usize) -> Vec<HistoryEntry>` where `HistoryEntry { from_me: bool, who: String, text: Vec<u8>, wall_clock: u64 }`.
- **Received** entries: peer-authored `Message` events in the persistent log for `conversation`, decrypted via `open_dm_event` (roster lookup); `who` = peer name, `from_me = false`.
- **Sent** entries: `sentlog.entries(conversation)`; `who` = "you", `from_me = true`, `text` = stored plaintext.
- Merge both, sort by `(wall_clock, …)`, return the last `limit`.

### 3.4 `bin/mesh-talk-node.rs` — wiring + `/history` command
- The normal-node path opens the node via `Node::open` at `<keystore-dir>/messages.log` (event log) and `<keystore-dir>/sent.log` (sidecar), under `args.password`. (The `--post-office` path is unchanged — a relay has no user history.)
- New REPL command `/history <peer-prefix> [n]` (default n = 20): resolve the peer prefix against the roster (reusing `resolve_recipient`), derive the conversation id, call `Node::history`, and print each entry as `from you: <text>` or `from <name> (<uid8>): <text>` with a wall-clock timestamp. Unknown/ambiguous prefix → the same usage messages as `/msg`.

## 4. Data flow

**Send** (`/msg bob hi`): seal → append the `Message` event to the persistent log (as today) → **also** `sentlog.record(conv, seq, now_millis, b"hi")`. (A sidecar write error is logged, non-fatal: the message was sent and is in the synced log.)

**Restart:** `Node::open` loads `messages.log` (restoring history + enabling continued sync/serve) and `sent.log`. It **seeds the `emitted` set with every `Message` event id already in the log**, so restored history is not re-streamed to stdout — only messages received/pulled *after* startup print live. The node resumes discovery, the accept loop, and the periodic PO drain normally.

**History** (`/history bob 5`): resolve `bob` → `conv = dm_conversation_id(self, bob)` → `Node::history(conv, 5)` merges decrypted-received + sidecar-sent, sorted by time → print the last 5.

## 5. Error handling
- **Open failure** (wrong password, corrupt/incompatible file): `Node::open` returns `Err(LogError)`; the binary exits with a clear message (same posture as the keystore / post office today).
- **Sidecar write failure** on send: logged via `emit`, non-fatal — the DM was sealed, appended to the synced log, and delivered/replicated; only the local sent-history copy is missing.
- **Decryption** of received history: fail-closed per event (an undecryptable event is skipped), unchanged from `open_dm_event`.
- **No re-stream** on restart is a correctness requirement, not best-effort: the `emitted` seed must cover all loaded `Message` events.

## 6. Testing
- **Unit (sidecar):** `SentLog` round-trips across `open → record → drop → reopen → entries` (encrypted on disk, survives restart); `entries` returns only the requested conversation, ordered.
- **Unit (node):** `Node::open` seeds `emitted` so a reopened node with prior history does NOT emit that history on a subsequent serve/drain, but DOES emit a genuinely-new event; `history()` merges sent + received in time order and respects `limit`.
- **Integration (`#[ignore]`d, CLI harness):** two nodes exchange a DM (A sends, B receives); A exits and restarts on the same keystore; A's `/history <b> ` shows BOTH the sent line and (if B replied) the received line — proving persistence across a real process restart. Bounded timeouts, poll-to-converge, `#[ignore]`d (real processes); the unit tests carry the fast proof.
- **CI:** existing gates carry over (clippy `-D warnings`, fmt, tests), CPU-throttled (`nice` + `--test-threads=2`); the heavy rig is `#[ignore]`d.

## 7. Module boundaries & dependencies
- `node::sentlog` depends only on the encrypted-file primitive (`eventlog::persist`/`storage`/`crypto`) and `eventlog::event::ConversationId` — no transport/discovery/dm coupling.
- `node::Node` gains a dependency on `eventlog::persist::PersistentEventLog` (already a crate module) and `node::sentlog`.
- `bin/mesh-talk-node` gains the `/history` command and the `Node::open` paths; no new third-party dependencies.

## 8. Accepted limitations (this slice)
- The local sent log stores your sent plaintext (encrypted at rest) — the cost of sealing-to-recipient; seal-to-self and cross-device sent-history sync are deferred.
- DMs only — channel/group history is a later slice.
- No retention/pruning/compaction; logs grow unbounded (fine at LAN-team / Phase-0 scale).
- No paginated/streaming history query API for a UI — `/history <peer> [n]` (last-N) only; the Tauri UI slice will build a richer query on the same persisted log.
- The `Node::new → Node::open` signature change ripples to the in-crate node tests (each gains a tempdir + password) — bounded, expected churn.
