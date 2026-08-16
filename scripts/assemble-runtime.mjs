#!/usr/bin/env node
/**
 * Assemble the self-contained web runtime the desktop shell spawns.
 *
 * Layout produced under `resources/runtime/`:
 *
 *   node/bin/node[.exe]   the bundled Node.js binary (see download-node.mjs)
 *   app/                  production install of @deepseek-ai/dsh:
 *                         lib/ (CLI entry + mode chunks), config/ (shipped
 *                         agent presets), node_modules/ (real directories,
 *                         npm layout), package.json
 *   manifest.json         platform, node version, structural hash
 *
 * Unlike the original in-repo POC (which deployed a workspace package), this
 * repository consumes `@deepseek-ai/dsh` as a plain npm dependency, so the
 * app/ tree is produced by a clean `npm install --omit=dev` of the pinned
 * version into a temp dir. npm's node_modules is a tree of real directories
 * (no symlinks, no virtual store), so the result is self-contained by
 * construction and survives Tauri's resource bundling and the tar archive
 * extraction on the user's machine.
 *
 * The pinned version is read from the root package.json dependency and can
 * be overridden with `DSH_DESKTOP_DSH_VERSION`.
 * @module deepseek-harness-desktop/assemble-runtime
 */

import { execFileSync } from 'node:child_process'
import { tmpdir } from 'node:os'
import { cpSync, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { createHash } from 'node:crypto'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { downloadNode, nodeBinName, NODE_VERSION } from './download-node.mjs'

const DESKTOP_ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)))
const RUNTIME_DIR = join(DESKTOP_ROOT, 'resources', 'runtime')
const APP_DIR = join(RUNTIME_DIR, 'app')

/** The pinned @deepseek-ai/dsh version, from the root package.json or the env. */
export function dshVersion() {
  if (process.env.DSH_DESKTOP_DSH_VERSION) return process.env.DSH_DESKTOP_DSH_VERSION
  const manifest = JSON.parse(readFileSync(join(DESKTOP_ROOT, 'package.json'), 'utf8'))
  const spec = manifest.dependencies?.['@deepseek-ai/dsh']
  if (typeof spec !== 'string') throw new Error('assemble: root package.json has no @deepseek-ai/dsh dependency')
  return spec.replace(/^[\^~]/, '')
}

/**
 * Install @deepseek-ai/dsh and its full production closure into a temp dir
 * with npm, then move the result to app/. npm's node_modules layout uses
 * real directories, so no symlink materialization is needed.
 */
export function installApp() {
  const work = join(tmpdir(), `dsh-app-${process.pid}`)
  rmSync(work, { recursive: true, force: true })
  mkdirSync(work, { recursive: true })
  try {
    const version = dshVersion()
    writeFileSync(join(work, 'package.json'), JSON.stringify({
      name: 'dsh-runtime-app',
      private: true,
      version: '0.0.0',
      dependencies: { '@deepseek-ai/dsh': version },
    }, null, 2) + '\n')
    console.log(`assemble: npm install @deepseek-ai/dsh@${version} (production)`)
    execFileSync('npm', ['install', '--omit=dev', '--no-audit', '--no-fund', '--no-package-lock'], {
      cwd: work,
      stdio: 'inherit',
    })
    const app = join(work, 'node_modules', '@deepseek-ai', 'dsh')
    for (const required of ['lib/bin.js', 'package.json']) {
      if (!existsSync(join(app, required))) {
        throw new Error(`assemble: installed package missing ${required}`)
      }
    }
    rmSync(APP_DIR, { recursive: true, force: true })
    mkdirSync(dirname(APP_DIR), { recursive: true })
    // Move the installed tree: app/ must contain the package itself at its
    // root plus node_modules/ with the full closure.
    const staged = join(work, 'app')
    mkdirSync(staged, { recursive: true })
    cpSync(app, join(staged, 'pkg'), { recursive: true })
    cpSync(join(work, 'node_modules'), join(staged, 'node_modules'), { recursive: true })
    // The package must resolve its own node_modules for transitive deps, and
    // the CLI entry lives at <root>/node_modules/@deepseek-ai/dsh/lib/bin.js.
    // Reconstruct that shape: app/node_modules/@deepseek-ai/dsh -> pkg copy,
    // with the rest of the closure sitting alongside in app/node_modules.
    mkdirSync(join(staged, 'node_modules', '@deepseek-ai'), { recursive: true })
    rmSync(join(staged, 'node_modules', '@deepseek-ai', 'dsh'), { recursive: true, force: true })
    cpSync(join(staged, 'pkg'), join(staged, 'node_modules', '@deepseek-ai', 'dsh'), { recursive: true })
    rmSync(join(staged, 'pkg'), { recursive: true, force: true })
    // Point the runtime layout at <runtime>/app: keep lib/config/package.json
    // at app/ root AND the canonical node_modules path. The manifest records
    // appEntry = 'app/lib/bin.js'; node resolution from app/lib walks up to
    // app/node_modules.
    rmSync(APP_DIR, { recursive: true, force: true })
    mkdirSync(APP_DIR, { recursive: true })
    const pkg = join(staged, 'node_modules', '@deepseek-ai', 'dsh')
    cpSync(join(pkg, 'lib'), join(APP_DIR, 'lib'), { recursive: true })
    cpSync(join(pkg, 'config'), join(APP_DIR, 'config'), { recursive: true })
    cpSync(join(pkg, 'package.json'), join(APP_DIR, 'package.json'))
    cpSync(join(staged, 'node_modules'), join(APP_DIR, 'node_modules'), { recursive: true })
    // npm writes bin shims into node_modules/.bin as symlinks whose targets
    // are absolute paths into the temp install; cpSync rewrites them to the
    // temp dir, which is gone by the time the bundle runs. The desktop shell
    // never invokes those bins (it spawns node with the CLI entry directly),
    // so drop the whole directory instead of repairing links.
    rmSync(join(APP_DIR, 'node_modules', '.bin'), { recursive: true, force: true })
    // Any other files the package ships (README etc.) are not needed.
    console.log(`assemble: installed @deepseek-ai/dsh@${version} into ${relative(DESKTOP_ROOT, APP_DIR)}`)
  } finally {
    rmSync(work, { recursive: true, force: true })
  }
}

function dirname(p) {
  return resolve(p, '..')
}

function relative(from, to) {
  const rel = resolve(to).startsWith(resolve(from)) ? resolve(to).slice(resolve(from).length) : resolve(to)
  return rel.replace(/^\//, '') || '.'
}

/**
 * A stable structural hash over every regular file under `dir`
 * (relative path + size). Symbolic links are skipped: their targets are
 * hashed as the files they point at, and a dangling link (e.g. a leftover
 * temp-dir shim) must never fail the walk.
 */
export function hashTree(dir) {
  const hash = createHash('sha256')
  const walk = (current) => {
    let entries
    try {
      entries = readdirSync(current, { withFileTypes: true })
    } catch {
      return
    }
    for (const entry of entries) {
      if (entry.isSymbolicLink()) continue
      const path = join(current, entry.name)
      let stat
      try {
        stat = lstatSync(path)
      } catch {
        continue
      }
      if (stat.isDirectory()) {
        walk(path)
      } else if (stat.isFile()) {
        hash.update(relative(dir, path)).update('\0').update(String(stat.size)).update('\0')
      }
    }
  }
  walk(dir)
  return hash.digest('hex')
}

/**
 * Assemble the full runtime. Idempotent: reruns replace app/ and re-verify
 * the node binary; downloads only happen when the binary is missing.
 */
export function assembleRuntime() {
  installApp()
  const node = downloadNode()
  const modulesHash = hashTree(join(APP_DIR, 'node_modules'))
  writeFileSync(join(RUNTIME_DIR, 'manifest.json'), JSON.stringify({
    platform: `${process.platform}-${process.arch}`,
    node: NODE_VERSION,
    nodeBin: nodeBinName(),
    dsh: dshVersion(),
    appEntry: 'app/lib/bin.js',
    nodeModulesHash: modulesHash,
    assembledAt: new Date().toISOString(),
  }, null, 2) + '\n')
  // The bundle ships ONE archive: Tauri's resource bundling drops symlinks
  // (npm layout has none, but the archive keeps everything atomic), and the
  // archive is extracted to the app cache at first launch (see server.rs).
  const archive = join(DESKTOP_ROOT, 'resources', 'runtime.tar.gz')
  rmSync(archive, { force: true })
  execFileSync('tar', ['-czf', archive, '-C', RUNTIME_DIR, '.'], { stdio: 'inherit' })
  const archiveBytes = statSync(archive).size
  console.log(`assemble: runtime archive ready at ${relative(DESKTOP_ROOT, archive)} (${(archiveBytes / 1024 / 1024).toFixed(1)} MiB, modules ${modulesHash.slice(0, 12)}…)`)
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  assembleRuntime()
}
