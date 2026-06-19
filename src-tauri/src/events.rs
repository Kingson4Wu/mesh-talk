//! Tauri events emitted to the frontend by the node. (Legacy
//! message/contact/network/file-transfer events were retired with the legacy stack.)

use crate::eventlog::event::EventId;
use tauri::{Emitter, Runtime};

/// A received DM, surfaced to the `/` UI.
pub const EVENT_DM_RECEIVED: &str = "dm-received";
/// A received channel message.
pub const EVENT_CHANNEL_MESSAGE: &str = "channel-message";
/// A received file (DM or channel).
pub const EVENT_FILE_RECEIVED: &str = "file-received";

#[derive(serde::Serialize, Clone)]
pub struct DmReceivedEvent {
    pub from: String,
    pub from_name: String,
    pub text: String,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
}

#[derive(serde::Serialize, Clone)]
pub struct ChannelMessageEvent {
    pub channel_id: String, // hex
    pub channel_name: String,
    pub from: String,
    pub text: String,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
}

#[derive(serde::Serialize, Clone)]
pub struct FileReceivedEvent {
    pub conv: String, // hex (channel id for a channel file; the DM conv otherwise)
    pub from: String, // sender user-id
    pub name: String,
    pub size: u64,
    pub file_conv: String, // hex — pass to save
}

/// Emit a received DM to the frontend (text decoded lossily for display).
pub fn emit_dm_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    from: String,
    from_name: String,
    text: Vec<u8>,
    reply_to: Option<EventId>,
) {
    let event = DmReceivedEvent {
        from,
        from_name,
        text: String::from_utf8_lossy(&text).into_owned(),
        reply_to: reply_to.map(|id| hex::encode(id.as_bytes())),
    };
    if let Err(e) = app_handle.emit(EVENT_DM_RECEIVED, event) {
        log::error!("Failed to emit dm event: {e}");
    }
}

pub fn emit_channel_message<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    channel_id: String,
    channel_name: String,
    from: String,
    text: Vec<u8>,
    reply_to: Option<EventId>,
) {
    let event = ChannelMessageEvent {
        channel_id,
        channel_name,
        from,
        text: String::from_utf8_lossy(&text).into_owned(),
        reply_to: reply_to.map(|id| hex::encode(id.as_bytes())),
    };
    if let Err(e) = app_handle.emit(EVENT_CHANNEL_MESSAGE, event) {
        log::error!("Failed to emit channel event: {e}");
    }
}

pub fn emit_file_received<R: Runtime>(
    app_handle: &tauri::AppHandle<R>,
    conv: String,
    from: String,
    name: String,
    size: u64,
    file_conv: String,
) {
    let event = FileReceivedEvent {
        conv,
        from,
        name,
        size,
        file_conv,
    };
    if let Err(e) = app_handle.emit(EVENT_FILE_RECEIVED, event) {
        log::error!("Failed to emit file event: {e}");
    }
}
