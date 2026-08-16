<div align="center">

# 🐳 DeepSeek Harness Developer Preview

**All-in-one native desktop client · bundles Node.js runtime + deepseek-harness**

[![release](https://img.shields.io/github/v/release/Lsnsh/deepseek-harness-desktop?include_prereleases&label=version&style=flat-square)](https://github.com/Lsnsh/deepseek-harness-desktop/releases)
[![ci](https://img.shields.io/github/actions/workflow/status/Lsnsh/deepseek-harness-desktop/ci.yml?branch=main&label=CI&style=flat-square)](https://github.com/Lsnsh/deepseek-harness-desktop/actions/workflows/ci.yml)
[![check-upstream](https://img.shields.io/github/actions/workflow/status/Lsnsh/deepseek-harness-desktop/check-upstream.yml?branch=main&label=upstream%20check&style=flat-square)](https://github.com/Lsnsh/deepseek-harness-desktop/actions/workflows/check-upstream.yml)
[![license](https://img.shields.io/github/license/Lsnsh/deepseek-harness-desktop?style=flat-square)](LICENSE)

</div>

## What is this

**DeepSeek Harness Developer Preview** is a **native desktop client** (all-in-one architecture) for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness):

- 📦 **No Node.js install needed** — ships its own Node.js LTS runtime (v24.19.0)
- 🚀 **Ready to run** — bundles the latest `@deepseek-ai/dsh`; starts `dsh web` on launch
- 🖥️ **Native feel** — Tauri 2 native window, system tray, native menus
- 🔔 **Session notifications** — native notification when a session turn completes; click to return to the window
- 🔄 **Auto-update** — distributed via GitHub Releases; silent startup check plus a manual menu item

> ⚠️ This is a **community-maintained third-party desktop client**, not affiliated with DeepSeek. For development and research use only.
> Currently in **beta (Developer Preview)** and supports **macOS (Apple Silicon) only**.

## Features

| Capability | Details |
| --- | --- |
| Bundled runtime | Node.js LTS v24.19.0 + full `@deepseek-ai/dsh` production closure (~63 MiB compressed archive with size pruning; see below) |
| Local-first | Sessions and settings live on your machine (default `~/.dsh`); the service binds `127.0.0.1` |
| Single instance | Launching again focuses the existing window instead of starting a second server |
| Tray resident | Closing the window hides to the tray; the background service keeps running |
| Auto-update | Silent check 3s after startup; "Check for Updates…" menu item; signed update manifests |
| Session notifications | Polls the session store; notifies on `turn/end` with reason `completed`/`error` |
| Plugin manager | Manage `dsh-plugin` topic plugins from GitHub: list/search/install/remove; pre-install manifest verification (`dsh.bundle`/`dsh.client`), install confirmation, and an audit log at `~/.dsh/desktop-audit.log` |
| External links | GUI links open in the system browser; the window only ever visits the local service |
| Error fallback | A dedicated error page when the bundled service fails to start; logs under the system log dir |

## Download & install

Grab the DMG (or .app) from the [Releases](https://github.com/Lsnsh/deepseek-harness-desktop/releases) page (Apple Silicon builds).

> macOS builds are unsigned/un-notarized: on first launch allow the app in **System Settings → Privacy & Security**, or right-click → Open.

## Development

```bash
pnpm install                        # install dependencies
pnpm run runtime                      # assemble the bundled runtime (downloads Node v24.19.0 + prod-installs @deepseek-ai/dsh)
DSH_DESKTOP_PRUNE=1 pnpm run runtime  # same, but prune dead weight first (node strip, .map/.d.ts, node-pty cross-platform prebuilds, READMEs/tests): ~97 MiB -> ~63 MiB
pnpm run smoke                      # boot the assembled runtime exactly like the shell and GET the root
pnpm run tauri:dev                  # development mode
pnpm run tauri:build                # release build (.app + .dmg)
pnpm run check:upstream             # check for a newer @deepseek-ai/dsh on npm
pnpm run verify:update              # verify the updater-latest manifest + update URL after a release
```

### Layout

```
├── scripts/                  # runtime assembly & tooling
│   ├── download-node.mjs     # download + verify the bundled Node binary
│   ├── assemble-runtime.mjs  # prod-install @deepseek-ai/dsh → resources/runtime
│   ├── smoke.mjs             # boot + probe the assembled runtime
│   ├── check-upstream.mjs    # npm upstream check (used by the scheduled workflow)
│   ├── verify-update.mjs     # post-release check: updater-latest manifest + update URL
│   ├── gen-icons.sh          # app / tray icons
│   └── gen-signer-key.sh     # updater signing keypair
├── resources/                # build artifacts (git-ignored): runtime/ + runtime.tar.gz
├── src-tauri/                # Tauri 2 shell (Rust)
│   └── src/
│       ├── lib.rs            # entry, menus, window lifecycle
│       ├── server.rs         # spawn/supervise dsh web, wait for readiness, navigate
│       ├── tray.rs           # system tray
│       ├── updater.rs        # auto-update (silent startup + manual menu)
│       └── notify.rs         # session-completion notifications (store polling)
├── public/error.html         # startup-failure fallback page
├── index.html                # splash page
└── .github/workflows/        # CI / upstream check / release
```

### How it works

1. `scripts/assemble-runtime.mjs` prod-installs `@deepseek-ai/dsh` with npm in a temp dir (real directories, no symlinks), then bundles it with the downloaded Node binary as `resources/runtime.tar.gz`. Release builds set `DSH_DESKTOP_PRUNE=1` to prune dead weight first (strip the node binary + re-sign, drop `.map`/`.d.ts`, node-pty cross-platform prebuilds, READMEs/tests): the archive goes from ~97 MiB to ~63 MiB.
2. Release builds ship the archive as a Tauri resource.
3. On first launch the app extracts the archive into a cache dir keyed by a content hash (stale versions are cleaned up automatically).
4. Rust spawns `dsh --profile web --port 0` with the bundled Node (OS-assigned port), reads the readiness line `dsh web: http://127.0.0.1:<port>` from stdout, polls the root, then navigates the window.
5. Navigation is confined to the local service origin; external links leave via the system browser.
6. A background thread polls `~/.dsh/sessions/**/session.jsonl.zstd` every 2s and posts a notification on a fresh `turn/end` (reason `completed`/`error`).

### Update signing

Auto-update needs a minisign keypair:

```bash
pnpm run signer-key
```

- The private key lives at `~/.tauri/dsh.key` — **never commit it**;
- Paste the public key into `src-tauri/tauri.conf.json` → `plugins.updater.pubkey`;
- CI signs update packages with the `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets.

### Release flow

1. Bump the version in `package.json` and `src-tauri/Cargo.toml` (must carry a `-beta` suffix) and update `CHANGELOG.md` / `CHANGELOG-zh.md`; commit and push.
2. Trigger the `release` workflow manually (optionally overriding the version).
3. The workflow builds on macOS (Apple Silicon, `macos-latest`), signs, and publishes a pre-release `vX.Y.Z-beta.N` with the `latest.json` update manifest attached.
4. Clients pick the manifest up at startup and via "Check for Updates…".
5. After the release, verify the update pointer end-to-end: `pnpm run verify:update` (fetches `updater-latest/latest.json`, validates the `darwin-aarch64` entry, and probes the update package URL for HTTP 200/302).

### Upstream sync

`.github/workflows/check-upstream.yml` checks npm daily for a newer `@deepseek-ai/dsh`:

- New version → opens/updates a dependency-bump PR; merge after CI passes;
- Or run manually: `gh workflow run check-upstream.yml`.

## Roadmap

- [x] macOS: development / CI / deployment / auto-update full loop
- [ ] Notification click navigates to the finished **session**
- [ ] Windows / Linux support
- [x] dsh plugin management (MVP: list/search/install/remove from the GitHub `dsh-plugin` topic, with pre-install manifest verification + audit log)
- [ ] Smaller bundle (prune the runtime closure)

## Credits

- [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) — the engine and the served web app
- [Tauri](https://tauri.app) — the native shell framework

## License

[MIT](LICENSE)
