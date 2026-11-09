/**
 * Flareite updating itself from its GitHub releases: the check at startup, the notifications
 * that offer an update, its download, and restarting to install it (ADR 0040).
 */
import { CommandError, log, openExternal } from '../ipc'
import {
  checkAppUpdate,
  downloadAppUpdate as download,
  getAppUpdateStatus,
  restartToUpdate as restart,
  type AppUpdateStatus,
  type DownloadProgress,
} from '../ipc/appUpdate'
import { notify, reportError } from './notifications'
import { createStore } from './store'

export interface AppUpdateView {
  status: AppUpdateStatus | null
  /** While the update downloads. */
  progress: DownloadProgress | null
}

export const appUpdateStore = createStore<AppUpdateView>({ status: null, progress: null })

/** How long after startup the automatic check waits, so it doesn't compete with restoring. */
const STARTUP_DELAY_MS = 5000

/** The tags already offered this session, so a check doesn't offer one twice unasked. */
const offered = new Set<string>()

function setStatus(status: AppUpdateStatus): void {
  appUpdateStore.update(view => ({ ...view, status }))
}

let started = false

/**
 * Reports an update that failed to install as Flareite last exited, then checks soon after.
 * Once per page load (development runs the startup effect twice).
 */
export async function startAppUpdates(): Promise<void> {
  if (started) {
    return
  }
  started = true
  try {
    const status = await getAppUpdateStatus()
    setStatus(status)
    if (status.failed) {
      notify('error', status.failed, [
        { label: 'Open Releases', run: () => openExternal(status.releasesUrl) },
      ])
    }
  } catch (error) {
    log('warn', `app_update_status failed: ${String(error)}`)
    return
  }
  window.setTimeout(() => void checkForAppUpdate(true), STARTUP_DELAY_MS)
}

/**
 * Looks for a newer Flareite. `automatic` (the check at startup) says nothing unless it finds
 * one; a check the user asked for also says when there's none, or why it failed.
 */
export async function checkForAppUpdate(automatic = false): Promise<void> {
  let status: AppUpdateStatus
  try {
    status = await checkAppUpdate(automatic)
  } catch (error) {
    if (automatic) {
      log('warn', `Checking for an update failed: ${String(error)}`)
    } else if (error instanceof CommandError && error.code === 'notRelease') {
      notify('info', error.message)
    } else {
      reportFailure('Checking for updates', error)
    }
    return
  }
  setStatus(status)
  const update = status.update
  if (!update) {
    if (!automatic) {
      notify('info', `Flareite ${status.current} is the newest version.`)
    }
    return
  }
  if (status.ready) {
    if (!automatic) {
      offerRestart(status)
    }
    return
  }
  if (automatic && offered.has(update.tag)) {
    return
  }
  offered.add(update.tag)
  notify(
    'info',
    `Flareite ${update.version} is available. You have ${status.current}.`,
    [
      { label: 'Download', run: downloadAppUpdate },
      { label: 'Release Notes', run: () => openExternal(update.url) },
    ],
    { sticky: true },
  )
}

/** Downloads the update found, then offers to restart to install it. */
export async function downloadAppUpdate(): Promise<void> {
  const current = appUpdateStore.get()
  if (current.progress) {
    return
  }
  appUpdateStore.update(view => ({ ...view, progress: { downloaded: 0, total: null } }))
  try {
    const status = await download(progress =>
      appUpdateStore.update(view => ({ ...view, progress })),
    )
    setStatus(status)
    if (status.ready) {
      offerRestart(status)
    }
  } catch (error) {
    reportFailure('Downloading the update', error)
  } finally {
    appUpdateStore.update(view => ({ ...view, progress: null }))
  }
}

function offerRestart(status: AppUpdateStatus): void {
  notify(
    'info',
    `Flareite ${status.update?.version ?? ''} is downloaded. Restart Flareite to install it.`,
    [{ label: 'Restart to Update', run: restartToUpdate }],
    { sticky: true },
  )
}

/** Closes Flareite (asking about unsaved changes) to install the downloaded update. */
export async function restartToUpdate(): Promise<void> {
  if (!appUpdateStore.get().status?.ready) {
    notify('info', 'No update is downloaded yet. Check for updates first.')
    return
  }
  try {
    await restart()
  } catch (error) {
    reportFailure('Restarting to update', error)
  }
}

/** Reports a failure; when the release has nothing for this installation, links to it. */
function reportFailure(action: string, error: unknown): void {
  const status = appUpdateStore.get().status
  if (error instanceof CommandError && error.code === 'noInstaller') {
    const url = status?.update?.url ?? status?.releasesUrl
    notify(
      'warning',
      error.message,
      url ? [{ label: 'Open Release', run: () => openExternal(url) }] : [],
    )
    return
  }
  reportError(action, error)
}
