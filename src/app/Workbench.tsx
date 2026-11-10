import { useKeybindings } from '../commands/registry'
import { ActivityBar } from '../components/ActivityBar'
import { ContextMenu } from '../components/ContextMenu'
import { DragGhost } from '../components/DragGhost'
import { ErrorBoundary } from '../components/ErrorBoundary'
import { Notifications } from '../components/Notifications'
import { Palette } from '../components/Palette'
import { PerfOverlay } from '../components/PerfOverlay'
import { StatusBar } from '../components/StatusBar'
import { ConfirmDialog } from '../components/ConfirmDialog'
import { TrustDialog } from '../components/TrustDialog'
import { DockArea } from '../dock/DockArea'
import type { AppInfo } from '../ipc'
import { useStore } from '../state/store'
import { InWindow, OtherWindows } from './WindowContent'
import { focusedWindow } from './windows'
import './Workbench.css'

interface WorkbenchProps {
  appInfo: AppInfo | null
}

export function Workbench({ appInfo }: WorkbenchProps) {
  useKeybindings()
  // The palette, dialogs, and menus show in the window the user is in.
  const overlays = useStore(focusedWindow)

  return (
    <div className="workbench">
      <div className="workbench__body">
        <ActivityBar />
        <ErrorBoundary name="The workbench layout">
          <DockArea appInfo={appInfo} window={null} />
        </ErrorBoundary>
      </div>
      <ErrorBoundary name="The status bar">
        <StatusBar appInfo={appInfo} />
      </ErrorBoundary>
      <Notifications />
      <InWindow window={overlays}>
        <Palette />
        <TrustDialog />
        <ConfirmDialog />
        <ContextMenu />
      </InWindow>
      <DragGhost window={null} />
      <PerfOverlay />
      <OtherWindows appInfo={appInfo} />
    </div>
  )
}
