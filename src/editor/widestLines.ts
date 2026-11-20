/**
 * The widest lines the editor has shown, which set how far it scrolls
 * sideways.
 *
 * Only some lines are rendered at a time, so sizing the content to them
 * would make the horizontal scroll range grow and shrink as lines scroll in
 * and out. Instead the editor remembers the widest lines it has rendered
 * (their line numbers following edits, as the lines do) and keeps the
 * content at least as wide as the widest. The range grows as wider lines
 * are rendered, and shrinks only when the widest line gets narrower or is
 * removed, to the next widest remembered.
 */
import type { LineSplice } from '../ipc/editor'

/** How many of the widest lines are remembered. */
export const KEPT_LINES = 8

export interface WidestLines {
  /** The text version the line numbers are of. */
  version: number
  /** The widest lines and their widths in pixels, widest first. */
  lines: { line: number; width: number }[]
}

/** The widest width remembered, 0 if none. */
export const widestWidth = (widest: WidestLines | null): number => widest?.lines[0]?.width ?? 0

/**
 * `widest` after a view of `version`, whose lines moved by `edits` since
 * the view before (`null` base when unknown), with the widths of the lines
 * it rendered (`measured`, by line). A line edited or removed is forgotten
 * unless it's measured again.
 */
export function followWidest(
  widest: WidestLines | null,
  version: number,
  edits: { base: number | null; splices: readonly LineSplice[] },
  measured: ReadonlyMap<number, number>,
): WidestLines {
  let lines = widest?.lines ?? []
  if (widest && widest.version !== version) {
    if (edits.base === widest.version) {
      for (const { start, removed, inserted } of edits.splices) {
        lines = lines.flatMap(({ line, width }) =>
          line < start
            ? [{ line, width }]
            : line < start + removed
              ? []
              : [{ line: line + inserted - removed, width }],
        )
      }
    } else {
      // The lines can't be followed: start over from those measured.
      lines = []
    }
  }
  const next = lines.filter(({ line }) => !measured.has(line))
  for (const [line, width] of measured) {
    next.push({ line, width })
  }
  next.sort((a, b) => b.width - a.width)
  return { version, lines: next.slice(0, KEPT_LINES) }
}
