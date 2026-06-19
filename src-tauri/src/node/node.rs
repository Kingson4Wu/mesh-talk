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
use crate::identity::account::Account;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::channel::{seal_keys_for, ChannelBook, ReceivedChannelMessage};
use crate::node::channel_senders::ChannelSenderStore;
use crate::node::conversation::{account_conversation_id, dm_conversation_id};
use crate::node::dm_envelope::{DmEnvelope, ReactionEnvelope};
use crate::node::dm_ratchet::DmRatchet;
use crate::node::filebook::{FileBook, ReceivedFile};
use crate::node::message::MessageBody;
use crate::node::pairing::{BackfillRecord, PairingCode, PairingRequest, PairingResponse};
use crate::node::postbox::elected_post_office;
use crate::node::ratchet_sessions::RatchetSessions;
use crate::node::reaction::{aggregate, ReactionPayload, ReactionView};
use crate::node::received_log::ReceivedLog;
use crate::node::sentlog::SentLog;
use crate::node::session::{request_round, serve_one, serve_wire_bytes, Served, SessionError};
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
    identity: DeviceIdentity,
    /// This node's cryptographic account (cross-device handle). A per-device account
    /// derived from the device key when opened via [`Node::open`]; the real shared
    /// account is injected by [`Node::open_with_account`]. Holds the secret so this
    /// device can serve a pairing request (transfer the account to a new device).
    account: Account,
    /// One-time pairing code while in "link a device" mode (linker side). Set by
    /// [`Node::start_linking`], cleared on success or [`Node::stop_linking`].
    pending_link: Mutex<Option<PairingCode>>,
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
        let my_account = self.account.account_id();
        let inner = MessageBody::new(text.to_vec(), reply_to).encode();
        // A stable logical id shared by every per-device copy, so reactions/replies
        // can target this message account-wide.
        let msg_id = random_msg_id();
        let envelope = DmEnvelope::new(
            my_account.clone(),
            target_account_id.to_string(),
            msg_id,
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
            // Store the full envelope (carries msg_id) so account history surfaces a
            // stable id for reactions/replies.
            let mut sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            let seq = sentlog.entries(&conv_account).len() as u64 + 1;
            let _ = sentlog.record(conv_account, seq, wall_clock, &envelope);
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

    /// Enter "link a device" mode: generate a one-time code to display. The next valid
    /// pairing request from a device proving this code is served the account secret.
    pub fn start_linking(&self) -> String {
        let code = PairingCode::generate();
        let hex = code.as_hex();
        *self.pending_link.lock().expect("pending_link mutex") = Some(code);
        hex
    }

    /// Leave linking mode (clear any pending code).
    pub fn stop_linking(&self) {
        *self.pending_link.lock().expect("pending_link mutex") = None;
    }

    /// Joiner side: dial `addr` (the linker, pinned to `peer_public`), prove `code_hex`,
    /// and receive the account secret + a certificate for this device.
    pub async fn link_to_device(
        &self,
        addr: std::net::SocketAddr,
        peer_public: &PublicIdentity,
        code_hex: &str,
    ) -> Result<LinkedAccount, NodeError> {
        let code = PairingCode::from_hex(code_hex)
            .ok_or_else(|| NodeError::Channel("invalid pairing code".into()))?;
        let mut channel = dial(addr, &self.identity, Some(peer_public))
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let tag = code.authenticator(
            &peer_public.ed25519_pub,
            &self.identity.public().ed25519_pub,
        );
        let req = PairingRequest {
            joiner: self.identity.public(),
            tag,
        };
        channel
            .send(&req.encode())
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let resp_bytes = channel
            .recv()
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let resp = PairingResponse::decode(&resp_bytes)
            .ok_or_else(|| NodeError::Channel("pairing rejected".into()))?;
        // Verify the returned cert really binds THIS device to THAT account.
        if !resp.cert.verify()
            || resp.cert.device_ed25519_pub != self.identity.public().ed25519_pub
            || resp.cert.account_ed25519_pub != resp.account_ed25519_pub
        {
            return Err(NodeError::Channel("pairing cert invalid".into()));
        }
        // Receive the linker's account-history backfill (records until an empty frame),
        // so this device starts populated. Best-effort: stop on any recv error.
        let mut backfill = Vec::new();
        loop {
            let frame = match channel.recv().await {
                Ok(f) => f,
                Err(_) => break,
            };
            if frame.is_empty() {
                break; // terminator
            }
            match BackfillRecord::decode(&frame) {
                Some(rec) => backfill.push(rec),
                None => break,
            }
        }
        self.import_account_backfill(&backfill);

        let account = Account::from_secret_bytes(resp.account_secret);
        Ok(LinkedAccount {
            secret: resp.account_secret,
            account_id: account.account_id(),
        })
    }

    /// Linker side: handle a pairing request on an inbound (authenticated) channel.
    /// Releases the account secret only if a code is pending AND the request both
    /// proves it and binds the authenticated channel peer. Clears the code on success.
    async fn serve_pairing(&self, channel: &mut SecureChannel<TcpStream>, req: PairingRequest) {
        // Bind the proof to the Noise-authenticated peer.
        if req.joiner.ed25519_pub != channel.peer_identity().ed25519_pub {
            return;
        }
        let code = {
            self.pending_link
                .lock()
                .expect("pending_link mutex")
                .clone()
        };
        let Some(code) = code else {
            return;
        };
        let my_ed = self.identity.public().ed25519_pub;
        if !code.verify(&my_ed, &req.joiner.ed25519_pub, &req.tag) {
            return;
        }
        let cert = self.account.certify(&req.joiner.ed25519_pub);
        let resp = PairingResponse {
            account_secret: self.account.secret_bytes(),
            account_ed25519_pub: self.account.public().ed25519_pub,
            cert,
        };
        if channel.send(&resp.encode()).await.is_err() {
            return;
        }
        self.stop_linking(); // single-use
                             // Backfill: stream our account history so the new device starts populated,
                             // then an empty frame as terminator. Best-effort — failures just mean the
                             // joiner backfills nothing.
        for rec in self.export_account_backfill() {
            if channel.send(&rec.encode()).await.is_err() {
                return;
            }
        }
        let _ = channel.send(&[]).await;
    }

    /// Export every account-history message we hold (sent + received) as transferable
    /// [`BackfillRecord`]s — for handing to a freshly-linked device. Only entries that
    /// are account `DmEnvelope`s are included (device DMs / channels are skipped). Sent
    /// entries are attributed to our own account; received to the recorded sender.
    fn export_account_backfill(&self) -> Vec<BackfillRecord> {
        let my_account = self.account.account_id();
        let mut out = Vec::new();
        {
            let sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            for conv in sentlog.conversations() {
                for e in sentlog.entries(&conv) {
                    if let Some(env) = DmEnvelope::decode(&e.plaintext) {
                        out.push(BackfillRecord {
                            conv: *conv.as_bytes(),
                            from: my_account.clone(),
                            wall_clock: e.wall_clock,
                            plaintext: e.plaintext.clone(),
                            event_id: env.msg_id,
                        });
                    }
                }
            }
        }
        {
            let received = self.received.lock().expect("received mutex not poisoned");
            for conv in received.conversations() {
                for e in received.entries(&conv) {
                    if DmEnvelope::decode(&e.plaintext).is_some() {
                        out.push(BackfillRecord {
                            conv: *conv.as_bytes(),
                            from: e.from.clone(),
                            wall_clock: e.wall_clock,
                            plaintext: e.plaintext.clone(),
                            event_id: *e.event_id.as_bytes(),
                        });
                    }
                }
            }
        }
        out
    }

    /// Import backfilled account history into our received store (used by a freshly
    /// linked device). Each record is keyed by its account conversation id, which is
    /// identical on both devices once they share the account.
    fn import_account_backfill(&self, records: &[BackfillRecord]) {
        let mut received = self.received.lock().expect("received mutex not poisoned");
        for r in records {
            let _ = received.record(
                ConversationId::new(r.conv),
                r.from.clone(),
                r.wall_clock,
                &r.plaintext,
                EventId::new(r.event_id),
            );
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
    /// any newly-received DMs, until the peer disconnects. The first frame is peeked:
    /// a device-pairing request gets the linking handler; anything else is a sync wire
    /// and is served normally (then the loop continues).
    pub async fn serve_connection(&self, mut channel: SecureChannel<TcpStream>) {
        let first = match channel.recv().await {
            Ok(b) => b,
            Err(_) => return,
        };
        if let Some(req) = PairingRequest::decode(&first) {
            self.serve_pairing(&mut channel, req).await;
            return;
        }
        match serve_wire_bytes(&mut channel, &self.log, &first).await {
            Ok(Served::Handled(conv)) => {
                self.emit_new_messages(conv);
                self.process_channel(conv);
                self.process_file_events(conv);
            }
            _ => return,
        }
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

    /// Send a file to an ACCOUNT: stage it once (shared symmetric chunks), then seal
    /// the manifest to every known device of `target_account_id` AND our own other
    /// devices (self-sync), delivering the file + manifest to each. Mirrors
    /// [`Node::send_to_account`] for files. Errors only if no device of the target
    /// account is known.
    pub async fn send_file_to_account(
        &self,
        target_account_id: &str,
        path: &Path,
    ) -> Result<ConversationId, NodeError> {
        let my_account = self.account.account_id();
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

        let (manifest, file_conv) = self.stage_file(path)?;
        let manifest_bytes = manifest.encode();
        for peer in targets.iter().chain(own.iter()) {
            let sealed =
                match crate::dm::seal(&self.identity, &peer.public.x25519_pub, &manifest_bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
            let dm_conv = dm_conversation_id(&self.identity.public(), &peer.public);
            if self
                .append_event(dm_conv, EventKind::FileManifest, sealed)
                .is_err()
            {
                continue;
            }
            self.deliver_direct(peer, file_conv).await.ok();
            self.replicate_to_post_office(file_conv).await.ok();
            self.deliver_direct(peer, dm_conv).await.ok();
            self.replicate_to_post_office(dm_conv).await.ok();
        }
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
        let my_account = self.account.account_id();
        let conv = account_conversation_id(&my_account, peer_account_id);
        let mut entries: Vec<HistoryEntry> = Vec::new();

        // Account messages are stored as full `DmEnvelope`s; the logical `msg_id` is
        // the stable, cross-device id reactions/replies target.
        for sent in self
            .sentlog
            .lock()
            .expect("sentlog mutex not poisoned")
            .entries(&conv)
        {
            let (id, body) = decode_account_entry(&sent.plaintext);
            entries.push(HistoryEntry {
                id,
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
            let (id, body) = decode_account_entry(&rcv.plaintext);
            let from_me = rcv.from == my_account;
            entries.push(HistoryEntry {
                id,
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

    /// React to an account message (toggle off with `remove = true`). `target` is the
    /// logical message id (from account history). Fans the reaction out to every device
    /// of `target_account_id` + our own devices, so it aggregates account-wide.
    pub async fn react_to_account(
        &self,
        target_account_id: &str,
        target: EventId,
        emoji: &str,
        remove: bool,
    ) -> Result<(), NodeError> {
        let my_account = self.account.account_id();
        let me = self.identity.public().user_id();
        let dests: Vec<PeerRecord> = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            roster
                .peers()
                .into_iter()
                .filter(|p| {
                    let a = p.account_id.as_deref();
                    a == Some(target_account_id)
                        || (a == Some(my_account.as_str()) && p.public.user_id() != me)
                })
                .collect()
        };
        let envelope = ReactionEnvelope::new(
            my_account.clone(),
            target_account_id.to_string(),
            *target.as_bytes(),
            emoji.to_string(),
            remove,
        )
        .encode();
        for peer in &dests {
            let sealed = match crate::dm::seal(&self.identity, &peer.public.x25519_pub, &envelope) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let conv = dm_conversation_id(&self.identity.public(), &peer.public);
            if self.append_event(conv, EventKind::React, sealed).is_err() {
                continue;
            }
            self.deliver_direct(peer, conv).await.ok();
            self.replicate_to_post_office(conv).await.ok();
        }
        // Record our own reaction (sealed to peers, so un-openable from our own log)
        // under the account conversation, for `account_reactions` to merge.
        let acct_conv = account_conversation_id(&my_account, target_account_id);
        self.my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned")
            .push((
                acct_conv,
                ReactionPayload {
                    target,
                    emoji: emoji.to_string(),
                    remove,
                },
            ));
        Ok(())
    }

    /// Aggregated reactions for the account conversation with `peer_account_id`. Scans
    /// `React` events across every device-pair conversation between us and that
    /// account's devices (and our own devices), opens each account reaction envelope,
    /// keys it by its logical target + reacting account, and merges our own.
    pub fn account_reactions(&self, peer_account_id: &str) -> Vec<ReactionView> {
        let my_account = self.account.account_id();
        let me = self.identity.public().user_id();
        // The device-pair conversations whose React events can carry this account's
        // reactions: ours-with-each-target-device and ours-with-each-own-other-device.
        let convs: Vec<(ConversationId, [u8; 32])> = {
            let roster = self.roster.lock().expect("roster mutex not poisoned");
            roster
                .peers()
                .into_iter()
                .filter(|p| {
                    let a = p.account_id.as_deref();
                    a == Some(peer_account_id)
                        || (a == Some(my_account.as_str()) && p.public.user_id() != me)
                })
                .map(|p| {
                    (
                        dm_conversation_id(&self.identity.public(), &p.public),
                        p.public.x25519_pub,
                    )
                })
                .collect()
        };
        let mut decoded: Vec<(String, ReactionPayload)> = Vec::new();
        for (conv, author_x) in convs {
            let react_events: Vec<Event> = {
                let log = self.log.lock().expect("log mutex not poisoned");
                log.events(&conv)
                    .into_iter()
                    .filter(|e| e.kind == EventKind::React)
                    .cloned()
                    .collect()
            };
            for event in &react_events {
                let Ok(pt) = crate::dm::open(&self.identity, &author_x, &event.ciphertext) else {
                    continue;
                };
                if let Some(env) = ReactionEnvelope::decode(&pt) {
                    decoded.push((
                        env.route.sender_account,
                        ReactionPayload {
                            target: EventId::new(env.target),
                            emoji: env.emoji,
                            remove: env.remove,
                        },
                    ));
                }
            }
        }
        // Merge our own account reactions (sealed to peers; not openable from our log).
        let acct_conv = account_conversation_id(&my_account, peer_account_id);
        for (c, p) in self
            .my_dm_reactions
            .lock()
            .expect("my_dm_reactions mutex not poisoned")
            .iter()
        {
            if *c == acct_conv {
                decoded.push((my_account.clone(), p.clone()));
            }
        }
        aggregate(&decoded)
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
            let (record_conv, record_from, record_plaintext, body) =
                match DmEnvelope::decode(&wrapped) {
                    Some(env) => {
                        let my_account = self.account.account_id();
                        let counterparty = if env.route.sender_account == my_account {
                            env.route.recipient_account.clone() // self-synced copy of our own send
                        } else {
                            env.route.sender_account.clone()
                        };
                        let acct_conv = account_conversation_id(&my_account, &counterparty);
                        let body = MessageBody::decode(&env.body);
                        // Record the FULL envelope so account history recovers the
                        // logical msg_id (for reactions/replies).
                        (
                            acct_conv,
                            env.route.sender_account.clone(),
                            wrapped.clone(),
                            body,
                        )
                    }
                    None => {
                        let body = MessageBody::decode(&wrapped);
                        (conv, author_uid.clone(), wrapped.clone(), body)
                    }
                };
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
                    &record_plaintext,
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

/// A fresh random 32-byte logical message id (shared across an account send's
/// per-device copies, so reactions/replies can target it account-wide).
fn random_msg_id() -> [u8; 32] {
    use rand::RngCore;
    let mut id = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut id);
    id
}

/// Decode an account-history store entry (a full `DmEnvelope`) into its logical id
/// and inner body. A malformed or legacy entry falls back to a bare `MessageBody`
/// with a zero id, so it still renders rather than breaking history.
fn decode_account_entry(plaintext: &[u8]) -> (EventId, MessageBody) {
    match DmEnvelope::decode(plaintext) {
        Some(env) => (EventId::new(env.msg_id), MessageBody::decode(&env.body)),
        None => (EventId::new([0u8; 32]), MessageBody::decode(plaintext)),
    }
}

#[cfg(test)]
#[path = "node_tests.rs"]
mod tests;
