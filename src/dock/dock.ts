/**
 * The dock's state: where every document, view, terminal, preview, and
 * page shows, which group the user is working in, and the actions on them.
 *
 * The documents, terminals, and previews stores own what's open; the dock
 * follows them (adding a tab for a new document, removing it when the
 * document closes) and decides only where things go. Views and pages are
 * added when shown.
 *
 * A document can show in several groups, once in each: each of its tabs
 * shows a view of its own (`ipc/editor.ts`), with its own selections,
 * folds, and scroll position.
 *
 * The layout is saved with the rest of the workbench layout. Documents are
 * saved by path (`file:<path>`, and `copy:<n>:<path>` for their other
 * tabs), because document ids change between runs; they hold their place
 * until the documents load. Previews aren't saved: their extensions open
 * them again. Files in viewers (and binary files waiting to be opened as
 * text) are saved as they are.
 */
import { setDropFallback } from '../components/drag'
import {
  activeEditor,
  editorsFor,
  savedScroll,
  saveScroll,
  setActiveOverride,
  setShownView,
  shownView,
} from '../editor/activeEditor'
import { log } from '../ipc'
import * as ipc from '../ipc/editor'
import {
  activate as activateDocument,
  close as closeDocument,
  documentsStore,
  openPath,
  takeOpenRequest,
  activationCount,
} from '../state/documents'
import {
  activeTabChanged,
  adoptDocument,
  findLayout,
  layoutDirty,
  layoutItem,
  noteOpening,
  paneEditor,
  prefetchLayouts,
  paneToFocus,
  parseLayoutItem,
  releaseLayout,
  setLayoutTabHandler,
} from '../state/layouts'
import { setActiveLayoutTab } from '../state/extensions'
import { layoutStore, updateLayout } from '../state/layout'
import { notify, reportError } from '../state/notifications'
import { openFolder } from '../state/workspace'
import {
  closePreview,
  previewsStore,
  setPreviewHandler,
  takeBesideRequest,
} from '../state/previews'
import { createStore } from '../state/store'
import { setPageHandler, setViewHandler } from '../state/viewRequests'
import { setFileTabHandler, showFileTab, viewerClosed } from '../state/viewers'
import {
  closeTerminal,
  focusTerminal,
  newTerminal,
  setActiveTerminal,
  startingTerminals,
  terminalHasFocus,
  terminalsStore,
} from '../terminal/terminals'
import {
  MAIN_GROUP,
  activateItem,
  allItems,
  emptyGroup,
  findGroup,
  groupOf,
  groups,
  itemKind,
  neighbourInDirection,
  normalize,
  parseDock,
  placeItem,
  removeItem,
  resizeSplit,
  setHidden,
  sidebarGroups,
  updateGroup,
  type DockGroup,
  type DockLayout,
  type DropTarget,
  type Edge,
  type ItemId,
  type ItemKind,
} from './model'

/** A document's tab showing its view `view` (see `ipc/editor.ts`); its first tab shows view 0. */
export const documentItem = (id: number, view = 0): ItemId =>
  view === 0 ? `document:${id}` : `document:${id}:${view}`
export const viewItem = (id: string): ItemId => `view:${id}`
export const terminalItem = (id: number): ItemId => `terminal:${id}`
export const previewItem = (id: string): ItemId => `preview:${id}`
/** A built-in page, such as `settings`. */
export const pageItem = (id: string): ItemId => `page:${id}`
/** A saved document, until the documents load. */
const fileItem = (path: string): ItemId => `file:${path}`
/** Another saved tab of a document, the `n`th (from 1). */
const savedCopyItem = (n: number, path: string): ItemId => `copy:${n}:${path}`
/**
 * A file in an extension's viewer (its full id), or, for `null`, a binary
 * file waiting for the user to open it as text.
 */
export const viewerItem = (viewer: string | null, path: string): ItemId =>
  `viewer:${viewer ?? ''}:${path}`

/** The viewer and file of a viewer item. Viewer ids have no `:`; paths may. */
export function parseViewerItem(item: ItemId): { viewer: string | null; path: string } {
  const rest = itemId(item)
  const colon = rest.indexOf(':')
  return { viewer: rest.slice(0, colon) || null, path: rest.slice(colon + 1) }
}

/** The id after the kind: a document or terminal number, or a view, preview, or page id. */
export function itemId(item: ItemId): string {
  return item.slice(item.indexOf(':') + 1)
}

/** The document of a document's tab, and the view it shows. */
export function parseDocumentItem(item: ItemId): { document: number; view: number } {
  const [document, view] = itemId(item).split(':')
  return { document: Number(document), view: Number(view ?? 0) }
}

/** The document a tab shows, if it's a document's. */
function documentOf(item: ItemId): number | null {
  return itemKind(item) === 'document' ? parseDocumentItem(item).document : null
}

/** The tabs of a document, in reading order. */
function documentTabs(layout: DockLayout, id: number): ItemId[] {
  return allItems(layout).filter(item => documentOf(item) === id)
}

/** The tab of document `id` in `group`, if it shows there. */
function documentTabIn(group: DockGroup | undefined, id: number): ItemId | undefined {
  return group?.items.find(item => documentOf(item) === id)
}

/** The saved tabs of the file at `path`, in reading order. */
function savedTabs(layout: DockLayout, path: string): ItemId[] {
  return allItems(layout).filter(
    item => item === fileItem(path) || (item.startsWith('copy:') && itemId(itemId(item)) === path),
  )
}

const BUILTIN_VIEWS = ['explorer', 'search', 'extensions'].map(viewItem)
/**
 * Views that open along the bottom with the terminals, rather than in the
 * side bar, and aren't in the activity bar unless docked in the side bar.
 */
const PANEL_VIEWS: readonly ItemId[] = ['problems', 'output'].map(viewItem)

/** Views on the left, the main group filling the rest. */
export function defaultDock(
  options: { viewsHidden?: boolean; activeView?: string } = {},
): DockLayout {
  const active = BUILTIN_VIEWS.find(item => item === viewItem(options.activeView ?? '')) ?? null
  const views: DockGroup = {
    ...emptyGroup('g1', [...BUILTIN_VIEWS]),
    ...(active ? { active } : {}),
    ...(options.viewsHidden ? { hidden: true } : {}),
  }
  return {
    main: MAIN_GROUP,
    root: {
      kind: 'split',
      id: 's1',
      direction: 'row',
      sizes: [0.2, 0.8],
      children: [views, emptyGroup(MAIN_GROUP)],
    },
  }
}

export const dockStore = createStore<DockLayout>(defaultDock())
/** The group the user last worked in. Commands such as Close Tab act on it. */
export const currentGroup = createStore<string>(MAIN_GROUP)
/** The group new documents open in, while it exists. */
let documentGroup = MAIN_GROUP
/** Where closed views were, to reopen them there. */
const closedViews = new Map<ItemId, { group: string; index: number }>()

/**
 * A closed tab Reopen Closed Tab can bring back: a view, a page, or a
 * document by path (`file:<path>`), and where it was.
 */
interface ClosedTab {
  item: ItemId
  group: string
  index: number
}

const MAX_CLOSED_TABS = 20
/** Closed tabs, most recent last. Terminals, previews, and untitled documents aren't kept. */
const closedTabs: ClosedTab[] = []

function rememberClosed(entry: ClosedTab): void {
  const existing = closedTabs.findIndex(other => other.item === entry.item)
  if (existing >= 0) {
    closedTabs.splice(existing, 1)
  }
  closedTabs.push(entry)
  if (closedTabs.length > MAX_CLOSED_TABS) {
    closedTabs.shift()
  }
}

export function hasClosedTab(): boolean {
  return closedTabs.length > 0
}

function setLayout(layout: DockLayout): void {
  dockStore.set(layout)
  // A document whose tab in use closed goes on in another.
  for (const { id } of documentsStore.get().documents) {
    const tabs = documentTabs(layout, id)
    if (tabs.length > 0 && !tabs.includes(documentItem(id, shownView(id)))) {
      noteDocumentTab(preferredTab(layout, id, tabs)!)
    }
  }
}

/** Records the tab a document is used in: its editor is the one commands act on. */
function noteDocumentTab(item: ItemId): void {
  const { document, view } = parseDocumentItem(item)
  setShownView(document, view)
}

/**
 * The tab to show document `id` in: the one in the group documents open
 * in, else the one last used.
 */
function preferredTab(
  layout: DockLayout,
  id: number,
  tabs = documentTabs(layout, id),
): ItemId | null {
  return (
    documentTabIn(findGroup(layout, documentGroup), id) ??
    tabs.find(item => item === documentItem(id, shownView(id))) ??
    tabs[0] ??
    null
  )
}

/** The tab document `id` is used in (as `preferredTab` decides). */
export function documentTab(id: number): ItemId {
  return preferredTab(dockStore.get(), id) ?? documentItem(id)
}

/** The next view number for each document's new tabs. */
const nextViews = new Map<number, number>()

function newView(id: number): number {
  const view = nextViews.get(id) ?? 1
  nextViews.set(id, view + 1)
  return view
}

/**
 * A new tab for document `id` showing a copy of its view `from`: the same
 * selections, folds, and scroll position, as when splitting an editor.
 */
function copyTab(id: number, from: number): ItemId {
  const view = newView(id)
  ipc.copyView(id, from, view).catch((error: unknown) => {
    // Then the new view starts as a copy of another.
    log('warn', `Copying view ${from} of document ${id}: ${String(error)}`)
  })
  const source = editorsFor(id).find(editor => editor.view === from)
  saveScroll(id, view, source?.scrollPosition() ?? savedScroll(id, from))
  return documentItem(id, view)
}

/** Forgets the view of a document's tab that closed while others show the document. */
function closeView(item: ItemId): void {
  const { document, view } = parseDocumentItem(item)
  saveScroll(document, view, null)
  ipc.closeView(document, view).catch((error: unknown) => {
    log('warn', `Closing view ${view} of document ${document}: ${String(error)}`)
  })
}

function setCurrent(id: string): void {
  currentGroup.set(id)
  const active = findGroup(dockStore.get(), id)?.active
  if (active && itemKind(active) === 'document') {
    documentGroup = id
  }
}

/** Where a new item goes, and the share of the dock a new group for it gets. */
function placementFor(layout: DockLayout, item: ItemId): { target: DropTarget; size: number } {
  const all = groups(layout)
  const holding = (kind: 'view' | 'terminal') =>
    all.filter(group => group.items.some(other => itemKind(other) === kind))
  /** Groups along the bottom: those with terminals or panel views. */
  const panels = all.filter(group =>
    group.items.some(other => itemKind(other) === 'terminal' || PANEL_VIEWS.includes(other)),
  )
  const appendTo = (group: DockGroup, afterActive = false): DropTarget => ({
    kind: 'tabs',
    group: group.id,
    index: afterActive && group.active ? group.items.indexOf(group.active) + 1 : group.items.length,
  })
  switch (itemKind(item)) {
    case 'document':
    case 'viewer':
    case 'page':
    case 'layout': {
      const group = findGroup(layout, documentGroup) ?? findGroup(layout, layout.main)!
      return { target: appendTo(group, true), size: 0.5 }
    }
    case 'terminal': {
      const terminals = holding('terminal')
      const group = terminals.find(g => g.id === currentGroup.get()) ?? terminals[0] ?? panels[0]
      return {
        target: group ? appendTo(group) : { kind: 'root', edge: 'bottom' },
        size: 0.3,
      }
    }
    case 'preview':
      return { target: appendTo(previewSourceGroup(layout, item), true), size: 0.5 }
    default: {
      const remembered = closedViews.get(item)
      if (remembered && findGroup(layout, remembered.group)) {
        return {
          target: { kind: 'tabs', group: remembered.group, index: remembered.index },
          size: 0.2,
        }
      }
      if (PANEL_VIEWS.includes(item)) {
        const group = panels.find(g => g.id === currentGroup.get()) ?? panels[0]
        return {
          target: group ? appendTo(group) : { kind: 'root', edge: 'bottom' },
          size: 0.3,
        }
      }
      return { target: sidebarTarget(layout, 'view'), size: 0.2 }
    }
  }
}

/** The group of a preview's document, else the group documents open in. */
function previewSourceGroup(layout: DockLayout, item: ItemId): DockGroup {
  const document = previewsStore.get().find(preview => previewItem(preview.id) === item)?.document
  const tab = document != null ? preferredTab(layout, document) : null
  return (
    (tab ? groupOf(layout, tab) : undefined) ??
    findGroup(layout, documentGroup) ??
    findGroup(layout, layout.main)!
  )
}

/**
 * Beside a preview's document: in the group to the right of the document's
 * group, or in a new group there.
 */
function besideTarget(layout: DockLayout, item: ItemId): DropTarget {
  const source = previewSourceGroup(layout, item)
  const rects = visibleGroupRects()
  const from = rects.find(entry => entry.id === source.id)
  const neighbour =
    from &&
    neighbourInDirection(
      from.rect,
      'right',
      rects.filter(entry => entry.id !== source.id),
    )
  const group = neighbour && findGroup(layout, neighbour.id)
  return group
    ? { kind: 'tabs', group: group.id, index: group.items.length }
    : { kind: 'split', group: source.id, edge: 'right' }
}

/**
 * Where an item of `kind` joins the side bar: after the items of its kind
 * there, else in a new group below the side bar's others (not along the
 * left edge, which would push them out of it), else on the left.
 */
function sidebarTarget(layout: DockLayout, kind: ReturnType<typeof itemKind>): DropTarget {
  const sidebar = sidebarGroups(layout)
  const same = sidebar.find(group => group.items.some(item => itemKind(item) === kind))
  if (same) {
    return { kind: 'tabs', group: same.id, index: same.items.length }
  }
  const last = sidebar.at(-1)
  return last ? { kind: 'split', group: last.id, edge: 'bottom' } : { kind: 'root', edge: 'left' }
}

/** Whether `item` is in the side bar. */
function inSidebar(layout: DockLayout, item: ItemId): boolean {
  return sidebarGroups(layout).some(group => group.items.includes(item))
}

/**
 * What the activity bar shows: everything in the side bar (views, terminals,
 * documents), in tab order, then the views not docked anywhere (which open
 * in the side bar), except panel views. `views` are the known view ids.
 */
export function activityBarItems(layout: DockLayout, views: readonly string[]): ItemId[] {
  const docked = new Set(allItems(layout))
  return [
    ...sidebarGroups(layout).flatMap(group => group.items),
    ...views.map(viewItem).filter(item => !docked.has(item) && !PANEL_VIEWS.includes(item)),
  ]
}

/** `layout` with `item` in it, placed by the usual rules if it wasn't. */
function ensure(layout: DockLayout, item: ItemId): DockLayout {
  if (groupOf(layout, item)) {
    return layout
  }
  const { target, size } = placementFor(layout, item)
  return placeItem(layout, item, target, size)
}

function replaceItem(layout: DockLayout, from: ItemId, to: ItemId): DockLayout {
  const group = groupOf(layout, from)
  if (!group) {
    return layout
  }
  return updateGroup(layout, group.id, g => ({
    ...g,
    items: g.items.map(item => (item === from ? to : item)),
    active: g.active === from ? to : g.active,
  }))
}

// Following the documents, terminals, and previews.

let knownDocuments = new Set<number>()
/** The active document and the activation (`activationCount`) the dock last showed. */
let shownActive: { id: number | null; count: number } = { id: null, count: -1 }
let knownTerminals = new Set<number>()
let knownPreviews = new Set<string>()

function syncDocuments(): void {
  const { documents, activeId } = documentsStore.get()
  const before = dockStore.get()
  let layout = before
  const ids = new Set(documents.map(document => document.id))
  for (const id of knownDocuments) {
    if (!ids.has(id)) {
      layout = documentTabs(layout, id).reduce(removeItem, layout)
      setShownView(id, null)
      nextViews.delete(id)
    }
  }
  /** Documents given their tabs just now. */
  const placed = new Set<number>()
  for (const document of documents) {
    if (documentTabs(layout, document.id).length > 0) {
      continue
    }
    placed.add(document.id)
    const saved = document.path === null ? [] : savedTabs(layout, document.path)
    if (saved.length === 0) {
      layout = ensure(layout, documentItem(document.id))
    }
    // The first saved tab shows view 0, the others views of their own.
    saved.forEach((item, index) => {
      const view = index === 0 ? 0 : newView(document.id)
      layout = documentTabIn(groupOf(layout, item), document.id)
        ? removeItem(layout, item)
        : replaceItem(layout, item, documentItem(document.id, view))
    })
  }
  knownDocuments = ids
  let tab: ItemId | null = null
  // The active document's tab shows when it becomes active (or is opened
  // or chosen again), or gets its tab, not on other changes to the list.
  const count = activationCount()
  const activated =
    activeId !== shownActive.id || count !== shownActive.count || placed.has(activeId ?? -1)
  shownActive = { id: activeId, count }
  if (activated && activeId !== null && ids.has(activeId)) {
    tab = documentTabIn(findGroup(layout, documentGroup), activeId) ?? null
    if (!tab && takeOpenRequest(activeId) && !placed.has(activeId)) {
      // Opened while it shows elsewhere: it opens where documents do, too.
      tab = copyTab(activeId, shownView(activeId))
      const { target, size } = placementFor(layout, tab)
      layout = placeItem(layout, tab, target, size)
    }
    tab ??= preferredTab(layout, activeId)
  }
  if (tab) {
    layout = activateItem(layout, tab)
  }
  if (layout !== before) {
    setLayout(layout)
  }
  if (tab) {
    const group = groupOf(layout, tab)
    if (group) {
      setCurrent(group.id)
    }
    noteDocumentTab(tab)
  }
}

function syncTerminals(): void {
  const { tabs } = terminalsStore.get()
  const before = dockStore.get()
  let layout = before
  const ids = new Set(tabs.map(tab => tab.id))
  for (const id of knownTerminals) {
    if (!ids.has(id)) {
      layout = removeItem(layout, terminalItem(id))
    }
  }
  for (const tab of tabs) {
    if (!knownTerminals.has(tab.id)) {
      layout = activateItem(ensure(layout, terminalItem(tab.id)), terminalItem(tab.id))
    }
  }
  knownTerminals = ids
  if (layout !== before) {
    setLayout(layout)
  }
}

/** New previews open with their document, or beside it if their extension asked. */
function syncPreviews(): void {
  const previews = previewsStore.get()
  const before = dockStore.get()
  let layout = before
  const ids = new Set(previews.map(preview => preview.id))
  for (const id of knownPreviews) {
    if (!ids.has(id)) {
      layout = removeItem(layout, previewItem(id))
    }
  }
  for (const preview of previews) {
    const item = previewItem(preview.id)
    if (groupOf(layout, item)) {
      continue
    }
    layout = takeBesideRequest(preview.id)
      ? placeItem(layout, item, besideTarget(layout, item), 0.5)
      : ensure(layout, item)
  }
  knownPreviews = ids
  if (layout !== before) {
    setLayout(layout)
  }
}

documentsStore.subscribe(syncDocuments)
terminalsStore.subscribe(syncTerminals)
previewsStore.subscribe(syncPreviews)

// Saving and restoring.

/**
 * The layout as saved: documents by path (a document's first tab in
 * reading order as `file:<path>`, the others as `copy:<n>:<path>`);
 * untitled documents and previews aren't saved.
 */
export function serializeDock(layout: DockLayout = dockStore.get()): DockLayout {
  const paths = new Map(
    documentsStore.get().documents.map(document => [document.id, document.path]),
  )
  /** Each item as saved; `null` if it isn't. */
  const saved = new Map<ItemId, ItemId | null>()
  const tabsOfPath = new Map<string, number>()
  for (const item of allItems(layout)) {
    const document = documentOf(item)
    const path = document === null ? null : paths.get(document)
    if (path) {
      const n = tabsOfPath.get(path) ?? 0
      tabsOfPath.set(path, n + 1)
      saved.set(item, n === 0 ? fileItem(path) : savedCopyItem(n, path))
    } else {
      const unsaved = itemKind(item) === 'preview'
      saved.set(item, document !== null || unsaved ? null : item)
    }
  }
  const save = (item: ItemId) => saved.get(item) ?? null
  let result = layout
  for (const group of groups(layout)) {
    result = updateGroup(result, group.id, g => {
      const items = g.items.map(save).filter((item): item is ItemId => item !== null)
      const active = g.active === null ? null : save(g.active)
      return { ...g, items, active: active ?? items[0] ?? null }
    })
  }
  return result
}

const SAVED_ITEM = /^(file:.+|copy:\d+:.+|view:.+|terminal:\d+|page:.+|viewer:[^:]*:.+|layout:.+)$/s

/**
 * Restores the saved layout (or the default, honouring the side bar
 * settings of older versions). Call before the documents and terminals load.
 */
export function restoreDock(): void {
  const saved = layoutStore.get().dock
  const legacy = (typeof saved === 'object' && saved !== null ? saved : {}) as Record<
    string,
    unknown
  >
  const layout =
    parseDock(saved, item => SAVED_ITEM.test(item)) ??
    defaultDock({
      viewsHidden: legacy.sidebarVisible === false,
      activeView: typeof legacy.activeView === 'string' ? legacy.activeView : undefined,
    })
  knownDocuments = new Set()
  nextViews.clear()
  knownTerminals = new Set()
  knownPreviews = new Set()
  closedTabs.length = 0
  setLayout(layout)
  currentGroup.set(layout.main)
  documentGroup = layout.main
  syncDocuments()
  syncTerminals()
  syncPreviews()
  prefetchLayouts(allItems(layout))
}

/** Drops saved documents that didn't open again. */
export function documentsRestored(): void {
  const layout = dockStore.get()
  const stale = allItems(layout).filter(
    item => item.startsWith('file:') || item.startsWith('copy:'),
  )
  if (stale.length > 0) {
    setLayout(stale.reduce(removeItem, layout))
  }
}

/**
 * After reattaching to the running terminals: if the saved layout showed
 * terminals but none is running (a new start of the app), starts one where
 * they were. Then drops the saved terminals that are gone.
 */
export function terminalsRestored(): Promise<void> {
  // Restoring twice (React runs effects twice in development) shares one run.
  restoringTerminals ??= restoreTerminalSlots().finally(() => {
    restoringTerminals = null
  })
  return restoringTerminals
}

let restoringTerminals: Promise<void> | null = null

async function restoreTerminalSlots(): Promise<void> {
  const running = new Set(terminalsStore.get().tabs.map(tab => terminalItem(tab.id)))
  const shown = groups(dockStore.get()).some(
    group => !group.hidden && group.items.some(item => itemKind(item) === 'terminal'),
  )
  if (running.size === 0 && shown && startingTerminals.get() === 0) {
    try {
      await newTerminal()
    } catch (error) {
      reportError('Starting a terminal', error)
    }
  }
  const alive = new Set(terminalsStore.get().tabs.map(tab => terminalItem(tab.id)))
  const layout = dockStore.get()
  const stale = allItems(layout).filter(item => itemKind(item) === 'terminal' && !alive.has(item))
  if (stale.length > 0) {
    setLayout(stale.reduce(removeItem, layout))
  }
}

// Persist with the rest of the layout, after it has been restored.
dockStore.subscribe(() => updateLayout({ dock: serializeDock() }))

// Showing, hiding, and moving.

/** Focuses an item once it's rendered: the editor, the terminal, or the view's first control. */
export function focusItem(item: ItemId): void {
  const id = itemId(item)
  switch (itemKind(item)) {
    case 'terminal':
      focusTerminal(Number(id))
      return
    case 'document': {
      const { document, view } = parseDocumentItem(item)
      requestAnimationFrame(() =>
        (editorsFor(document).find(editor => editor.view === view) ?? activeEditor.get())?.focus(),
      )
      return
    }
    case 'layout': {
      // The pane last used, or the layout's main pane. Its editor may
      // still be on its way.
      let tries = 0
      const attempt = () => {
        const group = groupOf(dockStore.get(), item)
        const content = group && contentElement(group.id)
        const pane = paneToFocus(item)
        const target =
          pane === null
            ? null
            : content?.querySelector<HTMLElement>(
                `[data-pane="${CSS.escape(pane)}"] [data-editor-input], [data-pane="${CSS.escape(pane)}"] iframe`,
              )
        if (target) {
          target.focus({ preventScroll: true })
        } else if (tries++ < 20) {
          requestAnimationFrame(attempt)
        }
      }
      requestAnimationFrame(attempt)
      return
    }
    default:
      requestAnimationFrame(() => {
        const group = groupOf(dockStore.get(), item)
        const content = group && contentElement(group.id)
        const control = content?.querySelector<HTMLElement>(
          'input, textarea, [role="tree"], [tabindex="0"], button',
        )
        ;(control ?? content)?.focus()
      })
  }
}

function contentElement(group: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-group-content="${CSS.escape(group)}"]`)
}

function groupElement(group: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-group="${CSS.escape(group)}"]`)
}

/** Whether keyboard focus is inside the group `id`. */
function hasFocus(id: string): boolean {
  return groupElement(id)?.contains(document.activeElement) ?? false
}

/** Shows `item` (adding it if needed), makes its group current, and optionally focuses it. */
export function showItem(item: ItemId, focus = true): void {
  setLayout(activateItem(ensure(dockStore.get(), item), item))
  const group = groupOf(dockStore.get(), item)
  if (group) {
    setCurrent(group.id)
  }
  noteActive(item)
  if (focus) {
    focusItem(item)
  }
}

export function showView(id: string, focus = true): void {
  showItem(viewItem(id), focus)
}

setViewHandler(showView)
setPageHandler(id => showItem(pageItem(id)))
setLayoutTabHandler(showLayout, item => void closeTab(item))
setActiveLayoutTab(() => {
  const tab = currentTab()
  return tab === null ? null : (parseLayoutItem(tab)?.layout ?? null)
})

/**
 * Shows a layout tab, adding it if needed: in the current group, or
 * (`beside`) in the group to the right of it, leaving focus where it is.
 */
function showLayout(item: ItemId, beside: boolean): void {
  const layout = dockStore.get()
  if (beside && !groupOf(layout, item)) {
    setLayout(activateItem(placeItem(layout, item, besideTarget(layout, item), 0.5), item))
    return
  }
  showItem(item)
}

/**
 * Shows a preview its extension asked to show. One opened beside its
 * document leaves focus (and the current group) in the editor, so typing
 * carries on and updates it.
 */
function showPreview(id: string, beside: boolean): void {
  const item = previewItem(id)
  if (beside) {
    setLayout(activateItem(ensure(dockStore.get(), item), item))
  } else {
    showItem(item)
  }
}

setPreviewHandler(showPreview)

/**
 * The tab showing `path` without a document, if any: a viewer, or the
 * layout the file opens in by default. (A layout the user chose, such as a
 * page beside which they edit the file, doesn't stand for the file: opening
 * the file opens its text.)
 */
function fileTabOf(layout: DockLayout, path: string): ItemId | undefined {
  return allItems(layout).find(
    item =>
      (itemKind(item) === 'viewer' && parseViewerItem(item).path === path) ||
      isDefaultLayoutOf(item, path),
  )
}

/** Whether `item` is a tab of the layout `path` opens in by default, showing it. */
function isDefaultLayoutOf(item: ItemId, path: string): boolean {
  const tab = parseLayoutItem(item)
  const declared = tab && findLayout(tab.layout)
  return (
    tab !== null &&
    tab.key === path &&
    declared !== null &&
    declared.patterns.length > 0 &&
    declared.priority === 'default'
  )
}

// A file showing in a viewer, or waiting to be opened as text: one tab per
// file, which changes viewer in place. A new one takes the place its file
// held, if any (reopening, dropping, restoring), as documents do.
setFileTabHandler((path, tab) => {
  const layout = dockStore.get()
  const existing = fileTabOf(layout, path)
  if (!tab) {
    if (existing) {
      showItem(existing)
    }
    return existing !== undefined
  }
  if (existing && itemKind(existing) === 'layout') {
    // A layout shows the file: the viewer replaces it.
    void reopenWith(existing, { viewer: tab.viewer })
    return true
  }
  const item = viewerItem(tab.viewer, tab.path)
  const saved = fileItem(path)
  let next = layout
  if (existing && existing !== item) {
    viewerClosed(existing)
    next = replaceItem(next, existing, item)
  } else if (!existing) {
    next = groupOf(next, saved) ? replaceItem(next, saved, item) : ensure(next, item)
  }
  setLayout(next)
  showItem(item)
  return true
})

/** Another way to show a tab's file: as text (`null`), in a viewer, or in a layout. */
export type EditorChoice = null | { viewer: string | null } | { layout: string }

/**
 * Shows the file of a document's, viewer's, or file layout's tab another
 * way, in the same place: as text (`null`, even if it's binary), in an
 * extension's viewer, or in a layout (whose editor keeps the document
 * open). Only that tab changes: a document showing in other groups stays
 * open there. Closing the last tab of a document with unsaved changes
 * asks first.
 */
export async function reopenWith(item: ItemId, choice: EditorChoice): Promise<void> {
  const layout = dockStore.get()
  const group = groupOf(layout, item)
  if (!group) {
    return
  }
  if (choice !== null && 'layout' in choice) {
    await reopenInLayout(item, choice.layout)
    return
  }
  const viewer = choice?.viewer ?? null
  if (itemKind(item) === 'layout') {
    await reopenFromLayout(item, viewer)
    return
  }
  if (itemKind(item) === 'document') {
    const id = parseDocumentItem(item).document
    const path = documentsStore.get().documents.find(document => document.id === id)?.path
    if (viewer === null || !path) {
      return
    }
    const tab = viewerItem(viewer, path)
    if (documentTabs(layout, id).length > 1) {
      // It shows elsewhere too: the viewer takes only this tab's place.
      closeView(item)
      setLayout(replaceItem(groupOf(layout, tab) ? removeItem(layout, tab) : layout, item, tab))
      showItem(tab)
      return
    }
    // The viewer waits beside the document while it closes, which may ask.
    const index = group.items.indexOf(item) + 1
    setLayout(placeItem(layout, tab, { kind: 'tabs', group: group.id, index }, 0.5))
    await closeDocument(id)
    if (isOpen(id)) {
      setLayout(removeItem(dockStore.get(), tab))
    } else {
      showItem(tab)
    }
    return
  }
  if (itemKind(item) !== 'viewer') {
    return
  }
  const { path } = parseViewerItem(item)
  if (viewer !== null) {
    showFileTab({ viewer, path })
    return
  }
  const open = documentsStore.get().documents.find(document => document.path === path)
  if (open) {
    // Open as text elsewhere: it shows here too, in the viewer's place.
    const there = documentTabIn(group, open.id)
    const tab = there ?? copyTab(open.id, shownView(open.id))
    viewerClosed(item)
    setLayout(there ? removeItem(layout, item) : replaceItem(layout, item, tab))
    showItem(tab)
    return
  }
  // The document takes the place when it opens (`syncDocuments`).
  const saved = fileItem(path)
  viewerClosed(item)
  setLayout(replaceItem(layout, item, saved))
  const summary = await openPath(path, undefined, { asText: true })
  if (!summary && groupOf(dockStore.get(), saved)) {
    setLayout(replaceItem(dockStore.get(), saved, item))
  }
}

/**
 * Shows a tab's file in a layout, in the tab's place. A document's tab
 * hands its document to the layout, which keeps it open (hidden) for its
 * editor pane.
 */
async function reopenInLayout(item: ItemId, layoutId: string): Promise<void> {
  const path =
    itemKind(item) === 'document'
      ? (documentsStore.get().documents.find(d => d.id === parseDocumentItem(item).document)
          ?.path ?? null)
      : itemKind(item) === 'viewer'
        ? parseViewerItem(item).path
        : (parseLayoutItem(item)?.key ?? null)
  if (path === null) {
    return
  }
  const target = layoutItem({ layout: layoutId, key: path })
  if (target === item) {
    return
  }
  const layout = dockStore.get()
  if (groupOf(layout, target)) {
    // Already open: that tab shows, and this one closes.
    showItem(target)
    await closeTab(item)
    return
  }
  if (itemKind(item) === 'document') {
    const id = parseDocumentItem(item).document
    const document = documentsStore.get().documents.find(d => d.id === id)
    if (!document) {
      return
    }
    if (documentTabs(layout, id).length > 1) {
      closeView(item)
    } else {
      adoptDocument(target, path, document)
    }
    noteOpening({ layout: layoutId, key: path })
    setLayout(replaceItem(dockStore.get(), item, target))
    showItem(target)
    if (documentTabs(dockStore.get(), id).length === 0) {
      // Its own tab is gone: it stays open, hidden, for the layout.
      await closeDocument(id)
    }
    return
  }
  if (itemKind(item) === 'viewer') {
    viewerClosed(item)
  } else if (!(await releaseLayout(item))) {
    return
  }
  noteOpening({ layout: layoutId, key: path })
  setLayout(replaceItem(dockStore.get(), item, target))
  showItem(target)
}

/** Shows a file layout's file as text, or in a viewer, in the layout tab's place. */
async function reopenFromLayout(item: ItemId, viewer: string | null): Promise<void> {
  const tab = parseLayoutItem(item)
  if (!tab) {
    return
  }
  const path = tab.key
  if (viewer !== null) {
    if (!(await releaseLayout(item))) {
      return
    }
    const target = viewerItem(viewer, path)
    setLayout(replaceItem(dockStore.get(), item, target))
    showItem(target)
    return
  }
  // The document's tab takes the layout's place when it opens.
  const saved = fileItem(path)
  setLayout(replaceItem(dockStore.get(), item, saved))
  const summary = await openPath(path, undefined, { asText: true })
  // The layout's documents close unless the document's tab shows them now.
  await releaseLayout(item)
  if (!summary && groupOf(dockStore.get(), saved)) {
    setLayout(removeItem(dockStore.get(), saved))
  }
}

// Files from other programs dropped where nothing takes them open where documents do.
setDropFallback(drag => {
  if (drag.payload.kind === 'files') {
    void openDroppedFiles(drag.payload.files, drag.payload.folders, null)
  }
})

/** Records the item as the one in use, for the stores that care. */
function noteActive(item: ItemId): void {
  // A layout tab's editor in use is the active editor (Save, Find, and the
  // like act on it): the focused pane's, or else the one last used.
  setActiveOverride(null)
  activeTabChanged(item)
  if (itemKind(item) === 'layout') {
    const focused = (window.document.activeElement as HTMLElement | null)?.closest<HTMLElement>(
      '[data-pane]',
    )?.dataset.pane
    const pane = focused ?? paneToFocus(item)
    const editor = pane === null ? null : paneEditor(item, pane)
    if (editor) {
      setActiveOverride(editor)
    }
  }
  const document = documentOf(item)
  if (document !== null) {
    noteDocumentTab(item)
    if (documentsStore.get().activeId !== document) {
      activateDocument(document)
    }
  } else if (itemKind(item) === 'terminal') {
    setActiveTerminal(Number(itemId(item)))
  }
}

/** A tab was chosen: shows and focuses it. */
export function activateTab(item: ItemId, focus = true): void {
  showItem(item, focus)
}

/** Focus moved into a group: it becomes the current group. */
export function groupFocused(id: string): void {
  if (currentGroup.get() === id) {
    return
  }
  setCurrent(id)
  const active = findGroup(dockStore.get(), id)?.active
  if (active) {
    noteActive(active)
  }
}

/**
 * The activity bar's buttons: shows an item, or, when it's already the
 * shown tab of its group, hides that group (as collapsing a side bar does).
 * A view that isn't docked opens in the side bar, where its button is.
 */
export function toggleItem(item: ItemId): void {
  const layout = dockStore.get()
  const group = groupOf(layout, item)
  if (group && !group.hidden && group.active === item) {
    hideGroups([group.id])
    return
  }
  if (!group) {
    const remembered = closedViews.get(item)
    const target =
      remembered && sidebarGroups(layout).some(g => g.id === remembered.group)
        ? ({ kind: 'tabs', group: remembered.group, index: remembered.index } as const)
        : sidebarTarget(layout, 'view')
    setLayout(placeItem(layout, item, target, 0.2))
  }
  showItem(item)
}

/** An item dropped on the activity bar: moves it into the side bar. */
export function dockInSidebar(item: ItemId): void {
  const layout = dockStore.get()
  if (inSidebar(layout, item)) {
    showItem(item)
  } else {
    moveItem(item, sidebarTarget(layout, itemKind(item)))
    focusItem(item)
  }
}

/** Hides groups, moving focus to the editor if it was in one of them. */
function hideGroups(ids: readonly string[]): void {
  const hadFocus = ids.some(hasFocus)
  setLayout(ids.reduce((layout, id) => setHidden(layout, id, true), dockStore.get()))
  if (hadFocus) {
    activeEditor.get()?.focus()
  }
}

function showGroups(ids: readonly string[]): void {
  setLayout(ids.reduce((layout, id) => setHidden(layout, id, false), dockStore.get()))
}

/**
 * The bottom panel: groups holding only terminals and panel views such as
 * Problems (never the main group). They show and hide together.
 */
function panelGroups(): DockGroup[] {
  const layout = dockStore.get()
  return groups(layout).filter(
    group =>
      group.id !== layout.main &&
      group.items.length > 0 &&
      group.items.every(item => itemKind(item) === 'terminal' || PANEL_VIEWS.includes(item)),
  )
}

/**
 * Hides the side bar, or shows it again (View: Toggle Side Bar). With
 * nothing in it, opens the first built-in view not docked elsewhere there.
 */
export function toggleSidebar(): void {
  const layout = dockStore.get()
  const sidebar = sidebarGroups(layout).map(group => group.id)
  if (sidebar.some(id => !findGroup(layout, id)!.hidden)) {
    hideGroups(sidebar)
  } else if (sidebar.length > 0) {
    showGroups(sidebar)
  } else {
    const undocked = BUILTIN_VIEWS.find(item => !groupOf(layout, item))
    if (undocked) {
      setLayout(placeItem(layout, undocked, sidebarTarget(layout, 'view'), 0.2))
      showItem(undocked, false)
    }
  }
}

/** The terminal to show: the last used one, else any. */
function preferredTerminal(): ItemId | null {
  const { tabs, activeId } = terminalsStore.get()
  const id = activeId ?? tabs.at(-1)?.id
  return id === undefined ? null : terminalItem(id)
}

/** Starts a terminal with a profile (the default if `null`) and shows it. */
export async function openTerminal(profile: string | null = null): Promise<void> {
  const id = await newTerminal(profile)
  showItem(terminalItem(id))
}

/** Shows and focuses a terminal, starting one if none is open. */
export async function focusActiveTerminal(): Promise<void> {
  const item = preferredTerminal()
  if (item) {
    showGroups(panelGroups().map(group => group.id))
    showItem(item)
  } else {
    await openTerminal()
  }
}

/**
 * Shows and focuses the terminal; from a terminal, hides the bottom panel
 * (terminals, Problems, Output) and returns to the editor.
 */
export async function toggleTerminal(): Promise<void> {
  if (terminalHasFocus()) {
    const groupsToHide = panelGroups().map(group => group.id)
    if (groupsToHide.length > 0) {
      hideGroups(groupsToHide)
    } else {
      activeEditor.get()?.focus()
    }
  } else {
    await focusActiveTerminal()
  }
}

/** Hides the bottom panel, or shows it (starting a terminal if there's none). */
export async function toggleTerminals(): Promise<void> {
  const terminalGroups = panelGroups()
  if (terminalGroups.some(group => !group.hidden)) {
    hideGroups(terminalGroups.map(group => group.id))
  } else {
    await focusActiveTerminal()
  }
}

/** Closes a tab: closes the document, ends the terminal, or closes the view, preview, or page. */
export function closeItem(item: ItemId): void {
  void closeTab(item)
}

const isOpen = (id: number) => documentsStore.get().documents.some(document => document.id === id)

/**
 * Closes a tab, remembering it for Reopen Closed Tab. Resolves to whether
 * it closed: closing a document with unsaved changes can be cancelled.
 */
async function closeTab(item: ItemId): Promise<boolean> {
  const group = groupOf(dockStore.get(), item)
  const hadFocus = group ? hasFocus(group.id) : false
  const place = group && { group: group.id, index: group.items.indexOf(item) }
  const id = itemId(item)
  switch (itemKind(item)) {
    case 'document': {
      const { document } = parseDocumentItem(item)
      const path = documentsStore.get().documents.find(d => d.id === document)?.path
      const remember = () => path && place && rememberClosed({ item: fileItem(path), ...place })
      if (documentTabs(dockStore.get(), document).length > 1) {
        // It shows elsewhere too: only this tab closes.
        closeView(item)
        setLayout(removeItem(dockStore.get(), item))
        remember()
        break
      }
      await closeDocument(document)
      const closed = !isOpen(document)
      if (closed) {
        remember()
      }
      return closed
    }
    case 'terminal':
      closeTerminal(Number(id))
      break
    case 'preview':
      closePreview(id)
      setLayout(removeItem(dockStore.get(), item))
      break
    case 'viewer':
      viewerClosed(item)
      if (place) {
        rememberClosed({ item, ...place })
      }
      setLayout(removeItem(dockStore.get(), item))
      break
    case 'page':
      if (place) {
        rememberClosed({ item, ...place })
      }
      setLayout(removeItem(dockStore.get(), item))
      break
    case 'layout':
      if (!(await releaseLayout(item))) {
        return false
      }
      if (place) {
        rememberClosed({ item, ...place })
      }
      setLayout(removeItem(dockStore.get(), item))
      break
    default:
      if (place) {
        closedViews.set(item, place)
        rememberClosed({ item, ...place })
      }
      setLayout(removeItem(dockStore.get(), item))
  }
  // Keep focus in the group, or go back to the editor.
  const next = group && findGroup(dockStore.get(), group.id)?.active
  if (hadFocus) {
    if (next) {
      showItem(next)
    } else {
      activeEditor.get()?.focus()
    }
  }
  return true
}

/** The current group, if it still exists. */
function current(): DockGroup | undefined {
  return findGroup(dockStore.get(), currentGroup.get())
}

/** The current group's active tab, if any. */
export function currentTab(): ItemId | null {
  return current()?.active ?? null
}

export function hasCurrentTab(): boolean {
  return current()?.active != null
}

export function closeCurrentTab(): void {
  const active = current()?.active
  if (active) {
    closeItem(active)
  }
}

/**
 * Closes tabs one at a time, the last first, so Reopen Closed Tab puts them
 * back in order. Stops if closing one is cancelled.
 */
async function closeTabs(items: readonly ItemId[]): Promise<void> {
  for (const item of [...items].reverse()) {
    if (!(await closeTab(item))) {
      return
    }
  }
}

/** Tabs that are editors: documents, viewers, previews, and pages, rather than views and terminals. */
const isEditorTab = (item: ItemId) => {
  const kind = itemKind(item)
  return (
    kind === 'document' ||
    kind === 'viewer' ||
    kind === 'preview' ||
    kind === 'page' ||
    kind === 'layout'
  )
}

const isDirty = (item: ItemId) =>
  layoutDirty(item) ||
  documentsStore.get().documents.some(d => d.id === documentOf(item) && d.dirty)

/** Closes the current group's tabs, or those `keep` doesn't keep. */
export function closeGroupTabs(keep: (item: ItemId) => boolean = () => false): Promise<void> {
  const items = current()?.items ?? []
  return closeTabs(items.filter(item => itemKind(item) !== null && !keep(item)))
}

export const closeOtherTabs = () => closeGroupTabs(item => item === current()?.active)

/** Closes the current group's documents without unsaved changes, previews, and pages. */
export const closeSavedTabs = () => closeGroupTabs(item => !isEditorTab(item) || isDirty(item))

/** Closes every document, preview, and page, leaving views and terminals. */
export function closeAllEditors(): Promise<void> {
  return closeTabs(allItems(dockStore.get()).filter(isEditorTab))
}

/**
 * Reopens the most recently closed tab that's still closed: where it was,
 * if its group remains, else where it would open.
 */
export async function reopenClosedTab(): Promise<void> {
  for (let entry = closedTabs.pop(); entry; entry = closedTabs.pop()) {
    const { item, group, index } = entry
    const path = item.startsWith('file:') ? itemId(item) : null
    // A document still showing counts as closed where its group lacks it.
    const document = documentsStore.get().documents.find(document => document.path === path)
    const there = findGroup(dockStore.get(), group)
    const open =
      path === null
        ? groupOf(dockStore.get(), item) !== undefined
        : document !== undefined && (!there || documentTabIn(there, document.id) !== undefined)
    if (open) {
      continue
    }
    const target = findGroup(dockStore.get(), group)
      ? ({ kind: 'tabs', group, index } as const)
      : null
    if (path === null) {
      if (target) {
        setLayout(placeItem(dockStore.get(), item, target, 0.2))
      }
      showItem(item)
      return
    }
    const opened = await openPathAt(path, target)
    if (opened) {
      showItem(opened)
    }
    return
  }
}

/**
 * Opens the file at `path` at `target`, or where documents open for `null`,
 * and resolves to its tab (`null` if it didn't open). An open document gets
 * a tab there (`placeDocument`). A new one's saved item (`file:<path>`)
 * holds the place until it opens and takes it (`syncDocuments`).
 */
async function openPathAt(path: string, target: DropTarget | null): Promise<ItemId | null> {
  const open = documentsStore.get().documents.find(document => document.path === path)
  if (open && target) {
    return placeDocument(open.id, target)
  }
  const saved = fileItem(path)
  if (target) {
    setLayout(placeItem(dockStore.get(), saved, target, 0.5))
  }
  const summary = await openPath(path)
  // In a viewer instead, or asking whether to open it as text.
  const tab = summary ? undefined : fileTabOf(dockStore.get(), path)
  if (groupOf(dockStore.get(), saved)) {
    // It didn't open (`openPath` reported why), opened under another
    // spelling of its path, or showed in a tab it already had; then that
    // moves to the place.
    setLayout(removeItem(dockStore.get(), saved))
    if (summary && target) {
      return placeDocument(summary.id, target)
    }
    if (tab && target) {
      moveItem(tab, target)
    }
  }
  return summary ? documentTab(summary.id) : (tab ?? null)
}

/**
 * Shows an open document at `target`: its tab in that group moves there,
 * or, if it has none there, a new tab of it does.
 */
function placeDocument(id: number, target: DropTarget): ItemId {
  const there =
    target.kind === 'tabs' ? documentTabIn(findGroup(dockStore.get(), target.group), id) : undefined
  const tab = there ?? copyTab(id, shownView(id))
  moveItem(tab, target)
  return tab
}

/**
 * Opens files and folders dropped from another program: the files at
 * `target` (the first there, the rest after it), or where documents open for
 * `null`, then the first folder as the open folder.
 */
export async function openDroppedFiles(
  files: readonly string[],
  folders: readonly string[],
  target: DropTarget | null,
): Promise<void> {
  let next = target
  let last: ItemId | null = null
  for (const path of files) {
    const item = await openPathAt(path, next)
    const group = item && groupOf(dockStore.get(), item)
    if (item && group) {
      last = item
      next = next && { kind: 'tabs', group: group.id, index: group.items.indexOf(item) + 1 }
    }
  }
  if (last) {
    showItem(last)
  }
  const [folder, ...others] = folders
  if (folder) {
    if (others.length > 0) {
      notify('info', `Flareite opens one folder at a time, so it opened ${baseName(folder)}.`)
    }
    await openFolder(folder)
  }
}

const baseName = (path: string) => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path

/** Activates the current group's `n`th tab (from 1), or its last for 0. */
export function activateTabNumber(n: number): void {
  const items = current()?.items ?? []
  const item = n === 0 ? items.at(-1) : items[n - 1]
  if (item) {
    showItem(item)
  }
}

/** Activates the tab `step` places from the current one, in the current group. */
export function cycleTab(step: number): void {
  const group = current()
  if (!group || group.items.length === 0) {
    return
  }
  const index = group.active ? group.items.indexOf(group.active) : 0
  const next = group.items[(index + step + group.items.length) % group.items.length]!
  showItem(next)
}

/**
 * Moves an item (dragged, or from a command) and makes it current. A
 * document's tab moved into a group that shows the document already joins
 * the tab there, as a group shows a document once.
 */
export function moveItem(item: ItemId, target: DropTarget): void {
  const layout = dockStore.get()
  const kind = itemKind(item)
  const document = documentOf(item)
  const twin =
    document !== null && target.kind === 'tabs'
      ? documentTabIn(findGroup(layout, target.group), document)
      : undefined
  if (twin && twin !== item) {
    closeView(item)
    setLayout(removeItem(layout, item))
    moveItem(twin, target)
    return
  }
  const next = placeItem(layout, item, target, kind === 'terminal' ? 0.3 : 0.2)
  if (next === layout) {
    return
  }
  setLayout(next)
  const group = groupOf(next, item)
  if (group) {
    setCurrent(group.id)
  }
  noteActive(item)
}

/**
 * Copies a tab to `target` (dragged with the copy key held, or split off):
 * a document shows there too, in a view of its own. Anything else can be
 * in one place only, so it moves. Returns the tab at the target.
 */
export function copyItem(item: ItemId, target: DropTarget): ItemId {
  const document = documentOf(item)
  const from = groupOf(dockStore.get(), item)
  if (document === null || (target.kind === 'tabs' && target.group === from?.id)) {
    moveItem(item, target)
    return item
  }
  const there =
    target.kind === 'tabs'
      ? documentTabIn(findGroup(dockStore.get(), target.group), document)
      : undefined
  const tab = there ?? copyTab(document, parseDocumentItem(item).view)
  moveItem(tab, target)
  return tab
}

/** The groups on screen, with their rectangles. */
function visibleGroupRects(): { id: string; rect: DOMRect }[] {
  return [...document.querySelectorAll<HTMLElement>('[data-group]')].map(element => ({
    id: element.dataset.group!,
    rect: element.getBoundingClientRect(),
  }))
}

/**
 * Moves the current tab into the neighbouring group on one side, or into a
 * new group on that side when there is none.
 */
export function moveCurrentTab(edge: Edge): void {
  const group = current()
  const item = group?.active
  if (!group || !item) {
    return
  }
  const rects = visibleGroupRects()
  const from = rects.find(entry => entry.id === group.id)
  const neighbour =
    from &&
    neighbourInDirection(
      from.rect,
      edge,
      rects.filter(entry => entry.id !== group.id),
    )
  if (neighbour) {
    const target = findGroup(dockStore.get(), neighbour.id)
    moveItem(item, { kind: 'tabs', group: neighbour.id, index: target?.items.length ?? 0 })
  } else {
    moveItem(item, { kind: 'split', group: group.id, edge })
  }
  focusItem(item)
}

/**
 * Splits the current tab off into a new group beside its group: a document
 * shows there too, as in VS Code; anything else moves there.
 */
export function splitCurrentTab(edge: Edge): void {
  const group = current()
  if (group?.active) {
    focusItem(copyItem(group.active, { kind: 'split', group: group.id, edge }))
  }
}

/** Focuses the next (or previous) visible group, in reading order. */
export function focusGroupRelative(step: number): void {
  const rects = visibleGroupRects()
  if (rects.length === 0) {
    return
  }
  const index = rects.findIndex(entry => entry.id === currentGroup.get())
  focusGroup(rects[(index + step + rects.length) % rects.length]!.id)
}

/**
 * Focuses the `n`th visible group (from 1) outside the side bar, in reading
 * order: with the default layout, 1 is the main group.
 */
export function focusGroupNumber(n: number): void {
  const sidebar = new Set(sidebarGroups(dockStore.get()).map(group => group.id))
  const target = visibleGroupRects().filter(entry => !sidebar.has(entry.id))[n - 1]
  if (target) {
    focusGroup(target.id)
  }
}

function focusGroup(id: string): void {
  const group = findGroup(dockStore.get(), id)
  setCurrent(id)
  if (group?.active) {
    showItem(group.active)
  } else {
    contentElement(id)?.focus()
  }
}

export function resize(splitId: string, sizes: number[]): void {
  setLayout(resizeSplit(dockStore.get(), splitId, sizes))
}

/**
 * Puts everything back where it starts out: views on the left, documents in
 * the main group, terminals along the bottom.
 */
export function resetLayout(): void {
  const old = dockStore.get()
  // Documents all go to the main group, so each keeps one tab there: the one in use.
  const items = allItems(old).filter(item => {
    const document = documentOf(item)
    if (document === null || item === preferredTab(old, document)) {
      return true
    }
    closeView(item)
    return false
  })
  let layout = defaultDock()
  const placeAll = (kind: ItemKind) => {
    for (const item of items.filter(other => itemKind(other) === kind)) {
      if (!groupOf(layout, item)) {
        const { target, size } = placementFor(layout, item)
        layout = placeItem(layout, item, target, size)
      }
    }
  }
  closedViews.clear()
  documentGroup = MAIN_GROUP
  currentGroup.set(MAIN_GROUP)
  placeAll('view')
  placeAll('document')
  placeAll('viewer')
  placeAll('page')
  placeAll('preview')
  placeAll('terminal')
  setLayout(normalize(layout))
  const { activeId } = documentsStore.get()
  if (activeId !== null) {
    showItem(documentTab(activeId), false)
  }
}
