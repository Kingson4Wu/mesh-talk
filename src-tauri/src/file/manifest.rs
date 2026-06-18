//! The `FileManifest`: the plaintext announcement of a file (sealed by the node layer
//! with the conversation crypto). Carries the per-file key + the id of the per-file
//! conversation whose events are the chunk ciphertexts, plus a whole-file checksum.

use crate::eventlog::event::ConversationId;
use crate::file::crypto::{file_checksum, open_chunk, FileError, FileKey};
use bincode::Options;
use serde::{Deserialize, Serialize};

/// The announcement of a shared file. Encoded then sealed (DM box or channel group
/// key) into a `FileManifest` event in the original conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileManifest {
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub checksum: [u8; 32],
    pub file_key: [u8; 32],
    pub file_conv: ConversationId,
    pub chunk_count: u32,
}

impl FileManifest {
    /// Serialize for an event payload (fixint, matching the codebase wire style).
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("file manifest serializes")
    }

    /// Parse a manifest, fail-closed (reject trailing bytes).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }

    /// The per-file key as a [`FileKey`].
    pub fn key(&self) -> FileKey {
        FileKey::from_bytes(self.file_key)
    }
}

/// Decrypt `chunks` (the per-file conversation's chunk ciphertexts, in order),
/// concatenate, and verify the whole-file SHA-256 against `manifest.checksum`.
/// Errors if a chunk fails to open or the checksum mismatches.
pub fn reassemble_and_verify(
    manifest: &FileManifest,
    chunks: &[Vec<u8>],
) -> Result<Vec<u8>, FileError> {
    let key = manifest.key();
    let mut out: Vec<u8> = Vec::with_capacity(manifest.size as usize);
    for chunk in chunks {
        out.extend_from_slice(&open_chunk(&key, chunk)?);
    }
    if file_checksum(&out) != manifest.checksum {
        return Err(FileError::ChecksumMismatch);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::crypto::{seal_chunk, split_chunks, CHUNK_SIZE};
    use rand::rngs::OsRng;
    use rand::RngCore;

    fn random_conv() -> ConversationId {
        let mut id = [0u8; 32];
        OsRng.fill_bytes(&mut id);
        ConversationId::new(id)
    }

    fn seal_file(data: &[u8]) -> (FileManifest, Vec<Vec<u8>>) {
        let key = FileKey::generate();
        let chunks: Vec<Vec<u8>> = split_chunks(data)
            .iter()
            .map(|c| seal_chunk(&key, c).unwrap())
            .collect();
        let manifest = FileManifest {
            name: "report.pdf".into(),
            size: data.len() as u64,
            mime: "application/pdf".into(),
            checksum: file_checksum(data),
            file_key: *key.as_bytes(),
            file_conv: random_conv(),
            chunk_count: chunks.len() as u32,
        };
        (manifest, chunks)
    }

    #[test]
    fn manifest_round_trips_and_rejects_trailing() {
        let (m, _) = seal_file(b"hello");
        assert_eq!(FileManifest::decode(&m.encode()), Some(m.clone()));
        let mut junk = m.encode();
        junk.push(0xAB);
        assert_eq!(FileManifest::decode(&junk), None);
    }

    #[test]
    fn reassembles_a_multi_chunk_file_and_verifies() {
        let data = vec![42u8; CHUNK_SIZE * 2 + 17];
        let (m, chunks) = seal_file(&data);
        assert_eq!(m.chunk_count, 3);
        assert_eq!(reassemble_and_verify(&m, &chunks).unwrap(), data);
    }

    #[test]
    fn an_empty_file_reassembles() {
        let (m, chunks) = seal_file(b"");
        assert_eq!(m.chunk_count, 1);
        assert_eq!(
            reassemble_and_verify(&m, &chunks).unwrap(),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn a_tampered_chunk_fails_reassembly() {
        let (m, mut chunks) = seal_file(b"important data");
        let last = chunks[0].len() - 1;
        chunks[0][last] ^= 0xFF;
        assert!(reassemble_and_verify(&m, &chunks).is_err());
    }

    #[test]
    fn a_wrong_checksum_is_rejected() {
        let (mut m, chunks) = seal_file(b"important data");
        m.checksum[0] ^= 0xFF;
        assert!(matches!(
            reassemble_and_verify(&m, &chunks),
            Err(FileError::ChecksumMismatch)
        ));
    }
}
