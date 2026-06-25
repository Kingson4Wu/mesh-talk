//! The signaling relay forwards a frame from one room member to the other.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn relays_within_a_room_and_not_to_self() {
    // Start the relay on an ephemeral port.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(mesh_talk_signal::serve(listener));

    let url = format!("ws://{addr}");
    let (mut a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Both join the same room.
    let join = Message::Text(r#"{"type":"join","room":"r1"}"#.into());
    a.send(join.clone()).await.unwrap();
    b.send(join).await.unwrap();
    // Let both joins register before A sends.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A sends an "offer"; B must receive it verbatim.
    a.send(Message::Text("OFFER-SDP".into())).await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(2), b.next())
        .await
        .expect("B should receive within 2s")
        .expect("stream open")
        .expect("a message");
    assert_eq!(got, Message::Text("OFFER-SDP".into()));

    // A must NOT receive its own message (no echo). Nothing should arrive promptly.
    let self_echo = tokio::time::timeout(Duration::from_millis(300), a.next()).await;
    assert!(
        self_echo.is_err(),
        "sender should not get its own frame back"
    );
}

#[tokio::test]
async fn mesh_mode_addresses_peers_and_announces_membership() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(mesh_talk_signal::serve(listener));
    let url = format!("ws://{addr}");

    async fn read(
        ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    ) -> String {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => t,
            other => panic!("expected a text frame, got {other:?}"),
        }
    }

    // The hub joins first (mesh), then two spokes.
    let (mut hub, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    hub.send(Message::Text(
        r#"{"type":"join","room":"m","mesh":true}"#.into(),
    ))
    .await
    .unwrap();
    let w = read(&mut hub).await; // welcome: you=0, peers=[]
    assert!(
        w.contains("\"kind\":\"welcome\"") && w.contains("\"peers\":[]"),
        "hub welcome: {w}"
    );

    let (mut s1, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    s1.send(Message::Text(
        r#"{"type":"join","room":"m","mesh":true}"#.into(),
    ))
    .await
    .unwrap();
    let w1 = read(&mut s1).await; // welcome: peers=[0]
    assert!(
        w1.contains("\"welcome\"") && w1.contains("\"peers\":[0]"),
        "s1 welcome: {w1}"
    );
    // The hub is told a peer joined.
    let pj = read(&mut hub).await;
    assert!(
        pj.contains("\"peer-joined\"") && pj.contains("\"peer\":1"),
        "hub peer-joined: {pj}"
    );

    let (mut s2, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    s2.send(Message::Text(
        r#"{"type":"join","room":"m","mesh":true}"#.into(),
    ))
    .await
    .unwrap();
    let _w2 = read(&mut s2).await;
    let _ = read(&mut hub).await; // hub: peer-joined 2
    tokio::time::sleep(Duration::from_millis(100)).await;

    // s1 addresses the hub (to:0); ONLY the hub gets it, tagged from:1. s2 gets nothing.
    s1.send(Message::Text(r#"{"kind":"offer","sdp":"X","to":0}"#.into()))
        .await
        .unwrap();
    let at_hub = read(&mut hub).await;
    assert!(
        at_hub.contains("\"sdp\":\"X\"") && at_hub.contains("\"from\":1"),
        "hub got: {at_hub}"
    );
    let leaked = tokio::time::timeout(Duration::from_millis(300), s2.next()).await;
    assert!(
        leaked.is_err(),
        "an addressed frame must not reach other spokes"
    );
}
