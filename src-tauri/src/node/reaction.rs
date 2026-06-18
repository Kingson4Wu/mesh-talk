//! Reaction model: the `React` event payload + aggregation. Pure — the `Node` seals
//! the payload with the conversation crypto and folds opened reactions on read.

use crate::eventlog::event::EventId;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The plaintext payload of a `React` event: which message, which emoji, and whether
/// this is an add or a remove (toggle off). Sealed like a message before it ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionPayload {
    pub target: EventId,
    pub emoji: String,
    pub remove: bool,
}

impl ReactionPayload {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("reaction payload serializes")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// Aggregated reactions for one `(target message, emoji)`: the set of author user-ids
/// that currently react. The UI derives count + whether the local user is among them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionView {
    pub target: String, // hex EventId
    pub emoji: String,
    pub who: Vec<String>, // author user-ids, sorted
}

/// Fold reactions IN EVENT ORDER into per-`(target, emoji)` author sets: a normal
/// reaction adds the author, `remove` removes them. Empty sets are dropped. Input is
/// `(author_user_id, payload)` pairs; output is sorted by (target, emoji).
pub fn aggregate(reactions: &[(String, ReactionPayload)]) -> Vec<ReactionView> {
    // key: (target hex, emoji) -> set of authors (BTreeMap<author,()> keeps it sorted)
    let mut sets: BTreeMap<(String, String), BTreeMap<String, ()>> = BTreeMap::new();
    for (author, p) in reactions {
        let key = (hex::encode(p.target.as_bytes()), p.emoji.clone());
        let entry = sets.entry(key).or_default();
        if p.remove {
            entry.remove(author);
        } else {
            entry.insert(author.clone(), ());
        }
    }
    sets.into_iter()
        .filter(|(_, who)| !who.is_empty())
        .map(|((target, emoji), who)| ReactionView {
            target,
            emoji,
            who: who.into_keys().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(n: u8) -> EventId {
        EventId::new([n; 32])
    }
    fn react(author: &str, t: u8, emoji: &str, remove: bool) -> (String, ReactionPayload) {
        (
            author.to_string(),
            ReactionPayload {
                target: target(t),
                emoji: emoji.into(),
                remove,
            },
        )
    }

    #[test]
    fn payload_round_trips_and_rejects_trailing() {
        let p = ReactionPayload {
            target: target(1),
            emoji: "👍".into(),
            remove: false,
        };
        assert_eq!(ReactionPayload::decode(&p.encode()), Some(p.clone()));
        let mut junk = p.encode();
        junk.push(0xAB);
        assert_eq!(ReactionPayload::decode(&junk), None);
    }

    #[test]
    fn aggregates_distinct_authors_and_emojis() {
        let views = aggregate(&[
            react("alice", 1, "👍", false),
            react("bob", 1, "👍", false),
            react("bob", 1, "🎉", false),
        ]);
        let thumbs = views.iter().find(|v| v.emoji == "👍").unwrap();
        assert_eq!(thumbs.who, vec!["alice".to_string(), "bob".to_string()]);
        assert!(views
            .iter()
            .any(|v| v.emoji == "🎉" && v.who == vec!["bob".to_string()]));
    }

    #[test]
    fn remove_toggles_a_reaction_off_and_drops_empty() {
        let views = aggregate(&[
            react("alice", 1, "👍", false),
            react("alice", 1, "👍", true), // alice un-reacts
        ]);
        assert!(views.is_empty());
    }

    #[test]
    fn remove_only_affects_that_author() {
        let views = aggregate(&[
            react("alice", 1, "👍", false),
            react("bob", 1, "👍", false),
            react("alice", 1, "👍", true),
        ]);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].who, vec!["bob".to_string()]);
    }
}
