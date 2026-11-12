/**
 * Typed wrappers for the workspace, settings, session-state, and event
 * commands (`src-tauri/src/{workspace,settings,ui_state,events}.rs`).
 */
import { Channel } from '@tauri-apps/api/core'
import type { DocumentSummary, Position, SearchQuery } from './editor'
import { call, isDesktop } from './index'
import type { MessageLevel, Prompt } from './extensions'
import type { FileDragEvent } from '../components/drag'
import type { WindowBounds } from './windows'

/**
 * Whether the user trusts a folder's files: until they do (`unknown` means
 * they haven't said), language servers and extensions' programs don't run.
 */
export type Trust = 'trusted' | 'untrusted' | 'unknown'

export interface WorkspaceInfo {
  root: string
  name: string
  trust: Trust
}

export interface DirectoryEntry {
  name: string
  path: string
  directory: boolean
  /** Matched by .gitignore: shown dimmed. */
  ignored: boolean
}

export interface CreatedEntry {
  path: string
  /** Folders whose listings gained an entry, outermost first. */
  directories: string[]
}

export interface FileResult {
  path: string
  relative: string
  /** UTF-16 indices of matched characters in `relative`. */
  indices: number[]
}

export interface FindFilesResult {
  files: FileResult[]
  indexing: boolean
  truncated: boolean
}

export interface LineMatch {
  line: number
  startColumn: number
  endColumn: number
  preview: string
  previewStart: number
  previewEnd: number
}

export type SearchEvent =
  | { type: 'file'; path: string; relative: string; matches: LineMatch[] }
  | {
      type: 'done'
      filesSearched: number
      filesWithMatches: number
      matches: number
      truncated: boolean
      cancelled: boolean
    }

/** `system`, `dark`, `light`, or a contributed theme's id. */
export type Theme = string

export interface FrontendSettings {
  theme: Theme
  /** `default` or a contributed icon theme's id. */
  iconTheme: string
  /** Theme color overrides: token (without `--color-`) to hex color. */
  colors: Record<string, string>
  /** Command id to keys, replacing the defaults; empty removes them. */
  keybindings: Record<string, string[]>
  fontSize: number
  fontFamily: string
  tabSize: number
  /** Lines kept above a terminal's screen. */
  terminalScrollback: number
  /** Whether terminals announce output to screen readers. */
  terminalScreenReader: boolean
  /** Whether the minimap labels named `#region` comments. */
  minimapRegionHeaders: boolean
  /** Whether the minimap labels `MARK:` comments. */
  minimapMarkHeaders: boolean
  /** Where lines wrap: not at all, at the editor's width, at `wordWrapColumn`, or the lesser. */
  wordWrap: 'off' | 'on' | 'column' | 'bounded'
  wordWrapColumn: number
  /** How wrapped rows after a line's first are indented. */
  wrappingIndent: WrappingIndent
  /** Who opens web links: `auto`, `default`, or a link handler's id. */
  openWebLinks: string
  /** Who opens file links: `auto`, `default`, or a link handler's id. */
  openFileLinks: string
}

/** How wrapped rows after a line's first are indented, as VS Code's `wrappingIndent`. */
export type WrappingIndent = 'none' | 'same' | 'indent' | 'deep_indent'

/** A value the settings editor shows or sets. */
export type SettingValue = boolean | number | string | string[]

export interface SettingChoice {
  value: string
  label: string
}

/** What a setting holds, and how it's edited. */
export type SettingKind =
  | { type: 'boolean' }
  | { type: 'integer' | 'number'; minimum: number | null; maximum: number | null }
  /** `choices`: the values offered (built-in settings may hold others, set in the file). */
  | { type: 'string'; choices: SettingChoice[] | null }
  | { type: 'list' }
  /** Entries edited in `settings.toml`; the value is their names. */
  | { type: 'table' }

/** One setting, built in or declared by an extension (`settings_schema`). */
export type Setting = SettingKind & {
  /** Its keys in `settings.toml`, e.g. `["editor", "tab_size"]`. */
  path: string[]
  section: string
  /** The id of the extension that declares it. */
  extension: string | null
  title: string
  description: string
  default: SettingValue
  /** The value in effect. */
  value: SettingValue
  /** Whether `settings.toml` sets it. */
  modified: boolean
  /** Why the value in `settings.toml` isn't used. */
  problem: string | null
  /** Whether a folder's own settings (`.flareite/settings.toml`) can set it. */
  folder: boolean
}

/** The open folder's own settings, `.flareite/settings.toml`. */
export interface FolderSettingsInfo {
  /** The folder's name. */
  name: string
  file: string
  exists: boolean
  /** Its settings apply only once the user trusts the folder. */
  trusted: boolean
  /** Why its settings don't apply, if they can't. */
  error: string | null
  /** Settings its file sets that only the user's settings can, as dotted keys. */
  ignored: string[]
}

/** Which settings file the settings editor changes. */
export type SettingsScope = 'user' | 'workspace'

export interface SettingsSchema {
  /** The settings file, if there's a configuration directory. */
  file: string | null
  /** Why the settings file isn't in effect, if it's invalid. */
  error: string | null
  settings: Setting[]
  /** The open folder's settings, if a folder is open. */
  folder: FolderSettingsInfo | null
}

export interface SavedUi {
  layout: Record<string, unknown>
  activeFile: string | null
}

export type AppEvent =
  | { type: 'workspaceChanged'; workspace: WorkspaceInfo | null }
  | { type: 'workspaceTrustChanged'; workspace: WorkspaceInfo }
  /** An extension (by id) asked the user to trust the open folder. */
  | { type: 'workspaceTrustRequested'; extension: string }
  | { type: 'filesChanged'; directories: string[] }
  | { type: 'documentReloaded'; document: DocumentSummary }
  | { type: 'documentChangedOnDisk'; document: DocumentSummary }
  /** Its file was renamed or moved from the explorer. */
  | { type: 'documentMoved'; document: DocumentSummary }
  /** Saved because an extension changed its file. */
  | { type: 'documentSaved'; document: DocumentSummary }
  /** `announce`: tell the user (not when they changed one setting in the settings editor). */
  | { type: 'settingsChanged'; settings: FrontendSettings; announce: boolean }
  | { type: 'searchCounted'; documentId: number }
  | { type: 'syntaxUpdated'; documentId: number }
  | { type: 'diagnosticsChanged'; documentId: number }
  /** Diagnostics of files that aren't open changed. */
  | { type: 'problemsChanged' }
  | { type: 'languageServerChanged' }
  | { type: 'languagesChanged' }
  | { type: 'documentOpened'; document: DocumentSummary; active: boolean }
  /** A file named at startup that isn't text, to open in a viewer or ask about. */
  | { type: 'openFile'; path: string; active: boolean }
  /** A document's changes from its original are ready: its editors fetch views again. */
  | { type: 'quickDiffChanged'; documentId: number }
  /** An extension asked to apply the changes the active editor's selections touch to the original. */
  | { type: 'applySelectedRanges' }
  /** An extension asked to open a folder in place of the open one. */
  | { type: 'openFolder'; path: string }
  /** An extension asked to open the document at an address it provides. */
  | { type: 'openProvided'; uri: string }
  /** A link handler handed its link back: open it Flareite's own way. */
  | {
      type: 'openLinkDefault'
      link: {
        kind: 'web' | 'file'
        target: string
        line: number | null
        column: number | null
      }
    }
  /** An extension asked to open one of its layouts' tabs, or show it. */
  | { type: 'openLayout'; layout: string; key: string; beside: boolean }
  /** What a layout tab shows changed. */
  | { type: 'layoutChanged'; layout: string; key: string }
  /** An extension asked to close a layout tab. */
  | { type: 'closeLayout'; layout: string; key: string }
  /** What a layout's editor pane shows besides its text changed. */
  | { type: 'paneDecorationsChanged'; layout: string; key: string; pane: string }
  /** An extension asked to select and show a range in a layout's editor pane. */
  | {
      type: 'revealInPane'
      layout: string
      key: string
      pane: string
      start: { line: number; column: number }
      end: { line: number; column: number }
    }
  | { type: 'indexStatus'; indexing: boolean; files: number; truncated: boolean }
  | { type: 'extensionsChanged' }
  /** `id` is the extension's id, `extension` its name. */
  | {
      type: 'extensionMessage'
      id: string
      extension: string
      level: MessageLevel
      message: string
    }
  | { type: 'extensionFailed'; id: string; name: string; reason: string }
  | { type: 'documentEdited'; documentId: number }
  | { type: 'extensionStatusItemsChanged' }
  | { type: 'extensionViewChanged'; view: string }
  | { type: 'extensionShowView'; view: string }
  | { type: 'decorationsChanged'; documentId: number }
  | { type: 'extensionFileMarksChanged' }
  | { type: 'extensionPrompt'; prompt: Prompt }
  | { type: 'extensionPromptClosed'; id: number }
  | { type: 'extensionReveal'; documentId: number; start: Position; end: Position }
  | { type: 'extensionPreviewsChanged' }
  | { type: 'extensionPreviewChanged'; preview: string }
  | { type: 'extensionShowPreview'; preview: string; beside: boolean }
  | { type: 'extensionWebviewChanged'; preview: string }
  | { type: 'extensionWebviewMessage'; preview: string; message: string }
  | {
      type: 'extensionStorageChanged'
      extension: string
      scope: string | null
      key: string | null
      value: string | null
      source: string | null
    }
  | { type: 'notice'; level: 'warning'; message: string }
  /** Files dragged in from another program; positions in physical pixels. */
  | ({ type: 'fileDrag' } & FileDragEvent)
  /** The user asked to close another window: its tabs close, then it does. */
  | { type: 'windowCloseRequested'; window: string }
  /** Another window moved or was resized: where it is (`null` while maximized). */
  | {
      type: 'windowBoundsChanged'
      window: string
      bounds: WindowBounds | null
      maximized: boolean
    }

export const getWorkspace = () => call<WorkspaceInfo | null>('workspace_get')
export const openWorkspace = (path: string) => call<WorkspaceInfo>('workspace_open', { path })
export const openWorkspaceDialog = () => call<WorkspaceInfo | null>('workspace_open_dialog')
export const closeWorkspace = () => call<void>('workspace_close')
/** Trusts the open folder (or, with `parent`, the folder it's in), or records that the user doesn't. */
export const setWorkspaceTrust = (trusted: boolean, parent: boolean) =>
  call<void>('workspace_set_trust', { trusted, parent })
export const listDirectory = (path: string) =>
  call<DirectoryEntry[]>('workspace_list_dir', { path })
/** Creates an empty file or a folder; `name` may include subfolders (`src/lib.rs`). */
export const createEntry = (parent: string, name: string, directory: boolean) =>
  call<CreatedEntry>('workspace_create', { parent, name, directory })
/** Renames a file or folder; `name` may name new subfolders to move it into. */
export const renameEntry = (path: string, name: string) =>
  call<CreatedEntry>('workspace_rename', { path, name })
/** Copies what was dropped from another program (drop `dropId`) into `target`. */
export const copyDropped = (dropId: number, target: string) =>
  call<string[]>('workspace_copy_dropped', { dropId, target })
/** Moves files and folders into `target`, returning their new paths. */
export const moveEntries = (paths: string[], target: string) =>
  call<string[]>('workspace_move', { paths, target })
/** Copies files and folders into `target` (as `name copy.ext` if taken), returning the copies. */
export const copyEntries = (paths: string[], target: string) =>
  call<string[]>('workspace_copy', { paths, target })
/**
 * Asks, then deletes files and folders: to the system's trash, or
 * permanently. Resolves to false if the user cancelled.
 */
export const deleteEntries = (paths: string[], permanent: boolean) =>
  call<boolean>('workspace_delete', { paths, permanent })
/** Shows a file or folder in the system's file manager. */
export const revealEntry = (path: string) => call<void>('workspace_reveal', { path })
/** Puts text on the system clipboard. */
export const writeClipboardText = (text: string) => call<void>('clipboard_write_text', { text })
export const findFiles = (query: string) => call<FindFilesResult>('workspace_find_files', { query })
export const cancelSearch = () => call<void>('workspace_search_cancel')

/** Starts a find-in-files search; `onEvent` receives results as they arrive. */
export function searchWorkspace(
  query: SearchQuery,
  onEvent: (event: SearchEvent) => void,
): Promise<void> {
  const channel = new Channel<SearchEvent>()
  channel.onmessage = onEvent
  return call<void>('workspace_search', { query, channel })
}

export const getSettings = () => call<FrontendSettings>('settings_get')
export const settingsFilePath = () => call<string>('settings_file')
export const setTheme = (theme: Theme) => call<void>('settings_set_theme', { theme })
export const setIconTheme = (iconTheme: string) =>
  call<void>('settings_set_icon_theme', { iconTheme })
/**
 * Every setting: as the user's settings set it, or (`workspace`) as it is
 * in effect in the open folder, with what the folder's file sets.
 */
export const getSettingsSchema = (scope: SettingsScope = 'user') =>
  call<SettingsSchema>('settings_schema', { scope })
/** Why `settings.toml` isn't in effect (unreadable or invalid); `null` if it's fine. */
export const getSettingsProblem = () => call<string | null>('settings_problem')
/**
 * Sets one setting in `settings.toml` (or the open folder's, for
 * `workspace`); `null` removes it, so its default (or the user's value)
 * applies.
 */
export const setSetting = (
  path: string[],
  value: SettingValue | null,
  scope: SettingsScope = 'user',
) => call<void>('settings_set', { path, value, scope })
/** A command, for the settings' schema (`[keybindings]`). */
export interface SchemaCommand {
  id: string
  /** Its category and title, as the command palette shows it. */
  label: string
  /** Its default keys on this platform, as settings write them. */
  keys: string[]
}

/** Tells the backend every command, for completing `[keybindings]` in settings. */
export const setSchemaCommands = (commands: SchemaCommand[]) =>
  call<void>('schemas_set_commands', { commands })

/** The open folder's settings file, created if needed. */
export const folderSettingsFilePath = () => call<string>('settings_folder_file')

export const getSavedUi = () => call<SavedUi>('ui_state_get')
export const saveUi = (layout: Record<string, unknown>, activeFile: string | null) =>
  call<void>('ui_state_save', { layout, activeFile })

/** Subscribes to backend notifications. Does nothing outside the desktop app. */
export function subscribeToEvents(onEvent: (event: AppEvent) => void): void {
  if (!isDesktop()) {
    return
  }
  const channel = new Channel<AppEvent>()
  channel.onmessage = onEvent
  void call<void>('events_subscribe', { channel })
}
