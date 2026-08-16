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
mod plugins;
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

/// Kill the bundled web server and start it fresh (used after installing or
/// removing plugins, whose bundle layers are composed at boot). The window
/// returns to the splash until the new server is ready.
fn restart_server(app: &AppHandle) {
    if let Some(mut child) = app.state::<ServerChild>().0.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    *app.state::<ServerOrigin>().0.lock().unwrap() = None;
    match server::spawn_server(app) {
        Ok(spawned) => {
            *app.state::<ServerChild>().0.lock().unwrap() = Some(spawned.child);
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.navigate(local_app_url("index.html"));
                server::watch_server(app.clone(), window);
            }
        }
        Err(err) => {
            eprintln!("[dsh-desktop] failed to restart the web server: {err}");
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.navigate(local_app_url("error.html"));
            }
        }
    }
}

/// Open (or focus) the plugin manager window — a local page with full IPC,
/// unlike the remote-origin harness GUI.
fn open_plugin_manager(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("plugins") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    // Local pages are fine; remote links leave via the system browser (same
    // fence as the main window).
    let handle = app.clone();
    let navigation = move |url: &Url| {
        if is_local_app_page(url) {
            return true;
        }
        if url.scheme() == "http" || url.scheme() == "https" {
            let h = handle.clone();
            let target = url.clone();
            std::thread::spawn(move || {
                let _ = h.opener().open_url(target.as_str(), None::<&str>);
            });
        }
        false
    };
    if let Ok(window) = WebviewWindowBuilder::new(app, "plugins", WebviewUrl::App("plugins.html".into()))
        .title("Plugin Manager — DeepSeek Harness")
        .inner_size(900.0, 640.0)
        .min_inner_size(700.0, 480.0)
        .center()
        .on_navigation(navigation)
        .build()
    {
        let _ = window;
    }
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
            let restart = MenuItem::with_id(handle, "restart_server", "Restart Web Server", true, None::<&str>)?;
            let plugins_menu_item = MenuItem::with_id(handle, "plugin_manager", "Plugin Manager…", true, None::<&str>)?;
            let app_menu = Submenu::with_items(
                handle,
                "DeepSeek Harness",
                true,
                &[
                    &about,
                    &check,
                    &restart,
                    &plugins_menu_item,
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
            "check_update" => updater::handle_menu_click(app.clone()),
            "about" => show_about(&app),
            "restart_server" => restart_server(&app),
            "plugin_manager" => open_plugin_manager(&app),
            _ => {}
        })
        .manage(ServerChild::default())
        .manage(ServerOrigin::default())
        .manage(notify::NotifyState::default())
        .manage(plugins::SearchCache::default())
        .invoke_handler(tauri::generate_handler![
            plugins::list_plugins,
            plugins::search_plugins,
            plugins::install_plugin,
            plugins::uninstall_plugin,
        ])
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
                .initialization_script(INIT_JUMP_SCRIPT)
                .on_navigation(navigation)
                .build()
                .map_err(|err| format!("dsh-desktop: failed to create the main window: {err}"))?;
            // The close button hides the window instead of quitting: the
            // client keeps running in the background (server included) and is
            // restored from the tray icon (or the macOS dock icon). Quit via
            // the tray menu, or Cmd+Q on macOS.
            let window_for_close = window.clone();
            let last_focus = app.state::<notify::NotifyState>().last_focus.clone();
            window.on_window_event(move |event| {
                match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = window_for_close.hide();
                    }
                    WindowEvent::Focused(true) => {
                        // Remember when the user last looked at the window, so
                        // jump_to_last only jumps for genuinely unviewed
                        // completion notifications.
                        if let Ok(mut guard) = last_focus.lock() {
                            *guard = Some(std::time::Instant::now());
                        }
                    }
                    _ => {}
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
            // Dock icon clicked (or a notification clicked — the platform
            // reports both the same way) while the window is hidden: bring it
            // back, and jump to the session that completed if there is an
            // unviewed completion notification.
            tray::show_window(&app_handle);
            notify::jump_to_last(&app_handle);
        }
        RunEvent::Exit => {
            if let Some(state) = app_handle.try_state::<ServerChild>() {
                if let Some(mut child) = state.0.lock().unwrap().take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            // Install any downloaded-but-not-installed update synchronously
            // (replaces the .app on macOS), then relaunch the app.
            updater::install_pending_on_exit(&app_handle);
        }
        _ => {}
    });
}

/// Runs before any page script on every page load (splash, error page, and
/// the served GUI). When the URL carries `?jump=<sessionId>`, writes the
/// dsh frontend's persisted current-session key (`dsh.sessions.current`) so
/// the SPA opens that session on boot, then strips the query parameter.
///
/// The key name is the frontend's own data contract (verified in
/// @deepseek-ai/dsh-client-runtime), not an internal hack; the desktop
/// bundles a pinned frontend version, so the contract is stable. The
/// frontend's session list is a cross-workspace aggregate (every workspace's
/// sessions are listed and grouped in the sidebar), so the jump opens a
/// session of any workspace; [`notify::jump_to_last`] probes the harness
/// `workspace.list` for the log and falls back to focusing only when the
/// probe itself fails.
const INIT_JUMP_SCRIPT: &str = r#"
(() => {
  try {
    const u = new URL(location.href);
    const id = u.searchParams.get("jump");
    if (id && (location.hostname === "127.0.0.1" || location.hostname === "localhost")) {
      localStorage.setItem("dsh.sessions.current", JSON.stringify({ sessionId: id }));
      u.searchParams.delete("jump");
      history.replaceState(null, "", u);
    }
  } catch (e) { /* never break the page */ }
})();
"#;
