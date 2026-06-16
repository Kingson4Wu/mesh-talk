//! The node App context: owns the device identity, an in-memory event log, and a
//! shared roster; wires DM crypto + the event log + the networked sync driver
//! into `send_dm` (outbound) and a per-connection serve loop (inbound) that
//! surfaces received DMs on an mpsc channel. No global singletons. Starting
//! discovery and binding the TCP listener is the binary's job (next plan); this
//! module provides the pieces and proves them with an in-process two-node
//! exchange over loopback TCP.

use crate::discovery::roster::{PeerRecord, Roster, UserId};
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::persist::PersistentEventLog;
use crate::eventlog::LogError;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::conversation::{build_dm_event, dm_conversation_id, open_dm_event};
use crate::node::postbox::elected_post_office;
use crate::node::sentlog::SentLog;
use crate::node::session::{request_round, serve_one, Served, SessionError};
use crate::node::transport::{accept, dial};
use crate::transport::SecureChannel;
use std::collections::HashSet;
use std::path::Path;
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

/// A merged conversation-history entry (sent or received), for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub from_me: bool,
    pub who: String,
    pub text: Vec<u8>,
    pub wall_clock: u64,
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
/// received DMs. Construct with [`Node::open`]; share as `Arc<Node>`.
pub struct Node {
    identity: DeviceIdentity,
    log: Mutex<PersistentEventLog>,
    sentlog: Mutex<SentLog>,
    roster: Arc<Mutex<Roster>>,
    incoming: mpsc::UnboundedSender<ReceivedDm>,
    emitted: Mutex<HashSet<EventId>>,
}

impl Node {
    /// Open a node backed by a durable event log at `log_path` and a sent-plaintext
    /// sidecar at `sent_path` (both encrypted with `password`). Restored history is
    /// seeded into the already-emitted set so it is NOT re-streamed — only events
    /// received after open are surfaced live.
    pub fn open(
        identity: DeviceIdentity,
        roster: Arc<Mutex<Roster>>,
        incoming: mpsc::UnboundedSender<ReceivedDm>,
        log_path: &Path,
        sent_path: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        let log = PersistentEventLog::open(log_path, password)?;
        let sentlog = SentLog::open(sent_path, password)?;
        let emitted: HashSet<EventId> = log.all_event_ids().into_iter().collect();
        Ok(Arc::new(Self {
            identity,
            log: Mutex::new(log),
            sentlog: Mutex::new(sentlog),
            roster,
            incoming,
            emitted: Mutex::new(emitted),
        }))
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> UserId {
        self.identity.public().user_id()
    }

    /// Send a DM to `recipient` (a known peer): seal it, append the Message event
    /// locally, then deliver it. Delivery is best-effort DIRECT (the recipient may
    /// be offline) plus ALWAYS replicating to the elected post office (so an
    /// offline recipient can retrieve it later). Succeeds if EITHER the direct
    /// delivery or a post office accepted the event; errors only if the recipient
    /// is unreachable and no post office is available.
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
        let wall_clock = now_millis();
        let seq;
        {
            let mut log = self.log.lock().expect("log mutex not poisoned");
            let (parents, lamport) = log.prepare(&conv);
            seq = log
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
                wall_clock,
                text,
            )
            .map_err(NodeError::Seal)?;
            log.append(event).map_err(NodeError::Log)?;
        }

        // Keep a local plaintext copy of what we sent (sealed events aren't
        // self-decryptable). Best-effort: a sidecar write error doesn't fail a
        // message that was sealed, appended, and about to be delivered.
        let _ = self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .record(conv, seq, wall_clock, text);

        // Best-effort direct delivery (the recipient may be offline) plus always
        // replicating to the elected post office (store-and-forward).
        let direct = self.deliver_direct(&peer, conv).await;
        let replicated = self.replicate_to_post_office(conv).await;
        // Either round may also have pulled events back to us.
        self.emit_new_messages(conv);

        match (direct, replicated) {
            (Ok(()), _) => Ok(()),                             // delivered directly
            (Err(_), Ok(true)) => Ok(()),                      // a post office holds it
            (Err(e), Ok(false)) => Err(NodeError::Session(e)), // offline peer, no PO
            (Err(_), Err(e)) => Err(NodeError::Session(e)),    // both paths failed
        }
    }

    /// Dial `peer` directly and run one sync round for `conv`. Best-effort: the
    /// peer may be offline, in which case the dial fails.
    async fn deliver_direct(
        &self,
        peer: &PeerRecord,
        conv: ConversationId,
    ) -> Result<(), SessionError> {
        let mut channel = dial(peer.addr, &self.identity, Some(&peer.public))
            .await
            .map_err(SessionError::Transport)?;
        request_round(&mut channel, &self.log, conv)
            .await
            .map(|_| ())
    }

    /// Replicate `conv` to the elected post office, if one is known. Returns
    /// `Ok(true)` if a post office accepted a round, `Ok(false)` if no post office
    /// is known.
    async fn replicate_to_post_office(&self, conv: ConversationId) -> Result<bool, SessionError> {
        let po = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            elected_post_office(&roster)
        };
        let Some(po) = po else {
            return Ok(false);
        };
        let mut channel = dial(po.addr, &self.identity, Some(&po.public))
            .await
            .map_err(SessionError::Transport)?;
        request_round(&mut channel, &self.log, conv).await?;
        Ok(true)
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
                Err(_e) => {
                    // A failed accept (handshake failure, or a listener-level error such
                    // as fd exhaustion) shouldn't stop the loop; back off briefly so a
                    // persistent error can't become a busy-spin.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    /// Serve one authenticated inbound connection: handle sync rounds and surface
    /// any newly-received DMs, until the peer disconnects.
    pub async fn serve_connection(&self, mut channel: SecureChannel<TcpStream>) {
        while let Ok(Served::Handled(conv)) = serve_one(&mut channel, &self.log).await {
            self.emit_new_messages(conv);
        }
    }

    /// Pull any DMs held for this node from the elected post office: dial it once,
    /// then run a sync round for each known peer's DM conversation, surfacing
    /// anything new. A no-op if no post office is known. Best-effort and
    /// fail-soft — a dial/round error just ends this drain; the next one retries.
    pub async fn drain_from_post_office(&self) {
        let (post_office, peers) = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            (elected_post_office(&roster), roster.peers())
        };
        let Some(po) = post_office else {
            return;
        };
        let mut channel = match dial(po.addr, &self.identity, Some(&po.public)).await {
            Ok(c) => c,
            Err(_) => return,
        };
        // Drain a DM conversation per non-PO peer (we never DM a post office).
        for peer in peers.iter().filter(|p| !p.post_office) {
            let conv = dm_conversation_id(&self.identity.public(), &peer.public);
            if request_round(&mut channel, &self.log, conv).await.is_err() {
                return; // channel broke; the next drain re-dials
            }
            self.emit_new_messages(conv);
        }
    }

    /// The last `limit` messages of the DM with `peer`, both directions, in time
    /// order. Convenience wrapper that derives the conversation id.
    pub fn dm_history(&self, peer: &PublicIdentity, limit: usize) -> Vec<HistoryEntry> {
        let conv = dm_conversation_id(&self.identity.public(), peer);
        self.history(conv, limit)
    }

    /// The last `limit` messages of `conversation`, both directions, sorted by
    /// wall-clock: peer-authored events decrypted from the durable log, plus our
    /// own sent plaintext from the local sidecar.
    pub fn history(&self, conversation: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Snapshot the conversation's events, then release the log lock.
        let events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conversation).into_iter().cloned().collect()
        };

        let mut entries: Vec<HistoryEntry> = Vec::new();
        {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            for event in &events {
                if event.kind != EventKind::Message || event.author == self_author {
                    continue; // our own sent events come from the sidecar (below)
                }
                if let Some((_uid, name, text)) = open_dm_event(&self.identity, &roster, event) {
                    entries.push(HistoryEntry {
                        from_me: false,
                        who: name,
                        text,
                        wall_clock: event.wall_clock,
                    });
                }
            }
        }
        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&conversation)
        {
            entries.push(HistoryEntry {
                from_me: true,
                who: "you".to_string(),
                text: sent.plaintext,
                wall_clock: sent.wall_clock,
            });
        }
        entries.sort_by_key(|e| e.wall_clock);
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }

    /// Decrypt and emit any not-yet-emitted, non-self `Message` events in `conv`.
    fn emit_new_messages(&self, conv: ConversationId) {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let fresh: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let mut emitted = self.emitted.lock().expect("emitted mutex not poisoned");
            // Collect candidates first (ends the filter borrow on `emitted`),
            // then mark them as emitted in a separate pass.
            let candidates: Vec<Event> = log
                .events(&conv)
                .into_iter()
                .filter(|e| {
                    e.kind == EventKind::Message
                        && e.author != self_author
                        && !emitted.contains(&e.id)
                })
                .cloned()
                .collect();
            for e in &candidates {
                emitted.insert(e.id);
            }
            candidates
        };
        let roster = self.roster.lock().expect("roster mutex not poisoned");
        for event in fresh {
            if let Some((from, from_name, text)) = open_dm_event(&self.identity, &roster, &event) {
                let _ = self.incoming.send(ReceivedDm {
                    from,
                    from_name,
                    text,
                });
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
    use crate::node::postbox::run_relay_accept_loop;
    use crate::postoffice::PostOffice;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    fn seed_roster(
        peer: &DeviceIdentity,
        name: &str,
        port: u16,
        self_user_id: &str,
    ) -> Arc<Mutex<Roster>> {
        let roster = Arc::new(Mutex::new(Roster::default()));
        roster.lock().unwrap().update(
            &Announce::new(peer, name, port),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            self_user_id,
        );
        roster
    }

    #[tokio::test]
    async fn send_dm_to_unknown_peer_errors() {
        let dir = tempfile::tempdir().unwrap();
        let me = DeviceIdentity::generate();
        let roster = Arc::new(Mutex::new(Roster::default())); // empty
        let (tx, _rx) = mpsc::unbounded_channel();
        let node = Node::open(
            me,
            roster,
            tx,
            &dir.path().join("me.log"),
            &dir.path().join("me-sent.log"),
            "pw",
        )
        .unwrap();
        let err = node.send_dm("nope", b"hi").await.unwrap_err();
        assert!(matches!(err, NodeError::UnknownPeer(u) if u == "nope"));
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

        let dir = tempfile::tempdir().unwrap();
        let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
        let alice_node = Node::open(
            alice,
            alice_roster,
            alice_tx,
            &dir.path().join("alice.log"),
            &dir.path().join("alice-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            bob_tx,
            &dir.path().join("bob.log"),
            &dir.path().join("bob-sent.log"),
            "pw",
        )
        .unwrap();

        // Bob accepts and serves connections.
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        // Alice sends Bob a DM.
        alice_node
            .send_dm(&bob_uid, b"meet at 5")
            .await
            .expect("send_dm");

        // Bob surfaces the decrypted DM (bounded wait).
        let received = tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
            .await
            .expect("bob received a dm within 5s")
            .expect("incoming channel open");
        assert_eq!(received.from, alice_uid);
        assert_eq!(received.from_name, "Alice");
        assert_eq!(received.text, b"meet at 5");
    }

    #[tokio::test]
    async fn offline_dm_delivered_via_post_office_over_loopback() {
        // Alice sends Bob a DM while Bob is offline; a post office holds it; Bob
        // drains it and decrypts — all in-process over loopback TCP.
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let po_id = DeviceIdentity::generate();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();

        // Post office relay on loopback. Keep extra identity copies (open() moves one).
        let po_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let po_addr = po_listener.local_addr().unwrap();
        let (po_ed, po_x) = po_id.secret_bytes();
        let po_transport = DeviceIdentity::from_secret_bytes(po_ed, po_x);
        let po_seed = DeviceIdentity::from_secret_bytes(po_ed, po_x);
        let po_store = Arc::new(Mutex::new(
            PostOffice::open(&dir.path().join("po.log"), "pw", po_id).unwrap(),
        ));
        tokio::spawn(run_relay_accept_loop(
            po_transport,
            po_listener,
            Arc::clone(&po_store),
        ));

        // A closed port stands in for offline Bob — Alice's direct dial fails fast.
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_dead_addr = dead.local_addr().unwrap();
        drop(dead);

        // Seed a roster entry whose addr/keys/role we control (addr = (ip, port)).
        fn seed(
            roster: &Arc<Mutex<Roster>>,
            id: &DeviceIdentity,
            name: &str,
            addr: SocketAddr,
            post_office: bool,
            self_uid: &str,
        ) {
            let announce = if post_office {
                Announce::new_post_office(id, name, addr.port())
            } else {
                Announce::new(id, name, addr.port())
            };
            roster
                .lock()
                .unwrap()
                .update(&announce, addr.ip(), self_uid);
        }

        // Alice knows offline-Bob (dead addr) + the PO (real addr).
        let alice_roster = Arc::new(Mutex::new(Roster::default()));
        seed(&alice_roster, &bob, "Bob", bob_dead_addr, false, &alice_uid);
        seed(&alice_roster, &po_seed, "PO", po_addr, true, &alice_uid);
        // Bob knows Alice (real keys, for decryption) + the PO (to drain).
        let bob_roster = Arc::new(Mutex::new(Roster::default()));
        seed(
            &bob_roster,
            &alice,
            "Alice",
            "127.0.0.1:4000".parse().unwrap(),
            false,
            &bob_uid,
        );
        seed(&bob_roster, &po_seed, "PO", po_addr, true, &bob_uid);

        let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
        let alice_node = Node::open(
            alice,
            alice_roster,
            alice_tx,
            &dir.path().join("alice.log"),
            &dir.path().join("alice-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            bob_tx,
            &dir.path().join("bob.log"),
            &dir.path().join("bob-sent.log"),
            "pw",
        )
        .unwrap();

        // Alice sends while Bob is offline: direct fails, PO replication succeeds.
        // Delivery is provably PO-only here: Alice's direct dial targets a closed
        // port pinned to Bob's identity (cannot succeed), and Bob never dials Alice
        // (his drain only dials the PO) — so anything Bob receives transited the PO.
        alice_node
            .send_dm(&bob_uid, b"held-hello")
            .await
            .expect("send_dm succeeds via the post office despite offline recipient");

        // Bob drains from the PO (retry to absorb the relay's async ingest).
        let mut received = None;
        for _ in 0..25 {
            bob_node.drain_from_post_office().await;
            if let Ok(dm) = tokio::time::timeout(Duration::from_millis(200), bob_rx.recv()).await {
                received = dm;
                break;
            }
        }
        let received = received.expect("Bob received the held DM from the post office");
        assert_eq!(received.from, alice_uid);
        assert_eq!(received.from_name, "Alice");
        assert_eq!(received.text, b"held-hello");
    }

    #[tokio::test]
    async fn history_merges_sent_and_received_in_time_order() {
        let dir = tempfile::tempdir().unwrap();
        let me = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let me_pub = me.public();
        let conv = crate::node::conversation::dm_conversation_id(&me_pub, &bob.public());
        // A signing copy of our identity (the original is moved into the node below).
        let (me_ed, me_x) = me.secret_bytes();
        let me_signer = DeviceIdentity::from_secret_bytes(me_ed, me_x);

        let roster = Arc::new(Mutex::new(Roster::default()));
        roster.lock().unwrap().update(
            &Announce::new(&bob, "Bob", 4000),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &me_pub.user_id(),
        );
        let (tx, _rx) = mpsc::unbounded_channel();
        let node = Node::open(
            me,
            roster,
            tx,
            &dir.path().join("me.log"),
            &dir.path().join("me-sent.log"),
            "pw",
        )
        .unwrap();

        // Bob sent us a message at t=1000 (a received event in our durable log).
        let recv = crate::node::conversation::build_dm_event(
            &bob,
            &me_pub,
            conv,
            1,
            vec![],
            1,
            1000,
            b"hi from bob",
        )
        .unwrap();
        node.log.lock().unwrap().append(recv).unwrap();
        // We sent one at t=2000 (recorded in the sidecar).
        node.sentlog
            .lock()
            .unwrap()
            .record(conv, 1, 2000, b"hi from me")
            .unwrap();
        // Our OWN sent event is also in the durable log (sealed); history must take
        // its plaintext from the sidecar and suppress the self-authored log event —
        // otherwise it would double-list. This event must NOT appear in history.
        let self_in_log = crate::node::conversation::build_dm_event(
            &me_signer,
            &bob.public(),
            conv,
            1,
            vec![],
            1,
            2000,
            b"hi from me",
        )
        .unwrap();
        node.log.lock().unwrap().append(self_in_log).unwrap();

        let hist = node.dm_history(&bob.public(), 10);
        assert_eq!(hist.len(), 2); // self-authored log event suppressed (not 3)
        assert!(!hist[0].from_me && hist[0].text == b"hi from bob"); // t=1000 first
        assert!(hist[1].from_me && hist[1].who == "you" && hist[1].text == b"hi from me");

        // `limit` keeps the most-recent entries.
        let last1 = node.dm_history(&bob.public(), 1);
        assert_eq!(last1.len(), 1);
        assert!(last1[0].from_me); // the newer (t=2000) one
    }

    #[tokio::test]
    async fn reopen_seeds_emitted_so_history_is_not_restreamed() {
        let dir = tempfile::tempdir().unwrap();
        let me = DeviceIdentity::generate();
        let alice = DeviceIdentity::generate();
        let me_pub = me.public();
        let conv = crate::node::conversation::dm_conversation_id(&me_pub, &alice.public());
        let log_path = dir.path().join("me.log");
        let sent_path = dir.path().join("me-sent.log");

        // Prior session: a received DM from Alice is already in the persistent log.
        let old = crate::node::conversation::build_dm_event(
            &alice,
            &me_pub,
            conv,
            1,
            vec![],
            1,
            1000,
            b"old",
        )
        .unwrap();
        {
            let mut log =
                crate::eventlog::persist::PersistentEventLog::open(&log_path, "pw").unwrap();
            log.append(old.clone()).unwrap();
        }

        // Roster knows Alice (so decryption is possible if it were attempted).
        let roster = Arc::new(Mutex::new(Roster::default()));
        roster.lock().unwrap().update(
            &Announce::new(&alice, "Alice", 4000),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &me_pub.user_id(),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let node = Node::open(me, roster, tx, &log_path, &sent_path, "pw").unwrap();

        // Restored history must NOT be re-streamed.
        node.emit_new_messages(conv);
        assert!(rx.try_recv().is_err(), "restored history was re-streamed");

        // A genuinely-new received event (not present at open) IS surfaced.
        let fresh = crate::node::conversation::build_dm_event(
            &alice,
            &me_pub,
            conv,
            2,
            vec![old.id],
            2,
            2000,
            b"new",
        )
        .unwrap();
        node.log.lock().unwrap().append(fresh).unwrap();
        node.emit_new_messages(conv);
        let got = rx.try_recv().expect("a new message is emitted");
        assert_eq!(got.text, b"new");
    }
}
