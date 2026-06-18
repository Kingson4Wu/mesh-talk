//! Tauri IPC commands backing the redesign `/redesign` route. They delegate to a
//! per-session [`RedesignRuntime`] held in managed [`RedesignState`] (populated on
//! login, cleared on logout). All are thin pass-throughs over the node API.

use crate::eventlog::event::ConversationId;
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
#[derive(Serialize)]
pub struct PeerInfo {
    pub user_id: String,
    pub name: String,
    pub addr: String,
    pub post_office: bool,
}

/// One merged history line (sent or received) for display.
#[derive(Serialize)]
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
    // Cap the page size so a frontend accident (e.g. a huge JS number) can't
    // request an unbounded scan; the node truncates to this anyway.
    let limit = limit.min(500);
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

/// A channel as shown in the redesign UI.
#[derive(Serialize)]
pub struct ChannelInfo {
    pub channel_id: String, // hex
    pub name: String,
    pub member_count: usize,
}

fn parse_channel_id(hex_id: &str) -> Result<ConversationId, String> {
    let bytes = hex::decode(hex_id).map_err(|_| "invalid channel id".to_string())?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "channel id must be 32 bytes".to_string())?;
    Ok(ConversationId::new(arr))
}

#[tauri::command]
pub async fn redesign_list_channels(
    state: tauri::State<'_, RedesignState>,
) -> Result<Vec<ChannelInfo>, String> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .list_channels()
        .into_iter()
        .map(|c| ChannelInfo {
            channel_id: hex::encode(c.id.as_bytes()),
            name: c.name,
            member_count: c.member_count,
        })
        .collect())
}

#[tauri::command]
pub async fn redesign_create_channel(
    state: tauri::State<'_, RedesignState>,
    name: String,
    member_ids: Vec<String>,
) -> Result<String, String> {
    let (node, members) = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        let mut members = Vec::new();
        for uid in &member_ids {
            let p = rt
                .peer_public(uid)
                .ok_or_else(|| format!("unknown peer: {uid}"))?;
            members.push(p);
        }
        (rt.handle(), members)
    };
    let id = node
        .create_channel(&name, members)
        .await
        .map_err(|e| e.to_string())?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn redesign_send_channel_message(
    state: tauri::State<'_, RedesignState>,
    channel_id: String,
    text: String,
) -> Result<(), String> {
    let id = parse_channel_id(&channel_id)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.send_channel_message(id, text.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn redesign_channel_history(
    state: tauri::State<'_, RedesignState>,
    channel_id: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, String> {
    let limit = limit.min(500);
    let id = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .channel_history(id, limit)
        .into_iter()
        .map(|h| HistoryItem {
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
        })
        .collect())
}
