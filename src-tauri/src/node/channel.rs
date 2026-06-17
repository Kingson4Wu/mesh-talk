//! Channel orchestration over the event log: the per-member sealed group-key
//! payload carried by `KeyRotation` events, the payload builders for the three
//! channel event kinds, and a [`ChannelBook`] that replays a channel's events into
//! per-channel state + decrypted messages. Operates on `Event`s — no live network.

use crate::channel::{seal_group_key, GroupKey};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use bincode::Options;
use serde::{Deserialize, Serialize};

/// The `KeyRotation` event payload: the epoch's group key sealed to each member.
/// Each `(user_id, sealed)` is `seal_group_key(author, member.x25519, key)`; only
/// that member can open its entry. The post office relays it but never reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKeys {
    pub epoch: u64,
    pub entries: Vec<SealedKeyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKeyEntry {
    pub user_id: String,
    pub sealed: Vec<u8>,
}

impl SealedKeys {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("sealed keys serialize")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// Seal `key` (for `epoch`) to each member's X25519, producing the `KeyRotation`
/// payload. `author` is the sender whose static key the recipients use to open.
pub fn seal_keys_for(
    author: &DeviceIdentity,
    members: &[PublicIdentity],
    key: &GroupKey,
    epoch: u64,
) -> Result<SealedKeys, crate::channel::ChannelError> {
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        let sealed = seal_group_key(author, &member.x25519_pub, key)?;
        entries.push(SealedKeyEntry {
            user_id: member.user_id(),
            sealed,
        });
    }
    Ok(SealedKeys { epoch, entries })
}

/// A decrypted channel message surfaced to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedChannelMessage {
    pub channel_id: crate::eventlog::event::ConversationId,
    pub channel_name: String,
    pub from: String,
    pub text: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{open_group_key, GroupKey};

    /// Open the group key sealed to `me` in a `SealedKeys`, using the rotation author's
    /// X25519 (looked up from the channel membership). `None` if there is no entry for
    /// us or it fails to open.
    fn open_my_key(
        me: &DeviceIdentity,
        author_x25519: &[u8; 32],
        sealed: &SealedKeys,
    ) -> Option<GroupKey> {
        let my_uid = me.public().user_id();
        let entry = sealed.entries.iter().find(|e| e.user_id == my_uid)?;
        open_group_key(me, author_x25519, &entry.sealed).ok()
    }

    #[test]
    fn sealed_keys_round_trip_and_reject_trailing() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let key = GroupKey::generate();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &key, 0).unwrap();

        let bytes = sealed.encode();
        assert_eq!(SealedKeys::decode(&bytes), Some(sealed.clone()));
        let mut junk = bytes.clone();
        junk.push(0xAB);
        assert_eq!(SealedKeys::decode(&junk), None);
        assert_eq!(sealed.entries.len(), 2);
    }

    #[test]
    fn a_member_opens_its_own_sealed_key() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let key = GroupKey::generate();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &key, 0).unwrap();

        let opened = open_my_key(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened.as_bytes(), key.as_bytes());
    }

    #[test]
    fn a_non_member_has_no_entry_to_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let key = GroupKey::generate();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &key, 0).unwrap();
        assert!(open_my_key(&carol, &alice.public().x25519_pub, &sealed).is_none());
    }
}
