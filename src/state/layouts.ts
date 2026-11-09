/**
 * Layout tabs (API 1.15): tabs an extension splits into panes, such as
 * Git's diff and merge editors and HTML & CSS's preview beside its editor.
 *
 * A tab is named by its layout (`<extension id>.<layout id>`) and a key the
 * extension chooses (a file's path, for a layout with patterns). The
 * backend keeps what each open tab shows as its extension set it; this
 * module fetches it, asks the extension to fill a tab in when the backend
 * has nothing for it (`opened` when the workbench or the extension opened
 * it, `restored` when it came back with the window), and keeps its
 * documents open while it shows them.
 *
 * Its documents open hidden, as other tabs' documents do: the backend keeps
 * them but they have no tab of their own. Closing a document's own tab
 * while a layout shows it only hides it, and the last layout tab showing a
 * hidden document closes it, asking to save unsaved changes first. Once a
 * tab's documents are open, the extension is told (`ready`), so it can read
 * them and decorate the panes.
 */
import * as ipc from '../ipc/editor'
import type { DocumentSummary, Position } from '../ipc/editor'
import {
  closeWebPage,
  fillLayout,
  getLayout,
  getPaneDecorations,
  layoutClosed,
  layoutFocus,
  layoutReady,
  runLayoutCommand,
  type LayoutContribution,
  type LayoutSpec,
  type LayoutView,
  type PaneSpec,
} from '../ipc/extensions'
import {
  activeEditor,
  editorsFor,
  selectRange,
  type EditorController,
} from '../editor/activeEditor'
import { documentsStore, setKeptOpen, setLayoutOpener } from './documents'
import { extensionsStore, patternMatcher, usableExtensions } from './extensions'
import { reportError } from './notifications'
import { createStore } from './store'
import { webviewClosed } from './webviews'

export interface LayoutTab {
  /** `<extension id>.<layout id>`. */
  layout: string
  key: string
}

export interface LayoutTabState {
  /** What it shows; `null` until fetched, or while the extension fills it in. */
  view: LayoutView | null
  /** Why it couldn't be filled in. */
  error: string | null
  /** Its open documents, by the address its layout names them by. */
  documents: Readonly<Record<string, DocumentSummary>>
  /** Its editor panes whose documents have unsaved changes. */
  dirty: readonly string[]
}

/** The open layout tabs, by dock item. */
export const layoutsStore = createStore<Readonly<Record<string, LayoutTabState>>>({})

const PREFIX = 'layout:'

/** A layout tab's dock item. */
export const layoutItem = ({ layout, key }: LayoutTab): string =>
  `${PREFIX}${JSON.stringify([layout, key])}`

/** What tab a dock item is; `null` if it isn't a layout's. */
export function parseLayoutItem(item: string): LayoutTab | null {
  if (!item.startsWith(PREFIX)) {
    return null
  }
  try {
    const parsed: unknown = JSON.parse(item.slice(PREFIX.length))
    if (
      Array.isArray(parsed) &&
      parsed.length === 2 &&
      parsed.every(part => typeof part === 'string')
    ) {
      const [layout, key] = parsed as [string, string]
      return { layout, key }
    }
  } catch {
    // Not one of ours.
  }
  return null
}

/** Whether a document's side is an address an extension provides, rather than a file. */
export const isAddress = (uri: string): boolean => /^[a-z][a-z0-9+.-]{1,31}:/.test(uri)

/** The panes of a layout, by id. */
export function panesOf(spec: LayoutSpec): [string, PaneSpec][] {
  return spec.nodes.flatMap(node => (node.kind === 'pane' ? [[node.id, node] as const] : []))
}

/** The ids of a layout's web page panes' views. */
const webPagesOf = (spec: LayoutSpec): string[] =>
  panesOf(spec).flatMap(([, pane]) => (pane.content.type === 'webPage' ? [pane.content.id] : []))

/** Every document a layout keeps open: its editors', then the others. */
export function documentsOf(spec: LayoutSpec): string[] {
  const editors = panesOf(spec).flatMap(([, pane]) =>
    pane.content.type === 'editor' ? [pane.content.uri] : [],
  )
  return [...new Set([...editors, ...spec.documents])]
}

/** The layouts of usable extensions, by full id. */
export function findLayout(id: string): LayoutContribution | null {
  for (const extension of usableExtensions(extensionsStore.get())) {
    const layout = extension.contributions.layouts.find(candidate => candidate.fullId === id)
    if (layout) {
      return layout
    }
  }
  return null
}

const fileName = (path: string) => path.split(/[/\\]/).pop() ?? path

/** The layouts for files whose names match `path`'s, defaults first. */
export function layoutsFor(path: string): LayoutContribution[] {
  const name = fileName(path)
  const matching = usableExtensions(extensionsStore.get()).flatMap(extension =>
    extension.contributions.layouts.filter(
      layout =>
        layout.patterns.length > 0 &&
        layout.patterns.some(pattern => patternMatcher(pattern).test(name)),
    ),
  )
  return [
    ...matching.filter(layout => layout.priority === 'default'),
    ...matching.filter(layout => layout.priority !== 'default'),
  ]
}

/** The layout `path` opens in, unless the user chooses another way. */
export function defaultLayoutFor(path: string): LayoutContribution | null {
  return layoutsFor(path).find(layout => layout.priority === 'default') ?? null
}

/** A tab's file: its main pane's (or else first editor's) file, or a file layout's key. */
export function layoutFile(item: string): string | null {
  const tab = parseLayoutItem(item)
  if (!tab) {
    return null
  }
  const spec = layoutsStore.get()[item]?.view?.spec
  if (spec) {
    const panes = panesOf(spec)
    const main = panes.find(([id]) => id === spec.main)
    const files = (main ? [main, ...panes] : panes).flatMap(([, pane]) =>
      pane.content.type === 'editor' && !isAddress(pane.content.uri)
        ? [pane.content.uri]
        : pane.content.type === 'filePage'
          ? [pane.content.path]
          : [],
    )
    if (files[0]) {
      return files[0]
    }
  }
  return (findLayout(tab.layout)?.patterns.length ?? 0) > 0 ? tab.key : null
}

function update(item: string, change: (state: LayoutTabState) => LayoutTabState | null): void {
  layoutsStore.update(tabs => {
    const current = tabs[item] ?? { view: null, error: null, documents: {}, dirty: [] }
    const next = change(current)
    if (next === current) {
      return tabs
    }
    const { [item]: _, ...rest } = tabs
    return next ? { ...rest, [item]: next } : rest
  })
}

/**
 * Views numbered from here are layouts' editor panes: the backend doesn't
 * remember their folds, which are the extension's (`PANE_VIEWS` in
 * `src-tauri/src/editor/mod.rs`).
 */
const PANE_VIEWS = 2 ** 30
let nextView = PANE_VIEWS
const paneViews = new Map<string, Map<string, number>>()

/** The view an editor pane shows its document in, the same each time it's shown. */
export function paneView(item: string, pane: string): number {
  let views = paneViews.get(item)
  if (!views) {
    views = new Map()
    paneViews.set(item, views)
  }
  let view = views.get(pane)
  if (view === undefined) {
    view = nextView++
    views.set(pane, view)
  }
  return view
}

/** Tabs the workbench or an extension opened, which haven't been filled in yet. */
const opened = new Set<string>()

/** Tabs being fetched or filled in, so a tab shown again meanwhile doesn't ask twice. */
const loading = new Set<string>()

/**
 * Shows a tab's layout: fetches it, or, when the backend has none, asks
 * the extension to fill the tab in (`width` is the tab's, in pixels).
 */
export async function loadLayout(item: string, width: number): Promise<void> {
  const tab = parseLayoutItem(item)
  if (!tab || loading.has(item) || layoutsStore.get()[item]?.view?.spec) {
    return
  }
  loading.add(item)
  try {
    const view = await getLayout(tab.layout, tab.key)
    if (view?.spec) {
      update(item, state => ({ ...state, view, error: null }))
      await openDocuments(item)
      return
    }
    const reason = opened.delete(item) ? 'opened' : 'restored'
    update(item, state => ({ ...state, error: null }))
    await fillLayout(tab.layout, tab.key, reason, Math.round(width))
    // The extension's layout arrives as an event (`layoutChanged`).
    if (!layoutsStore.get()[item]?.view?.spec) {
      const filled = await getLayout(tab.layout, tab.key)
      if (filled?.spec) {
        update(item, state => ({ ...state, view: filled }))
        await openDocuments(item)
      } else {
        update(item, state => ({
          ...state,
          error: 'The extension didn’t say what this tab shows.',
        }))
      }
    }
  } catch (error) {
    update(item, state => ({
      ...state,
      error: error instanceof Error ? error.message : String(error),
    }))
  } finally {
    loading.delete(item)
  }
}

/**
 * Fills in restored layout tabs that aren't shown yet, once their
 * extensions have loaded, so their tabs have their titles. (Shown ones
 * fill themselves in.)
 */
export function prefetchLayouts(items: readonly string[]): void {
  const pending = new Set(items.filter(item => parseLayoutItem(item) !== null))
  if (pending.size === 0) {
    return
  }
  let stop: (() => void) | null = null
  const attempt = () => {
    for (const item of [...pending]) {
      const tab = parseLayoutItem(item)
      if (!tab || layoutsStore.get()[item]?.view) {
        pending.delete(item)
      } else if (findLayout(tab.layout)) {
        pending.delete(item)
        void loadLayout(item, 0)
      }
    }
    if (pending.size === 0) {
      stop?.()
    }
  }
  stop = extensionsStore.subscribe(attempt)
  attempt()
}

/** Tries filling a tab in again, after it failed. */
export function retryLayout(item: string, width: number): void {
  update(item, state => ({ ...state, error: null, view: null }))
  void loadLayout(item, width)
}

/** The tab crossed one of its breakpoints. */
export function layoutResized(item: string, width: number): void {
  const tab = parseLayoutItem(item)
  if (tab) {
    fillLayout(tab.layout, tab.key, 'resized', Math.round(width)).catch((error: unknown) =>
      reportError('Laying out the tab', error),
    )
  }
}

/** An extension set what a tab shows. */
export async function layoutChanged(tab: LayoutTab): Promise<void> {
  const item = layoutItem(tab)
  if (!layoutsStore.get()[item] && !loading.has(item)) {
    // Not shown yet: it's fetched when it is.
    return
  }
  try {
    const view = await getLayout(tab.layout, tab.key)
    if (!view?.spec) {
      return
    }
    // Web pages the layout no longer shows close.
    const before = layoutsStore.get()[item]?.view?.spec
    const kept = new Set(webPagesOf(view.spec))
    for (const id of before ? webPagesOf(before) : []) {
      if (!kept.has(id)) {
        closeWebPage(id).catch(() => {})
      }
    }
    update(item, state => ({ ...state, view, error: null }))
    await openDocuments(item)
  } catch (error) {
    reportError('Showing the tab', error)
  }
}

/** What an editor pane shows besides its text changed. */
export async function paneDecorationsChanged(tab: LayoutTab, pane: string): Promise<void> {
  const item = layoutItem(tab)
  if (!layoutsStore.get()[item]?.view) {
    return
  }
  try {
    const decorations = await getPaneDecorations(tab.layout, tab.key, pane)
    update(item, state => {
      if (!state.view) {
        return state
      }
      const { [pane]: _, ...others } = state.view.decorations
      return {
        ...state,
        view: {
          ...state.view,
          decorations: decorations ? { ...others, [pane]: decorations } : others,
        },
      }
    })
  } catch (error) {
    reportError('Showing the pane', error)
  }
}

const openDocument = (uri: string) =>
  isAddress(uri) ? ipc.openProvidedDocument(uri, true) : ipc.openDocument(uri, false, true)

/** Each tab's document opening, one at a time. */
const openings = new Map<string, Promise<void>>()

/**
 * Opens a tab's documents (hidden) that aren't open for it, closes those
 * it no longer shows, and tells the extension they're ready.
 */
function openDocuments(item: string): Promise<void> {
  const run = async () => {
    const spec = layoutsStore.get()[item]?.view?.spec
    const tab = parseLayoutItem(item)
    if (!spec || !tab) {
      return
    }
    const wanted = documentsOf(spec)
    const have = layoutsStore.get()[item]?.documents ?? {}
    const missing = wanted.filter(uri => !have[uri])
    const results = await Promise.allSettled(missing.map(openDocument))
    if (!layoutsStore.get()[item]) {
      // Closed meanwhile.
      await closeUnused(
        results.flatMap(result => (result.status === 'fulfilled' ? [result.value.id] : [])),
      )
      return
    }
    const failure = results.find(result => result.status === 'rejected')
    const added: Record<string, DocumentSummary> = {}
    results.forEach((result, index) => {
      if (result.status === 'fulfilled') {
        added[missing[index]!] = result.value
      }
    })
    const dropped = Object.entries(have).filter(([uri]) => !wanted.includes(uri))
    update(item, state => {
      const documents = { ...state.documents, ...added }
      for (const [uri] of dropped) {
        delete documents[uri]
      }
      return { ...state, documents }
    })
    await closeUnused(dropped.map(([, document]) => document.id))
    if (failure?.status === 'rejected') {
      reportError('Opening the tab’s documents', failure.reason)
    }
    const documents = layoutsStore.get()[item]?.documents ?? {}
    try {
      await layoutReady(
        tab.layout,
        tab.key,
        Object.entries(documents).map(([uri, document]) => ({ uri, document: document.id })),
      )
    } catch (error) {
      reportError('Showing the tab', error)
    }
  }
  const next = (openings.get(item) ?? Promise.resolve()).then(run, run)
  openings.set(item, next)
  return next
}

/**
 * Takes a document the user had open into a layout tab that's about to
 * replace its tab (Reopen Editor With), so it stays open, hidden, when its
 * own tab closes.
 */
export function adoptDocument(item: string, uri: string, document: DocumentSummary): void {
  update(item, state => ({ ...state, documents: { ...state.documents, [uri]: document } }))
}

/** Whether a layout tab shows document `id`. */
export function layoutShows(id: number, except?: string): boolean {
  return Object.entries(layoutsStore.get()).some(
    ([item, state]) =>
      item !== except && Object.values(state.documents).some(document => document.id === id),
  )
}

const listed = (id: number) => documentsStore.get().documents.some(document => document.id === id)

/** Closes the documents among `ids` that no tab shows. Resolves to false if one stayed open. */
async function closeUnused(ids: number[], except?: string): Promise<boolean> {
  for (const id of new Set(ids)) {
    if (!listed(id) && !layoutShows(id, except) && !(await ipc.closeDocument(id))) {
      return false
    }
  }
  return true
}

/**
 * A layout tab is closing: closes its documents that no other tab shows,
 * asking to save unsaved changes, and tells its extension. Resolves to
 * false if the user cancelled, when the tab stays.
 */
export async function releaseLayout(item: string): Promise<boolean> {
  const state = layoutsStore.get()[item]
  const tab = parseLayoutItem(item)
  if (state) {
    const documents = Object.values(state.documents)
    if (
      !(await closeUnused(
        documents.map(document => document.id),
        item,
      ))
    ) {
      return false
    }
    // The documents other tabs show forget this tab's views.
    const views = paneViews.get(item)
    const spec = state.view?.spec
    if (views && spec) {
      for (const [pane, content] of panesOf(spec)) {
        const view = views.get(pane)
        const document =
          content.content.type === 'editor' ? state.documents[content.content.uri] : undefined
        if (view !== undefined && document) {
          ipc.closeView(document.id, view).catch(() => {
            // Closed with the document.
          })
        }
      }
    }
    for (const [, pane] of spec ? panesOf(spec) : []) {
      if (pane.content.type === 'webPage') {
        closeWebPage(pane.content.id).catch(() => {})
      } else if (pane.content.type !== 'editor') {
        webviewClosed(pane.content.id)
      }
    }
  }
  paneViews.delete(item)
  focusedPanes.delete(item)
  opened.delete(item)
  update(item, () => null)
  if (tab) {
    layoutClosed(tab.layout, tab.key).catch((error: unknown) =>
      reportError('Closing the tab', error),
    )
  }
  return true
}

/** Notes whether an editor pane's document has unsaved changes, for its tab. */
export function setPaneDirty(item: string, pane: string, dirty: boolean): void {
  update(item, state => {
    if (state.dirty.includes(pane) === dirty) {
      return state
    }
    return {
      ...state,
      dirty: dirty ? [...state.dirty, pane] : state.dirty.filter(other => other !== pane),
    }
  })
}

export const layoutDirty = (item: string): boolean =>
  (layoutsStore.get()[item]?.dirty.length ?? 0) > 0

/** The pane last focused in each tab. */
const focusedPanes = new Map<string, string>()

/** The editor pane the backend thinks is in use, to tell it only of changes. */
let reportedPane: string | null = null

function reportFocus(item: string | null, pane: string | null): void {
  const tab = item === null ? null : parseLayoutItem(item)
  const key = tab && pane ? JSON.stringify([item, pane]) : null
  if (key === reportedPane) {
    return
  }
  reportedPane = key
  layoutFocus(tab && pane ? tab.layout : null, tab && pane ? tab.key : null, tab && pane).catch(
    () => {
      // The backend is gone; nothing to tell.
    },
  )
}

/** An editor pane got focus. */
export function paneFocused(item: string, pane: string): void {
  focusedPanes.set(item, pane)
  reportFocus(item, pane)
}

/** The pane to focus when a tab is shown: the one last focused, or its main pane. */
export function paneToFocus(item: string): string | null {
  const spec = layoutsStore.get()[item]?.view?.spec
  const last = focusedPanes.get(item)
  if (!spec) {
    return last ?? null
  }
  const panes = panesOf(spec)
  if (last && panes.some(([id]) => id === last)) {
    return last
  }
  return (
    spec.main ??
    panes.find(([, pane]) => pane.content.type === 'editor')?.[0] ??
    panes[0]?.[0] ??
    null
  )
}

/** The editor showing a tab's editor pane, if it's on screen. */
export function paneEditor(item: string, pane: string): EditorController | null {
  const state = layoutsStore.get()[item]
  const content =
    state?.view?.spec && panesOf(state.view.spec).find(([id]) => id === pane)?.[1].content
  const document = state && content?.type === 'editor' ? state.documents[content.uri] : undefined
  if (!document) {
    return null
  }
  const view = paneView(item, pane)
  return editorsFor(document.id).find(editor => editor.view === view) ?? null
}

/** The active tab changed: the backend hears which pane of it is in use, if it's a layout. */
export function activeTabChanged(item: string | null): void {
  if (item === null || !parseLayoutItem(item)) {
    reportFocus(null, null)
    return
  }
  const pane = paneToFocus(item)
  const spec = layoutsStore.get()[item]?.view?.spec
  const isEditor =
    pane !== null &&
    spec?.nodes.some(
      node => node.id === pane && node.kind === 'pane' && node.content.type === 'editor',
    )
  reportFocus(item, isEditor ? pane : null)
}

/** Runs a button's or zone's command for a tab (`pane` `null`: the tab's own buttons). */
export function runLayoutButton(
  item: string,
  pane: string | null,
  command: string,
  argument: string | null,
  title: string,
): void {
  const tab = parseLayoutItem(item)
  if (!tab) {
    return
  }
  runLayoutCommand(
    tab.layout,
    tab.key,
    pane,
    command,
    argument,
    activeEditor.get()?.documentId ?? null,
  ).catch((error: unknown) => reportError(title, error))
}

/** Selects a range in an editor pane, showing its tab first if needed. */
export function revealInPane(tab: LayoutTab, pane: string, start: Position, end: Position): void {
  const item = layoutItem(tab)
  const state = layoutsStore.get()[item]
  const spec = state?.view?.spec
  const content = spec && panesOf(spec).find(([id]) => id === pane)?.[1].content
  if (!state || content?.type !== 'editor') {
    return
  }
  const document = state.documents[content.uri]
  if (!document) {
    return
  }
  layoutTabHandler?.(item, false)
  const view = paneView(item, pane)
  let tries = 0
  const attempt = () => {
    const editor = editorsFor(document.id).find(candidate => candidate.view === view)
    if (editor) {
      selectRange(editor, start, end, 'center')
    } else if (tries++ < 10) {
      requestAnimationFrame(attempt)
    }
  }
  attempt()
}

/** Shows a layout tab, opening it if needed; set by the dock. */
type LayoutTabHandler = (item: string, beside: boolean) => void

let layoutTabHandler: LayoutTabHandler | null = null
let layoutTabCloser: ((item: string) => void) | null = null

export function setLayoutTabHandler(show: LayoutTabHandler, close: (item: string) => void): void {
  layoutTabHandler = show
  layoutTabCloser = close
}

/**
 * Notes that the workbench is opening a layout tab itself (it puts the tab
 * in place), so the extension hears `opened` rather than `restored`.
 */
export function noteOpening(tab: LayoutTab): void {
  const item = layoutItem(tab)
  if (!layoutsStore.get()[item]?.view?.spec) {
    opened.add(item)
  }
}

/** Opens a layout tab (for the user, or because an extension asked), or shows it. */
export function openLayoutTab(tab: LayoutTab, beside = false): void {
  noteOpening(tab)
  layoutTabHandler?.(layoutItem(tab), beside)
}

/** An extension asked to close one of its tabs. */
export function closeLayoutTab(tab: LayoutTab): void {
  layoutTabCloser?.(layoutItem(tab))
}

/** How many times each document was reloaded: zones showing its lines fetch them again. */
export const reloadsStore = createStore<Readonly<Record<number, number>>>({})

/** A document's text was replaced (its file changed, or its extension provided it again). */
export function noteReloaded(id: number): void {
  if (layoutShows(id)) {
    reloadsStore.update(reloads => ({ ...reloads, [id]: (reloads[id] ?? 0) + 1 }))
  }
}

setKeptOpen(id => layoutShows(id))
setLayoutOpener(path => {
  const layout = defaultLayoutFor(path)
  if (!layout) {
    return false
  }
  openLayoutTab({ layout: layout.fullId, key: path })
  return true
})
