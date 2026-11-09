import { afterEach, describe, expect, it } from 'vitest'
import type { ExtensionStatus } from '../ipc/extensions'
import type { FrontendSettings } from '../ipc/workspace'
import { applySettings } from './appearance'
import { extensionsStore } from './extensions'

const settings = (theme: string, colors: Record<string, string> = {}): FrontendSettings => ({
  theme,
  iconTheme: 'default',
  colors,
  keybindings: {},
  fontSize: 14,
  fontFamily: '',
  tabSize: 4,
  terminalScrollback: 1000,
  terminalScreenReader: false,
  minimapRegionHeaders: true,
  minimapMarkHeaders: true,
})

const themes = {
  id: 'example.themes',
  name: 'Themes',
  version: '1.0.0',
  description: '',
  path: '',
  development: false,
  state: 'ready',
  error: null,
  permissions: [],
  commands: [],
  configErrors: [],
  hasSettings: false,
  author: null,
  hasIcon: false,
  hasReadme: false,
  contributions: {
    themes: [
      { id: 'example.themes.paper', label: 'Paper', kind: 'light', colors: { text: '#111111' } },
    ],
    iconThemes: [],
    keybindings: [],
    menus: [],
    views: [],
    decorations: [],
    previews: [],
    viewers: [],
    layouts: [],
    languages: [],
    languageServers: [],
    programs: [],
    colors: [],
    viewContainers: [],
    textProviders: [],
    quickDiff: null,
  },
  storedBytes: 0,
  build: null,
  usage: {
    busyMs: 0,
    calls: 0,
    slowestCallMs: 0,
    memoryBytes: 0,
    peakMemoryBytes: 0,
    droppedEvents: 0,
    compileMs: 0,
    activationMs: null,
  },
} satisfies ExtensionStatus

describe('appearance', () => {
  afterEach(() => {
    extensionsStore.set([])
    applySettings(settings('dark'))
  })

  it('applies a contributed theme once its extension is loaded, with user colors on top', () => {
    const root = document.documentElement
    applySettings(settings('example.themes.paper', { 'editor-background': '#fafafa' }))
    expect(root.style.getPropertyValue('--color-text')).toBe('')

    extensionsStore.set([themes])
    expect(root.dataset.theme).toBe('light')
    expect(root.style.getPropertyValue('--color-text')).toBe('#111111')
    expect(root.style.getPropertyValue('--color-editor-background')).toBe('#fafafa')

    extensionsStore.set([{ ...themes, state: 'disabled' }])
    expect(root.style.getPropertyValue('--color-text')).toBe('')
  })
})
