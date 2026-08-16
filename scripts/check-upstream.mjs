#!/usr/bin/env node
/**
 * Check whether the pinned @deepseek-ai/dsh dependency is behind the latest
 * npm release, and — with `--apply` — bump the root package.json dependency
 * to the new version. Used by the scheduled `check-upstream` workflow; the
 * actual dependency update is left to a PR so CI validates it first.
 *
 * Exit codes:
 *   0  up to date (or --apply bumped the dependency)
 *   1  update available, no --apply given (CI opens a PR)
 *   2  network/registry error
 *
 * @module deepseek-harness-desktop/check-upstream
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)))

/** Compare two semver-ish versions (numbers dotted; prerelease sorts lower). */
export function compareVersions(a, b) {
  const parse = (v) => {
    const [core, pre] = String(v).split('-', 2)
    const nums = core.split('.').map((n) => Number(n) || 0)
    return { nums, pre: pre ?? '' }
  }
  const pa = parse(a)
  const pb = parse(b)
  for (let i = 0; i < Math.max(pa.nums.length, pb.nums.length); i++) {
    const na = pa.nums[i] ?? 0
    const nb = pb.nums[i] ?? 0
    if (na !== nb) return na > nb ? 1 : -1
  }
  if (pa.pre === pb.pre) return 0
  if (pa.pre === '') return 1
  if (pb.pre === '') return -1
  return pa.pre < pb.pre ? -1 : 1
}

/** The currently pinned version in the root package.json. */
export function pinnedVersion() {
  const manifest = JSON.parse(readFileSync(resolve(ROOT, 'package.json'), 'utf8'))
  const spec = manifest.dependencies?.['@deepseek-ai/dsh']
  if (typeof spec !== 'string') throw new Error('check-upstream: no @deepseek-ai/dsh dependency in package.json')
  return spec.replace(/^[\^~]/, '')
}

/** The latest published version on the npm registry. */
export async function latestVersion() {
  const url = 'https://registry.npmjs.org/@deepseek-ai/dsh/latest'
  const response = await fetch(url, { headers: { accept: 'application/json' } })
  if (!response.ok) throw new Error(`registry returned HTTP ${response.status}`)
  const data = await response.json()
  const version = data?.version
  if (typeof version !== 'string') throw new Error(`registry response has no version: ${JSON.stringify(data).slice(0, 200)}`)
  return version
}

/** Bump the root package.json dependency to `version` (in place). */
export function applyBump(version) {
  const manifestPath = resolve(ROOT, 'package.json')
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'))
  manifest.dependencies['@deepseek-ai/dsh'] = version
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n')
}

export async function main(argv) {
  const apply = argv.includes('--apply')
  let pinned
  let latest
  try {
    pinned = pinnedVersion()
  } catch (error) {
    console.error(`check-upstream: ${error.message}`)
    return 2
  }
  try {
    latest = await latestVersion()
  } catch (error) {
    console.error(`check-upstream: cannot reach the registry: ${error.message}`)
    return 2
  }
  console.log(`check-upstream: pinned ${pinned}, latest ${latest}`)
  if (compareVersions(latest, pinned) <= 0) {
    console.log('check-upstream: up to date')
    return 0
  }
  console.log(`check-upstream: update available: ${pinned} -> ${latest}`)
  if (apply) {
    applyBump(latest)
    console.log(`check-upstream: bumped package.json to ${latest}`)
    return 0
  }
  return 1
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2)).then((code) => process.exit(code))
}
