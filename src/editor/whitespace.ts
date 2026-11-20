/**
 * Drawing whitespace and indentation (ADR 0054). Both are drawn as
 * backgrounds, so the text and its measuring stay as they are: whitespace
 * as spans (dots for spaces, an arrow for each tab) through the line's
 * overlays, and indent guides as thin lines in the line's own background.
 */
import type { CSSProperties } from 'react'
import type { SelectionView } from '../ipc/editor'
import type { FrontendSettings } from '../ipc/workspace'
import type { TextOverlay } from './segments'

export type RenderWhitespace = FrontendSettings['renderWhitespace']

const SPACES = 'editor-view__ws'
const TAB = 'editor-view__ws editor-view__ws-tab'

/** Runs of spaces and tabs in `text`, as `[start, end)`. */
function runs(text: string): [number, number][] {
  const found: [number, number][] = []
  const pattern = /[ \t]+/g
  for (let match = pattern.exec(text); match; match = pattern.exec(text)) {
    found.push([match.index, match.index + match[0].length])
  }
  return found
}

/** Overlays drawing `text[start, end)`'s whitespace: a span per run of spaces, and per tab. */
function drawn(text: string, start: number, end: number, into: TextOverlay[]): void {
  let run = start
  for (let at = start; at <= end; at++) {
    const ch = at < end ? text[at] : undefined
    if (ch !== ' ') {
      if (at > run) {
        into.push({ from: run, to: at, className: SPACES })
      }
      if (ch === '\t') {
        into.push({ from: at, to: at + 1, className: TAB })
      }
      run = at + 1
    }
  }
}

/**
 * The overlays that draw `text`'s whitespace as `mode` says, given the
 * selections' parts on the line (`selected`, UTF-16 `[start, end)`).
 */
export function whitespaceOverlays(
  text: string,
  mode: RenderWhitespace,
  selected: readonly [number, number][] = [],
): TextOverlay[] {
  if (mode === 'none') {
    return []
  }
  const overlays: TextOverlay[] = []
  for (const [start, end] of runs(text)) {
    switch (mode) {
      case 'all':
        drawn(text, start, end, overlays)
        break
      case 'trailing':
        if (end === text.length) {
          drawn(text, start, end, overlays)
        }
        break
      case 'boundary': {
        // All but a single space between two words.
        const single = end - start === 1 && text[start] === ' ' && start > 0 && end < text.length
        if (!single) {
          drawn(text, start, end, overlays)
        }
        break
      }
      case 'selection':
        for (const [from, to] of selected) {
          const a = Math.max(start, from)
          const b = Math.min(end, to)
          if (a < b) {
            drawn(text, a, b, overlays)
          }
        }
        break
    }
  }
  return overlays
}

/** The parts of `line` (of `length`) the selections cover, in UTF-16 columns. */
export function selectedOnLine(
  selections: readonly SelectionView[],
  line: number,
  length: number,
): [number, number][] {
  const parts: [number, number][] = []
  for (const { anchor, head } of selections) {
    const [start, end] =
      anchor.line < head.line || (anchor.line === head.line && anchor.column <= head.column)
        ? [anchor, head]
        : [head, anchor]
    if (start.line > line || end.line < line) {
      continue
    }
    const from = start.line === line ? start.column : 0
    const to = end.line === line ? end.column : length
    if (from < to) {
      parts.push([from, to])
    }
  }
  return parts
}

/** How deep `text` is indented, in columns, tabs going to tab stops. `null` for a blank line. */
export function indentColumns(text: string, tabSize: number): number | null {
  let columns = 0
  for (const ch of text) {
    if (ch === ' ') {
      columns += 1
    } else if (ch === '\t') {
      columns += tabSize - (columns % tabSize)
    } else {
      return columns
    }
  }
  return null
}

/**
 * How deep each of `texts` (lines in order) is indented, for guides. A
 * blank line takes the deeper of the lines around it, so guides run
 * through blank lines in a block.
 */
export function guideColumns(texts: readonly string[], tabSize: number): number[] {
  const own = texts.map(text => indentColumns(text, tabSize))
  const above: number[] = []
  let last = 0
  for (const columns of own) {
    above.push(columns ?? last)
    last = columns ?? last
  }
  const result = new Array<number>(own.length)
  let next = 0
  for (let index = own.length - 1; index >= 0; index--) {
    const columns = own[index]
    result[index] = columns ?? Math.max(above[index]!, next)
    next = columns ?? next
  }
  return result
}

/**
 * The line's background drawing its indent guides: a thin line at each
 * level an indentation of `columns` passes (0, a tab, two tabs, ...), on
 * its first row only.
 */
export function guideStyle(columns: number, tabSize: number): CSSProperties | undefined {
  const levels = Math.ceil(columns / tabSize)
  if (levels <= 0) {
    return undefined
  }
  const layers = Array.from({ length: levels }, () => GUIDE)
  return {
    backgroundImage: layers.join(', '),
    backgroundPosition: Array.from({ length: levels }, (_, level) => `${level * tabSize}ch 0`).join(
      ', ',
    ),
    backgroundSize: layers.map(() => '1px var(--editor-line-height)').join(', '),
    backgroundRepeat: 'no-repeat',
  }
}

const GUIDE = 'linear-gradient(var(--color-editor-indent-guide), var(--color-editor-indent-guide))'
