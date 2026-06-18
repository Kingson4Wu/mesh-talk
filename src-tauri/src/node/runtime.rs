//! The desktop App context for the redesign node: opens a per-user `Node`, starts
//! its network loops (discovery, accept, drain) and an inbound-DM forwarder, and
//! holds the handles. Tauri-agnostic — inbound DMs are delivered via an injected
//! `on_dm` callback, so this is unit-testable without a GUI.

use crate::discovery::service::{run_broadcast, run_listen};
use crate::discovery::{Announce, PeerRecord, Roster};
use crate::eventlog::LogError;
use crate::identity::device::PublicIdentity;
use crate::identity::keystore;
use crate::node::net::discovery_socket;
use crate::node::{HistoryEntry, Node, NodeError, ReceivedDm};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// How often the desktop node drains held DMs from the elected post office.
const DRAIN_INTERVAL_SECS: u64 = 3;

/// Errors starting the redesign runtime.
#[derive(Debug)]
pub enum RuntimeError {
    /// Opening the keystore or the durable logs failed.
    Open(LogError),
    /// Binding the TCP listener or discovery socket failed.
    Io(std::io::Error),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Open(e) => write!(f, "redesign open error: {e}"),
            RuntimeError::Io(e) => write!(f, "redesign io error: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<LogError> for RuntimeError {
    fn from(e: LogError) -> Self {
        RuntimeError::Open(e)
    }
}
impl From<std::io::Error> for RuntimeError {
    fn from(e: std::io::Error) -> Self {
        RuntimeError::Io(e)
    }
}

/// A running redesign node plus the background tasks that drive it. Dropping it
/// aborts every task (clean logout/shutdown).
pub struct RedesignRuntime {
    node: Arc<Node>,
    roster: Arc<Mutex<Roster>>,
    user_id: String,
    tasks: Vec<JoinHandle<()>>,
}

impl RedesignRuntime {
    /// Open the per-account node under `base_dir/redesign/<account_id>/` (keystore +
    /// durable logs encrypted with `password`), advertise `display_name`, and start
    /// discovery + the accept loop + a periodic post-office drain + an inbound
    /// forwarder that calls `on_dm` for each received DM.
    ///
    /// `account_id` is the host app's account identifier — it only namespaces the
    /// data directory so each logged-in user gets their own keystore/logs. It is
    /// distinct from the node's own cryptographic fingerprint ([`Self::user_id`]),
    /// which is derived from the keystore identity and is what peers see.
    pub async fn start(
        base_dir: &Path,
        account_id: &str,
        display_name: &str,
        password: &str,
        discovery_port: u16,
        on_dm: impl Fn(ReceivedDm) + Send + 'static,
        on_channel: impl Fn(crate::node::channel::ReceivedChannelMessage) + Send + 'static,
    ) -> Result<RedesignRuntime, RuntimeError> {
        let dir = base_dir.join("redesign").join(account_id);
        std::fs::create_dir_all(&dir)?;

        let identity = keystore::load_or_create(&dir.join("identity.keystore"), password)
            .map_err(|e| RuntimeError::Open(e.into()))?;
        let self_uid = identity.public().user_id();

        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0u16)).await?;
        let tcp_port = listener.local_addr()?.port();

        // Build the announce BEFORE the identity is moved into the node.
        let announce = Announce::new(&identity, display_name.to_string(), tcp_port);

        let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
        let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<ReceivedDm>();
        let (channel_tx, mut channel_rx) =
            mpsc::unbounded_channel::<crate::node::channel::ReceivedChannelMessage>();
        let node = Node::open(
            identity,
            Arc::clone(&roster),
            incoming_tx,
            channel_tx,
            &dir.join("messages.log"),
            &dir.join("sent.log"),
            password,
        )?;

        let socket = Arc::new(discovery_socket(discovery_port)?);
        let target: SocketAddr = (Ipv4Addr::BROADCAST, discovery_port).into();

        let mut tasks: Vec<JoinHandle<()>> = Vec::new();
        tasks.push(tokio::spawn(run_listen(
            Arc::clone(&socket),
            Arc::clone(&roster),
            self_uid.clone(),
        )));
        tasks.push(tokio::spawn(run_broadcast(
            Arc::clone(&socket),
            announce,
            target,
            Duration::from_secs(2),
        )));
        tasks.push(tokio::spawn(Arc::clone(&node).run_accept_loop(listener)));
        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(async move {
                loop {
                    node.drain_from_post_office().await;
                    tokio::time::sleep(Duration::from_secs(DRAIN_INTERVAL_SECS)).await;
                }
            }));
        }
        tasks.push(tokio::spawn(async move {
            while let Some(dm) = incoming_rx.recv().await {
                on_dm(dm);
            }
        }));
        tasks.push(tokio::spawn(async move {
            while let Some(msg) = channel_rx.recv().await {
                on_channel(msg);
            }
        }));

        Ok(RedesignRuntime {
            node,
            roster,
            user_id: self_uid,
            tasks,
        })
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// A snapshot of currently-known peers.
    pub fn peers(&self) -> Vec<PeerRecord> {
        self.roster
            .lock()
            .expect("roster mutex not poisoned")
            .peers()
    }

    /// The public identity of a known peer (to derive its DM conversation), if known.
    pub fn peer_public(&self, user_id: &str) -> Option<PublicIdentity> {
        self.roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(user_id)
            .map(|p| p.public.clone())
    }

    /// Send a DM to a known peer by user-id.
    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError> {
        self.node.send_dm(recipient, text).await
    }

    /// The last `limit` messages of the DM with `peer`, both directions.
    pub fn history(&self, peer: &PublicIdentity, limit: usize) -> Vec<HistoryEntry> {
        self.node.dm_history(peer, limit)
    }

    /// Create a channel; returns its conversation id.
    pub async fn create_channel(
        &self,
        name: &str,
        members: Vec<PublicIdentity>,
    ) -> Result<crate::eventlog::event::ConversationId, NodeError> {
        self.node.create_channel(name, members).await
    }

    /// Channels this node is a member of.
    pub fn list_channels(&self) -> Vec<crate::node::ChannelSummary> {
        self.node.list_channels()
    }

    /// History of a channel (all senders).
    pub fn channel_history(
        &self,
        channel: crate::eventlog::event::ConversationId,
        limit: usize,
    ) -> Vec<HistoryEntry> {
        self.node.channel_history(channel, limit)
    }

    /// A cloned handle to the underlying node, so an IPC command can snapshot it
    /// and release the `RedesignState` lock before an async send.
    pub fn handle(&self) -> Arc<Node> {
        Arc::clone(&self.node)
    }
}

impl Drop for RedesignRuntime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_opens_a_node_and_exposes_its_identity() {
        let dir = tempfile::tempdir().unwrap();
        // An uncommon discovery port so the test doesn't touch the real one.
        let dp = 47990;
        let runtime = RedesignRuntime::start(
            dir.path(),
            "alice-user-id",
            "Alice",
            "pw",
            dp,
            |_dm| {},
            |_ch| {},
        )
        .await
        .expect("runtime starts");

        // The node opened and exposes a stable fingerprint; no peers yet.
        assert_eq!(runtime.user_id().len(), 32); // user_id is 32 hex chars
        assert!(runtime.peers().is_empty());
        assert!(runtime.peer_public("nobody").is_none());

        // Per-user files were created on disk under redesign/<user_id>/.
        let node_dir = dir.path().join("redesign").join("alice-user-id");
        assert!(node_dir.join("identity.keystore").exists());
        assert!(node_dir.join("messages.log").exists());
        assert!(node_dir.join("sent.log").exists());

        // Reopening the same dir reloads the SAME identity (persistent).
        let runtime2 = RedesignRuntime::start(
            dir.path(),
            "alice-user-id",
            "Alice",
            "pw",
            dp,
            |_dm| {},
            |_ch| {},
        )
        .await
        .expect("runtime reopens");
        assert_eq!(runtime.user_id(), runtime2.user_id());
    }
}
