# File Crypto + Manifest Model Implementation Plan (File Plan 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The pure (no-I/O, no-network) crypto + data model for files-over-event-log: a per-file key, chunk seal/open, a whole-file checksum, and a `FileManifest` with fail-closed encode/decode + reassembly/verify.

**Architecture:** A new `file` module mirroring `channel/{crypto,model}`. `file::crypto` provides `FileKey`, `seal_chunk`/`open_chunk` (raw-key AES-256-GCM, nonce-prepended — same primitive as `channel::crypto::seal_channel_message`), `file_checksum` (SHA-256 of the whole plaintext), and `split_chunks`. `file::manifest` provides `FileManifest` (encode/decode) + `reassemble_and_verify`. The node layer (Plan 2) seals the encoded manifest with the conversation's existing crypto (DM box or channel group key) — this module stays pure.

**Tech Stack:** Rust; `aes-gcm` 0.10, `rand` 0.8 (`OsRng`), `sha2` 0.10, `bincode` (fixint, fail-closed). `ConversationId` from `crate::eventlog::event`. No new dependencies.

---

## Background the implementer needs

This is **File Plan 1** of the file-sharing feature (spec: `docs/superpowers/specs/2026-06-18-file-sharing-design.md`). Files are split into chunks; each chunk is sealed with a per-file key and (Plan 2) carried as an event in a dedicated per-file conversation; a `FileManifest` event announces the file. This plan is the pure foundation.

**Reference — the proven pattern to mirror** (`src-tauri/src/channel/crypto.rs`): `GroupKey([u8;32])` with `generate`/`from_bytes`/`as_bytes`; `seal_channel_message(key, plaintext) -> nonce ‖ AES-256-GCM(...)` with a fresh 96-bit `OsRng` nonce; `open_channel_message` rejecting `< NONCE_SIZE + TAG_SIZE` (28) bytes as `Malformed`. Consts `KEY_SIZE=32`, `NONCE_SIZE=12`, `TAG_SIZE=16`. `aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead}`.

**Fail-closed bincode** (matches the codebase): encode with `bincode::DefaultOptions::new().with_fixint_encoding().serialize(...)`; decode with `...with_fixint_encoding().reject_trailing_bytes().deserialize(...).ok()`.

**`ConversationId`** (`crate::eventlog::event::ConversationId`): a `[u8;32]` newtype, `Serialize`/`Deserialize`/`Copy`/`Eq`, `new([u8;32])`, `as_bytes()`. A random one is minted like `channel::new_channel_id` (fill 32 bytes from `OsRng`).

**Module registration:** `pub mod file;` goes in `src-tauri/src/lib.rs`'s alphabetical `pub mod` list, between `pub mod error;` and `pub mod eventlog;`.

**CPU/test discipline (MANDATORY):** never a bare `cargo test`. Use:
```bash
cd src-tauri && nice -n 10 cargo test --lib file -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
Confirm `git status | head -1` == `On branch feat/redesign-phase0` after committing.

---

### Task 1: `file::crypto` — FileKey, chunk seal/open, checksum, chunking

**Files:** Create `src-tauri/src/file/crypto.rs`, `src-tauri/src/file/mod.rs`; modify `src-tauri/src/lib.rs`.

- [ ] **Step 1: register the module**

In `src-tauri/src/lib.rs`, add `pub mod file;` between `pub mod error;` and `pub mod eventlog;`.

Create `src-tauri/src/file/mod.rs`:
```rust
//! File sharing (files-over-event-log): a per-file key + chunked AES-256-GCM
//! (`crypto`) and the `FileManifest` data model (`manifest`). Pure — the node layer
//! carries chunks as events and seals the manifest with the conversation crypto.

pub mod crypto;
pub mod manifest;

pub use crypto::{file_checksum, open_chunk, seal_chunk, split_chunks, FileError, FileKey, CHUNK_SIZE};
pub use manifest::{reassemble_and_verify, FileManifest};
```

- [ ] **Step 2: write `file/crypto.rs`**
```rust
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

/// The chunk size for splitting a file (256 KiB). The last chunk may be smaller.
pub const CHUNK_SIZE: usize = 256 * 1024;

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
        return Err(FileError::Malformed("chunk too short (need nonce + tag)".into()));
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
        assert!(matches!(open_chunk(&key, &[0u8; 4]), Err(FileError::Malformed(_))));
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
```

- [ ] **Step 3: run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib file::crypto -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/file/crypto.rs src-tauri/src/file/mod.rs src-tauri/src/lib.rs
git commit -m "feat(file): file crypto primitive — FileKey, chunk seal/open, checksum, chunking"
git status | head -1
```
NOTE: `file/mod.rs` re-exports `manifest` items that don't exist until Task 2 — so for THIS commit, temporarily comment out the `pub mod manifest;` + the `pub use manifest::...;` line (or make `mod.rs` only expose `crypto` for now and add `manifest` in Task 2). Cleanest: in Task 1, `mod.rs` contains only the `crypto` mod + its re-export; Task 2 adds the `manifest` lines. Do that — don't reference unwritten code.

---

### Task 2: `file::manifest` — FileManifest model + reassemble/verify

**Files:** Create `src-tauri/src/file/manifest.rs`; modify `src-tauri/src/file/mod.rs`.

- [ ] **Step 1: write `file/manifest.rs`**
```rust
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
        assert_eq!(reassemble_and_verify(&m, &chunks).unwrap(), Vec::<u8>::new());
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
```

- [ ] **Step 2: wire `manifest` into `file/mod.rs`**

Update `src-tauri/src/file/mod.rs` to its final form (add the `manifest` mod + re-exports that Task 1 deferred):
```rust
//! File sharing (files-over-event-log): a per-file key + chunked AES-256-GCM
//! (`crypto`) and the `FileManifest` data model (`manifest`). Pure — the node layer
//! carries chunks as events and seals the manifest with the conversation crypto.

pub mod crypto;
pub mod manifest;

pub use crypto::{file_checksum, open_chunk, seal_chunk, split_chunks, FileError, FileKey, CHUNK_SIZE};
pub use manifest::{reassemble_and_verify, FileManifest};
```

- [ ] **Step 3: run, fmt, clippy, commit**
```bash
cd src-tauri && nice -n 10 cargo test --lib file -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --lib && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/file/manifest.rs src-tauri/src/file/mod.rs
git commit -m "feat(file): FileManifest model + reassemble/verify"
git status | head -1
```
Expected: all `file` tests pass; clippy clean.

---

## Notes for the reviewer / next plan

- **Delivered:** the pure file crypto + data model — per-file key, chunk seal/open (AES-256-GCM, nonce-prepended), whole-file SHA-256, chunking, and `FileManifest` (encode/decode + reassemble/verify). No I/O, no networking — fully unit-tested.
- **Deferred to File Plan 2 (node):** `send_file`/`save_file`; carrying chunks as events in a per-file conversation; processing the `FileManifest` event (sealing it with the DM box or channel group key, surfacing `ReceivedFile`); distributing both conversations; the loopback rig. **File Plan 3:** IPC commands + the Vue file card.
- **Design note for Plan 2:** the manifest's `file_key` is protected by sealing the *encoded manifest* (DM box / group key) into the FileManifest event — the node layer does that sealing, keeping this module pure. Chunks are content-addressed by their event id automatically (sender is the sole author; reassembly is seq-ordered).
- **Reviewer checks:** chunk crypto mirrors `channel::crypto` exactly (nonce-prepend, `< NONCE+TAG` rejected); `split_chunks`/reassembly are inverse and cover the empty-file edge; checksum verification is end-to-end (post-decrypt); fail-closed manifest decode; no key material in any `Debug`/log.
