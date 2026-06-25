//! A tiny room-keyed WebSocket signaling relay for Mesh-Talk's browser gateway.
//!
//! Clients connect, send a `{"type":"join","room":"<id>"}` frame, and thereafter every text
//! frame they send is forwarded verbatim to the *other* members of the same room. That's all a
//! WebRTC SDP/ICE handshake needs. The relay never inspects or stores the payload, and the
//! eventual data channel is peer-to-peer + end-to-end encrypted — this server only ever sees the
//! (already public) connection-setup handshake. See
//! docs/superpowers/specs/2026-06-23-mobile-pwa-design.md.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Map a request URL to a file under `dir`, or None (→ 404). Rejects any `..` segment (no path
/// traversal). Falls back to `index.html` for extensionless routes (SPA), so a deep link / a
/// scanned `/?relay=…` URL still loads the app shell.
pub fn resolve_static_path(dir: &Path, url: &str) -> Option<PathBuf> {
    let raw = url.split(['?', '#']).next().unwrap_or("");
    let rel = raw.trim_start_matches('/');
    if rel.split('/').any(|seg| seg == "..") {
        return None;
    }
    let candidate = if rel.is_empty() {
        dir.join("index.html")
    } else {
        dir.join(rel)
    };
    if candidate.is_file() {
        return Some(candidate);
    }
    // SPA fallback: a route with no file extension serves index.html.
    if !rel.contains('.') {
        let index = dir.join("index.html");
        if index.is_file() {
            return Some(index);
        }
    }
    None
}

/// Serve the built PWA over plain http from `dir` (blocking; run on its own thread). This is the
/// LAN "download the app" host — phones fetch the static shell here, then connect peer-to-peer.
/// `application/wasm` and the other MIME types come from the file extension (browsers require the
/// correct wasm type to instantiate).
pub fn serve_static_blocking(addr: &str, dir: PathBuf) {
    let server = match tiny_http::Server::http(addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("static host failed to bind {addr}: {e}");
            return;
        }
    };
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

#[derive(Deserialize)]
struct Join {
    #[serde(rename = "type")]
    kind: String,
    room: String,
    /// If set, the relay replies with the client's WebRTC role on join (so two peers don't
    /// both try to offer): the first member in a room becomes the answerer, the next the
    /// offerer. Absent (e.g. the native gateway, which has a fixed role) → no reply.
    want_role: Option<bool>,
    /// Multi-peer (mesh) mode: the relay assigns this client a numeric id, tells it its id + the
    /// ids of existing mesh members (`welcome`), notifies others when it joins (`peer-joined`) /
    /// leaves (`peer-left`), and routes each data frame to the single peer named by its `to` field
    /// (tagging the frame with `from`). One hub can thus serve many spokes in one room — no
    /// 2-peer limit. Absent/false → the legacy broadcast behaviour (unchanged).
    mesh: Option<bool>,
}

type Tx = mpsc::UnboundedSender<Message>;

/// A connected room member.
#[derive(Clone)]
struct Member {
    id: usize,
    tx: Tx,
    mesh: bool,
}

#[derive(Default)]
struct Rooms {
    inner: Mutex<HashMap<String, Vec<Member>>>,
}

/// Accept connections forever, relaying signaling within rooms. Runs until the listener errors.
pub async fn serve(listener: TcpListener) {
    let rooms = Arc::new(Rooms::default());
    let next_id = AtomicUsize::new(0);
    while let Ok((stream, _)) = listener.accept().await {
        let rooms = Arc::clone(&rooms);
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(handle(stream, rooms, id));
    }
}

async fn handle(stream: TcpStream, rooms: Arc<Rooms>, id: usize) {
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    let (mut sink, mut source) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // One writer task drains this client's outbound queue (other peers push to `tx`).
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });

    let mut my_room: Option<String> = None;
    let mut my_mesh = false;
    while let Some(Ok(msg)) = source.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        match &my_room {
            // Not joined yet: the first frame must be a join (anything else is ignored).
            None => {
                if let Ok(j) = serde_json::from_str::<Join>(&text) {
                    if j.kind == "join" {
                        my_mesh = j.mesh.unwrap_or(false);
                        // Register; in mesh mode, also gather existing mesh peers + announce us.
                        let (was_present, peers) = {
                            let mut guard = rooms.inner.lock().unwrap();
                            let members = guard.entry(j.room.clone()).or_default();
                            let was_present = !members.is_empty();
                            let peers: Vec<usize> =
                                members.iter().filter(|m| m.mesh).map(|m| m.id).collect();
                            if my_mesh {
                                let joined = format!("{{\"kind\":\"peer-joined\",\"peer\":{id}}}");
                                for m in members.iter().filter(|m| m.mesh) {
                                    let _ = m.tx.send(Message::Text(joined.clone()));
                                }
                            }
                            members.push(Member {
                                id,
                                tx: tx.clone(),
                                mesh: my_mesh,
                            });
                            (was_present, peers)
                        };
                        my_room = Some(j.room);
                        if my_mesh {
                            let list = peers
                                .iter()
                                .map(|p| p.to_string())
                                .collect::<Vec<_>>()
                                .join(",");
                            let _ = tx.send(Message::Text(format!(
                                "{{\"kind\":\"welcome\",\"you\":{id},\"peers\":[{list}]}}"
                            )));
                        } else if j.want_role.unwrap_or(false) {
                            // Legacy 2-peer: first member is the answerer, the next the offerer.
                            let role = format!("{{\"kind\":\"role\",\"initiator\":{was_present}}}");
                            let _ = tx.send(Message::Text(role));
                        }
                    }
                }
            }
            // Joined: relay this frame.
            Some(room) => {
                let members = rooms
                    .inner
                    .lock()
                    .unwrap()
                    .get(room)
                    .cloned()
                    .unwrap_or_default();
                if my_mesh {
                    // Addressed routing: deliver only to the peer named by `to`, tagged with `from`.
                    let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    let target = val.get("to").and_then(|t| t.as_u64()).map(|n| n as usize);
                    let Some(target) = target else { continue };
                    if let Some(obj) = val.as_object_mut() {
                        obj.insert("from".into(), serde_json::json!(id));
                    }
                    let out = val.to_string();
                    for m in members.iter().filter(|m| m.id == target) {
                        let _ = m.tx.send(Message::Text(out.clone()));
                    }
                } else {
                    // Legacy broadcast to the other non-mesh members.
                    for m in members.iter().filter(|m| m.id != id && !m.mesh) {
                        let _ = m.tx.send(Message::Text(text.clone()));
                    }
                }
            }
        }
    }

    // Leave the room on disconnect; tell remaining mesh peers.
    if let Some(room) = my_room {
        let mut guard = rooms.inner.lock().unwrap();
        if let Some(members) = guard.get_mut(&room) {
            members.retain(|m| m.id != id);
            if my_mesh {
                let left = format!("{{\"kind\":\"peer-left\",\"peer\":{id}}}");
                for m in members.iter().filter(|m| m.mesh) {
                    let _ = m.tx.send(Message::Text(left.clone()));
                }
            }
            if members.is_empty() {
                guard.remove(&room);
            }
        }
    }
    writer.abort();
}

#[cfg(test)]
mod static_tests {
    use super::resolve_static_path;
    use std::fs;

    #[test]
    fn maps_routes_and_blocks_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("index.html"), b"<html></html>").unwrap();
        fs::create_dir(root.join("assets")).unwrap();
        fs::write(root.join("assets").join("app.js"), b"//js").unwrap();
        fs::write(root.join("app.wasm"), b"\0asm").unwrap();

        // Root + query-only URL → index.html.
        assert_eq!(
            resolve_static_path(root, "/"),
            Some(root.join("index.html"))
        );
        assert_eq!(
            resolve_static_path(root, "/?relay=ws://x&room=y"),
            Some(root.join("index.html"))
        );
        // An existing asset is served as-is.
        assert_eq!(
            resolve_static_path(root, "/assets/app.js"),
            Some(root.join("assets").join("app.js"))
        );
        assert_eq!(
            resolve_static_path(root, "/app.wasm"),
            Some(root.join("app.wasm"))
        );
        // Extensionless route with no file → SPA fallback to index.html.
        assert_eq!(
            resolve_static_path(root, "/some/route"),
            Some(root.join("index.html"))
        );
        // A missing file WITH an extension is a 404 (not an html fallback).
        assert_eq!(resolve_static_path(root, "/missing.wasm"), None);
        // Path traversal is rejected.
        assert_eq!(resolve_static_path(root, "/../secret"), None);
        assert_eq!(resolve_static_path(root, "/assets/../../etc/passwd"), None);
    }
}
