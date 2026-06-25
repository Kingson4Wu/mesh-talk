//! E2E test harness: a headless node that runs the REAL desktop [`NodeRuntime`] — the exact code
//! path the Tauri app uses (discovery, accept loop, presence gossip incl. gossip-on-newcomer, and
//! optionally `spawn_gateway_hub` → `run_gateway_hub`/`run_mesh_hub`). This lets the Playwright e2e
//! drive the literal desktop runtime (not the simpler `mesh-talk-node` REPL) without a GUI, so the
//! "phone sees a LAN PC via the hub" test exercises the code your desktop actually runs.
//!
//! `/dir` on stdin dumps the gossiped presence directory; `/quit` exits.
use clap::Parser;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(Parser)]
struct Args {
    /// Base data dir (the runtime creates `<data_dir>/accounts/<account_id>/`).
    #[arg(long)]
    data_dir: String,
    /// Account namespace (just scopes the data dir; the node derives its own crypto id).
    #[arg(long)]
    account_id: String,
    /// Display name advertised to peers.
    #[arg(long)]
    name: String,
    #[arg(long, default_value = "pw")]
    password: String,
    /// UDP discovery port (must match across LAN peers).
    #[arg(long)]
    discovery_port: u16,
    /// If set, ALSO host a gateway hub on this relay (the desktop's phone-serving path).
    #[arg(long)]
    signal_url: Option<String>,
    #[arg(long, default_value = "mesh-talk")]
    gateway_room: String,
}

#[tokio::main]
async fn main() {
    // Emit the node's [mesh] logs (gossip diagnostics, DIAL/SYNC failures) to stderr so the e2e can
    // OBSERVE them. Default to info; override with RUST_LOG.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let rt = mesh_talk_core::node::NodeRuntime::start(
        std::path::Path::new(&args.data_dir),
        &args.account_id,
        &args.name,
        &args.password,
        args.discovery_port,
        |_dm| {},
        |_ch| {},
        |_f| {},
        |_p| {},
    )
    .await
    .expect("runtime starts");
    // Same startup line shape as mesh-talk-node, so the e2e can grab the user_id.
    println!("node {} listening", rt.user_id());

    #[cfg(feature = "gateway")]
    if let Some(url) = args.signal_url.clone() {
        rt.spawn_gateway_hub(url.clone(), args.gateway_room.clone());
        println!("gateway: hub on {url} room {}", args.gateway_room);
    }

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        match line.trim() {
            "/dir" => {
                let dir = rt.gossip_directory();
                for (id, _, name, _) in &dir {
                    println!("dir {id} {name}");
                }
                println!("dir-end {}", dir.len());
            }
            "/quit" => return,
            _ => {}
        }
    }
    // stdin hit EOF (e.g. a detached container with no stdin attached) — DON'T exit; the node's
    // background tasks (discovery, gossip, gateway hub) must keep running for the lifetime of the
    // container. Only `/quit` (above) or a kill ends it.
    std::future::pending::<()>().await
}
