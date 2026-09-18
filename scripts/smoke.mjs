#!/usr/bin/env node
/**
 * Smoke-test the assembled runtime exactly the way the desktop shell starts
 * it: spawn the bundled Node with the deployed CLI, wait for the readiness
 * URL line, GET the served root, then shut the child down.
 *
 * Usage: `node scripts/smoke.mjs [timeoutSeconds]` from the repo root.
 * @module deepseek-harness-desktop/smoke
 */

import { spawn } from 'node:child_process'
import { readFileSync, existsSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { get } from 'node:http'

const DESKTOP_ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)))
const RUNTIME_DIR = join(DESKTOP_ROOT, 'resources', 'runtime')

const manifest = JSON.parse(readFileSync(join(RUNTIME_DIR, 'manifest.json'), 'utf8'))
const nodeBin = join(RUNTIME_DIR, 'node', 'bin', manifest.nodeBin)
const appEntry = join(RUNTIME_DIR, manifest.appEntry)

if (!existsSync(nodeBin) || !existsSync(appEntry)) {
  throw new Error(`smoke: runtime incomplete (${nodeBin}, ${appEntry}); run assemble-runtime.mjs first`)
}

const timeoutSeconds = Number(process.argv[2] ?? '90')

const child = spawn(nodeBin, [appEntry, '--profile', 'web', '--port', '0', '--no-open'], {
  cwd: RUNTIME_DIR,
  stdio: ['ignore', 'pipe', 'pipe'],
})

let url = ''
const deadline = Date.now() + timeoutSeconds * 1000
let finished = false

const finish = (code) => {
  if (finished) return
  finished = true
  child.kill()
  setTimeout(() => child.kill('SIGKILL'), 2000)
  process.exit(code)
}

child.stdout.setEncoding('utf8')
child.stdout.on('data', (chunk) => {
  process.stdout.write(`[server] ${chunk}`)
  // dsh ≤ 0.1.1 prints a bare URL; 0.1.2 appends a one-time token
  // (`?token=…`) that must be redeemed for later requests to pass, so
  // capture and GET the full URL, not just the port.
  const match = chunk.match(/dsh web: (http:\/\/127\.0\.0\.1:\d+\S*)/)
  if (match && !url) url = match[1]
})
child.stderr.setEncoding('utf8')
child.stderr.on('data', (chunk) => process.stderr.write(`[server] ${chunk}`))
child.on('exit', (code) => {
  if (!url && !finished) {
    console.error(`smoke: server exited (code ${code}) before printing a URL`)
    finish(1)
  }
})

const poll = () => {
  if (url) {
    get(url, (res) => {
      const ok = res.statusCode === 200
      console.log(`smoke: GET ${url} -> ${res.statusCode} ${ok ? 'OK' : 'FAIL'}`)
      finish(ok ? 0 : 1)
    }).on('error', (err) => {
      console.log(`smoke: GET ${url} failed (${err.message}); retrying`)
      schedulePoll()
    })
  } else if (Date.now() > deadline) {
    console.error('smoke: timed out waiting for the server URL line')
    finish(1)
  } else {
    schedulePoll()
  }
}

const schedulePoll = () => {
  if (Date.now() > deadline) {
    console.error('smoke: timed out waiting for the server to become ready')
    finish(1)
  } else {
    setTimeout(poll, 250)
  }
}

poll()
