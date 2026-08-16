#!/usr/bin/env bash
# Generate the Tauri updater signing key pair and print the CI configuration
# steps. The private key never leaves your machine: the values printed here
# are the ones to put into the repository's GitHub Actions secrets.
#
# Usage: bash scripts/gen-signer-key.sh [--force]
#
# Outputs:
#   ~/.tauri/dsh.key       the private key (keep secret; never commit)
#   ~/.tauri/dsh.key.pub   the public key (paste into tauri.conf.json)
set -euo pipefail
cd "$(dirname "$0")/.."

KEY="$HOME/.tauri/dsh.key"
force=""
if [[ "${1:-}" == "--force" ]]; then force="--force"; fi

if [[ -f "$KEY" && -z "$force" ]]; then
  echo "gen-signer-key: key pair already exists at $KEY (use --force to regenerate)." >&2
  exit 1
fi

echo "gen-signer-key: pick a password for the private key (used by CI to sign bundles)."
read -rsp "password: " password
echo

args=(-w "$KEY" --ci)
if [[ -n "$password" ]]; then args+=(--password "$password"); fi
if [[ -n "$force" ]]; then args+=(--force); fi
pnpm tauri signer generate "${args[@]}"

echo
echo "==== public key: paste into src-tauri/tauri.conf.json (plugins.updater.pubkey) ===="
cat "$KEY.pub"
echo
echo "==== configure these repository Actions secrets ===="
echo "TAURI_SIGNING_PRIVATE_KEY        = contents of $KEY"
echo "TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $password"
echo
echo "gen-signer-key: rotate by regenerating with --force, updating tauri.conf.json,"
echo "and replacing both secrets; old versions stay installable but stop updating."
