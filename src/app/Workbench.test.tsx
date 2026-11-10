import { act, fireEvent, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { builtinCommands } from '../commands/builtin'
import { registerCommands } from '../commands/registry'
import { contextMenuStore } from '../components/contextMenuState'
import { dockStore, enableWindows, moveItem, restoreDock } from '../dock/dock'
import { findGroup, groupOf } from '../dock/model'
import type { AppInfo } from '../ipc'
import { documentsStore } from '../state/documents'
import { fileMarksStore } from '../state/fileMarks'
import { DEFAULT_LAYOUT, layoutStore } from '../state/layout'
import { dragFilesTo, dragTo, pressToDrag } from '../test/drag'
import { Workbench } from './Workbench'

vi.mock('../ipc/editor', async importOriginal => ({
  ...(await importOriginal<typeof import('../ipc/editor')>()),
  openDocument: vi.fn(),
}))
const editorIpc = await import('../ipc/editor')

const appInfo: AppInfo = {
  name: 'Flareite',
  version: '0.1.0',
  os: 'windows',
  arch: 'x86_64',
  tauriVersion: '2.12.5',
}

/** jsdom has no layout: gives every element a 1000×1000 box. */
function stubLayout() {
  vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue(
    DOMRect.fromRect({ x: 0, y: 0, width: 1000, height: 1000 }),
  )
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockReturnValue(1000)
  vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(1000)
  // The window is as large: past its edges, a drag is outside it.
  vi.spyOn(window, 'innerWidth', 'get').mockReturnValue(1000)
  vi.spyOn(window, 'innerHeight', 'get').mockReturnValue(1000)
}

const groupHolding = (item: string) => groupOf(dockStore.get(), item)

const doc = (id: number, path: string) => ({
  id,
  title: path.split('/').pop()!,
  path,
  dirty: false,
  lineEnding: 'LF' as const,
  encoding: 'UTF-8',
  encodingId: 'utf-8',
})

describe('Workbench', () => {
  beforeEach(() => {
    // Documents first: closing them updates the saved dock, which the reset replaces.
    documentsStore.set({ documents: [], activeId: null })
    layoutStore.set(DEFAULT_LAYOUT)
    restoreDock()
  })

  afterEach(async () => {
    // Focus moves on the next frame; don't let it land in the next test.
    await act(() => new Promise(resolve => requestAnimationFrame(resolve)))
    vi.restoreAllMocks()
  })

  it('opens with the views on the left and the welcome page', () => {
    render(<Workbench appInfo={null} />)

    const views = screen.getByRole('region', { name: 'Explorer' })
    expect(
      within(views)
        .getAllByRole('tab')
        .map(tab => tab.getAttribute('aria-label')),
    ).toEqual(['Explorer', 'Search', 'Extensions'])
    expect(screen.getByRole('button', { name: 'Explorer' })).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByRole('heading', { name: 'Flareite' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /New File/ })).toBeInTheDocument()
  })

  it('switches views from the activity bar, and hides their group when selected again', async () => {
    const user = userEvent.setup()
    render(<Workbench appInfo={null} />)
    const search = screen.getByRole('button', { name: 'Search' })

    await user.click(search)
    expect(screen.getByRole('tab', { name: 'Search' })).toHaveAttribute('aria-selected', 'true')
    expect(search).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByRole('button', { name: 'Explorer' })).toHaveAttribute(
      'aria-pressed',
      'false',
    )

    await user.click(search)
    expect(screen.queryByRole('tab', { name: 'Search' })).not.toBeInTheDocument()
    expect(search).toHaveAttribute('aria-pressed', 'false')

    await user.click(screen.getByRole('button', { name: 'Explorer' }))
    expect(screen.getByRole('tab', { name: 'Explorer' })).toHaveAttribute('aria-selected', 'true')
  })

  it('shows the side bar views on the activity bar, and takes views dropped on it', () => {
    stubLayout()
    render(<Workbench appInfo={null} />)
    const activityBar = screen.getByRole('navigation', { name: 'Views' })
    const buttons = () =>
      within(activityBar)
        .getAllByRole('button')
        .map(button => button.getAttribute('aria-label'))
    expect(buttons()).toEqual(['Explorer', 'Search', 'Extensions'])

    // Docked on the right, the explorer leaves the activity bar.
    act(() => moveItem('view:explorer', { kind: 'root', edge: 'right' }))
    expect(buttons()).toEqual(['Search', 'Extensions'])

    // Its tab dropped on the activity bar brings it back into the side bar.
    const drag = pressToDrag(screen.getByRole('tab', { name: 'Explorer' }))
    drag.moveTo({ element: activityBar, x: 10, y: 500 })
    expect(activityBar).toHaveAttribute('data-drop', 'true')
    expect(document.querySelector('.drag-ghost')).toHaveTextContent('Explorer')
    drag.drop()
    expect(activityBar).not.toHaveAttribute('data-drop')
    expect(document.querySelector('.drag-ghost')).not.toBeInTheDocument()
    expect(groupHolding('view:explorer')!.id).toBe('g1')
    expect(buttons()).toEqual(['Search', 'Extensions', 'Explorer'])

    // Anything docked there gets a button, such as a document.
    act(() => documentsStore.set({ documents: [doc(1, '/w/main.rs')], activeId: 1 }))
    expect(buttons()).toEqual(['Search', 'Extensions', 'Explorer'])
    dragTo(screen.getByRole('tab', { name: 'main.rs' }), { element: activityBar, x: 10, y: 500 })
    expect(buttons()).toEqual(['Search', 'Extensions', 'Explorer', 'main.rs'])
    expect(within(activityBar).getByRole('button', { name: 'main.rs' })).toHaveAttribute(
      'aria-pressed',
      'true',
    )
  })

  it('resizes groups from the keyboard within their limits', async () => {
    stubLayout()
    const user = userEvent.setup()
    render(<Workbench appInfo={null} />)
    const sash = screen.getByRole('separator', { name: 'Resize group' })
    const sizes = () => {
      const root = dockStore.get().root
      return root.kind === 'split' ? root.sizes : []
    }

    sash.focus()
    await user.keyboard('{ArrowRight}')
    expect(sizes()[0]).toBeCloseTo(0.216)
    await user.keyboard('{End}')
    expect(sizes()[0]).toBeCloseTo(0.9)
    await user.keyboard('{Home}')
    expect(sizes()[0]).toBeCloseTo(0.1)
  })

  it('can still resize groups after the splits holding them were hidden and shown', async () => {
    stubLayout()
    // As in browsers: elements out of the page measure 0, and observers report
    // what they observe when it changes size, including on leaving the page.
    const sized = function (this: HTMLElement) {
      return this.isConnected ? 1000 : 0
    }
    vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(sized)
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(sized)
    const observers = new Set<{ callback: () => void; targets: Set<Element> }>()
    vi.stubGlobal(
      'ResizeObserver',
      class {
        entry: { callback: () => void; targets: Set<Element> }
        constructor(callback: () => void) {
          this.entry = { callback, targets: new Set() }
          observers.add(this.entry)
        }
        observe(target: Element) {
          this.entry.targets.add(target)
        }
        unobserve(target: Element) {
          this.entry.targets.delete(target)
        }
        disconnect() {
          observers.delete(this.entry)
        }
      },
    )
    const layoutChanged = () =>
      act(() => observers.forEach(entry => entry.targets.size > 0 && entry.callback()))
    const user = userEvent.setup()
    render(<Workbench appInfo={null} />)
    const activityBar = screen.getByRole('navigation', { name: 'Views' })
    const button = (name: string) => within(activityBar).getByRole('button', { name })

    // Search docked below the explorer: the left side split in two.
    act(() => moveItem('view:search', { kind: 'split', group: 'g1', edge: 'bottom' }))
    layoutChanged()
    expect(screen.getAllByRole('separator', { name: 'Resize group' })).toHaveLength(2)

    // Both hidden from the activity bar, then shown again.
    await user.click(button('Explorer'))
    await user.click(button('Search'))
    layoutChanged()
    expect(screen.queryAllByRole('separator', { name: 'Resize group' })).toHaveLength(0)
    await user.click(button('Explorer'))
    await user.click(button('Search'))
    layoutChanged()

    // Both splitters are back and work: the side bar's width, and the
    // explorer's height above Search.
    const sashes = screen.getAllByRole('separator', { name: 'Resize group' })
    expect(sashes).toHaveLength(2)
    const root = () => dockStore.get().root
    const column = () => {
      const node = root()
      return node.kind === 'split' ? node.children[0]! : node
    }
    const sizes = (node: ReturnType<typeof root>) => (node.kind === 'split' ? node.sizes : [])
    // Showing a view focuses it on the next frame; let that happen first.
    await act(() => new Promise(resolve => requestAnimationFrame(resolve)))
    sashes.find(sash => sash.getAttribute('aria-orientation') === 'vertical')!.focus()
    await user.keyboard('{End}')
    expect(sizes(root())[0]).toBeCloseTo(0.9)
    sashes.find(sash => sash.getAttribute('aria-orientation') === 'horizontal')!.focus()
    // Its value is the lower group's height: Home shrinks Search.
    await user.keyboard('{Home}')
    expect(sizes(column())[0]).toBeCloseTo(0.9)
    vi.unstubAllGlobals()
  })

  it('docks a dragged tab into another group, or beside it', () => {
    stubLayout()
    act(() => documentsStore.set({ documents: [doc(1, '/w/main.rs')], activeId: 1 }))
    render(<Workbench appInfo={null} />)
    const editor = screen.getByRole('region', { name: 'main.rs' })

    // Onto the editor's tabs: a tab of that group, even at the top edge of the window.
    const bar = within(editor).getByRole('tablist').parentElement!
    dragTo(screen.getByRole('tab', { name: 'Search' }), { element: bar, x: 500, y: 5 })
    expect(groupHolding('view:search')!.id).toBe('main')

    // Onto the right side of the content: a new group there.
    const content = within(screen.getByRole('region', { name: 'Search' })).getByRole('tabpanel')
    const drag = pressToDrag(screen.getByRole('tab', { name: 'Explorer' }))
    drag.moveTo({ element: content, x: 900, y: 500 })
    expect(content.querySelector('.dock-drop')).toHaveAttribute('data-zone', 'right')
    drag.drop()
    const group = groupHolding('view:explorer')!
    expect(group.items).toEqual(['view:explorer'])
    expect(group.id).not.toBe('g1')

    // Near the edge of the whole dock, over a group along it: a group along that edge.
    const outer = within(screen.getByRole('region', { name: 'Search' })).getByRole('tabpanel')
    const edgeDrag = pressToDrag(screen.getByRole('tab', { name: 'Extensions' }))
    edgeDrag.moveTo({ element: outer, x: 500, y: 990 })
    expect(document.querySelector('.dock-drop--root')).toHaveAttribute('data-zone', 'bottom')
    edgeDrag.drop()
    const root = dockStore.get().root
    expect(root.kind === 'split' && root.direction).toBe('column')
    expect(root.kind === 'split' && root.children.at(-1)).toBe(groupHolding('view:extensions'))
  })

  it('opens a tab dropped outside the window in a window of its own', () => {
    // As the desktop app does once it can open windows.
    enableWindows()
    stubLayout()
    act(() =>
      documentsStore.set({ documents: [doc(1, '/w/main.rs'), doc(2, '/w/lib.rs')], activeId: 2 }),
    )
    render(<Workbench appInfo={null} />)
    dragTo(screen.getByRole('tab', { name: 'lib.rs' }), { element: document.body, x: 1500, y: 300 })
    const windows = dockStore.get().windows ?? []
    expect(windows).toHaveLength(1)
    expect(groupOf({ main: '', root: windows[0]!.root }, 'document:2')).toBeDefined()
    expect(groupHolding('document:1')!.id).toBe('main')

    // Over the window's own edge, but inside it, nothing detaches.
    dragTo(screen.getByRole('tab', { name: 'main.rs' }), { element: document.body, x: 999, y: 300 })
    expect(dockStore.get().windows).toHaveLength(1)
  })

  it('cancels a drag with Escape, and keeps the click that ends a drag from the tab', () => {
    stubLayout()
    render(<Workbench appInfo={null} />)
    const search = screen.getByRole('tab', { name: 'Search' })
    const content = within(screen.getByRole('region', { name: 'Explorer' })).getByRole('tabpanel')

    // Pressing moves focus; the blur of what had it doesn't end the drag.
    act(() => screen.getByRole('tab', { name: 'Explorer' }).focus())
    const drag = pressToDrag(search)
    act(() => search.focus())
    drag.moveTo({ element: content, x: 900, y: 500 })
    expect(content.querySelector('.dock-drop')).toBeInTheDocument()
    drag.cancel()
    expect(content.querySelector('.dock-drop')).not.toBeInTheDocument()
    expect(findGroup(dockStore.get(), 'g1')!.items).toContain('view:search')

    // The click that ends a drag (here, dropped where nothing takes it) is swallowed.
    const extensions = screen.getByRole('tab', { name: 'Extensions' })
    dragTo(extensions, { element: document.body, x: 900, y: 500 })
    fireEvent.click(extensions)
    expect(extensions).toHaveAttribute('aria-selected', 'false')

    // A press without moving is a click: it selects the tab.
    pressToDrag(search).drop()
    fireEvent.click(search)
    expect(search).toHaveAttribute('aria-selected', 'true')
  })

  it('opens files dropped from other programs where they are dropped', async () => {
    stubLayout()
    vi.mocked(editorIpc.openDocument).mockImplementation(async path => doc(7, path))
    act(() => documentsStore.set({ documents: [doc(1, '/w/main.rs')], activeId: 1 }))
    render(<Workbench appInfo={null} />)

    // Beside the editor: a new group on its right.
    const content = within(screen.getByRole('region', { name: 'main.rs' })).getByRole('tabpanel')
    dragFilesTo({ element: content, x: 900, y: 500 }, { files: ['C:\\elsewhere\\notes.txt'] })
    await act(async () => {})
    expect(editorIpc.openDocument).toHaveBeenCalledWith('C:\\elsewhere\\notes.txt', false)
    const group = groupHolding('document:7')!
    expect(group.items).toEqual(['document:7'])
    expect(group.id).not.toBe('main')
  })

  it("shows extensions' marks on a document's tab, as in the explorer", () => {
    render(<Workbench appInfo={null} />)
    act(() => {
      fileMarksStore.set([
        {
          extension: 'flareite.git',
          marks: [
            {
              path: '/w/main.rs',
              badge: 'M',
              color: null,
              tooltip: 'Modified',
              faded: false,
              inherit: false,
              propagate: true,
              priority: 1,
            },
          ],
        },
      ])
      documentsStore.set({ documents: [doc(1, '/w/main.rs')], activeId: 1 })
    })
    const tab = screen.getByRole('tab', { name: 'main.rs' })
    expect(within(tab).getByText('M')).toHaveClass('dock-tab__badge')
    expect(tab).toHaveAttribute('title', '/w/main.rs\nModified')
    act(() => fileMarksStore.set([]))
  })

  it('moves between tabs with the arrow keys and closes them with Delete', async () => {
    const user = userEvent.setup()
    render(<Workbench appInfo={null} />)
    const explorer = screen.getByRole('tab', { name: 'Explorer' })

    explorer.focus()
    await user.keyboard('{ArrowRight}')
    expect(screen.getByRole('tab', { name: 'Search' })).toHaveAttribute('aria-selected', 'true')
    await act(() => new Promise(resolve => requestAnimationFrame(resolve)))
    expect(screen.getByRole('tab', { name: 'Search' })).toHaveFocus()

    await user.keyboard('{Delete}')
    expect(screen.queryByRole('tab', { name: 'Search' })).not.toBeInTheDocument()
    expect(findGroup(dockStore.get(), 'g1')!.items).toEqual(['view:explorer', 'view:extensions'])
  })

  it('closes the focused tab with Ctrl+W and reopens it with Ctrl+Shift+T', async () => {
    const user = userEvent.setup()
    const unregister = registerCommands(builtinCommands)
    render(<Workbench appInfo={null} />)

    await user.click(screen.getByRole('tab', { name: 'Search' }))
    await user.keyboard('{Control>}w{/Control}')
    expect(findGroup(dockStore.get(), 'g1')!.items).toEqual(['view:explorer', 'view:extensions'])

    await user.keyboard('{Control>}{Shift>}T{/Shift}{/Control}')
    expect(findGroup(dockStore.get(), 'g1')!.items).toEqual([
      'view:explorer',
      'view:search',
      'view:extensions',
    ])
    expect(screen.getByRole('tab', { name: 'Search' })).toHaveAttribute('aria-selected', 'true')
    unregister()
  })

  it('offers splitting and moving in a tab menu, from the keyboard too', async () => {
    const user = userEvent.setup()
    render(<Workbench appInfo={null} />)

    screen.getByRole('tab', { name: 'Explorer' }).focus()
    await user.keyboard('{Shift>}{F10}{/Shift}')
    const labels = contextMenuStore.get()?.entries.map(entry => entry?.label ?? '—')
    expect(labels).toEqual([
      'Close Explorer',
      '—',
      'Split Right',
      'Split Down',
      'Move into New Window',
      '—',
      'Move Left',
      'Move Right',
      'Move Up',
      'Move Down',
    ])
    act(() => contextMenuStore.set(null))
  })

  it('shows backend application details once available', () => {
    const { rerender } = render(<Workbench appInfo={null} />)
    const statusBar = screen.getByRole('contentinfo')
    expect(within(statusBar).queryByText(/Flareite/)).not.toBeInTheDocument()

    rerender(<Workbench appInfo={appInfo} />)
    expect(within(statusBar).getByText('Flareite 0.1.0')).toBeInTheDocument()
    expect(screen.getByText('Windows x86_64')).toBeInTheDocument()
  })
})
