//! The redesign node: the explicit App context that wires identity, discovery,
//! transport, the event log, and DM crypto into a runnable peer. This plan adds
//! only [`transport`] (authenticated channel dial/accept); the conversation
//! layer, sync driver, and node API arrive in the next plan.

pub mod channel;
pub mod conversation;
pub mod filebook;
pub mod net;
pub mod node;
pub mod postbox;
pub mod reaction;
pub mod runtime;
pub mod sentlog;
pub mod session;
pub mod transport;

pub use filebook::{FileBook, ReceivedFile};
pub use node::{ChannelSummary, HistoryEntry, Node, NodeError, ReceivedDm, SearchHit};
pub use postbox::elected_post_office;
pub use reaction::{aggregate, ReactionPayload, ReactionView};
pub use runtime::{RedesignRuntime, RuntimeError};
