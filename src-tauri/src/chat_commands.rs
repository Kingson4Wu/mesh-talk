//! Tauri IPC commands backing the `/` route. They delegate to a
//! per-session [`NodeRuntime`] held in managed [`NodeState`] (populated on
//! login, cleared on logout). All are thin pass-throughs over the node API.

use crate::commands::CommandError;
use mesh_talk_core::eventlog::event::{ConversationId, EventId};
use mesh_talk_core::node::NodeRuntime;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Managed state holding the current session's node runtime (`None` until login).
#[derive(Clone)]
pub struct NodeState(pub Arc<Mutex<Option<NodeRuntime>>>);

impl NodeState {
    pub fn empty() -> Self {
        NodeState(Arc::new(Mutex::new(None)))
    }
}

impl Default for NodeState {
    fn default() -> Self {
        Self::empty()
    }
}

/// A peer as shown in the roster.
#[derive(Serialize)]
pub struct PeerInfo {
    pub user_id: String,
    pub name: String,
    pub addr: String,
    pub post_office: bool,
    /// The account this device belongs to (devices sharing it are one user's). The
    /// UI keys conversations by this so a multi-device contact is one conversation.
    pub account_id: Option<String>,
}

/// One merged history line (sent or received) for display.
#[derive(Serialize)]
pub struct HistoryItem {
    pub id: Option<String>, // hex EventId; null when there is no stable id (see From impl)
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
}

impl From<mesh_talk_core::node::HistoryEntry> for HistoryItem {
    fn from(h: mesh_talk_core::node::HistoryEntry) -> Self {
        // A sent entry whose event isn't yet in the log gets the all-zero sentinel id; surface
        // it as null (like a pending message) so the UI never targets a react/reply at a
        // bogus id, instead of leaking the sentinel as a real hex id.
        let id = if h.id.as_bytes() == &[0u8; 32] {
            None
        } else {
            Some(hex::encode(h.id.as_bytes()))
        };
        HistoryItem {
            id,
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
            reply_to: h.reply_to.map(|id| hex::encode(id.as_bytes())),
        }
    }
}

/// Aggregated reaction for display.
#[derive(Serialize)]
pub struct ReactionInfo {
    pub target: String, // hex EventId
    pub emoji: String,
    pub who: Vec<String>,
}

/// A channel member as shown in the chat UI.
#[derive(Serialize)]
pub struct ChannelMemberInfo {
    pub user_id: String,
    pub name: String,
}

const NOT_STARTED: &str = "node not started";

#[tauri::command]
pub async fn my_id(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt.user_id().to_string())
}

#[tauri::command]
pub async fn list_peers(state: tauri::State<'_, NodeState>) -> Result<Vec<PeerInfo>, CommandError> {
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
            account_id: p.account_id,
        })
        .collect())
}

#[tauri::command]
pub async fn send_dm(
    state: tauri::State<'_, NodeState>,
    recipient: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    // Snapshot the node handle, then release the state lock before the .await send.
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    node.send_dm_reply(&recipient, text.as_bytes(), reply)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn history(
    state: tauri::State<'_, NodeState>,
    peer: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    // Cap the page size so a frontend accident (e.g. a huge JS number) can't
    // request an unbounded scan; the node truncates to this anyway.
    let limit = limit.min(500);
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    let public = rt
        .peer_public(&peer)
        .ok_or_else(|| CommandError::Validation(format!("unknown peer: {peer}")))?;
    Ok(rt
        .history(&public, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect())
}

#[tauri::command]
pub async fn account_id(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt.account_id().to_string())
}

#[tauri::command]
pub async fn send_to_account(
    state: tauri::State<'_, NodeState>,
    account: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    // Snapshot the node handle, then release the state lock before the .await send.
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    node.send_to_account(&account, text.as_bytes(), reply)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn account_history(
    state: tauri::State<'_, NodeState>,
    account: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    let limit = limit.min(500);
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .account_history(&account, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect())
}

#[tauri::command]
pub async fn start_linking(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt.start_linking())
}

#[tauri::command]
pub async fn stop_linking(state: tauri::State<'_, NodeState>) -> Result<(), CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    rt.stop_linking();
    Ok(())
}

#[tauri::command]
pub async fn link_device(
    state: tauri::State<'_, NodeState>,
    peer: String,
    code: String,
) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    rt.link_device(&peer, &code)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn rekey_account(state: tauri::State<'_, NodeState>) -> Result<String, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    rt.rekey_account().map_err(CommandError::from)
}

/// An account (group of devices) as shown in the chat UI.
#[derive(Serialize)]
pub struct AccountInfo {
    pub account_id: String,
    pub device_count: usize,
    pub names: Vec<String>,
}

#[tauri::command]
pub async fn list_accounts(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<AccountInfo>, CommandError> {
    use std::collections::BTreeMap;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    let mut by_account: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in rt.peers() {
        if let Some(acct) = p.account_id {
            by_account.entry(acct).or_default().push(p.name);
        }
    }
    Ok(by_account
        .into_iter()
        .map(|(account_id, names)| AccountInfo {
            device_count: names.len(),
            names,
            account_id,
        })
        .collect())
}

/// A channel as shown in the chat UI.
#[derive(Serialize)]
pub struct ChannelInfo {
    pub channel_id: String, // hex
    pub name: String,
    pub member_count: usize,
}

fn parse_channel_id(hex_id: &str) -> Result<ConversationId, CommandError> {
    let bytes =
        hex::decode(hex_id).map_err(|_| CommandError::Validation("invalid channel id".into()))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| CommandError::Validation("channel id must be 32 bytes".into()))?;
    Ok(ConversationId::new(arr))
}

fn parse_event_id(hex_id: &str) -> Result<EventId, CommandError> {
    let bytes =
        hex::decode(hex_id).map_err(|_| CommandError::Validation("invalid event id".into()))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| CommandError::Validation("event id must be 32 bytes".into()))?;
    Ok(EventId::new(arr))
}

fn to_reaction_infos(views: Vec<mesh_talk_core::node::ReactionView>) -> Vec<ReactionInfo> {
    views
        .into_iter()
        .map(|v| ReactionInfo {
            target: v.target,
            emoji: v.emoji,
            who: v.who,
        })
        .collect()
}

#[tauri::command]
pub async fn list_channels(
    state: tauri::State<'_, NodeState>,
) -> Result<Vec<ChannelInfo>, CommandError> {
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
pub async fn channel_members(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<Vec<ChannelMemberInfo>, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    // Build a user_id -> display name map from the known roster so members show a
    // friendly name when we know one; otherwise fall back to the user_id.
    let names: std::collections::HashMap<String, String> = rt
        .peers()
        .into_iter()
        .map(|p| (p.public.user_id(), p.name))
        .collect();
    Ok(rt
        .channel_members(channel)
        .into_iter()
        .map(|p| {
            let user_id = p.user_id();
            let name = names
                .get(&user_id)
                .cloned()
                .unwrap_or_else(|| user_id.clone());
            ChannelMemberInfo { user_id, name }
        })
        .collect())
}

#[tauri::command]
pub async fn create_channel(
    state: tauri::State<'_, NodeState>,
    name: String,
    member_ids: Vec<String>,
) -> Result<String, CommandError> {
    let (node, members) = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        let mut members = Vec::new();
        for uid in &member_ids {
            let p = rt
                .peer_public(uid)
                .ok_or_else(|| CommandError::Validation(format!("unknown peer: {uid}")))?;
            members.push(p);
        }
        (rt.handle(), members)
    };
    let id = node
        .create_channel(&name, members)
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn add_channel_member(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let (node, member) = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        let member = rt
            .peer_public(&member_id)
            .ok_or_else(|| CommandError::Validation(format!("unknown peer: {member_id}")))?;
        (rt.handle(), member)
    };
    node.add_channel_member(channel, member)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn remove_channel_member(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    member_id: String,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.remove_channel_member(channel, &member_id)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn send_channel_message(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    text: String,
    reply_to: Option<String>,
) -> Result<(), CommandError> {
    let id = parse_channel_id(&channel_id)?;
    let reply = match reply_to {
        Some(h) => Some(parse_event_id(&h)?),
        None => None,
    };
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.send_channel_message_reply(id, text.as_bytes(), reply)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn send_file_dm(
    state: tauri::State<'_, NodeState>,
    recipient: String,
    path: String,
) -> Result<String, CommandError> {
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    let id = node
        .send_file_dm(&recipient, std::path::Path::new(&path))
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn send_file_to_account(
    state: tauri::State<'_, NodeState>,
    account: String,
    path: String,
) -> Result<String, CommandError> {
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    let id = node
        .send_file_to_account(&account, std::path::Path::new(&path))
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(id.as_bytes()))
}

#[tauri::command]
pub async fn send_file_channel(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    path: String,
) -> Result<String, CommandError> {
    let id = parse_channel_id(&channel_id)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    let file_conv = node
        .send_file_channel(id, std::path::Path::new(&path))
        .await
        .map_err(CommandError::from)?;
    Ok(hex::encode(file_conv.as_bytes()))
}

#[tauri::command]
pub async fn save_file(
    state: tauri::State<'_, NodeState>,
    file_conv: String,
    dest: String,
) -> Result<(), CommandError> {
    let id = parse_channel_id(&file_conv)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    // save_file is synchronous (reads chunk events, decrypts, writes) — run it on a
    // blocking thread so it doesn't stall the async runtime on a large file.
    tokio::task::spawn_blocking(move || node.save_file(id, std::path::Path::new(&dest)))
        .await
        .map_err(|e| format!("join error: {e}"))?
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn channel_history(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    limit: usize,
) -> Result<Vec<HistoryItem>, CommandError> {
    let limit = limit.min(500);
    let id = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .channel_history(id, limit)
        .into_iter()
        .map(HistoryItem::from)
        .collect())
}

#[tauri::command]
pub async fn react_dm(
    state: tauri::State<'_, NodeState>,
    recipient: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let id = parse_event_id(&target)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.react_dm(&recipient, id, &emoji, remove)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn react_channel(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let id = parse_event_id(&target)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.react_channel(channel, id, &emoji, remove)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn reactions(
    state: tauri::State<'_, NodeState>,
    peer: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    let public = rt
        .peer_public(&peer)
        .ok_or_else(|| CommandError::Validation(format!("unknown peer: {peer}")))?;
    Ok(to_reaction_infos(rt.reactions_dm(&public)))
}

#[tauri::command]
pub async fn channel_reactions(
    state: tauri::State<'_, NodeState>,
    channel_id: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let channel = parse_channel_id(&channel_id)?;
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(to_reaction_infos(rt.channel_reactions(channel)))
}

#[tauri::command]
pub async fn react_account(
    state: tauri::State<'_, NodeState>,
    account: String,
    target: String,
    emoji: String,
    remove: bool,
) -> Result<(), CommandError> {
    let id = parse_event_id(&target)?;
    let node = {
        let guard = state.0.lock().await;
        let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
        rt.handle()
    };
    node.react_to_account(&account, id, &emoji, remove)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn account_reactions(
    state: tauri::State<'_, NodeState>,
    account: String,
) -> Result<Vec<ReactionInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(to_reaction_infos(rt.account_reactions(&account)))
}

/// A search result hit for display in the UI.
#[derive(Serialize)]
pub struct SearchHitInfo {
    pub is_channel: bool,
    pub target: String,
    pub label: String,
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
}

#[tauri::command]
pub async fn search(
    state: tauri::State<'_, NodeState>,
    query: String,
) -> Result<Vec<SearchHitInfo>, CommandError> {
    let guard = state.0.lock().await;
    let rt = guard.as_ref().ok_or_else(|| NOT_STARTED.to_string())?;
    Ok(rt
        .search(&query)
        .into_iter()
        .map(|h| SearchHitInfo {
            is_channel: h.is_channel,
            target: h.target,
            label: h.label,
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_channel_id_accepts_64_hex_chars() {
        let id = parse_channel_id(&"ab".repeat(32)).expect("valid id");
        assert_eq!(id.as_bytes(), &[0xab; 32]);
    }

    #[test]
    fn parse_channel_id_rejects_wrong_length() {
        assert!(parse_channel_id(&"ab".repeat(16)).is_err()); // 16 bytes, not 32
        assert!(parse_channel_id("").is_err());
    }

    #[test]
    fn parse_channel_id_rejects_non_hex() {
        assert!(parse_channel_id(&"zz".repeat(32)).is_err()); // not hex
        assert!(parse_channel_id("abc").is_err()); // odd-length hex
    }

    #[test]
    fn parse_event_id_accepts_valid_and_rejects_bad_input() {
        assert_eq!(
            parse_event_id(&"cd".repeat(32)).expect("valid").as_bytes(),
            &[0xcd; 32]
        );
        assert!(parse_event_id(&"cd".repeat(10)).is_err()); // too short
        assert!(parse_event_id(&"gg".repeat(32)).is_err()); // not hex
    }
}
