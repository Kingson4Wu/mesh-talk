//! Tauri IPC commands backing the redesign `/redesign` route. They delegate to a
//! per-session [`RedesignRuntime`] held in managed [`RedesignState`] (populated on
//! login, cleared on logout). All are thin pass-throughs over the node API.

use crate::node::runtime::RedesignRuntime;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Managed state holding the current session's redesign runtime (`None` until login).
#[derive(Clone)]
pub struct RedesignState(pub Arc<Mutex<Option<RedesignRuntime>>>);

impl RedesignState {
    pub fn empty() -> Self {
        RedesignState(Arc::new(Mutex::new(None)))
    }
}

impl Default for RedesignState {
    fn default() -> Self {
        Self::empty()
    }
}

/// A peer as shown in the redesign roster.
#[derive(Serialize, Clone)]
pub struct PeerInfo {
    pub user_id: String,
    pub name: String,
    pub addr: String,
    pub post_office: bool,
}

/// One merged history line (sent or received) for display.
#[derive(Serialize, Clone)]
pub struct HistoryItem {
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
}

const NOT_STARTED: &str = "redesign node not started";

#[tauri::command]
pub async fn redesign_my_id(state: tauri::State<'_, RedesignState>) -> Result<String, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt.user_id().to_string())
}

#[tauri::command]
pub async fn redesign_list_peers(
    state: tauri::State<'_, RedesignState>,
) -> Result<Vec<PeerInfo>, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .peers()
        .into_iter()
        .map(|p| PeerInfo {
            user_id: p.public.user_id(),
            name: p.name,
            addr: p.addr.to_string(),
            post_office: p.post_office,
        })
        .collect())
}

#[tauri::command]
pub async fn redesign_send_dm(
    state: tauri::State<'_, RedesignState>,
    recipient: String,
    text: String,
) -> Result<(), String> {
    // Snapshot the node handle, then release the state lock before the .await send.
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.send_dm(&recipient, text.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn redesign_history(
    state: tauri::State<'_, RedesignState>,
    peer: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    let public = rt
        .peer_public(&peer)
        .ok_or_else(|| format!("unknown peer: {peer}"))?;
    Ok(rt
        .history(&public, limit)
        .into_iter()
        .map(|h| HistoryItem {
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
        })
        .collect())
}
