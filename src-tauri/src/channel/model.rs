//! Channel membership + per-epoch key state. `ChannelMeta` is the plaintext
//! membership snapshot carried by a `MembershipChange` event (the post office may
//! read it — membership is accepted metadata; the event is still author-signed).
//! `ChannelState` is a member's view: current membership + the group keys it holds
//! per epoch, used to seal/open channel messages (which self-describe their epoch).

use crate::channel::crypto::{open_channel_message, seal_channel_message, ChannelError, GroupKey};
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
/// group keys this node holds keyed by epoch.
pub struct ChannelState {
    id: ConversationId,
    name: String,
    members: Vec<PublicIdentity>,
    epoch: u64,
    keys: HashMap<u64, GroupKey>,
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
            keys: HashMap::new(),
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

    /// Record the group key this node holds for `epoch` (generated locally or opened
    /// from a sealed key delivered by another member).
    pub fn record_key(&mut self, epoch: u64, key: GroupKey) {
        self.keys.insert(epoch, key);
    }

    pub fn key_for(&self, epoch: u64) -> Option<&GroupKey> {
        self.keys.get(&epoch)
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

    /// Seal `plaintext` under the CURRENT epoch's key. The wire payload is
    /// `epoch (u64 LE) ‖ channel-message-envelope`, so any receiver can pick the
    /// right key. Errors if this node lacks the current epoch's key.
    pub fn seal_message(&self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let key = self
            .keys
            .get(&self.epoch)
            .ok_or_else(|| ChannelError::Malformed("no group key for the current epoch".into()))?;
        let envelope = seal_channel_message(key, plaintext)?;
        let mut out = Vec::with_capacity(8 + envelope.len());
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.extend_from_slice(&envelope);
        Ok(out)
    }

    /// Open a channel-message payload (`epoch ‖ envelope`) using the key we hold for
    /// that epoch. `None` if the payload is malformed or we lack that epoch's key
    /// (e.g. a message from before we joined, or after we were removed).
    pub fn open_message(&self, payload: &[u8]) -> Option<Vec<u8>> {
        if payload.len() < 8 {
            return None;
        }
        let (epoch_bytes, envelope) = payload.split_at(8);
        let epoch = u64::from_le_bytes(epoch_bytes.try_into().ok()?);
        let key = self.keys.get(&epoch)?;
        open_channel_message(key, envelope).ok()
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

    #[test]
    fn seal_then_open_a_message_at_the_current_epoch() {
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let key = GroupKey::generate();

        let mut alice = ChannelState::from_meta(id, meta("general", &[&a], 0));
        alice.record_key(0, key.clone());
        let payload = alice.seal_message(b"hi channel").unwrap();

        let mut bob = ChannelState::from_meta(id, meta("general", &[&a], 0));
        bob.record_key(0, key);
        assert_eq!(
            bob.open_message(&payload).as_deref(),
            Some(&b"hi channel"[..])
        );
    }

    #[test]
    fn open_returns_none_without_the_epochs_key() {
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let key = GroupKey::generate();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a], 0));
        alice.record_key(0, key);
        let payload = alice.seal_message(b"secret").unwrap();

        let outsider = ChannelState::from_meta(id, meta("general", &[&a], 0));
        assert!(outsider.open_message(&payload).is_none());
        assert!(outsider.open_message(&[0u8; 4]).is_none());
    }

    #[test]
    fn seal_fails_without_a_key_for_the_current_epoch() {
        let a = DeviceIdentity::generate();
        let state = ChannelState::from_meta(new_channel_id(), meta("general", &[&a], 2));
        assert!(state.seal_message(b"x").is_err());
    }

    #[test]
    fn open_returns_none_for_an_epoch_whose_key_we_lack() {
        // The read-side of rotation: a member who only holds a LATER epoch's key
        // cannot open an earlier-epoch message (they joined after that rotation).
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a], 0));
        alice.record_key(0, GroupKey::generate());
        let payload = alice.seal_message(b"epoch-0 message").unwrap();

        let mut latecomer = ChannelState::from_meta(id, meta("general", &[&a], 1));
        latecomer.record_key(1, GroupKey::generate()); // has epoch-1 key, not epoch-0
        assert!(latecomer.open_message(&payload).is_none());
    }

    // --- sender-key path (additive) ---

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
