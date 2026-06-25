pub mod encryption;
pub mod errors;
pub mod serialization;

// Filesystem-backed persistence (append-only record log + received-file store) — native only.
// The wasm/PWA build reuses `encryption` + `serialization` over an IndexedDB-backed store.
#[cfg(feature = "native")]
pub mod file_manager;
#[cfg(feature = "native")]
pub mod record_log;

#[cfg(test)]
mod tests;
