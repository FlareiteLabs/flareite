import { activeElement } from '../app/windows'
import { createStore } from '../state/store'

export interface MenuAction {
  label: string
  /** A keybinding to show, e.g. "Ctrl+C". */
  hint?: string | null
  disabled?: boolean
  run: () => void | Promise<void>
}

/** An action, or `null` for a separator. */
export type MenuEntry = MenuAction | null

export interface ContextMenuState {
  x: number
  y: number
  entries: readonly MenuEntry[]
  /** Restores focus when the menu closes without running anything. */
  returnFocus: Element | null
}

export const contextMenuStore = createStore<ContextMenuState | null>(null)

/**
 * Opens a context menu at a pointer position. Consecutive and trailing
 * separators are dropped.
 */
export function openContextMenu(x: number, y: number, entries: readonly MenuEntry[]): void {
  const cleaned: MenuEntry[] = []
  for (const entry of entries) {
    if (entry === null && (cleaned.length === 0 || cleaned[cleaned.length - 1] === null)) {
      continue
    }
    cleaned.push(entry)
  }
  while (cleaned.length > 0 && cleaned[cleaned.length - 1] === null) {
    cleaned.pop()
  }
  if (cleaned.length > 0) {
    contextMenuStore.set({ x, y, entries: cleaned, returnFocus: activeElement() })
  }
}

export function closeContextMenu(): void {
  contextMenuStore.set(null)
}
