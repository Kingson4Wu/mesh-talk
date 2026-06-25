//! mDNS LAN-hostname registration.
//!
//! A desktop that hosts the relay/download service registers a STABLE `meshtalk-<name>.local`, so a
//! phone can re-find it after the desktop's LAN IP changes — a cached relay URL pinned to an IP
//! goes stale, a hostname doesn't. Pure-Rust (`mdns-sd`); no system Bonjour/Avahi required (so it
//! also works on Windows). The responder runs on its own background thread; dropping the handle
//! shuts it down (unregisters).
//!
//! IMPORTANT — this is an OPTIMISATION layer, never a hard dependency: phone-side `.local`
//! resolution is OS-dependent (solid on macOS/iOS, unreliable on Android, and a plain-`http://`
//! `.local` page may not be a secure context). So callers ALWAYS also advertise the raw-IP URL;
//! the `.local` name only helps re-find a node whose IP changed, on platforms that resolve it.

use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::net::Ipv4Addr;

/// Our mDNS service type. The instance is `meshtalk-<label>`, the host `meshtalk-<label>.local`.
pub const SERVICE_TYPE: &str = "_meshtalk._tcp.local.";

/// A live mDNS registration. Dropping it shuts the responder down.
pub struct MdnsHandle {
    daemon: ServiceDaemon,
    fullname: String,
    /// The advertised hostname, e.g. `meshtalk-alice.local` (no trailing dot) — for building URLs.
    pub hostname: String,
}

impl Drop for MdnsHandle {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Turn a user's device name into a stable, DNS-safe label: `meshtalk-<lowercased a-z0-9->`,
/// runs of `-` collapsed, trimmed, length-bounded. Empty/garbage names fall back to `device`.
pub fn hostname_label(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-');
    let s = if s.is_empty() { "device" } else { s };
    format!("meshtalk-{}", &s[..s.len().min(40)])
}

/// Register `<label>.local` → `ip` on `port` (service type [`SERVICE_TYPE`]). Returns a handle;
/// drop to unregister. `label` should come from [`hostname_label`].
pub fn register(label: &str, ip: Ipv4Addr, port: u16) -> Result<MdnsHandle, String> {
    let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
    let host = format!("{label}.local.");
    let ip_str = ip.to_string();
    // Advertise the caller's IP AND auto-detect every other interface address, so the `.local`
    // name resolves no matter which NIC a phone is on (Wi-Fi vs Ethernet).
    let info = ServiceInfo::new(SERVICE_TYPE, label, &host, ip_str.as_str(), port, None)
        .map_err(|e| e.to_string())?
        .enable_addr_auto();
    let fullname = info.get_fullname().to_string();
    daemon.register(info).map_err(|e| e.to_string())?;
    Ok(MdnsHandle {
        daemon,
        fullname,
        hostname: format!("{label}.local"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_label_is_dns_safe_and_stable() {
        assert_eq!(hostname_label("Alice's PC!"), "meshtalk-alice-s-pc");
        assert_eq!(hostname_label("  夜空 box  "), "meshtalk-box");
        assert_eq!(hostname_label(""), "meshtalk-device");
        assert_eq!(hostname_label("---"), "meshtalk-device");
        // bounded
        assert!(hostname_label(&"x".repeat(200)).len() <= "meshtalk-".len() + 40);
    }

    // Real multicast round-trip — #[ignore]d so the fast gate (no multicast guarantees, flaky in
    // CI sandboxes) skips it; run explicitly: `cargo test -p mesh-talk-core --features native
    // mdns_round_trip -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn mdns_round_trip_register_then_resolve() {
        use mdns_sd::ServiceEvent;
        use std::time::{Duration, Instant};

        let label = hostname_label("roundtrip-test");
        let _h = register(&label, Ipv4Addr::new(127, 0, 0, 1), 9001).expect("register");

        let browser = ServiceDaemon::new().unwrap();
        let rx = browser.browse(SERVICE_TYPE).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(ServiceEvent::ServiceResolved(info)) =
                rx.recv_timeout(Duration::from_millis(500))
            {
                if info.get_port() == 9001 && info.get_hostname().contains(&label) {
                    return; // resolved our stable hostname → success
                }
            }
        }
        panic!("mDNS service did not resolve within 10s");
    }
}
