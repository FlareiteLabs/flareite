/**
 * Links in the terminal: web addresses and file paths in the output,
 * underlined on hover and opened with Ctrl+click (Cmd+click on macOS), or
 * Ctrl+Shift+click to choose who opens them (see `state/links.ts`).
 *
 * A path can end with where in the file it points: `main.rs:12`,
 * `main.rs:12:5`, or `Program.cs(12,5)`. Relative paths are found from the
 * shell's folder: as the shell reports it (OSC 7), else as the nearest
 * PowerShell prompt above says (`PS C:\code>`), else the open folder. Only
 * paths that lead to a file or folder are links.
 */
import type { IBufferLine, ILink, ILinkProvider, Terminal } from '@xterm/xterm'
import { pathKinds } from '../ipc/extensions'
import { openLink, type Link } from '../state/links'
import { workspaceStore } from '../state/workspace'

/** Something in a line of output that may be a link. */
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

/** The web addresses and possible paths in a line of output. */
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

/** The folder a PowerShell prompt names (`PS C:\code> `), if `line` is one. */
export function promptFolder(line: string): string | null {
  const match = /^PS ([A-Za-z]:\\[^>]*|\\\\[^>]+|\/[^>]*)> /.exec(line)
  return match?.[1] ?? null
}

/** How long a path's kind is trusted before it's checked again. */
const CHECKED_FOR_MS = 10_000
const checked = new Map<string, { kind: 'file' | 'folder' | null; at: number }>()

/** What each path is (cached briefly), checking at most 200 at once. */
async function kindsOf(paths: readonly string[]): Promise<Map<string, 'file' | 'folder' | null>> {
  const now = Date.now()
  const missing = paths.filter(path => {
    const known = checked.get(path)
    return !known || now - known.at > CHECKED_FOR_MS
  })
  if (missing.length > 0) {
    const kinds = await pathKinds(missing.slice(0, 200)).catch(() => missing.map(() => null))
    missing.slice(0, 200).forEach((path, index) => {
      checked.set(path, { kind: kinds[index] ?? null, at: now })
    })
  }
  return new Map(paths.map(path => [path, checked.get(path)?.kind ?? null]))
}

/** The text of buffer line `y` (from 0), `''` past the end. */
function lineText(terminal: Terminal, y: number): string {
  const line: IBufferLine | undefined = terminal.buffer.active.getLine(y)
  return line?.translateToString(true) ?? ''
}

/** The folder relative paths on line `y` are probably in: the nearest prompt above it. */
function promptAbove(terminal: Terminal, y: number): string | null {
  for (let row = y; row >= Math.max(0, y - 500); row--) {
    const folder = promptFolder(lineText(terminal, row))
    if (folder) {
      return folder
    }
  }
  return null
}

const TOOLTIP = `Follow link (${/Mac/.test(navigator.userAgent) ? 'Cmd' : 'Ctrl'}+click; with Shift, choose how)`

/**
 * The terminal's links. `cwd` is the shell's folder, when it has said
 * (OSC 7).
 */
export function linkProvider(terminal: Terminal, cwd: () => string | null): ILinkProvider {
  const activate = (link: Link) => (event: MouseEvent) => {
    if (!(event.ctrlKey || event.metaKey)) {
      return
    }
    openLink(link, event.shiftKey)
  }
  // Shift+mousedown extends the selection in xterm.js, so a link never sees
  // Ctrl+Shift+click: it's caught first, for the link under the pointer.
  let hovered: Link | null = null
  let listening: HTMLElement | null = null
  const onMouseDown = (event: MouseEvent) => {
    if (hovered && event.button === 0 && event.shiftKey && (event.ctrlKey || event.metaKey)) {
      event.preventDefault()
      event.stopPropagation()
      openLink(hovered, true)
    }
  }
  const decorate = (text: string, range: ILink['range'], link: Link): ILink => ({
    text,
    range,
    decorations: { underline: true, pointerCursor: true },
    activate: activate(link),
    hover: () => {
      hovered = link
      const element = terminal.element
      if (element && listening !== element) {
        listening?.removeEventListener('mousedown', onMouseDown, true)
        element.addEventListener('mousedown', onMouseDown, true)
        listening = element
      }
      element?.setAttribute('title', TOOLTIP)
    },
    leave: () => {
      hovered = null
      terminal.element?.removeAttribute('title')
    },
  })
  return {
    provideLinks(row, callback) {
      // `row` counts from 1.
      const y = row - 1
      const text = lineText(terminal, y)
      const candidates = findCandidates(text)
      if (candidates.length === 0) {
        callback(undefined)
        return
      }
      const range = (candidate: Candidate): ILink['range'] => ({
        start: { x: candidate.start + 1, y: row },
        end: { x: candidate.end, y: row },
      })
      const links: ILink[] = []
      const paths = candidates.filter(candidate => candidate.kind === 'path')
      for (const candidate of candidates) {
        if (candidate.kind === 'web') {
          links.push(
            decorate(candidate.target, range(candidate), { kind: 'web', target: candidate.target }),
          )
        }
      }
      if (paths.length === 0) {
        callback(links)
        return
      }
      const root = workspaceStore.get().info?.root ?? null
      const folders = [cwd(), promptAbove(terminal, y), root].filter(
        (folder): folder is string => folder !== null,
      )
      const options = paths.map(candidate => resolveCandidates(candidate.target, folders, null))
      void kindsOf(options.flat()).then(kinds => {
        paths.forEach((candidate, index) => {
          const target = options[index]!.find(path => kinds.get(path))
          if (!target) {
            return
          }
          links.push(
            decorate(text.slice(candidate.start, candidate.end), range(candidate), {
              kind: 'file',
              target,
              line: candidate.line === undefined ? null : Math.max(candidate.line - 1, 0),
              column: candidate.column === undefined ? null : Math.max(candidate.column - 1, 0),
            }),
          )
        })
        callback(links.sort((a, b) => a.range.start.x - b.range.start.x))
      })
    },
  }
}

/** The folder an OSC 7 report names (`file://host/path`), if it's valid. */
export function osc7Folder(data: string): string | null {
  return fileUrlPath(data)
}
