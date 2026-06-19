//! A local-only, password-encrypted store mapping a channel id to this node's
//! serialized per-epoch sending keys (`my_sender`). On-disk framing mirrors
//! [`crate::node::ratchet_sessions`] (magic + salt header, then length-prefixed
//! AES-256-GCM records). Replay is last-write-wins per channel, so the in-memory
//! index always holds the latest snapshot of our sending chains for each channel.
//!
//! Why this exists: `my_sender` is in-memory only and rebuilt EMPTY on restart (the
//! event log only carries OTHER members' sender-key distributions). Without this
//! store, a node that sends again post-restart generates a NEW sender key and a NEW
//! distribution, but receivers are first-wins on chains they already hold and keep
//! the OLD chain — so they cannot decrypt the post-restart messages. Persisting our
//! OLD chains lets us resume them, which is exactly what receivers can still follow.

use crate::eventlog::event::ConversationId;
use crate::eventlog::LogError;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTCSND";

/// One channel's sending-key snapshot: the channel id (hex) and the serialized
/// per-epoch sending keys (`epoch -> SenderKey::serialize()` bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SenderRecord {
    channel: String,
    senders: Vec<(u64, Vec<u8>)>,
}

/// An append-only, password-encrypted store of this node's channel sending keys, one
/// snapshot per channel. Last write wins: replaying the file rebuilds the latest
/// snapshot for every channel.
pub struct ChannelSenderStore {
    file: File,
    key: EncryptionKey,
    latest: HashMap<String, Vec<(u64, Vec<u8>)>>,
}

impl ChannelSenderStore {
    /// Open (or create) the channel-senders store at `path`.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(path)
        {
            Ok(mut file) => {
                let salt = generate_salt();
                let key = EncryptionKey::from_password(password, &salt)?;
                file.write_all(MAGIC)?;
                file.write_all(&salt)?;
                file.flush()?;
                Ok(Self {
                    file,
                    key,
                    latest: HashMap::new(),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Self::load(path, password),
            Err(e) => Err(e.into()),
        }
    }

    fn load(path: &Path, password: &str) -> Result<Self, LogError> {
        let mut file = OpenOptions::new().read(true).append(true).open(path)?;
        let mut magic = [0u8; 6];
        file.read_exact(&mut magic)
            .map_err(|_| LogError::CorruptFile("missing magic".into()))?;
        if &magic != MAGIC {
            return Err(LogError::CorruptFile("bad magic".into()));
        }
        let mut salt = [0u8; SALT_SIZE];
        file.read_exact(&mut salt)
            .map_err(|_| LogError::CorruptFile("missing salt".into()))?;
        let key = EncryptionKey::from_password(password, &salt)?;
        let mut rest = Vec::new();
        file.read_to_end(&mut rest)?;

        let mut latest: HashMap<String, Vec<(u64, Vec<u8>)>> = HashMap::new();
        for record in Self::parse_records(&rest, &key)? {
            latest.insert(record.channel, record.senders);
        }
        Ok(Self { file, key, latest })
    }

    /// Parse the record region. A torn trailing record (crash mid-write) is dropped; a
    /// record that fails to decrypt (tampering) is an error.
    fn parse_records(mut data: &[u8], key: &EncryptionKey) -> Result<Vec<SenderRecord>, LogError> {
        let mut records = Vec::new();
        loop {
            if data.len() < 4 {
                break;
            }
            let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            data = &data[4..];
            if data.len() < len {
                break; // torn trailing record
            }
            let record = &data[..len];
            data = &data[len..];
            if record.len() < NONCE_SIZE {
                return Err(LogError::CorruptFile("record shorter than nonce".into()));
            }
            let nonce: [u8; NONCE_SIZE] = record[..NONCE_SIZE]
                .try_into()
                .expect("nonce length checked");
            let ciphertext = &record[NONCE_SIZE..];
            let plaintext = decrypt_data(ciphertext, &nonce, key)
                .map_err(|_| LogError::CorruptFile("record failed to decrypt".into()))?;
            let record: SenderRecord = bincode::deserialize(&plaintext)
                .map_err(|e| LogError::CorruptFile(format!("record decode: {e}")))?;
            records.push(record);
        }
        Ok(records)
    }

    /// The latest stored sending keys for `channel` (empty if none).
    pub fn get(&self, channel: &ConversationId) -> Vec<(u64, Vec<u8>)> {
        self.latest
            .get(&hex::encode(channel.as_bytes()))
            .cloned()
            .unwrap_or_default()
    }

    /// Persist `senders` for `channel` (append a record; update the index).
    pub fn put(
        &mut self,
        channel: &ConversationId,
        senders: &[(u64, Vec<u8>)],
    ) -> Result<(), LogError> {
        let channel_hex = hex::encode(channel.as_bytes());
        let record = SenderRecord {
            channel: channel_hex.clone(),
            senders: senders.to_vec(),
        };
        let bytes =
            bincode::serialize(&record).map_err(|e| LogError::Serialization(e.to_string()))?;
        let (ciphertext, nonce) = encrypt_data(&bytes, &self.key)?;
        let record_len = u32::try_from(NONCE_SIZE + ciphertext.len())
            .map_err(|_| LogError::Serialization("sender record exceeds u32 length".into()))?;
        let mut buf = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        buf.extend_from_slice(&record_len.to_be_bytes());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        self.file.write_all(&buf)?;
        self.file.flush()?;
        self.latest.insert(channel_hex, senders.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::new_channel_id;

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        let mut store = ChannelSenderStore::open(&path, "pw").unwrap();
        let ch = new_channel_id();
        assert!(store.get(&ch).is_empty());
        let senders = vec![(0u64, vec![1, 2, 3]), (1u64, vec![4, 5, 6])];
        store.put(&ch, &senders).unwrap();
        assert_eq!(store.get(&ch), senders);
    }

    #[test]
    fn reopen_restores_latest_put_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        let ch = new_channel_id();
        {
            let mut store = ChannelSenderStore::open(&path, "pw").unwrap();
            store.put(&ch, &[(0u64, vec![1, 1, 1])]).unwrap();
            // A later put for the same channel must win on reopen.
            store.put(&ch, &[(0u64, vec![9, 9, 9])]).unwrap();
        }
        let store2 = ChannelSenderStore::open(&path, "pw").unwrap();
        assert_eq!(store2.get(&ch), vec![(0u64, vec![9, 9, 9])]);
    }

    #[test]
    fn wrong_password_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        {
            let mut store = ChannelSenderStore::open(&path, "right").unwrap();
            store.put(&new_channel_id(), &[(0u64, vec![1])]).unwrap();
        }
        assert!(matches!(
            ChannelSenderStore::open(&path, "wrong"),
            Err(LogError::CorruptFile(_))
        ));
    }
}
