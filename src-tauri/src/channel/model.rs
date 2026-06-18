//! Channel membership + per-sender ratchet state. `ChannelMeta` is the plaintext
//! membership snapshot carried by a `MembershipChange` event (the post office may
//! read it — membership is accepted metadata; the event is still author-signed).
//! `ChannelState` is a member's view: current membership + the sender-key ratchet
//! state it holds (its own per-epoch sending key + per-`(author,epoch)` receiving
//! chains), used to seal/open channel messages with single-use forward-secret keys.

use crate::channel::crypto::ChannelError;
use crate::channel::sender_key::{
    open_message as sk_open, seal_message as sk_seal, SenderChain, SenderKey, SenderKeyDistribution,
};
use crate::eventlog::event::ConversationId;
use crate::identity::device::PublicIdentity;
use bincode::Options;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The plaintext membership snapshot carried by a `MembershipChange` event: the full
/// member set + channel name + epoch. A snapshot (not a delta) so the latest one
/// fully defines current membership on replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub name: String,
    pub members: Vec<PublicIdentity>,
    pub epoch: u64,
}

impl ChannelMeta {
    /// Serialize for an event's payload (fixint, matching the codebase wire style).
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("channel meta serializes")
    }

    /// Parse a membership snapshot, fail-closed (reject trailing bytes).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }

    /// Whether `user_id` (an Ed25519 fingerprint) is a current member.
    pub fn is_member(&self, user_id: &str) -> bool {
        self.members.iter().any(|m| m.user_id() == user_id)
    }
}

/// A fresh random channel id — a `ConversationId` minted at creation, distinct from
/// the derived DM conversation ids.
pub fn new_channel_id() -> ConversationId {
    let mut id = [0u8; 32];
    OsRng.fill_bytes(&mut id);
    ConversationId::new(id)
}

/// The per-message header for the sender-key path: epoch, the author's user id, and
/// the message's position `n` in that author's chain. Encoded as the AAD bound into
/// each sealed channel message (epoch ‖ sender ‖ n).
#[derive(Serialize, Deserialize)]
struct MsgHeader {
    epoch: u64,
    sender: String,
    n: u32,
}

impl MsgHeader {
    fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("hdr")
    }
    fn decode(b: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(b)
            .ok()
    }
}

/// A member's view of a channel: its id, current membership/name/epoch, and the
/// sender-key ratchet state this node holds (its own per-epoch sending key plus the
/// receiving chains it follows for each `(author, epoch)`).
pub struct ChannelState {
    id: ConversationId,
    name: String,
    members: Vec<PublicIdentity>,
    epoch: u64,
    my_user_id: Option<String>,
    my_sender: HashMap<u64, SenderKey>,
    sender_chains: HashMap<(String, u64), SenderChain>,
}

impl ChannelState {
    /// Build state from a channel id and its current membership snapshot (no keys yet).
    pub fn from_meta(id: ConversationId, meta: ChannelMeta) -> Self {
        ChannelState {
            id,
            name: meta.name,
            members: meta.members,
            epoch: meta.epoch,
            my_user_id: None,
            my_sender: HashMap::new(),
            sender_chains: HashMap::new(),
        }
    }

    /// Adopt a (possibly newer) membership snapshot: if it advances the epoch, take
    /// its name/members/epoch. Older or equal snapshots are ignored — idempotent
    /// replay regardless of event arrival order.
    pub fn apply_meta(&mut self, meta: ChannelMeta) {
        if meta.epoch >= self.epoch {
            self.name = meta.name;
            self.members = meta.members;
            self.epoch = meta.epoch;
        }
    }

    pub fn id(&self) -> ConversationId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn members(&self) -> &[PublicIdentity] {
        &self.members
    }
    pub fn is_member(&self, user_id: &str) -> bool {
        self.members.iter().any(|m| m.user_id() == user_id)
    }

    /// Set this node's identity (who "I" am) for sender-key send/AAD. Idempotent.
    pub fn set_identity(&mut self, my_user_id: String) {
        self.my_user_id = Some(my_user_id);
    }

    /// My current-epoch sender-key distribution (generating my sender key if needed).
    /// Distribute this (sealed per-member) so members can follow my chain from n=0.
    pub fn my_sender_distribution(&mut self) -> SenderKeyDistribution {
        let epoch = self.epoch;
        self.my_sender
            .entry(epoch)
            .or_insert_with(SenderKey::generate)
            .distribution()
    }

    /// Whether this node has already generated (and thus should have distributed) its
    /// sender key for `epoch`.
    pub fn has_my_sender(&self, epoch: u64) -> bool {
        self.my_sender.contains_key(&epoch)
    }

    /// Record a peer's sender chain for `(author, epoch)` from their distribution.
    pub fn record_sender_chain(&mut self, author: String, epoch: u64, skd: &SenderKeyDistribution) {
        self.sender_chains
            .entry((author, epoch))
            .or_insert_with(|| SenderChain::from_distribution(skd));
    }

    /// Seal `plaintext` with my current-epoch sender key. Wire = `u16 hdr_len ‖ hdr ‖ ct`,
    /// AAD = hdr (epoch‖sender‖n). Errors if my identity isn't set.
    pub fn seal_sender_message(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let me = self
            .my_user_id
            .clone()
            .ok_or_else(|| ChannelError::Malformed("no identity".into()))?;
        let epoch = self.epoch;
        let sk = self
            .my_sender
            .entry(epoch)
            .or_insert_with(SenderKey::generate);
        let (n, mk) = sk.ratchet();
        let hdr = MsgHeader {
            epoch,
            sender: me,
            n,
        }
        .encode();
        let ct = sk_seal(&mk, plaintext, &hdr).map_err(|_| ChannelError::Encrypt)?;
        let mut out = Vec::with_capacity(2 + hdr.len() + ct.len());
        out.extend_from_slice(&(hdr.len() as u16).to_be_bytes());
        out.extend_from_slice(&hdr);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Open a sender-keyed message (`&mut` — the receiving chain ratchets + deletes the
    /// key). `None` if we lack that sender's chain for the epoch or the key is consumed.
    pub fn open_sender_message(&mut self, wire: &[u8]) -> Option<Vec<u8>> {
        if wire.len() < 2 {
            return None;
        }
        let hlen = u16::from_be_bytes([wire[0], wire[1]]) as usize;
        if wire.len() < 2 + hlen {
            return None;
        }
        let hdr_bytes = &wire[2..2 + hlen];
        let ct = &wire[2 + hlen..];
        let hdr = MsgHeader::decode(hdr_bytes)?;
        let chain = self
            .sender_chains
            .get_mut(&(hdr.sender.clone(), hdr.epoch))?;
        let mk = chain.message_key(hdr.n).ok()?;
        sk_open(&mk, ct, hdr_bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    fn meta(name: &str, members: &[&DeviceIdentity], epoch: u64) -> ChannelMeta {
        ChannelMeta {
            name: name.to_string(),
            members: members.iter().map(|d| d.public()).collect(),
            epoch,
        }
    }

    #[test]
    fn channel_meta_round_trips_and_rejects_trailing() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let m = meta("general", &[&a, &b], 0);
        let bytes = m.encode();
        assert_eq!(ChannelMeta::decode(&bytes), Some(m.clone()));
        let mut junk = bytes.clone();
        junk.push(0xAB);
        assert_eq!(ChannelMeta::decode(&junk), None);
        assert!(m.is_member(&a.public().user_id()));
        assert!(!m.is_member("nobody"));
    }

    #[test]
    fn channel_ids_are_random() {
        assert_ne!(new_channel_id(), new_channel_id());
    }

    #[test]
    fn apply_meta_adopts_newer_epoch_only() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut state = ChannelState::from_meta(id, meta("general", &[&a], 0));
        assert_eq!(state.members().len(), 1);

        state.apply_meta(meta("general", &[&a, &b], 1));
        assert_eq!(state.epoch(), 1);
        assert_eq!(state.members().len(), 2);
        assert!(state.is_member(&b.public().user_id()));

        state.apply_meta(meta("general", &[&a], 0));
        assert_eq!(state.epoch(), 1);
        assert_eq!(state.members().len(), 2);
    }

    // --- sender-key path ---

    fn sender_pair(epoch: u64) -> (ChannelState, ChannelState) {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a, &b], epoch));
        alice.set_identity("alice".into());
        let mut bob = ChannelState::from_meta(id, meta("general", &[&a, &b], epoch));
        bob.set_identity("bob".into());
        // Bob follows Alice's chain from her distribution.
        let skd = alice.my_sender_distribution();
        bob.record_sender_chain("alice".into(), epoch, &skd);
        (alice, bob)
    }

    #[test]
    fn sender_key_seal_then_open() {
        let (mut alice, mut bob) = sender_pair(0);
        let wire = alice.seal_sender_message(b"hi").unwrap();
        assert_eq!(bob.open_sender_message(&wire).as_deref(), Some(&b"hi"[..]));
    }

    #[test]
    fn sender_key_out_of_order() {
        let (mut alice, mut bob) = sender_pair(0);
        let w0 = alice.seal_sender_message(b"m0").unwrap();
        let w1 = alice.seal_sender_message(b"m1").unwrap();
        let w2 = alice.seal_sender_message(b"m2").unwrap();
        // Bob opens 2, 0, 1 — the chain ratchets forward and buffers skipped keys.
        assert_eq!(bob.open_sender_message(&w2).as_deref(), Some(&b"m2"[..]));
        assert_eq!(bob.open_sender_message(&w0).as_deref(), Some(&b"m0"[..]));
        assert_eq!(bob.open_sender_message(&w1).as_deref(), Some(&b"m1"[..]));
    }

    #[test]
    fn sender_key_is_single_use() {
        let (mut alice, mut bob) = sender_pair(0);
        let wire = alice.seal_sender_message(b"once").unwrap();
        assert_eq!(
            bob.open_sender_message(&wire).as_deref(),
            Some(&b"once"[..])
        );
        // The key was consumed (forward secrecy) — re-opening the same wire fails.
        assert!(bob.open_sender_message(&wire).is_none());
    }

    #[test]
    fn sender_key_open_without_chain_is_none() {
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a], 0));
        alice.set_identity("alice".into());
        let wire = alice.seal_sender_message(b"secret").unwrap();
        // A member who never recorded Alice's chain cannot open her message.
        let mut outsider = ChannelState::from_meta(id, meta("general", &[&a], 0));
        outsider.set_identity("carol".into());
        assert!(outsider.open_sender_message(&wire).is_none());
    }

    #[test]
    fn seal_sender_message_fails_without_identity() {
        let a = DeviceIdentity::generate();
        let mut state = ChannelState::from_meta(new_channel_id(), meta("general", &[&a], 0));
        assert!(state.seal_sender_message(b"x").is_err());
    }
}
