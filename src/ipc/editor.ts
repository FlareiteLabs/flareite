/**
 * Typed wrappers for the document and editing commands in
 * `src-tauri/src/editor/commands.rs`. Shapes mirror `editor/protocol.rs`.
 *
 * Positions are a line plus a UTF-16 column, i.e. a JavaScript string index
 * into that line's text.
 */
import { call } from './index'

export interface Position {
  line: number
  column: number
}

/**
 * Display rows `startRow..endRow` (end exclusive). Rows skip the hidden
 * lines of folded regions, and a wrapped line takes a row for each of its
 * rows.
 */
export interface Viewport {
  startRow: number
  endRow: number
  /** Where the editor wraps lines; absent if it doesn't. */
  wrap?: WrapOptions
}

export type Motion =
  | 'left'
  | 'right'
  | 'wordLeft'
  | 'wordRight'
  | 'up'
  | 'down'
  | 'pageUp'
  | 'pageDown'
  | 'lineStart'
  | 'lineEnd'
  | 'documentStart'
  | 'documentEnd'

export type EditorOp =
  | { type: 'insert'; text: string }
  /**
   * Text typed at the keyboard: like insert, but brackets and quotes are
   * typed in pairs as the language declares (closers added, stepped over,
   * or wrapped around the selection).
   */
  | { type: 'type'; text: string }
  /**
   * Text an extension inserts after typing (from `autoInsert`): only if the
   * document is still at `version` with its only cursor at `position`.
   * `cursor` is where to leave the cursor, in UTF-8 bytes into `text`.
   */
  | { type: 'autoInsert'; version: number; position: Position; text: string; cursor: number }
  /**
   * A snippet with tab stops (`$1`, `${1:placeholder}`, `$0`) at each
   * selection; Tab and Shift+Tab then move between its stops. With
   * `adjustIndentation`, its lines are indented like the line it's on.
   */
  | { type: 'insertSnippet'; snippet: string; adjustIndentation: boolean }
  /** Like insert, but text copied as whole lines pastes as whole lines. */
  | { type: 'paste'; text: string }
  | { type: 'newline' }
  | { type: 'insertLine'; direction: 'up' | 'down' }
  | { type: 'tab' }
  | { type: 'indent' }
  | { type: 'outdent' }
  /** Uses the comment syntax of the document's language. */
  | { type: 'toggleComment' }
  | { type: 'moveLines'; direction: 'up' | 'down' }
  | { type: 'copyLines'; direction: 'up' | 'down' }
  | { type: 'deleteLines' }
  | { type: 'delete'; direction: 'backward' | 'forward'; unit: 'grapheme' | 'word' }
  | { type: 'deleteSelection' }
  | { type: 'move'; motion: Motion; extend: boolean; pageLines?: number }
  | { type: 'selectAll' }
  | { type: 'selectLine' }
  | { type: 'cursorsAtLineEnds' }
  | { type: 'collapseSelections' }
  | { type: 'cursorUndo' }
  | { type: 'setCursor'; position: Position; extend: boolean; add: boolean }
  | { type: 'selectWord'; position: Position }
  | { type: 'addCursor'; direction: 'up' | 'down' }
  | { type: 'undo' }
  | { type: 'redo' }
  | { type: 'findNext'; query: SearchQuery; backward: boolean }
  | { type: 'selectAllMatches'; query: SearchQuery }
  | { type: 'replace'; query: SearchQuery; replacement: string }
  | { type: 'replaceAll'; query: SearchQuery; replacement: string }
  | { type: 'addNextMatch' }
  | { type: 'selectAllOccurrences' }
  | { type: 'jumpToBracket' }
  /** Fold operations use the document's foldable regions around the cursors. */
  | { type: 'fold' }
  | { type: 'unfold' }
  | { type: 'toggleFold' }
  /** A click on the fold marker of `line`. */
  | { type: 'toggleFoldAt'; line: number }
  | { type: 'foldAll' }
  | { type: 'unfoldAll' }
  /** Every region marked by `#region` comments. */
  | { type: 'foldAllRegions' }
  | { type: 'unfoldAllRegions' }
  | { type: 'foldRecursively' }
  | { type: 'unfoldRecursively' }
  /** Folds the regions nested `level` deep (1 is outermost), except around the cursors. */
  | { type: 'foldLevel'; level: number }
  /** Folds exactly these `[start, end)` line ranges (line `start` stays visible), unfolding others. */
  | { type: 'setFolds'; regions: [number, number][] }

/** Where a view wraps lines, and how it indents the rows after a line's first. */
export interface WrapOptions {
  columns: number
  indent: 'none' | 'same' | 'indent' | 'deepIndent'
}

/** A wrap as a string, to compare: empty for none. */
export const wrapKey = (wrap: WrapOptions | undefined | null): string =>
  wrap ? `${wrap.columns}:${wrap.indent}` : ''

/**
 * Where a line wraps: the UTF-16 columns that start its rows after the
 * first, and those rows' indent, in columns (`ch`).
 */
export interface LineWrap {
  line: number
  breaks: number[]
  indent: number
}

export interface SearchQuery {
  pattern: string
  regex: boolean
  caseSensitive: boolean
  wholeWord: boolean
}

export interface Range {
  start: Position
  end: Position
}

/** Find results included in a view when the request carried a query. */
export interface SearchView {
  /** Matches on the returned lines. */
  matches: Range[]
  /** Total matches, or `null` while a background count is running. */
  total: number | null
  truncated: boolean
  /** Index of the match the primary selection covers, if any. */
  current: number | null
  /** Why the query could not be used, e.g. an invalid regular expression. */
  error: string | null
}

export type ClipboardAction = 'copy' | 'cut' | 'paste'

export interface DocumentSummary {
  id: number
  title: string
  path: string | null
  dirty: boolean
  lineEnding: string
  /** The encoding's label, e.g. `UTF-8` or `Western (Windows 1252)`. */
  encoding: string
  /** Its id, e.g. `utf-8` or `windows-1252`. */
  encodingId: string
}

/** An encoding a document can be reopened or saved with. */
export interface EncodingChoice {
  id: string
  label: string
}

export interface SelectionView {
  anchor: Position
  head: Position
  primary: boolean
}

/** Lines `start..start + removed` were replaced by `start..start + inserted`. */
export interface LineSplice {
  start: number
  removed: number
  inserted: number
}

/** A line as sent: its text, or its first part plus full length if it was cut. */
export type WireLine = string | { text: string; fullLength: number }

/**
 * How to update held lines: replay `splices` on the lines held at version
 * `base` (or drop them all when `base` is null), then fill in `chunks`.
 */
export interface LinesUpdate {
  base: number | null
  splices: LineSplice[]
  chunks: { start: number; lines: WireLine[] }[]
}

/** Lines a client holds at `version`: the `visible` ranges of its last view. */
export interface KnownLines {
  version: number
  ranges: [number, number][]
}

/** Highlight categories, in protocol order: span kind `k` is `TOKEN_KINDS[k - 1]`. */
export const TOKEN_KINDS = [
  'comment',
  'keyword',
  'string',
  'escape',
  'number',
  'constant',
  'function',
  'macro',
  'type',
  'variable',
  'property',
  'operator',
  'punctuation',
  'attribute',
  'tag',
  'label',
  'module',
] as const

/**
 * A line's syntax highlights, flattened as `[start, end, kind, ...]` with
 * UTF-16 columns, in order and non-overlapping.
 */
export interface LineHighlights {
  line: number
  spans: number[]
}

/** A diagnostic on the view's lines. Severity: 1 error, 2 warning, 3 info, 4 hint. */
export interface DiagnosticView {
  start: Position
  end: Position
  severity: number
}

export interface ServerStatus {
  name: string
  state: 'starting' | 'ready' | 'busy' | 'failed'
  message: string | null
}

/** The document's language and language server. */
export interface LanguageStatus {
  /** The language's id, e.g. `css`. */
  id: string
  /** Its name, e.g. `CSS`. */
  language: string
  server: ServerStatus | null
  /** Characters that open completion; `null` without completion support. */
  completionTriggers: string[] | null
  /**
   * Characters whose typing asks extensions for text to insert, such as an
   * HTML closing tag; absent or `null` if none insert for the language.
   */
  autoInsertTriggers?: string[] | null
  /**
   * Characters that open signature help (`open`), and those that update it
   * while it's open; absent or `null` without signature help.
   */
  signatureTriggers?: { open: string[]; update: string[] } | null
  /** Whether the language server offers code actions. */
  codeActions?: boolean
}

interface ViewFields {
  documentId: number
  version: number
  lineCount: number
  /** Display rows: lines less those hidden in folded regions, plus wrapped lines' extra rows. */
  rowCount: number
  /**
   * The viewport: rows from `startRow`, showing the `visible` line ranges
   * (`[start, end)`), which run from `startLine` to `endLine`.
   */
  startRow: number
  startLine: number
  endLine: number
  visible: [number, number][]
  /** The visible lines that wrap, in order; absent if none do. */
  wraps?: LineWrap[]
  /** Visible lines whose region is folded. */
  folded: number[]
  /** Visible lines that start a foldable region. */
  foldable: number[]
  /** Changes whenever the folded regions do. */
  foldGeneration: number
  /** Selections touching the viewport, plus the primary selection. */
  selections: SelectionView[]
  selectionCount: number
  /** The primary cursor, and its display row. */
  primary: { position: Position; visualColumn: number; row: number }
  dirty: boolean
  search: SearchView | null
  diagnostics: DiagnosticView[]
  language: LanguageStatus | null
  /** Extension decorations on the view's lines. */
  decorations: DecorationView[]
  /** Changes from the file's original touching the view's lines, for the gutter. */
  changes: GutterChange[]
  /** Extensions' code lenses in the whole document, in order. */
  lenses: CodeLens[]
  /** Extensions' faint text after the view's lines. */
  annotations: LineAnnotation[]
}

/** Faint text an extension shows after a line, such as who last changed it. */
export interface LineAnnotation {
  line: number
  text: string
  tooltip: string | null
}

/** A link an extension shows above a line; clicking it runs `command`. */
export interface CodeLens {
  line: number
  title: string
  tooltip: string | null
  command: string
  argument: string | null
}

/**
 * Lines changed from a file's original (an extension names it, such as
 * Git's index): `start..end`, or, for lines removed, the place before `start`.
 */
export interface GutterChange {
  start: number
  end: number
  kind: 'added' | 'modified' | 'removed'
}

/** A change from a file's original, with its original lines, to show inline. */
export interface Hunk {
  /** Its place among the changes (from 0), and how many there are. */
  index: number
  total: number
  original: [number, number]
  modified: [number, number]
  lines: PeekLines
  chars: { original: Range; modified: Range }[]
  /** What applying it to the original is called (such as Stage), if it can be. */
  applyTitle: string | null
}

/** The change at `line` of a document, or the next or previous one from it. */
export const quickDiffHunk = (id: number, line: number, direction: 'at' | 'next' | 'previous') =>
  call<Hunk | null>('quick_diff_hunk', { id, line, direction })
/**
 * Reverts a document's changes from its original that `lines` (`[first, last]`
 * pairs) touch, or else those the selections of its view `view` touch.
 */
export const quickDiffRevert = (id: number, view: number, lines: [number, number][] | null) =>
  call<boolean>('quick_diff_revert', { id, view, lines })
/** Applies (stages) a document's changes, chosen as for `quickDiffRevert`, to its original. */
export const quickDiffApply = (id: number, view: number, lines: [number, number][] | null) =>
  call<boolean>('quick_diff_apply', { id, view, lines })
/** A range decorated by an extension. */
export interface DecorationView {
  start: Position
  end: Position
  /** The decoration type: `<extension id>/<name>`. */
  key: string
}

/** A view as the backend sends it: only the lines the client was missing. */
export interface WireView extends ViewFields {
  lines: LinesUpdate
  /**
   * Highlights for lines whose spans changed. Lines in `lines.chunks` start
   * with none; other held lines keep theirs.
   */
  highlights: LineHighlights[]
}

export interface DisplayLine {
  text: string
  /** Full length in UTF-16 units when `text` was shortened for display. */
  fullLength: number | null
  /** Syntax highlights, as in `LineHighlights.spans`. */
  spans: readonly number[]
}

/** A view with every viewport line resolved, ready to render. */
export interface EditorView extends ViewFields {
  /** The visible lines, in order from row `startRow`. */
  lines: DisplayLine[]
  /** The document line each of `lines` is, in order. */
  lineNumbers: number[]
  /**
   * The row each of `lines` starts on, counted from `startRow`, and then the
   * rows they take in all (one longer than `lines`).
   */
  rowStarts: number[]
  /** `wraps`, by line. */
  wrapped: ReadonlyMap<number, LineWrap>
  /** The wrap the view was made with, as `wrapKey` writes it. */
  wrapKey: string
}

/**
 * Display rows `startRow..startRow + lines.length` drawn small for the
 * minimap: each line's runs of non-blank columns as `[start, end, kind,
 * ...]`, in visual columns. Kind 0 is plain text; others index
 * `TOKEN_KINDS` from 1.
 */
export interface MinimapView {
  documentId: number
  version: number
  rowCount: number
  startRow: number
  lines: number[][]
  /** Section headers on these rows, to label. */
  headers: MinimapHeader[]
}

/**
 * A labelled minimap row: a named `#region` comment (`region`), or a
 * `MARK:` comment, which may draw a separator above it (`MARK: -`).
 */
export interface MinimapHeader {
  row: number
  name: string
  separator: boolean
  region: boolean
}

/** Lines of a file for a peek view's preview, from `start`. */
export interface PeekLines {
  lineCount: number
  start: number
  lines: { text: string; spans: number[] }[]
}

export const listDocuments = () => call<DocumentSummary[]>('document_list')
/** Creates an untitled document, optionally with `text`. */
export const newDocument = (text?: string) =>
  call<DocumentSummary>('document_new', { text: text ?? null })
/** Asks for a file to open; `null` if cancelled. */
export const pickFile = () => call<string | null>('document_pick_file')
/** Saves a document; `format: false` skips formatting it on save. */
export const saveDocument = (id: number, format = true) =>
  call<DocumentSummary | null>('document_save', { id, format })
export const saveDocumentAs = (id: number) =>
  call<DocumentSummary | null>('document_save_as', { id })
export const closeDocument = (id: number) => call<boolean>('document_close', { id })
/**
 * Opens a file as text. One that looks binary fails with `binary`, unless
 * `binary` is set (the user chose to open it anyway).
 */
export const openDocument = (path: string, binary = false, hidden = false) =>
  call<DocumentSummary>('document_open', { path, binary, hidden })
/**
 * Opens the read-only document an extension provides at `uri` (such as
 * `git:HEAD:src/main.rs`), asking the extension for its text.
 *
 * With `hidden`, a document not open yet opens only for a diff tab, and
 * isn't listed (`listDocuments`) until opened without it. The same holds
 * for `openDocument`.
 */
export const openProvidedDocument = (uri: string, hidden = false) =>
  call<DocumentSummary>('document_open_provided', { uri, hidden })
/** Stops listing a document whose last tab closed while a layout tab still shows it. */
export const hideDocument = (id: number) => call<void>('document_hide', { id })

/** Lines that changed: `original` lines `[start, end)` became `modified` ones. */
export interface LineChange {
  original: [number, number]
  modified: [number, number]
  /** The characters that changed within them, when they were compared. */
  chars: { original: Range; modified: Range }[]
}

/** Lines `start..end` of an open document with their highlights. */
export const documentLines = (id: number, start: number, end: number) =>
  call<PeekLines>('editor_document_lines', { id, start, end })
/** A language a document can be set to. */
export interface LanguageChoice {
  id: string
  name: string
  /** The file extensions it's for, without the dot. */
  fileExtensions: string[]
}

/** The languages a document can be set to, by name. */
export const listLanguages = () => call<LanguageChoice[]>('languages_list')
/** Sets a document's language: an id, `plaintext` for none, or `null` to go by its file name. */
export const setDocumentLanguage = (id: number, language: string | null) =>
  call<void>('document_set_language', { id, language })

export const listEncodings = () => call<EncodingChoice[]>('document_encodings')
export const reopenWithEncoding = (id: number, encoding: string) =>
  call<DocumentSummary>('document_reopen_with_encoding', { id, encoding })
export const saveWithEncoding = (id: number, encoding: string) =>
  call<DocumentSummary | null>('document_save_with_encoding', { id, encoding })

/*
 * Each editor showing a document has a view of it (`view`, numbered by the
 * dock; the first is 0) with its own selections and folds.
 */
export const getView = (
  id: number,
  view: number,
  viewport: Viewport,
  known: KnownLines | null,
  search: SearchQuery | null,
) => call<WireView>('editor_view', { id, view, viewport, known, search })
export const getMinimap = (id: number, view: number, startRow: number, endRow: number) =>
  call<MinimapView>('editor_minimap', { id, view, startRow, endRow })
export const applyOps = (
  id: number,
  view: number,
  ops: EditorOp[],
  viewport: Viewport,
  known: KnownLines | null,
  search: SearchQuery | null,
) => call<WireView>('editor_apply', { id, view, ops, viewport, known, search })
export const clipboard = (
  id: number,
  view: number,
  action: ClipboardAction,
  viewport: Viewport,
  known: KnownLines | null,
  search: SearchQuery | null,
) => call<WireView>('editor_clipboard', { id, view, action, viewport, known, search })
/** Text an extension inserts after typing, as an `autoInsert` operation would apply it. */
export interface AutoInsert {
  version: number
  position: Position
  text: string
  cursor: number
}
/**
 * Asks extensions what to insert at a view's cursor now that `trigger` was
 * typed there (an HTML closing tag after `>`); `null` for nothing.
 */
export const autoInsert = (id: number, view: number, trigger: string) =>
  call<AutoInsert | null>('editor_auto_insert', { id, view, trigger })
/** Makes view `to` of a document a copy of its view `from`: the same selections and folds. */
export const copyView = (id: number, from: number, to: number) =>
  call<void>('editor_copy_view', { id, from, to })
/** Forgets a view of a document that no editor shows any more. */
export const closeView = (id: number, view: number) => call<void>('editor_close_view', { id, view })
/**
 * Lines `start..end` of the file at `path` with their highlights: the open
 * document's if it's open, otherwise the file on disk.
 */
export const peekLines = (path: string, start: number, end: number) =>
  call<PeekLines>('editor_peek_lines', { path, start, end })
