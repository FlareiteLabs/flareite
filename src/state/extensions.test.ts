import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ExtensionStatus } from '../ipc/extensions'

vi.mock('../ipc/extensions', () => ({
  listExtensions: vi.fn(),
  runExtensionCommand: vi.fn(),
  setExtensionPermissions: vi.fn(),
  restartExtension: vi.fn(),
}))
vi.mock('../ipc/workspace', () => ({ setTheme: vi.fn(), setIconTheme: vi.fn() }))

const ipc = await import('../ipc/extensions')
const workspaceIpc = await import('../ipc/workspace')
const { getCommand, executeCommand } = await import('../commands/registry')
const { activeEditor } = await import('../editor/activeEditor')
const { notifications } = await import('./notifications')
const {
  applyStatuses,
  contributedMenuItems,
  extensionsStore,
  patternMatcher,
  showExtensionFailure,
  showExtensionMessage,
} = await import('./extensions')
const { reportedProblems } = await import('./problems')

function extension(overrides: Partial<ExtensionStatus> = {}): ExtensionStatus {
  return {
    id: 'example.tidy',
    name: 'Tidy',
    version: '0.1.0',
    description: '',
    path: '/ext/tidy',
    development: false,
    state: 'active',
    error: null,
    permissions: [
      {
        permission: 'documents-read',
        description: 'Read open documents',
        granted: true,
        decided: true,
      },
    ],
    commands: [{ id: 'example.tidy.trim', title: 'Trim', category: 'Tidy', hidden: false }],
    configErrors: [],
    hasSettings: false,
    author: null,
    hasIcon: false,
    hasReadme: false,
    contributions: {
      themes: [],
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
    ...overrides,
  }
}

describe('extensions state', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    notifications.set([])
    reportedProblems.set([])
    applyStatuses([])
  })

  it('registers the commands of usable extensions in the palette', async () => {
    applyStatuses([extension()])
    expect(extensionsStore.get()).toHaveLength(1)
    expect(getCommand('example.tidy.trim')).toMatchObject({ title: 'Trim', category: 'Tidy' })

    activeEditor.set({ documentId: 4 } as never)
    await executeCommand('example.tidy.trim')
    expect(ipc.runExtensionCommand).toHaveBeenCalledWith('example.tidy.trim', 4, null)
    activeEditor.set(null)

    applyStatuses([extension({ state: 'disabled' })])
    expect(getCommand('example.tidy.trim')).toBeUndefined()
  })

  it('offers menu items for the files and folders they declare', () => {
    const menu = (command: string, forFiles: boolean, forFolders: boolean, patterns: string[]) => ({
      location: 'explorer/context' as const,
      command,
      group: '',
      forFiles,
      forFolders,
      patterns,
      view: null,
      contexts: [],
      inline: false,
      icon: null,
      when: null,
    })
    applyStatuses([
      extension({
        commands: ['any', 'folder', 'markdown'].map(id => ({
          id: `example.tidy.${id}`,
          title: id,
          category: 'Tidy',
          hidden: false,
        })),
        contributions: {
          ...extension().contributions,
          menus: [
            menu('example.tidy.any', true, true, []),
            menu('example.tidy.folder', false, true, []),
            menu('example.tidy.markdown', true, false, ['*.{md,markdown}', 'README']),
          ],
        },
      }),
    ])
    const titles = (path: string | null, directory: boolean) =>
      contributedMenuItems('explorer/context', { path, directory }).map(item => item.title)
    expect(titles('/w/src', true)).toEqual(['any', 'folder'])
    expect(titles('/w/notes.MD', false)).toEqual(['any', 'markdown'])
    expect(titles('C:\\w\\README', false)).toEqual(['any', 'markdown'])
    expect(titles('/w/main.rs', false)).toEqual(['any'])
    expect(titles(null, false)).toEqual(['any'])
    expect(contributedMenuItems('editor/context', { path: '/w/a.md', directory: false })).toEqual(
      [],
    )
  })

  it('matches file names against patterns', () => {
    const matches = (pattern: string, name: string) => patternMatcher(pattern).test(name)
    expect(matches('*.ts', 'index.ts')).toBe(true)
    expect(matches('*.ts', 'index.tsx')).toBe(false)
    expect(matches('*.{ts,tsx}', 'App.TSX')).toBe(true)
    expect(matches('?.txt', 'a.txt')).toBe(true)
    expect(matches('?.txt', 'ab.txt')).toBe(false)
    // Other characters are literal.
    expect(matches('a+b(1).txt', 'a+b(1).txt')).toBe(true)
    expect(matches('a.b', 'axb')).toBe(false)
    expect(matches('x,y', 'x,y')).toBe(true)
  })

  it('offers contributed icon themes in the palette', async () => {
    applyStatuses([
      extension({
        contributions: {
          ...extension().contributions,
          iconThemes: [{ id: 'example.tidy.colorful', label: 'Colorful' }],
        },
      }),
    ])
    const command = getCommand('preferences.iconTheme.example.tidy.colorful')
    expect(command).toMatchObject({ title: 'File Icon Theme: Colorful', category: 'Preferences' })
    await executeCommand('preferences.iconTheme.example.tidy.colorful')
    expect(workspaceIpc.setIconTheme).toHaveBeenCalledWith('example.tidy.colorful')
  })

  it('asks once for permissions an extension is waiting for', async () => {
    const waiting = extension({
      id: 'example.waiting',
      state: 'awaitingPermission',
      permissions: [
        {
          permission: 'documents-write',
          description: 'Edit open documents',
          granted: false,
          decided: false,
        },
      ],
    })
    applyStatuses([waiting])
    applyStatuses([waiting])
    const prompts = notifications.get()
    expect(prompts).toHaveLength(1)
    expect(prompts[0]?.message).toContain('Tidy wants to: edit open documents')
    expect(prompts[0]?.sticky).toBe(true)
    const allow = prompts[0]?.actions?.find(action => action.label === 'Allow')
    await allow?.run()
    expect(ipc.setExtensionPermissions).toHaveBeenCalledWith('example.waiting', ['documents-write'])
  })

  it('offers to restart an extension that stopped', async () => {
    showExtensionFailure('example.tidy', 'Tidy', 'crashed')
    const [failure] = notifications.get()
    expect(failure).toMatchObject({ kind: 'error' })
    expect(failure?.message).toBe('The extension Tidy stopped: crashed')
    await failure?.actions?.find(action => action.label === 'Restart')?.run()
    expect(ipc.restartExtension).toHaveBeenCalledWith('example.tidy')
    // Problems lists it from its status instead.
    expect(reportedProblems.get()).toEqual([])
  })

  it('keeps warnings extensions show in Problems, under the extension', () => {
    showExtensionMessage('example.tidy', 'Tidy', 'warning', 'Line 3 is long')
    showExtensionMessage('example.tidy', 'Tidy', 'info', 'Done')
    expect(notifications.get().map(n => n.message)).toEqual(['Tidy: Line 3 is long', 'Tidy: Done'])
    expect(reportedProblems.get().map(p => [p.source, p.extension, p.message])).toEqual([
      ['Tidy', 'example.tidy', 'Line 3 is long'],
    ])
  })
})
