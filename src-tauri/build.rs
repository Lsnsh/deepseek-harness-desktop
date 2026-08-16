/// Capture the git commit and its date so the About dialog can show build
/// provenance without a server round-trip. Outside a git checkout (source
/// tarball builds) the env vars stay unset and `option_env!` reports
/// "unknown".
fn main() {
    tauri_build::build();
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    if let Some(commit) = git(&["rev-parse", "--short=12", "HEAD"]) {
        println!("cargo:rustc-env=DSH_GIT_COMMIT={commit}");
    }
    if let Some(date) = git(&["log", "-1", "--format=%cd", "--date=format:%Y-%m-%d %H:%M:%S %z"]) {
        println!("cargo:rustc-env=DSH_GIT_COMMIT_DATE={date}");
    }
}
