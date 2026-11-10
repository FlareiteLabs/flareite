/**
 * The windows tabs are dragged into (`src-tauri/src/windows.rs`). The
 * workbench opens them itself, with `window.open` (`app/windows.ts`); these
 * calls place, move, and close them.
 */
import { call } from './index'

/**
 * Where a window is on screen, in physical pixels: its outer top left
 * corner, and its inner (page) size.
 */
export interface WindowBounds {
  x: number
  y: number
  width: number
  height: number
}

/** Where a window's page is on screen, in physical pixels. */
export interface WindowGeometry {
  /** Another window's id; `null` for the main window. */
  window: string | null
  x: number
  y: number
  width: number
  height: number
  /** Its outer top left corner. */
  outerX: number
  outerY: number
}

/** Says where window `id` goes, before `window.open` opens it. */
export const prepareWindow = (id: string, bounds: WindowBounds | null, maximized: boolean) =>
  call<void>('window_prepare', { id, bounds, maximized })

/** Closes window `id` without asking. */
export const closeWindow = (id: string) => call<void>('window_close', { id })

/** Closes the windows the main window's page opened before it loaded again. */
export const resetWindows = () => call<void>('windows_reset')

/** Brings a window (`null`: the main one) to the front. */
export const focusWindow = (id: string | null) => call<void>('window_focus', { id })

/** Moves window `id`'s outer top left corner (physical pixels). */
export const moveWindow = (id: string, x: number, y: number) =>
  call<void>('window_move', { id, x: Math.round(x), y: Math.round(y) })

/** Zooms the other windows' pages. */
export const setWindowsZoom = (zoom: number) => call<void>('windows_set_zoom', { zoom })

/** Where each window that isn't minimized is on screen. */
export const windowsGeometry = () => call<WindowGeometry[]>('windows_geometry')
