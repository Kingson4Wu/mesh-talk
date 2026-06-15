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
// Task 2 (seal/open) will use these — suppress dead_code until then.
#[allow(dead_code)]
const DM_DOMAIN: &[u8] = b"mesh-talk-dm-v1";
#[allow(dead_code)]
const KEY_LEN: usize = 32;
#[allow(dead_code)]
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
// Task 2 (seal/open) will call this — suppress dead_code until then.
#[allow(dead_code)]
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
    hk.expand(&info, &mut okm)
        .expect("hkdf okm length is valid");

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
        let env = SealedEnvelope {
            ephemeral_pub: [8u8; 32],
            ciphertext: vec![1, 2, 3, 4],
        };
        let bytes = bincode::serialize(&env).unwrap();
        let back: SealedEnvelope = bincode::deserialize(&bytes).unwrap();
        assert_eq!(env, back);
    }
}
