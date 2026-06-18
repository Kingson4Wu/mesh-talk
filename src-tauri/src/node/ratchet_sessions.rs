//! A local-only, password-encrypted store mapping peer `user_id` to serialized
//! `RatchetState`. On-disk framing mirrors [`crate::node::sentlog`] (magic + salt
//! header, then length-prefixed AES-256-GCM records). Replay is last-write-wins
//! per peer, so the in-memory index always holds the latest session per peer.

use crate::eventlog::LogError;
use crate::ratchet::RatchetState;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTRTCH";

/// One peer's session record (the `state` bytes are `RatchetState::serialize()` output).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    peer: String,
    state: Vec<u8>,
}

/// An append-only, password-encrypted store of ratchet sessions, one per peer.
/// Last write wins: replaying the file rebuilds the latest session for every peer.
pub struct RatchetSessions {
    file: File,
    key: EncryptionKey,
    latest: HashMap<String, Vec<u8>>,
}

impl RatchetSessions {
    /// Open (or create) the session store at `path`.
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

        let mut latest: HashMap<String, Vec<u8>> = HashMap::new();
        for record in Self::parse_records(&rest, &key)? {
            latest.insert(record.peer, record.state);
        }
        Ok(Self { file, key, latest })
    }

    /// Parse the record region. A torn trailing record (crash mid-write) is
    /// dropped; a record that fails to decrypt (tampering) is an error.
    fn parse_records(mut data: &[u8], key: &EncryptionKey) -> Result<Vec<SessionRecord>, LogError> {
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
            let session: SessionRecord = bincode::deserialize(&plaintext)
                .map_err(|e| LogError::CorruptFile(format!("record decode: {e}")))?;
            records.push(session);
        }
        Ok(records)
    }

    /// The latest stored session for `peer`, deserialized, if any.
    pub fn get(&self, peer: &str) -> Option<RatchetState> {
        RatchetState::deserialize(self.latest.get(peer)?)
    }

    /// Persist `state` for `peer` (append a record; update the index).
    pub fn put(&mut self, peer: &str, state: &RatchetState) -> Result<(), LogError> {
        let state_bytes = state.serialize();
        let record = SessionRecord {
            peer: peer.to_string(),
            state: state_bytes.clone(),
        };
        let bytes =
            bincode::serialize(&record).map_err(|e| LogError::Serialization(e.to_string()))?;
        let (ciphertext, nonce) = encrypt_data(&bytes, &self.key)?;
        let record_len = u32::try_from(NONCE_SIZE + ciphertext.len())
            .map_err(|_| LogError::Serialization("session record exceeds u32 length".into()))?;
        let mut buf = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        buf.extend_from_slice(&record_len.to_be_bytes());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        self.file.write_all(&buf)?;
        self.file.flush()?;
        self.latest.insert(peer.to_string(), state_bytes);
        Ok(())
    }

    /// Returns `true` if there is a stored session for `peer`.
    pub fn has(&self, peer: &str) -> bool {
        self.latest.contains_key(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratchet::{init_alice, init_bob};
    use x25519_dalek::{PublicKey, StaticSecret};

    fn make_pair() -> (RatchetState, RatchetState) {
        let shared = [7u8; 32];
        let bob_secret = [5u8; 32];
        let bob_pub = PublicKey::from(&StaticSecret::from(bob_secret)).to_bytes();
        let alice = init_alice(&shared, &bob_pub);
        let bob = init_bob(&shared, bob_secret);
        (alice, bob)
    }

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let mut store = RatchetSessions::open(&path, "pw").unwrap();
        let (alice, _bob) = make_pair();
        assert!(!store.has("peer1"));
        store.put("peer1", &alice).unwrap();
        assert!(store.has("peer1"));
        // get returns a deserialized copy; verify it can encrypt (has a sending chain)
        let mut restored = store.get("peer1").unwrap();
        assert!(restored.ratchet_encrypt(b"test").is_ok());
    }

    #[test]
    fn reopen_restores_latest_put_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let (mut alice, mut bob) = make_pair();
        {
            let mut store = RatchetSessions::open(&path, "pw").unwrap();
            store.put("peer1", &alice).unwrap();
            // advance the session and put again — the second write should win
            let (h, ct) = alice.ratchet_encrypt(b"msg1").unwrap();
            bob.ratchet_decrypt(&h, &ct).unwrap();
            store.put("peer1", &alice).unwrap();
        }
        // Reopen: should get the latest (advanced) session, not the first
        let store2 = RatchetSessions::open(&path, "pw").unwrap();
        assert!(store2.has("peer1"));
        let mut restored = store2.get("peer1").unwrap();
        // The restored alice should be able to send msg2 (ns == 1, not 0)
        let (h2, ct2) = restored.ratchet_encrypt(b"msg2").unwrap();
        // Bob (not reopened) should decrypt it — this validates the session is the
        // advanced one (n=1, not n=0 which bob already consumed)
        assert_eq!(bob.ratchet_decrypt(&h2, &ct2).unwrap(), b"msg2");
    }

    #[test]
    fn wrong_password_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        {
            let (alice, _) = make_pair();
            let mut store = RatchetSessions::open(&path, "right").unwrap();
            store.put("peer1", &alice).unwrap();
        }
        assert!(matches!(
            RatchetSessions::open(&path, "wrong"),
            Err(LogError::CorruptFile(_))
        ));
    }
}
