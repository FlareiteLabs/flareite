/**
 * Rows drawn from the editor's copy of the lines (`lineStore.ts`) where its
 * view has none yet (ADR 0055). They're plain HTML, built as one string and
 * set at once, so they can be drawn in the frame a scroll happens without a
 * React render. They show the text and its highlights only; selections,
 * diagnostics, indent guides, and the like appear once the view covers
 * those rows.
 */
import type { DisplayLine } from '../ipc/editor'
import { segments } from './segments'

export interface FillRow {
  line: DisplayLine
  /** The document line, for its number in the gutter. */
  number: number
  /** The row's top, in pixels from the top of the content. */
  top: number
}

/** The rows' HTML for the text and for the gutter, given the gutter's label for a line. */
export function fillHtml(
  rows: readonly FillRow[],
  label: (line: number) => string,
): { text: string; gutter: string } {
  let text = ''
  let gutter = ''
  for (const { line, number, top } of rows) {
    text += `<div class="editor-view__line" data-line="${number}" style="top:${top}px">${lineHtml(line)}</div>`
    // The fold marker's room keeps the numbers where the gutter's are.
    gutter +=
      `<div class="editor-view__line-number" data-line="${number}" style="top:${top}px">${label(number)}` +
      `<span class="editor-view__fold"></span></div>`
  }
  return { text, gutter }
}

function lineHtml({ text, spans }: DisplayLine): string {
  if (spans.length === 0) {
    return escapeHtml(text)
  }
  let html = ''
  for (const { start, end, className } of segments(text, spans)) {
    const part = escapeHtml(text.slice(start, end))
    html += className ? `<span class="${className}">${part}</span>` : part
  }
  return html
}

function escapeHtml(text: string): string {
  return text.replace(/[&<>]/g, char => (char === '&' ? '&amp;' : char === '<' ? '&lt;' : '&gt;'))
}
