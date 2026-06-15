# Mesh-Talk Redesign — Design Spec

Date: 2026-06-15
Status: Approved for planning

## 1. North star & scope

Mesh-Talk is being redesigned into a **serverless, end-to-end-encrypted team chat
scoped to a single local network** — a "local Slack" that needs no server, no
accounts, and no internet.

- **Audience / scenario:** a persistent small team / office, ~10–50 people, on the
  same LAN every day.
- **Defining properties:** zero-config, no central server, **offline-first**, and
  **E2E-encrypted** throughout.
- **Explicitly out of scope (for now):** cross-network / internet operation, NAT
  traversal, voice/video, multi-device sync, and metadata privacy. These may be
  revisited later but do not shape v1.

## 2. Key decisions (the load-bearing ones)

| Axis | Decision |
|------|----------|
| Reach | LAN-only, zero-config, no central server |
| Offline delivery | **Auto-elected "post office"** peer: store-and-forward + history, treated as a special always-on full replica |
| Content security | **E2EE** everywhere; the post office only ever holds ciphertext |
| Storage/sync model | **Replicated encrypted event-log** per conversation |
| DM crypto (v1) | **Sealed-box** (static-DH + per-message ephemeral); Double Ratchet deferred to Phase 3 |
| Identity | Key-based; `user_id` = public-key fingerprint |

## 3. System overview

Topology: N peer devices on one LAN. Some are post-office-eligible (always-on
machines); exactly one is the *active* post office at a time, with hot standbys.

Per-device components:

1. **Identity & keystore** — long-term keypair (Ed25519 signing + X25519 key
   exchange), held in the password-encrypted keystore (reuses the at-rest crypto
   already hardened in the current codebase). `user_id` = fingerprint of the
   Ed25519 public key — so identity, address, and key are finally distinct.
   Display name is mutable and signed.
2. **Discovery** — UDP broadcast announce/heartbeat: *signed* announces,
   self-filtered by key, rate-limited. Builds the member roster.
3. **Secure transport** — authenticated, encrypted peer channels via a **Noise**
   handshake over TCP. Replaces today's plaintext JSON; mutual auth from the
   identity keys.
4. **Conversation store** — append-only, hash-linked per-conversation **event
   logs**. Local **encrypted SQLite** + a full-text search index.
5. **Sync engine** — reconciles logs between any two peers ("I have up to X, send
   the rest"); drives offline catch-up.
6. **Group crypto** — pairwise sealed-box for DMs; **sender-keys** for channels
   (per-channel group key, rotated on membership change).
7. **Post-office role** — when elected, a full replica + store-and-forward for
   everyone; otherwise dormant.
8. **App/command layer** — a clean Rust API the UI calls (replaces the
   ~1600-line `commands.rs` and the global singletons).
9. **UI (Vue)** — a redesigned 3-pane team-chat interface.

## 4. Identity & E2E crypto

- **Identity.** First run generates the keypair into the password-encrypted
  keystore. `user_id` is the public-key fingerprint — stable, self-certifying.
- **Verification.** Trust-on-first-use by default, plus a **safety-number / QR
  compare** flow (out-of-band, no directory) to mark a contact *verified*. The
  roster shows verified vs unverified.
- **DM encryption (v1).** X3DH-style handshake → shared secret; messages sealed
  with static-DH + a per-message ephemeral key. Authenticated and confidential,
  but **no forward secrecy** (accepted v1 tradeoff). Double Ratchet is a Phase-3
  upgrade.
- **Channel encryption.** Per-channel symmetric **group key**; members encrypt
  with sender-keys derived from it; the key **rotates on join/leave** so removed
  members cannot read new messages and joiners cannot read prior messages unless
  explicitly given history. Sender-keys are distributed over the pairwise
  channels.
- **What the post office sees.** Only ciphertext blobs + routing metadata
  (conversation id, sender, seq, timestamps) — enough to order/route, never to
  read. It *does* see traffic metadata (who↔who, when); metadata privacy is out
  of scope for a LAN office tool and is explicitly accepted.

## 5. The event-log & sync

- **Conversation = append-only log.** Every DM and channel has a
  `conversation_id` and is a log of **events**: `Message, Edit, Delete, React,
  ReadMarker, MembershipChange, KeyRotation, FileManifest`.
- **Event shape:**
  `{ id = content-hash, conversation_id, author, seq (per-author), parents[]
  (hash links), lamport_ts, wall_clock, ciphertext, sig }`. Hash-linked →
  tamper-evident and causally ordered.
- **Ordering & conflicts.** Causal order via `parents` + Lamport clock;
  deterministic total order by `(lamport, id)`. Concurrent edits →
  last-writer-wins with edit history retained; deletes are tombstones. Reactions,
  threads (a thread = events whose `parent` is the root message), and
  read-markers all ride the same mechanism.
- **Sync protocol.** Peers exchange per-author version vectors → "you're missing
  these ids, here they are." Identical for peer↔peer and peer↔post-office. A
  returning member syncs from whoever is online (usually the post office).
- **Local storage.** Encrypted SQLite: raw `events`, a decrypted `messages` view
  for rendering, and an FTS `search_index`. Decryption happens once on ingest;
  plaintext lives only inside the local encrypted DB.
- **Files.** A `FileManifest` event (name/size/checksum/chunk-hashes/sealed
  file-key) + content-addressed chunks. The existing chunked/resumable/
  checksum-verified transfer becomes the chunk-fetch layer, pulling chunks from
  any peer that has them (including the post office).

## 6. Offline delivery & the post office

- **Election.** Eligible nodes (flagged always-on, or auto-detected by uptime)
  run a small leader election (e.g. lowest-fingerprint-among-eligible /
  raft-lite). Exactly one *active*; standbys also replicate so failover loses
  nothing.
- **Role.** A peer with two extra hats — **full replica** (holds the encrypted
  logs it is entitled to) + **store-and-forward** (accepts events for offline
  members, serves them on reconnect).
- **Zero-config.** No setup; an eligible machine auto-volunteers, else the
  longest-online peer is chosen, with a gentle UI nudge to keep one machine on as
  the team hub.
- **Authorization.** For **channels** the post office is a member → holds
  ciphertext, never the key. For **DMs** it is a *dumb relay* of recipient-sealed
  envelopes → relays without joining the conversation. It never needs a
  decryption key for anything.
- **Failure modes.** Post office down → online peers still talk directly and
  reconcile on its return. Split-brain (two post offices) → both are just
  replicas; content-addressed events merge idempotently. A message is only
  *delayed* (never lost) if every post office **and** the sender are offline at
  once.

## 7. Backend architecture (Rust)

Fix the current smells while restructuring:

- **Remove global singletons** (the `OnceLock` services) → an explicit `App`
  context that owns the services and is handle-cloned. Testable, no hidden
  globals.
- **Layered modules with clean boundaries:** `identity` · `transport` (Noise) ·
  `discovery` · `crypto` (DM + channel keys) · `log` (events, hash-linking,
  encrypted-SQLite store, sync engine) · `conversation` (DMs/channels/membership)
  · `files` · `postoffice` · `presence` · `app` (the command API the UI calls).
- **Split the monoliths:** `commands.rs` (~1600 lines) → per-feature command
  modules; `node_service.rs` (~900) → transport + discovery + roster.
- **One canonical event type** — delete the two parallel `ContactRequest` structs
  and the `Message`/struct duplication.
- **CLI stays a thin client** over the same `app` API — two CLIs on localhost are
  the integration-test rig for the whole stack.

## 8. UI / interaction redesign

**Principles:** identity-first & zero-config (you *are* your key); calm and
professional, keyboard-friendly; trust made visible but never naggy; resilience
made legible (presence, delivery state, hub status, sync always understandable).

**Main layout — a 3-pane team-chat shell:**

```
┌─────────────┬──────────────────────────────────────┬─────────────┐
│ OFFICE-LAN  │  # general            🔒 e2ee   ⚙     │ Members (12)│
│ 🟢 Kingson  │ ──────────────────────────────────────│ 🟢 Kingson ✓│
│             │  Alice · 9:01                          │ 🟢 Alice   ✓│
│ CHANNELS  + │   morning team 👋                      │ 🟡 Bob      │
│ # general•3 │     🙂2   💬1 thread                   │ ⚪ Carol    │
│ # dev       │  Bob · 9:03 · via hub                  │ …           │
│ # random    │   pushed the fix, CI green ✅          │ [ Invite + ]│
│             │  ───────── Today ─────────             │             │
│ DIRECT    + │  You · 9:05   ✓✓ read                  │             │
│ 🟢 Alice ✓  │   nice, merging                        │             │
│ 🟡 Bob      │ ┌──────────────────────────────────┐  │             │
│ ⚪ Carol    │ │ Message #general…   📎  🙂  @      │  │             │
│             │ └──────────────────────────────────┘  │             │
│ NETWORK +2  │  Bob is typing…                        │             │
├─────────────┴──────────────────────────────────────┴─────────────┤
│ 🟢 You · OFFICE-LAN · hub: Mac-Studio · all synced ✓               │
└───────────────────────────────────────────────────────────────────┘
```

- **Left rail:** Channels (unread •count, mentions); Direct messages (presence dot
  + ✓ verified shield); a **"People on the network"** section where newly
  discovered peers surface for a one-tap signed add.
- **Center:** the conversation — day separators, inline reactions, thread
  affordance, read receipts (✓✓), and a subtle **"via hub"** tag when a message
  was relayed while the recipient was offline.
- **Right:** contextual — member list/presence for channels, the thread panel, or
  file/details.
- **Status bar:** your presence · which machine is the hub · sync state.

**First-run (zero-config):** "Welcome — what's your name?" → generates your key,
shows your safety fingerprint → "You're on OFFICE-LAN with 11 others" → lands in
`#general`. No server, no account, no email.

**Key flows:** drag-drop files into the composer (inline chunked-upload progress);
@mention autocomplete; edit/delete own messages; thread replies in the right
panel; global search (`in:#dev from:alice`); verify-a-contact via safety-number /
QR compare (turns the shield solid); membership changes quietly note "channel key
rotated."

## 9. Phased roadmap

This is too large for a single implementation cycle. Build in order; each phase
is independently shippable.

- **Phase 0 — Foundation (the spine):** key-based identity + Noise secure
  transport + event-log store + sync engine + 1:1 DMs with offline delivery via a
  minimal post office. Validated by the two-CLI integration rig.
- **Phase 1 — Team:** channels/groups + group sender-keys + presence/typing/read +
  full post-office election & standbys.
- **Phase 2 — Rich:** files-over-event-log, reactions/@mentions/threads, full-text
  search.
- **Phase 3 — Polish:** Double Ratchet upgrade for DMs, multi-device, voice
  (later).

The implementation plan sequences **Phase 0** first.

## 10. Testing strategy

- **Unit:** crypto (sealed-box & group-key round-trips, key rotation, tamper
  detection), event-log (hash-linking, ordering, conflict resolution, sync
  convergence).
- **Integration:** the **two-CLI-on-localhost** rig — real handshake → DM →
  go offline → post-office relay → reconnect → assert convergence. Property tests
  for log convergence under reordering and partition.
- **Frontend:** Vitest component tests for the panes and key flows.
- **CI:** the existing gates carry over (clippy `-D warnings`, cargo-deny, typos,
  coverage, frontend lint), now enforced on both Linux and macOS.

## 11. Open questions / accepted limitations

- **Metadata privacy:** the post office sees who-talks-to-whom and when. Accepted
  for a LAN office tool.
- **No forward secrecy in v1 DMs** (sealed-box). Upgraded in Phase 3.
- **History for new channel members:** by default they do *not* get prior history
  (key rotation). Whether to offer an explicit "share history on invite" is a
  Phase-1 detail.
- **Post-office trust for channels:** it holds channel ciphertext as a member-
  replica; it cannot read content but is a metadata vantage point. Acceptable.
- **Persistence of the existing data:** this is a redesign; migrating current
  users' data is not a goal (the app is pre-release).
