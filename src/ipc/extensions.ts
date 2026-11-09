/**
 * Typed wrappers for the extension commands in `src-tauri/src/extensions.rs`.
 */
import { call } from './index'

export type Permission =
  | 'documents-read'
  | 'documents-write'
  | 'files-read'
  | 'files-write'
  | 'language-servers'
  | 'network'
  | 'processes'
  | 'web-pages'

export type ExtensionState =
  | 'inactive'
  | 'awaitingPermission'
  | 'starting'
  | 'active'
  | 'failed'
  | 'disabled'
  | 'invalid'
  | 'ready'

export interface PermissionStatus {
  permission: Permission
  description: string
  granted: boolean
  /** Whether the user has granted or denied it. */
  decided: boolean
}

export interface ExtensionCommand {
  id: string
  title: string
  category: string
  /** Not listed in the command palette. */
  hidden: boolean
}

export interface ThemeContribution {
  /** `<extension id>.<theme id>`. */
  id: string
  label: string
  kind: 'dark' | 'light'
  colors: Record<string, string>
}

export interface IconThemeContribution {
  /** `<extension id>.<icon theme id>`. */
  id: string
  label: string
}

/** Icon ids by what they are for. Names and extensions are lowercase. */
export interface IconAssociations {
  file: string | null
  folder: string | null
  folderExpanded: string | null
  /** Without the leading dot, e.g. `rs` or `d.ts`. */
  fileExtensions: Record<string, string>
  fileNames: Record<string, string>
  folderNames: Record<string, string>
  folderNamesExpanded: Record<string, string>
}

/** A file icon theme (`extensions_icon_theme`). */
export interface IconTheme {
  /** Icon id to a `data:` URL. */
  icons: Record<string, string>
  associations: IconAssociations
  /** Looked up before `associations` with a light color theme. */
  light: IconAssociations
}

export interface KeybindingContribution {
  command: string
  key: string
  mac: string | null
  editorFocus: boolean
  /** Only while a tab of one of these layouts (full ids) is active; empty: always. */
  layouts: string[]
}

export type MenuLocation = 'editor/context' | 'explorer/context' | 'view/title' | 'view/item'

export interface MenuContribution {
  location: MenuLocation
  command: string
  group: string
  /** In the explorer: shown for files. */
  forFiles: boolean
  /** In the explorer: shown for folders. */
  forFolders: boolean
  /** File name patterns (`*.md`, `*.{ts,tsx}`); only matching files get it. Empty: all. */
  patterns: string[]
  /** For view menus: the view, `<extension id>.<view id>`. */
  view: string | null
  /** For `view/item`: the item contexts it's for; empty means every item. */
  contexts: string[]
  /** A button rather than a menu entry. */
  inline: boolean
  /** A named icon. */
  icon: string | null
  /** Shown only while the view has this flag (or with `!`, hasn't). */
  when: string | null
}

export interface ViewContribution {
  id: string
  /** `<extension id>.<view id>`. */
  fullId: string
  title: string
  icon: string
  /** The container it's a section of; `null` for a view of its own. */
  container: string | null
  /** A text box above its items. */
  input: { placeholder: string | null; command: string | null } | null
  /** Run to load an opened item's children. */
  onExpand: string | null
  /** Hidden until the extension shows it. (1.13) */
  hidden: boolean
}

/** An activity bar entry whose views are sections. */
export interface ViewContainerContribution {
  id: string
  /** `<extension id>.<container id>`. */
  fullId: string
  title: string
  icon: string
}

export interface PreviewContribution {
  id: string
  /** `<extension id>.<preview id>`. */
  fullId: string
  icon: string
  /** Set when it shows a page instead of preview content. */
  webview: WebviewContribution | null
  /** Buttons beside its tabs. */
  actions: TabAction[]
}

/** A button beside a viewer's or preview's tabs. */
export interface TabAction {
  /** What the page's `flareite.onAction` gets. */
  id: string
  title: string
  icon: { kind: 'named'; name: string } | { kind: 'image'; url: string }
  /** Run with the tab as its argument; without one, the click goes to the page. */
  command: string | null
}

/** The page a viewer or preview shows instead of preview content. */
export interface WebviewContribution {
  /** The HTML file, relative to the extension's folder; `null` when the extension sets the page. */
  page: string | null
  /** The command run when the page sends a message. */
  onMessage: string | null
  /** Keeps the page loaded while its tab is hidden. */
  retainWhenHidden: boolean
}

/** A viewer: a tab that shows matching files the extension's way, not as text. */
export interface ViewerContribution {
  id: string
  /** `<extension id>.<viewer id>`. */
  fullId: string
  title: string
  /** File name patterns, as for menu items. */
  patterns: string[]
  /** Fills it in; optional for a webview. */
  command: string | null
  /** `default`: matching files open in it; `option`: only when chosen. */
  priority: 'default' | 'option'
  /** Set when it shows a page instead of preview content. */
  webview: WebviewContribution | null
  /** Buttons beside its tabs. */
  actions: TabAction[]
}

/** A layout: tabs an extension splits into panes, filled in by its command. (1.15) */
export interface LayoutContribution {
  id: string
  /** `<extension id>.<layout id>`. */
  fullId: string
  title: string
  command: string
  onMessage: string | null
  /** File name patterns of the files it's for; empty for a layout only its extension opens. */
  patterns: string[]
  /** With patterns: `default`, matching files open in it; `option`, only when chosen. */
  priority: 'default' | 'option'
  /** A named icon for the button that switches a file's tab to it. */
  icon: string | null
}

/** Links an extension opens instead of Flareite. (1.15) */
export interface LinkHandlerContribution {
  id: string
  /** `<extension id>.<handler id>`. */
  fullId: string
  title: string
  kind: 'web' | 'file'
  /** Web links: the schemes it takes. */
  schemes: string[]
  /** File links: name patterns; empty takes every file and folder. */
  patterns: string[]
  command: string
}

export interface DecorationColors {
  background: string | null
  color: string | null
  border: string | null
  underlineColor: string | null
}

export interface DecorationType {
  name: string
  /** `<extension id>/<name>`, as in `DecorationView.key`. */
  key: string
  dark: DecorationColors
  light: DecorationColors
  underline: 'solid' | 'wavy' | 'dotted' | 'dashed' | null
  bold: boolean
  italic: boolean
  /** The background spans each covered line's whole width. */
  wholeLine: boolean
}

/** A language an extension adds, highlighted by its own grammar. */
export interface LanguageContribution {
  /** The language id, e.g. `html`. */
  id: string
  name: string
  fileExtensions: string[]
  fileNames: string[]
}

/** A language server an extension declares; run once allowed. */
export interface LanguageServerContribution {
  name: string
  command: string
  args: string[]
  languages: string[]
  rootMarkers: string[]
}

/** A program an extension runs. */
export interface ProgramContribution {
  name: string
  /** On the PATH, or a path in the extension's folder. */
  command: string
  /** A path in the open folder, preferred when there (trusted folders only). */
  workspace: string | null
}

/** A color an extension's explorer marks use, which themes can change. */
export interface ColorContribution {
  name: string
  /** `<extension id>.<name>`: the token themes and `workbench.colors` set. */
  id: string
  description: string
  dark: string
  light: string
}

export interface Contributions {
  themes: ThemeContribution[]
  iconThemes: IconThemeContribution[]
  keybindings: KeybindingContribution[]
  menus: MenuContribution[]
  views: ViewContribution[]
  decorations: DecorationType[]
  previews: PreviewContribution[]
  viewers: ViewerContribution[]
  layouts: LayoutContribution[]
  linkHandlers: LinkHandlerContribution[]
  languages: LanguageContribution[]
  languageServers: LanguageServerContribution[]
  programs: ProgramContribution[]
  colors: ColorContribution[]
  viewContainers: ViewContainerContribution[]
  textProviders: TextProviderContribution[]
  quickDiff: QuickDiffContribution | null
}

/** Read-only documents an extension provides at addresses with `scheme`. (1.8) */
export interface TextProviderContribution {
  scheme: string
  command: string
}

/** Comparing files with their originals as they're edited. (1.9) */
export interface QuickDiffContribution {
  original: string
  apply: string | null
  /** What applying changes to the original is called, such as Stage. */
  applyTitle: string | null
}

export interface StatusItem {
  /** `<extension id>.<item id>`. */
  id: string
  /** The extension's name. */
  extension: string
  alignment: 'left' | 'right'
  priority: number
  text: string
  tooltip: string | null
  command: string | null
}

export type ItemIcon =
  | { kind: 'named'; name: string }
  | { kind: 'file'; path: string }
  | { kind: 'folder'; path: string }

export interface ItemMark {
  badge: string | null
  /** A color's token, `<extension id>.<name>`. */
  color: string | null
  strikethrough: boolean
  faded: boolean
}

export interface ViewEntry {
  id: string
  parent: string | null
  label: string
  description: string | null
  tooltip: string | null
  expanded: boolean
  command: string | null
  argument: string | null
  icon: ItemIcon | null
  mark: ItemMark | null
  /** Which `view/item` menus apply to it. */
  context: string | null
  /** It can be opened with no children listed: opening asks the extension for them. */
  expandable: boolean
}

export interface Welcome {
  text: string
  buttons: { label: string; command: string; argument: string | null }[]
}

export interface ViewContent {
  welcome: Welcome | null
  /** Shown on its header and activity bar entry; 0 shows none. */
  badge: number
  /** What its menus' `when` test. */
  flags: string[]
  /** The text in its text box. */
  input: string
  message: string | null
  items: ViewEntry[]
  /** A history graph beside the items, by item. (1.11) */
  graph: GraphRow[]
  /** Shown or hidden by the extension; `null` follows the manifest. (1.13) */
  visible: boolean | null
}

/** A lane of a history graph: what it leads to, and its color (0 to 4). */
export interface GraphLane {
  id: string
  color: number
}

/**
 * A row of a history graph beside an item: its node, what that leads to,
 * and the lanes at its top and bottom.
 */
export interface GraphRow {
  item: string
  node: string
  parents: string[]
  inputs: GraphLane[]
  outputs: GraphLane[]
}

/** A question an extension asks the user. */
export type Prompt = { id: number; extension: string } & (
  | {
      type: 'pick'
      title: string
      items: { label: string; description: string | null; value: string }[]
    }
  | {
      type: 'input'
      title: string
      value: string
      placeholder: string | null
      /** What's typed is hidden: a program asking for a password or passphrase. */
      password: boolean
    }
  /** A folder, in the system's dialog; answered with its path as text. */
  | { type: 'folder'; title: string }
  | {
      type: 'confirm'
      message: string
      detail: string | null
      buttons: string[]
      destructive: boolean
    }
)

export type PromptReply =
  | { type: 'cancelled' }
  | { type: 'picked'; value: string }
  | { type: 'text'; text: string }
  | { type: 'button'; index: number }

/** An open extension preview: a tab of formatted text. */
export interface PreviewInfo {
  /** `<extension id>.<preview id>/<key>`. */
  id: string
  /** The extension's name. */
  extension: string
  title: string
  icon: string
  /** The document it shows. */
  document: number | null
  /** Whether it shows a webview page instead of content. */
  webview: boolean
}

/** Where a webview loads its page from. */
export interface WebviewInfo {
  url: string
  /** Changes when the extension replaces the page. */
  version: number
  /** Why a file page can't be shown (an untrusted folder, a file outside it). */
  blocked?: string
}

/** Where a preview's link goes, as the backend resolved it. */
export type LinkTarget =
  | { kind: 'web'; url: string }
  /** A heading in the same preview, by anchor (without `#`). */
  | { kind: 'heading'; anchor: string }
  | { kind: 'file'; path: string; anchor: string | null }
  /** Not followed; shown as text. */
  | { kind: 'unsupported'; destination: string }

export type ImageSource =
  /** Loaded with `getPreviewImage`. */
  | { kind: 'file'; path: string }
  /** Not loaded: the description is shown. */
  | { kind: 'web'; url: string }
  | { kind: 'unsupported'; source: string }

export type ColumnAlignment = 'none' | 'left' | 'center' | 'right'

/** What a preview node is. */
export type PreviewElement =
  | { type: 'paragraph' }
  | { type: 'heading'; level: number }
  | { type: 'quote' }
  | { type: 'bulletedList' }
  | { type: 'numberedList'; start: number }
  | { type: 'listItem'; checked: boolean | null }
  | { type: 'codeBlock'; language: string | null; text: string }
  | { type: 'rule' }
  | { type: 'table'; alignments: ColumnAlignment[] }
  | { type: 'tableHead' }
  | { type: 'tableRow' }
  | { type: 'tableCell' }
  | { type: 'text'; text: string }
  | { type: 'emphasis' }
  | { type: 'strong' }
  | { type: 'strikethrough' }
  | { type: 'code'; text: string }
  | { type: 'link'; target: LinkTarget; title: string | null }
  | { type: 'image'; source: ImageSource; description: string; title: string | null }
  | { type: 'lineBreak' }

/** A node of a preview: each comes after the node it's inside. */
export type PreviewNode = PreviewElement & { parent: number | null }

export interface PreviewContent {
  nodes: PreviewNode[]
  /**
   * Code blocks in known languages, by node index: each line's highlights
   * as flat `[start, end, kind]` triples, as in editor views.
   */
  highlights: Record<number, number[][]>
  /** Code blocks were left plain because time ran out; fetching again highlights them. */
  highlightsPending: boolean
}

export interface ExtensionStatus {
  id: string
  name: string
  version: string
  description: string
  author: string | null
  /** Whether it has an icon (`getExtensionIcon`). */
  hasIcon: boolean
  /** Whether it has a README (`getExtensionReadme`). */
  hasReadme: boolean
  /** The extension's folder. */
  path: string
  development: boolean
  state: ExtensionState
  error: string | null
  permissions: PermissionStatus[]
  commands: ExtensionCommand[]
  configErrors: string[]
  /** Whether it declares settings (shown in the settings editor). */
  hasSettings: boolean
  contributions: Contributions
  usage: ResourceUsage
  /** Bytes it stores (`clearExtensionStorage`). */
  storedBytes: number
  /** Its most recent build, if it's a development extension with a `[build]`. */
  build: BuildStatus | null
}

export type BuildState = 'building' | 'succeeded' | 'failed'

/** A development extension's build (`buildExtension`). */
export interface BuildStatus {
  state: BuildState
  /** Why it failed. */
  error: string | null
  /** How long the last finished build took. */
  durationMs: number | null
  /** The extension id whose log has the build's output. */
  log: string
}

/** What an extension has used during this session. */
export interface ResourceUsage {
  /** Time spent running its code. */
  busyMs: number
  calls: number
  slowestCallMs: number
  /** Memory of the running instance; 0 when not running. */
  memoryBytes: number
  peakMemoryBytes: number
  /** Document changes dropped because it was busy. */
  droppedEvents: number
  compileMs: number
  /** How long it took to start, most recently (compiling included); `null` before it has. */
  activationMs: number | null
}

export type MessageLevel = 'info' | 'warning' | 'error'

/** A mark an extension puts on a file or folder in the explorer. */
export interface FileMark {
  path: string
  /** One or two characters, such as `M`. */
  badge: string | null
  /** A color's token, `<extension id>.<name>`. */
  color: string | null
  tooltip: string | null
  /** Shown dimmed. */
  faded: boolean
  /** Marks everything inside a folder too. */
  inherit: boolean
  /** Folders that contain it show its color and a dot. */
  propagate: boolean
  /** Among marks propagated to a folder, the highest wins. */
  priority: number
}

/** One extension's explorer marks. */
export interface ExtensionMarks {
  /** The extension's id. */
  extension: string
  marks: FileMark[]
}

export const listExtensions = () => call<ExtensionStatus[]>('extensions_list')
export const reloadExtensions = () => call<void>('extensions_reload')
/** Runs an extension command for the active document; resolves when it finishes. */
export const runExtensionCommand = (
  command: string,
  documentId: number | null,
  argument: string | null = null,
) => call<void>('extension_run_command', { command, documentId, argument })
/** A file icon theme with its icons; `null` if no enabled extension has it. */
export const getIconTheme = (id: string) => call<IconTheme | null>('extensions_icon_theme', { id })
export const listStatusItems = () => call<StatusItem[]>('extension_status_items')
export const listFileMarks = () => call<ExtensionMarks[]>('extension_file_marks')
/** Tells extensions which file the editor in use shows (`null`: none). */
export const setActiveFile = (path: string | null) => call<void>('extension_active_file', { path })
/** The active editor's cursor moved to another line, or another document (or none) is in use. */
export const setSelectionChanged = (document: number | null) =>
  call<void>('extension_selection_changed', { document })
/** Asks for a folder in the system's dialog, without opening it; `null` if cancelled. */
export const pickFolderDialog = (title: string) =>
  call<string | null>('dialog_pick_folder', { title })
/** Runs a command from a view's menu or button for the selected items' arguments. */
export const runViewCommand = (command: string, argument: string | null, selection: string[]) =>
  call<void>('extension_run_view_command', { command, argument, selection })
/** Runs a code lens's command for document `document`. */
export const runCodeLens = (document: number, command: string, argument: string | null) =>
  call<void>('extension_run_code_lens', { document, command, argument })
/** Asks a view's extension for the children of an opened item. */
export const expandViewItem = (view: string, item: string) =>
  call<void>('extension_view_expand', { view, item })
/** The text in a view's text box changed. */
export const setViewInput = (view: string, text: string) =>
  call<void>('extension_view_input', { view, text })
/** Views' badges, by full view id. */
export const listViewBadges = () => call<Record<string, number>>('extension_view_badges')
export const answerPrompt = (id: number, reply: PromptReply) =>
  call<void>('extension_prompt_answer', { id, reply })
export const getViewContent = (view: string) => call<ViewContent>('extension_view', { view })
/** Tells the backend a view is shown, which may start its extension. */
export const viewOpened = (view: string) => call<void>('extension_view_opened', { view })
export const listPreviews = () => call<PreviewInfo[]>('extension_previews')
/** A preview's content; `null` if it isn't open. */
export const getPreviewContent = (preview: string) =>
  call<PreviewContent | null>('extension_preview_content', { preview })
/** Tells the backend the user closed a preview's tab. */
export const previewClosed = (preview: string) =>
  call<void>('extension_preview_closed', { preview })
/** A picture a preview shows, as a `data:` URL. */
/**
 * Opens a file in an extension's viewer, or fills it in again, and resolves
 * to the viewer's preview id once the extension has, for
 * `getPreviewContent`, `getPreviewImage`, and `previewClosed`.
 */
export const openViewer = (viewer: string, path: string) =>
  call<string>('extension_viewer_open', { viewer, path })
export const getPreviewImage = (preview: string, path: string) =>
  call<string>('extension_preview_image', { preview, path })
/** Where a viewer's or preview's webview page is; `null` if it isn't open or isn't a webview. */
export const getWebview = (preview: string) =>
  call<WebviewInfo | null>('extension_webview', { preview })
/** Passes a message (JSON text) from a webview's page to its extension. */
export const sendWebviewMessage = (preview: string, message: string) =>
  call<void>('extension_webview_message', { preview, message })
/** The workbench's theme, for webview pages served from now on. */
export const setWebviewTheme = (kind: string, variables: Record<string, string>) =>
  call<void>('extension_webview_theme', { kind, variables })
/** Saves a webview page's state (JSON text). */
export const setWebviewState = (preview: string, pageState: string) =>
  call<void>('extension_webview_set_state', { preview, pageState })
/**
 * A webview page's storage request, in its extension's `global` or
 * `workspace` scope: `get` (JSON text or `null`), `set` (`value` `null`
 * removes), or `keys`.
 */
export const webviewStorage = (
  preview: string,
  scope: string,
  op: string,
  key: string | null,
  value: string | null,
) => call<unknown>('extension_webview_storage', { preview, scope, op, key, value })
/** Removes everything an extension has stored. */
export const clearExtensionStorage = (id: string) => call<void>('extension_clear_storage', { id })
/** An extension's icon as a `data:` URL; `null` if it has none. */
export const getExtensionIcon = (id: string) => call<string | null>('extension_icon', { id })
/** An extension's README, rendered as preview content; `null` if it has none. */
export const getExtensionReadme = (id: string) =>
  call<PreviewContent | null>('extension_readme', { id })
/** A picture in an extension's README, as a `data:` URL. */
export const getExtensionReadmeImage = (id: string, path: string) =>
  call<string>('extension_readme_image', { id, path })
/** Grants `granted` and denies the extension's other requested permissions. */
export const setExtensionPermissions = (id: string, granted: Permission[]) =>
  call<void>('extension_set_permissions', { id, granted })
export const setExtensionEnabled = (id: string, enabled: boolean) =>
  call<void>('extension_set_enabled', { id, enabled })
export const restartExtension = (id: string) => call<void>('extension_restart', { id })
/** Builds a development extension (by its folder) and loads the result. */
export const buildExtension = (path: string) => call<void>('extension_build', { path })

/** Asks for a folder and loads it as a development extension. */
export const addDevelopmentExtension = () => call<string | null>('extension_add_development')

/** A button in a layout: beside its tab, or in a pane's header. */
export interface LayoutButton {
  id: string
  title: string
  /** A named icon; `null` shows the title as text. */
  icon: string | null
  command: string
  argument: string | null
  primary: boolean
  /** A toggle, on or off; `null` for a plain button. */
  pressed: boolean | null
  disabled: boolean
}

export type PaneContent =
  | { type: 'editor'; uri: string; minimap: boolean; folding: boolean; quickDiff: boolean }
  /** A page of the extension's; `id` is its webview's. */
  | { type: 'webview'; page: string | null; id: string }
  /** A file of the open folder shown as a page; `id` is its webview's. */
  | { type: 'filePage'; path: string; id: string }
  /** A page from the web, in a browser view of its own named `id`. */
  | { type: 'webPage'; url: string; id: string }

export interface PaneHeader {
  title: string
  description: string | null
  buttons: LayoutButton[]
}

export interface PaneSpec {
  content: PaneContent
  header: PaneHeader | null
  scrollGroup: string | null
}

export type LayoutNode = {
  id: string
  parent: string | null
  /** Its share of the parent split, relative to its siblings'. */
  size: number
} & ({ kind: 'split'; direction: 'row' | 'column' } | ({ kind: 'pane' } & PaneSpec))

/** What a layout tab shows, as its extension set it. */
export interface LayoutSpec {
  title: string
  /** Parents first; the first fills the tab. */
  nodes: LayoutNode[]
  /** Documents kept open besides the editor panes'. */
  documents: string[]
  buttons: LayoutButton[]
  /** Widths whose crossing runs the layout's command again. */
  breakpoints: number[]
  main: string | null
}

export interface LayoutPoint {
  line: number
  column: number
}

/** A range decorated with a decoration type (`<extension id>/<name>`). */
export interface DecoratedRange {
  key: string
  start: LayoutPoint
  end: LayoutPoint
}

export interface LineRange {
  start: number
  end: number
}

export interface ZoneLink {
  title: string
  tooltip: string | null
  command: string
  argument: string | null
}

/** Rows above document line `line` of an editor pane. */
export type PaneZone = { line: number; decoration: string | null } & (
  | { type: 'filler'; rows: number }
  | {
      type: 'text'
      text: string
      tooltip: string | null
      command: string | null
      argument: string | null
    }
  | { type: 'links'; label: string | null; links: ZoneLink[] }
  | {
      type: 'lines'
      uri: string
      lines: LineRange
      decorations: DecoratedRange[]
      numbers: boolean
    }
)

export interface RulerMark {
  lines: LineRange
  decoration: string
}

/** What an editor pane shows besides its text. */
export interface PaneDecorations {
  decorations: DecoratedRange[]
  zones: PaneZone[]
  /** Lines hidden as folded regions. */
  folds: LineRange[]
  ruler: RulerMark[]
}

export interface LayoutView {
  /** `null` until the extension sets it. */
  spec: LayoutSpec | null
  /** By pane. */
  decorations: Record<string, PaneDecorations>
}

/** What a layout tab shows; `null` if the backend has no such tab open. */
export const getLayout = (layout: string, key: string) =>
  call<LayoutView | null>('extension_layout', { layout, key })
export const getPaneDecorations = (layout: string, key: string, pane: string) =>
  call<PaneDecorations | null>('extension_pane_decorations', { layout, key, pane })
/** Runs the layout's command to fill a tab in, or to say it crossed a breakpoint. */
export const fillLayout = (
  layout: string,
  key: string,
  reason: 'opened' | 'restored' | 'resized',
  width: number,
) => call<void>('extension_layout_fill', { layout, key, reason, width })
/** The tab's documents are open: tells the extension, if they're new to it. */
export const layoutReady = (
  layout: string,
  key: string,
  documents: { uri: string; document: number }[],
) => call<void>('extension_layout_ready', { layout, key, documents })
export const layoutClosed = (layout: string, key: string) =>
  call<void>('extension_layout_closed', { layout, key })
/** The editor pane in use, when it's a layout's; all `null` when it isn't. */
export const layoutFocus = (layout: string | null, key: string | null, pane: string | null) =>
  call<void>('extension_layout_focus', { layout, key, pane })
/** Runs a command from a layout's button or zone (`pane` `null`: the tab's own buttons). */
export const runLayoutCommand = (
  layout: string,
  key: string,
  pane: string | null,
  command: string,
  argument: string | null,
  documentId: number | null,
) => call<void>('extension_layout_command', { layout, key, pane, command, argument, documentId })

/** Opens a link with an extension's link handler (its full id). */
export const openLinkWith = (
  handler: string,
  link: { kind: 'web' | 'file'; target: string; line: number | null; column: number | null },
) => call<void>('extension_open_link', { handler, link })
/** What each absolute path is: a file, a folder, or nothing (`null`). */
export const pathKinds = (paths: string[]) =>
  call<('file' | 'folder' | null)[]>('path_kinds', { paths })
/** Opens a folder in the system's file manager. */
export const openFolderExternal = (path: string) => call<void>('open_folder_external', { path })

/** Where a web page pane's view goes, in logical pixels. */
export interface WebPageBounds {
  x: number
  y: number
  width: number
  height: number
}

/**
 * The last call sent for each web page view, so the next waits for it.
 * The backend runs `web_page_show` on another thread (creating a view can't
 * block the main one) and the rest on the main thread, so calls sent
 * together could run out of order: a hide before the show it follows (the
 * view stays over the workbench), or two shows each creating the view.
 */
const webPageCalls = new Map<string, Promise<unknown>>()

function inOrder<T>(id: string, send: () => Promise<T>): Promise<T> {
  const previous = webPageCalls.get(id) ?? Promise.resolve()
  const next = previous.catch(() => {}).then(send)
  webPageCalls.set(id, next)
  const forget = () => {
    if (webPageCalls.get(id) === next) {
      webPageCalls.delete(id)
    }
  }
  next.then(forget, forget)
  return next
}

/** Shows a web page pane's view (creating it with `url` the first time). */
export const showWebPage = (id: string, url: string, bounds: WebPageBounds) =>
  inOrder(id, () => call<void>('web_page_show', { id, url, bounds }))
export const moveWebPage = (id: string, bounds: WebPageBounds) =>
  inOrder(id, () => call<void>('web_page_bounds', { id, bounds }))
export const hideWebPage = (id: string) => inOrder(id, () => call<void>('web_page_hide', { id }))
export const closeWebPage = (id: string) => inOrder(id, () => call<void>('web_page_close', { id }))
