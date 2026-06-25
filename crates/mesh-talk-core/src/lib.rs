//! Mesh-Talk core — the serverless, end-to-end-encrypted LAN messaging protocol.
//!
//! This crate is UI-free and embeddable: it is the foundation an SDK builds on. It
//! provides the whole protocol stack and nothing about any particular front end:
//!
//! - [`identity`] — Ed25519/X25519 device identities, accounts, and device certificates.
//! - [`eventlog`] — a content-addressed, hash-linked, encrypted event log + peer sync.
//! - [`ratchet`] / [`channel`] — Double Ratchet (DM) and sender-key (group) forward secrecy.
//! - [`dm`] — sealed direct-message envelopes.
//! - [`transport`] — a Noise (`snow`) secure channel with identity binding.
//! - [`discovery`] — LAN peer discovery.
//! - [`postoffice`] — a store-and-forward relay for offline delivery.
//! - [`file`] — chunked, encrypted file transfer.
//! - [`node`] — the runtime that wires the above into a working peer.
//!
//! The desktop app and the `mesh-talk-node` CLI are thin layers over this crate.

// Pragmatic, intentional lint allowances (the substantive clippy lints stay on):
//  - module_inception: test modules are `mod tests` inside `*/tests.rs`.
//  - too_many_arguments: a few constructors; refactor tracked separately.
//  - assertions_on_constants: placeholder "can construct" smoke tests.
//  - single_match: a couple of intentional single-arm matches.
//  - manual_flatten: a nested `if let Ok` loop kept for readability.
#![allow(
    clippy::module_inception,
    clippy::too_many_arguments,
    clippy::assertions_on_constants,
    clippy::single_match,
    clippy::manual_flatten
)]

// The pure protocol stack — compiles for both native and `wasm32` (browser PWA): identity &
// account crypto, the event-log data model, ratchets, sealed DMs, channel/file crypto, and the
// record-log encryption. The filesystem- and socket-backed parts inside these modules are
// themselves `#[cfg(feature = "native")]`-gated.
pub mod channel;
pub mod dm;
pub mod eventlog;
pub mod file;
pub mod identity;
pub mod limits;
pub mod message;
pub mod ratchet;
// The event-log sync protocol (requester `request_round` + responder `serve_one`) over a
// `SecureChannel`. Pure (no sockets) → available to wasm too, so the PWA can sync over a data
// channel; the native node + the browser gateway both drive it.
pub mod session;
pub mod storage;
// The Noise `SecureChannel` is byte-stream-generic + uses only the pure-Rust snow resolver, so
// it compiles for wasm (the browser gateway runs it over a data channel); the socket-backed
// `transport::net` inside it stays native-gated.
pub mod transport;
pub mod util;

// Native-only: OS sockets, threads, and the runtime. Gated out of the `wasm32` build, which
// substitutes a browser transport (WebRTC/WebSocket) + IndexedDB storage at the app layer.
#[cfg(feature = "native")]
pub mod discovery;
// The browser WebRTC gateway (opt-in `gateway` feature; pulls the heavy `webrtc` stack).
#[cfg(feature = "gateway")]
pub mod gateway;
#[cfg(feature = "native")]
pub mod node;
#[cfg(feature = "native")]
pub mod postoffice;
