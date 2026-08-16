# Changelog

All notable changes to **DeepSeek Harness Developer Preview** are documented
here. The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to
[Semantic Versioning](https://semver.org/) — every release carries a `-beta`
prerelease suffix during development.

> ⏰ 本文档中的日期与时间均为 **UTC+8（北京时间）**。

## [0.1.0-beta.8] — 2026-08-16

### Changed

- **Runtime slim-down, round 2**: the bundled runtime archive drops to
  ~66 MiB (from ~67.3 MiB) — OTel `build/esm`+`build/esnext` dual builds,
  shiki `onig.wasm` (the JS regex engine is used), npm's
  `.package-lock.json`, and gzip level 9 removed/raised. Smoke passes;
  boot module trace unchanged (1117 files).
- `verify-update` now also checks that the manifest signature's embedded
  key id matches the configured updater pubkey (minisign has no built-in
  verify subcommand; full content verification remains manual).

### Added

- `docs/platform-preflight.md` — Windows/Linux support preflight research:
  per-file platform audit, 5 blocking items (pnpm.cmd shim, USERPROFILE
  fallback, Linux tray, AppImage-only self-update, notification-click
  activation), CI matrix plan (darwin/windows/linux), suggested order.
- Notes for the next size jump: switching the runtime archive to
  `xz -9e` would save ~38% more (41 MiB) but requires the shell's
  extraction command to use xz (`tar -xJf`; macOS/Linux/Windows bsdtar
  all support it) — deliberately left for the next round.

## [0.1.0-beta.7] — 2026-08-16

### Added

- **Plugin Manager, round 2**:
  - Streaming install/remove output: `dsh plugin` stdout/stderr is pushed
    line-by-line via a Tauri event and shown live in the plugin page (no more
    waiting on a spinner).
  - Installed plugin versions are read from the profile and shown; for
    GitHub-sourced plugins an update badge appears when a newer release/tag
    exists (10-minute cache to respect the GitHub rate limit).
  - Uninstall failure now surfaces explicit recovery hints; failures are
    recorded in the audit log too.
- **Cross-workspace notification jump**: clicking a completion notification
  probes the target session's workspace (via the loopback `workspace.list`
  API), logs it, and navigates even when the session belongs to another
  workspace (the current frontend aggregates all workspaces, so the jump
  works); if the probe fails it focuses the window instead.
- Docs: `docs/platform-preflight.md` — Windows/Linux support preflight
  research (per-file audit, 5 blocking items, CI matrix plan).

## [0.1.0-beta.6] — 2026-08-16

### Changed

- **Release builds now use the pruned runtime** (`DSH_DESKTOP_PRUNE=1` in the
  release workflow): the bundled runtime archive drops from ~97 MiB to
  ~63 MiB (−35%) — node binary stripped (re-signed), node-pty cross-platform
  payloads, source maps, `.d.ts`/`@types`, tests/docs, and the otel
  `sdk-trace` removed; smoke still passes.
- CI caches the Rust build (`Swatinem/rust-cache`) so repeated
  `cargo check --release` no longer recompiles the whole dependency tree.
- READMEs: size figures updated to the pruned runtime; `verify:update`
  documented.

### Added

- `scripts/verify-update.mjs` (`pnpm run verify:update`): fetches the
  updater-latest manifest, asserts the version (optionally
  `--expect-version <v>`) and that the darwin-aarch64 archive is reachable.
  Wired into the release workflow as a post-publish verification step, so
  every release is end-to-end checked for the auto-update chain.

## [0.1.0-beta.5] — 2026-08-16

### Added

- **Plugin Manager hardening**:
  - Pre-install manifest verification: GitHub-repo specs are checked against
    the raw `package.json` for the `dsh.bundle` / `dsh.client` contract
    before installing — bare repositories are rejected with a clear error.
  - Install confirmation dialog in the Plugin Manager (repo name, stars,
    description, third-party-execution warning).
  - Install/remove audit log at `$DSH_HOME/desktop-audit.log` (UTC+8, action,
    plugin, result).
- READMEs: Plugin Manager documented; roadmap item marked as implemented (MVP).

## [0.1.0-beta.4] — 2026-08-16

### Added

- **Plugin Manager** (menu "Plugin Manager…"): a native window to manage
  dsh plugins — lists plugins installed in the web profile (with remove
  buttons), searches GitHub for `topic:dsh-plugin` repositories (cached),
  and installs by `dsh plugin --profile web add <spec>`. Installs/removes
  surface the "restart web server to take effect" hint.
- `withGlobalTauri` enabled so the local plugin-manager page can invoke
  commands; the remote harness GUI remains IPC-less (capabilities still
  scope to the main/plugins windows).

## [0.1.0-beta.3] — 2026-08-16

### Added

- **Notification click jumps to the finished session** (macOS): clicking a
  session-completion notification (or the dock icon with an unviewed
  completion) navigates the GUI to that conversation via `?jump=<sessionId>`
  and the frontend's own persisted current-session key.
- **Restart Web Server** menu/tray action — restarts the bundled dsh web
  server, required for plugin installs/removals (bundle layers compose at
  boot) to take effect.
- **Bundled pnpm** (runtime, `pnpm/bin/pnpm.cjs` on the bundled Node) —
  foundation for `dsh plugin` support; the runtime archive grows ~4.5 MiB.

## [0.1.0-beta.2] — 2026-08-16

### Changed

- Release assets use the short **DSH-DP** convention:
  `DSH-DP_{version}_{os}_{arch}.{ext}` (e.g. `DSH-DP_v0.1.0-beta.2_macOS_aarch64.dmg`),
  so the file extension stays visible in the release list; release notes now
  end with per-platform download links.
- GitHub Actions upgraded (checkout@v7, setup-node@v7, upload-artifact@v7,
  download-artifact@v8, pnpm/action-setup@v6), removing the Node.js 20
  deprecation warnings.
- CI trigger policy: the Linux runtime artifact is not consumed downstream,
  so pushes to main run the full CI only when code/runtime paths change;
  PRs and manual dispatch still always run it — saving runner minutes during
  the frequent internal commit loop.
- Documentation timestamps are now consistently **UTC+8 (Beijing time)**.

### Fixed

- App icon: the β developer-preview badge is centered inside its badge
  background (previously shifted left).
- Tray icon replaced with the hand-tuned reference from the POC
  (no longer cropped); gen-icons.sh preserves it when present.

## [Unreleased]

## [0.1.0-beta.1] — 2026-08-16

### Fixed

- Updater manifest URL: the release workflow now uploads the updater
  archive under a hyphenated asset name. GitHub rewrites spaces in
  uploaded asset names to dots, which made the `%20`-encoded URL in
  `latest.json` return 404 and silently broke auto-update.

### Changed

- Release workflow builds Apple Silicon only (`macos-latest`) to keep
  the CI/CD loop fast; the updater manifest is `darwin-aarch64` only.


### Added

- `check-upstream` workflow: scheduled daily check of the npm registry for a
  newer `@deepseek-ai/dsh`; opens/updates a dependency-bump PR when one exists.
- CI workflow: frontend build, runtime assembly, runtime smoke test, and
  `cargo check` on every push/PR.
- Release workflow: builds and signs the macOS app (Apple Silicon), publishes a
  `vX.Y.Z-beta.N` pre-release with the signed `latest.json` updater manifest.

## [0.1.0-beta.0] — 2026-08-16

### Added

- Native macOS desktop shell (Tauri 2) for DeepSeek Harness:
  - All-in-one architecture: bundles its own Node.js LTS runtime (v24.19.0)
    and a production install of `@deepseek-ai/dsh` (`dsh web`) as a single
    compressed archive (~97 MiB), extracted to the app cache on first launch.
  - Bundled-server lifecycle: spawns `dsh --profile web --port 0` with the
    bundled Node, waits for the readiness URL line, polls the served root, and
    navigates the main window; server output is mirrored to the platform
    app-log directory; failure falls back to a dedicated error page.
  - Navigation fence: the window only ever visits the local service origin;
    external links open in the system browser.
  - System tray: close-to-tray keeps the server running; tray menu restores
    the window or quits; left-click shows the window (macOS dock reopen too).
  - Auto-update: silent check 3s after startup plus a "Check for Updates…"
    menu item; signed manifests from GitHub Releases (`latest.json`);
    `DSH_DESKTOP_AUTO_UPDATE=0` disables the startup check.
  - Session-completion notifications: a background poller watches
    `~/.dsh/sessions/**/session.jsonl.zstd` and posts a native notification on
    a fresh `turn/end` (reason `completed` or `error`); clicking shows/focuses
    the window. Tuning: `DSH_DESKTOP_NOTIFY=0`,
    `DSH_DESKTOP_NOTIFY_INTERVAL_MS`.
  - Single-instance guard; native app menu (About with build provenance, Edit
    roles for copy/paste, Window roles); custom app icon and tray icon.
- Runtime assembly tooling:
  - `scripts/download-node.mjs` — pinned Node.js LTS binary (v24.19.0,
    `DSH_DESKTOP_NODE_VERSION` override), idempotent.
  - `scripts/assemble-runtime.mjs` — clean production install of
    `@deepseek-ai/dsh` via npm (real-directory layout, self-contained by
    construction), manifest with structural hash, single `runtime.tar.gz`.
  - `scripts/smoke.mjs` — boots the assembled runtime exactly like the shell
    and verifies the served root responds.
  - `scripts/check-upstream.mjs` — npm registry comparison for the scheduled
    upstream check.
  - `scripts/gen-icons.sh`, `scripts/gen-signer-key.sh` — icon and update
    signing-key tooling.
- Documentation: bilingual README (`README.md` / `README-zh.md`) and this
  changelog (`CHANGELOG.md` / `CHANGELOG-zh.md`), MIT license.

[unreleased]: https://github.com/Lsnsh/deepseek-harness-desktop/compare/v0.1.0-beta.8...HEAD
[0.1.0-beta.8]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.8
[0.1.0-beta.7]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.7
[0.1.0-beta.6]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.6
[0.1.0-beta.5]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.5
[0.1.0-beta.4]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.4
[0.1.0-beta.3]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.3
[0.1.0-beta.2]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.2
[0.1.0-beta.1]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.1
[0.1.0-beta.0]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.0
