//! Channel orchestration over the event log: the per-member sealed group-key
//! payload carried by `KeyRotation` events, the payload builders for the three
//! channel event kinds, and a [`ChannelBook`] that replays a channel's events into
//! per-channel state + decrypted messages. Operates on `Event`s — no live network.

use crate::channel::{open_group_key, seal_group_key, ChannelMeta, ChannelState, GroupKey};
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::message::MessageBody;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

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

/// A decrypted channel message surfaced to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedChannelMessage {
    pub channel_id: ConversationId,
    pub channel_name: String,
    pub from: String,
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
}

/// A node's view of all the channels it knows: each channel's [`ChannelState`]
/// keyed by channel id. [`ChannelBook::process`] replays a channel's events (in the
/// causal order the log returns them) to update membership/keys and surface
/// decrypted messages.
#[derive(Default)]
pub struct ChannelBook {
    states: HashMap<ConversationId, ChannelState>,
    emitted: HashSet<EventId>,
}

impl ChannelBook {
    pub fn new() -> Self {
        ChannelBook::default()
    }

    /// The state for a known channel, if any.
    pub fn state(&self, channel_id: &ConversationId) -> Option<&ChannelState> {
        self.states.get(channel_id)
    }

    /// The ids of all channels this book knows.
    pub fn channel_ids(&self) -> Vec<crate::eventlog::event::ConversationId> {
        self.states.keys().copied().collect()
    }

    /// Replay `events` (a channel's events, in log/causal order) for the node `me`.
    /// Updates membership + keys and returns the channel `Message`s that are newly
    /// decryptable and authored by someone other than `me`. Events we can't act on
    /// (a key we don't have, a membership we're not in) are skipped.
    ///
    /// Call with the channel's FULL event log, not a delta: an event skipped because
    /// its prerequisite hasn't been applied yet (e.g. a `KeyRotation` seen before its
    /// `MembershipChange` under out-of-order delivery) is recovered on the next
    /// full-log replay, where the prerequisite is processed first. The caller dedups
    /// the returned messages across calls (the live node uses an emitted-id set).
    pub fn process(
        &mut self,
        me: &DeviceIdentity,
        channel_id: ConversationId,
        events: &[&Event],
    ) -> Vec<ReceivedChannelMessage> {
        let my_author = Author::from_ed25519(me.public().ed25519_pub);
        let mut out = Vec::new();

        for event in events {
            match event.kind {
                EventKind::MembershipChange => {
                    if let Some(meta) = ChannelMeta::decode(&event.ciphertext) {
                        if !meta.is_member(&me.public().user_id()) {
                            continue;
                        }
                        match self.states.get_mut(&channel_id) {
                            Some(state) => state.apply_meta(meta),
                            None => {
                                self.states
                                    .insert(channel_id, ChannelState::from_meta(channel_id, meta));
                            }
                        }
                    }
                }
                EventKind::KeyRotation => {
                    let Some(sealed) = SealedKeys::decode(&event.ciphertext) else {
                        continue;
                    };
                    let Some(state) = self.states.get_mut(&channel_id) else {
                        continue;
                    };
                    let author_uid = event.author.user_id();
                    let Some(author_x25519) = state
                        .members()
                        .iter()
                        .find(|m| m.user_id() == author_uid)
                        .map(|m| m.x25519_pub)
                    else {
                        continue;
                    };
                    if let Some(key) = open_my_key(me, &author_x25519, &sealed) {
                        state.record_key(sealed.epoch, key);
                    }
                }
                EventKind::Message => {
                    if event.author == my_author || self.emitted.contains(&event.id) {
                        continue; // our own message, or already surfaced
                    }
                    let Some(state) = self.states.get(&channel_id) else {
                        continue;
                    };
                    if let Some(plaintext) = state.open_message(&event.ciphertext) {
                        let body = MessageBody::decode(&plaintext);
                        let msg = ReceivedChannelMessage {
                            channel_id,
                            channel_name: state.name().to_string(),
                            from: event.author.user_id(),
                            text: body.text,
                            reply_to: body.reply_to,
                        };
                        self.emitted.insert(event.id);
                        out.push(msg);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{new_channel_id, ChannelMeta, ChannelState};
    use crate::eventlog::event::{Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::eventlog::sync::reconcile;

    fn append(
        log: &mut EventLog,
        author: &DeviceIdentity,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) {
        let (parents, lamport) = log.prepare(&channel);
        let self_author = Author::from_ed25519(author.public().ed25519_pub);
        let seq = log
            .version_vector(&channel)
            .get(&self_author)
            .copied()
            .unwrap_or(0)
            + 1;
        let event = Event::new(author, channel, seq, parents, lamport, 0, kind, ciphertext);
        log.append(event).unwrap();
    }

    fn collect<'a>(log: &'a EventLog, channel: &ConversationId) -> Vec<&'a Event> {
        log.events(channel)
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

    #[test]
    fn channel_create_message_and_rotation_on_add() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();

        // Alice creates the channel {Alice, Bob} at epoch 0.
        let mut alice_log = EventLog::default();
        let key0 = GroupKey::generate();
        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let sealed0 = seal_keys_for(&alice, &[alice.public(), bob.public()], &key0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );

        let mut alice_state = ChannelState::from_meta(channel, meta0.clone());
        alice_state.record_key(0, key0.clone());
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_message(b"hello team").unwrap(),
        );

        // Bob reconciles + processes → learns the channel, opens his key, reads it.
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut alice_log, channel);
        let mut bob_book = ChannelBook::new();
        let got = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, b"hello team");
        assert_eq!(got[0].from, alice.public().user_id());
        assert_eq!(bob_book.state(&channel).unwrap().epoch(), 0);

        // Alice adds Carol → epoch 1.
        let key1 = GroupKey::generate();
        let meta1 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public(), carol.public()],
            epoch: 1,
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta1.encode(),
        );
        let sealed1 = seal_keys_for(&alice, &meta1.members, &key1, 1).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed1.encode(),
        );
        alice_state.apply_meta(meta1.clone());
        alice_state.record_key(1, key1.clone());
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_message(b"welcome carol").unwrap(),
        );

        // Carol reconciles fresh → gets epoch-1 key only → reads "welcome carol" but NOT epoch-0.
        let mut carol_log = EventLog::default();
        reconcile(&mut carol_log, &mut alice_log, channel);
        let mut carol_book = ChannelBook::new();
        let carol_got = carol_book.process(&carol, channel, &collect(&carol_log, &channel));
        assert_eq!(carol_got.len(), 1);
        assert_eq!(carol_got[0].text, b"welcome carol");
        assert_eq!(carol_book.state(&channel).unwrap().epoch(), 1);

        // Bob reconciles the new events and reads the epoch-1 message.
        reconcile(&mut bob_log, &mut alice_log, channel);
        let bob_got2 = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert!(bob_got2.iter().any(|m| m.text == b"welcome carol"));
        assert_eq!(bob_book.state(&channel).unwrap().epoch(), 1);
    }

    #[test]
    fn process_does_not_re_emit_an_already_seen_message() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut alice_log = EventLog::default();
        let key0 = GroupKey::generate();
        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let sealed0 = seal_keys_for(&alice, &meta0.members, &key0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        let mut alice_state = ChannelState::from_meta(channel, meta0.clone());
        alice_state.record_key(0, key0);
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_message(b"hi").unwrap(),
        );

        let mut book = ChannelBook::new();
        let first = book.process(&bob, channel, &collect(&alice_log, &channel));
        assert_eq!(first.len(), 1);
        let second = book.process(&bob, channel, &collect(&alice_log, &channel));
        assert!(second.is_empty());
    }
}
