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

    /// Build a file whose chunks have DISTINCT content (so reordering changes the bytes):
    /// chunk0 = all 0s, chunk1 = all 1s, chunk2 = a short all-2s tail.
    fn distinct_three_chunk() -> Vec<u8> {
        let mut data = vec![0u8; CHUNK_SIZE];
        data.extend(std::iter::repeat(1u8).take(CHUNK_SIZE));
        data.extend(std::iter::repeat(2u8).take(5));
        data
    }

    #[test]
    fn reordered_chunks_are_rejected() {
        let (m, chunks) = seal_file(&distinct_three_chunk());
        assert_eq!(m.chunk_count, 3);
        let reordered = vec![chunks[1].clone(), chunks[0].clone(), chunks[2].clone()];
        // Every chunk still opens, but concatenation order is wrong → checksum rejects.
        assert!(matches!(
            reassemble_and_verify(&m, &reordered),
            Err(FileError::ChecksumMismatch)
        ));
    }

    #[test]
    fn duplicated_chunk_is_rejected() {
        let (m, chunks) = seal_file(&distinct_three_chunk());
        // Same count, but c0 twice instead of c0,c1 → wrong bytes → checksum rejects.
        let dup = vec![chunks[0].clone(), chunks[0].clone(), chunks[2].clone()];
        assert!(reassemble_and_verify(&m, &dup).is_err());
    }

    #[test]
    fn a_missing_chunk_is_rejected_at_the_crypto_layer() {
        let (m, chunks) = seal_file(&distinct_three_chunk()); // 3 chunks
                                                              // The crypto layer doesn't check count (the node layer does) — undersupply must
                                                              // still fail the whole-file checksum rather than silently return partial data.
        let short = vec![chunks[0].clone(), chunks[1].clone()];
        assert!(matches!(
            reassemble_and_verify(&m, &short),
            Err(FileError::ChecksumMismatch)
        ));
    }

    #[test]
    fn a_wrong_file_key_fails_to_open() {
        let (mut m, chunks) = seal_file(b"important data");
        m.file_key[0] ^= 0xFF; // wrong key → first chunk fails AEAD, not a checksum miss
        assert!(matches!(
            reassemble_and_verify(&m, &chunks),
            Err(FileError::Decrypt)
        ));
    }

    #[test]
    fn each_sealed_chunk_uses_a_fresh_nonce() {
        // Nonce reuse under a fixed key is catastrophic for AES-GCM; sealing identical
        // plaintext twice must yield different ciphertext (a fresh random nonce).
        let key = FileKey::generate();
        let a = seal_chunk(&key, b"same bytes").unwrap();
        let b = seal_chunk(&key, b"same bytes").unwrap();
        assert_ne!(a, b);
    }
}
