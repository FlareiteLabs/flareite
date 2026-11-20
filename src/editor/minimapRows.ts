/**
 * The minimap's rows as fetched so far, and which to fetch next.
 *
 * Rows are kept for one state of the document (its version and refresh)
 * and added to as the minimap scrolls, so scrolling fetches only the rows it
 * brings in instead of the whole window again. The rows shown come first;
 * then rows ahead in the direction of scrolling, asked for once a quarter
 * of those held have scrolled into view (as the editor does, see
 * `scrollAhead.ts`). A document of at most `FULL_ROWS` rows is then
 * fetched whole, so dragging through it never waits for rows.
 *
 * After an edit, the rows of the document as it was are kept (`stale`) and
 * drawn until the new ones replace them, so the minimap is never blank
 * while it catches up.
 */
import type { MinimapHeader, MinimapView } from '../ipc/editor'
import type { Margins } from './scrollAhead'

/** Most rows one request returns (`MAX_MINIMAP_LINES` in the backend). */
export const MAX_REQUEST_ROWS = 1000
/** Documents with at most this many rows are fetched whole. */
export const FULL_ROWS = 20_000
/** Most rows kept; those farthest from the latest ones go first. */
export const MAX_KEPT_ROWS = 24_000

export interface MinimapRows {
  /** The state of the document the rows show (see `Minimap.tsx`). */
  key: string
  documentId: number
  rowCount: number
  /** Each row's runs: `[start, end, kind, ...]`. */
  rows: Map<number, readonly number[]>
  headers: Map<number, MinimapHeader>
  /** Rows and headers of an earlier state, drawn where `rows` has none yet. */
  stale: Map<number, readonly number[]>
  staleHeaders: Map<number, MinimapHeader>
}

/** The runs to draw for `row`: current, or else from an earlier state. */
export function runsAt(store: MinimapRows, row: number): readonly number[] | undefined {
  return store.rows.get(row) ?? store.stale.get(row)
}

/** The headers to draw: current, and those of an earlier state on rows not fetched again. */
export function headersOf(store: MinimapRows): MinimapHeader[] {
  return [...store.headers.values(), ...store.staleHeaders.values()]
}

/**
 * `store` with the rows of `view` added, if it has the same `key`;
 * otherwise new rows with only those of `view`, and the rows `store` had
 * kept to draw until they're fetched again.
 */
export function withRows(store: MinimapRows | null, key: string, view: MinimapView): MinimapRows {
  const earlier = store?.documentId === view.documentId ? store : null
  const next: MinimapRows =
    store?.key === key
      ? store
      : {
          key,
          documentId: view.documentId,
          rowCount: view.rowCount,
          rows: new Map(),
          headers: new Map(),
          // The earlier rows are dropped, so their maps are reused: only the
          // rows fetched since its stale ones were kept are copied.
          stale: mergeInto(earlier?.stale, earlier?.rows),
          staleHeaders: mergeInto(earlier?.staleHeaders, earlier?.headers),
        }
  const end = view.startRow + view.lines.length
  next.rowCount = view.rowCount
  view.lines.forEach((runs, index) => {
    next.rows.set(view.startRow + index, runs)
    next.stale.delete(view.startRow + index)
  })
  for (const headers of [next.headers, next.staleHeaders]) {
    for (const row of [...headers.keys()]) {
      if ((row >= view.startRow && row < end) || row >= view.rowCount) {
        headers.delete(row)
      }
    }
  }
  for (const header of view.headers) {
    next.headers.set(header.row, header)
  }
  if (next.rows.size > MAX_KEPT_ROWS) {
    const middle = (view.startRow + end) / 2
    const farthest = [...next.rows.keys()].sort(
      (a, b) => Math.abs(b - middle) - Math.abs(a - middle),
    )
    for (const row of farthest.slice(0, next.rows.size - MAX_KEPT_ROWS)) {
      next.rows.delete(row)
      next.headers.delete(row)
    }
  }
  return next
}

/** Whether `store` holds every row of `start..end` that the document has. */
export function holds(store: MinimapRows | null, start: number, end: number): boolean {
  if (!store) {
    return false
  }
  for (let row = Math.max(0, start); row < Math.min(end, store.rowCount); row++) {
    if (!store.rows.has(row) && !store.stale.has(row)) {
      return false
    }
  }
  return true
}

/**
 * The rows to fetch once those needed are held: for a document of at most
 * `FULL_ROWS` rows, the next missing ones nearest those `shown`, a
 * request's worth. `null` when there are none.
 */
export function restRequest(
  store: MinimapRows,
  shown: readonly [number, number],
  rowCount: number,
): [number, number] | null {
  if (rowCount > FULL_ROWS || store.rows.size >= rowCount) {
    return null
  }
  const middle = Math.min(Math.max(0, Math.floor((shown[0] + shown[1]) / 2)), rowCount - 1)
  let below = -1
  for (let row = middle; row < rowCount; row++) {
    if (!store.rows.has(row)) {
      below = row
      break
    }
  }
  let above = -1
  for (let row = middle - 1; row >= 0; row--) {
    if (!store.rows.has(row)) {
      above = row
      break
    }
  }
  if (above < 0 || (below >= 0 && below - middle <= middle - above)) {
    return below < 0 ? null : [below, Math.min(rowCount, below + MAX_REQUEST_ROWS)]
  }
  return [Math.max(0, above + 1 - MAX_REQUEST_ROWS), above + 1]
}

/**
 * The rows to fetch next, as `[start, end]`, for a minimap showing rows
 * `shown` of `rowCount` and keeping `margins` more around them; `null` if
 * the rows held already include `needed` around those shown.
 */
export function nextRequest(
  store: MinimapRows | null,
  shown: readonly [number, number],
  margins: Margins,
  needed: Margins,
  rowCount: number,
): [number, number] | null {
  const [start, end] = [shown[0], Math.min(shown[1], rowCount)]
  const has = (row: number) => store?.rows.has(row) ?? false
  if (holdsAll(has, Math.max(0, start - needed.above), Math.min(rowCount, end + needed.below))) {
    return null
  }
  // After a jump, when most of what's shown is missing, only that: the
  // rest follows once it's drawn.
  let missing = 0
  for (let row = start; row < end; row++) {
    missing += has(row) ? 0 : 1
  }
  const [from, to] =
    missing > (end - start) / 2
      ? [start, end]
      : [Math.max(0, start - margins.above), Math.min(rowCount, end + margins.below)]
  let first = from
  while (first < to && has(first)) {
    first++
  }
  let last = to
  while (last > first && has(last - 1)) {
    last--
  }
  if (first >= last) {
    return null
  }
  // Rows nearest those shown matter most.
  return last - first <= MAX_REQUEST_ROWS
    ? [first, last]
    : first < start
      ? [last - MAX_REQUEST_ROWS, last]
      : [first, first + MAX_REQUEST_ROWS]
}

/** `into` (or a new map) with the entries of `from` set in it. */
function mergeInto<T>(into: Map<number, T> | undefined, from: Map<number, T> | undefined) {
  const merged = into ?? new Map<number, T>()
  for (const [row, value] of from ?? []) {
    merged.set(row, value)
  }
  return merged
}

function holdsAll(has: (row: number) => boolean, start: number, end: number): boolean {
  for (let row = start; row < end; row++) {
    if (!has(row)) {
      return false
    }
  }
  return true
}
