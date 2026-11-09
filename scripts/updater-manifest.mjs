#!/usr/bin/env node
/**
 * Writes a release's update manifest (latest.json), which Flareite's updater
 * reads (src-tauri/src/app_update.rs, ADR 0040), from the signatures of the
 * installers the release workflow built:
 *
 *   node scripts/updater-manifest.mjs <tag> <owner/repo> <folder of .sig files>
 *
 * Each installer's signature (`<installer>.sig`, made with the key in the
 * TAURI_SIGNING_PRIVATE_KEY secret) becomes the entries for the platforms
 * and kinds of installation it updates, `{os}-{arch}-{kind}` (as the updater
 * looks them up) and `{os}-{arch}` for the default kind. The manifest's
 * version is the tag's `major.minor.patch`, the version the installers were
 * built and signed as (a signature records it, and the updater requires the
 * two to match); the app picks releases by their tags.
 */
import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

/** The kinds of installer, by how their names end. `fallback` also serves `{os}-{arch}`. */
const KINDS = [
  { suffix: '.app.tar.gz', os: 'darwin', kind: 'app', fallback: true },
  { suffix: '.msi', os: 'windows', kind: 'msi', fallback: true },
  { suffix: '-setup.exe', os: 'windows', kind: 'nsis', fallback: false },
  { suffix: '.AppImage', os: 'linux', kind: 'appimage', fallback: true },
  { suffix: '.deb', os: 'linux', kind: 'deb', fallback: false },
  { suffix: '.rpm', os: 'linux', kind: 'rpm', fallback: false },
]

/** The architectures an installer's name says it's for, as the updater names them. */
function architectures(name, os) {
  if (/universal/i.test(name)) {
    return ['x86_64', 'aarch64']
  }
  if (/aarch64|arm64/i.test(name)) {
    return ['aarch64']
  }
  if (/x86_64|x64|amd64/i.test(name)) {
    return ['x86_64']
  }
  if (/i686|_x86[_.]|i386/i.test(name)) {
    return ['i686']
  }
  // Tauri names macOS bundles without an architecture when built for one.
  if (os === 'darwin') {
    return ['x86_64', 'aarch64']
  }
  throw new Error(`can't tell which architecture ${name} is for`)
}

/** The version a signature records in its trusted comment, if it records one. */
export function signedVersion(signature) {
  const text = Buffer.from(signature.trim(), 'base64').toString('utf8')
  const comment = text.split('\n').find(line => line.startsWith('trusted comment:'))
  const field = comment
    ?.slice('trusted comment:'.length)
    .trim()
    .split('\t')
    .find(part => part.startsWith('version:'))
  return field?.slice('version:'.length)
}

/**
 * The manifest for `tag` of `repository`, from `signatures`: `{ name, signature }` for each
 * installer, `name` being the installer's (without `.sig`).
 */
export function manifest({ tag, repository, signatures, date = new Date() }) {
  const version = tag.replace(/^v/, '').split(/[-+]/)[0]
  if (!/^\d+\.\d+\.\d+$/.test(version)) {
    throw new Error(`${tag} doesn't name a version`)
  }
  const platforms = {}
  const add = (key, entry) => {
    if (platforms[key]) {
      throw new Error(`two installers update ${key}`)
    }
    platforms[key] = entry
  }
  for (const { name, signature } of [...signatures].sort((a, b) => a.name.localeCompare(b.name))) {
    const kind = KINDS.find(kind => name.endsWith(kind.suffix))
    if (!kind) {
      continue
    }
    const signed = signedVersion(signature)
    if (signed !== undefined && signed.replace(/^v/, '') !== version) {
      throw new Error(`${name} was signed as ${signed}, not ${version}`)
    }
    const entry = {
      signature: signature.trim(),
      url: `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`,
    }
    for (const arch of architectures(name, kind.os)) {
      add(`${kind.os}-${arch}-${kind.kind}`, entry)
      if (kind.fallback) {
        add(`${kind.os}-${arch}`, entry)
      }
    }
  }
  if (Object.keys(platforms).length === 0) {
    throw new Error('no signed installers were found')
  }
  return {
    version,
    notes: `https://github.com/${repository}/releases/tag/${tag}`,
    pub_date: date.toISOString(),
    platforms,
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [tag, repository, folder] = process.argv.slice(2)
  if (!tag || !repository || !folder) {
    console.error(
      'usage: node scripts/updater-manifest.mjs <tag> <owner/repo> <folder of .sig files>',
    )
    process.exit(2)
  }
  const signatures = readdirSync(folder)
    .filter(file => file.endsWith('.sig'))
    .map(file => ({
      name: file.slice(0, -'.sig'.length),
      signature: readFileSync(join(folder, file), 'utf8'),
    }))
  process.stdout.write(`${JSON.stringify(manifest({ tag, repository, signatures }), null, 2)}\n`)
}
