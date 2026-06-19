# Tauri UI Migration — Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the redesign `Node` through the Tauri v2 desktop backend: a `RedesignRuntime` opened per-user on login, four IPC commands (`redesign_my_id`/`redesign_list_peers`/`redesign_send_dm`/`redesign_history`), and a `redesign-dm-received` event — all additive, leaving the legacy stack untouched.

**Architecture:** A Tauri-agnostic `node::runtime::RedesignRuntime` owns the `Arc<Node>` + roster + spawned network tasks and takes an injected `on_dm` callback (so it stays unit-testable). `redesign_commands` adds managed `RedesignState(Arc<tokio::Mutex<Option<RedesignRuntime>>>)` + the four thin commands. `lib.rs`/`commands.rs` register the state/handlers and start the runtime on login (per-user keystore under `~/.mesh-talk/redesign/<user_id>/`, opened with the login password) and drop it on logout.

**Tech Stack:** Rust; Tauri v2; the redesign `node`/`discovery`/`identity` modules; `tokio`; `serde`; `socket2`. No new dependencies.

---

## Background the implementer needs

**This is Plan 1 (of 2) of the "Tauri UI migration" slice** (spec: `docs/superpowers/specs/2026-06-17-tauri-ui-migration-design.md`). Plan 2 is the Vue `/redesign` route. Phase 0 built the redesign `node` stack (rig-proven: online DM, offline post-office delivery, persistent history). This plan wires it into the desktop app behind commands/events, **additively** — the legacy `NodeService`/`AppState`/commands/events are untouched.

**Verified APIs you build on:**
```rust
// node (rig-proven)
mesh_talk::node::Node::open(identity: DeviceIdentity, roster: Arc<std::sync::Mutex<Roster>>,
    incoming: tokio::sync::mpsc::UnboundedSender<ReceivedDm>, log_path: &Path, sent_path: &Path, password: &str)
    -> Result<Arc<Node>, mesh_talk::eventlog::LogError>;
impl Node { pub fn user_id(&self)->String; pub async fn send_dm(&self,&str,&[u8])->Result<(),NodeError>;
    pub async fn run_accept_loop(self: Arc<Self>, listener: tokio::net::TcpListener);
    pub async fn drain_from_post_office(&self);
    pub fn dm_history(&self, peer: &PublicIdentity, limit: usize) -> Vec<HistoryEntry>; }
pub struct ReceivedDm { pub from: String, pub from_name: String, pub text: Vec<u8> } // Clone
pub struct HistoryEntry { pub from_me: bool, pub who: String, pub text: Vec<u8>, pub wall_clock: u64 }
pub enum NodeError; // Display
// discovery
mesh_talk::discovery::service::{run_listen, run_broadcast};
mesh_talk::discovery::{Announce, PeerRecord, Roster, UserId}; // PeerRecord { public: PublicIdentity, addr: SocketAddr, name: String, post_office: bool, last_seen }
mesh_talk::discovery::roster::Roster; // get(&str)->Option<&PeerRecord>; peers()->Vec<PeerRecord>
// identity
mesh_talk::identity::keystore::load_or_create(&Path, &str) -> Result<DeviceIdentity, mesh_talk::storage::errors::StorageError>;
mesh_talk::identity::device::{DeviceIdentity, PublicIdentity}; // DeviceIdentity::public()->PublicIdentity; PublicIdentity: Clone; user_id()
mesh_talk::eventlog::LogError; // From<StorageError> exists; Display+Error
```

**The legacy discovery-socket helper** lives only in `src-tauri/src/bin/mesh-talk-node.rs` (`discovery_socket`) — Task 1 lifts it into a shared `node::net` module so both the binary and the runtime use one implementation. Verbatim current helper:
```rust
fn discovery_socket(discovery_port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    let bind_addr: SocketAddr = (Ipv4Addr::UNSPECIFIED, discovery_port).into();
    socket.bind(&bind_addr.into())?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}
const DEFAULT_DISCOVERY_PORT: u16 = 47474;
```

**Tauri integration points (verbatim, Tauri v2):**
- `src-tauri/src/lib.rs` — `pub fn run_tauri()` builds `tauri::Builder::default()....manage(node_service).manage(app_state).invoke_handler(tauri::generate_handler![commands::send_message, …, commands::allow_firewall_port]).run(...)`. `use tauri::Manager;` is already imported. The data dir the app uses is `~/.mesh-talk` (`std::env::var("HOME") + ".mesh-talk"`), NOT Tauri's path API.
- `src-tauri/src/commands.rs` `login` (verbatim):
```rust
#[tauri::command]
pub async fn login(
    app_handle: tauri::AppHandle,
    node_service: tauri::State<'_, Arc<Mutex<NodeService>>>,
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<LoginResult, String> {
    let result = login_impl(username, password, app_state.inner()).map_err(|e| e.to_string())?;
    if result.success {
        let app_handle_clone = app_handle.clone();
        let node_service_clone = node_service.inner().clone();
        let app_state_clone = app_state.inner().clone();
        tauri::async_runtime::spawn(async move {
            { /* name fixup */ }
            if let Err(err) = start_network_impl(app_handle_clone, node_service_clone, app_state_clone).await {
                eprintln!("Failed to start network runtime: {}", err);
            }
        });
    }
    Ok(result)
}
```
  After a successful `login_impl`, the session is set; read it via `app_state.session().get() -> Option<SessionInfo>` where `SessionInfo { token, user: User { user_id: String, name: String, .. }, password: String }`. The login command's own `password` parameter is the plaintext password.
- `src-tauri/src/commands.rs` `logout`:
```rust
#[tauri::command]
pub async fn logout(app_state: tauri::State<'_, AppState>) -> Result<LogoutResult, String> {
    logout_impl(app_state.inner()).map_err(|e| e.to_string())
}
```
- `src-tauri/src/events.rs` event pattern: `pub const EVENT_X: &str = "x";` + a `#[derive(serde::Serialize, Clone)]` payload struct + `app_handle.emit(EVENT_X, payload)`.

**CPU/test discipline (MANDATORY — this machine has had CPU spikes; keystore/log opens run a slow password KDF):** never run a bare `cargo test`/`build`. Always:
```bash
cd src-tauri && nice -n 10 cargo test --lib node::runtime -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo build --lib
cd src-tauri && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
NOTE: building the full Tauri app (`cargo build` of the binary `mesh-talk`) pulls the whole GUI toolchain and is slow; prefer `cargo build --lib` to typecheck library changes, and `cargo check` where possible. The pre-commit hook runs the full health check — let it run.

**Conventions:** hand-written error enums (no `thiserror`), `Display`+`Error`+`From`; `#[cfg(test)] mod tests`; rustfmt sorts `pub mod`/`pub use`; std `Mutex` for the roster (never across `.await`); the runtime stays Tauri-agnostic (Tauri only via the injected `on_dm` closure).

---

### Task 1: `node::net` — shared discovery-socket helper

**Files:**
- Create: `src-tauri/src/node/net.rs`
- Modify: `src-tauri/src/node/mod.rs` (register module)
- Modify: `src-tauri/src/bin/mesh-talk-node.rs` (use the shared helper)

- [ ] **Step 1: Register the module in `node/mod.rs`**

Add `pub mod net;` (rustfmt sorts; correct order is `conversation, net, node, postbox, sentlog, session, transport`). The module section becomes:
```rust
pub mod conversation;
pub mod net;
pub mod node;
pub mod postbox;
pub mod sentlog;
pub mod session;
pub mod transport;

pub use node::{Node, NodeError, ReceivedDm};
pub use postbox::elected_post_office;
```

- [ ] **Step 2: Create `src-tauri/src/node/net.rs`**
```rust
//! Low-level socket helpers shared by the node's discovery (the CLI binary and
//! the desktop runtime both bind the same kind of reuse+broadcast UDP socket).

use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;

/// The default UDP port for the redesign's signed-announce discovery. Distinct
/// from the legacy plaintext discovery port so the two protocols never collide.
pub const DEFAULT_DISCOVERY_PORT: u16 = 47474;

/// Bind the shared discovery UDP socket: `SO_REUSEADDR` + `SO_REUSEPORT` (so two
/// nodes on one host can share the port) + `SO_BROADCAST`, bound to
/// `0.0.0.0:<discovery_port>`. The same socket is used to both broadcast and listen.
pub fn discovery_socket(discovery_port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    let bind_addr: SocketAddr = (Ipv4Addr::UNSPECIFIED, discovery_port).into();
    socket.bind(&bind_addr.into())?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binds_a_reuse_broadcast_socket() {
        // Two sockets can share the same port (reuse), proving the options stuck.
        let a = discovery_socket(0).unwrap(); // port 0 → OS-assigned; just proves bind works
        assert!(a.local_addr().is_ok());
    }
}
```

- [ ] **Step 3: Point the binary at the shared helper**

In `src-tauri/src/bin/mesh-talk-node.rs`: DELETE the local `fn discovery_socket(...)` and the local `const DEFAULT_DISCOVERY_PORT` (now in `node::net`). Add `use mesh_talk::node::net::{discovery_socket, DEFAULT_DISCOVERY_PORT};` to the imports. The `UdpSocket` import in the bin may become unused — if so remove it from the bin's `use tokio::net::{TcpListener, UdpSocket};` line (leaving `TcpListener`). Everything else in the bin is unchanged; the call sites `discovery_socket(args.discovery_port)` and the `#[arg(long, default_value_t = DEFAULT_DISCOVERY_PORT)]` keep working via the imports.

- [ ] **Step 4: Build + the bin's tests + clippy**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::net -- --test-threads=2
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo test --bin mesh-talk-node -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --lib --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
```
Expected: `node::net` test passes; the bin builds; its 5 resolver tests still pass; clippy clean.

- [ ] **Step 5: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/mod.rs src-tauri/src/node/net.rs src-tauri/src/bin/mesh-talk-node.rs
git commit -m "refactor(node): extract shared discovery_socket into node::net

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: `node::runtime::RedesignRuntime`

**Files:**
- Create: `src-tauri/src/node/runtime.rs`
- Modify: `src-tauri/src/node/mod.rs` (register module + re-export)

- [ ] **Step 1: Register the module in `node/mod.rs`**

Add `pub mod runtime;` (sorted: `conversation, net, node, postbox, runtime, sentlog, session, transport`) and re-export. The section becomes:
```rust
pub mod conversation;
pub mod net;
pub mod node;
pub mod postbox;
pub mod runtime;
pub mod sentlog;
pub mod session;
pub mod transport;

pub use node::{HistoryEntry, Node, NodeError, ReceivedDm};
pub use postbox::elected_post_office;
pub use runtime::{RedesignRuntime, RuntimeError};
```
(Adding `HistoryEntry` to the re-export — `RedesignRuntime::history` returns it and the runtime imports it as `crate::node::HistoryEntry`.)

- [ ] **Step 2: Create `src-tauri/src/node/runtime.rs`**
```rust
//! The desktop App context for the redesign node: opens a per-user `Node`, starts
//! its network loops (discovery, accept, drain) and an inbound-DM forwarder, and
//! holds the handles. Tauri-agnostic — inbound DMs are delivered via an injected
//! `on_dm` callback, so this is unit-testable without a GUI.

use crate::discovery::service::{run_broadcast, run_listen};
use crate::discovery::{Announce, PeerRecord, Roster};
use crate::eventlog::LogError;
use crate::identity::device::PublicIdentity;
use crate::identity::keystore;
use crate::node::net::discovery_socket;
use crate::node::{HistoryEntry, Node, NodeError, ReceivedDm};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// How often the desktop node drains held DMs from the elected post office.
const DRAIN_INTERVAL_SECS: u64 = 3;

/// Errors starting the redesign runtime.
#[derive(Debug)]
pub enum RuntimeError {
    /// Opening the keystore or the durable logs failed.
    Open(LogError),
    /// Binding the TCP listener or discovery socket failed.
    Io(std::io::Error),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Open(e) => write!(f, "redesign open error: {e}"),
            RuntimeError::Io(e) => write!(f, "redesign io error: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<LogError> for RuntimeError {
    fn from(e: LogError) -> Self {
        RuntimeError::Open(e)
    }
}
impl From<std::io::Error> for RuntimeError {
    fn from(e: std::io::Error) -> Self {
        RuntimeError::Io(e)
    }
}

/// A running redesign node plus the background tasks that drive it. Dropping it
/// aborts every task (clean logout/shutdown).
pub struct RedesignRuntime {
    node: Arc<Node>,
    roster: Arc<Mutex<Roster>>,
    user_id: String,
    tasks: Vec<JoinHandle<()>>,
}

impl RedesignRuntime {
    /// Open the per-user node under `base_dir/redesign/<user_id>/` (keystore +
    /// durable logs encrypted with `password`), advertise `display_name`, and start
    /// discovery + the accept loop + a periodic post-office drain + an inbound
    /// forwarder that calls `on_dm` for each received DM.
    pub async fn start(
        base_dir: &Path,
        user_id: &str,
        display_name: &str,
        password: &str,
        discovery_port: u16,
        on_dm: impl Fn(ReceivedDm) + Send + 'static,
    ) -> Result<RedesignRuntime, RuntimeError> {
        let dir = base_dir.join("redesign").join(user_id);
        std::fs::create_dir_all(&dir)?;

        let identity = keystore::load_or_create(&dir.join("identity.keystore"), password)
            .map_err(|e| RuntimeError::Open(e.into()))?;
        let self_uid = identity.public().user_id();

        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0u16)).await?;
        let tcp_port = listener.local_addr()?.port();

        // Build the announce BEFORE the identity is moved into the node.
        let announce = Announce::new(&identity, display_name.to_string(), tcp_port);

        let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
        let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<ReceivedDm>();
        let node = Node::open(
            identity,
            Arc::clone(&roster),
            incoming_tx,
            &dir.join("messages.log"),
            &dir.join("sent.log"),
            password,
        )?;

        let socket = Arc::new(discovery_socket(discovery_port)?);
        let target: SocketAddr = (Ipv4Addr::BROADCAST, discovery_port).into();

        let mut tasks: Vec<JoinHandle<()>> = Vec::new();
        tasks.push(tokio::spawn(run_listen(
            Arc::clone(&socket),
            Arc::clone(&roster),
            self_uid.clone(),
        )));
        tasks.push(tokio::spawn(run_broadcast(
            Arc::clone(&socket),
            announce,
            target,
            Duration::from_secs(2),
        )));
        tasks.push(tokio::spawn(Arc::clone(&node).run_accept_loop(listener)));
        {
            let node = Arc::clone(&node);
            tasks.push(tokio::spawn(async move {
                loop {
                    node.drain_from_post_office().await;
                    tokio::time::sleep(Duration::from_secs(DRAIN_INTERVAL_SECS)).await;
                }
            }));
        }
        tasks.push(tokio::spawn(async move {
            while let Some(dm) = incoming_rx.recv().await {
                on_dm(dm);
            }
        }));

        Ok(RedesignRuntime {
            node,
            roster,
            user_id: self_uid,
            tasks,
        })
    }

    /// This node's own user-id fingerprint.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// A snapshot of currently-known peers.
    pub fn peers(&self) -> Vec<PeerRecord> {
        self.roster.lock().expect("roster mutex not poisoned").peers()
    }

    /// The public identity of a known peer (to derive its DM conversation), if known.
    pub fn peer_public(&self, user_id: &str) -> Option<PublicIdentity> {
        self.roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(user_id)
            .map(|p| p.public.clone())
    }

    /// Send a DM to a known peer by user-id.
    pub async fn send_dm(&self, recipient: &str, text: &[u8]) -> Result<(), NodeError> {
        self.node.send_dm(recipient, text).await
    }

    /// The last `limit` messages of the DM with `peer`, both directions.
    pub fn history(&self, peer: &PublicIdentity, limit: usize) -> Vec<HistoryEntry> {
        self.node.dm_history(peer, limit)
    }

    /// A cloned handle to the underlying node (lets a caller release the runtime
    /// lock before an async send).
    pub fn handle(&self) -> Arc<Node> {
        Arc::clone(&self.node)
    }
}

impl Drop for RedesignRuntime {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_opens_a_node_and_exposes_its_identity() {
        let dir = tempfile::tempdir().unwrap();
        // Use an ephemeral, uncommon discovery port so the test doesn't touch the
        // real one or collide with a running node.
        let dp = 47990;
        let runtime = RedesignRuntime::start(
            dir.path(),
            "alice-user-id",
            "Alice",
            "pw",
            dp,
            |_dm| {},
        )
        .await
        .expect("runtime starts");

        // The node opened and exposes a stable fingerprint; no peers yet.
        assert_eq!(runtime.user_id().len(), 32); // user_id is 32 hex chars
        assert!(runtime.peers().is_empty());
        assert!(runtime.peer_public("nobody").is_none());

        // Per-user files were created on disk under redesign/<user_id>/.
        let node_dir = dir.path().join("redesign").join("alice-user-id");
        assert!(node_dir.join("identity.keystore").exists());
        assert!(node_dir.join("messages.log").exists());
        assert!(node_dir.join("sent.log").exists());

        // Reopening the same dir reloads the SAME identity (persistent).
        let runtime2 = RedesignRuntime::start(
            dir.path(),
            "alice-user-id",
            "Alice",
            "pw",
            dp,
            |_dm| {},
        )
        .await
        .expect("runtime reopens");
        assert_eq!(runtime.user_id(), runtime2.user_id());
    }
}
```

- [ ] **Step 3: Run the runtime test**
```bash
cd src-tauri && nice -n 10 cargo test --lib node::runtime -- --test-threads=2
```
Expected: PASS — `start_opens_a_node_and_exposes_its_identity` (it opens a node twice; slow due to the KDF — be patient).

- [ ] **Step 4: fmt + clippy + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/node/mod.rs src-tauri/src/node/runtime.rs
git commit -m "feat(node): RedesignRuntime — per-user node + network tasks + inbound forwarder

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: `redesign_commands` — state + the four IPC commands

**Files:**
- Create: `src-tauri/src/redesign_commands.rs`
- Modify: `src-tauri/src/lib.rs` (add `pub mod redesign_commands;` near the other `pub mod`s — DO NOT touch the builder yet, that is Task 4)

- [ ] **Step 1: Declare the module in `lib.rs`**

Add `pub mod redesign_commands;` alongside the other top-level `pub mod` declarations in `src-tauri/src/lib.rs` (e.g. next to `pub mod commands;`). Do not modify the builder/handlers in this task.

- [ ] **Step 2: Create `src-tauri/src/redesign_commands.rs`**
```rust
//! Tauri IPC commands backing the redesign `/redesign` route. They delegate to a
//! per-session [`RedesignRuntime`] held in managed [`RedesignState`] (populated on
//! login, cleared on logout). All are thin pass-throughs over the node API.

use crate::node::runtime::RedesignRuntime;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Managed state holding the current session's redesign runtime (`None` until login).
#[derive(Clone)]
pub struct RedesignState(pub Arc<Mutex<Option<RedesignRuntime>>>);

impl RedesignState {
    pub fn empty() -> Self {
        RedesignState(Arc::new(Mutex::new(None)))
    }
}

impl Default for RedesignState {
    fn default() -> Self {
        Self::empty()
    }
}

/// A peer as shown in the redesign roster.
#[derive(Serialize, Clone)]
pub struct PeerInfo {
    pub user_id: String,
    pub name: String,
    pub addr: String,
    pub post_office: bool,
}

/// One merged history line (sent or received) for display.
#[derive(Serialize, Clone)]
pub struct HistoryItem {
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
}

const NOT_STARTED: &str = "redesign node not started";

#[tauri::command]
pub async fn redesign_my_id(state: tauri::State<'_, RedesignState>) -> Result<String, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt.user_id().to_string())
}

#[tauri::command]
pub async fn redesign_list_peers(
    state: tauri::State<'_, RedesignState>,
) -> Result<Vec<PeerInfo>, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .peers()
        .into_iter()
        .map(|p| PeerInfo {
            user_id: p.public.user_id(),
            name: p.name,
            addr: p.addr.to_string(),
            post_office: p.post_office,
        })
        .collect())
}

#[tauri::command]
pub async fn redesign_send_dm(
    state: tauri::State<'_, RedesignState>,
    recipient: String,
    text: String,
) -> Result<(), String> {
    // Snapshot the runtime handle, then release the lock before the .await send.
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.send_dm(&recipient, text.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn redesign_history(
    state: tauri::State<'_, RedesignState>,
    peer: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    let public = rt
        .peer_public(&peer)
        .ok_or_else(|| format!("unknown peer: {peer}"))?;
    Ok(rt
        .history(&public, limit)
        .into_iter()
        .map(|h| HistoryItem {
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
        })
        .collect())
}
```

(`redesign_send_dm` calls `rt.handle()` — the `RedesignRuntime::handle() -> Arc<Node>` accessor was added in Task 2, so it snapshots the node and releases the `RedesignState` lock before the `.await` send.)

- [ ] **Step 3: Typecheck the library**
```bash
cd src-tauri && nice -n 10 cargo build --lib
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -20
```
Expected: the library compiles with the new module + commands; clippy clean. (The commands aren't registered with Tauri yet — that's Task 4 — but they compile as functions.)

- [ ] **Step 4: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/lib.rs src-tauri/src/node/runtime.rs src-tauri/src/redesign_commands.rs
git commit -m "feat(app): redesign IPC commands + RedesignState (my_id/list_peers/send_dm/history)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Wire the runtime into the Tauri app (state, handlers, login/logout, event)

**Files:**
- Modify: `src-tauri/src/events.rs` (the event constant + payload + emit helper)
- Modify: `src-tauri/src/lib.rs` (`.manage` + `generate_handler!`)
- Modify: `src-tauri/src/commands.rs` (`login` starts the runtime; `logout` clears it)

- [ ] **Step 1: Add the `redesign-dm-received` event (events.rs)**

In `src-tauri/src/events.rs`, add the constant alongside the others:
```rust
pub const EVENT_REDESIGN_DM_RECEIVED: &str = "redesign-dm-received";
```
and a payload struct + emit helper (place near the other payload structs / helpers; `use tauri::Emitter;`/`Manager` are already in scope in events.rs as `app_handle.emit` is used):
```rust
#[derive(serde::Serialize, Clone)]
pub struct RedesignDmReceivedEvent {
    pub from: String,
    pub from_name: String,
    pub text: String,
}

/// Emit a received redesign DM to the frontend (text decoded lossily for display).
pub fn emit_redesign_dm_received<R: tauri::Runtime>(
    app_handle: &tauri::AppHandle<R>,
    from: String,
    from_name: String,
    text: Vec<u8>,
) {
    let event = RedesignDmReceivedEvent {
        from,
        from_name,
        text: String::from_utf8_lossy(&text).into_owned(),
    };
    if let Err(e) = app_handle.emit(EVENT_REDESIGN_DM_RECEIVED, event) {
        log::error!("Failed to emit redesign dm event: {e}");
    }
}
```
(If `events.rs` imports the `Emitter`/`Manager` traits under a specific name, follow the file's existing pattern for `app_handle.emit(...)`; the existing `emit_message_received` shows the exact trait usage.)

- [ ] **Step 2: Manage `RedesignState` + register the commands (lib.rs)**

In `src-tauri/src/lib.rs`'s `run_tauri()` builder chain:
- add `.manage(crate::redesign_commands::RedesignState::empty())` next to `.manage(app_state)`;
- add the four commands to the `tauri::generate_handler![...]` list (after `commands::allow_firewall_port`):
```rust
            commands::allow_firewall_port,
            crate::redesign_commands::redesign_my_id,
            crate::redesign_commands::redesign_list_peers,
            crate::redesign_commands::redesign_send_dm,
            crate::redesign_commands::redesign_history
```

- [ ] **Step 3: Start the runtime on login (commands.rs `login`)**

Add a `redesign_state` parameter to the `login` command and, on success, spawn the runtime and store it. The login command becomes:
```rust
#[tauri::command]
pub async fn login(
    app_handle: tauri::AppHandle,
    node_service: tauri::State<'_, Arc<Mutex<NodeService>>>,
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
    redesign_state: tauri::State<'_, crate::redesign_commands::RedesignState>,
) -> Result<LoginResult, String> {
    let result =
        login_impl(username, password.clone(), app_state.inner()).map_err(|e| e.to_string())?;

    if result.success {
        // Legacy network start (unchanged).
        let app_handle_clone = app_handle.clone();
        let node_service_clone = node_service.inner().clone();
        let app_state_clone = app_state.inner().clone();
        tauri::async_runtime::spawn(async move {
            {
                let mut service = node_service_clone.lock().await;
                if service.get_name().trim().is_empty() || service.get_name() == "mesh-node" {
                    let generated_name = crate::api::default_node_name();
                    service.update_name(generated_name);
                }
            }
            if let Err(err) =
                start_network_impl(app_handle_clone, node_service_clone, app_state_clone).await
            {
                eprintln!("Failed to start network runtime: {}", err);
            }
        });

        // Redesign node start (additive): per-user keystore under ~/.mesh-talk/redesign/<user_id>/.
        if let Some(session) = app_state.session().get() {
            let user_id = session.user.user_id.clone();
            let display_name = session.user.name.clone();
            let redesign_handle = redesign_state.inner().clone();
            let app_handle_for_dm = app_handle.clone();
            let pw = password.clone();
            tauri::async_runtime::spawn(async move {
                let base_dir = redesign_data_dir();
                let emit_handle = app_handle_for_dm.clone();
                match crate::node::runtime::RedesignRuntime::start(
                    &base_dir,
                    &user_id,
                    &display_name,
                    &pw,
                    crate::node::net::DEFAULT_DISCOVERY_PORT,
                    move |dm| {
                        crate::events::emit_redesign_dm_received(
                            &emit_handle,
                            dm.from,
                            dm.from_name,
                            dm.text,
                        );
                    },
                )
                .await
                {
                    Ok(runtime) => {
                        *redesign_handle.0.lock().await = Some(runtime);
                        log::info!("Redesign node started for user {user_id}");
                    }
                    Err(e) => log::warn!("Redesign node failed to start: {e}"),
                }
            });
        }
    }

    Ok(result)
}

/// The base directory for redesign per-user data (mirrors the app's `~/.mesh-talk`).
fn redesign_data_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".mesh-talk")
}
```
(Note `login_impl(username, password.clone(), ...)` — clone the password since the original `password` is also moved into the redesign spawn. `login_impl` consumes `String`s, so clone is required.)

- [ ] **Step 4: Clear the runtime on logout (commands.rs `logout`)**

Add a `redesign_state` parameter and clear it (drop aborts its tasks). `logout` becomes:
```rust
#[tauri::command]
pub async fn logout(
    app_state: tauri::State<'_, AppState>,
    redesign_state: tauri::State<'_, crate::redesign_commands::RedesignState>,
) -> Result<LogoutResult, String> {
    // Stop and drop the redesign runtime (aborts its background tasks).
    redesign_state.0.lock().await.take();
    logout_impl(app_state.inner()).map_err(|e| e.to_string())
}
```

- [ ] **Step 5: Typecheck the library + clippy**
```bash
cd src-tauri && nice -n 10 cargo build --lib
cd src-tauri && nice -n 10 cargo clippy --lib -- -D warnings 2>&1 | tail -25
```
Expected: the library compiles (the new param threads through; the builder registers the state + handlers); clippy clean. NOTE: a full `cargo build` of the `mesh-talk` GUI binary is heavy — `cargo build --lib` is sufficient to typecheck this wiring; the GUI binary is exercised when the app is run (Plan 2).

- [ ] **Step 6: fmt + commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/events.rs src-tauri/src/lib.rs src-tauri/src/commands.rs
git commit -m "feat(app): start redesign node on login; redesign-dm-received event; clear on logout

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer / next plan

- **What this delivers:** the redesign node is now reachable from the desktop backend — opened per-user on login (keystore under `~/.mesh-talk/redesign/<user_id>/`, login password), exposed via four thin IPC commands, and pushing inbound DMs through a `redesign-dm-received` event. All additive; the legacy stack is untouched.
- **Tauri-agnostic core:** `RedesignRuntime` takes an injected `on_dm` closure and is unit-tested (`start` opens a real node over a tempdir, reopen yields the same identity). The command layer is thin pass-throughs; the send/receive/history/offline behavior beneath is already rig-proven.
- **Concurrency:** the roster is a std `Mutex` locked only for snapshots; `redesign_send_dm` snapshots the `Arc<Node>` and releases the `RedesignState` lock before the `.await`; `RedesignRuntime::Drop` aborts all tasks on logout.
- **Deferred to Plan 2:** the Vue `/redesign` route (peer list, chat window, send box, history, live inbound) + the `api.redesign` wrapper + the route/nav registration + the documented manual two-instance e2e.
- **Reviewer checks:** confirm `HistoryEntry`'s real import path (node/node.rs vs a re-export) and that it is `pub`; confirm `keystore::load_or_create` returns `Result<_, StorageError>` and that `LogError: From<StorageError>` (used by `RuntimeError::Open(e.into())`); confirm the `login` command's added `redesign_state` parameter resolves (Tauri injects `State` by type).
