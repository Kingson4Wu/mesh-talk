//! The post office: an elected always-on peer that stores-and-forwards encrypted
//! events so a message reaches an offline recipient. It is a peer with two extra
//! hats — a full replica (a [`crate::eventlog::PersistentEventLog`]) and a
//! store-and-forward relay (it reconciles as a [`crate::eventlog::sync::SyncStore`]).
//! For DMs it is a *dumb relay*: it holds recipient-sealed envelopes it can never
//! decrypt, and never holds any conversation key.
//!
//! [`election`] decides — deterministically, so every peer agrees — which eligible
//! (always-on) peer is the post office: the one with the lowest identity
//! fingerprint. Full election with standbys and uptime detection is Phase 1.

use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::eventlog::persist::PersistentEventLog;
use crate::eventlog::store::AppendOutcome;
use crate::eventlog::sync::SyncStore;
use crate::eventlog::LogError;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use std::collections::HashSet;
use std::path::Path;

pub mod election;

pub use election::{elect, is_post_office};

/// A post office: a durable replica plus a store-and-forward relay. It accepts
/// validly-signed events (it cannot forge or alter them) and serves them to
/// peers via reconciliation, but it never decrypts a DM — it has no key and no
/// API to do so.
pub struct PostOffice {
    log: PersistentEventLog,
    identity: DeviceIdentity,
}

impl PostOffice {
    /// Open (or create) the post office's replica at `path`, under `identity`.
    pub fn open(path: &Path, password: &str, identity: DeviceIdentity) -> Result<Self, LogError> {
        Ok(Self {
            log: PersistentEventLog::open(path, password)?,
            identity,
        })
    }

    /// This post office's public identity (used by the election).
    pub fn public(&self) -> PublicIdentity {
        self.identity.public()
    }

    /// Accept an event for relay: validate (signature, integrity, parents, …) and
    /// persist it. A forged or malformed event is rejected.
    pub fn accept(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.log.append(event)
    }

    /// Whether this post office holds the event with the given id.
    pub fn has(&self, id: &EventId) -> bool {
        self.log.has(id)
    }

    /// The ids this post office holds for a conversation, sorted (for comparison).
    pub fn held_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut ids: Vec<EventId> = self.log.events(conversation).iter().map(|e| e.id).collect();
        ids.sort();
        ids
    }
}

impl SyncStore for PostOffice {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        SyncStore::event_ids(&self.log, conversation)
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        SyncStore::events_excluding(&self.log, conversation, have)
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        SyncStore::ingest(&mut self.log, event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::EventKind;
    use crate::eventlog::store::EventLog;
    use crate::eventlog::sync::reconcile;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    fn message(
        author: &DeviceIdentity,
        seq: u64,
        parents: Vec<EventId>,
        lamport: u64,
        body: &[u8],
    ) -> Event {
        Event::new(
            author,
            conv(),
            seq,
            parents,
            lamport,
            0,
            EventKind::Message,
            body.to_vec(),
        )
    }

    #[test]
    fn accepts_and_holds_a_valid_event() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let po_id = DeviceIdentity::generate();
        let po_pub = po_id.public();
        let mut po = PostOffice::open(&dir.path().join("po.log"), "pw", po_id).unwrap();
        assert_eq!(po.public(), po_pub); // the post office knows its own identity (for election)

        let ev = message(&alice, 1, vec![], 1, b"x");
        assert_eq!(po.accept(ev.clone()).unwrap(), AppendOutcome::Appended);
        assert!(po.has(&ev.id));
        assert_eq!(po.held_ids(&conv()), vec![ev.id]);
    }

    #[test]
    fn rejects_a_forged_event() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let mut po =
            PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();

        let mut forged = message(&alice, 1, vec![], 1, b"x");
        forged.sig[0] ^= 0xFF;
        assert!(po.accept(forged).is_err());
    }

    #[test]
    fn a_peer_reconciles_its_events_into_the_post_office() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let mut alice_log = EventLog::default();
        let ev = message(&alice, 1, vec![], 1, b"x");
        alice_log.append(ev.clone()).unwrap();

        let mut po =
            PostOffice::open(&dir.path().join("po.log"), "pw", DeviceIdentity::generate()).unwrap();
        // The post office is just another SyncStore.
        reconcile(&mut alice_log, &mut po, conv());
        assert!(po.has(&ev.id));
    }
}
