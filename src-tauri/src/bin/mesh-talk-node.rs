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
        assert_eq!(
            resolve_recipient(&peers, "zzzz"),
            Err(ResolveError::NotFound)
        );
    }

    #[test]
    fn ambiguous_prefix_reports_the_count() {
        let peers = vec![peer("Alice"), peer("Bob"), peer("Carol")];
        // The empty prefix matches every peer.
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
