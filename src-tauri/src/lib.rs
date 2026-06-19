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

pub mod channel;
pub mod commands;
pub mod discovery;
pub mod dm;
pub mod error;
pub mod eventlog;
pub mod events;
pub mod file;
pub mod identity;
pub mod logger;
pub mod node;
pub mod perf;
pub mod postoffice;
pub mod ratchet;
pub mod redesign_commands;
pub mod services;
pub mod state;
pub mod storage;
pub mod transport;
pub mod tray;

use crate::state::AppState;
use std::sync::Arc;
use tauri::Manager;

/// Tauri application entry point. The serverless redesign node is the whole product now;
/// it starts per-session on login (see `commands::login` → `spawn_redesign_runtime`).
pub fn run_tauri() {
    let _timer = perf_monitor!("application_startup");
    log::info!("Starting Mesh-Talk desktop runtime");

    // Data directory + file manager (~/.mesh-talk), shared by the auth keystore.
    lazy_static::lazy_static! {
        static ref FILE_MANAGER: Arc<crate::storage::file_manager::FileManager> = {
            let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let data_path = std::path::PathBuf::from(home_dir).join(".mesh-talk");
            log::info!("Data path: {}", data_path.to_str().unwrap_or(".mesh-talk"));
            Arc::new(crate::storage::file_manager::FileManager::new(data_path))
        };
    }
    let _file_manager = FILE_MANAGER.clone();

    // Auth (login/register) is the only stateful service the shell needs; the redesign
    // node manages its own per-account stores out of `RedesignState`.
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
        .manage(crate::redesign_commands::RedesignState::empty())
        .invoke_handler(tauri::generate_handler![
            commands::login,
            commands::logout,
            commands::register,
            commands::redesign_adopt_linked_account,
            crate::redesign_commands::redesign_my_id,
            crate::redesign_commands::redesign_list_peers,
            crate::redesign_commands::redesign_send_dm,
            crate::redesign_commands::redesign_history,
            crate::redesign_commands::redesign_account_id,
            crate::redesign_commands::redesign_send_to_account,
            crate::redesign_commands::redesign_account_history,
            crate::redesign_commands::redesign_start_linking,
            crate::redesign_commands::redesign_stop_linking,
            crate::redesign_commands::redesign_link_device,
            crate::redesign_commands::redesign_rekey_account,
            crate::redesign_commands::redesign_list_accounts,
            crate::redesign_commands::redesign_send_file_to_account,
            crate::redesign_commands::redesign_react_account,
            crate::redesign_commands::redesign_account_reactions,
            crate::redesign_commands::redesign_list_channels,
            crate::redesign_commands::redesign_create_channel,
            crate::redesign_commands::redesign_add_channel_member,
            crate::redesign_commands::redesign_remove_channel_member,
            crate::redesign_commands::redesign_channel_members,
            crate::redesign_commands::redesign_send_channel_message,
            crate::redesign_commands::redesign_channel_history,
            crate::redesign_commands::redesign_send_file_dm,
            crate::redesign_commands::redesign_send_file_channel,
            crate::redesign_commands::redesign_save_file,
            crate::redesign_commands::redesign_react_dm,
            crate::redesign_commands::redesign_react_channel,
            crate::redesign_commands::redesign_reactions,
            crate::redesign_commands::redesign_channel_reactions,
            crate::redesign_commands::redesign_search
        ])
        .run(tauri::generate_context!())
        .map_err(|e| log::error!("Error while running tauri application: {e}"))
        .unwrap_or(());
}
