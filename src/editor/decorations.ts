/**
 * Drawing extension decorations. Each decorated range becomes rectangles in
 * the overlay layer (background, border, underline) and, for decoration
 * types that change the text itself (color, bold, italic), styled runs in
 * the line. Colors come from validated decoration types; they are checked
 * again here and used only as CSS colors.
 */
import type { CSSProperties } from 'react'
import type { DecorationView } from '../ipc/editor'
import type { DecorationColors, DecorationType } from '../ipc/extensions'
import { THEME_TOKENS, type ThemeKind } from '../themes/theme'
import type { TextOverlay } from './segments'

const HEX_COLOR = /^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i

/** A decoration type's color as CSS: a hex color, or a theme token's variable (API 1.15). */
export const color = (value: string | null): string | undefined =>
  value && HEX_COLOR.test(value)
    ? value
    : value && THEME_TOKENS.has(value)
      ? `var(--color-${value})`
      : undefined

export function colorsFor(type: DecorationType, kind: ThemeKind): DecorationColors {
  return kind === 'light' ? type.light : type.dark
}

/** Style of an overlay rectangle for a decoration type. */
export function decorationRectStyle(type: DecorationType, kind: ThemeKind): CSSProperties {
  const colors = colorsFor(type, kind)
  const style: Record<string, string> = {}
  const background = color(colors.background)
  const border = color(colors.border)
  const underline = color(colors.underlineColor) ?? color(colors.color)
  if (background) {
    style['--decoration-background'] = background
  }
  if (border) {
    style['--decoration-border'] = border
  }
  if (type.underline && underline) {
    style['--decoration-underline'] = underline
  }
  return style as CSSProperties
}

/** The background of lines a whole-line decoration type covers, if it has one. */
export function wholeLineBackground(type: DecorationType, kind: ThemeKind): string | undefined {
  return type.wholeLine ? color(colorsFor(type, kind).background) : undefined
}

/**
 * Backgrounds of whole lines from whole-line decorations, by line: a range
 * ending at the start of a line doesn't cover it.
 */
export function lineBackgrounds(
  decorations: readonly DecorationView[],
  types: ReadonlyMap<string, DecorationType>,
  kind: ThemeKind,
): Map<number, string> {
  const byLine = new Map<number, string>()
  for (const decoration of decorations) {
    const type = types.get(decoration.key)
    const background = type && wholeLineBackground(type, kind)
    if (!background) {
      continue
    }
    const { start, end } = decoration
    const last = end.line > start.line && end.column === 0 ? end.line - 1 : end.line
    for (let line = start.line; line <= last; line++) {
      byLine.set(line, background)
    }
  }
  return byLine
}

/** Whether a decoration type styles the text itself. */
function textStyle(type: DecorationType, kind: ThemeKind): CSSProperties | null {
  const style: CSSProperties = {}
  const text = color(colorsFor(type, kind).color)
  if (text) {
    style.color = text
  }
  if (type.bold) {
    style.fontWeight = 'bold'
  }
  if (type.italic) {
    style.fontStyle = 'italic'
  }
  return Object.keys(style).length > 0 ? style : null
}

/**
 * Text overlays per line for decorations that style text, for the lines
 * `lineNumbers` (with lengths `lineLengths`, in UTF-16 units).
 */
export function textOverlays(
  decorations: readonly DecorationView[],
  types: ReadonlyMap<string, DecorationType>,
  kind: ThemeKind,
  lineNumbers: readonly number[],
  lineLengths: readonly number[],
): Map<number, TextOverlay[]> {
  const byLine = new Map<number, TextOverlay[]>()
  for (const decoration of decorations) {
    const type = types.get(decoration.key)
    const style = type && textStyle(type, kind)
    if (!style) {
      continue
    }
    lineNumbers.forEach((line, index) => {
      if (line < decoration.start.line || line > decoration.end.line) {
        return
      }
      const length = lineLengths[index] ?? 0
      const from = line === decoration.start.line ? decoration.start.column : 0
      const to = line === decoration.end.line ? decoration.end.column : length
      if (to > from) {
        const overlays = byLine.get(line) ?? []
        overlays.push({ from, to, style })
        byLine.set(line, overlays)
      }
    })
  }
  return byLine
}
