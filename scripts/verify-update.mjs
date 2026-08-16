#!/usr/bin/env node
/**
 * Verify the auto-update pointer end-to-end after a release.
 *
 * Every publish overwrites the `latest.json` manifest asset on the fixed
 * `updater-latest` tag (see .github/workflows/release.yml). This script:
 *
 *   1. fetches that manifest,
 *   2. validates its shape (version + platforms.darwin-aarch64.url/signature),
 *   3. checks that the minisign key id embedded in the signature matches the
 *      updater public key configured in src-tauri/tauri.conf.json (the
 *      8-byte key id is a fingerprint of the signing key, so a manifest
 *      signed by any other key is rejected here),
 *   4. probes the update package URL and accepts HTTP 200/302,
 *   5. prints the version + URL so the result is greppable.
 *
 * Limitation: step 3 proves the signature belongs to the configured key, not
 * that the downloaded archive hashes to the signed bytes. Full content
 * verification needs a minisign tool against the downloaded package:
 *   minisign -V -p <public.key> -m <archive> -s <signature-file>
 * (not automated here — it requires a ~66 MiB download; `tauri signer` has
 * no `verify` subcommand).
 *
 * Usage:
 *   node scripts/verify-update.mjs                      # default repo URL
 *   node scripts/verify-update.mjs <latestJsonUrl>      # explicit manifest URL
 *   node scripts/verify-update.mjs --expect-version 0.1.0-beta.6   # assert the published version
 *   node scripts/verify-update.mjs --help
 *
 * The default repo is read from `git remote get-url origin` (github.com),
 * falling back to Lsnsh/deepseek-harness-desktop. Exit code 0 = OK,
 * 1 = manifest missing/invalid, signature key mismatch, or the update URL
 * unreachable (e.g. before the first release, when the updater-latest tag
 * does not exist yet).
 *
 * `--expect-version` is the post-release assertion the release workflow
 * passes: the manifest must name exactly the version that was just
 * published, so a release whose updater pointer was not overwritten (or was
 * overwritten by a newer one mid-flight) fails loudly instead of shipping a
 * client that updates to the wrong build.
 * @module deepseek-harness-desktop/verify-update
 */

import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const DESKTOP_ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)))
const MANIFEST_FILE = 'latest.json'
const TIMEOUT_MS = 30_000
const ACCEPTED_STATUS = new Set([200, 302]) // 302: GitHub asset redirect; 200: final CDN
/** The beta semver shape the release workflow enforces for published versions. */
const BETA_VERSION_RE = /^[0-9]+\.[0-9]+\.[0-9]+(-beta(\.[0-9]+)?)?$/

/** https://github.com/<owner>/<repo> from `git remote get-url origin`, else a default. */
function defaultRepo() {
  try {
    const url = execFileSync('git', ['config', '--get', 'remote.origin.url'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim()
    const match = url.match(/github\.com[:/]([^/]+\/[^/]+?)(?:\.git)?$/)
    if (match) return match[1]
  } catch {
    // not a git repo or no origin — fall through to the default
  }
  return 'Lsnsh/deepseek-harness-desktop'
}

/**
 * Parse argv: an optional leading positional is the explicit manifest URL
 * (legacy usage); flags are --expect-version / --help. Returns the URL and
 * the parsed options.
 */
export function parseArgs(argv) {
  const options = { url: undefined, expectVersion: undefined, help: false }
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    if (arg === '--help' || arg === '-h') {
      options.help = true
    } else if (arg === '--expect-version') {
      const value = argv[++i]
      if (value === undefined || value.startsWith('-')) throw new Error('--expect-version needs a value')
      options.expectVersion = value
    } else if (arg.startsWith('--expect-version=')) {
      options.expectVersion = arg.slice('--expect-version='.length)
    } else if (arg.startsWith('-') && arg !== '-') {
      throw new Error(`unknown option ${JSON.stringify(arg)}`)
    } else if (options.url === undefined) {
      options.url = arg
    } else {
      throw new Error(`unexpected extra argument ${JSON.stringify(arg)}`)
    }
  }
  if (options.expectVersion !== undefined && !BETA_VERSION_RE.test(options.expectVersion)) {
    throw new Error(`--expect-version must be a beta semver, got ${JSON.stringify(options.expectVersion)}`)
  }
  return options
}

function manifestUrl(options) {
  if (options.url) return options.url
  if (process.env.DSH_UPDATE_MANIFEST_URL) return process.env.DSH_UPDATE_MANIFEST_URL
  return `https://github.com/${defaultRepo()}/releases/download/updater-latest/${MANIFEST_FILE}`
}

async function fetchJson(url) {
  const res = await fetch(url, {
    redirect: 'follow',
    signal: AbortSignal.timeout(TIMEOUT_MS),
    headers: { 'user-agent': 'dsh-desktop-verify-update' },
  })
  if (!res.ok) throw new Error(`GET ${url} -> HTTP ${res.status}`)
  return res.json()
}

/** Probe the update package URL; accept 200 or 302 (GitHub asset redirects). */
async function checkUrl(url) {
  // HEAD first (cheap); fall back to a ranged GET when the CDN rejects HEAD.
  for (const method of ['HEAD', 'GET']) {
    try {
      const res = await fetch(url, {
        method,
        redirect: 'follow',
        signal: AbortSignal.timeout(TIMEOUT_MS),
        headers: { 'user-agent': 'dsh-desktop-verify-update', ...(method === 'GET' ? { range: 'bytes=0-0' } : {}) },
      })
      if (ACCEPTED_STATUS.has(res.status)) return res.status
      throw new Error(`${method} ${url} -> HTTP ${res.status}`)
    } catch (err) {
      if (method === 'HEAD' && /405|not allowed|unsupported/i.test(String(err.message))) continue // retry with GET
      throw err
    }
  }
  throw new Error(`HEAD and GET both failed for ${url}`)
}

/**
 * The updater public key configured in src-tauri/tauri.conf.json
 * (plugins.updater.pubkey): base64 of a minisign .pub file —
 * "untrusted comment: minisign public key: <hex>\n<base64 key line>".
 * The key line decodes to "Ed" (2) + keyId (8) + ed25519 key (32).
 * @returns {{ keyId: Buffer } | null}
 */
function configuredPubkey() {
  try {
    const conf = JSON.parse(readFileSync(join(DESKTOP_ROOT, 'src-tauri', 'tauri.conf.json'), 'utf8'))
    const pubkey = conf.plugins?.updater?.pubkey
    if (typeof pubkey !== 'string' || !pubkey) return null
    const text = Buffer.from(pubkey, 'base64').toString('utf8')
    const keyLine = text.split('\n').find((l) => l && !l.startsWith('untrusted'))
    if (!keyLine) return null
    return { keyId: Buffer.from(keyLine, 'base64').subarray(2, 10) }
  } catch {
    return null
  }
}

/**
 * Extract the 8-byte minisign key id from a manifest signature: base64 of a
 * minisign .sig file — "untrusted comment: …\n<base64 sig line>". The sig
 * line decodes to "ED" (2) + keyId (8) + signature (64).
 * @returns {Buffer}
 */
function signatureKeyId(signatureB64) {
  const text = Buffer.from(signatureB64, 'base64').toString('utf8')
  const sigLine = text.split('\n').find((l) => l && !l.startsWith('untrusted'))
  if (!sigLine) throw new Error('signature has no signature line')
  const sigRaw = Buffer.from(sigLine, 'base64')
  if (sigRaw.length < 74) throw new Error(`signature too short (${sigRaw.length} bytes)`)
  return sigRaw.subarray(2, 10)
}

async function main() {
  let options
  try {
    options = parseArgs(process.argv.slice(2))
  } catch (err) {
    console.error(`verify-update: ${err.message}`)
    console.error('usage: node scripts/verify-update.mjs [<latestJsonUrl>] [--expect-version <v>]')
    process.exit(2)
  }
  if (options.help) {
    console.log('verify-update: fetch updater-latest/latest.json, validate the darwin-aarch64 entry, probe the update URL')
    console.log('usage: node scripts/verify-update.mjs [<latestJsonUrl>] [--expect-version <v>]')
    console.log('  <latestJsonUrl>     explicit manifest URL (default: github.com/<origin remote>/releases/download/updater-latest/latest.json)')
    console.log('  --expect-version v  assert the manifest version equals v (post-release check)')
    return 0
  }

  const manifestUrl_ = manifestUrl(options)
  console.log(`verify-update: fetching ${manifestUrl_}`)
  const manifest = await fetchJson(manifestUrl_)

  const version = manifest.version
  const platform = manifest.platforms?.['darwin-aarch64']
  if (typeof version !== 'string' || !BETA_VERSION_RE.test(version)) {
    throw new Error(`latest.json version is not a beta semver: ${JSON.stringify(version)}`)
  }
  if (!platform || typeof platform.url !== 'string' || !platform.url) {
    throw new Error('latest.json has no platforms.darwin-aarch64.url')
  }
  if (typeof platform.signature !== 'string' || !platform.signature) {
    throw new Error('latest.json has no platforms.darwin-aarch64.signature')
  }

  if (options.expectVersion !== undefined && version !== options.expectVersion) {
    throw new Error(`manifest version ${version} does not match --expect-version ${options.expectVersion}`)
  }

  console.log(`verify-update: manifest version = ${version}`)
  console.log(`verify-update: darwin-aarch64.url = ${platform.url}`)
  console.log(`verify-update: signature present = ${platform.signature.length > 0} (${platform.signature.length} chars)`)

  // Signature ↔ configured public key consistency check.
  const pubkey = configuredPubkey()
  if (pubkey) {
    const sigKeyId = signatureKeyId(platform.signature)
    if (sigKeyId.equals(pubkey.keyId)) {
      console.log(`verify-update: signature key id ${sigKeyId.toString('hex')} matches configured pubkey ✓`)
    } else {
      throw new Error(
        `signature key id ${sigKeyId.toString('hex')} does not match the configured ` +
          `updater pubkey (${pubkey.keyId.toString('hex')}) — manifest was NOT signed by this app's key`,
      )
    }
  } else {
    console.warn('verify-update: WARNING — could not read plugins.updater.pubkey from src-tauri/tauri.conf.json; key-id check skipped')
  }

  const status = await checkUrl(platform.url)
  console.log(`verify-update: update package reachable (HTTP ${status})`)
  console.log(`verify-update: OK — version ${version}${options.expectVersion !== undefined ? ` (expected ${options.expectVersion})` : ''}`)
}

main().catch((err) => {
  console.error(`verify-update: FAIL — ${err.message}`)
  process.exit(1)
})
