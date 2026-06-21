//! File transfer: chunked send (DM/account/channel) and save. Split out of node.rs (one `impl Node` block per domain).

use super::node::MAX_FILE_SIZE;
use super::*;
use crate::eventlog::event::{ConversationId, EventKind};
use crate::file::{
    file_checksum, reassemble_and_verify, seal_chunk, split_chunks, FileKey, FileManifest,
};
use crate::node::conversation::dm_conversation_id;
use std::path::Path;

impl Node {
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
        let dests = self.account_fanout_targets(target_account_id);
        if !dests
            .iter()
            .any(|p| p.account_id.as_deref() == Some(target_account_id))
        {
            return Err(NodeError::UnknownPeer(target_account_id.to_string()));
        }

        let (manifest, file_conv) = self.stage_file(path)?;
        let manifest_bytes = manifest.encode();
        for peer in &dests {
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
    pub(in crate::node) fn stage_file(
        &self,
        path: &Path,
    ) -> Result<(FileManifest, ConversationId), NodeError> {
        let data = std::fs::read(path).map_err(|e| NodeError::File(format!("read file: {e}")))?;
        // Bounded multi-round sync now transfers chunk events across frames, so files
        // up to MAX_FILE_SIZE (8 MB) are supported. The practical limit is the have
        // id-set fitting one frame; files over the cap are rejected up front.
        if data.len() > MAX_FILE_SIZE {
            return Err(NodeError::File(format!(
                "file too large: {} bytes (max {MAX_FILE_SIZE})",
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
}
