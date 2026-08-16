//! Auto-update support with a VSCode-style update state machine.
//!
//! The single "Check for Updates…" menu item walks three states:
//!
//! - **Idle** — "Check for Updates…" (enabled). Clicking checks the manifest
//!   (`latest.json` via the endpoint configured in tauri.conf.json, signature
//!   verified against the embedded public key) and, if a new version exists,
//!   starts downloading it.
//! - **Downloading** — "Downloading…" (disabled). The archive is streamed in
//!   memory; progress is emitted via the `update-progress` event and stderr
//!   logs (no progress-bar UI).
//! - **Ready** — "Restart to Update (1)" (enabled). The downloaded bytes and
//!   the [`Update`] handle are held in memory, and the version + archive path
//!   are persisted to `pending-update.json` in the app data dir. Clicking asks
//!   "restart now?" and, on confirm, exits the app.
//!
//! Installation happens synchronously in the [`RunEvent::Exit`] hook (see
//! `install_pending_on_exit`): the pending [`Update`] is installed from the
//! in-memory bytes (tauri-plugin-updater's `install` replaces the .app on
//! macOS), the pending files are cleared, and the app relaunches itself so the
//! next launch runs the new version.
//!
//! The harness GUI is served from a remote origin and has no `window.__TAURI__`
//! IPC access, so the entry points are native: the application menu and the
//! startup check. Set `DSH_DESKTOP_AUTO_UPDATE=0` to disable the startup
//! check (the menu action always stays available).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::Error as UpdateError;
use tauri_plugin_updater::{Update, UpdaterExt};

/// Delay before the silent startup check, so the window appears first.
const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(3);

/// The menu item id shared with lib.rs's menu builder.
const MENU_ID_CHECK: &str = "check_update";
/// Menu text in each state.
const MENU_TEXT_IDLE: &str = "Check for Updates…";
const MENU_TEXT_DOWNLOADING: &str = "Downloading…";
const MENU_TEXT_READY: &str = "Restart to Update (1)";

/// Whether the startup check is enabled. Any value of `DSH_DESKTOP_AUTO_UPDATE`
/// other than `0` keeps it on; the menu action is never disabled.
fn auto_update_enabled() -> bool {
    std::env::var("DSH_DESKTOP_AUTO_UPDATE").as_deref() != Ok("0")
}

/// The update-state machine.
#[derive(Clone, Copy, PartialEq)]
enum MenuState {
    Idle,
    Downloading,
    Ready,
}

/// The downloaded update, held in memory between download and the exit hook.
/// `tauri-plugin-updater`'s `install()` needs the original [`Update`] object
/// (it knows where the current .app lives) plus the verified bytes, so we
/// cannot reconstruct it after a restart — hence the in-memory holder.
struct PendingUpdate {
    update: Update,
    bytes: Vec<u8>,
    version: String,
}

static PENDING: OnceLock<Mutex<Option<PendingUpdate>>> = OnceLock::new();

fn pending_lock() -> &'static Mutex<Option<PendingUpdate>> {
    PENDING.get_or_init(|| Mutex::new(None))
}

fn store_pending(pending: PendingUpdate) {
    if let Ok(mut guard) = pending_lock().lock() {
        *guard = Some(pending);
    }
}

fn take_pending() -> Option<PendingUpdate> {
    pending_lock().lock().ok().and_then(|mut guard| guard.take())
}

fn has_pending() -> bool {
    pending_lock().lock().map(|guard| guard.is_some()).unwrap_or(false)
}

/// Reflect the state machine in the "Check for Updates…" menu item.
fn set_menu_state(app: &AppHandle, state: MenuState) {
    let Some(menu) = app.menu() else { return };
    let Some(kind) = menu.get(MENU_ID_CHECK) else { return };
    let Some(item) = kind.as_menuitem() else { return };
    match state {
        MenuState::Idle => {
            let _ = item.set_text(MENU_TEXT_IDLE);
            let _ = item.set_enabled(true);
        }
        MenuState::Downloading => {
            let _ = item.set_text(MENU_TEXT_DOWNLOADING);
            let _ = item.set_enabled(false);
        }
        MenuState::Ready => {
            let _ = item.set_text(MENU_TEXT_READY);
            let _ = item.set_enabled(true);
        }
    }
}

/// Menu click dispatch: with a downloaded update waiting, ask to restart now;
/// otherwise run the check-and-download flow.
pub fn handle_menu_click(app: AppHandle) {
    if has_pending() {
        confirm_restart_and_update(app);
    } else {
        check_for_updates(app, true);
    }
}

/// Run one update flow. `interactive` controls whether "up to date" and error
/// outcomes surface a dialog (menu) or stay silent (startup check); in both
/// modes a newly found update is downloaded and the menu moves to "Ready".
pub fn check_for_updates(app: AppHandle, interactive: bool) {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = run_check(&app, interactive).await {
            eprintln!("[dsh-desktop] update check failed: {err}");
            set_menu_state(&app, MenuState::Idle);
            if interactive {
                let _ = app
                    .dialog()
                    .message(format!(
                        "Update check failed: {err}\nPlease check your network connection and try again."
                    ))
                    .kind(MessageDialogKind::Error)
                    .title("Check for Updates")
                    .show(|_| {});
            }
        }
    });
}

/// The actual check + download; returns Ok(()) when no update was found or
/// the download completed into the pending state.
async fn run_check(app: &AppHandle, interactive: bool) -> Result<(), String> {
    let updater = app.updater().map_err(|err| err.to_string())?;
    let update = match updater.check().await {
        Ok(update) => update,
        Err(UpdateError::ReleaseNotFound) => {
            // Normal before the first release exists: the endpoint (the
            // repository's /releases/latest) returns 404 until a release is
            // out.
            if interactive {
                let _ = app
                    .dialog()
                    .message("No updates available: the repository has not published any release yet.")
                    .kind(MessageDialogKind::Info)
                    .title("Check for Updates")
                    .show(|_| {});
            }
            set_menu_state(app, MenuState::Idle);
            return Ok(());
        }
        Err(err) => return Err(err.to_string()),
    };
    let Some(update) = update else {
        if interactive {
            let _ = app
                .dialog()
                .message("You are up to date.")
                .kind(MessageDialogKind::Info)
                .title("Check for Updates")
                .show(|_| {});
        }
        set_menu_state(app, MenuState::Idle);
        return Ok(());
    };

    let version = update.version.clone();
    set_menu_state(app, MenuState::Downloading);

    // Stream the archive; surface progress via the `update-progress` event
    // (consumed nowhere today, but useful for a future local progress page)
    // plus throttled stderr logs. Accounting/throttling/emission live in
    // `updater_progress`; the menu state machine above stays in charge of
    // Idle → Downloading → Ready.
    let tracker = Arc::new(Mutex::new(crate::updater_progress::ProgressTracker::new(
        app.clone(),
        app.state::<crate::updater_progress::ProgressState>().0.clone(),
        version.clone(),
    )));
    let chunk_tracker = tracker.clone();
    let finish_tracker = tracker.clone();
    let failed_tracker = tracker.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                if let Ok(mut tracker) = chunk_tracker.lock() {
                    tracker.on_chunk(chunk, total);
                }
            },
            move || {
                if let Ok(mut tracker) = finish_tracker.lock() {
                    tracker.on_download_finish();
                }
            },
        )
        .await
        .map_err(|err| {
            if let Ok(mut tracker) = failed_tracker.lock() {
                tracker.on_failed();
            }
            format!("download failed: {err}")
        })?;

    // Persist the "pending update" record (version + archive path) so a
    // future release can surface it; the bytes themselves stay in memory for
    // the exit hook, which is the only place we can still install.
    let archive_path = write_pending_files(app, &version, &bytes)?;
    eprintln!("[dsh-desktop] update v{version} downloaded to {}", archive_path.display());
    store_pending(PendingUpdate {
        update,
        bytes,
        version: version.clone(),
    });
    set_menu_state(app, MenuState::Ready);
    let _ = app.emit(
        "update-progress",
        serde_json::json!({ "version": version, "done": true }),
    );

    if interactive {
        confirm_restart_and_update(app.clone());
    }
    Ok(())
}

/// Ask "restart now?" and, on confirm, exit so the [`RunEvent::Exit`] hook
/// installs the pending update and relaunches the app.
fn confirm_restart_and_update(app: AppHandle) {
    let version = pending_lock()
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|p| p.version.clone()))
        .unwrap_or_default();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let confirmed = app
            .dialog()
            .message(format!(
                "更新 v{version} 已下载完成。\n是否立即重启应用以完成更新？"
            ))
            .buttons(MessageDialogButtons::OkCancel)
            .title("Restart to Update")
            .blocking_show();
        if confirmed {
            // Exiting triggers RunEvent::Exit, where install_pending_on_exit
            // installs the update and relaunches the app.
            app.exit(0);
        }
    });
}

/// Write `pending-update.json` (version + archive path) and the archive itself
/// into the app data dir, returning the archive path.
fn write_pending_files(app: &AppHandle, version: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|err| format!("cannot resolve app data dir: {err}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let archive = dir.join(format!("pending-update-{version}.tar.gz"));
    std::fs::write(&archive, bytes)
        .map_err(|err| format!("cannot write {}: {err}", archive.display()))?;
    let meta = serde_json::json!({
        "version": version,
        "path": archive.to_string_lossy(),
    });
    let meta_path = dir.join("pending-update.json");
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta).unwrap_or_default())
        .map_err(|err| format!("cannot write {}: {err}", meta_path.display()))?;
    Ok(archive)
}

/// Remove the pending-update record and its archive after a successful install.
fn clear_pending_files(app: &AppHandle) {
    if let Ok(dir) = app.path().app_data_dir() {
        let meta_path = dir.join("pending-update.json");
        if let Ok(text) = std::fs::read_to_string(&meta_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(path) = json.get("path").and_then(|v| v.as_str()) {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let _ = std::fs::remove_file(meta_path);
    }
}

/// Installed from the [`RunEvent::Exit`] hook: install any pending update
/// synchronously (tauri-plugin-updater swaps the .app on macOS), clear the
/// pending record, and relaunch so the next launch is the new version.
pub fn install_pending_on_exit(app: &AppHandle) {
    let Some(pending) = take_pending() else { return };
    eprintln!("[dsh-desktop] installing pending update v{}", pending.version);
    match pending.update.install(&pending.bytes) {
        Ok(()) => {
            clear_pending_files(app);
            eprintln!("[dsh-desktop] update installed; relaunching");
            relaunch_app();
        }
        Err(err) => {
            eprintln!("[dsh-desktop] failed to install pending update: {err}");
        }
    }
}

/// Spawn the current executable again. On macOS the disk image of the .app has
/// already been replaced by `install`, so this child runs the new version.
fn relaunch_app() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// Schedule the silent startup check; disabled by `DSH_DESKTOP_AUTO_UPDATE=0`.
/// It downloads into the Ready state without any dialog — the menu item then
/// reads "Restart to Update (1)".
pub fn schedule_startup_check(app: AppHandle) {
    if !auto_update_enabled() {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(STARTUP_CHECK_DELAY);
        check_for_updates(app, false);
    });
}
