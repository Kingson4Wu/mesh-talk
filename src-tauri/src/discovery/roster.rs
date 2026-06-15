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
        self.peers
            .retain(|_, r| now.duration_since(r.last_seen) < ttl);
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
        let rec = roster
            .get(&alice.public().user_id())
            .expect("alice recorded");
        assert_eq!(rec.addr, SocketAddr::new(ip(), 4000));
        assert_eq!(rec.name, "Alice");
        assert_eq!(rec.public, alice.public());
    }

    #[test]
    fn self_announce_is_filtered_out() {
        let me = DeviceIdentity::generate();
        let announce = Announce::new(&me, "Me", 4000);
        let mut roster = Roster::default();
        assert!(!roster.update(&announce, ip(), &me.public().user_id()));
        assert!(roster.get(&me.public().user_id()).is_none());
    }

    #[test]
    fn forged_announce_is_rejected() {
        let alice = DeviceIdentity::generate();
        let mut announce = Announce::new(&alice, "Alice", 4000);
        announce.sig[0] ^= 0xFF;
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

        roster.evict_stale(Duration::from_secs(3600));
        assert!(roster.get(&alice.public().user_id()).is_some());
        roster.evict_stale(Duration::ZERO);
        assert!(roster.get(&alice.public().user_id()).is_none());
    }

    #[test]
    fn re_announce_refreshes_addr_for_same_peer() {
        // A peer that re-announces on a new port overwrites its prior record
        // (the addr-binding argument relies on this).
        let alice = DeviceIdentity::generate();
        let mut roster = Roster::default();
        roster.update(&Announce::new(&alice, "Alice", 4000), ip(), "self");
        roster.update(&Announce::new(&alice, "Alice", 5000), ip(), "self");

        let rec = roster
            .get(&alice.public().user_id())
            .expect("alice present");
        assert_eq!(rec.addr.port(), 5000); // refreshed to the new port
        assert_eq!(roster.peers().len(), 1); // still one peer, not two
    }

    #[test]
    fn get_unknown_user_returns_none() {
        let roster = Roster::default();
        assert!(roster.get("nonexistent-user-id").is_none());
    }
}
