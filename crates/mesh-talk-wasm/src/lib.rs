//! WASM bindings for `mesh-talk-core` — the browser/PWA entry point to the protocol stack.
//!
//! Part of the mobile-PWA plan (`docs/superpowers/specs/2026-06-23-mobile-pwa-design.md`).
//! Phase 1 exposes:
//!   - a smoke export proving the crypto runs in a JS/wasm runtime, and
//!   - a password-sealed device-identity keystore, so the browser can persist an identity in
//!     IndexedDB (the JS side stores the opaque blob; plaintext keys never leave wasm).
//! Real messaging bindings — event log, ratchets, sync over a browser transport — land in later
//! phases.

mod datachannel;

use mesh_talk_core::dm::dm_conversation_id;
use mesh_talk_core::identity::device::{DeviceIdentity, PublicIdentity};
use mesh_talk_core::ratchet::{dm_shared_root, init_alice, init_bob, Header, RatchetState};
use mesh_talk_core::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use wasm_bindgen::prelude::*;
use zeroize::Zeroize;

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Generate a fresh device identity and return its fingerprint (user id).
///
/// Smoke test: drives dalek Ed25519/X25519 key generation — i.e. the wasm randomness path
/// (`getrandom/js`) — plus the fingerprint hashing, so a non-empty return from a browser/Node
/// wasm runtime proves the protocol crypto works there.
#[wasm_bindgen]
pub fn generate_identity_fingerprint() -> String {
    DeviceIdentity::generate().public().user_id()
}

/// Run a full mesh `SecureChannel` (Noise XX) handshake in-wasm over an in-memory duplex, then
/// send `message` through it and return what the other end received.
///
/// Proves the authenticated, encrypted channel — the same one the browser gateway runs over a
/// WebRTC data channel — actually *executes* in a JS/wasm runtime (the snow handshake, AES-GCM,
/// and the length-prefixed framing), not merely compiles. The two endpoints are driven
/// concurrently on the single wasm task via `join!`.
#[wasm_bindgen]
pub async fn noise_loopback(message: String) -> Result<String, JsError> {
    use mesh_talk_core::transport::channel::SecureChannel;

    let (a_io, b_io) = tokio::io::duplex(64 * 1024);
    let a_id = DeviceIdentity::generate();
    let b_id = DeviceIdentity::generate();
    let (a_res, b_res) = futures::join!(
        SecureChannel::connect(a_io, &a_id, None),
        SecureChannel::accept(b_io, &b_id),
    );
    let mut a = a_res.map_err(to_js)?;
    let mut b = b_res.map_err(to_js)?;
    a.send(message.as_bytes()).await.map_err(to_js)?;
    let got = b.recv().await.map_err(to_js)?;
    String::from_utf8(got).map_err(to_js)
}

/// Run the mesh `SecureChannel` (Noise XX) over a browser `RTCDataChannel` and return the
/// authenticated peer's fingerprint. `initiator` is the offerer side. This is the browser
/// counterpart to the node gateway: in production the PWA runs this over the WebRTC data channel
/// to the node, then syncs the mesh event log through it.
#[wasm_bindgen]
pub async fn secure_handshake_over_dc(
    dc: web_sys::RtcDataChannel,
    initiator: bool,
) -> Result<String, JsError> {
    use mesh_talk_core::transport::channel::SecureChannel;

    let stream = datachannel::DataChannelStream::new(dc);
    let id = DeviceIdentity::generate();
    let channel = if initiator {
        SecureChannel::connect(stream, &id, None)
            .await
            .map_err(to_js)?
    } else {
        SecureChannel::accept(stream, &id).await.map_err(to_js)?
    };
    Ok(channel.peer_identity().user_id())
}

/// Run the mesh event-log sync over a data channel and return how many events the local store
/// holds afterwards. The initiator drives `request_round` (and closes the channel when done so
/// the responder's serve loop stops); the responder serves rounds. With `seed`, the store starts
/// with one event. This is the in-wasm proof that the PWA can sync the mesh over WebRTC: seed one
/// side and both converge to the same event.
#[wasm_bindgen]
pub async fn sync_demo_over_dc(
    dc: web_sys::RtcDataChannel,
    initiator: bool,
    seed: bool,
) -> Result<u32, JsError> {
    use mesh_talk_core::eventlog::event::{Event, EventKind};
    use mesh_talk_core::eventlog::store::EventLog;
    use mesh_talk_core::eventlog::{ConversationId, SyncStore};
    use mesh_talk_core::session::{request_round, serve_one, Served};
    use mesh_talk_core::transport::channel::SecureChannel;
    use std::sync::Mutex;

    let id = DeviceIdentity::generate();
    let conv = ConversationId::new([9u8; 32]);
    let store = Mutex::new(EventLog::default());
    if seed {
        let ev = Event::new(
            &id,
            conv,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"hi from the PWA".to_vec(),
        );
        store.lock().unwrap().append(ev).map_err(to_js)?;
    }

    let close_handle = dc.clone();
    let stream = datachannel::DataChannelStream::new(dc);
    let mut channel = if initiator {
        SecureChannel::connect(stream, &id, None)
            .await
            .map_err(to_js)?
    } else {
        SecureChannel::accept(stream, &id).await.map_err(to_js)?
    };

    if initiator {
        request_round(&mut channel, &store, conv)
            .await
            .map_err(to_js)?;
        // Tell the responder we're done so its serve loop stops (no wasm timer to idle out on).
        let _ = close_handle.close();
    } else {
        // Serve until the initiator closes the channel.
        loop {
            match serve_one(&mut channel, &store).await {
                Ok(Served::Closed) => break,
                Ok(Served::Handled(_)) => {}
                Err(_) => break,
            }
        }
    }

    let count = store.lock().unwrap().event_ids(&conv).len();
    Ok(count as u32)
}

// --- Stateful node (the seed of the PWA's command surface) ----------------------------------
//
// A device identity + an in-memory event log for a single demo conversation. send_message
// appends a Message event (the node's prepare → seq → Event::new path); messages reads them back
// in order. The full surface (per-account DM addressing, ratchet sealing, channels, IndexedDB
// persistence, sync of its own log) grows from here; this proves the stateful store + event
// construction run in wasm.

/// Build a peer `PublicIdentity` from its raw ed25519 + x25519 public keys (32 bytes each).
fn peer_identity(ed: &[u8], x: &[u8]) -> Result<PublicIdentity, JsError> {
    let ed25519_pub: [u8; 32] = ed
        .try_into()
        .map_err(|_| JsError::new("peer ed25519 key must be 32 bytes"))?;
    let x25519_pub: [u8; 32] = x
        .try_into()
        .map_err(|_| JsError::new("peer x25519 key must be 32 bytes"))?;
    Ok(PublicIdentity {
        ed25519_pub,
        x25519_pub,
    })
}

/// A fresh, independent copy of a stored ratchet session (via a serialize round-trip), or None if
/// absent. Used so decrypt/encrypt mutate a copy and the kept session changes only on success.
fn session_copy(sessions: &HashMap<String, RatchetState>, peer_id: &str) -> Option<RatchetState> {
    sessions
        .get(peer_id)
        .map(|s| RatchetState::deserialize(&s.serialize()).expect("stored session round-trips"))
}

/// Frame a ratchet header + ciphertext: `u16-BE header_len ‖ header ‖ ct` (matches the node).
fn frame(header: &Header, ct: &[u8]) -> Vec<u8> {
    let h = header.encode();
    let mut out = Vec::with_capacity(2 + h.len() + ct.len());
    out.extend_from_slice(&(h.len() as u16).to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(ct);
    out
}

fn unframe(wire: &[u8]) -> Option<(Header, &[u8])> {
    if wire.len() < 2 {
        return None;
    }
    let hlen = u16::from_be_bytes([wire[0], wire[1]]) as usize;
    if wire.len() < 2 + hlen {
        return None;
    }
    let header = Header::decode(&wire[2..2 + hlen])?;
    Some((header, &wire[2 + hlen..]))
}

/// Lowercase hex of bytes (used to key DM plaintext by event id).
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

type SharedLog = std::rc::Rc<std::sync::Mutex<mesh_talk_core::eventlog::store::EventLog>>;
/// Per-peer Double Ratchet sessions, keyed by the peer's user-id. In-memory; serialize via
/// `ratchet_sessions_snapshot`/`restore_ratchet_sessions` to persist across reloads.
type Sessions = std::rc::Rc<std::sync::Mutex<HashMap<String, RatchetState>>>;
/// Decrypted DM plaintext, keyed by event-id hex. Ratchet message keys are single-use, so a DM is
/// opened exactly once (own messages on send, peer messages on receive) and the plaintext kept
/// here for history — re-opening would advance/break the ratchet.
type DmPlain = std::rc::Rc<std::sync::Mutex<HashMap<String, String>>>;
/// Peers discovered via the gateway Noise handshake, keyed by user-id → their authenticated
/// public identity. The roster the UI lists to start/continue a DM.
type Roster = std::rc::Rc<std::sync::Mutex<HashMap<String, PublicIdentity>>>;

/// Well-known conversation id for the presence directory: every node announces its identity here
/// and the conversation is gossiped, so all nodes learn all identities (mesh discovery).
const DIRECTORY_CONV: [u8; 32] = [7u8; 32];

/// Well-known conversation id for the relay directory: nodes announce the relay endpoint(s) they
/// host/use here (payload = the ws URL, UTF-8). Gossiped like the identity directory, so a phone
/// learns every online relay through the mesh and can fail over to any of them if the one it
/// scanned goes offline — zero-config relay failover.
const RELAYS_CONV: [u8; 32] = [8u8; 32];

/// The persistable DM state: ratchet sessions, decrypted plaintext (by event-id hex), and the
/// peer roster (user-id → ed25519/x25519 bytes). Sealed into IndexedDB so DMs survive a reload.
#[derive(Serialize, Deserialize)]
struct DmState {
    sessions: Vec<(String, Vec<u8>)>,
    plain: Vec<(String, String)>,
    roster: Vec<(String, (Vec<u8>, Vec<u8>))>,
}

#[wasm_bindgen]
pub struct WasmNode {
    identity: std::rc::Rc<DeviceIdentity>,
    log: SharedLog,
    conv: mesh_talk_core::eventlog::ConversationId,
    ratchets: Sessions,
    dm_plain: DmPlain,
    peers: Roster,
}

impl WasmNode {
    /// Append a Message event carrying `payload` to `conv`; returns the appended event (for its id
    /// + serialization). Shared by the demo conversation and per-peer ratcheted DMs.
    fn append_message(
        &self,
        conv: mesh_talk_core::eventlog::ConversationId,
        payload: Vec<u8>,
    ) -> Result<mesh_talk_core::eventlog::event::Event, JsError> {
        use mesh_talk_core::eventlog::event::{Author, Event, EventKind};
        let mut log = self.log.lock().unwrap();
        let author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let (parents, lamport) = log.prepare(&conv);
        let seq = log.version_vector(&conv).get(&author).copied().unwrap_or(0) + 1;
        let wall_clock = js_sys::Date::now() as u64;
        let event = Event::new(
            &self.identity,
            conv,
            seq,
            parents,
            lamport,
            wall_clock,
            EventKind::Message,
            payload,
        );
        log.append(event.clone()).map_err(to_js)?;
        Ok(event)
    }

    /// Open any of `peer`'s DM events that have arrived (e.g. via sync) but aren't decrypted yet,
    /// keeping the plaintext for history. Opens in log order; the ratchet tolerates gaps via its
    /// skipped-key buffer. Own messages are skipped (already kept on send).
    fn process_dm_events(&self, peer: &PublicIdentity) -> Result<(), JsError> {
        use mesh_talk_core::eventlog::event::Author;
        let conv = dm_conversation_id(&self.identity.public(), peer);
        let me_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let to_open: Vec<(String, Vec<u8>)> = {
            let log = self.log.lock().unwrap();
            let plain = self.dm_plain.lock().unwrap();
            log.events(&conv)
                .iter()
                .filter(|e| e.author != me_author)
                .filter_map(|e| {
                    let h = hex(e.id.as_bytes());
                    if plain.contains_key(&h) {
                        None
                    } else {
                        Some((h, e.ciphertext.clone()))
                    }
                })
                .collect()
        };
        for (id_hex, wire) in to_open {
            if let Ok(pt) = self.open_dm_ratcheted(&peer.ed25519_pub, &peer.x25519_pub, &wire) {
                self.dm_plain.lock().unwrap().insert(id_hex, pt);
            }
        }
        Ok(())
    }

    /// Every identity this node knows: directly-handshook peers + everyone in the gossiped presence
    /// directory, deduped by user-id. The set a node tries to gossip/open DM conversations with, so
    /// it reaches any mesh member (not just peers it connected to directly). Takes no locks across
    /// the two it briefly holds.
    fn known_identities(&self) -> Vec<PublicIdentity> {
        let mut by_id: HashMap<String, PublicIdentity> = HashMap::new();
        for p in self.peers.lock().unwrap().values() {
            by_id.insert(
                p.user_id(),
                PublicIdentity {
                    ed25519_pub: p.ed25519_pub,
                    x25519_pub: p.x25519_pub,
                },
            );
        }
        let conv = mesh_talk_core::eventlog::ConversationId::new(DIRECTORY_CONV);
        for e in self.log.lock().unwrap().events(&conv) {
            // Payload = ed25519 (32) || x25519 (32) || optional name; older ones were exactly 64.
            if e.ciphertext.len() >= 64 {
                let ed: [u8; 32] = e.ciphertext[..32].try_into().unwrap();
                let x: [u8; 32] = e.ciphertext[32..64].try_into().unwrap();
                let pid = PublicIdentity {
                    ed25519_pub: ed,
                    x25519_pub: x,
                };
                by_id.entry(pid.user_id()).or_insert(pid);
            }
        }
        by_id.into_values().collect()
    }
}

#[wasm_bindgen]
impl WasmNode {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> WasmNode {
        WasmNode {
            identity: std::rc::Rc::new(DeviceIdentity::generate()),
            log: std::rc::Rc::new(std::sync::Mutex::new(
                mesh_talk_core::eventlog::store::EventLog::default(),
            )),
            conv: mesh_talk_core::eventlog::ConversationId::new([42u8; 32]),
            ratchets: std::rc::Rc::new(std::sync::Mutex::new(HashMap::new())),
            dm_plain: std::rc::Rc::new(std::sync::Mutex::new(HashMap::new())),
            peers: std::rc::Rc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Build a node with a persistent identity from a sealed keystore blob + password (vs `new`,
    /// which generates a fresh identity each time). The conversation log starts empty; restore it
    /// separately. Lets the PWA keep the same identity across reloads.
    pub fn from_keystore(blob: &[u8], password: &str) -> Result<WasmNode, JsError> {
        Ok(WasmNode {
            identity: std::rc::Rc::new(open_keystore(blob, password)?),
            log: std::rc::Rc::new(std::sync::Mutex::new(
                mesh_talk_core::eventlog::store::EventLog::default(),
            )),
            conv: mesh_talk_core::eventlog::ConversationId::new([42u8; 32]),
            ratchets: std::rc::Rc::new(std::sync::Mutex::new(HashMap::new())),
            dm_plain: std::rc::Rc::new(std::sync::Mutex::new(HashMap::new())),
            peers: std::rc::Rc::new(std::sync::Mutex::new(HashMap::new())),
        })
    }

    /// This device's fingerprint (user id).
    pub fn fingerprint(&self) -> String {
        self.identity.public().user_id()
    }

    /// This node's X25519 public key — a peer seals DMs to it (and authenticates DMs from us).
    pub fn x25519_public(&self) -> Vec<u8> {
        self.identity.public().x25519_pub.to_vec()
    }

    /// This node's Ed25519 public key — a peer needs it (with the X25519 key) to ratchet DMs.
    pub fn ed25519_public(&self) -> Vec<u8> {
        self.identity.public().ed25519_pub.to_vec()
    }

    /// Seal `text` as a private DM to a recipient's X25519 public key, returning the sealed
    /// envelope. Per-recipient encrypted + sender-authenticated — reuses the mesh's `dm`
    /// sealed-box (the same primitive the desktop node uses), not new crypto.
    pub fn seal_dm(&self, recipient_x25519: &[u8], text: &str) -> Result<Vec<u8>, JsError> {
        let key: [u8; 32] = recipient_x25519
            .try_into()
            .map_err(|_| JsError::new("recipient x25519 key must be 32 bytes"))?;
        mesh_talk_core::dm::seal(&self.identity, &key, text.as_bytes()).map_err(to_js)
    }

    /// Open a sealed DM addressed to us from a sender with the given X25519 public key. Errors if
    /// it wasn't sealed to us or the sender doesn't match (the AEAD + sender binding reject it).
    pub fn open_dm(&self, sender_x25519: &[u8], envelope: &[u8]) -> Result<String, JsError> {
        let key: [u8; 32] = sender_x25519
            .try_into()
            .map_err(|_| JsError::new("sender x25519 key must be 32 bytes"))?;
        let plaintext = mesh_talk_core::dm::open(&self.identity, &key, envelope).map_err(to_js)?;
        String::from_utf8(plaintext).map_err(to_js)
    }

    /// Seal `text` as a forward-secret DM to a peer using the Double Ratchet (the same wire format
    /// the desktop node speaks), returning the wire blob to sync. Establishes the session as
    /// initiator on first use; advances + keeps the per-peer session. Reuses the core `ratchet`
    /// crypto — bidirectional + forward-secret, unlike the one-shot `seal_dm` sealed-box.
    pub fn seal_dm_ratcheted(
        &self,
        peer_ed25519: &[u8],
        peer_x25519: &[u8],
        text: &str,
    ) -> Result<Vec<u8>, JsError> {
        let peer = peer_identity(peer_ed25519, peer_x25519)?;
        let peer_id = peer.user_id();
        let mut sessions = self.ratchets.lock().unwrap();
        // Work on a fresh copy (deserialize round-trip), commit only on success — mirrors the
        // native store's get()/put() semantics so a failure never corrupts the kept session.
        let mut state = session_copy(&sessions, &peer_id).unwrap_or_else(|| {
            init_alice(&dm_shared_root(&self.identity, &peer), &peer.x25519_pub)
        });
        let (header, ct) = state
            .ratchet_encrypt(text.as_bytes())
            .map_err(|_| JsError::new("ratchet encrypt"))?;
        sessions.insert(peer_id, state);
        Ok(frame(&header, &ct))
    }

    /// TEST-ONLY: seal a DM the way the NATIVE node does — wrapping the text in a MessageBody
    /// (`MTB1`) frame before the ratchet — so a test can prove the receiver strips the frame and
    /// surfaces the bare text (the "messages arrive with an MTB1 prefix" bug).
    pub fn seal_framed_dm_ratcheted(
        &self,
        peer_ed25519: &[u8],
        peer_x25519: &[u8],
        text: &str,
    ) -> Result<Vec<u8>, JsError> {
        let peer = peer_identity(peer_ed25519, peer_x25519)?;
        let peer_id = peer.user_id();
        let framed =
            mesh_talk_core::message::MessageBody::new(text.as_bytes().to_vec(), None).encode();
        let mut sessions = self.ratchets.lock().unwrap();
        let mut state = session_copy(&sessions, &peer_id).unwrap_or_else(|| {
            init_alice(&dm_shared_root(&self.identity, &peer), &peer.x25519_pub)
        });
        let (header, ct) = state
            .ratchet_encrypt(&framed)
            .map_err(|_| JsError::new("ratchet encrypt"))?;
        sessions.insert(peer_id, state);
        Ok(frame(&header, &ct))
    }

    /// Open a forward-secret DM from a peer. Bootstraps as responder on first contact; resolves a
    /// simultaneous-init race by the user-id tie-break. Decrypts on a copy and persists the
    /// advanced session ONLY on success (ratchet_decrypt mutates before AEAD auth, so a forged
    /// message must not be able to corrupt the stored session).
    pub fn open_dm_ratcheted(
        &self,
        peer_ed25519: &[u8],
        peer_x25519: &[u8],
        wire: &[u8],
    ) -> Result<String, JsError> {
        let peer = peer_identity(peer_ed25519, peer_x25519)?;
        let peer_id = peer.user_id();
        let (header, ct) = unframe(wire).ok_or_else(|| JsError::new("bad DM wire"))?;
        let my_secret = self.identity.secret_bytes().1;
        let mut sessions = self.ratchets.lock().unwrap();

        let mut state = session_copy(&sessions, &peer_id)
            .unwrap_or_else(|| init_bob(&dm_shared_root(&self.identity, &peer), my_secret));
        if let Ok(pt) = state.ratchet_decrypt(&header, ct) {
            sessions.insert(peer_id, state);
            // Strip the MessageBody frame (MTB1 ‖ body) the native node wraps DMs in; a raw,
            // unframed plaintext (what the wasm node sends) falls back to itself.
            return String::from_utf8(mesh_talk_core::message::MessageBody::decode(&pt).text)
                .map_err(to_js);
        }
        // Simultaneous init: if the PEER is the canonical initiator (lower user-id), re-bootstrap
        // as responder and retry once.
        if peer_id < self.identity.public().user_id() {
            let mut fresh = init_bob(&dm_shared_root(&self.identity, &peer), my_secret);
            let pt = fresh
                .ratchet_decrypt(&header, ct)
                .map_err(|_| JsError::new("ratchet decrypt"))?;
            sessions.insert(peer_id, fresh);
            return String::from_utf8(mesh_talk_core::message::MessageBody::decode(&pt).text)
                .map_err(to_js);
        }
        Err(JsError::new("ratchet decrypt"))
    }

    /// Send a forward-secret DM to `peer`: seal it with the ratchet, append it as an event to the
    /// per-peer conversation, and remember the plaintext (own messages can't be ratchet-reopened).
    /// Returns the serialized event to deliver to the peer (the gateway sync carries these).
    pub fn send_ratcheted_dm(
        &self,
        peer_ed25519: &[u8],
        peer_x25519: &[u8],
        text: &str,
    ) -> Result<Vec<u8>, JsError> {
        let peer = peer_identity(peer_ed25519, peer_x25519)?;
        let conv = dm_conversation_id(&self.identity.public(), &peer);
        let wire = self.seal_dm_ratcheted(peer_ed25519, peer_x25519, text)?;
        let event = self.append_message(conv, wire)?;
        self.dm_plain
            .lock()
            .unwrap()
            .insert(hex(event.id.as_bytes()), text.to_string());
        bincode::serialize(&event).map_err(to_js)
    }

    /// Receive a serialized DM event from `peer`: open the ratchet payload ONCE (keeping the
    /// plaintext for history), then file the event in the per-peer conversation.
    pub fn receive_ratcheted_dm(
        &self,
        peer_ed25519: &[u8],
        peer_x25519: &[u8],
        event_bytes: &[u8],
    ) -> Result<(), JsError> {
        let event: mesh_talk_core::eventlog::event::Event =
            bincode::deserialize(event_bytes).map_err(to_js)?;
        let plaintext = self.open_dm_ratcheted(peer_ed25519, peer_x25519, &event.ciphertext)?;
        self.dm_plain
            .lock()
            .unwrap()
            .insert(hex(event.id.as_bytes()), plaintext);
        self.log.lock().unwrap().append(event).map_err(to_js)?;
        Ok(())
    }

    /// The DM conversation with `peer` as `HistoryItem`-shaped JSON (decrypted plaintext per event).
    pub fn ratcheted_dm_history(
        &self,
        peer_ed25519: &[u8],
        peer_x25519: &[u8],
    ) -> Result<String, JsError> {
        use mesh_talk_core::eventlog::event::Author;

        #[derive(serde::Serialize)]
        struct Item {
            id: Option<String>,
            from_me: bool,
            who: String,
            text: String,
            wall_clock: u64,
            reply_to: Option<String>,
            file: Option<()>,
        }

        let peer = peer_identity(peer_ed25519, peer_x25519)?;
        let conv = dm_conversation_id(&self.identity.public(), &peer);
        let me_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let me = self.identity.public().user_id();
        let peer_id = peer.user_id();
        let plain = self.dm_plain.lock().unwrap();
        let log = self.log.lock().unwrap();
        let items: Vec<Item> = log
            .events(&conv)
            .iter()
            .map(|e| {
                let from_me = e.author == me_author;
                let id_hex = hex(e.id.as_bytes());
                let text = plain.get(&id_hex).cloned().unwrap_or_default();
                Item {
                    id: Some(id_hex),
                    from_me,
                    who: if from_me { me.clone() } else { peer_id.clone() },
                    text,
                    wall_clock: e.wall_clock,
                    reply_to: None,
                    file: None,
                }
            })
            .collect();
        serde_json::to_string(&items).map_err(to_js)
    }

    /// Sync the DM conversation with the peer on the other end of `dc` over the gateway: run the
    /// Noise handshake, take the peer's AUTHENTICATED identity from it, record the peer in the
    /// roster, sync the per-peer DM conversation, and open any newly-arrived messages from them.
    /// Returns the peer's fingerprint. This is the encrypted-DM counterpart to `sync_over_dc`.
    pub async fn sync_dm_over_dc(
        &self,
        dc: web_sys::RtcDataChannel,
        initiator: bool,
    ) -> Result<String, JsError> {
        use mesh_talk_core::session::{request_round, serve_one, Served};
        use mesh_talk_core::transport::channel::SecureChannel;

        let log = std::rc::Rc::clone(&self.log);
        let identity = std::rc::Rc::clone(&self.identity);
        let close_handle = dc.clone();
        let stream = datachannel::DataChannelStream::new(dc);
        let mut channel = if initiator {
            SecureChannel::connect(stream, &identity, None)
                .await
                .map_err(to_js)?
        } else {
            SecureChannel::accept(stream, &identity)
                .await
                .map_err(to_js)?
        };

        // The peer identity is authenticated by the Noise handshake (not asserted by the caller).
        let peer = {
            let p = channel.peer_identity();
            PublicIdentity {
                ed25519_pub: p.ed25519_pub,
                x25519_pub: p.x25519_pub,
            }
        };
        let conv = dm_conversation_id(&identity.public(), &peer);

        if initiator {
            request_round(&mut channel, &log, conv)
                .await
                .map_err(to_js)?;
            let _ = close_handle.close();
        } else {
            loop {
                match serve_one(&mut channel, &log).await {
                    Ok(Served::Closed) => break,
                    Ok(Served::Handled(_)) => {}
                    Err(_) => break,
                }
            }
        }

        let uid = peer.user_id();
        self.peers.lock().unwrap().insert(
            uid.clone(),
            PublicIdentity {
                ed25519_pub: peer.ed25519_pub,
                x25519_pub: peer.x25519_pub,
            },
        );
        self.process_dm_events(&peer)?;
        Ok(uid)
    }

    /// The conversation set this node syncs as a sync INITIATOR (a phone/spoke gossiping with its
    /// hub): the presence ([7;32]) + relay ([8;32]) directories ALWAYS — because a phone is a
    /// first-class directory participant, equal to any LAN node, the relay merely its transport —
    /// plus every conversation already in the log and a DM conversation with every known identity.
    /// Shared by `sync_all_over_dc` and its introspection helper so the two cannot drift.
    fn sync_conv_set(
        &self,
        known: &[PublicIdentity],
    ) -> Vec<mesh_talk_core::eventlog::ConversationId> {
        use mesh_talk_core::eventlog::ConversationId;
        let log = self.log.lock().unwrap();
        let mut set = log.conversations();
        for dir in [
            ConversationId::new(DIRECTORY_CONV),
            ConversationId::new(RELAYS_CONV),
        ] {
            if !set.contains(&dir) {
                set.push(dir);
            }
        }
        let me = self.identity.public();
        for p in known {
            let c = dm_conversation_id(&me, p);
            if !set.contains(&c) {
                set.push(c);
            }
        }
        set
    }

    /// Introspection (tests): the conversation ids this node would sync right now, as a hex-string
    /// JSON array. Lets a test assert that even a fresh phone (empty log, never announced) ALWAYS
    /// pulls the presence + relay directories — the "phone is an equal node" guarantee.
    pub fn sync_conv_set_hex(&self) -> Result<String, JsError> {
        let known = self.known_identities();
        let ids: Vec<String> = self
            .sync_conv_set(&known)
            .into_iter()
            .map(|c| c.as_bytes().iter().map(|b| format!("{b:02x}")).collect())
            .collect();
        serde_json::to_string(&ids).map_err(to_js)
    }

    /// Gossip ALL conversations with the peer on the other end of `dc` (vs sync_dm_over_dc, which
    /// syncs only the conversation with that peer). The initiator runs a sync round for every
    /// conversation it knows (the presence + relay directories, its event log, + a DM conversation
    /// per known identity); the responder serves whatever is requested. This is what lets a node
    /// reach everyone through a hub: the hub gossips every conversation with each phone, so A→hub→B
    /// routes transitively. Returns the peer's fingerprint.
    pub async fn sync_all_over_dc(
        &self,
        dc: web_sys::RtcDataChannel,
        initiator: bool,
    ) -> Result<String, JsError> {
        use mesh_talk_core::session::{request_round, serve_one, Served};
        use mesh_talk_core::transport::channel::SecureChannel;

        let log = std::rc::Rc::clone(&self.log);
        let identity = std::rc::Rc::clone(&self.identity);
        let close_handle = dc.clone();
        let stream = datachannel::DataChannelStream::new(dc);
        let mut channel = if initiator {
            SecureChannel::connect(stream, &identity, None)
                .await
                .map_err(to_js)?
        } else {
            SecureChannel::accept(stream, &identity)
                .await
                .map_err(to_js)?
        };

        let peer = {
            let p = channel.peer_identity();
            PublicIdentity {
                ed25519_pub: p.ed25519_pub,
                x25519_pub: p.x25519_pub,
            }
        };

        // Remember the peer we just handshook with (e.g. the hub) BEFORE building the conversation
        // set — so THIS sync already requests the DM conversation with it. Otherwise the hub is only
        // known on the NEXT pass, and a desktop→phone DM in a fresh conversation is never pulled
        // (the exact "online but DMs never arrive" bug).
        self.peers.lock().unwrap().insert(
            peer.user_id(),
            PublicIdentity {
                ed25519_pub: peer.ed25519_pub,
                x25519_pub: peer.x25519_pub,
            },
        );

        // Everyone we know (handshook peers + the gossiped directory) — computed before locking the
        // log below (known_identities locks it itself).
        let known = self.known_identities();

        if initiator {
            // Every conversation we sync: the presence + relay directories (ALWAYS), those in the
            // log, and a DM conversation with every known identity (so we also pull conversations we
            // have no events for yet — incl. messages a hub holds from a peer we've never met).
            let convs = self.sync_conv_set(&known);
            // Resilient: one conversation failing to sync must not abort the whole pass (and
            // crucially must not skip the steps after this loop).
            for conv in convs {
                let _ = request_round(&mut channel, &log, conv).await;
            }
            let _ = close_handle.close();
        } else {
            loop {
                match serve_one(&mut channel, &log).await {
                    Ok(Served::Closed) => break,
                    Ok(Served::Handled(_)) => {}
                    Err(_) => break,
                }
            }
        }

        // Open any newly-arrived DM events from everyone we know (re-read: the directory may have
        // grown during this sync, revealing peers whose messages the hub just delivered).
        for p in &self.known_identities() {
            let _ = self.process_dm_events(p);
        }
        Ok(peer.user_id())
    }

    /// The conversation ids present in the local event log, as hex JSON (introspection/tests).
    pub fn conversation_ids(&self) -> Result<String, JsError> {
        let log = self.log.lock().unwrap();
        let ids: Vec<String> = log
            .conversations()
            .iter()
            .map(|c| hex(c.as_bytes()))
            .collect();
        serde_json::to_string(&ids).map_err(to_js)
    }

    /// Announce this node's identity into the well-known directory conversation (presence). The
    /// payload is the public ed25519+x25519 keys (identities are public). Idempotent: skips if we
    /// already announced. Gossiped by sync_all, so everyone learns everyone — the discovery that
    /// lets a node address (DM) any other node in the mesh.
    pub fn announce_self(&self, name: &str) -> Result<(), JsError> {
        use mesh_talk_core::eventlog::event::{Author, Event, EventKind};
        let pubid = self.identity.public();
        let conv = mesh_talk_core::eventlog::ConversationId::new(DIRECTORY_CONV);
        // Heartbeat: append a fresh presence event with a current wall_clock, so peers can tell
        // who is ONLINE NOW, not merely who ever announced. Payload = ed25519 (32) || x25519 (32)
        // || display name (UTF-8) — the name lets the UI show real names like the desktop.
        let mut payload = Vec::with_capacity(64 + name.len());
        payload.extend_from_slice(&pubid.ed25519_pub);
        payload.extend_from_slice(&pubid.x25519_pub);
        payload.extend_from_slice(name.as_bytes());
        let mut log = self.log.lock().unwrap();
        // Parentless on purpose: presence events are never referenced, so old heartbeats stay
        // unreferenced and `compact_presence` can drop all but the newest per author.
        let author = Author::from_ed25519(pubid.ed25519_pub);
        let (_, lamport) = log.prepare(&conv);
        let seq = log.version_vector(&conv).get(&author).copied().unwrap_or(0) + 1;
        let event = Event::new(
            &self.identity,
            conv,
            seq,
            Vec::new(),
            lamport,
            js_sys::Date::now() as u64,
            EventKind::Message,
            payload,
        );
        log.append(event).map_err(to_js)?;
        log.compact_presence(&conv);
        Ok(())
    }

    /// TEST-ONLY: this node's own latest presence event ([7;32]), bincode-encoded, hex. Another
    /// node can `ingest_event` it to simulate pulling this node's presence over a sync.
    pub fn directory_event_hex(&self) -> Result<String, JsError> {
        let conv = mesh_talk_core::eventlog::ConversationId::new(DIRECTORY_CONV);
        let log = self.log.lock().unwrap();
        let ev = log
            .events(&conv)
            .into_iter()
            .next_back()
            .ok_or_else(|| to_js("no presence event"))?;
        Ok(hex(&bincode::serialize(ev).map_err(to_js)?))
    }

    /// TEST-ONLY: the user-ids this node would treat as DM peers (handshook peers + the gossiped
    /// directory) — the exact set `sync_all_over_dc` builds DM conversations for.
    pub fn known_identity_ids(&self) -> Result<String, JsError> {
        let ids: Vec<String> = self
            .known_identities()
            .iter()
            .map(|p| p.user_id())
            .collect();
        serde_json::to_string(&ids).map_err(to_js)
    }

    /// The directory: every identity announced into the mesh, as `[{id, ed25519, x25519}]` (hex).
    /// Built from the gossiped directory conversation — the roster a node can DM anyone from.
    pub fn directory(&self) -> Result<String, JsError> {
        use mesh_talk_core::identity::device::PublicIdentity;

        #[derive(serde::Serialize)]
        struct P {
            id: String,
            ed25519: String,
            x25519: String,
            name: String,
            last_seen_ms: u64,
        }
        let conv = mesh_talk_core::eventlog::ConversationId::new(DIRECTORY_CONV);
        let log = self.log.lock().unwrap();
        // Keep the LATEST announcement per identity (newest wall_clock wins), so a node's name +
        // last-seen reflect its most recent heartbeat — not a stale first announcement.
        let mut latest: std::collections::HashMap<String, P> = std::collections::HashMap::new();
        for e in log.events(&conv) {
            if e.ciphertext.len() < 64 {
                continue;
            }
            let ed: [u8; 32] = e.ciphertext[..64][..32].try_into().unwrap();
            let id = PublicIdentity::user_id_from(&ed);
            let name = String::from_utf8_lossy(&e.ciphertext[64..]).into_owned();
            let entry = P {
                id: id.clone(),
                ed25519: hex(&ed),
                x25519: hex(&e.ciphertext[32..64]),
                name,
                last_seen_ms: e.wall_clock,
            };
            match latest.get(&id) {
                Some(prev) if prev.last_seen_ms >= e.wall_clock => {}
                _ => {
                    latest.insert(id, entry);
                }
            }
        }
        let out: Vec<P> = latest.into_values().collect();
        serde_json::to_string(&out).map_err(to_js)
    }

    /// Announce a relay endpoint (ws URL) into the gossiped relay directory. Idempotent per URL.
    /// Gossiped by sync_all, so other nodes — including phones reached only through a hub — learn
    /// this relay and can use it for failover.
    pub fn announce_relay(&self, url: &str) -> Result<(), JsError> {
        let conv = mesh_talk_core::eventlog::ConversationId::new(RELAYS_CONV);
        let bytes = url.as_bytes().to_vec();
        let already = self
            .log
            .lock()
            .unwrap()
            .events(&conv)
            .iter()
            .any(|e| e.ciphertext == bytes);
        if already {
            return Ok(());
        }
        self.append_message(conv, bytes)?;
        Ok(())
    }

    /// Every relay endpoint announced into the mesh (deduped), as a JSON array of ws URL strings.
    /// The basis for zero-config failover: a phone tries these when its current relay drops.
    pub fn known_relay_urls(&self) -> Result<String, JsError> {
        let conv = mesh_talk_core::eventlog::ConversationId::new(RELAYS_CONV);
        let log = self.log.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for e in log.events(&conv) {
            if let Ok(url) = std::str::from_utf8(&e.ciphertext) {
                if seen.insert(url.to_string()) {
                    out.push(url.to_string());
                }
            }
        }
        serde_json::to_string(&out).map_err(to_js)
    }

    /// The discovered peers (from gateway handshakes) as JSON: `[{id, ed25519, x25519}]` (hex keys).
    pub fn peer_roster(&self) -> Result<String, JsError> {
        #[derive(serde::Serialize)]
        struct P {
            id: String,
            ed25519: String,
            x25519: String,
        }
        let peers = self.peers.lock().unwrap();
        let list: Vec<P> = peers
            .values()
            .map(|p| P {
                id: p.user_id(),
                ed25519: hex(&p.ed25519_pub),
                x25519: hex(&p.x25519_pub),
            })
            .collect();
        serde_json::to_string(&list).map_err(to_js)
    }

    /// Append a serialized event to the log (what the sync does); used for direct event transfer.
    pub fn ingest_event(&self, event_bytes: &[u8]) -> Result<(), JsError> {
        let event: mesh_talk_core::eventlog::event::Event =
            bincode::deserialize(event_bytes).map_err(to_js)?;
        self.log.lock().unwrap().append(event).map_err(to_js)?;
        Ok(())
    }

    /// Open any of a peer's DM events present in the log but not yet decrypted (post-sync step).
    pub fn process_dm_events_with(
        &self,
        peer_ed25519: &[u8],
        peer_x25519: &[u8],
    ) -> Result<(), JsError> {
        let peer = peer_identity(peer_ed25519, peer_x25519)?;
        self.process_dm_events(&peer)
    }

    /// Serialize + password-seal the full DM state — ratchet sessions (secret keys), decrypted
    /// plaintext (ratchet messages can't be re-opened), and the peer roster — for IndexedDB, so a
    /// DM conversation survives a reload intact. Sealed with the same password as the keystore.
    pub fn dm_state_snapshot(&self, password: &str) -> Result<Vec<u8>, JsError> {
        let state = DmState {
            sessions: self
                .ratchets
                .lock()
                .unwrap()
                .iter()
                .map(|(peer, s)| (peer.clone(), s.serialize()))
                .collect(),
            plain: self
                .dm_plain
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            roster: self
                .peers
                .lock()
                .unwrap()
                .iter()
                .map(|(k, p)| (k.clone(), (p.ed25519_pub.to_vec(), p.x25519_pub.to_vec())))
                .collect(),
        };
        let plaintext = bincode::serialize(&state).map_err(to_js)?;
        seal_bytes(&plaintext, password)
    }

    /// Restore the DM state from a sealed snapshot (replaces current sessions/plaintext/roster).
    pub fn restore_dm_state(&self, blob: &[u8], password: &str) -> Result<(), JsError> {
        let bytes = open_bytes(blob, password)?;
        let state: DmState = bincode::deserialize(&bytes).map_err(to_js)?;
        {
            let mut sessions = self.ratchets.lock().unwrap();
            sessions.clear();
            for (peer, b) in state.sessions {
                if let Some(s) = RatchetState::deserialize(&b) {
                    sessions.insert(peer, s);
                }
            }
        }
        {
            let mut plain = self.dm_plain.lock().unwrap();
            plain.clear();
            plain.extend(state.plain);
        }
        {
            let mut peers = self.peers.lock().unwrap();
            peers.clear();
            for (k, (ed, x)) in state.roster {
                if let Ok(p) = peer_identity(&ed, &x) {
                    peers.insert(k, p);
                }
            }
        }
        Ok(())
    }

    /// Append a message to the conversation; returns the event count afterwards.
    pub fn send_message(&self, text: String) -> Result<u32, JsError> {
        self.append_message(self.conv, text.into_bytes())?;
        Ok(self.log.lock().unwrap().events(&self.conv).len() as u32)
    }

    /// The conversation's messages, in order, as a JSON array of strings.
    pub fn messages(&self) -> Result<String, JsError> {
        let log = self.log.lock().unwrap();
        let texts: Vec<String> = log
            .events(&self.conv)
            .iter()
            .map(|e| String::from_utf8_lossy(&e.ciphertext).to_string())
            .collect();
        serde_json::to_string(&texts).map_err(to_js)
    }

    /// The conversation's messages as `HistoryItem`-shaped JSON (what the app's UI renders).
    pub fn history_json(&self) -> Result<String, JsError> {
        use mesh_talk_core::eventlog::event::Author;

        #[derive(serde::Serialize)]
        struct Item {
            id: Option<String>,
            from_me: bool,
            who: String,
            text: String,
            wall_clock: u64,
            reply_to: Option<String>,
            file: Option<()>,
        }

        let self_author = Author::from_ed25519(self.identity.public().ed25519_pub);
        let me = self.identity.public().user_id();
        let log = self.log.lock().unwrap();
        let items: Vec<Item> = log
            .events(&self.conv)
            .iter()
            .map(|e| {
                let from_me = e.author == self_author;
                Item {
                    id: None,
                    from_me,
                    who: if from_me { me.clone() } else { "peer".into() },
                    text: String::from_utf8_lossy(&e.ciphertext).to_string(),
                    wall_clock: e.wall_clock,
                    reply_to: None,
                    file: None,
                }
            })
            .collect();
        serde_json::to_string(&items).map_err(to_js)
    }

    /// Serialize the conversation's event log to bytes, for persistence (e.g. IndexedDB).
    pub fn snapshot(&self) -> Result<Vec<u8>, JsError> {
        let log = self.log.lock().unwrap();
        let events = log.events(&self.conv);
        bincode::serialize(&events).map_err(to_js)
    }

    /// Restore events from a `snapshot` blob into this node's log (idempotent — duplicate or
    /// out-of-order events are skipped). Returns the event count afterwards.
    pub fn restore(&self, blob: &[u8]) -> Result<u32, JsError> {
        use mesh_talk_core::eventlog::event::Event;

        let events: Vec<Event> = bincode::deserialize(blob).map_err(to_js)?;
        let mut log = self.log.lock().unwrap();
        for event in events {
            let _ = log.append(event); // validated on append; skip dups/causal gaps
        }
        Ok(log.events(&self.conv).len() as u32)
    }

    /// Sync this node's log with a peer over a data channel: open the Noise channel (initiator =
    /// offerer), then reconcile the conversation's event log. After this, messages sent on either
    /// side are present on both. The initiator drives one round + closes; the responder serves.
    pub async fn sync_over_dc(
        &self,
        dc: web_sys::RtcDataChannel,
        initiator: bool,
    ) -> Result<(), JsError> {
        use mesh_talk_core::session::{request_round, serve_one, Served};
        use mesh_talk_core::transport::channel::SecureChannel;

        // Clone the shared handles so the future owns them (no borrow of `self` held across await).
        let log = std::rc::Rc::clone(&self.log);
        let identity = std::rc::Rc::clone(&self.identity);
        let conv = self.conv;

        let close_handle = dc.clone();
        let stream = datachannel::DataChannelStream::new(dc);
        let mut channel = if initiator {
            SecureChannel::connect(stream, &identity, None)
                .await
                .map_err(to_js)?
        } else {
            SecureChannel::accept(stream, &identity)
                .await
                .map_err(to_js)?
        };

        if initiator {
            request_round(&mut channel, &log, conv)
                .await
                .map_err(to_js)?;
            let _ = close_handle.close();
        } else {
            loop {
                match serve_one(&mut channel, &log).await {
                    Ok(Served::Closed) => break,
                    Ok(Served::Handled(_)) => {}
                    Err(_) => break,
                }
            }
        }
        Ok(())
    }
}

// --- Identity keystore (persisted to IndexedDB by the JS side) -------------------------------
//
// The same scheme as the native file keystore (PBKDF2 → AES-256-GCM, see
// `storage::encryption`), but it returns/accepts the sealed blob as bytes instead of touching a
// filesystem. The browser stores the opaque blob in IndexedDB; secret keys exist only inside
// wasm, decrypted transiently on open.

const SECRET_LEN: usize = 64; // ed25519 (32) || x25519 (32)

#[derive(Serialize, Deserialize)]
struct KeystoreBlob {
    salt: [u8; SALT_SIZE],
    nonce: [u8; NONCE_SIZE],
    ciphertext: Vec<u8>,
}

fn to_js<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}

fn seal(identity: &DeviceIdentity, password: &str) -> Result<Vec<u8>, JsError> {
    let (ed, x) = identity.secret_bytes();
    let mut plaintext = Vec::with_capacity(SECRET_LEN);
    plaintext.extend_from_slice(&ed);
    plaintext.extend_from_slice(&x);
    let salt = generate_salt();
    let key = EncryptionKey::from_password(password, &salt).map_err(to_js)?;
    let (ciphertext, nonce) = encrypt_data(&plaintext, &key).map_err(to_js)?;
    plaintext.zeroize();
    bincode::serialize(&KeystoreBlob {
        salt,
        nonce,
        ciphertext,
    })
    .map_err(to_js)
}

/// Password-seal arbitrary bytes (PBKDF2 + AES-256-GCM), framed like the keystore blob.
fn seal_bytes(plaintext: &[u8], password: &str) -> Result<Vec<u8>, JsError> {
    let salt = generate_salt();
    let key = EncryptionKey::from_password(password, &salt).map_err(to_js)?;
    let (ciphertext, nonce) = encrypt_data(plaintext, &key).map_err(to_js)?;
    bincode::serialize(&KeystoreBlob {
        salt,
        nonce,
        ciphertext,
    })
    .map_err(to_js)
}

/// Inverse of `seal_bytes`.
fn open_bytes(blob: &[u8], password: &str) -> Result<Vec<u8>, JsError> {
    let blob: KeystoreBlob = bincode::deserialize(blob).map_err(to_js)?;
    let key = EncryptionKey::from_password(password, &blob.salt).map_err(to_js)?;
    decrypt_data(&blob.ciphertext, &blob.nonce, &key).map_err(to_js)
}

/// Generate a fresh device identity, seal it under `password` (PBKDF2 + AES-256-GCM), and return
/// the encrypted blob to persist (e.g. in IndexedDB). Plaintext keys never leave wasm.
#[wasm_bindgen]
pub fn create_identity_keystore(password: &str) -> Result<Vec<u8>, JsError> {
    seal(&DeviceIdentity::generate(), password)
}

/// Decrypt a keystore blob with `password` to the device identity.
fn open_keystore(blob: &[u8], password: &str) -> Result<DeviceIdentity, JsError> {
    let blob: KeystoreBlob = bincode::deserialize(blob).map_err(to_js)?;
    let key = EncryptionKey::from_password(password, &blob.salt).map_err(to_js)?;
    let mut plaintext = decrypt_data(&blob.ciphertext, &blob.nonce, &key).map_err(to_js)?;
    if plaintext.len() != SECRET_LEN {
        return Err(JsError::new("keystore plaintext has the wrong length"));
    }
    let mut ed = [0u8; 32];
    let mut x = [0u8; 32];
    ed.copy_from_slice(&plaintext[..32]);
    x.copy_from_slice(&plaintext[32..]);
    plaintext.zeroize();
    let identity = DeviceIdentity::from_secret_bytes(ed, x);
    ed.zeroize();
    x.zeroize();
    Ok(identity)
}

/// Open a keystore blob with `password`, returning the device fingerprint (user id). Errors if
/// the password is wrong or the blob is corrupt/tampered (AEAD rejects it).
#[wasm_bindgen]
pub fn open_identity_keystore(blob: &[u8], password: &str) -> Result<String, JsError> {
    Ok(open_keystore(blob, password)?.public().user_id())
}
