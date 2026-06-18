//! Channel sender-key ratchet: per-sender symmetric chains for forward-secret group
//! messages. Reuses the DM ratchet's chain step + message-key derivation; there is no
//! DH ratchet (sender keys rotate wholesale on membership change). A sender advances
//! its own chain per message (deleting old keys); each receiver tracks that sender's
//! chain and ratchets forward to a message's position, buffering bounded skipped keys.

use crate::ratchet::{kdf_ck, message_keys};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use bincode::Options;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// Max messages a receiver will skip forward in one sender's chain (DoS bound) and
/// the total buffered skipped keys.
const MAX_SKIP: u32 = 1000;
const MAX_SKIPPED_TOTAL: usize = 2000;

#[derive(Debug)]
pub enum SenderKeyError {
    Encrypt,
    Decrypt,
    TooManySkipped,
    Malformed(String),
}

/// A member's SENDING chain for one channel epoch.
pub struct SenderKey {
    chain_key: [u8; 32],
    n: u32,
}

impl SenderKey {
    /// A fresh sender key (new chain, position 0).
    pub fn generate() -> Self {
        let mut chain_key = [0u8; 32];
        OsRng.fill_bytes(&mut chain_key);
        SenderKey { chain_key, n: 0 }
    }

    /// The distribution snapshot to seal to members so they can follow this chain
    /// from the CURRENT position. (Distribute right after `generate`, before sending,
    /// so members start at n=0.)
    pub fn distribution(&self) -> SenderKeyDistribution {
        SenderKeyDistribution {
            chain_key: self.chain_key,
            n: self.n,
        }
    }

    /// Advance the chain by one message: returns `(n, message_key)` and deletes the
    /// old chain key (forward secrecy).
    pub fn ratchet(&mut self) -> (u32, [u8; 32]) {
        let (next, mk) = kdf_ck(&self.chain_key);
        let n = self.n;
        self.chain_key = next;
        self.n += 1;
        (n, mk)
    }
}

/// The sealed-per-member initial sender chain (chain key + starting position).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderKeyDistribution {
    pub chain_key: [u8; 32],
    pub n: u32,
}

impl SenderKeyDistribution {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("skd serializes")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// A receiver's view of one sender's chain.
pub struct SenderChain {
    chain_key: [u8; 32],
    n: u32,
    skipped: HashMap<u32, [u8; 32]>,
    order: VecDeque<u32>,
}

impl SenderChain {
    pub fn from_distribution(skd: &SenderKeyDistribution) -> Self {
        SenderChain {
            chain_key: skd.chain_key,
            n: skd.n,
            skipped: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// The message key for position `target`, ratcheting forward (buffering skipped
    /// keys) as needed. `None` if `target` is behind a consumed position with no
    /// buffered key, or beyond the skip bound.
    pub fn message_key(&mut self, target: u32) -> Result<[u8; 32], SenderKeyError> {
        if let Some(mk) = self.skipped.remove(&target) {
            return Ok(mk);
        }
        if target < self.n {
            return Err(SenderKeyError::Malformed(
                "message key already consumed".into(),
            ));
        }
        if target - self.n > MAX_SKIP {
            return Err(SenderKeyError::TooManySkipped);
        }
        while self.n < target {
            let (next, mk) = kdf_ck(&self.chain_key);
            self.skipped.insert(self.n, mk);
            self.order.push_back(self.n);
            self.chain_key = next;
            self.n += 1;
            while self.skipped.len() > MAX_SKIPPED_TOTAL {
                if let Some(old) = self.order.pop_front() {
                    self.skipped.remove(&old);
                } else {
                    break;
                }
            }
        }
        let (next, mk) = kdf_ck(&self.chain_key);
        self.chain_key = next;
        self.n += 1;
        Ok(mk)
    }
}

/// Seal `plaintext` with a single-use message key; `aad` authenticates the wire
/// framing (epoch ‖ sender ‖ n) the channel layer prepends.
pub fn seal_message(
    mk: &[u8; 32],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, SenderKeyError> {
    let (key, nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| SenderKeyError::Encrypt)?;
    cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| SenderKeyError::Encrypt)
}

pub fn open_message(
    mk: &[u8; 32],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, SenderKeyError> {
    let (key, nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| SenderKeyError::Decrypt)?;
    cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| SenderKeyError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(sk: &mut SenderKey, rc: &mut SenderChain, pt: &[u8], aad: &[u8]) -> Vec<u8> {
        let (n, mk) = sk.ratchet();
        let ct = seal_message(&mk, pt, aad).unwrap();
        let (rn, rmk) = (n, rc.message_key(n).unwrap());
        assert_eq!(rn, n);
        open_message(&rmk, &ct, aad).unwrap()
    }

    #[test]
    fn in_order_messages_round_trip() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        assert_eq!(exchange(&mut sk, &mut rc, b"m0", b"aad0"), b"m0");
        assert_eq!(exchange(&mut sk, &mut rc, b"m1", b"aad1"), b"m1");
    }

    #[test]
    fn out_of_order_uses_skipped_keys() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        let (n0, mk0) = sk.ratchet();
        let c0 = seal_message(&mk0, b"m0", b"a").unwrap();
        let (n1, mk1) = sk.ratchet();
        let c1 = seal_message(&mk1, b"m1", b"a").unwrap();
        let (n2, mk2) = sk.ratchet();
        let c2 = seal_message(&mk2, b"m2", b"a").unwrap();
        // receive 2, 0, 1
        let r2 = rc.message_key(n2).unwrap();
        assert_eq!(open_message(&r2, &c2, b"a").unwrap(), b"m2");
        let r0 = rc.message_key(n0).unwrap();
        assert_eq!(open_message(&r0, &c0, b"a").unwrap(), b"m0");
        let r1 = rc.message_key(n1).unwrap();
        assert_eq!(open_message(&r1, &c1, b"a").unwrap(), b"m1");
    }

    #[test]
    fn a_consumed_position_cannot_be_reopened() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        let (n0, _mk0) = sk.ratchet();
        let _ = rc.message_key(n0).unwrap();
        assert!(rc.message_key(n0).is_err()); // single-use (forward secrecy)
    }

    #[test]
    fn too_many_skipped_is_rejected() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        for _ in 0..(MAX_SKIP + 2) {
            sk.ratchet();
        }
        let (n, _mk) = sk.ratchet();
        assert!(matches!(
            rc.message_key(n),
            Err(SenderKeyError::TooManySkipped)
        ));
    }

    #[test]
    fn a_fresh_sender_key_after_rotation_is_unrelated() {
        let mut sk1 = SenderKey::generate();
        let (_n, mk1) = sk1.ratchet();
        let mut sk2 = SenderKey::generate(); // rotation: brand-new chain
                                             // Snapshot the distribution at position 0 (before sending) so a receiver
                                             // follows the new chain from the start.
        let mut rc2 = SenderChain::from_distribution(&sk2.distribution());
        let (_n2, mk2) = sk2.ratchet();
        assert_ne!(mk1, mk2);
        // A receiver with only the new distribution cannot open the old chain's key
        // position (different chain key).
        assert_ne!(rc2.message_key(0).unwrap(), mk1);
    }

    #[test]
    fn tampered_ciphertext_or_aad_fails() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        let (n, mk) = sk.ratchet();
        let mut ct = seal_message(&mk, b"secret", b"aad").unwrap();
        let rmk = rc.message_key(n).unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 0xFF;
        assert!(open_message(&rmk, &ct, b"aad").is_err());
        ct[last] ^= 0xFF;
        assert!(open_message(&rmk, &ct, b"WRONG").is_err()); // AAD mismatch
    }

    #[test]
    fn skd_codec_round_trips() {
        let skd = SenderKeyDistribution {
            chain_key: [4u8; 32],
            n: 5,
        };
        assert_eq!(SenderKeyDistribution::decode(&skd.encode()), Some(skd));
        let mut junk = SenderKeyDistribution {
            chain_key: [0u8; 32],
            n: 0,
        }
        .encode();
        junk.push(0xAB);
        assert_eq!(SenderKeyDistribution::decode(&junk), None);
    }
}
