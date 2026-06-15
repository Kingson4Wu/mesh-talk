//! The post office: an elected always-on peer that stores-and-forwards encrypted
//! events so a message reaches an offline recipient. It is a peer with two extra
//! hats — a full replica (a [`crate::eventlog::PersistentEventLog`]) and a
//! store-and-forward relay (it reconciles as a [`crate::eventlog::sync::SyncStore`]).
//! For DMs it is a *dumb relay*: it holds recipient-sealed envelopes it can never
//! decrypt, and never holds any conversation key.
//!
//! [`election`] decides — deterministically, so every peer agrees — which eligible
//! (always-on) peer is the post office: the one with the lowest identity
//! fingerprint. Full election with standbys and uptime detection is Phase 1.

pub mod election;

pub use election::{elect, is_post_office};
