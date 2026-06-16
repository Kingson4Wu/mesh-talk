//! Encrypted, append-only on-disk log. Header: a 6-byte magic + a 16-byte salt.
//! Body: a sequence of length-prefixed AES-256-GCM records, one per event:
//! `[u32 be record_len][nonce(12)][ciphertext]`, where `ciphertext` is the
//! AES-GCM encryption of `bincode(Event)`. Reuses `storage::encryption`.
//!
//! ## Trust boundary
//!
//! Each record is independently AEAD-authenticated, so an event's *content*
//! cannot be forged or altered on disk without the tampered record failing to
//! decrypt (→ [`LogError::CorruptFile`]). What this layer does NOT provide is
//! truncation/reorder resistance: the length prefixes are not authenticated, so
//! a corrupted mid-file frame causes the remainder to be treated as a torn tail
//! and dropped, and an attacker who knows the password could reorder records.
//!
//! That is by design. The file is a local *cache*, not the source of truth.
//! Truncation and reordering are recovered by the higher layers: events are
//! content-addressed and signed, so on re-ingest via [`crate::eventlog::EventLog`]
//! any altered event is rejected, and the sync engine re-fetches missing events
//! from peers. A dropped or reordered local record is therefore self-healing.

use crate::eventlog::event::{Author, ConversationId, Event, EventId};
use crate::eventlog::store::{AppendOutcome, EventLog};
use crate::eventlog::sync::SyncStore;
use crate::eventlog::LogError;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use std::collections::{HashMap, HashSet};
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Atomically create-or-detect: `create_new` fails if the file exists,
        // so there is no exists()-then-create race.
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
                Ok((Self { file, key }, Vec::new()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Self::load(path, password),
            Err(e) => Err(e.into()),
        }
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
        let record_len = u32::try_from(NONCE_SIZE + ciphertext.len())
            .map_err(|_| LogError::Serialization("event record exceeds u32 length".into()))?;

        let mut buf = Vec::with_capacity(4 + NONCE_SIZE + ciphertext.len());
        buf.extend_from_slice(&record_len.to_be_bytes());
        buf.extend_from_slice(&nonce);
        buf.extend_from_slice(&ciphertext);
        self.file.write_all(&buf)?;
        self.file.flush()?;
        Ok(())
    }
}

/// A durable event log: an in-memory [`EventLog`] backed by an encrypted,
/// append-only [`LogFile`]. On append it validates, persists, then indexes —
/// so a write failure never leaves an unpersisted event in memory.
///
/// Invariant: every event in `log` has a record in `file`; after an interrupted
/// write `file` may hold at most one extra (torn) record, which is dropped on
/// the next [`PersistentEventLog::open`].
pub struct PersistentEventLog {
    log: EventLog,
    file: LogFile,
}

impl PersistentEventLog {
    /// Open (or create) the log at `path`, replaying any stored events.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, events) = LogFile::open(path, password)?;
        let mut log = EventLog::default();
        // File order IS causal order: `append` validates that all parents are
        // already indexed before writing, so a child can only appear in the file
        // after its parents — exactly what `index_trusted` requires. (Records are
        // also validated on first write and AEAD-authenticated at rest, so we
        // replay them as trusted.) Writing records via `LogFile` directly, or an
        // adversary reordering the file, breaks this; per the module trust
        // boundary, the sync layer heals any resulting stale state.
        for event in events {
            log.index_trusted(event);
        }
        Ok(Self { log, file })
    }

    /// Validate, persist, then index an event.
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        if !self.log.validate(&event)? {
            return Ok(AppendOutcome::Duplicate);
        }
        self.file.append_record(&event)?;
        self.log.index_trusted(event);
        Ok(AppendOutcome::Appended)
    }

    pub fn has(&self, id: &EventId) -> bool {
        self.log.has(id)
    }
    pub fn get(&self, id: &EventId) -> Option<&Event> {
        self.log.get(id)
    }
    pub fn events(&self, conversation: &ConversationId) -> Vec<&Event> {
        self.log.events(conversation)
    }
    pub fn heads(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.log.heads(conversation)
    }
    pub fn version_vector(&self, conversation: &ConversationId) -> HashMap<Author, u64> {
        self.log.version_vector(conversation)
    }
    pub fn prepare(&self, conversation: &ConversationId) -> (Vec<EventId>, u64) {
        self.log.prepare(conversation)
    }

    /// All event ids currently held (used to seed an already-emitted set).
    pub fn all_event_ids(&self) -> Vec<EventId> {
        self.log.all_event_ids()
    }
}

// NOTE: this impl is structurally identical to `impl SyncStore for EventLog`
// in sync.rs — keep the two in sync.
impl SyncStore for PersistentEventLog {
    fn event_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.events(conversation).iter().map(|e| e.id).collect()
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Vec<Event> {
        self.events(conversation)
            .into_iter()
            .filter(|e| !have.contains(&e.id))
            .cloned()
            .collect()
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{ConversationId, EventKind};
    use crate::eventlog::sync::{reconcile, SyncStore};
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

    #[test]
    fn torn_length_prefix_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        {
            let (mut f, _) = LogFile::open(&path, "pw").unwrap();
            f.append_record(&mk(&id, 1, 1, b"good")).unwrap();
        }
        // A crash mid-length-prefix: only 2 of the 4 prefix bytes were written.
        {
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&[0xAB, 0xCD]).unwrap();
        }
        let (_f, events) = LogFile::open(&path, "pw").unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn multiple_reopen_append_cycles_accumulate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();
        {
            let (mut f, _) = LogFile::open(&path, "pw").unwrap();
            f.append_record(&mk(&id, 1, 1, b"one")).unwrap();
        }
        {
            let (mut f, events) = LogFile::open(&path, "pw").unwrap();
            assert_eq!(events.len(), 1);
            f.append_record(&mk(&id, 2, 2, b"two")).unwrap();
        }
        let (_f, events) = LogFile::open(&path, "pw").unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn header_only_log_reopens_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        {
            let (_f, events) = LogFile::open(&path, "pw").unwrap(); // writes header, no records
            assert!(events.is_empty());
        }
        let (_f, events) = LogFile::open(&path, "pw").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn durable_log_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();

        let root = mk(&id, 1, 1, b"root");
        let child = Event::new(
            &id,
            conv,
            2,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            plog.append(root.clone()).unwrap();
            plog.append(child.clone()).unwrap();
            assert_eq!(plog.heads(&conv), vec![child.id]);
        }

        // Reopen: events, heads, and version vector are rebuilt from disk.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        let events = plog.events(&conv);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, root.id);
        assert_eq!(events[1].id, child.id);
        assert_eq!(plog.heads(&conv), vec![child.id]);
        assert_eq!(plog.version_vector(&conv).get(&root.author), Some(&2));
    }

    #[test]
    fn invalid_event_is_not_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let id = DeviceIdentity::generate();

        let mut bad = mk(&id, 1, 1, b"bad");
        bad.sig[0] ^= 0xFF; // break the signature
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            assert!(matches!(plog.append(bad), Err(LogError::BadSignature)));
        }
        // Reopen: nothing was written.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.events(&ConversationId::new([1u8; 32])).len(), 0);
    }

    #[test]
    fn duplicate_append_writes_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();
        let e = mk(&id, 1, 1, b"hi");
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            assert_eq!(plog.append(e.clone()).unwrap(), AppendOutcome::Appended);
            assert_eq!(plog.append(e.clone()).unwrap(), AppendOutcome::Duplicate);
        }
        // Reopen: the duplicate was not written a second time.
        let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.events(&conv).len(), 1);
        // Re-appending the same event after reopen is still a Duplicate.
        assert_eq!(plog.append(e.clone()).unwrap(), AppendOutcome::Duplicate);
    }

    #[test]
    fn concurrent_events_survive_reopen_with_both_heads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();

        let root = mk(&alice, 1, 1, b"root");
        // Two concurrent children of root, from different authors → two heads.
        let a = Event::new(
            &alice,
            conv,
            2,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"a".to_vec(),
        );
        let b = Event::new(
            &bob,
            conv,
            1,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"b".to_vec(),
        );
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            plog.append(root.clone()).unwrap();
            plog.append(a.clone()).unwrap();
            plog.append(b.clone()).unwrap();
        }

        // Reopen: the two-head frontier and per-author version vector are restored.
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        let mut frontier = plog.heads(&conv);
        frontier.sort();
        let mut expected = vec![a.id, b.id];
        expected.sort();
        assert_eq!(frontier, expected);
        assert_eq!(plog.events(&conv).len(), 3);
        assert_eq!(plog.version_vector(&conv).get(&a.author), Some(&2)); // alice: root(1), a(2)
        assert_eq!(plog.version_vector(&conv).get(&b.author), Some(&1)); // bob: b(1)
    }

    #[test]
    fn persistent_store_syncs_and_persists_received_events() {
        let dir = tempfile::tempdir().unwrap();
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();

        // Source log has a root + child.
        let root = mk(&id, 1, 1, b"root");
        let child = Event::new(
            &id,
            conv,
            2,
            vec![root.id],
            2,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );

        let src_path = dir.path().join("src.log");
        let mut source = PersistentEventLog::open(&src_path, "pw").unwrap();
        source.append(root.clone()).unwrap();
        source.append(child.clone()).unwrap();

        // Empty destination syncs from the source.
        let dst_path = dir.path().join("dst.log");
        {
            let mut dest = PersistentEventLog::open(&dst_path, "pw").unwrap();
            reconcile(&mut dest, &mut source, conv);
            assert!(dest.has(&root.id) && dest.has(&child.id));
            // Sanity: the SyncStore view agrees.
            assert_eq!(SyncStore::event_ids(&dest, &conv).len(), 2);
        }

        // Received events were persisted — they survive reopen.
        let dest = PersistentEventLog::open(&dst_path, "pw").unwrap();
        assert_eq!(dest.events(&conv).len(), 2);
    }

    #[test]
    fn append_after_reopen_uses_replayed_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.log");
        let conv = ConversationId::new([1u8; 32]);
        let id = DeviceIdentity::generate();

        let root = mk(&id, 1, 1, b"root");
        {
            let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
            plog.append(root.clone()).unwrap();
        }

        // Reopen, derive the next event from the replayed state via `prepare`.
        let mut plog = PersistentEventLog::open(&path, "pw").unwrap();
        let (parents, lamport) = plog.prepare(&conv);
        assert_eq!(parents, vec![root.id]);
        assert_eq!(lamport, 2);
        let child = Event::new(
            &id,
            conv,
            2,
            parents,
            lamport,
            0,
            EventKind::Message,
            b"child".to_vec(),
        );
        plog.append(child.clone()).unwrap();
        assert_eq!(plog.heads(&conv), vec![child.id]);

        // The appended-after-reopen event persists across another reopen.
        drop(plog);
        let plog = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(plog.events(&conv).len(), 2);
        assert_eq!(plog.heads(&conv), vec![child.id]);
    }
}
