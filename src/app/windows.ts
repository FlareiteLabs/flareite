/**
 * The workbench's windows: the main one, and the windows tabs are dragged
 * out into (ADR 0046).
 *
 * The main window's page draws every window. Another window is opened with
 * `window.open` on a blank page of Flareite's own (`public/window.html`),
 * which the backend turns into a native window (`src-tauri/src/windows.rs`);
 * the workbench copies its styles there and renders into it through a React
 * portal. So the stores, editors, and terminals stay in this one page, and
 * code that touches the page must use the document (and window) the element
 * is in, rather than the globals: `ownerWindow`, `onEveryWindow`, and
 * `activeElement` here.
 */
import { isDesktop, log } from '../ipc'
import { closeWindow, prepareWindow, type WindowBounds } from '../ipc/windows'
import { createStore } from '../state/store'

/** A window: `null` for the main one, else another window's id (`w1`, ...). */
export type WindowId = string | null

/** Another window, open and ready to draw in. */
export interface OpenWindow {
  id: string
  window: Window
  /** The element the workbench draws the window's content in. */
  root: HTMLElement
}

/** The windows open and ready, in the order they opened. */
export const openWindows = createStore<readonly OpenWindow[]>([])

/** The window the user was last in. */
export const focusedWindow = createStore<WindowId>(null)

/** Windows by how recently they had focus, most recent first. */
let focusOrder: WindowId[] = [null]

function noteFocus(id: WindowId): void {
  focusOrder = [id, ...focusOrder.filter(other => other !== id)]
  if (focusedWindow.get() !== id) {
    focusedWindow.set(id)
  }
}

if (typeof window !== 'undefined') {
  window.addEventListener('focus', () => noteFocus(null))
}

/** The open windows (the main one first) by how recently they had focus. */
export function windowsByFocus(): WindowId[] {
  const open = new Set<WindowId>([null, ...openWindows.get().map(entry => entry.id)])
  return [
    ...focusOrder.filter(id => open.has(id)),
    ...[...open].filter(id => !focusOrder.includes(id)),
  ]
}

/** The `Window` of a window, if it's open. */
export function windowObject(id: WindowId): Window | null {
  return id === null ? window : (openWindows.get().find(entry => entry.id === id)?.window ?? null)
}

/** Every window's `Window`, the main one first, with its id. */
export function allWindows(): { id: WindowId; window: Window }[] {
  return [
    { id: null, window },
    ...openWindows.get().map(entry => ({ id: entry.id, window: entry.window })),
  ]
}

// Another window's nodes and events are made by its page's `Node`,
// `Element`, ... (another realm), so `instanceof` this page's fails for
// them. These test what a value is in any window.

/** Whether `value` is a DOM node, in any window. */
export function isNode(value: unknown): value is Node {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as Node).nodeType === 'number' &&
    typeof (value as Node).nodeName === 'string'
  )
}

/** Whether `value` is an element, in any window. */
export function isElement(value: unknown): value is Element {
  return isNode(value) && value.nodeType === 1
}

/** Whether `value` is an HTML element, in any window. */
export function isHTMLElement(value: unknown): value is HTMLElement {
  return (
    isElement(value) && 'dataset' in value && typeof (value as HTMLElement).focus === 'function'
  )
}

/** The `Window` whose page `node` is in (the main window's for none). */
export function ownerWindow(node: Node | null | undefined): Window {
  const document = node?.nodeType === 9 ? (node as Document) : node?.ownerDocument
  return document?.defaultView ?? window
}

/**
 * The `ResizeObserver` of the window `node` is in: one of another window
 * follows the main window's frames, which stop while it's minimized.
 */
export function resizeObserverFor(node: Node | null | undefined): typeof ResizeObserver {
  const view = ownerWindow(node) as Window & { ResizeObserver?: typeof ResizeObserver }
  return view.ResizeObserver ?? ResizeObserver
}

/** The window `node` is in, or `undefined` if it isn't one of the workbench's. */
export function windowIdOf(node: Node | Window | null | undefined): WindowId | undefined {
  if (!node) {
    return undefined
  }
  const view = 'document' in node && node.document ? (node as Window) : ownerWindow(node as Node)
  return allWindows().find(entry => entry.window === view)?.id
}

/** The document of the window the user was last in. */
export function focusedDocument(): Document {
  return (windowObject(focusedWindow.get()) ?? window).document
}

/** The focused element of the window the user was last in. */
export function activeElement(): Element | null {
  return focusedDocument().activeElement
}

/** Every window's elements matching `selector`, the main window's first. */
export function queryAll<T extends Element = HTMLElement>(selector: string): T[] {
  return allWindows().flatMap(entry => [...entry.window.document.querySelectorAll<T>(selector)])
}

/** The first element matching `selector` in any window. */
export function query<T extends Element = HTMLElement>(selector: string): T | null {
  for (const entry of allWindows()) {
    const found = entry.window.document.querySelector<T>(selector)
    if (found) {
      return found
    }
  }
  return null
}

/**
 * Runs `callback` before the next frame of the window `node` is in (the
 * window the user is in, without one). A window's frames stop while it's
 * minimized, so this must be the window that draws what the callback
 * touches.
 */
export function afterFrame(callback: () => void, node?: Node | null): () => void {
  const view = node ? ownerWindow(node) : (windowObject(focusedWindow.get()) ?? window)
  const frame = view.requestAnimationFrame(callback)
  return () => view.cancelAnimationFrame(frame)
}

type WindowSetup = (window: Window, id: WindowId) => (() => void) | void

const setups = new Set<WindowSetup>()
/** What each setup left to undo, by window. */
const undo = new Map<WindowId, Map<WindowSetup, () => void>>()

function runSetup(setup: WindowSetup, view: Window, id: WindowId): void {
  const cleanup = setup(view, id)
  if (cleanup) {
    let cleanups = undo.get(id)
    if (!cleanups) {
      cleanups = new Map()
      undo.set(id, cleanups)
    }
    cleanups.set(setup, cleanup)
  }
}

/**
 * Runs `setup` for every window, now and as they open (adding listeners to
 * each, say), and what it returns as they close. Returns a function that
 * undoes it in every window.
 */
export function onEveryWindow(setup: WindowSetup): () => void {
  setups.add(setup)
  for (const entry of allWindows()) {
    runSetup(setup, entry.window, entry.id)
  }
  return () => {
    setups.delete(setup)
    for (const cleanups of undo.values()) {
      cleanups.get(setup)?.()
      cleanups.delete(setup)
    }
  }
}

/** Undoes what `attach` did to each window. */
const detachers = new Map<string, () => void>()

function windowClosed(id: string): void {
  detachers.get(id)?.()
  detachers.delete(id)
  const cleanups = undo.get(id)
  undo.delete(id)
  for (const cleanup of cleanups?.values() ?? []) {
    cleanup()
  }
  focusOrder = focusOrder.filter(other => other !== id)
  if (focusedWindow.get() === id) {
    focusedWindow.set(focusOrder[0] ?? null)
  }
}

// Opening and closing.

/** The page another window shows (`public/window.html`); its fragment names the window. */
const PAGE = '/window.html'
/** How long a new window's page may take to load. */
const OPEN_TIMEOUT_MS = 10_000

const opening = new Map<string, Promise<OpenWindow | null>>()
/** Windows being closed on purpose; their pages going away is expected. */
const closing = new Set<string>()

/** Whether window `id` is open or opening. */
export function isWindowOpen(id: string): boolean {
  return opening.has(id) || openWindows.get().some(entry => entry.id === id)
}

/**
 * Opens window `id` at `bounds` (physical pixels; the system places it
 * without), and resolves to it once its page is ready, or `null` if it
 * couldn't open.
 */
export function openWindow(
  id: string,
  bounds: WindowBounds | null,
  maximized: boolean,
): Promise<OpenWindow | null> {
  const existing = openWindows.get().find(entry => entry.id === id)
  if (existing) {
    return Promise.resolve(existing)
  }
  let pending = opening.get(id)
  if (!pending) {
    pending = open(id, bounds, maximized).finally(() => opening.delete(id))
    opening.set(id, pending)
  }
  return pending
}

async function open(
  id: string,
  bounds: WindowBounds | null,
  maximized: boolean,
): Promise<OpenWindow | null> {
  if (!isDesktop()) {
    return null
  }
  try {
    await prepareWindow(id, bounds, maximized)
  } catch (error) {
    log('warn', `Preparing window ${id}: ${String(error)}`)
    return null
  }
  const view = window.open(`${PAGE}#${id}`, '_blank')
  if (!view) {
    log('warn', `Window ${id} didn't open`)
    return null
  }
  if (!(await pageReady(view))) {
    log('warn', `Window ${id}'s page didn't load`)
    return null
  }
  return attach(id, view)
}

/** Waits for a window's page (not the blank one it starts with) to load. */
async function pageReady(view: Window): Promise<boolean> {
  const deadline = Date.now() + OPEN_TIMEOUT_MS
  while (Date.now() < deadline) {
    if (view.closed) {
      return false
    }
    try {
      if (view.location.pathname === PAGE && view.document.readyState === 'complete') {
        return true
      }
    } catch {
      // Not yet our page.
    }
    await new Promise(resolve => setTimeout(resolve, 16))
  }
  return false
}

/** Readies a window's page to draw in: styles, root element, and listeners. */
function attach(id: string, view: Window): OpenWindow {
  const page = view.document
  const root = page.createElement('div')
  root.id = 'root'
  page.body.replaceChildren(root)
  const stopMirroring = mirrorPage(page)
  const focus = () => noteFocus(id)
  view.addEventListener('focus', focus)
  if (page.hasFocus()) {
    noteFocus(id)
  }
  // Its page going away without being closed (reloaded with F5, say): draw it again.
  const lost = () => {
    if (closing.has(id)) {
      return
    }
    removeEntry(id)
    void pageReady(view).then(ready => {
      if (ready && !closing.has(id)) {
        attach(id, view)
      }
    })
  }
  view.addEventListener('pagehide', lost, { once: true })
  const entry: OpenWindow = { id, window: view, root }
  openWindows.update(entries => [...entries.filter(other => other.id !== id), entry])
  for (const setup of setups) {
    runSetup(setup, view, id)
  }
  detachers.set(id, () => {
    stopMirroring()
    view.removeEventListener('focus', focus)
    view.removeEventListener('pagehide', lost)
  })
  return entry
}

function removeEntry(id: string): void {
  openWindows.update(entries => entries.filter(entry => entry.id !== id))
  windowClosed(id)
}

/** Closes window `id`: stops drawing in it, then closes it. */
export async function closeOpenWindow(id: string): Promise<void> {
  closing.add(id)
  try {
    await opening.get(id)
    removeEntry(id)
    if (isDesktop()) {
      await closeWindow(id)
    }
  } catch (error) {
    log('warn', `Closing window ${id}: ${String(error)}`)
  } finally {
    closing.delete(id)
  }
}

/**
 * Keeps a window's page styled as the main one: its style sheets (Vite
 * replaces them as files change in development) and the attributes of its
 * `<html>` element (the theme, and the colors and fonts set on it).
 */
function mirrorPage(page: Document): () => void {
  const clones = new Map<Element, Element>()
  const syncStyles = () => {
    const sources = [
      ...document.head.querySelectorAll('style, link[rel="stylesheet"]'),
    ] as HTMLElement[]
    for (const source of sources) {
      let clone = clones.get(source)
      if (!clone) {
        clone = page.importNode(source, true)
        if (source.tagName === 'LINK') {
          // The page's address differs; links are made absolute.
          ;(clone as HTMLLinkElement).href = (source as HTMLLinkElement).href
        }
        clones.set(source, clone)
      } else if (source.tagName === 'STYLE' && clone.textContent !== source.textContent) {
        clone.textContent = source.textContent
      }
      // In the main page's order: later sheets win.
      page.head.appendChild(clone)
    }
    for (const [source, clone] of clones) {
      if (!sources.includes(source as HTMLElement)) {
        clone.remove()
        clones.delete(source)
      }
    }
  }
  const syncAttributes = () => {
    const from = document.documentElement
    const to = page.documentElement
    for (const { name } of [...to.attributes]) {
      if (!from.hasAttribute(name)) {
        to.removeAttribute(name)
      }
    }
    for (const { name, value } of [...from.attributes]) {
      if (to.getAttribute(name) !== value) {
        to.setAttribute(name, value)
      }
    }
  }
  syncStyles()
  syncAttributes()
  const styles = new MutationObserver(syncStyles)
  styles.observe(document.head, { childList: true, subtree: true, characterData: true })
  const attributes = new MutationObserver(syncAttributes)
  attributes.observe(document.documentElement, { attributes: true })
  return () => {
    styles.disconnect()
    attributes.disconnect()
  }
}
