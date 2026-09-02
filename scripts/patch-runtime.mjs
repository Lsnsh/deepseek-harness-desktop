#!/usr/bin/env node
/**
 * Patch the bundled dsh web GUI to eliminate a per-frame layout/compositing
 * thrash while an assistant "Think" (reasoning) block streams.
 *
 * Background (diagnosed on 0.1.0-beta.10 / @deepseek-ai/dsh 0.1.0-rc.6):
 * the conversation plugin's ReasoningRow keeps a horizontally scrolling
 * "latest line" summary while a turn streams. Every streamed text chunk
 * re-arms a requestAnimationFrame chain whose update reads
 * `element.scrollWidth` (forcing a full synchronous document layout) and then
 * writes `element.scrollLeft = scrollWidth - clientWidth` (forcing a full
 * compositing pass, which in turn re-dirties event regions). On a big
 * conversation this saturates the WebKit main thread (Activity Monitor energy
 * impact ≈ 1000) for the whole reasoning phase.
 *
 * The patch:
 *   1. pins the summary to the right edge with a clamped sentinel
 *      (`scrollLeft = 1e9`) instead of computing `scrollWidth - clientWidth`,
 *      so the update never forces a synchronous layout read;
 *   2. raises the update cooldown from 3 frames to 30 (~500 ms at 60 fps), so
 *      the follow-scroll at most runs twice per second per thinking row while
 *      the text keeps growing.
 *
 * Both changes are visually negligible for a streaming status line, and they
 * collapse the per-frame layout + compositing + event-region work that the
 * sample profile attributes to the scroll thrash.
 *
 * The patch is applied to the *served* client bundle of the conversation
 * plugin inside the assembled runtime (the static shell bundle is untouched).
 * It is idempotent and version-guarded: if the exact upstream snippet is no
 * longer present the script exits non-zero so assembly fails loudly instead of
 * silently shipping an unpatched GUI.
 *
 * Usage:
 *   node scripts/patch-runtime.mjs [path-to-client.js]
 * (defaults to the repo's assembled runtime conversation plugin bundle)
 * @module deepseek-harness-desktop/patch-runtime
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const DESKTOP_ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)))

const DEFAULT_TARGET = join(
  DESKTOP_ROOT,
  'resources',
  'runtime',
  'app',
  'node_modules',
  '@deepseek-ai',
  'dsh-client-ui-conversation',
  'lib',
  'client.js',
)

const DEFAULT_RUNTIME_TARGET = join(
  DESKTOP_ROOT,
  'resources',
  'runtime',
  'app',
  'node_modules',
  '@deepseek-ai',
  'dsh-client-runtime',
  'lib',
  'client.js',
)

/** @type {string} the exact upstream block we patch (tabs as in the published file). */
const OLD_BLOCK = `\t\t\tconst scheduleSummaryScroll = useThrottledVisualUpdate(() => {
\t\t\t\tconst element = summaryRef.current;
\t\t\t\tif (element === null) return;
\t\t\t\telement.scrollLeft = running ? element.scrollWidth - element.clientWidth : 0;
\t\t\t});`

/** @type {string} the patched block. */
const NEW_BLOCK = `\t\t\tconst scheduleSummaryScroll = useThrottledVisualUpdate(() => {
\t\t\t\tconst element = summaryRef.current;
\t\t\t\tif (element === null) return;
\t\t\t\telement.scrollLeft = running ? 1e9 : 0;
\t\t\t}, 30);`

/**
 * Apply the reasoning-row scroll patch to one conversation plugin client
 * bundle. Idempotent; throws when the upstream snippet cannot be located.
 * @param {string} file - path to dsh-client-ui-conversation/lib/client.js.
 * @returns {{applied: boolean, file: string}} applied=true when this run
 *   performed the replacement, false when it was already patched.
 */
export function patchConversationClient(file) {
  const source = readFileSync(file, 'utf8')
  if (source.includes('scrollLeft = running ? 1e9 : 0')) {
    return { applied: false, file }
  }
  if (!source.includes(OLD_BLOCK)) {
    throw new Error(
      `patch-runtime: could not locate the ReasoningRow scroll snippet in ${file}.\n` +
        'The upstream @deepseek-ai/dsh-client-ui-conversation bundle has changed; ' +
        're-check the patch before shipping.',
    )
  }
  const patched = source.replace(OLD_BLOCK, NEW_BLOCK)
  writeFileSync(file, patched)
  return { applied: true, file }
}

/** @type {string} the exact upstream Notifier#schedule block we patch. */
const OLD_SCHEDULE_BLOCK = `\t\t\tschedule(kind) {
\t\t\t\tconst generation = ++this.scheduleGeneration;
\t\t\t\tthis.scheduled = kind;
\t\t\t\tconst publish = () => {
\t\t\t\t\tif (generation !== this.scheduleGeneration) return;
\t\t\t\t\tthis.scheduled = "none";
\t\t\t\t\tthis.flush();
\t\t\t\t};
\t\t\t\tif (kind === "frame") globalThis.requestAnimationFrame(publish);
\t\t\t\telse queueMicrotask(publish);
\t\t\t}`

/** @type {string} the patched block: 32 ms floor between frame flushes,
 *  trailing edge re-arms so no events are ever dropped. */
const NEW_SCHEDULE_BLOCK = `\t\t\tschedule(kind) {
\t\t\t\tconst generation = ++this.scheduleGeneration;
\t\t\t\tthis.scheduled = kind;
\t\t\t\tconst publish = () => {
\t\t\t\t\tif (generation !== this.scheduleGeneration) return;
\t\t\t\t\tif (kind === "frame") {
\t\t\t\t\t\tconst now = performance.now();
\t\t\t\t\t\tconst last = this.lastFramePublish;
\t\t\t\t\t\tif (last !== undefined && now - last < 32) {
\t\t\t\t\t\t\tglobalThis.requestAnimationFrame(publish);
\t\t\t\t\t\t\treturn;
\t\t\t\t\t\t}
\t\t\t\t\t\tthis.lastFramePublish = now;
\t\t\t\t\t}
\t\t\t\t\tthis.scheduled = "none";
\t\t\t\t\tthis.flush();
\t\t\t\t};
\t\t\t\tif (kind === "frame") globalThis.requestAnimationFrame(publish);
\t\t\t\telse queueMicrotask(publish);
\t\t\t}`

/**
 * Apply the 32 ms frame-flush floor to the runtime's conversation notifier
 * (dsh-client-runtime). While a session streams faster than ~31 events per
 * second, the GUI re-renders and repaints at most ~31 fps instead of once per
 * animation frame; below that rate every event flushes as before (no added
 * latency), and the trailing-edge re-arm guarantees no event is dropped.
 * @param {string} file - path to dsh-client-runtime/lib/client.js.
 * @returns {{applied: boolean, file: string}}
 */
export function patchRuntimeFlushFloor(file) {
  const source = readFileSync(file, 'utf8')
  if (source.includes('this.lastFramePublish')) {
    return { applied: false, file }
  }
  if (!source.includes(OLD_SCHEDULE_BLOCK)) {
    throw new Error(
      `patch-runtime: could not locate the Notifier#schedule snippet in ${file}.\n` +
        'The upstream @deepseek-ai/dsh-client-runtime bundle has changed; ' +
        're-check the patch before shipping.',
    )
  }
  const patched = source.replace(OLD_SCHEDULE_BLOCK, NEW_SCHEDULE_BLOCK)
  writeFileSync(file, patched)
  return { applied: true, file }
}

/** Apply every runtime patch to their default repo locations. */
export function patchAll() {
  const results = []
  results.push(patchConversationClient(DEFAULT_TARGET))
  results.push(patchRuntimeFlushFloor(DEFAULT_RUNTIME_TARGET))
  return results
}

// CLI entry.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const results = []
    if (process.argv[2]) {
      // explicit single-file mode: detect which patch applies by path hint
      if (process.argv[2].includes('dsh-client-runtime')) results.push(patchRuntimeFlushFloor(resolve(process.argv[2])))
      else results.push(patchConversationClient(resolve(process.argv[2])))
    } else {
      results.push(...patchAll())
    }
    for (const { applied, file } of results) {
      console.log(`patch-runtime: ${applied ? 'patched' : 'already patched'} ${file}`)
    }
  } catch (error) {
    console.error(String(error))
    process.exit(1)
  }
}
