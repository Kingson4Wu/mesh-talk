//! The desktop App context for the redesign node: opens a per-user `Node`, starts
//! its network loops (discovery, accept, drain) and an inbound-DM forwarder, and
//! holds the handles. Tauri-agnostic — inbound DMs are delivered via an injected
//! `on_dm` callback, so this is unit-testable without a GUI.

use crate::discovery::service::{run_broadcast, run_listen};
use crate::discovery::{Announce, PeerRecord, Roster};
use crate::eventlog::LogError;
use crate::identity::account_keystore;
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
    account_id: String,
    /// Where the account keystore lives, so a successful device link can persist the
    /// adopted account secret (effective on next login).
    account_path: std::path::PathBuf,
    /// The session password, to re-encrypt the keystore when adopting a linked account.
    password: String,
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
        on_file: impl Fn(crate::node::filebook::ReceivedFile) + Send + 'static,
    ) -> Result<RedesignRuntime, RuntimeError> {
        let dir = base_dir.join("redesign").join(account_id);
        std::fs::create_dir_all(&dir)?;

        let identity = keystore::load_or_create(&dir.join("identity.keystore"), password)
            .map_err(|e| RuntimeError::Open(e.into()))?;
        let self_uid = identity.public().user_id();

        let account = account_keystore::load_or_create(&dir.join("account.keystore"), password)
            .map_err(|e| RuntimeError::Open(e.into()))?;
        let account_id = account.account_id();

        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0u16)).await?;
        let tcp_port = listener.local_addr()?.port();

        // Build the announce BEFORE the identity is moved into the node.
        let announce =
            Announce::new_with_account(&identity, &account, display_name.to_string(), tcp_port);

        let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
        let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<ReceivedDm>();
        let (channel_tx, mut channel_rx) =
            mpsc::unbounded_channel::<crate::node::channel::ReceivedChannelMessage>();
        let (file_tx, mut file_rx) =
            mpsc::unbounded_channel::<crate::node::filebook::ReceivedFile>();
        let node = Node::open_with_account(
            identity,
            account,
            Arc::clone(&roster),
            incoming_tx,
            channel_tx,
            file_tx,
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
        tasks.push(tokio::spawn(async move {
            while let Some(f) = file_rx.recv().await {
                on_file(f);
            }
        }));

        Ok(RedesignRuntime {
            node,
            roster,
            user_id: self_uid,
            account_id,
            account_path: dir.join("account.keystore"),
            password: password.to_string(),
            tasks,
        })
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// This node's cryptographic account id (stable across devices once linking
    /// is wired; today each device generates its own account on first run).
    pub fn account_id(&self) -> &str {
        &self.account_id
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

    /// Send a DM addressed to an account (fan-out to its devices + self-sync to ours).
    pub async fn send_to_account(
        &self,
        target_account_id: &str,
        text: &[u8],
        reply_to: Option<crate::eventlog::event::EventId>,
    ) -> Result<(), NodeError> {
        self.node
            .send_to_account(target_account_id, text, reply_to)
            .await
    }

    /// Account-level conversation history with `peer_account_id`.
    pub fn account_history(&self, peer_account_id: &str, limit: usize) -> Vec<HistoryEntry> {
        self.node.account_history(peer_account_id, limit)
    }

    /// The session password held for re-encrypting the keystore — used only to
    /// re-spawn the runtime when adopting a freshly-linked account (no re-login).
    pub(crate) fn restart_password(&self) -> &str {
        &self.password
    }

    /// Re-key: replace this device's account with a fresh one on disk and return the
    /// new account id. The caller adopts it by re-spawning the runtime. This abandons
    /// the old `account_id` — the honest response to a lost/compromised device in the
    /// shared-account v1 (a lost device keeps the OLD secret, so the only way to lock it
    /// out is to move to a new identity and re-link the devices you still trust). It is
    /// NOT a silent revocation: contacts re-discover you under the new account, and your
    /// other good devices must be re-linked.
    pub fn rekey_account(&self) -> Result<String, NodeError> {
        let account = crate::identity::account::Account::generate();
        crate::identity::account_keystore::save(&self.account_path, &self.password, &account)
            .map_err(|e| NodeError::Channel(format!("persist new account: {e}")))?;
        Ok(account.account_id())
    }

    /// Enter "link a device" mode; returns the one-time code to display.
    pub fn start_linking(&self) -> String {
        self.node.start_linking()
    }

    /// Leave "link a device" mode.
    pub fn stop_linking(&self) {
        self.node.stop_linking()
    }

    /// Link THIS device to an existing device `peer_user_id` using `code`. On success
    /// persists the adopted account secret to disk (effective next login) and returns
    /// the adopted account id.
    pub async fn link_device(&self, peer_user_id: &str, code: &str) -> Result<String, NodeError> {
        let peer = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            roster
                .get(peer_user_id)
                .cloned()
                .ok_or_else(|| NodeError::UnknownPeer(peer_user_id.to_string()))?
        };
        let linked = self
            .node
            .link_to_device(peer.addr, &peer.public, code)
            .await?;
        let account = crate::identity::account::Account::from_secret_bytes(linked.secret);
        crate::identity::account_keystore::save(&self.account_path, &self.password, &account)
            .map_err(|e| NodeError::Channel(format!("persist linked account: {e}")))?;
        Ok(linked.account_id)
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

    /// The current members of a channel (empty if unknown).
    pub fn channel_members(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Vec<PublicIdentity> {
        self.node.channel_members(channel)
    }

    /// History of a channel (all senders).
    pub fn channel_history(
        &self,
        channel: crate::eventlog::event::ConversationId,
        limit: usize,
    ) -> Vec<HistoryEntry> {
        self.node.channel_history(channel, limit)
    }

    /// Search all DMs + channels for `query`.
    pub fn search(&self, query: &str) -> Vec<crate::node::SearchHit> {
        self.node.search(query)
    }

    /// Aggregated reactions in the DM with `peer`.
    pub fn reactions_dm(&self, peer: &PublicIdentity) -> Vec<crate::node::reaction::ReactionView> {
        self.node.reactions_dm(peer)
    }

    /// Aggregated reactions in a channel.
    pub fn channel_reactions(
        &self,
        channel: crate::eventlog::event::ConversationId,
    ) -> Vec<crate::node::reaction::ReactionView> {
        self.node.reactions(channel)
    }

    /// Aggregated reactions in the account conversation with `peer_account_id`.
    pub fn account_reactions(
        &self,
        peer_account_id: &str,
    ) -> Vec<crate::node::reaction::ReactionView> {
        self.node.account_reactions(peer_account_id)
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
            |_f| {},
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
            |_f| {},
        )
        .await
        .expect("runtime reopens");
        assert_eq!(runtime.user_id(), runtime2.user_id());

        // The cryptographic account is created on disk and stable across reopen.
        assert_eq!(runtime.account_id().len(), 32); // account_id is 32 hex chars
        assert!(node_dir.join("account.keystore").exists());
        assert_eq!(runtime.account_id(), runtime2.account_id());
    }
}
