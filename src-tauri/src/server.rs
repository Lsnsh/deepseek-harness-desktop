//! Bundled-server process management.
//!
//! The desktop shell ships its own Node.js runtime and a production install
//! of `@deepseek-ai/dsh` (see `scripts/assemble-runtime.mjs`). This module
//! spawns that server with `--port 0` (the OS picks a free port, so a stray
//! `dsh web` or another app can never collide), reads the readiness URL line
//! (`dsh web: http://127.0.0.1:<port>`) from its stdout, polls the served
//! root, and navigates the window there. Server output is mirrored to a log
//! file under the platform app-log directory for support.
//!
//! Conflict guard (beta.9): if the user already launched their own `dsh web`
//! (e.g. from a terminal), two dsh processes would share `~/.dsh` and a
//! running session could be corrupted. [`check_conflict`] scans `ps` for a
//! user-launched dsh web (excluding this process and the server the shell
//! itself spawned) and probes its port for dsh's HTML signature;
//! [`take_over`] kills that process and starts the bundled server, while
//! [`attach`] validates the port and navigates the window to the user's dsh
//! (browser mode) without spawning anything.

use std::fs::{create_dir_all, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager, Url, WebviewWindow};

use crate::{local_app_url, ServerChild, ServerOrigin, StartupError};

/// How long the server may take to print its readiness URL before we give up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the server may stay unresponsive after becoming ready before we
/// declare it dead and show the error page.
const UNRESPONSIVE_TIMEOUT: Duration = Duration::from_secs(15);
/// Poll interval while waiting for the URL line / server health.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// dsh web's default port when none is passed on the command line.
const DEFAULT_DSH_PORT: u16 = 3080;
/// How long to wait for a taken-over dsh web process to exit before giving up.
const TAKE_OVER_WAIT: Duration = Duration::from_secs(5);
/// Cap for the HTTP probe body (enough to find the HTML signature).
const PROBE_BODY_LIMIT: usize = 262_144;

/// A spawned server: the child handle (owned by [`ServerChild`] state).
pub struct SpawnedServer {
    pub child: Child,
}

/// Events observed while supervising the server process.
enum ServerEvent {
    /// The readiness URL line was printed: `http://127.0.0.1:<port>`.
    Url(String),
    /// The process exited (with its exit code, if any).
    Exited(Option<i32>),
}

/// The bundled runtime's layout.
///
/// Development builds read the assembled directory straight from the source
/// tree. Release builds ship one gzip archive (`runtime.tar.gz`): the archive
/// is extracted to the app cache on first launch and reused while the
/// archive's structural hash matches (see manifest.json).
pub(crate) fn runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    {
        let _ = app;
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../resources/runtime");
        if !base.is_dir() {
            return Err(format!(
                "bundled runtime missing at {} (run `pnpm run runtime` to assemble it)",
                base.display()
            ));
        }
        Ok(base)
    }
    #[cfg(not(debug_assertions))]
    {
        let archive = app
            .path()
            .resolve(
                "../resources/runtime.tar.gz",
                tauri::path::BaseDirectory::Resource,
            )
            .map_err(|err| format!("cannot resolve the bundled runtime archive: {err}"))?;
        if !archive.is_file() {
            return Err(format!(
                "bundled runtime archive missing at {} (run `pnpm run runtime` to assemble it)",
                archive.display()
            ));
        }
        let cache_root = app
            .path()
            .app_cache_dir()
            .map_err(|err| format!("cannot resolve the app cache directory: {err}"))?;
        create_dir_all(&cache_root)
            .map_err(|err| format!("cannot create {}: {err}", cache_root.display()))?;
        let key = archive_manifest_key(&archive)?;
        let target = cache_root.join(format!("runtime-{}", &key[..key.len().min(12)]));
        if !target.join("manifest.json").is_file() {
            // Extract a fresh copy for this archive's content hash.
            let _ = std::fs::remove_dir_all(&target);
            create_dir_all(&target)
                .map_err(|err| format!("cannot create {}: {err}", target.display()))?;
            let status = Command::new("tar")
                .args(["-xzf"])
                .arg(&archive)
                .arg("-C")
                .arg(&target)
                .status()
                .map_err(|err| format!("cannot run tar: {err}"))?;
            if !status.success() {
                return Err(format!("failed to extract the runtime archive to {}", target.display()));
            }
        }
        // Drop stale runtime copies from previous app versions.
        if let Ok(entries) = std::fs::read_dir(&cache_root) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("runtime-") && entry.path() != target {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        Ok(target)
    }
}

/// Read the `nodeModulesHash` out of the archive's embedded manifest without
/// extracting the whole archive (`tar -xOf` streams the member).
#[cfg(not(debug_assertions))]
fn archive_manifest_key(archive: &std::path::Path) -> Result<String, String> {
    let output = Command::new("tar")
        .args(["-xOf"])
        .arg(archive)
        .arg("manifest.json")
        .output()
        .map_err(|err| format!("cannot read the runtime manifest from the archive: {err}"))?;
    if !output.status.success() {
        return Err("cannot read the runtime manifest from the archive".to_string());
    }
    let manifest: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("cannot parse the runtime manifest: {err}"))?;
    manifest
        .get("nodeModulesHash")
        .and_then(|value| value.as_str())
        .map(String::from)
        .ok_or_else(|| "runtime manifest has no nodeModulesHash".to_string())
}

/// The bundled Node.js binary and the deployed CLI entry.
pub(crate) fn runtime_binaries(app: &AppHandle) -> Result<(PathBuf, PathBuf), String> {
    let runtime = runtime_dir(app)?;
    let manifest_path = runtime.join("manifest.json");
    let (node_rel, entry_rel) = if manifest_path.is_file() {
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path)
                .map_err(|err| format!("cannot read {}: {err}", manifest_path.display()))?,
        )
        .map_err(|err| format!("cannot parse {}: {err}", manifest_path.display()))?;
        let node_bin = manifest
            .get("nodeBin")
            .and_then(|value| value.as_str())
            .unwrap_or(if cfg!(windows) { "node.exe" } else { "node" });
        let entry = manifest
            .get("appEntry")
            .and_then(|value| value.as_str())
            .unwrap_or("app/lib/bin.js");
        (format!("node/bin/{node_bin}"), entry.to_string())
    } else {
        let node = if cfg!(windows) { "node/bin/node.exe" } else { "node/bin/node" };
        (node.to_string(), "app/lib/bin.js".to_string())
    };
    let node_path = runtime.join(node_rel);
    let entry_path = runtime.join(entry_rel);
    if !node_path.is_file() {
        return Err(format!("bundled Node.js binary missing at {}", node_path.display()));
    }
    if !entry_path.is_file() {
        return Err(format!("bundled CLI entry missing at {}", entry_path.display()));
    }
    Ok((node_path, entry_path))
}

/// The platform app-log directory, created on demand.
pub fn log_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_log_dir()
        .map_err(|err| format!("cannot resolve the app log directory: {err}"))?;
    create_dir_all(&dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    Ok(dir)
}

/// The user's home directory (server working directory).
fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
    }
}

/// Spawn the bundled server. The child handle is returned; stdout/stderr are
/// piped for the supervisor.
pub fn spawn_server(app: &AppHandle) -> Result<SpawnedServer, String> {
    let (node_path, entry_path) = runtime_binaries(app)?;
    let mut command = Command::new(&node_path);
    command
        .arg(&entry_path)
        // --host is pinned explicitly: the shell must never silently serve
        // the (unauthenticated) harness GUI on anything but loopback, even if
        // a future dsh release changes its default bind address.
        // --no-open: dsh (0.1.1-rc.2+) opens the served URL in the user's
        // default browser on boot; the desktop window IS the UI, so opening
        // a second browser surface on every launch is noise (and confusing
        // on attach flows).
        .args(["--profile", "web", "--host", "127.0.0.1", "--port", "0", "--no-open"])
        .current_dir(home_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn {}: {err}", node_path.display()))?;
    Ok(SpawnedServer { child })
}

/// Record one startup/supervision failure reason for the error page (the
/// page reads it via the `server_diagnostics` command). Cleared on every new
/// spawn.
pub(crate) fn record_startup_error(app: &AppHandle, reason: String) {
    eprintln!("[dsh-desktop] {reason}");
    if let Some(state) = app.try_state::<StartupError>() {
        if let Ok(mut guard) = state.0.lock() {
            *guard = Some(reason);
        }
    }
}

/// Supervise the spawned server: mirror its output to the log file, wait for
/// the readiness URL line, verify the served root, navigate the window, and
/// fall back to the error page on failure. Non-blocking: runs on threads.
pub fn watch_server(app: AppHandle, window: WebviewWindow) {
    let child_state = app.state::<ServerChild>().0.clone();
    let origin_state = app.state::<ServerOrigin>().0.clone();

    let stdout = {
        let mut guard = child_state.lock().unwrap();
        match guard.as_mut() {
            Some(child) => match child.stdout.take() {
                Some(stdout) => stdout,
                None => {
                    record_startup_error(&app, "server stdout already taken".to_string());
                    let _ = window.navigate(local_app_url("error.html"));
                    return;
                }
            },
            None => {
                record_startup_error(&app, "server child missing while starting supervisor".to_string());
                let _ = window.navigate(local_app_url("error.html"));
                return;
            }
        }
    };
    let stderr = {
        let mut guard = child_state.lock().unwrap();
        guard.as_mut().and_then(|child| child.stderr.take())
    };

    let (tx, rx) = channel::<ServerEvent>();
    let log_path = log_dir(&app).ok();

    // stdout: mirror to the log and parse the readiness URL line.
    let tx_stdout = tx.clone();
    let log_path_stdout = log_path.clone();
    thread::spawn(move || {
        let mut log = open_log(&log_path_stdout, "dsh-web.log");
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            mirror(&mut log, &line);
            if let Some(port) = parse_port(&line) {
                let url = format!("http://127.0.0.1:{port}");
                if tx_stdout.send(ServerEvent::Url(url)).is_err() {
                    break;
                }
            }
        }
    });

    // stderr: mirror to the log only.
    if let Some(stderr) = stderr {
        let log_path_stderr = log_path.clone();
        thread::spawn(move || {
            let mut log = open_log(&log_path_stderr, "dsh-web.log");
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                mirror(&mut log, &line);
            }
        });
    }

    // Exit watcher: report when the process is gone.
    let tx_exit = tx.clone();
    let state_for_watcher = child_state.clone();
    thread::spawn(move || {
        loop {
            let exited = {
                let mut guard = state_for_watcher.lock().unwrap();
                match guard.as_mut().and_then(|child| child.try_wait().ok().flatten()) {
                    Some(status) => {
                        guard.take();
                        Some(status.code())
                    }
                    None => None,
                }
            };
            if let Some(code) = exited {
                let _ = tx_exit.send(ServerEvent::Exited(code));
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }
    });

    // Supervisor: wait for the URL (or exit / timeout), navigate, then keep
    // supervising the live server so a crash after readiness still lands on
    // the error page instead of a dead window.
    let state_for_supervisor = child_state.clone();
    thread::spawn(move || {
        let start = Instant::now();
        let mut url: Option<Url> = None;
        let mut ready = false;
        let mut unresponsive_since: Option<Instant> = None;
        loop {
            if let Some(url) = url.clone() {
                if http_get_ok(&url) {
                    if !ready {
                        *origin_state.lock().unwrap() = Some(url.clone());
                        let _ = window.navigate(url.clone());
                        ready = true;
                    }
                    unresponsive_since = None;
                } else if ready {
                    // The server was ready but is now unresponsive; give it a
                    // grace window before declaring it dead.
                    let since = *unresponsive_since.get_or_insert(Instant::now());
                    if since.elapsed() > UNRESPONSIVE_TIMEOUT {
                        record_startup_error(
                            &app,
                            format!("服务器就绪后停止响应（{} 秒无 HTTP 应答）", UNRESPONSIVE_TIMEOUT.as_secs()),
                        );
                        break;
                    }
                }
            }
            match rx.try_recv() {
                Ok(ServerEvent::Url(found)) => match found.parse() {
                    Ok(parsed) => url = Some(parsed),
                    Err(_) => {
                        record_startup_error(&app, format!("server printed an invalid URL: {found}"));
                        break;
                    }
                },
                Ok(ServerEvent::Exited(code)) => {
                    record_startup_error(
                        &app,
                        format!(
                            "服务器进程在就绪前退出（退出码 {code:?}）；详情见 dsh-web.log"
                        ),
                    );
                    break;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break,
            }
            if !ready && start.elapsed() > STARTUP_TIMEOUT {
                record_startup_error(
                    &app,
                    format!("等待服务器就绪超时（{} 秒）；详情见 dsh-web.log", STARTUP_TIMEOUT.as_secs()),
                );
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }
        // Failure path: stop the server and show the error page.
        if let Some(mut child) = state_for_supervisor.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = window.navigate(local_app_url("error.html"));
    });
}

/// Parse the readiness URL line: `dsh web: http://127.0.0.1:<port>`.
fn parse_port(line: &str) -> Option<String> {
    let marker = "dsh web: http://127.0.0.1:";
    let index = line.find(marker)?;
    let rest = &line[index + marker.len()..];
    let port: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if port.is_empty() {
        None
    } else {
        Some(port)
    }
}

/// Open (and create) the log file; `None` when logging is unavailable.
fn open_log(dir: &Option<PathBuf>, name: &str) -> Option<File> {
    let dir = dir.as_ref()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))
        .map_err(|err| eprintln!("[dsh-desktop] cannot open log file: {err}"))
        .ok()
}

/// Write one server line to the log file.
fn mirror(log: &mut Option<File>, line: &str) {
    if let Some(file) = log {
        let _ = writeln!(file, "{line}");
    }
}

/// Minimal HTTP GET returning whether the root answered 2xx/3xx.
fn http_get_ok(url: &Url) -> bool {
    let host = url.host_str().unwrap_or("127.0.0.1");
    let port = url.port().unwrap_or(80);
    let Ok(mut stream) = TcpStream::connect((host, port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let request = format!("GET / HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut head = [0u8; 64];
    let Ok(n) = stream.read(&mut head) else {
        return false;
    };
    let head = String::from_utf8_lossy(&head[..n]);
    let status = head.split_whitespace().nth(1).unwrap_or("");
    status.starts_with('2') || status.starts_with('3')
}

/// Startup conflict scan result.
#[derive(Serialize)]
pub struct ConflictInfo {
    pub has_conflict: bool,
    pub port: Option<u16>,
}

/// Scan for a user-launched `dsh web` that shares the `~/.dsh` session store,
/// excluding this process and any server the desktop shell itself spawned
/// (which uses `--port 0` and is tracked in [`ServerChild`]). The candidate's
/// port comes from its command line (`--port N`) or defaults to 3080, and is
/// only reported as a conflict after the root answers with dsh's HTML
/// signature — so a random process named "dsh" cannot trip the guard.
#[tauri::command]
pub fn check_conflict(app: AppHandle) -> Result<ConflictInfo, String> {
    let own_pid = std::process::id();
    let spawned_pid = app
        .state::<ServerChild>()
        .0
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|child| child.id()));
    for (pid, command) in list_processes()? {
        if pid as u32 == own_pid || Some(pid as u32) == spawned_pid {
            continue;
        }
        if !is_dsh_web_command(&command) {
            continue;
        }
        let port = port_from_command(&command).unwrap_or(DEFAULT_DSH_PORT);
        if probe_dsh(port) {
            return Ok(ConflictInfo { has_conflict: true, port: Some(port) });
        }
    }
    Ok(ConflictInfo { has_conflict: false, port: None })
}

/// Take over: kill the user's `dsh web` on `port` (and only dsh web processes
/// on that port — never anything else), then start the bundled server and
/// watch it exactly like a normal launch.
#[tauri::command]
pub fn take_over(app: AppHandle, port: u16) -> Result<(), String> {
    kill_dsh_on_port(&app, port)?;
    crate::spawn_and_watch(&app)
}

/// Attach (browser mode): verify `port` really serves dsh, remember it as the
/// navigation origin, and point the window at it without spawning a server.
/// The navigation fence in lib.rs already allows this origin once
/// [`ServerOrigin`] is set, so external links still leave via the browser.
#[tauri::command]
pub fn attach(app: AppHandle, port: u16) -> Result<(), String> {
    let url_text = format!("http://127.0.0.1:{port}/");
    let body = http_get_body(&url_text)?;
    if !looks_like_dsh(&body) {
        return Err(format!(
            "端口 {port} 上运行的服务不是 dsh web（缺少 dsh 页面特征），已拒绝连接"
        ));
    }
    let url: Url = url_text
        .parse()
        .map_err(|err| format!("invalid attach URL {url_text}: {err}"))?;
    *app.state::<ServerOrigin>().0.lock().unwrap() = Some(url.clone());
    // Attach mode owns no server child; drop any (should not exist) and never
    // touch the user's process.
    if let Some(mut child) = app.state::<ServerChild>().0.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "main window not found".to_string())?;
    window
        .navigate(url)
        .map_err(|err| format!("无法导航到 {url_text}: {err}"))?;
    Ok(())
}

/// `(pid, command)` for every process, via `ps -axo pid=,command=`.
fn list_processes() -> Result<Vec<(i32, String)>, String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .map_err(|err| format!("cannot list processes: {err}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let Some(pid) = parts.next().and_then(|part| part.trim().parse::<i32>().ok()) else {
            continue;
        };
        processes.push((pid, parts.next().unwrap_or("").trim().to_string()));
    }
    Ok(processes)
}

/// Whether a process command line looks like a user-launched `dsh web`
/// (`dsh web …` or `… --profile web …`). The desktop shell's own server runs
/// `node …/bin.js --profile web` and is excluded by PID in [`check_conflict`],
/// so the `dsh` token requirement mainly matches `dsh web` on PATH.
fn is_dsh_web_command(command: &str) -> bool {
    command.contains("dsh")
        && (command.contains("--profile web")
            || command.contains(" web ")
            || command.trim_end().ends_with(" web"))
}

/// Parse `--port N` / `--port=N` from a command line.
fn port_from_command(command: &str) -> Option<u16> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    for (index, token) in tokens.iter().enumerate() {
        if *token == "--port" {
            if let Some(next) = tokens.get(index + 1) {
                if let Ok(port) = next.parse() {
                    return Some(port);
                }
            }
        } else if let Some(value) = token.strip_prefix("--port=") {
            if let Ok(port) = value.parse() {
                return Some(port);
            }
        }
    }
    None
}

/// Whether `http://127.0.0.1:<port>/` answers with dsh's HTML signature.
fn probe_dsh(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/");
    http_get_body(&url).map(|body| looks_like_dsh(&body)).unwrap_or(false)
}

/// dsh's served index carries the React mount point and the boot marker.
fn looks_like_dsh(body: &str) -> bool {
    body.contains("__DSH_BOOT__") || body.contains("<div id=\"root\"")
}

/// Minimal HTTP GET returning the (first 256 KiB of the) response body.
fn http_get_body(url: &str) -> Result<String, String> {
    let parsed: Url = url
        .parse()
        .map_err(|err| format!("invalid URL {url}: {err}"))?;
    let host = parsed.host_str().unwrap_or("127.0.0.1").to_string();
    let port = parsed.port().unwrap_or(80);
    let mut stream = TcpStream::connect((host.as_str(), port))
        .map_err(|err| format!("cannot connect {url}: {err}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let request = format!("GET / HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("request to {url} failed: {err}"))?;
    let mut body = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                body.extend_from_slice(&chunk[..n]);
                if body.len() > PROBE_BODY_LIMIT {
                    break;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(err) => return Err(format!("read from {url} failed: {err}")),
        }
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Kill the dsh web process(es) listening on `port`. Guards: only kills
/// processes whose command line matches dsh web, never the current process or
/// a server this app spawned. Waits up to [`TAKE_OVER_WAIT`] for exit.
fn kill_dsh_on_port(app: &AppHandle, port: u16) -> Result<(), String> {
    let output = Command::new("lsof")
        .args(["-ti", &format!("tcp:{port}")])
        .output()
        .map_err(|err| format!("cannot inspect port {port}: {err}"))?;
    let pids: Vec<i32> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();
    if pids.is_empty() {
        return Err(format!("端口 {port} 上未发现任何进程，已中止接管"));
    }
    let own_pid = std::process::id();
    let spawned_pid = app
        .state::<ServerChild>()
        .0
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|child| child.id()));
    let mut targets = Vec::new();
    for pid in &pids {
        if *pid as u32 == own_pid || Some(*pid as u32) == spawned_pid {
            return Err(format!("端口 {port} 上的进程 (PID {pid}) 是桌面端自身，已中止接管"));
        }
        let command = process_command(*pid).unwrap_or_default();
        if !is_dsh_web_command(&command) {
            return Err(format!(
                "端口 {port} 上的进程 (PID {pid}) 不是 dsh web（{command}），已中止接管"
            ));
        }
        targets.push(*pid);
    }
    for pid in &targets {
        eprintln!("[dsh-desktop] taking over: terminating user dsh web (PID {pid}) on port {port}");
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
    let deadline = Instant::now() + TAKE_OVER_WAIT;
    while Instant::now() < deadline {
        if !targets.iter().any(|pid| process_exists(*pid)) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "端口 {port} 上的 dsh web 进程（PID {targets:?}）未能按时退出，已中止接管"
    ))
}

/// The command line of a PID (empty when it no longer exists).
fn process_command(pid: i32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.trim().to_string())
}

/// Whether a PID still exists (`kill -0`).
fn process_exists(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// What the error page shows: the recorded failure reason, the tail of the
/// server log, and version provenance — everything needed to tell a store
/// migration crash from a Gatekeeper kill or a port/profile problem.
#[derive(Serialize)]
pub struct ServerDiagnostics {
    pub reason: Option<String>,
    pub log_path: Option<String>,
    pub log_tail: Vec<String>,
    pub app_version: String,
    pub dsh_version: Option<String>,
}

/// Maximum number of log lines handed to the error page.
const LOG_TAIL_LINES: usize = 120;

#[tauri::command]
pub fn server_diagnostics(app: AppHandle) -> ServerDiagnostics {
    let reason = app
        .try_state::<StartupError>()
        .and_then(|state| state.0.lock().ok().and_then(|guard| guard.clone()));
    let log_path = log_dir(&app).ok().map(|dir| dir.join("dsh-web.log"));
    let log_tail = log_path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(LOG_TAIL_LINES);
            lines[start..].iter().map(|line| line.to_string()).collect()
        })
        .unwrap_or_default();
    let dsh_version = runtime_dir(&app)
        .ok()
        .and_then(|runtime| std::fs::read_to_string(runtime.join("manifest.json")).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| json.get("dsh").and_then(|v| v.as_str()).map(String::from));
    ServerDiagnostics {
        reason,
        log_path: log_path.map(|path| path.to_string_lossy().into_owned()),
        log_tail,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        dsh_version,
    }
}

/// Error-page retry: kill whatever is left of the old server, spawn a fresh
/// one, and send the main window back to the splash. The supervisor navigates
/// to the GUI once the new server is ready (or back here on failure).
#[tauri::command]
pub fn retry_server(app: AppHandle) -> Result<(), String> {
    crate::restart_server(&app);
    Ok(())
}
