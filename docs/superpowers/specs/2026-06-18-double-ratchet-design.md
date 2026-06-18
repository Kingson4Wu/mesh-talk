# Double Ratchet for DMs (Phase 3) Design

**Status:** Master-spec Phase 3 ("Double Ratchet / multi-device"). Architecture + scope authored against the north-star; user deferred the approach to engineering judgment with a mandate for the professional/reliable/clean path. **Security-critical** — built on the codebase's already-vetted primitives, exhaustively tested, adversarially reviewed.

## Goal

Give DMs forward secrecy + post-compromise security: each message uses a single-use key derived by a ratchet, so compromising long-term keys (or a current key) does not expose past (or future) messages. Implement the published Signal Double Ratchet algorithm over the existing X25519 / HKDF-SHA256 / AES-256-GCM primitives.

## Why not a library / not from-scratch crypto

The codebase already uses Signal's recommended primitives (`x25519-dalek`, `hkdf`, `aes-gcm`). A heavyweight library (vodozemac/Olm) carries its own account/session/storage model that fights the content-addressed event log. So we implement the **ratchet state machine** (the algorithm) ourselves over those vetted primitives — we do NOT roll our own curve, KDF, or AEAD. Reliability comes from: vetted primitives + published-spec conformance + known test vectors + out-of-order/skipped-key tests + adversarial review.

## The core tension and its resolution

Forward secrecy requires **deleting** a message key after use. But mesh-talk keeps messages re-decryptable from the persisted, content-addressed event log (history survives restart; sync re-delivers). These conflict. Resolution (Signal's model):

- **Decrypt once, on receive; store plaintext locally** in an encrypted store (extending the existing sent-plaintext sidecar pattern to received messages), then **delete the wire key**. History renders from the local plaintext store, NOT by re-decrypting the wire.
- The sealed events stay in the log for **sync/transport only**; they are no longer needed for local display, and once a key is deleted they are no longer locally decryptable (that's the point — an attacker who later seizes the device + log can't read them).
- **Durable ratchet state + skipped-message keys** persist (encrypted) so the session survives restart and tolerates the event log's out-of-order / multi-path delivery. Replayed events (same id) are deduped before any key use (the node already tracks an `emitted` set).

## The algorithm (what Plan 1 builds — pure, no I/O)

Standard Double Ratchet:
- **Root chain:** `(root_key, dh_out) → (root_key', chain_key)` via HKDF-SHA256, stepped on each DH ratchet.
- **Symmetric chains:** a sending chain + a receiving chain; `chain_key → (chain_key', message_key)` via an HMAC/HKDF step. Each message key is single-use.
- **DH ratchet:** each party holds a ratchet X25519 keypair; a message header carries the sender's current ratchet public key. When a received header shows a new ratchet pubkey, the receiver finishes the old receiving chain (storing skipped keys), DH-ratchets the root chain, and starts a new receiving chain; on its next send it generates a new ratchet keypair and steps the root chain for sending.
- **Message header:** `{ ratchet_pub: [u8;32], pn: u32 (previous sending-chain length), n: u32 (message number) }` — authenticated as AEAD associated data.
- **Skipped keys:** if a header's `n` is ahead, derive + store the intervening message keys (bounded cap, e.g. 1000) so earlier-but-later-arriving messages still open. Try stored skipped keys first on decrypt.
- **Session init:** the initial `root_key` is derived from a shared secret between the two identities (X3DH-style; v1 bootstraps from the existing identity-key DH used by `dm::seal`, so no new prekey-server infrastructure is needed). The initiator also sends its first ratchet pubkey.

## Decomposition (plans; Plan 1 only here, rest scoped later)

1. **Pure ratchet core** (`crate::ratchet`): `RootKey`/`ChainKey`/`MessageKey` KDF steps; `RatchetState` (DHs, DHr, root, sending/receiving chains, Ns/Nr/PN, skipped-keys map); `init_alice`/`init_bob` from a shared secret + Bob's initial ratchet pubkey; `ratchet_encrypt` / `ratchet_decrypt` (with DH-ratchet + skipped-key handling); a `Header` codec. Exhaustive tests: round-trip both directions, interleaved, out-of-order within a chain, across DH steps, skipped-key cap, tamper/replay rejection, and a deterministic known-answer vector. **No I/O, no event log, no networking.**
2. **Durable session + plaintext stores:** an encrypted ratchet-state store (per peer) + a received-plaintext store (so history persists after key deletion), extending the sidecar pattern.
3. **Node integration:** establish/load a ratchet session per DM peer; seal DM `Message` events with `ratchet_encrypt`; on receive, `ratchet_decrypt` → store plaintext + delete key; serve history from the plaintext store; handle event-log ordering/dedup. Loopback rig: forward secrecy property (a deleted key can't re-open a past message) + out-of-order delivery.
4. (Later) migration/coexistence with the existing DM box; channels are out of scope (sender-keys/group ratchet is a separate effort); multi-device.

This spec covers the design; **Plan 1 (the pure core) is implemented first** — the most security-critical and the most verifiable piece, in isolation.
