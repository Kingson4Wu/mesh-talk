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

        let server = tokio::spawn(async move {
            let ch = accept(&listener, &b).await.expect("accept");
            assert_eq!(ch.peer_identity(), &a_pub);
        });

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

        let server = tokio::spawn(async move {
            let _ = accept(&listener, &b).await;
        });

        let result = dial(addr, &a, Some(&wrong)).await;
        assert!(matches!(result, Err(TransportError::UnexpectedPeer)));
        let _ = server.await;
    }
}
