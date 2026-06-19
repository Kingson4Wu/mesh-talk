# mesh-talk-node CLI Binary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `mesh-talk-node`, a thin CLI over the `node` API that loads a persistent identity, runs signed-announce discovery + a TCP listener, and exchanges 1:1 DMs from a line-based REPL — validated by a two-process loopback rig where one node `/msg`s another and the other prints the decrypted message.

**Architecture:** A new binary `src-tauri/src/bin/mesh-talk-node.rs` wires the already-built pieces: `identity::keystore::load_or_create` (persistent identity), a socket2 UDP socket (reuse+broadcast) feeding `discovery::service::{run_listen, run_broadcast}` into a shared `Arc<Mutex<Roster>>`, and `node::Node` (owning the event log) with `run_accept_loop` for inbound and `send_dm` for outbound. A `#[tokio::main]` REPL reads stdin lines (`/peers`, `/msg <prefix> <text>`, `/quit`) and a spawned task drains the `ReceivedDm` mpsc to stdout. The headline test drives two real child processes.

**Tech Stack:** Rust; `clap` 4.4 (derive) for args; `tokio` (full) for async/stdin/TCP/UDP; `socket2` 0.5 for the reuse+broadcast UDP socket; the redesign modules `identity`, `discovery`, `node` (and transitively `transport`, `eventlog`, `dm`). No new dependencies. Integration test uses `std::process::Command` + `tempfile` (existing dev-dependency).

---

## Background the implementer needs

**This is Plan 3 (final) of the "online direct DM" slice** (spec: `docs/superpowers/specs/2026-06-16-node-integration-online-dm-design.md`, §2.3 + §5). Plan 1 built `discovery` + `node::transport`; Plan 2 built `node::conversation`/`node::session`/`node::node` (the `Node` App context, proven by an in-process two-node loopback test). This plan adds the runnable CLI and the **two-process** validation rig. The legacy `mesh-talk-cli` binary is left untouched.

**Exact APIs you build on (all implemented — verified signatures):**
```rust
// crate name for bins is `mesh_talk` (Cargo: package "mesh-talk", lib "mesh_talk")
// identity (src-tauri/src/identity/) — NO re-exports; use full module paths
mesh_talk::identity::keystore::load_or_create(path: &std::path::Path, password: &str)
    -> Result<mesh_talk::identity::device::DeviceIdentity, mesh_talk::storage::errors::StorageError>; // StorageError: Display + Error
mesh_talk::identity::device::DeviceIdentity::generate() -> DeviceIdentity;
impl DeviceIdentity { pub fn public(&self) -> PublicIdentity; pub fn user_id(&self) -> String; }
impl PublicIdentity { pub fn user_id(&self) -> String; pub ed25519_pub: [u8;32]; pub x25519_pub: [u8;32]; }

// discovery (src-tauri/src/discovery/) — re-exports: Announce, PeerRecord, Roster, UserId
mesh_talk::discovery::Announce::new(identity: &DeviceIdentity, name: impl Into<String>, tcp_port: u16) -> Announce; // Clone
mesh_talk::discovery::Roster::default();
impl Roster { pub fn get(&self,&str)->Option<&PeerRecord>; pub fn update(&mut self,&Announce,IpAddr,&str)->bool; pub fn peers(&self)->Vec<PeerRecord>; pub fn evict_stale(&mut self,Duration); }
pub struct PeerRecord { pub public: PublicIdentity, pub addr: std::net::SocketAddr, pub name: String, pub last_seen: std::time::Instant } // Clone
pub type UserId = String;
mesh_talk::discovery::service::run_listen(socket: std::sync::Arc<tokio::net::UdpSocket>, roster: std::sync::Arc<std::sync::Mutex<Roster>>, self_user_id: String); // async, spawn it
mesh_talk::discovery::service::run_broadcast(socket: std::sync::Arc<tokio::net::UdpSocket>, announce: Announce, target: std::net::SocketAddr, interval: std::time::Duration); // async, spawn it

// node (src-tauri/src/node/) — re-exports: Node, NodeError, ReceivedDm
mesh_talk::node::Node::new(identity: DeviceIdentity, roster: std::sync::Arc<std::sync::Mutex<Roster>>, incoming: tokio::sync::mpsc::UnboundedSender<ReceivedDm>) -> std::sync::Arc<Node>;
impl Node { pub fn user_id(&self)->String; pub async fn send_dm(&self, recipient:&str, text:&[u8])->Result<(),NodeError>; pub async fn run_accept_loop(self: Arc<Self>, listener: tokio::net::TcpListener); }
pub struct ReceivedDm { pub from: UserId, pub from_name: String, pub text: Vec<u8> }
pub enum NodeError; // Display
```

**Critical environment facts (verified against the codebase):**
- The roster is shared as `Arc<std::sync::Mutex<Roster>>` (std Mutex, NOT tokio) — `Node::new` and `run_listen` both take that exact type. Lock only synchronously; never hold the guard across `.await`.
- `Node::new` CONSUMES `identity` by value. Build the `Announce` (which borrows `&identity`) and read `user_id` BEFORE moving `identity` into `Node`.
- **Discovery socket:** `service` does NOT bind a socket — the caller binds it. For two processes on one host to discover each other, bind ONE socket to `0.0.0.0:<discovery_port>` with `SO_REUSEADDR` + `SO_REUSEPORT` + `SO_BROADCAST` (via socket2), share it (`Arc`) for both `run_listen` and `run_broadcast`, and broadcast to `255.255.255.255:<discovery_port>`. This is the established working pattern in `src-tauri/src/network/udp.rs:58-134` (limited broadcast loops back to all same-host sockets bound to that port; self-announces are filtered by `Roster::update` via `self_user_id`).
- **Piped stdout is block-buffered in Rust.** When the binary's stdout is a pipe (as in the integration rig), `println!` will NOT flush promptly and the parent will hang waiting for lines. Every user-visible line MUST be flushed — use the `emit()` helper below, never bare `println!`, for anything the rig reads.

**CPU/test discipline (MANDATORY — this machine has had CPU spikes):** never run a bare `cargo test`/`cargo build`. Always `nice` + throttle + scope:
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo test --bin mesh-talk-node -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo fmt
```
The pre-commit hook runs the full health check on commit — let it run. NOTE: the Task-3 integration test is `#[ignore]`d (it spawns real processes + UDP broadcast, which can be blocked/flaky in sandboxed CI), so it does NOT run in the pre-commit/CI test sweep; run it explicitly to verify.

**Conventions:** match the existing bins (`src-tauri/src/bin/mesh-talk-cli.rs` uses `#[tokio::main]`, `clap::Parser`, `use mesh_talk::...`). Hand-written errors, no `thiserror`.

---

### Task 1: Register the binary + args + the recipient resolver

**Files:**
- Modify: `src-tauri/Cargo.toml` (add `[[bin]]`)
- Create: `src-tauri/src/bin/mesh-talk-node.rs`

- [ ] **Step 1: Register the binary in `src-tauri/Cargo.toml`**

After the existing `[[bin]]` block for `mesh-talk-cli` (around line 67-69), append:
```toml
[[bin]]
name = "mesh-talk-node"
path = "src/bin/mesh-talk-node.rs"
```

- [ ] **Step 2: Create `src-tauri/src/bin/mesh-talk-node.rs` (skeleton + resolver)**

This first version parses args, loads the persistent identity, prints a startup line, and exits. It also defines the pure `resolve_recipient` helper (used by the REPL in Task 2) with unit tests. The `#[allow(dead_code)]` is removed in Task 2 when `main` starts calling the resolver.

```rust
//! `mesh-talk-node`: a thin CLI over the redesign `node` API — a persistent
//! identity, signed-announce LAN discovery, a TCP listener, and a line-based
//! REPL for 1:1 DMs. The runtime wiring lands in the next task; this skeleton
//! parses args, loads the identity, and proves the recipient resolver.

use clap::Parser;
use mesh_talk::discovery::{PeerRecord, UserId};

/// Default UDP port for the redesign's signed-announce discovery. Distinct from
/// the legacy plaintext discovery port so the two protocols never collide.
const DEFAULT_DISCOVERY_PORT: u16 = 47474;

#[derive(Parser, Debug)]
#[command(name = "mesh-talk-node", about = "Mesh-Talk redesign node (serverless E2E LAN DM)")]
struct Args {
    /// Path to the encrypted identity keystore (created if absent).
    #[arg(long)]
    keystore: String,
    /// Password that encrypts the keystore.
    #[arg(long)]
    password: String,
    /// Display name advertised to peers.
    #[arg(long)]
    name: String,
    /// TCP port to listen on for peer connections (0 = OS-assigned).
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// UDP port for signed-announce discovery (must match across peers).
    #[arg(long, default_value_t = DEFAULT_DISCOVERY_PORT)]
    discovery_port: u16,
}

/// Why a `/msg` prefix did not resolve to exactly one peer.
#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)] // used by the REPL added in the next task
enum ResolveError {
    NotFound,
    Ambiguous(usize),
}

/// Resolve a `/msg` recipient by a `user_id` PREFIX against a roster snapshot.
/// Returns the unique matching peer's full `user_id`, or an error if zero or
/// more than one peer matches.
#[allow(dead_code)] // used by the REPL added in the next task
fn resolve_recipient(peers: &[PeerRecord], prefix: &str) -> Result<UserId, ResolveError> {
    let matches: Vec<UserId> = peers
        .iter()
        .map(|p| p.public.user_id())
        .filter(|uid| uid.starts_with(prefix))
        .collect();
    match matches.len() {
        0 => Err(ResolveError::NotFound),
        1 => Ok(matches.into_iter().next().expect("len checked == 1")),
        n => Err(ResolveError::Ambiguous(n)),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let identity = mesh_talk::identity::keystore::load_or_create(
        std::path::Path::new(&args.keystore),
        &args.password,
    )?;
    let user_id = identity.public().user_id();
    println!(
        "node {user_id} (name={}) tcp/{} discovery/{}",
        args.name, args.port, args.discovery_port
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk::identity::device::DeviceIdentity;
    use std::net::SocketAddr;
    use std::time::Instant;

    fn peer(name: &str) -> PeerRecord {
        PeerRecord {
            public: DeviceIdentity::generate().public(),
            addr: "127.0.0.1:4000".parse::<SocketAddr>().unwrap(),
            name: name.to_string(),
            last_seen: Instant::now(),
        }
    }

    #[test]
    fn unique_prefix_resolves_to_the_full_user_id() {
        let p = peer("Alice");
        let full = p.public.user_id();
        let prefix = &full[..8];
        let peers = vec![p];
        assert_eq!(resolve_recipient(&peers, prefix), Ok(full));
    }

    #[test]
    fn no_match_is_not_found() {
        let peers = vec![peer("Alice")];
        // user_ids are lowercase hex; "zzzz" can never be a prefix.
        assert_eq!(resolve_recipient(&peers, "zzzz"), Err(ResolveError::NotFound));
    }

    #[test]
    fn ambiguous_prefix_reports_the_count() {
        let peers = vec![peer("Alice"), peer("Bob"), peer("Carol")];
        // The empty prefix matches every peer.
        assert_eq!(resolve_recipient(&peers, ""), Err(ResolveError::Ambiguous(3)));
    }

    #[test]
    fn empty_roster_is_not_found() {
        assert_eq!(resolve_recipient(&[], "ab"), Err(ResolveError::NotFound));
    }
}
```

- [ ] **Step 3: Build, test, and check the binary**
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo test --bin mesh-talk-node -- --test-threads=2
cd src-tauri && nice -n 10 cargo clippy --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo run --quiet --bin mesh-talk-node -- --help
```
Expected: build OK; **4 tests pass**; clippy clean; `--help` prints usage listing `--keystore`, `--password`, `--name`, `--port`, `--discovery-port` and exits 0.

- [ ] **Step 4: fmt, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/Cargo.toml src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(node-cli): register mesh-talk-node binary; args + recipient resolver

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Wire identity + discovery + node + the REPL into a runnable node

**Files:**
- Modify: `src-tauri/src/bin/mesh-talk-node.rs` (replace `main`, add the socket helper + `emit`, drop the two `#[allow(dead_code)]`)

- [ ] **Step 1: Replace the file's imports, helpers, and `main`**

Replace the top `use` block, the two `#[allow(dead_code)]` attributes (remove them — `main` now uses both items), and the entire `main` function. Keep `Args`, `DEFAULT_DISCOVERY_PORT`, `ResolveError`, `resolve_recipient`, and the `#[cfg(test)] mod tests` block exactly as they are.

New imports at the top of the file (replace the existing two `use` lines):
```rust
use clap::Parser;
use mesh_talk::discovery::service::{run_broadcast, run_listen};
use mesh_talk::discovery::{Announce, PeerRecord, Roster, UserId};
use mesh_talk::node::{Node, ReceivedDm};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
```

Remove the `#[allow(dead_code)]` line above `enum ResolveError` and the one above `fn resolve_recipient` (both are now used by `main`).

Add these two helpers (place them just above `main`):
```rust
/// Write one line to stdout and FLUSH it. Piped stdout is block-buffered, so a
/// bare `println!` would not reach a parent process promptly — every
/// rig-observable line must go through here.
fn emit(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Bind the shared discovery UDP socket: `SO_REUSEADDR` + `SO_REUSEPORT` (so two
/// nodes on one host can share the port) + `SO_BROADCAST`, bound to
/// `0.0.0.0:<discovery_port>`. Mirrors the working pattern in
/// `network/udp.rs`. The same socket is used to both broadcast and listen.
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
```

Replace `main` with:
```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Persistent identity (created on first run).
    let identity = mesh_talk::identity::keystore::load_or_create(
        std::path::Path::new(&args.keystore),
        &args.password,
    )?;
    let user_id = identity.public().user_id();

    // TCP listener for peer connections; resolve the actual (possibly OS-assigned) port.
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, args.port)).await?;
    let tcp_port = listener.local_addr()?.port();

    // Build the signed announce (borrows identity) BEFORE moving identity into the node.
    let announce = Announce::new(&identity, args.name.clone(), tcp_port);

    // Shared roster (discovery writes it; the node + REPL read it) and the node.
    let roster: Arc<Mutex<Roster>> = Arc::new(Mutex::new(Roster::default()));
    let (incoming_tx, mut incoming_rx) = mpsc::unbounded_channel::<ReceivedDm>();
    let node = Node::new(identity, Arc::clone(&roster), incoming_tx);

    // Discovery: one shared reuse+broadcast socket drives both loops.
    let socket = Arc::new(discovery_socket(args.discovery_port)?);
    let target: SocketAddr = (Ipv4Addr::BROADCAST, args.discovery_port).into();
    tokio::spawn(run_listen(Arc::clone(&socket), Arc::clone(&roster), user_id.clone()));
    tokio::spawn(run_broadcast(
        Arc::clone(&socket),
        announce,
        target,
        Duration::from_secs(2),
    ));

    // Inbound: serve sync rounds on each accepted connection.
    tokio::spawn(Arc::clone(&node).run_accept_loop(listener));

    // Print received DMs as they arrive.
    tokio::spawn(async move {
        while let Some(dm) = incoming_rx.recv().await {
            let text = String::from_utf8_lossy(&dm.text);
            emit(&format!("from {} ({}): {}", dm.from, dm.from_name, text));
        }
    });

    emit(&format!(
        "node {user_id} listening on tcp/{tcp_port}, discovery udp/{}",
        args.discovery_port
    ));

    // Line-based REPL on stdin.
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line == "/quit" {
            break;
        } else if line == "/peers" {
            let peers = roster.lock().expect("roster mutex not poisoned").peers();
            if peers.is_empty() {
                emit("(no peers yet)");
            } else {
                for p in peers {
                    emit(&format!("peer {} {} {}", p.public.user_id(), p.name, p.addr));
                }
            }
        } else if let Some(rest) = line.strip_prefix("/msg ") {
            handle_msg(&node, &roster, rest);
        } else if !line.is_empty() {
            emit("commands: /peers, /msg <user_id-prefix> <text>, /quit");
        }
    }
    Ok(())
}

/// Parse and dispatch a `/msg <prefix> <text>` command: resolve the recipient
/// against the current roster, then send the DM on a spawned task so the REPL
/// stays responsive while the dial + sync round runs.
fn handle_msg(node: &Arc<Node>, roster: &Arc<Mutex<Roster>>, rest: &str) {
    let mut parts = rest.splitn(2, ' ');
    let prefix = parts.next().unwrap_or("").trim();
    let text = parts.next().unwrap_or("").to_string();
    if prefix.is_empty() || text.is_empty() {
        emit("usage: /msg <user_id-prefix> <text>");
        return;
    }
    let peers = roster.lock().expect("roster mutex not poisoned").peers();
    match resolve_recipient(&peers, prefix) {
        Ok(uid) => {
            let node = Arc::clone(node);
            tokio::spawn(async move {
                if let Err(e) = node.send_dm(&uid, text.as_bytes()).await {
                    emit(&format!("send failed: {e}"));
                }
            });
        }
        Err(ResolveError::NotFound) => emit(&format!("no peer matches prefix '{prefix}'")),
        Err(ResolveError::Ambiguous(n)) => {
            emit(&format!("'{prefix}' matches {n} peers; be more specific"))
        }
    }
}
```

Note: the test module references `PeerRecord` and `UserId`; both remain imported at the top, so the `#[cfg(test)]` block compiles unchanged. `resolve_recipient`/`ResolveError` are now used by `handle_msg`, so removing their `#[allow(dead_code)]` is correct.

- [ ] **Step 2: Build + clippy**
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo clippy --bin mesh-talk-node -- -D warnings 2>&1 | tail -20
cd src-tauri && nice -n 10 cargo test --bin mesh-talk-node -- --test-threads=2
```
Expected: build OK; clippy clean; the 4 resolver tests still pass.

- [ ] **Step 3: Single-process smoke test (startup line flushes + clean exit)**
```bash
cd src-tauri && TMP=$(mktemp -d) && printf '/quit\n' | nice -n 10 cargo run --quiet --bin mesh-talk-node -- \
  --keystore "$TMP/id.keystore" --password pw --name Smoke 2>&1 | grep -m1 "^node "
```
Expected: `grep` prints a line like `node <32-hex-chars> listening on tcp/<port>, discovery udp/47474` and returns 0 (proving `emit` flushes a piped stdout and the process started, read `/quit`, and exited cleanly so the pipe closed). A second run reuses the keystore — identity is stable.

- [ ] **Step 4: fmt, then commit**
```bash
cd src-tauri && nice -n 10 cargo fmt
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/src/bin/mesh-talk-node.rs
git commit -m "feat(node-cli): discovery + TCP listener + node wiring + DM REPL

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Two-process integration rig

**Files:**
- Create: `src-tauri/tests/two_node_cli.rs`

This is the Phase-0 validation rig: spawn two real `mesh-talk-node` processes on loopback, let them discover each other over UDP broadcast, have A `/msg` B, and assert B prints the decrypted message. It is `#[ignore]`d so it never runs in the pre-commit/CI test sweep (real processes + UDP broadcast can be blocked/flaky in sandboxes); run it explicitly.

- [ ] **Step 1: Create `src-tauri/tests/two_node_cli.rs`**

```rust
//! Two-process integration rig for the "online direct DM" slice: two real
//! `mesh-talk-node` processes discover each other over UDP broadcast on
//! loopback, then one DMs the other and we assert the recipient prints the
//! decrypted message. `#[ignore]`d — spawns real processes and uses UDP
//! broadcast, which can be blocked/flaky in sandboxed CI. Run explicitly:
//!   cd src-tauri && nice -n 10 cargo test --test two_node_cli -- --ignored --test-threads=2

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// A spawned `mesh-talk-node` child with a line-buffered view of its stdout.
struct CliNode {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl CliNode {
    fn spawn(keystore: &std::path::Path, name: &str, discovery_port: u16) -> CliNode {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
            .arg("--keystore")
            .arg(keystore)
            .arg("--password")
            .arg("pw")
            .arg("--name")
            .arg(name)
            .arg("--port")
            .arg("0")
            .arg("--discovery-port")
            .arg(discovery_port.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mesh-talk-node");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        CliNode { child, stdin, lines: rx }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write to child stdin");
        self.stdin.flush().ok();
    }

    /// Block until a line satisfying `pred` arrives, or `timeout` elapses.
    /// Non-matching lines are discarded.
    fn wait_for(&self, pred: impl Fn(&str) -> bool, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self.lines.recv_timeout(remaining) {
                Ok(l) => {
                    if pred(&l) {
                        return Some(l);
                    }
                }
                Err(_) => return None,
            }
        }
    }
}

impl Drop for CliNode {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read the `user_id` from a node's startup line: `node <uid> listening ...`.
fn read_user_id(node: &CliNode) -> String {
    let line = node
        .wait_for(|l| l.starts_with("node "), Duration::from_secs(10))
        .expect("node printed its startup line");
    line.split_whitespace()
        .nth(1)
        .expect("startup line has a user_id")
        .to_string()
}

/// Poll `/peers` until `want_uid` appears in the roster, or fail after 20s.
fn await_discovery(node: &mut CliNode, want_uid: &str, who: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        node.send("/peers");
        if node
            .wait_for(|l| l.contains(want_uid), Duration::from_secs(1))
            .is_some()
        {
            return;
        }
    }
    panic!("{who} never discovered peer {want_uid}");
}

#[test]
#[ignore = "spawns two real processes using UDP broadcast; run with --ignored"]
fn two_cli_nodes_exchange_a_dm() {
    let dir = tempfile::tempdir().expect("tempdir");
    // An uncommon shared discovery port for the rig (avoid the 47474 default so a
    // real node running on the dev machine can't interfere).
    let discovery_port = 47600;

    let mut alpha = CliNode::spawn(&dir.path().join("alpha.keystore"), "Alpha", discovery_port);
    let mut bravo = CliNode::spawn(&dir.path().join("bravo.keystore"), "Bravo", discovery_port);

    let alpha_uid = read_user_id(&alpha);
    let bravo_uid = read_user_id(&bravo);
    assert_ne!(alpha_uid, bravo_uid, "two identities must differ");

    // Both directions must converge: Alpha needs Bravo to dial; Bravo needs
    // Alpha in its roster to look up her x25519 key and DECRYPT.
    await_discovery(&mut alpha, &bravo_uid, "Alpha");
    await_discovery(&mut bravo, &alpha_uid, "Bravo");

    // Alpha DMs Bravo by a user_id prefix.
    alpha.send(&format!("/msg {} hello-bravo", &bravo_uid[..8]));

    // Bravo prints the decrypted DM, attributed to Alpha by id and name.
    let got = bravo
        .wait_for(
            |l| l.starts_with(&format!("from {alpha_uid}")) && l.contains("hello-bravo"),
            Duration::from_secs(10),
        )
        .expect("Bravo received and decrypted Alpha's DM");
    assert!(got.contains("(Alpha)"), "expected sender name Alpha, got: {got}");
}
```

- [ ] **Step 2: Build the binary, then run the rig explicitly**
```bash
cd src-tauri && nice -n 10 cargo build --bin mesh-talk-node
cd src-tauri && nice -n 10 cargo test --test two_node_cli -- --ignored --test-threads=2
```
Expected: `test two_cli_nodes_exchange_a_dm ... ok` (1 passed). If it fails on `await_discovery`, the host is dropping same-host UDP broadcast (rare on macOS/Linux); if it fails after discovery, capture the cause — do NOT just raise the timeouts.

- [ ] **Step 3: Confirm the normal test sweep still excludes the rig**
```bash
cd src-tauri && nice -n 10 cargo test --test two_node_cli -- --test-threads=2
```
Expected: `0 passed; 0 failed; 1 ignored` (the rig does not run without `--ignored`, so the pre-commit hook stays fast and deterministic).

- [ ] **Step 4: Commit**
```bash
cd /Users/kingsonwu/programming/rust-src/mesh-talk
git add src-tauri/tests/two_node_cli.rs
git commit -m "test(node-cli): two-process loopback rig — A /msg B, B prints decrypted DM

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Notes for the reviewer

- **What this delivers:** the `mesh-talk-node` binary (persistent identity via keystore, signed-announce discovery on a reuse+broadcast UDP socket, a TCP listener serving sync rounds, and a `/peers`/`/msg`/`/quit` REPL that prints inbound DMs) plus the two-process loopback rig that is the Phase-0 acceptance test for the online path. With this, the "online direct DM" slice (Plans 1–3) is complete end-to-end across two real processes.
- **Key correctness choices:** (1) `emit()` flushes every rig-observable line because piped stdout is block-buffered; (2) the announce is built before `identity` is moved into `Node`; (3) one reuse+broadcast socket serves both discovery loops so two nodes share the port on one host; (4) `send_dm` runs on a spawned task so the REPL never blocks on a dial; (5) the roster guard is always dropped before any `.await`.
- **Why the rig is `#[ignore]`d:** it spawns OS processes and depends on same-host UDP broadcast delivery, which is environment-sensitive; keeping it out of the default sweep protects the pre-commit hook from flakiness while still shipping the rig. It is run explicitly (`--ignored`) as the acceptance check.
- **Accepted limitations (carried from the slice):** online-only (no offline/post-office delivery), in-memory message log (identity persists, messages don't), 1:1 DMs only, single LAN broadcast domain, fresh dial per `send_dm` (no connection reuse). The discovery default port (47474) is the redesign protocol's own, distinct from the legacy app's.
- **Next:** later slices — post-office offline delivery, a persistent message log, channels/groups, and migrating the Tauri UI onto this `node` API.
