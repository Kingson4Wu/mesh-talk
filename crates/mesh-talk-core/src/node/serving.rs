//! Outbound delivery (direct + post-office replication) and the inbound serve loop. Split out of node.rs (one `impl Node` block per domain).

use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{Author, ConversationId, Event, EventKind};
use crate::file::FileManifest;
use crate::node::conversation::{account_conversation_id, dm_conversation_id};
use crate::node::session::{request_round, serve_one, serve_wire_bytes, Served, SessionError};
use crate::node::transport::{dial, secure_accept};
use crate::transport::SecureChannel;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

impl Node {
    /// Dial `peer` directly and run one sync round for `conv`. Best-effort: the
    /// peer may be offline, in which case the dial fails.
    pub(in crate::node) async fn deliver_direct(
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
    pub(in crate::node) async fn replicate_to_post_office(
        &self,
        conv: ConversationId,
    ) -> Result<bool, SessionError> {
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
            // Only the (fast) TCP accept runs on the loop; the Noise handshake runs in the
            // spawned task (bounded by HANDSHAKE_TIMEOUT), so a peer that connects then stalls
            // mid-handshake can't park the loop and block all other inbound connections.
            let stream = match listener.accept().await {
                Ok((stream, _addr)) => stream,
                Err(_) => {
                    // A listener-level error (e.g. fd exhaustion) shouldn't stop the loop;
                    // back off briefly so a persistent error can't become a busy-spin.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            };
            let node = Arc::clone(&self);
            tokio::spawn(async move {
                if let Ok(channel) = secure_accept(stream, &node.identity).await {
                    node.serve_connection(channel).await;
                }
            });
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
    pub(in crate::node) fn process_file_events(&self, conv: ConversationId) {
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
                    .and_then(|s| s.open_sender_message(&event.author.user_id(), &event.ciphertext))
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
            // Persist the surfaced manifest durably so the file book's emitted set + this
            // manifest survive a restart — WITHOUT the old bug of marking never-opened
            // manifests emitted (which lost the file). Best-effort: a failure here at worst
            // re-surfaces the file after restart, never loses it.
            let _ = self
                .received_files
                .lock()
                .expect("received_files mutex not poisoned")
                .record(
                    manifest.file_conv,
                    event.author.user_id(),
                    event.wall_clock,
                    &manifest.encode(),
                    event.id,
                );
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
    pub(in crate::node) fn process_channel(&self, conv: ConversationId) {
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

    /// Decrypt (via the Double Ratchet) and emit any not-yet-emitted, non-self
    /// `Message` events in `conv`. A message is marked emitted ONLY after it
    /// successfully decrypts and is recorded — so a transiently-undecryptable one
    /// (author not yet in the roster, or its ratchet key not yet derivable) is
    /// retried on a later sync rather than lost.
    pub(in crate::node) fn emit_new_messages(&self, conv: ConversationId) {
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
            let (peer_public, peer_name, peer_account) = {
                let roster = self.roster.lock().expect("roster mutex not poisoned");
                match roster.get(&author_uid) {
                    Some(p) => (p.public.clone(), p.name.clone(), p.account_id.clone()),
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
                        // Bind the envelope's claimed sender account to the AUTHENTICATED
                        // author device's certified account — a device cannot forge a
                        // message "from" another account. (A self-synced copy is authored
                        // by our own device, which carries our own account, so it matches
                        // when sender_account == my_account.)
                        if peer_account.as_deref() != Some(env.route.sender_account.as_str()) {
                            self.emitted
                                .lock()
                                .expect("emitted mutex not poisoned")
                                .insert(event.id);
                            continue;
                        }
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
