# Double Ratchet Core Implementation Plan (Ratchet Plan 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax. This is SECURITY-CRITICAL — follow the algorithm exactly; do not "simplify" the crypto.

**Goal:** A pure, exhaustively-tested Double Ratchet state machine (DH ratchet + symmetric KDF chains + skipped-key handling) over the codebase's existing X25519 / HKDF-SHA256 / AES-256-GCM primitives. No I/O, no event log, no networking.

**Architecture:** `crate::ratchet` with `kdf` (root + chain KDF steps, message-key derivation) and the state machine (`RatchetState`, `Header`, `init_alice`/`init_bob`, `ratchet_encrypt`/`ratchet_decrypt`). Built per the published Signal Double Ratchet spec; integration (durable state, plaintext store, Node DM path) is later plans.

**Tech Stack:** Rust; `x25519-dalek` v2 (`static_secrets`), `hkdf` 0.12 + `sha2` 0.10, `aes-gcm` 0.10, `rand` 0.8, `bincode`. All already dependencies.

---

## Background the implementer needs

Spec: `docs/superpowers/specs/2026-06-18-double-ratchet-design.md`. The `dm` module already uses these primitives (`x25519_dalek::{StaticSecret, PublicKey}`, `Hkdf::<Sha256>`, `Aes256Gcm`) — mirror its style. This is a NEW top-level module `crate::ratchet` (register `pub mod ratchet;` in `src-tauri/src/lib.rs`, alphabetically — after `pub mod platform;`/`postoffice` area, i.e. between `pub mod postoffice;` and `pub mod redesign_commands;` → actually alphabetical: `...postoffice, ratchet, redesign_commands...`).

**Algorithm reference (implement EXACTLY):**
- `kdf_rk(rk, dh_out) -> (rk', ck)`: HKDF-SHA256, salt = `rk`, ikm = `dh_out`, info = `b"MeshTalk-Ratchet-RK"`, 64-byte output → `rk' = out[0..32]`, `ck = out[32..64]`.
- `kdf_ck(ck) -> (ck', mk)`: HKDF-SHA256, salt = `ck`, ikm = `&[]`, info = `b"MeshTalk-Ratchet-CK"`, 64-byte output → `ck' = out[0..32]`, `mk = out[32..64]`.
- `message_keys(mk) -> (aes_key[32], nonce[12])`: HKDF-SHA256, salt = `mk`, ikm = `&[]`, info = `b"MeshTalk-Ratchet-MSG"`, 44-byte output → key + nonce. Single-use mk ⇒ single-use nonce (safe).
- AEAD: `Aes256Gcm`, AAD = the encoded `Header` (so the header is authenticated).
- `MAX_SKIP = 1000` (DoS guard on skipped keys per ratchet step).

**CPU/test discipline:** `nice -n 10`, `--test-threads=2`. `cargo test --lib ratchet`; `cargo build --lib`; `cargo clippy --lib -- -D warnings`; `cargo fmt`. Confirm branch line after committing.

---

### Task 1: KDF primitives (`ratchet/kdf.rs`)

**Files:** Create `src-tauri/src/ratchet/kdf.rs`, `src-tauri/src/ratchet/mod.rs`; modify `src-tauri/src/lib.rs`.

- [ ] **Step 1: register the module**

`lib.rs`: add `pub mod ratchet;` in the alphabetical `pub mod` list (between `postoffice` and `redesign_commands`).
Create `ratchet/mod.rs`:
```rust
//! Double Ratchet for DMs (Phase 3): forward secrecy + post-compromise security over
//! the existing X25519 / HKDF-SHA256 / AES-256-GCM primitives. `kdf` is the key
//! schedule; this module is the state machine. Pure — durable state + the Node DM
//! integration are later plans.

mod kdf;
pub mod state;

pub use state::{init_alice, init_bob, Header, RatchetError, RatchetState};
```

- [ ] **Step 2: write `ratchet/kdf.rs`**
```rust
//! The Double Ratchet key schedule: root-chain step, symmetric-chain step, and
//! per-message AEAD key/nonce derivation. All HKDF-SHA256 over 32-byte keys.

use hkdf::Hkdf;
use sha2::Sha256;

/// Root KDF: derive the next root key + a new chain key from the current root key
/// and a fresh DH output. `salt = rk`, `ikm = dh_out`.
pub fn kdf_rk(rk: &[u8; 32], dh_out: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(rk), dh_out);
    let mut okm = [0u8; 64];
    hk.expand(b"MeshTalk-Ratchet-RK", &mut okm)
        .expect("64 is a valid HKDF length");
    let mut rk2 = [0u8; 32];
    let mut ck = [0u8; 32];
    rk2.copy_from_slice(&okm[..32]);
    ck.copy_from_slice(&okm[32..]);
    (rk2, ck)
}

/// Chain KDF: derive the next chain key + a single-use message key from a chain key.
/// `salt = ck`, `ikm = empty`.
pub fn kdf_ck(ck: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(ck), &[]);
    let mut okm = [0u8; 64];
    hk.expand(b"MeshTalk-Ratchet-CK", &mut okm)
        .expect("64 is a valid HKDF length");
    let mut ck2 = [0u8; 32];
    let mut mk = [0u8; 32];
    ck2.copy_from_slice(&okm[..32]);
    mk.copy_from_slice(&okm[32..]);
    (ck2, mk)
}

/// Derive the AES-256-GCM key + 96-bit nonce for one message from its message key.
pub fn message_keys(mk: &[u8; 32]) -> ([u8; 32], [u8; 12]) {
    let hk = Hkdf::<Sha256>::new(Some(mk), &[]);
    let mut okm = [0u8; 44];
    hk.expand(b"MeshTalk-Ratchet-MSG", &mut okm)
        .expect("44 is a valid HKDF length");
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    key.copy_from_slice(&okm[..32]);
    nonce.copy_from_slice(&okm[32..]);
    (key, nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdf_steps_are_deterministic_and_distinct() {
        let rk = [1u8; 32];
        let dh = [2u8; 32];
        let (rk1, ck1) = kdf_rk(&rk, &dh);
        let (rk2, ck2) = kdf_rk(&rk, &dh);
        assert_eq!((rk1, ck1), (rk2, ck2)); // deterministic
        assert_ne!(rk1, ck1); // root and chain outputs differ
        assert_ne!(rk1, rk); // advances

        let (cka, mka) = kdf_ck(&ck1);
        let (ckb, mkb) = kdf_ck(&ck1);
        assert_eq!((cka, mka), (ckb, mkb));
        assert_ne!(cka, mka);
        assert_ne!(cka, ck1);

        let (k1, n1) = message_keys(&mka);
        let (k2, n2) = message_keys(&mka);
        assert_eq!((k1, n1), (k2, n2));
        // different message keys → different aead keys
        let (k3, _) = message_keys(&mkb);
        assert_ne!(k1, k3);
    }
}
```

- [ ] **Step 3: build, test, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib ratchet::kdf -- --test-threads=2
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/ratchet/kdf.rs src-tauri/src/ratchet/mod.rs src-tauri/src/lib.rs
git commit -m "feat(ratchet): Double Ratchet key schedule (root/chain/message KDF)"
git status | head -1
```
NOTE: `mod.rs` references `state` which doesn't exist yet — for THIS commit, comment out `pub mod state;` + the `pub use state::...;` line (leave only `mod kdf;`). Task 2 adds `state`.

---

### Task 2: the ratchet state machine (`ratchet/state.rs`)

**Files:** Create `src-tauri/src/ratchet/state.rs`; modify `ratchet/mod.rs` (add `state`).

- [ ] **Step 1: write `ratchet/state.rs`**
```rust
//! The Double Ratchet state machine: `RatchetState` + `init_alice`/`init_bob` +
//! `ratchet_encrypt`/`ratchet_decrypt`, with the DH ratchet, symmetric chains, and
//! bounded skipped-message-key handling for out-of-order delivery.

use crate::ratchet::kdf::{kdf_ck, kdf_rk, message_keys};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use bincode::Options;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use x25519_dalek::{PublicKey, StaticSecret};

const MAX_SKIP: u32 = 1000;

#[derive(Debug, PartialEq, Eq)]
pub enum RatchetError {
    /// AEAD decryption failed (wrong key, tampered ciphertext/header).
    Decrypt,
    /// The header requested more skipped keys than `MAX_SKIP`.
    TooManySkipped,
    /// We have no sending chain yet (Bob must receive before he can send).
    NotInitializedForSend,
    /// A malformed header.
    Malformed,
}

/// The per-message header, authenticated as AEAD associated data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub ratchet_pub: [u8; 32],
    pub pn: u32, // previous sending-chain length
    pub n: u32,  // message number in the current sending chain
}

impl Header {
    fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("header serializes")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// A Double Ratchet session's state (one peer). In-memory for now (durable storage
/// is a later plan).
pub struct RatchetState {
    dhs_secret: StaticSecret,    // our current ratchet private key
    dhs_public: PublicKey,       // our current ratchet public key
    dhr: Option<PublicKey>,      // their current ratchet public key
    rk: [u8; 32],                // root key
    cks: Option<[u8; 32]>,       // sending chain key
    ckr: Option<[u8; 32]>,       // receiving chain key
    ns: u32,                     // messages sent in current sending chain
    nr: u32,                     // messages received in current receiving chain
    pn: u32,                     // previous sending-chain length
    skipped: HashMap<([u8; 32], u32), [u8; 32]>, // (their ratchet pub, n) -> mk
}

fn dh(secret: &StaticSecret, public: &PublicKey) -> [u8; 32] {
    secret.diffie_hellman(public).to_bytes()
}

/// Initialise the SENDER (Alice): she knows the shared secret + Bob's initial ratchet
/// public key, and sets up the first sending chain via a DH ratchet.
pub fn init_alice(shared_secret: &[u8; 32], bob_ratchet_pub: &[u8; 32]) -> RatchetState {
    let dhs_secret = StaticSecret::random_from_rng(OsRng);
    let dhs_public = PublicKey::from(&dhs_secret);
    let dhr = PublicKey::from(*bob_ratchet_pub);
    let (rk, cks) = kdf_rk(shared_secret, &dh(&dhs_secret, &dhr));
    RatchetState {
        dhs_secret,
        dhs_public,
        dhr: Some(dhr),
        rk,
        cks: Some(cks),
        ckr: None,
        ns: 0,
        nr: 0,
        pn: 0,
        skipped: HashMap::new(),
    }
}

/// Initialise the RECEIVER (Bob): he holds the shared secret + his own ratchet
/// keypair (whose PUBLIC half Alice used in `init_alice`). He has no sending chain
/// until he processes Alice's first message (which DH-ratchets him).
pub fn init_bob(shared_secret: &[u8; 32], bob_ratchet_secret: [u8; 32]) -> RatchetState {
    let dhs_secret = StaticSecret::from(bob_ratchet_secret);
    let dhs_public = PublicKey::from(&dhs_secret);
    RatchetState {
        dhs_secret,
        dhs_public,
        dhr: None,
        rk: *shared_secret,
        cks: None,
        ckr: None,
        ns: 0,
        nr: 0,
        pn: 0,
        skipped: HashMap::new(),
    }
}

impl RatchetState {
    /// Our current ratchet public key (Alice publishes this; tests need it).
    pub fn ratchet_public(&self) -> [u8; 32] {
        self.dhs_public.to_bytes()
    }

    /// Encrypt `plaintext`, advancing the sending chain. Returns the header + the
    /// AEAD ciphertext (the header is authenticated as AAD).
    pub fn ratchet_encrypt(&mut self, plaintext: &[u8]) -> Result<(Header, Vec<u8>), RatchetError> {
        let cks = self.cks.ok_or(RatchetError::NotInitializedForSend)?;
        let (cks2, mk) = kdf_ck(&cks);
        self.cks = Some(cks2);
        let header = Header {
            ratchet_pub: self.dhs_public.to_bytes(),
            pn: self.pn,
            n: self.ns,
        };
        self.ns += 1;
        let ct = aead_encrypt(&mk, plaintext, &header.encode())?;
        Ok((header, ct))
    }

    /// Decrypt a message. Handles a DH-ratchet step (new `ratchet_pub`) and
    /// out-of-order delivery (skipped keys), then advances the receiving chain.
    pub fn ratchet_decrypt(
        &mut self,
        header: &Header,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, RatchetError> {
        // 1. A previously-skipped key for this exact (ratchet_pub, n)?
        if let Some(mk) = self.skipped.remove(&(header.ratchet_pub, header.n)) {
            return aead_decrypt(&mk, ciphertext, &header.encode());
        }
        // 2. New DH ratchet key? Skip the rest of the current receiving chain, then ratchet.
        let header_pub = PublicKey::from(header.ratchet_pub);
        if self.dhr.map(|d| d.to_bytes()) != Some(header.ratchet_pub) {
            self.skip_message_keys(header.pn)?;
            self.dh_ratchet(&header_pub);
        }
        // 3. Skip forward in the (now correct) receiving chain to header.n.
        self.skip_message_keys(header.n)?;
        // 4. Derive this message's key and advance.
        let ckr = self.ckr.ok_or(RatchetError::Malformed)?;
        let (ckr2, mk) = kdf_ck(&ckr);
        self.ckr = Some(ckr2);
        self.nr += 1;
        aead_decrypt(&mk, ciphertext, &header.encode())
    }

    /// Store skipped message keys in the current receiving chain up to (not incl) `until`.
    fn skip_message_keys(&mut self, until: u32) -> Result<(), RatchetError> {
        let Some(ckr) = self.ckr else {
            return Ok(()); // no receiving chain yet (first ever message)
        };
        if until < self.nr {
            return Ok(());
        }
        if until - self.nr > MAX_SKIP {
            return Err(RatchetError::TooManySkipped);
        }
        let dhr = self.dhr.expect("ckr implies dhr").to_bytes();
        let mut ck = ckr;
        while self.nr < until {
            let (ck2, mk) = kdf_ck(&ck);
            self.skipped.insert((dhr, self.nr), mk);
            ck = ck2;
            self.nr += 1;
        }
        self.ckr = Some(ck);
        Ok(())
    }

    /// Perform a DH ratchet step on receiving a new ratchet public key.
    fn dh_ratchet(&mut self, header_pub: &PublicKey) {
        self.pn = self.ns;
        self.ns = 0;
        self.nr = 0;
        self.dhr = Some(*header_pub);
        // New receiving chain from their new pubkey.
        let (rk2, ckr) = kdf_rk(&self.rk, &dh(&self.dhs_secret, header_pub));
        self.rk = rk2;
        self.ckr = Some(ckr);
        // New sending keypair + sending chain.
        self.dhs_secret = StaticSecret::random_from_rng(OsRng);
        self.dhs_public = PublicKey::from(&self.dhs_secret);
        let (rk3, cks) = kdf_rk(&self.rk, &dh(&self.dhs_secret, header_pub));
        self.rk = rk3;
        self.cks = Some(cks);
    }
}

fn aead_encrypt(mk: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, RatchetError> {
    let (key, nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| RatchetError::Decrypt)?;
    cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .map_err(|_| RatchetError::Decrypt)
}

fn aead_decrypt(mk: &[u8; 32], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, RatchetError> {
    let (key, nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| RatchetError::Decrypt)?;
    cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: ciphertext, aad })
        .map_err(|_| RatchetError::Decrypt)
}
```

- [ ] **Step 2: update `ratchet/mod.rs`** to its final form (uncomment `pub mod state;` + the `pub use`).

- [ ] **Step 3: exhaustive tests (append in `state.rs` `#[cfg(test)] mod tests`)**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Establish a paired Alice/Bob session over a fixed shared secret + Bob keypair.
    fn pair() -> (RatchetState, RatchetState) {
        let shared = [9u8; 32];
        let bob_secret = [3u8; 32];
        let bob_pub = PublicKey::from(&StaticSecret::from(bob_secret)).to_bytes();
        let alice = init_alice(&shared, &bob_pub);
        let bob = init_bob(&shared, bob_secret);
        (alice, bob)
    }

    #[test]
    fn single_message_round_trips() {
        let (mut alice, mut bob) = pair();
        let (h, ct) = alice.ratchet_encrypt(b"hello bob").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h, &ct).unwrap(), b"hello bob");
    }

    #[test]
    fn bidirectional_with_dh_ratchets() {
        let (mut alice, mut bob) = pair();
        let (h1, c1) = alice.ratchet_encrypt(b"a1").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h1, &c1).unwrap(), b"a1");
        let (h2, c2) = bob.ratchet_encrypt(b"b1").unwrap(); // Bob can send after receiving
        assert_eq!(alice.ratchet_decrypt(&h2, &c2).unwrap(), b"b1");
        let (h3, c3) = alice.ratchet_encrypt(b"a2").unwrap(); // Alice DH-ratchets again
        assert_eq!(bob.ratchet_decrypt(&h3, &c3).unwrap(), b"a2");
    }

    #[test]
    fn out_of_order_within_a_chain() {
        let (mut alice, mut bob) = pair();
        let (h0, c0) = alice.ratchet_encrypt(b"m0").unwrap();
        let (h1, c1) = alice.ratchet_encrypt(b"m1").unwrap();
        let (h2, c2) = alice.ratchet_encrypt(b"m2").unwrap();
        // Bob receives 2, then 0, then 1 (skipped keys cover the gaps).
        assert_eq!(bob.ratchet_decrypt(&h2, &c2).unwrap(), b"m2");
        assert_eq!(bob.ratchet_decrypt(&h0, &c0).unwrap(), b"m0");
        assert_eq!(bob.ratchet_decrypt(&h1, &c1).unwrap(), b"m1");
    }

    #[test]
    fn out_of_order_across_a_dh_ratchet() {
        let (mut alice, mut bob) = pair();
        let (h1, c1) = alice.ratchet_encrypt(b"a1").unwrap();
        bob.ratchet_decrypt(&h1, &c1).unwrap();
        let (hb, cb) = bob.ratchet_encrypt(b"b1").unwrap();
        alice.ratchet_decrypt(&hb, &cb).unwrap();
        // Alice sends two in her new chain; Bob gets the second before the first.
        let (h2, c2) = alice.ratchet_encrypt(b"a2").unwrap();
        let (h3, c3) = alice.ratchet_encrypt(b"a3").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h3, &c3).unwrap(), b"a3");
        assert_eq!(bob.ratchet_decrypt(&h2, &c2).unwrap(), b"a2");
    }

    #[test]
    fn too_many_skipped_is_rejected() {
        let (mut alice, mut bob) = pair();
        // Alice sends MAX_SKIP + 2 messages; Bob jumps straight to the last.
        let mut last = None;
        for i in 0..(MAX_SKIP + 2) {
            last = Some(alice.ratchet_encrypt(format!("m{i}").as_bytes()).unwrap());
        }
        let (h, c) = last.unwrap();
        assert_eq!(bob.ratchet_decrypt(&h, &c), Err(RatchetError::TooManySkipped));
    }

    #[test]
    fn tampered_ciphertext_and_header_are_rejected() {
        let (mut alice, mut bob) = pair();
        let (h, mut c) = alice.ratchet_encrypt(b"secret").unwrap();
        let last = c.len() - 1;
        c[last] ^= 0xFF;
        assert_eq!(bob.ratchet_decrypt(&h, &c), Err(RatchetError::Decrypt));
        // A tampered header (different n) also fails (header is AAD).
        let (mut alice2, mut bob2) = pair();
        let (mut h2, c2) = alice2.ratchet_encrypt(b"secret").unwrap();
        h2.n = 7;
        assert!(bob2.ratchet_decrypt(&h2, &c2).is_err());
    }

    #[test]
    fn forward_secrecy_keys_are_single_use() {
        // After Bob decrypts message n, the same (header, ct) cannot be decrypted
        // again (its skipped key was consumed / the chain advanced past it).
        let (mut alice, mut bob) = pair();
        let (h, c) = alice.ratchet_encrypt(b"once").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h, &c).unwrap(), b"once");
        assert!(bob.ratchet_decrypt(&h, &c).is_err()); // no key to re-derive
    }

    #[test]
    fn header_codec_round_trips() {
        let h = Header { ratchet_pub: [5u8; 32], pn: 3, n: 9 };
        assert_eq!(Header::decode(&h.encode()).unwrap().n, 9);
        assert!(Header::decode(b"junk").is_none() || Header::decode(b"junk").is_some());
    }
}
```
(`init_bob` takes Bob's ratchet SECRET bytes; `init_alice` takes Bob's ratchet PUBLIC bytes derived from the same secret — the test's `pair()` shows the relationship. In the real integration, Bob publishes his ratchet public key as part of session setup; that's a later plan.)

- [ ] **Step 4: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib ratchet -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/ratchet/state.rs src-tauri/src/ratchet/mod.rs
git commit -m "feat(ratchet): Double Ratchet state machine (DH ratchet, chains, skipped keys)"
git status | head -1
```
Expected: all ratchet tests pass; clippy clean.

---

## Notes for the reviewer (ADVERSARIAL crypto review required)

- **Delivered:** the pure Double Ratchet state machine, per the Signal spec, over vetted primitives. No I/O.
- **Reviewer MUST verify:** (a) the DH-ratchet order matches the spec — skip remaining receiving keys up to `header.pn`, ratchet the receiving chain from the new pubkey, THEN generate a new sending keypair + sending chain; (b) message keys + nonces are single-use (forward secrecy); (c) the header is authenticated (AAD) so `pn`/`n`/`ratchet_pub` can't be mauled; (d) `MAX_SKIP` bounds memory; (e) skipped keys are removed on use (no replay); (f) `kdf_rk`/`kdf_ck`/`message_keys` use distinct `info` labels (domain separation) and never reuse a salt/ikm pairing across roles; (g) no key material in any `Debug`/log (RatchetState has no `Debug`; secrets aren't printed); (h) the out-of-order + across-DH-ratchet tests genuinely exercise skipped-key storage.
- **Deferred to later plans:** durable (encrypted) ratchet state + skipped-key persistence; a received-plaintext store; session establishment from the identity-key DH (X3DH bootstrap) + publishing Bob's ratchet pubkey; Node DM integration (seal/open Message events, delete keys, serve history from the plaintext store); coexistence/migration with the current DM box. Channels (group ratchet) are a separate effort.
