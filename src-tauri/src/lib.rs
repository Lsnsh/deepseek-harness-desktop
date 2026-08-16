//! The desktop shell for DeepSeek Harness.
//!
//! Responsibilities: spawn the bundled `dsh web` server (bundled Node.js +
//! installed harness, see `scripts/assemble-runtime.mjs`), wait for its
//! readiness URL line, navigate the main window to the served GUI, keep the
//! server's lifecycle tied to the app's, keep navigation confined to the
//! server origin (external links leave the window via the system browser),
//! and post native notifications when a session's turn completes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod notify;
mod server;
mod tray;
mod updater;

use std::process::Child;
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu, SubmenuBuilder};
use tauri::{
    AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

/// The spawned server child, owned for the app's lifetime and killed on exit.
#[derive(Default)]
pub struct ServerChild(pub Arc<Mutex<Option<Child>>>);

/// The server origin the window may navigate to (set once it is known).
#[derive(Default)]
pub struct ServerOrigin(pub Arc<Mutex<Option<Url>>>);

/// Whether a URL is one of the app's own local pages (splash / error).
fn is_local_app_page(url: &Url) -> bool {
    url.scheme() == "tauri"
        || url.scheme() == "tauri-localhost"
        || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost"))
}

/// The URL of this app's bundled local page (scheme differs per platform).
pub(crate) fn local_app_url(path: &str) -> Url {
    #[cfg(target_os = "macos")]
    let base = "tauri://localhost";
    #[cfg(not(target_os = "macos"))]
    let base = "http://tauri.localhost";
    Url::parse(&format!("{base}/{path}")).expect("static local page URL")
}

/// The git commit this build was compiled from; "unknown" when the build ran
/// outside a git checkout (build.rs sets it via `cargo:rustc-env`).
fn git_commit() -> &'static str {
    option_env!("DSH_GIT_COMMIT").unwrap_or("unknown")
}

/// Commit date (`YYYY-MM-DD HH:MM:SS +ZZZZ`) of the build commit.
fn git_commit_date() -> &'static str {
    option_env!("DSH_GIT_COMMIT_DATE").unwrap_or("unknown")
}

/// Show the About dialog: build provenance, the third-party disclaimer, and
/// the upstream-sync statement.
fn show_about(app: &AppHandle) {
    let text = format!(
        "DeepSeek Harness Developer Preview\n\
         Version {version}\n\
         Build: {commit} ({date})\n\
         \n\
         This is a community-maintained third-party desktop client for \
         development and research only; it is not affiliated with DeepSeek.\n\
         It bundles an independent Node.js runtime and the dsh web service; \
         sessions and settings are stored locally (default ~/.dsh).\n\
         \n\
         This client is regularly synchronized with the official DeepSeek \
         Harness repository, and updates are published as GitHub Releases.",
        version = env!("CARGO_PKG_VERSION"),
        commit = git_commit(),
        date = git_commit_date(),
    );
    let _ = app
        .dialog()
        .message(text)
        .title("About DeepSeek Harness")
        .show(|_| {});
}

/// The user's home directory, used as the server's working directory so the
/// default file-sandbox workspace root is meaningful for GUI launches.
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .menu(|handle| {
            // Native menu entries only, because the served GUI has no IPC
            // access to the plugins. The About entry always comes first.
            let about = MenuItem::with_id(handle, "about", "About DeepSeek Harness", true, None::<&str>)?;
            let check = MenuItem::with_id(handle, "check_update", "Check for Updates…", true, None::<&str>)?;
            let app_menu = Submenu::with_items(
                handle,
                "DeepSeek Harness",
                true,
                &[
                    &about,
                    &check,
                    &PredefinedMenuItem::separator(handle)?,
                    &PredefinedMenuItem::quit(handle, None)?,
                ],
            )?;
            // The standard editing roles carry the platform accelerators
            // (Cmd+C / Cmd+V / … on macOS, Ctrl+… on Windows/Linux) and send
            // their commands to the focused webview. A custom menu without an
            // Edit submenu leaves copy/paste/cut/select-all dead in the GUI.
            let edit_menu = SubmenuBuilder::new(handle, "Edit")
                .undo()
                .redo()
                .separator()
                .cut()
                .copy()
                .paste()
                .select_all()
                .build()?;
            // A minimal Window menu restores the standard window shortcuts;
            // Close still routes through CloseRequested and hides to the tray.
            let window_menu = SubmenuBuilder::new(handle, "Window")
                .minimize()
                .close_window()
                .build()?;
            Menu::with_items(handle, &[&app_menu, &edit_menu, &window_menu])
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "check_update" => updater::check_for_updates(app.clone(), true),
            "about" => show_about(&app),
            _ => {}
        })
        .manage(ServerChild::default())
        .manage(ServerOrigin::default())
        .manage(notify::NotifyState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let origin_state = app.state::<ServerOrigin>().0.clone();
            let handle_for_nav = handle.clone();
            let navigation = move |url: &Url| {
                if is_local_app_page(url) {
                    return true;
                }
                let allowed = origin_state
                    .lock()
                    .ok()
                    .and_then(|guard| guard.clone())
                    .is_some_and(|allowed| {
                        url.scheme() == allowed.scheme()
                            && url.host_str() == allowed.host_str()
                            && url.port() == allowed.port()
                    });
                if allowed {
                    return true;
                }
                if url.scheme() == "http" || url.scheme() == "https" {
                    // Leave the window: hand the link to the system browser.
                    let app = handle_for_nav.clone();
                    let target = url.clone();
                    std::thread::spawn(move || {
                        let _ = app.opener().open_url(target.as_str(), None::<&str>);
                    });
                }
                false
            };
            let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("DeepSeek Harness")
                .inner_size(1280.0, 840.0)
                .min_inner_size(960.0, 640.0)
                .center()
                .resizable(true)
                .on_navigation(navigation)
                .build()
                .map_err(|err| format!("dsh-desktop: failed to create the main window: {err}"))?;
            // The close button hides the window instead of quitting: the
            // client keeps running in the background (server included) and is
            // restored from the tray icon (or the macOS dock icon). Quit via
            // the tray menu, or Cmd+Q on macOS.
            let window_for_close = window.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window_for_close.hide();
                }
            });
            tray::build(app.handle())?;
            match server::spawn_server(&handle) {
                Ok(spawned) => {
                    *app.state::<ServerChild>().0.lock().unwrap() = Some(spawned.child);
                    server::watch_server(handle.clone(), window);
                }
                Err(err) => {
                    eprintln!("[dsh-desktop] failed to start the web server: {err}");
                    let _ = window.navigate(local_app_url("error.html"));
                }
            }
            // Session-completion notifications (background poller).
            notify::spawn(handle.clone());
            // Silent auto-update check shortly after the window is up.
            updater::schedule_startup_check(handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("dsh-desktop: failed to build the tauri application");

    app.run(|app_handle, event| match event {
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => {
            // Dock icon clicked while the window is hidden: bring it back.
            tray::show_window(&app_handle);
        }
        RunEvent::Exit => {
            if let Some(state) = app_handle.try_state::<ServerChild>() {
                if let Some(mut child) = state.0.lock().unwrap().take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        _ => {}
    });
}
