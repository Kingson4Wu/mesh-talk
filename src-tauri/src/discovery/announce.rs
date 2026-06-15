//! The signed discovery announce and its UDP wire format.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use serde::{Deserialize, Serialize};

/// Domain separator for announce signatures.
const ANNOUNCE_DOMAIN: &[u8] = b"mesh-talk-announce-v1";
/// Wire framing: 4-byte magic + 1-byte version, then `bincode(Announce)`.
const MAGIC: &[u8; 4] = b"MTAN";
const VERSION: u8 = 1;

/// A peer's self-announcement: its identity keys, display name, and TCP listen
/// port, signed by its Ed25519 key. `user_id` is the fingerprint of `ed25519_pub`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announce {
    pub user_id: String,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    pub name: String,
    pub tcp_port: u16,
    pub sig: Vec<u8>,
}

/// Length-prefixed, domain-separated bytes the announce signs over (everything
/// except `sig`). Length prefixes make the concatenation unambiguous.
fn signing_input(
    user_id: &str,
    ed25519_pub: &[u8; 32],
    x25519_pub: &[u8; 32],
    name: &str,
    tcp_port: u16,
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(ANNOUNCE_DOMAIN);
    v.extend_from_slice(&(user_id.len() as u32).to_be_bytes());
    v.extend_from_slice(user_id.as_bytes());
    v.extend_from_slice(ed25519_pub);
    v.extend_from_slice(x25519_pub);
    v.extend_from_slice(&(name.len() as u32).to_be_bytes());
    v.extend_from_slice(name.as_bytes());
    v.extend_from_slice(&tcp_port.to_be_bytes());
    v
}

impl Announce {
    /// Build and sign an announce for `identity`.
    pub fn new(identity: &DeviceIdentity, name: impl Into<String>, tcp_port: u16) -> Self {
        let public = identity.public();
        let user_id = public.user_id();
        let name = name.into();
        let sig = identity
            .sign(&signing_input(
                &user_id,
                &public.ed25519_pub,
                &public.x25519_pub,
                &name,
                tcp_port,
            ))
            .to_vec();
        Announce {
            user_id,
            ed25519_pub: public.ed25519_pub,
            x25519_pub: public.x25519_pub,
            name,
            tcp_port,
            sig,
        }
    }

    /// True if the announce is internally consistent and authentically signed:
    /// `user_id` is the fingerprint of `ed25519_pub`, and `sig` verifies.
    pub fn verify(&self) -> bool {
        if self.user_id != PublicIdentity::user_id_from(&self.ed25519_pub) {
            return false;
        }
        let Ok(sig): Result<[u8; 64], _> = self.sig.as_slice().try_into() else {
            return false;
        };
        DeviceIdentity::verify(
            &self.ed25519_pub,
            &signing_input(
                &self.user_id,
                &self.ed25519_pub,
                &self.x25519_pub,
                &self.name,
                self.tcp_port,
            ),
            &sig,
        )
    }

    /// The advertised public identity (Ed25519 + X25519 keys).
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            ed25519_pub: self.ed25519_pub,
            x25519_pub: self.x25519_pub,
        }
    }
}

/// Frame an announce for the wire (magic + version + bincode).
pub fn encode(announce: &Announce) -> Vec<u8> {
    let body = bincode::serialize(announce).expect("announce serializes");
    let mut out = Vec::with_capacity(MAGIC.len() + 1 + body.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&body);
    out
}

/// Parse a wire datagram into an announce, or `None` if the framing/bincode is
/// invalid. The result is NOT yet authenticated — call [`Announce::verify`].
pub fn decode(data: &[u8]) -> Option<Announce> {
    if data.len() < MAGIC.len() + 1 || &data[..MAGIC.len()] != MAGIC || data[MAGIC.len()] != VERSION
    {
        return None;
    }
    bincode::deserialize(&data[MAGIC.len() + 1..]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_announce_verifies() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "Alice", 4000);
        assert!(a.verify());
        assert_eq!(a.user_id, id.public().user_id());
        assert_eq!(a.public(), id.public());
    }

    #[test]
    fn tampered_signature_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.sig[0] ^= 0xFF;
        assert!(!a.verify());
    }

    #[test]
    fn mismatched_user_id_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.user_id = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert!(!a.verify());
    }

    #[test]
    fn tampered_field_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.tcp_port = 9999; // not what was signed
        assert!(!a.verify());
    }

    #[test]
    fn encode_decode_round_trips() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "Alice", 4000);
        let bytes = encode(&a);
        let back = decode(&bytes).expect("decodes");
        assert_eq!(back, a);
        assert!(back.verify());
    }

    #[test]
    fn decode_rejects_bad_framing() {
        assert!(decode(&[]).is_none());
        assert!(decode(b"XXXX\x01garbage").is_none()); // bad magic
        assert!(decode(b"MTAN\x02garbage").is_none()); // bad version
    }
}
