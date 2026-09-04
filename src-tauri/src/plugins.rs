//! dsh plugin management: list / search / install / remove.
//!
//! The harness forwards plugin operations to `pnpm` inside the profile
//! directory (`$DSH_HOME/profiles/<name>`), and bundle layers composed from
//! `dsh.profile.bundles` are read at boot — so installs/removals only take
//! effect after the server recomposes. The shell therefore restarts the web
//! server automatically after every successful install/remove (the "Restart
//! Web Server" menu action remains as a manual fallback). The bundled runtime
//! ships its own pnpm (npm-package form, run on the bundled Node) because
//! normal users do not have pnpm installed; we expose it on PATH via a tiny
//! shim before spawning the `dsh plugin` subcommand.
//!
//! Search hits the GitHub search API (`topic:dsh-plugin`); the unauthenticated
//! rate limit is 10 req/min, so results are cached in memory.
//!
//! Safety & UX (beta.5 / beta.7):
//! - Before installing a plugin referenced by a GitHub repo (`owner/repo`), we
//!   fetch the repo's `package.json` and require the dsh plugin contract
//!   (`dsh.bundle` or `dsh.client`); non-plugin repos are rejected with an
//!   explicit error.
//! - Install/uninstall output is streamed to the frontend via the
//!   `plugin-progress` Tauri event so the Plugin Manager shows live output
//!   instead of blocking silently until completion.
//! - Successful installs/removals (and failures) are appended to an audit log
//!   at `$DSH_HOME/desktop-audit.log` (UTC+8 timestamps).
//! - Installed versions are reported from the profile's `node_modules`, and
//!   for plugins installed from a GitHub repo, the latest remote release/tag
//!   is checked (cached) to flag "update available".

use std::collections::{HashMap, HashSet};
use std::fs::{create_dir_all, write, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

/// GitHub search rate budget: one fresh search per 6s, cached otherwise.
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(60);
/// Remote-version check budget: at most one fresh GitHub request per repo
/// every 10 minutes (unauthenticated API limit is 60 req/h).
const UPDATE_CACHE_TTL: Duration = Duration::from_secs(600);
/// The web profile name the desktop shell serves.
const WEB_PROFILE: &str = "web";

/// In-memory search cache (query → results).
#[derive(Default)]
pub struct SearchCache {
    entries: Mutex<HashMap<String, (Instant, Vec<GitHubRepo>)>>,
}

/// Process-global remote-version cache (full_name → (checked_at, latest tag)).
static UPDATE_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Option<String>)>>> = OnceLock::new();

/// Process-global repo-metadata cache (full_name → (checked_at, RepoMeta)).
/// Same TTL budget as the remote-version cache: the unauthenticated GitHub
/// core API limit is 60 req/h, and the installed GitHub-sourced plugin set is
/// small, so one fresh `/repos/{full_name}` per plugin per 10 min is plenty.
static REPO_META_CACHE: OnceLock<Mutex<HashMap<String, (Instant, Option<RepoMeta>)>>> = OnceLock::new();

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
    /// Owner avatar URL from the search item's `owner.avatar_url` (already in
    /// the search response, so exposing it costs no extra API request).
    pub avatar_url: Option<String>,
}

/// GitHub repository metadata for an installed, GitHub-sourced plugin,
/// cached from `GET /repos/{full_name}` (description + owner avatar).
#[derive(Clone, Serialize, Default)]
pub struct RepoMeta {
    pub description: Option<String>,
    pub owner_avatar: Option<String>,
}

/// The manifest of the web profile: installed plugin dependencies, the active
/// bundle layer list, each dependency's installed version, the subset of
/// GitHub-sourced plugins that have a newer remote release/tag, and per-plugin
/// GitHub metadata (description / owner avatar) for the installed list.
///
/// `repos` is additive: npm-sourced plugins (no recorded GitHub source) and
/// plugins whose metadata fetch failed (rate limit / offline) simply have no
/// entry — the frontend renders it as an optional decoration.
#[derive(Serialize)]
pub struct ProfileManifest {
    pub dependencies: Vec<String>,
    pub bundles: Vec<String>,
    pub versions: HashMap<String, String>,
    pub updates: Vec<String>,
    pub repos: HashMap<String, RepoMeta>,
}

/// One plugin command's result (stdout + stderr + exit status).
#[derive(Serialize)]
pub struct PluginCommandResult {
    pub ok: bool,
    pub output: String,
}

/// One chunk of streamed plugin-command output (`plugin-progress` event).
#[derive(Clone, Serialize)]
pub struct PluginProgress {
    /// "install" | "uninstall"
    pub op: String,
    /// One output line when streaming; `None` on the terminal event.
    pub line: Option<String>,
    /// Terminal event marker.
    pub done: bool,
    /// Terminal exit status.
    pub ok: Option<bool>,
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

/// Record of which installed package name came from which GitHub repo
/// (`$DSH_HOME/desktop-plugin-sources.json`), used for update detection.
fn sources_path() -> PathBuf {
    dsh_home().join("desktop-plugin-sources.json")
}

fn load_sources() -> HashMap<String, String> {
    std::fs::read_to_string(sources_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_sources(sources: &HashMap<String, String>) {
    if let Some(parent) = sources_path().parent() {
        let _ = create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(sources) {
        let _ = write(sources_path(), text);
    }
}

fn record_source(package: &str, full_name: &str) {
    let mut sources = load_sources();
    sources.insert(package.to_string(), full_name.to_string());
    save_sources(&sources);
}

fn remove_source(package: &str) {
    let mut sources = load_sources();
    sources.remove(package);
    save_sources(&sources);
}

/// Names currently declared in the web profile's package.json dependencies.
fn installed_dep_names() -> Result<HashSet<String>, String> {
    let manifest_path = profile_dir().join("package.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|err| format!("cannot read {}: {err}", manifest_path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("cannot parse {}: {err}", manifest_path.display()))?;
    Ok(value
        .get("dependencies")
        .and_then(|d| d.as_object())
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default())
}

/// The actually-installed version of a package, read from the profile's
/// `node_modules` (the manifest dependency entry is only a range).
fn installed_version(name: &str) -> Option<String> {
    let pkg = profile_dir().join("node_modules").join(name).join("package.json");
    let text = std::fs::read_to_string(&pkg).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("version").and_then(|v| v.as_str()).map(String::from)
}

/// Read the web profile manifest (dependencies + bundle layer list + versions
/// + GitHub update flags).
#[tauri::command]
pub fn list_plugins(_app: AppHandle) -> Result<ProfileManifest, String> {
    let names: Vec<String> = {
        let mut names: Vec<String> = installed_dep_names()?.into_iter().collect();
        names.sort();
        names
    };
    let manifest_path = profile_dir().join("package.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|err| format!("cannot read {}: {err}", manifest_path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("cannot parse {}: {err}", manifest_path.display()))?;
    let bundles = value
        .pointer("/dsh/profile/bundles")
        .and_then(|b| b.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let mut versions = HashMap::new();
    for name in &names {
        if let Some(version) = installed_version(name) {
            versions.insert(name.clone(), version);
        }
    }
    let sources = load_sources();
    let mut updates: Vec<String> = sources
        .iter()
        .filter(|(name, _)| names.iter().any(|n| n == *name))
        .filter(|(name, full_name)| {
            let local = versions.get(*name);
            remote_latest_version(full_name)
                .is_some_and(|remote| local.map_or(true, |local| is_newer(&remote, local)))
        })
        .map(|(name, _)| name.clone())
        .collect();
    updates.sort();
    // GitHub repo metadata (description / owner avatar) for installed plugins
    // whose install source was recorded. Fetches are cached; a failed fetch
    // (rate limit / offline) just leaves that plugin without decoration.
    let mut repos = HashMap::new();
    for (name, full_name) in &sources {
        if names.iter().any(|n| n == name) {
            if let Some(meta) = repo_meta(full_name) {
                repos.insert(name.clone(), meta);
            }
        }
    }
    Ok(ProfileManifest {
        dependencies: names,
        bundles,
        versions,
        updates,
        repos,
    })
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
            avatar_url: item
                .get("owner")
                .and_then(|o| o.get("avatar_url"))
                .and_then(|a| a.as_str())
                .map(String::from),
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
/// is refused with an explicit error. Output streams live to the frontend via
/// the `plugin-progress` event.
#[tauri::command]
pub fn install_plugin(app: AppHandle, spec: String) -> Result<PluginCommandResult, String> {
    let spec = spec.trim();
    if is_github_repo_spec(spec) {
        verify_github_plugin_manifest(spec)?;
    }
    let before = installed_dep_names()?;
    let result = run_plugin_command_streaming(&app, &["add", spec], "install")?;
    if result.ok {
        // If the install came from a GitHub repo, remember the package → repo
        // mapping so update detection can resolve the remote repo later.
        if is_github_repo_spec(spec) {
            if let Ok(after) = installed_dep_names() {
                for name in after.difference(&before) {
                    record_source(name, spec);
                }
            }
        }
        append_audit("install", spec, "success");
        // Bundle layers compose at boot: restart the server so the plugin is
        // active immediately instead of waiting for a manual restart.
        crate::restart_server(&app);
    } else {
        append_audit("install", spec, "failure");
    }
    Ok(result)
}

/// Remove a plugin from the web profile: `dsh plugin --profile web remove <name>`.
///
/// On failure the error includes a recovery hint; a success that leaves the
/// package in the manifest is surfaced as well.
#[tauri::command]
pub fn uninstall_plugin(app: AppHandle, name: String) -> Result<PluginCommandResult, String> {
    let result = run_plugin_command_streaming(&app, &["remove", &name], "uninstall")?;
    if result.ok {
        remove_source(&name);
        append_audit("uninstall", &name, "success");
        let still_installed = installed_dep_names().map(|names| names.contains(&name)).unwrap_or(false);
        if still_installed {
            return Err(format!(
                "卸载 {name} 命令已成功，但依赖清单中仍存在该插件。\n恢复建议：重启 Web 服务后重试卸载，或手动执行 `dsh plugin --profile web remove {name}`。"
            ));
        }
        // Bundle layers compose at boot: restart so the removal takes effect
        // immediately instead of waiting for a manual restart.
        crate::restart_server(&app);
        Ok(result)
    } else {
        append_audit("uninstall", &name, "failure");
        Err(format!(
            "卸载 {name} 失败：{}\n恢复建议：该插件可能仍在依赖清单中——请重试卸载，或重启 Web 服务后重试；若已部分移除，可在插件页重新安装该插件。",
            result.output.trim()
        ))
    }
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

/// The latest published release/tag of a GitHub repo, cached per repo for
/// `UPDATE_CACHE_TTL`. `releases/latest` first, falling back to the newest tag.
fn remote_latest_version(full_name: &str) -> Option<String> {
    let cache = UPDATE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        if let Ok(guard) = cache.lock() {
            if let Some((at, cached)) = guard.get(full_name) {
                if at.elapsed() < UPDATE_CACHE_TTL {
                    return cached.clone();
                }
            }
        }
    }
    let remote = fetch_latest_tag(full_name);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(full_name.to_string(), (Instant::now(), remote.clone()));
    }
    remote
}

fn fetch_latest_tag(full_name: &str) -> Option<String> {
    let releases = format!("https://api.github.com/repos/{full_name}/releases/latest");
    if let Ok(body) = http_get(&releases) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(tag) = json.get("tag_name").and_then(|t| t.as_str()) {
                return Some(tag.to_string());
            }
        }
    }
    let tags = format!("https://api.github.com/repos/{full_name}/tags?per_page=1");
    if let Ok(body) = http_get(&tags) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(first) = json.as_array().and_then(|arr| arr.first()) {
                if let Some(tag) = first.get("name").and_then(|t| t.as_str()) {
                    return Some(tag.to_string());
                }
            }
        }
    }
    None
}

/// Repository metadata (description + owner avatar) for one GitHub repo,
/// cached per repo for `UPDATE_CACHE_TTL`. `None` on a failed fetch (rate
/// limit, offline, invalid JSON) — callers treat it as "no decoration".
fn repo_meta(full_name: &str) -> Option<RepoMeta> {
    let cache = REPO_META_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    {
        if let Ok(guard) = cache.lock() {
            if let Some((at, cached)) = guard.get(full_name) {
                if at.elapsed() < UPDATE_CACHE_TTL {
                    return cached.clone();
                }
            }
        }
    }
    let meta = fetch_repo_meta(full_name);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(full_name.to_string(), (Instant::now(), meta.clone()));
    }
    meta
}

/// `GET /repos/{full_name}` — the same endpoint `resolve_default_branch`
/// probes, so it doubles as a cheap existence check; only the fields the
/// installed list needs are kept.
fn fetch_repo_meta(full_name: &str) -> Option<RepoMeta> {
    let url = format!("https://api.github.com/repos/{full_name}");
    let body = http_get(&url).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    Some(RepoMeta {
        description: json.get("description").and_then(|d| d.as_str()).map(String::from),
        owner_avatar: json
            .get("owner")
            .and_then(|o| o.get("avatar_url"))
            .and_then(|a| a.as_str())
            .map(String::from),
    })
}

/// `remote` is "newer" than `local` when its numeric version segments sort
/// higher (leading `v`/`V`, prerelease/build suffixes ignored).
fn is_newer(remote: &str, local: &str) -> bool {
    parse_version(remote) > parse_version(local)
}

fn parse_version(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches(['v', 'V'])
        .split(['.', '-', '+'])
        .filter_map(|part| part.parse::<u64>().ok())
        .collect()
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

/// Run `dsh plugin --profile web <args>` with the bundled pnpm on PATH,
/// streaming stdout/stderr lines to the frontend via `plugin-progress`.
fn run_plugin_command_streaming(app: &AppHandle, args: &[&str], op: &str) -> Result<PluginCommandResult, String> {
    let (node_path, entry_path) = crate::server::runtime_binaries(app)?;
    let pnpm_shim_dir = pnpm_shim_dir(app)?;
    // The web profile directory must exist before spawn: `current_dir()` fails
    // with a cryptic "No such file or directory" on a machine where the
    // profile was never booted yet. Creating it here lets the very first
    // plugin operation proceed (dsh's own profile init then fills in the
    // manifest).
    create_dir_all(profile_dir())
        .map_err(|err| format!("cannot create {}: {err}", profile_dir().display()))?;
    // dsh plugin spawns `pnpm` from PATH; prepend the shim directory.
    let mut path = pnpm_shim_dir.to_string_lossy().into_owned();
    if let Some(existing) = std::env::var_os("PATH") {
        path.push(':');
        path.push_str(&existing.to_string_lossy());
    }
    let mut child = Command::new(&node_path)
        .arg(&entry_path)
        .args(["plugin", "--profile", WEB_PROFILE])
        .args(args)
        .current_dir(profile_dir())
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to run dsh plugin: {err}"))?;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let output = std::sync::Arc::new(Mutex::new(String::new()));
    let stdout_thread = {
        let app = app.clone();
        let op = op.to_string();
        let sink = output.clone();
        std::thread::spawn(move || drain_lines(stdout, &app, &op, &sink))
    };
    let stderr_thread = {
        let app = app.clone();
        let op = op.to_string();
        let sink = output.clone();
        std::thread::spawn(move || drain_lines(stderr, &app, &op, &sink))
    };

    let status = child.wait().map_err(|err| format!("failed to wait dsh plugin: {err}"))?;
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();

    let text = output.lock().map(|guard| guard.clone()).unwrap_or_default();
    let _ = app.emit(
        "plugin-progress",
        PluginProgress {
            op: op.to_string(),
            line: None,
            done: true,
            ok: Some(status.success()),
        },
    );
    Ok(PluginCommandResult {
        ok: status.success(),
        output: text,
    })
}

/// Read a piped stream line-by-line, appending to the shared sink and emitting
/// each line as a `plugin-progress` event.
fn drain_lines<R: std::io::Read>(reader: R, app: &AppHandle, op: &str, sink: &Mutex<String>) {
    use std::io::BufRead;
    let reader = std::io::BufReader::new(reader);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if let Ok(mut guard) = sink.lock() {
            guard.push_str(&line);
            guard.push('\n');
        }
        let _ = app.emit(
            "plugin-progress",
            PluginProgress {
                op: op.to_string(),
                line: Some(line),
                done: false,
                ok: None,
            },
        );
    }
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
