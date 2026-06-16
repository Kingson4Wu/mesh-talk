//! A local-only, password-encrypted sidecar recording the plaintext of DMs this
//! node SENT. Outbound DMs are sealed to the recipient, so the sender cannot
//! decrypt its own messages back from the synced event log; this store keeps a
//! private plaintext copy so history can show both sides. It is never synced,
//! never served to peers, and holds no signatures. On-disk framing mirrors
//! [`crate::eventlog::persist`] (magic + salt header, then length-prefixed
//! AES-256-GCM records) but stores [`SentEntry`] records, not events.

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

const MAGIC: &[u8; 6] = b"MTSENT";

/// One sent message's local plaintext record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentEntry {
    pub conversation: ConversationId,
    pub seq: u64,
    pub wall_clock: u64,
    pub plaintext: Vec<u8>,
}

/// An append-only, password-encrypted log of locally-sent message plaintext,
/// indexed in memory by conversation.
pub struct SentLog {
    file: File,
    key: EncryptionKey,
    by_conversation: HashMap<ConversationId, Vec<SentEntry>>,
}

impl SentLog {
    /// Open (or create) the sent log at `path`, replaying stored entries.
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
                    by_conversation: HashMap::new(),
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

        let mut by_conversation: HashMap<ConversationId, Vec<SentEntry>> = HashMap::new();
        for entry in Self::parse_records(&rest, &key)? {
            by_conversation
                .entry(entry.conversation)
                .or_default()
                .push(entry);
        }
        Ok(Self {
            file,
            key,
            by_conversation,
        })
    }

    /// Parse the record region. A torn trailing record (crash mid-write) is
    /// dropped; a record that fails to decrypt (tampering) is an error.
    fn parse_records(mut data: &[u8], key: &EncryptionKey) -> Result<Vec<SentEntry>, LogError> {
        let mut entries = Vec::new();
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
            let entry: SentEntry = bincode::deserialize(&plaintext)
                .map_err(|e| LogError::CorruptFile(format!("record decode: {e}")))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Record a sent message's plaintext: append an encrypted record, then index.
    pub fn record(
        &mut self,
        conversation: ConversationId,
        seq: u64,
        wall_clock: u64,
        plaintext: &[u8],
    ) -> Result<(), LogError> {
        let entry = SentEntry {
            conversation,
            seq,
            wall_clock,
            plaintext: plaintext.to_vec(),
        };
        let bytes =
            bincode::serialize(&entry).map_err(|e| LogError::Serialization(e.to_string()))?;
        let (ciphertext, nonce) = encrypt_data(&bytes, &self.key)?;
        let record_len = u32::try_from(NONCE_SIZE + ciphertext.len())
            .map_err(|_| LogError::Serialization("sent record exceeds u32 length".into()))?;
        let mut buf = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        buf.extend_from_slice(&record_len.to_be_bytes());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        self.file.write_all(&buf)?;
        self.file.flush()?;
        self.by_conversation
            .entry(conversation)
            .or_default()
            .push(entry);
        Ok(())
    }

    /// All sent entries for `conversation`, sorted by `(wall_clock, seq)`.
    pub fn entries(&self, conversation: &ConversationId) -> Vec<SentEntry> {
        let mut v = self
            .by_conversation
            .get(conversation)
            .cloned()
            .unwrap_or_default();
        v.sort_by_key(|e| (e.wall_clock, e.seq));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(n: u8) -> ConversationId {
        ConversationId::new([n; 32])
    }

    #[test]
    fn records_survive_reopen_and_filter_by_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sent.log");
        {
            let mut s = SentLog::open(&path, "pw").unwrap();
            s.record(conv(1), 1, 2000, b"second").unwrap();
            s.record(conv(1), 0, 1000, b"first").unwrap();
            s.record(conv(2), 1, 1500, b"other-conv").unwrap();
        }
        // Reopen from disk: entries are restored, per-conversation, time-ordered.
        let s = SentLog::open(&path, "pw").unwrap();
        let c1 = s.entries(&conv(1));
        assert_eq!(c1.len(), 2);
        assert_eq!(c1[0].plaintext, b"first"); // wall_clock 1000 sorts before 2000
        assert_eq!(c1[1].plaintext, b"second");
        assert_eq!(s.entries(&conv(2)).len(), 1);
        assert!(s.entries(&conv(9)).is_empty());
    }

    #[test]
    fn wrong_password_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sent.log");
        {
            let mut s = SentLog::open(&path, "right").unwrap();
            s.record(conv(1), 1, 1, b"secret").unwrap();
        }
        assert!(matches!(
            SentLog::open(&path, "wrong"),
            Err(LogError::CorruptFile(_))
        ));
    }

    #[test]
    fn header_only_log_reopens_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sent.log");
        {
            let _s = SentLog::open(&path, "pw").unwrap(); // header only, no records
        }
        let s = SentLog::open(&path, "pw").unwrap();
        assert!(s.entries(&conv(1)).is_empty());
    }
}
