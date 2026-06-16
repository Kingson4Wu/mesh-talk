//! The signed discovery announce and its UDP wire format.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use bincode::Options;
use serde::{Deserialize, Serialize};

/// Domain separator for announce signatures.
const ANNOUNCE_DOMAIN: &[u8] = b"mesh-talk-announce-v1";
/// Wire framing: 4-byte magic + 1-byte version, then `bincode(Announce)`.
const MAGIC: &[u8; 4] = b"MTAN";
const VERSION: u8 = 1;

/// A peer's self-announcement: its identity keys, display name, TCP listen port,
/// and whether it serves as a post office, signed by its Ed25519 key. `user_id`
/// is the fingerprint of `ed25519_pub`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announce {
    pub user_id: String,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    pub name: String,
    pub tcp_port: u16,
    pub post_office: bool,
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
    post_office: bool,
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
    v.push(post_office as u8);
    v
}

impl Announce {
    /// Build and sign a normal (non-post-office) announce for `identity`.
    pub fn new(identity: &DeviceIdentity, name: impl Into<String>, tcp_port: u16) -> Self {
        Self::new_with_role(identity, name, tcp_port, false)
    }

    /// Build and sign an announce that advertises the post-office role.
    pub fn new_post_office(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Self {
        Self::new_with_role(identity, name, tcp_port, true)
    }

    fn new_with_role(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
        post_office: bool,
    ) -> Self {
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
                post_office,
            ))
            .to_vec();
        Announce {
            user_id,
            ed25519_pub: public.ed25519_pub,
            x25519_pub: public.x25519_pub,
            name,
            tcp_port,
            post_office,
            sig,
        }
    }

    /// True if the announce is internally consistent and authentically signed:
    /// `user_id` is the fingerprint of `ed25519_pub`, and `sig` verifies (over all
    /// fields including `post_office`).
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
                self.post_office,
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
    // Strict parse: reject trailing bytes (fail closed). The fixint encoding
    // matches `bincode::serialize` in `encode`, so valid announces still decode.
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize(&data[MAGIC.len() + 1..])
        .ok()
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

    #[test]
    fn announce_from_a_does_not_verify_under_b() {
        // Take A's signed announce, swap in B's identity keys + user_id (so the
        // user_id still matches its ed25519_pub) but keep A's signature → fails.
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let a_announce = Announce::new(&a, "Alice", 4000);
        let spoofed = Announce {
            user_id: b.public().user_id(),
            ed25519_pub: b.public().ed25519_pub,
            x25519_pub: a_announce.x25519_pub,
            name: a_announce.name.clone(),
            tcp_port: a_announce.tcp_port,
            post_office: a_announce.post_office,
            sig: a_announce.sig.clone(),
        };
        assert!(!spoofed.verify());
    }

    #[test]
    fn tampered_x25519_key_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.x25519_pub[0] ^= 0xFF; // the DH key is a signed field
        assert!(!a.verify());
    }

    #[test]
    fn decode_rejects_valid_framing_with_garbage_body() {
        assert!(decode(b"MTAN\x01\xFF\xFF\xFF\xFF").is_none());
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let id = DeviceIdentity::generate();
        let mut bytes = encode(&Announce::new(&id, "Alice", 4000));
        bytes.push(0xAB); // junk appended after a valid announce
        assert!(decode(&bytes).is_none());
    }

    #[test]
    fn post_office_role_round_trips_and_is_signed() {
        let id = DeviceIdentity::generate();
        let normal = Announce::new(&id, "Node", 4000);
        assert!(!normal.post_office);
        assert!(normal.verify());

        let po = Announce::new_post_office(&id, "Relay", 4000);
        assert!(po.post_office);
        assert!(po.verify());

        // The role survives the wire round-trip.
        let back = decode(&encode(&po)).expect("decodes");
        assert!(back.post_office);
        assert!(back.verify());
    }

    #[test]
    fn flipping_the_post_office_bit_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Node", 4000);
        a.post_office = true; // not what was signed
        assert!(!a.verify());
    }
}
