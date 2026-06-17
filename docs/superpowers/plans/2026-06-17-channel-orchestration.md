# Channel Orchestration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the channel crypto + model into the event-log: encode the rotating group key as `KeyRotation` events sealed per-member, and a `ChannelBook` that replays a channel's events (`MembershipChange`/`KeyRotation`/`Message`) into per-channel state and decrypted messages — proven by an in-process multi-member exchange with key rotation on add.

**Architecture:** A new `node::channel` module. `SealedKeys` is the `KeyRotation` event payload (the group key sealed to each member's X25519 via `channel::seal_group_key`). Pure builders produce the event payloads (`MembershipChange` = `ChannelMeta::encode()`, `KeyRotation` = `SealedKeys::encode()`, `Message` = `ChannelState::seal_message`). `ChannelBook` holds `HashMap<ConversationId, ChannelState>` and `process(identity, channel_id, events)` replays events in causal order — creating/updating state from `MembershipChange`, opening this node's sealed key from `KeyRotation` (using the author's X25519 from the membership), decrypting `Message`s — returning the decrypted channel messages. No live network: it operates on `Event`s, so it's tested over the existing in-process `reconcile`.

**Tech Stack:** Rust; the Plan-1/2 `channel` module, `eventlog` (`Event`/`EventKind`/`reconcile`), `identity`; `bincode`/`serde`. No new dependencies.

---

## Background the implementer needs

**This is Plan 3 (of 4) of the "channels/groups" slice** (spec: `docs/superpowers/specs/2026-06-17-channels-design.md` §3–§4). Plans 1–2 shipped `channel::{crypto, model}`. This plan adds the node-side orchestration (the data/logic layer). Plan 4 wires it into the live `Node` networking + the IPC commands + the channels UI.

**Key design (the rotation distribution):**
- A channel's group key for an epoch is distributed by a **`KeyRotation` event** whose `ciphertext` is `SealedKeys { epoch, entries: Vec<(user_id, sealed_key_bytes)> }`, where each `sealed_key_bytes` = `channel::seal_group_key(author, member.x25519, &key)`. The event is synced to everyone + the post office, but each entry is sealed to a member's X25519 — only that member opens its own; the post office never can.
- A **`MembershipChange` event**'s `ciphertext` is `ChannelMeta::encode()` (the plaintext membership snapshot). It is authored+signed; the post office may read membership (accepted metadata). It is the causal **parent** of its `KeyRotation`, so a member processes membership (learning who's in + their X25519) before opening the key.
- A **`Message` event**'s `ciphertext` is `ChannelState::seal_message(text)` (epoch-framed AEAD).
- To open a `KeyRotation` authored by X, the receiver needs X's X25519 — which it gets from the channel's **membership** (`ChannelMeta.members` carries each member's full `PublicIdentity`), since `Event.author` only carries the Ed25519 fingerprint.

**Verified APIs:**
```rust
// channel (Plans 1-2)
crate::channel::{GroupKey, ChannelError, ChannelMeta, ChannelState, new_channel_id, seal_group_key, open_group_key};
// GroupKey: Clone; ChannelMeta { name, members: Vec<PublicIdentity>, epoch }; ::encode()/decode();
// ChannelState::{from_meta(ConversationId, ChannelMeta), apply_meta, record_key(u64,GroupKey), key_for, id, name, epoch, members()->&[PublicIdentity], is_member, seal_message(&[u8])->Result<Vec<u8>,ChannelError>, open_message(&[u8])->Option<Vec<u8>>}
// eventlog
crate::eventlog::event::{Event, EventId, EventKind, ConversationId, Author};
// Event { pub id, pub conversation_id: ConversationId, pub author: Author, pub seq, pub parents: Vec<EventId>, pub lamport, pub wall_clock, pub kind: EventKind, pub ciphertext: Vec<u8>, pub sig }
// EventKind::{Message, MembershipChange, KeyRotation, ...}; Author::user_id()->String; Author::from_ed25519([u8;32])
// Event::new(&DeviceIdentity, ConversationId, seq, Vec<EventId> parents, lamport, wall_clock, EventKind, Vec<u8> ciphertext) -> Event
crate::eventlog::store::EventLog; // Default; append(Event)->Result<AppendOutcome,LogError>; events(&ConversationId)->Vec<&Event>; prepare(&ConversationId)->(Vec<EventId>,u64); version_vector(&ConversationId)->HashMap<Author,u64>
crate::eventlog::sync::reconcile; // reconcile(&mut A, &mut B, ConversationId) for the in-process test
// identity
crate::identity::device::{DeviceIdentity, PublicIdentity}; // PublicIdentity { ed25519_pub, x25519_pub }; user_id(); DeviceIdentity::{generate, public}
```

**CPU/test discipline (MANDATORY):** never run a bare `cargo test`/`build`. Always:
```bash
cd src-tauri && nice -n 10 cargo test --lib node::channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
Confirm `git status | head -1` is `On branch feat/redesign-phase0` after committing.

**Conventions:** fail-closed bincode decode; hand-written error enum; `#[cfg(test)] mod tests`.

---

### Task 1: `SealedKeys` codec + key-sealing helpers

**Files:**
- Create: `src-tauri/src/node/channel.rs`
- Modify: `src-tauri/src/node/mod.rs` (register module)

- [ ] **Step 1: Register the module in `src-tauri/src/node/mod.rs`**

Add `pub mod channel;` (rustfmt sorts; order becomes `channel, conversation, net, node, postbox, runtime, sentlog, session, transport`). Keep the existing `pub use` lines.

- [ ] **Step 2: Create `src-tauri/src/node/channel.rs` with this content (Task 2 appends the `ChannelBook` + tests):**
```rust
//! Channel orchestration over the event log: the per-member sealed group-key
//! payload carried by `KeyRotation` events, the payload builders for the three
//! channel event kinds, and a [`ChannelBook`] that replays a channel's events into
//! per-channel state + decrypted messages. Operates on `Event`s — no live network.

use crate::channel::{
    open_group_key, seal_group_key, ChannelMeta, ChannelState, GroupKey,
};
use crate::eventlog::event::{Event, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub channel_id: crate::eventlog::event::ConversationId,
    pub channel_name: String,
    pub from: String,
    pub text: Vec<u8>,
}
```

- [ ] **Step 3: Add the codec tests (append inside `channel.rs`, in a `#[cfg(test)] mod tests` block)**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::GroupKey;

    #[test]
    fn sealed_keys_round_trip_and_reject_trailing() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let key = GroupKey::generate();
        let sealed =
            seal_keys_for(&alice, &[alice.public(), bob.public()], &key, 0).unwrap();

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
        let sealed =
            seal_keys_for(&alice, &[alice.public(), bob.public()], &key, 0).unwrap();

        // Bob opens his entry using Alice's (the author's) X25519.
        let opened = open_my_key(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened.as_bytes(), key.as_bytes());
    }

    #[test]
    fn a_non_member_has_no_entry_to_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let key = GroupKey::generate();
        // Sealed only to Alice + Bob; Carol has no entry.
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &key, 0).unwrap();
        assert!(open_my_key(&carol, &alice.public().x25519_pub, &sealed).is_none());
    }
}
```

- [ ] **Step 4: Run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/mod.rs src-tauri/src/node/channel.rs
git commit -m "feat(node): channel KeyRotation sealed-keys codec + per-member sealing"
git status | head -1
```
Expected: 3 tests pass; clippy clean. (Note: `open_my_key`/`ReceivedChannelMessage` are used by Task 2 — if clippy `-D warnings` flags them as unused in THIS commit, add `#[allow(dead_code)]` with a `// used by ChannelBook in the next task` comment, to be removed in Task 2. Prefer keeping them.)

---

### Task 2: `ChannelBook` — replay events into state + decrypted messages

**Files:**
- Modify: `src-tauri/src/node/channel.rs` (add `ChannelBook` + the multi-member rig)

- [ ] **Step 1: Add `ChannelBook` (insert ABOVE the `#[cfg(test)] mod tests` block)**
```rust
/// A node's view of all the channels it knows: each channel's [`ChannelState`]
/// keyed by channel id. [`process`] replays a channel's events (in the causal order
/// the log returns them) to update membership/keys and surface decrypted messages.
#[derive(Default)]
pub struct ChannelBook {
    states: HashMap<crate::eventlog::event::ConversationId, ChannelState>,
}

impl ChannelBook {
    pub fn new() -> Self {
        ChannelBook::default()
    }

    /// The state for a known channel, if any.
    pub fn state(
        &self,
        channel_id: &crate::eventlog::event::ConversationId,
    ) -> Option<&ChannelState> {
        self.states.get(channel_id)
    }

    /// Replay `events` (a channel's events, in log/causal order) for the node
    /// `me`. Updates membership + keys and returns the channel `Message`s that are
    /// newly decryptable and authored by someone other than `me`. Events we can't
    /// act on (a key we don't have, a membership we're not in) are skipped.
    pub fn process(
        &mut self,
        me: &DeviceIdentity,
        channel_id: crate::eventlog::event::ConversationId,
        events: &[&Event],
    ) -> Vec<ReceivedChannelMessage> {
        let my_author = crate::eventlog::event::Author::from_ed25519(me.public().ed25519_pub);
        let mut out = Vec::new();

        for event in events {
            match event.kind {
                EventKind::MembershipChange => {
                    if let Some(meta) = ChannelMeta::decode(&event.ciphertext) {
                        // Only track a channel we are a member of.
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
                        continue; // no membership context yet
                    };
                    // The rotation author's X25519 comes from the membership.
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
                    if event.author == my_author {
                        continue; // our own message
                    }
                    let Some(state) = self.states.get(&channel_id) else {
                        continue;
                    };
                    if let Some(text) = state.open_message(&event.ciphertext) {
                        out.push(ReceivedChannelMessage {
                            channel_id,
                            channel_name: state.name().to_string(),
                            from: event.author.user_id(),
                            text,
                        });
                    }
                }
                _ => {}
            }
        }
        out
    }
}
```

- [ ] **Step 2: Add the multi-member rig (append inside the `mod tests` block)**

This builds real channel events on Alice's `EventLog`, reconciles them into Bob's and Carol's logs, and asserts the full create → message → add-member → rotation flow. Add the imports at the top of `mod tests`:
```rust
    use crate::channel::{new_channel_id, ChannelMeta, ChannelState};
    use crate::eventlog::event::{Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::eventlog::sync::reconcile;
```
Then the test:
```rust
    // Append a channel event to `log`, deriving seq/parents/lamport from the log.
    fn append(
        log: &mut EventLog,
        author: &DeviceIdentity,
        channel: crate::eventlog::event::ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) {
        let (parents, lamport) = log.prepare(&channel);
        let self_author = crate::eventlog::event::Author::from_ed25519(author.public().ed25519_pub);
        let seq = log
            .version_vector(&channel)
            .get(&self_author)
            .copied()
            .unwrap_or(0)
            + 1;
        let event = Event::new(author, channel, seq, parents, lamport, 0, kind, ciphertext);
        log.append(event).unwrap();
    }

    fn collect<'a>(log: &'a EventLog, channel: &crate::eventlog::event::ConversationId) -> Vec<&'a Event> {
        log.events(channel)
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
        let meta0 = ChannelMeta { name: "general".into(), members: vec![alice.public(), bob.public()], epoch: 0 };
        append(&mut alice_log, &alice, channel, EventKind::MembershipChange, meta0.encode());
        let sealed0 = seal_keys_for(&alice, &[alice.public(), bob.public()], &key0, 0).unwrap();
        append(&mut alice_log, &alice, channel, EventKind::KeyRotation, sealed0.encode());

        // Alice's own state (records her epoch-0 key) and sends a message.
        let mut alice_state = ChannelState::from_meta(channel, meta0.clone());
        alice_state.record_key(0, key0.clone());
        append(&mut alice_log, &alice, channel, EventKind::Message, alice_state.seal_message(b"hello team").unwrap());

        // Bob reconciles and processes → learns the channel, opens his key, reads the message.
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut alice_log, channel);
        let mut bob_book = ChannelBook::new();
        let got = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, b"hello team");
        assert_eq!(got[0].from, alice.public().user_id());
        assert_eq!(bob_book.state(&channel).unwrap().epoch(), 0);

        // Alice adds Carol → epoch 1: new membership + new sealed keys to all three.
        let key1 = GroupKey::generate();
        let meta1 = ChannelMeta { name: "general".into(), members: vec![alice.public(), bob.public(), carol.public()], epoch: 1 };
        append(&mut alice_log, &alice, channel, EventKind::MembershipChange, meta1.encode());
        let sealed1 = seal_keys_for(&alice, &meta1.members, &key1, 1).unwrap();
        append(&mut alice_log, &alice, channel, EventKind::KeyRotation, sealed1.encode());
        alice_state.apply_meta(meta1.clone());
        alice_state.record_key(1, key1.clone());
        append(&mut alice_log, &alice, channel, EventKind::Message, alice_state.seal_message(b"welcome carol").unwrap());

        // Carol reconciles fresh and processes ALL events: she gets epoch-1 key only,
        // so she reads "welcome carol" but NOT the epoch-0 "hello team".
        let mut carol_log = EventLog::default();
        reconcile(&mut carol_log, &mut alice_log, channel);
        let mut carol_book = ChannelBook::new();
        let carol_got = carol_book.process(&carol, channel, &collect(&carol_log, &channel));
        assert_eq!(carol_got.len(), 1);
        assert_eq!(carol_got[0].text, b"welcome carol");
        assert_eq!(carol_book.state(&channel).unwrap().epoch(), 1);

        // Bob reconciles the new events and reads the epoch-1 message (he's still a member).
        reconcile(&mut bob_log, &mut alice_log, channel);
        let bob_got2 = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        // Bob already saw "hello team"; the new decryptable non-self message is "welcome carol".
        assert!(bob_got2.iter().any(|m| m.text == b"welcome carol"));
        assert_eq!(bob_book.state(&channel).unwrap().epoch(), 1);
    }
```

NOTE on the re-process: `process` returns ALL decryptable non-self messages each call (it does not dedup across calls — that's the caller's job, mirroring how the DM node uses an `emitted` set). The Bob re-process asserts the new message is present (`any(...)`), which is correct regardless of whether "hello team" reappears.

- [ ] **Step 3: Remove any `#[allow(dead_code)]` added in Task 1**

`open_my_key` and `ReceivedChannelMessage` are now used by `ChannelBook`. Delete any `#[allow(dead_code)]` attributes added in Task 1.

- [ ] **Step 4: Run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/channel.rs
git commit -m "feat(node): ChannelBook replays channel events; multi-member rotation rig"
git status | head -1
```
Expected: PASS — the 3 codec tests + the multi-member rig.

---

## Notes for the reviewer / next plan

- **What this delivers:** the channel node-orchestration logic — the `KeyRotation` sealed-keys codec (`SealedKeys`, sealed per member via `channel::seal_group_key`), and `ChannelBook` which replays a channel's `MembershipChange`/`KeyRotation`/`Message` events into per-channel `ChannelState` + decrypted `ReceivedChannelMessage`s. The multi-member rig proves create → message → add-member → key-rotation end to end over the in-process `reconcile`, including that a member added at epoch 1 cannot read epoch-0 messages.
- **Why no live network here:** `ChannelBook::process` consumes `Event`s, so it is exercised over the existing in-process `reconcile` — the same way the eventlog/postoffice logic is tested. This keeps the orchestration reliable and decoupled.
- **Deferred to Plan 4 (live node + app):** holding a `ChannelBook` in the live `Node`, building the channel events with `log.prepare` and syncing them to members + the post office (create/send dial members and replicate; the drain includes known channel ids), surfacing `ReceivedChannelMessage` on a stream, and the IPC commands (`redesign_create_channel`/`list_channels`/`send_channel_message`/`channel_history`/`add_member`) + a channels section in the `/redesign` Vue route + a `redesign-channel-message` event.
- **Reviewer checks:** confirm `Event.author`/`kind`/`ciphertext` are `pub`; `Author::from_ed25519`/`user_id`; that processing `MembershipChange` before `KeyRotation` holds because the KeyRotation is appended after (causal parent) the MembershipChange, so `log.events(channel)` returns them in that order; and that `process` returning non-deduped messages is acceptable (the live Node will dedup via an emitted set, like DMs).
