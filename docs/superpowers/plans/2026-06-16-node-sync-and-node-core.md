# Networked Sync + Node Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make two peers exchange a real 1:1 DM end-to-end — derive a shared DM conversation id, drive event-log reconciliation over an authenticated `SecureChannel`, and assemble it into a `Node` that seals/sends and receives/decrypts — validated by an in-process two-node exchange over loopback TCP.

**Architecture:** `node::conversation` derives the deterministic DM `ConversationId` (sorted key pair) and provides the DM event ↔ sealed-payload helpers (`build_dm_event`/`open_dm_event`). `node::session` is the networked counterpart of Plan-4's in-process `reconcile`: it frames Plan-4's sync messages over a `SecureChannel` (`request_round` as requester, `serve_one` as responder). `node::node` is the App context — it owns the identity, an in-memory `EventLog`, and a shared roster, exposes `send_dm` (seal → append → dial → sync round) and `serve_connection` (serve rounds + surface received DMs on an mpsc), with no global singletons.

**Tech Stack:** Rust; the Phase-0 modules (`identity`, `transport`, `eventlog` store+sync, `dm`) and Plan-1's `discovery` + `node::transport`; `tokio` (async, already a dependency); `serde`/`bincode`; `sha2`. No new dependencies.

---

## Background the implementer needs

**This is Plan 2 of the "online direct DM" slice** (spec: `docs/superpowers/specs/2026-06-16-node-integration-online-dm-design.md`); Plan 1 (committed) built `discovery` + `node::transport` (`dial`/`accept`). This plan adds the messaging core. The **`mesh-talk-node` CLI binary + the two-process rig are deferred to a short Plan 3** — this plan proves correctness with an in-process two-node test over real loopback TCP.

**APIs you build on (all implemented):**
```rust
// identity/device.rs
pub struct PublicIdentity { pub ed25519_pub: [u8;32], pub x25519_pub: [u8;32] } // Clone, PartialEq, Eq
impl PublicIdentity { pub fn user_id(&self) -> String; }
impl DeviceIdentity { pub fn generate() -> Self; pub fn public(&self) -> PublicIdentity; }
// eventlog
pub struct ConversationId([u8;32]); // Copy; ConversationId::new([u8;32])
pub struct EventId([u8;32]);          // Copy, Eq, Hash
pub struct Author([u8;32]);           // Copy, Eq, Hash; Author::from_ed25519([u8;32]); .user_id()->String
pub enum EventKind { Message, /* ... */ } // PartialEq, Copy
pub struct Event { pub id: EventId, pub author: Author, pub kind: EventKind, pub ciphertext: Vec<u8>, /* ... */ } // Clone
impl Event { pub fn new(identity, conversation_id, seq, parents: Vec<EventId>, lamport, wall_clock, kind, ciphertext) -> Event; }
pub struct EventLog;  // Default; append, events(&conv)->Vec<&Event>, has, prepare(&conv)->(Vec<EventId>, u64), version_vector(&conv)->HashMap<Author,u64>
pub enum LogError;    // Display
// eventlog::sync — Plan 4
pub struct SyncRequest  { pub conversation: ConversationId, pub have: Vec<EventId> }       // Serialize, Deserialize
pub struct SyncResponse { pub conversation: ConversationId, pub events: Vec<Event>, pub have: Vec<EventId> }
pub struct SyncFollowup { pub conversation: ConversationId, pub events: Vec<Event> }
pub struct ApplyReport;
pub trait SyncStore;  // implemented by EventLog
pub fn build_request(store: &impl SyncStore, conversation: ConversationId) -> SyncRequest;
pub fn handle_request(store: &impl SyncStore, request: &SyncRequest) -> SyncResponse;
pub fn handle_response(store: &mut impl SyncStore, response: &SyncResponse) -> (ApplyReport, SyncFollowup);
pub fn handle_followup(store: &mut impl SyncStore, followup: &SyncFollowup) -> ApplyReport;
// dm.rs
pub fn seal(sender: &DeviceIdentity, recipient_x25519_pub: &[u8;32], plaintext: &[u8]) -> Result<Vec<u8>, DmError>;
pub fn open(recipient: &DeviceIdentity, sender_x25519_pub: &[u8;32], envelope: &[u8]) -> Result<Vec<u8>, DmError>; // DmError: Display
// discovery (Plan 1)
pub struct Roster; impl Roster { pub fn get(&self, user_id: &str) -> Option<&PeerRecord>; pub fn update(...) -> bool; } // Default
pub struct PeerRecord { pub public: PublicIdentity, pub addr: SocketAddr, pub name: String, pub last_seen: Instant } // Clone
pub type UserId = String;
pub struct Announce; impl Announce { pub fn new(&DeviceIdentity, name, tcp_port) -> Self; } // for test roster seeding
// transport (Plan 2) + node::transport (Plan-1 of this slice)
pub struct SecureChannel<S>; // send(&mut self,&[u8])->Result<(),TransportError>; recv(&mut self)->Result<Vec<u8>,TransportError>; S: AsyncRead+AsyncWrite+Unpin
pub enum TransportError; // Display
pub async fn dial(addr: SocketAddr, identity: &DeviceIdentity, expected_peer: Option<&PublicIdentity>) -> Result<SecureChannel<TcpStream>, TransportError>;
pub async fn accept(listener: &TcpListener, identity: &DeviceIdentity) -> Result<SecureChannel<TcpStream>, TransportError>;
```

**Half-duplex note:** `SecureChannel` is `&mut self` for `send`/`recv` (no concurrent read+write). The sync protocol is a strict ping-pong, so this is fine: the requester does send/recv/send; the responder does recv/send (Request round) then recv (Followup). Each side only ever alternates.

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare full `cargo test`/`cargo build`. Always `nice` + throttle, scoped:
```bash
cd src-tauri && nice -n 10 cargo test --lib node:: -- --test-threads=2
nice -n 10 cargo fmt
nice -n 10 cargo clippy --lib -- -D warnings
```
The pre-commit hook runs the full health check on commit — let it run. Async tests use in-memory duplexes / loopback TCP (fast, deterministic with bounded timeouts).

**Conventions:** match `eventlog`/`transport`/`dm`/`discovery` — hand-written error enums (NO `thiserror`), `Display`+`Error`+`From`, `#[cfg(test)] mod tests`. rustfmt's `reorder_modules` sorts `pub mod` lines — keep alphabetical.

---

### Task 1: `node::conversation` — DM conversation id + DM event helpers

**Files:**
- Create: `src-tauri/src/node/conversation.rs`
- Modify: `src-tauri/src/node/mod.rs` (add module)

- [ ] **Step 1: Add the module declaration to `node/mod.rs`**

In `src-tauri/src/node/mod.rs`, add `pub mod conversation;` (sorted: `conversation`, `transport`) so it reads:
```rust
//! The redesign node: the explicit App context that wires identity, discovery,
//! transport, the event log, and DM crypto into a runnable peer. This plan adds
//! only [`transport`] (authenticated channel dial/accept); the conversation
//! layer, sync driver, and node API arrive in the next plan.

pub mod conversation;
pub mod transport;
```

- [ ] **Step 2: Create `src-tauri/src/node/conversation.rs`**

```rust
//! DM conversation identity and the DM event ↔ sealed-payload helpers.

use crate::discovery::roster::{Roster, UserId};
use crate::dm;
use crate::eventlog::event::{ConversationId, Event, EventId, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use sha2::{Digest, Sha256};

/// Domain separator for DM conversation-id derivation.
const DM_CONV_DOMAIN: &[u8] = b"mesh-talk-dm-conversation-v1";

/// The deterministic conversation id for the 1:1 DM between two peers — a hash of
/// the SORTED pair of Ed25519 keys, so both peers compute the same id regardless
/// of who is "a" and who is "b".
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
/// is unknown to us, or it fails to decrypt (e.g. it wasn't sealed for us).
pub fn open_dm_event(
    recipient: &DeviceIdentity,
    roster: &Roster,
    event: &Event,
) -> Option<(UserId, String, Vec<u8>)> {
    if event.kind != EventKind::Message {
        return None;
    }
    let author_user_id = event.author.user_id();
    let peer = roster.get(&author_user_id)?;
    let plaintext = dm::open(recipient, &peer.public.x25519_pub, &event.ciphertext).ok()?;
    Some((author_user_id, peer.name.clone(), plaintext))
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
        let (from, name, text) = open_dm_event(&bob, &roster, &event).expect("bob opens");
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
        let event = build_dm_event(&alice, &bob.public(), conv, 1, vec![], 1, 0, b"secret").unwrap();
        let roster = roster_knowing(&alice, "Alice", &carol.public().user_id());
        assert!(open_dm_event(&carol, &roster, &event).is_none());
    }
}
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::conversation -- --test-threads=2
```
Expected: PASS — 5 tests.

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/mod.rs src-tauri/src/node/conversation.rs
git commit -m "feat(node): DM conversation id + seal/open event helpers

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: `node::session` — networked sync over a SecureChannel

**Files:**
- Create: `src-tauri/src/node/session.rs`
- Modify: `src-tauri/src/node/mod.rs` (add module)

- [ ] **Step 1: Add the module declaration to `node/mod.rs`**

In `src-tauri/src/node/mod.rs`, add `pub mod session;` (sorted: `conversation`, `session`, `transport`):
```rust
pub mod conversation;
pub mod session;
pub mod transport;
```

- [ ] **Step 2: Create `src-tauri/src/node/session.rs`**

```rust
//! Driving event-log reconciliation over a [`SecureChannel`] — the networked
//! counterpart of Plan-4's in-process `reconcile`. The same handlers
//! (`build_request`/`handle_request`/`handle_response`/`handle_followup`) run on
//! each side; the three messages are framed (bincode) and sent over the channel.
//! The requester runs one round with [`request_round`]; the responder serves
//! rounds one message at a time with [`serve_one`].
//!
//! The store is shared as a `&Mutex<S>` and locked only for the synchronous
//! handler calls — never across an `.await` — so a `std::sync::Mutex` is correct.

use crate::eventlog::event::ConversationId;
use crate::eventlog::sync::{
    build_request, handle_followup, handle_request, handle_response, ApplyReport, SyncFollowup,
    SyncRequest, SyncResponse, SyncStore,
};
use crate::transport::{SecureChannel, TransportError};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

/// One framed sync message on the wire.
#[derive(Debug, Serialize, Deserialize)]
enum SyncWire {
    Request(SyncRequest),
    Response(SyncResponse),
    Followup(SyncFollowup),
}

/// Errors from a sync session.
#[derive(Debug)]
pub enum SessionError {
    /// Channel send/recv failure.
    Transport(TransportError),
    /// (De)serialization of a wire message failed.
    Serialization(String),
    /// Received a wire message that doesn't fit the protocol state.
    UnexpectedMessage,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Transport(e) => write!(f, "sync transport error: {e}"),
            SessionError::Serialization(m) => write!(f, "sync serialization error: {m}"),
            SessionError::UnexpectedMessage => write!(f, "unexpected sync message"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<TransportError> for SessionError {
    fn from(e: TransportError) -> Self {
        SessionError::Transport(e)
    }
}

fn encode(wire: &SyncWire) -> Result<Vec<u8>, SessionError> {
    bincode::serialize(wire).map_err(|e| SessionError::Serialization(e.to_string()))
}

fn decode(bytes: &[u8]) -> Result<SyncWire, SessionError> {
    bincode::deserialize(bytes).map_err(|e| SessionError::Serialization(e.to_string()))
}

/// Run one reconciliation round as the REQUESTER over `channel`, for
/// `conversation`: send a Request built from the local store, receive the
/// Response (ingesting its events), then send the Followup. Converges the
/// conversation in both directions. Returns what this side applied.
pub async fn request_round<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
) -> Result<ApplyReport, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let request = {
        let store = store.lock().expect("store mutex not poisoned");
        build_request(&*store, conversation)
    };
    channel.send(&encode(&SyncWire::Request(request))?).await?;

    let response = match decode(&channel.recv().await?)? {
        SyncWire::Response(r) => r,
        _ => return Err(SessionError::UnexpectedMessage),
    };

    let (report, followup) = {
        let mut store = store.lock().expect("store mutex not poisoned");
        handle_response(&mut *store, &response)
    };
    channel.send(&encode(&SyncWire::Followup(followup))?).await?;
    Ok(report)
}

/// The outcome of serving one inbound wire message.
pub enum Served {
    /// Handled a message for this conversation (the caller may surface new events).
    Handled(ConversationId),
    /// The channel closed / errored — stop serving it.
    Closed,
}

/// Serve ONE inbound wire message as the RESPONDER over `channel`: a Request is
/// answered with a Response; a Followup is ingested. Returns the conversation
/// handled, or `Closed` when the peer hangs up (a recv error is treated as a
/// clean close so the serve loop can stop).
pub async fn serve_one<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
) -> Result<Served, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let bytes = match channel.recv().await {
        Ok(b) => b,
        Err(_) => return Ok(Served::Closed),
    };
    match decode(&bytes)? {
        SyncWire::Request(request) => {
            let conversation = request.conversation;
            let response = {
                let store = store.lock().expect("store mutex not poisoned");
                handle_request(&*store, &request)
            };
            channel.send(&encode(&SyncWire::Response(response))?).await?;
            Ok(Served::Handled(conversation))
        }
        SyncWire::Followup(followup) => {
            let conversation = followup.conversation;
            {
                let mut store = store.lock().expect("store mutex not poisoned");
                handle_followup(&mut *store, &followup);
            }
            Ok(Served::Handled(conversation))
        }
        SyncWire::Response(_) => Err(SessionError::UnexpectedMessage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    #[tokio::test]
    async fn one_round_converges_two_stores_over_a_channel() {
        // A SecureChannel pair over an in-memory duplex (no real sockets).
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        // A has one event; B is empty.
        let event = Event::new(&a_id, conv(), 1, vec![], 1, 0, EventKind::Message, b"x".to_vec());
        let a_store = Mutex::new({
            let mut log = EventLog::default();
            log.append(event.clone()).unwrap();
            log
        });
        let event_id = event.id;

        // Responder: accept the channel and serve two messages (Request, then Followup).
        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(EventLog::default());
            // Request → Response
            assert!(matches!(serve_one(&mut b_ch, &b_store).await.unwrap(), Served::Handled(_)));
            // Followup (carries A's event)
            assert!(matches!(serve_one(&mut b_ch, &b_store).await.unwrap(), Served::Handled(_)));
            b_store.into_inner().unwrap()
        });

        // Requester: connect and run one round.
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv()).await.unwrap();

        let b_log = server.await.unwrap();
        assert!(b_log.has(&event_id), "B received A's event via the sync round");
        assert!(a_store.lock().unwrap().has(&event_id));
    }
}
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::session -- --test-threads=2
```
Expected: PASS — 1 test.

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/mod.rs src-tauri/src/node/session.rs
git commit -m "feat(node): networked sync driver (request_round / serve_one) over a channel

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `node::node` — the Node App context + the end-to-end exchange

**Files:**
- Create: `src-tauri/src/node/node.rs`
- Modify: `src-tauri/src/node/mod.rs` (add module + re-exports)

- [ ] **Step 1: Add the module + re-exports to `node/mod.rs`**

In `src-tauri/src/node/mod.rs`, add `pub mod node;` (sorted: `conversation`, `node`, `session`, `transport`) and a re-export. The module section becomes:
```rust
pub mod conversation;
pub mod node;
pub mod session;
pub mod transport;

pub use node::{Node, NodeError, ReceivedDm};
```

- [ ] **Step 2: Create `src-tauri/src/node/node.rs`**

```rust
//! The node App context: owns the device identity, an in-memory event log, and a
//! shared roster; wires DM crypto + the event log + the networked sync driver
//! into `send_dm` (outbound) and a per-connection serve loop (inbound) that
//! surfaces received DMs on an mpsc channel. No global singletons. Starting
//! discovery and binding the TCP listener is the binary's job (next plan); this
//! module provides the pieces and proves them with an in-process two-node
//! exchange over loopback TCP.

use crate::discovery::roster::{Roster, UserId};
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::store::EventLog;
use crate::identity::device::DeviceIdentity;
use crate::node::conversation::{build_dm_event, dm_conversation_id, open_dm_event};
use crate::node::session::{request_round, serve_one, Served, SessionError};
use crate::node::transport::{accept, dial};
use crate::transport::SecureChannel;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// A received direct message, surfaced to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedDm {
    pub from: UserId,
    pub from_name: String,
    pub text: Vec<u8>,
}

/// Errors from node operations.
#[derive(Debug)]
pub enum NodeError {
    /// `send_dm` to a `user_id` not in the roster.
    UnknownPeer(UserId),
    /// Sealing the DM payload failed.
    Seal(crate::dm::DmError),
    /// Appending the event locally failed.
    Log(crate::eventlog::LogError),
    /// The networked sync session failed.
    Session(SessionError),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::UnknownPeer(u) => write!(f, "unknown peer: {u}"),
            NodeError::Seal(e) => write!(f, "seal error: {e}"),
            NodeError::Log(e) => write!(f, "log error: {e}"),
            NodeError::Session(e) => write!(f, "session error: {e}"),
        }
    }
}

impl std::error::Error for NodeError {}

/// The node: identity + event log + shared roster + an outbound stream of
/// received DMs. Construct with [`Node::new`]; share as `Arc<Node>`.
pub struct Node {
    identity: DeviceIdentity,
    log: Mutex<EventLog>,
    roster: Arc<Mutex<Roster>>,
    incoming: mpsc::UnboundedSender<ReceivedDm>,
    emitted: Mutex<HashSet<EventId>>,
}

impl Node {
    /// Build a node over an existing (shared) roster, emitting received DMs to
    /// `incoming`.
    pub fn new(
        identity: DeviceIdentity,
        roster: Arc<Mutex<Roster>>,
        incoming: mpsc::UnboundedSender<ReceivedDm>,
    ) -> Arc<Self> {
        Arc::new(Self {
            identity,
            log: Mutex::new(EventLog::default()),
            roster,
            incoming,
            emitted: Mutex::new(HashSet::new()),
        })
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> UserId {
        self.identity.public().user_id()
    }

    /// Send a DM to `recipient` (a known peer): seal it, append the Message event
    /// to the local log, dial the peer, and run one sync round to deliver it.
    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError> {
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;

        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);

        {
            let mut log = self.log.lock().expect("log mutex not poisoned");
            let (parents, lamport) = log.prepare(&conv);
            let seq = log
                .version_vector(&conv)
                .get(&self_author)
                .copied()
                .unwrap_or(0)
                + 1;
            let event = build_dm_event(
                &self.identity,
                &peer.public,
                conv,
                seq,
                parents,
                lamport,
                now_millis(),
                text,
            )
            .map_err(NodeError::Seal)?;
            log.append(event).map_err(NodeError::Log)?;
        }

        let mut channel = dial(peer.addr, &self.identity, Some(&peer.public))
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        request_round(&mut channel, &self.log, conv)
            .await
            .map_err(NodeError::Session)?;
        // The round may also have pulled events from the peer.
        self.emit_new_messages(conv);
        Ok(())
    }

    /// Accept inbound connections on `listener` and serve each on its own task,
    /// until the listener errors. (The binary calls this; the test drives it too.)
    pub async fn run_accept_loop(self: Arc<Self>, listener: TcpListener) {
        loop {
            match accept(&listener, &self.identity).await {
                Ok(channel) => {
                    let node = Arc::clone(&self);
                    tokio::spawn(async move { node.serve_connection(channel).await });
                }
                Err(_) => continue, // a failed handshake shouldn't stop accepting
            }
        }
    }

    /// Serve one authenticated inbound connection: handle sync rounds and surface
    /// any newly-received DMs, until the peer disconnects.
    pub async fn serve_connection(&self, mut channel: SecureChannel<TcpStream>) {
        loop {
            match serve_one(&mut channel, &self.log).await {
                Ok(Served::Handled(conv)) => self.emit_new_messages(conv),
                Ok(Served::Closed) | Err(_) => break,
            }
        }
    }

    /// Decrypt and emit any not-yet-emitted, non-self `Message` events in `conv`.
    fn emit_new_messages(&self, conv: ConversationId) {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let fresh: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let mut emitted = self.emitted.lock().expect("emitted mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| {
                    e.kind == EventKind::Message
                        && e.author != self_author
                        && !emitted.contains(&e.id)
                })
                .map(|e| {
                    emitted.insert(e.id);
                    e.clone()
                })
                .collect()
        };
        let roster = self.roster.lock().expect("roster mutex not poisoned");
        for event in fresh {
            if let Some((from, from_name, text)) = open_dm_event(&self.identity, &roster, &event) {
                let _ = self.incoming.send(ReceivedDm { from, from_name, text });
            }
        }
    }
}

/// Milliseconds since the Unix epoch, for an event's wall-clock field.
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::announce::Announce;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    fn seed_roster(peer: &DeviceIdentity, name: &str, port: u16, self_user_id: &str) -> Arc<Mutex<Roster>> {
        let roster = Arc::new(Mutex::new(Roster::default()));
        roster.lock().unwrap().update(
            &Announce::new(peer, name, port),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            self_user_id,
        );
        roster
    }

    #[tokio::test]
    async fn two_nodes_exchange_a_dm_over_loopback_tcp() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();

        // Bob's listener (ephemeral loopback port).
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();

        // Rosters: Alice knows Bob (at his real listen port); Bob knows Alice
        // (any port — Bob never dials Alice; he just needs her key to decrypt).
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
        let bob_roster = seed_roster(&alice, "Alice", 4000, &bob_uid);

        let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
        let alice_node = Node::new(alice, alice_roster, alice_tx);
        let bob_node = Node::new(bob, bob_roster, bob_tx);

        // Bob accepts and serves connections.
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        // Alice sends Bob a DM.
        alice_node.send_dm(&bob_uid, b"meet at 5").await.expect("send_dm");

        // Bob surfaces the decrypted DM (bounded wait).
        let received = tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
            .await
            .expect("bob received a dm within 5s")
            .expect("incoming channel open");
        assert_eq!(received.from, alice_uid);
        assert_eq!(received.from_name, "Alice");
        assert_eq!(received.text, b"meet at 5");
    }
}
```

- [ ] **Step 3: Run the whole `node` suite**
```bash
cd src-tauri && nice -n 10 cargo test --lib node:: -- --test-threads=2
```
Expected: PASS — all `node` tests (conversation 5 + session 1 + transport 4 + the new node integration test).

- [ ] **Step 4: Full fmt + clippy gate**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt --check; echo "fmt EXIT: $?"
```
Expected: clippy clean; `fmt EXIT: 0`.

- [ ] **Step 5: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/mod.rs src-tauri/src/node/node.rs
git commit -m "feat(node): Node App context with send_dm + serve; two-node DM over TCP

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next plan

- **What this delivers:** the messaging core — `dm_conversation_id` + `build_dm_event`/`open_dm_event` (`conversation.rs`), the networked sync driver `request_round`/`serve_one` (`session.rs`, the on-the-wire counterpart of Plan-4's `reconcile`), and the `Node` App context with `send_dm`/`serve_connection`/`run_accept_loop` and a `ReceivedDm` mpsc stream (`node.rs`). The integration test proves **two nodes exchange a real, sealed, signed DM over loopback TCP**, decrypted via the roster.
- **Deliberately deferred to Plan 3 of this slice:** the `mesh-talk-node` CLI binary (clap args, keystore load, discovery startup, REPL) and the two-process integration rig. Also deferred (later slices): the post office / offline delivery, a persistent message log, channels, and migrating the Tauri UI.
- **Security/correctness:** every received event passes the store's `append` validation (signature/integrity/parents); the DM is sealed to the recipient's X25519 and signed by the author's Ed25519; `serve_connection` only surfaces messages it can decrypt via the roster; `send_dm` dials with the peer pinned (`Some(&peer.public)`), so a wrong endpoint is rejected at the handshake. Locks are held only across synchronous handler calls, never across `.await` (so `std::sync::Mutex` is correct and there is no deadlock).
- **Accepted limitations (this slice):** one sync round per `send_dm` (a fresh channel per send — no connection reuse/pooling yet); in-memory log (messages don't persist across restart; identity does); per-`send_dm` dial means no long-lived bidirectional channel (B replying to A dials A separately); the emit is keyed off the conversation handled by each round (no global "all conversations" scan).
