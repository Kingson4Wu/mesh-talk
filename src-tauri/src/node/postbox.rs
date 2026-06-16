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
    let winner = elect(
        &candidates
            .iter()
            .map(|r| r.public.clone())
            .collect::<Vec<_>>(),
    )?;
    candidates.into_iter().find(|r| r.public == winner)
}

/// Serve one authenticated inbound connection as the relay: run sync rounds over
/// the shared durable store until the peer disconnects. The PostOffice ingests
/// (validates + persists) pushed events and serves held events to pullers; it
/// never decrypts. Generic over the IO so it is testable over an in-memory duplex.
pub async fn serve_relay_connection<IO>(
    mut channel: SecureChannel<IO>,
    store: Arc<Mutex<PostOffice>>,
) where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    while let Ok(Served::Handled(_)) = serve_one(&mut channel, &store).await {}
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
        let event = Event::new(
            &client_id,
            conv,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
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
            let r_ch = SecureChannel::accept(r_io, &relay_transport_id)
                .await
                .unwrap();
            serve_relay_connection(r_ch, server_store).await;
        });

        let mut c_ch = SecureChannel::connect(c_io, &client_id, None)
            .await
            .unwrap();
        request_round(&mut c_ch, &client_store, conv).await.unwrap();
        drop(c_ch); // hang up so the relay's serve loop ends and the task joins
        server.await.unwrap();

        assert!(
            relay_store.lock().unwrap().has(&event_id),
            "relay persisted the pushed event"
        );
    }
}
