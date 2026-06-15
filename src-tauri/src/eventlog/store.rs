//! In-memory, append-only event index for all conversations. Validates events
//! (integrity, signature, canonical form, causal completeness, equivocation)
//! and answers ordering/heads/version queries. Pure data structure — no I/O.

use crate::eventlog::event::{Author, ConversationId, Event, EventId};
use crate::eventlog::LogError;
use std::collections::{HashMap, HashSet};

/// Result of an append: a genuinely new event, or an already-present duplicate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppendOutcome {
    Appended,
    Duplicate,
}

/// The in-memory event log across all conversations.
#[derive(Default)]
pub struct EventLog {
    /// Every known event, by id.
    events: HashMap<EventId, Event>,
    /// Event ids grouped by conversation.
    by_conversation: HashMap<ConversationId, HashSet<EventId>>,
    /// Per-conversation frontier: events not (yet) referenced as a parent.
    heads: HashMap<ConversationId, HashSet<EventId>>,
    /// Per-conversation version vector: author → highest seq seen.
    version: HashMap<ConversationId, HashMap<Author, u64>>,
    /// Per-conversation highest Lamport timestamp seen.
    max_lamport: HashMap<ConversationId, u64>,
    /// (conversation, author, seq) → id, for equivocation detection.
    author_seq: HashMap<(ConversationId, Author, u64), EventId>,
}

impl EventLog {
    /// Validate an event for appending.
    ///
    /// Returns `Ok(true)` if it is new and valid (ready to index), `Ok(false)`
    /// if it is already present (a content-addressed duplicate — safe to skip),
    /// or `Err` if it is corrupt, unsigned, non-canonical, causally incomplete,
    /// or an equivocation. Pure — does not mutate the log.
    pub fn validate(&self, event: &Event) -> Result<bool, LogError> {
        if !event.verify_integrity() || !event.is_canonical() {
            return Err(LogError::CorruptId);
        }
        if !event.verify_signature() {
            return Err(LogError::BadSignature);
        }
        if self.events.contains_key(&event.id) {
            return Ok(false);
        }
        let conv = event.conversation_id;
        // Every parent must already be present, in the same conversation.
        let missing: Vec<EventId> = event
            .parents
            .iter()
            .filter(|p| self.events.get(*p).map(|e| e.conversation_id) != Some(conv))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(LogError::MissingParents(missing));
        }
        // The author must not already have a different event at this seq.
        if let Some(existing) = self.author_seq.get(&(conv, event.author, event.seq)) {
            if *existing != event.id {
                return Err(LogError::AuthorEquivocation {
                    author: event.author,
                    seq: event.seq,
                });
            }
        }
        Ok(true)
    }

    /// Insert an event that is already known to be valid (validated here, or
    /// authenticated at rest on reload). Must be called in causal order:
    /// every parent must already be indexed.
    pub(crate) fn index_trusted(&mut self, event: Event) {
        let conv = event.conversation_id;
        let id = event.id;

        let heads = self.heads.entry(conv).or_default();
        for p in &event.parents {
            heads.remove(p);
        }
        heads.insert(id);

        let vv = self.version.entry(conv).or_default();
        let slot = vv.entry(event.author).or_insert(0);
        if event.seq > *slot {
            *slot = event.seq;
        }

        let ml = self.max_lamport.entry(conv).or_insert(0);
        if event.lamport > *ml {
            *ml = event.lamport;
        }

        self.author_seq.insert((conv, event.author, event.seq), id);
        self.by_conversation.entry(conv).or_default().insert(id);
        self.events.insert(id, event);
    }

    /// Validate and append an event.
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.validate(&event)? {
            return Ok(AppendOutcome::Duplicate);
        }
        self.index_trusted(event);
        Ok(AppendOutcome::Appended)
    }

    pub fn has(&self, id: &EventId) -> bool {
        self.events.contains_key(id)
    }

    pub fn get(&self, id: &EventId) -> Option<&Event> {
        self.events.get(id)
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

    fn mk(
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
    fn appends_a_root_then_a_child() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();

        let root = mk(&id, 1, vec![], 1, b"root");
        assert_eq!(log.append(root.clone()).unwrap(), AppendOutcome::Appended);
        assert!(log.has(&root.id));

        let child = mk(&id, 2, vec![root.id], 2, b"child");
        assert_eq!(log.append(child.clone()).unwrap(), AppendOutcome::Appended);
        assert!(log.has(&child.id));
    }

    #[test]
    fn rejects_event_with_missing_parent() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let orphan = mk(&id, 1, vec![EventId::new([42u8; 32])], 1, b"orphan");
        assert!(matches!(
            log.append(orphan),
            Err(LogError::MissingParents(_))
        ));
    }

    #[test]
    fn duplicate_append_is_idempotent() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let e = mk(&id, 1, vec![], 1, b"hi");
        assert_eq!(log.append(e.clone()).unwrap(), AppendOutcome::Appended);
        assert_eq!(log.append(e.clone()).unwrap(), AppendOutcome::Duplicate);
        assert!(log.has(&e.id));
    }

    #[test]
    fn rejects_bad_signature() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let mut e = mk(&id, 1, vec![], 1, b"hi");
        e.sig[0] ^= 0xFF;
        assert!(matches!(log.append(e), Err(LogError::BadSignature)));
    }

    #[test]
    fn rejects_corrupt_id() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        let mut e = mk(&id, 1, vec![], 1, b"hi");
        e.ciphertext.push(0); // mutate content without recomputing id
        assert!(matches!(log.append(e), Err(LogError::CorruptId)));
    }

    #[test]
    fn rejects_author_equivocation() {
        let id = DeviceIdentity::generate();
        let mut log = EventLog::default();
        // Two different roots from the same author at the same seq.
        let a = mk(&id, 1, vec![], 1, b"one");
        let b = mk(&id, 1, vec![], 1, b"two");
        assert_eq!(log.append(a).unwrap(), AppendOutcome::Appended);
        assert!(matches!(
            log.append(b),
            Err(LogError::AuthorEquivocation { seq: 1, .. })
        ));
    }
}
