/**
 * What the editor's input textarea contains, for screen readers.
 *
 * The textarea receives keyboard and IME input. Screen readers read an edit
 * field's text, so it holds the primary cursor's line (or the lines of the
 * primary selection) with the same selection as the editor: moving the
 * cursor reads the new line, character, or word, and selecting reads the
 * selected text. The textarea is placed over its first line, so its text
 * lines up with the rendered text and IME candidate windows appear at the
 * cursor.
 *
 * The textarea never edits itself: typed text is applied to the document,
 * and the mirror follows the view the backend returns.
 */
import type { EditorView, Position } from '../ipc/editor'
import { indexAtOrAfter, lineIndex, segmentOf, segmentRange } from './rows'

/** At most this many lines (around the cursor) are mirrored. */
export const MIRROR_MAX_LINES = 100
/** Lines longer than this aren't mirrored, so the text stays aligned. */
const MIRROR_MAX_LINE_LENGTH = 10_000

export interface InputMirror {
  /** The first mirrored line. */
  line: number
  /**
   * The row of `line` the mirror starts on: when the cursor's line wraps,
   * only the row the cursor is on is mirrored, so the text lines up.
   */
  segment: number
  value: string
  selectionStart: number
  selectionEnd: number
  direction: 'forward' | 'backward'
}

const before = (a: Position, b: Position) =>
  a.line < b.line || (a.line === b.line && a.column < b.column)

/**
 * The mirror for `view`, or `null` when the cursor's line isn't in the
 * view or is too long. A selection across a folded region mirrors only the
 * cursor's line.
 */
export function inputMirror(view: EditorView): InputMirror | null {
  const primary = view.selections.find(selection => selection.primary)
  if (!primary) {
    return null
  }
  const { anchor, head } = primary
  const headRow = lineIndex(view.lineNumbers, head.line)
  if (headRow < 0) {
    return null
  }
  const reach = Math.floor(MIRROR_MAX_LINES / 2)
  let firstRow = indexAtOrAfter(
    view.lineNumbers,
    Math.max(Math.min(anchor.line, head.line), head.line - reach),
  )
  let lastRow =
    indexAtOrAfter(
      view.lineNumbers,
      Math.min(Math.max(anchor.line, head.line), head.line + reach) + 1,
    ) - 1
  const wraps = (index: number) => view.wrapped.has(view.lineNumbers[index]!)
  if (
    view.lineNumbers[lastRow]! - view.lineNumbers[firstRow]! !== lastRow - firstRow ||
    view.lineNumbers.slice(firstRow, lastRow + 1).some((_, i) => wraps(firstRow + i))
  ) {
    firstRow = lastRow = headRow
  }
  const first = view.lineNumbers[firstRow]!
  const last = view.lineNumbers[lastRow]!
  const lines = view.lines.slice(firstRow, lastRow + 1)
  if (lines.some(line => line.fullLength !== null || line.text.length > MIRROR_MAX_LINE_LENGTH)) {
    return null
  }
  const texts = lines.map(line => line.text)
  const value = texts.join('\n')
  const offsetOf = (position: Position) => {
    if (position.line < first) {
      return 0
    }
    if (position.line > last) {
      return value.length
    }
    let offset = 0
    for (let line = first; line < position.line; line++) {
      offset += texts[line - first]!.length + 1
    }
    return offset + Math.min(position.column, texts[position.line - first]!.length)
  }
  const backward = before(head, anchor)
  const [start, end] = backward ? [head, anchor] : [anchor, head]
  const wrap = view.wrapped.get(head.line)
  if (wrap) {
    // Only the cursor's row of its wrapped line.
    const text = texts[0]!
    const segment = segmentOf(wrap, head.column)
    const [from, to] = segmentRange(wrap, segment, text.length)
    const clamp = (position: Position) =>
      position.line < head.line
        ? 0
        : position.line > head.line
          ? to - from
          : Math.max(0, Math.min(position.column, to) - from)
    return {
      line: head.line,
      segment,
      value: text.slice(from, to),
      selectionStart: clamp(start),
      selectionEnd: clamp(end),
      direction: backward ? 'backward' : 'forward',
    }
  }
  return {
    line: first,
    segment: 0,
    value,
    selectionStart: offsetOf(start),
    selectionEnd: offsetOf(end),
    direction: backward ? 'backward' : 'forward',
  }
}

/**
 * The text typed into a textarea that held `mirror`: what replaced the
 * mirrored selection in `value`, or `null` if `value` isn't such an edit.
 * For input the browser applied itself (when `beforeinput` couldn't be
 * cancelled).
 */
export function typedText(mirror: InputMirror, value: string): string | null {
  const prefix = mirror.value.slice(0, mirror.selectionStart)
  const suffix = mirror.value.slice(mirror.selectionEnd)
  if (
    value.length < prefix.length + suffix.length ||
    !value.startsWith(prefix) ||
    !value.endsWith(suffix)
  ) {
    return null
  }
  return value.slice(prefix.length, value.length - suffix.length)
}
