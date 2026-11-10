/**
 * Dragging in the workbench: tabs, activity bar buttons, and explorer rows,
 * moved with the pointer, and files dragged in from other programs, which
 * the backend reports as they move (`fileDrag` events).
 *
 * HTML5 drag and drop isn't used. Files dragged in from other programs come
 * through Tauri's drop handler, which on Windows replaces WebView2's, and
 * HTML5 drags stop working with it. So a drag here is a pointer held down
 * and moved. Drop targets register on elements (`useDropTarget`), and the
 * element under the pointer and its ancestors are asked, innermost first,
 * whether they take the drag, as `dragover` bubbles.
 *
 * A drag goes on over the workbench's other windows (ADR 0046): the
 * pointer is captured, so its events keep coming to the window the drag
 * started in, and where it is on screen tells which window it's over (the
 * backend says where each is, `windowsGeometry`). Dropped outside every
 * window, a tab goes to a new window there (the fallback, `dock.ts`).
 */
import { useCallback, useRef, type PointerEvent as ReactPointerEvent } from 'react'
import {
  allWindows,
  isHTMLElement,
  ownerWindow,
  windowIdOf,
  windowObject,
  windowsByFocus,
  type WindowId,
} from '../app/windows'
import { isMac } from '../editor/keymap'
import { isDesktop } from '../ipc'
import { windowsGeometry, type WindowGeometry } from '../ipc/windows'
import { createStore } from '../state/store'

export type DragPayload =
  /** A tab or activity bar button. */
  | { kind: 'dock'; item: string; label: string }
  /** Files and folders dragged in the explorer. */
  | { kind: 'entries'; paths: readonly string[]; label: string }
  /**
   * Files and folders dragged in from another program. `dropId` names the
   * drop to the backend, once dropped (for copying them).
   */
  | { kind: 'files'; files: readonly string[]; folders: readonly string[]; dropId: number | null }

export interface Drag {
  payload: DragPayload
  /** The pointer, in CSS pixels from the top left of the page of `window`. */
  x: number
  y: number
  /**
   * The window the pointer is over: `null` for the main one, `undefined`
   * outside every window.
   */
  window: WindowId | undefined
  /** The pointer on screen, in physical pixels, if known. */
  screen: { x: number; y: number } | null
  /** Whether the copy key is held: Ctrl, or Option on macOS. Never for files from other programs. */
  copy: boolean
}

export interface DropHandlers {
  /** Whether this element takes the drag where the pointer is. Called as it moves. */
  over(drag: Drag, element: HTMLElement): boolean
  drop(drag: Drag, element: HTMLElement): void
  /** The drag left this element: it ended, or the pointer moved to another target. */
  leave?(): void
}

/** The drag under way, if any. */
export const dragStore = createStore<Drag | null>(null)

/** Pointer movement before a press becomes a drag, in pixels. */
const DRAG_THRESHOLD = 4
/** Dragging this close to the edge of a scrolling area scrolls it, in pixels. */
const SCROLL_EDGE = 24
/** Pixels per frame at the very edge. */
const SCROLL_SPEED = 12

const targets = new WeakMap<Element, { current: DropHandlers }>()
/** The target that took the drag where the pointer last was. */
let active: { element: HTMLElement; handlers: { current: DropHandlers } } | null = null
/** What a drop nothing takes does. */
let fallback: ((drag: Drag) => void) | null = null

/** Makes an element a drop target; pass the result as its `ref`. */
export function useDropTarget<T extends HTMLElement>(
  handlers: DropHandlers,
): (element: T | null) => void {
  const ref = useRef(handlers)
  ref.current = handlers
  return useCallback((element: T | null) => {
    if (element) {
      targets.set(element, ref)
    }
  }, [])
}

/** Handles drops no target takes, such as files dropped on the status bar. */
export function setDropFallback(handler: (drag: Drag) => void): void {
  fallback = handler
}

/** The element under the pointer, in the window it's over. */
export function elementAt(drag: Drag): Element | null {
  const view = drag.window === undefined ? null : windowObject(drag.window)
  return view?.document.elementFromPoint(drag.x, drag.y) ?? null
}

/** Asks the targets under the pointer, innermost first, until one takes the drag. */
function hitTest(drag: Drag): void {
  let taken: typeof active = null
  for (let element = elementAt(drag); element; element = element.parentElement) {
    const handlers = targets.get(element)
    if (handlers?.current.over(drag, element as HTMLElement)) {
      taken = { element: element as HTMLElement, handlers }
      break
    }
  }
  if (active && active.element !== taken?.element) {
    // Leaving may clear what the new target just showed, so it's asked again.
    active.handlers.current.leave?.()
    taken?.handlers.current.over(drag, taken.element)
  }
  active = taken
}

function update(drag: Drag): void {
  dragStore.set(drag)
  hitTest(drag)
}

/** Ends the drag: drops it (on the target under the pointer, else the fallback), or cancels it. */
function finish(drag: Drag | null): void {
  const target = active
  active = null
  for (const { window: view } of allWindows()) {
    delete view.document.body.dataset.dragging
  }
  if (drag) {
    if (target) {
      target.handlers.current.drop(drag, target.element)
    } else {
      fallback?.(drag)
    }
  }
  target?.handlers.current.leave?.()
  dragStore.set(null)
}

const copyHeld = (event: { ctrlKey: boolean; altKey: boolean }) =>
  isMac() ? event.altKey : event.ctrlKey

/**
 * Starts watching a press for a drag (call from `onPointerDown`). Once the
 * pointer moves a few pixels, `payload` says what's dragged (or `null` for
 * nothing). Escape cancels. The click that ends a drag doesn't reach the
 * element.
 */
export function startDrag(
  event: ReactPointerEvent<HTMLElement>,
  payload: () => DragPayload | null,
): void {
  if (event.button !== 0 || event.pointerType === 'touch' || dragStore.get()) {
    return
  }
  const source = event.currentTarget
  // The pointer's events come to the window it was pressed in.
  const view = ownerWindow(source)
  const sourceWindow = windowIdOf(view) ?? null
  const { pointerId } = event
  const start = { x: event.clientX, y: event.clientY }
  let drag: Drag | null = null
  let geometry: readonly WindowGeometry[] | null = null

  const move = (e: PointerEvent) => {
    if (e.pointerId !== pointerId) {
      return
    }
    if (!drag) {
      if (Math.hypot(e.clientX - start.x, e.clientY - start.y) < DRAG_THRESHOLD) {
        return
      }
      const what = payload()
      if (!what) {
        stop()
        return
      }
      drag = {
        payload: what,
        ...locate(e, view, sourceWindow, geometry),
        copy: copyHeld(e),
      }
      try {
        // Keeps the pointer's events coming over iframes and outside the window.
        source.setPointerCapture?.(pointerId)
      } catch {
        // The pointer is already gone.
      }
      view.getSelection()?.removeAllRanges()
      for (const { window: other } of allWindows()) {
        other.document.body.dataset.dragging = ''
      }
      autoScroll(() => drag, view)
      if (isDesktop()) {
        windowsGeometry()
          .then(found => {
            geometry = found
          })
          .catch(() => {})
      }
    }
    e.preventDefault()
    drag = { ...drag, ...locate(e, view, sourceWindow, geometry), copy: copyHeld(e) }
    update(drag)
  }
  const up = (e: PointerEvent) => {
    if (e.pointerId !== pointerId) {
      return
    }
    const dropped = drag && { ...drag, ...locate(e, view, sourceWindow, geometry) }
    stop()
    if (dropped) {
      swallowClick(view)
      const target = dropped.window === undefined ? null : windowObject(dropped.window)
      if (target && target !== view) {
        swallowClick(target)
      }
      if (dropped.window !== drag?.window) {
        update(dropped)
      }
      finish(dropped)
    }
  }
  const cancel = () => {
    const cancelled = drag
    stop()
    if (cancelled) {
      finish(null)
    }
  }
  const key = (e: KeyboardEvent) => {
    if (!drag) {
      return
    }
    if (e.key === 'Escape') {
      e.preventDefault()
      e.stopImmediatePropagation()
      cancel()
    } else if (copyHeld(e) !== drag.copy) {
      drag = { ...drag, copy: copyHeld(e) }
      update(drag)
    }
  }
  // The window losing focus ends the drag; elements blur as the press moves
  // focus, and those blurs reach this capturing listener too.
  const blur = (e: FocusEvent) => {
    if (e.target === view) {
      cancel()
    }
  }
  const listeners = [
    ['pointermove', move],
    ['pointerup', up],
    ['pointercancel', cancel],
    ['keydown', key],
    ['keyup', key],
    ['blur', blur],
  ] as const
  const stop = () => {
    drag = null
    for (const [type, listener] of listeners) {
      view.removeEventListener(type, listener as EventListener, true)
    }
  }
  for (const [type, listener] of listeners) {
    view.addEventListener(type, listener as EventListener, true)
  }
}

/**
 * Where the pointer is: over the window the drag started in (which is in
 * front, as the press was there), else over the window in front of the
 * others there (by how recently they had focus), else outside them all.
 */
function locate(
  event: { clientX: number; clientY: number },
  view: Window,
  sourceWindow: WindowId,
  geometry: readonly WindowGeometry[] | null,
): Pick<Drag, 'x' | 'y' | 'window' | 'screen'> {
  const { clientX: x, clientY: y } = event
  const inside = x >= 0 && y >= 0 && x < view.innerWidth && y < view.innerHeight
  const from = geometry?.find(entry => entry.window === sourceWindow)
  const screen = from
    ? { x: from.x + x * view.devicePixelRatio, y: from.y + y * view.devicePixelRatio }
    : null
  if (inside || !screen) {
    return { x, y, window: inside ? sourceWindow : undefined, screen }
  }
  for (const id of windowsByFocus()) {
    const entry = id === sourceWindow ? undefined : geometry?.find(other => other.window === id)
    const other = entry && windowObject(id)
    if (
      entry &&
      other &&
      screen.x >= entry.x &&
      screen.y >= entry.y &&
      screen.x < entry.x + entry.width &&
      screen.y < entry.y + entry.height
    ) {
      const ratio = other.devicePixelRatio
      return {
        x: (screen.x - entry.x) / ratio,
        y: (screen.y - entry.y) / ratio,
        window: id,
        screen,
      }
    }
  }
  return { x, y, window: undefined, screen }
}

/** Keeps the click that ends a drag from activating what's under it. */
function swallowClick(view: Window): void {
  const swallow = (event: MouseEvent) => {
    event.preventDefault()
    event.stopPropagation()
  }
  view.addEventListener('click', swallow, { capture: true, once: true })
  setTimeout(() => view.removeEventListener('click', swallow, true), 0)
}

/** The nearest ancestor that scrolls, and the directions it can. */
function scroller(
  element: Element | null,
): { element: HTMLElement; x: boolean; y: boolean } | null {
  for (let node = element; isHTMLElement(node); node = node.parentElement) {
    const style = ownerWindow(node).getComputedStyle(node)
    const y = /auto|scroll/.test(style.overflowY) && node.scrollHeight > node.clientHeight
    const x = /auto|scroll/.test(style.overflowX) && node.scrollWidth > node.clientWidth
    if (x || y) {
      return { element: node, x, y }
    }
  }
  return null
}

/**
 * While a drag lasts, scrolls the area under the pointer when it's near its
 * edge, in frames of `view` (the window the drag started in).
 */
function autoScroll(current: () => Drag | null, view: Window): void {
  const step = (distance: number) =>
    distance < SCROLL_EDGE ? Math.ceil(SCROLL_SPEED * (1 - Math.max(0, distance) / SCROLL_EDGE)) : 0
  const tick = () => {
    const drag = current()
    if (!drag) {
      return
    }
    const area = scroller(elementAt(drag))
    if (area) {
      const r = area.element.getBoundingClientRect()
      const dy = area.y ? step(r.bottom - drag.y) - step(drag.y - r.top) : 0
      const dx = area.x ? step(r.right - drag.x) - step(drag.x - r.left) : 0
      if (dx !== 0 || dy !== 0) {
        area.element.scrollBy(dx, dy)
        hitTest(drag)
      }
    }
    view.requestAnimationFrame(tick)
  }
  view.requestAnimationFrame(tick)
}

/** A file drag from another program, as the backend reports it (physical pixels). */
export interface FileDragEvent {
  /** The window it's over: another window's id, `null` for the main one. */
  window: string | null
  phase: 'enter' | 'over' | 'drop' | 'leave'
  x: number
  y: number
  files: readonly string[]
  folders: readonly string[]
  dropId: number | null
}

/** Follows a file drag from another program. */
export function fileDrag(event: FileDragEvent): void {
  const current = dragStore.get()
  if (event.phase === 'leave') {
    if (current?.payload.kind === 'files') {
      finish(null)
    }
    return
  }
  const view = windowObject(event.window)
  if (!view) {
    return
  }
  const x = event.x / view.devicePixelRatio
  const y = event.y / view.devicePixelRatio
  const payload =
    event.phase === 'over' && current?.payload.kind === 'files'
      ? current.payload
      : ({
          kind: 'files',
          files: event.files,
          folders: event.folders,
          dropId: event.dropId,
        } as const)
  if (payload.files.length === 0 && payload.folders.length === 0) {
    return
  }
  const drag: Drag = { payload, x, y, window: event.window, screen: null, copy: false }
  update(drag)
  if (event.phase === 'drop') {
    finish(drag)
  }
}
