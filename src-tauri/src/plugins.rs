//! dsh plugin management: list / search / install / remove.
//!
//! The harness forwards plugin operations to `pnpm` inside the profile
//! directory (`$DSH_HOME/profiles/<name>`), and bundle layers composed from
//! `dsh.profile.bundles` are read at boot — so installs/removals need a
//! server restart to take effect (see the Restart Web Server action in
//! lib.rs). The bundled runtime ships its own pnpm (npm-package form, run on
//! the bundled Node) because normal users do not have pnpm installed; we
//! expose it on PATH via a tiny shim before spawning the `dsh plugin`
//! subcommand.
//!
//! Search hits the GitHub search API (`topic:dsh-plugin`); the unauthenticated
//! rate limit is 10 req/min, so results are cached in memory.
//!
//! Before installing a plugin referenced by a GitHub repo (`owner/repo`), we
//! fetch the repo's `package.json` and verify it declares the dsh plugin
//! contract — a `dsh.bundle` or `dsh.client` field in the manifest. Repos that
//! do not declare either are rejected with an explicit error, because any
//! third-party code installed this way runs with the same permissions as the
//! app itself. Successful installs/removals are appended to an audit log at
//! `$DSH_HOME/desktop-audit.log` (UTC+8 timestamps).

use std::collections::HashMap;
use std::fs::{create_dir_all, write, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// GitHub search rate budget: one fresh search per 6s, cached otherwise.
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(60);
/// The web profile name the desktop shell serves.
const WEB_PROFILE: &str = "web";

/// In-memory search cache (query → results).
#[derive(Default)]
pub struct SearchCache {
    entries: Mutex<HashMap<String, (Instant, Vec<GitHubRepo>)>>,
}

/// A GitHub repository search result item.
#[derive(Clone, Serialize)]
pub struct GitHubRepo {
    pub full_name: String,
    pub description: Option<String>,
    pub stars: u64,
    pub language: Option<String>,
    pub default_branch: String,
    pub pushed_at: Option<String>,
    pub topics: Vec<String>,
}

/// The manifest of the web profile: installed plugin dependencies and the
/// active bundle layer list.
#[derive(Serialize)]
pub struct ProfileManifest {
    pub dependencies: Vec<String>,
    pub bundles: Vec<String>,
}

/// One plugin command's result (stdout + exit status).
#[derive(Serialize)]
pub struct PluginCommandResult {
    pub ok: bool,
    pub output: String,
}

/// The dsh home directory (default `~/.dsh`, honor `DSH_HOME`).
fn dsh_home() -> PathBuf {
    std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".dsh")))
        .unwrap_or_else(|| PathBuf::from(".dsh"))
}

/// The web profile directory.
fn profile_dir() -> PathBuf {
    dsh_home().join("profiles").join(WEB_PROFILE)
}

/// Read the web profile manifest (dependencies + bundle layer list).
#[tauri::command]
pub fn list_plugins(_app: AppHandle) -> Result<ProfileManifest, String> {
    let manifest_path = profile_dir().join("package.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|err| format!("cannot read {}: {err}", manifest_path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("cannot parse {}: {err}", manifest_path.display()))?;
    let dependencies = value
        .get("dependencies")
        .and_then(|d| d.as_object())
        .map(|map| {
            let mut names: Vec<String> = map.keys().cloned().collect();
            names.sort();
            names
        })
        .unwrap_or_default();
    let bundles = value
        .pointer("/dsh/profile/bundles")
        .and_then(|b| b.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    Ok(ProfileManifest { dependencies, bundles })
}

/// Search GitHub for dsh-plugin repositories (cached).
#[tauri::command]
pub fn search_plugins(app: AppHandle, query: Option<String>, page: Option<u32>) -> Result<Vec<GitHubRepo>, String> {
    let q = query.unwrap_or_default().trim().to_string();
    let page = page.unwrap_or(1).clamp(1, 10);
    let cache_key = format!("{}:{}", q, page);
    let cache = app.state::<SearchCache>();
    {
        let guard = cache.entries.lock().map_err(|_| "search cache poisoned".to_string())?;
        if let Some((at, results)) = guard.get(&cache_key) {
            if at.elapsed() < SEARCH_CACHE_TTL {
                return Ok(results.clone());
            }
        }
    }
    let search_q = if q.is_empty() {
        "topic:dsh-plugin".to_string()
    } else {
        format!("topic:dsh-plugin {}", q)
    };
    let url = format!(
        "https://api.github.com/search/repositories?q={}&sort=stars&order=desc&page={}&per_page=30",
        urlencoding(&search_q),
        page
    );
    let body = http_get(&url)?;
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|err| format!("GitHub search returned invalid JSON: {err}"))?;
    let mut repos = Vec::new();
    for item in json.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default() {
        repos.push(GitHubRepo {
            full_name: item.get("full_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            description: item.get("description").and_then(|v| v.as_str()).map(String::from),
            stars: item.get("stargazers_count").and_then(|v| v.as_u64()).unwrap_or(0),
            language: item.get("language").and_then(|v| v.as_str()).map(String::from),
            default_branch: item.get("default_branch").and_then(|v| v.as_str()).unwrap_or("master").to_string(),
            pushed_at: item.get("pushed_at").and_then(|v| v.as_str()).map(String::from),
            topics: item.get("topics").and_then(|t| t.as_array()).map(|arr| {
                arr.iter().filter_map(|v| v.as_str().map(String::from)).collect()
            }).unwrap_or_default(),
        });
    }
    let mut guard = cache.entries.lock().map_err(|_| "search cache poisoned".to_string())?;
    guard.insert(cache_key, (Instant::now(), repos.clone()));
    Ok(repos)
}

/// Install a plugin into the web profile: `dsh plugin --profile web add <spec>`.
///
/// When `spec` looks like a GitHub repo (`owner/repo`, not an `@scope/name`
/// npm spec), the repo's `package.json` is fetched first and must declare the
/// dsh plugin contract (`dsh.bundle` or `dsh.client`) — otherwise the install
/// is refused with an explicit error.
#[tauri::command]
pub fn install_plugin(app: AppHandle, spec: String) -> Result<PluginCommandResult, String> {
    let spec = spec.trim();
    if is_github_repo_spec(spec) {
        verify_github_plugin_manifest(spec)?;
    }
    let result = run_plugin_command(&app, &["add", spec])?;
    if result.ok {
        append_audit("install", spec, "success");
    }
    Ok(result)
}

/// Remove a plugin from the web profile: `dsh plugin --profile web remove <name>`.
#[tauri::command]
pub fn uninstall_plugin(app: AppHandle, name: String) -> Result<PluginCommandResult, String> {
    let result = run_plugin_command(&app, &["remove", &name])?;
    if result.ok {
        append_audit("uninstall", &name, "success");
    }
    Ok(result)
}

/// A GitHub repo spec is a bare `owner/repo` (contains `/` and is not an npm
/// scoped spec `@scope/name`). Anything else (npm name, scoped package, URL)
/// is passed through to `dsh plugin add` without manifest verification.
fn is_github_repo_spec(spec: &str) -> bool {
    spec.contains('/') && !spec.starts_with('@')
}

/// Fetch `<owner>/<repo>`'s package.json and require the dsh plugin contract.
fn verify_github_plugin_manifest(full_name: &str) -> Result<(), String> {
    let branch = resolve_default_branch(full_name)?;
    let url = format!("https://raw.githubusercontent.com/{full_name}/{branch}/package.json");
    let pkg_text = http_get(&url)?;
    check_plugin_contract(full_name, &pkg_text)
}

/// Resolve the repo's default branch — first via the GitHub API (which also
/// confirms the repo exists), falling back to probing `master` then `main`
/// through the raw package.json endpoint.
fn resolve_default_branch(full_name: &str) -> Result<String, String> {
    let api_url = format!("https://api.github.com/repos/{full_name}");
    if let Ok(body) = http_get(&api_url) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(branch) = json.get("default_branch").and_then(|b| b.as_str()) {
                return Ok(branch.to_string());
            }
        }
    }
    for candidate in ["master", "main"] {
        let probe = format!("https://raw.githubusercontent.com/{full_name}/{candidate}/package.json");
        if http_get(&probe).is_ok() {
            return Ok(candidate.to_string());
        }
    }
    Err(format!(
        "无法确定 {full_name} 的默认分支（GitHub API 与 master/main 探测均失败），已拒绝安装"
    ))
}

/// A real dsh plugin manifest declares `dsh.bundle` (server/bundle plugin) or
/// `dsh.client` (client plugin) in its package.json.
fn check_plugin_contract(full_name: &str, pkg_text: &str) -> Result<(), String> {
    let json: serde_json::Value = serde_json::from_str(pkg_text)
        .map_err(|err| format!("{full_name} 的 package.json 解析失败：{err}"))?;
    let is_plugin = json
        .get("dsh")
        .and_then(|d| d.as_object())
        .is_some_and(|dsh| dsh.contains_key("bundle") || dsh.contains_key("client"));
    if is_plugin {
        Ok(())
    } else {
        Err(format!(
            "拒绝安装 {full_name}：package.json 未声明 dsh.bundle 或 dsh.client，不是有效的 dsh 插件"
        ))
    }
}

/// Append one audit line to `$DSH_HOME/desktop-audit.log`
/// (`YYYY-MM-DD HH:MM:SS +08:00 | action | plugin | result`).
fn append_audit(action: &str, plugin: &str, result: &str) {
    let path = dsh_home().join("desktop-audit.log");
    if let Some(parent) = path.parent() {
        let _ = create_dir_all(parent);
    }
    let line = format!("{} | {} | {} | {}\n", utc8_now(), action, plugin, result);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Current time in UTC+8 (Asia/Shanghai), formatted without external deps.
fn utc8_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_add(8 * 3600);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} +08:00")
}

/// Days-since-epoch → (year, month, day); Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

/// Run `dsh plugin --profile web <args>` with the bundled pnpm on PATH.
fn run_plugin_command(app: &AppHandle, args: &[&str]) -> Result<PluginCommandResult, String> {
    let (node_path, entry_path) = crate::server::runtime_binaries(app)?;
    let pnpm_shim_dir = pnpm_shim_dir(app)?;
    // dsh plugin spawns `pnpm` from PATH; prepend the shim directory.
    let mut path = pnpm_shim_dir.to_string_lossy().into_owned();
    if let Some(existing) = std::env::var_os("PATH") {
        path.push(':');
        path.push_str(&existing.to_string_lossy());
    }
    let output = Command::new(&node_path)
        .arg(&entry_path)
        .args(["plugin", "--profile", WEB_PROFILE])
        .args(args)
        .current_dir(profile_dir())
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|err| format!("failed to run dsh plugin: {err}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Ok(PluginCommandResult {
        ok: output.status.success(),
        output: text,
    })
}

/// Ensure the pnpm shim exists in the runtime and return its directory. The
/// shim execs the bundled Node with the bundled pnpm entry, so the harness's
/// `spawnSync("pnpm", …)` finds a working pnpm without any system install.
fn pnpm_shim_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let runtime = crate::server::runtime_dir(app)?;
    let shim_dir = runtime.join("pnpm").join("bin");
    create_dir_all(&shim_dir).map_err(|err| format!("cannot create {}: {err}", shim_dir.display()))?;
    let shim_path = shim_dir.join("pnpm");
    if !shim_path.is_file() {
        let node = runtime.join("node").join("bin").join(if cfg!(windows) { "node.exe" } else { "node" });
        let entry = runtime.join("pnpm").join("bin").join("pnpm.cjs");
        let shim = format!(
            "#!/bin/sh\nexec \"{}\" \"{}\" \"$@\"\n",
            node.display(),
            entry.display()
        );
        write(&shim_path, shim).map_err(|err| format!("cannot write pnpm shim: {err}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim_path, std::fs::Permissions::from_mode(0o755))
                .map_err(|err| format!("cannot chmod pnpm shim: {err}"))?;
        }
    }
    Ok(shim_dir)
}

/// Minimal URL query encoding (space → %20 etc.), enough for search terms.
fn urlencoding(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Simple HTTPS GET with a browser-ish User-Agent (GitHub API requires one).
fn http_get(url: &str) -> Result<String, String> {
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-H", "User-Agent: dsh-desktop", url])
        .output()
        .map_err(|err| format!("cannot reach GitHub: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "GitHub request failed (HTTP {}): {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
