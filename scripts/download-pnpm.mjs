#!/usr/bin/env node
/**
 * Download the pnpm npm package into `resources/runtime/pnpm/` and verify it
 * executes with the bundled Node.
 *
 * The desktop shell's `dsh plugin` support needs a pnpm on PATH inside the
 * bundled runtime (the harness forwards plugin operations to pnpm). We ship
 * the plain npm package (bin/pnpm.cjs + dist/) rather than the standalone
 * SEA binary: the standalone embeds its own Node (~162 MiB), while the npm
 * package runs on the Node we already bundle (~37 MiB, ~10 MiB compressed).
 *
 * Layout produced:
 *   resources/runtime/pnpm/bin/pnpm.cjs   (and .mjs / pnpx shims)
 *   resources/runtime/pnpm/dist/           (bundled modules)
 *   resources/runtime/pnpm/package.json
 *
 * The shell exposes it on PATH via a `pnpm` shim that execs
 * `node …/pnpm/bin/pnpm.cjs`.
 * @module deepseek-harness-desktop/download-pnpm
 */

import { chmodSync, cpSync, existsSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execFileSync } from 'node:child_process'

/** The pinned pnpm version, matching the repo's packageManager. */
export const PNPM_VERSION = process.env.DSH_DESKTOP_PNPM_VERSION ?? '11.21.0'

/** The directory this script's output lives in (…/resources/runtime/pnpm). */
export const PNPM_DIR = resolve(fileURLToPath(new URL('../resources/runtime/pnpm', import.meta.url)))

/** The pnpm.cjs entry, relative to PNPM_DIR. */
export const PNPM_ENTRY = 'bin/pnpm.cjs'

/** Whether the pnpm package (entry + dist bundle) is present. */
export function pnpmPresent() {
  return (
    existsSync(join(PNPM_DIR, PNPM_ENTRY))
    && existsSync(join(PNPM_DIR, 'dist', 'pnpm.mjs'))
  )
}

/**
 * Download and unpack the pnpm npm package. Idempotent: a verified install in
 * place skips the download.
 * @returns the path of the pnpm.cjs entry.
 */
export function downloadPnpm() {
  const entryPath = join(PNPM_DIR, PNPM_ENTRY)
  if (pnpmPresent()) {
    verifyPnpm(entryPath)
    return entryPath
  }
  const url = `https://registry.npmjs.org/pnpm/-/pnpm-${PNPM_VERSION}.tgz`
  const work = join(tmpdir(), `dsh-pnpm-${process.pid}`)
  rmSync(work, { recursive: true, force: true })
  mkdirSync(work, { recursive: true })
  try {
    console.log(`download-pnpm: fetching ${url}`)
    execFileSync('curl', ['-fsSL', '-o', join(work, 'pnpm.tgz'), url], { stdio: 'inherit' })
    execFileSync('tar', ['-xzf', join(work, 'pnpm.tgz'), '-C', work], { stdio: 'inherit' })
    const pkg = join(work, 'package')
    for (const required of [PNPM_ENTRY, 'dist/pnpm.mjs']) {
      if (!existsSync(join(pkg, required))) {
        throw new Error(`download-pnpm: package missing ${required}`)
      }
    }
    rmSync(PNPM_DIR, { recursive: true, force: true })
    mkdirSync(PNPM_DIR, { recursive: true })
    cpSync(join(pkg, 'bin'), join(PNPM_DIR, 'bin'), { recursive: true })
    cpSync(join(pkg, 'dist'), join(PNPM_DIR, 'dist'), { recursive: true })
    cpSync(join(pkg, 'package.json'), join(PNPM_DIR, 'package.json'))
    if (process.platform !== 'win32') chmodSync(entryPath, 0o755)
    verifyPnpm(entryPath)
    return entryPath
  } finally {
    rmSync(work, { recursive: true, force: true })
  }
}

/** Run `pnpm --version` through the bundled Node to prove it works. */
function verifyPnpm(entryPath) {
  execFileSync(process.execPath, [entryPath, '--version'], { stdio: 'inherit' })
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  downloadPnpm()
}
