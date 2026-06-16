# Offline Post-Office Delivery — Integration Slice — Design Spec

Date: 2026-06-16
Status: Approved for planning

## 1. Purpose & scope

The "online direct DM" slice (Plans 1–3) made two running nodes exchange a 1:1 DM **while both are online**. Phase 0 also built the `PostOffice` *primitive* — an elected always-on peer that stores-and-forwards recipient-sealed events it can never decrypt (`src-tauri/src/postoffice/`), proven by an **in-process** test (`offline_dm_delivered_via_post_office`). This slice wires that primitive into the **running, networked system** so a DM sent while the recipient is offline is delivered later, over real processes.

**This slice — "offline post-office delivery":** a node started with `--post-office` runs as a durable, discoverable relay. Normal nodes always replicate each outgoing DM to the elected post office and periodically drain their conversations from it, so a recipient that was offline at send time retrieves the message when it comes back online.

**Strategy:** extend the existing `mesh-talk-node` binary and the `node`/`discovery` modules — no new appliance binary. The `PostOffice` primitive, the sync engine, `Node::send_dm`, and the session driver are reused unchanged where possible; the session driver is already generic over `SyncStore`, which `PostOffice` implements.

**Out of scope (later slices):** "sender also offline at delivery" (requires the recipient to obtain a never-co-present sender's keys); roster/identity persistence across restart; a post-office identity directory; post-office standbys / hot failover; channels/groups; migrating the Tauri UI.

## 2. The identity-availability boundary (why the scenario is bounded)

The post office stores **ciphertext**, not identities. To decrypt a DM pulled from the PO, the recipient needs the sender's identity keys: the sender's `ed25519_pub` to derive `dm_conversation_id(sender, recipient)`, and the sender's `x25519_pub` to open the sealed box. Those keys come from **discovery** (the sender's signed `Announce` in the recipient's roster), not from the post office.

Therefore the delivery scenario this slice proves is precisely:

> The recipient is offline when the sender sends → the sender's direct dial fails and it replicates the DM to the post office → the recipient comes back online, **re-discovers the sender (and the post office) via the normal announce broadcast**, and its drain loop pulls the held DM from the post office and decrypts it.

The post office provides **message durability**; discovery provides **identity**. The case where the sender is *also* offline at the moment of delivery (so the recipient cannot re-discover it) is explicitly deferred — it requires identity persistence or a PO identity directory (§7).

## 3. Components

### 3.1 `discovery::announce` — a signed post-office role
- Add `pub post_office: bool` to `Announce`. It is included in the domain-separated serialization that `Announce::new` signs and `verify()` checks, so the role is **authenticated and unforgeable** — a peer cannot claim (or be spoofed into) the post-office role without its key.
- `Announce::new(identity, name, tcp_port)` keeps its signature and sets `post_office = false`; add `Announce::new_post_office(identity, name, tcp_port)` (or a `post_office: bool` parameter on a shared builder) that sets it `true`. The chosen form is an implementation detail for the plan; both call sites (normal node, `--post-office` node) must be covered.
- Wire-stability: `post_office` is appended to the signed byte layout in a fixed position; a tamper or role-flip fails `verify()`.

### 3.2 `discovery::roster` — exposing the role
- `PeerRecord` gains `pub post_office: bool`, populated from the verified announce in `Roster::update`.
- `Roster::post_offices(&self) -> Vec<PeerRecord>` returns the PO-flagged peers (for election).

### 3.3 `node::postbox` (new module) — election + replication + drain
- `elected_post_office(roster: &Roster) -> Option<PeerRecord>`: runs the existing `postoffice::election::elect` over the `public()` of the roster's `post_offices()`, returning the matching `PeerRecord` (keys + addr) or `None` if no PO is known. Deterministic lowest-fingerprint.
- The **sender-replicate** step (a function the `Node` calls inside `send_dm`): given the elected PO's `PeerRecord` and a conversation, dial the PO (peer-pinned Noise) and run one `request_round` for that conversation against the local log, pushing the just-appended event into the PO.
- The **drain loop** (an async task the binary spawns on a normal node): every N seconds (and once at startup), snapshot the roster; for each known non-self peer, derive `dm_conversation_id(me, peer)`; if a PO is elected, dial it and `request_round` that conversation against the local log; then `emit_new_messages(conv)` to surface any newly-pulled, decryptable DMs. Fail-soft per peer/round.

### 3.4 `node::Node` — replicate on send; best-effort direct
- `send_dm` keeps: seal → append locally → (best-effort) direct dial + `request_round` to the recipient. It then adds: if `elected_post_office(roster)` is `Some`, dial the PO and `request_round` the conversation (replicate).
- **Success semantics change:** a `send_dm` succeeds if the event was appended locally and **either** the direct delivery **or** the PO replication succeeded (PO preferred). A failed direct dial to an offline recipient is no longer a `send_dm` error when a PO holds the event. If neither path is available (no PO and direct dial failed), `send_dm` returns an error.
- The node continues to expose `serve_connection` over its store for the inbound direct path (unchanged).

### 3.5 `bin/mesh-talk-node.rs` — `--post-office` mode + drain task
- New flag `--post-office`. When set: build a `PostOffice::open(<keystore-dir>/relay.log, password, identity)` as the served store, announce with `post_office = true`, and run the accept loop serving sync rounds over the `PostOffice` (which is a `SyncStore`). A PO-mode node does **not** run the DM REPL send path or the drain loop — it is a pure relay. It still prints a startup line and `/peers`/`/quit` for operability.
- When **not** set (normal node): announce with `post_office = false` and additionally spawn the periodic drain task (§3.3).

## 4. Data flow

**Send** (`/msg bob hi`, Bob offline):
1. `append` the sealed `Message` event locally.
2. Best-effort: dial Bob directly + `request_round` — fails (Bob offline), tallied, not fatal.
3. `elected_post_office(roster)` → the PO's `PeerRecord`; dial it (peer-pinned) + `request_round` the conversation → the PO `accept`s/ingests the signed event. `send_dm` succeeds (the PO holds it).

**Drain** (Bob, after coming back online; every N s + at startup):
1. Bob's announce broadcast resumes; he re-discovers Alice and the PO (roster repopulated).
2. For each known peer (incl. Alice): `conv = dm_conversation_id(bob, peer)`; dial the elected PO + `request_round(conv)` → Alice's held event flows into Bob's log.
3. `emit_new_messages(conv)` → looks up Alice's `x25519` in the roster → `dm::open` → emits `ReceivedDm` → the CLI prints `from <alice> (Alice): hi`.

**Layered authentication (unchanged):** the `SecureChannel` proves the PO holds its claimed identity; each event's Ed25519 signature proves authorship; the PO validates every event on ingest and holds only recipient-sealed ciphertext.

## 5. Error handling
- **No PO discovered:** `elected_post_office` returns `None`; send replicates nothing (direct-only) and drain is a no-op — no error.
- **PO unreachable at send:** replication round fails; if direct delivery also failed and no PO copy exists, `send_dm` errors; otherwise it succeeds and the drain/next send retries.
- **PO unreachable at drain:** the tick fails soft; the next tick retries.
- **Forged/oversized events at the PO:** rejected by `PostOffice::accept`'s validation (signature/integrity/parents); tallied, never fatal.
- **Undecryptable pulled event** (sender not yet re-discovered): skipped by `open_dm_event` (returns `None`); re-attempted on a later drain once discovery completes — consistent with the existing convergence behavior.

## 6. Testing
- **Unit:** `Announce` post-office role round-trips through sign/verify; a flipped `post_office` bit or tampered field fails `verify`. `elected_post_office` is deterministic and picks the lowest-fingerprint PO among several; returns `None` on a PO-less roster.
- **Component (in-process, fast):** a normal node replicates a DM into a `PostOffice` store and a second node drains it and decrypts — over in-memory duplex channels (the Plan-2 pattern), no real sockets.
- **Integration (the 3-process rig, `#[ignore]`d):** start a `--post-office` node + Alice + Bob; all discover each other; Bob exits; Alice `/msg`s Bob (direct fails, PO stores); Bob restarts, re-discovers, his drain pulls and prints the decrypted DM. Bounded timeouts, poll-to-converge, children killed on drop — mirrors `tests/two_node_cli.rs`.
- **CI discipline:** the heavy rig is `#[ignore]`d (real processes + UDP broadcast); the unit/component tests run in the normal sweep. CPU-throttled (`nice` + `--test-threads=2`).

## 7. Decomposition into plans
This slice becomes **two implementation plans**, each independently testable:
1. **PO role + serving mode** — the signed `post_office` announce field, `Roster::post_offices` + `node::postbox::elected_post_office`, and the `--post-office` serving mode (a runnable, discoverable, durable relay). Validated by unit tests + a manual "two nodes see the PO and elect it" check.
2. **Replication + drain + offline rig** — `send_dm` PO replication with the new success semantics, the recipient drain loop, and the 3-process offline integration rig.

## 8. Accepted limitations (this slice)
- Offline delivery requires the recipient to **re-discover the sender** for identity/decryption; "sender also offline at delivery," roster/identity persistence across restart, and a PO identity directory are deferred.
- The online direct path (Plans 2–3) is unchanged and remains primary; the PO is an additional store-and-forward target.
- A **single** elected post office, no standbys / hot failover (Phase 1).
- Drain enumerates one DM conversation per known peer (fine at LAN-team scale; no "all conversations" index).
- 1:1 DMs only; no channels/groups. LAN broadcast domain only.
