/**
 * The editor's copy of a document's lines, with their highlights (ADR 0055).
 *
 * The editor renders its view's lines, which come from the backend. A scroll
 * faster than views arrive (dragging the minimap or the scrollbar, a fast
 * fling) would show nothing until they do, so the editor also keeps this
 * copy and draws the rows its view doesn't cover from it, in the same frame
 * (`fill.ts`).
 *
 * - **Fetched in the background** (`editor_lines`), a chunk at a time,
 *   nearest the view first: the whole document when it has at most
 *   `FULL_LINES` lines, otherwise `WINDOW_LINES` on each side of the view.
 * - **Edits** reach it through each view's line splices, as they reach the
 *   view's own lines (`lineCache.ts`), and the lines in a view replace their
 *   copies. A chunk is taken only if it's of the copy's text version.
 * - **Highlights** of lines outside the view can be out of date after an
 *   edit that changes them (opening a block comment, say) until those lines
 *   are in a view again, which draws them correctly.
 */
import type { DisplayLine, EditorView, LinesView, WireLine } from '../ipc/editor'

/** Documents with at most this many lines are copied whole. */
export const FULL_LINES = 20_000
/** Lines copied on each side of the view in larger documents. */
export const WINDOW_LINES = 5_000
/** Lines fetched at a time. */
export const CHUNK_LINES = 1_000

export interface LineStore {
  documentId: number
  /** The text version the lines are of. */
  version: number
  lineCount: number
  /** By document line; `undefined` where not fetched yet. */
  lines: (DisplayLine | undefined)[]
  /** Bumped whenever the lines change, so what's drawn from them can tell. */
  revision: number
}

export function emptyStore(documentId: number, version: number, lineCount: number): LineStore {
  return { documentId, version, lineCount, lines: new Array(lineCount), revision: 0 }
}

/**
 * `store` brought up to `view`: its edits replayed (or, if they can't be,
 * started over) and the view's lines taken. Changes `store` in place when it
 * can follow.
 */
export function followView(store: LineStore | null, view: EditorView): LineStore {
  let next = store?.documentId === view.documentId ? store : null
  if (next && next.version !== view.version) {
    if (view.edits.base === next.version) {
      for (const { start, removed, inserted } of view.edits.splices) {
        spliceLines(next.lines, start, removed, inserted)
      }
      next.version = view.version
    } else {
      next = null
    }
  }
  next ??= emptyStore(view.documentId, view.version, view.lineCount)
  next.lineCount = view.lineCount
  next.lines.length = view.lineCount
  view.lineNumbers.forEach((line, index) => {
    next.lines[line] = view.lines[index]
  })
  next.revision += 1
  return next
}

/** Takes `fetched` lines if they're of `store`'s version; returns whether it did. */
export function addFetched(store: LineStore, fetched: LinesView): boolean {
  if (fetched.documentId !== store.documentId || fetched.version !== store.version) {
    return false
  }
  fetched.lines.forEach((line, index) => {
    const at = fetched.start + index
    if (at < store.lineCount) {
      store.lines[at] = toDisplay(line, fetched.spans[index] ?? [])
    }
  })
  store.revision += 1
  return true
}

/**
 * The lines to fetch next, as `[start, end]`, for a view of lines
 * `viewStart..viewEnd`: from the missing line nearest the view's middle, a
 * chunk away from it. `null` when every line wanted is held.
 */
export function nextFetch(
  store: LineStore,
  viewStart: number,
  viewEnd: number,
): [number, number] | null {
  const { lineCount, lines } = store
  if (lineCount === 0) {
    return null
  }
  const [from, to] =
    lineCount <= FULL_LINES
      ? [0, lineCount]
      : [Math.max(0, viewStart - WINDOW_LINES), Math.min(lineCount, viewEnd + WINDOW_LINES)]
  const middle = Math.min(Math.max(Math.floor((viewStart + viewEnd) / 2), from), to - 1)
  let below = -1
  for (let line = middle; line < to; line++) {
    if (lines[line] === undefined) {
      below = line
      break
    }
  }
  let above = -1
  for (let line = middle - 1; line >= from; line--) {
    if (lines[line] === undefined) {
      above = line
      break
    }
  }
  if (below < 0 && above < 0) {
    return null
  }
  if (above < 0 || (below >= 0 && below - middle <= middle - above)) {
    return [below, Math.min(to, below + CHUNK_LINES)]
  }
  return [Math.max(from, above + 1 - CHUNK_LINES), above + 1]
}

/** Replaces `removed` lines at `start` with `inserted` not yet known. */
function spliceLines(
  lines: (DisplayLine | undefined)[],
  start: number,
  removed: number,
  inserted: number,
): void {
  if (removed === inserted) {
    lines.fill(undefined, start, start + removed)
    return
  }
  // Not `splice(start, removed, ...new Array(inserted))`: a large paste
  // would pass more arguments than a call takes.
  const tail = lines.slice(start + removed)
  lines.length = start + inserted
  lines.fill(undefined, start)
  for (const line of tail) {
    lines.push(line)
  }
}

function toDisplay(line: WireLine, spans: readonly number[]): DisplayLine {
  return typeof line === 'string'
    ? { text: line, fullLength: null, spans }
    : { text: line.text, fullLength: line.fullLength, spans }
}
