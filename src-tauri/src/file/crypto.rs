//! File crypto: a per-file symmetric key + raw-key AES-256-GCM per chunk (the same
//! primitive as `channel::crypto`), a whole-file SHA-256 checksum for end-to-end
//! verification, and fixed-size chunking. Pure — no I/O, no network.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

const KEY_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;

/// The chunk size for splitting a file (48 KiB). Must be smaller than the
/// transport's `MAX_PLAINTEXT` (65,519 B) once AES-GCM overhead is added;
/// 48 KiB leaves ample room for sync framing overhead. The last chunk may be
/// smaller.
pub const CHUNK_SIZE: usize = 48 * 1024;

/// A per-file symmetric key (AES-256-GCM). Generated fresh per file; carried inside
/// the (sealed) `FileManifest` so only conversation members can decrypt chunks.
#[derive(Clone)]
pub struct FileKey([u8; KEY_SIZE]);

impl FileKey {
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_SIZE];
        OsRng.fill_bytes(&mut key);
        FileKey(key)
    }
    pub fn from_bytes(bytes: [u8; KEY_SIZE]) -> Self {
        FileKey(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        &self.0
    }
}

/// Errors from file crypto.
#[derive(Debug)]
pub enum FileError {
    /// AES-256-GCM chunk encryption failed.
    Encrypt,
    /// AES-256-GCM chunk decryption failed (wrong key or tampered).
    Decrypt,
    /// A malformed chunk envelope (too short for nonce + tag).
    Malformed(String),
    /// Reassembled plaintext failed its SHA-256 checksum.
    ChecksumMismatch,
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Encrypt => write!(f, "file chunk encrypt error"),
            FileError::Decrypt => write!(f, "file chunk decrypt error"),
            FileError::Malformed(m) => write!(f, "malformed file chunk: {m}"),
            FileError::ChecksumMismatch => write!(f, "file checksum mismatch after reassembly"),
        }
    }
}

impl std::error::Error for FileError {}

/// SHA-256 of the whole file plaintext — recorded in the manifest, re-checked after
/// reassembly for end-to-end integrity.
pub fn file_checksum(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Split `data` into `CHUNK_SIZE` pieces (the last may be smaller). An empty input
/// yields a single empty chunk so the manifest always has `chunk_count >= 1` and
/// reassembly is symmetric.
pub fn split_chunks(data: &[u8]) -> Vec<&[u8]> {
    if data.is_empty() {
        return vec![&data[0..0]];
    }
    data.chunks(CHUNK_SIZE).collect()
}

/// Seal one chunk: `nonce ‖ AES-256-GCM(key, nonce, plaintext)`, fresh 96-bit nonce.
pub fn seal_chunk(key: &FileKey, plaintext: &[u8]) -> Result<Vec<u8>, FileError> {
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| FileError::Encrypt)?;
    let mut nonce = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| FileError::Encrypt)?;
    let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Open a chunk produced by [`seal_chunk`].
pub fn open_chunk(key: &FileKey, envelope: &[u8]) -> Result<Vec<u8>, FileError> {
    if envelope.len() < NONCE_SIZE + TAG_SIZE {
        return Err(FileError::Malformed(
            "chunk too short (need nonce + tag)".into(),
        ));
    }
    let (nonce, ciphertext) = envelope.split_at(NONCE_SIZE);
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| FileError::Decrypt)?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| FileError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_round_trips() {
        let key = FileKey::generate();
        let sealed = seal_chunk(&key, b"hello file").unwrap();
        assert_eq!(open_chunk(&key, &sealed).unwrap(), b"hello file");
    }

    #[test]
    fn a_different_key_cannot_open_a_chunk() {
        let key = FileKey::generate();
        let other = FileKey::generate();
        let sealed = seal_chunk(&key, b"secret").unwrap();
        assert!(open_chunk(&other, &sealed).is_err());
    }

    #[test]
    fn a_tampered_chunk_fails() {
        let key = FileKey::generate();
        let mut sealed = seal_chunk(&key, b"secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(open_chunk(&key, &sealed).is_err());
        sealed[last] ^= 0xFF;
        sealed[0] ^= 0xFF; // tamper the nonce too
        assert!(open_chunk(&key, &sealed).is_err());
    }

    #[test]
    fn a_short_envelope_is_malformed() {
        let key = FileKey::generate();
        assert!(matches!(
            open_chunk(&key, &[0u8; 4]),
            Err(FileError::Malformed(_))
        ));
    }

    #[test]
    fn split_chunks_covers_all_bytes_and_handles_empty() {
        assert_eq!(split_chunks(b"").len(), 1);
        assert_eq!(split_chunks(b"").first().unwrap().len(), 0);
        let data = vec![7u8; CHUNK_SIZE * 2 + 5];
        let chunks = split_chunks(&data);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), CHUNK_SIZE);
        assert_eq!(chunks[2].len(), 5);
        let rejoined: Vec<u8> = chunks.concat();
        assert_eq!(rejoined, data);
    }

    #[test]
    fn checksum_is_stable_and_sensitive() {
        assert_eq!(file_checksum(b"abc"), file_checksum(b"abc"));
        assert_ne!(file_checksum(b"abc"), file_checksum(b"abd"));
    }
}
