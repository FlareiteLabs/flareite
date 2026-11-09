/** Typed wrappers for Flareite updating itself (`src-tauri/src/app_update.rs`). */
import { Channel } from '@tauri-apps/api/core'
import { call } from './index'

/** An update found. */
export interface AppUpdate {
  /** Its version, from its tag (`0.2.0-alpha.1`). */
  version: string
  tag: string
  prerelease: boolean
  /** The release's page. */
  url: string
}

export interface AppUpdateStatus {
  /** The running version. */
  current: string
  /** Whether this build updates itself: only release builds do. */
  updatable: boolean
  /** The page listing Flareite's releases. */
  releasesUrl: string
  /** The update the last check found. */
  update: AppUpdate | null
  downloading: boolean
  /** Whether it's downloaded, to install when Flareite restarts. */
  ready: boolean
  /** Why the update installed when Flareite last exited failed. Reported once. */
  failed: string | null
}

export interface DownloadProgress {
  downloaded: number
  /** Null if the server didn't say. */
  total: number | null
}

/** Where updates stand. The first call also reports an install that failed. */
export const getAppUpdateStatus = () => call<AppUpdateStatus>('app_update_status')
/**
 * Looks for an update. `automatic` is the check at startup, which the backend skips unless
 * it's a release build, `update.check` is on, and it hasn't checked in a day.
 */
export const checkAppUpdate = (automatic: boolean) =>
  call<AppUpdateStatus>('app_update_check', { automatic })

/** Downloads the update found and checks its signature. */
export function downloadAppUpdate(
  onProgress: (progress: DownloadProgress) => void,
): Promise<AppUpdateStatus> {
  const progress = new Channel<DownloadProgress>()
  progress.onmessage = onProgress
  return call<AppUpdateStatus>('app_update_download', { progress })
}

/**
 * Closes Flareite to install the downloaded update and start again, asking about unsaved
 * changes first. False if the user cancelled.
 */
export const restartToUpdate = () => call<boolean>('app_update_restart')
