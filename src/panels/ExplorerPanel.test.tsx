import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { DirectoryEntry } from '../ipc/workspace'
import { dragFilesTo, pressToDrag } from '../test/drag'

vi.mock('../ipc/workspace', () => ({
  listDirectory: vi.fn(),
  createEntry: vi.fn(),
  renameEntry: vi.fn(),
  moveEntries: vi.fn(),
  copyEntries: vi.fn(),
  copyDropped: vi.fn(),
  deleteEntries: vi.fn(),
  revealEntry: vi.fn(),
  writeClipboardText: vi.fn(),
  getWorkspace: vi.fn(),
  openWorkspaceDialog: vi.fn(),
  closeWorkspace: vi.fn(),
}))
vi.mock('../ipc/editor', () => ({ openDocument: vi.fn() }))

const ipc = await import('../ipc/workspace')
const editorIpc = await import('../ipc/editor')
const { CommandError } = await import('../ipc/index')
const { setWorkspace, toggleFolder } = await import('../state/workspace')
const { documentsStore } = await import('../state/documents')
const { DEFAULT_LAYOUT, layoutStore } = await import('../state/layout')
const { notifications } = await import('../state/notifications')
const { iconThemeStore } = await import('../state/iconTheme')
const { extensionColorOverrides, themeKind } = await import('../themes/theme')
const { fileMarksStore } = await import('../state/fileMarks')
const { ExplorerActions, ExplorerPanel } = await import('./ExplorerPanel')
const { explorerStore } = await import('./explorerState')
const { contextMenuStore } = await import('../components/contextMenuState')
const { fileProblemsStore } = await import('../state/problems')
const { applyStatuses } = await import('../state/extensions')

const entry = (path: string, directory = false, ignored = false): DirectoryEntry => ({
  name: path.split('/').pop() ?? path,
  path,
  directory,
  ignored,
})

let listings: Record<string, DirectoryEntry[]>

describe('ExplorerPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    layoutStore.set(DEFAULT_LAYOUT)
    explorerStore.set({
      focused: null,
      creating: null,
      renaming: null,
      clipboard: null,
      revealed: null,
    })
    notifications.set([])
    listings = {
      '/w': [entry('/w/src', true), entry('/w/target', true, true), entry('/w/README.md')],
      '/w/src': [entry('/w/src/main.rs')],
    }
    vi.mocked(ipc.listDirectory).mockImplementation(async path => listings[path] ?? [])
    vi.mocked(ipc.createEntry).mockImplementation(async (parent, name, directory) => {
      const path = `${parent}/${name}`
      listings[parent] = [...(listings[parent] ?? []), entry(path, directory)]
      return { path, directories: [parent] }
    })
    vi.mocked(editorIpc.openDocument).mockImplementation(async path => ({
      id: 1,
      title: path.split('/').pop() ?? path,
      path,
      dirty: false,
      lineEnding: 'LF',
      encoding: 'UTF-8',
      encodingId: 'utf-8',
    }))
    documentsStore.set({ documents: [], activeId: null })
    fileProblemsStore.set({ files: [], leftOut: 0 })
    vi.mocked(ipc.renameEntry).mockImplementation(async (path, name) => {
      const parent = path.slice(0, path.lastIndexOf('/'))
      const renamed = `${parent}/${name}`
      listings[parent] = (listings[parent] ?? []).map(e =>
        e.path === path ? entry(renamed, e.directory) : e,
      )
      return { path: renamed, directories: [parent] }
    })
    vi.mocked(ipc.moveEntries).mockImplementation(async (paths, target) =>
      paths.map(path => `${target}/${path.split('/').pop()}`),
    )
    vi.mocked(ipc.copyEntries).mockImplementation(async (paths, target) =>
      paths.map(path => `${target}/${path.split('/').pop()} copy`),
    )
    vi.mocked(ipc.deleteEntries).mockImplementation(async paths => {
      for (const [folder, entries] of Object.entries(listings)) {
        listings[folder] = entries.filter(e => !paths.includes(e.path))
      }
      return true
    })
  })

  /** The menu of a row, opened by right-clicking it. */
  function menuOf(row: HTMLElement) {
    fireEvent.contextMenu(row)
    const menu = contextMenuStore.get()
    act(() => contextMenuStore.set(null))
    return menu?.entries ?? []
  }
  const labels = (entries: ReturnType<typeof menuOf>) => entries.map(item => item?.label ?? null)
  const action = (entries: ReturnType<typeof menuOf>, label: string) => {
    const found = entries.find(item => item?.label === label)
    if (!found) {
      throw new Error(`no ${label} in ${labels(entries).join(', ')}`)
    }
    return found
  }

  async function renderTree() {
    render(
      <>
        <ExplorerActions />
        <ExplorerPanel />
      </>,
    )
    await act(async () => setWorkspace({ root: '/w', name: 'w', trust: 'trusted' }))
    return screen.getByRole('tree', { name: 'w' })
  }

  it('offers to open a folder when none is open', () => {
    render(<ExplorerPanel />)
    act(() => setWorkspace(null))
    expect(screen.getByRole('button', { name: 'Open Folder…' })).toBeInTheDocument()
  })

  it('lists the root and marks ignored entries', async () => {
    const tree = await renderTree()
    const items = within(tree).getAllByRole('treeitem')
    expect(items.map(item => item.textContent)).toEqual(['▸src', '▸target', 'README.md'])
    expect(items[1]).toHaveAttribute('data-ignored')
  })

  it('marks everything inside an ignored folder as ignored', async () => {
    const user = userEvent.setup()
    // A rule can match inside an ignored folder without un-ignoring it.
    listings['/w/target'] = [entry('/w/target/debug', true), entry('/w/target/keep.txt')]
    const tree = await renderTree()
    await user.click(within(tree).getByText('target'))
    expect((await within(tree).findByText('keep.txt')).closest('li')).toHaveAttribute(
      'data-ignored',
    )
    expect(within(tree).getByText('debug').closest('li')).toHaveAttribute('data-ignored')
    expect(within(tree).getByText('src').closest('li')).not.toHaveAttribute('data-ignored')
  })

  it('marks files with problems and the folders that contain them', async () => {
    const user = userEvent.setup()
    const problem = (severity: 1 | 2 | 3) => ({
      start: { line: 0, column: 0 },
      end: { line: 0, column: 1 },
      severity,
      message: 'm',
      source: null,
    })
    fileProblemsStore.set({
      files: [
        {
          path: '/w/src/main.rs',
          document: null,
          title: 'main.rs',
          problems: [problem(1), problem(2)],
        },
        {
          path: '/w/README.md',
          document: 1,
          title: 'README.md',
          problems: [problem(2), problem(3)],
        },
      ],
      leftOut: 0,
    })
    const tree = await renderTree()
    const row = (name: string) => within(tree).getByText(name).closest('li')

    expect(row('src')).toHaveAttribute('data-problems', 'error')
    expect(row('src')).toHaveAttribute('aria-description', 'Contains 1 error, 1 warning')
    expect(row('target')).not.toHaveAttribute('data-problems')
    expect(row('README.md')).toHaveAttribute('data-problems', 'warning')
    expect(row('README.md')).toHaveAttribute('aria-description', '1 warning')
    expect(row('README.md')).toHaveAttribute('title', '/w/README.md\n1 warning')
    expect(row('README.md')?.querySelector('.explorer-tree__problems')).toHaveTextContent('1')

    await user.click(within(tree).getByText('src'))
    await within(tree).findByText('main.rs')
    expect(row('main.rs')).toHaveAttribute('data-problems', 'error')
    expect(row('main.rs')?.querySelector('.explorer-tree__problems')).toHaveTextContent('2')

    // Fixed: the marks go.
    act(() => fileProblemsStore.set({ files: [], leftOut: 0 }))
    expect(row('src')).not.toHaveAttribute('data-problems')
    expect(row('main.rs')?.querySelector('.explorer-tree__problems')).toBeNull()
  })

  it("shows extensions' marks in their colors, which problems override", async () => {
    const user = userEvent.setup()
    const color = { description: '', dark: '#e2c08d', light: '#895503' }
    applyStatuses([
      {
        id: 'flareite.git',
        name: 'Git',
        state: 'active',
        permissions: [],
        commands: [],
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
          colors: [
            { ...color, name: 'modified', id: 'flareite.git.modified' },
            { ...color, name: 'untracked', id: 'flareite.git.untracked', dark: '#73c991' },
          ],
        },
      } as never,
    ])
    const mark = (path: string, badge: string, name: string, inherit = false) => ({
      path,
      badge,
      color: `flareite.git.${name}`,
      tooltip: name === 'modified' ? 'Modified' : 'Untracked',
      faded: false,
      inherit,
      propagate: true,
      priority: name === 'modified' ? 3 : 1,
    })
    fileMarksStore.set([
      {
        extension: 'flareite.git',
        marks: [mark('/w/src/main.rs', 'M', 'modified'), mark('/w/target', 'U', 'untracked', true)],
      },
    ])
    themeKind.set('dark')
    try {
      const tree = await renderTree()
      const row = (name: string) => within(tree).getByText(name).closest('li')
      // The folder holding a change takes its color, with a dot.
      expect(row('src')).toHaveAttribute('data-marked')
      expect(row('src')?.style.getPropertyValue('--explorer-mark-color')).toBe('#e2c08d')
      expect(row('src')?.querySelector('.explorer-tree__mark')).toHaveTextContent('●')
      expect(row('src')).toHaveAttribute('aria-description', 'Contains: Modified')
      expect(row('README.md')).not.toHaveAttribute('data-marked')

      await user.click(within(tree).getByText('src'))
      await within(tree).findByText('main.rs')
      expect(row('main.rs')?.querySelector('.explorer-tree__mark')).toHaveTextContent('M')
      expect(row('main.rs')).toHaveAttribute('aria-description', 'Modified')

      // Everything in an untracked folder is untracked; a theme can change the color.
      listings['/w/target'] = [entry('/w/target/out.txt', false, true)]
      await user.click(within(tree).getByText('target'))
      await within(tree).findByText('out.txt')
      expect(row('out.txt')?.querySelector('.explorer-tree__mark')).toHaveTextContent('U')
      act(() => extensionColorOverrides.set({ 'flareite.git.untracked': '#00ff00' }))
      expect(row('out.txt')?.style.getPropertyValue('--explorer-mark-color')).toBe('#00ff00')

      // A problem's color wins, and both badges show.
      act(() =>
        fileProblemsStore.set({
          files: [
            {
              path: '/w/src/main.rs',
              document: null,
              title: 'main.rs',
              problems: [
                {
                  start: { line: 0, column: 0 },
                  end: { line: 0, column: 1 },
                  severity: 1,
                  message: 'm',
                  source: null,
                },
              ],
            },
          ],
          leftOut: 0,
        }),
      )
      expect(row('main.rs')).toHaveAttribute('data-problems', 'error')
      expect(row('main.rs')?.querySelector('.explorer-tree__badges')).toHaveTextContent('1M')
      expect(row('main.rs')).toHaveAttribute('aria-description', '1 error. Modified')
    } finally {
      fileMarksStore.set([])
      extensionColorOverrides.set({})
      applyStatuses([])
    }
  })

  it('expands folders lazily and opens files', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(within(tree).getByText('src'))
    expect(await within(tree).findByText('main.rs')).toBeInTheDocument()
    expect(ipc.listDirectory).toHaveBeenCalledWith('/w/src')
    expect(layoutStore.get().expanded).toEqual(['/w/src'])

    await user.click(within(tree).getByText('README.md'))
    expect(editorIpc.openDocument).toHaveBeenCalledWith('/w/README.md', false)
  })

  it('navigates with the keyboard', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    tree.focus()
    await user.keyboard('{ArrowRight}')
    expect(await within(tree).findByText('main.rs')).toBeInTheDocument()
    await user.keyboard('{ArrowDown}')
    expect(within(tree).getByText('main.rs').closest('li')).toHaveAttribute('data-focused')
    await user.keyboard('{ArrowLeft}')
    expect(within(tree).getByText('src').closest('li')).toHaveAttribute('data-focused')
    await user.keyboard('{ArrowLeft}')
    expect(within(tree).queryByText('main.rs')).not.toBeInTheDocument()
  })

  it("opens the focused file's menu from the keyboard", async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    tree.focus()
    await user.keyboard('{End}')
    fireEvent.contextMenu(tree)
    expect(contextMenuStore.get()?.entries[0]).toMatchObject({ label: 'Open' })
  })

  it('reveals the active file: expands its folders, selects it, and scrolls to it', async () => {
    const scrolled = vi.spyOn(Element.prototype, 'scrollIntoView')
    const tree = await renderTree()
    expect(within(tree).queryByText('main.rs')).not.toBeInTheDocument()

    // As when Go to Definition opens a file in a collapsed folder.
    const summary = {
      id: 3,
      title: 'main.rs',
      path: '/w/src/main.rs',
      dirty: false,
      lineEnding: 'LF',
      encoding: 'UTF-8',
      encodingId: 'utf-8',
    }
    await act(async () => documentsStore.set({ documents: [summary], activeId: 3 }))
    const row = (await within(tree).findByText('main.rs')).closest('li')
    expect(layoutStore.get().expanded).toEqual(['/w/src'])
    expect(row).toHaveAttribute('aria-selected', 'true')
    expect(row).toHaveAttribute('data-focused')
    expect(scrolled.mock.contexts).toContain(row)

    // Collapsing it afterwards sticks: only a change of file reveals again.
    scrolled.mockClear()
    act(() => toggleFolder('/w/src'))
    await act(async () =>
      documentsStore.update(state => ({ ...state, documents: [{ ...summary, dirty: true }] })),
    )
    expect(layoutStore.get().expanded).toEqual([])
    expect(scrolled).not.toHaveBeenCalled()
    act(() => documentsStore.set({ documents: [], activeId: null }))
    scrolled.mockRestore()
  })

  it('shows folder and file icons', async () => {
    const tree = await renderTree()
    const kinds = within(tree)
      .getAllByRole('treeitem')
      .map(item => item.querySelector('svg')?.dataset.kind)
    expect(kinds).toEqual(['folder', 'folder', 'file'])
  })

  it("shows the icon theme's images where it has them", async () => {
    const none = {
      file: null,
      folder: null,
      folderExpanded: null,
      fileExtensions: new Map<string, string>(),
      fileNames: new Map<string, string>(),
      folderNames: new Map<string, string>(),
      folderNamesExpanded: new Map<string, string>(),
    }
    iconThemeStore.set({
      id: 'example.icons.colorful',
      icons: new Map([
        ['src', 'data:image/svg+xml;base64,c3Jj'],
        ['md', 'data:image/svg+xml;base64,bWQ='],
      ]),
      associations: { ...none, folderNames: new Map([['src', 'src']]) },
      light: { ...none, fileExtensions: new Map([['md', 'md']]) },
    })
    try {
      const tree = await renderTree()
      const icons = within(tree)
        .getAllByRole('treeitem')
        .map(item => item.querySelector('.explorer-tree__icon'))
      expect(icons[0]).toHaveAttribute('src', 'data:image/svg+xml;base64,c3Jj')
      expect(icons[0]).toHaveAttribute('alt', '')
      expect(icons[1]?.tagName).toBe('svg')
      // The light section only applies with a light color theme.
      expect(icons[2]?.tagName).toBe('svg')
      act(() => themeKind.set('light'))
      expect(within(tree).getByText('README.md').previousElementSibling).toHaveAttribute(
        'src',
        'data:image/svg+xml;base64,bWQ=',
      )
    } finally {
      act(() => {
        iconThemeStore.set(null)
        themeKind.set('dark')
      })
    }
  })

  it('creates a file at the root from the header and opens it', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(screen.getByRole('button', { name: 'New File' }))
    const input = within(tree).getByRole('textbox', { name: 'File name' })
    expect(input).toHaveFocus()
    expect(input.closest('li')).toHaveAttribute('aria-level', '1')

    await user.type(input, 'notes.md{Enter}')
    expect(ipc.createEntry).toHaveBeenCalledWith('/w', 'notes.md', false)
    expect(await within(tree).findByText('notes.md')).toBeInTheDocument()
    expect(within(tree).queryByRole('textbox')).not.toBeInTheDocument()
    expect(within(tree).getByText('notes.md').closest('li')).toHaveAttribute('data-focused')
    expect(editorIpc.openDocument).toHaveBeenCalledWith('/w/notes.md', false)
  })

  it("creates a folder inside a folder from the folder's menu", async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    fireEvent.contextMenu(within(tree).getByText('src'))
    const menu = contextMenuStore.get()
    expect(menu?.entries.map(item => item?.label).slice(0, 2)).toEqual(['New File…', 'New Folder…'])
    await act(async () => menu?.entries[1]?.run())

    expect(layoutStore.get().expanded).toEqual(['/w/src'])
    const input = await within(tree).findByRole('textbox', { name: 'Folder name' })
    expect(input.closest('li')).toHaveAttribute('aria-level', '2')
    await user.type(input, 'net{Enter}')
    expect(ipc.createEntry).toHaveBeenCalledWith('/w/src', 'net', true)
    expect(await within(tree).findByText('net')).toBeInTheDocument()
    expect(tree).toHaveFocus()
    expect(editorIpc.openDocument).not.toHaveBeenCalled()
  })

  it("creates beside the focused file and offers creation in a file's menu", async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(within(tree).getByText('src'))
    expect(labels(menuOf(await within(tree).findByText('main.rs')))).toEqual([
      'Open',
      null,
      'New File…',
      'New Folder…',
      'Open Containing Folder',
      null,
      'Cut',
      'Copy',
      'Paste',
      null,
      'Copy Path',
      'Copy Relative Path',
      null,
      'Rename…',
      'Delete',
    ])

    await user.click(screen.getByRole('button', { name: 'New File' }))
    await user.type(within(tree).getByRole('textbox'), 'lib.rs{Enter}')
    expect(ipc.createEntry).toHaveBeenCalledWith('/w/src', 'lib.rs', false)
  })

  it('creates at the top level from the space below the rows', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(within(tree).getByText('src'))
    await user.click(await within(tree).findByText('main.rs'))
    const blank = document.querySelector('.explorer__blank')
    expect(blank).not.toBeNull()
    fireEvent.contextMenu(blank as Element)
    const menu = contextMenuStore.get()
    expect(menu?.entries.map(item => item?.label ?? null)).toEqual([
      'New File…',
      'New Folder…',
      'Open Containing Folder',
      null,
      'Paste',
    ])
    await act(async () => menu?.entries[0]?.run())

    const input = within(tree).getByRole('textbox', { name: 'File name' })
    expect(input.closest('li')).toHaveAttribute('aria-level', '1')
    await user.type(input, 'top.txt{Enter}')
    expect(ipc.createEntry).toHaveBeenCalledWith('/w', 'top.txt', false)
  })

  it('keeps the field open with the problem when creating fails', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    vi.mocked(ipc.createEntry).mockRejectedValueOnce(
      new CommandError('invalidName', `".." can't be used as a name.`),
    )
    await user.click(screen.getByRole('button', { name: 'New Folder' }))
    const input = within(tree).getByRole('textbox')
    await user.type(input, '..{Enter}')
    expect(await within(tree).findByRole('alert')).toHaveTextContent("can't be used as a name")
    expect(input).toHaveAttribute('aria-invalid', 'true')
    expect(input).toHaveFocus()

    await user.keyboard('{Escape}')
    expect(within(tree).queryByRole('textbox')).not.toBeInTheDocument()
    expect(tree).toHaveFocus()
    expect(notifications.get()).toEqual([])
  })

  it('warns about a name that is taken and cancels when left', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(screen.getByRole('button', { name: 'New File' }))
    await user.type(within(tree).getByRole('textbox'), 'readme.md')
    expect(within(tree).getByRole('alert')).toHaveTextContent(
      'A file or folder named README.md already exists here.',
    )
    await user.click(document.body)
    expect(within(tree).queryByRole('textbox')).not.toBeInTheDocument()
    expect(ipc.createEntry).not.toHaveBeenCalled()
  })

  it('renames in place with F2, selecting the name without its extension', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    tree.focus()
    await user.keyboard('{End}{F2}')
    const input = within(tree).getByRole<HTMLInputElement>('textbox', {
      name: 'Rename README.md',
    })
    expect(input).toHaveFocus()
    expect([input.selectionStart, input.selectionEnd]).toEqual([0, 'README'.length])

    // Another entry's name is taken, but its own in another case isn't.
    await user.clear(input)
    await user.type(input, 'SRC')
    expect(within(tree).getByRole('alert')).toHaveTextContent('named src already exists')
    await user.clear(input)
    await user.type(input, 'readme.md{Enter}')
    expect(ipc.renameEntry).toHaveBeenCalledWith('/w/README.md', 'readme.md')
    const renamed = (await within(tree).findByText('readme.md')).closest('li')
    expect(renamed).toHaveAttribute('data-focused')
    expect(tree).toHaveFocus()
  })

  it('keeps a renamed folder expanded and cancels renaming with Escape', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(within(tree).getByText('src'))
    await within(tree).findByText('main.rs')
    listings['/w/lib'] = listings['/w/src'] ?? []
    act(() => action(menuOf(within(tree).getByText('src')), 'Rename…').run())
    const input = within(tree).getByRole<HTMLInputElement>('textbox', { name: 'Rename src' })
    expect([input.selectionStart, input.selectionEnd]).toEqual([0, 3])
    await user.keyboard('{Escape}')
    expect(within(tree).queryByRole('textbox')).not.toBeInTheDocument()
    expect(ipc.renameEntry).not.toHaveBeenCalled()

    act(() => action(menuOf(within(tree).getByText('src')), 'Rename…').run())
    await user.keyboard('{Control>}a{/Control}lib{Enter}')
    expect(ipc.renameEntry).toHaveBeenCalledWith('/w/src', 'lib')
    await within(tree).findByText('lib')
    expect(layoutStore.get().expanded).toEqual(['/w/lib'])
    expect(await within(tree).findByText('main.rs')).toBeInTheDocument()
  })

  it('deletes with Delete, or permanently with Shift+Delete', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    tree.focus()
    await user.keyboard('{End}{Delete}')
    expect(ipc.deleteEntries).toHaveBeenCalledWith(['/w/README.md'], false)
    // The row before takes the focus.
    await waitFor(() =>
      expect(within(tree).getByText('target').closest('li')).toHaveAttribute('data-focused'),
    )
    await user.keyboard('{Shift>}{Delete}{/Shift}')
    expect(ipc.deleteEntries).toHaveBeenLastCalledWith(['/w/target'], true)

    // From the menu; cancelling in the dialog changes nothing.
    vi.mocked(ipc.deleteEntries).mockResolvedValueOnce(false)
    await act(async () => action(menuOf(within(tree).getByText('src')), 'Delete').run())
    expect(ipc.deleteEntries).toHaveBeenLastCalledWith(['/w/src'], false)
  })

  it('cuts and pastes to move, and copies and pastes to copy', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(within(tree).getByText('src'))
    const main = (await within(tree).findByText('main.rs')).closest('li') as HTMLElement
    expect(action(menuOf(main), 'Paste').disabled).toBe(true)

    tree.focus()
    await user.keyboard('{Control>}x{/Control}')
    expect(main).toHaveAttribute('data-cut')
    // Pasting into the open folder, from the menu below the rows.
    fireEvent.contextMenu(document.querySelector('.explorer__blank') as Element)
    const paste = contextMenuStore.get()?.entries.find(item => item?.label === 'Paste')
    expect(paste?.disabled).toBe(false)
    await act(async () => paste?.run())
    expect(ipc.moveEntries).toHaveBeenCalledWith(['/w/src/main.rs'], '/w')
    expect(main).not.toHaveAttribute('data-cut')

    // Copying and pasting beside the original makes a copy.
    await act(async () => action(menuOf(within(tree).getByText('README.md')), 'Copy').run())
    await act(async () => action(menuOf(within(tree).getByText('README.md')), 'Paste').run())
    expect(ipc.copyEntries).toHaveBeenCalledWith(['/w/README.md'], '/w')
  })

  it('copies paths and shows entries in the file manager', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(within(tree).getByText('src'))
    const main = await within(tree).findByText('main.rs')
    await act(async () => action(menuOf(main), 'Copy Path').run())
    expect(ipc.writeClipboardText).toHaveBeenLastCalledWith('/w/src/main.rs')
    await act(async () => action(menuOf(main), 'Copy Relative Path').run())
    expect(ipc.writeClipboardText).toHaveBeenLastCalledWith('src/main.rs')
    await act(async () => action(menuOf(main), 'Open Containing Folder').run())
    expect(ipc.revealEntry).toHaveBeenCalledWith('/w/src/main.rs')

    // The same from the keyboard.
    tree.focus()
    await user.keyboard('{Shift>}{Alt>}c{/Alt}{/Shift}')
    expect(ipc.writeClipboardText).toHaveBeenLastCalledWith('/w/src/main.rs')
  })

  it('moves by dragging onto a folder, and copies with Ctrl held', async () => {
    const tree = await renderTree()
    let readme = within(tree).getByText('README.md').closest('li') as HTMLElement
    const src = within(tree).getByText('src').closest('li') as HTMLElement

    const drag = pressToDrag(readme)
    drag.moveTo({ element: src, x: 10, y: 40 })
    expect(src).toHaveAttribute('data-drop-target')
    // Dropping where it already is does nothing: no target there.
    drag.moveTo({ element: readme, x: 10, y: 80 })
    expect(readme).not.toHaveAttribute('data-drop-target')
    drag.moveTo({ element: src, x: 10, y: 40 })
    await act(async () => drag.drop())
    expect(ipc.moveEntries).toHaveBeenCalledWith(['/w/README.md'], '/w/src')
    expect(src).not.toHaveAttribute('data-drop-target')

    // Without Ctrl, dropping where it is does nothing. (Moving it expanded
    // src, so its row is a new element.)
    readme = within(tree).getAllByText('README.md')[0]!.closest('li') as HTMLElement
    const again = pressToDrag(readme)
    again.moveTo({ element: readme, x: 10, y: 80 })
    await act(async () => again.drop())
    expect(ipc.moveEntries).toHaveBeenCalledTimes(1)
    const copy = pressToDrag(readme)
    copy.moveTo({ element: readme, x: 10, y: 80, ctrlKey: true })
    expect(tree).toHaveAttribute('data-drop-target')
    await act(async () => copy.drop())
    expect(ipc.copyEntries).toHaveBeenCalledWith(['/w/README.md'], '/w')
  })

  it('copies files dropped from another program into the folder under them', async () => {
    vi.mocked(ipc.copyDropped).mockResolvedValue(['/w/src/notes.txt'])
    const tree = await renderTree()
    const src = within(tree).getByText('src').closest('li') as HTMLElement

    dragFilesTo({ element: src, x: 10, y: 40 }, { files: ['C:\elsewhere\notes.txt'], dropId: 4 })
    await act(async () => {})
    expect(ipc.copyDropped).toHaveBeenCalledWith(4, '/w/src')
    expect(src).not.toHaveAttribute('data-drop-target')
  })

  it("offers extensions' items for the files and folders they're for", async () => {
    const menu = (command: string, forFolders: boolean, patterns: string[]) => ({
      location: 'explorer/context' as const,
      command,
      group: '',
      forFiles: !forFolders,
      forFolders,
      patterns,
      view: null,
      contexts: [],
      inline: false,
      icon: null,
      when: null,
    })
    applyStatuses([
      {
        id: 'example.fmt',
        name: 'Fmt',
        state: 'active',
        permissions: [],
        commands: [
          { id: 'example.fmt.all', title: 'Format All', category: 'Fmt', hidden: false },
          { id: 'example.fmt.md', title: 'Format Markdown', category: 'Fmt', hidden: false },
        ],
        contributions: {
          themes: [],
          iconThemes: [],
          keybindings: [],
          menus: [menu('example.fmt.all', true, []), menu('example.fmt.md', false, ['*.md'])],
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
      } as never,
    ])
    try {
      const tree = await renderTree()
      const extensionItems = (name: string) =>
        labels(menuOf(within(tree).getByText(name))).filter(label => label?.startsWith('Format'))
      expect(extensionItems('src')).toEqual(['Format All'])
      expect(extensionItems('README.md')).toEqual(['Format Markdown'])
      expect(extensionItems('target')).toEqual(['Format All'])
    } finally {
      applyStatuses([])
    }
  })

  it('creates the entry when the field is left with a new name', async () => {
    const user = userEvent.setup()
    const tree = await renderTree()
    await user.click(screen.getByRole('button', { name: 'New File' }))
    await user.type(within(tree).getByRole('textbox'), 'todo.txt')
    await user.click(document.body)
    expect(ipc.createEntry).toHaveBeenCalledWith('/w', 'todo.txt', false)
    expect(await within(tree).findByText('todo.txt')).toBeInTheDocument()
  })
})
