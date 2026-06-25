//! Protocol-wide wire limits.
//!
//! These live at the crate root (rather than inside `transport`) so the pure protocol stack
//! can reference them in the `wasm32` build, where the socket `transport` module is gated out.
//! The event-log sync uses `MAX_PLAINTEXT` to bound a reconciliation message to one frame.
//! `transport` re-exports both, so `transport::MAX_FRAME` / `transport::MAX_PLAINTEXT` keep
//! working unchanged for native code.

/// Maximum size of a single Noise message on the wire (Noise spec limit).
pub const MAX_FRAME: usize = 65535;

/// Maximum plaintext per frame = `MAX_FRAME` minus the 16-byte AEAD tag.
pub const MAX_PLAINTEXT: usize = MAX_FRAME - 16;
