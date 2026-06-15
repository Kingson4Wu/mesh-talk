//! Signing and verification of contact requests/responses.
//!
//! A contact request/response carries the sender's ed25519 public key and a
//! signature over a canonical payload of its meaningful fields. The receiver
//! recomputes the payload and verifies it against the included public key, which
//! proves the message was produced by the holder of that private key and was not
//! tampered with in transit.

use crate::identity::keys::KeyManager;
use ed25519_dalek::SigningKey;

// Unit separator — unambiguous field delimiter that cannot appear in the values.
const SEP: char = '\u{1f}';

/// Canonical bytes signed for a contact request.
pub fn contact_request_payload(
    user_id: &str,
    address: &str,
    alias: &str,
    timestamp: u64,
) -> Vec<u8> {
    format!("contact-request{SEP}{user_id}{SEP}{address}{SEP}{alias}{SEP}{timestamp}").into_bytes()
}

/// Canonical bytes signed for a contact response.
pub fn contact_response_payload(
    user_id: &str,
    address: &str,
    alias: &str,
    approved: bool,
    timestamp: u64,
) -> Vec<u8> {
    format!(
        "contact-response{SEP}{user_id}{SEP}{address}{SEP}{alias}{SEP}{approved}{SEP}{timestamp}"
    )
    .into_bytes()
}

/// Sign `payload`, returning the signer's serialized public key and the signature.
pub fn sign(signing_key: &SigningKey, payload: &[u8]) -> (String, Vec<u8>) {
    let public_key = KeyManager::serialize_public_key(&signing_key.verifying_key());
    let signature = KeyManager::sign_message(signing_key, payload).unwrap_or_default();
    (public_key, signature)
}

/// Result of checking a contact message's signature.
#[derive(Debug, PartialEq, Eq)]
pub enum SignatureCheck {
    /// Signature is present and valid for the included public key.
    Valid,
    /// No public key / signature was attached (older or unsigned sender).
    Missing,
    /// A signature was attached but does not verify — treat as forged/tampered.
    Invalid,
}

/// Verify `signature` over `payload` against `public_key` (base64).
pub fn verify(public_key: &Option<String>, signature: &[u8], payload: &[u8]) -> SignatureCheck {
    let public_key = match public_key {
        Some(pk) if !pk.is_empty() => pk,
        _ => return SignatureCheck::Missing,
    };
    if signature.is_empty() {
        return SignatureCheck::Missing;
    }
    let verifying_key = match KeyManager::deserialize_public_key(public_key) {
        Ok(key) => key,
        Err(_) => return SignatureCheck::Invalid,
    };
    match KeyManager::verify_signature(&verifying_key, payload, signature) {
        Ok(true) => SignatureCheck::Valid,
        _ => SignatureCheck::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keys::KeyManager;

    #[test]
    fn round_trip_request_signature_verifies() {
        let (_pub, signing_key) = KeyManager::generate_keypair().expect("keypair");
        let payload = contact_request_payload("uid-1", "10.0.0.1:7000", "alice", 42);
        let (public_key, signature) = sign(&signing_key, &payload);
        assert_eq!(
            verify(&Some(public_key), &signature, &payload),
            SignatureCheck::Valid
        );
    }

    #[test]
    fn tampered_payload_is_invalid() {
        let (_pub, signing_key) = KeyManager::generate_keypair().expect("keypair");
        let payload = contact_response_payload("uid-1", "10.0.0.1:7000", "alice", true, 42);
        let (public_key, signature) = sign(&signing_key, &payload);

        // Same signature, but a payload an attacker altered (approved flipped).
        let tampered = contact_response_payload("uid-1", "10.0.0.1:7000", "alice", false, 42);
        assert_eq!(
            verify(&Some(public_key), &signature, &tampered),
            SignatureCheck::Invalid
        );
    }

    #[test]
    fn wrong_key_is_invalid() {
        let (_pub, signing_key) = KeyManager::generate_keypair().expect("keypair");
        let (_pub2, other_key) = KeyManager::generate_keypair().expect("keypair2");
        let payload = contact_request_payload("uid-1", "10.0.0.1:7000", "alice", 42);
        let (_pk, signature) = sign(&signing_key, &payload);
        let (other_pk, _sig) = sign(&other_key, &payload);

        assert_eq!(
            verify(&Some(other_pk), &signature, &payload),
            SignatureCheck::Invalid
        );
    }

    #[test]
    fn absent_signature_is_missing() {
        let payload = contact_request_payload("uid-1", "10.0.0.1:7000", "alice", 42);
        assert_eq!(verify(&None, &[], &payload), SignatureCheck::Missing);
        assert_eq!(
            verify(&Some(String::new()), &[], &payload),
            SignatureCheck::Missing
        );
    }
}
