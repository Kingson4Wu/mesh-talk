# Live Channels in the Node Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire channels into the live `Node`: hold a `ChannelBook`, create channels + send channel messages + distribute the events over the network/post-office, and surface received channel messages on a stream — validated by an in-process two-node channel exchange over loopback TCP.

**Architecture:** The `Node` gains a `Mutex<ChannelBook>` and a `mpsc<ReceivedChannelMessage>` stream. `Node::open` seeds the book from the durable log (so channels survive restart). `process_channel(conv)` runs the book over a conversation's events and streams new messages (mirroring `emit_new_messages` for DMs); it is invoked wherever events arrive (serve loop, drain). `create_channel`/`send_channel_message` build channel events and `distribute` them by dialing each known member + replicating to the elected post office. `channel_history` decrypts the channel log with the group key (the sender holds it, so unlike DMs no sidecar is needed).

**Tech Stack:** Rust; the `channel::{crypto,model}` + `node::channel` (`ChannelBook`, `SealedKeys`, `ReceivedChannelMessage`, `seal_keys_for`), `eventlog`, `node` (`request_round`, `dial`, `elected_post_office`); `tokio`. No new dependencies.

---

## Background the implementer needs

**This is Plan 4a (of the channels slice)** (spec: `docs/superpowers/specs/2026-06-17-channels-design.md` §4). Plans 1–3 shipped `channel::{crypto,model}` and `node::channel` (`ChannelBook::process` replays a channel's events into state + decrypted `ReceivedChannelMessage`s; `seal_keys_for` builds the `KeyRotation` payload). This plan wires it into the live networked `Node`. Plan 4b adds the IPC commands + the Vue channels UI.

**Current `Node` (verbatim shape):**
```rust
pub struct Node {
    identity: DeviceIdentity,
    log: Mutex<PersistentEventLog>,
    sentlog: Mutex<SentLog>,
    roster: Arc<Mutex<Roster>>,
    incoming: mpsc::UnboundedSender<ReceivedDm>,
    emitted: Mutex<HashSet<EventId>>,
}
impl Node {
    pub fn open(identity, roster: Arc<Mutex<Roster>>, incoming: mpsc::UnboundedSender<ReceivedDm>, log_path: &Path, sent_path: &Path, password: &str) -> Result<Arc<Self>, LogError>;
    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError>;
    async fn deliver_direct(&self, peer: &PeerRecord, conv: ConversationId) -> Result<(), SessionError>; // dial+request_round
    async fn replicate_to_post_office(&self, conv: ConversationId) -> Result<bool, SessionError>;
    pub async fn run_accept_loop(self: Arc<Self>, listener: TcpListener);
    pub async fn serve_connection(&self, channel: SecureChannel<TcpStream>); // while Served::Handled(conv) -> emit_new_messages(conv)
    pub async fn drain_from_post_office(&self); // per non-PO peer: request_round(dm conv) + emit_new_messages
    pub fn dm_history(&self, peer: &PublicIdentity, limit) -> Vec<HistoryEntry>;
    fn emit_new_messages(&self, conv: ConversationId); // collect non-self un-emitted Message events under log+emitted lock, mark emitted, decrypt via open_dm_event, send ReceivedDm on `incoming`
}
```
Callers of `Node::open`: `RedesignRuntime::start` (`src-tauri/src/node/runtime.rs`), the binary `mesh-talk-node.rs` (normal-node `main`), and the in-crate `#[cfg(test)] mod tests` in `node.rs`.

**Verified channel APIs (Plans 1-3):**
```rust
crate::node::channel::{ChannelBook, SealedKeys, ReceivedChannelMessage, seal_keys_for};
// ChannelBook::{new, state(&ConversationId)->Option<&ChannelState>, process(&DeviceIdentity, ConversationId, &[&Event])->Vec<ReceivedChannelMessage>}
// ReceivedChannelMessage { channel_id: ConversationId, channel_name: String, from: String, text: Vec<u8> }
// seal_keys_for(&DeviceIdentity, &[PublicIdentity], &GroupKey, u64) -> Result<SealedKeys, ChannelError>
crate::channel::{GroupKey, ChannelMeta, ChannelState, new_channel_id};
// ChannelMeta { name, members: Vec<PublicIdentity>, epoch }; ::encode(); ChannelState::{from_meta, name, epoch, members, open_message, seal_message, key_for, record_key}
crate::eventlog::event::{Event, EventId, EventKind, ConversationId, Author}; // Event::new(...); EventKind::{Message, MembershipChange, KeyRotation}
crate::eventlog::store::EventLog; // events(&ConversationId)->Vec<&Event>; prepare; version_vector; append
crate::node::session::request_round; crate::node::transport::dial; crate::node::postbox::elected_post_office;
```

**CPU/test discipline (MANDATORY):** never run a bare `cargo test`/`build`. Always `nice` + scope:
```bash
cd src-tauri && nice -n 10 cargo test --lib node::channel node::node eventlog -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
Always confirm `git status | head -1` is `On branch feat/redesign-phase0` after committing.

---

### Task 1: ChannelBook internal dedup + `EventLog::conversations()`

**Files:**
- Modify: `src-tauri/src/node/channel.rs` (ChannelBook gains an `emitted` set)
- Modify: `src-tauri/src/eventlog/store.rs` (+ `conversations()`)
- Modify: `src-tauri/src/eventlog/persist.rs` (+ `conversations()` delegate)

- [ ] **Step 1: Make `ChannelBook` dedup `Message` emission internally**

In `src-tauri/src/node/channel.rs`, change `ChannelBook` to own an `emitted: HashSet<EventId>` and only surface a `Message` whose event id is not yet emitted (marking it). Replace the struct + the `Message` arm of `process`:
```rust
#[derive(Default)]
pub struct ChannelBook {
    states: HashMap<crate::eventlog::event::ConversationId, ChannelState>,
    emitted: std::collections::HashSet<crate::eventlog::event::EventId>,
}
```
And in `process`, the `EventKind::Message` arm becomes (note the `!self.emitted.contains` guard + `insert`):
```rust
                EventKind::Message => {
                    if event.author == my_author || self.emitted.contains(&event.id) {
                        continue; // our own message, or already surfaced
                    }
                    let Some(state) = self.states.get(&channel_id) else {
                        continue;
                    };
                    if let Some(text) = state.open_message(&event.ciphertext) {
                        self.emitted.insert(event.id);
                        out.push(ReceivedChannelMessage {
                            channel_id,
                            channel_name: state.name().to_string(),
                            from: event.author.user_id(),
                            text,
                        });
                    }
                }
```
(The `states.get`/`self.emitted` borrow: `process` already takes `&mut self`. The `Message` arm reads `self.states.get` immutably and then `self.emitted.insert` mutably — these are disjoint fields, so split borrows are fine; if the borrow checker complains, lift the decryption result into a local before the `self.emitted.insert`/`out.push`.) The existing Plan-3 rig still passes: Bob's re-process now skips the already-emitted "hello team" and only surfaces "welcome carol" — the test's `any(... == "welcome carol")` assertion holds.

- [ ] **Step 2: Add a dedup test to `node::channel`**

Append inside the `#[cfg(test)] mod tests` block in `channel.rs`:
```rust
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
        append(&mut alice_log, &alice, channel, EventKind::MembershipChange, meta0.encode());
        let sealed0 = seal_keys_for(&alice, &meta0.members, &key0, 0).unwrap();
        append(&mut alice_log, &alice, channel, EventKind::KeyRotation, sealed0.encode());
        let mut alice_state = ChannelState::from_meta(channel, meta0.clone());
        alice_state.record_key(0, key0);
        append(&mut alice_log, &alice, channel, EventKind::Message, alice_state.seal_message(b"hi").unwrap());

        let mut book = ChannelBook::new();
        let first = book.process(&bob, channel, &alice_log.events(&channel));
        assert_eq!(first.len(), 1);
        // Re-processing the SAME full log surfaces nothing new.
        let second = book.process(&bob, channel, &alice_log.events(&channel));
        assert!(second.is_empty());
    }
```

- [ ] **Step 3: Add `conversations()` to `EventLog` (`src-tauri/src/eventlog/store.rs`)**

`EventLog` has `by_conversation: HashMap<ConversationId, HashSet<EventId>>`. Add this accessor (e.g. after `all_event_ids`):
```rust
    /// All conversation ids this log holds events for.
    pub fn conversations(&self) -> Vec<ConversationId> {
        self.by_conversation.keys().copied().collect()
    }
```

- [ ] **Step 4: Add `conversations()` to `PersistentEventLog` (`src-tauri/src/eventlog/persist.rs`)**
```rust
    /// All conversation ids this log holds events for (used to rebuild channel state).
    pub fn conversations(&self) -> Vec<ConversationId> {
        self.log.conversations()
    }
```

- [ ] **Step 5: Run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::channel eventlog -- --test-threads=2
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/channel.rs src-tauri/src/eventlog/store.rs src-tauri/src/eventlog/persist.rs
git commit -m "feat(node): ChannelBook dedups emitted messages; EventLog::conversations()"
git status | head -1
```
Expected: the channel tests (incl. the new dedup test) + the eventlog tests pass; clippy clean.

---

### Task 2: Node holds a ChannelBook + receive-side wiring + constructor migration

**Files:**
- Modify: `src-tauri/src/node/node.rs`
- Modify: `src-tauri/src/node/runtime.rs`, `src-tauri/src/bin/mesh-talk-node.rs` (constructor call sites)

- [ ] **Step 1: Extend the imports + `Node` struct (node.rs)**

Add to the top imports:
```rust
use crate::node::channel::{ChannelBook, ReceivedChannelMessage};
```
Add two fields to `Node`:
```rust
pub struct Node {
    identity: DeviceIdentity,
    log: Mutex<PersistentEventLog>,
    sentlog: Mutex<SentLog>,
    roster: Arc<Mutex<Roster>>,
    incoming: mpsc::UnboundedSender<ReceivedDm>,
    channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
    channels: Mutex<ChannelBook>,
    emitted: Mutex<HashSet<EventId>>,
}
```

- [ ] **Step 2: `Node::open` gains `channel_incoming` + seeds the ChannelBook from the log**

Replace `Node::open` with:
```rust
    pub fn open(
        identity: DeviceIdentity,
        roster: Arc<Mutex<Roster>>,
        incoming: mpsc::UnboundedSender<ReceivedDm>,
        channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
        log_path: &Path,
        sent_path: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        let log = PersistentEventLog::open(log_path, password)?;
        let sentlog = SentLog::open(sent_path, password)?;
        let emitted: HashSet<EventId> = log.all_event_ids().into_iter().collect();
        // Rebuild channel state from the durable log (channels survive restart). The
        // seed marks existing channel messages emitted, so they are not re-streamed.
        let mut channels = ChannelBook::new();
        for conv in log.conversations() {
            let events = log.events(&conv);
            let _ = channels.process(&identity, conv, &events);
        }
        Ok(Arc::new(Self {
            identity,
            log: Mutex::new(log),
            sentlog: Mutex::new(sentlog),
            roster,
            incoming,
            channel_incoming,
            channels: Mutex::new(channels),
            emitted: Mutex::new(emitted),
        }))
    }
```

- [ ] **Step 3: Add `process_channel` and wire it into the serve loop**

Add this private method (next to `emit_new_messages`). It snapshots the conversation's events under the log lock, runs the book under its lock, then streams the results — no lock held across a send (all sync):
```rust
    /// Run the channel book over `conv`'s events and stream any newly-decryptable
    /// channel messages. A no-op for DM conversations (no channel state).
    fn process_channel(&self, conv: ConversationId) {
        let events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv).into_iter().cloned().collect()
        };
        let refs: Vec<&Event> = events.iter().collect();
        let messages = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            book.process(&self.identity, conv, &refs)
        };
        for msg in messages {
            let _ = self.channel_incoming.send(msg);
        }
    }
```
In `serve_connection`, process channels too:
```rust
    pub async fn serve_connection(&self, mut channel: SecureChannel<TcpStream>) {
        while let Ok(Served::Handled(conv)) = serve_one(&mut channel, &self.log).await {
            self.emit_new_messages(conv);
            self.process_channel(conv);
        }
    }
```

- [ ] **Step 4: Migrate the constructor call sites**

`Node::open` now takes a `channel_incoming` before `log_path`. Update every caller:

a) `src-tauri/src/node/runtime.rs` — in `RedesignRuntime::start`, the runtime already creates `let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<ReceivedDm>();` and spawns a forwarder. Add a channel stream and pass it (the receiver is wired to the GUI in Plan 4b; for now forward it to a no-op so it isn't dropped immediately — actually just create it and KEEP the receiver alive on the struct is overkill; pass the sender and let the receiver drop, which makes `channel_incoming.send` return Err harmlessly). Concretely, add before the `Node::open` call:
```rust
        let (channel_tx, _channel_rx) = mpsc::unbounded_channel::<crate::node::channel::ReceivedChannelMessage>();
```
and pass `channel_tx` as the new `Node::open` argument:
```rust
        let node = Node::open(
            identity,
            Arc::clone(&roster),
            incoming_tx,
            channel_tx,
            &dir.join("messages.log"),
            &dir.join("sent.log"),
            password,
        )?;
```
(Plan 4b replaces `_channel_rx` with a real forwarder to a Tauri event.)

b) `src-tauri/src/bin/mesh-talk-node.rs` — the normal-node `main` calls `Node::open`. Add before it:
```rust
    let (channel_tx, _channel_rx) = mpsc::unbounded_channel::<mesh_talk::node::channel::ReceivedChannelMessage>();
```
and pass `channel_tx` as the new arg (after `incoming_tx`).

c) `src-tauri/src/node/node.rs` `#[cfg(test)] mod tests` — every `Node::open(...)` call gains a channel sink. At the top of each test that opens a node, add a channel channel and pass its sender; keep the receiver if the test needs to assert channel messages, else `_channel_rx`. For the existing DM tests (`two_nodes_exchange_a_dm_over_loopback_tcp`, `send_dm_to_unknown_peer_errors`, `offline_dm_delivered_via_post_office_over_loopback`, `reopen_seeds_emitted_...`, `history_merges_...`): add `let (ch_tx, _ch_rx) = mpsc::unbounded_channel();` before each `Node::open` and insert `ch_tx` as the argument after the `tx`/incoming arg. (These tests don't exercise channels; the sink is unused.)

- [ ] **Step 5: Build + test + clippy**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
```
Expected: the node tests pass (migrated); the bin builds; clippy clean.

- [ ] **Step 6: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs src-tauri/src/node/runtime.rs src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(node): hold a ChannelBook; seed it on open; process channels in the serve loop"
git status | head -1
```

---

### Task 3: create_channel + send_channel_message + channel_history + distribute + loopback rig

**Files:**
- Modify: `src-tauri/src/node/node.rs`

- [ ] **Step 1: Add the channel imports + `NodeError` variant if needed**

Add to imports:
```rust
use crate::channel::{ChannelMeta, GroupKey};
use crate::node::channel::seal_keys_for;
use crate::eventlog::event::EventKind;
use crate::identity::device::PublicIdentity; // if not already imported
```
(`ConversationId`, `Author`, `Event`, `request_round`, `dial`, `elected_post_office`, `PeerRecord` are already imported. `EventKind` may already be imported — if so don't duplicate.)

- [ ] **Step 2: Add `create_channel`, `send_channel_message`, `distribute_channel`, `channel_history`**
```rust
    /// Create a channel named `name` with `members` (the creator is added
    /// automatically). Mints a channel id, generates the epoch-0 group key, posts
    /// the membership + sealed-keys events, and distributes them to the members.
    pub async fn create_channel(
        &self,
        name: &str,
        mut members: Vec<PublicIdentity>,
    ) -> Result<ConversationId, NodeError> {
        let me = self.identity.public();
        if !members.iter().any(|m| m.user_id() == me.user_id()) {
            members.push(me);
        }
        let channel = crate::channel::new_channel_id();
        let key = GroupKey::generate();
        let meta = ChannelMeta { name: name.to_string(), members: members.clone(), epoch: 0 };
        let sealed = seal_keys_for(&self.identity, &members, &key, 0)
            .map_err(|_| NodeError::Session(SessionError::UnexpectedMessage))?;

        self.append_channel_event(channel, EventKind::MembershipChange, meta.encode())?;
        self.append_channel_event(channel, EventKind::KeyRotation, sealed.encode())?;
        // Build our own channel state by replaying our own events (opens our key).
        self.process_channel(channel);

        self.distribute_channel(channel, &members).await;
        Ok(channel)
    }

    /// Send a message to a channel we are a member of (and hold the key for).
    pub async fn send_channel_message(
        &self,
        channel: ConversationId,
        text: &[u8],
    ) -> Result<(), NodeError> {
        let (payload, members) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::UnknownPeer(format!("unknown channel {channel:?}")))?;
            let payload = state
                .seal_message(text)
                .map_err(|_| NodeError::Session(SessionError::UnexpectedMessage))?;
            (payload, state.members().to_vec())
        };
        self.append_channel_event(channel, EventKind::Message, payload)?;
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Append a channel event (sequencing it from the channel's log position).
    fn append_channel_event(
        &self,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) -> Result<(), NodeError> {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let mut log = self.log.lock().expect("log mutex not poisoned");
        let (parents, lamport) = log.prepare(&channel);
        let seq = log
            .version_vector(&channel)
            .get(&self_author)
            .copied()
            .unwrap_or(0)
            + 1;
        let event = Event::new(
            &self.identity,
            channel,
            seq,
            parents,
            lamport,
            now_millis(),
            kind,
            ciphertext,
        );
        log.append(event).map_err(NodeError::Log)?;
        Ok(())
    }

    /// Push the channel conversation to each known online member (dial + sync) and
    /// replicate it to the elected post office. Best-effort and fail-soft.
    async fn distribute_channel(&self, channel: ConversationId, members: &[PublicIdentity]) {
        let me = self.identity.public().user_id();
        let targets: Vec<PeerRecord> = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            members
                .iter()
                .filter(|m| m.user_id() != me)
                .filter_map(|m| roster.get(&m.user_id()).cloned())
                .collect()
        };
        for peer in targets {
            if let Ok(mut ch) = dial(peer.addr, &self.identity, Some(&peer.public)).await {
                let _ = request_round(&mut ch, &self.log, channel).await;
            }
        }
        let _ = self.replicate_to_post_office(channel).await;
    }

    /// The last `limit` messages of a channel (all directions), decrypted with the
    /// group key we hold (the sender holds the key too, so own messages decrypt).
    pub fn channel_history(&self, channel: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&channel).into_iter().cloned().collect()
        };
        let book = self.channels.lock().expect("channels mutex not poisoned");
        let Some(state) = book.state(&channel) else {
            return Vec::new();
        };
        let mut out: Vec<HistoryEntry> = Vec::new();
        for event in &events {
            if event.kind != EventKind::Message {
                continue;
            }
            if let Some(text) = state.open_message(&event.ciphertext) {
                out.push(HistoryEntry {
                    from_me: event.author == self_author,
                    who: event.author.user_id(),
                    text,
                    wall_clock: event.wall_clock,
                });
            }
        }
        out.sort_by_key(|e| e.wall_clock);
        if out.len() > limit {
            out.drain(0..out.len() - limit);
        }
        out
    }
```

- [ ] **Step 3: Drain channel conversations from the post office too**

In `drain_from_post_office`, after the DM loop, drain known channels. Add (before the method returns), reusing the already-dialed `channel` SecureChannel:
```rust
        // Drain known channel conversations as well.
        let channel_ids: Vec<ConversationId> = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.channel_ids()
        };
        for cid in channel_ids {
            if request_round(&mut channel, &self.log, cid).await.is_err() {
                return;
            }
            self.process_channel(cid);
        }
```
And add `channel_ids()` to `ChannelBook` (in `node/channel.rs`):
```rust
    /// The ids of all channels this book knows.
    pub fn channel_ids(&self) -> Vec<crate::eventlog::event::ConversationId> {
        self.states.keys().copied().collect()
    }
```

- [ ] **Step 4: Add the in-process loopback-TCP channel rig (append in `node.rs` `mod tests`)**
```rust
    #[tokio::test]
    async fn two_nodes_exchange_a_channel_message_over_loopback_tcp() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_pub = bob.public();
        let dir = tempfile::tempdir().unwrap();

        // Bob listens.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();

        // Rosters: Alice knows Bob (at his real port). Bob needs no roster to receive.
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
        let bob_roster = Arc::new(Mutex::new(Roster::default()));

        let (alice_dm_tx, _alice_dm_rx) = mpsc::unbounded_channel();
        let (alice_ch_tx, _alice_ch_rx) = mpsc::unbounded_channel();
        let (bob_dm_tx, _bob_dm_rx) = mpsc::unbounded_channel();
        let (bob_ch_tx, mut bob_ch_rx) = mpsc::unbounded_channel();

        let alice_node = Node::open(alice, alice_roster, alice_dm_tx, alice_ch_tx,
            &dir.path().join("a.log"), &dir.path().join("a-sent.log"), "pw").unwrap();
        let bob_node = Node::open(bob, bob_roster, bob_dm_tx, bob_ch_tx,
            &dir.path().join("b.log"), &dir.path().join("b-sent.log"), "pw").unwrap();

        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        // Alice creates a channel with Bob (distribution dials Bob → he learns it + his key).
        let channel = alice_node.create_channel("general", vec![bob_pub]).await.unwrap();
        // Alice sends a channel message (distribution dials Bob again).
        alice_node.send_channel_message(channel, b"hello channel").await.unwrap();

        // Bob surfaces the decrypted channel message.
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), bob_ch_rx.recv())
            .await
            .expect("bob received a channel message within 5s")
            .expect("channel stream open");
        assert_eq!(got.text, b"hello channel");
        assert_eq!(got.from, alice_node.user_id());
        assert_eq!(got.channel_name, "general");
    }
```
(Reuse the existing `seed_roster` test helper in `node.rs`'s `mod tests`. If its signature differs, adapt — it builds an `Arc<Mutex<Roster>>` knowing a peer at a port.)

- [ ] **Step 5: Build, test, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::node node::channel -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/node.rs src-tauri/src/node/channel.rs
git commit -m "feat(node): create/send/history channels + distribute; loopback channel rig"
git status | head -1
```
Expected: the channel rig + all node/channel tests pass; clippy clean.

---

## Notes for the reviewer / next plan

- **What this delivers:** live channels in the `Node` — a `ChannelBook` seeded from the durable log (restart-safe), `create_channel`/`send_channel_message`/`channel_history`, distribution by dialing each known member + replicating to the post office, and channel messages streamed on a new `mpsc<ReceivedChannelMessage>`. The loopback rig proves two real nodes create + exchange a channel message over TCP+Noise.
- **Key points:** the creator builds its own channel state by replaying its OWN events (`process_channel` after append — the `KeyRotation` seals the key to the creator too, so it opens its own entry); channel messages are group-key-symmetric so `channel_history` decrypts the sender's OWN messages (no sidecar needed, unlike DMs); `process_channel` and `emit_new_messages` coexist per conversation (each no-ops on the other's conversations).
- **Deferred to Plan 4b (app + UI):** the `RedesignRuntime` forwards the channel stream to a `redesign-channel-message` Tauri event; IPC commands `redesign_create_channel`/`redesign_list_channels`/`redesign_send_channel_message`/`redesign_channel_history`; a channels section in the `/redesign` Vue route. Also deferred: dynamic add/remove member over the network (the rotation read-side is proven; live add-member is a small follow-up), and channel name/membership listing for the UI.
- **Reviewer checks:** confirm no lock held across `.await` in `distribute_channel`/`send_channel_message` (roster/channels/log snapshotted under the guard, dropped before dial); the constructor migration covers all `Node::open` call sites (runtime, bin, node tests); `process_channel` after `create_channel`'s append correctly opens the creator's own key; and the rig's two `distribute` dials (create + send) both reach Bob's serve loop.
