//! LAN peer discovery: nodes broadcast Ed25519-signed [`announce::Announce`]
//! datagrams carrying their identity keys + TCP port; receivers verify them and
//! build a roster mapping `user_id` → keys + address. Replaces the legacy
//! plaintext UDP discovery (which carried no keys).

pub mod announce;

pub use announce::Announce;
