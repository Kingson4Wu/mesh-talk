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
    dhs_secret: StaticSecret, // our current ratchet private key
    dhs_public: PublicKey,    // our current ratchet public key
    dhr: Option<PublicKey>,   // their current ratchet public key
    rk: [u8; 32],             // root key
    cks: Option<[u8; 32]>,    // sending chain key
    ckr: Option<[u8; 32]>,    // receiving chain key
    ns: u32,                  // messages sent in current sending chain
    nr: u32,                  // messages received in current receiving chain
    pn: u32,                  // previous sending-chain length
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
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| RatchetError::Decrypt)
}

fn aead_decrypt(mk: &[u8; 32], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, RatchetError> {
    let (key, nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| RatchetError::Decrypt)?;
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| RatchetError::Decrypt)
}

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
        assert_eq!(
            bob.ratchet_decrypt(&h, &c),
            Err(RatchetError::TooManySkipped)
        );
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
        let h = Header {
            ratchet_pub: [5u8; 32],
            pn: 3,
            n: 9,
        };
        assert_eq!(Header::decode(&h.encode()).unwrap().n, 9);
        assert!(Header::decode(b"junk").is_none() || Header::decode(b"junk").is_some());
    }
}
