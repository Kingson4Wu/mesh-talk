//! Browser gateway (mobile-PWA plan, Phase 2): a WebRTC data-channel endpoint a PWA connects to
//! — via the tiny signaling server (crates/mesh-talk-signal) — whose Noise frames the node
//! relays into the mesh. See `docs/superpowers/specs/2026-06-23-mobile-pwa-design.md`.
//!
//! [`connect_secure`] establishes a data channel to the other party in a signaling *room*, then
//! runs the mesh's authenticated [`SecureChannel`] (Noise XX) straight over it — so the
//! phone↔node hop is mutually authenticated + encrypted exactly like a mesh peer link, on top of
//! WebRTC's own DTLS. Signaling is **non-trickle** (ICE candidates ride in the SDP → one offer +
//! one answer, no candidate races), ideal on a LAN. The data channel is *detached* into a byte
//! stream ([`PollDataChannel`]) so `SecureChannel<S>` can frame over it directly.
//!
//! Behind the opt-in `gateway` feature so the heavy `webrtc` stack stays out of normal builds.

use std::sync::{Arc, Mutex as StdMutex};

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex as TokioMutex};
use tokio_tungstenite::tungstenite::Message;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::APIBuilder;
use webrtc::data::data_channel::PollDataChannel;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidate;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

use crate::eventlog::SyncStore;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::session::{serve_one, Served};
use crate::transport::channel::SecureChannel;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Which end of the handshake this peer drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Creates the data channel + offer, and runs the Noise initiator (the browser/PWA).
    Offerer,
    /// Waits for the offer + answers, and runs the Noise responder (the desktop node).
    Answerer,
}

#[derive(Serialize)]
struct Join<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    room: &'a str,
}

/// Signaling payloads exchanged through the relay; field names match the browser client.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Signal {
    Offer { sdp: String },
    Answer { sdp: String },
}

/// Build a data-channel-capable peer connection with channel *detaching* enabled (so an open
/// channel can become a byte stream). No ICE servers — host candidates only, all a LAN needs.
async fn new_peer_connection() -> Result<Arc<RTCPeerConnection>, BoxError> {
    let mut media = MediaEngine::default();
    let registry = register_default_interceptors(Registry::new(), &mut media)?;
    let mut setting = SettingEngine::default();
    setting.detach_data_channels();
    let api = APIBuilder::new()
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_setting_engine(setting)
        .build();
    Ok(Arc::new(
        api.new_peer_connection(RTCConfiguration::default()).await?,
    ))
}

/// Gather ICE fully, then return our local description's SDP (candidates included).
async fn local_sdp_after_gather(
    pc: &Arc<RTCPeerConnection>,
    desc: RTCSessionDescription,
) -> Result<String, BoxError> {
    let mut gather_done = pc.gathering_complete_promise().await;
    pc.set_local_description(desc).await?;
    let _ = gather_done.recv().await;
    Ok(pc
        .local_description()
        .await
        .ok_or("no local description after gathering")?
        .sdp)
}

/// Read the next signaling payload from the websocket.
async fn next_signal<S>(source: &mut S) -> Result<Signal, BoxError>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(msg) = source.next().await {
        if let Message::Text(text) = msg? {
            return Ok(serde_json::from_str::<Signal>(&text)?);
        }
    }
    Err("signaling stream closed before a payload arrived".into())
}

type OpenSignal = Arc<StdMutex<Option<oneshot::Sender<()>>>>;

/// Fire `open_tx` when the data channel opens.
fn wire_open(dc: &Arc<RTCDataChannel>, open_tx: OpenSignal) {
    dc.on_open(Box::new(move || {
        let open_tx = Arc::clone(&open_tx);
        Box::pin(async move {
            if let Some(tx) = open_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
        })
    }));
}

/// Run the signaling handshake for `role` and return the data channel once it is open.
async fn establish(
    signal_url: &str,
    room: &str,
    role: Role,
) -> Result<(Arc<RTCPeerConnection>, Arc<RTCDataChannel>), BoxError> {
    let pc = new_peer_connection().await?;
    let (ws, _) = tokio_tungstenite::connect_async(signal_url).await?;
    let (mut sink, mut source) = ws.split();

    // Non-trickle: candidates ride in the SDP, so drop per-candidate callbacks.
    pc.on_ice_candidate(Box::new(|_c: Option<RTCIceCandidate>| Box::pin(async {})));

    sink.send(Message::Text(serde_json::to_string(&Join {
        kind: "join",
        room,
    })?))
    .await?;

    // Capture the open channel + signal readiness.
    let (open_tx, open_rx) = oneshot::channel::<()>();
    let open_tx: OpenSignal = Arc::new(StdMutex::new(Some(open_tx)));
    let slot: Arc<TokioMutex<Option<Arc<RTCDataChannel>>>> = Arc::new(TokioMutex::new(None));

    match role {
        Role::Offerer => {
            let dc = pc.create_data_channel("mesh", None).await?;
            wire_open(&dc, Arc::clone(&open_tx));
            *slot.lock().await = Some(dc);
            let offer = pc.create_offer(None).await?;
            let sdp = local_sdp_after_gather(&pc, offer).await?;
            sink.send(Message::Text(serde_json::to_string(&Signal::Offer {
                sdp,
            })?))
            .await?;
            match next_signal(&mut source).await? {
                Signal::Answer { sdp } => {
                    pc.set_remote_description(RTCSessionDescription::answer(sdp)?)
                        .await?
                }
                _ => return Err("expected an answer".into()),
            }
        }
        Role::Answerer => {
            let slot2 = Arc::clone(&slot);
            let open_tx2 = Arc::clone(&open_tx);
            pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
                let slot2 = Arc::clone(&slot2);
                let open_tx2 = Arc::clone(&open_tx2);
                Box::pin(async move {
                    wire_open(&dc, open_tx2);
                    *slot2.lock().await = Some(dc);
                })
            }));
            match next_signal(&mut source).await? {
                Signal::Offer { sdp } => {
                    pc.set_remote_description(RTCSessionDescription::offer(sdp)?)
                        .await?;
                    let answer = pc.create_answer(None).await?;
                    let sdp = local_sdp_after_gather(&pc, answer).await?;
                    sink.send(Message::Text(serde_json::to_string(&Signal::Answer {
                        sdp,
                    })?))
                    .await?;
                }
                _ => return Err("expected an offer".into()),
            }
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), open_rx)
        .await
        .map_err(|_| "timed out establishing the data channel")?
        .map_err(|_| "data channel never opened")?;
    let dc = slot
        .lock()
        .await
        .take()
        .ok_or("data channel missing after open")?;
    Ok((pc, dc))
}

/// An authenticated, encrypted channel to the peer over WebRTC, plus the peer connection it rides
/// on (kept alive for the lifetime of the channel).
pub struct SecureGateway {
    pub channel: SecureChannel<PollDataChannel>,
    _pc: Arc<RTCPeerConnection>,
}

/// Connect to the other party in `room` via the signaling relay, then run the mesh's `SecureChannel`
/// (Noise XX) over the data channel. The offerer is the Noise initiator; the answerer is the
/// responder. `expected_peer`, if set, must match the authenticated remote identity.
pub async fn connect_secure(
    signal_url: &str,
    room: &str,
    role: Role,
    identity: &DeviceIdentity,
    expected_peer: Option<&PublicIdentity>,
) -> Result<SecureGateway, BoxError> {
    let (pc, dc) = establish(signal_url, room, role).await?;
    let detached = dc.detach().await?;
    let stream = PollDataChannel::new(detached);
    let channel = match role {
        Role::Offerer => SecureChannel::connect(stream, identity, expected_peer).await?,
        Role::Answerer => SecureChannel::accept(stream, identity).await?,
    };
    Ok(SecureGateway { channel, _pc: pc })
}

/// Relay a gateway-connected PWA into the mesh: drive the node's responder sync loop over the
/// gateway channel — treating the PWA as a mesh peer — against `store`, until it disconnects. The
/// node's normal peer sync then propagates the PWA's events to other peers and vice versa, so the
/// PWA participates in the mesh through the node.
pub async fn serve_connection<S: SyncStore>(
    channel: &mut SecureChannel<PollDataChannel>,
    store: &std::sync::Mutex<S>,
) -> Result<(), BoxError> {
    loop {
        match serve_one(channel, store).await? {
            Served::Closed => return Ok(()),
            Served::Handled(_) => {}
        }
    }
}

// ---- Multi-peer (mesh) hub: one desktop node serving MANY spokes over a single relay socket ----
//
// The relay's mesh mode (crates/mesh-talk-signal) assigns each client a numeric id, announces
// peers joining/leaving, and routes frames addressed by `to` (tagging them with `from`). That lets
// the desktop node hold one WebRTC connection per spoke (phone) in a single room — no 2-peer
// limit — and serve the mesh sync over each, so every spoke reaches every other through the node.

use std::collections::HashMap;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::MaybeTlsStream;

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>,
    Message,
>;

/// Mesh-mode join: like `Join` but asks the relay for addressed multi-peer routing.
#[derive(Serialize)]
struct MeshJoin<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    room: &'a str,
    mesh: bool,
}

/// Frames the relay delivers in mesh mode (field names match the JS client + the relay).
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum MeshFrame {
    // `you` (our own assigned id) is also sent but unused on the native side — we only address peers.
    Welcome { peers: Vec<usize> },
    PeerJoined { peer: usize },
    PeerLeft { peer: usize },
    Offer { sdp: String, from: usize },
    Answer { sdp: String, from: usize },
}

/// An addressed signal we send out (the relay routes by `to`, then injects `from`).
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum OutSignal {
    Offer { sdp: String, to: usize },
    Answer { sdp: String, to: usize },
}

async fn next_mesh_frame<S>(source: &mut S) -> Result<MeshFrame, BoxError>
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(msg) = source.next().await {
        if let Message::Text(text) = msg? {
            if let Ok(f) = serde_json::from_str::<MeshFrame>(&text) {
                return Ok(f);
            }
        }
    }
    Err("mesh signaling stream closed".into())
}

/// Hub side of one spoke: answer the spoke's offer (signals arrive on `rx`, replies go out the
/// shared `sink` addressed to `peer`), open the data channel, run the Noise responder, then hand
/// the established secure channel to `on_ready` (which serves the mesh sync over it).
async fn run_one_spoke<F, Fut>(
    peer: usize,
    mut rx: mpsc::UnboundedReceiver<MeshFrame>,
    sink: Arc<TokioMutex<WsSink>>,
    identity: Arc<DeviceIdentity>,
    on_ready: F,
) -> Result<(), BoxError>
where
    F: Fn(SecureChannel<PollDataChannel>) -> Fut + Clone,
    Fut: std::future::Future<Output = ()>,
{
    // Serve the spoke across MANY rounds. A wasm spoke re-offers periodically (a fresh WebRTC
    // connection each round) so it can pull messages that arrived since its last sync — if the hub
    // served only ONE round per spoke, anything sent after the spoke's first sync would never reach
    // it. So loop: each offer is a new round; a failed round is non-fatal (wait for the next
    // offer); only the spoke leaving (the frame stream ending) stops this.
    loop {
        let offer_sdp = loop {
            match rx.recv().await {
                Some(MeshFrame::Offer { sdp, .. }) => break sdp,
                Some(_) => continue,   // ignore answers / stray frames
                None => return Ok(()), // spoke left
            }
        };
        if let Err(e) = serve_spoke_round(peer, offer_sdp, &sink, &identity, on_ready.clone()).await
        {
            log::warn!("[mesh] hub: spoke {peer} round failed: {e} (awaiting next offer)");
        }
    }
}

/// One round with a spoke: a fresh peer connection answering `offer_sdp`, then serve the mesh sync
/// over its data channel until it closes. Errors end only THIS round (the caller waits for the
/// spoke's next offer).
async fn serve_spoke_round<F, Fut>(
    peer: usize,
    offer_sdp: String,
    sink: &Arc<TokioMutex<WsSink>>,
    identity: &Arc<DeviceIdentity>,
    on_ready: F,
) -> Result<(), BoxError>
where
    F: Fn(SecureChannel<PollDataChannel>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let pc = new_peer_connection().await?;
    pc.on_ice_candidate(Box::new(|_c: Option<RTCIceCandidate>| Box::pin(async {})));
    let (open_tx, open_rx) = oneshot::channel::<()>();
    let open_tx: OpenSignal = Arc::new(StdMutex::new(Some(open_tx)));
    let slot: Arc<TokioMutex<Option<Arc<RTCDataChannel>>>> = Arc::new(TokioMutex::new(None));
    let slot2 = Arc::clone(&slot);
    let open_tx2 = Arc::clone(&open_tx);
    pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
        let slot2 = Arc::clone(&slot2);
        let open_tx2 = Arc::clone(&open_tx2);
        Box::pin(async move {
            wire_open(&dc, open_tx2);
            *slot2.lock().await = Some(dc);
        })
    }));

    pc.set_remote_description(RTCSessionDescription::offer(offer_sdp)?)
        .await?;
    let answer = pc.create_answer(None).await?;
    let sdp = local_sdp_after_gather(&pc, answer).await?;
    let out = serde_json::to_string(&OutSignal::Answer { sdp, to: peer })?;
    sink.lock().await.send(Message::Text(out)).await?;

    tokio::time::timeout(std::time::Duration::from_secs(15), open_rx)
        .await
        .map_err(|_| "timed out establishing the data channel")?
        .map_err(|_| "data channel never opened")?;
    let dc = slot.lock().await.take().ok_or("data channel missing")?;
    let detached = dc.detach().await?;
    let stream = PollDataChannel::new(detached);
    let channel = SecureChannel::accept(stream, identity).await?;
    log::info!(
        "[mesh] hub: spoke {peer} round — connected to {}",
        channel.peer_identity().user_id()
    );
    on_ready(channel).await;
    Ok(())
}

/// Serve this node as a mesh HUB in `room` against `store`: a thin wrapper over [`run_mesh_hub`]
/// that serves the mesh sync (responder) over each spoke's channel.
pub async fn serve_mesh_hub<S: SyncStore + Send + 'static>(
    signal_url: &str,
    room: &str,
    identity: Arc<DeviceIdentity>,
    store: Arc<std::sync::Mutex<S>>,
) -> Result<(), BoxError> {
    run_mesh_hub(signal_url, room, identity, move |mut channel| {
        let store = Arc::clone(&store);
        async move {
            let _ = serve_connection(&mut channel, &store).await;
        }
    })
    .await
}

/// Run this node as a mesh HUB in `room`: join the relay in mesh mode and, for every spoke that
/// connects, hold a WebRTC connection, run the Noise responder, and hand the secure channel to
/// `on_spoke` (which serves the mesh sync over it). Runs until the relay socket closes (so spawn
/// it as a background task). One node, many phones, no 2-peer limit — every spoke reaches every
/// other through this hub.
pub async fn run_mesh_hub<F, Fut>(
    signal_url: &str,
    room: &str,
    identity: Arc<DeviceIdentity>,
    on_spoke: F,
) -> Result<(), BoxError>
where
    F: Fn(SecureChannel<PollDataChannel>) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (ws, _) = tokio_tungstenite::connect_async(signal_url).await?;
    let (mut sink, mut source) = ws.split();
    sink.send(Message::Text(serde_json::to_string(&MeshJoin {
        kind: "join",
        room,
        mesh: true,
    })?))
    .await?;
    let sink = Arc::new(TokioMutex::new(sink));

    // Existing peers (if we weren't first) are also served as spokes.
    let initial = match next_mesh_frame(&mut source).await? {
        MeshFrame::Welcome { peers, .. } => peers,
        _ => return Err("expected welcome".into()),
    };
    let mut spokes: HashMap<usize, mpsc::UnboundedSender<MeshFrame>> = HashMap::new();
    let start_spoke = |peer: usize| {
        let (tx, rx) = mpsc::unbounded_channel();
        let sink = Arc::clone(&sink);
        let identity = Arc::clone(&identity);
        let on_spoke = on_spoke.clone();
        tokio::spawn(async move {
            let _ = run_one_spoke(peer, rx, sink, identity, on_spoke).await;
        });
        tx
    };
    for p in initial {
        spokes.insert(p, start_spoke(p));
    }

    loop {
        match next_mesh_frame(&mut source).await? {
            MeshFrame::PeerJoined { peer } => {
                spokes.insert(peer, start_spoke(peer));
            }
            MeshFrame::PeerLeft { peer } => {
                spokes.remove(&peer);
            }
            frame @ (MeshFrame::Offer { .. } | MeshFrame::Answer { .. }) => {
                let from = match &frame {
                    MeshFrame::Offer { from, .. } | MeshFrame::Answer { from, .. } => *from,
                    _ => unreachable!(),
                };
                if let Some(tx) = spokes.get(&from) {
                    let _ = tx.send(frame);
                }
            }
            MeshFrame::Welcome { .. } => {}
        }
    }
}

/// Spoke side: join `room` in mesh mode, connect to the hub (the oldest peer) as the Noise
/// initiator, and return the secure channel. Mirrors `connect_secure` but over mesh signaling.
pub async fn connect_mesh_spoke(
    signal_url: &str,
    room: &str,
    identity: &DeviceIdentity,
    expected_peer: Option<&PublicIdentity>,
) -> Result<SecureGateway, BoxError> {
    let (ws, _) = tokio_tungstenite::connect_async(signal_url).await?;
    let (mut sink, mut source) = ws.split();
    sink.send(Message::Text(serde_json::to_string(&MeshJoin {
        kind: "join",
        room,
        mesh: true,
    })?))
    .await?;
    let hub = match next_mesh_frame(&mut source).await? {
        MeshFrame::Welcome { peers, .. } => *peers.first().ok_or("no hub in the room yet")?,
        _ => return Err("expected welcome".into()),
    };

    let pc = new_peer_connection().await?;
    pc.on_ice_candidate(Box::new(|_c: Option<RTCIceCandidate>| Box::pin(async {})));
    let (open_tx, open_rx) = oneshot::channel::<()>();
    let open_tx: OpenSignal = Arc::new(StdMutex::new(Some(open_tx)));
    let dc = pc.create_data_channel("mesh", None).await?;
    wire_open(&dc, open_tx);

    let offer = pc.create_offer(None).await?;
    let sdp = local_sdp_after_gather(&pc, offer).await?;
    sink.send(Message::Text(serde_json::to_string(&OutSignal::Offer {
        sdp,
        to: hub,
    })?))
    .await?;
    loop {
        match next_mesh_frame(&mut source).await? {
            MeshFrame::Answer { sdp, .. } => {
                pc.set_remote_description(RTCSessionDescription::answer(sdp)?)
                    .await?;
                break;
            }
            _ => continue,
        }
    }

    tokio::time::timeout(std::time::Duration::from_secs(15), open_rx)
        .await
        .map_err(|_| "timed out establishing the data channel")?
        .map_err(|_| "data channel never opened")?;
    let detached = dc.detach().await?;
    let stream = PollDataChannel::new(detached);
    let channel = SecureChannel::connect(stream, identity, expected_peer).await?;
    Ok(SecureGateway { channel, _pc: pc })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn noise_channel_runs_over_webrtc_via_signaling() {
        // Real signaling relay on an ephemeral port.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(mesh_talk_signal::serve(listener));
        let url = format!("ws://{addr}");

        let node = DeviceIdentity::generate();
        let phone = DeviceIdentity::generate();
        let node_pub = node.public();
        let phone_pub = phone.public();

        // Answerer (node) waits first; offerer (phone) then connects.
        let url_a = url.clone();
        let answerer = tokio::spawn(async move {
            connect_secure(&url_a, "room-1", Role::Answerer, &node, None).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let mut phone_gw = connect_secure(&url, "room-1", Role::Offerer, &phone, Some(&node_pub))
            .await
            .expect("phone connects");
        let mut node_gw = answerer.await.unwrap().expect("node connects");

        // Each end authenticated the other.
        assert_eq!(
            phone_gw.channel.peer_identity().user_id(),
            node_pub.user_id()
        );
        assert_eq!(
            node_gw.channel.peer_identity().user_id(),
            phone_pub.user_id()
        );

        // An encrypted message round-trips over the Noise channel.
        phone_gw.channel.send(b"hello mesh").await.unwrap();
        let got = node_gw.channel.recv().await.unwrap();
        assert_eq!(got, b"hello mesh");

        node_gw.channel.send(b"ack").await.unwrap();
        let back = phone_gw.channel.recv().await.unwrap();
        assert_eq!(back, b"ack");
    }

    #[tokio::test]
    async fn pwa_pulls_a_mesh_event_through_the_gateway() {
        use crate::eventlog::event::{Event, EventKind};
        use crate::eventlog::store::EventLog;
        use crate::eventlog::ConversationId;
        use crate::session::request_round;
        use std::sync::Mutex;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(mesh_talk_signal::serve(listener));
        let url = format!("ws://{addr}");

        let node_id = DeviceIdentity::generate();
        let phone_id = DeviceIdentity::generate();
        let conv = ConversationId::new([7u8; 32]);

        // The node already holds an event (as if synced from the mesh); the phone starts empty.
        let event = Event::new(
            &node_id,
            conv,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"from the mesh".to_vec(),
        );
        let event_id = event.id;

        // Node (answerer): relay the gateway peer into the mesh by serving sync over the channel.
        let url_a = url.clone();
        let node = tokio::spawn(async move {
            let mut gw = connect_secure(&url_a, "relay-room", Role::Answerer, &node_id, None)
                .await
                .expect("node connects");
            let store = Mutex::new({
                let mut log = EventLog::default();
                log.append(event).unwrap();
                log
            });
            let _ = serve_connection(&mut gw.channel, &store).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Phone (offerer): one sync round must pull the node's event over the gateway.
        let mut gw = connect_secure(&url, "relay-room", Role::Offerer, &phone_id, None)
            .await
            .expect("phone connects");
        let phone_store = Mutex::new(EventLog::default());
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            request_round(&mut gw.channel, &phone_store, conv),
        )
        .await
        .expect("sync round did not time out")
        .expect("sync round");

        assert!(
            phone_store.lock().unwrap().has(&event_id),
            "phone pulled the node's mesh event over the WebRTC gateway"
        );

        // The serve loop blocks on recv until the peer link tears down (WebRTC's disconnect
        // timeout); we've verified the sync, so stop it rather than waiting that out.
        node.abort();
    }

    #[tokio::test]
    async fn mesh_hub_relays_an_event_between_two_spokes() {
        // One desktop-style HUB serving TWO spokes in a single room: spoke A pushes an event to
        // the hub, spoke B pulls it from the hub — A reaches B through the hub, no direct link.
        use crate::eventlog::event::{Event, EventKind};
        use crate::eventlog::store::EventLog;
        use crate::eventlog::ConversationId;
        use crate::session::request_round;
        use std::sync::Mutex;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(mesh_talk_signal::serve(listener));
        let url = format!("ws://{addr}");

        let hub_id = Arc::new(DeviceIdentity::generate());
        let a_id = DeviceIdentity::generate();
        let b_id = DeviceIdentity::generate();
        let conv = ConversationId::new([9u8; 32]);

        // Hub joins first (so the relay makes it the hub), serving a shared store.
        let hub_store = Arc::new(Mutex::new(EventLog::default()));
        let url_h = url.clone();
        let hs = Arc::clone(&hub_store);
        let hub = tokio::spawn(async move {
            let _ = serve_mesh_hub(&url_h, "hubroom", hub_id, hs).await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // Spoke A holds an event; one sync round pushes it to the hub.
        let event = Event::new(
            &a_id,
            conv,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"hi from A".to_vec(),
        );
        let event_id = event.id;
        let a_store = Mutex::new({
            let mut l = EventLog::default();
            l.append(event).unwrap();
            l
        });
        let mut a_gw = connect_mesh_spoke(&url, "hubroom", &a_id, None)
            .await
            .expect("A connects");
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            request_round(&mut a_gw.channel, &a_store, conv),
        )
        .await
        .expect("A round not timed out")
        .expect("A round");

        // Spoke B starts empty; one sync round pulls A's event from the hub.
        let b_store = Mutex::new(EventLog::default());
        let mut b_gw = connect_mesh_spoke(&url, "hubroom", &b_id, None)
            .await
            .expect("B connects");
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            request_round(&mut b_gw.channel, &b_store, conv),
        )
        .await
        .expect("B round not timed out")
        .expect("B round");

        assert!(
            hub_store.lock().unwrap().has(&event_id),
            "hub received + retained A's event"
        );
        assert!(
            b_store.lock().unwrap().has(&event_id),
            "B pulled A's event through the hub (A never connected to B)"
        );
        hub.abort();
    }
}
