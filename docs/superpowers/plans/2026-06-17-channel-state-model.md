# Channel State + Membership Model Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the pure channel-domain model: the plaintext membership metadata carried by `MembershipChange` events, a random channel id, and a per-member `ChannelState` that tracks membership + the group keys it holds per epoch and seals/opens channel messages with epoch framing.

**Architecture:** Restructure `channel.rs` into a focused module dir — `channel/crypto.rs` (the pure AEAD primitive from Plan 1, moved verbatim) + `channel/model.rs` (new). The model adds `ChannelMeta` (a bincode-encoded membership snapshot — name + member `PublicIdentity`s + epoch — readable metadata), `new_channel_id()` (a random `ConversationId`), and `ChannelState` (id/name/members/epoch + `HashMap<epoch, GroupKey>`) whose `seal_message`/`open_message` prepend the epoch so a receiver knows which key to use. No network, no node coupling.

**Tech Stack:** Rust; the Plan-1 `channel::crypto`, `eventlog::event::ConversationId`, `identity::device::PublicIdentity`; `bincode`/`serde`, `rand` (`OsRng`). No new dependencies.

---

## Background the implementer needs

**This is Plan 2 (of 4) of the "channels/groups" slice** (spec: `docs/superpowers/specs/2026-06-17-channels-design.md` §3). Plan 1 shipped `channel.rs` — the pure crypto primitive (`GroupKey`, `seal/open_group_key`, `seal/open_channel_message`, `ChannelError`). This plan adds the membership/state model and reorganizes the file into a dir. Plan 3 wires it into the node; Plan 4 the app.

**Why the epoch framing:** an `Event` has no "epoch" field, so a channel `Message`'s payload must self-describe which group-key epoch it used. The model frames a message as `epoch (u64 LE) ‖ channel-message-envelope`. A receiver reads the epoch, looks up the key it holds for that epoch, and decrypts — or skips it if it lacks that key (e.g. a member added in a later epoch, or removed in an earlier one).

**Verified APIs:**
```rust
// channel/crypto.rs (Plan 1, being moved — paths stay `crate::channel::*` via re-export)
crate::channel::{GroupKey, ChannelError, seal_group_key, open_group_key, seal_channel_message, open_channel_message};
// GroupKey: Clone; generate(); from_bytes([u8;32]); as_bytes()->&[u8;32]
// seal_channel_message(&GroupKey, &[u8]) -> Result<Vec<u8>, ChannelError>; open_channel_message(&GroupKey, &[u8]) -> Result<Vec<u8>, ChannelError>
// eventlog
crate::eventlog::event::ConversationId; // Copy, Eq, Hash, Serialize, Deserialize; ConversationId::new([u8;32])
// identity
crate::identity::device::PublicIdentity; // { ed25519_pub:[u8;32], x25519_pub:[u8;32] }; Clone, PartialEq, Eq, Serialize, Deserialize; user_id()->String
// fail-closed bincode (mirror discovery::announce::decode)
use bincode::Options; // DefaultOptions::new().with_fixint_encoding().reject_trailing_bytes()
```

**CPU/test discipline (MANDATORY):** never run a bare `cargo test`/`build`. Always:
```bash
cd src-tauri && nice -n 10 cargo test --lib channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```

**Conventions:** focused files; fail-closed bincode decode; `#[cfg(test)] mod tests`; no `thiserror`. Always confirm `git status | head -1` is `On branch feat/redesign-phase0` after committing.

---

### Task 1: Restructure `channel.rs` → `channel/{mod.rs, crypto.rs}`

**Files:**
- Create: `src-tauri/src/channel/mod.rs`
- Create: `src-tauri/src/channel/crypto.rs` (moved content)
- Delete: `src-tauri/src/channel.rs`

- [ ] **Step 1: Move the file**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk/src-tauri/src
mkdir -p channel
git mv channel.rs channel/crypto.rs
```

- [ ] **Step 2: Create `src-tauri/src/channel/mod.rs`**
```rust
//! Channels (group chat): the crypto primitive ([`crypto`]) and the membership +
//! per-epoch key-state model ([`model`]). Re-exported flat so callers use
//! `crate::channel::{GroupKey, ChannelState, …}`.

pub mod crypto;
pub mod model;

pub use crypto::{
    open_channel_message, open_group_key, seal_channel_message, seal_group_key, ChannelError,
    GroupKey,
};
pub use model::{new_channel_id, ChannelMeta, ChannelState};
```

- [ ] **Step 3: Fix the module doc in `channel/crypto.rs`**

The moved file's top doc comment says it is the channel crypto. Leave its code unchanged. (Its `use crate::dm...`, `use crate::identity...` paths still resolve — they are crate-absolute.) No code edits beyond the file move are needed; the `mod tests` stays.

- [ ] **Step 4: Verify the move compiles (model.rs not created yet, so temporarily stub it)**

`mod.rs` references `pub mod model;`, which doesn't exist yet. Create a minimal placeholder so the crate compiles now and Task 2 fills it in:
```bash
# create an empty model.rs placeholder (Task 2 replaces it)
printf '//! Channel membership + key-state model (filled in by the next task).\n' > channel/model.rs
```
And temporarily make `mod.rs` not re-export from the empty model — for THIS task, comment out the model lines:
```rust
pub mod crypto;
pub mod model;

pub use crypto::{
    open_channel_message, open_group_key, seal_channel_message, seal_group_key, ChannelError,
    GroupKey,
};
// pub use model::{new_channel_id, ChannelMeta, ChannelState};  // Task 2
```
Then:
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk/src-tauri
nice -n 10 cargo test --lib channel -- --test-threads=2
nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
```
Expected: the moved crypto tests (9) pass under `channel::crypto::tests`; clippy clean; `crate::channel::GroupKey` etc. still resolve via the re-export.

- [ ] **Step 5: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/channel
git commit -m "refactor(channel): split into channel/{crypto,model}; re-export flat"
git status | head -1
```

---

### Task 2: `channel::model` — ChannelMeta, channel id, ChannelState

**Files:**
- Modify: `src-tauri/src/channel/model.rs` (replace the placeholder)
- Modify: `src-tauri/src/channel/mod.rs` (enable the model re-export)

- [ ] **Step 1: Replace `src-tauri/src/channel/model.rs` with:**
```rust
//! Channel membership + per-epoch key state. `ChannelMeta` is the plaintext
//! membership snapshot carried by a `MembershipChange` event (the post office may
//! read it — membership is accepted metadata; the event is still author-signed).
//! `ChannelState` is a member's view: current membership + the group keys it holds
//! per epoch, used to seal/open channel messages (which self-describe their epoch).

use crate::channel::crypto::{open_channel_message, seal_channel_message, ChannelError, GroupKey};
use crate::eventlog::event::ConversationId;
use crate::identity::device::PublicIdentity;
use bincode::Options;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The plaintext membership snapshot carried by a `MembershipChange` event: the full
/// member set + channel name + epoch. A snapshot (not a delta) so the latest one
/// fully defines current membership on replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub name: String,
    pub members: Vec<PublicIdentity>,
    pub epoch: u64,
}

impl ChannelMeta {
    /// Serialize for an event's payload (fixint, matching the codebase wire style).
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("channel meta serializes")
    }

    /// Parse a membership snapshot, fail-closed (reject trailing bytes).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }

    /// Whether `user_id` (an Ed25519 fingerprint) is a current member.
    pub fn is_member(&self, user_id: &str) -> bool {
        self.members.iter().any(|m| m.user_id() == user_id)
    }
}

/// A fresh random channel id — a `ConversationId` minted at creation, distinct from
/// the derived DM conversation ids.
pub fn new_channel_id() -> ConversationId {
    let mut id = [0u8; 32];
    OsRng.fill_bytes(&mut id);
    ConversationId::new(id)
}

/// A member's view of a channel: its id, current membership/name/epoch, and the
/// group keys this node holds keyed by epoch.
pub struct ChannelState {
    id: ConversationId,
    name: String,
    members: Vec<PublicIdentity>,
    epoch: u64,
    keys: HashMap<u64, GroupKey>,
}

impl ChannelState {
    /// Build state from a channel id and its current membership snapshot (no keys yet).
    pub fn from_meta(id: ConversationId, meta: ChannelMeta) -> Self {
        ChannelState {
            id,
            name: meta.name,
            members: meta.members,
            epoch: meta.epoch,
            keys: HashMap::new(),
        }
    }

    /// Adopt a (possibly newer) membership snapshot: if it advances the epoch, take
    /// its name/members/epoch. Older or equal snapshots are ignored — idempotent
    /// replay regardless of event arrival order.
    pub fn apply_meta(&mut self, meta: ChannelMeta) {
        if meta.epoch >= self.epoch {
            self.name = meta.name;
            self.members = meta.members;
            self.epoch = meta.epoch;
        }
    }

    /// Record the group key this node holds for `epoch` (generated locally or opened
    /// from a sealed key delivered by another member).
    pub fn record_key(&mut self, epoch: u64, key: GroupKey) {
        self.keys.insert(epoch, key);
    }

    pub fn key_for(&self, epoch: u64) -> Option<&GroupKey> {
        self.keys.get(&epoch)
    }
    pub fn id(&self) -> ConversationId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn members(&self) -> &[PublicIdentity] {
        &self.members
    }
    pub fn is_member(&self, user_id: &str) -> bool {
        self.members.iter().any(|m| m.user_id() == user_id)
    }

    /// Seal `plaintext` under the CURRENT epoch's key. The wire payload is
    /// `epoch (u64 LE) ‖ channel-message-envelope`, so any receiver can pick the
    /// right key. Errors if this node lacks the current epoch's key.
    pub fn seal_message(&self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let key = self
            .keys
            .get(&self.epoch)
            .ok_or_else(|| ChannelError::Malformed("no group key for the current epoch".into()))?;
        let envelope = seal_channel_message(key, plaintext)?;
        let mut out = Vec::with_capacity(8 + envelope.len());
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.extend_from_slice(&envelope);
        Ok(out)
    }

    /// Open a channel-message payload (`epoch ‖ envelope`) using the key we hold for
    /// that epoch. `None` if the payload is malformed or we lack that epoch's key
    /// (e.g. a message from before we joined, or after we were removed).
    pub fn open_message(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < 8 {
            return None;
        }
        let (epoch_bytes, envelope) = payload.split_at(8);
        let epoch = u64::from_le_bytes(epoch_bytes.try_into().ok()?);
        let key = self.keys.get(&epoch)?;
        open_channel_message(key, envelope).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    fn meta(name: &str, members: &[&DeviceIdentity], epoch: u64) -> ChannelMeta {
        ChannelMeta {
            name: name.to_string(),
            members: members.iter().map(|d| d.public()).collect(),
            epoch,
        }
    }

    #[test]
    fn channel_meta_round_trips_and_rejects_trailing() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let m = meta("general", &[&a, &b], 0);
        let bytes = m.encode();
        assert_eq!(ChannelMeta::decode(&bytes), Some(m.clone()));
        let mut junk = bytes.clone();
        junk.push(0xAB);
        assert_eq!(ChannelMeta::decode(&junk), None);
        assert!(m.is_member(&a.public().user_id()));
        assert!(!m.is_member("nobody"));
    }

    #[test]
    fn channel_ids_are_random() {
        assert_ne!(new_channel_id(), new_channel_id());
    }

    #[test]
    fn apply_meta_adopts_newer_epoch_only() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut state = ChannelState::from_meta(id, meta("general", &[&a], 0));
        assert_eq!(state.members().len(), 1);

        // Newer epoch (a member added) is adopted.
        state.apply_meta(meta("general", &[&a, &b], 1));
        assert_eq!(state.epoch(), 1);
        assert_eq!(state.members().len(), 2);
        assert!(state.is_member(&b.public().user_id()));

        // An older snapshot is ignored.
        state.apply_meta(meta("general", &[&a], 0));
        assert_eq!(state.epoch(), 1);
        assert_eq!(state.members().len(), 2);
    }

    #[test]
    fn seal_then_open_a_message_at_the_current_epoch() {
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let key = GroupKey::generate();

        // Alice's state: epoch 0 with key0.
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a], 0));
        alice.record_key(0, key.clone());
        let payload = alice.seal_message(b"hi channel").unwrap();

        // Bob holds the same epoch-0 key → opens it.
        let mut bob = ChannelState::from_meta(id, meta("general", &[&a], 0));
        bob.record_key(0, key);
        assert_eq!(bob.open_message(&payload).as_deref(), Some(&b"hi channel"[..]));
    }

    #[test]
    fn open_returns_none_without_the_epochs_key() {
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let key = GroupKey::generate();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a], 0));
        alice.record_key(0, key);
        let payload = alice.seal_message(b"secret").unwrap();

        // A member with NO key for epoch 0 cannot open it.
        let outsider = ChannelState::from_meta(id, meta("general", &[&a], 0));
        assert!(outsider.open_message(&payload).is_none());
        // A too-short payload is also None.
        assert!(outsider.open_message(&[0u8; 4]).is_none());
    }

    #[test]
    fn seal_fails_without_a_key_for_the_current_epoch() {
        let a = DeviceIdentity::generate();
        let state = ChannelState::from_meta(new_channel_id(), meta("general", &[&a], 2));
        assert!(state.seal_message(b"x").is_err());
    }
}
```

- [ ] **Step 2: Enable the model re-export in `src-tauri/src/channel/mod.rs`**

Uncomment / add the model re-export line so it reads:
```rust
pub mod crypto;
pub mod model;

pub use crypto::{
    open_channel_message, open_group_key, seal_channel_message, seal_group_key, ChannelError,
    GroupKey,
};
pub use model::{new_channel_id, ChannelMeta, ChannelState};
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib channel -- --test-threads=2
```
Expected: PASS — the 9 crypto tests + the 6 model tests.

- [ ] **Step 4: fmt + clippy + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/channel/model.rs src-tauri/src/channel/mod.rs
git commit -m "feat(channel): membership snapshot + ChannelState (epoch-keyed seal/open)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
git status | head -1
```

---

## Notes for the reviewer / next plan

- **What this delivers:** the pure channel-domain model — `ChannelMeta` (a fail-closed-decoded membership snapshot for `MembershipChange` events), `new_channel_id()`, and `ChannelState` (membership + per-epoch `GroupKey`s) whose `seal_message`/`open_message` frame the epoch so messages self-describe which key opens them. No network, no node coupling; the `channel` module is now `crypto` (pure AEAD) + `model` (state).
- **Key design point:** epoch framing (`u64 LE ‖ envelope`) lets a member open only the epochs it holds keys for — a member added at epoch 2 can't read epoch-0/1 messages (no key), and a removed member can't read epoch-N messages (never gets key N). This is the rotation-on-membership-change mechanism's read-side.
- **Deferred to Plan 3 (node integration):** generating + sealing the epoch key to each member (`seal_group_key` over the network), minting the channel + posting `MembershipChange`/`KeyRotation` events, sending/receiving channel `Message` events over the event-log sync engine + post office, and surfacing decrypted channel messages. Plan 4: the IPC commands + the channels UI.
- **Reviewer checks:** confirm `PublicIdentity` is `Serialize + Deserialize + PartialEq + Eq` (used in `ChannelMeta`) and has `user_id()`; confirm `ConversationId::new([u8;32])` + `Copy`; confirm the fixint+reject-trailing bincode matches the codebase's `announce`/`dm` decode style.
