// Pragmatic, intentional lint allowances (the substantive clippy lints stay on):
//  - module_inception: test modules are `mod tests` inside `*/tests.rs`.
//  - too_many_arguments: a few constructors; refactor tracked separately.
//  - assertions_on_constants: placeholder "can construct" smoke tests.
//  - single_match: a couple of intentional single-arm matches.
//  - manual_flatten: a nested `if let Ok` loop kept for readability.
#![allow(
    clippy::module_inception,
    clippy::too_many_arguments,
    clippy::assertions_on_constants,
    clippy::single_match,
    clippy::manual_flatten
)]

// Protocol/core modules live in the `mesh_talk_core` crate; this crate is the
// Tauri desktop shell over it.
pub mod chat_commands;
pub mod commands;
pub mod events;
pub mod logger;
pub mod perf;
pub mod services;
pub mod state;
pub mod tray;

use crate::state::AppState;
use std::sync::Arc;
use tauri::Manager;

/// The user's home directory, cross-platform: `HOME` on Unix/macOS, `USERPROFILE` on
/// Windows (where `HOME` is normally unset). Anchors the app's data + logs at `~/.mesh-talk`.
pub(crate) fn user_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
}

/// Tauri application entry point. The serverless node is the whole product now;
/// it starts per-session on login (see `commands::login` → `spawn_node_runtime`).
pub fn run_tauri() {
    let _timer = perf_monitor!("application_startup");
    log::info!("Starting Mesh-Talk desktop runtime");

    // Data directory + file manager (~/.mesh-talk), shared by the auth keystore.
    lazy_static::lazy_static! {
        static ref FILE_MANAGER: Arc<mesh_talk_core::storage::file_manager::FileManager> = {
            let data_path = user_home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".mesh-talk");
            log::info!("Data path: {}", data_path.to_str().unwrap_or(".mesh-talk"));
            Arc::new(mesh_talk_core::storage::file_manager::FileManager::new(data_path))
        };
    }
    let _file_manager = FILE_MANAGER.clone();

    // Auth (login/register) is the only stateful service the shell needs; the
    // node manages its own per-account stores out of `NodeState`.
    crate::services::auth_service::AuthService::init_global(FILE_MANAGER.as_ref().clone());
    let app_state = AppState::new(crate::services::auth_service::AuthService::global().clone());

    log::info!("No sockets are bound until a user signs in.");

    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .setup(move |app| {
            if let Err(e) = crate::logger::init_logging(app.handle()) {
                log::error!("Failed to initialize logging: {e}");
            }
            crate::tray::create_system_tray(&app.handle().clone())?;

            #[cfg(debug_assertions)]
            {
                let window = app.get_webview_window("main").unwrap();
                window.open_devtools();
                window.close_devtools();
            }
            Ok(())
        })
        .manage(app_state)
        .manage(crate::chat_commands::NodeState::empty())
        .invoke_handler(tauri::generate_handler![
            commands::login,
            commands::logout,
            commands::register,
            commands::adopt_linked_account,
            crate::chat_commands::my_id,
            crate::chat_commands::list_peers,
            crate::chat_commands::send_dm,
            crate::chat_commands::history,
            crate::chat_commands::account_id,
            crate::chat_commands::send_to_account,
            crate::chat_commands::account_history,
            crate::chat_commands::start_linking,
            crate::chat_commands::stop_linking,
            crate::chat_commands::link_device,
            crate::chat_commands::rekey_account,
            crate::chat_commands::list_accounts,
            crate::chat_commands::send_file_to_account,
            crate::chat_commands::react_account,
            crate::chat_commands::account_reactions,
            crate::chat_commands::list_channels,
            crate::chat_commands::create_channel,
            crate::chat_commands::add_channel_member,
            crate::chat_commands::remove_channel_member,
            crate::chat_commands::channel_members,
            crate::chat_commands::send_channel_message,
            crate::chat_commands::channel_history,
            crate::chat_commands::send_file_dm,
            crate::chat_commands::send_file_channel,
            crate::chat_commands::save_file,
            crate::chat_commands::react_dm,
            crate::chat_commands::react_channel,
            crate::chat_commands::reactions,
            crate::chat_commands::channel_reactions,
            crate::chat_commands::search
        ])
        .run(tauri::generate_context!())
        .map_err(|e| log::error!("Error while running tauri application: {e}"))
        .unwrap_or(());
}
