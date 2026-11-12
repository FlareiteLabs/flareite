/**
 * Finding lines among the rows of a view. A view holds its visible lines in
 * order (`lineNumbers`); lines in folded regions have none. Each line takes
 * one display row, or one per row it wraps to (`wraps`), so `rowStarts`
 * gives the row each held line starts on, counted from the view's first.
 */
import type { LineWrap } from '../ipc/editor'

/** The index of the first of `lineNumbers` (sorted) at or after `line`. */
export function indexAtOrAfter(lineNumbers: readonly number[], line: number): number {
  let low = 0
  let high = lineNumbers.length
  while (low < high) {
    const middle = (low + high) >> 1
    if (lineNumbers[middle]! < line) {
      low = middle + 1
    } else {
      high = middle
    }
  }
  return low
}

/** The index of `line` in `lineNumbers`, or -1 if the view doesn't hold it. */
export function lineIndex(lineNumbers: readonly number[], line: number): number {
  const index = indexAtOrAfter(lineNumbers, line)
  return lineNumbers[index] === line ? index : -1
}

/**
 * The row each of `lineNumbers` starts on, counted from the first, and
 * after them the rows they take in all (one longer than `lineNumbers`).
 */
export function rowStartsOf(
  lineNumbers: readonly number[],
  wraps: ReadonlyMap<number, LineWrap>,
): number[] {
  const starts = [0]
  for (const line of lineNumbers) {
    starts.push(starts.at(-1)! + 1 + (wraps.get(line)?.breaks.length ?? 0))
  }
  return starts
}

interface Rows {
  lineNumbers: readonly number[]
  /** See `rowStartsOf`. */
  rowStarts: readonly number[]
  lineCount: number
  rowCount: number
  startRow: number
}

/** The rows the view's lines take. */
export function viewRows(view: Rows): number {
  return view.rowStarts.at(-1) ?? 0
}

/**
 * The line on `row` (counted from the view's first row) and which of its
 * rows it is, with its index in the view (-1 outside it). Rows outside the
 * view are guessed as if nothing beyond it were folded or wrapped.
 */
export function positionAtRow(
  view: Rows,
  row: number,
): { line: number; index: number; segment: number } {
  const { lineNumbers, rowStarts } = view
  if (row < 0) {
    return { line: Math.max(0, (lineNumbers[0] ?? 0) + row), index: -1, segment: 0 }
  }
  const rows = viewRows(view)
  if (row >= rows) {
    const line = Math.min(view.lineCount - 1, (lineNumbers.at(-1) ?? 0) + row - rows + 1)
    return { line, index: -1, segment: 0 }
  }
  // The last line starting at or before `row`.
  let low = 0
  let high = lineNumbers.length - 1
  while (low < high) {
    const middle = (low + high + 1) >> 1
    if (rowStarts[middle]! <= row) {
      low = middle
    } else {
      high = middle - 1
    }
  }
  return { line: lineNumbers[low]!, index: low, segment: row - rowStarts[low]! }
}

/** The line on `row` (counted from the view's first row); see `positionAtRow`. */
export function lineAtRow(view: Rows, row: number): number {
  return positionAtRow(view, row).line
}

/**
 * The display row of `line`'s first row: exact in the view (a hidden line
 * is on its region's first row), guessed outside it.
 */
export function displayRow(view: Rows, line: number): number {
  const { lineNumbers, rowStarts } = view
  const first = lineNumbers[0] ?? 0
  const last = lineNumbers.at(-1) ?? 0
  if (line < first) {
    return Math.min(line, view.startRow)
  }
  if (line > last) {
    return Math.max(view.startRow + viewRows(view), line - (view.lineCount - view.rowCount))
  }
  const index = indexAtOrAfter(lineNumbers, line)
  return view.startRow + rowStarts[lineNumbers[index] === line ? index : index - 1]!
}

/** The display row of `line`'s last row (its only one, unless it wraps). */
export function lastRow(view: Rows, line: number): number {
  const index = lineIndex(view.lineNumbers, line)
  if (index < 0) {
    return displayRow(view, line)
  }
  return view.startRow + view.rowStarts[index + 1]! - 1
}

/** The row of a line (0 for its first) that UTF-16 `column` is on. */
export function segmentOf(wrap: LineWrap | undefined, column: number): number {
  if (!wrap) {
    return 0
  }
  let segment = 0
  while (segment < wrap.breaks.length && wrap.breaks[segment]! <= column) {
    segment++
  }
  return segment
}

/** Where row `segment` of a line of `length` starts and ends (UTF-16). */
export function segmentRange(
  wrap: LineWrap | undefined,
  segment: number,
  length: number,
): [number, number] {
  if (!wrap) {
    return [0, length]
  }
  const start = segment === 0 ? 0 : (wrap.breaks[segment - 1] ?? length)
  return [Math.min(start, length), Math.min(wrap.breaks[segment] ?? length, length)]
}
