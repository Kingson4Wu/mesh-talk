//! Low-level socket helpers shared by the node's discovery (the CLI binary and
//! the desktop runtime both bind the same kind of reuse+broadcast UDP socket).

use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;

/// The default UDP port for the node's signed-announce discovery. Distinct
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
        // Port 0 → OS-assigned; just proves the bind + option-setting path works.
        let a = discovery_socket(0).unwrap();
        assert!(a.local_addr().is_ok());
    }
}
