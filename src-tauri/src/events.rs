//! Tauri events emitted to the frontend by the node. (Legacy
//! message/contact/network/file-transfer events were retired with the legacy stack.)

use crate::settings::SettingsState;
use mesh_talk_core::eventlog::event::EventId;
use tauri::{Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

/// How many characters of a message body we surface in a notification preview.
const PREVIEW_LEN: usize = 80;

/// Fire a native notification for an incoming item, but only when it would
/// actually be useful: notifications are enabled in settings AND the main window
/// is not currently focused (no point pinging the user about a message they can
/// already see). Best-effort — failures are logged, never propagated.
///
/// `body` must contain only what the UI already shows (decoded plaintext / a
/// generic "sent a file"); never raw ciphertext or anything more than the chat view.
fn notify_if_unfocused<R: Runtime>(app_handle: &tauri::AppHandle<R>, title: &str, body: &str) {
    // Respect the user's toggle.
    let enabled = app_handle
        .try_state::<SettingsState>()
        .map(|s| s.get().notifications)
        .unwrap_or(true);
    if !enabled {
        return;
    }

    // Skip when the window is focused — the message is already on screen.
    if let Some(window) = app_handle.get_webview_window("main") {
        if window.is_focused().unwrap_or(false) {
            return;
        }
    }

    if let Err(e) = app_handle
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        log::warn!("Failed to show notification: {e}");
    }
}

/// Truncate a preview to a sane length on a char boundary, appending an ellipsis.
fn preview(text: &str) -> String {
    if text.chars().count() <= PREVIEW_LEN {
        return text.to_string();
    }
    let truncated: String = text.chars().take(PREVIEW_LEN).collect();
    format!("{truncated}…")
}

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
    notify_if_unfocused(app_handle, &event.from_name, &preview(&event.text));
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
    notify_if_unfocused(
        app_handle,
        &format!("# {}", event.channel_name),
        &preview(&event.text),
    );
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
    notify_if_unfocused(app_handle, &event.from, "sent a file");
    if let Err(e) = app_handle.emit(EVENT_FILE_RECEIVED, event) {
        log::error!("Failed to emit file event: {e}");
    }
}
