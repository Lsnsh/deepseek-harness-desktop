#!/usr/bin/env bash
# Build the app locally (with the pruned runtime) and install it into
# /Applications for X6 to run and confirm. The installable keeps its standard
# name so the dock/spotlight entry stays stable; a versioned copy of the
# bundle is also kept under artifacts/ for reference.
#
# Usage: bash scripts/build-local.sh [--no-install] [--no-prune]
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(node -p "require('./package.json').version")"
echo "==> [build-local] 本地构建 v$VERSION (UTC+8 $(date '+%Y-%m-%d %H:%M'))"

PRUNE=1
if [[ "${1:-}" == "--no-prune" || "${2:-}" == "--no-prune" ]]; then PRUNE=0; fi
INSTALL=1
if [[ "${1:-}" == "--no-install" || "${2:-}" == "--no-install" ]]; then INSTALL=0; fi

echo "==> [build-local] 装配运行时（prune=${PRUNE}）"
if [[ "$PRUNE" == "1" ]]; then
  DSH_DESKTOP_PRUNE=1 pnpm run runtime
else
  pnpm run runtime
fi

echo "==> [build-local] 前端构建"
pnpm build

echo "==> [build-local] tauri build (app, dmg)"
# Sign the updater artifact with the local key (createUpdaterArtifacts
# requires a private key even for a local app-only build).
export TAURI_SIGNING_PRIVATE_KEY="${TAURI_SIGNING_PRIVATE_KEY:-$(cat "$HOME/.tauri/dsh.key" 2>/dev/null || true)}"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
if [[ -z "$TAURI_SIGNING_PRIVATE_KEY" ]]; then
  echo "==> [build-local] 未找到本地签名密钥 (~/.tauri/dsh.key)；跳过签名（更新功能不可用）"
  export TAURI_SIGNING_PRIVATE_KEY=""
fi
# Developer ID code signing is configured in tauri.conf.json
# (bundle.macOS.signingIdentity), so the app is signed automatically.
pnpm exec tauri build --bundles app,dmg

APP="src-tauri/target/release/bundle/macos/DeepSeek Harness Developer Preview.app"
if [[ ! -d "$APP" ]]; then
  echo "==> [build-local] 构建产物缺失: $APP"; exit 1
fi

# Notarize the app with the credentials already stored in the Keychain
# (xcrun notarytool --keychain-profile "deepseek-harness-desktop-profile"),
# then staple the ticket into the bundle so Gatekeeper trusts it. No
# plaintext credentials appear in this script.
NOTARY_PROFILE="${DSH_DESKTOP_NOTARY_PROFILE:-deepseek-harness-desktop-profile}"
if [[ "$INSTALL" == "1" ]] && command -v xcrun >/dev/null 2>&1; then
  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  echo "==> [build-local] 公证（notarytool, profile=${NOTARY_PROFILE}）"
  ditto -c -k --sequesterRsrc --keepParent "$APP" "$WORK/app.zip"
  if xcrun notarytool submit "$WORK/app.zip" --keychain-profile "$NOTARY_PROFILE" --wait \
    | tee "$WORK/notary.log"; then
    xcrun stapler staple "$APP" || echo "==> [build-local] staple 警告（app 仍可运行，但可能提示未验证）"
    echo "==> [build-local] 公证完成"
  else
    echo "==> [build-local] 公证失败（跳过 staple，继续安装）"
    tail -5 "$WORK/notary.log" 2>/dev/null || true
  fi
fi

# Versioned copy for reference.
mkdir -p artifacts
VERSIONED="artifacts/DSH-DP_v${VERSION}_macOS_aarch64.app"
rm -rf "$VERSIONED"
cp -R "$APP" "$VERSIONED"
echo "==> [build-local] 版本化副本: $VERSIONED"

if [[ "$INSTALL" == "1" ]]; then
  echo "==> [build-local] 安装到 /Applications（先退出运行中的实例）"
  osascript -e 'quit app "DeepSeek Harness Developer Preview"' >/dev/null 2>&1 || true
  sleep 1
  rm -rf "/Applications/DeepSeek Harness Developer Preview.app"
  cp -R "$APP" "/Applications/DeepSeek Harness Developer Preview.app"
  echo "==> [build-local] 已安装: /Applications/DeepSeek Harness Developer Preview.app"
fi

echo "==> [build-local] 完成 v$VERSION"
