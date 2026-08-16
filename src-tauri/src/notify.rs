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
//!
//! ## Click-to-jump and workspaces
//!
//! Clicking a completion notification (or the dock icon) navigates the window
//! to `?jump=<sessionId>`; the GUI's initialization script writes the
//! frontend's persisted current-session key and the SPA opens that session on
//! boot.
//!
//! Workspace handling: the frontend's session list is a *cross-workspace*
//! aggregate — `session.list` returns every workspace's sessions (verified
//! against a live harness: 37 sessions across 6 workspaces) and the sidebar
//! renders them grouped per workspace, so opening a session of another
//! workspace is a normal, supported action and `?jump=` works for it too. The
//! frontend has no persisted "current workspace" key to switch (the only
//! persisted keys across all client bundles are `dsh.sessions.current`,
//! `dsh.conversation.chat`, `dsh.workspace.view.v5` and
//! `dsh.trajectory.duration`), which is why no `?ws=` parameter exists. We
//! still probe the harness (`workspace.list`) before navigating: it resolves
//! the target workspace for the log line, and when the probe itself fails
//! (server unreachable / API hiccup) we focus the window only instead of
//! reloading the GUI on uncertainty.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// How long to wait for the store to exist before giving up on the first pass.
const STORE_BOOT_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on a decoded session log, per file per pass. Session logs can
/// legitimately grow large (tool output, file reads), but an unbounded decode
/// would let a single huge log pin memory every poll; past the cap the log is
/// skipped (and backed off) instead.
const MAX_DECODED_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB decompressed

/// Compressed size above which a session log is never even opened.
const MAX_COMPRESSED_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB on disk

/// Timeout for the loopback `/api` probes used by [`jump_to_last`] (the
/// bundled harness answers in milliseconds; anything slower is a stall).
const API_TIMEOUT: Duration = Duration::from_secs(2);

/// Upper bound on a `/api` probe response body (workspace.list / session.list
/// stay well under this; the cap only guards against a runaway server).
const MAX_API_BODY_BYTES: u64 = 4 * 1024 * 1024; // 4 MiB

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
        .unwrap_or_else(|| PathBuf::from(".dsh"))
}

/// One watched session log: its last observed size plus the turn/end events
/// already notified (`(turn, seq)` pairs, so a file append is idempotent).
/// `baselined` marks the first observation of the file: its history is
/// recorded silently so the app never replays turns that finished before (or
/// around) launch. `consecutive_failures` counts decode failures so a broken
/// file is backed off instead of re-decoded (and re-failed) every pass.
#[derive(Default)]
struct FileState {
    size: u64,
    baselined: bool,
    consecutive_failures: u32,
    notified: std::collections::HashSet<(u64, u64)>,
}

/// A session log that keeps failing to decode is left alone for this many
/// polling passes before we try again.
const FAILURE_BACKOFF_PASSES: u32 = 30; // 30 × 2s ≈ 1 minute of quiet

/// Shared watcher state, owned by the app (see [`spawn`]).
///
/// `last_notified` records the most recent completion notification (session
/// log path + when it fired); `last_focus` is updated by the window's
/// Focused event. [`jump_to_last`] navigates to the completed session only
/// when there is a notification the user has not yet seen (i.e. it fired
/// after the window was last focused) — clicking the notification or the
/// dock icon then lands on that conversation.
pub struct NotifyState {
    files: Mutex<HashMap<PathBuf, FileState>>,
    last_notified: Mutex<Option<(PathBuf, std::time::Instant)>>,
    pub last_focus: Arc<Mutex<Option<std::time::Instant>>>,
}

impl Default for NotifyState {
    fn default() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
            last_notified: Mutex::new(None),
            last_focus: Arc::new(Mutex::new(None)),
        }
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
        let entry = files.entry(path.clone()).or_default();
        if entry.consecutive_failures > 0 {
            entry.consecutive_failures -= 1;
            continue; // backing off a previously-broken file
        }
        let (size, completed) = match read_completed_turns(&path) {
            Ok(found) => found,
            Err(_) => {
                // Mid-write (dsh appends whole zstd frames, so a torn tail
                // fails the whole decode) or unreadable; back off so a
                // persistently broken file is not re-decoded every pass.
                entry.consecutive_failures = FAILURE_BACKOFF_PASSES;
                continue;
            }
        };
        if entry.size > size {
            // The log was truncated/rotated; re-base so we do not resend the
            // whole history.
            entry.notified.clear();
            entry.baselined = false;
        }
        if entry.baselined && entry.size == size {
            // Unchanged since the last scan: skip the expensive full decode.
            continue;
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
/// `turn/end` events found (reason `completed` or `error`). Decoding is
/// bounded: a session log that expands beyond the cap (or is compressed
/// beyond the hard limit) is treated as an error so a runaway or hostile
/// log cannot exhaust memory.
fn read_completed_turns(path: &Path) -> Result<(u64, Vec<CompletedTurn>), String> {
    let file = File::open(path).map_err(|err| format!("open {}: {err}", path.display()))?;
    let size = file
        .metadata()
        .map_err(|err| format!("stat {}: {err}", path.display()))?
        .len();
    if size > MAX_COMPRESSED_BYTES {
        return Err(format!("session log too large ({} bytes)", size));
    }
    let mut decoder =
        zstd::stream::read::Decoder::new(file).map_err(|err| format!("zstd header: {err}"))?;
    let mut raw = String::new();
    decoder
        .by_ref()
        .take(MAX_DECODED_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|err| format!("zstd decode: {err}"))?;
    if raw.len() as u64 > MAX_DECODED_BYTES {
        return Err(format!("session log expands beyond {} bytes", MAX_DECODED_BYTES));
    }
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
    // Remember this notification for jump_to_last (only the most recent one
    // matters: clicking the notification should land on the latest finished
    // session).
    if let Ok(mut guard) = app.state::<NotifyState>().last_notified.lock() {
        *guard = Some((path.to_path_buf(), std::time::Instant::now()));
    }
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

/// Navigate the main window to the session of the most recent completion
/// notification — but only when that notification has not been seen yet
/// (it fired after the window was last focused). Called from `RunEvent::Reopen`
/// (macOS dock/notification click); a plain dock click with nothing new does
/// nothing.
///
/// Before navigating, the harness's `workspace.list` is probed to resolve the
/// target workspace for the log (the frontend's session list is a
/// cross-workspace aggregate, so a session of another workspace opens fine —
/// see the module docs). When the probe itself fails, we focus the window
/// only and never reload the GUI on uncertainty.
pub fn jump_to_last(app: &AppHandle) {
    let Some(origin) = server_origin(app) else {
        return;
    };
    let state = app.state::<NotifyState>();
    let (session_path, notified_at) = {
        let guard = state.last_notified.lock().ok();
        match guard.and_then(|g| g.clone()) {
            Some((path, at)) => (Some(path), Some(at)),
            None => (None, None),
        }
    };
    let (Some(session_path), Some(notified_at)) = (session_path, notified_at) else {
        return;
    };
    let Some(session_id) = session_path
        .parent()
        .and_then(|dir| dir.file_name())
        .map(|name| name.to_string_lossy().into_owned())
    else {
        return;
    };
    // Only jump when the notification has not been seen yet (it fired after
    // the window was last focused); a plain dock click with nothing new does
    // nothing.
    let seen = state
        .last_focus
        .lock()
        .ok()
        .and_then(|g| *g)
        .is_some_and(|since| since > notified_at);
    if seen {
        return;
    }
    // Probe the harness for the target workspace (log-only; the GUI groups
    // sessions per workspace, so if the target workspace is not the one
    // currently shown the user can switch to it in the sidebar after the
    // jump). A failed probe means the server may not be healthy — focus only.
    match resolve_target_workspace(&origin, &session_path) {
        Ok(Some((workspace_id, label))) => {
            eprintln!(
                "[dsh-desktop] session {session_id} belongs to workspace {label} \
                 (id {workspace_id}); the GUI lists sessions across workspaces, \
                 so the jump opens it regardless of the currently shown workspace"
            );
        }
        Ok(None) => {
            eprintln!(
                "[dsh-desktop] session {session_id}: target workspace not found \
                 in workspace.list (removed?); jumping anyway"
            );
        }
        Err(err) => {
            eprintln!(
                "[dsh-desktop] cannot probe the workspace of session {session_id} \
                 ({err}); focusing the window only"
            );
            return;
        }
    }
    // Same origin as the served GUI, so the navigation fence in lib.rs lets
    // it through; the initialization script picks up ?jump= on load.
    let mut url = origin;
    url.set_query(Some(&format!("jump={session_id}")));
    if let Some(window) = app.get_webview_window("main") {
        eprintln!("[dsh-desktop] jumping to session {session_id}");
        let _ = window.navigate(url);
    }
}

/// The current server origin, if the server has become ready.
fn server_origin(app: &AppHandle) -> Option<tauri::Url> {
    app.try_state::<crate::ServerOrigin>()?.0.lock().ok()?.clone()
}

/// Resolve the workspace that owns a session log path by asking the harness
/// `workspace.list` and matching each workspace `path` against the session
/// store's slug encoding. `Ok(None)` means the workspace is not in the list
/// (e.g. it was removed); `Err` means the probe itself failed.
fn resolve_target_workspace(origin: &tauri::Url, session_path: &Path) -> Result<Option<(String, String)>, String> {
    let Some(slug) = session_store_workspace(session_path) else {
        return Ok(None);
    };
    let request = serde_json::json!({
        "type": "client-request",
        "rpcId": "dsh-desktop-workspace-list",
        "method": "workspace.list",
        "payload": {}
    });
    let response = api_post(origin, "workspace.list", &request)?;
    let items = response
        .pointer("/result/value/items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "workspace.list response has no items".to_string())?;
    for item in items {
        let Some(path) = item.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if workspace_slug_of(path) != slug {
            continue;
        }
        let label = match item.get("title").and_then(serde_json::Value::as_str) {
            Some(title) => format!("{title} ({path})"),
            None => path.to_string(),
        };
        let workspace_id = item
            .get("workspaceId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        return Ok(Some((workspace_id, label)));
    }
    Ok(None)
}

/// The session-store directory name for a workspace path: `/a/b` →
/// `--a-b--` (the harness slugs workspace paths this way under
/// `$DSH_HOME/sessions`).
fn workspace_slug_of(path: &str) -> String {
    let inner = path.trim_start_matches('/').replace('/', "-");
    format!("--{inner}--")
}

/// The workspace slug of a session log path (`<store>/<slug>/<id>/session.jsonl.zstd`).
fn session_store_workspace(path: &Path) -> Option<String> {
    path.parent()?
        .parent()?
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Minimal HTTP/1.1 JSON POST to the bundled harness's `/api` gateway
/// (loopback only, same host/port as the served GUI). The body is the
/// client-request envelope and the args live directly in `payload` (the
/// harness parses the whole payload per method). Returns the parsed response
/// body; a non-2xx status or a malformed body is an error.
fn api_post(origin: &tauri::Url, method: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    use std::io::BufRead;
    use std::io::BufReader;

    let host = origin.host_str().unwrap_or("127.0.0.1").to_string();
    let port = origin.port().unwrap_or(80);
    let payload = body.to_string();
    let request = format!(
        "POST /api/{method} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {payload}",
        payload.len()
    );
    let mut stream = TcpStream::connect((host.as_str(), port))
        .map_err(|err| format!("connect {host}:{port}: {err}"))?;
    stream
        .set_read_timeout(Some(API_TIMEOUT))
        .map_err(|err| format!("set read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(API_TIMEOUT))
        .map_err(|err| format!("set write timeout: {err}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("write: {err}"))?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|err| format!("read status: {err}"))?;
    let status = status_line.split_whitespace().nth(1).unwrap_or("");
    let mut content_length: Option<u64> = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| format!("read header: {err}"))?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
    if !status.starts_with('2') {
        return Err(format!("HTTP status {status}"));
    }
    let mut bytes = Vec::new();
    match content_length {
        Some(len) => {
            if len > MAX_API_BODY_BYTES {
                return Err(format!("response too large ({len} bytes)"));
            }
            reader
                .take(len)
                .read_to_end(&mut bytes)
                .map_err(|err| format!("read body: {err}"))?;
        }
        None => {
            reader
                .take(MAX_API_BODY_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|err| format!("read body: {err}"))?;
            if bytes.len() as u64 > MAX_API_BODY_BYTES {
                return Err("response too large".to_string());
            }
        }
    }
    serde_json::from_slice(&bytes).map_err(|err| format!("parse JSON: {err}"))
}

/// The last `session/title` event's title in the log, if any. Bounded like
/// `read_completed_turns`; title is additionally truncated to a sane length
/// for the notification.
fn session_title(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    if file.metadata().map(|m| m.len()).unwrap_or(0) > MAX_COMPRESSED_BYTES {
        return String::new();
    }
    let mut decoder = match zstd::stream::read::Decoder::new(file) {
        Ok(decoder) => decoder,
        Err(_) => return String::new(),
    };
    let mut raw = String::new();
    if decoder
        .by_ref()
        .take(MAX_DECODED_BYTES + 1)
        .read_to_string(&mut raw)
        .is_err()
        || raw.len() as u64 > MAX_DECODED_BYTES
    {
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
    // Strip control characters and cap the length for the notification.
    let clean: String = title
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    clean
}
