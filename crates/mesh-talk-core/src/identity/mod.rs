pub mod account;
pub mod auth;
pub mod device;
pub mod errors;
pub mod keys;
pub mod user;

// Filesystem-backed key persistence — native only (the wasm/PWA build persists keys in
// IndexedDB at the app layer).
#[cfg(feature = "native")]
pub mod account_keystore;
#[cfg(feature = "native")]
pub mod keystore;
#[cfg(feature = "native")]
pub mod manager;

#[cfg(test)]
mod tests;
