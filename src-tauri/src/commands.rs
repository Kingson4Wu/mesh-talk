//! Tauri IPC commands. After the legacy stack was retired this is just the auth surface
//! (login / logout / register) plus the bridge that starts the redesign node on login.
//! All messaging/contact/file/channel commands live in [`crate::redesign_commands`].

use crate::services::auth_service::AuthError;
use crate::services::user::User;
use crate::state::AppState;

/// Represents possible errors that can occur in command execution.
#[derive(Debug)]
pub enum CommandError {
    Validation(String),
    Authentication(String),
    Authorization(String),
    Service(String),
    Network(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Validation(msg)
            | CommandError::Authentication(msg)
            | CommandError::Authorization(msg)
            | CommandError::Service(msg)
            | CommandError::Network(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CommandError {}

type CommandResult<T> = Result<T, CommandError>;

#[derive(serde::Serialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
}

impl From<User> for UserInfo {
    fn from(user: User) -> Self {
        Self {
            id: user.user_id,
            username: user.name,
        }
    }
}

#[derive(serde::Serialize)]
pub struct LoginResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserInfo>,
}

#[derive(serde::Serialize)]
pub struct LogoutResult {
    pub success: bool,
}

#[derive(serde::Serialize)]
pub struct RegisterResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserInfo>,
}

// ---------------------------------------------------------------------------
// Auth commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn login(
    app_handle: tauri::AppHandle,
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
    redesign_state: tauri::State<'_, crate::redesign_commands::RedesignState>,
) -> Result<LoginResult, String> {
    let result =
        login_impl(username, password.clone(), app_state.inner()).map_err(|e| e.to_string())?;

    if result.success {
        // Start the redesign node: per-account stores under ~/.mesh-talk/redesign/<user_id>/.
        if let Some(session) = app_state.session().get() {
            spawn_redesign_runtime(
                app_handle.clone(),
                session.user.user_id.clone(),
                session.user.name.clone(),
                password,
                redesign_state.inner().clone(),
            );
        }
    }

    Ok(result)
}

fn login_impl(
    username: String,
    password: String,
    app_state: &AppState,
) -> CommandResult<LoginResult> {
    if username.trim().is_empty() || password.trim().is_empty() {
        return Err(CommandError::Validation(
            "Username and password are required".into(),
        ));
    }

    let normalized_username = username.trim().to_string();
    let normalized_password = password.trim().to_string();

    match app_state
        .auth_service()
        .login(normalized_username.clone(), normalized_password.clone())
    {
        Ok((user, token)) => {
            app_state
                .session()
                .set(token.clone(), user.clone(), normalized_password);
            Ok(LoginResult {
                success: true,
                token: Some(token),
                user: Some(UserInfo::from(user)),
            })
        }
        Err(AuthError::UserNotFound) | Err(AuthError::InvalidCredentials) => Err(
            CommandError::Authentication("Invalid username or password".into()),
        ),
        Err(AuthError::InvalidInput(msg)) => Err(CommandError::Validation(msg)),
        Err(AuthError::AlreadyLoggedIn) => Err(CommandError::Authentication(
            "A user is already signed in".into(),
        )),
        Err(AuthError::StorageError(msg)) | Err(AuthError::InternalError(msg)) => {
            Err(CommandError::Service(msg))
        }
        Err(other) => Err(CommandError::Service(format!(
            "Failed to login user '{normalized_username}': {other:?}"
        ))),
    }
}

#[tauri::command]
pub async fn logout(
    app_state: tauri::State<'_, AppState>,
    redesign_state: tauri::State<'_, crate::redesign_commands::RedesignState>,
) -> Result<LogoutResult, String> {
    // Stop and drop the redesign runtime (Drop aborts its background tasks).
    redesign_state.0.lock().await.take();
    logout_impl(app_state.inner()).map_err(|e| e.to_string())
}

fn logout_impl(app_state: &AppState) -> CommandResult<LogoutResult> {
    let session = require_session(app_state)?;
    app_state
        .auth_service()
        .logout(session.token.clone())
        .map_err(|e| CommandError::Service(format!("Failed to logout: {e:?}")))?;
    app_state.session().clear();
    Ok(LogoutResult { success: true })
}

#[tauri::command]
pub async fn register(
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<RegisterResult, String> {
    register_impl(username, password, app_state.inner()).map_err(|e| e.to_string())
}

fn register_impl(
    username: String,
    password: String,
    app_state: &AppState,
) -> CommandResult<RegisterResult> {
    if username.trim().is_empty() || password.trim().is_empty() {
        return Err(CommandError::Validation(
            "Username and password are required".into(),
        ));
    }

    let normalized_password = password.trim().to_string();
    if normalized_password.len() < 8 {
        return Err(CommandError::Validation(
            "Password must be at least 8 characters".into(),
        ));
    }

    let user = match app_state.auth_service().register(
        username.clone(),
        normalized_password,
        "127.0.0.1:7000".into(),
    ) {
        Ok(user) => user,
        Err(AuthError::UserAlreadyExists) => {
            return Err(CommandError::Validation("Username already exists".into()))
        }
        Err(AuthError::PasswordTooWeak) => {
            return Err(CommandError::Validation(
                "Password must be at least 8 characters".into(),
            ))
        }
        Err(AuthError::InvalidInput(msg)) => return Err(CommandError::Validation(msg)),
        Err(AuthError::StorageError(msg)) | Err(AuthError::InternalError(msg)) => {
            return Err(CommandError::Service(msg))
        }
        Err(other) => {
            return Err(CommandError::Service(format!(
                "Failed to register user: {other:?}"
            )))
        }
    };

    Ok(RegisterResult {
        success: true,
        user: Some(UserInfo::from(user)),
    })
}

fn require_session(state: &AppState) -> CommandResult<crate::state::SessionInfo> {
    state
        .session()
        .get()
        .ok_or_else(|| CommandError::Authentication("User session not found. Please login.".into()))
}

// ---------------------------------------------------------------------------
// Redesign node lifecycle bridge
// ---------------------------------------------------------------------------

/// The base directory for redesign per-account data (mirrors the app's `~/.mesh-talk`).
fn redesign_data_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".mesh-talk")
}

/// Spawn the redesign runtime in the background, wiring its inbound callbacks to the
/// app's Tauri events, and store it in `redesign_handle`. Shared by login and by account
/// adoption after device linking (which drops the old runtime and re-spawns; `start`
/// reloads the account keystore, so a re-spawn adopts a freshly-linked account secret).
/// `account_id` is the host-app namespace for the data directory — distinct from the
/// node's cryptographic account.
pub(crate) fn spawn_redesign_runtime(
    app_handle: tauri::AppHandle,
    account_id: String,
    display_name: String,
    password: String,
    redesign_handle: crate::redesign_commands::RedesignState,
) {
    let app_handle_for_dm = app_handle.clone();
    let app_handle_for_channel = app_handle.clone();
    let app_handle_for_file = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let base_dir = redesign_data_dir();
        match crate::node::runtime::RedesignRuntime::start(
            &base_dir,
            &account_id,
            &display_name,
            &password,
            crate::node::net::DEFAULT_DISCOVERY_PORT,
            move |dm| {
                crate::events::emit_redesign_dm_received(
                    &app_handle_for_dm,
                    dm.from,
                    dm.from_name,
                    dm.text,
                    dm.reply_to,
                );
            },
            move |msg: crate::node::channel::ReceivedChannelMessage| {
                crate::events::emit_redesign_channel_message(
                    &app_handle_for_channel,
                    hex::encode(msg.channel_id.as_bytes()),
                    msg.channel_name,
                    msg.from,
                    msg.text,
                    msg.reply_to,
                );
            },
            move |f: crate::node::filebook::ReceivedFile| {
                crate::events::emit_redesign_file_received(
                    &app_handle_for_file,
                    hex::encode(f.conv.as_bytes()),
                    f.from,
                    f.name,
                    f.size,
                    hex::encode(f.file_conv.as_bytes()),
                );
            },
        )
        .await
        {
            Ok(runtime) => {
                *redesign_handle.0.lock().await = Some(runtime);
                log::info!("Redesign node started for account {account_id}");
            }
            Err(e) => log::warn!("Redesign node failed to start: {e}"),
        }
    });
}

/// Adopt an account secret just persisted by a successful device link: drop the running
/// redesign runtime and re-spawn it so it reloads the account keystore (now holding the
/// linked account) and re-advertises under it. Reuses the held session credentials — no
/// re-login required.
#[tauri::command]
pub async fn redesign_adopt_linked_account(
    app_handle: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
    redesign_state: tauri::State<'_, crate::redesign_commands::RedesignState>,
) -> Result<(), String> {
    let pw = {
        let guard = redesign_state.0.lock().await;
        guard
            .as_ref()
            .map(|rt| rt.restart_password().to_string())
            .ok_or_else(|| "redesign node not started".to_string())?
    };
    let session = app_state
        .session()
        .get()
        .ok_or_else(|| "not logged in".to_string())?;
    let account_id = session.user.user_id.clone();
    let display_name = session.user.name.clone();
    redesign_state.0.lock().await.take();
    spawn_redesign_runtime(
        app_handle,
        account_id,
        display_name,
        pw,
        redesign_state.inner().clone(),
    );
    Ok(())
}
