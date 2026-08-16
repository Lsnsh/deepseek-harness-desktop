# Changelog

All notable changes to **DeepSeek Harness Developer Preview** are documented
here. The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to
[Semantic Versioning](https://semver.org/) — every release carries a `-beta`
prerelease suffix during development.

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

[unreleased]: https://github.com/Lsnsh/deepseek-harness-desktop/compare/v0.1.0-beta.1...HEAD
[0.1.0-beta.1]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.1
[0.1.0-beta.0]: https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/v0.1.0-beta.0
