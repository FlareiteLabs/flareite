/**
 * Opens and closes native windows as the dock's layout gains and loses
 * windows (ADR 0046), and records where they are, to open them there again.
 *
 * - A window the layout gains opens where it was saved (or where its tab
 *   was dropped). One that can't open gives its tabs back to the main
 *   window, so nothing is lost.
 * - A window the layout loses (its last tab left, or it was closed) closes.
 * - Closing a window (its close button) closes its tabs, asking about
 *   unsaved changes; once they're all closed, it closes.
 */
import { closeOpenWindow, isWindowOpen, openWindow, openWindows } from '../app/windows'
import { isDesktop, log } from '../ipc'
import { resetWindows, setWindowsZoom, type WindowBounds } from '../ipc/windows'
import { layoutStore, updateLayout } from '../state/layout'
import {
  closeWindowTabs,
  dockStore,
  enableWindows,
  moveWindowTabsToMain,
  rememberWindow,
} from './dock'

/** Windows whose tabs are closing because the user closed the window. */
const closingTabs = new Set<string>()

/** Opens and closes windows to match the layout. */
function sync(): void {
  const wanted = new Set((dockStore.get().windows ?? []).map(window => window.id))
  // After the change that added a window, where it goes is recorded
  // (`toNewWindow` records it once the window is in the layout).
  queueMicrotask(openWanted)
  for (const { id } of openWindows.get()) {
    if (!wanted.has(id)) {
      void closeOpenWindow(id)
    }
  }
  // Forget where closed windows were.
  const saved = layoutStore.get().windows
  if (Object.keys(saved).some(id => !wanted.has(id))) {
    updateLayout({
      windows: Object.fromEntries(Object.entries(saved).filter(([id]) => wanted.has(id))),
    })
  }
}

/** Opens the windows the layout has that aren't open. */
function openWanted(): void {
  for (const { id } of dockStore.get().windows ?? []) {
    if (!isWindowOpen(id)) {
      const saved = layoutStore.get().windows[id]
      void openWindow(id, saved?.bounds ?? null, saved?.maximized ?? false).then(opened => {
        const stillWanted = (dockStore.get().windows ?? []).some(window => window.id === id)
        if (!opened && stillWanted) {
          log('warn', `Window ${id} couldn't open; its tabs go to the main window`)
          moveWindowTabsToMain(id)
        } else if (opened && !stillWanted) {
          void closeOpenWindow(id)
        }
      })
    }
  }
}

let started = false

/**
 * Closes the windows a previous load of the page left open, then follows
 * the layout. Call once the saved layout is loaded, before the dock is
 * restored.
 */
export async function startWindows(): Promise<void> {
  if (started) {
    return
  }
  started = true
  if (isDesktop()) {
    try {
      await resetWindows()
    } catch (error) {
      log('warn', `Closing windows left open: ${String(error)}`)
    }
  }
  dockStore.subscribe(sync)
  sync()
  if (!isDesktop()) {
    // Outside the desktop shell no window opens, so tabs come back (`sync`).
    return
  }
  enableWindows()
  // Other windows' pages zoom with the main window's.
  let zoom = layoutStore.get().zoom
  const applyZoom = () => {
    setWindowsZoom(zoom).catch(() => {})
  }
  applyZoom()
  layoutStore.subscribe(() => {
    if (layoutStore.get().zoom !== zoom) {
      zoom = layoutStore.get().zoom
      applyZoom()
    }
  })
}

/** The user asked to close window `id`: its tabs close, then it does. */
export function windowCloseRequested(id: string): void {
  if (closingTabs.has(id)) {
    return
  }
  closingTabs.add(id)
  void closeWindowTabs(id).finally(() => closingTabs.delete(id))
}

/** Window `id` moved or was resized. */
export function windowBoundsChanged(
  id: string,
  bounds: WindowBounds | null,
  maximized: boolean,
): void {
  if (!(dockStore.get().windows ?? []).some(window => window.id === id)) {
    return
  }
  const previous = layoutStore.get().windows[id]
  // While maximized, the bounds to go back to are kept.
  rememberWindow(id, { bounds: bounds ?? previous?.bounds ?? null, maximized })
}
