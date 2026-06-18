# Reactions: Model + Node Implementation Plan (Reactions Plan 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add message reactions to the `Node`: a `React` event model + aggregation, `react_dm`/`react_channel`, `reactions(conv)`, and an `id` on history entries — validated by an in-process two-node rig where one node reacts and the other sees it.

**Architecture:** A `React` event carries a sealed `ReactionPayload { target, emoji, remove }` in the target's conversation. React events sync through the normal store gate (no serve-loop/stream/constructor changes). `reactions(conv)` opens the conversation's React events (DM sealed-box via the author's roster key; channel group key) and folds them into per-`(target, emoji)` author sets. Own DM reactions are sealed to the peer (un-openable locally), so they're additionally recorded in an in-memory list and merged.

**Tech Stack:** Rust; `dm::{seal,open}`, `channel::ChannelState::{seal_message,open_message}`, `bincode` (fixint, fail-closed), the node's distribution helpers. No new dependencies.

---

## Background the implementer needs

Spec: `docs/superpowers/specs/2026-06-18-reactions-design.md`. `EventKind::React = 3` already exists. The `Node` (relevant parts):
```rust
// Event { id: EventId, author: Author, seq: u64, kind: EventKind, ciphertext: Vec<u8>, wall_clock: u64, .. }
// HistoryEntry { from_me: bool, who: String, text: Vec<u8>, wall_clock: u64 }   // <- gains `id: EventId`
// Node fields incl: log: Mutex<PersistentEventLog>, roster, channels: Mutex<ChannelBook>, sentlog: Mutex<SentLog>
// dm_conversation_id(&PublicIdentity,&PublicIdentity)->ConversationId; Author::from_ed25519(pub.ed25519_pub)
// append_event(&self, conv, kind: EventKind, ciphertext: Vec<u8>) -> Result<(), NodeError>  // generic appender
// deliver_direct(&self, &PeerRecord, conv)->Result<(),SessionError>; replicate_to_post_office(&self, conv)->Result<bool,SessionError>
// distribute_channel(&self, channel, members: &[PublicIdentity])  // dial members + PO
// channels: ChannelBook::state(&conv)->Option<&ChannelState>; ChannelState::{seal_message,open_message,members}
// dm::seal(&DeviceIdentity, recipient_x25519:&[u8;32], &[u8])->Result<Vec<u8>,DmError>; dm::open(&DeviceIdentity, sender_x25519:&[u8;32], &[u8])->Result<Vec<u8>,DmError>
// history(&self, conv, limit) -> Vec<HistoryEntry>   // DM: received from log (has event.id) + sent from sentlog (has seq, NOT id)
// channel_history(&self, channel, limit) -> Vec<HistoryEntry>  // all from log events (have event.id)
// SentEntry { seq: u64, wall_clock: u64, plaintext: Vec<u8> }
// NodeError { ..., Channel(String) }; map reaction seal/open failures to a clear variant (reuse Channel or add Reaction)
```
`EventId`: `[u8;32]` newtype, `new([u8;32])`, `as_bytes()`, `Serialize`/`Deserialize`/`Copy`/`Eq`/`Hash`. `ConversationId` likewise.

**CPU/test discipline:** `nice -n 10`, one filter per run. `cargo test --lib node::reaction` / `node::node`; `cargo build --lib --bins`; `cargo clippy --lib --bins -- -D warnings`; `cargo fmt`. Confirm `git status | head -1` == `On branch feat/redesign-phase0` after committing.

---

### Task 1: `node::reaction` — payload + aggregation (pure)

**Files:** Create `src-tauri/src/node/reaction.rs`; modify `src-tauri/src/node/mod.rs`.

- [ ] **Step 1: write `node/reaction.rs`**
```rust
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
        (author.to_string(), ReactionPayload { target: target(t), emoji: emoji.into(), remove })
    }

    #[test]
    fn payload_round_trips_and_rejects_trailing() {
        let p = ReactionPayload { target: target(1), emoji: "👍".into(), remove: false };
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
        assert!(views.iter().any(|v| v.emoji == "🎉" && v.who == vec!["bob".to_string()]));
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
```
(`hex` is already a dependency. If `EventId` isn't `Hash`, the `BTreeMap` keyed by the hex string sidesteps it.)

- [ ] **Step 2: register the module (`node/mod.rs`)**

Add `pub mod reaction;` with the other `pub mod`s and re-export:
```rust
pub use reaction::{aggregate, ReactionPayload, ReactionView};
```
(Match the existing re-export grouping.)

- [ ] **Step 3: run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::reaction -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/reaction.rs src-tauri/src/node/mod.rs
git commit -m "feat(node): reaction payload model + aggregation"
git status | head -1
```

---

### Task 2: Node — react_dm/react_channel/reactions + HistoryEntry.id + loopback rig

**Files:** Modify `src-tauri/src/node/node.rs`. (Check `src-tauri/tests/persistent_history.rs` for `HistoryEntry` construction/asserts — update if it breaks.)

- [ ] **Step 1: imports + the `id` field + the own-DM-reactions record**

Add imports: `use crate::node::reaction::{aggregate, ReactionPayload, ReactionView};`.
Add `id: EventId` to `HistoryEntry`:
```rust
pub struct HistoryEntry {
    pub id: EventId,
    pub from_me: bool,
    pub who: String,
    pub text: Vec<u8>,
    pub wall_clock: u64,
}
```
Add a `Node` field for own DM reactions (sealed to the peer, so un-openable from our own log):
```rust
    my_dm_reactions: Mutex<Vec<(ConversationId, ReactionPayload)>>,
```
Initialize it in `Node::open`'s struct literal: `my_dm_reactions: Mutex::new(Vec::new()),`. (No constructor-signature change.)

- [ ] **Step 2: attach `id` in `history` and `channel_history`**

In `history` (DM): received entries use `id: event.id`. For sent entries, build a `seq -> EventId` map from this node's authored `Message` events in the snapshot, and look it up:
```rust
        // Map our own Message events' seq -> id, to give sent sidecar entries an id.
        let mut my_msg_ids: std::collections::HashMap<u64, EventId> = std::collections::HashMap::new();
        for event in &events {
            if event.kind == EventKind::Message && event.author == self_author {
                my_msg_ids.insert(event.seq, event.id);
            }
        }
```
Received-entry push gains `id: event.id`. Sent-entry push gains `id: my_msg_ids.get(&sent.seq).copied().unwrap_or(EventId::new([0u8; 32]))` (a sent message is always in our log, so the map hits; the zero fallback is only a defensive default).
In `channel_history`: each entry gains `id: event.id` (all entries are log events there).

- [ ] **Step 3: `react_dm`, `react_channel`, `reactions`**
```rust
    /// React to a message in a DM (toggle off with `remove = true`).
    pub async fn react_dm(
        &self,
        recipient: &str,
        target: EventId,
        emoji: &str,
        remove: bool,
    ) -> Result<(), NodeError> {
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;
        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let payload = ReactionPayload { target, emoji: emoji.to_string(), remove };
        let sealed = crate::dm::seal(&self.identity, &peer.public.x25519_pub, &payload.encode())
            .map_err(NodeError::Seal)?;
        self.append_event(conv, EventKind::React, sealed)?;
        // Our own DM reaction is sealed to the peer; record it so `reactions` can
        // include it (we can't open it from our own log).
        self.my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned")
            .push((conv, payload));
        self.deliver_direct(&peer, conv).await.ok();
        self.replicate_to_post_office(conv).await.ok();
        Ok(())
    }

    /// React to a message in a channel we hold the key for.
    pub async fn react_channel(
        &self,
        channel: ConversationId,
        target: EventId,
        emoji: &str,
        remove: bool,
    ) -> Result<(), NodeError> {
        let payload = ReactionPayload { target, emoji: emoji.to_string(), remove };
        let (sealed, members) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            let sealed = state
                .seal_message(&payload.encode())
                .map_err(|e| NodeError::Channel(format!("reaction seal failed: {e}")))?;
            (sealed, state.members().to_vec())
        };
        self.append_event(channel, EventKind::React, sealed)?;
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Aggregated reactions for a conversation (DM or channel). Opens each `React`
    /// event with the conversation crypto, merges our own (un-openable) DM reactions,
    /// and folds them per `(target, emoji)`.
    pub fn reactions(&self, conv: ConversationId) -> Vec<ReactionView> {
        let self_uid = self.identity.public().user_id();
        let react_events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| e.kind == EventKind::React)
                .cloned()
                .collect()
        };
        let is_channel = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&conv).is_some()
        };
        let mut decoded: Vec<(String, ReactionPayload)> = Vec::new();
        for event in &react_events {
            let plaintext = if is_channel {
                let book = self.channels.lock().expect("channels mutex not poisoned");
                book.state(&conv).and_then(|s| s.open_message(&event.ciphertext))
            } else {
                let author_uid = event.author.user_id();
                let sender_x25519 = {
                    let roster = self.roster.lock().expect("roster mutex not poisoned");
                    roster.get(&author_uid).map(|p| p.public.x25519_pub)
                };
                sender_x25519
                    .and_then(|x| crate::dm::open(&self.identity, &x, &event.ciphertext).ok())
            };
            if let Some(p) = plaintext.and_then(|b| ReactionPayload::decode(&b)) {
                decoded.push((event.author.user_id(), p));
            }
        }
        // Merge our own DM reactions for this conversation (not in the openable log).
        if !is_channel {
            for (c, p) in self
                .my_dm_reactions
                .lock()
                .expect("my_dm_reactions mutex not poisoned")
                .iter()
            {
                if *c == conv {
                    decoded.push((self_uid.clone(), p.clone()));
                }
            }
        }
        aggregate(&decoded)
    }
```

- [ ] **Step 4: fix any broken `HistoryEntry` constructions**

`grep -rn "HistoryEntry {" src-tauri/src src-tauri/tests` — add `id: ...` to every literal construction (the node's `history`/`channel_history` are covered above; check `tests/persistent_history.rs` — if it constructs `HistoryEntry` literally, add an `id` field; if it only reads fields, no change). Any test asserting exact `HistoryEntry` equality needs the `id` too.

- [ ] **Step 5: loopback rig — a reaction crosses the wire**
```rust
    #[tokio::test]
    async fn a_dm_reaction_is_visible_to_both_peers() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
        let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alice_addr = alice_listener.local_addr().unwrap();
        let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob.public().user_id());

        let (a_dm, _a1) = mpsc::unbounded_channel();
        let (a_ch, _a2) = mpsc::unbounded_channel();
        let (a_f, _a3) = mpsc::unbounded_channel();
        let (b_dm, mut b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, _b2) = mpsc::unbounded_channel();
        let (b_f, _b3) = mpsc::unbounded_channel();

        let alice_node = Node::open(alice, alice_roster, a_dm, a_ch, a_f,
            &dir.path().join("a.log"), &dir.path().join("a-sent.log"), "pw").unwrap();
        let bob_node = Node::open(bob, bob_roster, b_dm, b_ch, b_f,
            &dir.path().join("b.log"), &dir.path().join("b-sent.log"), "pw").unwrap();
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));
        tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));

        // Alice DMs Bob; Bob receives it and learns its event id from history.
        let bob_uid = bob_node.user_id();
        let alice_uid = alice_node.user_id();
        alice_node.send_dm(&bob_uid, b"hi bob").await.unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), b_dm_r.recv())
            .await.expect("bob got the dm").expect("stream open");
        assert_eq!(got.text, b"hi bob");

        let target = {
            let h = bob_node.dm_history(&alice_node.handle_public(), 10); // see note
            h.iter().find(|e| !e.from_me).map(|e| e.id).expect("bob has the message id")
        };
        // Bob reacts; it distributes to Alice.
        bob_node.react_dm(&alice_uid, target, "👍", false).await.unwrap();

        // Both see the reaction.
        let bob_views = bob_node.reactions_for_dm(&alice_node.handle_public()); // see note
        assert!(bob_views.iter().any(|v| v.emoji == "👍" && v.who.contains(&bob_uid)));
        // Give the distribution a moment, then check Alice.
        let mut ok = false;
        for _ in 0..50 {
            let av = alice_node.reactions_for_dm(&bob_node.handle_public());
            if av.iter().any(|v| v.emoji == "👍" && v.who.contains(&bob_uid)) { ok = true; break; }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(ok, "alice sees bob's reaction");
    }
```
**IMPORTANT for the rig:** the test needs the DM conversation id to call `reactions(conv)` and `dm_history`. There is no `handle_public()`/`reactions_for_dm` helper — either (a) derive the conv with the existing `dm_conversation_id(&a_pub, &b_pub)` using the public identities the test already has (capture `alice.public()`/`bob.public()` BEFORE moving them into `Node::open`), and call `node.reactions(conv)` + `node.dm_history(&peer_public, 10)` directly; or (b) add tiny test-only helpers. Prefer (a): capture `let alice_pub = alice.public(); let bob_pub = bob.public();` before `Node::open`, compute `let conv = dm_conversation_id(&alice_pub, &bob_pub);`, and use `bob_node.dm_history(&alice_pub, 10)` / `alice_node.reactions(conv)` / `bob_node.reactions(conv)`. Adjust the pseudo-helpers above to the real `reactions(conv)` + `dm_history(&PublicIdentity, usize)` API.

- [ ] **Step 6: build, test, clippy, fmt, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::reaction -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib --bins && nice -n 10 cargo clippy --lib --bins -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo test --test persistent_history -- --test-threads=2   # if it touches HistoryEntry
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add -A
git commit -m "feat(node): react_dm/react_channel + reactions aggregation; HistoryEntry.id"
git status | head -1
```

---

## Notes for the reviewer / next plan

- **Delivered:** reactions in the node — React events (sealed like messages), `reactions(conv)` aggregation (DM + channel), `HistoryEntry.id` (sent-DM id recovered via seq), and own-DM-reaction merge. React events sync through the normal store gate, so no serve-loop/stream/constructor changes. The rig proves a reaction crosses the wire both ways.
- **Reviewer checks:** no `MutexGuard` across `.await` in `react_dm`/`react_channel` (snapshot peer/members/sealed before await); the DM-vs-channel open branch in `reactions`; own-DM reactions merged exactly once (channel own-reactions come from the log, NOT the memory list); the `seq→id` map always hits for sent messages; aggregation order/remove semantics.
- **Known MVP limitation (note in code):** own DM reactions live in memory → lost on restart (channel reactions + others' DM reactions persist in the log). A durable own-reaction sidecar is deferred.
- **Deferred to Reactions Plan 2 (app+UI):** IPC `redesign_react_dm`/`redesign_react_channel`/`redesign_reactions`; `HistoryItem.id`; a react button + reaction chips in `RedesignChatView.vue` (reload on send + inbound message).
