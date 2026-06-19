# Channels / Groups — Design Spec (Phase 1)

Date: 2026-06-17
Status: Approved for planning (driven autonomously against the master roadmap)

## 1. Purpose & scope

DMs work end to end (online, offline-via-post-office, persistent, in the desktop app). This slice adds **channels** — multi-party group conversations — the headline of Phase 1 of the master redesign (`2026-06-15-mesh-talk-redesign-design.md` §4, §6, §9).

A channel is a named conversation with a **membership** and a **per-channel symmetric group key**. Members encrypt messages with the group key; the key is distributed to each member by sealing it to their X25519 key (reusing the DM sealed-box); the key **rotates on join/leave** so a removed member can't read new messages and a new member can't read prior ones. The post office relays channel ciphertext as a member-replica — it holds the encrypted events, never the key.

**v1 simplifications (deliberate, analogous to deferring Double Ratchet for DMs):**
- A single per-channel **group key** (not per-sender "sender-keys" with ratcheting). Authorship is proven by the per-event Ed25519 signature, not by a per-sender key. Sender-key ratcheting is a later refinement.
- No "share history on invite": a new member gets the new key only and sees messages from join onward.
- Membership/authorization is **creator-and-members-extend** (any member can add/remove + rotate); a stricter admin/role model is deferred.

**Out of scope (later):** sender-key ratcheting; forward secrecy within a channel; history-sharing on invite; roles/permissions; large-group key-distribution efficiency (v1 reseals to each member individually).

## 2. The crypto primitive (this slice's foundation — `channel` module)

A pure, unit-testable module `src-tauri/src/channel.rs` that reuses existing primitives. No network, no state.

- **`GroupKey`** — wraps `[u8; 32]`. `GroupKey::generate()` (via `rand::rngs::OsRng`), `as_bytes()`, `from_bytes([u8;32])`.
- **Key distribution (reuse `dm`):**
  - `seal_group_key(sender: &DeviceIdentity, recipient_x25519: &[u8;32], key: &GroupKey) -> Result<Vec<u8>, ChannelError>` = `dm::seal(sender, recipient_x25519, key.as_bytes())`.
  - `open_group_key(recipient: &DeviceIdentity, sender_x25519: &[u8;32], envelope: &[u8]) -> Result<GroupKey, ChannelError>` = `dm::open(...)` then parse exactly 32 bytes → `GroupKey` (reject wrong length).
- **Message encryption (raw-key AES-256-GCM, fresh random nonce per message):**
  - `seal_channel_message(key: &GroupKey, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError>` = generate a fresh 12-byte nonce (OsRng), `Aes256Gcm::new_from_slice(key.as_bytes())` then `encrypt`, return `nonce ‖ ciphertext`.
  - `open_channel_message(key: &GroupKey, envelope: &[u8]) -> Result<Vec<u8>, ChannelError>` = split off the 12-byte nonce, decrypt the rest (reject `< NONCE_SIZE` bytes).
  - **Nonce safety:** a per-message random 96-bit nonce with a fixed key is the standard sealed-message construction; the key also rotates on membership change. This is the same AES-256-GCM the DM module uses (`aes_gcm::Aes256Gcm`), just keyed by a shared group key instead of a per-message DH-derived key.
- **`ChannelError`** (hand-written enum + Display/Error): `Seal(dm::DmError)`, `Encrypt`, `Decrypt`, `Malformed(String)`.

## 3. The channel model (subsequent plan)

- **Channel id:** a random 32-byte `ConversationId` minted at creation (distinct from the DM `dm_conversation_id` derivation). Carried in events like any conversation.
- **Channel events** (reuse the existing `EventKind` variants — they already exist):
  - `Message` — `ciphertext` = `seal_channel_message(group_key, plaintext)`.
  - `MembershipChange` — `ciphertext` = a signed, plaintext-readable membership delta (added/removed user_ids + a channel name); membership is metadata the post office may see (consistent with the master spec's accepted metadata exposure).
  - `KeyRotation` — emitted on a membership change; the *new* group key is distributed out-of-band as per-member `seal_group_key` envelopes carried in a companion structure (sealed to each member), NOT in the cleartext event. The event records that a rotation happened (epoch number); the sealed keys are delivered to members.
- **Channel state (`ChannelState`):** id, name, members (`Vec<UserId>` / their `PublicIdentity`), the current group key + epoch. Built by replaying the channel's events + the member's received sealed keys. A member who lacks the key for an epoch simply cannot decrypt those messages (skipped, like an undecryptable DM).

## 4. Node + app integration (subsequent plans)

- **`node`/`runtime`:** create-channel (mint id, seal the key to initial members, append the `MembershipChange`), send-channel-message (`seal_channel_message` → `Message` event → sync), receive (decrypt with the member's group key for the epoch → surface), add/remove member (rotate key, reseal, `MembershipChange`+`KeyRotation`). Channel events sync over the SAME event-log sync engine + post office as DMs — no new transport.
- **IPC commands + Vue:** `redesign_create_channel`, `redesign_list_channels`, `redesign_channel_history`, `redesign_send_channel_message`, `redesign_add_member`; channel events surface via a `redesign-channel-message` event. A channels section in the `/redesign` route. (Mirrors the DM commands/route already shipped.)

## 5. Data flow (a channel message)
1. Sender: `seal_channel_message(group_key_for_current_epoch, plaintext)` → `Message` event in the channel's log → append → sync to members + the post office (same engine as DMs).
2. Recipient member: ingests the event → looks up its group key for the event's epoch → `open_channel_message` → surfaces the plaintext. A member without that epoch's key skips it.
3. The post office holds the ciphertext event; it has no group key, so it can never read the message.

## 6. Error handling
- A member who lacks the key (not yet delivered, or a removed member) → `open_channel_message`/`open_group_key` fails → the message is skipped (logged), re-attempted if the key later arrives. Fail-soft, like undecryptable DMs.
- Forged/malformed channel or key events → rejected by the event log's `append` validation (signature/integrity) and `ChannelError::Malformed` on parse. Never fatal.
- Nonce/length errors on `open_channel_message` → `ChannelError::Malformed`.

## 7. Testing
- **Unit (this slice / the crypto primitive — the reliable core):** group-key seal→open round-trip (a member opens; a non-member/ wrong key fails); channel-message seal→open round-trip (right key decrypts; wrong key fails; tampered ciphertext fails); two seals of the same plaintext differ (fresh nonce); malformed/short envelopes rejected.
- **Component/integration (later plans):** in-process channel reconcile across N members over the existing sync engine; the two/three-CLI rig extended for a channel; the post office holding channel ciphertext it can't read.
- CPU-throttled (`nice` + `--test-threads=2`).

## 8. Decomposition into plans
1. **Channel crypto primitive** — the `channel` module (`GroupKey`, `seal/open_group_key`, `seal/open_channel_message`, `ChannelError`) + unit tests. Pure, no network. (THIS plan first.)
2. **Channel state + membership/rotation** — channel id minting, `ChannelState` replay, the membership-delta + key-rotation event encoding, key-epoch tracking.
3. **Node integration** — create/send/receive/add-member over the event-log sync engine + post office; an in-process multi-member rig.
4. **App integration** — IPC commands + a channels section in the `/redesign` Vue route + the channel-message event.

## 9. Accepted limitations (this slice)
- Single per-channel group key (no sender-key ratcheting / forward secrecy within a channel).
- New members get no prior history; no "share history on invite".
- v1 reseals the group key to each member individually on rotation (fine at LAN-team scale).
- Membership and channel names are metadata the post office can see (accepted per the master spec).
- DMs and channels share the event-log/sync/post-office machinery unchanged.
