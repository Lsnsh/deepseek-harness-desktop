//! System tray icon: keeps the client running in the background after the
//! window is closed and restores it on demand.
//!
//! The main window's close button hides the window instead of quitting (see
//! `lib.rs`); the tray menu is the deliberate exit point ("Quit") and the
//! restore point ("Show DeepSeek Harness"), and a left click on the icon also
//! shows the window. On macOS the dock icon works too (`RunEvent::Reopen`).

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

/// Build the tray icon and its menu. The returned icon is reference-counted
/// and removed when the last handle is dropped, so it is stored in app state
/// for the app's lifetime.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show_window", "Show DeepSeek Harness", true, None::<&str>)?;
    let check = MenuItem::with_id(app, "check_update", "Check for Updates…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &check, &quit])?;
    let tray = TrayIconBuilder::new()
        // Monochrome DeepSeek mark (see icons/tray.png); macOS renders it as
        // a template image, so the system recolors it for the light/dark
        // menu bar.
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(true)
        .tooltip("DeepSeek Harness")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show_window" => show_window(app),
            "check_update" => crate::updater::check_for_updates(app.clone(), true),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    app.manage(tray);
    Ok(())
}

/// Show, unminimize, and focus the main window.
pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
