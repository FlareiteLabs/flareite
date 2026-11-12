import { act, fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { EditorOp, Viewport, WireView as View } from '../ipc/editor'
import { appearance } from '../state/appearance'
import { editorStatus } from '../state/editorStatus'
import { contextMenuStore } from '../components/contextMenuState'
import { editorFor } from './activeEditor'
import { EditorView } from './EditorView'
import type { EditorTransport } from './session'

const runCodeLens = vi.hoisted(() => vi.fn(() => Promise.resolve()))
const pathKinds = vi.hoisted(() =>
  vi.fn((paths: string[]) => Promise.resolve(paths.map(() => 'file' as const))),
)
vi.mock('../ipc/extensions', async importOriginal => ({
  ...(await importOriginal<typeof import('../ipc/extensions')>()),
  runCodeLens,
  pathKinds,
}))
const autoInsert = vi.hoisted(() =>
  vi.fn((_id: number, _view: number, _trigger: string) =>
    Promise.resolve({
      version: 2,
      position: { line: 0, column: 5 },
      text: '</div>',
      cursor: 0,
    }),
  ),
)
vi.mock('../ipc/editor', async importOriginal => ({
  ...(await importOriginal<typeof import('../ipc/editor')>()),
  autoInsert,
}))
const openLink = vi.hoisted(() => vi.fn())
vi.mock('../state/links', async importOriginal => ({
  ...(await importOriginal<typeof import('../state/links')>()),
  openLink,
}))

/** A full (non-incremental) view of `lines`. */
function makeView(lines: string[], overrides: Partial<View> = {}): View {
  return {
    documentId: 7,
    version: 1,
    lineCount: lines.length,
    rowCount: lines.length,
    startRow: 0,
    startLine: 0,
    endLine: lines.length,
    visible: [[0, lines.length]],
    folded: [],
    foldable: [],
    foldGeneration: 0,
    lines: { base: null, splices: [], chunks: [{ start: 0, lines }] },
    selections: [{ anchor: { line: 0, column: 0 }, head: { line: 0, column: 0 }, primary: true }],
    selectionCount: 1,
    primary: { position: { line: 0, column: 0 }, visualColumn: 0, row: 0 },
    dirty: false,
    search: null,
    highlights: [],
    diagnostics: [],
    language: null,
    decorations: [],
    changes: [],
    lenses: [],
    annotations: [],
    ...overrides,
  } as View
}

function fakeTransport(initial: View) {
  const applied: EditorOp[][] = []
  const transport: EditorTransport = {
    view: vi.fn((_id: number, _view: number, _viewport: Viewport) => Promise.resolve(initial)),
    apply: vi.fn((_id: number, _view: number, ops: EditorOp[]) => {
      applied.push(ops)
      return Promise.resolve(
        makeView(['changed'], {
          version: 2,
          dirty: true,
          primary: { position: { line: 0, column: 3 }, visualColumn: 3, row: 0 },
        }),
      )
    }),
    clipboard: vi.fn(() => Promise.resolve(initial)),
  }
  return { transport, applied }
}

async function renderEditor(transport: EditorTransport) {
  render(<EditorView documentId={7} title="main.rs" transport={transport} />)
  await act(async () => {})
  return screen.getByRole('textbox', { name: 'Editor for main.rs' })
}

describe('EditorView', () => {
  afterEach(() => {
    editorStatus.set(null)
  })

  it('renders the lines of the initial view with line numbers', async () => {
    const { transport } = fakeTransport(makeView(['fn main() {', '}']))
    await renderEditor(transport)
    expect(screen.getByText('fn main() {')).toBeInTheDocument()
    expect(screen.getByText('2')).toBeInTheDocument()
    expect(transport.view).toHaveBeenCalledWith(
      7,
      0,
      expect.objectContaining({ startRow: 0 }),
      null,
      null,
    )
  })

  it('sends typed text and keys as operations and renders the result', async () => {
    const user = userEvent.setup()
    const { transport, applied } = fakeTransport(makeView(['']))
    const input = await renderEditor(transport)

    input.focus()
    await user.keyboard('x')
    expect(applied[0]).toEqual([{ type: 'type', text: 'x' }])
    expect(await screen.findByText('changed')).toBeInTheDocument()

    await user.keyboard('{Backspace}')
    await user.keyboard('{Shift>}{ArrowRight}{/Shift}')
    expect(applied.flat()).toContainEqual({
      type: 'delete',
      direction: 'backward',
      unit: 'grapheme',
    })
    expect(applied.flat()).toContainEqual({ type: 'move', motion: 'right', extend: true })
  })

  it('asks extensions what to insert after their trigger characters, and inserts it', async () => {
    const user = userEvent.setup()
    const language = {
      id: 'html',
      language: 'HTML',
      server: null,
      completionTriggers: null,
      autoInsertTriggers: ['>', '/'],
    }
    const { transport, applied } = fakeTransport(makeView(['<div'], { language }))
    const input = await renderEditor(transport)
    input.focus()
    expect(autoInsert).not.toHaveBeenCalled()
    await user.keyboard('>')
    await vi.waitFor(() => expect(autoInsert).toHaveBeenCalledWith(7, 0, '>'))
    await vi.waitFor(() =>
      expect(applied.flat()).toContainEqual({
        type: 'autoInsert',
        version: 2,
        position: { line: 0, column: 5 },
        text: '</div>',
        cursor: 0,
      }),
    )
  })

  it('asks extensions what to insert after Enter, if Enter is a trigger', async () => {
    const user = userEvent.setup()
    autoInsert.mockClear()
    const language = {
      id: 'luau',
      language: 'Luau',
      server: null,
      completionTriggers: null,
      autoInsertTriggers: ['\n'],
    }
    const { transport } = fakeTransport(makeView(['if x then'], { language }))
    const input = await renderEditor(transport)
    input.focus()
    await user.keyboard('{Enter}')
    await vi.waitFor(() => expect(autoInsert).toHaveBeenCalledWith(7, 0, '\n'))
  })

  it('reports the cursor position for the status bar', async () => {
    const user = userEvent.setup()
    const { transport } = fakeTransport(makeView(['abc']))
    const input = await renderEditor(transport)
    expect(editorStatus.get()).toEqual({ line: 1, column: 1, selectionCount: 1, language: null })
    input.focus()
    await user.keyboard('a')
    expect(editorStatus.get()).toEqual({ line: 1, column: 4, selectionCount: 1, language: null })
  })

  it("holds the cursor's line and selection for screen readers", async () => {
    const user = userEvent.setup()
    const { transport } = fakeTransport(
      makeView(['fn main() {', '  ok();', '}'], {
        selections: [
          { anchor: { line: 1, column: 2 }, head: { line: 1, column: 6 }, primary: true },
        ],
        primary: { position: { line: 1, column: 6 }, visualColumn: 6, row: 1 },
      }),
    )
    const input = (await renderEditor(transport)) as HTMLTextAreaElement
    expect(input.value).toBe('  ok();')
    expect([input.selectionStart, input.selectionEnd]).toEqual([2, 6])

    // After typing, it follows the view the backend returns.
    input.focus()
    await user.keyboard('x')
    expect(await screen.findByText('changed')).toBeInTheDocument()
    expect(input.value).toBe('changed')
    expect(input.selectionStart).toBe(0)
  })

  it('inserts composed text once, when composition ends', async () => {
    const { transport, applied } = fakeTransport(makeView(['ab']))
    const input = (await renderEditor(transport)) as HTMLTextAreaElement
    fireEvent.compositionStart(input)
    input.value = 'かab'
    fireEvent.input(input, { isComposing: true })
    expect(applied).toHaveLength(0)
    fireEvent.compositionEnd(input, { data: 'か' })
    fireEvent.input(input)
    expect(applied).toEqual([[{ type: 'type', text: 'か' }]])
    expect(input.value).toBe('ab')
  })

  it('opens the context menu from the keyboard without moving the cursor', async () => {
    const { transport, applied } = fakeTransport(makeView(['abc']))
    const input = await renderEditor(transport)
    fireEvent.contextMenu(input)
    expect(contextMenuStore.get()?.entries.map(entry => entry?.label)).toContain('Copy')
    expect(applied).toHaveLength(0)
    contextMenuStore.set(null)
  })

  it('moves the cursor to the end of the last line when clicked below it', async () => {
    const { transport, applied } = fakeTransport(makeView(['fn main() {', '}']))
    const input = await renderEditor(transport)
    const content = document.querySelector('.editor-view__content')
    expect(content).not.toBeNull()
    // Lines start at y = 0 in jsdom, so y = 500 is far below the second line.
    fireEvent.mouseDown(content as Element, { button: 0, clientX: 40, clientY: 500 })
    fireEvent.mouseUp(window)
    await act(async () => {})
    expect(input).toHaveFocus()
    expect(applied.flat()).toContainEqual({
      type: 'setCursor',
      position: { line: 1, column: Number.MAX_SAFE_INTEGER },
      extend: false,
      add: false,
    })
  })

  it('opens web addresses and paths in the text with Ctrl+click', async () => {
    const { transport } = fakeTransport(
      makeView(['see /code/src/a.rs:3:2', 'docs: https://example.com/docs']),
    )
    await renderEditor(transport)
    const content = document.querySelector('.editor-view__content') as Element
    // In jsdom every point is the end of its row's line. The web address, on
    // the second line, with Shift to choose who opens it:
    const { lineHeight } = appearance.get()
    fireEvent.mouseDown(content, {
      button: 0,
      clientX: 2,
      clientY: lineHeight * 1.5,
      ctrlKey: true,
      shiftKey: true,
    })
    fireEvent.mouseUp(window)
    await act(async () => {})
    expect(openLink).toHaveBeenLastCalledWith(
      { kind: 'web', target: 'https://example.com/docs' },
      true,
    )
    // The path, once the backend says it's a file (the fake says every path is).
    fireEvent.mouseDown(content, { button: 0, clientX: 2, clientY: 2, ctrlKey: true })
    fireEvent.mouseUp(window)
    await vi.waitFor(() =>
      expect(openLink).toHaveBeenLastCalledWith(
        { kind: 'file', target: '/code/src/a.rs', line: 2, column: 1 },
        false,
      ),
    )
  })

  it('folds and unfolds from the gutter', async () => {
    const { transport, applied } = fakeTransport(
      makeView(['fn a() {', '}', 'x'], { foldable: [0] }),
    )
    await renderEditor(transport)
    expect(document.querySelector('[data-fold-line="1"]')).toBeNull()
    const marker = document.querySelector('[data-fold-line="0"]')
    expect(marker).not.toBeNull()
    fireEvent.mouseDown(marker as Element, { button: 0 })
    expect(applied.flat()).toContainEqual({ type: 'toggleFoldAt', line: 0 })
  })

  it('shows a folded region as its first line, with a marker that unfolds it', async () => {
    const { transport, applied } = fakeTransport(
      makeView([], {
        lineCount: 10,
        rowCount: 3,
        endLine: 10,
        visible: [
          [0, 1],
          [8, 10],
        ],
        folded: [0],
        foldable: [0],
        lines: {
          base: null,
          splices: [],
          chunks: [
            { start: 0, lines: ['fn a() {'] },
            { start: 8, lines: ['}', ''] },
          ],
        },
      }),
    )
    await renderEditor(transport)
    expect(screen.getByText('9')).toBeInTheDocument()
    expect(screen.queryByText('2')).toBeNull()
    expect(document.querySelector('[data-folded]')).not.toBeNull()
    fireEvent.mouseDown(screen.getByTitle('Unfold'), { button: 0, clientX: 100, clientY: 1 })
    expect(applied.flat()).toContainEqual({ type: 'toggleFoldAt', line: 0 })
  })

  it('highlights whole lines for a Go to Line or Go to Symbol preview', async () => {
    const lines = Array.from({ length: 10 }, (_, index) => `line ${index}`)
    const { transport, applied } = fakeTransport(makeView(lines))
    await renderEditor(transport)
    const highlights = () =>
      [...document.querySelectorAll<HTMLElement>('.editor-view__range-highlight')].map(
        highlight => highlight.style.top,
      )

    act(() => editorFor(7)!.previewLines({ start: 2, end: 3, center: 2 }))
    expect(highlights()).toEqual(['40px', '60px'])
    // A preview doesn't move the cursor.
    expect(applied).toEqual([])

    act(() => editorFor(7)!.previewLines(null))
    expect(highlights()).toEqual([])
  })

  it('sends pasted text as a paste', async () => {
    const user = userEvent.setup()
    const { transport, applied } = fakeTransport(makeView(['']))
    const input = await renderEditor(transport)
    input.focus()
    await user.paste('pasted\ntext')
    expect(applied[0]).toEqual([{ type: 'paste', text: 'pasted\ntext' }])
  })

  it("shows extensions' code lenses above their lines, running their commands", async () => {
    const lenses = [
      {
        line: 1,
        title: 'Accept Current Change',
        tooltip: null,
        command: 'x.accept',
        argument: '1',
      },
      {
        line: 1,
        title: 'Compare Changes',
        tooltip: 'Compare',
        command: 'x.compare',
        argument: '1',
      },
    ]
    const { transport } = fakeTransport(makeView(['one', '<<<<<<< HEAD', 'two'], { lenses }))
    await renderEditor(transport)
    const accept = screen.getByRole('button', { name: 'Accept Current Change' })
    expect(screen.getByRole('button', { name: 'Compare Changes' })).toHaveAttribute(
      'title',
      'Compare',
    )
    // A row between the first line and the second.
    expect(accept.closest('.editor-view__lens-zone')).not.toBeNull()
    fireEvent.click(accept)
    expect(runCodeLens).toHaveBeenCalledWith(7, 'x.accept', '1')
  })

  it("shows an extension's faint text after its line, with a tooltip", async () => {
    const annotations = [{ line: 1, text: 'first, You (now)', tooltip: 'Commit 0123' }]
    const { transport } = fakeTransport(makeView(['one', 'two'], { annotations }))
    await renderEditor(transport)
    const annotation = screen.getByText('first, You (now)')
    expect(annotation).toHaveClass('editor-view__annotation')
    expect(annotation).toHaveAttribute('title', 'Commit 0123')
    expect(annotation.closest('.editor-view__line')).toHaveTextContent('two')
  })
})
