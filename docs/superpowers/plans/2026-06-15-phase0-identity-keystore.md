# Phase 0 · Plan 1 — Identity & Keystore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give each device a key-based identity (Ed25519 signing + X25519 key-exchange) with a self-certifying `user_id`, persisted in a password-encrypted keystore.

**Architecture:** A new `identity::device` module holds the in-memory `DeviceIdentity` (the two secret keys) and exposes its `PublicIdentity` + a fingerprint `user_id`. A new `identity::keystore` module serializes the secret keys and seals them with the project's existing password-based AEAD (`storage::encryption`: PBKDF2 → AES-256-GCM). No migration of existing data (pre-release).

**Tech Stack:** Rust, `ed25519-dalek` (already a dependency), `x25519-dalek` (new), `sha2`/`hex` (already present), `storage::encryption` (existing in-repo).

This plan is the foundation for the redesign spec at
`docs/superpowers/specs/2026-06-15-mesh-talk-redesign-design.md`.

---

## File structure

- Create `src-tauri/src/identity/device.rs` — `DeviceIdentity`, `PublicIdentity`, fingerprint/`user_id`, signing. Tests inline.
- Create `src-tauri/src/identity/keystore.rs` — encrypt/decrypt the secret keys to/from a file; `load_or_create`. Tests inline.
- Modify `src-tauri/src/identity/mod.rs` — add `pub mod device;` and `pub mod keystore;`.
- Modify `src-tauri/Cargo.toml` — add `x25519-dalek`.

> Note: this introduces the *new* identity model alongside the existing
> `identity::manager`/`keys`. Later plans migrate callers over and delete the old
> path. Do not touch the old modules in this plan.

All cargo commands run from `src-tauri/` and MUST be prefixed with `nice -n 10`
and use `-- --test-threads=2` for tests (repo CPU guard; see `.cargo/config.toml`
and `.claude/hooks/`).

---

### Task 1: Add the x25519 dependency

**Files:**
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `src-tauri/Cargo.toml`, under `[dependencies]`, next to the existing
`ed25519-dalek` line, add:

```toml
x25519-dalek = { version = "2", features = ["static_secrets"] }
```

- [ ] **Step 2: Verify it resolves and compiles**

Run: `nice -n 10 cargo check`
Expected: `Finished` with no errors (it will download `x25519-dalek`).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/../Cargo.lock
git commit -m "build: add x25519-dalek for key-exchange identity"
```

---

### Task 2: `PublicIdentity` and the self-certifying `user_id`

**Files:**
- Create: `src-tauri/src/identity/device.rs`
- Modify: `src-tauri/src/identity/mod.rs`

- [ ] **Step 1: Register the module**

Add to `src-tauri/src/identity/mod.rs` (after the existing `pub mod` lines):

```rust
pub mod device;
```

- [ ] **Step 2: Write the failing test**

Create `src-tauri/src/identity/device.rs` with ONLY this content for now:

```rust
//! Device identity: Ed25519 (signing) + X25519 (key exchange) keys, with a
//! self-certifying `user_id` derived from the signing public key.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The public half of a device identity — safe to share on the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicIdentity {
    /// Ed25519 verifying (signature) key.
    pub ed25519_pub: [u8; 32],
    /// X25519 public key for Diffie-Hellman key exchange.
    pub x25519_pub: [u8; 32],
}

impl PublicIdentity {
    /// Stable, self-certifying user id = hex of the first 16 bytes of
    /// SHA-256("mesh-talk-id-v1" || ed25519_pub). 32 hex chars.
    pub fn user_id(&self) -> String {
        Self::user_id_from(&self.ed25519_pub)
    }

    pub fn user_id_from(ed25519_pub: &[u8; 32]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"mesh-talk-id-v1");
        hasher.update(ed25519_pub);
        let digest = hasher.finalize();
        hex::encode(&digest[..16])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_id_is_stable_and_derived_from_signing_key() {
        let pid = PublicIdentity {
            ed25519_pub: [7u8; 32],
            x25519_pub: [9u8; 32],
        };
        let id = pid.user_id();
        assert_eq!(id.len(), 32, "user_id is 32 hex chars");
        // Deterministic: same key -> same id.
        assert_eq!(id, pid.user_id());
        // Independent of the x25519 key.
        let pid2 = PublicIdentity {
            ed25519_pub: [7u8; 32],
            x25519_pub: [0u8; 32],
        };
        assert_eq!(id, pid2.user_id());
        // Different signing key -> different id.
        let pid3 = PublicIdentity {
            ed25519_pub: [8u8; 32],
            x25519_pub: [9u8; 32],
        };
        assert_ne!(id, pid3.user_id());
    }
}
```

- [ ] **Step 3: Run the test to verify it passes**

Run: `nice -n 10 cargo test --lib -- --test-threads=2 identity::device::tests::user_id_is_stable`
Expected: PASS (1 test).

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/identity/device.rs src-tauri/src/identity/mod.rs
git commit -m "feat(identity): PublicIdentity with self-certifying user_id"
```

---

### Task 3: `DeviceIdentity` — generate, public bundle, sign

**Files:**
- Modify: `src-tauri/src/identity/device.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src-tauri/src/identity/device.rs`:

```rust
    #[test]
    fn generated_identity_signs_and_exposes_matching_public() {
        let id = DeviceIdentity::generate();
        let public = id.public();

        // user_id from the device matches the one derived from its public bundle.
        assert_eq!(id.user_id(), public.user_id());

        // A signature over a message verifies against the public signing key.
        let msg = b"hello mesh";
        let sig = id.sign(msg);
        assert!(DeviceIdentity::verify(&public.ed25519_pub, msg, &sig));

        // A tampered message does not verify.
        assert!(!DeviceIdentity::verify(&public.ed25519_pub, b"hello mesh!", &sig));

        // Two generated identities differ.
        let other = DeviceIdentity::generate();
        assert_ne!(id.user_id(), other.user_id());
    }
```

- [ ] **Step 2: Run it to confirm it fails to compile**

Run: `nice -n 10 cargo test --lib -- --test-threads=2 identity::device::tests::generated_identity`
Expected: FAIL — `cannot find ... DeviceIdentity`.

- [ ] **Step 3: Implement `DeviceIdentity`**

Add these imports at the top of `src-tauri/src/identity/device.rs` (below the
existing `use` lines):

```rust
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};
```

And add this `DeviceIdentity` block above the `#[cfg(test)]` module:

```rust
/// A device's secret identity. Hold in memory only; persist via `keystore`.
pub struct DeviceIdentity {
    signing_key: SigningKey,
    dh_secret: StaticSecret,
}

impl DeviceIdentity {
    /// Generate a fresh identity from the OS CSPRNG.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let dh_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        Self {
            signing_key,
            dh_secret,
        }
    }

    /// Reconstruct from raw secret-key bytes (used by the keystore on load).
    pub fn from_secret_bytes(ed25519_secret: [u8; 32], x25519_secret: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&ed25519_secret),
            dh_secret: StaticSecret::from(x25519_secret),
        }
    }

    /// The two 32-byte secret scalars, for sealing into the keystore.
    pub fn secret_bytes(&self) -> ([u8; 32], [u8; 32]) {
        (self.signing_key.to_bytes(), self.dh_secret.to_bytes())
    }

    /// The shareable public identity.
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            ed25519_pub: self.signing_key.verifying_key().to_bytes(),
            x25519_pub: X25519Public::from(&self.dh_secret).to_bytes(),
        }
    }

    pub fn user_id(&self) -> String {
        self.public().user_id()
    }

    /// Sign a message with the Ed25519 signing key.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }

    /// Verify a signature against a raw Ed25519 public key.
    pub fn verify(ed25519_pub: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(ed25519_pub) else {
            return false;
        };
        vk.verify(message, &ed25519_dalek::Signature::from_bytes(signature))
            .is_ok()
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `nice -n 10 cargo test --lib -- --test-threads=2 identity::device::tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/identity/device.rs
git commit -m "feat(identity): DeviceIdentity generate/sign/verify"
```

---

### Task 4: Encrypted keystore — round-trip the secret keys

**Files:**
- Create: `src-tauri/src/identity/keystore.rs`
- Modify: `src-tauri/src/identity/mod.rs`

- [ ] **Step 1: Register the module**

Add to `src-tauri/src/identity/mod.rs`:

```rust
pub mod keystore;
```

- [ ] **Step 2: Write the failing test**

Create `src-tauri/src/identity/keystore.rs`:

```rust
//! Password-encrypted, on-disk keystore for a `DeviceIdentity`.
//!
//! Format: salt(16) || nonce(12) || AES-256-GCM ciphertext of the serialized
//! secret keys. Key derived from the password via PBKDF2 (see
//! `storage::encryption`).

use crate::identity::device::DeviceIdentity;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use crate::storage::errors::StorageError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct StoredKeys {
    ed25519_secret: [u8; 32],
    x25519_secret: [u8; 32],
}

/// Encrypt `identity` with `password` and write it to `path` (creating parent
/// directories as needed).
pub fn save(path: &Path, password: &str, identity: &DeviceIdentity) -> Result<(), StorageError> {
    let (ed, dh) = identity.secret_bytes();
    let stored = StoredKeys {
        ed25519_secret: ed,
        x25519_secret: dh,
    };
    let plaintext =
        bincode::serialize(&stored).map_err(|e| StorageError::Serialization(e.to_string()))?;

    let salt = generate_salt();
    let key = EncryptionKey::from_password(password, &salt)?;
    let (ciphertext, nonce) = encrypt_data(&plaintext, &key)?;

    let mut out = Vec::with_capacity(SALT_SIZE + NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| StorageError::DirectoryCreationFailed(parent.to_path_buf()))?;
    }
    std::fs::write(path, out)?;
    Ok(())
}

/// Read and decrypt the keystore at `path` with `password`.
pub fn load(path: &Path, password: &str) -> Result<DeviceIdentity, StorageError> {
    let content = std::fs::read(path)?;
    if content.len() < SALT_SIZE + NONCE_SIZE {
        return Err(StorageError::Decryption(
            "keystore file too short".to_string(),
        ));
    }
    let salt: [u8; SALT_SIZE] = content[0..SALT_SIZE].try_into().unwrap();
    let nonce: [u8; NONCE_SIZE] = content[SALT_SIZE..SALT_SIZE + NONCE_SIZE]
        .try_into()
        .unwrap();
    let ciphertext = &content[SALT_SIZE + NONCE_SIZE..];

    let key = EncryptionKey::from_password(password, &salt)?;
    let plaintext = decrypt_data(ciphertext, &nonce, &key)?;
    let stored: StoredKeys =
        bincode::deserialize(&plaintext).map_err(|e| StorageError::Deserialization(e.to_string()))?;

    Ok(DeviceIdentity::from_secret_bytes(
        stored.ed25519_secret,
        stored.x25519_secret,
    ))
}

/// Load the keystore if present, otherwise generate a new identity and save it.
pub fn load_or_create(path: &Path, password: &str) -> Result<DeviceIdentity, StorageError> {
    if path.exists() {
        load(path, password)
    } else {
        let identity = DeviceIdentity::generate();
        save(path, password, &identity)?;
        Ok(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips_the_identity() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");

        let original = DeviceIdentity::generate();
        save(&path, "correct horse battery staple", &original).expect("save");

        let loaded = load(&path, "correct horse battery staple").expect("load");
        assert_eq!(original.user_id(), loaded.user_id());
        assert_eq!(original.public(), loaded.public());
    }

    #[test]
    fn wrong_password_fails_to_decrypt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");
        let id = DeviceIdentity::generate();
        save(&path, "right", &id).expect("save");

        assert!(load(&path, "wrong").is_err());
    }

    #[test]
    fn load_or_create_is_stable_across_calls() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("keystore.enc");

        let first = load_or_create(&path, "pw").expect("first");
        let second = load_or_create(&path, "pw").expect("second");
        assert_eq!(first.user_id(), second.user_id());
    }
}
```

- [ ] **Step 3: Run it to confirm it fails**

Run: `nice -n 10 cargo test --lib -- --test-threads=2 identity::keystore::tests`
Expected: FAIL — likely a compile error if `storage::encryption` does not yet
expose `NONCE_SIZE`/`SALT_SIZE` publicly. (They were made `pub` in an earlier
commit; if not, make `SALT_SIZE` and `NONCE_SIZE` `pub const` in
`src-tauri/src/storage/encryption.rs`.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `nice -n 10 cargo test --lib -- --test-threads=2 identity::keystore::tests`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/identity/keystore.rs src-tauri/src/identity/mod.rs
git commit -m "feat(identity): password-encrypted keystore with load_or_create"
```

---

### Task 5: Lint clean + final verification

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `nice -n 10 cargo fmt --all`

- [ ] **Step 2: Clippy (the CI gate)**

Run: `nice -n 10 cargo clippy --all-targets -- -D warnings`
Expected: `Finished`, no warnings. Fix any that appear (e.g. derive `Default`,
needless borrows) before continuing.

- [ ] **Step 3: Run the new module's tests together**

Run: `nice -n 10 cargo test --lib -- --test-threads=2 identity::device identity::keystore`
Expected: PASS (5 tests total).

- [ ] **Step 4: Commit any formatting/clippy fixes**

```bash
git add -A
git commit -m "chore(identity): fmt + clippy clean for identity foundation"
```

---

## Self-review

- **Spec coverage (§4 Identity):** keypair generation ✓ (Task 3), `user_id` =
  signing-key fingerprint ✓ (Task 2), password-encrypted keystore ✓ (Task 4),
  signing/verify ✓ (Task 3). Verification UX (safety-number/QR) is a *UI* concern
  deferred to a later plan — out of scope here.
- **Placeholders:** none — every code step has complete code.
- **Type consistency:** `DeviceIdentity::{generate, from_secret_bytes,
  secret_bytes, public, user_id, sign, verify}` and `PublicIdentity::{user_id,
  user_id_from}` are used consistently across Tasks 2–4; `StoredKeys` field names
  (`ed25519_secret`, `x25519_secret`) match between `save` and `load`.
- **Dependency note:** `x25519-dalek` `StaticSecret::random_from_rng` and
  `StaticSecret::to_bytes()` require the `static_secrets` feature (added in
  Task 1). If the exact API name differs in the resolved version, the failing-test
  step surfaces it and the fix is a one-line API adjustment.
