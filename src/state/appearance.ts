/**
 * Settings from `settings.toml` that the frontend applies: theme (built-in
 * or contributed by an extension) and color overrides, file icon theme, editor font,
 * terminal options, and keybinding overrides. The editor's line height derives from the
 * font size.
 */
import { linkSettings } from './links'
import { setKeybindingOverrides } from '../commands/registry'
import type { FrontendSettings, Theme, WrappingIndent } from '../ipc/workspace'
import { applyTheme, followSystemTheme, type ThemeKind } from '../themes/theme'
import { extensionsStore, usableExtensions } from './extensions'
import { setActiveIconTheme } from './iconTheme'
import { createStore } from './store'

export interface Appearance {
  theme: Theme
  fontSize: number
  fontFamily: string
  tabSize: number
  /** Editor line height in pixels. */
  lineHeight: number
  terminalScrollback: number
  terminalScreenReader: boolean
  /** Which section headers the minimap labels. */
  minimapRegionHeaders: boolean
  minimapMarkHeaders: boolean
  /** Where lines wrap (see `state/wordWrap.ts`). */
  wordWrap: FrontendSettings['wordWrap']
  wordWrapColumn: number
  wrappingIndent: WrappingIndent
}

const lineHeightFor = (fontSize: number) => Math.round(fontSize * 1.45)

export const appearance = createStore<Appearance>({
  theme: 'system',
  fontSize: 14,
  fontFamily: '',
  tabSize: 4,
  lineHeight: lineHeightFor(14),
  terminalScrollback: 1000,
  terminalScreenReader: false,
  minimapRegionHeaders: true,
  minimapMarkHeaders: true,
  wordWrap: 'off',
  wordWrapColumn: 80,
  wrappingIndent: 'same',
})

let stopFollowingSystem: (() => void) | null = null
let current: FrontendSettings | null = null

/** A theme's base and colors: built in, or from an enabled extension. */
function resolveTheme(id: Theme): { kind: ThemeKind | 'system'; colors: Record<string, string> } {
  if (id === 'system' || id === 'dark' || id === 'light') {
    return { kind: id, colors: {} }
  }
  for (const extension of usableExtensions(extensionsStore.get())) {
    const theme = extension.contributions.themes.find(candidate => candidate.id === id)
    if (theme) {
      return { kind: theme.kind, colors: theme.colors }
    }
  }
  // Not loaded (yet): extensions load after settings, and this runs again.
  return { kind: 'system', colors: {} }
}

function applyColors(settings: FrontendSettings): void {
  const theme = resolveTheme(settings.theme)
  const colors = { ...theme.colors, ...settings.colors }
  stopFollowingSystem?.()
  stopFollowingSystem = null
  if (theme.kind === 'system') {
    stopFollowingSystem = followSystemTheme(colors)
  } else {
    applyTheme(theme.kind, colors)
  }
}

export function applySettings(settings: FrontendSettings): void {
  current = settings
  appearance.set({
    theme: settings.theme,
    fontSize: settings.fontSize,
    fontFamily: settings.fontFamily,
    tabSize: settings.tabSize,
    lineHeight: lineHeightFor(settings.fontSize),
    terminalScrollback: settings.terminalScrollback,
    terminalScreenReader: settings.terminalScreenReader,
    minimapRegionHeaders: settings.minimapRegionHeaders,
    minimapMarkHeaders: settings.minimapMarkHeaders,
    wordWrap: settings.wordWrap,
    wordWrapColumn: settings.wordWrapColumn,
    wrappingIndent: settings.wrappingIndent,
  })
  applyColors(settings)
  setActiveIconTheme(settings.iconTheme)
  setKeybindingOverrides(settings.keybindings)
  linkSettings.set({ web: settings.openWebLinks, file: settings.openFileLinks })

  const root = document.documentElement.style
  root.setProperty('--font-size-editor', `${settings.fontSize}px`)
  root.setProperty('--editor-line-height', `${lineHeightFor(settings.fontSize)}px`)
  root.setProperty('--editor-tab-size', String(settings.tabSize))
  if (settings.fontFamily.trim()) {
    root.setProperty('--font-mono', `${JSON.stringify(settings.fontFamily.trim())}, monospace`)
  } else {
    root.removeProperty('--font-mono')
  }
}

// A contributed theme may load (or be disabled) after settings are applied.
extensionsStore.subscribe(() => {
  if (current) {
    applyColors(current)
  }
})
