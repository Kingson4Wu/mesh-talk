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
