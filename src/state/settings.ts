/**
 * The settings editor's state: every setting with its value (from the
 * backend, which owns `settings.toml`), and the search it opens with.
 *
 * Changes are written to `settings.toml` one setting at a time; the backend
 * applies them and announces the new settings, which reloads these.
 */
import {
  getSettingsSchema,
  setSetting,
  type SettingValue,
  type SettingsSchema,
  type SettingsScope,
} from '../ipc/workspace'
import { reportError } from './notifications'
import { createStore } from './store'
import { showPage } from './viewRequests'

/** `null` until loaded. */
export const settingsSchemaStore = createStore<SettingsSchema | null>(null)
/** The search the settings editor shows; set when it's opened for something. */
export const settingsQuery = createStore<string>('')
/**
 * Which settings the settings editor shows and changes: the user's, or the
 * open folder's (the Workspace tab).
 */
export const settingsScope = createStore<SettingsScope>('user')

let loading: Promise<void> | null = null
let loadAgain = false

/** Reloads the settings; coalesces calls made while one is running. */
export function refreshSettings(): Promise<void> {
  if (loading) {
    loadAgain = true
    return loading
  }
  loading = (async () => {
    do {
      loadAgain = false
      try {
        const schema = await getSettingsSchema(settingsScope.get())
        // The folder was closed: show the user's settings.
        if (schema.folder === null && settingsScope.get() === 'workspace') {
          settingsScope.set('user')
        }
        settingsSchemaStore.set(schema)
      } catch (error) {
        reportError('Loading settings', error)
      }
    } while (loadAgain)
    loading = null
  })()
  return loading
}

/** Reloads the settings if the settings editor has loaded them. */
export function refreshSettingsIfLoaded(): void {
  if (settingsSchemaStore.get() !== null) {
    void refreshSettings()
  }
}

/**
 * Sets a setting in `settings.toml` (or the open folder's, on the Workspace
 * tab), or removes it (`null`) so its default applies. Resolves once it's
 * applied; a rejected change is reported.
 */
export async function changeSetting(path: string[], value: SettingValue | null): Promise<void> {
  try {
    await setSetting(path, value, settingsScope.get())
  } catch (error) {
    reportError('Changing the setting', error)
  }
  await refreshSettings()
}

/** Shows the user's settings, or the open folder's, in the settings editor. */
export function showSettingsScope(scope: SettingsScope): void {
  if (settingsScope.get() !== scope) {
    settingsScope.set(scope)
    void refreshSettings()
  }
}

/**
 * Opens the settings editor, searching for `query` (e.g. `@ext:<id>`), on
 * the user's settings or (`workspace`) the open folder's.
 */
export function openSettings(query = '', scope: SettingsScope = 'user'): void {
  settingsQuery.set(query)
  const changed = settingsScope.get() !== scope
  settingsScope.set(scope)
  showPage('settings')
  if (changed) {
    settingsSchemaStore.set(null)
    void refreshSettings()
  } else {
    // It may already be open, showing settings from before the file changed.
    refreshSettingsIfLoaded()
  }
}

/** An extension's settings: the query that shows only them. */
export const extensionQuery = (id: string) => `@ext:${id}`
