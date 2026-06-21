//! Background-presence settings: the two non-autostart toggles that the Rust
//! close-handler and notification path read, plus their persistence.
//!
//! `launch_at_login` is intentionally NOT stored here — it is owned by
//! `tauri-plugin-autostart` (the source of truth is the OS launch-agent), so the
//! frontend reads/writes it through the plugin's own enable/disable/is-enabled.

use crate::commands::CommandError;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tauri::Manager;

/// The persisted, user-facing toggles (both default on: a messenger should keep
/// running in the background and tell you when something arrives).
///
/// This is a genuine self-describing JSON-at-rest format (`settings.json`, read via
/// `serde_json::from_str`), so per-field `#[serde(default)]` IS the right forward-compat
/// tool: a `settings.json` written by an OLDER build that lacks a field added LATER
/// still loads (the missing field falls back to its default) instead of failing the
/// whole parse and silently resetting every toggle. New fields MUST carry a
/// `#[serde(default = ...)]` (or a `Default`-backed `#[serde(default)]`) for this.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AppSettings {
    /// Closing the window hides it to the tray instead of quitting.
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    /// Show a native notification on incoming messages when the window isn't focused.
    #[serde(default = "default_true")]
    pub notifications: bool,
}

/// Default for both toggles (a messenger should run in the background and notify).
fn default_true() -> bool {
    true
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            minimize_to_tray: true,
            notifications: true,
        }
    }
}

/// Managed Tauri state wrapping [`AppSettings`] so the close-handler and the
/// notification path can read it cheaply behind a `Mutex`.
#[derive(Clone, Default)]
pub struct SettingsState(Arc<Mutex<AppSettings>>);

impl SettingsState {
    pub fn get(&self) -> AppSettings {
        *self.0.lock().unwrap()
    }
    pub fn set(&self, value: AppSettings) {
        *self.0.lock().unwrap() = value;
    }
}

/// `<app config dir>/settings.json`, the persistence path for the two toggles.
fn settings_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("settings.json"))
}

/// Load persisted settings (defaults if the file is missing or unreadable) and
/// seed the managed state. Called once during setup.
pub fn load_into_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>, state: &SettingsState) {
    let loaded = settings_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<AppSettings>(&s).ok())
        .unwrap_or_default();
    state.set(loaded);
}

/// Persist settings to disk (best-effort; logged on failure).
fn save<R: tauri::Runtime>(app: &tauri::AppHandle<R>, value: &AppSettings) {
    let Some(path) = settings_path(app) else {
        log::warn!("No app config dir; settings not persisted");
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("Failed to persist settings: {e}");
            }
        }
        Err(e) => log::warn!("Failed to serialize settings: {e}"),
    }
}

/// Read the current two non-autostart toggles. (Launch-at-login is queried via
/// the autostart plugin's `is_enabled` on the frontend.)
#[tauri::command]
pub fn get_app_settings(state: tauri::State<'_, SettingsState>) -> AppSettings {
    state.get()
}

/// Update the two non-autostart toggles and persist them.
#[tauri::command]
pub fn set_app_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, SettingsState>,
    settings: AppSettings,
) -> Result<(), CommandError> {
    state.set(settings);
    save(&app, &settings);
    Ok(())
}
