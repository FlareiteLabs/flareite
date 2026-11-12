import { act, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { registerCommands } from '../commands/registry'
import { activeEditor, type EditorController } from '../editor/activeEditor'
import * as lsp from '../ipc/lsp'
import { findFiles } from '../ipc/workspace'
import { openPath } from '../state/documents'
import { iconThemeStore } from '../state/iconTheme'
import { workspaceStore } from '../state/workspace'
import { Palette } from './Palette'
import { openPalette, openQuickPick, paletteStore } from './paletteState'

vi.mock('../ipc/workspace', async importOriginal => ({
  ...(await importOriginal<typeof import('../ipc/workspace')>()),
  findFiles: vi.fn(),
}))
vi.mock('../ipc/lsp', () => ({
  documentSymbols: vi.fn(),
  workspaceSymbols: vi.fn(),
  resolveSymbol: vi.fn(),
}))
vi.mock('../state/documents', async importOriginal => ({
  ...(await importOriginal<typeof import('../state/documents')>()),
  openPath: vi.fn(),
}))

function mockEditor(): EditorController {
  return {
    documentId: 1,
    view: 0,
    dispatch: vi.fn(),
    openFind: vi.fn(),
    findNext: vi.fn(),
    refresh: vi.fn(),
    focus: vi.fn(),
    lineCount: () => 120,
    scrollToLine: vi.fn(),
    scrollBy: vi.fn(),
    goToDefinition: vi.fn(),
    goToReferences: vi.fn(),
    peekReferences: vi.fn(),
    triggerSuggest: vi.fn(),
    showHover: vi.fn(),
    format: vi.fn(),
    rename: vi.fn(),
    codeActions: vi.fn(),
    triggerParameterHints: vi.fn(),
    showChange: vi.fn(),
    cursor: () => ({ line: 4, column: 2 }),
    previewLines: vi.fn(),
    scrollPosition: () => ({ top: 300, left: 0 }),
    restoreScroll: vi.fn(),
  }
}

const symbol = (
  name: string,
  kind: string,
  line: number,
  container: string | null = null,
): lsp.DocumentSymbol => ({
  name,
  kind,
  container,
  deprecated: false,
  start: { line, column: 0 },
  end: { line: line + 2, column: 1 },
  selection: { line, column: 4 },
})

describe('Palette', () => {
  let unregister = () => {}
  const save = vi.fn()
  const disabled = vi.fn()

  beforeEach(() => {
    unregister = registerCommands([
      { id: 'file.save', category: 'File', title: 'Save', keybinding: 'mod+s', run: save },
      {
        id: 'file.never',
        category: 'File',
        title: 'Never Shown',
        enabled: () => false,
        run: disabled,
      },
    ])
  })

  afterEach(() => {
    unregister()
    activeEditor.set(null)
    act(() => paletteStore.set({ open: false, initial: '', session: 0, pick: null }))
    vi.clearAllMocks()
  })

  it('lists enabled commands with their keybindings and runs the chosen one', async () => {
    const user = userEvent.setup()
    render(<Palette />)
    act(() => openPalette('>'))

    expect(screen.getByRole('option', { name: /File: Save/ })).toHaveTextContent('Ctrl+S')
    expect(screen.queryByRole('option', { name: /Never Shown/ })).not.toBeInTheDocument()

    await user.keyboard('sav{Enter}')
    expect(save).toHaveBeenCalledOnce()
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('goes to a line in the active editor, previewing it', async () => {
    const user = userEvent.setup()
    const editor = mockEditor()
    activeEditor.set(editor)
    render(<Palette />)
    act(() => openPalette(':'))
    expect(screen.getByRole('option')).toHaveTextContent(
      'Current Line: 5, Character: 3. Type a line number between 1 and 120 to navigate to.',
    )

    await user.keyboard('42')
    expect(screen.getByRole('option')).toHaveTextContent('Go to line 42.')
    expect(editor.previewLines).toHaveBeenLastCalledWith({ start: 41, end: 41, center: 41 })
    await user.keyboard(':7')
    expect(screen.getByRole('option')).toHaveTextContent('Go to line 42 and character 7.')
    await user.keyboard('{Enter}')
    expect(editor.dispatch).toHaveBeenCalledWith(
      { type: 'setCursor', position: { line: 41, column: 6 }, extend: false, add: false },
      'center',
    )
    // Chosen, so the editor stays where the preview put it.
    expect(editor.previewLines).toHaveBeenLastCalledWith(null)
    expect(editor.restoreScroll).not.toHaveBeenCalled()
  })

  it('counts lines back from the end, and scrolls back when closed', async () => {
    const user = userEvent.setup()
    const editor = mockEditor()
    activeEditor.set(editor)
    render(<Palette />)
    act(() => openPalette(':'))

    await user.keyboard('-1')
    expect(screen.getByRole('option')).toHaveTextContent('Go to line 120.')
    await user.keyboard('{Backspace}{Backspace}500')
    expect(screen.getByRole('option')).toHaveTextContent('between 1 and 120')
    // The message does nothing when chosen.
    await user.keyboard('{Enter}')
    expect(screen.getByRole('dialog')).toBeInTheDocument()

    await user.keyboard('{Escape}')
    expect(editor.dispatch).not.toHaveBeenCalled()
    expect(editor.previewLines).toHaveBeenLastCalledWith(null)
    expect(editor.restoreScroll).toHaveBeenCalledWith({ top: 300, left: 0 })
  })

  it("goes to a symbol in the editor, previewing the selected one's lines", async () => {
    const user = userEvent.setup()
    const editor = mockEditor()
    activeEditor.set(editor)
    vi.mocked(lsp.documentSymbols).mockResolvedValue([
      symbol('Parser', 'struct', 0),
      symbol('parse', 'method', 3, 'Parser'),
      symbol('main', 'function', 10),
    ])
    render(<Palette />)
    act(() => openPalette('@'))

    const options = await screen.findAllByRole('option')
    expect(options.map(option => option.textContent)).toEqual([
      'TParsersymbols (3)',
      'ƒparseParser',
      'ƒmain',
    ])
    expect(lsp.documentSymbols).toHaveBeenCalledWith(1)
    expect(editor.previewLines).toHaveBeenLastCalledWith({ start: 0, end: 2, center: 0 })

    // The first word matches names; the rest, the symbols they're in.
    await user.keyboard('pa par')
    expect(screen.getAllByRole('option').map(option => option.textContent)).toEqual([
      'ƒparseParsersymbols (1)',
    ])
    expect(editor.previewLines).toHaveBeenLastCalledWith({ start: 3, end: 5, center: 3 })
    await user.keyboard('{Enter}')
    expect(editor.dispatch).toHaveBeenCalledWith(
      { type: 'setCursor', position: { line: 3, column: 4 }, extend: false, add: false },
      'center',
    )
  })

  it('groups symbols by kind after @:', async () => {
    activeEditor.set(mockEditor())
    vi.mocked(lsp.documentSymbols).mockResolvedValue([
      symbol('b', 'function', 0),
      symbol('Point', 'struct', 2),
      symbol('a', 'function', 5),
    ])
    render(<Palette />)
    act(() => openPalette('@:'))

    const options = await screen.findAllByRole('option')
    expect(options.map(option => option.textContent)).toEqual([
      'ƒbfunctions (2)',
      'ƒa',
      'TPointstructs (1)',
    ])
  })

  it('explains when the editor has no symbols to go to', async () => {
    activeEditor.set(mockEditor())
    vi.mocked(lsp.documentSymbols).mockResolvedValue(null)
    render(<Palette />)
    act(() => openPalette('@'))
    expect(
      await screen.findByText('The active text editor does not provide symbol information.'),
    ).toBeInTheDocument()
  })

  it('searches symbols across the workspace and opens the chosen one', async () => {
    const user = userEvent.setup()
    const location = { path: '/w/src/util.rs', range: null, encoding: 'utf-8' }
    vi.mocked(lsp.workspaceSymbols).mockResolvedValue([
      { name: 'total', kind: 'function', container: null, deprecated: false, location },
      {
        name: 'Totals',
        kind: 'struct',
        container: 'report',
        deprecated: true,
        location: { ...location, path: '/w/src/report.rs' },
      },
    ])
    vi.mocked(lsp.resolveSymbol).mockResolvedValue({
      path: '/w/src/util.rs',
      start: { line: 4, column: 7 },
      end: { line: 4, column: 12 },
    })
    workspaceStore.set({
      info: { root: '/w', name: 'w', trust: 'trusted' },
      listings: new Map(),
      indexing: false,
    })
    try {
      render(<Palette />)
      act(() => openPalette('#'))
      await user.keyboard('tot')

      const options = await screen.findAllByRole('option')
      expect(lsp.workspaceSymbols).toHaveBeenLastCalledWith('tot')
      expect(options.map(option => option.textContent)).toEqual([
        'ƒtotalsrc/util.rs',
        'TTotalsreport • src/report.rs',
      ])
      expect(options[1]?.querySelector('[data-deprecated]')).toHaveTextContent('Totals')

      await user.keyboard('{Enter}')
      expect(lsp.resolveSymbol).toHaveBeenCalledWith(location)
      expect(openPath).toHaveBeenCalledWith('/w/src/util.rs', { start: { line: 4, column: 7 } })
    } finally {
      act(() => workspaceStore.set({ info: null, listings: new Map(), indexing: false }))
    }
  })

  it('closes on Escape', async () => {
    const user = userEvent.setup()
    render(<Palette />)
    act(() => openPalette('>'))
    await user.keyboard('{Escape}')
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
  })

  it('shows file results with their icons', async () => {
    vi.mocked(findFiles).mockResolvedValue({
      files: [
        { path: '/w/src/main.rs', relative: 'src/main.rs', indices: [4] },
        { path: '/w/README.md', relative: 'README.md', indices: [] },
      ],
      indexing: false,
      truncated: false,
    })
    const none = {
      file: null,
      folder: null,
      folderExpanded: null,
      fileExtensions: new Map<string, string>(),
      fileNames: new Map<string, string>(),
      folderNames: new Map<string, string>(),
      folderNamesExpanded: new Map<string, string>(),
    }
    workspaceStore.set({
      info: { root: '/w', name: 'w', trust: 'trusted' },
      listings: new Map(),
      indexing: false,
    })
    try {
      render(<Palette />)
      act(() => openPalette(''))
      const options = await screen.findAllByRole('option')
      expect(options.map(option => option.textContent)).toEqual(['main.rssrc', 'README.md'])
      const icons = options.map(option => option.querySelector('.palette__icon'))
      expect(icons.map(icon => icon?.tagName)).toEqual(['svg', 'svg'])

      act(() =>
        iconThemeStore.set({
          id: 'example.icons.colorful',
          icons: new Map([['rust', 'data:image/svg+xml;base64,cnM=']]),
          associations: { ...none, fileExtensions: new Map([['rs', 'rust']]) },
          light: none,
        }),
      )
      const rust = screen.getAllByRole('option')[0]?.querySelector('.palette__icon')
      expect(rust).toHaveAttribute('src', 'data:image/svg+xml;base64,cnM=')
      expect(rust).toHaveAttribute('alt', '')
    } finally {
      act(() => {
        iconThemeStore.set(null)
        workspaceStore.set({ info: null, listings: new Map(), indexing: false })
      })
    }
  })

  it('explains that files need an open folder', () => {
    render(<Palette />)
    act(() => openPalette(''))
    expect(screen.getByText('Open a folder to search its files')).toBeInTheDocument()
  })

  it('shows a quick pick, filtered by typing, and runs the chosen item', async () => {
    const user = userEvent.setup()
    const chosen = vi.fn()
    render(<Palette />)
    act(() =>
      openQuickPick({
        placeholder: 'Reopen with encoding',
        items: [
          { label: 'UTF-8', detail: 'Current', run: () => chosen('utf-8') },
          { label: 'Western (Windows 1252)', detail: 'windows-1252', run: () => chosen('1252') },
          { label: 'Japanese (Shift JIS)', detail: 'shift_jis', run: () => chosen('sjis') },
        ],
      }),
    )
    expect(screen.getByRole('dialog', { name: 'Reopen with encoding' })).toBeInTheDocument()
    expect(screen.getAllByRole('option')).toHaveLength(3)
    expect(screen.getByRole('option', { name: /UTF-8/ })).toHaveTextContent('Current')

    await user.keyboard('shift')
    expect(screen.getAllByRole('option')).toHaveLength(1)
    await user.keyboard('{Enter}')
    expect(chosen).toHaveBeenCalledWith('sjis')
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()

    // Opening the palette as usual drops the pick.
    act(() => openPalette('>'))
    expect(screen.getByRole('option', { name: /File: Save/ })).toBeInTheDocument()
  })
})
