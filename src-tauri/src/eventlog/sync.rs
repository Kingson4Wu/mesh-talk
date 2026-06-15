//! Event-log reconciliation ("sync"). Two peers converge a conversation's log
//! by exchanging the set of event ids each holds; the responder returns exactly
//! the events the requester is missing, in topological order. Received events
//! are validated through [`SyncStore::ingest`] (the store's normal `append`
//! gate), so sync can never inject forged or out-of-order history.
//!
//! Reconciliation is id-set based, not version-vector based: the store does not
//! guarantee dense per-author sequences, so a version-vector diff would miss a
//! gap. See the module plan for the rationale.
//!
//! [`SyncStore::events_excluding`] returns the missing events in the store's
//! `(lamport, id)` order, which is topological for honestly-built events, so a
//! requester can ingest them one-by-one (each event's parents precede it or
//! already exist). [`SyncStore::ingest`] re-validates every event regardless,
//! so a peer that violates the Lamport invariant cannot corrupt the receiver —
//! at worst its events are rejected with `MissingParents`.

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
    /// The responder's OWN event ids (note: `SyncRequest::have` is the
    /// requester's). The requester uses these to compute the events the
    /// responder is missing, returned via [`SyncFollowup`].
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
    /// Events rejected by validation, with a human-readable reason. (id, message)
    /// Phase-0 keeps the reason as a `String` (diagnostics only); a structured
    /// `LogError` would be needed before sync branches on the failure kind.
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

// NOTE: this impl is structurally identical to `impl SyncStore for
// PersistentEventLog` in persist.rs — keep the two in sync.
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

/// Build the opening request: the ids this store holds for `conversation`.
pub fn build_request(store: &impl SyncStore, conversation: ConversationId) -> SyncRequest {
    SyncRequest {
        conversation,
        have: store.event_ids(&conversation),
    }
}

/// Answer a request: the events the requester is missing (topological order),
/// plus this store's own id-set so the requester can reciprocate.
pub fn handle_request(store: &impl SyncStore, request: &SyncRequest) -> SyncResponse {
    let have: HashSet<EventId> = request.have.iter().copied().collect();
    let events = store.events_excluding(&request.conversation, &have);
    let have = store.event_ids(&request.conversation);
    SyncResponse {
        conversation: request.conversation,
        events,
        have,
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

    #[test]
    fn events_excluding_preserves_topo_order_skipping_middle() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        let mid = ev(&id, 2, vec![root.id], 2, b"mid");
        let tip = ev(&id, 3, vec![mid.id], 3, b"tip");
        log.append(root.clone()).unwrap();
        log.append(mid.clone()).unwrap();
        log.append(tip.clone()).unwrap();

        // Exclude the MIDDLE event; the remainder must stay in topo order.
        let have: HashSet<EventId> = std::iter::once(mid.id).collect();
        let remaining = log.events_excluding(&conv(), &have);
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].id, root.id); // root before tip
        assert_eq!(remaining[1].id, tip.id);
    }

    #[test]
    fn event_ids_is_empty_for_unknown_conversation() {
        let log = EventLog::default();
        assert!(log.event_ids(&conv()).is_empty());
        let all: HashSet<EventId> = HashSet::new();
        assert!(log.events_excluding(&conv(), &all).is_empty());
    }

    #[test]
    fn followup_round_trips_through_bincode() {
        let id = DeviceIdentity::generate();
        let a = ev(&id, 1, vec![], 1, b"a");
        let followup = SyncFollowup {
            conversation: conv(),
            events: vec![a.clone()],
        };
        let back: SyncFollowup =
            bincode::deserialize(&bincode::serialize(&followup).unwrap()).unwrap();
        assert_eq!(back.conversation, followup.conversation);
        assert_eq!(back.events, followup.events);
    }

    #[test]
    fn request_lists_all_local_ids() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let a = ev(&id, 1, vec![], 1, b"a");
        let b = ev(&id, 2, vec![a.id], 2, b"b");
        log.append(a.clone()).unwrap();
        log.append(b.clone()).unwrap();

        let req = build_request(&log, conv());
        assert_eq!(req.conversation, conv());
        assert_eq!(req.have.len(), 2);
    }

    #[test]
    fn response_sends_exactly_the_requesters_missing_events() {
        let id = DeviceIdentity::generate();
        // Responder has root -> a -> b; requester has only root.
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![a.id], 3, b"b");
        responder.append(root.clone()).unwrap();
        responder.append(a.clone()).unwrap();
        responder.append(b.clone()).unwrap();

        let req = SyncRequest {
            conversation: conv(),
            have: vec![root.id],
        };
        let resp = handle_request(&responder, &req);

        // Missing = {a, b}, in topological order (a before b).
        assert_eq!(resp.events.len(), 2);
        assert_eq!(resp.events[0].id, a.id);
        assert_eq!(resp.events[1].id, b.id);
        // Responder advertises all three of its ids.
        assert_eq!(resp.have.len(), 3);
    }

    #[test]
    fn response_is_empty_when_requester_has_everything() {
        let id = DeviceIdentity::generate();
        let mut responder = EventLog::default();
        let root = ev(&id, 1, vec![], 1, b"root");
        responder.append(root.clone()).unwrap();

        let req = SyncRequest {
            conversation: conv(),
            have: vec![root.id],
        };
        let resp = handle_request(&responder, &req);
        assert!(resp.events.is_empty());
    }

    #[test]
    fn response_for_disjoint_branches_sends_the_responders_branch() {
        let id = DeviceIdentity::generate();
        // Shared root, then responder has branch `b`, requester has branch `a`.
        let root = ev(&id, 1, vec![], 1, b"root");
        let a = ev(&id, 2, vec![root.id], 2, b"a");
        let b = ev(&id, 3, vec![root.id], 2, b"b");

        let mut responder = EventLog::default();
        responder.append(root.clone()).unwrap();
        responder.append(b.clone()).unwrap();

        // Requester has root + a.
        let req = SyncRequest {
            conversation: conv(),
            have: vec![root.id, a.id],
        };
        let resp = handle_request(&responder, &req);
        // Responder only has root + b; requester is missing b (and has root already).
        assert_eq!(resp.events.len(), 1);
        assert_eq!(resp.events[0].id, b.id);
    }
}
