#!/usr/bin/env node
/**
 * Download the official Node.js binary for the current platform into
 * `resources/runtime/node/` and verify it executes.
 *
 * The desktop app ships its own Node runtime because the harness engine
 * requires Node ^22.19 || >=24 and normal users must not install one. The
 * version is pinned to the project's dev runtime (v24.19.0) and can be
 * overridden with `DSH_DESKTOP_NODE_VERSION` (any `vX.Y.Z` published on
 * nodejs.org/dist).
 * @module deepseek-harness-desktop/download-node
 */

import { chmodSync, copyFileSync, existsSync, mkdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execFileSync } from 'node:child_process'

/** The pinned Node.js version for the bundled runtime. */
export const NODE_VERSION = process.env.DSH_DESKTOP_NODE_VERSION ?? 'v24.19.0'

/** The directory this script's output lives in (…/resources/runtime). */
export const RUNTIME_DIR = resolve(fileURLToPath(new URL('../resources/runtime', import.meta.url)))
/** Where the extracted `node` (or `node.exe`) binary is placed. */
export const NODE_BIN_DIR = join(RUNTIME_DIR, 'node', 'bin')

/** The executable file name of the Node binary per platform. */
export function nodeBinName() {
  return process.platform === 'win32' ? 'node.exe' : 'node'
}

/**
 * The nodejs.org dist file name, archive member directory, and the
 * member-relative path of the node binary for this platform. Windows zips
 * keep `node.exe` at the member root; Unix tarballs nest it under `bin/`.
 */
export function distFileName() {
  const v = NODE_VERSION
  const arch = process.arch === 'arm64' ? 'arm64' : process.arch === 'x64' ? 'x64' : process.arch
  switch (process.platform) {
    case 'darwin': return { archive: `node-${v}-darwin-${arch}.tar.gz`, memberDir: `node-${v}-darwin-${arch}`, memberBin: 'bin/node' }
    case 'win32': return { archive: `node-${v}-win-${arch}.zip`, memberDir: `node-${v}-win-${arch}`, memberBin: 'node.exe' }
    case 'linux': return { archive: `node-${v}-linux-${arch}.tar.gz`, memberDir: `node-${v}-linux-${arch}`, memberBin: 'bin/node' }
    default: throw new Error(`download-node: unsupported platform ${process.platform}-${process.arch}`)
  }
}

/** Whether the bundled Node binary is already present. */
export function nodePresent() {
  return existsSync(join(NODE_BIN_DIR, nodeBinName()))
}

/**
 * Download and extract the platform Node binary. Idempotent: a verified
 * binary in place skips the download.
 * @returns the path of the extracted `node` executable.
 */
export function downloadNode() {
  const binPath = join(NODE_BIN_DIR, nodeBinName())
  if (nodePresent()) {
    execFileSync(binPath, ['--version'], { stdio: 'ignore' })
    return binPath
  }
  const { archive, memberDir, memberBin } = distFileName()
  const url = `https://nodejs.org/dist/${NODE_VERSION}/${archive}`
  const work = join(tmpdir(), `dsh-node-${process.pid}`)
  rmSync(work, { recursive: true, force: true })
  mkdirSync(work, { recursive: true })
  try {
    console.log(`download-node: fetching ${url}`)
    execFileSync('curl', ['-fsSL', '-o', join(work, archive), url], { stdio: 'inherit' })
    // bsdtar handles both .tar.gz and .zip; no extra tool per platform.
    execFileSync('tar', ['-xf', join(work, archive), '-C', work], { stdio: 'inherit' })
    const extractedBin = join(work, memberDir, memberBin)
    if (!existsSync(extractedBin)) {
      throw new Error(`download-node: archive did not contain ${memberDir}/${memberBin}`)
    }
    mkdirSync(NODE_BIN_DIR, { recursive: true })
    copyFileSync(extractedBin, binPath)
    if (process.platform !== 'win32') chmodSync(binPath, 0o755)
    execFileSync(binPath, ['--version'], { stdio: 'inherit' })
    return binPath
  } finally {
    rmSync(work, { recursive: true, force: true })
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  downloadNode()
}
