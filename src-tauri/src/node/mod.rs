//! The redesign node: the explicit App context that wires identity, discovery,
//! transport, the event log, and DM crypto into a runnable peer. This plan adds
//! only [`transport`] (authenticated channel dial/accept); the conversation
//! layer, sync driver, and node API arrive in the next plan.

pub mod channel;
pub mod channel_senders;
pub mod conversation;
pub mod dm_envelope;
pub mod dm_ratchet;
pub mod filebook;
pub mod message;
pub mod net;
pub mod node;
pub mod pairing;
pub mod postbox;
pub mod ratchet_sessions;
pub mod reaction;
pub mod received_log;
pub mod runtime;
pub mod sentlog;
pub mod session;
pub mod transport;

pub use dm_envelope::{DmEnvelope, DmRoute, ReactionEnvelope};
pub use dm_ratchet::DmRatchet;
pub use filebook::{FileBook, ReceivedFile};
pub use message::MessageBody;
pub use node::{ChannelSummary, HistoryEntry, Node, NodeError, ReceivedDm, SearchHit};
pub use pairing::{BackfillRecord, PairingCode, PairingRequest, PairingResponse};
pub use postbox::elected_post_office;
pub use ratchet_sessions::RatchetSessions;
pub use reaction::{aggregate, ReactionPayload, ReactionView};
pub use received_log::{ReceivedEntry, ReceivedLog};
pub use runtime::{RedesignRuntime, RuntimeError};
