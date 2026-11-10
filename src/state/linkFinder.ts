/**
 * Finding links in text: web addresses and file paths, for the terminal,
 * the editor's text, the Output view, notifications, and anywhere else
 * text is shown. Opening them is `links.ts`'s job.
 *
 * A path can end with where in the file it points: `main.rs:12`,
 * `main.rs:12:5`, or `Program.cs(12,5)`, and `file:` addresses are paths.
 * Only paths that lead to a file or folder are links: each is checked with
 * the backend (`path_kinds`), from the folders the caller says relative
 * paths are in.
 */
import { pathKinds } from '../ipc/extensions'
import type { Link } from './links'

/** Something in a line of text that may be a link. */
export interface Candidate {
  /** Where it is in the line: `[start, end)` in characters. */
  start: number
  end: number
  kind: 'web' | 'path'
  /** The address, or the path as written (without where in the file). */
  target: string
  /** For a path: the line and column it names, from 1, as written. */
  line?: number
  column?: number
}

/** A link found in text: `[start, end)` in characters. */
export interface FoundLink {
  start: number
  end: number
  link: Link
}

const URL = /\b(?:https?:\/\/|mailto:)[^\s<>"'`]+|\bfile:\/\/\/?[^\s<>"'`]+/g
// Absolute Windows and Unix paths, and relative ones with a separator or
// a file extension.
const PATH =
  /(?:[A-Za-z]:[\\/]|\\\\|~[\\/]|\.{1,2}[\\/]|\/)?(?:[\w.$@+~-]+[\\/])*[\w.$@+~-]+(?::\d+(?::\d+)?|\(\d+(?:,\s*\d+)?\))?/g
const WHERE = /(?::(\d+)(?::(\d+))?|\((\d+)(?:,\s*(\d+))?\))$/

/** Punctuation that ends a sentence rather than a link. */
const TRAILING = /[.,;:!?'"`)\]}>]+$/

/** `text` without trailing punctuation, keeping a `)` that closes a `(` inside it. */
function trimEnd(text: string): string {
  let trimmed = text.replace(TRAILING, '')
  const rest = text.slice(trimmed.length)
  // Keep closing parentheses that balance ones in the link (Wikipedia links).
  for (const c of rest) {
    const open = (trimmed.match(/\(/g) ?? []).length
    const close = (trimmed.match(/\)/g) ?? []).length
    if (c === ')' && open > close) {
      trimmed += c
    } else {
      break
    }
  }
  return trimmed
}

/** The web addresses and possible paths in a line of text. */
export function findCandidates(line: string): Candidate[] {
  const found: Candidate[] = []
  const taken: [number, number][] = []
  for (const match of line.matchAll(URL)) {
    const text = trimEnd(match[0])
    const start = match.index
    if (/^file:/i.test(text)) {
      const path = fileUrlPath(text)
      if (path) {
        found.push({ start, end: start + text.length, kind: 'path', target: path })
      }
    } else if (text.length > 8) {
      found.push({ start, end: start + text.length, kind: 'web', target: text })
    }
    taken.push([start, start + text.length])
  }
  for (const match of line.matchAll(PATH)) {
    const start = match.index
    let text = trimEnd(match[0])
    // `(12,5)` was trimmed with the `)`: put it back.
    if (/\(\d+(?:,\s*\d+)?$/.test(text) && match[0].startsWith(text + ')')) {
      text += ')'
    }
    const end = start + text.length
    if (taken.some(([from, to]) => start < to && end > from)) {
      continue
    }
    const where = WHERE.exec(text)
    const path = where ? text.slice(0, where.index) : text
    // A path needs a separator or an extension to be worth checking.
    if (!/[\\/]/.test(path) && !/\.[A-Za-z][A-Za-z0-9]{0,7}$/.test(path)) {
      continue
    }
    if (/^\d+(?:\.\d+)*$/.test(path) || path.length < 2) {
      continue
    }
    const line = where ? Number(where[1] ?? where[3]) : undefined
    const columnText = where ? (where[2] ?? where[4]) : undefined
    found.push({
      start,
      end,
      kind: 'path',
      target: path,
      line,
      column: columnText === undefined ? undefined : Number(columnText),
    })
  }
  return found.sort((a, b) => a.start - b.start)
}

/** The path of a `file:` address; `null` if it isn't one. */
export function fileUrlPath(url: string): string | null {
  try {
    const parsed = new globalThis.URL(url)
    if (parsed.protocol !== 'file:') {
      return null
    }
    const path = decodeURIComponent(parsed.pathname)
    // `file:///C:/code` is `/C:/code`.
    return /^\/[A-Za-z]:\//.test(path) ? path.slice(1) : path
  } catch {
    return null
  }
}

const isWindowsPath = (path: string) => /^[A-Za-z]:[\\/]/.test(path) || path.startsWith('\\\\')
const isAbsolute = (path: string) => isWindowsPath(path) || path.startsWith('/')

/** `relative` in `folder`, with the folder's separators. */
export function joinPath(folder: string, relative: string): string {
  const separator = folder.includes('\\') || isWindowsPath(folder) ? '\\' : '/'
  const parts = folder.split(/[\\/]/)
  if (parts.at(-1) === '') {
    parts.pop()
  }
  for (const part of relative.split(/[\\/]/)) {
    if (part === '..') {
      if (parts.length > 1) {
        parts.pop()
      }
    } else if (part !== '.' && part !== '') {
      parts.push(part)
    }
  }
  const joined = parts.join(separator)
  return /^[A-Za-z]:$/.test(joined) ? joined + separator : joined
}

/** The folder a file's path is in; `null` for a path with no folder. */
export function folderOf(path: string): string | null {
  const cut = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
  if (cut < 0) {
    return null
  }
  const folder = path.slice(0, cut)
  // `C:\a.txt` is in `C:\`, `/a.txt` in `/`.
  return /^[A-Za-z]:$/.test(folder) || folder === '' ? path.slice(0, cut + 1) : folder
}

/** The absolute paths a path may be: itself, or in one of `folders`. */
export function resolveCandidates(
  path: string,
  folders: readonly string[],
  home: string | null,
): string[] {
  if (path.startsWith('~') && home) {
    return [joinPath(home, path.slice(2))]
  }
  if (isAbsolute(path)) {
    return [path]
  }
  return [...new Set(folders.map(folder => joinPath(folder, path)))]
}

/** How long a path's kind is trusted before it's checked again. */
const CHECKED_FOR_MS = 10_000
/** Paths asked about at once; a batch with more asks in turns. */
const CHUNK = 200
type Kind = 'file' | 'folder' | null
const checked = new Map<string, { kind: Kind; at: number }>()
let waiting: { paths: Set<string>; done: Promise<void> } | null = null

/**
 * Checks `paths` with the backend. Paths asked for in the same moment (a
 * view showing many lines at once) are checked in one call.
 */
function check(paths: readonly string[]): Promise<void> {
  if (!waiting) {
    const batch: { paths: Set<string>; done: Promise<void> } = {
      paths: new Set(),
      done: Promise.resolve(),
    }
    batch.done = Promise.resolve().then(async () => {
      waiting = null
      const all = [...batch.paths]
      const at = Date.now()
      if (checked.size > 10_000) {
        for (const [path, known] of checked) {
          if (at - known.at > CHECKED_FOR_MS) {
            checked.delete(path)
          }
        }
      }
      for (let index = 0; index < all.length; index += CHUNK) {
        const chunk = all.slice(index, index + CHUNK)
        const kinds = await pathKinds(chunk).catch(() => chunk.map(() => null))
        chunk.forEach((path, offset) => checked.set(path, { kind: kinds[offset] ?? null, at }))
      }
    })
    waiting = batch
  }
  for (const path of paths) {
    waiting.paths.add(path)
  }
  return waiting.done
}

/** What each path is: a file, a folder, or nothing (`null`), cached briefly. */
export async function kindsOf(paths: readonly string[]): Promise<Map<string, Kind>> {
  const now = Date.now()
  const missing = paths.filter(path => {
    const known = checked.get(path)
    return !known || now - known.at > CHECKED_FOR_MS
  })
  if (missing.length > 0) {
    await check(missing)
  }
  return new Map(paths.map(path => [path, checked.get(path)?.kind ?? null]))
}

/** Forgets what paths were found to be (for tests). */
export function forgetPathKinds(): void {
  checked.clear()
}

/** The link a path candidate makes, `null` if it doesn't lead anywhere. */
export async function resolvePath(
  candidate: Candidate,
  folders: readonly string[],
): Promise<Link | null> {
  const options = resolveCandidates(candidate.target, folders, null)
  const kinds = await kindsOf(options)
  const target = options.find(path => kinds.get(path))
  return target ? pathLink(candidate, target) : null
}

function pathLink(candidate: Candidate, target: string): Link {
  return {
    kind: 'file',
    target,
    line: candidate.line === undefined ? null : Math.max(candidate.line - 1, 0),
    column: candidate.column === undefined ? null : Math.max(candidate.column - 1, 0),
  }
}

/** The web addresses in `text`, found at once. */
export function findWebLinks(text: string): FoundLink[] {
  return findCandidates(text)
    .filter(candidate => candidate.kind === 'web')
    .map(({ start, end, target }) => ({ start, end, link: { kind: 'web', target } }))
}

/**
 * The links in `text`: its web addresses, and its paths that lead to a
 * file or folder, relative ones looked for in `folders` in turn.
 */
export async function findLinks(text: string, folders: readonly string[]): Promise<FoundLink[]> {
  const candidates = findCandidates(text)
  const paths = candidates.filter(candidate => candidate.kind === 'path')
  const options = paths.map(candidate => resolveCandidates(candidate.target, folders, null))
  const kinds = options.length > 0 ? await kindsOf(options.flat()) : new Map<string, Kind>()
  const found: FoundLink[] = []
  for (const candidate of candidates) {
    if (candidate.kind === 'web') {
      found.push({
        start: candidate.start,
        end: candidate.end,
        link: { kind: 'web', target: candidate.target },
      })
      continue
    }
    const target = options[paths.indexOf(candidate)]!.find(path => kinds.get(path))
    if (target) {
      found.push({ start: candidate.start, end: candidate.end, link: pathLink(candidate, target) })
    }
  }
  return found
}
