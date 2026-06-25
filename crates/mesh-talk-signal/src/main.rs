//! Run the Mesh-Talk signaling relay. A single self-hostable binary:
//!   mesh-talk-signal --port 47480
//!
//! For a pure-LAN setup (no TLS, no GitHub Pages), also host the PWA so phones can download it
//! on the LAN, in one command:
//!   mesh-talk-signal --serve-dir ./frontend/dist --port 47480 --http-port 8080
//! then open/scan  http://<lan-ip>:8080/?relay=ws://<lan-ip>:47480&room=<room>

use clap::Parser;
use std::net::{IpAddr, UdpSocket};
use std::path::PathBuf;
use tokio::net::TcpListener;

/// Best-effort LAN IP of this machine: open a UDP socket "toward" an off-link address (no packet
/// is actually sent) and read back the local address the OS would route from.
fn lan_ip() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

#[derive(Parser)]
#[command(about = "WebSocket signaling relay for Mesh-Talk's browser gateway")]
struct Args {
    /// TCP port the ws signaling relay listens on.
    #[arg(long, default_value_t = 47480)]
    port: u16,
    /// Address to bind (use 0.0.0.0 to accept LAN/remote clients).
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    /// Optional: also serve the built PWA from this directory over http, so phones can download
    /// the app on the LAN. Turns this into a one-command LAN hub (app host + relay).
    #[arg(long)]
    serve_dir: Option<PathBuf>,
    /// Port for the PWA http host (used with --serve-dir).
    #[arg(long, default_value_t = 8080)]
    http_port: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if let Some(dir) = args.serve_dir.clone() {
        let http_addr = format!("{}:{}", args.host, args.http_port);
        let msg = format!(
            "mesh-talk-signal hosting the PWA from {} at http://{http_addr}/",
            dir.display()
        );
        log::info!("{msg}");
        eprintln!("{msg}");
        // Print a ready-to-scan join URL (+ a terminal QR) with the detected LAN IP.
        if let Some(ip) = lan_ip() {
            let url = format!(
                "http://{ip}:{}/?relay=ws://{ip}:{}&room=mesh-talk",
                args.http_port, args.port
            );
            eprintln!("  → join from a phone on the same Wi-Fi: {url}");
            if let Ok(code) = qrcode::QrCode::new(url.as_bytes()) {
                let qr = code
                    .render::<qrcode::render::unicode::Dense1x2>()
                    .quiet_zone(true)
                    .build();
                eprintln!("\n{qr}\n  (scan the QR above with the phone's camera)\n");
            }
        }
        // tiny_http is blocking; run it off the tokio runtime.
        std::thread::spawn(move || mesh_talk_signal::serve_static_blocking(&http_addr, dir));
    }

    let addr = format!("{}:{}", args.host, args.port);
    let listener = TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    log::info!("mesh-talk-signal listening on {addr}");
    eprintln!("mesh-talk-signal listening on {addr}");
    mesh_talk_signal::serve(listener).await;
}
