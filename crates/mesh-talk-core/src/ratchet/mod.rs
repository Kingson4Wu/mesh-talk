//! Double Ratchet for DMs (Phase 3): forward secrecy + post-compromise security over
//! the existing X25519 / HKDF-SHA256 / AES-256-GCM primitives. `kdf` is the key
//! schedule; this module is the state machine. Pure — durable state + the Node DM
//! integration are later plans.

mod kdf;
pub mod state;

pub(crate) use kdf::{kdf_ck, message_keys};

pub use state::{init_alice, init_bob, Header, RatchetError, RatchetState};

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// Deterministic DM session root: HKDF-SHA256 over the static-static X25519 DH of the two
/// identities. Symmetric — each peer derives the same secret from the other's public key, so a
/// session bootstraps with no key-exchange round-trip. Shared by the native node and the wasm
/// node so both speak the identical DM wire format.
pub fn dm_shared_root(me: &DeviceIdentity, peer: &PublicIdentity) -> [u8; 32] {
    let my_secret = StaticSecret::from(me.secret_bytes().1); // x25519 secret
    let dh = my_secret
        .diffie_hellman(&PublicKey::from(peer.x25519_pub))
        .to_bytes();
    let hk = Hkdf::<Sha256>::new(None, &dh);
    let mut out = [0u8; 32];
    hk.expand(b"MeshTalk-DM-Root", &mut out).expect("32 valid");
    out
}
