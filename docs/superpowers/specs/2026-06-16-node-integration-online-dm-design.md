# Node Integration — "Online Direct DM" Vertical Slice — Design Spec

Date: 2026-06-16
Status: Approved for planning

## 1. Purpose & scope

The Phase-0 redesign built six primitives in isolation (`identity`, `transport` Noise channel, `eventlog` store + `sync`, `dm` sealed box, `postoffice`). This spec wires the first of them into a **runnable node** validated by the two-CLI-on-localhost rig the redesign spec calls for.

**This slice — "online direct DM":** two CLI nodes discover each other, establish an authenticated `SecureChannel`, and exchange a 1:1 direct message **directly, while both are online**. It proves that `identity → discovery → transport → sync → dm` compose end-to-end.

**Strategy:** build a **fresh, parallel, CLI-first node stack**. The legacy Tauri network stack (`services/node_service.rs`, `network/*`, the `mesh-talk-cli` binary) is **left untouched** — it still serves the current UI. The redesign UI migrates onto this new `node` API in later work, out of scope here.

**Explicitly out of scope (later slices):** the post office / offline delivery; a persistent message log; channels/groups; migrating the Tauri UI; reconnection/standbys.

## 2. Components

Three new units, each with one responsibility and a clean interface.

### 2.1 `discovery` module (`src-tauri/src/discovery/`)
Replaces the legacy plaintext UDP discovery with **signed announces** that carry identity keys.

- **`announce.rs` — `Announce`**: `{ user_id: String, ed25519_pub: [u8;32], x25519_pub: [u8;32], name: String, tcp_port: u16, sig: Vec<u8> }`, serde-serializable. `Announce::new(&DeviceIdentity, name, tcp_port)` builds and Ed25519-signs over a domain-separated serialization of all fields except `sig`. `verify(&self) -> bool` recomputes and checks the signature against `ed25519_pub`, and checks `user_id == PublicIdentity::user_id_from(&ed25519_pub)` (the fingerprint must match the advertised key). A forged or key-mismatched announce fails `verify`.
- **`roster.rs` — `Roster`**: `HashMap<UserId, PeerRecord>` where `PeerRecord { public: PublicIdentity, addr: SocketAddr, last_seen: Instant }` and `UserId = String` (the fingerprint). `update(announce, source_ip)` verifies the announce, forms `addr = (source_ip, announce.tcp_port)`, and inserts/refreshes. `get(user_id)`, `peers() -> Vec<PeerRecord>`, and time-out eviction of stale records. The roster is the `user_id → (keys, addr)` map the node needs both to **dial** a peer and to **look up a recipient's x25519** for sealing and an author's x25519 for opening.
- **`service.rs`**: two loops over a UDP socket (reusing the legacy socket *pattern*, not its code) — a **broadcast loop** that periodically sends the local node's signed `Announce` to the broadcast address, and a **listen loop** that receives announces, **self-filters** (ignores its own `user_id`), verifies, and feeds the `Roster`. Self-filtering by `user_id` is robust regardless of local-IP quirks.

### 2.2 `node` module (`src-tauri/src/node/`)
The explicit **App context** — owns all state, no global singletons.

- **`conversation.rs`**: `dm_conversation_id(a: &PublicIdentity, b: &PublicIdentity) -> ConversationId` = a hash over the **sorted pair** of the two `ed25519_pub`s (domain-separated), so both peers independently derive the **same** conversation id for their DM.
- **`session.rs`**: per-peer `SecureChannel` management and the **networked sync driver**. The driver is the network counterpart of Plan-4's in-process `reconcile`: it serializes Plan-4's `SyncRequest`/`SyncResponse`/`SyncFollowup` (bincode) and sends/receives them over a `SecureChannel` (`channel.send`/`channel.recv`), invoking the existing Plan-4 handlers (`build_request`, `handle_request`, `handle_response`, `handle_followup`) on each side against the local `EventLog`. A dial path (`SecureChannel::connect(stream, identity, Some(expected_peer))`) and an accept path (`SecureChannel::accept`) both yield an authenticated channel keyed by the verified peer identity.
- **`node.rs`**: `Node { identity: DeviceIdentity, log: EventLog, roster: Arc<Mutex<Roster>> (shared with discovery's listen loop), sessions: map of UserId → active SecureChannel }`. The local node derives its **own next `seq`** for a conversation from the event log's version vector for its own author (`version_vector(conv).get(self_author).unwrap_or(0) + 1`). Public API the CLI calls:
  - `send_dm(recipient_user_id, text) -> Result<...>`: roster-lookup recipient → `dm::seal` → `log.prepare(conv)` → `Event::new(Message, sealed)` → `log.append` → ensure a `SecureChannel` to the recipient (dial if needed) → run the sync driver for `conv`.
  - `incoming() -> stream of (sender_user_id, plaintext)`: events ingested from peers are scanned for new `Message` events; each is decrypted by looking up the **author's x25519 via the roster** and calling `dm::open`; the result is emitted to the stream.
  - `public()`/`user_id()` for display; `roster_snapshot()` for `/peers`.

The peer's message log is the **in-memory `EventLog`** in this slice (durability arrives with the offline slice). The identity, however, IS persistent (keystore).

### 2.3 `mesh-talk-node` CLI (`src-tauri/src/bin/mesh-talk-node.rs`)
A thin client over `node.rs`. New binary; the legacy `mesh-talk-cli` is unchanged.

- **Args**: `--keystore <path> --password <pw> --name <name> [--port <tcp_port>]`. Startup: `keystore::load_or_create(path, password)` → build `Node` → start discovery (broadcast + listen) and the TCP listener → print the node's own `user_id` and listen port.
- **Commands** (line-based): `/peers` (list roster: user_id, name, addr), `/msg <user_id-prefix> <text>` (send a DM; prefix-match against the roster), `/quit`. Incoming DMs print as `from <user_id> (<name>): <text>`.

## 3. Data flow

**Send** (`/msg bob hi`):
1. `roster.get(bob)` → Bob's `PublicIdentity` (x25519) + `addr`.
2. `conv = dm_conversation_id(me.public(), bob_public)`.
3. `sealed = dm::seal(&identity, &bob_public.x25519_pub, b"hi")`.
4. `(parents, lamport) = log.prepare(&conv)`; `event = Event::new(&identity, conv, next_seq, parents, lamport, now_millis, EventKind::Message, sealed)`; `log.append(event)`.
5. Ensure a `SecureChannel` to Bob's `addr` (dial + Noise handshake verifying Bob's identity); run the **sync driver** for `conv` over that channel.

**Receive** (Bob): TCP listener → `SecureChannel::accept(stream, identity)` (authenticated peer) → read a framed sync message → dispatch to `handle_request` (→ send `SyncResponse`) / `handle_followup` (ingest) → after ingest, scan the conversation for new `Message` events → for each, derive the author's `user_id` from `event.author`, look it up in the roster for the x25519 key, `dm::open(&identity, &author_x25519, &event.ciphertext)` → emit `(author_user_id, plaintext)` → CLI prints.

**Layered authentication:** the `SecureChannel` proves the peer holds its claimed identity (Noise + Ed25519 binding); the event's Ed25519 signature proves message authorship; the roster binds author → x25519 for decryption.

**Sync trigger model:** a sync round is **initiated by the side that has new events** — i.e., the sender runs one round (`build_request → … → handle_followup`) right after `log.append`, and one round when a `SecureChannel` is first established (catch-up). There is no tight polling loop; an optional slow periodic re-sync is a later refinement, not part of this slice.

## 4. Error handling
- **Discovery**: malformed, unsigned, or key-mismatched announces are dropped; own announce self-filtered.
- **Transport**: `SecureChannel` handshake failure → drop the connection, retry on the next dial/announce.
- **Sync**: Plan-4 handlers already fail closed; rejected (forged/incomplete) events are tallied, not fatal.
- **DM open**: a `Message` whose author isn't in the roster yet, or that fails to decrypt, is skipped (logged) — it can be re-derived on a later sync once discovery completes.
- All paths are fail-soft: a single bad packet, peer, or event never crashes the node.

## 5. Testing
- **Unit:** announce sign/verify (+ tamper/key-mismatch rejection) and roster build/self-filter/eviction; `dm_conversation_id` determinism and symmetry (both orders → same id); the **sync driver over an in-memory duplex** (`tokio::io::duplex`, the Plan-2/4 pattern) — two `Node`s converge a DM conversation without real sockets.
- **Integration (the two-CLI rig):** spawn two `mesh-talk-node` processes on localhost (each with its own keystore/temp dir), let them discover each other, have A `/msg` B, and assert B prints the decrypted message. This is the Phase-0 validation rig for the online path.
- **CI:** the existing gates carry over (clippy `-D warnings`, fmt, cargo-deny, tests), CPU-throttled (`nice` + `--test-threads=2`).

## 6. Module boundaries & dependencies
- `discovery` depends on `identity` (sign/verify) only — no transport or log coupling.
- `node` depends on `identity`, `discovery` (roster), `transport` (SecureChannel), `eventlog` (store + sync), `dm`.
- `mesh-talk-node` (bin) depends on `node` + `identity::keystore` + `clap` + `tokio`.
- No new third-party dependencies are anticipated (UDP via `tokio`/`socket2` already present; the legacy `network/utils::get_preferred_local_ip` pattern may be reused for the broadcast address).

## 7. Decomposition into plans
This slice is sizeable; it is expected to become **two implementation plans**:
1. **Discovery + transport wiring** — the `discovery` module (announce/roster/service) and the node's TCP listener + dialer producing authenticated `SecureChannel`s and a live roster.
2. **Sync-over-channel + node API + CLI** — the networked sync driver, `conversation.rs`, `node.rs` (`send_dm`/`incoming`), and the `mesh-talk-node` binary + the two-CLI integration rig.

Each plan produces independently testable software.

## 8. Accepted limitations (this slice)
- Online-only: no offline delivery / post office yet (next slice).
- In-memory message log (no persistence across restarts for messages; identity persists).
- 1:1 DMs only (no channels/groups).
- Single conversation focus per `/msg`; no multi-conversation UI beyond the roster + inbound print.
- Discovery on the local broadcast domain only (LAN), matching the redesign's LAN-only scope.
