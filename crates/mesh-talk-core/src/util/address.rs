//! Address display formatting. `SocketAddr`'s own `Display` already brackets IPv6
//! (`[::1]:80`), but peer addresses are frequently formatted from `host` + `port`
//! pieces (e.g. for logs, diagnostics, the UI), where a bare IPv6 literal would
//! produce the ambiguous `::1:80`. This brackets the host iff it needs it.

/// Format `host:port`, bracketing the host if it is a bare IPv6 literal.
///
/// - `("127.0.0.1", 80)` → `127.0.0.1:80`
/// - `("::1", 80)` → `[::1]:80`
/// - already-bracketed (`"[::1]"`) is left as-is.
/// - hostnames (`"example.com"`) are unaffected.
pub fn format_host_port(host: &str, port: u16) -> String {
    if needs_brackets(host) {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// True if `host` is a bare (unbracketed) IPv6 literal — heuristically, it contains a
/// colon and is not already bracketed. IPv4 and hostnames have no colon.
fn needs_brackets(host: &str) -> bool {
    host.contains(':') && !host.starts_with('[')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_is_unbracketed() {
        assert_eq!(format_host_port("127.0.0.1", 80), "127.0.0.1:80");
        assert_eq!(format_host_port("192.168.1.5", 4000), "192.168.1.5:4000");
    }

    #[test]
    fn bare_ipv6_is_bracketed() {
        assert_eq!(format_host_port("::1", 80), "[::1]:80");
        assert_eq!(
            format_host_port("fe80::1ff:fe23:4567:890a", 4000),
            "[fe80::1ff:fe23:4567:890a]:4000"
        );
    }

    #[test]
    fn already_bracketed_is_left_alone() {
        assert_eq!(format_host_port("[::1]", 80), "[::1]:80");
    }

    #[test]
    fn hostname_is_unaffected() {
        assert_eq!(format_host_port("example.com", 443), "example.com:443");
    }
}
