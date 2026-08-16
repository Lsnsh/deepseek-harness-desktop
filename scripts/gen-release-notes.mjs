#!/usr/bin/env node
/**
 * Generate per-version release notes from the changelogs into
 * `docs/release-notes/v<version>-<lang>.md` (lang: zh | en), following the
 * cc-switch convention. The GitHub release description uses the zh notes by
 * default; the en notes are archived for reference.
 *
 * Usage:
 *   node scripts/gen-release-notes.mjs <version>     # e.g. 0.1.0-beta.8
 *   node scripts/gen-release-notes.mjs --all         # all released versions
 * @module deepseek-harness-desktop/gen-release-notes
 */

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)))
const OUT_DIR = join(ROOT, 'docs', 'release-notes')

/** Extract the changelog section for one version from a changelog file. */
function extractSection(changelogPath, version) {
  const text = readFileSync(changelogPath, 'utf8')
  const marker = `## [${version}]`
  const start = text.indexOf(marker)
  if (start === -1) return null
  const after = text.slice(start + marker.length)
  const end = after.search(/\n## \[/)
  const section = (end === -1 ? after : after.slice(0, end)).trim()
  // Drop a leading "— YYYY-MM-DD" date line if present.
  return section.replace(/^—\s*\d{4}-\d{2}-\d{2}\s*\n?/, '').trim()
}

/** Generate one language's release notes file. */
export function genReleaseNotes(version, lang) {
  const changelog = lang === 'zh' ? 'CHANGELOG-zh.md' : 'CHANGELOG.md'
  const section = extractSection(join(ROOT, changelog), version)
  if (section === null) {
    console.error(`gen-release-notes: no ${version} section in ${changelog}`)
    return false
  }
  const tag = `v${version}`
  const header = `# DeepSeek Harness Developer Preview ${tag} — ${lang === 'zh' ? '更新说明' : 'Release Notes'}\n\n`
  const footer =
    lang === 'zh'
      ? `\n---\n📦 下载：<https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/${tag}>\n`
      : `\n---\n📦 Download: <https://github.com/Lsnsh/deepseek-harness-desktop/releases/tag/${tag}>\n`
  const body = `${header}${section}${footer}\n`
  mkdirSync(OUT_DIR, { recursive: true })
  const out = join(OUT_DIR, `${tag}-${lang}.md`)
  writeFileSync(out, body)
  console.log(`gen-release-notes: wrote ${out}`)
  return true
}

/** Released versions to (re)generate, newest first. */
export const RELEASED_VERSIONS = [
  '0.1.0-beta.8',
  '0.1.0-beta.7',
  '0.1.0-beta.6',
  '0.1.0-beta.5',
  '0.1.0-beta.4',
  '0.1.0-beta.3',
  '0.1.0-beta.2',
  '0.1.0-beta.1',
  '0.1.0-beta.0',
]

export function main(argv) {
  const versions = argv.includes('--all') ? RELEASED_VERSIONS : argv.filter((a) => !a.startsWith('--'))
  if (versions.length === 0) {
    console.error('usage: node scripts/gen-release-notes.mjs <version> | --all')
    return 1
  }
  let ok = true
  for (const version of versions) {
    if (!genReleaseNotes(version, 'zh')) ok = false
    if (!genReleaseNotes(version, 'en')) ok = false
  }
  return ok ? 0 : 1
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)))
}
