//! Direct + account-addressed message sends and DM reactions. Split out of node.rs (one `impl Node` block per domain).

use super::node::{now_millis, random_msg_id};
use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{Author, Event, EventId, EventKind};
use crate::node::conversation::{account_conversation_id, dm_conversation_id};

impl Node {
    /// The deduped set of devices an account-addressed message fans out to: every known
    /// device of `target_account_id` PLUS our own account's other devices, never ourselves,
    /// de-duped by user-id. (Note-to-self makes target/own overlap; the dedup collapses it,
    /// so a device never receives — or surfaces — the same message twice.) Callers that
    /// require the target to be reachable check for a target device in the result themselves.
    pub(in crate::node) fn account_fanout_targets(
        &self,
        target_account_id: &str,
    ) -> Vec<PeerRecord> {
        let my_account = self.account.account_id();
        let me = self.identity.public().user_id();
        let roster = self.roster.lock().expect("roster mutex not poisoned");
        let mut seen = std::collections::HashSet::new();
        roster
            .peers()
            .into_iter()
            .filter(|p| {
                let a = p.account_id.as_deref();
                a == Some(target_account_id) || a == Some(my_account.as_str())
            })
            .filter(|p| p.public.user_id() != me) // never seal to ourselves
            .filter(|p| seen.insert(p.public.user_id())) // dedup by user-id
            .collect()
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
        // Resolve the recipient: a LAN peer (roster — delivered direct + post office) or a
        // gossip-only peer such as a phone (presence directory — never on LAN discovery, reached
        // through the gateway sync this node hubs). The phone's DMs are device-pair, so the desktop
        // addresses it the same way (this is the desktop↔device-pair alignment).
        let roster_peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(recipient)
            .cloned();
        let peer_public = match &roster_peer {
            Some(p) => p.public.clone(),
            None => self
                .gossip_directory()
                .into_iter()
                .find(|(id, _, _, _)| id == recipient)
                .map(|(_, pid, _, _)| pid)
                .ok_or_else(|| NodeError::UnknownPeer(recipient.to_string()))?,
        };

        let wrapped = MessageBody::new(text.to_vec(), reply_to).encode();
        let conv = dm_conversation_id(&self.identity.public(), &peer_public);
        log::info!(
            "[mesh] send DM to {recipient} (gossip-only={}) conv={}",
            roster_peer.is_none(),
            hex::encode(&conv.as_bytes()[..8])
        );
        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Seal the wrapped body with the Double Ratchet (forward-secret). Compute the
        // wire OUTSIDE the log lock so no lock spans the seal.
        let wire = {
            let mut r = self
                .dm_ratchet
                .lock()
                .expect("dm_ratchet mutex not poisoned");
            r.encrypt(&self.identity, &peer_public, &wrapped)
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

        // Deliver. LAN peer: best-effort direct + always replicate to the post office
        // (store-and-forward). Gossip-only peer (phone): there is no LAN transport — the gateway
        // sync (this node hubs the relay the phone is connected to) carries the appended event when
        // the phone next pulls, so a durable append IS the send.
        let result = match roster_peer {
            Some(peer) => {
                let direct = self.deliver_direct(&peer, conv).await;
                let replicated = self.replicate_to_post_office(conv).await;
                match (direct, replicated) {
                    (Ok(()), _) => Ok(()),                             // delivered directly
                    (Err(_), Ok(true)) => Ok(()),                      // a post office holds it
                    (Err(e), Ok(false)) => Err(NodeError::Session(e)), // offline peer, no PO
                    (Err(_), Err(e)) => Err(NodeError::Session(e)),    // both paths failed
                }
            }
            None => Ok(()), // gossip peer: durably appended; the gateway sync delivers it
        };
        // Either round may also have pulled events back to us.
        self.emit_new_messages(conv);
        result
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

        // Resolve destinations: the target account's devices + our OWN other devices (deduped).
        let dests = self.account_fanout_targets(target_account_id);
        if !dests
            .iter()
            .any(|p| p.account_id.as_deref() == Some(target_account_id))
        {
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

        // Fan out: seal+append+deliver one copy per (deduped) destination device.
        for peer in &dests {
            self.deliver_enveloped(peer, &envelope).await;
        }
        Ok(())
    }

    /// Seal `plaintext` to `peer`'s device ratchet, append it to the device-pair
    /// conversation's event log, and deliver it (direct, then post office). Best-effort:
    /// transport failures are swallowed (the event is durably logged and syncs later).
    /// The conversation id here is the per-DEVICE-pair id (the transport layer).
    pub(in crate::node) async fn deliver_enveloped(&self, peer: &PeerRecord, plaintext: &[u8]) {
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
}
