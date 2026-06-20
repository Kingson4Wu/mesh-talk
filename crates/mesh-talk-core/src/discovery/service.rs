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

/// Peers not heard from within this window are evicted from the roster. Broadcast
/// interval is ~2s, so this tolerates ~15 missed announces before dropping a peer —
/// long enough to ride out brief loss, short enough that stale addresses (a peer that
/// went offline or changed IP) don't linger and misdirect fan-out / post-office picks.
const PEER_TTL: Duration = Duration::from_secs(30);

/// Listen for announces on `socket`, feeding the roster, until the socket errors
/// (e.g. the task is aborted on shutdown). Also evicts stale peers each iteration —
/// after every datagram (busy network) and on a `PEER_TTL` recv timeout (silent
/// network) — so a roster never grows unbounded or retains dead endpoints.
/// Phase-1: a single socket error currently ends the loop silently; production
/// needs transient-error retry, supervision/restart, and error surfacing to a
/// caller. The same applies to [`run_broadcast`].
pub async fn run_listen(socket: Arc<UdpSocket>, roster: Arc<Mutex<Roster>>, self_user_id: String) {
    let mut buf = vec![0u8; 2048];
    loop {
        match tokio::time::timeout(PEER_TTL, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, source))) => {
                handle_datagram(&roster, &buf[..n], source, &self_user_id);
            }
            Ok(Err(_)) => break, // socket error — end the loop (shutdown)
            Err(_) => {}         // recv timed out → fall through to evict
        }
        roster
            .lock()
            .expect("roster mutex not poisoned")
            .evict_stale(PEER_TTL);
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
        assert!(roster
            .lock()
            .unwrap()
            .get(&alice.public().user_id())
            .is_some());
    }

    #[test]
    fn handle_datagram_self_filters_own_announce() {
        // A datagram carrying our OWN user_id is dropped (not recorded), even
        // though it is a valid announce.
        let me = DeviceIdentity::generate();
        let roster = Mutex::new(Roster::default());
        let bytes = encode(&Announce::new(&me, "Me", 4000));
        assert!(!handle_datagram(
            &roster,
            &bytes,
            source(),
            &me.public().user_id()
        ));
        assert!(roster.lock().unwrap().get(&me.public().user_id()).is_none());
    }

    #[test]
    fn handle_datagram_rejects_garbage() {
        let roster = Mutex::new(Roster::default());
        assert!(!handle_datagram(
            &roster,
            b"not a datagram",
            source(),
            "self"
        ));
    }

    #[tokio::test]
    async fn listen_loop_records_a_received_announce() {
        let alice = DeviceIdentity::generate();
        let listener = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listen_addr = listener.local_addr().unwrap();
        let roster = Arc::new(Mutex::new(Roster::default()));
        tokio::spawn(run_listen(
            listener.clone(),
            roster.clone(),
            "self".to_string(),
        ));

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let announce = Announce::new(&alice, "Alice", 4000);
        sender
            .send_to(&encode(&announce), listen_addr)
            .await
            .unwrap();

        let mut recorded = false;
        for _ in 0..100 {
            if roster
                .lock()
                .unwrap()
                .get(&alice.public().user_id())
                .is_some()
            {
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
