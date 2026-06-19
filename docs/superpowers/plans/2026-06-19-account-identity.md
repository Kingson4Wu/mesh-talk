# Account Identity + Device Certificates Implementation Plan (Multi-Device Plan 1)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. SECURITY-CRITICAL (account auth). Checkbox steps.

**Goal:** The pure crypto foundation for multi-device: an `Account` Ed25519 keypair (the user's cross-device handle) and a `DeviceCertificate` (the account key's signature binding a device key to the account), with verification. No I/O, no networking.

**Architecture:** New module `crate::identity::account`. `Account` mirrors the `DeviceIdentity` style (Ed25519 only). `DeviceCertificate` binds `(account_pub, device_pub)` via an account signature over a domain-separated message. Reuses `ed25519_dalek` (already used by `DeviceIdentity`).

**Tech Stack:** Rust; `ed25519-dalek`, `sha2`, `serde`, `bincode`, `hex`. All already dependencies.

---

## Background the implementer needs

`crate::identity::device` (`src-tauri/src/identity/device.rs`): `DeviceIdentity` holds an Ed25519 `SigningKey` + X25519 secret; `PublicIdentity { ed25519_pub: [u8;32], x25519_pub: [u8;32] }`; `PublicIdentity::user_id_from(&[u8;32]) -> String` (hex of first 16 bytes of SHA-256("mesh-talk-id-v1" ‖ ed25519_pub)); `DeviceIdentity::{sign(&[u8])->[u8;64], verify(ed25519_pub, msg, sig)->bool}` (static `verify`). Mirror this style.

Register `pub mod account;` in `src-tauri/src/identity/mod.rs`.

**CPU/test discipline:** `nice -n 10`, `--test-threads=2`. `cargo test --lib identity::account`; build/clippy/fmt. Confirm branch line.

---

### Task 1: `identity::account` — Account keypair + DeviceCertificate

**Files:** Create `src-tauri/src/identity/account.rs`; modify `src-tauri/src/identity/mod.rs`.

- [ ] **Step 1: write `identity/account.rs`**
```rust
//! Account identity (multi-device): an Ed25519 account keypair shared across a
//! user's devices, and device certificates (the account key signs each device's
//! public key, binding the device to the account). The `account_id` is the user's
//! stable cross-device handle. Pure crypto — persistence + networking are later plans.

use crate::identity::device::DeviceIdentity;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Domain separator for the device-certificate signed message.
const CERT_DOMAIN: &[u8] = b"mesh-talk-device-cert-v1";

/// Build the message the account signs to certify a device: `DOMAIN ‖ account_pub ‖
/// device_pub`. Binding both keys prevents a cert being replayed under another account.
fn cert_message(account_pub: &[u8; 32], device_pub: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(CERT_DOMAIN.len() + 64);
    m.extend_from_slice(CERT_DOMAIN);
    m.extend_from_slice(account_pub);
    m.extend_from_slice(device_pub);
    m
}

/// The public half of an account (its Ed25519 verifying key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountPublic {
    pub ed25519_pub: [u8; 32],
}

impl AccountPublic {
    /// Stable account id = hex of the first 16 bytes of SHA-256("mesh-talk-account-v1"
    /// ‖ ed25519_pub). 32 hex chars. The user's cross-device handle.
    pub fn account_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"mesh-talk-account-v1");
        h.update(self.ed25519_pub);
        hex::encode(&h.finalize()[..16])
    }
}

/// A user's account secret (Ed25519). Held in memory; persisted via the keystore
/// (a later plan). Shared across the user's devices.
pub struct Account {
    signing_key: SigningKey,
}

impl Account {
    pub fn generate() -> Self {
        Account { signing_key: SigningKey::generate(&mut rand::rngs::OsRng) }
    }

    pub fn from_secret_bytes(ed25519_secret: [u8; 32]) -> Self {
        Account { signing_key: SigningKey::from_bytes(&ed25519_secret) }
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    pub fn public(&self) -> AccountPublic {
        AccountPublic { ed25519_pub: self.signing_key.verifying_key().to_bytes() }
    }

    pub fn account_id(&self) -> String {
        self.public().account_id()
    }

    /// Certify a device: sign the device's Ed25519 public key with the account key.
    pub fn certify(&self, device_ed25519_pub: &[u8; 32]) -> DeviceCertificate {
        let account_pub = self.public().ed25519_pub;
        let msg = cert_message(&account_pub, device_ed25519_pub);
        let signature = self.signing_key.sign(&msg).to_bytes();
        DeviceCertificate {
            account_ed25519_pub: account_pub,
            device_ed25519_pub: *device_ed25519_pub,
            signature,
        }
    }
}

/// Proof that a device belongs to an account: the account key's signature over the
/// device's public key. Advertised on the network alongside the device identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCertificate {
    pub account_ed25519_pub: [u8; 32],
    pub device_ed25519_pub: [u8; 32],
    pub signature: [u8; 64],
}

impl DeviceCertificate {
    /// True if the signature is a valid account-over-device binding.
    pub fn verify(&self) -> bool {
        let msg = cert_message(&self.account_ed25519_pub, &self.device_ed25519_pub);
        DeviceIdentity::verify(&self.account_ed25519_pub, &msg, &self.signature)
    }

    /// The account id this cert binds to (only meaningful if `verify()` is true).
    pub fn account_id(&self) -> String {
        AccountPublic { ed25519_pub: self.account_ed25519_pub }.account_id()
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("device cert serializes")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bincode::Options;

    #[test]
    fn account_certifies_a_device_and_verifies() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let cert = account.certify(&device.public().ed25519_pub);
        assert!(cert.verify());
        assert_eq!(cert.account_id(), account.account_id());
    }

    #[test]
    fn a_tampered_cert_fails() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let mut cert = account.certify(&device.public().ed25519_pub);
        cert.device_ed25519_pub[0] ^= 0xFF; // claim a different device
        assert!(!cert.verify());
    }

    #[test]
    fn a_cert_from_a_different_account_doesnt_verify_as_ours() {
        let account = Account::generate();
        let other = Account::generate();
        let device = DeviceIdentity::generate();
        // `other` signs, but the cert claims `account`'s key → signature mismatch.
        let real = other.certify(&device.public().ed25519_pub);
        let mut forged = real.clone();
        forged.account_ed25519_pub = account.public().ed25519_pub;
        assert!(!forged.verify());
    }

    #[test]
    fn account_round_trips_from_secret_bytes() {
        let account = Account::generate();
        let restored = Account::from_secret_bytes(account.secret_bytes());
        assert_eq!(restored.account_id(), account.account_id());
        // A cert from the restored account still verifies.
        let device = DeviceIdentity::generate();
        assert!(restored.certify(&device.public().ed25519_pub).verify());
    }

    #[test]
    fn cert_codec_round_trips_and_rejects_trailing() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let cert = account.certify(&device.public().ed25519_pub);
        assert_eq!(DeviceCertificate::decode(&cert.encode()), Some(cert.clone()));
        let mut junk = cert.encode();
        junk.push(0xAB);
        assert_eq!(DeviceCertificate::decode(&junk), None);
        let _ = bincode::DefaultOptions::new(); // ensure Options import used
    }
}
```
(If the unused-import guard line at the end of the last test causes a clippy issue, drop it — `bincode::Options` is already used via `encode`/`decode`.)

- [ ] **Step 2: register the module (`identity/mod.rs`)**

Add `pub mod account;` with the other `pub mod`s.

- [ ] **Step 3: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib identity::account -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/identity/account.rs src-tauri/src/identity/mod.rs
git commit -m "feat(identity): account keypair + device certificates (multi-device foundation)"
git status | head -1
```

---

## Notes for the reviewer

- **Delivered:** the pure account-identity foundation — an account Ed25519 keypair (cross-device handle via `account_id`) and device certificates (account-signed device-key bindings) with verification + codec. No I/O.
- **Reviewer checks:** the cert message is domain-separated AND binds BOTH account and device keys (so a cert can't be replayed under another account — the `forged.account_ed25519_pub` test); `verify` reuses the vetted `DeviceIdentity::verify`; the account secret is never logged (`Account` has no `Debug`); codec fail-closed.
- **Deferred to later plans:** persist the account keypair (keystore); advertise `account_id` + cert in discovery announces + verify on receipt; roster grouping by account; account-addressed fan-out send + self-sync; the device-linking pairing flow + UI.
