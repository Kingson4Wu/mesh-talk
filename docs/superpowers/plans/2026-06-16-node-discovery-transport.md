# Node Discovery + Transport Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `discovery` module (signed announces → a live roster of `user_id → keys + addr`) and the node's transport wiring (dial/accept producing mutually-authenticated `SecureChannel`s over TCP) — the first half of the "online direct DM" vertical slice.

**Architecture:** A new `discovery` module: `Announce` is an Ed25519-signed UDP datagram carrying a peer's identity keys + TCP port; `Roster` verifies announces and maps `user_id → PeerRecord`; a `service` provides the broadcast and listen loops. A new `node::transport` provides `dial`/`accept` that run the Plan-2 Noise handshake over real `TcpStream`s and return authenticated channels. Pure logic (sign/verify, roster) is unit-tested; the async loops are integration-tested over localhost sockets.

**Tech Stack:** Rust; the existing `identity` (Ed25519 sign/verify) and `transport` (`SecureChannel`, Plan 2) modules; `tokio` (UDP/TCP, already a dependency, `features = ["full"]`); `serde`/`bincode`. No new dependencies.

---

## Background the implementer needs

**This is Plan 1 of 2** for the "online direct DM" slice (spec: `docs/superpowers/specs/2026-06-16-node-integration-online-dm-design.md`). It delivers discovery + the authenticated-channel wiring. Plan 2 adds the sync-over-channel driver, conversation id, the `node` App context, and the `mesh-talk-node` CLI. **Do not** build messaging, sync, or the CLI here.

**APIs you build on (already implemented):**
```rust
// identity/device.rs
pub struct PublicIdentity { pub ed25519_pub: [u8;32], pub x25519_pub: [u8;32] } // Clone, PartialEq, Eq
impl PublicIdentity { pub fn user_id(&self) -> String; pub fn user_id_from(ed25519_pub: &[u8;32]) -> String; }
impl DeviceIdentity {
    pub fn generate() -> Self;
    pub fn public(&self) -> PublicIdentity;
    pub fn sign(&self, message: &[u8]) -> [u8;64];
    pub fn verify(ed25519_pub: &[u8;32], message: &[u8], signature: &[u8;64]) -> bool; // associated fn
}
// transport/ (Plan 2)
pub struct SecureChannel<S>; // S: AsyncRead + AsyncWrite + Unpin
impl<S> SecureChannel<S> {
    pub async fn connect(stream: S, identity: &DeviceIdentity, expected_peer: Option<&PublicIdentity>) -> Result<Self, TransportError>;
    pub async fn accept(stream: S, identity: &DeviceIdentity) -> Result<Self, TransportError>;
    pub fn peer_identity(&self) -> &PublicIdentity;
}
pub enum TransportError { /* ... has From<std::io::Error> */ }
```

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare full `cargo test`/`cargo build`. Always `nice` + throttle, scoped:
```bash
cd src-tauri && nice -n 10 cargo test --lib discovery:: -- --test-threads=2     # discovery tasks
cd src-tauri && nice -n 10 cargo test --lib node:: -- --test-threads=2          # transport task
nice -n 10 cargo fmt
nice -n 10 cargo clippy --lib -- -D warnings
```
The pre-commit hook runs the full health check on commit — let it run. The async tests use real localhost sockets (UDP loopback / TCP) and complete in milliseconds; keep `--test-threads=2`.

**Conventions:** match `eventlog`/`transport`/`dm` — hand-written types, `#[cfg(test)] mod tests`, domain-separated signatures. rustfmt's `reorder_modules` sorts `pub mod` lines — keep them alphabetical.

---

### Task 1: `discovery::announce` — signed announce datagram

**Files:**
- Create: `src-tauri/src/discovery/mod.rs`
- Create: `src-tauri/src/discovery/announce.rs`
- Modify: `src-tauri/src/lib.rs` (module declaration)

- [ ] **Step 1: Register the module in `lib.rs`**

In `src-tauri/src/lib.rs`, add `pub mod discovery;` in alphabetical position — **between** `pub mod crypto;` and `pub mod dm;** (`discovery` sorts before `dm`: `di` < `dm`):
```rust
pub mod crypto;
pub mod discovery;
pub mod dm;
```

- [ ] **Step 2: Create `discovery/mod.rs` with only the `announce` module**

Create `src-tauri/src/discovery/mod.rs` declaring ONLY `announce` (Tasks 2 and 3 add `pub mod roster;` / `pub mod service;` and their re-exports incrementally — this keeps the crate compiling after each task, with no placeholder files):
```rust
//! LAN peer discovery: nodes broadcast Ed25519-signed [`announce::Announce`]
//! datagrams carrying their identity keys + TCP port; receivers verify them and
//! build a roster mapping `user_id` → keys + address. Replaces the legacy
//! plaintext UDP discovery (which carried no keys).

pub mod announce;

pub use announce::Announce;
```

- [ ] **Step 3: Create `discovery/announce.rs` with the type, sign/verify, wire codec, and tests**

```rust
//! The signed discovery announce and its UDP wire format.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use serde::{Deserialize, Serialize};

/// Domain separator for announce signatures.
const ANNOUNCE_DOMAIN: &[u8] = b"mesh-talk-announce-v1";
/// Wire framing: 4-byte magic + 1-byte version, then `bincode(Announce)`.
const MAGIC: &[u8; 4] = b"MTAN";
const VERSION: u8 = 1;

/// A peer's self-announcement: its identity keys, display name, and TCP listen
/// port, signed by its Ed25519 key. `user_id` is the fingerprint of `ed25519_pub`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announce {
    pub user_id: String,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    pub name: String,
    pub tcp_port: u16,
    pub sig: Vec<u8>,
}

/// Length-prefixed, domain-separated bytes the announce signs over (everything
/// except `sig`). Length prefixes make the concatenation unambiguous.
fn signing_input(
    user_id: &str,
    ed25519_pub: &[u8; 32],
    x25519_pub: &[u8; 32],
    name: &str,
    tcp_port: u16,
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(ANNOUNCE_DOMAIN);
    v.extend_from_slice(&(user_id.len() as u32).to_be_bytes());
    v.extend_from_slice(user_id.as_bytes());
    v.extend_from_slice(ed25519_pub);
    v.extend_from_slice(x25519_pub);
    v.extend_from_slice(&(name.len() as u32).to_be_bytes());
    v.extend_from_slice(name.as_bytes());
    v.extend_from_slice(&tcp_port.to_be_bytes());
    v
}

impl Announce {
    /// Build and sign an announce for `identity`.
    pub fn new(identity: &DeviceIdentity, name: impl Into<String>, tcp_port: u16) -> Self {
        let public = identity.public();
        let user_id = public.user_id();
        let name = name.into();
        let sig = identity
            .sign(&signing_input(
                &user_id,
                &public.ed25519_pub,
                &public.x25519_pub,
                &name,
                tcp_port,
            ))
            .to_vec();
        Announce {
            user_id,
            ed25519_pub: public.ed25519_pub,
            x25519_pub: public.x25519_pub,
            name,
            tcp_port,
            sig,
        }
    }

    /// True if the announce is internally consistent and authentically signed:
    /// `user_id` is the fingerprint of `ed25519_pub`, and `sig` verifies.
    pub fn verify(&self) -> bool {
        if self.user_id != PublicIdentity::user_id_from(&self.ed25519_pub) {
            return false;
        }
        let Ok(sig): Result<[u8; 64], _> = self.sig.as_slice().try_into() else {
            return false;
        };
        DeviceIdentity::verify(
            &self.ed25519_pub,
            &signing_input(
                &self.user_id,
                &self.ed25519_pub,
                &self.x25519_pub,
                &self.name,
                self.tcp_port,
            ),
            &sig,
        )
    }

    /// The advertised public identity (Ed25519 + X25519 keys).
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            ed25519_pub: self.ed25519_pub,
            x25519_pub: self.x25519_pub,
        }
    }
}

/// Frame an announce for the wire (magic + version + bincode).
pub fn encode(announce: &Announce) -> Vec<u8> {
    let body = bincode::serialize(announce).expect("announce serializes");
    let mut out = Vec::with_capacity(MAGIC.len() + 1 + body.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&body);
    out
}

/// Parse a wire datagram into an announce, or `None` if the framing/bincode is
/// invalid. The result is NOT yet authenticated — call [`Announce::verify`].
pub fn decode(data: &[u8]) -> Option<Announce> {
    if data.len() < MAGIC.len() + 1 || &data[..MAGIC.len()] != MAGIC || data[MAGIC.len()] != VERSION
    {
        return None;
    }
    bincode::deserialize(&data[MAGIC.len() + 1..]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_announce_verifies() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "Alice", 4000);
        assert!(a.verify());
        assert_eq!(a.user_id, id.public().user_id());
        assert_eq!(a.public(), id.public());
    }

    #[test]
    fn tampered_signature_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.sig[0] ^= 0xFF;
        assert!(!a.verify());
    }

    #[test]
    fn mismatched_user_id_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.user_id = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert!(!a.verify());
    }

    #[test]
    fn tampered_field_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.tcp_port = 9999; // not what was signed
        assert!(!a.verify());
    }

    #[test]
    fn encode_decode_round_trips() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "Alice", 4000);
        let bytes = encode(&a);
        let back = decode(&bytes).expect("decodes");
        assert_eq!(back, a);
        assert!(back.verify());
    }

    #[test]
    fn decode_rejects_bad_framing() {
        assert!(decode(&[]).is_none());
        assert!(decode(b"XXXX\x01garbage").is_none()); // bad magic
        assert!(decode(b"MTAN\x02garbage").is_none()); // bad version
    }
}
```

- [ ] **Step 4: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib discovery::announce -- --test-threads=2
```
Expected: PASS — 6 tests.

- [ ] **Step 5: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/lib.rs src-tauri/src/discovery/mod.rs src-tauri/src/discovery/announce.rs
git commit -m "feat(discovery): signed announce datagram + wire codec

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: `discovery::roster` — the peer roster

**Files:**
- Create: `src-tauri/src/discovery/roster.rs`
- Modify: `src-tauri/src/discovery/mod.rs` (add module + re-exports)

- [ ] **Step 1: Add the module + re-exports to `mod.rs`**

In `src-tauri/src/discovery/mod.rs`, add `pub mod roster;` (sorted after `announce`) and the re-export:
```rust
pub mod announce;
pub mod roster;

pub use announce::Announce;
pub use roster::{PeerRecord, Roster, UserId};
```

- [ ] **Step 2: Create `discovery/roster.rs` with the roster + tests**

```rust
//! The roster: the set of currently-known peers, keyed by `user_id`.

use crate::discovery::announce::Announce;
use crate::identity::device::PublicIdentity;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

/// A peer's stable fingerprint id.
pub type UserId = String;

/// What we know about a discovered peer: its keys, where to reach it, and when
/// we last heard from it.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub public: PublicIdentity,
    pub addr: SocketAddr,
    pub name: String,
    pub last_seen: Instant,
}

/// The known-peers map.
#[derive(Default)]
pub struct Roster {
    peers: HashMap<UserId, PeerRecord>,
}

impl Roster {
    /// Verify and record an announce received from `source_ip`. Returns `true`
    /// if it was accepted (authentic and not our own `self_user_id`). The peer's
    /// address is `(source_ip, announce.tcp_port)` — the announce names the TCP
    /// port; the IP comes from the datagram source.
    pub fn update(&mut self, announce: &Announce, source_ip: IpAddr, self_user_id: &str) -> bool {
        if announce.user_id == self_user_id {
            return false; // self-filter
        }
        if !announce.verify() {
            return false;
        }
        self.peers.insert(
            announce.user_id.clone(),
            PeerRecord {
                public: announce.public(),
                addr: SocketAddr::new(source_ip, announce.tcp_port),
                name: announce.name.clone(),
                last_seen: Instant::now(),
            },
        );
        true
    }

    pub fn get(&self, user_id: &str) -> Option<&PeerRecord> {
        self.peers.get(user_id)
    }

    pub fn peers(&self) -> Vec<PeerRecord> {
        self.peers.values().cloned().collect()
    }

    /// Drop peers not seen within `ttl`.
    pub fn evict_stale(&mut self, ttl: Duration) {
        let now = Instant::now();
        self.peers.retain(|_, r| now.duration_since(r.last_seen) < ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;
    use std::net::Ipv4Addr;

    fn ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50))
    }

    #[test]
    fn records_a_valid_announce_with_source_ip_and_port() {
        let alice = DeviceIdentity::generate();
        let announce = Announce::new(&alice, "Alice", 4000);
        let mut roster = Roster::default();

        assert!(roster.update(&announce, ip(), "some-other-self"));
        let rec = roster.get(&alice.public().user_id()).expect("alice recorded");
        assert_eq!(rec.addr, SocketAddr::new(ip(), 4000));
        assert_eq!(rec.name, "Alice");
        assert_eq!(rec.public, alice.public());
    }

    #[test]
    fn self_announce_is_filtered_out() {
        let me = DeviceIdentity::generate();
        let announce = Announce::new(&me, "Me", 4000);
        let mut roster = Roster::default();
        // self_user_id matches the announce → rejected.
        assert!(!roster.update(&announce, ip(), &me.public().user_id()));
        assert!(roster.get(&me.public().user_id()).is_none());
    }

    #[test]
    fn forged_announce_is_rejected() {
        let alice = DeviceIdentity::generate();
        let mut announce = Announce::new(&alice, "Alice", 4000);
        announce.sig[0] ^= 0xFF; // break the signature
        let mut roster = Roster::default();
        assert!(!roster.update(&announce, ip(), "self"));
        assert!(roster.get(&alice.public().user_id()).is_none());
    }

    #[test]
    fn peers_lists_all_records() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");
        roster.update(&Announce::new(&bob, "Bob", 4001), ip(), "self");
        assert_eq!(roster.peers().len(), 2);
    }

    #[test]
    fn evict_stale_drops_old_records() {
        let alice = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");

        // A generous TTL keeps the just-added record; a zero TTL drops it.
        roster.evict_stale(Duration::from_secs(3600));
        assert!(roster.get(&alice.public().user_id()).is_some());
        roster.evict_stale(Duration::ZERO);
        assert!(roster.get(&alice.public().user_id()).is_none());
    }
}
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib discovery::roster -- --test-threads=2
```
Expected: PASS — 5 tests.

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/discovery/mod.rs src-tauri/src/discovery/roster.rs
git commit -m "feat(discovery): roster of verified peers

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `discovery::service` — broadcast + listen loops

**Files:**
- Create: `src-tauri/src/discovery/service.rs`
- Modify: `src-tauri/src/discovery/mod.rs` (add module)

- [ ] **Step 1: Add the module declaration to `mod.rs`**

In `src-tauri/src/discovery/mod.rs`, add `pub mod service;` (sorted: `announce`, `roster`, `service`):
```rust
pub mod announce;
pub mod roster;
pub mod service;

pub use announce::Announce;
pub use roster::{PeerRecord, Roster, UserId};
```

- [ ] **Step 2: Create `discovery/service.rs` with the datagram handler, the loops, and tests**

```rust
//! The discovery service: a broadcast loop (advertise our announce) and a listen
//! loop (record peers). The datagram handler is pure so it can be unit-tested;
//! the loops are thin wrappers over a `tokio` UDP socket.

use crate::discovery::announce::{decode, encode, Announce};
use crate::discovery::roster::Roster;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UdpSocket;

/// Decode + verify + record one received datagram. Returns `true` if it updated
/// the roster. Pure (apart from locking the roster) — no I/O.
pub fn handle_datagram(
    roster: &Mutex<Roster>,
    data: &[u8],
    source: SocketAddr,
    self_user_id: &str,
) -> bool {
    let Some(announce) = decode(data) else {
        return false;
    };
    roster
        .lock()
        .expect("roster mutex not poisoned")
        .update(&announce, source.ip(), self_user_id)
}

/// Listen for announces on `socket`, feeding the roster, until the socket errors
/// (e.g. the task is aborted on shutdown).
pub async fn run_listen(socket: Arc<UdpSocket>, roster: Arc<Mutex<Roster>>, self_user_id: String) {
    let mut buf = vec![0u8; 2048];
    loop {
        match socket.recv_from(&mut buf).await {
            Ok((n, source)) => {
                handle_datagram(&roster, &buf[..n], source, &self_user_id);
            }
            Err(_) => break,
        }
    }
}

/// Periodically send our `announce` to `target` (the broadcast address in
/// production) over `socket`, every `interval`. `socket` must already be
/// configured for the target (e.g. broadcast-enabled if `target` is a broadcast
/// address); this function only sends.
pub async fn run_broadcast(
    socket: Arc<UdpSocket>,
    announce: Announce,
    target: SocketAddr,
    interval: Duration,
) {
    let bytes = encode(&announce);
    let mut tick = tokio::time::interval(interval);
    loop {
        tick.tick().await;
        if socket.send_to(&bytes, target).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;
    use std::net::{IpAddr, Ipv4Addr};

    fn source() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345)
    }

    #[test]
    fn handle_datagram_records_a_valid_announce() {
        let alice = DeviceIdentity::generate();
        let roster = Mutex::new(Roster::default());
        let bytes = encode(&Announce::new(&alice, "Alice", 4000));
        assert!(handle_datagram(&roster, &bytes, source(), "self"));
        assert!(roster.lock().unwrap().get(&alice.public().user_id()).is_some());
    }

    #[test]
    fn handle_datagram_rejects_garbage() {
        let roster = Mutex::new(Roster::default());
        assert!(!handle_datagram(&roster, b"not a datagram", source(), "self"));
    }

    #[tokio::test]
    async fn listen_loop_records_a_received_announce() {
        let alice = DeviceIdentity::generate();
        let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listen_addr = listener.local_addr().unwrap();
        let roster = Arc::new(Mutex::new(Roster::default()));
        tokio::spawn(run_listen(listener.clone(), roster.clone(), "self".to_string()));

        // Send one announce (unicast to the listener, stands in for broadcast).
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let announce = Announce::new(&alice, "Alice", 4000);
        sender.send_to(&encode(&announce), listen_addr).await.unwrap();

        // Poll (bounded) until the roster reflects the announce.
        let mut recorded = false;
        for _ in 0..100 {
            if roster.lock().unwrap().get(&alice.public().user_id()).is_some() {
                recorded = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(recorded, "listener did not record the announce");
        let r = roster.lock().unwrap();
        assert_eq!(r.get(&alice.public().user_id()).unwrap().addr.port(), 4000);
    }

    #[tokio::test]
    async fn broadcast_loop_sends_decodable_announces() {
        let alice = DeviceIdentity::generate();
        let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let recv_addr = receiver.local_addr().unwrap();
        let sender = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let announce = Announce::new(&alice, "Alice", 4000);

        // Broadcast "to" the receiver (a unicast addr stands in for the broadcast addr).
        tokio::spawn(run_broadcast(
            sender,
            announce.clone(),
            recv_addr,
            Duration::from_millis(10),
        ));

        let mut buf = vec![0u8; 2048];
        let (n, _src) = tokio::time::timeout(Duration::from_secs(2), receiver.recv_from(&mut buf))
            .await
            .expect("received an announce within 2s")
            .unwrap();
        let decoded = decode(&buf[..n]).expect("decodes");
        assert_eq!(decoded, announce);
        assert!(decoded.verify());
    }
}
```

- [ ] **Step 3: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib discovery::service -- --test-threads=2
```
Expected: PASS — 4 tests.

- [ ] **Step 4: fmt + clippy clean, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/discovery/mod.rs src-tauri/src/discovery/service.rs
git commit -m "feat(discovery): broadcast + listen loops

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: `node::transport` — authenticated SecureChannel over TCP

**Files:**
- Create: `src-tauri/src/node/mod.rs`
- Create: `src-tauri/src/node/transport.rs`
- Modify: `src-tauri/src/lib.rs` (module declaration)

- [ ] **Step 1: Register the module in `lib.rs`**

In `src-tauri/src/lib.rs`, add `pub mod node;` in alphabetical position — **between** `pub mod network;` and `pub mod notifications;** (`node` sorts after `network`, before `notifications`):
```rust
pub mod network;
pub mod node;
pub mod notifications;
```

- [ ] **Step 2: Create `node/mod.rs`**

```rust
//! The redesign node: the explicit App context that wires identity, discovery,
//! transport, the event log, and DM crypto into a runnable peer. This plan adds
//! only [`transport`] (authenticated channel dial/accept); the conversation
//! layer, sync driver, and node API arrive in the next plan.

pub mod transport;
```

- [ ] **Step 3: Create `node/transport.rs` with `dial`/`accept` and tests**

```rust
//! Establishing authenticated, encrypted channels over TCP. `dial` connects to a
//! peer and runs the Noise handshake as initiator (optionally pinning the
//! expected peer); `accept` takes one inbound connection and authenticates it.
//! Both return a [`SecureChannel`] keyed by the cryptographically-verified peer.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::transport::{SecureChannel, TransportError};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};

/// Dial `addr`, perform the Noise XX handshake + identity auth as the initiator.
/// If `expected_peer` is `Some`, the authenticated identity must match it
/// (`TransportError::UnexpectedPeer` otherwise).
pub async fn dial(
    addr: SocketAddr,
    identity: &DeviceIdentity,
    expected_peer: Option<&PublicIdentity>,
) -> Result<SecureChannel<TcpStream>, TransportError> {
    let stream = TcpStream::connect(addr).await?;
    SecureChannel::connect(stream, identity, expected_peer).await
}

/// Accept one inbound connection on `listener` and authenticate the peer.
pub async fn accept(
    listener: &TcpListener,
    identity: &DeviceIdentity,
) -> Result<SecureChannel<TcpStream>, TransportError> {
    let (stream, _addr) = listener.accept().await?;
    SecureChannel::accept(stream, identity).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dial_and_accept_mutually_authenticate_over_tcp() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let (a_pub, b_pub) = (a.public(), b.public());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Responder side: accept one connection as `b`, confirm it is `a`.
        let server = tokio::spawn(async move {
            let ch = accept(&listener, &b).await.expect("accept");
            assert_eq!(ch.peer_identity(), &a_pub);
        });

        // Initiator side: dial as `a`, pinning `b` as the expected peer.
        let ch = dial(addr, &a, Some(&b_pub)).await.expect("dial");
        assert_eq!(ch.peer_identity(), &b_pub);

        server.await.expect("server task");
    }

    #[tokio::test]
    async fn dial_rejects_an_unexpected_peer() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let wrong = DeviceIdentity::generate().public();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Responder completes its side normally.
        let server = tokio::spawn(async move {
            let _ = accept(&listener, &b).await;
        });

        // Initiator expects `wrong` but actually reaches `b` → UnexpectedPeer.
        let result = dial(addr, &a, Some(&wrong)).await;
        assert!(matches!(result, Err(TransportError::UnexpectedPeer)));
        let _ = server.await;
    }
}
```

- [ ] **Step 4: Run the tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::transport -- --test-threads=2
```
Expected: PASS — 2 tests.

- [ ] **Step 5: Full fmt + clippy gate**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt --check; echo "fmt EXIT: $?"
```
Expected: clippy clean; `fmt EXIT: 0`.

- [ ] **Step 6: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/lib.rs src-tauri/src/node/mod.rs src-tauri/src/node/transport.rs
git commit -m "feat(node): authenticated SecureChannel dial/accept over TCP

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next plan

- **What this delivers:** the discovery half (signed `Announce` + wire codec, a verifying `Roster`, the broadcast/listen loops) and the transport-wiring half (`node::transport::dial`/`accept` → mutually-authenticated `SecureChannel<TcpStream>`). A node can now find peers and open authenticated channels to them.
- **Deliberately deferred to Plan 2 of this slice:** `dm_conversation_id`, the networked sync driver (Plan-4 handlers over a `SecureChannel`), the `node::Node` App context (`send_dm`/`incoming`), and the `mesh-talk-node` CLI + two-CLI integration rig. Also deferred (later slices): the post office / offline delivery, a persistent message log, channels.
- **Security properties:** announces are Ed25519-signed and `user_id`-bound (a forged or key-mismatched announce fails `verify` and never enters the roster); the roster self-filters our own announce; `dial`/`accept` produce only cryptographically-authenticated channels (`peer_identity()` is trustworthy, and `dial` can pin the expected peer).
- **Test strategy:** pure logic (sign/verify, roster, datagram handler) is unit-tested; the async loops and TCP wiring are integration-tested over localhost sockets (UDP unicast stands in for broadcast to stay deterministic in CI; TCP uses ephemeral ports). No broadcast permissions or fixed ports are required.
- **Accepted limitations:** `run_broadcast` assumes a socket already configured for its target (the broadcast-enabled UDP socket setup is node-startup wiring in Plan 2). (`decode` was hardened during review to reject trailing bytes — fail-closed, matching the `dm` envelope parse.)
