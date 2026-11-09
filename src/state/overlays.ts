/**
 * Overlays: the palette, context menus, and dialogs, while they're open.
 *
 * Web page panes are native browser views laid over the workbench's page,
 * so anything drawn over them would be hidden behind them. They hide while
 * an overlay is open (`LayoutView.tsx`), as VS Code's do.
 */
import { useEffect } from 'react'
import { createStore } from './store'

/** How many overlays are open. */
export const openOverlays = createStore(0)

/** Counts the calling component as an overlay while it's mounted. */
export function useOverlay(): void {
  useEffect(() => {
    openOverlays.update(count => count + 1)
    return () => openOverlays.update(count => count - 1)
  }, [])
}
