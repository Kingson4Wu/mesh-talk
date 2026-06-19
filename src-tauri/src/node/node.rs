//! The node App context: owns the device identity, an in-memory event log, and a
//! shared roster; wires DM crypto + the event log + the networked sync driver
//! into `send_dm` (outbound) and a per-connection serve loop (inbound) that
//! surfaces received DMs on an mpsc channel. No global singletons. Starting
//! discovery and binding the TCP listener is the binary's job (next plan); this
//! module provides the pieces and proves them with an in-process two-node
//! exchange over loopback TCP.

use crate::channel::sender_key::SenderKey;
use crate::channel::ChannelMeta;
use crate::discovery::roster::{PeerRecord, Roster, UserId};
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::eventlog::persist::PersistentEventLog;
use crate::eventlog::LogError;
use crate::file::{
    file_checksum, reassemble_and_verify, seal_chunk, split_chunks, FileKey, FileManifest,
};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::channel::{seal_keys_for, ChannelBook, ReceivedChannelMessage};
use crate::node::channel_senders::ChannelSenderStore;
use crate::node::conversation::{account_conversation_id, dm_conversation_id};
use crate::node::dm_envelope::DmEnvelope;
use crate::node::dm_ratchet::DmRatchet;
use crate::node::filebook::{FileBook, ReceivedFile};
use crate::node::message::MessageBody;
use crate::node::postbox::elected_post_office;
use crate::node::ratchet_sessions::RatchetSessions;
use crate::node::reaction::{aggregate, ReactionPayload, ReactionView};
use crate::node::received_log::ReceivedLog;
use crate::node::sentlog::SentLog;
use crate::node::session::{request_round, serve_one, Served, SessionError};
use crate::node::transport::{accept, dial};
use crate::transport::SecureChannel;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// Maximum raw file size for `send_file_*`. Bounded multi-round sync transfers a
/// file's chunk events across frames now, so the practical limit is the `have`
/// id-set size (a conversation's full id-set still travels in one frame): ~8 MB of
/// 48 KiB chunks ≈ 180 ids ≈ 7 KB, comfortably within a frame. Lifting further
/// needs id-set compaction (a separate slice).
const MAX_FILE_SIZE: usize = 8 * 1024 * 1024;

/// A received direct message, surfaced to the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedDm {
    pub from: UserId,
    pub from_name: String,
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
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
    identity: DeviceIdentity,
    /// This node's cryptographic account id (cross-device handle). Defaults to the
    /// device's own user-id when opened via [`Node::open`]; the real per-account id is
    /// injected by [`Node::open_with_account`]. Account-addressed sends route by this.
    account_id: String,
    log: Mutex<PersistentEventLog>,
    sentlog: Mutex<SentLog>,
    roster: Arc<Mutex<Roster>>,
    incoming: mpsc::UnboundedSender<ReceivedDm>,
    channel_incoming: mpsc::UnboundedSender<ReceivedChannelMessage>,
    channels: Mutex<ChannelBook>,
    /// Durable, encrypted store of this node's per-channel sending keys (`my_sender`).
    /// `my_sender` is in-memory only and rebuilt empty on restart; persisting it here
    /// lets us resume our sending chains so receivers (first-wins on chains they hold)
    /// can still decrypt our post-restart messages.
    channel_senders: Mutex<ChannelSenderStore>,
    emitted: Mutex<HashSet<EventId>>,
    file_incoming: mpsc::UnboundedSender<ReceivedFile>,
    files: Mutex<FileBook>,
    // wired up in Task 2
    #[allow(dead_code)]
    dm_ratchet: Mutex<DmRatchet>,
    // wired up in Task 2
    #[allow(dead_code)]
    received: Mutex<ReceivedLog>,
    /// Own DM reactions: sealed to the peer so un-openable from our own log.
    /// Stored in memory so `reactions` can merge them. Lost on restart (MVP limitation).
    my_dm_reactions: Mutex<Vec<(ConversationId, ReactionPayload)>>,
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
        let account_id = identity.public().user_id();
        Self::open_with_account(
            identity,
            account_id,
            roster,
            incoming,
            channel_incoming,
            file_incoming,
            log_path,
            sent_path,
            password,
        )
    }

    /// Open a node bound to an explicit cryptographic `account_id` (the cross-device
    /// handle). Used by the real app/CLI, which load the account from its keystore;
    /// most tests use [`Node::open`], which defaults the account to the device user-id.
    #[allow(clippy::too_many_arguments)]
    pub fn open_with_account(
        identity: DeviceIdentity,
        account_id: String,
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
        let (log_res, sent_res, sessions_res, received_res, csenders_res) =
            std::thread::scope(|s| {
                let h_log = s.spawn(|| PersistentEventLog::open(log_path, password));
                let h_sent = s.spawn(|| SentLog::open(sent_path, password));
                let h_sess =
                    s.spawn(|| RatchetSessions::open(&dir.join("ratchet.sessions"), password));
                let h_recv = s.spawn(|| ReceivedLog::open(&dir.join("received.log"), password));
                let h_csnd =
                    s.spawn(|| ChannelSenderStore::open(&dir.join("channel.senders"), password));
                (
                    h_log.join(),
                    h_sent.join(),
                    h_sess.join(),
                    h_recv.join(),
                    h_csnd.join(),
                )
            });
        // A join() error means the opening thread panicked; re-raise the panic so it
        // is not silently swallowed. A normal open failure surfaces as the inner Err.
        let log = log_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let sentlog = sent_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let sessions = sessions_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let received = received_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let channel_senders = csenders_res.unwrap_or_else(|e| std::panic::resume_unwind(e))?;
        let dm_ratchet = DmRatchet::new(sessions);
        let emitted: HashSet<EventId> = log.all_event_ids().into_iter().collect();
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
        // Seed the file book's emitted set so existing manifests are not re-surfaced
        // after restart. Opening them needs roster/channel crypto unavailable at open
        // time — persisting opened manifests across restart is deferred.
        let mut files = FileBook::new();
        for conv in log.conversations() {
            for event in log.events(&conv) {
                if event.kind == EventKind::FileManifest {
                    files.mark_emitted(event.id);
                }
            }
        }
        Ok(Arc::new(Self {
            identity,
            account_id,
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
            my_dm_reactions: Mutex::new(Vec::new()),
        }))
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> UserId {
        self.identity.public().user_id()
    }

    /// This node's cryptographic account id (cross-device handle).
    pub fn account_id(&self) -> &str {
        &self.account_id
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

    /// Send a DM to `recipient` (a known peer): seal it, append the Message event
    /// locally, then deliver it. Delivery is best-effort DIRECT (the recipient may
    /// be offline) plus ALWAYS replicating to the elected post office (so an
    /// offline recipient can retrieve it later). Succeeds if EITHER the direct
    /// delivery or a post office accepted the event; errors only if the recipient
    /// is unreachable and no post office is available.
    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError> {
        self.send_dm_reply(recipient, text, None).await
    }

    /// Send a DM that replies to `reply_to` (or `None` for a normal message).
    pub async fn send_dm_reply(
        &self,
        recipient: &str,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;

        let wrapped = MessageBody::new(text.to_vec(), reply_to).encode();
        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Seal the wrapped body with the Double Ratchet (forward-secret). Compute the
        // wire OUTSIDE the log lock so no lock spans the seal.
        let wire = {
            let mut r = self
                .dm_ratchet
                .lock()
                .expect("dm_ratchet mutex not poisoned");
            r.encrypt(&self.identity, &peer.public, &wrapped)
                .map_err(NodeError::Log)?
        };
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
            let event = Event::new(
                &self.identity,
                conv,
                seq,
                parents,
                lamport,
                wall_clock,
                EventKind::Message,
                wire,
            );
            log.append(event).map_err(NodeError::Log)?;
        }

        // Keep a local plaintext copy of what we sent (the ratchet wire key is
        // single-use and not self-decryptable). Best-effort: a sidecar write error
        // doesn't fail a message that was sealed, appended, and about to be delivered.
        let _ = self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .record(conv, seq, wall_clock, &wrapped);

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

    /// Send a DM to an ACCOUNT: fan out a per-device ratcheted copy to every known
    /// device of `target_account_id`, and self-sync a copy to this user's own other
    /// devices. Records ONE sent entry under the account conversation (so own history
    /// shows it once). Best-effort delivery per device (direct, else post office).
    /// Errors only if there is no known device of the target account to send to.
    pub async fn send_to_account(
        &self,
        target_account_id: &str,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        let my_account = self.account_id.clone();
        let inner = MessageBody::new(text.to_vec(), reply_to).encode();
        let envelope = DmEnvelope::new(
            my_account.clone(),
            target_account_id.to_string(),
            inner.clone(),
        )
        .encode();

        // Resolve destinations: the target account's devices + our OWN other devices.
        let me = self.identity.public().user_id();
        let (targets, own): (Vec<PeerRecord>, Vec<PeerRecord>) = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            let targets = roster
                .peers()
                .into_iter()
                .filter(|p| p.account_id.as_deref() == Some(target_account_id))
                .collect();
            let own = roster
                .peers()
                .into_iter()
                .filter(|p| p.account_id.as_deref() == Some(my_account.as_str()))
                .filter(|p| p.public.user_id() != me)
                .collect();
            (targets, own)
        };

        if targets.is_empty() {
            return Err(NodeError::UnknownPeer(target_account_id.to_string()));
        }

        // Record one plaintext copy for our own account history (the from_me side).
        let conv_account = account_conversation_id(&my_account, target_account_id);
        let wall_clock = now_millis();
        {
            let mut sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            let seq = sentlog.entries(&conv_account).len() as u64 + 1;
            let _ = sentlog.record(conv_account, seq, wall_clock, &inner);
        }

        // Fan out: seal+append+deliver one copy per destination device.
        for peer in targets.iter().chain(own.iter()) {
            self.deliver_enveloped(peer, &envelope).await;
        }
        Ok(())
    }

    /// Seal `plaintext` to `peer`'s device ratchet, append it to the device-pair
    /// conversation's event log, and deliver it (direct, then post office). Best-effort:
    /// transport failures are swallowed (the event is durably logged and syncs later).
    /// The conversation id here is the per-DEVICE-pair id (the transport layer).
    async fn deliver_enveloped(&self, peer: &PeerRecord, plaintext: &[u8]) {
        let conv = dm_conversation_id(&self.identity.public(), &peer.public);
        let wire = {
            let mut r = self
                .dm_ratchet
                .lock()
                .expect("dm_ratchet mutex not poisoned");
            match r.encrypt(&self.identity, &peer.public, plaintext) {
                Ok(w) => w,
                Err(_) => return,
            }
        };
        if self.append_event(conv, EventKind::Message, wire).is_err() {
            return;
        }
        let _ = self.deliver_direct(peer, conv).await;
        let _ = self.replicate_to_post_office(conv).await;
        self.emit_new_messages(conv);
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
            self.process_channel(conv);
            self.process_file_events(conv);
        }
    }

    /// Open and surface any new `FileManifest` events in `conv`. Channel manifests
    /// open with the channel group key; DM manifests with the DM sealed-box (the
    /// author's X25519 from the roster). A no-op for conversations with no new
    /// manifest events. Own manifests are skipped (the sender already knows).
    fn process_file_events(&self, conv: ConversationId) {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let manifest_events: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| e.kind == EventKind::FileManifest && e.author != self_author)
                .cloned()
                .collect()
        };
        if manifest_events.is_empty() {
            return;
        }
        // Is this a channel conversation? If so, open with its group key.
        let is_channel = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&conv).is_some()
        };
        let mut surfaced: Vec<ReceivedFile> = Vec::new();
        for event in manifest_events {
            if self
                .files
                .lock()
                .expect("files mutex not poisoned")
                .is_emitted(&event.id)
            {
                continue;
            }
            // NB: we mark emitted only AFTER a successful open+decode (below), so a
            // manifest we can't open yet — e.g. a DM whose sender isn't in the roster
            // until discovery catches up — is retried on a later sync, not lost.
            let plaintext = if is_channel {
                let mut book = self.channels.lock().expect("channels mutex not poisoned");
                match book
                    .state_mut(&conv)
                    .and_then(|s| s.open_sender_message(&event.ciphertext))
                {
                    Some(p) => p,
                    None => continue,
                }
            } else {
                let author_uid = event.author.user_id();
                let sender_x25519 = {
                    let roster = self.roster.lock().expect("roster mutex not poisoned");
                    match roster.get(&author_uid) {
                        Some(p) => p.public.x25519_pub,
                        None => continue, // author unknown → can't open yet
                    }
                };
                match crate::dm::open(&self.identity, &sender_x25519, &event.ciphertext) {
                    Ok(p) => p,
                    Err(_) => continue,
                }
            };
            let Some(manifest) = FileManifest::decode(&plaintext) else {
                continue;
            };
            let received = ReceivedFile {
                conv,
                from: event.author.user_id(),
                name: manifest.name.clone(),
                size: manifest.size,
                mime: manifest.mime.clone(),
                file_conv: manifest.file_conv,
            };
            {
                let mut files = self.files.lock().expect("files mutex not poisoned");
                files.mark_emitted(event.id);
                files.record(manifest);
            }
            surfaced.push(received);
        }
        for rf in surfaced {
            let _ = self.file_incoming.send(rf);
        }
    }

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
            // Persist the decrypted plaintext: the sender-key wire key is single-use,
            // so channel history is served from the received store, not re-opened.
            let _ = self
                .received
                .lock()
                .expect("received mutex not poisoned")
                .record(
                    conv,
                    msg.from.clone(),
                    msg.wall_clock,
                    &msg.wrapped,
                    msg.event_id,
                );
            let _ = self.channel_incoming.send(msg);
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
            self.process_file_events(conv);
        }
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
            self.process_file_events(cid);
        }
        // Drain per-file conversations the file book knows (so a recipient pulls
        // chunks the PO holds).
        let file_convs: Vec<ConversationId> = {
            let book = self.files.lock().expect("files mutex not poisoned");
            book.file_convs()
        };
        for fc in file_convs {
            if request_round(&mut channel, &self.log, fc).await.is_err() {
                return;
            }
        }
    }

    /// Create a channel named `name` with `members` (the creator is added
    /// automatically). Mints a channel id, posts the membership event, builds our own
    /// channel state, then distributes our epoch-0 sender-key distribution to the
    /// members via a `KeyRotation` event so they can follow our chain.
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
        let meta = ChannelMeta {
            name: name.to_string(),
            members: members.clone(),
            epoch: 0,
        };

        self.append_event(channel, EventKind::MembershipChange, meta.encode())?;
        // Build our own channel state (sets our identity for the sender-key path).
        self.process_channel(channel);
        // Distribute our epoch-0 sender-key distribution to every member.
        self.distribute_my_sender_key(channel, &members)?;

        self.distribute_channel(channel, &members).await;
        Ok(channel)
    }

    /// Generate (if needed) our current-epoch sender-key distribution for `channel`
    /// and post a `KeyRotation` event carrying it sealed to each member. Members open
    /// their entry to follow our chain from `n = 0`. Called at create and on each
    /// membership change so members at a new epoch can read our new messages.
    fn distribute_my_sender_key(
        &self,
        channel: ConversationId,
        members: &[PublicIdentity],
    ) -> Result<(), NodeError> {
        let sealed = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            let epoch = state.epoch();
            let skd = state.my_sender_distribution();
            seal_keys_for(&self.identity, members, &skd, epoch)
                .map_err(|e| NodeError::Channel(format!("key sealing failed: {e}")))?
        };
        self.append_event(channel, EventKind::KeyRotation, sealed.encode())?;
        self.persist_my_senders(channel);
        Ok(())
    }

    /// Snapshot `channel`'s `my_sender` (under the channels lock) and persist it to the
    /// durable store so our sending chains survive a restart. Best-effort: a store
    /// write error must not fail an already-appended message. No `MutexGuard` is held
    /// across the persist (the snapshot is taken first, then the lock is dropped).
    fn persist_my_senders(&self, channel: ConversationId) {
        let snapshot = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.state(&channel).map(|s| s.export_my_senders())
        };
        if let Some(senders) = snapshot {
            let _ = self
                .channel_senders
                .lock()
                .expect("channel_senders mutex not poisoned")
                .put(&channel, &senders);
        }
    }

    /// Add `new_member` to `channel`, bumping the epoch. Posts a `MembershipChange`
    /// carrying the new member set at `old_epoch + 1`, adopts it locally, then pushes
    /// the channel to the new member set (so the joiner gets the membership event). The
    /// fresh-epoch sender key is distributed lazily on our next send (the
    /// `has_my_sender(new_epoch)` guard in the send path), so the joiner can read
    /// post-join messages but not pre-join ones.
    pub async fn add_channel_member(
        &self,
        channel: ConversationId,
        new_member: PublicIdentity,
    ) -> Result<(), NodeError> {
        let (name, mut new_members, new_epoch) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            (
                state.name().to_string(),
                state.members().to_vec(),
                state.epoch() + 1,
            )
        };
        if !new_members
            .iter()
            .any(|m| m.user_id() == new_member.user_id())
        {
            new_members.push(new_member);
        }
        let meta = ChannelMeta {
            name,
            members: new_members.clone(),
            epoch: new_epoch,
        };
        self.append_event(channel, EventKind::MembershipChange, meta.encode())?;
        self.process_channel(channel);
        self.distribute_channel(channel, &new_members).await;
        Ok(())
    }

    /// Remove the member with `member_user_id` from `channel`, bumping the epoch. Posts
    /// a `MembershipChange` carrying the reduced member set at `old_epoch + 1`, adopts it
    /// locally, then pushes the channel to the remaining members only — the removed
    /// member never receives the new epoch's events or sender keys, so it cannot read
    /// post-removal messages.
    pub async fn remove_channel_member(
        &self,
        channel: ConversationId,
        member_user_id: &str,
    ) -> Result<(), NodeError> {
        let (name, mut new_members, new_epoch) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            (
                state.name().to_string(),
                state.members().to_vec(),
                state.epoch() + 1,
            )
        };
        new_members.retain(|m| m.user_id() != member_user_id);
        let meta = ChannelMeta {
            name,
            members: new_members.clone(),
            epoch: new_epoch,
        };
        self.append_event(channel, EventKind::MembershipChange, meta.encode())?;
        self.process_channel(channel);
        self.distribute_channel(channel, &new_members).await;
        Ok(())
    }

    /// Send a message to a channel we hold the key for. Distribution is best-effort
    /// and fail-soft: returns `Ok(())` once the event is appended locally, regardless
    /// of how many members were reachable (an offline member catches up via sync).
    pub async fn send_channel_message(
        &self,
        channel: ConversationId,
        text: &[u8],
    ) -> Result<(), NodeError> {
        self.send_channel_message_reply(channel, text, None).await
    }

    /// Send a channel message that replies to `reply_to` (or `None` for a normal message).
    pub async fn send_channel_message_reply(
        &self,
        channel: ConversationId,
        text: &[u8],
        reply_to: Option<EventId>,
    ) -> Result<(), NodeError> {
        let wrapped = MessageBody::new(text.to_vec(), reply_to).encode();
        // Ensure this node's sender-key distribution for the current epoch is published
        // BEFORE the first seal (which generates+ratchets our chain to n=1). Without
        // this, a non-creator member's messages are undecryptable by everyone.
        let (members, already_distributed) = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            let epoch = state.epoch();
            (state.members().to_vec(), state.has_my_sender(epoch))
        };
        if !already_distributed {
            self.distribute_my_sender_key(channel, &members)?;
        }
        let payload = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            state
                .seal_sender_message(&wrapped)
                .map_err(|e| NodeError::Channel(format!("message sealing failed: {e}")))?
        };
        // Persist our advanced sending chain so a post-restart send resumes THIS chain
        // (receivers are first-wins and keep the chain they already hold).
        self.persist_my_senders(channel);
        let seq = self.append_event(channel, EventKind::Message, payload)?;
        // Keep a local plaintext copy of what we sent — the sender-key wire key is
        // single-use and not self-decryptable, so channel history is served from the
        // stores. Best-effort: a sidecar write error doesn't fail a sealed+appended
        // message.
        let _ = self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .record(channel, seq, now_millis(), &wrapped);
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    /// Send the file at `path` to a DM peer. Chunks + seals it into a fresh per-file
    /// conversation, seals the manifest to the recipient, posts a `FileManifest`
    /// event into the DM conversation, and distributes both. Returns the per-file
    /// conversation id (the handle for the recipient to save).
    pub async fn send_file_dm(
        &self,
        recipient: &str,
        path: &Path,
    ) -> Result<ConversationId, NodeError> {
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?;

        let (manifest, file_conv) = self.stage_file(path)?;
        let sealed = crate::dm::seal(&self.identity, &peer.public.x25519_pub, &manifest.encode())
            .map_err(NodeError::Seal)?;
        let dm_conv = dm_conversation_id(&self.identity.public(), &peer.public);
        self.append_event(dm_conv, EventKind::FileManifest, sealed)?;

        self.deliver_direct(&peer, file_conv).await.ok();
        self.replicate_to_post_office(file_conv).await.ok();
        self.deliver_direct(&peer, dm_conv).await.ok();
        self.replicate_to_post_office(dm_conv).await.ok();
        Ok(file_conv)
    }

    /// Send the file at `path` to a channel we hold the key for.
    pub async fn send_file_channel(
        &self,
        channel: ConversationId,
        path: &Path,
    ) -> Result<ConversationId, NodeError> {
        let (manifest, file_conv) = self.stage_file(path)?;
        // Publish our sender-key distribution before the first seal (see
        // `send_channel_message_reply`).
        let (members, already_distributed) = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            let epoch = state.epoch();
            (state.members().to_vec(), state.has_my_sender(epoch))
        };
        if !already_distributed {
            self.distribute_my_sender_key(channel, &members)?;
        }
        let sealed = {
            let mut book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state_mut(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            state
                .seal_sender_message(&manifest.encode())
                .map_err(|e| NodeError::File(format!("manifest sealing failed: {e}")))?
        };
        // Persist our advanced sending chain (see `send_channel_message_reply`).
        self.persist_my_senders(channel);
        self.append_event(channel, EventKind::FileManifest, sealed)?;
        self.distribute_channel(file_conv, &members).await;
        self.distribute_channel(channel, &members).await;
        Ok(file_conv)
    }

    /// Read `path`, chunk + seal it into a fresh per-file conversation (appending a
    /// chunk event per piece), and build the (unsealed) manifest. Shared by both
    /// send paths. The caller seals + posts the manifest into the original conv.
    fn stage_file(&self, path: &Path) -> Result<(FileManifest, ConversationId), NodeError> {
        let data = std::fs::read(path).map_err(|e| NodeError::File(format!("read file: {e}")))?;
        // Bounded multi-round sync now transfers chunk events across frames, so files
        // up to MAX_FILE_SIZE (8 MB) are supported. The practical limit is the have
        // id-set fitting one frame; files over the cap are rejected up front.
        if data.len() > MAX_FILE_SIZE {
            return Err(NodeError::File(format!(
                "file too large: {} bytes (max {MAX_FILE_SIZE} until streaming sync lands)",
                data.len()
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let key = FileKey::generate();
        let checksum = file_checksum(&data);
        let file_conv = crate::channel::new_channel_id();
        let chunks = split_chunks(&data);
        let chunk_count = chunks.len() as u32;
        for chunk in &chunks {
            let sealed =
                seal_chunk(&key, chunk).map_err(|e| NodeError::File(format!("chunk seal: {e}")))?;
            self.append_event(file_conv, EventKind::Message, sealed)?;
        }
        let manifest = FileManifest {
            name,
            size: data.len() as u64,
            mime: "application/octet-stream".to_string(),
            checksum,
            file_key: *key.as_bytes(),
            file_conv,
            chunk_count,
        };
        Ok((manifest, file_conv))
    }

    /// Save a received file (identified by its per-file conversation id) to `dest`:
    /// gather the chunk events, reassemble, verify the checksum, and write. Errors if
    /// the manifest is unknown, not all chunks have synced yet, or verification fails.
    pub fn save_file(&self, file_conv: ConversationId, dest: &Path) -> Result<(), NodeError> {
        let manifest = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .cloned()
            .ok_or_else(|| NodeError::File("unknown file".into()))?;
        let chunks: Vec<Vec<u8>> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            log.events(&file_conv)
                .into_iter()
                .filter(|e| e.kind == EventKind::Message)
                .map(|e| e.ciphertext.clone())
                .collect()
        };
        if chunks.len() as u32 != manifest.chunk_count {
            return Err(NodeError::File(format!(
                "file incomplete: {}/{} chunks",
                chunks.len(),
                manifest.chunk_count
            )));
        }
        let data = reassemble_and_verify(&manifest, &chunks)
            .map_err(|e| NodeError::File(format!("reassemble: {e}")))?;
        std::fs::write(dest, data).map_err(|e| NodeError::File(format!("write file: {e}")))?;
        Ok(())
    }

    /// Append a channel event, sequencing it from the channel's log position. Returns
    /// the event's `seq` (its position in our own per-author chain), used to index the
    /// sent sidecar for channel history.
    fn append_event(
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

    /// The last `limit` messages of a channel (all directions), sorted by wall-clock.
    /// Sender-key wire keys are single-use and gone after one decrypt, so history is
    /// served from the stores — our own sent plaintext from the sent sidecar, plus the
    /// plaintext of others' messages we decrypted on receipt (the received store). That
    /// IS the forward-secrecy property, not a limitation. Returns empty for an unknown
    /// channel.
    pub fn channel_history(&self, channel: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            if book.state(&channel).is_none() {
                return Vec::new();
            }
        }
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Map our own Message events' seq -> id, to give sent sidecar entries an id.
        let mut my_msg_ids: std::collections::HashMap<u64, EventId> =
            std::collections::HashMap::new();
        {
            let log = self.log.lock().expect("log mutex not poisoned");
            for event in log.events(&channel) {
                if event.kind == EventKind::Message && event.author == self_author {
                    my_msg_ids.insert(event.seq, event.id);
                }
            }
        }

        let mut entries: Vec<HistoryEntry> = Vec::new();
        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&channel)
        {
            let body = MessageBody::decode(&sent.plaintext);
            entries.push(HistoryEntry {
                id: my_msg_ids
                    .get(&sent.seq)
                    .copied()
                    .unwrap_or(EventId::new([0u8; 32])),
                from_me: true,
                who: "you".to_string(),
                text: body.text,
                wall_clock: sent.wall_clock,
                reply_to: body.reply_to,
            });
        }
        for rcv in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&channel)
        {
            let body = MessageBody::decode(&rcv.plaintext);
            entries.push(HistoryEntry {
                id: rcv.event_id,
                from_me: false,
                who: rcv.from,
                text: body.text,
                wall_clock: rcv.wall_clock,
                reply_to: body.reply_to,
            });
        }
        entries.sort_by_key(|e| e.wall_clock);
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }

    /// The last `limit` messages of the DM with `peer`, both directions, in time
    /// order. Convenience wrapper that derives the conversation id.
    pub fn dm_history(&self, peer: &PublicIdentity, limit: usize) -> Vec<HistoryEntry> {
        let conv = dm_conversation_id(&self.identity.public(), peer);
        self.history(conv, limit)
    }

    /// Account-level conversation history with `peer_account_id`, merging our sent
    /// copies (recorded once under the account conversation) and the per-device copies
    /// we received and filed under it. `from_me` is derived from the recorded sender
    /// account, so self-synced copies of our own sends show as ours.
    pub fn account_history(&self, peer_account_id: &str, limit: usize) -> Vec<HistoryEntry> {
        let conv = account_conversation_id(&self.account_id, peer_account_id);
        let mut entries: Vec<HistoryEntry> = Vec::new();

        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&conv)
        {
            let body = MessageBody::decode(&sent.plaintext);
            entries.push(HistoryEntry {
                id: EventId::new([0u8; 32]),
                from_me: true,
                who: "you".to_string(),
                text: body.text,
                wall_clock: sent.wall_clock,
                reply_to: body.reply_to,
            });
        }

        for rcv in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&conv)
        {
            let body = MessageBody::decode(&rcv.plaintext);
            let from_me = rcv.from == self.account_id;
            entries.push(HistoryEntry {
                id: rcv.event_id,
                from_me,
                who: if from_me { "you".to_string() } else { rcv.from },
                text: body.text,
                wall_clock: rcv.wall_clock,
                reply_to: body.reply_to,
            });
        }

        entries.sort_by_key(|e| e.wall_clock);
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }

    /// The last `limit` messages of `conversation`, both directions, sorted by
    /// wall-clock: our own sent plaintext from the local sidecar, plus the plaintext
    /// of peer-authored messages we decrypted on receipt (from the received store).
    /// The wire ratchet key is single-use and gone, so history is served from the
    /// stores — that IS the forward-secrecy property, not a limitation.
    pub fn history(&self, conversation: ConversationId, limit: usize) -> Vec<HistoryEntry> {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Map our own Message events' seq -> id, to give sent sidecar entries an id.
        let mut my_msg_ids: std::collections::HashMap<u64, EventId> =
            std::collections::HashMap::new();
        {
            let log = self.log.lock().expect("log mutex not poisoned");
            for event in log.events(&conversation) {
                if event.kind == EventKind::Message && event.author == self_author {
                    my_msg_ids.insert(event.seq, event.id);
                }
            }
        }

        let mut entries: Vec<HistoryEntry> = Vec::new();
        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&conversation)
        {
            let body = MessageBody::decode(&sent.plaintext);
            entries.push(HistoryEntry {
                id: my_msg_ids
                    .get(&sent.seq)
                    .copied()
                    .unwrap_or(EventId::new([0u8; 32])),
                from_me: true,
                who: "you".to_string(),
                text: body.text,
                wall_clock: sent.wall_clock,
                reply_to: body.reply_to,
            });
        }
        for rcv in self
            .received
            .lock()
            .expect("received mutex not poisoned")
            .entries(&conversation)
        {
            let body = MessageBody::decode(&rcv.plaintext);
            entries.push(HistoryEntry {
                id: rcv.event_id,
                from_me: false,
                who: rcv.from,
                text: body.text,
                wall_clock: rcv.wall_clock,
                reply_to: body.reply_to,
            });
        }
        entries.sort_by_key(|e| e.wall_clock);
        if entries.len() > limit {
            entries.drain(0..entries.len() - limit);
        }
        entries
    }

    /// Search decrypted history across all known DMs + channels for `query`
    /// (case-insensitive substring). Reuses the per-conversation history methods, so
    /// it inherits their decryption + labels. Empty/whitespace query → no hits.
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        const PER_CONV: usize = 2000;
        let mut hits: Vec<SearchHit> = Vec::new();

        let peers = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .peers();
        for peer in peers.iter().filter(|p| !p.post_office) {
            for entry in self.dm_history(&peer.public, PER_CONV) {
                if String::from_utf8_lossy(&entry.text)
                    .to_lowercase()
                    .contains(&q)
                {
                    hits.push(SearchHit {
                        is_channel: false,
                        target: peer.public.user_id(),
                        label: peer.name.clone(),
                        from_me: entry.from_me,
                        who: entry.who,
                        text: entry.text,
                        wall_clock: entry.wall_clock,
                    });
                }
            }
        }
        for ch in self.list_channels() {
            for entry in self.channel_history(ch.id, PER_CONV) {
                if String::from_utf8_lossy(&entry.text)
                    .to_lowercase()
                    .contains(&q)
                {
                    hits.push(SearchHit {
                        is_channel: true,
                        target: hex::encode(ch.id.as_bytes()),
                        label: ch.name.clone(),
                        from_me: entry.from_me,
                        who: entry.who,
                        text: entry.text,
                        wall_clock: entry.wall_clock,
                    });
                }
            }
        }
        hits.sort_by_key(|b| std::cmp::Reverse(b.wall_clock)); // most recent first
        hits
    }

    /// Aggregated reactions in the DM with `peer` (derives the conversation id).
    pub fn reactions_dm(&self, peer: &PublicIdentity) -> Vec<ReactionView> {
        let conv = dm_conversation_id(&self.identity.public(), peer);
        self.reactions(conv)
    }

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
        let payload = ReactionPayload {
            target,
            emoji: emoji.to_string(),
            remove,
        };
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
        let payload = ReactionPayload {
            target,
            emoji: emoji.to_string(),
            remove,
        };
        let members = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            state.members().to_vec()
        };
        // Reactions must be re-readable by every member (aggregated on each `reactions`
        // call), so they are sealed per-member with the DM sealed box, not the
        // single-use sender-key ratchet used for messages.
        let sealed =
            crate::node::channel::SealedPayload::seal(&self.identity, &members, &payload.encode())
                .map_err(|e| NodeError::Channel(format!("reaction seal failed: {e}")))?;
        self.append_event(channel, EventKind::React, sealed.encode())?;
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
                let Some(sealed) = crate::node::channel::SealedPayload::decode(&event.ciphertext)
                else {
                    continue;
                };
                let author_uid = event.author.user_id();
                let book = self.channels.lock().expect("channels mutex not poisoned");
                book.state(&conv)
                    .and_then(|s| {
                        s.members()
                            .iter()
                            .find(|m| m.user_id() == author_uid)
                            .map(|m| m.x25519_pub)
                    })
                    .and_then(|author_x| sealed.open(&self.identity, &author_x))
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

    /// Decrypt (via the Double Ratchet) and emit any not-yet-emitted, non-self
    /// `Message` events in `conv`. A message is marked emitted ONLY after it
    /// successfully decrypts and is recorded — so a transiently-undecryptable one
    /// (author not yet in the roster, or its ratchet key not yet derivable) is
    /// retried on a later sync rather than lost.
    fn emit_new_messages(&self, conv: ConversationId) {
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let candidates: Vec<Event> = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let emitted = self.emitted.lock().expect("emitted mutex not poisoned");
            log.events(&conv)
                .into_iter()
                .filter(|e| {
                    e.kind == EventKind::Message
                        && e.author != self_author
                        && !emitted.contains(&e.id)
                })
                .cloned()
                .collect()
        };
        for event in candidates {
            // Resolve the author's public identity + display name from the roster.
            let author_uid = event.author.user_id();
            let (peer_public, peer_name) = {
                let roster = self.roster.lock().expect("roster mutex not poisoned");
                match roster.get(&author_uid) {
                    Some(p) => (p.public.clone(), p.name.clone()),
                    None => continue, // unknown author yet; retry later (NOT marked emitted)
                }
            };
            // Decrypt the ratchet wire. The wire key is single-use: a successful
            // decrypt advances + persists the session, so a re-fed event won't reopen.
            let wrapped = {
                let mut r = self
                    .dm_ratchet
                    .lock()
                    .expect("dm_ratchet mutex not poisoned");
                match r.decrypt(&self.identity, &peer_public, &event.ciphertext) {
                    Ok(pt) => pt,
                    Err(_) => continue, // not yet decryptable / not a ratchet DM (NOT emitted)
                }
            };
            // Account-addressed (multi-device) envelope? File it under the ACCOUNT
            // conversation, recording the route's sender account (account history
            // derives `from_me` from it). A legacy plaintext keeps today's device-pair
            // behavior. The live `ReceivedDm` still carries the author device's
            // id/name — it is just a "something changed" poke; display reads history.
            let (record_conv, record_from, inner_body) = match DmEnvelope::decode(&wrapped) {
                Some(env) => {
                    let counterparty = if env.route.sender_account == self.account_id {
                        env.route.recipient_account.clone() // self-synced copy of our own send
                    } else {
                        env.route.sender_account.clone()
                    };
                    let acct_conv = account_conversation_id(&self.account_id, &counterparty);
                    (acct_conv, env.route.sender_account.clone(), env.body)
                }
                None => (conv, author_uid.clone(), wrapped.clone()),
            };
            let body = MessageBody::decode(&inner_body);
            // Persist the received plaintext (the wire key is single-use/gone), then
            // mark emitted — only AFTER a successful decrypt + record.
            let _ = self
                .received
                .lock()
                .expect("received mutex not poisoned")
                .record(
                    record_conv,
                    record_from,
                    event.wall_clock,
                    &inner_body,
                    event.id,
                );
            self.emitted
                .lock()
                .expect("emitted mutex not poisoned")
                .insert(event.id);
            let _ = self.incoming.send(ReceivedDm {
                from: author_uid,
                from_name: peer_name,
                text: body.text,
                reply_to: body.reply_to,
            });
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

    /// Add another known peer to an existing roster (for multi-peer rigs).
    fn add_peer(
        roster: &Arc<Mutex<Roster>>,
        peer: &DeviceIdentity,
        name: &str,
        port: u16,
        self_user_id: &str,
    ) {
        roster.lock().unwrap().update(
            &Announce::new(peer, name, port),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            self_user_id,
        );
    }

    #[tokio::test]
    async fn send_dm_to_unknown_peer_errors() {
        let dir = tempfile::tempdir().unwrap();
        let me = DeviceIdentity::generate();
        let roster = Arc::new(Mutex::new(Roster::default())); // empty
        let (tx, _rx) = mpsc::unbounded_channel();
        let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            me,
            roster,
            tx,
            ch_tx,
            file_tx,
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
        let (a_ch_tx, _a_ch_rx) = mpsc::unbounded_channel();
        let (b_ch_tx, _b_ch_rx) = mpsc::unbounded_channel();
        let (a_file_tx, _a_file_rx) = mpsc::unbounded_channel();
        let (b_file_tx, _b_file_rx) = mpsc::unbounded_channel();
        let alice_node = Node::open(
            alice,
            alice_roster,
            alice_tx,
            a_ch_tx,
            a_file_tx,
            &dir.path().join("alice.log"),
            &dir.path().join("alice-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            bob_tx,
            b_ch_tx,
            b_file_tx,
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
        let (a_ch_tx, _a_ch_rx) = mpsc::unbounded_channel();
        let (b_ch_tx, _b_ch_rx) = mpsc::unbounded_channel();
        let (a_file_tx, _a_file_rx) = mpsc::unbounded_channel();
        let (b_file_tx, _b_file_rx) = mpsc::unbounded_channel();
        let alice_node = Node::open(
            alice,
            alice_roster,
            alice_tx,
            a_ch_tx,
            a_file_tx,
            &dir.path().join("alice.log"),
            &dir.path().join("alice-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            bob_tx,
            b_ch_tx,
            b_file_tx,
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
        let bob_uid = bob.public().user_id();
        let conv = crate::node::conversation::dm_conversation_id(&me_pub, &bob.public());

        let roster = Arc::new(Mutex::new(Roster::default()));
        roster.lock().unwrap().update(
            &Announce::new(&bob, "Bob", 4000),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &me_pub.user_id(),
        );
        let (tx, _rx) = mpsc::unbounded_channel();
        let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            me,
            roster,
            tx,
            ch_tx,
            file_tx,
            &dir.path().join("me.log"),
            &dir.path().join("me-sent.log"),
            "pw",
        )
        .unwrap();

        // Bob sent us a message at t=1000 (decrypted on receipt → received store).
        node.received
            .lock()
            .unwrap()
            .record(
                conv,
                bob_uid.clone(),
                1000,
                &MessageBody::new(b"hi from bob".to_vec(), None).encode(),
                EventId::new([7u8; 32]),
            )
            .unwrap();
        // We sent one at t=2000 (recorded in the sidecar as the wrapped body).
        node.sentlog
            .lock()
            .unwrap()
            .record(
                conv,
                1,
                2000,
                &MessageBody::new(b"hi from me".to_vec(), None).encode(),
            )
            .unwrap();

        let hist = node.dm_history(&bob.public(), 10);
        assert_eq!(hist.len(), 2);
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

        // Alice's ratchet (to seal wire she sends us). Her sessions live in their own
        // temp dir — only the wire bytes matter to us.
        let alice_dir = dir.path().join("alice-sess");
        std::fs::create_dir_all(&alice_dir).unwrap();
        let mut alice_ratchet = DmRatchet::new(
            RatchetSessions::open(&alice_dir.join("ratchet.sessions"), "pw").unwrap(),
        );

        // Prior session: a received DM from Alice is already in the persistent log
        // (its ratchet wire, sealed to us). We seed `emitted` from it at open.
        let old_wire = alice_ratchet
            .encrypt(
                &alice,
                &me_pub,
                &MessageBody::new(b"old".to_vec(), None).encode(),
            )
            .unwrap();
        let old = crate::eventlog::event::Event::new(
            &alice,
            conv,
            1,
            vec![],
            1,
            1000,
            EventKind::Message,
            old_wire,
        );
        let old_id = old.id;
        {
            let mut log =
                crate::eventlog::persist::PersistentEventLog::open(&log_path, "pw").unwrap();
            log.append(old).unwrap();
        }

        // Roster knows Alice (so decryption is possible if it were attempted).
        let roster = Arc::new(Mutex::new(Roster::default()));
        roster.lock().unwrap().update(
            &Announce::new(&alice, "Alice", 4000),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &me_pub.user_id(),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(me, roster, tx, ch_tx, file_tx, &log_path, &sent_path, "pw").unwrap();

        // Restored history must NOT be re-streamed (seeded into `emitted` at open).
        node.emit_new_messages(conv);
        assert!(rx.try_recv().is_err(), "restored history was re-streamed");

        // A genuinely-new received event (not present at open) IS surfaced.
        let fresh_wire = alice_ratchet
            .encrypt(
                &alice,
                &me_pub,
                &MessageBody::new(b"new".to_vec(), None).encode(),
            )
            .unwrap();
        let fresh = crate::eventlog::event::Event::new(
            &alice,
            conv,
            2,
            vec![old_id],
            2,
            2000,
            EventKind::Message,
            fresh_wire,
        );
        node.log.lock().unwrap().append(fresh).unwrap();
        node.emit_new_messages(conv);
        let got = rx.try_recv().expect("a new message is emitted");
        assert_eq!(got.text, b"new");
    }

    #[tokio::test]
    async fn two_nodes_exchange_a_channel_message_over_loopback_tcp() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_pub = bob.public();
        let dir = tempfile::tempdir().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();

        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
        let bob_roster = Arc::new(Mutex::new(Roster::default()));

        let (alice_dm_tx, _alice_dm_rx) = mpsc::unbounded_channel();
        let (alice_ch_tx, _alice_ch_rx) = mpsc::unbounded_channel();
        let (alice_file_tx, _alice_file_rx) = mpsc::unbounded_channel();
        let (bob_dm_tx, _bob_dm_rx) = mpsc::unbounded_channel();
        let (bob_ch_tx, mut bob_ch_rx) = mpsc::unbounded_channel();
        let (bob_file_tx, _bob_file_rx) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            alice_dm_tx,
            alice_ch_tx,
            alice_file_tx,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            bob_dm_tx,
            bob_ch_tx,
            bob_file_tx,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        let channel = alice_node
            .create_channel("general", vec![bob_pub])
            .await
            .unwrap();
        alice_node
            .send_channel_message(channel, b"hello channel")
            .await
            .unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(5), bob_ch_rx.recv())
            .await
            .expect("bob received a channel message within 5s")
            .expect("channel stream open");
        assert_eq!(got.text, b"hello channel");
        assert_eq!(got.from, alice_node.user_id());
        assert_eq!(got.channel_name, "general");

        // Alice's own channel_history shows her sent message — served from the sent
        // sidecar (the sender-key wire key is single-use and not self-decryptable).
        let hist = alice_node.channel_history(channel, 10);
        assert_eq!(hist.len(), 1);
        assert!(hist[0].from_me);
        assert_eq!(hist[0].text, b"hello channel");

        // Bob's channel_history shows Alice's message — served from his received store
        // (he decrypted it once via the sender-key ratchet; the key is now consumed).
        let bob_hist = bob_node.channel_history(channel, 10);
        assert_eq!(bob_hist.len(), 1);
        assert!(!bob_hist[0].from_me);
        assert_eq!(bob_hist[0].text, b"hello channel");
        assert_eq!(bob_hist[0].who, alice_node.user_id());
    }

    /// Bidirectional channel rig: Alice (creator) + Bob both run accept loops and know
    /// each other. Alice sends -> Bob receives (already worked). THEN Bob sends -> Alice
    /// receives + decrypts. The reverse direction was BROKEN before the fix: a
    /// non-creator never distributed its own sender-key distribution, so the creator
    /// had no `SenderChain` for it and silently dropped its messages.
    #[tokio::test]
    async fn two_nodes_exchange_channel_messages_both_directions_over_loopback_tcp() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_pub = bob.public();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();
        let dir = tempfile::tempdir().unwrap();

        // Both nodes listen, so each can receive the other's pushed channel events.
        let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alice_addr = alice_listener.local_addr().unwrap();
        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();

        // Each knows the other at its real listen port (so distribution can dial back).
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
        let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, mut a_ch_r) = mpsc::unbounded_channel();
        let (a_file, _a_file_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
        let (b_file, _b_file_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_file,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_file,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

        let channel = alice_node
            .create_channel("general", vec![bob_pub])
            .await
            .unwrap();

        // Forward direction: Alice -> Bob (already worked).
        alice_node
            .send_channel_message(channel, b"hi bob")
            .await
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
            .await
            .expect("bob received Alice's message within 5s")
            .expect("channel stream open");
        assert_eq!(got.text, b"hi bob");
        assert_eq!(got.from, alice_node.user_id());

        // Reverse direction: Bob -> Alice. This is the path the fix repairs. Bob is a
        // non-creator member; his send must first distribute his own SKD so Alice can
        // build his sender chain and decrypt.
        bob_node
            .send_channel_message(channel, b"hi alice")
            .await
            .unwrap();
        let back = tokio::time::timeout(Duration::from_secs(5), a_ch_r.recv())
            .await
            .expect("alice received Bob's message within 5s (broken before the fix)")
            .expect("channel stream open");
        assert_eq!(back.text, b"hi alice");
        assert_eq!(back.from, bob_node.user_id());

        // Alice's channel_history now shows both: her own sent line + Bob's decrypted one.
        let alice_hist = alice_node.channel_history(channel, 10);
        assert!(alice_hist.iter().any(|h| h.from_me && h.text == b"hi bob"));
        assert!(alice_hist
            .iter()
            .any(|h| !h.from_me && h.text == b"hi alice" && h.who == bob_node.user_id()));
    }

    /// Restart rig: Alice creates a {Alice, Bob} channel and sends a message Bob
    /// decrypts. Alice's `Node` is then DROPPED and REOPENED from the SAME dir (same
    /// log + derived stores + password). Alice sends ANOTHER message in the SAME epoch;
    /// Bob must still decrypt it. This works only if Alice resumed her SAME sending
    /// chain (persisted in `channel.senders`) rather than minting a fresh one: Bob is
    /// first-wins on the chain he already holds, so a fresh chain would be undecryptable.
    /// Without the persist+inject fix, the post-restart message is silently dropped.
    #[tokio::test]
    async fn channel_sending_survives_restart() {
        let alice = DeviceIdentity::generate();
        let (alice_ed, alice_x) = alice.secret_bytes(); // to reconstruct Alice on restart
        let bob = DeviceIdentity::generate();
        let bob_pub = bob.public();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();
        let dir = tempfile::tempdir().unwrap();
        // Separate sub-dirs so the two nodes' derived stores (ratchet.sessions,
        // channel.senders, ...) don't collide. Alice reopens from her SAME sub-dir.
        let alice_dir = dir.path().join("alice");
        let bob_dir = dir.path().join("bob");
        std::fs::create_dir_all(&alice_dir).unwrap();
        std::fs::create_dir_all(&bob_dir).unwrap();
        let alice_log = alice_dir.join("a.log");
        let alice_sent = alice_dir.join("a-sent.log");

        let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alice_addr = alice_listener.local_addr().unwrap();
        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();

        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
        // Build Alice's post-restart roster now, before `bob` is moved into his node.
        let alice_roster2 = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
        let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_file, _a_file_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
        let (b_file, _b_file_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_file,
            &alice_log,
            &alice_sent,
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_file,
            &bob_dir.join("b.log"),
            &bob_dir.join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

        let channel = alice_node
            .create_channel("general", vec![bob_pub])
            .await
            .unwrap();
        alice_node
            .send_channel_message(channel, b"before restart")
            .await
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
            .await
            .expect("bob received the pre-restart message within 5s")
            .expect("channel stream open");
        assert_eq!(got.text, b"before restart");

        // RESTART Alice: drop her node, reopen from the SAME paths/password. Her
        // `my_sender` is rebuilt from `channel.senders`, not the (sender-key-less) log.
        drop(alice_node);
        let (a_dm2, _a_dm_r2) = mpsc::unbounded_channel();
        let (a_ch2, _a_ch_r2) = mpsc::unbounded_channel();
        let (a_file2, _a_file_r2) = mpsc::unbounded_channel();
        let alice2 = DeviceIdentity::from_secret_bytes(alice_ed, alice_x);
        let alice_node2 = Node::open(
            alice2,
            alice_roster2,
            a_dm2,
            a_ch2,
            a_file2,
            &alice_log,
            &alice_sent,
            "pw",
        )
        .unwrap();

        // Same-epoch send AFTER restart. Bob (still running, first-wins on Alice's
        // existing chain) must decrypt it — proving Alice resumed her SAME chain.
        alice_node2
            .send_channel_message(channel, b"after restart")
            .await
            .unwrap();
        let after = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
            .await
            .expect("bob received the post-restart message within 5s (broken without persist)")
            .expect("channel stream open");
        assert_eq!(after.text, b"after restart");
        assert_eq!(after.from, alice_node2.user_id());
    }

    /// Live add-member rig over loopback TCP: Alice creates a {Alice, Bob} channel and
    /// they exchange an epoch-0 message. Alice then adds Carol (a third node, member of
    /// nobody) via `add_channel_member`, bumping to epoch 1, and sends a new message.
    /// Carol — who got the `MembershipChange` + Alice's lazily-distributed epoch-1 SKD —
    /// receives + decrypts the epoch-1 message, but NOT the epoch-0 one she joined after
    /// (late-join isolation / forward secrecy).
    #[tokio::test]
    async fn adding_a_channel_member_lets_them_receive_new_messages() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let bob_pub = bob.public();
        let carol_pub = carol.public();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();
        let carol_uid = carol.public().user_id();
        let dir = tempfile::tempdir().unwrap();

        let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alice_addr = alice_listener.local_addr().unwrap();
        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();
        let carol_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let carol_addr = carol_listener.local_addr().unwrap();

        // Alice knows Bob and Carol (so she can dial both to distribute). Bob and Carol
        // each know Alice (enough to receive her pushes).
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
        add_peer(
            &alice_roster,
            &carol,
            "Carol",
            carol_addr.port(),
            &alice_uid,
        );
        let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);
        let carol_roster = seed_roster(&alice, "Alice", alice_addr.port(), &carol_uid);

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_file, _a_file_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
        let (b_file, _b_file_r) = mpsc::unbounded_channel();
        let (c_dm, _c_dm_r) = mpsc::unbounded_channel();
        let (c_ch, mut c_ch_r) = mpsc::unbounded_channel();
        let (c_file, _c_file_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_file,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_file,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();
        let carol_node = Node::open(
            carol,
            carol_roster,
            c_dm,
            c_ch,
            c_file,
            &dir.path().join("c.log"),
            &dir.path().join("c-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));
        tokio::spawn(Arc::clone(&carol_node).run_accept_loop(carol_listener));

        // Epoch 0: {Alice, Bob}. Alice sends; Bob receives.
        let channel = alice_node
            .create_channel("general", vec![bob_pub])
            .await
            .unwrap();
        alice_node
            .send_channel_message(channel, b"epoch0 msg")
            .await
            .unwrap();
        let got0 = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
            .await
            .expect("bob received the epoch-0 message within 5s")
            .expect("channel stream open");
        assert_eq!(got0.text, b"epoch0 msg");

        // Add Carol (epoch 1). She gets the MembershipChange via Alice's distribution.
        alice_node
            .add_channel_member(channel, carol_pub)
            .await
            .unwrap();

        // Epoch 1: Alice sends. Her lazy SKD for epoch 1 is distributed before the seal,
        // so both Bob (still a member) and Carol (newly joined) can read it.
        alice_node
            .send_channel_message(channel, b"epoch1 msg")
            .await
            .unwrap();

        let got_carol = tokio::time::timeout(Duration::from_secs(5), c_ch_r.recv())
            .await
            .expect("carol received the epoch-1 message within 5s")
            .expect("channel stream open");
        assert_eq!(got_carol.text, b"epoch1 msg");
        assert_eq!(got_carol.from, alice_node.user_id());

        // Late-join isolation: Carol never receives the epoch-0 message (she joined at
        // epoch 1). Her history holds only the epoch-1 message.
        let carol_hist = carol_node.channel_history(channel, 10);
        assert!(
            carol_hist.iter().all(|h| h.text != b"epoch0 msg"),
            "late joiner must not read pre-join messages"
        );
        assert!(carol_hist.iter().any(|h| h.text == b"epoch1 msg"));
        assert!(
            c_ch_r.try_recv().is_err(),
            "carol must not receive the pre-join epoch-0 message"
        );
    }

    /// Forward-secrecy rig: Alice + Bob channel; Alice sends two messages; Bob reads
    /// both via the sender-key ratchet and `channel_history` serves them from his
    /// store; re-feeding the events does NOT re-open them (single-use keys).
    #[tokio::test]
    async fn channel_messages_are_forward_secret_and_history_comes_from_stores() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_pub = bob.public();
        let dir = tempfile::tempdir().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
        let bob_roster = Arc::new(Mutex::new(Roster::default()));

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_file, _a_file_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
        let (b_file, _b_file_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_file,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_file,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        let channel = alice_node
            .create_channel("general", vec![bob_pub])
            .await
            .unwrap();
        alice_node
            .send_channel_message(channel, b"m0")
            .await
            .unwrap();
        alice_node
            .send_channel_message(channel, b"m1")
            .await
            .unwrap();

        // Bob receives both via the sender-key ratchet.
        let mut texts: Vec<Vec<u8>> = Vec::new();
        for _ in 0..2 {
            let got = tokio::time::timeout(std::time::Duration::from_secs(5), b_ch_r.recv())
                .await
                .expect("bob received a channel message within 5s")
                .expect("channel stream open");
            texts.push(got.text);
        }
        texts.sort();
        assert_eq!(texts, vec![b"m0".to_vec(), b"m1".to_vec()]);

        // Bob's history is served from his received store (single-use keys consumed).
        let bob_hist = bob_node.channel_history(channel, 10);
        assert_eq!(bob_hist.len(), 2);
        assert!(bob_hist.iter().all(|e| !e.from_me));
        let mut htexts: Vec<Vec<u8>> = bob_hist.iter().map(|e| e.text.clone()).collect();
        htexts.sort();
        assert_eq!(htexts, vec![b"m0".to_vec(), b"m1".to_vec()]);

        // Re-feeding the same events does NOT re-open them (single-use ratchet keys);
        // history is unchanged and nothing new is streamed.
        bob_node.process_channel(channel);
        assert!(
            b_ch_r.try_recv().is_err(),
            "a consumed message was re-opened"
        );
        assert_eq!(bob_node.channel_history(channel, 10).len(), 2);
    }

    #[tokio::test]
    async fn list_channels_reports_created_channels() {
        let me = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let roster = Arc::new(Mutex::new(Roster::default()));
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (ch_tx, _ch_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        let node = Node::open(
            me,
            roster,
            dm_tx,
            ch_tx,
            file_tx,
            &dir.path().join("m.log"),
            &dir.path().join("m-sent.log"),
            "pw",
        )
        .unwrap();

        assert!(node.list_channels().is_empty());
        let id = node.create_channel("general", vec![]).await.unwrap();
        let channels = node.list_channels();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].id, id);
        assert_eq!(channels[0].name, "general");
        assert_eq!(channels[0].member_count, 1); // just the creator
    }

    #[tokio::test]
    async fn two_nodes_transfer_a_file_over_loopback_tcp() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();
        // Bob knows Alice (to open her DM-sealed manifest); Alice knows Bob (to dial).
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
        let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_f, _a_f_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
        let (b_f, mut b_f_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_f,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_f,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        // A multi-chunk payload.
        let payload = vec![0xABu8; crate::file::CHUNK_SIZE + 1234];
        let src = dir.path().join("photo.bin");
        std::fs::write(&src, &payload).unwrap();

        let bob_uid = bob_node.user_id();
        let file_conv = alice_node.send_file_dm(&bob_uid, &src).await.unwrap();

        // Bob surfaces the received file.
        let rf = tokio::time::timeout(std::time::Duration::from_secs(5), b_f_r.recv())
            .await
            .expect("bob received a file within 5s")
            .expect("file stream open");
        assert_eq!(rf.name, "photo.bin");
        assert_eq!(rf.size, payload.len() as u64);
        assert_eq!(rf.file_conv, file_conv);
        assert_eq!(rf.from, alice_node.user_id());

        // Bob saves it and the bytes match. Retry briefly: the file_conv chunk
        // sync runs in a separate task and may land just after the manifest
        // notification fires (separate TCP connection, same loopback).
        let dest = dir.path().join("saved.bin");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match bob_node.save_file(rf.file_conv, &dest) {
                    Ok(()) => break,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                }
            }
        })
        .await
        .expect("bob saved the file within 5s");
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
    }

    #[tokio::test]
    async fn two_nodes_transfer_a_multi_chunk_file_over_loopback_tcp() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice.public().user_id());
        let bob_roster = seed_roster(&alice, "Alice", 1, &bob.public().user_id());

        let (a_dm, _a) = mpsc::unbounded_channel();
        let (a_ch, _b) = mpsc::unbounded_channel();
        let (a_f, _c) = mpsc::unbounded_channel();
        let (b_dm, _d) = mpsc::unbounded_channel();
        let (b_ch, _e) = mpsc::unbounded_channel();
        let (b_f, mut b_f_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_f,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_f,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        // ~5 chunks → the file_conv batch far exceeds one frame → multiple rounds.
        let payload = vec![0x5Au8; crate::file::CHUNK_SIZE * 4 + 999];
        let src = dir.path().join("big.bin");
        std::fs::write(&src, &payload).unwrap();

        let bob_uid = bob_node.user_id();
        let file_conv = alice_node.send_file_dm(&bob_uid, &src).await.unwrap();

        let rf = tokio::time::timeout(std::time::Duration::from_secs(10), b_f_r.recv())
            .await
            .expect("received within 10s")
            .expect("stream open");
        assert_eq!(rf.size, payload.len() as u64);

        // Saving may need a brief retry while the file_conv chunks finish syncing.
        let dest = dir.path().join("big-saved.bin");
        let mut saved = false;
        for _ in 0..100 {
            if bob_node.save_file(rf.file_conv, &dest).is_ok() {
                saved = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(saved, "bob saved the multi-chunk file");
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
        let _ = file_conv;
    }

    #[tokio::test]
    async fn a_dm_reaction_is_visible_to_both_peers() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Capture public identities BEFORE moving into Node::open.
        let alice_pub = alice.public();
        let bob_pub = bob.public();
        let alice_uid = alice_pub.user_id();
        let bob_uid = bob_pub.user_id();

        let dir = tempfile::tempdir().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();
        let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alice_addr = alice_listener.local_addr().unwrap();

        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_pub.user_id());
        let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_pub.user_id());

        let (a_dm, _a1) = mpsc::unbounded_channel();
        let (a_ch, _a2) = mpsc::unbounded_channel();
        let (a_f, _a3) = mpsc::unbounded_channel();
        let (b_dm, mut b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, _b2) = mpsc::unbounded_channel();
        let (b_f, _b3) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_f,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_f,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));
        tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));

        // Alice DMs Bob; Bob receives it and learns its event id from history.
        alice_node.send_dm(&bob_uid, b"hi bob").await.unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), b_dm_r.recv())
            .await
            .expect("bob got the dm")
            .expect("stream open");
        assert_eq!(got.text, b"hi bob");

        // Derive the DM conversation id from the two public identities.
        let conv = dm_conversation_id(&alice_pub, &bob_pub);

        let target = {
            let h = bob_node.dm_history(&alice_pub, 10);
            h.iter()
                .find(|e| !e.from_me)
                .map(|e| e.id)
                .expect("bob has the message id")
        };

        // Bob reacts; it distributes to Alice.
        bob_node
            .react_dm(&alice_uid, target, "👍", false)
            .await
            .unwrap();

        // Bob sees his own reaction (from my_dm_reactions).
        let bob_views = bob_node.reactions(conv);
        assert!(
            bob_views
                .iter()
                .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid)),
            "bob sees his own reaction"
        );

        // Give the distribution a moment, then check Alice.
        let mut ok = false;
        for _ in 0..50 {
            let av = alice_node.reactions(conv);
            if av
                .iter()
                .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid))
            {
                ok = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(ok, "alice sees bob's reaction");
    }

    /// Channel file transfer over the wire, both directions. The manifest is sealed
    /// with the sender-key ratchet (`seal_sender_message`) and opened by
    /// `process_file_events`; the chunk conversation syncs separately. Bob is a
    /// non-creator member, so his send must first distribute his own sender key —
    /// the same path the creator-only-send fix repaired for messages.
    #[tokio::test]
    async fn two_nodes_transfer_a_file_in_a_channel() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_pub = bob.public();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();
        let dir = tempfile::tempdir().unwrap();

        // Both nodes listen so each can receive the other's pushed file events.
        let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alice_addr = alice_listener.local_addr().unwrap();
        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();

        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
        let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_f, mut a_f_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
        let (b_f, mut b_f_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_f,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_f,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

        let channel = alice_node
            .create_channel("general", vec![bob_pub])
            .await
            .unwrap();

        // Forward direction: Alice -> Bob. Multi-chunk payload.
        let payload = vec![0xABu8; crate::file::CHUNK_SIZE + 1234];
        let src = dir.path().join("photo.bin");
        std::fs::write(&src, &payload).unwrap();
        let file_conv = alice_node.send_file_channel(channel, &src).await.unwrap();

        let rf = tokio::time::timeout(Duration::from_secs(5), b_f_r.recv())
            .await
            .expect("bob received the channel file within 5s")
            .expect("file stream open");
        assert_eq!(rf.name, "photo.bin");
        assert_eq!(rf.size, payload.len() as u64);
        assert_eq!(rf.conv, channel);
        assert_eq!(rf.file_conv, file_conv);
        assert_eq!(rf.from, alice_node.user_id());

        let dest = dir.path().join("saved.bin");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match bob_node.save_file(rf.file_conv, &dest) {
                    Ok(()) => break,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await
        .expect("bob saved the channel file within 5s");
        assert_eq!(std::fs::read(&dest).unwrap(), payload);

        // Reverse direction: Bob -> Alice. Bob is a non-creator member; his
        // send_file_channel must distribute his own sender key first so Alice can
        // open the manifest (mirrors the bidirectional message fix).
        let payload2 = vec![0xCDu8; crate::file::CHUNK_SIZE * 2 + 77];
        let src2 = dir.path().join("from-bob.bin");
        std::fs::write(&src2, &payload2).unwrap();
        let file_conv2 = bob_node.send_file_channel(channel, &src2).await.unwrap();

        let rf2 = tokio::time::timeout(Duration::from_secs(5), a_f_r.recv())
            .await
            .expect("alice received Bob's channel file within 5s (non-creator send)")
            .expect("file stream open");
        assert_eq!(rf2.name, "from-bob.bin");
        assert_eq!(rf2.size, payload2.len() as u64);
        assert_eq!(rf2.conv, channel);
        assert_eq!(rf2.file_conv, file_conv2);
        assert_eq!(rf2.from, bob_node.user_id());

        let dest2 = dir.path().join("alice-saved.bin");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match alice_node.save_file(rf2.file_conv, &dest2) {
                    Ok(()) => break,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        })
        .await
        .expect("alice saved Bob's channel file within 5s");
        assert_eq!(std::fs::read(&dest2).unwrap(), payload2);
    }

    /// Channel reactions over the wire. They use a per-member `SealedPayload`
    /// (re-readable, not single-use), so both the reacting author and the other
    /// members must see them via `reactions(channel)`. Also covers toggle-off.
    #[tokio::test]
    async fn channel_reactions_are_visible_to_members() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_pub = bob.public();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();
        let dir = tempfile::tempdir().unwrap();

        let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alice_addr = alice_listener.local_addr().unwrap();
        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();

        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
        let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_uid);

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_f, _a_f_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, mut b_ch_r) = mpsc::unbounded_channel();
        let (b_f, _b_f_r) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_f,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_f,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

        let channel = alice_node
            .create_channel("general", vec![bob_pub])
            .await
            .unwrap();

        // Alice sends a channel message; Bob receives it and learns its event id.
        alice_node
            .send_channel_message(channel, b"hi bob")
            .await
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), b_ch_r.recv())
            .await
            .expect("bob received Alice's channel message within 5s")
            .expect("channel stream open");
        assert_eq!(got.text, b"hi bob");
        let target = got.event_id;

        // Bob reacts; it distributes to Alice.
        bob_node
            .react_channel(channel, target, "👍", false)
            .await
            .unwrap();

        // Bob re-reads his own channel reaction (sealed per-member, re-readable).
        let bob_views = bob_node.reactions(channel);
        assert!(
            bob_views
                .iter()
                .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid)),
            "bob sees his own channel reaction"
        );

        // Alice sees Bob's reaction after distribution lands.
        let mut ok = false;
        for _ in 0..50 {
            let av = alice_node.reactions(channel);
            if av
                .iter()
                .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid))
            {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(ok, "alice sees bob's channel reaction");
        let _ = alice_uid;

        // Toggle off: Bob removes the reaction; it disappears for both.
        bob_node
            .react_channel(channel, target, "👍", true)
            .await
            .unwrap();

        let bob_after = bob_node.reactions(channel);
        assert!(
            !bob_after
                .iter()
                .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid)),
            "bob's reaction is gone after toggle-off"
        );

        let mut gone = false;
        for _ in 0..50 {
            let av = alice_node.reactions(channel);
            if !av
                .iter()
                .any(|v| v.emoji == "👍" && v.who.contains(&bob_uid))
            {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(gone, "alice no longer sees bob's reaction after toggle-off");
    }

    #[tokio::test]
    async fn search_finds_a_sent_dm_by_keyword() {
        let me = DeviceIdentity::generate();
        let peer = DeviceIdentity::generate();
        let dir = tempfile::tempdir().unwrap();
        let roster = seed_roster(&peer, "Peer", 1, &me.public().user_id());
        let (dm, _a) = mpsc::unbounded_channel();
        let (ch, _b) = mpsc::unbounded_channel();
        let (f, _c) = mpsc::unbounded_channel();
        let node = Node::open(
            me,
            roster,
            dm,
            ch,
            f,
            &dir.path().join("m.log"),
            &dir.path().join("m-sent.log"),
            "pw",
        )
        .unwrap();

        // No peer is reachable (port 1), but send_dm still records to the sidecar.
        let peer_uid = peer.public().user_id();
        let _ = node.send_dm(&peer_uid, b"lunch at noon tomorrow").await; // delivery fails; sidecar written
        let hits = node.search("NOON");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].from_me);
        assert_eq!(hits[0].label, "Peer");
        assert_eq!(hits[0].target, peer_uid);
        assert!(node.search("   ").is_empty());
        assert!(node.search("absent-keyword").is_empty());
    }

    #[tokio::test]
    async fn a_reply_round_trips_with_its_reply_to() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        // Capture public identities BEFORE moving into Node::open.
        let alice_pub = alice.public();
        let bob_pub = bob.public();
        let alice_uid = alice_pub.user_id();
        let bob_uid = bob_pub.user_id();

        let dir = tempfile::tempdir().unwrap();

        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();
        let alice_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alice_addr = alice_listener.local_addr().unwrap();

        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_pub.user_id());
        let bob_roster = seed_roster(&alice, "Alice", alice_addr.port(), &bob_pub.user_id());

        let (a_dm, mut a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch) = mpsc::unbounded_channel();
        let (a_f, _a_f) = mpsc::unbounded_channel();
        let (b_dm, mut b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, _b_ch) = mpsc::unbounded_channel();
        let (b_f, _b_f) = mpsc::unbounded_channel();

        let alice_node = Node::open(
            alice,
            alice_roster,
            a_dm,
            a_ch,
            a_f,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            b_dm,
            b_ch,
            b_f,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));
        tokio::spawn(Arc::clone(&alice_node).run_accept_loop(alice_listener));

        // Alice sends a normal DM to Bob (no reply_to).
        alice_node
            .send_dm(&bob_uid, b"hi")
            .await
            .expect("alice sends hi");

        // Bob receives it.
        let got = tokio::time::timeout(Duration::from_secs(5), b_dm_r.recv())
            .await
            .expect("bob got alice's dm within 5s")
            .expect("stream open");
        assert_eq!(got.text, b"hi");
        assert_eq!(got.reply_to, None, "normal message has no reply_to");

        // Bob looks up the parent message id from his history.
        let parent_id = bob_node
            .dm_history(&alice_pub, 10)
            .into_iter()
            .find(|e| !e.from_me)
            .map(|e| e.id)
            .expect("bob has alice's message in history");

        // Bob replies to Alice's message.
        bob_node
            .send_dm_reply(&alice_uid, b"hey back", Some(parent_id))
            .await
            .expect("bob sends reply");

        // Alice receives the reply with reply_to set.
        let reply = tokio::time::timeout(Duration::from_secs(5), a_dm_r.recv())
            .await
            .expect("alice got bob's reply within 5s")
            .expect("stream open");
        assert_eq!(reply.text, b"hey back");
        assert_eq!(
            reply.reply_to,
            Some(parent_id),
            "alice's ReceivedDm carries reply_to"
        );

        // Alice's dm_history shows the reply entry with reply_to.
        let alice_hist = alice_node.dm_history(&bob_pub, 10);
        let reply_entry = alice_hist
            .iter()
            .find(|e| !e.from_me)
            .expect("alice has bob's reply in history");
        assert_eq!(reply_entry.text, b"hey back");
        assert_eq!(
            reply_entry.reply_to,
            Some(parent_id),
            "history entry carries reply_to"
        );

        // A normal send_dm (no reply) yields reply_to == None in history.
        alice_node.send_dm(&bob_uid, b"just a message").await.ok();
        let alice_hist2 = alice_node.dm_history(&bob_pub, 10);
        let normal_sent = alice_hist2
            .iter()
            .find(|e| e.from_me && e.text == b"just a message")
            .expect("alice's normal sent message is in history");
        assert_eq!(
            normal_sent.reply_to, None,
            "normal send_dm yields reply_to == None"
        );
    }

    #[tokio::test]
    async fn dm_messages_are_forward_secret_over_the_ratchet() {
        // Two nodes over loopback TCP. Alice sends DMs sealed with the Double
        // Ratchet; Bob decrypts them once into his received store. Proves: (a) both
        // land in Bob's dm_history; (b) re-feeding the SAME wire event to
        // emit_new_messages does NOT re-surface it (the wire key is single-use); and
        // (c) out-of-order arrival still surfaces every message.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let alice_pub = alice.public();
        let bob_pub = bob.public();
        let alice_uid = alice_pub.user_id();
        let bob_uid = bob_pub.user_id();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = listener.local_addr().unwrap();
        let alice_roster = seed_roster(&bob, "Bob", bob_addr.port(), &alice_uid);
        let bob_roster = seed_roster(&alice, "Alice", 4000, &bob_uid);

        let dir = tempfile::tempdir().unwrap();
        let (alice_tx, _alice_rx) = mpsc::unbounded_channel();
        let (bob_tx, mut bob_rx) = mpsc::unbounded_channel();
        let (a_ch_tx, _a_ch_rx) = mpsc::unbounded_channel();
        let (b_ch_tx, _b_ch_rx) = mpsc::unbounded_channel();
        let (a_file_tx, _a_file_rx) = mpsc::unbounded_channel();
        let (b_file_tx, _b_file_rx) = mpsc::unbounded_channel();
        let alice_node = Node::open(
            alice,
            alice_roster,
            alice_tx,
            a_ch_tx,
            a_file_tx,
            &dir.path().join("alice.log"),
            &dir.path().join("alice-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open(
            bob,
            bob_roster,
            bob_tx,
            b_ch_tx,
            b_file_tx,
            &dir.path().join("bob.log"),
            &dir.path().join("bob-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(listener));

        // Alice sends two DMs; Bob receives both via the ratchet.
        alice_node.send_dm(&bob_uid, b"first").await.unwrap();
        let r1 = tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
            .await
            .expect("bob got the first dm")
            .expect("stream open");
        assert_eq!(r1.text, b"first");
        assert_eq!(r1.from, alice_uid);
        alice_node.send_dm(&bob_uid, b"second").await.unwrap();
        let r2 = tokio::time::timeout(Duration::from_secs(5), bob_rx.recv())
            .await
            .expect("bob got the second dm")
            .expect("stream open");
        assert_eq!(r2.text, b"second");

        // (a) Bob's dm_history shows both (served from his received store).
        let conv = dm_conversation_id(&alice_pub, &bob_pub);
        let hist = bob_node.dm_history(&alice_pub, 10);
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].text, b"first");
        assert_eq!(hist[1].text, b"second");

        // (b) Re-feeding the same wire events does NOT re-surface them: the ratchet
        // keys were consumed (single-use) AND they're marked emitted. Draining again
        // yields nothing new.
        bob_node.emit_new_messages(conv);
        assert!(
            bob_rx.try_recv().is_err(),
            "consumed-key events must not re-surface"
        );
        assert_eq!(
            bob_node.dm_history(&alice_pub, 10).len(),
            2,
            "history unchanged after re-feed"
        );

        // (c) Out-of-order: Alice seals m0,m1,m2 and Bob is fed them as 2,0,1. The
        // ratchet opens skipped messages, so all three surface. Use a fresh peer pair
        // so this exercises a clean session.
        let carol = DeviceIdentity::generate();
        let dave = DeviceIdentity::generate();
        let carol_pub = carol.public();
        let dave_pub = dave.public();
        let dave_uid = dave_pub.user_id();
        let conv2 = dm_conversation_id(&carol_pub, &dave_pub);

        // Carol's ratchet (seals the wire she "sends"); only the bytes matter.
        let carol_sess_dir = dir.path().join("carol-sess");
        std::fs::create_dir_all(&carol_sess_dir).unwrap();
        let mut carol_ratchet = DmRatchet::new(
            RatchetSessions::open(&carol_sess_dir.join("ratchet.sessions"), "pw").unwrap(),
        );
        let mk = |r: &mut DmRatchet, body: &[u8], seq: u64, wc: u64| {
            let wire = r
                .encrypt(
                    &carol,
                    &dave_pub,
                    &MessageBody::new(body.to_vec(), None).encode(),
                )
                .unwrap();
            Event::new(
                &carol,
                conv2,
                seq,
                vec![],
                seq,
                wc,
                EventKind::Message,
                wire,
            )
        };
        let m0 = mk(&mut carol_ratchet, b"m0", 1, 1000);
        let m1 = mk(&mut carol_ratchet, b"m1", 2, 2000);
        let m2 = mk(&mut carol_ratchet, b"m2", 3, 3000);

        // Dave's node knows Carol.
        let dave_roster = seed_roster(&carol, "Carol", 4000, &dave_uid);
        let (dave_tx, mut dave_rx) = mpsc::unbounded_channel();
        let (d_ch_tx, _d_ch_rx) = mpsc::unbounded_channel();
        let (d_file_tx, _d_file_rx) = mpsc::unbounded_channel();
        let dave_node = Node::open(
            dave,
            dave_roster,
            dave_tx,
            d_ch_tx,
            d_file_tx,
            &dir.path().join("dave.log"),
            &dir.path().join("dave-sent.log"),
            "pw",
        )
        .unwrap();

        // Feed them in arrival order 2,0,1 into Dave's log, draining after each.
        for ev in [m2, m0, m1] {
            dave_node.log.lock().unwrap().append(ev).unwrap();
            dave_node.emit_new_messages(conv2);
        }
        let mut got: Vec<Vec<u8>> = Vec::new();
        while let Ok(dm) = dave_rx.try_recv() {
            got.push(dm.text);
        }
        got.sort();
        assert_eq!(
            got,
            vec![b"m0".to_vec(), b"m1".to_vec(), b"m2".to_vec()],
            "out-of-order delivery surfaces all three messages"
        );
        // History (sorted by wall_clock) shows them in send order.
        let dhist = dave_node.dm_history(&carol_pub, 10);
        assert_eq!(
            dhist.iter().map(|e| e.text.clone()).collect::<Vec<_>>(),
            vec![b"m0".to_vec(), b"m1".to_vec(), b"m2".to_vec()]
        );
    }

    /// Seed `roster` with `peer` advertised under `account`, reachable at `port`.
    fn add_account_peer(
        roster: &Arc<Mutex<Roster>>,
        peer: &DeviceIdentity,
        account: &crate::identity::account::Account,
        name: &str,
        port: u16,
        self_user_id: &str,
    ) {
        roster.lock().unwrap().update(
            &Announce::new_with_account(peer, account, name, port),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            self_user_id,
        );
    }

    #[tokio::test]
    async fn send_to_account_delivers_to_a_peer_account_over_loopback() {
        use crate::identity::account::Account;
        let alice = DeviceIdentity::generate();
        let alice_acct = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_acct = Account::generate();
        let alice_uid = alice.public().user_id();
        let bob_uid = bob.public().user_id();
        let dir = tempfile::tempdir().unwrap();

        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();

        let alice_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(
            &alice_roster,
            &bob,
            &bob_acct,
            "Bob",
            bob_addr.port(),
            &alice_uid,
        );
        let bob_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(&bob_roster, &alice, &alice_acct, "Alice", 4000, &bob_uid);

        let (a_dm, _a_dm_r) = mpsc::unbounded_channel();
        let (a_ch, _a_ch_r) = mpsc::unbounded_channel();
        let (a_f, _a_f_r) = mpsc::unbounded_channel();
        let (b_dm, _b_dm_r) = mpsc::unbounded_channel();
        let (b_ch, _b_ch_r) = mpsc::unbounded_channel();
        let (b_f, _b_f_r) = mpsc::unbounded_channel();

        let alice_node = Node::open_with_account(
            alice,
            alice_acct.account_id(),
            alice_roster,
            a_dm,
            a_ch,
            a_f,
            &dir.path().join("a.log"),
            &dir.path().join("a-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open_with_account(
            bob,
            bob_acct.account_id(),
            bob_roster,
            b_dm,
            b_ch,
            b_f,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

        alice_node
            .send_to_account(&bob_acct.account_id(), b"hi account", None)
            .await
            .unwrap();

        // Bob's account history (keyed by Alice's account) shows the message.
        let mut bob_hist = Vec::new();
        for _ in 0..50 {
            bob_hist = bob_node.account_history(&alice_acct.account_id(), 10);
            if !bob_hist.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(
            bob_hist.len(),
            1,
            "bob received the account-addressed message"
        );
        assert!(!bob_hist[0].from_me);
        assert_eq!(bob_hist[0].text, b"hi account");

        // Alice's own account history shows it once, from her.
        let a_hist = alice_node.account_history(&bob_acct.account_id(), 10);
        assert_eq!(a_hist.len(), 1);
        assert!(a_hist[0].from_me);
        assert_eq!(a_hist[0].text, b"hi account");
    }

    #[tokio::test]
    async fn send_to_account_self_syncs_to_own_other_device() {
        use crate::identity::account::Account;
        // Alice has TWO devices sharing one account; Bob is a separate account.
        let a1 = DeviceIdentity::generate();
        let a2 = DeviceIdentity::generate();
        let alice_acct = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_acct = Account::generate();
        let a1_uid = a1.public().user_id();
        let dir = tempfile::tempdir().unwrap();

        let a2_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a2_addr = a2_listener.local_addr().unwrap();
        let bob_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bob_addr = bob_listener.local_addr().unwrap();

        // A1 knows its sibling A2 (same account) and Bob (other account).
        let a1_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(&a1_roster, &a2, &alice_acct, "A2", a2_addr.port(), &a1_uid);
        add_account_peer(&a1_roster, &bob, &bob_acct, "Bob", bob_addr.port(), &a1_uid);
        let a2_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(
            &a2_roster,
            &a1,
            &alice_acct,
            "A1",
            4000,
            &a2.public().user_id(),
        );
        let bob_roster = Arc::new(Mutex::new(Roster::default()));
        add_account_peer(
            &bob_roster,
            &a1,
            &alice_acct,
            "A1",
            4000,
            &bob.public().user_id(),
        );

        let (a1_dm, _x1) = mpsc::unbounded_channel();
        let (a1_ch, _x2) = mpsc::unbounded_channel();
        let (a1_f, _x3) = mpsc::unbounded_channel();
        let (a2_dm, _x4) = mpsc::unbounded_channel();
        let (a2_ch, _x5) = mpsc::unbounded_channel();
        let (a2_f, _x6) = mpsc::unbounded_channel();
        let (b_dm, _x7) = mpsc::unbounded_channel();
        let (b_ch, _x8) = mpsc::unbounded_channel();
        let (b_f, _x9) = mpsc::unbounded_channel();

        let a1_node = Node::open_with_account(
            a1,
            alice_acct.account_id(),
            a1_roster,
            a1_dm,
            a1_ch,
            a1_f,
            &dir.path().join("a1.log"),
            &dir.path().join("a1-sent.log"),
            "pw",
        )
        .unwrap();
        let a2_node = Node::open_with_account(
            a2,
            alice_acct.account_id(),
            a2_roster,
            a2_dm,
            a2_ch,
            a2_f,
            &dir.path().join("a2.log"),
            &dir.path().join("a2-sent.log"),
            "pw",
        )
        .unwrap();
        let bob_node = Node::open_with_account(
            bob,
            bob_acct.account_id(),
            bob_roster,
            b_dm,
            b_ch,
            b_f,
            &dir.path().join("b.log"),
            &dir.path().join("b-sent.log"),
            "pw",
        )
        .unwrap();

        tokio::spawn(Arc::clone(&a2_node).run_accept_loop(a2_listener));
        tokio::spawn(Arc::clone(&bob_node).run_accept_loop(bob_listener));

        a1_node
            .send_to_account(&bob_acct.account_id(), b"to bob", None)
            .await
            .unwrap();

        // A2 (Alice's other device) shows the message in the Alice↔Bob conversation,
        // marked from_me (it was sent by Alice's account).
        let mut a2_hist = Vec::new();
        for _ in 0..50 {
            a2_hist = a2_node.account_history(&bob_acct.account_id(), 10);
            if !a2_hist.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(a2_hist.len(), 1, "A2 self-synced the message");
        assert!(a2_hist[0].from_me, "shown as ours on the other device");
        assert_eq!(a2_hist[0].text, b"to bob");

        // Bob receives it once, not from_me.
        let mut b_hist = Vec::new();
        for _ in 0..50 {
            b_hist = bob_node.account_history(&alice_acct.account_id(), 10);
            if !b_hist.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert_eq!(b_hist.len(), 1);
        assert!(!b_hist[0].from_me);
        assert_eq!(b_hist[0].text, b"to bob");
    }
}
