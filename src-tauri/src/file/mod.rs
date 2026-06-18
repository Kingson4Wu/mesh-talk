//! File sharing (files-over-event-log): a per-file key + chunked AES-256-GCM
//! (`crypto`) and the `FileManifest` data model (`manifest`). Pure — the node layer
//! carries chunks as events and seals the manifest with the conversation crypto.

pub mod crypto;

pub use crypto::{
    file_checksum, open_chunk, seal_chunk, split_chunks, FileError, FileKey, CHUNK_SIZE,
};
