/**
 * Placing what an extension shows in a layout's editor pane (API 1.15):
 * its folds hide lines, and its zones sit above document lines, which the
 * editor places by display row (rows left once folded lines are gone).
 */
import type { LineRange } from '../ipc/extensions'

/**
 * The hidden line ranges `[start, end)` of `folds`, sorted and merged. Line 0
 * can't be hidden: a fold keeps the line before its hidden lines showing.
 */
export function hiddenRanges(folds: readonly LineRange[]): [number, number][] {
  const sorted = folds
    .map(({ start, end }) => [Math.max(start, 1), end] as [number, number])
    .filter(([start, end]) => end > start)
    .sort((a, b) => a[0] - b[0])
  const merged: [number, number][] = []
  for (const [start, end] of sorted) {
    const last = merged.at(-1)
    if (last && start <= last[1]) {
      last[1] = Math.max(last[1], end)
    } else {
      merged.push([start, end])
    }
  }
  return merged
}

/** The fold regions (`[start, end)`, `start` showing) that hide `hidden`'s lines. */
export const foldRegions = (hidden: readonly (readonly [number, number])[]): [number, number][] =>
  hidden.map(([start, end]) => [start - 1, end])

/** The display row of `line` when `hidden` ranges (in order) are folded away. */
export function rowOfLine(hidden: readonly (readonly [number, number])[], line: number): number {
  let row = line
  for (const [start, end] of hidden) {
    if (start > line) {
      break
    }
    row -= Math.min(line + 1, end) - start
    if (line < end) {
      // A hidden line is on the row of the line before.
      return row
    }
  }
  return row
}

/**
 * The row a zone above document line `line` takes in the editor: zones go
 * below the row they name, so it's the row of the line before (`-1` above
 * the first line).
 */
export const zoneRow = (hidden: readonly (readonly [number, number])[], line: number): number =>
  rowOfLine(hidden, line - 1)
