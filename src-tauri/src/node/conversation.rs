//! DM conversation identity and the DM event ↔ sealed-payload helpers.

use crate::discovery::roster::{Roster, UserId};
use crate::dm;
use crate::eventlog::event::{ConversationId, Event, EventId, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::message::MessageBody;
use sha2::{Digest, Sha256};

/// Domain separator for DM conversation-id derivation.
const DM_CONV_DOMAIN: &[u8] = b"mesh-talk-dm-conversation-v1";

/// The deterministic conversation id for the 1:1 DM between two peers — a hash of
/// the SORTED pair of Ed25519 keys, so both peers compute the same id regardless
/// of who is "a" and who is "b". A self-DM (`a == b`) yields a well-defined,
/// unique id (it hashes the key twice); it never collides with a two-party pair.
pub fn dm_conversation_id(a: &PublicIdentity, b: &PublicIdentity) -> ConversationId {
    let (lo, hi) = if a.ed25519_pub <= b.ed25519_pub {
        (a.ed25519_pub, b.ed25519_pub)
    } else {
        (b.ed25519_pub, a.ed25519_pub)
    };
    let mut hasher = Sha256::new();
    hasher.update(DM_CONV_DOMAIN);
    hasher.update(lo);
    hasher.update(hi);
    ConversationId::new(hasher.finalize().into())
}

/// Seal `text` for `recipient` and wrap it in a signed `Message` event for
/// `conversation`. The caller supplies `seq`/`parents`/`lamport` from the local
/// event log (and `wall_clock` from the system clock).
#[allow(clippy::too_many_arguments)]
pub fn build_dm_event(
    sender: &DeviceIdentity,
    recipient: &PublicIdentity,
    conversation: ConversationId,
    seq: u64,
    parents: Vec<EventId>,
    lamport: u64,
    wall_clock: u64,
    text: &[u8],
) -> Result<Event, dm::DmError> {
    let sealed = dm::seal(sender, &recipient.x25519_pub, text)?;
    Ok(Event::new(
        sender,
        conversation,
        seq,
        parents,
        lamport,
        wall_clock,
        EventKind::Message,
        sealed,
    ))
}

/// Try to open a received `Message` event as a DM addressed to us: look up the
/// author's X25519 key in the roster and decrypt. Returns `(author_user_id,
/// author_name, plaintext)`, or `None` if the event isn't a Message, the author
/// is unknown to us, or `dm::open` fails — whether because it wasn't sealed for
/// us or because the envelope is malformed (both are collapsed to `None` here).
pub fn open_dm_event(
    recipient: &DeviceIdentity,
    roster: &Roster,
    event: &Event,
) -> Option<(UserId, String, Vec<u8>, Option<EventId>)> {
    if event.kind != EventKind::Message {
        return None;
    }
    let author_user_id = event.author.user_id();
    let peer = roster.get(&author_user_id)?;
    let plaintext = dm::open(recipient, &peer.public.x25519_pub, &event.ciphertext).ok()?;
    let body = MessageBody::decode(&plaintext);
    Some((author_user_id, peer.name.clone(), body.text, body.reply_to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::announce::Announce;
    use std::net::{IpAddr, Ipv4Addr};

    fn roster_knowing(peer: &DeviceIdentity, name: &str, self_user_id: &str) -> Roster {
        let mut roster = Roster::default();
        roster.update(
            &Announce::new(peer, name, 4000),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            self_user_id,
        );
        roster
    }

    #[test]
    fn conversation_id_is_symmetric_and_deterministic() {
        let a = DeviceIdentity::generate().public();
        let b = DeviceIdentity::generate().public();
        let id = dm_conversation_id(&a, &b);
        assert_eq!(id, dm_conversation_id(&b, &a)); // order-independent → both peers agree
        assert_eq!(id, dm_conversation_id(&a, &b)); // deterministic
    }

    #[test]
    fn different_pairs_have_different_conversation_ids() {
        let a = DeviceIdentity::generate().public();
        let b = DeviceIdentity::generate().public();
        let c = DeviceIdentity::generate().public();
        assert_ne!(dm_conversation_id(&a, &b), dm_conversation_id(&a, &c));
    }

    #[test]
    fn build_then_open_round_trips_via_roster() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let conv = dm_conversation_id(&alice.public(), &bob.public());

        let event =
            build_dm_event(&alice, &bob.public(), conv, 1, vec![], 1, 0, b"hi bob").unwrap();

        // Bob's roster knows Alice (so he can find her X25519 key to decrypt).
        let roster = roster_knowing(&alice, "Alice", &bob.public().user_id());
        let (from, name, text, _reply) = open_dm_event(&bob, &roster, &event).expect("bob opens");
        assert_eq!(from, alice.public().user_id());
        assert_eq!(name, "Alice");
        assert_eq!(text, b"hi bob");
    }

    #[test]
    fn open_returns_none_when_author_is_unknown() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let conv = dm_conversation_id(&alice.public(), &bob.public());
        let event = build_dm_event(&alice, &bob.public(), conv, 1, vec![], 1, 0, b"hi").unwrap();
        // Empty roster → Bob can't resolve Alice's key.
        assert!(open_dm_event(&bob, &Roster::default(), &event).is_none());
    }

    #[test]
    fn open_returns_none_for_a_dm_not_addressed_to_us() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let conv = dm_conversation_id(&alice.public(), &bob.public());
        // Alice seals for Bob; Carol (who knows Alice) cannot open it.
        let event =
            build_dm_event(&alice, &bob.public(), conv, 1, vec![], 1, 0, b"secret").unwrap();
        let roster = roster_knowing(&alice, "Alice", &carol.public().user_id());
        assert!(open_dm_event(&carol, &roster, &event).is_none());
    }
}
