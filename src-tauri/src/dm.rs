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

use crate::identity::device::DeviceIdentity;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use bincode::Options;
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

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
///
/// - `dh1` = `DH(sender_static, recipient_static)` — authenticates the sender.
/// - `dh2` = `DH(ephemeral, recipient_static)` — fresh per message.
///
/// `info` binds the key to the sender, recipient, and ephemeral public keys (in
/// that fixed order, so direction matters) so a ciphertext cannot be
/// reinterpreted under a different triple. Callers MUST pass `dh1`/`dh2` in this
/// order on both the seal and open sides or the keys will not match.
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

    let mut info = Vec::with_capacity(DM_DOMAIN.len() + 32 * 3); // sender|recipient|ephemeral
    info.extend_from_slice(DM_DOMAIN);
    info.extend_from_slice(sender_x25519_pub);
    info.extend_from_slice(recipient_x25519_pub);
    info.extend_from_slice(ephemeral_pub);

    // No salt: both DH outputs are already uniformly random, so HKDF-Extract
    // with a zero-length salt is sound (RFC 5869 §2.2).
    let hk = Hkdf::<Sha256>::new(None, &ikm);
    let mut okm = [0u8; KEY_LEN + NONCE_LEN];
    hk.expand(&info, &mut okm)
        .expect("hkdf okm length is valid");

    // Phase-1 hardening: the derived key/nonce and `ikm`/`okm` are not zeroized
    // on drop yet (consistent with the keystore's current handling).
    let mut key = [0u8; KEY_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    key.copy_from_slice(&okm[..KEY_LEN]);
    nonce.copy_from_slice(&okm[KEY_LEN..]);
    (key, nonce)
}

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

    let envelope = SealedEnvelope {
        ephemeral_pub: ephemeral_pub.to_bytes(),
        ciphertext,
    };
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
    // Strict parse: reject any trailing bytes (fail closed). This config uses
    // fixed-int little-endian encoding, byte-identical to `bincode::serialize`
    // used in `seal`, so legitimate envelopes still round-trip.
    let envelope: SealedEnvelope = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize(envelope_bytes)
        .map_err(|e| DmError::Serialization(e.to_string()))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

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
        // dh2 (the ephemeral-static DH) must independently affect the key.
        let (k6, _) = derive_key_nonce(&dh1, &[9u8; 32], &a, &b, &e);
        assert_ne!(k1, k6);
        // Key and nonce are distinct derivations, not the same bytes copied twice.
        assert_ne!(&k1[..NONCE_LEN], &n1[..]);
    }

    #[test]
    fn envelope_round_trips_through_bincode() {
        let env = SealedEnvelope {
            ephemeral_pub: [8u8; 32],
            ciphertext: vec![1, 2, 3, 4],
        };
        let bytes = bincode::serialize(&env).unwrap();
        let back: SealedEnvelope = bincode::deserialize(&bytes).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn seal_then_open_round_trips() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        let sealed = seal(&alice, &bob.public().x25519_pub, b"hello bob").unwrap();
        let opened = open(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened, b"hello bob");
    }

    #[test]
    fn open_rejects_trailing_bytes() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut sealed = seal(&alice, &bob.public().x25519_pub, b"hi").unwrap();
        // It opens cleanly as-is.
        assert!(open(&bob, &alice.public().x25519_pub, &sealed).is_ok());
        // Appending junk must make it fail (strict parse), not silently succeed.
        sealed.push(0xAB);
        assert!(open(&bob, &alice.public().x25519_pub, &sealed).is_err());
    }

    #[test]
    fn round_trips_empty_and_large_plaintexts() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Empty payload.
        let sealed = seal(&alice, &bob.public().x25519_pub, b"").unwrap();
        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &sealed).unwrap(),
            b""
        );

        // Large payload.
        let big = vec![0x5au8; 100_000];
        let sealed = seal(&alice, &bob.public().x25519_pub, &big).unwrap();
        assert_eq!(
            open(&bob, &alice.public().x25519_pub, &sealed).unwrap(),
            big
        );
    }
}
