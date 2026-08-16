//! Bundled-server process management.
//!
//! The desktop shell ships its own Node.js runtime and a production install
//! of `@deepseek-ai/dsh` (see `scripts/assemble-runtime.mjs`). This module
//! spawns that server with `--port 0` (the OS picks a free port, so a stray
//! `dsh web` or another app can never collide), reads the readiness URL line
//! (`dsh web: http://127.0.0.1:<port>`) from its stdout, polls the served
//! root, and navigates the window there. Server output is mirrored to a log
//! file under the platform app-log directory for support.

use std::fs::{create_dir_all, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Url, WebviewWindow};

use crate::{local_app_url, ServerChild, ServerOrigin};

/// How long the server may take to print its readiness URL before we give up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
/// Poll interval while waiting for the URL line / server health.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

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
fn runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
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
fn runtime_binaries(app: &AppHandle) -> Result<(PathBuf, PathBuf), String> {
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
        .args(["--profile", "web", "--port", "0"])
        .current_dir(home_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .map_err(|err| format!("failed to spawn {}: {err}", node_path.display()))?;
    Ok(SpawnedServer { child })
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
                    eprintln!("[dsh-desktop] server stdout already taken");
                    let _ = window.navigate(local_app_url("error.html"));
                    return;
                }
            },
            None => {
                eprintln!("[dsh-desktop] server child missing while starting supervisor");
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

    // Supervisor: wait for the URL (or exit / timeout), then navigate.
    let state_for_supervisor = child_state.clone();
    thread::spawn(move || {
        let start = Instant::now();
        let mut url: Option<Url> = None;
        loop {
            if let Some(url) = url.clone() {
                if http_get_ok(&url) {
                    *origin_state.lock().unwrap() = Some(url.clone());
                    let _ = window.navigate(url);
                    return;
                }
            }
            match rx.try_recv() {
                Ok(ServerEvent::Url(found)) => match found.parse() {
                    Ok(parsed) => url = Some(parsed),
                    Err(_) => {
                        eprintln!("[dsh-desktop] server printed an invalid URL: {found}");
                        break;
                    }
                },
                Ok(ServerEvent::Exited(code)) => {
                    if url.is_none() {
                        eprintln!("[dsh-desktop] server exited before becoming ready (code {code:?})");
                    }
                    break;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => break,
            }
            if start.elapsed() > STARTUP_TIMEOUT {
                eprintln!("[dsh-desktop] timed out waiting for the server (url={url:?})");
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
