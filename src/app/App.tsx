import { useEffect, useState } from 'react'
import { runRequestedBenchmark } from '../bench/runner'
import { builtinCommands } from '../commands/builtin'
import { registerCommands } from '../commands/registry'
import { ErrorBoundary } from '../components/ErrorBoundary'
import { getAppInfo, isDesktop, log, notifyFrontendReady, type AppInfo } from '../ipc'
import { benchConfig } from '../ipc/perf'
import { getSettings } from '../ipc/workspace'
import { applySettings } from '../state/appearance'
import { startAppUpdates } from '../state/appUpdate'
import { loadDocuments } from '../state/documents'
import { listenToBackend } from '../state/events'
import { refreshExtensions } from '../state/extensions'
import { refreshSources } from '../state/extensionInstall'
import { refreshFileMarks } from '../state/fileMarks'
import { restoreLayout } from '../state/layout'
import { reportError } from '../state/notifications'
import { followFileProblems, refreshSettingsProblem } from '../state/problems'
import { refreshPreviews } from '../state/previews'
import { loadWorkspace } from '../state/workspace'
import { restoreTerminals } from '../terminal/terminals'
import { documentsRestored, restoreDock, terminalsRestored } from '../dock/dock'
import { Workbench } from './Workbench'
import { followActiveFile } from '../dock/activeFile'

/**
 * Reattaches to terminals still running (after a window reload), or starts
 * one where the saved layout showed terminals. Benchmarks measure the editor
 * alone.
 */
async function loadTerminals(): Promise<void> {
  try {
    if ((await benchConfig()) === null) {
      await restoreTerminals()
      await terminalsRestored()
    }
  } catch (error) {
    reportError('Restoring terminals', error)
  }
}

/**
 * Restores settings, layout, then the workspace, open documents, extensions,
 * and terminals.
 */
async function restoreSession(): Promise<void> {
  listenToBackend()
  try {
    applySettings(await getSettings())
  } catch (error) {
    reportError('Loading settings', error)
  }
  // An invalid settings.toml at startup is only logged by the backend.
  void refreshSettingsProblem()
  followFileProblems()
  followActiveFile()
  let activeFile: string | null = null
  try {
    activeFile = await restoreLayout()
  } catch (error) {
    reportError('Restoring the layout', error)
  }
  restoreDock()
  await Promise.all([
    loadWorkspace(),
    // Previews still open (after a window reload) go with their documents.
    loadDocuments(activeFile).then(documentsRestored).then(refreshPreviews),
    refreshExtensions(),
    refreshSources(),
    // Marks extensions set before the window loaded (or reloaded).
    refreshFileMarks(),
    loadTerminals(),
  ])
  await runRequestedBenchmark()
  void startAppUpdates()
}

export function App() {
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null)

  useEffect(() => registerCommands(builtinCommands), [])

  useEffect(() => {
    let cancelled = false
    getAppInfo()
      .then(info => {
        if (!cancelled) {
          setAppInfo(info)
        }
      })
      .catch((error: unknown) => log('error', `app_info failed: ${String(error)}`))
    if (isDesktop()) {
      void restoreSession()
    }
    notifyFrontendReady()
    return () => {
      cancelled = true
    }
  }, [])

  return (
    <ErrorBoundary name="Flareite">
      <Workbench appInfo={appInfo} />
    </ErrorBoundary>
  )
}
