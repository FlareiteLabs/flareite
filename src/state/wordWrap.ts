/**
 * Where editors wrap lines: `editor.word_wrap` and its companions, and
 * **View: Toggle Word Wrap**, which turns wrapping on or off for a document
 * over the setting, until Flareite restarts (ADR 0052).
 */
import type { WrapOptions } from '../ipc/editor'
import type { FrontendSettings } from '../ipc/workspace'
import { appearance } from './appearance'
import { createStore } from './store'

/** Documents whose wrapping View: Toggle Word Wrap turned on (`true`) or off. */
export const wrapOverrides = createStore<ReadonlyMap<number, boolean>>(new Map())

type WrapSettings = Pick<FrontendSettings, 'wordWrap' | 'wordWrapColumn' | 'wrappingIndent'>

/** Whether a document's editors wrap, as the settings and the toggle say. */
export function wraps(settings: WrapSettings, override: boolean | undefined): boolean {
  return override ?? settings.wordWrap !== 'off'
}

/** Turns wrapping on or off for a document, whichever it isn't. */
export function toggleWordWrap(documentId: number): void {
  const overrides = wrapOverrides.get()
  const next = new Map(overrides)
  next.set(documentId, !wraps(appearance.get(), overrides.get(documentId)))
  wrapOverrides.set(next)
}

/** Narrowest width wrapped at, in columns, however narrow the editor. */
const MIN_COLUMNS = 10

/**
 * Where an editor wraps: `null` for not at all. `fit` is how many columns
 * fit in its width (`null` if not known yet).
 */
export function wrapFor(
  settings: WrapSettings,
  override: boolean | undefined,
  fit: number | null,
): WrapOptions | null {
  if (!wraps(settings, override)) {
    return null
  }
  // Turned on over `off`: at the editor's width.
  const mode = settings.wordWrap === 'off' ? 'on' : settings.wordWrap
  const indent = settings.wrappingIndent === 'deep_indent' ? 'deepIndent' : settings.wrappingIndent
  const fitting = fit === null ? null : Math.max(MIN_COLUMNS, fit)
  switch (mode) {
    case 'column':
      return { columns: settings.wordWrapColumn, indent }
    case 'bounded':
      return {
        columns:
          fitting === null ? settings.wordWrapColumn : Math.min(fitting, settings.wordWrapColumn),
        indent,
      }
    default:
      return fitting === null ? null : { columns: fitting, indent }
  }
}

/** Whether lines wrap at the editor's width, so nothing scrolls sideways. */
export function fitsWidth(settings: WrapSettings, override: boolean | undefined): boolean {
  return wraps(settings, override) && settings.wordWrap !== 'column'
}
