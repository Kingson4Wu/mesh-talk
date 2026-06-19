# Mesh-Talk Architecture

A decentralized, end-to-end-encrypted LAN messenger. **Tauri** desktop shell, **Rust**
backend, **Vue 3** frontend. No server: peers discover each other over signed UDP
broadcasts, connect directly over a Noise-encrypted TCP channel, and store messages as
an append-only, hash-linked **event log** that syncs CRDT-style. When a peer is offline,
an elected **post office** node stores-and-forwards the (still-encrypted) events.

> The earlier RSA-contact / plaintext-UDP / TCP-relay *legacy* stack has been retired;
> this serverless stack is the entire product and lives at `/`. The only retained
> piece outside the node is the auth/session layer (`services/auth_service.rs` + `state.rs`),
> which `login` uses before starting the node.

---

## 1. Process & layers

```
Vue 3 UI (ChatView.vue) ──invoke()──▶ Tauri IPC (chat_commands.rs)
        ▲   ──listen() events──                         │
        │                                               ▼
        │                                   NodeRuntime (node/runtime.rs)
        │                                   starts 7 bg tasks per login:
        │                                   UDP listen / UDP broadcast /
        └────────── on_dm/on_channel/on_file callbacks ── TCP accept / PO drain /
                                                          DM·channel·file forwarders
                                                               │
                                                          Node (node/node.rs) — orchestration
        ┌──────────────┬───────────────┬──────────────┬───────┴──────┐
   identity/      transport/        eventlog/       discovery/    postoffice/
   ratchet/       (Noise XX)        (DAG + sync)    (signed UDP)  (elect+relay)
   channel/ dm/
        └────────────── storage/encryption.rs (PBKDF2-600k + AES-256-GCM at rest) ──┘
```

## 2. Crypto & identity (`identity/`, `transport/`, `ratchet/`, `dm/`, `channel/`)

- **DeviceIdentity** — per device: Ed25519 (sign) + X25519 (DH). `user_id =
  SHA-256("mesh-talk-id-v1" ‖ ed25519_pub)[:16]` (32 hex).
- **Account** (multi-device) — cross-device Ed25519. `account_id =
  SHA-256("mesh-talk-account-v1" ‖ pub)[:16]`. A **DeviceCertificate** is the account
  key's signature over a device key (domain `mesh-talk-device-cert-v1`), binding device→account.
- **At rest** (`storage/encryption.rs`) — `salt(16) ‖ nonce(12) ‖ AES-256-GCM(secret)`,
  key via **PBKDF2-HMAC-SHA256, 600k rounds**. Used by every store (device/account
  keystore, event log, sent/received logs, ratchet sessions, channel senders, post office).
- **Transport** (`transport/`, snow) — **Noise_XX_25519_ChaChaPoly_BLAKE2s** with a
  post-handshake identity exchange: each side signs `"mesh-talk-transport-auth-v1" ‖
  handshake_hash` and verifies the advertised X25519 == the Noise-authenticated static key
  → binds the Ed25519 identity to the channel. 4-byte length framing, MAX_FRAME 65535.
- **DM crypto** — **Double Ratchet** (`ratchet/state.rs` + `node/dm_ratchet.rs`):
  `shared_root = HKDF(DH(me,peer))`; init_alice/init_bob; DH ratchet on each inbound →
  forward secrecy + post-compromise recovery; bounded out-of-order (1000/2000); lower
  `user_id` is the canonical initiator (simultaneous-init tie-break); state encrypted on
  disk (`node/ratchet_sessions.rs`). The `dm.rs` X3DH sealed-box (no FS) is now used only
  to distribute channel keys and seal file manifests.
- **Channels** — per-sender **sender-key** group ratchet (`channel/sender_key.rs`):
  single-use message keys; membership add/remove rotates the **epoch** and re-distributes
  sender-key distributions (sealed per member via the DM sealed-box). Sender chains are
  persisted so a restarted node can resume sending.
- **Multi-device** — `DmEnvelope{route, msg_id, body}` (magic `MTDE1`) carries
  sender/recipient *account* routing inside the ciphertext; `send_to_account` fans a
  per-device ratcheted copy to every device of the target account + self-syncs to own
  devices; `account_history` merges by `msg_id`. Device **linking** (`node/pairing.rs`):
  one-time 128-bit code (SHA-256 authenticator binding both device keys, constant-time
  check) → account secret + cert + history backfill transferred over the Noise channel.

## 3. Event log & sync (`eventlog/`)

- **Event** — content-addressed: `id = SHA-256(domain ‖ content)`; fields `conversation,
  author (Ed25519 pub, self-certifying), seq, parents (→ hash-linked DAG), lamport,
  wall_clock, kind, ciphertext, sig`. `EventKind`: Message/Edit/Delete/React/ReadMarker/
  MembershipChange/KeyRotation/FileManifest (indices frozen for wire stability).
- **EventLog** (in-memory, `store.rs`) — per-conversation DAG; tracks `heads` (frontier),
  `version` (per-author max seq → equivocation/fork detection), `max_lamport`. `events()`
  returns `(lamport, id)`-sorted (deterministic topological order).
- **Sync** (`sync.rs`) — three-message id-set reconciliation: `Request(have)` →
  `Response(missing + responder have)` → `Followup(what responder lacks)`, bounded by
  frame budget (`MAX_PLAINTEXT`), multiple rounds (≤ `MAX_SYNC_ROUNDS`). Every event is
  re-verified (hash + signature) on ingest.
- **Persistence** (`persist.rs`) — `MTLOG1 ‖ salt ‖ [len ‖ nonce ‖ AES-256-GCM(event)]…`,
  append-only; a torn trailing record (crash mid-write) is dropped on reload and re-synced.

## 4. Networking & delivery (`discovery/`, `node/`, `postoffice/`)

- **Discovery** — signed `Announce` (Ed25519, version 2) carries `x25519_pub`, `tcp_port`,
  `post_office` flag, and the `account_cert`; the device signature commits to the account
  key (prevents cert-swap). `run_broadcast` every 2 s; `run_listen` verifies + updates the
  roster; TTL eviction; `devices_of_account()` groups devices by account.
- **Delivery** — `send_*` appends the sealed event, then `deliver_direct` (Noise dial +
  one sync round) and, on failure/always, `replicate_to_post_office`. Receivers run an
  accept loop (`serve_connection` → `serve_one` ingest → `emit_new_messages` decrypt/surface).
- **Post office** — deterministic election (lowest-fingerprint peer advertising
  `post_office`); `drain_from_post_office` every 3 s; relay only ever sees ciphertext.

## 5. Frontend (`frontend/`)

Vue 3 + Pinia + Vue Router (hash). `src/services/api.js` exposes `authAPI` + `chatAPI`
(wrapping every messaging Tauri command). `views/chat/ChatView.vue` is the
app (3-pane UI: peers/channels · messages · members) with @mentions, replies, reactions,
files, search, and the link-a-device panel, served at `/`. `stores/appStore.js` is an
auth/session-only store; `LoginView` is the only other view.

## 6. Binaries

- `mesh-talk` (`main.rs` → `lib.rs::run_tauri`) — the desktop app.
- `mesh-talk-node` (`bin/mesh-talk-node.rs`) — headless node CLI; `--post-office`
  runs relay mode.

## 7. Build, test, CI

- **Workspace**: root `Cargo.toml` (`members = ["src-tauri"]`); Rust code in `src-tauri/`.
- **Local gate** (`scripts/check-health.sh`, run by the `hooks/pre-commit` hook): fmt,
  `clippy --all-targets -D warnings`, ESLint, full `cargo test`, typos, cargo-deny,
  cargo-machete, gitleaks, shellcheck, audits, both builds — **mirrors CI** so failures
  surface locally, not on CI.
- **CI** (`.github/workflows/`): `ci.yml` (ubuntu+macOS matrix → the `verify` aggregate
  check, required by branch protection), `check-health.yml`, `gitleaks.yml`,
  `dependabot-auto-merge.yml` (patch-only), `glib-0.20-watch.yml` (monthly dep watcher).
- **Crypto primitives**: ed25519-dalek, x25519-dalek (need rand_core 0.6 — keep `rand` at
  0.8), aes-gcm, snow (Noise), sha2, hkdf, pbkdf2, bincode.

## 8. Security posture (summary)

Standard primitives + consistent domain separation; AEAD everywhere; PBKDF2-600k at rest;
Noise XX with identity binding; Double-Ratchet + sender-key forward secrecy with
zeroize-on-drop; signed content-addressed log with fork detection; deterministic relay
election. Known limitations: device linking has no key-pinning/SAS (LAN MITM window);
backfill history travels as plaintext over Noise.
