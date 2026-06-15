//! Event-log reconciliation ("sync"). Two peers converge a conversation's log
//! by exchanging the set of event ids each holds; the responder returns exactly
//! the events the requester is missing, in topological order. Received events
//! are validated through [`SyncStore::ingest`] (the store's normal `append`
//! gate), so sync can never inject forged or out-of-order history.
//!
//! Reconciliation is id-set based, not version-vector based: the store does not
//! guarantee dense per-author sequences, so a version-vector diff would miss a
//! gap. See the module plan for the rationale.

use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::eventlog::store::{AppendOutcome, EventLog};
use crate::eventlog::LogError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Sent by the peer initiating sync: "for this conversation, here are the ids I have."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    pub conversation: ConversationId,
    pub have: Vec<EventId>,
}

/// The responder's reply: the events the requester is missing (topological
/// order), plus the responder's own id-set so the requester can reciprocate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResponse {
    pub conversation: ConversationId,
    pub events: Vec<Event>,
    pub have: Vec<EventId>,
}

/// The requester's final message: the events the responder is missing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncFollowup {
    pub conversation: ConversationId,
    pub events: Vec<Event>,
}

/// Outcome of ingesting a batch of received events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Events newly appended.
    pub applied: usize,
    /// Events already present (content-addressed duplicates).
    pub duplicates: usize,
    /// Events rejected by validation, with the reason. (id, message)
    pub rejected: Vec<(EventId, String)>,
}

/// The data operations sync needs from an event store. Implemented by both the
/// in-memory [`EventLog`] and the durable `PersistentEventLog`.
pub trait SyncStore {
    /// All event ids this store holds for the conversation.
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId>;
    /// The conversation's events whose id is NOT in `have`, cloned, in
    /// topological (`(lamport, id)`) order — safe to ingest one-by-one.
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event>;
    /// Validate and store a received event (the store's normal append gate).
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError>;
}

impl SyncStore for EventLog {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.events(conversation).iter().map(|e| e.id).collect()
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        self.events(conversation)
            .into_iter()
            .filter(|e| !have.contains(&e.id))
            .cloned()
            .collect()
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::EventKind;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    /// Build a signed event in `conv()`.
    fn ev(
        id: &DeviceIdentity,
        seq: u64,
        parents: Vec<EventId>,
        lamport: u64,
        payload: &[u8],
    ) -> Event {
        Event::new(
            id,
            conv(),
            seq,
            parents,
            lamport,
            0,
            EventKind::Message,
            payload.to_vec(),
        )
    }

    #[test]
    fn store_reports_ids_and_excludes_known() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let a = ev(&id, 1, vec![], 1, b"a");
        let b = ev(&id, 2, vec![a.id], 2, b"b");
        log.append(a.clone()).unwrap();
        log.append(b.clone()).unwrap();

        let ids = log.event_ids(&conv());
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));

        // Excluding `a` leaves only `b`.
        let have: HashSet<EventId> = std::iter::once(a.id).collect();
        let missing = log.events_excluding(&conv(), &have);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].id, b.id);
    }

    #[test]
    fn messages_round_trip_through_bincode() {
        let id = DeviceIdentity::generate();
        let a = ev(&id, 1, vec![], 1, b"a");
        let req = SyncRequest {
            conversation: conv(),
            have: vec![a.id],
        };
        let resp = SyncResponse {
            conversation: conv(),
            events: vec![a.clone()],
            have: vec![a.id],
        };

        let req2: SyncRequest = bincode::deserialize(&bincode::serialize(&req).unwrap()).unwrap();
        let resp2: SyncResponse =
            bincode::deserialize(&bincode::serialize(&resp).unwrap()).unwrap();
        assert_eq!(req2.have, req.have);
        assert_eq!(resp2.events, resp.events);
    }
}
