# Post-Office Role + Serving Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a post office a first-class, discoverable role: a signed `post_office` flag in the announce, roster/election plumbing to find the elected PO, and a `--post-office` mode of `mesh-talk-node` that runs the durable `PostOffice` relay and advertises itself — so other nodes can discover it and elect it (replication + drain are Plan B).

**Architecture:** `discovery::announce` gains a signed `post_office: bool` (covered by the existing Ed25519 signature, so the role is unforgeable); `discovery::roster` surfaces it (`PeerRecord.post_office`, `Roster::post_offices`); a new `node::postbox` module adds `elected_post_office` (the existing lowest-fingerprint `elect` over PO-flagged peers) and a generic relay serve loop (`serve_one` over a `Mutex<PostOffice>`, which is already a `SyncStore`); the binary gains a `--post-office` branch that builds a `PostOffice`, announces the role, and runs the relay accept loop.

**Tech Stack:** Rust; the Phase-0 modules (`identity`, `transport`, `eventlog` store+sync, `postoffice`), the redesign `discovery` + `node` (incl. `node::session`/`node::transport`); `tokio`; `bincode`/`serde`; `clap`. No new dependencies.

---

## Background the implementer needs

**This is Plan A (of 2) of the "offline post-office delivery" slice** (spec: `docs/superpowers/specs/2026-06-16-offline-post-office-delivery-design.md`). Plans 1–3 of the *online* slice built `discovery`, `node` (`Node`, `send_dm`, `run_accept_loop`, `node::session`, `node::transport`), and the `mesh-talk-node` CLI. Phase 0 built the `PostOffice` primitive. **This plan makes the PO discoverable and runnable; Plan B adds sender replication + the recipient drain loop + the 3-process offline rig.**

**Exact verified APIs you build on:**
```rust
// postoffice (src-tauri/src/postoffice/) — re-exports: elect, is_post_office
crate::postoffice::PostOffice::open(path: &std::path::Path, password: &str, identity: DeviceIdentity) -> Result<PostOffice, crate::eventlog::LogError>;
impl PostOffice { pub fn public(&self) -> PublicIdentity; pub fn has(&self, &EventId) -> bool; }
impl SyncStore for PostOffice { /* event_ids / events_excluding / ingest — so request_round/serve_one work over it */ }
crate::postoffice::elect(eligible: &[PublicIdentity]) -> Option<PublicIdentity>; // lowest user_id()

// identity (src-tauri/src/identity/device.rs)
impl DeviceIdentity { pub fn generate() -> Self; pub fn public(&self) -> PublicIdentity;
    pub fn secret_bytes(&self) -> ([u8;32],[u8;32]); pub fn from_secret_bytes(ed:[u8;32], x:[u8;32]) -> Self; }
pub struct PublicIdentity { pub ed25519_pub: [u8;32], pub x25519_pub: [u8;32] } // Clone, PartialEq, Eq; user_id()

// node::session (generic over any SyncStore)
crate::node::session::serve_one<S: SyncStore, IO: AsyncRead+AsyncWrite+Unpin>(&mut SecureChannel<IO>, &Mutex<S>) -> Result<Served, SessionError>;
crate::node::session::request_round<S: SyncStore, IO>(&mut SecureChannel<IO>, &Mutex<S>, ConversationId) -> Result<ApplyReport, SessionError>;
pub enum Served { Handled(ConversationId), Closed }
// node::transport (Plan 1 of online slice)
crate::node::transport::accept(listener: &tokio::net::TcpListener, identity: &DeviceIdentity) -> Result<SecureChannel<TcpStream>, TransportError>;
// transport
crate::transport::SecureChannel; // connect/accept/send/recv

// discovery (current)
crate::discovery::service::{run_listen, run_broadcast}; crate::discovery::{Announce, PeerRecord, Roster, UserId};
```

**`Announce` internals you are modifying** (`src-tauri/src/discovery/announce.rs`): the struct has `user_id, ed25519_pub, x25519_pub, name, tcp_port, sig`. A private `signing_input(user_id, ed25519_pub, x25519_pub, name, tcp_port) -> Vec<u8>` builds the length-prefixed, domain-separated bytes that `new` signs and `verify` checks. You will thread a `post_office: bool` through both.

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare `cargo test`/`build`. Always `nice` + throttle + scope:
```bash
cd src-tauri && nice -n 10 cargo test --lib discovery:: -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --lib node::postbox -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
The pre-commit hook runs the full health check on commit — let it run.

**Conventions:** hand-written errors (no `thiserror`); `#[cfg(test)] mod tests`; fail-closed bincode (already in `announce::decode`); rustfmt sorts `pub mod`/`pub use` alphabetically — keep them sorted. Locks are `std::sync::Mutex`; never hold a guard across `.await`.

---

### Task 1: Signed `post_office` field on `Announce`

**Files:**
- Modify: `src-tauri/src/discovery/announce.rs`

- [ ] **Step 1: Add the field and thread it through signing (write the change)**

Replace the `Announce` struct, `signing_input`, and the `impl Announce` block (`new`/`verify`) with the versions below. The new bool is a signed field (appended to `signing_input`), so a flipped role fails `verify`. `new` keeps its exact signature (`post_office = false`); a new `new_post_office` sets it `true`; both delegate to a private `new_with_role`.

```rust
/// A peer's self-announcement: its identity keys, display name, TCP listen port,
/// and whether it serves as a post office, signed by its Ed25519 key. `user_id`
/// is the fingerprint of `ed25519_pub`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announce {
    pub user_id: String,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    pub name: String,
    pub tcp_port: u16,
    pub post_office: bool,
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
    post_office: bool,
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
    v.push(post_office as u8);
    v
}

impl Announce {
    /// Build and sign a normal (non-post-office) announce for `identity`.
    pub fn new(identity: &DeviceIdentity, name: impl Into<String>, tcp_port: u16) -> Self {
        Self::new_with_role(identity, name, tcp_port, false)
    }

    /// Build and sign an announce that advertises the post-office role.
    pub fn new_post_office(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Self {
        Self::new_with_role(identity, name, tcp_port, true)
    }

    fn new_with_role(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
        post_office: bool,
    ) -> Self {
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
                post_office,
            ))
            .to_vec();
        Announce {
            user_id,
            ed25519_pub: public.ed25519_pub,
            x25519_pub: public.x25519_pub,
            name,
            tcp_port,
            post_office,
            sig,
        }
    }

    /// True if the announce is internally consistent and authentically signed:
    /// `user_id` is the fingerprint of `ed25519_pub`, and `sig` verifies (over all
    /// fields including `post_office`).
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
                self.post_office,
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
```

- [ ] **Step 2: Fix the one struct-literal test for the new field**

In the `#[cfg(test)] mod tests` block, the `announce_from_a_does_not_verify_under_b` test builds an `Announce { .. }` literal. Add `post_office: a_announce.post_office,` to it (just before `sig:`), so it still compiles:
```rust
        let spoofed = Announce {
            user_id: b.public().user_id(),
            ed25519_pub: b.public().ed25519_pub,
            x25519_pub: a_announce.x25519_pub,
            name: a_announce.name.clone(),
            tcp_port: a_announce.tcp_port,
            post_office: a_announce.post_office,
            sig: a_announce.sig.clone(),
        };
```

- [ ] **Step 3: Add tests for the new field**

Append these two tests inside the `mod tests` block:
```rust
    #[test]
    fn post_office_role_round_trips_and_is_signed() {
        let id = DeviceIdentity::generate();
        let normal = Announce::new(&id, "Node", 4000);
        assert!(!normal.post_office);
        assert!(normal.verify());

        let po = Announce::new_post_office(&id, "Relay", 4000);
        assert!(po.post_office);
        assert!(po.verify());

        // The role survives the wire round-trip.
        let back = decode(&encode(&po)).expect("decodes");
        assert!(back.post_office);
        assert!(back.verify());
    }

    #[test]
    fn flipping_the_post_office_bit_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Node", 4000);
        a.post_office = true; // not what was signed
        assert!(!a.verify());
    }
```

- [ ] **Step 4: Run the discovery tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib discovery::announce -- --test-threads=2
```
Expected: PASS — all existing announce tests plus the 2 new ones.

- [ ] **Step 5: fmt + clippy, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/discovery/announce.rs
git commit -m "feat(discovery): signed post_office role flag on Announce

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Surface the role on the roster

**Files:**
- Modify: `src-tauri/src/discovery/roster.rs`
- Modify: `src-tauri/src/bin/mesh-talk-node.rs` (one test-helper struct literal)

- [ ] **Step 1: Add `post_office` to `PeerRecord` and populate it; add `post_offices()`**

In `src-tauri/src/discovery/roster.rs`, add the field to `PeerRecord` (just before `last_seen`), set it in `update`, and add the accessor. The changed pieces:

```rust
/// What we know about a discovered peer: its keys, where to reach it, whether it
/// serves as a post office, and when we last heard from it.
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub public: PublicIdentity,
    pub addr: SocketAddr,
    pub name: String,
    pub post_office: bool,
    pub last_seen: Instant,
}
```

In `update`, set the field from the verified announce:
```rust
        self.peers.insert(
            announce.user_id.clone(),
            PeerRecord {
                public: announce.public(),
                addr: SocketAddr::new(source_ip, announce.tcp_port),
                name: announce.name.clone(),
                post_office: announce.post_office,
                last_seen: Instant::now(),
            },
        );
        true
```

Add this accessor inside `impl Roster` (after `peers`):
```rust
    /// The known peers that advertise the post-office role.
    pub fn post_offices(&self) -> Vec<PeerRecord> {
        self.peers
            .values()
            .filter(|r| r.post_office)
            .cloned()
            .collect()
    }
```

- [ ] **Step 2: Add a roster test**

Append inside `mod tests`:
```rust
    #[test]
    fn post_offices_lists_only_flagged_peers() {
        let alice = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");
        roster.update(&Announce::new_post_office(&relay, "Relay", 4001), ip(), "self");

        // Both peers are present; only the relay is a post office.
        assert_eq!(roster.peers().len(), 2);
        let pos = roster.post_offices();
        assert_eq!(pos.len(), 1);
        assert_eq!(pos[0].public, relay.public());
        assert!(pos[0].post_office);
        // The normal peer's record carries the flag too (false).
        assert!(!roster.get(&alice.public().user_id()).unwrap().post_office);
    }
```

- [ ] **Step 3: Fix the binary's `PeerRecord` test-helper literal**

`src-tauri/src/bin/mesh-talk-node.rs` builds a `PeerRecord { .. }` in its test helper `peer(name)`. Add `post_office: false,` (just before `last_seen`):
```rust
    fn peer(name: &str) -> PeerRecord {
        PeerRecord {
            public: DeviceIdentity::generate().public(),
            addr: "127.0.0.1:4000".parse::<SocketAddr>().unwrap(),
            name: name.to_string(),
            post_office: false,
            last_seen: Instant::now(),
        }
    }
```
(There are no other `PeerRecord { .. }` literals — confirm with `grep -rn "PeerRecord {" src-tauri/src` before building; the only hits should be `roster.rs` (the real constructor) and this test helper.)

- [ ] **Step 4: Build + run the affected tests**
```bash
cd src-tauri && grep -rn "PeerRecord {" src/   # expect only roster.rs + bin/mesh-talk-node.rs
cd src-tauri && nice -n 10 cargo test --lib discovery::roster -- --test-threads=2
cd src-tauri && nice -n 10 cargo test --bin mesh-talk-node -- --test-threads=2
```
Expected: roster tests pass (incl. the new one); the binary's 5 resolver tests still pass.

- [ ] **Step 5: fmt + clippy, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/discovery/roster.rs src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(discovery): expose post_office on PeerRecord; Roster::post_offices

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `node::postbox` — elected PO + relay serve loop

**Files:**
- Create: `src-tauri/src/node/postbox.rs`
- Modify: `src-tauri/src/node/mod.rs` (register module + re-export)

- [ ] **Step 1: Register the module in `node/mod.rs`**

Add `pub mod postbox;` (alphabetical: `conversation`, `node`, `postbox`, `session`, `transport`) and re-export the elected-PO helper. The module section becomes:
```rust
pub mod conversation;
pub mod node;
pub mod postbox;
pub mod session;
pub mod transport;

pub use node::{Node, NodeError, ReceivedDm};
pub use postbox::elected_post_office;
```

- [ ] **Step 2: Create `src-tauri/src/node/postbox.rs`**

```rust
//! Post-office integration: find the elected post office in the roster, and run
//! the relay's server side (sync rounds over a durable [`PostOffice`]). The
//! sender-replication and recipient-drain logic that USES the elected PO lands in
//! the next plan; this module provides the election lookup and the serving loop.

use crate::discovery::roster::{PeerRecord, Roster};
use crate::identity::device::DeviceIdentity;
use crate::node::session::{serve_one, Served};
use crate::node::transport::accept;
use crate::postoffice::{elect, PostOffice};
use crate::transport::SecureChannel;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;

/// The post office this node should use: the lowest-fingerprint peer among those
/// advertising the post-office role, or `None` if no post office is known.
/// Deterministic — every node computes the same winner from the same roster.
pub fn elected_post_office(roster: &Roster) -> Option<PeerRecord> {
    let candidates = roster.post_offices();
    let winner = elect(&candidates.iter().map(|r| r.public.clone()).collect::<Vec<_>>())?;
    candidates.into_iter().find(|r| r.public == winner)
}

/// Serve one authenticated inbound connection as the relay: run sync rounds over
/// the shared durable store until the peer disconnects. The PostOffice ingests
/// (validates + persists) pushed events and serves held events to pullers; it
/// never decrypts. Generic over the IO so it is testable over an in-memory duplex.
pub async fn serve_relay_connection<IO>(mut channel: SecureChannel<IO>, store: Arc<Mutex<PostOffice>>)
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        match serve_one(&mut channel, &store).await {
            Ok(Served::Handled(_)) => {}
            Ok(Served::Closed) | Err(_) => break,
        }
    }
}

/// Accept inbound connections on `listener` and serve each as a relay connection
/// on its own task, sharing the one durable store. A failed handshake backs off
/// briefly (so a persistent accept error can't busy-spin) and keeps accepting.
pub async fn run_relay_accept_loop(
    identity: DeviceIdentity,
    listener: TcpListener,
    store: Arc<Mutex<PostOffice>>,
) {
    loop {
        match accept(&listener, &identity).await {
            Ok(channel) => {
                let store = Arc::clone(&store);
                tokio::spawn(async move { serve_relay_connection(channel, store).await });
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::announce::Announce;
    use crate::eventlog::event::{ConversationId, Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::node::session::request_round;
    use std::net::{IpAddr, Ipv4Addr};

    fn po_roster(relays: &[(&DeviceIdentity, u16)], normals: &[&DeviceIdentity]) -> Roster {
        let mut roster = Roster::default();
        for (id, port) in relays {
            roster.update(
                &Announce::new_post_office(id, "Relay", *port),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                "self",
            );
        }
        for id in normals {
            roster.update(
                &Announce::new(id, "Node", 4000),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                "self",
            );
        }
        roster
    }

    #[test]
    fn elected_post_office_is_none_without_a_po() {
        let alice = DeviceIdentity::generate();
        let roster = po_roster(&[], &[&alice]);
        assert!(elected_post_office(&roster).is_none());
    }

    #[test]
    fn elected_post_office_picks_the_lowest_fingerprint_po() {
        // Generate three PO identities; the winner is the lowest user_id().
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let c = DeviceIdentity::generate();
        let normal = DeviceIdentity::generate();
        let roster = po_roster(&[(&a, 5001), (&b, 5002), (&c, 5003)], &[&normal]);

        let mut ids = [a.public(), b.public(), c.public()];
        ids.sort_by_key(|p| p.user_id());
        let expected_lowest = ids[0].clone();

        let elected = elected_post_office(&roster).expect("a PO is elected");
        assert_eq!(elected.public, expected_lowest);
        assert!(elected.post_office);
    }

    #[tokio::test]
    async fn relay_ingests_an_event_pushed_over_a_channel() {
        // A client with one event pushes it to a relay (PostOffice) over a
        // SecureChannel on an in-memory duplex; the relay persists it.
        let dir = tempfile::tempdir().unwrap();
        let client_id = DeviceIdentity::generate();
        let relay_id = DeviceIdentity::generate();
        // The relay needs an identity copy: one for the PostOffice, one for Noise.
        let (ed, x) = relay_id.secret_bytes();
        let relay_transport_id = DeviceIdentity::from_secret_bytes(ed, x);

        let conv = ConversationId::new([7u8; 32]);
        let event = Event::new(&client_id, conv, 1, vec![], 1, 0, EventKind::Message, b"x".to_vec());
        let event_id = event.id;
        let client_store = Mutex::new({
            let mut log = EventLog::default();
            log.append(event.clone()).unwrap();
            log
        });

        let relay_store = Arc::new(Mutex::new(
            PostOffice::open(&dir.path().join("relay.log"), "pw", relay_id).unwrap(),
        ));

        let (c_io, r_io) = tokio::io::duplex(64 * 1024);
        let server_store = Arc::clone(&relay_store);
        let server = tokio::spawn(async move {
            let r_ch = SecureChannel::accept(r_io, &relay_transport_id).await.unwrap();
            serve_relay_connection(r_ch, server_store).await;
        });

        let mut c_ch = SecureChannel::connect(c_io, &client_id, None).await.unwrap();
        request_round(&mut c_ch, &client_store, conv).await.unwrap();
        drop(c_ch); // hang up so the relay's serve loop ends and the task joins
        server.await.unwrap();

        assert!(
            relay_store.lock().unwrap().has(&event_id),
            "relay persisted the pushed event"
        );
    }
}
```

- [ ] **Step 3: Run the postbox tests**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::postbox -- --test-threads=2
```
Expected: PASS — 3 tests (election none/lowest + the relay-ingest round-trip).

- [ ] **Step 4: fmt + clippy, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/mod.rs src-tauri/src/node/postbox.rs
git commit -m "feat(node): postbox — elected_post_office + relay serve loop

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: `--post-office` mode in the binary

**Files:**
- Modify: `src-tauri/src/bin/mesh-talk-node.rs`

- [ ] **Step 1: Add imports for the relay path**

Add to the top `use` block (keep the existing imports; add these):
```rust
use mesh_talk::identity::device::DeviceIdentity;
use mesh_talk::node::postbox::run_relay_accept_loop;
use mesh_talk::postoffice::PostOffice;
```

- [ ] **Step 2: Add the `--post-office` flag to `Args`**

Add this field to the `Args` struct (after `discovery_port`):
```rust
    /// Run as a post office: a durable store-and-forward relay that advertises
    /// the post-office role (no DM REPL — it only relays).
    #[arg(long)]
    post_office: bool,
```

- [ ] **Step 3: Branch to the relay path at the top of `main`**

Immediately after `let args = Args::parse();` in `main`, add:
```rust
    if args.post_office {
        return run_post_office(args).await;
    }
```

- [ ] **Step 4: Add the `run_post_office` function**

Add this function (place it just below `main`, above `handle_msg`):
```rust
/// Run as a post office: load the identity, advertise the post-office role,
/// serve a durable `PostOffice` relay. No DM send/drain — a pure relay. Runs the
/// accept loop forever (stop with Ctrl-C / kill).
async fn run_post_office(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let identity = mesh_talk::identity::keystore::load_or_create(
        std::path::Path::new(&args.keystore),
        &args.password,
    )?;
    let user_id = identity.public().user_id();

    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, args.port)).await?;
    let tcp_port = listener.local_addr()?.port();

    // The relay advertises the post-office role.
    let announce = Announce::new_post_office(&identity, args.name.clone(), tcp_port);

    // Two identity copies: one is moved into the durable PostOffice, one is used
    // for the Noise handshake in the accept loop (same keypair, different owner).
    let (ed, x) = identity.secret_bytes();
    let transport_identity = DeviceIdentity::from_secret_bytes(ed, x);
    let relay_path = std::path::Path::new(&args.keystore)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("relay.log");
    let store = Arc::new(Mutex::new(PostOffice::open(
        &relay_path,
        &args.password,
        identity,
    )?));

    // Discovery: advertise ourselves (and harmlessly track peers) on the shared socket.
    let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
    let socket = Arc::new(discovery_socket(args.discovery_port)?);
    let target: SocketAddr = (Ipv4Addr::BROADCAST, args.discovery_port).into();
    tokio::spawn(run_listen(Arc::clone(&socket), Arc::clone(&roster), user_id.clone()));
    tokio::spawn(run_broadcast(
        Arc::clone(&socket),
        announce,
        target,
        Duration::from_secs(2),
    ));

    emit(&format!(
        "post-office {user_id} listening on tcp/{tcp_port}, discovery udp/{}",
        args.discovery_port
    ));

    // Serve relay connections until killed.
    run_relay_accept_loop(transport_identity, listener, store).await;
    Ok(())
}
```

- [ ] **Step 5: Build + clippy**
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo test --bin mesh-talk-node -- --test-threads=2
```
Expected: build OK; clippy clean; the 5 resolver tests still pass.

- [ ] **Step 6: Two-process manual smoke — a normal node sees the PO and elects it**

Confirms the role propagates over real discovery. Run the PO in the background, a normal node in the foreground; `/peers` should list the PO. (Uses an uncommon discovery port to avoid interference.)
```bash
cd src-tauri && TMP=$(mktemp -d) && DP=47711
# Start the post office in the background.
nice -n 10 cargo run --quiet --bin mesh-talk-node -- \
  --keystore "$TMP/po.keystore" --password pw --name Relay --discovery-port "$DP" --post-office &
PO_PID=$!
sleep 3
# Start a normal node, ask for peers, then quit.
printf '/peers\n/quit\n' | nice -n 10 cargo run --quiet --bin mesh-talk-node -- \
  --keystore "$TMP/node.keystore" --password pw --name Node --discovery-port "$DP" 2>&1 | grep -E "^(node|peer) "
kill "$PO_PID" 2>/dev/null
```
Expected: a `node <uid> listening ...` startup line, then a `peer <po_uid> Relay <addr>` line (the normal node discovered the post office). If no `peer` line appears within a couple seconds, discovery hasn't converged — increase the `sleep` to 5 and retry once (do not change the code).

- [ ] **Step 7: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(node-cli): --post-office mode runs a discoverable durable relay

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next plan

- **What this delivers:** the post office becomes a discoverable, runnable role — a signed `post_office` announce flag (unforgeable; a flipped bit fails `verify`), `PeerRecord.post_office` + `Roster::post_offices`, `node::postbox::elected_post_office` (deterministic lowest-fingerprint election over PO peers), a generic relay serve loop (`serve_one` over a `Mutex<PostOffice>`, duplex-testable), and a `--post-office` binary mode that serves the durable relay and advertises itself.
- **Deferred to Plan B of this slice:** `send_dm` replication to the elected PO (with the new success semantics), the periodic recipient drain loop, and the 3-process offline integration rig. Those USE `elected_post_office` and the running relay this plan delivers.
- **Security/correctness:** the role is a signed field, so discovery can't be tricked into trusting a fake PO; the relay validates every event on ingest (`PostOffice` → `PersistentEventLog` append gate) and holds only recipient-sealed ciphertext; the relay shares ONE `Mutex<PostOffice>` so concurrent connections serialize their (single-file) writes; the accept loop backs off on error (no busy-spin); no lock is held across an `.await`.
- **Accepted limitations (carried from the slice spec):** a single elected PO (no standbys); offline delivery still requires the recipient to re-discover the sender for decryption (Plan B's rig encodes that); in-memory roster; LAN broadcast domain only.
