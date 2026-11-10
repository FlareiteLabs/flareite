import { useEffect, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { DragGhost } from '../components/DragGhost'
import { ErrorBoundary } from '../components/ErrorBoundary'
import { WindowStatusBar } from '../components/StatusBar'
import { DockArea } from '../dock/DockArea'
import { dockStore } from '../dock/dock'
import { itemTitle, useResolver } from '../dock/items'
import { groupsIn, rootOf } from '../dock/model'
import type { AppInfo } from '../ipc'
import { useStore } from '../state/store'
import { openWindows, type WindowId } from './windows'

/**
 * Draws `children` in window `window` (through a portal; in place for the
 * main window, or a window that isn't open).
 */
export function InWindow({ window, children }: { window: WindowId; children: ReactNode }) {
  const open = useStore(openWindows)
  const entry = window === null ? undefined : open.find(other => other.id === window)
  return entry ? createPortal(children, entry.root, entry.id) : children
}

/** The content of every other window, drawn in each window's page. */
export function OtherWindows({ appInfo }: { appInfo: AppInfo | null }) {
  const open = useStore(openWindows)
  return open.map(entry =>
    createPortal(<WindowContent id={entry.id} appInfo={appInfo} />, entry.root, entry.id),
  )
}

/** One other window: its dock, a status bar, and what's dragged over it. */
function WindowContent({ id, appInfo }: { id: string; appInfo: AppInfo | null }) {
  useWindowTitle(id)
  return (
    <div className="workbench workbench--window">
      <div className="workbench__body">
        <ErrorBoundary name="The window's layout">
          <DockArea appInfo={appInfo} window={id} />
        </ErrorBoundary>
      </div>
      <ErrorBoundary name="The status bar">
        <WindowStatusBar window={id} />
      </ErrorBoundary>
      <DragGhost window={id} />
    </div>
  )
}

/** Titles a window after the tab shown in its first group. */
function useWindowTitle(id: string): void {
  const layout = useStore(dockStore)
  const resolve = useResolver()
  const root = rootOf(layout, id)
  const group = root ? groupsIn(root).find(g => !g.hidden && g.active) : undefined
  const resolved = group?.active ? resolve(group.active) : null
  const title = resolved ? `${itemTitle(resolved)} - Flareite` : 'Flareite'
  const view = useStore(openWindows).find(entry => entry.id === id)?.window
  useEffect(() => {
    if (view && view.document.title !== title) {
      view.document.title = title
    }
  }, [view, title])
}
