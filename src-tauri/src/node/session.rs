//! Driving event-log reconciliation over a [`SecureChannel`] — the networked
//! counterpart of Plan-4's in-process `reconcile`. The same handlers
//! (`build_request`/`handle_request`/`handle_response`/`handle_followup`) run on
//! each side; the three messages are framed (bincode) and sent over the channel.
//! The requester runs one round with [`request_round`]; the responder serves
//! rounds one message at a time with [`serve_one`].
//!
//! The store is shared as a `&Mutex<S>` and locked only for the synchronous
//! handler calls — never across an `.await` — so a `std::sync::Mutex` is correct.

use crate::eventlog::event::ConversationId;
use crate::eventlog::sync::{
    build_request, handle_followup, handle_request, handle_response, ApplyReport, SyncFollowup,
    SyncRequest, SyncResponse, SyncStore,
};
use crate::transport::{SecureChannel, TransportError};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

/// One framed sync message on the wire.
#[derive(Debug, Serialize, Deserialize)]
enum SyncWire {
    Request(SyncRequest),
    Response(SyncResponse),
    Followup(SyncFollowup),
}

/// Errors from a sync session.
#[derive(Debug)]
pub enum SessionError {
    /// Channel send/recv failure.
    Transport(TransportError),
    /// (De)serialization of a wire message failed.
    Serialization(String),
    /// Received a wire message that doesn't fit the protocol state.
    UnexpectedMessage,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Transport(e) => write!(f, "sync transport error: {e}"),
            SessionError::Serialization(m) => write!(f, "sync serialization error: {m}"),
            SessionError::UnexpectedMessage => write!(f, "unexpected sync message"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<TransportError> for SessionError {
    fn from(e: TransportError) -> Self {
        SessionError::Transport(e)
    }
}

fn encode(wire: &SyncWire) -> Result<Vec<u8>, SessionError> {
    bincode::serialize(wire).map_err(|e| SessionError::Serialization(e.to_string()))
}

fn decode(bytes: &[u8]) -> Result<SyncWire, SessionError> {
    bincode::deserialize(bytes).map_err(|e| SessionError::Serialization(e.to_string()))
}

/// Run one reconciliation round as the REQUESTER over `channel`, for
/// `conversation`: send a Request built from the local store, receive the
/// Response (ingesting its events), then send the Followup. Converges the
/// conversation in both directions. Returns what this side applied.
pub async fn request_round<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
) -> Result<ApplyReport, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let request = {
        let store = store.lock().expect("store mutex not poisoned");
        build_request(&*store, conversation)
    };
    channel.send(&encode(&SyncWire::Request(request))?).await?;

    let response = match decode(&channel.recv().await?)? {
        SyncWire::Response(r) => r,
        _ => return Err(SessionError::UnexpectedMessage),
    };

    let (report, followup) = {
        let mut store = store.lock().expect("store mutex not poisoned");
        handle_response(&mut *store, &response)
    };
    channel
        .send(&encode(&SyncWire::Followup(followup))?)
        .await?;
    Ok(report)
}

/// The outcome of serving one inbound wire message.
pub enum Served {
    /// Handled a message for this conversation (the caller may surface new events).
    Handled(ConversationId),
    /// The channel closed / errored — stop serving it.
    Closed,
}

/// Serve ONE inbound wire message as the RESPONDER over `channel`: a Request is
/// answered with a Response; a Followup is ingested. Returns the conversation
/// handled, or `Closed` when the peer hangs up (a recv error is treated as a
/// clean close so the serve loop can stop).
pub async fn serve_one<S, IO>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
) -> Result<Served, SessionError>
where
    S: SyncStore,
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let bytes = match channel.recv().await {
        Ok(b) => b,
        Err(_) => return Ok(Served::Closed),
    };
    match decode(&bytes)? {
        SyncWire::Request(request) => {
            let conversation = request.conversation;
            let response = {
                let store = store.lock().expect("store mutex not poisoned");
                handle_request(&*store, &request)
            };
            channel
                .send(&encode(&SyncWire::Response(response))?)
                .await?;
            Ok(Served::Handled(conversation))
        }
        SyncWire::Followup(followup) => {
            let conversation = followup.conversation;
            {
                let mut store = store.lock().expect("store mutex not poisoned");
                handle_followup(&mut *store, &followup);
            }
            Ok(Served::Handled(conversation))
        }
        SyncWire::Response(_) => Err(SessionError::UnexpectedMessage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::identity::device::DeviceIdentity;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    #[tokio::test]
    async fn one_round_converges_two_stores_over_a_channel() {
        // A SecureChannel pair over an in-memory duplex (no real sockets).
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        // A has one event; B is empty.
        let event = Event::new(
            &a_id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        let a_store = Mutex::new({
            let mut log = EventLog::default();
            log.append(event.clone()).unwrap();
            log
        });
        let event_id = event.id;

        // Responder: accept the channel and serve two messages (Request, then Followup).
        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(EventLog::default());
            // Request → Response
            assert!(matches!(
                serve_one(&mut b_ch, &b_store).await.unwrap(),
                Served::Handled(_)
            ));
            // Followup (carries A's event)
            assert!(matches!(
                serve_one(&mut b_ch, &b_store).await.unwrap(),
                Served::Handled(_)
            ));
            b_store.into_inner().unwrap()
        });

        // Requester: connect and run one round.
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv()).await.unwrap();

        let b_log = server.await.unwrap();
        assert!(
            b_log.has(&event_id),
            "B received A's event via the sync round"
        );
        assert!(a_store.lock().unwrap().has(&event_id));
    }
}
