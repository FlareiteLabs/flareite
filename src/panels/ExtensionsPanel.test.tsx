import { act, fireEvent, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ExtensionStatus } from '../ipc/extensions'

vi.mock('../ipc/extensions', () => ({
  buildExtension: vi.fn(() => Promise.resolve()),
  restartExtension: vi.fn(() => Promise.resolve()),
  setExtensionEnabled: vi.fn(() => Promise.resolve()),
  setExtensionPermissions: vi.fn(() => Promise.resolve()),
  getExtensionIcon: vi.fn(() => Promise.resolve('data:image/svg+xml;base64,PHN2Zy8+')),
  getExtensionReadme: vi.fn(),
  getExtensionReadmeImage: vi.fn(),
  listExtensions: vi.fn(() => new Promise(() => {})),
}))

vi.mock('../ipc/extensionInstall', () => ({
  getExtensionCatalog: vi.fn(() => Promise.resolve([])),
  installExtension: vi.fn(() => Promise.resolve(null)),
  listExtensionReleases: vi.fn(),
  listExtensionSources: vi.fn(() => Promise.resolve([])),
  uninstallExtension: vi.fn(() => Promise.resolve(false)),
  checkExtensionUpdates: vi.fn(() => Promise.resolve([])),
}))

const ipc = await import('../ipc/extensions')
const install = await import('../ipc/extensionInstall')
const { catalogStore, sourcesStore } = await import('../state/extensionInstall')
const { extensionsStore } = await import('../state/extensions')
const { setPageHandler } = await import('../state/viewRequests')
const { contextMenuStore } = await import('../components/contextMenuState')
const { ExtensionsPanel, rowTooltip } = await import('./ExtensionsPanel')
const { ExtensionPage } = await import('./ExtensionPage')

const tidy: ExtensionStatus = {
  id: 'example.tidy',
  name: 'Tidy',
  version: '0.1.0',
  description: 'Flags trailing whitespace and overlong lines, and trims trailing whitespace.',
  author: 'Flareite',
  hasIcon: true,
  hasReadme: true,
  path: '/ext/tidy',
  development: true,
  state: 'active',
  error: null,
  permissions: [
    {
      permission: 'documents-read',
      description: 'Read open documents',
      granted: true,
      decided: true,
    },
    {
      permission: 'documents-write',
      description: 'Edit open documents',
      granted: false,
      decided: true,
    },
  ],
  commands: [{ id: 'example.tidy.trim', title: 'Trim', category: 'Tidy', hidden: false }],
  configErrors: ['width must be at least 0; using the default'],
  hasSettings: false,
  contributions: {
    themes: [{ id: 'example.tidy.night', label: 'Night', kind: 'dark', colors: {} }],
    iconThemes: [],
    keybindings: [],
    menus: [],
    views: [],
    decorations: [],
    previews: [],
    viewers: [],
    layouts: [],
    linkHandlers: [],
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
    busyMs: 3.42,
    calls: 12,
    slowestCallMs: 1.2,
    memoryBytes: 1153434,
    peakMemoryBytes: 0,
    droppedEvents: 0,
    compileMs: 30,
    activationMs: 42.4,
  },
}

/** Tidy installed from GitHub (its own path, so its icon loads separately). */
const installedTidy: ExtensionStatus = { ...tidy, development: false, path: '/installed/tidy' }

const broken: ExtensionStatus = {
  ...tidy,
  id: '/ext/broken',
  name: 'broken',
  version: '',
  description: '',
  author: null,
  hasIcon: false,
  hasReadme: false,
  state: 'invalid',
  error: 'requires extension API version 2',
  permissions: [],
  commands: [],
  configErrors: [],
  development: false,
  usage: { ...tidy.usage, calls: 0, activationMs: null },
}

describe('ExtensionsPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    contextMenuStore.set(null)
    catalogStore.set({ state: 'idle' })
    sourcesStore.set({})
  })

  it('lists the official extensions that are not installed, to install', async () => {
    const user = userEvent.setup()
    vi.mocked(install.getExtensionCatalog).mockResolvedValue([
      {
        repo: 'FlareiteLabs/flareite-git-ext',
        id: 'flareite.git',
        name: 'Git',
        description: 'Source control.',
        author: 'Flareite',
        version: '0.5.0',
        tag: 'v0.5.0',
      },
      {
        repo: 'FlareiteLabs/flareite-tidy-ext',
        id: 'example.tidy',
        name: 'Tidy',
        description: 'Installed already.',
        author: 'Flareite',
        version: '0.1.0',
        tag: 'v0.1.0',
      },
    ])
    extensionsStore.set([installedTidy])
    render(<ExtensionsPanel />)
    const official = await screen.findByRole('list', { name: 'Official extensions to install' })
    const rows = within(official).getAllByRole('listitem')
    expect(rows).toHaveLength(1)
    expect(rows[0]).toHaveTextContent('Git0.5.0Source control.Flareite')
    expect(install.getExtensionCatalog).toHaveBeenCalledWith(false)
    await user.click(within(official).getByRole('button', { name: 'Install Git' }))
    expect(install.installExtension).toHaveBeenCalledWith('FlareiteLabs/flareite-git-ext', 'v0.5.0')
  })

  it('says why the official extensions could not be listed, and tries again', async () => {
    const user = userEvent.setup()
    vi.mocked(install.getExtensionCatalog).mockRejectedValueOnce(new Error('Offline.'))
    extensionsStore.set([installedTidy])
    render(<ExtensionsPanel />)
    expect(await screen.findByRole('alert')).toHaveTextContent('Offline.')
    await user.click(screen.getByRole('button', { name: 'Try Again' }))
    expect(install.getExtensionCatalog).toHaveBeenLastCalledWith(true)
  })

  it('marks updates, and offers updating, other versions, and uninstalling', async () => {
    const user = userEvent.setup()
    extensionsStore.set([installedTidy])
    sourcesStore.set({
      'example.tidy': {
        id: 'example.tidy',
        repository: 'FlareiteLabs/flareite-tidy-ext',
        tag: 'v0.1.0',
        update: 'v0.2.0',
      },
    })
    render(<ExtensionsPanel />)
    expect(screen.getByRole('button', { name: 'Tidy' })).toHaveTextContent('update')
    await user.click(screen.getByRole('button', { name: 'Manage Tidy' }))
    const labels = (contextMenuStore.get()?.entries ?? []).map(entry => entry?.label)
    expect(labels.slice(-3)).toEqual(['Update to v0.2.0', 'Install Another Version…', 'Uninstall…'])
    const updateEntry = contextMenuStore.get()?.entries.find(e => e?.label === 'Update to v0.2.0')
    act(() => void updateEntry?.run())
    expect(install.installExtension).toHaveBeenCalledWith(
      'FlareiteLabs/flareite-tidy-ext',
      'v0.2.0',
    )

    // A development extension is the developer's to remove.
    act(() => contextMenuStore.set(null))
    extensionsStore.set([{ ...installedTidy, development: true }])
    await user.click(screen.getByRole('button', { name: 'Manage Tidy' }))
    const devLabels = (contextMenuStore.get()?.entries ?? []).map(entry => entry?.label)
    expect(devLabels).not.toContain('Uninstall…')
  })

  it('explains how to add extensions when there are none', () => {
    extensionsStore.set([])
    render(<ExtensionsPanel />)
    expect(screen.getByText('No extensions are installed.')).toBeInTheDocument()
  })

  it('shows one row per extension: icon, name, description, author, and start time', async () => {
    extensionsStore.set([tidy, broken])
    render(<ExtensionsPanel />)
    const [row, other] = screen.getAllByRole('listitem')
    const open = within(row!).getByRole('button', { name: 'Tidy' })
    expect(open).toHaveTextContent('Tidy')
    expect(open).toHaveTextContent('dev')
    expect(open).toHaveTextContent('42 ms')
    expect(open).toHaveTextContent('Flareite')
    expect(open).toHaveAttribute('aria-description', tidy.description)
    // The tooltip has what the row shortens.
    expect(open.title).toBe(
      [
        'Tidy 0.1.0 (development)',
        'example.tidy · by Flareite',
        tidy.description,
        'Active · started in 42 ms',
      ].join('\n'),
    )
    expect(await within(row!).findByRole('presentation')).toHaveAttribute(
      'src',
      'data:image/svg+xml;base64,PHN2Zy8+',
    )
    // A broken extension says so, and has no icon to load.
    expect(within(other!).getByRole('button', { name: 'broken, Invalid' })).toHaveTextContent(
      'requires extension API version 2',
    )
    expect(ipc.getExtensionIcon).toHaveBeenCalledTimes(1)
  })

  it('opens the page of the extension chosen, and moves between rows with the arrow keys', async () => {
    const user = userEvent.setup()
    const shown: string[] = []
    setPageHandler(id => shown.push(id))
    extensionsStore.set([tidy, broken])
    render(<ExtensionsPanel />)
    await user.click(screen.getByRole('button', { name: 'Tidy' }))
    expect(shown).toEqual(['extension:example.tidy'])
    await user.keyboard('{ArrowDown}')
    expect(screen.getByRole('button', { name: 'broken, Invalid' })).toHaveFocus()
  })

  it('offers the actions in a manage menu', async () => {
    const user = userEvent.setup()
    extensionsStore.set([tidy])
    render(<ExtensionsPanel />)
    await user.click(screen.getByRole('button', { name: 'Manage Tidy' }))
    const entries = contextMenuStore.get()?.entries ?? []
    expect(entries.map(entry => entry?.label ?? null)).toEqual([
      'Show Details',
      null,
      'Disable',
      'Restart',
      null,
      'Show Log',
      null,
      'Copy Extension ID',
    ])
    await act(async () => entries.find(entry => entry?.label === 'Disable')?.run())
    expect(ipc.setExtensionEnabled).toHaveBeenCalledWith('example.tidy', false)

    act(() => contextMenuStore.set(null))
    fireEvent.contextMenu(screen.getByRole('button', { name: 'Tidy' }))
    expect(contextMenuStore.get()?.entries[0]?.label).toBe('Show Details')
  })

  it("shows a development extension's failed build, and offers building it", async () => {
    const user = userEvent.setup()
    const failed: ExtensionStatus = {
      ...tidy,
      build: {
        state: 'failed',
        error: 'cargo exited with code 101',
        durationMs: 900,
        log: 'example.tidy',
      },
    }
    extensionsStore.set([failed])
    render(<ExtensionsPanel />)
    const row = screen.getByRole('button', { name: 'Tidy, Build failed' })
    expect(row).toHaveTextContent('Build failed')
    expect(rowTooltip(failed)).toContain('Build failed: cargo exited with code 101')
    await user.click(screen.getByRole('button', { name: 'Manage Tidy' }))
    const entries = contextMenuStore.get()?.entries ?? []
    expect(entries.map(entry => entry?.label ?? null)).toContain('Build')
    await act(async () => entries.find(entry => entry?.label === 'Build')?.run())
    expect(ipc.buildExtension).toHaveBeenCalledWith('/ext/tidy')
  })

  it('says in the tooltip why an extension stopped', () => {
    expect(rowTooltip({ ...tidy, state: 'failed', error: 'trap: unreachable' })).toContain(
      'Stopped · started in 42 ms\ntrap: unreachable',
    )
  })
})

describe('ExtensionPage', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    vi.mocked(ipc.getExtensionReadme).mockResolvedValue({
      nodes: [
        { parent: null, type: 'heading', level: 1 },
        { parent: 0, type: 'text', text: 'Tidy keeps whitespace tidy' },
      ],
      highlights: {},
      highlightsPending: false,
    })
  })

  it('shows the README, details, permissions, commands, and problems', async () => {
    extensionsStore.set([tidy])
    render(<ExtensionPage id="example.tidy" />)
    const page = screen.getByRole('article', { name: 'Tidy' })
    expect(within(page).getByRole('heading', { level: 1, name: /Tidy/ })).toHaveTextContent(
      'Tidy0.1.0development',
    )
    expect(page).toHaveTextContent('Flareite · example.tidy')
    expect(page).toHaveTextContent('Active · started in 42 ms')
    const readme = within(page).getByRole('region', { name: 'README' })
    expect(await within(readme).findByText('Tidy keeps whitespace tidy')).toBeInTheDocument()
    expect(ipc.getExtensionReadme).toHaveBeenCalledWith('example.tidy')
    expect(page).toHaveTextContent('1 color theme')
    expect(page).toHaveTextContent('Ran 12 times for 3.4 ms in total')
    expect(page).toHaveTextContent('Compiled in 30 ms')
    expect(page).toHaveTextContent('/ext/tidy')
    expect(within(page).getByRole('button', { name: 'Trim' })).toBeEnabled()
    expect(page).toHaveTextContent('width must be at least 0')
  })

  it('changes permissions and turns the extension off', async () => {
    const user = userEvent.setup()
    extensionsStore.set([tidy])
    render(<ExtensionPage id="example.tidy" />)
    await user.click(screen.getByRole('checkbox', { name: 'Edit open documents' }))
    expect(ipc.setExtensionPermissions).toHaveBeenCalledWith('example.tidy', [
      'documents-read',
      'documents-write',
    ])
    await user.click(screen.getByRole('button', { name: 'Disable' }))
    expect(ipc.setExtensionEnabled).toHaveBeenCalledWith('example.tidy', false)
    await user.click(screen.getByRole('button', { name: 'Restart' }))
    expect(ipc.restartExtension).toHaveBeenCalledWith('example.tidy')
  })

  it("shows a development extension's build, builds it, and shows its log", async () => {
    const user = userEvent.setup()
    extensionsStore.set([
      {
        ...tidy,
        build: { state: 'succeeded', error: null, durationMs: 1400, log: 'example.tidy' },
      },
    ])
    render(<ExtensionPage id="example.tidy" />)
    expect(screen.getByRole('status')).toHaveTextContent('Built in 1.4 s')
    await user.click(screen.getByRole('button', { name: 'Build' }))
    expect(ipc.buildExtension).toHaveBeenCalledWith('/ext/tidy')
    expect(screen.getByRole('button', { name: 'Show Build Log' })).toBeEnabled()
  })

  it('offers building an invalid development extension, which may lack its build output', async () => {
    const user = userEvent.setup()
    extensionsStore.set([
      {
        ...broken,
        development: true,
        path: '/ext/broken',
        error: 'could not read /ext/broken/broken.wasm',
        build: { state: 'building', error: null, durationMs: null, log: 'example.broken' },
      },
    ])
    render(<ExtensionPage id="/ext/broken" />)
    expect(screen.getByRole('status')).toHaveTextContent('Building…')
    expect(screen.queryByRole('button', { name: 'Disable' })).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Build' }))
    expect(ipc.buildExtension).toHaveBeenCalledWith('/ext/broken')
  })

  it("shows a broken extension's problem, and that it has no README", () => {
    extensionsStore.set([broken])
    render(<ExtensionPage id="/ext/broken" />)
    expect(screen.getByRole('alert')).toHaveTextContent('requires extension API version 2')
    expect(screen.getByText('This extension has no README.')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Disable' })).not.toBeInTheDocument()
    expect(ipc.getExtensionReadme).not.toHaveBeenCalled()
  })

  it('names the languages an extension adds', () => {
    const server = { command: 'server', args: ['--stdio'], languages: ['html'], rootMarkers: [] }
    extensionsStore.set([
      {
        ...tidy,
        state: 'ready',
        contributions: {
          ...tidy.contributions,
          themes: [],
          languages: [
            { id: 'html', name: 'HTML', fileExtensions: ['html'], fileNames: [] },
            { id: 'css', name: 'CSS', fileExtensions: ['css'], fileNames: [] },
          ],
          languageServers: [
            { ...server, name: 'html' },
            { ...server, name: 'css' },
          ],
        },
      },
    ])
    render(<ExtensionPage id="example.tidy" />)
    expect(screen.getByText('Languages: HTML, CSS')).toBeInTheDocument()
    expect(screen.getByText('2 language servers')).toBeInTheDocument()
  })

  it('says so when the extension is gone', () => {
    extensionsStore.set([])
    render(<ExtensionPage id="example.tidy" />)
    expect(screen.getByText('This extension is no longer loaded.')).toBeInTheDocument()
  })
})
