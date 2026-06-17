# Channel Crypto Primitive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `channel` crypto primitive — a per-channel symmetric `GroupKey`, sealed to each member via the existing DM sealed-box, and raw-key AES-256-GCM for channel messages — a pure, unit-tested module with no network or state.

**Architecture:** `channel::GroupKey` wraps a random 32-byte key. `seal_group_key`/`open_group_key` reuse `dm::seal`/`dm::open` to distribute the key to a member's X25519 key. `seal_channel_message`/`open_channel_message` use `aes_gcm::Aes256Gcm` keyed by the group key with a fresh random 12-byte nonce prepended (the standard sealed-message construction; the same AEAD the DM module uses, keyed by a shared group key instead of a per-message DH-derived key).

**Tech Stack:** Rust; `aes-gcm` 0.10, `rand` 0.8 (`OsRng`); the existing `dm` module + `identity`. No new dependencies.

---

## Background the implementer needs

**This is Plan 1 (of 4) of the "channels/groups" slice** (spec: `docs/superpowers/specs/2026-06-17-channels-design.md`). It is the foundational crypto unit — analogous to how the `dm` sealed-box was the first DM primitive. Later plans add the channel state/membership model, node integration, and the app. Authorship of a channel message is proven by the per-event Ed25519 signature (the event log already signs events), so a single per-channel group key (not per-sender keys) suffices for v1.

**Verified APIs you reuse (from the codebase):**
```rust
// dm.rs — the sealed box (reused to distribute the group key)
crate::dm::seal(sender: &DeviceIdentity, recipient_x25519_pub: &[u8;32], plaintext: &[u8]) -> Result<Vec<u8>, dm::DmError>;
crate::dm::open(recipient: &DeviceIdentity, sender_x25519_pub: &[u8;32], envelope_bytes: &[u8]) -> Result<Vec<u8>, dm::DmError>;
pub enum dm::DmError { Encrypt, Decrypt, Serialization(String) } // Display
// identity
crate::identity::device::DeviceIdentity; // ::generate(); .public() -> PublicIdentity { ed25519_pub:[u8;32], x25519_pub:[u8;32] }
// aes-gcm 0.10 — the AEAD (mirror dm.rs's usage exactly)
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use aes_gcm::aead::Aead; // for .encrypt / .decrypt (confirm against dm.rs's imports — copy whatever it imports)
// rand 0.8
use rand::rngs::OsRng;
use rand::RngCore; // for OsRng.fill_bytes(&mut buf)
```
**Mirror `src-tauri/src/dm.rs`** for the exact `aes_gcm` import set and the `Aes256Gcm::new_from_slice(&key)?` + `cipher.encrypt(Nonce::from_slice(&nonce), plaintext)?` call shape — match it line-for-line so the two stay maintainable together.

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare `cargo test`/`build`. Always:
```bash
cd src-tauri && nice -n 10 cargo test --lib channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```

**Conventions:** hand-written error enum (no `thiserror`) + `Display` + `Error` + `From`; `#[cfg(test)] mod tests`; reject malformed input (fail-closed); match the `dm`/`identity` module style.

---

### Task 1: The `channel` crypto module

**Files:**
- Create: `src-tauri/src/channel.rs`
- Modify: `src-tauri/src/lib.rs` (declare `pub mod channel;`)

- [ ] **Step 1: Declare the module in `src-tauri/src/lib.rs`**

Add `pub mod channel;` alongside the other top-level `pub mod` declarations (e.g. right after `pub mod api;` / next to `pub mod dm;` — keep them roughly grouped/sorted as the file does).

- [ ] **Step 2: Create `src-tauri/src/channel.rs` with EXACTLY this content (adapt only the `aes_gcm` imports to match `dm.rs`)**
```rust
//! Channel (group) crypto primitive: a per-channel symmetric group key, sealed to
//! each member via the DM sealed-box, and raw-key AES-256-GCM for channel messages.
//! Pure crypto — no network, no state. Authorship of a channel message is proven by
//! the per-event Ed25519 signature (see `eventlog`), so a single shared group key
//! (not per-sender keys) suffices for v1; per-sender ratcheting is a later refinement.

use crate::dm::{self, DmError};
use crate::identity::device::DeviceIdentity;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;

const KEY_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;

/// A per-channel symmetric group key (AES-256-GCM). Generated fresh, distributed to
/// members via [`seal_group_key`], and rotated on membership change (by the caller).
#[derive(Clone)]
pub struct GroupKey([u8; KEY_SIZE]);

impl GroupKey {
    /// A fresh random group key.
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_SIZE];
        OsRng.fill_bytes(&mut key);
        GroupKey(key)
    }

    /// Wrap raw key bytes (e.g. after opening a sealed key).
    pub fn from_bytes(bytes: [u8; KEY_SIZE]) -> Self {
        GroupKey(bytes)
    }

    /// The raw 32 key bytes.
    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        &self.0
    }
}

/// Errors from channel crypto.
#[derive(Debug)]
pub enum ChannelError {
    /// Sealing/opening the group key (via the DM sealed-box) failed.
    Seal(DmError),
    /// AES-256-GCM message encryption failed.
    Encrypt,
    /// AES-256-GCM message decryption failed (wrong key or tampered).
    Decrypt,
    /// A malformed envelope (bad length / structure).
    Malformed(String),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::Seal(e) => write!(f, "channel key seal error: {e}"),
            ChannelError::Encrypt => write!(f, "channel message encrypt error"),
            ChannelError::Decrypt => write!(f, "channel message decrypt error"),
            ChannelError::Malformed(m) => write!(f, "malformed channel envelope: {m}"),
        }
    }
}

impl std::error::Error for ChannelError {}

impl From<DmError> for ChannelError {
    fn from(e: DmError) -> Self {
        ChannelError::Seal(e)
    }
}

/// Seal `key` for a member: encrypt the raw key bytes to their X25519 key using the
/// DM sealed-box. Only that member can open it; the post office never can.
pub fn seal_group_key(
    sender: &DeviceIdentity,
    recipient_x25519_pub: &[u8; KEY_SIZE],
    key: &GroupKey,
) -> Result<Vec<u8>, ChannelError> {
    dm::seal(sender, recipient_x25519_pub, key.as_bytes()).map_err(ChannelError::Seal)
}

/// Open a sealed group key addressed to us (the inverse of [`seal_group_key`]).
pub fn open_group_key(
    recipient: &DeviceIdentity,
    sender_x25519_pub: &[u8; KEY_SIZE],
    envelope: &[u8],
) -> Result<GroupKey, ChannelError> {
    let bytes = dm::open(recipient, sender_x25519_pub, envelope).map_err(ChannelError::Seal)?;
    let arr: [u8; KEY_SIZE] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| ChannelError::Malformed("group key is not 32 bytes".into()))?;
    Ok(GroupKey(arr))
}

/// Encrypt a channel message with the group key: `nonce ‖ AES-256-GCM(key, nonce,
/// plaintext)`, with a fresh random 96-bit nonce per message.
pub fn seal_channel_message(key: &GroupKey, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| ChannelError::Encrypt)?;
    let mut nonce = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| ChannelError::Encrypt)?;
    let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a channel message produced by [`seal_channel_message`]. Returns
/// `ChannelError::Decrypt` for the wrong key or a tampered ciphertext.
pub fn open_channel_message(key: &GroupKey, envelope: &[u8]) -> Result<Vec<u8>, ChannelError> {
    if envelope.len() < NONCE_SIZE {
        return Err(ChannelError::Malformed(
            "channel envelope shorter than nonce".into(),
        ));
    }
    let (nonce, ciphertext) = envelope.split_at(NONCE_SIZE);
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| ChannelError::Decrypt)?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| ChannelError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_key_seals_to_a_member_and_opens() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let key = GroupKey::generate();

        let sealed = seal_group_key(&alice, &bob.public().x25519_pub, &key).unwrap();
        let opened = open_group_key(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened.as_bytes(), key.as_bytes());
    }

    #[test]
    fn a_non_member_cannot_open_the_group_key() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let key = GroupKey::generate();

        // Sealed to Bob; Carol (the wrong recipient) cannot open it.
        let sealed = seal_group_key(&alice, &bob.public().x25519_pub, &key).unwrap();
        assert!(open_group_key(&carol, &alice.public().x25519_pub, &sealed).is_err());
    }

    #[test]
    fn channel_message_round_trips_with_the_group_key() {
        let key = GroupKey::generate();
        let sealed = seal_channel_message(&key, b"hello channel").unwrap();
        let opened = open_channel_message(&key, &sealed).unwrap();
        assert_eq!(opened, b"hello channel");
    }

    #[test]
    fn a_different_group_key_cannot_decrypt() {
        let key = GroupKey::generate();
        let other = GroupKey::generate();
        let sealed = seal_channel_message(&key, b"secret").unwrap();
        assert!(open_channel_message(&other, &sealed).is_err());
    }

    #[test]
    fn a_tampered_channel_message_fails_to_open() {
        let key = GroupKey::generate();
        let mut sealed = seal_channel_message(&key, b"secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF; // flip a byte in the AEAD tag region
        assert!(open_channel_message(&key, &sealed).is_err());
    }

    #[test]
    fn two_seals_of_the_same_plaintext_differ() {
        // Fresh random nonce per message → no deterministic ciphertext reuse.
        let key = GroupKey::generate();
        let a = seal_channel_message(&key, b"same").unwrap();
        let b = seal_channel_message(&key, b"same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_short_envelope_is_malformed() {
        let key = GroupKey::generate();
        assert!(matches!(
            open_channel_message(&key, &[0u8; 4]),
            Err(ChannelError::Malformed(_))
        ));
    }

    #[test]
    fn opening_a_non_key_sized_payload_is_malformed() {
        // Alice seals an arbitrary (non-32-byte) payload to Bob via the DM box;
        // opening it AS a group key must reject the wrong length.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let sealed = dm::seal(&alice, &bob.public().x25519_pub, b"not a key").unwrap();
        assert!(matches!(
            open_group_key(&bob, &alice.public().x25519_pub, &sealed),
            Err(ChannelError::Malformed(_))
        ));
    }
}
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib channel -- --test-threads=2
```
Expected: PASS — 8 tests.

- [ ] **Step 4: fmt + clippy + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/lib.rs src-tauri/src/channel.rs
git commit -m "feat(channel): group-key crypto primitive (seal/open key + channel message AEAD)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
git status | head -1   # confirm On branch feat/redesign-phase0
```
(Pre-commit hook runs the full health check — let it run; if it fails, read its output and fix.)

## If something doesn't compile
- Match `src-tauri/src/dm.rs`'s exact `aes_gcm` imports (the `aead::Aead` trait import name may differ slightly — copy dm.rs's `use aes_gcm::...` lines verbatim). `Aes256Gcm::new_from_slice` + `cipher.encrypt`/`cipher.decrypt` are the methods dm.rs uses.
- `OsRng.fill_bytes` needs `use rand::RngCore;` in scope (rand 0.8). dm.rs / device.rs show the rand usage.
- `dm::seal`/`dm::open` and `DmError` are `pub` in `crate::dm`. `PublicIdentity.x25519_pub` is a public `[u8;32]`.
- Adapt to the real API without changing behavior or the tests. Note any deviation.

## Report back
- STATUS: DONE / DONE_WITH_CONCERNS / BLOCKED / NEEDS_CONTEXT
- Test result (X/8) + commit SHA + the final `git status` line (MUST be "On branch feat/redesign-phase0")
- Any deviations (esp. the aes_gcm imports) and why; any concerns
