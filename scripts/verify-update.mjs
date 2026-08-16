#!/usr/bin/env node
/**
 * Verify the auto-update pointer end-to-end after a release.
 *
 * Every publish overwrites the `latest.json` manifest asset on the fixed
 * `updater-latest` tag (see .github/workflows/release.yml). This script:
 *
 *   1. fetches that manifest,
 *   2. validates its shape (version + platforms.darwin-aarch64.url/signature),
 *   3. probes the update package URL and accepts HTTP 200/302,
 *   4. prints the version + URL so the result is greppable.
 *
 * Usage:
 *   node scripts/verify-update.mjs                      # default repo URL
 *   node scripts/verify-update.mjs <latestJsonUrl>      # explicit manifest URL
 *
 * The default repo is read from `git remote get-url origin` (github.com),
 * falling back to Lsnsh/deepseek-harness-desktop. Exit code 0 = OK,
 * 1 = manifest missing/invalid or the update URL unreachable (e.g. before
 * the first release, when the updater-latest tag does not exist yet).
 * @module deepseek-harness-desktop/verify-update
 */

import { execFileSync } from 'node:child_process'

const MANIFEST_FILE = 'latest.json'
const TIMEOUT_MS = 30_000
const ACCEPTED_STATUS = new Set([200, 302]) // 302: GitHub asset redirect; 200: final CDN

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

function manifestUrl() {
  const explicit = process.argv[2]
  if (explicit) return explicit
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

async function main() {
  const manifestUrl_ = manifestUrl()
  console.log(`verify-update: fetching ${manifestUrl_}`)
  const manifest = await fetchJson(manifestUrl_)

  const version = manifest.version
  const platform = manifest.platforms?.['darwin-aarch64']
  if (typeof version !== 'string' || !version) {
    throw new Error('latest.json has no string "version"')
  }
  if (!platform || typeof platform.url !== 'string' || !platform.url) {
    throw new Error('latest.json has no platforms.darwin-aarch64.url')
  }
  if (typeof platform.signature !== 'string' || !platform.signature) {
    throw new Error('latest.json has no platforms.darwin-aarch64.signature')
  }

  console.log(`verify-update: manifest version = ${version}`)
  console.log(`verify-update: darwin-aarch64.url = ${platform.url}`)
  console.log(`verify-update: signature present = ${platform.signature.length > 0} (${platform.signature.length} chars)`)

  const status = await checkUrl(platform.url)
  console.log(`verify-update: update package reachable (HTTP ${status})`)
  console.log(`verify-update: OK — version ${version}`)
}

main().catch((err) => {
  console.error(`verify-update: FAIL — ${err.message}`)
  process.exit(1)
})
