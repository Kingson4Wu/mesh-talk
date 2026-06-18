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
    build_request, handle_followup, handle_request_bounded, handle_response_bounded, ApplyReport,
    SyncFollowup, SyncRequest, SyncResponse, SyncStore,
};
use crate::transport::{SecureChannel, TransportError, MAX_PLAINTEXT};
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};

/// Backstop on sync rounds for one conversation (a peer that resends
/// non-applying events forever can't spin us indefinitely). Generous: a round
/// transfers up to ~one frame, so this bounds a single conversation's transfer.
const MAX_SYNC_ROUNDS: usize = 10_000;

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
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .serialize(wire)
        .map_err(|e| SessionError::Serialization(e.to_string()))
}

fn decode(bytes: &[u8]) -> Result<SyncWire, SessionError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| SessionError::Serialization(e.to_string()))
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
    let mut total = ApplyReport::default();
    for _ in 0..MAX_SYNC_ROUNDS {
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
            handle_response_bounded(&mut *store, &response, MAX_PLAINTEXT)
        };
        let made_progress = report.applied > 0;
        let more_to_push = !followup.events.is_empty();

        channel
            .send(&encode(&SyncWire::Followup(followup))?)
            .await?;

        total.applied += report.applied;
        total.duplicates += report.duplicates;
        total.rejected.extend(report.rejected);

        if !made_progress && !more_to_push {
            break;
        }
    }
    Ok(total)
}

/// The outcome of serving one inbound wire message.
#[derive(Debug)]
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
                handle_request_bounded(&*store, &request, MAX_PLAINTEXT)
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

        // Responder: accept the channel and serve until the requester closes.
        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(EventLog::default());
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => {}
                }
            }
            b_store.into_inner().unwrap()
        });

        // Requester: connect and run request_round (loops until converged), then drop.
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv()).await.unwrap();
        drop(a_ch);

        let b_log = server.await.unwrap();
        assert!(
            b_log.has(&event_id),
            "B received A's event via the sync round"
        );
        assert!(a_store.lock().unwrap().has(&event_id));
    }

    #[tokio::test]
    async fn round_pulls_events_from_the_responder_too() {
        // B (responder) has an event A lacks; one requester round must pull it to A.
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        let event = Event::new(
            &b_id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"y".to_vec(),
        );
        let event_id = event.id;

        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new({
                let mut log = EventLog::default();
                log.append(event.clone()).unwrap();
                log
            });
            // Serve until the requester closes the channel.
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => {}
                }
            }
        });

        let a_store = Mutex::new(EventLog::default());
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv()).await.unwrap();
        drop(a_ch);
        server.await.unwrap();

        assert!(
            a_store.lock().unwrap().has(&event_id),
            "A pulled B's event in one round"
        );
    }

    #[tokio::test]
    async fn serve_one_reports_closed_when_peer_hangs_up() {
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let (a_io, b_io) = tokio::io::duplex(64 * 1024);

        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(EventLog::default());
            // The requester connects then drops without sending — recv sees EOF.
            serve_one(&mut b_ch, &b_store).await.unwrap()
        });

        let a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        drop(a_ch); // hang up immediately
        assert!(matches!(server.await.unwrap(), Served::Closed));
    }

    /// Build a responder store with `n` events in `conv`, each carrying a `payload_size`-byte
    /// ciphertext, so the combined encoded size can exceed MAX_PLAINTEXT.
    fn build_large_responder_store(
        id: &DeviceIdentity,
        conv: ConversationId,
        n: usize,
        payload_size: usize,
    ) -> EventLog {
        let mut log = EventLog::default();
        for i in 0..n {
            let ciphertext = vec![0xABu8; payload_size];
            let event = Event::new(
                id,
                conv,
                (i + 1) as u64,
                vec![],
                1,
                0,
                EventKind::Message,
                ciphertext,
            );
            log.append(event).unwrap();
        }
        log
    }

    #[tokio::test]
    async fn request_round_syncs_a_conversation_larger_than_one_frame() {
        // Build a responder store with 4 events of ~20 KB each (~80 KB total > MAX_PLAINTEXT=65519).
        // request_round loops internally until all events are transferred; assert full sync.
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        // Large duplex buffer so multiple frames can be in-flight.
        let (a_io, b_io) = tokio::io::duplex(512 * 1024);

        let conv_id = ConversationId::new([2u8; 32]);
        const EVENT_PAYLOAD: usize = 20 * 1024; // ~20 KB each → 4 events ≈ 80 KB > one frame
        const NUM_EVENTS: usize = 4;

        let b_log = build_large_responder_store(&b_id, conv_id, NUM_EVENTS, EVENT_PAYLOAD);
        let responder_event_count = b_log.event_ids(&conv_id).len();
        assert_eq!(responder_event_count, NUM_EVENTS);

        let server = tokio::spawn(async move {
            let mut b_ch = SecureChannel::accept(b_io, &b_id).await.unwrap();
            let b_store = Mutex::new(b_log);
            // Serve messages until the requester closes the channel (multi-round loop).
            loop {
                match serve_one(&mut b_ch, &b_store).await.unwrap() {
                    Served::Closed => break,
                    Served::Handled(_) => {}
                }
            }
            b_store.into_inner().unwrap()
        });

        // Requester starts empty; request_round loops until fully synced.
        let a_store = Mutex::new(EventLog::default());
        let mut a_ch = SecureChannel::connect(a_io, &a_id, None).await.unwrap();
        request_round(&mut a_ch, &a_store, conv_id).await.unwrap();
        // Drop the channel so the server's serve loop sees Closed and exits.
        drop(a_ch);

        let b_final = server.await.unwrap();
        let requester_count = a_store.lock().unwrap().event_ids(&conv_id).len();
        assert_eq!(
            requester_count, responder_event_count,
            "requester should have all {responder_event_count} events after multi-round sync"
        );
        // Responder also still has all events.
        assert_eq!(b_final.event_ids(&conv_id).len(), responder_event_count);
    }
}
