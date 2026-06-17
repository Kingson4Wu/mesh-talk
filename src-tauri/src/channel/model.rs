//! Channel membership + per-epoch key state. `ChannelMeta` is the plaintext
//! membership snapshot carried by a `MembershipChange` event (the post office may
//! read it — membership is accepted metadata; the event is still author-signed).
//! `ChannelState` is a member's view: current membership + the group keys it holds
//! per epoch, used to seal/open channel messages (which self-describe their epoch).

use crate::channel::crypto::{open_channel_message, seal_channel_message, ChannelError, GroupKey};
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

/// A member's view of a channel: its id, current membership/name/epoch, and the
/// group keys this node holds keyed by epoch.
pub struct ChannelState {
    id: ConversationId,
    name: String,
    members: Vec<PublicIdentity>,
    epoch: u64,
    keys: HashMap<u64, GroupKey>,
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
}
