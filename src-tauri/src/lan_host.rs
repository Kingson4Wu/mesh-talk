//! LAN hub: let this desktop app host the PWA over http (on demand) so phones on the same network
//! download the app by scanning a QR, and run the signaling relay in-process so those phones can
//! connect. Two independent controls, matching the settings model:
//!   - the relay is a persistent toggle ("act as a relay service"), and
//!   - the http app-host is on-demand (started when sharing, stoppable afterward).
//!
//! The PWA, once downloaded, reads `?relay=…&room=…` from the URL and auto-joins.

use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use mesh_talk_signal::resolve_static_path;
use tauri::{AppHandle, Manager, State};

/// Preferred relay port — uncommon (not a well-known port) so it rarely clashes, and stable so a
/// phone's saved/failover relay URLs keep working. If it's taken we fall back to an OS-assigned
/// free port. The http app-host always takes a free port (8080 etc. are too commonly occupied),
/// and the actual ports are baked into the QR/join URL, so nothing is hardcoded into the link.
const PREFERRED_RELAY_PORT: u16 = 47480;
const ROOM: &str = "mesh-talk";

/// Runtime handles for the (optional) http app-host, the (optional) relay task, and the
/// (optional) gateway-hub task that joins that relay so the desktop node serves browser phones.
#[derive(Default)]
pub struct LanHub {
    http: Mutex<Option<Arc<tiny_http::Server>>>,
    relay: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The relay's actual bound port (so the app-host's QR + the hub use the real port).
    relay_port: Mutex<Option<u16>>,
    hub: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Live mDNS registration of this host's stable `meshtalk-<name>.local` (while relaying), so a
    /// phone can re-find this relay after the LAN IP changes. Dropped when the relay is turned off.
    mdns: Mutex<Option<mesh_talk_core::node::mdns::MdnsHandle>>,
}

/// Best-effort LAN IP: open a UDP socket "toward" an off-link address (nothing is sent) and read
/// back the local address the OS would route from.
fn lan_ip() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

/// Locate the built PWA (`dist`): a bundled resource in a packaged app, else the dev build tree.
fn dist_dir(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(res) = app.path().resource_dir() {
        let p = res.join("dist");
        if p.join("index.html").is_file() {
            return Some(p);
        }
    }
    let dev = PathBuf::from("../frontend/dist");
    dev.join("index.html").is_file().then_some(dev)
}

/// The http app-host serve loop (blocking; runs on its own thread). Serves `dir` until the server
/// is unblocked. Reuses the relay crate's `resolve_static_path` (routing + `..` traversal guard +
/// SPA fallback) and `mime_guess` (so the wasm is served as application/wasm).
fn serve_http(server: Arc<tiny_http::Server>, dir: PathBuf) {
    for req in server.incoming_requests() {
        let resp = match resolve_static_path(&dir, req.url()) {
            Some(path) => match std::fs::read(&path) {
                Ok(bytes) => {
                    let mime = mime_guess::from_path(&path).first_or_octet_stream();
                    let header = tiny_http::Header::from_bytes(
                        &b"Content-Type"[..],
                        mime.essence_str().as_bytes(),
                    )
                    .expect("valid header");
                    tiny_http::Response::from_data(bytes).with_header(header)
                }
                Err(_) => tiny_http::Response::from_string("not found").with_status_code(404),
            },
            None => tiny_http::Response::from_string("not found").with_status_code(404),
        };
        let _ = req.respond(resp);
    }
}

/// The LAN join URL phones open/scan: download the app from this host and auto-join the relay.
fn join_url(ip: IpAddr, http_port: u16, relay_port: u16) -> String {
    format!("http://{ip}:{http_port}/?relay=ws://{ip}:{relay_port}&room={ROOM}")
}

/// Start hosting the PWA over http (idempotent), returning the join URL to render as a QR. Binds
/// an OS-assigned free port (so a busy 8080 etc. can't break it) and bakes the actual port into
/// the URL. The relay's actual port (if running) goes into the URL too, else the preferred one.
#[tauri::command]
pub fn start_app_host(app: AppHandle, hub: State<LanHub>) -> Result<String, String> {
    let dir = dist_dir(&app).ok_or("the PWA build (dist) was not found")?;
    let http_port = {
        let mut slot = hub.http.lock().unwrap();
        if slot.is_none() {
            let server = Arc::new(tiny_http::Server::http("0.0.0.0:0").map_err(|e| e.to_string())?);
            let serving = Arc::clone(&server);
            std::thread::spawn(move || serve_http(serving, dir));
            *slot = Some(server);
        }
        slot.as_ref()
            .and_then(|s| s.server_addr().to_ip())
            .map(|a| a.port())
            .ok_or("could not read the app-host port")?
    };
    let relay_port = hub
        .relay_port
        .lock()
        .unwrap()
        .unwrap_or(PREFERRED_RELAY_PORT);
    let ip = lan_ip().ok_or("could not determine this machine's LAN IP")?;
    Ok(join_url(ip, http_port, relay_port))
}

/// Stop the on-demand http app-host (the relay, if running, is left untouched).
#[tauri::command]
pub fn stop_app_host(hub: State<LanHub>) {
    if let Some(server) = hub.http.lock().unwrap().take() {
        server.unblock();
    }
}

/// Turn the in-process signaling relay on or off (the persistent "act as a relay" setting). While
/// the relay runs and a node is signed in, the desktop node also joins it as a mesh gateway HUB —
/// so phones reach this desktop (and, through it, each other + the LAN mesh), not just one another.
#[tauri::command]
pub async fn set_relay_running(
    running: bool,
    hub: State<'_, LanHub>,
    node: State<'_, crate::chat_commands::NodeState>,
) -> Result<(), String> {
    if running {
        // Don't hold the std Mutex across .await: check, bind, then store.
        if hub.relay.lock().unwrap().is_some() {
            return Ok(());
        }
        // Prefer the stable uncommon port (so saved/failover URLs keep working); if it's taken,
        // fall back to any free port rather than failing.
        let listener = match tokio::net::TcpListener::bind(("0.0.0.0", PREFERRED_RELAY_PORT)).await
        {
            Ok(l) => l,
            Err(_) => tokio::net::TcpListener::bind("0.0.0.0:0")
                .await
                .map_err(|e| e.to_string())?,
        };
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let task = tokio::spawn(mesh_talk_signal::serve(listener));
        *hub.relay.lock().unwrap() = Some(task);
        *hub.relay_port.lock().unwrap() = Some(port);
        // If signed in, run the desktop node as a hub on the local relay (reconnects until it's up).
        if let Some(rt) = node.0.lock().await.as_ref() {
            let handle = rt.spawn_gateway_hub(format!("ws://127.0.0.1:{port}"), ROOM.to_string());
            if let Some(prev) = hub.hub.lock().unwrap().replace(handle) {
                prev.abort();
            }
            // Announce this relay's LAN endpoint into the mesh so phones learn it (for failover)
            // even if they scanned a different host. Phones can't use 127.0.0.1 — use the LAN IP.
            // (This IS a relay concern — we only advertise a relay endpoint we actually host.
            // Identity presence is announced unconditionally by the node, NOT tied to the relay.)
            if let Some(ip) = lan_ip() {
                rt.announce_relay_endpoint(&format!("ws://{ip}:{port}"));
                // mDNS: register a STABLE `meshtalk-<name>.local` for this relay + advertise the
                // hostname URL too, so a phone whose cached IP went stale (the host's IP changed)
                // can still re-find it by name. Best-effort + IP-first: the IP URL above is the
                // primary path; `.local` is a resolve-by-name fallback for OSes that support it
                // (macOS/iOS solid, Android unreliable). Only IPv4 LAN IPs get an mDNS A record.
                if let std::net::IpAddr::V4(v4) = ip {
                    let label = mesh_talk_core::node::mdns::hostname_label(rt.display_name());
                    match mesh_talk_core::node::mdns::register(&label, v4, port) {
                        Ok(handle) => {
                            rt.announce_relay_endpoint(&format!("ws://{}:{port}", handle.hostname));
                            *hub.mdns.lock().unwrap() = Some(handle);
                        }
                        Err(e) => log::warn!("[mesh] mDNS register failed (continuing on IP): {e}"),
                    }
                }
            }
        }
    } else {
        if let Some(task) = hub.relay.lock().unwrap().take() {
            task.abort();
        }
        *hub.relay_port.lock().unwrap() = None;
        if let Some(task) = hub.hub.lock().unwrap().take() {
            task.abort();
        }
        // Drop the mDNS registration (unregisters + shuts the responder down).
        *hub.mdns.lock().unwrap() = None;
    }
    Ok(())
}

/// Whether the relay is currently running (for the settings toggle to reflect state).
#[tauri::command]
pub fn relay_running(hub: State<LanHub>) -> bool {
    hub.relay.lock().unwrap().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn http_host_serves_the_app_shell() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"<title>Mesh-Talk</title>").unwrap();
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let serving = Arc::clone(&server);
        let dirbuf = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || serve_http(serving, dirbuf));

        // GET /?relay=… (the scanned URL) must return the app shell (SPA fallback to index.html).
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        sock.write_all(
            b"GET /?relay=ws://x&room=y HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
        let mut resp = String::new();
        sock.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200"), "status: {resp}");
        assert!(resp.contains("Mesh-Talk"), "body: {resp}");

        server.unblock();
        let _ = handle.join();
    }
}
