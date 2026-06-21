//! Low-level socket helpers shared by the node's discovery (the CLI binary and
//! the desktop runtime both bind the same kind of reuse UDP socket and join the
//! discovery multicast group on every local interface).

use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;

/// The default UDP port for the node's signed-announce discovery. Distinct
/// from the legacy plaintext discovery port so the two protocols never collide.
pub const DEFAULT_DISCOVERY_PORT: u16 = 47474;

/// The IPv4 multicast group the node's discovery announces are sent to and
/// listened on. Mirrors LocalSend's `224.0.0.167`: a link-local (224.0.0.0/24)
/// group that stays on the LAN and survives where limited broadcast
/// (`255.255.255.255`) gets dropped or pinned to a single interface (e.g. macOS).
pub const DISCOVERY_MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);

/// Bind the shared discovery UDP socket: `SO_REUSEADDR` + `SO_REUSEPORT` (so two
/// nodes on one host can share the port) + `SO_BROADCAST`, bound to
/// `0.0.0.0:<discovery_port>`, then join [`DISCOVERY_MULTICAST_GROUP`] on every
/// non-loopback IPv4 interface (plus an unspecified-interface fallback). The same
/// socket is used to both send announces and listen.
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
    let sock = UdpSocket::from_std(socket.into())?;

    join_discovery_group_all_ifaces(&sock);

    Ok(sock)
}

/// Join [`DISCOVERY_MULTICAST_GROUP`] on every current non-loopback IPv4 interface
/// (plus an unspecified-interface fallback). A single interface (the default route)
/// is not enough on a multi-NIC / multi-WiFi host. Individual joins may fail (the
/// interface won't accept it, or it's already joined); that's fine, so per-join
/// errors are ignored. Called once at bind time and again periodically so interfaces
/// that come up after launch (e.g. WiFi associating late) get joined.
pub fn join_discovery_group_all_ifaces(sock: &UdpSocket) {
    for iface in ipv4_interface_addrs() {
        let _ = sock.join_multicast_v4(DISCOVERY_MULTICAST_GROUP, iface);
    }
    // Fallback: let the OS pick the interface, in case enumeration missed one.
    let _ = sock.join_multicast_v4(DISCOVERY_MULTICAST_GROUP, Ipv4Addr::UNSPECIFIED);
}

/// The local non-loopback IPv4 interface addresses, used as the per-interface
/// join points for the discovery multicast group and as the source set for the
/// unicast /24 scan. Errors enumerating interfaces degrade to an empty list (the
/// unspecified-interface fallback still applies).
pub fn ipv4_interface_addrs() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .filter_map(|iface| match iface.ip() {
            std::net::IpAddr::V4(addr) => Some(addr),
            std::net::IpAddr::V6(_) => None,
        })
        .collect()
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
