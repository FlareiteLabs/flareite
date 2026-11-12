/**
 * DOM measurement for the editor view. A rendered line's text is split
 * across text nodes (plain runs and highlight spans), optionally followed by
 * a badge for shortened lines that is not part of the text. A UTF-16 column
 * is an offset into the concatenation of those text nodes.
 *
 * A wrapped line is an element with one block per row (`.editor-view__row`),
 * each holding that row's text: a column is measured in the row it's on (a
 * column at a break is on the row the break starts).
 */
import type {
  DecorationView,
  DiagnosticView,
  LineWrap,
  Position,
  Range,
  SelectionView,
} from '../ipc/editor'
import { indexAtOrAfter, lineIndex, segmentOf, segmentRange } from './rows'

/** Width of the selection highlight shown for a selected line break. */
const LINE_BREAK_WIDTH = 6

export interface Rect {
  top: number
  left: number
  width: number
}

export interface Overlay {
  carets: (Rect & { primary: boolean })[]
  selections: Rect[]
  /** Find matches; `current` marks the match the primary selection covers. */
  matches: (Rect & { current: boolean })[]
  diagnostics: (Rect & { severity: number })[]
  /** Extension decorations; `key` names the decoration type. */
  decorations: (Rect & { key: string })[]
}

export const EMPTY_OVERLAY: Overlay = {
  carets: [],
  selections: [],
  matches: [],
  diagnostics: [],
  decorations: [],
}

/** Marks elements inside a line that are not part of its text. */
const NOT_TEXT = '.editor-view__cut, .editor-view__folded'
/** A row of a wrapped line. */
export const ROW_CLASS = 'editor-view__row'

/** The text nodes of a line element, in order. */
function textNodes(line: Element): Text[] {
  const nodes: Text[] = []
  const walker = line.ownerDocument.createTreeWalker(line, NodeFilter.SHOW_TEXT)
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    if (!node.parentElement?.closest(NOT_TEXT)) {
      nodes.push(node as Text)
    }
  }
  return nodes
}

/** The length of a line element's text, in UTF-16 units. */
export function lineLength(line: Element): number {
  return textNodes(line).reduce((length, node) => length + node.length, 0)
}

/** The room before an element's text: a wrapped row's indent. */
function startX(element: Element): number {
  if (!element.classList.contains(ROW_CLASS)) {
    return 0
  }
  const view = element.ownerDocument.defaultView
  return view ? parseFloat(view.getComputedStyle(element).paddingLeft) || 0 : 0
}

/** Horizontal offset of `column` from the start of a line (or row) element. */
export function columnX(line: Element, column: number): number {
  if (column <= 0) {
    return startX(line)
  }
  const nodes = textNodes(line)
  // The node containing `column`, or the end of the last one.
  let remaining = column
  let target: Text | undefined
  for (const node of nodes) {
    target = node
    if (remaining <= node.length) {
      break
    }
    remaining -= node.length
  }
  if (!target) {
    return startX(line)
  }
  const range = line.ownerDocument.createRange()
  range.setStart(target, 0)
  range.setEnd(target, Math.min(remaining, target.length))
  if (typeof range.getBoundingClientRect !== 'function') {
    return 0
  }
  return range.getBoundingClientRect().right - line.getBoundingClientRect().left
}

/** The column under a horizontal client coordinate within a line (or row) element. */
export function columnAt(line: Element, clientX: number, clientY: number): number {
  const nodes = textNodes(line)
  if (nodes.length === 0 || clientX <= line.getBoundingClientRect().left + startX(line)) {
    return 0
  }
  const doc = line.ownerDocument
  const position = doc.caretPositionFromPoint?.(clientX, clientY)
  const range = position ? null : doc.caretRangeFromPoint?.(clientX, clientY)
  const node = position?.offsetNode ?? range?.startContainer
  const offset = position?.offset ?? range?.startOffset ?? 0
  let column = 0
  for (const text of nodes) {
    if (text === node) {
      return column + offset
    }
    column += text.length
  }
  return column
}

/** The element of row `segment` of a line element: the line itself unless it wraps. */
export function rowElement(line: Element, segment: number): Element | undefined {
  const rows = [...line.children].filter(child => child.classList.contains(ROW_CLASS))
  return rows.length === 0 ? line : rows[Math.min(segment, rows.length - 1)]
}

/**
 * Where `column` of a line element is: its row (0 for the first) and its
 * horizontal offset from the line's start.
 */
export function caretPoint(
  line: Element,
  wrap: LineWrap | undefined,
  column: number,
): { segment: number; x: number } {
  const segment = segmentOf(wrap, column)
  const row = rowElement(line, segment) ?? line
  const [start] = segmentRange(wrap, segment, Number.MAX_SAFE_INTEGER)
  return { segment, x: columnX(row, column - start) }
}

/** The column at a point within row `segment` of a line element. */
export function columnAtPoint(
  line: Element,
  wrap: LineWrap | undefined,
  segment: number,
  clientX: number,
  clientY: number,
): number {
  const row = rowElement(line, segment) ?? line
  const [start] = segmentRange(wrap, segment, Number.MAX_SAFE_INTEGER)
  return start + columnAt(row, clientX, clientY)
}

/** The rendered lines: which document lines they are, the rows they start on, and their wraps. */
export interface MeasuredLines {
  lineNumbers: readonly number[]
  /** The row each line starts on, from the first; see `rowStartsOf`. */
  rowStarts: readonly number[]
  wrapped: ReadonlyMap<number, LineWrap>
}

/**
 * Carets and selection rectangles for the rendered lines, positioned
 * relative to the first row.
 */
export function measureOverlay(
  lines: HTMLElement,
  shown: MeasuredLines,
  selections: readonly SelectionView[],
  matches: readonly Range[],
  lineHeight: number,
  diagnostics: readonly DiagnosticView[] = [],
  decorations: readonly DecorationView[] = [],
): Overlay {
  const { lineNumbers, rowStarts, wrapped } = shown
  const count = Math.min(lines.children.length, lineNumbers.length)
  const indexOf = (line: number) => {
    const index = lineIndex(lineNumbers, line)
    return index >= 0 && index < count ? index : -1
  }
  /** The top of row `segment` of the `index`th line. */
  const topOf = (index: number, segment: number) =>
    ((rowStarts[index] ?? index) + segment) * lineHeight
  /** A position's place: its top and left, if its line is rendered. */
  const pointOf = (position: Position) => {
    const index = indexOf(position.line)
    const element = index >= 0 ? lines.children[index] : undefined
    if (!element) {
      return null
    }
    const { segment, x } = caretPoint(element, wrapped.get(position.line), position.column)
    return { top: topOf(index, segment), left: x }
  }
  const overlay: Overlay = {
    carets: [],
    selections: [],
    matches: [],
    diagnostics: [],
    decorations: [],
  }

  /** Rectangles covering `start..end`, one per rendered row. */
  const rectsFor = (from: Position, to: Position): Rect[] => {
    const [start, end] = ordered(from, to)
    const rects: Rect[] = []
    for (let index = indexAtOrAfter(lineNumbers, start.line); index < count; index++) {
      const line = lineNumbers[index]!
      if (line > end.line) {
        break
      }
      const element = lines.children[index]
      if (!element) {
        continue
      }
      const wrap = wrapped.get(line)
      const length = lineLength(element)
      const first = line === start.line ? start.column : 0
      const last = line === end.line ? end.column : length
      const segments = (wrap?.breaks.length ?? 0) + 1
      for (let segment = segmentOf(wrap, first); segment < segments; segment++) {
        const [segmentStart, segmentEnd] = segmentRange(wrap, segment, length)
        if (segment > 0 && last < segmentStart) {
          break
        }
        const row = rowElement(element, segment) ?? element
        const left = columnX(row, Math.max(first, segmentStart) - segmentStart)
        const lastSegment = segment === segments - 1
        let right: number
        if (last < segmentEnd || (lastSegment && line === end.line)) {
          right = columnX(row, Math.min(last, segmentEnd) - segmentStart)
        } else {
          right = columnX(row, segmentEnd - segmentStart) + (lastSegment ? LINE_BREAK_WIDTH : 0)
        }
        if (right > left) {
          rects.push({ top: topOf(index, segment), left, width: right - left })
        }
      }
    }
    return rects
  }

  let primary: SelectionView | undefined
  for (const selection of selections) {
    if (selection.primary) {
      primary = selection
    }
    const head = pointOf(selection.head)
    if (head) {
      overlay.carets.push({ ...head, width: 0, primary: selection.primary })
    }
    overlay.selections.push(...rectsFor(selection.anchor, selection.head))
  }

  for (const match of matches) {
    const current =
      primary !== undefined &&
      samePosition(...ordered(primary.anchor, primary.head), match.start, match.end)
    overlay.matches.push(...rectsFor(match.start, match.end).map(rect => ({ ...rect, current })))
  }

  for (const { start, end, severity } of diagnostics) {
    const rects = rectsFor(start, end)
    const point = rects.length === 0 ? pointOf(start) : null
    if (point) {
      // An empty range (e.g. "expected `;`") still gets a short mark.
      rects.push({ ...point, width: LINE_BREAK_WIDTH })
    }
    overlay.diagnostics.push(...rects.map(rect => ({ ...rect, severity })))
  }

  for (const { start, end, key } of decorations) {
    overlay.decorations.push(...rectsFor(start, end).map(rect => ({ ...rect, key })))
  }
  return overlay
}

function samePosition(a: Position, b: Position, c: Position, d: Position): boolean {
  return a.line === c.line && a.column === c.column && b.line === d.line && b.column === d.column
}

function ordered(a: Position, b: Position): [Position, Position] {
  return a.line < b.line || (a.line === b.line && a.column <= b.column) ? [a, b] : [b, a]
}
