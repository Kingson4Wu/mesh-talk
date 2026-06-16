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
    use std::net::{IpAddr, Ipv4Addr};
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
        let me = DeviceIdentity::generate();
        let roster = Arc::new(Mutex::new(Roster::default())); // empty
        let (tx, _rx) = mpsc::unbounded_channel();
        let node = Node::new(me, roster, tx);
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

        let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
        let alice_node = Node::new(alice, alice_roster, alice_tx);
        let bob_node = Node::new(bob, bob_roster, bob_tx);

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
}
