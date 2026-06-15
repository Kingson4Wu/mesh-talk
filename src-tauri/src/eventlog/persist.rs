//! Encrypted, append-only on-disk log. Header: a 6-byte magic + a 16-byte salt.
//! Body: a sequence of length-prefixed AES-256-GCM records, one per event:
//! `[u32 be record_len][nonce(12)][ciphertext]`, where `ciphertext` is the
//! AES-GCM encryption of `bincode(Event)`. Reuses `storage::encryption`.

use crate::eventlog::event::Event;
use crate::eventlog::LogError;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTLOG1";

/// An append-only encrypted log file plus the derived key for writing records.
pub struct LogFile {
    file: File,
    key: EncryptionKey,
}

impl LogFile {
    /// Open the log at `path`, creating it (with a fresh random salt) if absent.
    /// Returns the writer plus every event already stored, in file order.
    pub fn open(path: &Path, password: &str) -> Result<(Self, Vec<Event>), LogError> {
        if path.exists() {
            Self::load(path, password)
        } else {
            Self::create(path, password)
        }
    }

    fn create(path: &Path, password: &str) -> Result<(Self, Vec<Event>), LogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let salt = generate_salt();
        let key = EncryptionKey::from_password(password, &salt)?;
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        file.write_all(MAGIC)?;
        file.write_all(&salt)?;
        file.flush()?;
        Ok((Self { file, key }, Vec::new()))
    }

    fn load(path: &Path, password: &str) -> Result<(Self, Vec<Event>), LogError> {
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
        let events = Self::parse_records(&rest, &key)?;
        Ok((Self { file, key }, events))
    }

    /// Parse the record region. A torn trailing record (crash mid-write) is
    /// dropped; a record that fails to decrypt (tampering) is an error.
    fn parse_records(mut data: &[u8], key: &EncryptionKey) -> Result<Vec<Event>, LogError> {
        let mut events = Vec::new();
        loop {
            if data.len() < 4 {
                break; // clean end, or a torn length prefix
            }
            let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            data = &data[4..];
            if data.len() < len {
                break; // torn trailing record — drop it
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
            let event: Event = bincode::deserialize(&plaintext)
                .map_err(|e| LogError::CorruptFile(format!("record decode: {e}")))?;
            events.push(event);
        }
        Ok(events)
    }

    /// Append one event as an encrypted, length-prefixed record.
    pub fn append_record(&mut self, event: &Event) -> Result<(), LogError> {
        let plaintext =
            bincode::serialize(event).map_err(|e| LogError::Serialization(e.to_string()))?;
        let (ciphertext, nonce) = encrypt_data(&plaintext, &self.key)?;
        let record_len = (NONCE_SIZE + ciphertext.len()) as u32;

        let mut buf = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        buf.extend_from_slice(&record_len.to_be_bytes());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        self.file.write_all(&buf)?;
        self.file.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{ConversationId, EventKind};
    use crate::identity::device::DeviceIdentity;

    fn mk(id: &DeviceIdentity, seq: u64, lamport: u64, payload: &[u8]) -> Event {
        Event::new(
            id,
            ConversationId::new([1u8; 32]),
            seq,
            vec![],
            lamport,
            0,
            EventKind::Message,
            payload.to_vec(),
        )
    }

    #[test]
    fn append_then_reopen_returns_records_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        let e1 = mk(&id, 1, 1, b"one");
        let e2 = mk(&id, 2, 2, b"two");

        {
            let (mut f, existing) = LogFile::open(&path, "pw").unwrap();
            assert!(existing.is_empty());
            f.append_record(&e1).unwrap();
            f.append_record(&e2).unwrap();
        }

        let (_f, events) = LogFile::open(&path, "pw").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, e1.id);
        assert_eq!(events[1].id, e2.id);
    }

    #[test]
    fn wrong_password_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        {
            let (mut f, _) = LogFile::open(&path, "right").unwrap();
            f.append_record(&mk(&id, 1, 1, b"secret")).unwrap();
        }
        assert!(matches!(
            LogFile::open(&path, "wrong"),
            Err(LogError::CorruptFile(_))
        ));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        std::fs::write(&path, b"not a mesh-talk log file at all").unwrap();
        assert!(matches!(
            LogFile::open(&path, "pw"),
            Err(LogError::CorruptFile(_))
        ));
    }

    #[test]
    fn torn_trailing_record_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        {
            let (mut f, _) = LogFile::open(&path, "pw").unwrap();
            f.append_record(&mk(&id, 1, 1, b"good")).unwrap();
        }
        // Simulate a crash mid-write: a length prefix claiming more bytes than follow.
        {
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&100u32.to_be_bytes()).unwrap(); // claims 100 bytes
            file.write_all(&[0u8; 10]).unwrap(); // only 10 present
        }
        let (_f, events) = LogFile::open(&path, "pw").unwrap();
        assert_eq!(events.len(), 1); // the torn record is dropped, the good one survives
    }

    #[test]
    fn tampered_record_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        {
            let (mut f, _) = LogFile::open(&path, "pw").unwrap();
            f.append_record(&mk(&id, 1, 1, b"data")).unwrap();
        }
        // Flip the last byte (inside the AEAD tag region of the only record).
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();

        assert!(matches!(
            LogFile::open(&path, "pw"),
            Err(LogError::CorruptFile(_))
        ));
    }
}
