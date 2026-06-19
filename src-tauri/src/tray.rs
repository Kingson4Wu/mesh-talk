use lazy_static::lazy_static;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Wry;
use tauri::{AppHandle, Manager};

const MENU_SHOW: &str = "show";
const MENU_HIDE: &str = "hide";
const MENU_QUIT: &str = "quit";
const TRAY_ID: &str = "mesh-talk-tray";

// Global atomic counter for unread messages (reflected in the tray tooltip).
static UNREAD_COUNT: AtomicU32 = AtomicU32::new(0);

type TrayMenuItem = MenuItem<Wry>;

lazy_static! {
    static ref SHOW_ITEM: Mutex<Option<TrayMenuItem>> = Mutex::new(None);
}

/// Create the main tray icon and menu structure.
pub fn create_system_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, MENU_SHOW, "Show", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, MENU_HIDE, "Hide", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    let tray_menu = Menu::with_items(app, &[&show, &hide, &separator, &quit])?;

    {
        let mut global_show = SHOW_ITEM.lock().unwrap();
        *global_show = Some(show.clone());
    }

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&tray_menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_QUIT => {
                app.exit(0);
            }
            MENU_HIDE => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            MENU_SHOW => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_window_visibility(tray.app_handle());
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.tooltip("Mesh-Talk").build(app)?;

    Ok(())
}

/// Toggle window visibility.
fn toggle_window_visibility(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        match window.is_visible() {
            Ok(true) => {
                let _ = window.hide();
            }
            _ => {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }
    }
}

/// Update the unread message count reflected in the tray tooltip.
pub fn update_unread_count(app: &AppHandle, count: u32) {
    UNREAD_COUNT.store(count, Ordering::Relaxed);

    let tooltip = if count > 0 {
        format!("Mesh-Talk ({count})")
    } else {
        "Mesh-Talk".to_string()
    };

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(&tooltip));
    }
}

/// Get the current unread message count.
pub fn get_unread_count() -> u32 {
    UNREAD_COUNT.load(Ordering::Relaxed)
}
