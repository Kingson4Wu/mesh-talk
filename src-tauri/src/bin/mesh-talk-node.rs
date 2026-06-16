//! `mesh-talk-node`: a thin CLI over the redesign `node` API — loads a
//! persistent identity, runs signed-announce LAN discovery and a TCP listener,
//! and drives 1:1 DMs from a line-based REPL (`/peers`, `/msg <prefix> <text>`,
//! `/quit`), printing inbound DMs as they arrive.

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

/// Default UDP port for the redesign's signed-announce discovery. Distinct from
/// the legacy plaintext discovery port so the two protocols never collide.
const DEFAULT_DISCOVERY_PORT: u16 = 47474;

#[derive(Parser, Debug)]
#[command(
    name = "mesh-talk-node",
    about = "Mesh-Talk redesign node (serverless E2E LAN DM)"
)]
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
enum ResolveError {
    NotFound,
    Ambiguous(usize),
}

/// Resolve a `/msg` recipient by a `user_id` PREFIX against a roster snapshot.
/// Returns the unique matching peer's full `user_id`, or an error if zero or
/// more than one peer matches.
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
    tokio::spawn(run_listen(
        Arc::clone(&socket),
        Arc::clone(&roster),
        user_id.clone(),
    ));
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
            // DMs are text-only in this CLI; lossy decode is safe here.
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
                    emit(&format!(
                        "peer {} {} {}",
                        p.public.user_id(),
                        p.name,
                        p.addr
                    ));
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
        assert_eq!(
            resolve_recipient(&peers, "zzzz"),
            Err(ResolveError::NotFound)
        );
    }

    #[test]
    fn full_user_id_as_prefix_resolves() {
        // Users often paste a complete user_id; an exact match must still resolve.
        let p = peer("Alice");
        let full = p.public.user_id();
        let peers = vec![p];
        assert_eq!(resolve_recipient(&peers, &full), Ok(full));
    }

    #[test]
    fn ambiguous_prefix_reports_the_count() {
        let peers = vec![peer("Alice"), peer("Bob"), peer("Carol")];
        // The empty prefix matches every peer (random fingerprints share no
        // deterministic non-empty prefix, so "" is the portable way to force a
        // multi-way match); the REPL guards against an empty prefix separately.
        assert_eq!(
            resolve_recipient(&peers, ""),
            Err(ResolveError::Ambiguous(3))
        );
    }

    #[test]
    fn empty_roster_is_not_found() {
        assert_eq!(resolve_recipient(&[], "ab"), Err(ResolveError::NotFound));
    }
}
