//! Device-linking pairing crypto + wire (multi-device). An existing device (the
//! "linker") shows a one-time [`PairingCode`]; a new device (the "joiner") proves it
//! over the LAN Noise channel and receives the account secret. The authenticator is a
//! SHA-256 secret-prefix MAC over the code and BOTH device keys, so a captured proof
//! cannot be replayed to enrol a different device. Confidentiality of the transferred
//! secret is provided by the Noise channel (encrypted to the joiner's pinned key);
//! the code provides authorization (user consent carried out-of-band).

use crate::identity::account::DeviceCertificate;
use crate::identity::device::PublicIdentity;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PAIR_DOMAIN: &[u8] = b"mesh-talk-pairing-v1";
const REQ_MAGIC: &[u8] = b"MTPQ1";
const RESP_MAGIC: &[u8] = b"MTPS1";

/// A one-time, high-entropy linking code shown by the linker and entered on the joiner.
#[derive(Clone)]
pub struct PairingCode([u8; 16]);

impl PairingCode {
    /// A fresh random 128-bit code.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        PairingCode(bytes)
    }

    /// 32 lowercase hex chars (what the user reads / types).
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a code from its hex form (case-insensitive). `None` if malformed.
    pub fn from_hex(s: &str) -> Option<Self> {
        let bytes = hex::decode(s.trim()).ok()?;
        let arr: [u8; 16] = bytes.try_into().ok()?;
        Some(PairingCode(arr))
    }

    /// Authenticator = SHA-256(DOMAIN ‖ code ‖ linker_ed ‖ joiner_ed). Binding both
    /// device keys stops a captured tag being replayed to enrol another device.
    pub fn authenticator(&self, linker_ed: &[u8; 32], joiner_ed: &[u8; 32]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(PAIR_DOMAIN);
        h.update(self.0);
        h.update(linker_ed);
        h.update(joiner_ed);
        h.finalize().into()
    }

    /// Constant-time check that `tag` is the expected authenticator.
    pub fn verify(&self, linker_ed: &[u8; 32], joiner_ed: &[u8; 32], tag: &[u8; 32]) -> bool {
        ct_eq(&self.authenticator(linker_ed, joiner_ed), tag)
    }
}

/// Constant-time 32-byte equality (no early return on first mismatch).
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Joiner → linker: "I hold the code; here is my device identity." Magic-framed so the
/// responder can distinguish it from a sync wire on the first frame of a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRequest {
    pub joiner: PublicIdentity,
    pub tag: [u8; 32],
}

impl PairingRequest {
    pub fn encode(&self) -> Vec<u8> {
        frame(REQ_MAGIC, self)
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        unframe(REQ_MAGIC, bytes)
    }
}

/// Linker → joiner: the account secret + the account public key + a certificate
/// binding the joiner's device key to the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingResponse {
    pub account_secret: [u8; 32],
    pub account_ed25519_pub: [u8; 32],
    pub cert: DeviceCertificate,
}

impl PairingResponse {
    pub fn encode(&self) -> Vec<u8> {
        frame(RESP_MAGIC, self)
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        unframe(RESP_MAGIC, bytes)
    }
}

fn frame<T: Serialize>(magic: &[u8], v: &T) -> Vec<u8> {
    let mut out = Vec::with_capacity(magic.len() + 64);
    out.extend_from_slice(magic);
    out.extend_from_slice(
        &bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(v)
            .expect("pairing message serializes"),
    );
    out
}

fn unframe<T: for<'de> Deserialize<'de>>(magic: &[u8], bytes: &[u8]) -> Option<T> {
    let rest = bytes.strip_prefix(magic)?;
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize::<T>(rest)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn code_hex_round_trips() {
        let c = PairingCode::generate();
        assert_eq!(PairingCode::from_hex(&c.as_hex()).unwrap().0, c.0);
        assert!(PairingCode::from_hex("nothex").is_none());
        assert!(PairingCode::from_hex("ab").is_none()); // wrong length
    }

    #[test]
    fn authenticator_verifies_and_binds_both_keys() {
        let code = PairingCode::generate();
        let linker = DeviceIdentity::generate();
        let joiner = DeviceIdentity::generate();
        let l = linker.public().ed25519_pub;
        let j = joiner.public().ed25519_pub;
        let tag = code.authenticator(&l, &j);
        assert!(code.verify(&l, &j, &tag));
        // Wrong code, wrong joiner, wrong linker all fail.
        assert!(!PairingCode::generate().verify(&l, &j, &tag));
        let other = DeviceIdentity::generate().public().ed25519_pub;
        assert!(!code.verify(&l, &other, &tag));
        assert!(!code.verify(&other, &j, &tag));
    }

    #[test]
    fn request_and_response_round_trip() {
        let joiner = DeviceIdentity::generate();
        let code = PairingCode::generate();
        let req = PairingRequest {
            joiner: joiner.public(),
            tag: code.authenticator(&[1u8; 32], &joiner.public().ed25519_pub),
        };
        assert_eq!(PairingRequest::decode(&req.encode()), Some(req));
        assert!(PairingRequest::decode(b"not a pairing frame").is_none());

        let account = Account::generate();
        let cert = account.certify(&joiner.public().ed25519_pub);
        let resp = PairingResponse {
            account_secret: account.secret_bytes(),
            account_ed25519_pub: account.public().ed25519_pub,
            cert,
        };
        assert_eq!(PairingResponse::decode(&resp.encode()), Some(resp));
    }

    #[test]
    fn a_sync_like_frame_is_not_a_pairing_request() {
        // Bytes without the magic must not decode as a pairing request.
        assert!(PairingRequest::decode(&[0u8, 1, 2, 3, 4, 5]).is_none());
    }
}
