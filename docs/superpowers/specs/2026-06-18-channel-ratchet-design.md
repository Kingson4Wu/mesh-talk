# Channel Group Ratchet (Sender Keys) Design

**Status:** Phase 3, follow-on to the DM Double Ratchet. Forward secrecy for group/channel messages. Authored against the north-star; professional/reliable path per the standing directive. Reuses the adversarially-verified DM-ratchet chain primitives.

## Goal

Give channel messages forward secrecy: each message uses a single-use key from a ratcheting per-sender chain, so compromising a current key doesn't expose past messages, and a removed member can't read messages after a key rotation.

## Why sender keys (not a shared ratchet)

The Double Ratchet's DH step needs a roughly-ordered 1:1 stream. A channel has N senders delivering out-of-order over the event log + post office. A single shared, lockstep-ratcheted key can't work there. **Sender keys** (Signal's group design) fit: each member owns a symmetric **sender chain** that ratchets forward per message independently. Each member distributes its sender chain's initial key to the others (sealed pairwise via the now-forward-secret DM channel / the existing per-member seal). Authenticity already comes from the event log's Ed25519 signature, so the sender chain needs no separate signing key.

This reuses the DM ratchet's **symmetric-chain half** exactly: `kdf_ck` (chain step → next chain key + message key), `message_keys` (AEAD key+nonce from the message key), and the bounded skipped-key map for out-of-order — no DH ratchet.

## Model

- **SenderKey (sending):** `{ chain_key, n }`. `ratchet() -> (n, message_key)` advances the chain (`kdf_ck`), deleting the old chain key (forward secrecy).
- **SenderKeyDistribution (SKD):** `{ chain_key, n }` — a member's CURRENT sender-key, sealed per-recipient and shipped so receivers can follow the chain. Sent when a member first speaks in an epoch (or on rotation).
- **Receiving chain (per sender):** `{ chain_key, n, skipped }`. To open message `j`, ratchet forward to `j` storing skipped keys (bounded `MAX_SKIP`), derive the message key, delete it. Try skipped first. Out-of-order safe.
- **Message wire:** `epoch ‖ sender_id ‖ n ‖ AEAD(message_key, plaintext)` (sender_id + n let receivers pick the right chain + position; they're authenticated as AAD).
- **Rotation (membership change):** bump epoch; every member generates a FRESH sender key for the new epoch and re-distributes (so a removed member, lacking the new sender keys, can't read new messages; old chain keys are already deleted → past messages stay protected).
- **Forward-secrecy ↔ history (same tension as DMs):** decrypt-on-receive → store plaintext in a channel received-plaintext store → delete the message key. Channel history renders from that store + the sender's own sent store. Durable sender-chain state persists encrypted (like the DM sessions).

## Scope and deferrals

- **In:** per-sender ratcheting chains; SKD distribution sealed per-member (reuse the existing per-member group-key seal path); receiving chains with skipped keys; rotation on membership change; channel plaintext store + durable sender-chain state; history from stores.
- **Deferred:** out-of-band SKD fetch for a late joiner who missed a sender's SKD (v1: a sender re-broadcasts its SKD on a new epoch / when it detects an unknown recipient); MLS-style tree agreement (sender keys are the pragmatic v1); double-ratcheting the sender keys themselves.

## Decomposition

1. **Sender-key chain core** (pure, `channel::sender_key`): `SenderKey` (send), `SenderChain` (receive, skipped keys), `seal_message`/`open_message`, SKD codec. Reuses `ratchet::{kdf_ck, message_keys}` (expose them `pub(crate)`). Exhaustive tests (ratchet forward, out-of-order, skip cap, rotation isolation, tamper).
2. **Channel model integration:** `ChannelState`/`ChannelBook` hold per-sender chains + own sender key; build/open sender-keyed messages; SKD as a channel event (or fold into the existing `KeyRotation`); rotation.
3. **Node + stores + history:** durable sender-chain state + channel received-plaintext store; seal/open channel `Message` events via sender keys; serve channel history from stores. Loopback rig (group FS + out-of-order + a removed member can't read post-rotation).

Plan 1 (the pure core) lands first — most verifiable, zero integration risk.
