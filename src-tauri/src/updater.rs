//! Auto-update support: "Check for Updates…" menu action and the startup
//! silent check share one path. The updater plugin reads the update manifest
//! (`latest.json`) from the GitHub Releases endpoint configured in
//! tauri.conf.json, verifies the bundle signature against the embedded public
//! key, and — after the user confirms — downloads and installs the new
//! version (.app replacement on macOS).
//!
//! The harness GUI is served from a remote origin and has no `window.__TAURI__`
//! IPC access, so the entry points are native: the application menu and the
//! startup check. Set `DSH_DESKTOP_AUTO_UPDATE=0` to disable the startup
//! check (the menu action always stays available).

use std::time::Duration;

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::Error as UpdateError;
use tauri_plugin_updater::UpdaterExt;

/// Delay before the silent startup check, so the window appears first.
const STARTUP_CHECK_DELAY: Duration = Duration::from_secs(3);

/// Whether the startup check is enabled. Any value of `DSH_DESKTOP_AUTO_UPDATE`
/// other than `0` keeps it on; the menu action is never disabled.
fn auto_update_enabled() -> bool {
    std::env::var("DSH_DESKTOP_AUTO_UPDATE").as_deref() != Ok("0")
}

/// Run one update flow. `interactive` controls whether "up to date" and error
/// outcomes surface a dialog (menu) or stay silent (startup check).
pub fn check_for_updates(app: AppHandle, interactive: bool) {
    tauri::async_runtime::spawn(async move {
        match run_check(&app, interactive).await {
            Ok(_) => {}
            Err(err) => {
                eprintln!("[dsh-desktop] update check failed: {err}");
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
        }
    });
}

/// The actual check; returns whether an update was downloaded and installed.
async fn run_check(app: &AppHandle, interactive: bool) -> Result<bool, String> {
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
            return Ok(false);
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
        return Ok(false);
    };
    let version = update.version.clone();
    let dialog_app = app.clone();
    let confirmed = tauri::async_runtime::spawn_blocking(move || {
        dialog_app
            .dialog()
            .message(format!(
                "A new version v{version} is available.\nDownload and install it now? The app will need to be restarted afterwards."
            ))
            .buttons(MessageDialogButtons::OkCancel)
            .title("Update Available")
            .blocking_show()
    })
    .await
    .map_err(|err| format!("update confirmation dialog failed: {err}"))?;
    if !confirmed {
        return Ok(false);
    }
    // Progress is intentionally not surfaced: the download is small (the
    // runtime archive compresses well) and a modal progress UI would fight
    // the native dialogs.
    update
        .download_and_install(|_downloaded, _total| {}, || {})
        .await
        .map_err(|err| format!("download or install failed: {err}"))?;
    Ok(true)
}

/// Schedule the silent startup check; disabled by `DSH_DESKTOP_AUTO_UPDATE=0`.
pub fn schedule_startup_check(app: AppHandle) {
    if !auto_update_enabled() {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(STARTUP_CHECK_DELAY);
        check_for_updates(app, false);
    });
}
