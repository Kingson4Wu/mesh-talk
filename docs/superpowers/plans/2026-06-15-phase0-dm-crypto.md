# DM Sealed-Box Crypto Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Seal a direct-message payload so only the recipient can read it — an X3DH-style envelope (static-static DH + per-message ephemeral DH → HKDF → AES-256-GCM) that fills an event's `ciphertext` field.

**Architecture:** A new focused `dm` module. `seal(sender, recipient_x25519_pub, plaintext)` derives a one-time AEAD key from two X25519 Diffie-Hellman outputs — `DH(sender_static, recipient_static)` (authenticates the sender to the recipient) and `DH(ephemeral, recipient_static)` (fresh per message) — via HKDF-SHA256, encrypts with AES-256-GCM, and returns a serialized `SealedEnvelope { ephemeral_pub, ciphertext }`. `open(recipient, sender_x25519_pub, envelope)` recomputes the same key from the mirror DHs and decrypts. No forward secrecy (the accepted v1 tradeoff: compromising a long-term static key exposes past messages); Double Ratchet is a Phase-3 upgrade.

**Tech Stack:** Rust; `x25519-dalek` (DH, already a dependency with `static_secrets`), `hkdf` (HKDF-SHA256, the one new dependency), `aes-gcm` (AEAD, already a dependency), `sha2`/`bincode`/`serde` (already present); the existing `identity::DeviceIdentity`.

---

## Background the implementer needs

**The construction (X3DH-style v1, authenticated, no forward secrecy).** Each device has an X25519 static keypair (its identity key-exchange key). To send Alice→Bob:
```
ephemeral_sk, ephemeral_pk = X25519 keygen        (fresh, per message)
dh1 = X25519(alice_static_sk, bob_static_pk)        # static-static: only Alice & Bob can compute → authenticates Alice
dh2 = X25519(ephemeral_sk,    bob_static_pk)        # ephemeral-static: unique per message → key freshness
key || nonce = HKDF-SHA256(ikm = dh1 || dh2,
                           info = DM_DOMAIN || alice_x25519_pub || bob_x25519_pub || ephemeral_pk)
ciphertext = AES-256-GCM(key, nonce, plaintext)
envelope = { ephemeral_pk, ciphertext }
```
Bob recovers the same `key||nonce` by X25519 symmetry: `DH(bob_static_sk, alice_static_pk) == dh1` and `DH(bob_static_sk, ephemeral_pk) == dh2`, then AES-GCM-decrypts. A wrong sender key (Bob uses the wrong Alice key) or a tampered ciphertext/ephemeral makes the AEAD tag check fail → decryption error. The `info` string is byte-identical on both sides (sender key first, then recipient key, then ephemeral) so the derived key matches.

**Relationship to the event layer.** This is confidentiality + crypto-layer sender authentication for the *payload only*. The payload goes into an event's `ciphertext: Vec<u8>` field, and the **event itself is separately Ed25519-signed** by its author over the content hash (Plan 3), which also covers `conversation_id` and the ciphertext — so replaying a sealed envelope into a different event/conversation breaks the event signature. The `dm` module therefore does NOT need to bind the conversation id; that binding lives at the event layer. (Per the spec, the post office sees only this ciphertext blob + routing metadata, never plaintext.)

**Identity API you build on (`src-tauri/src/identity/device.rs`):**
```rust
impl DeviceIdentity {
    pub fn secret_bytes(&self) -> ([u8; 32], [u8; 32]);  // (ed25519_secret, x25519_secret) — use `.1`
    pub fn public(&self) -> PublicIdentity;               // PublicIdentity { ed25519_pub, x25519_pub: [u8;32] }
}
```
The X25519 static secret bytes (`secret_bytes().1`) reconstruct an `x25519_dalek::StaticSecret`; `public().x25519_pub` is the matching public key.

**x25519-dalek 2.x API (the crate uses `features = ["static_secrets"]`):**
```rust
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};
let s = StaticSecret::from([0u8; 32]);                 // from raw secret bytes
let p = PublicKey::from(&s);                            // or PublicKey::from([u8;32])
let shared = s.diffie_hellman(&peer_pub);              // -> SharedSecret (StaticSecret borrows; can reuse)
let bytes: [u8; 32] = shared.to_bytes();
let eph = EphemeralSecret::random_from_rng(rand::rngs::OsRng);  // consumed by diffie_hellman
let eph_pub = PublicKey::from(&eph);                   // compute BEFORE the DH (which consumes eph)
let dh = eph.diffie_hellman(&peer_pub).to_bytes();
let raw: [u8; 32] = p.to_bytes();
```
(`identity/device.rs` already uses `StaticSecret::random_from_rng(rand::rngs::OsRng)`, so `OsRng` is the established RNG.)

**aes-gcm 0.10 + hkdf 0.12 usage:** read `src-tauri/src/storage/encryption.rs` to mirror how `Aes256Gcm` is constructed and called there (same crate version). The standard API:
```rust
use aes_gcm::{Aes256Gcm, Nonce, KeyInit};
use aes_gcm::aead::Aead;
let cipher = Aes256Gcm::new_from_slice(&key_bytes32).map_err(|_| ...)?;   // 32-byte key
let ct = cipher.encrypt(Nonce::from_slice(&nonce12), plaintext).map_err(|_| ...)?;
let pt = cipher.decrypt(Nonce::from_slice(&nonce12), ct.as_ref()).map_err(|_| ...)?;

use hkdf::Hkdf;
use sha2::Sha256;
let hk = Hkdf::<Sha256>::new(None, &ikm);   // None salt
hk.expand(&info, &mut okm).expect("hkdf okm len <= 255*32");
```

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare full `cargo test`/`cargo build`. Always `nice` + throttle, scoped:
```bash
cd src-tauri && nice -n 10 cargo test --lib dm:: -- --test-threads=2
nice -n 10 cargo fmt
nice -n 10 cargo clippy --lib -- -D warnings
```
The pre-commit hook runs the full health check on commit — let it run.

**Conventions:** match `eventlog`/`transport` — hand-written error enums (NO `thiserror`) with `Display`+`Error` impls, `#[cfg(test)] mod tests`. rustfmt's `reorder_modules` sorts `pub mod` lines — keep them alphabetical.

---

### Task 1: `dm` module skeleton — dependency, types, key derivation

**Files:**
- Modify: `src-tauri/Cargo.toml` (`[dependencies]`)
- Create: `src-tauri/src/dm.rs`
- Modify: `src-tauri/src/lib.rs` (module declaration)

- [ ] **Step 1: Add the `hkdf` dependency**

In `src-tauri/Cargo.toml`, under `[dependencies]`, add next to the `sha2` line:
```toml
hkdf = "0.12"
```
`hkdf` 0.12 is RustCrypto, pure Rust (no C deps), licensed `MIT OR Apache-2.0` — both already in `deny.toml`'s allowlist. It uses the `sha2` crate already present.

- [ ] **Step 2: Register the module in `lib.rs`**

In `src-tauri/src/lib.rs`, add `pub mod dm;` in alphabetical position — **between** `pub mod crypto;` and `pub mod domain;** (`dm` sorts after `crypto`, before `domain`):
```rust
pub mod crypto;
pub mod dm;
pub mod domain;
```

- [ ] **Step 3: Create `src-tauri/src/dm.rs` with the skeleton + derivation helper + tests**

```rust
//! Direct-message payload sealing (X3DH-style v1).
//!
//! [`seal`] encrypts a plaintext so only the recipient can read it, deriving a
//! one-time AES-256-GCM key from two X25519 Diffie-Hellman outputs:
//! `DH(sender_static, recipient_static)` (authenticates the sender to the
//! recipient) and `DH(ephemeral, recipient_static)` (fresh per message). There
//! is no forward secrecy — compromising a long-term X25519 key exposes past
//! messages — which is the accepted v1 tradeoff (Double Ratchet is Phase 3).
//!
//! The sealed bytes fill an event's `ciphertext` field. The event is separately
//! Ed25519-signed over its content (Plan 3), which binds the ciphertext to its
//! conversation and author, so this module does not bind those itself.

use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// Domain separator for DM key derivation.
const DM_DOMAIN: &[u8] = b"mesh-talk-dm-v1";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// A recipient-sealed message: the sender's per-message ephemeral X25519 public
/// key plus the AES-256-GCM ciphertext (including its 16-byte tag).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedEnvelope {
    pub ephemeral_pub: [u8; 32],
    pub ciphertext: Vec<u8>,
}

/// Errors from sealing / opening a DM.
#[derive(Debug)]
pub enum DmError {
    /// AEAD encryption failed (should not happen for valid inputs).
    Encrypt,
    /// AEAD decryption failed — wrong key, wrong sender, or tampered ciphertext.
    Decrypt,
    /// (De)serialization of the envelope failed.
    Serialization(String),
}

impl std::fmt::Display for DmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DmError::Encrypt => write!(f, "dm encryption failed"),
            DmError::Decrypt => write!(f, "dm decryption failed"),
            DmError::Serialization(m) => write!(f, "dm serialization error: {m}"),
        }
    }
}

impl std::error::Error for DmError {}

/// Derive the AEAD key and nonce from the two DH outputs and the public context.
/// `info` binds the key to the sender, recipient, and ephemeral public keys so a
/// ciphertext cannot be reinterpreted under a different triple.
fn derive_key_nonce(
    dh1: &[u8; 32],
    dh2: &[u8; 32],
    sender_x25519_pub: &[u8; 32],
    recipient_x25519_pub: &[u8; 32],
    ephemeral_pub: &[u8; 32],
) -> ([u8; KEY_LEN], [u8; NONCE_LEN]) {
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(dh1);
    ikm[32..].copy_from_slice(dh2);

    let mut info = Vec::with_capacity(DM_DOMAIN.len() + 96);
    info.extend_from_slice(DM_DOMAIN);
    info.extend_from_slice(sender_x25519_pub);
    info.extend_from_slice(recipient_x25519_pub);
    info.extend_from_slice(ephemeral_pub);

    let hk = Hkdf::<Sha256>::new(None, &ikm);
    let mut okm = [0u8; KEY_LEN + NONCE_LEN];
    hk.expand(&info, &mut okm).expect("hkdf okm length is valid");

    let mut key = [0u8; KEY_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    key.copy_from_slice(&okm[..KEY_LEN]);
    nonce.copy_from_slice(&okm[KEY_LEN..]);
    (key, nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivation_is_deterministic_and_input_sensitive() {
        let dh1 = [1u8; 32];
        let dh2 = [2u8; 32];
        let a = [3u8; 32];
        let b = [4u8; 32];
        let e = [5u8; 32];

        let (k1, n1) = derive_key_nonce(&dh1, &dh2, &a, &b, &e);
        let (k2, n2) = derive_key_nonce(&dh1, &dh2, &a, &b, &e);
        assert_eq!(k1, k2);
        assert_eq!(n1, n2);

        // Any change in the inputs changes the derived key.
        let (k3, _) = derive_key_nonce(&[9u8; 32], &dh2, &a, &b, &e);
        assert_ne!(k1, k3);
        let (k4, _) = derive_key_nonce(&dh1, &dh2, &a, &b, &[7u8; 32]);
        assert_ne!(k1, k4);
        // Swapping sender/recipient context changes the key (direction matters).
        let (k5, _) = derive_key_nonce(&dh1, &dh2, &b, &a, &e);
        assert_ne!(k1, k5);
    }

    #[test]
    fn envelope_round_trips_through_bincode() {
        let env = SealedEnvelope { ephemeral_pub: [8u8; 32], ciphertext: vec![1, 2, 3, 4] };
        let bytes = bincode::serialize(&env).unwrap();
        let back: SealedEnvelope = bincode::deserialize(&bytes).unwrap();
        assert_eq!(env, back);
    }
}
```

- [ ] **Step 4: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib dm:: -- --test-threads=2
```
Expected: PASS — 2 tests.

- [ ] **Step 5: Verify cargo-deny still passes (license gate for the new dep)**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk && nice -n 10 cargo deny check licenses 2>&1 | tail -20
```
Expected: no license errors (`hkdf` is `MIT OR Apache-2.0`, both allowlisted). If a transitive dep trips an unlisted license, add that id to the `allow` list in `deny.toml` and note it in the commit. (If `cargo deny` is not installed, note that and skip — do not install it.)

- [ ] **Step 6: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/lib.rs src-tauri/src/dm.rs
git commit -m "feat(dm): module skeleton + HKDF key derivation for sealed DMs

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: `seal` and `open`

**Files:**
- Modify: `src-tauri/src/dm.rs` (add `seal`, `open` + round-trip tests)

- [ ] **Step 1: Add the imports and `seal` / `open` functions**

In `src-tauri/src/dm.rs`, add to the top `use` block:
```rust
use crate::identity::device::DeviceIdentity;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};
```
> Confirm the `aes-gcm` import paths against `src-tauri/src/storage/encryption.rs` (same crate version) and adjust if that file uses a different path for `Aes256Gcm`/`Nonce`/`Aead`/`KeyInit`.

Then add these functions after `derive_key_nonce` (before the `#[cfg(test)]` block):
```rust
/// Seal `plaintext` so only the holder of `recipient_x25519_pub`'s secret can
/// read it. Returns the serialized [`SealedEnvelope`] (goes into an event's
/// `ciphertext`). The recipient authenticates the sender via the static-static
/// DH, so they must know which sender key to expect when opening.
pub fn seal(
    sender: &DeviceIdentity,
    recipient_x25519_pub: &[u8; 32],
    plaintext: &[u8],
) -> Result<Vec<u8>, DmError> {
    let sender_static = StaticSecret::from(sender.secret_bytes().1);
    let sender_x_pub = sender.public().x25519_pub;
    let recipient_pub = PublicKey::from(*recipient_x25519_pub);

    // Fresh per-message ephemeral; compute its public key before the DH consumes it.
    let ephemeral = EphemeralSecret::random_from_rng(rand::rngs::OsRng);
    let ephemeral_pub = PublicKey::from(&ephemeral);

    let dh1 = sender_static.diffie_hellman(&recipient_pub).to_bytes();
    let dh2 = ephemeral.diffie_hellman(&recipient_pub).to_bytes();

    let (key, nonce) = derive_key_nonce(
        &dh1,
        &dh2,
        &sender_x_pub,
        recipient_x25519_pub,
        &ephemeral_pub.to_bytes(),
    );

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| DmError::Encrypt)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| DmError::Encrypt)?;

    let envelope = SealedEnvelope { ephemeral_pub: ephemeral_pub.to_bytes(), ciphertext };
    bincode::serialize(&envelope).map_err(|e| DmError::Serialization(e.to_string()))
}

/// Open a sealed envelope addressed to `recipient`, authenticating it came from
/// `sender_x25519_pub`. Returns the plaintext, or [`DmError::Decrypt`] if the
/// sender key is wrong or the envelope was tampered.
pub fn open(
    recipient: &DeviceIdentity,
    sender_x25519_pub: &[u8; 32],
    envelope_bytes: &[u8],
) -> Result<Vec<u8>, DmError> {
    let envelope: SealedEnvelope =
        bincode::deserialize(envelope_bytes).map_err(|e| DmError::Serialization(e.to_string()))?;

    let recipient_static = StaticSecret::from(recipient.secret_bytes().1);
    let recipient_x_pub = recipient.public().x25519_pub;
    let sender_pub = PublicKey::from(*sender_x25519_pub);
    let ephemeral_pub = PublicKey::from(envelope.ephemeral_pub);

    let dh1 = recipient_static.diffie_hellman(&sender_pub).to_bytes();
    let dh2 = recipient_static.diffie_hellman(&ephemeral_pub).to_bytes();

    let (key, nonce) = derive_key_nonce(
        &dh1,
        &dh2,
        sender_x25519_pub,
        &recipient_x_pub,
        &envelope.ephemeral_pub,
    );

    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| DmError::Decrypt)?;
    cipher
        .decrypt(Nonce::from_slice(&nonce), envelope.ciphertext.as_ref())
        .map_err(|_| DmError::Decrypt)
}
```

- [ ] **Step 2: Add the round-trip tests**

Add to the `#[cfg(test)] mod tests` block. First add this import at the top of the `tests` module (after `use super::*;`):
```rust
    use crate::identity::device::DeviceIdentity;
```
Then:
```rust
    #[test]
    fn seal_then_open_round_trips() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        let sealed = seal(&alice, &bob.public().x25519_pub, b"hello bob").unwrap();
        let opened = open(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened, b"hello bob");
    }

    #[test]
    fn round_trips_empty_and_large_plaintexts() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Empty payload.
        let sealed = seal(&alice, &bob.public().x25519_pub, b"").unwrap();
        assert_eq!(open(&bob, &alice.public().x25519_pub, &sealed).unwrap(), b"");

        // Large payload.
        let big = vec![0x5au8; 100_000];
        let sealed = seal(&alice, &bob.public().x25519_pub, &big).unwrap();
        assert_eq!(open(&bob, &alice.public().x25519_pub, &sealed).unwrap(), big);
    }
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib dm:: -- --test-threads=2
```
Expected: PASS — 4 tests (2 from Task 1 + 2 new).

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/dm.rs
git commit -m "feat(dm): X3DH-style seal and open

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Security properties + event integration

**Files:**
- Modify: `src-tauri/src/dm.rs` (add adversarial + integration tests)

- [ ] **Step 1: Add the security and integration tests**

Add to the `#[cfg(test)] mod tests` block in `src-tauri/src/dm.rs`. These use the `eventlog::Event` to prove the sealed bytes fit an event's `ciphertext`:
```rust
    use crate::eventlog::event::{ConversationId, Event, EventKind};

    #[test]
    fn wrong_recipient_cannot_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let eve = DeviceIdentity::generate();

        // Sealed for Bob; Eve (using Alice's key as the claimed sender) cannot open.
        let sealed = seal(&alice, &bob.public().x25519_pub, b"secret").unwrap();
        assert!(matches!(
            open(&eve, &alice.public().x25519_pub, &sealed),
            Err(DmError::Decrypt)
        ));
    }

    #[test]
    fn wrong_sender_key_fails_authentication() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mallory = DeviceIdentity::generate();

        // Sealed by Alice for Bob. Bob tries to open it claiming Mallory is the
        // sender → the static-static DH mismatches → decryption fails.
        let sealed = seal(&alice, &bob.public().x25519_pub, b"secret").unwrap();
        assert!(matches!(
            open(&bob, &mallory.public().x25519_pub, &sealed),
            Err(DmError::Decrypt)
        ));
        // With the correct sender key it opens.
        assert_eq!(open(&bob, &alice.public().x25519_pub, &sealed).unwrap(), b"secret");
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let sealed = seal(&alice, &bob.public().x25519_pub, b"secret").unwrap();

        // Flip a byte inside the serialized envelope's ciphertext region. The
        // envelope is bincode { ephemeral_pub:[u8;32], ciphertext:Vec<u8> }; the
        // last byte is inside the AEAD tag.
        let mut bytes = sealed.clone();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert!(matches!(open(&bob, &alice.public().x25519_pub, &bytes), Err(DmError::Decrypt)));
    }

    #[test]
    fn tampered_ephemeral_key_fails_to_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let sealed = seal(&alice, &bob.public().x25519_pub, b"secret").unwrap();

        // Deserialize, corrupt the ephemeral public key, re-serialize.
        let mut env: SealedEnvelope = bincode::deserialize(&sealed).unwrap();
        env.ephemeral_pub[0] ^= 0xFF;
        let tampered = bincode::serialize(&env).unwrap();
        assert!(matches!(
            open(&bob, &alice.public().x25519_pub, &tampered),
            Err(DmError::Decrypt)
        ));
    }

    #[test]
    fn each_seal_uses_a_fresh_ephemeral() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Two seals of the same plaintext differ (fresh ephemeral each time) and
        // both still open correctly.
        let s1 = seal(&alice, &bob.public().x25519_pub, b"same").unwrap();
        let s2 = seal(&alice, &bob.public().x25519_pub, b"same").unwrap();
        assert_ne!(s1, s2);
        assert_eq!(open(&bob, &alice.public().x25519_pub, &s1).unwrap(), b"same");
        assert_eq!(open(&bob, &alice.public().x25519_pub, &s2).unwrap(), b"same");
    }

    #[test]
    fn garbage_envelope_is_rejected_without_panic() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        // Not a valid bincode SealedEnvelope.
        assert!(open(&bob, &alice.public().x25519_pub, &[0u8; 3]).is_err());
        // Empty input.
        assert!(open(&bob, &alice.public().x25519_pub, &[]).is_err());
    }

    #[test]
    fn sealed_payload_round_trips_inside_an_event() {
        // The sealed bytes are what fills an event's `ciphertext`. Seal, place it
        // in a Message event authored by Alice, then open from the event field.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        let sealed = seal(&alice, &bob.public().x25519_pub, b"hi from a dm event").unwrap();
        let event = Event::new(
            &alice,
            ConversationId::new([1u8; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            sealed,
        );

        let opened = open(&bob, &alice.public().x25519_pub, &event.ciphertext).unwrap();
        assert_eq!(opened, b"hi from a dm event");
    }
```

- [ ] **Step 2: Run the whole `dm` suite**
```bash
cd src-tauri && nice -n 10 cargo test --lib dm:: -- --test-threads=2
```
Expected: PASS — 11 tests (4 + 7 new).

- [ ] **Step 3: Full fmt + clippy gate**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt --check; echo "fmt EXIT: $?"
```
Expected: clippy clean; `fmt EXIT: 0`.

- [ ] **Step 4: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/dm.rs
git commit -m "test(dm): authentication, tamper, freshness, and event-integration tests

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next phase

- **What this delivers:** the DM payload crypto primitive — `seal(sender, recipient_x25519_pub, plaintext) -> Vec<u8>` and `open(recipient, sender_x25519_pub, envelope) -> Vec<u8>`, plus the `SealedEnvelope` type — confidential and sender-authenticated, filling an event's `ciphertext`.
- **Security properties:** only the recipient's X25519 secret can derive the key (confidentiality); the static-static DH means only the claimed sender could have produced it (authentication, in addition to the event's Ed25519 signature); HKDF `info` binds the key to the sender/recipient/ephemeral triple; a fresh ephemeral per message gives a unique key+nonce (no AEAD nonce reuse); tampering with the ciphertext or ephemeral key fails the AEAD tag; malformed input errors without panicking.
- **Deliberately deferred (do NOT add here):** forward secrecy / Double Ratchet (Phase 3 — accepted v1 tradeoff); channel (group) crypto via sender-keys (Plan 6 / Phase 1 — a separate module); the conversation layer that assembles `Message` events, looks up the recipient's X25519 key from the roster, and wires seal/open into send/receive; and key zeroization of the derived AEAD key (a Phase-1 hardening, consistent with the keystore's current handling).
- **Accepted v1 limitations:** no forward secrecy (long-term static key compromise exposes past messages); the recipient must know the sender's X25519 key (from the roster/discovery) to open — fine, since the author is already cleartext event metadata.
