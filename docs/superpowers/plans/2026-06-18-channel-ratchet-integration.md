# Channel Sender-Key Integration Implementation Plan (Channel Ratchet Plans 2-3)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. SECURITY-SENSITIVE. Checkbox steps. The build must stay green after EACH task (additive-then-switch).

**Goal:** Wire the sender-key ratchet into channels end-to-end: per-sender ratcheting chains in `ChannelState`/`ChannelBook`, per-member sender-key distribution on the `KeyRotation` event, channel messages sealed with single-use keys, and channel history served from a plaintext store (since keys are consumed). Forward secrecy for group chat.

**Architecture:** `ChannelState` gains a sender-key model (my sending `SenderKey` per epoch + per-`(author,epoch)` receiving `SenderChain`) ADDED alongside the existing per-epoch `GroupKey` (Task 1, additive → build stays green). Then `ChannelBook` + the Node channel path SWITCH to sender keys and the old `GroupKey` path is removed (Task 2). Channel history moves to a received-plaintext store (Task 3). Mirrors the DM ratchet integration.

**Tech Stack:** Rust; `crate::channel::sender_key` (Plan 1, verified), `crate::node::received_log` pattern, the existing channel model/book/node code. No new deps.

---

## Background the implementer needs

`crate::channel::sender_key` (verified sound): `SenderKey::{generate, distribution()->SenderKeyDistribution, ratchet()->(u32,[u8;32])}`; `SenderKeyDistribution {chain_key:[u8;32], n:u32}` (+`encode`/`decode`); `SenderChain::{from_distribution(&skd), message_key(target:u32)->Result<[u8;32],SenderKeyError>}`; `seal_message(mk,pt,aad)->Result<Vec<u8>,_>`; `open_message(mk,ct,aad)->Result<Vec<u8>,_>`. **Integration contracts (from the adversarial review — MUST honor): the AAD for seal/open MUST be `epoch ‖ sender_id ‖ n`; a FRESH SenderKeyDistribution per epoch.**

Current `ChannelState` (`src-tauri/src/channel/model.rs`): `{ id, name, members: Vec<PublicIdentity>, epoch: u64, keys: HashMap<u64, GroupKey> }`; `from_meta(id, ChannelMeta)`, `apply_meta`, `record_key(epoch,GroupKey)`, `key_for`, `seal_message(&self, pt)->Result<Vec<u8>,ChannelError>` (frames `epoch ‖ AES(groupkey)`), `open_message(&self, payload)->Option<Vec<u8>>`. `ChannelMeta { name, members, epoch }`.

Current `ChannelBook::process` (`src-tauri/src/node/channel.rs`): replays `MembershipChange`→state, `KeyRotation`→`SealedKeys` (one GroupKey sealed per member via `seal_keys_for`/`open_my_key`)→`record_key`, `Message`→`state.open_message`→`MessageBody`. `SealedKeys { epoch, entries: Vec<SealedKeyEntry{user_id, sealed}> }`.

Node channel path (`src-tauri/src/node/node.rs`): `create_channel` builds the `GroupKey` + `seal_keys_for` + `KeyRotation` event; `send_channel_message_reply` → `state.seal_message`; `channel_history` → `state.open_message` per Message event.

**CPU/test discipline:** `nice -n 10`, ONE filter/run. `cargo test --lib channel`/`node::channel`/`node::node`; build/clippy/fmt. Green build after EVERY task. Confirm branch line.

---

### Task 1: `ChannelState` sender-key model (ADDITIVE — build stays green)

**Files:** Modify `src-tauri/src/channel/model.rs`.

- [ ] **Step 1: header codec + fields**

Add a per-message header + sender-key fields to `ChannelState` WITHOUT removing the existing `keys`/`seal_message`/`open_message` (additive). Add imports for `crate::channel::sender_key::{SenderKey, SenderChain, SenderKeyDistribution, seal_message as sk_seal, open_message as sk_open}`.
```rust
#[derive(Serialize, Deserialize)]
struct MsgHeader { epoch: u64, sender: String, n: u32 }
impl MsgHeader {
    fn encode(&self) -> Vec<u8> { bincode::DefaultOptions::new().with_fixint_encoding().serialize(self).expect("hdr") }
    fn decode(b: &[u8]) -> Option<Self> { bincode::DefaultOptions::new().with_fixint_encoding().reject_trailing_bytes().deserialize(b).ok() }
}
```
Add to `ChannelState`:
```rust
    my_user_id: Option<String>,
    my_sender: std::collections::HashMap<u64, SenderKey>,
    sender_chains: std::collections::HashMap<(String, u64), SenderChain>,
```
Initialize them (`None`/empty) in `from_meta`.

- [ ] **Step 2: sender-key methods (new names, coexist with the old)**
```rust
    /// Set this node's identity (who "I" am) for sender-key send/AAD. Idempotent.
    pub fn set_identity(&mut self, my_user_id: String) { self.my_user_id = Some(my_user_id); }

    /// My current-epoch sender-key distribution (generating my sender key if needed).
    /// Distribute this (sealed per-member) so members can follow my chain from n=0.
    pub fn my_sender_distribution(&mut self) -> SenderKeyDistribution {
        let epoch = self.epoch;
        self.my_sender.entry(epoch).or_insert_with(SenderKey::generate).distribution()
    }

    /// Record a peer's sender chain for `(author, epoch)` from their distribution.
    pub fn record_sender_chain(&mut self, author: String, epoch: u64, skd: &SenderKeyDistribution) {
        self.sender_chains.entry((author, epoch)).or_insert_with(|| SenderChain::from_distribution(skd));
    }

    /// Seal `plaintext` with my current-epoch sender key. Wire = `u16 hdr_len ‖ hdr ‖ ct`,
    /// AAD = hdr (epoch‖sender‖n). Errors if my identity isn't set.
    pub fn seal_sender_message(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let me = self.my_user_id.clone().ok_or_else(|| ChannelError::Malformed("no identity".into()))?;
        let epoch = self.epoch;
        let sk = self.my_sender.entry(epoch).or_insert_with(SenderKey::generate);
        let (n, mk) = sk.ratchet();
        let hdr = MsgHeader { epoch, sender: me, n }.encode();
        let ct = sk_seal(&mk, plaintext, &hdr).map_err(|_| ChannelError::Encrypt)?;
        let mut out = Vec::with_capacity(2 + hdr.len() + ct.len());
        out.extend_from_slice(&(hdr.len() as u16).to_be_bytes());
        out.extend_from_slice(&hdr);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Open a sender-keyed message (`&mut` — the receiving chain ratchets + deletes the
    /// key). `None` if we lack that sender's chain for the epoch or the key is consumed.
    pub fn open_sender_message(&mut self, wire: &[u8]) -> Option<Vec<u8>> {
        if wire.len() < 2 { return None; }
        let hlen = u16::from_be_bytes([wire[0], wire[1]]) as usize;
        if wire.len() < 2 + hlen { return None; }
        let hdr_bytes = &wire[2..2 + hlen];
        let ct = &wire[2 + hlen..];
        let hdr = MsgHeader::decode(hdr_bytes)?;
        let chain = self.sender_chains.get_mut(&(hdr.sender.clone(), hdr.epoch))?;
        let mk = chain.message_key(hdr.n).ok()?;
        sk_open(&mk, ct, hdr_bytes).ok()
    }
```
(`ChannelError` already has `Encrypt`/`Decrypt`/`Malformed` variants. `bincode::Options` is imported in model.rs already — verify; add if needed.)

- [ ] **Step 3: tests for the sender-key path**

Append in model.rs tests: build two `ChannelState`s for the same channel/epoch (`from_meta` + `set_identity("alice")`/`"bob"`); alice `my_sender_distribution()` → bob `record_sender_chain("alice", epoch, &skd)`; alice `seal_sender_message(b"hi")` → bob `open_sender_message(wire)` == `b"hi"`; out-of-order (alice seals 3, bob opens 2,0,1); single-use (bob can't re-open the same wire — key consumed); a member without alice's chain returns `None`.

- [ ] **Step 4: build, test, commit (ADDITIVE — everything still builds, old tests pass)**
```bash
cd src-tauri && nice -n 10 cargo test --lib channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/channel/model.rs
git commit -m "feat(channel): add sender-key model to ChannelState (additive, alongside group key)"
git status | head -1
```

---

### Task 2: switch `ChannelBook` + Node channel path to sender keys (remove the group-key path)

**Files:** Modify `src-tauri/src/node/channel.rs`, `node/node.rs`.

- [ ] **Step 1: `KeyRotation` distributes per-member SKD**

Rework `seal_keys_for`/`SealedKeys` (in `node/channel.rs`) so a `KeyRotation` event authored by member M carries M's `SenderKeyDistribution` sealed to EACH member (reuse `seal_group_key`/`open_my_key`, which seal/open arbitrary bytes to a member's x25519 — confirm; if they're `GroupKey`-typed, generalize to `&[u8]`). The payload: `{ epoch, sender: M_user_id, entries: Vec<{user_id, sealed(SKD bytes)}> }`. `ChannelBook::process` `KeyRotation` arm: decode, open MY entry → `SenderKeyDistribution::decode` → `state.record_sender_chain(author_uid, epoch, &skd)`.

- [ ] **Step 2: `ChannelBook::process` Message arm uses sender keys**

Change the `Message` arm to `self.states.get_mut(&channel_id)` and `state.open_sender_message(&event.ciphertext)` (single-use; dedup via `emitted` already prevents re-open). `set_identity` on the state when it's created (in the `MembershipChange` arm, after creating the state: `state.set_identity(me.public().user_id())`).

- [ ] **Step 3: Node channel send + create**

`create_channel`: after building membership, generate MY sender key (`state.my_sender_distribution()` once the state exists) and post a `KeyRotation` carrying my SKD sealed per-member. `send_channel_message_reply`: replace `state.seal_message(...)` with `state.seal_sender_message(...)` (lock the channel book mutably). On FIRST send in an epoch, ensure my SKD has been distributed (post a KeyRotation if not yet) — simplest: distribute my SKD at create + on each membership change (rotation). Ensure `set_identity` is called on every state the node owns.

- [ ] **Step 4: remove the dead group-key path**

Remove `ChannelState::{seal_message, open_message, record_key, key_for}` + the `keys: HashMap<u64,GroupKey>` field + `GroupKey` usage in the channel message path that's now unused. Keep `GroupKey`/`seal_group_key` ONLY if still used to seal the SKD bytes (generalize to bytes if needed). Update the existing channel model tests that used the old `seal_message`/`record_key` to the sender-key API (or remove the now-irrelevant ones). Update `ChannelBook`'s rig + the Node channel rigs to the new flow.

- [ ] **Step 5: build, ALL channel tests, clippy, commit (build green again)**
```bash
cd src-tauri && nice -n 10 cargo test --lib channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add -A && git commit -m "feat(channel): channels use the sender-key ratchet; retire the static group key"
git status | head -1
```
Note: `channel_history` will not compile if it called the removed `open_message` — Task 3 fixes history; for THIS task, temporarily make `channel_history` return the already-known data or a minimal form so the build is green, OR do Task 3 in the same commit. Prefer: do Task 3's history-store change in this commit too if `channel_history` would otherwise break.

---

### Task 3: channel received-plaintext store + history (forward-secret history)

**Files:** Modify `src-tauri/src/node/node.rs` (reuse `ReceivedLog` or a channel variant); add the FS rig.

- [ ] **Step 1: store channel plaintext on receive**

Channel messages are now single-use-decrypted, so `channel_history` can't re-open them. Reuse the existing `ReceivedLog` (it's keyed by `ConversationId` — a channel id is a `ConversationId`): when `process_channel` surfaces a `ReceivedChannelMessage`, record it in `self.received` (`record(channel_id, from, wall_clock, wrapped_plaintext, event_id)`). For the SENDER's own channel messages, record into `SentLog` at send time (channel id as the conversation). So both directions persist.

- [ ] **Step 2: `channel_history` from stores**

Rewrite `channel_history(channel, limit)` to merge `SentLog` (own) + `ReceivedLog` (others) for the channel id, decode `MessageBody` for text + reply_to, ids from the event log (seq→id map for sent; `event_id` for received) — mirror the DM `history` rewrite. (The `from_me` channel messages: record them in SentLog in `send_channel_message_reply`.)

- [ ] **Step 3: forward-secrecy loopback rig + regression**

Add a rig: a channel with Alice+Bob; Alice sends 2 messages; Bob receives them (sender-key ratchet) and `channel_history` shows them from the store; re-feeding a message doesn't re-open (single-use); a NON-member (or post-rotation removed member) lacking the SKD can't open. Run ALL channel + node rigs.
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add -A && git commit -m "feat(channel): forward-secret channel history from stores; group-FS loopback rig"
git status | head -1
```

---

## Notes for the reviewer (crypto-aware)

- **Delivered:** channels have forward secrecy via sender keys — each message single-use-keyed, per-sender chains distributed via per-member-sealed SKD on `KeyRotation`, history from local plaintext stores. The AAD is `epoch‖sender‖n` and SKD is fresh per epoch (the two integration contracts).
- **Reviewer checks:** the AAD binds epoch/sender/n; a removed member post-rotation lacks the new SKD → can't read; single-use channel keys (no re-open); `set_identity` set on every node-owned state; no `MutexGuard` across `.await`; channel history served from stores (keys consumed); all existing channel rigs pass via sender keys.
- **Deferred:** late-joiner SKD re-fetch (a sender re-broadcasts SKD on epoch change); prune `SenderChain.order`; zeroize; durable sender-chain state across restart (channel sessions persisted like DM sessions) — fold into a follow-up if `process` rebuilding from the log is insufficient.
