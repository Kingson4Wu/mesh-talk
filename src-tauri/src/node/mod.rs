//! The redesign node: the explicit App context that wires identity, discovery,
//! transport, the event log, and DM crypto into a runnable peer. This plan adds
//! only [`transport`] (authenticated channel dial/accept); the conversation
//! layer, sync driver, and node API arrive in the next plan.

pub mod conversation;
pub mod transport;
