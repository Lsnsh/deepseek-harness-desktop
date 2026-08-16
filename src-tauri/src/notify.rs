//! Session-completion notifications.
//!
//! The desktop shell has no IPC access into the remote-origin harness GUI, so
//! "the session finished" is detected from the harness's own durable session
//! store: every session is a zstd-compressed JSONL log under
//! `$DSH_HOME/sessions` (default `~/.dsh/sessions`), and each completed agent
//! turn appends a `turn/end` event. A lightweight poller watches those files,
//! and on a fresh `turn/end` whose reason is `completed` (or `error`) posts a
//! native notification carrying the session title. Clicking the notification
//! activates the app (macOS `RunEvent::Reopen` in lib.rs), which brings the
//! window to the front.
//!
//! The poll cadence and the notification gate can be tuned with env vars:
//! `DSH_DESKTOP_NOTIFY=0` disables notifications entirely, and
//! `DSH_DESKTOP_NOTIFY_INTERVAL_MS` changes the poll period (default 2000).

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// How long to wait for the store to exist before giving up on the first pass.
const STORE_BOOT_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether notifications are enabled. Any value of `DSH_DESKTOP_NOTIFY`
/// other than `0` keeps them on.
fn notifications_enabled() -> bool {
    std::env::var("DSH_DESKTOP_NOTIFY").as_deref() != Ok("0")
}

/// Poll interval from `DSH_DESKTOP_NOTIFY_INTERVAL_MS`, clamped to sane bounds.
fn poll_interval() -> Duration {
    let ms = std::env::var("DSH_DESKTOP_NOTIFY_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(2000);
    Duration::from_millis(ms.clamp(500, 30_000))
}

/// The dsh home directory (default `~/.dsh`, honor `DSH_HOME`).
fn dsh_home() -> PathBuf {
    std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".dsh")))
        .unwrap_or_else(|| PathBuf::from("~/.dsh"))
}

/// One watched session log: its last observed size plus the turn/end events
/// already notified (`(turn, seq)` pairs, so a file append is idempotent).
/// `baselined` marks the first observation of the file: its history is
/// recorded silently so the app never replays turns that finished before (or
/// around) launch.
#[derive(Default)]
struct FileState {
    size: u64,
    baselined: bool,
    notified: std::collections::HashSet<(u64, u64)>,
}

/// Shared watcher state, owned by the app (see [`spawn`]).
pub struct NotifyState {
    files: Mutex<HashMap<PathBuf, FileState>>,
}

impl Default for NotifyState {
    fn default() -> Self {
        Self { files: Mutex::new(HashMap::new()) }
    }
}

/// A decoded `turn/end` worth notifying about.
struct CompletedTurn {
    turn: u64,
    seq: u64,
}

/// Start the background poller. Idempotent; disabled by `DSH_DESKTOP_NOTIFY=0`.
pub fn spawn(app: AppHandle) {
    if !notifications_enabled() {
        return;
    }
    std::thread::spawn(move || {
        let store = dsh_home().join("sessions");
        // Give a slow first boot (e.g. a fresh machine materializing the
        // runtime cache) a chance to create the store before we scan nothing.
        let started = std::time::Instant::now();
        while !store.is_dir() && started.elapsed() < STORE_BOOT_TIMEOUT {
            std::thread::sleep(Duration::from_millis(500));
        }
        loop {
            let _ = scan_once(&app, &store);
            std::thread::sleep(poll_interval());
        }
    });
}

/// One polling pass: discover session logs, decode changed files, notify on
/// new completed turns. Never panics — a broken file is skipped and retried.
fn scan_once(app: &AppHandle, store: &Path) -> Result<(), String> {
    let logs = discover_logs(store);
    let state = app.state::<NotifyState>();
    let mut files = state.files.lock().map_err(|_| "notify state poisoned".to_string())?;
    for path in logs {
        let (size, completed) = match read_completed_turns(&path) {
            Ok(found) => found,
            Err(_) => continue, // mid-write or unreadable; try again next pass
        };
        let entry = files.entry(path.clone()).or_default();
        if entry.size > size {
            // The log was truncated/rotated; re-base so we do not resend the
            // whole history.
            entry.notified.clear();
            entry.baselined = false;
        }
        if !entry.baselined {
            // First observation: record the current history silently.
            for turn in &completed {
                entry.notified.insert((turn.turn, turn.seq));
            }
            entry.baselined = true;
            entry.size = size;
            continue;
        }
        let mut fresh = Vec::new();
        for turn in completed {
            if entry.notified.insert((turn.turn, turn.seq)) {
                fresh.push(turn);
            }
        }
        entry.size = size;
        for turn in fresh {
            notify_turn(app, &path, turn);
        }
    }
    Ok(())
}

/// Find every `session.jsonl.zstd` under the session store (two levels deep:
/// `<workspace-slug>/<session-id>/session.jsonl.zstd`).
fn discover_logs(store: &Path) -> Vec<PathBuf> {
    let mut logs = Vec::new();
    let Ok(workspaces) = std::fs::read_dir(store) else {
        return logs;
    };
    for workspace in workspaces.flatten() {
        let Ok(sessions) = std::fs::read_dir(workspace.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            let log = session.path().join("session.jsonl.zstd");
            if log.is_file() {
                logs.push(log);
            }
        }
    }
    logs
}

/// Decode a session log and return its current size plus the completed
/// `turn/end` events found (reason `completed` or `error`).
fn read_completed_turns(path: &Path) -> Result<(u64, Vec<CompletedTurn>), String> {
    let file = File::open(path).map_err(|err| format!("open {}: {err}", path.display()))?;
    let size = file
        .metadata()
        .map_err(|err| format!("stat {}: {err}", path.display()))?
        .len();
    let mut decoder =
        zstd::stream::read::Decoder::new(file).map_err(|err| format!("zstd header: {err}"))?;
    let mut raw = String::new();
    decoder
        .read_to_string(&mut raw)
        .map_err(|err| format!("zstd decode: {err}"))?;
    let mut completed = Vec::new();
    for line in raw.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("turn/end") {
            continue;
        }
        let Some(seq) = event.get("seq").and_then(Value::as_u64) else {
            continue;
        };
        let data = event.get("data");
        let Some(turn) = data.and_then(|d| d.get("turn")).and_then(Value::as_u64) else {
            continue;
        };
        let reason = data
            .and_then(|d| d.get("reason"))
            .and_then(|r| r.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if reason == "completed" || reason == "error" {
            completed.push(CompletedTurn { turn, seq });
        }
    }
    Ok((size, completed))
}

/// Post one native notification for a completed turn.
fn notify_turn(app: &AppHandle, path: &Path, turn: CompletedTurn) {
    let title = session_title(path);
    let summary = format!("会话已完成（第 {} 轮）", turn.turn);
    let body = if title.is_empty() {
        "DeepSeek Harness 已完成本次会话".to_string()
    } else {
        format!("「{title}」已完成")
    };
    eprintln!(
        "[dsh-desktop] session completed: {} (turn {}, seq {})",
        path.display(),
        turn.turn,
        turn.seq
    );
    // The notification plugin's builder has no click callback in 2.x; on
    // macOS clicking a notification activates the app, which surfaces as
    // `RunEvent::Reopen` in lib.rs and shows/focuses the window there.
    // Clicking therefore brings the running conversation to the front.
    let _ = app
        .notification()
        .builder()
        .title(summary)
        .body(body)
        .show();
}

/// The last `session/title` event's title in the log, if any.
fn session_title(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let mut decoder = match zstd::stream::read::Decoder::new(file) {
        Ok(decoder) => decoder,
        Err(_) => return String::new(),
    };
    let mut raw = String::new();
    if decoder.read_to_string(&mut raw).is_err() {
        return String::new();
    }
    let mut title = String::new();
    for line in raw.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("session/title") {
            continue;
        }
        if let Some(value) = event.get("data").and_then(|d| d.get("title")).and_then(Value::as_str) {
            title = value.to_string();
        }
    }
    title
}
