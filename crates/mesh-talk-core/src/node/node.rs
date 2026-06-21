//! The node App context: owns the device identity, an in-memory event log, and a
//! shared roster; wires DM crypto + the event log + the networked sync driver
//! into `send_dm` (outbound) and a per-connection serve loop (inbound) that
//! surfaces received DMs on an mpsc channel. No global singletons. Starting
//! discovery and binding the TCP listener is the binary's job (next plan); this
//! module provides the pieces and proves them with an in-process two-node
//! exchange over loopback TCP.

use crate::channel::sender_key::SenderKey;
use crate::discovery::roster::{Roster, UserId};
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::persist::PersistentEventLog;
use crate::eventlog::LogError;
use crate::identity::account::Account;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::channel::{ChannelBook, ReceivedChannelMessage};
use crate::node::channel_senders::ChannelSenderStore;
use crate::node::dm_ratchet::DmRatchet;
use crate::node::filebook::{FileBook, ReceivedFile};
use crate::node::pairing::PendingLink;
use crate::node::ratchet_sessions::RatchetSessions;
use crate::node::reaction::ReactionPayload;
use crate::node::received_log::ReceivedLog;
use crate::node::sentlog::SentLog;
use crate::node::session::SessionError;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Maximum raw file size for `send_file_*`. Bounded multi-round sync transfers a
/// file's chunk events across frames now, so the practical limit is the `have`
/// id-set size (a conversation's full id-set still travels in one frame): ~8 MB of
/// 48 KiB chunks ≈ 180 ids ≈ 7 KB, comfortably within a frame. Lifting further
/// needs id-set compaction (a separate slice).
pub(in crate::node) const MAX_FILE_SIZE: usize = 8 * 1024 * 1024;

/// A received direct message, surfaced to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedDm {
    pub from: UserId,
    pub from_name: String,
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
}

/// Account material a device receives when it links to an existing device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedAccount {
    pub secret: [u8; 32],
    pub account_id: String,
}

/// A channel summary for listing in the UI.
#[derive(Debug, Clone)]
pub struct ChannelSummary {
    pub id: ConversationId,
    pub name: String,
    pub member_count: usize,
}

/// A search match: a message whose text contains the query, with enough context to
/// label it and navigate to its conversation.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub is_channel: bool,
    /// The peer user-id (DM) or channel id hex (channel) — the navigation target.
    pub target: String,
    /// The peer or channel display name.
    pub label: String,
    pub from_me: bool,
    pub who: String,
    pub text: Vec<u8>,
    pub wall_clock: u64,
}

/// A merged conversation-history entry (sent or received), for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: EventId,
    pub from_me: bool,
    pub who: String,
    pub text: Vec<u8>,
    pub wall_clock: u64,
    pub reply_to: Option<EventId>,
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
    /// A channel operation failed (unknown channel, or key/message sealing).
    Channel(String),
    /// A file operation failed (read/seal/reassemble/write, unknown file, or a file
    /// too large for the current single-frame sync — see `send_file_*`).
    File(String),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeError::UnknownPeer(u) => write!(f, "unknown peer: {u}"),
            NodeError::Seal(e) => write!(f, "seal error: {e}"),
            NodeError::Log(e) => write!(f, "log error: {e}"),
            NodeError::Session(e) => write!(f, "session error: {e}"),
            NodeError::Channel(m) => write!(f, "channel error: {m}"),
            NodeError::File(m) => write!(f, "file error: {m}"),
        }
    }
}

impl std::error::Error for NodeError {}

/// The node: identity + event log + shared roster + an outbound stream of
/// received DMs. Construct with [`Node::open`]; share as `Arc<Node>`.
pub struct Node {
    // Fields are `pub(in crate::node)` so the per-domain `impl Node` blocks in sibling
    // files (dm/channels/files/queries/linking/serving) can reach them; they stay
    // private to the `node` module (no wider exposure).
    pub(in crate::node) identity: DeviceIdentity,
    /// This node's cryptographic account (cross-device handle). A per-device account
    /// derived from the device key when opened via [`Node::open`]; the real shared
    /// account is injected by [`Node::open_with_account`]. Holds the secret so this
    /// device can serve a pairing request (transfer the account to a new device).
    pub(in crate::node) account: Account,
    /// One-time pairing code while in "link a device" mode (linker side). Set by
    /// [`Node::start_linking`], cleared on success or [`Node::stop_linking`].
    pub(in crate::node) pending_link: Mutex<Option<PendingLink>>,
    pub(in crate::node) log: Mutex<PersistentEventLog>,
    pub(in crate::node) sentlog: Mutex<SentLog>,
    pub(in crate::node) roster: Arc<Mutex<Roster>>,
    pub(in crate::node) incoming: mpsc::UnboundedSender<ReceivedDm>,
    pub(in crate::node) channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
    pub(in crate::node) channels: Mutex<ChannelBook>,
    /// Durable, encrypted store of this node's per-channel sending keys (`my_sender`).
    /// `my_sender` is in-memory only and rebuilt empty on restart; persisting it here
    /// lets us resume our sending chains so receivers (first-wins on chains they hold)
    /// can still decrypt our post-restart messages.
    pub(in crate::node) channel_senders: Mutex<ChannelSenderStore>,
    pub(in crate::node) emitted: Mutex<HashSet<EventId>>,
    pub(in crate::node) file_incoming: mpsc::UnboundedSender<ReceivedFile>,
    pub(in crate::node) files: Mutex<FileBook>,
    /// Per-peer Double Ratchet sessions (forward-secret DM crypto), encrypted on disk.
    pub(in crate::node) dm_ratchet: Mutex<DmRatchet>,
    /// Decrypted received-message plaintext, for serving history after the wire key is gone.
    pub(in crate::node) received: Mutex<ReceivedLog>,
    /// Durable record of file manifests we've SURFACED (reuses the ReceivedLog format),
    /// so the file book's emitted set + manifests survive restart without marking
    /// never-opened manifests as emitted (which would lose the file). See `Node::open`.
    pub(in crate::node) received_files: Mutex<ReceivedLog>,
    /// Own DM reactions: sealed to the peer so un-openable from our own log. Stored in
    /// memory (with the wall-clock we made each at) so `reactions` can merge them and
    /// resolve toggles by recency independent of merge order. `(conv, wall_clock_ms,
    /// payload)`. Lost on restart (MVP limitation).
    pub(in crate::node) my_dm_reactions: Mutex<Vec<(ConversationId, u64, ReactionPayload)>>,
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
        channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
        file_incoming: mpsc::UnboundedSender<ReceivedFile>,
        log_path: &Path,
        sent_path: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        // Default: a deterministic per-device account derived from the device key, so
        // each unlinked device is its own stable account until it links (Plan 4).
        let account = Account::from_secret_bytes(identity.secret_bytes().0);
        Self::open_with_account(
            identity,
            account,
            roster,
            incoming,
            channel_incoming,
            file_incoming,
            log_path,
            sent_path,
            password,
        )
    }

    /// Open a node bound to an explicit cryptographic `account` (the cross-device
    /// handle). Used by the real app/CLI, which load the account from its keystore;
    /// most tests use [`Node::open`], which defaults the account to the device key.
    #[allow(clippy::too_many_arguments)]
    pub fn open_with_account(
        identity: DeviceIdentity,
        account: Account,
        roster: Arc<Mutex<Roster>>,
        incoming: mpsc::UnboundedSender<ReceivedDm>,
        channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
        file_incoming: mpsc::UnboundedSender<ReceivedFile>,
        log_path: &Path,
        sent_path: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        let dir = log_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        // Each store does its own slow, CPU-bound password KDF over an independent
        // file. Open them on scoped threads so the KDFs run in parallel: on a
        // multi-core machine the wall-clock cost collapses to roughly one KDF.
        let (log_res, sent_res, sessions_res, received_res, csenders_res, recv_files_res) =
            std::thread::scope(|s| {
                let h_log = s.spawn(|| PersistentEventLog::open(log_path, password));
                let h_sent = s.spawn(|| SentLog::open(sent_path, password));
                let h_sess =
                    s.spawn(|| RatchetSessions::open(&dir.join("ratchet.sessions"), password));
                let h_recv = s.spawn(|| ReceivedLog::open(&dir.join("received.log"), password));
                let h_csnd =
                    s.spawn(|| ChannelSenderStore::open(&dir.join("channel.senders"), password));
                // Durable record of surfaced file manifests (reuses the ReceivedLog format),
                // so the file book's emitted set survives restart WITHOUT marking unopened
                // manifests as emitted — see the seeding below.
                let h_files =
                    s.spawn(|| ReceivedLog::open(&dir.join("received_files.log"), password));
                (
                    h_log.join(),
                    h_sent.join(),
                    h_sess.join(),
                    h_recv.join(),
                    h_csnd.join(),
                    h_files.join(),
                )
            });
        // A join() error means the opening thread panicked; re-raise the panic so it
        // is not silently swallowed. A normal open failure surfaces as the inner Err.
        let log = log_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let sentlog = sent_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let sessions = sessions_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let received = received_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let channel_senders = csenders_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let received_files = recv_files_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let dm_ratchet = DmRatchet::new(sessions);
        // Seed `emitted` with the events we have ALREADY recorded to the received store — NOT
        // every id in the log. An event that was ingested durably but never recorded (it
        // arrived before we learned its author — the DM-convergence race — or before its
        // ratchet key was derivable) must stay eligible for `emit_new_messages` to retry after
        // a restart; seeding it as emitted here would drop it from history forever. Self-
        // authored events are excluded by the author check in `emit_new_messages`, so they
        // need not be seeded.
        let emitted: HashSet<EventId> = received
            .conversations()
            .iter()
            .flat_map(|c| received.entries(c))
            .map(|e| e.event_id)
            .collect();
        let mut channels = ChannelBook::new();
        for conv in log.conversations() {
            let events = log.events(&conv);
            let _ = channels.process(&identity, conv, &events);
        }
        // Restore our persisted sending chains into each channel state. The event log
        // only carries OTHER members' sender keys, so `my_sender` rebuilds empty above;
        // without this, a post-restart send would mint a fresh chain that receivers
        // (first-wins on the chain they already hold) cannot follow.
        for channel in channels.channel_ids() {
            for (epoch, bytes) in channel_senders.get(&channel) {
                if let Some(sk) = SenderKey::deserialize(&bytes) {
                    if let Some(state) = channels.state_mut(&channel) {
                        state.import_my_sender(epoch, sk);
                    }
                }
            }
        }
        // Seed the file book from the durable `received_files` record — the manifests we
        // actually SURFACED last session (restoring them so they stay saveable), and only
        // THOSE ids into the emitted set. A manifest that was ingested durably but never
        // opened (its author not yet in the roster — the DM-convergence race) is absent
        // here, so `process_file_events` retries it after restart instead of dropping the
        // file forever. (Mirrors the message-path `emitted` seeding above.)
        let mut files = FileBook::new();
        for c in received_files.conversations() {
            for entry in received_files.entries(&c) {
                if let Some(manifest) = crate::file::FileManifest::decode(&entry.plaintext) {
                    files.record(manifest);
                }
                files.mark_emitted(entry.event_id);
            }
        }
        Ok(Arc::new(Self {
            identity,
            account,
            pending_link: Mutex::new(None),
            log: Mutex::new(log),
            sentlog: Mutex::new(sentlog),
            roster,
            incoming,
            channel_incoming,
            channels: Mutex::new(channels),
            channel_senders: Mutex::new(channel_senders),
            emitted: Mutex::new(emitted),
            file_incoming,
            files: Mutex::new(files),
            dm_ratchet: Mutex::new(dm_ratchet),
            received: Mutex::new(received),
            received_files: Mutex::new(received_files),
            my_dm_reactions: Mutex::new(Vec::new()),
        }))
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> UserId {
        self.identity.public().user_id()
    }

    /// This node's cryptographic account id (cross-device handle).
    pub fn account_id(&self) -> String {
        self.account.account_id()
    }

    /// Summaries of all channels this node is a member of.
    pub fn list_channels(&self) -> Vec<ChannelSummary> {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        book.channel_ids()
            .into_iter()
            .filter_map(|id| {
                book.state(&id).map(|s| ChannelSummary {
                    id,
                    name: s.name().to_string(),
                    member_count: s.members().len(),
                })
            })
            .collect()
    }

    /// The current members of `channel` (empty if the channel is unknown).
    pub fn channel_members(&self, channel: ConversationId) -> Vec<PublicIdentity> {
        let book = self.channels.lock().expect("channels mutex not poisoned");
        book.state(&channel)
            .map(|s| s.members().to_vec())
            .unwrap_or_default()
    }

    /// Append a channel event, sequencing it from the channel's log position. Returns
    /// the event's `seq` (its position in our own per-author chain), used to index the
    /// sent sidecar for channel history.
    pub(in crate::node) fn append_event(
        &self,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) -> Result<u64, NodeError> {
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
        Ok(seq)
    }
}

/// Milliseconds since the Unix epoch, for an event's wall-clock field.
pub(in crate::node) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A fresh random 32-byte logical message id (shared across an account send's
/// per-device copies, so reactions/replies can target it account-wide).
pub(in crate::node) fn random_msg_id() -> [u8; 32] {
    use rand_core::RngCore;
    let mut id = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut id);
    id
}

#[cfg(test)]
#[path = "node_tests.rs"]
mod tests;
