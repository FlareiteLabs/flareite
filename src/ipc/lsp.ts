/**
 * Typed wrappers for the language server commands in
 * `src-tauri/src/lsp/commands.rs`. Each resolves to `null` when there is
 * nothing to show (no server, no answer, or the server failed).
 */
import { call } from './index'
import type { Position } from './editor'
import type { PreviewContent } from './extensions'

export interface DiagnosticMessage {
  severity: number
  message: string
  source: string | null
}

export interface Hover {
  /** What the server and any schema say, from their Markdown, drawn as previews are. */
  content: PreviewContent | null
  diagnostics: DiagnosticMessage[]
}

export interface Location {
  path: string
  start: Position
  end: Position
}

/** A reference to a symbol. */
export interface Reference extends Location {
  /** The text of the line it starts on. */
  line: string
}

/** A symbol in a document, in the order of `documentSymbols`. */
export interface DocumentSymbol {
  name: string
  kind: string
  /** The name of the symbol it's in. */
  container: string | null
  deprecated: boolean
  /** The whole symbol: a function with its body, say. */
  start: Position
  end: Position
  /** Where its name starts. */
  selection: Position
}

/** Where a workspace symbol is, as its server put it; see `resolveSymbol`. */
export interface SymbolLocation {
  path: string
  /** In the server's terms; `null` when it named only the file. */
  range: unknown
  encoding: string
}

export interface WorkspaceSymbol {
  name: string
  kind: string
  container: string | null
  deprecated: boolean
  location: SymbolLocation
}

export interface CompletionItem {
  label: string
  kind: string | null
  detail: string | null
  /** Replaces the text from `from` to the cursor. */
  insert: string
  from: Position
  /** Matched against what is typed after `from`. */
  filter: string
  /** Whether `insert` is a snippet, with tab stops (`$1`, `${1:name}`, `$0`). */
  snippet: boolean
  /** For a snippet: whether its lines are indented like the cursor's. */
  adjustIndentation: boolean
}

export interface Completions {
  items: CompletionItem[]
  /** Typing more should ask again rather than filter these. */
  incomplete: boolean
}

export const hover = (id: number, position: Position) =>
  call<Hover | null>('lsp_hover', { id, position })
export const definition = (id: number, position: Position) =>
  call<Location | null>('lsp_definition', { id, position })
/** References to the symbol at `position`, including its declaration, by path and position. */
export const references = (id: number, position: Position) =>
  call<Reference[] | null>('lsp_references', { id, position })
/** A document's symbols by position, each after the one it's in; `null` if its server can't list them. */
export const documentSymbols = (id: number) =>
  call<DocumentSymbol[] | null>('lsp_document_symbols', { id })
/** Symbols matching `query` from every running language server. A newer search cancels this one. */
export const workspaceSymbols = (query: string) =>
  call<WorkspaceSymbol[]>('lsp_workspace_symbols', { query })
/** Where a workspace symbol is in its file now; `null` if the file can't be read. */
export const resolveSymbol = (location: SymbolLocation) =>
  call<Location | null>('lsp_resolve_symbol', { location })
export const completion = (id: number, position: Position, trigger: string | null) =>
  call<Completions | null>('lsp_completion', { id, position, trigger })

/** A range of a document. */
export interface TextRange {
  start: Position
  end: Position
}

/** What formatting did: `noFormatter` when no language server formats the document (or ranges of it). */
export type FormatOutcome = 'formatted' | 'unchanged' | 'noFormatter' | 'failed'

/** Formats a document with its language server, or `range` of it. */
export const format = (id: number, range: TextRange | null) =>
  call<FormatOutcome>('lsp_format', { id, range })

/** What renaming at a place renames, and the name to start from. */
export interface RenameTarget extends TextRange {
  placeholder: string
}

/** What a language server's edit changed. */
export interface AppliedEdit {
  /** Open documents edited (left unsaved). */
  documents: number
  /** Files that weren't open, edited and saved, created, renamed, or deleted. */
  files: string[]
  /** What couldn't be done, after the rest was. */
  failures: string[]
}

/**
 * What renaming the symbol at `position` would rename; `null` if no language
 * server renames there. Fails with code `cannotRename` and the server's reason.
 */
export const prepareRename = (id: number, position: Position) =>
  call<RenameTarget | null>('lsp_prepare_rename', { id, position })
export const rename = (id: number, position: Position, name: string) =>
  call<AppliedEdit>('lsp_rename', { id, position, name })

/** The code action kind of Organize Imports. */
export const ORGANIZE_IMPORTS = 'source.organizeImports'

export interface CodeAction {
  title: string
  /** Such as `quickfix` or `refactor.extract`. */
  kind: string | null
  /** The fix to apply when one is chosen without looking. */
  preferred: boolean
  /** Why it can't be applied now, if it can't. */
  disabled: string | null
}

export interface CodeActions {
  /** Names this list for `applyCodeAction`. */
  token: number
  /** Preferred fixes first, then other fixes, then the rest. */
  actions: CodeAction[]
}

/**
 * The code actions for `range`: fixes for its problems, refactorings, and
 * so on. `automatic` when the editor looks for them where the cursor rests
 * (those that can't be applied are left out); `only` limits them to a kind
 * such as `source.organizeImports`. `null` without a server that offers them.
 */
export const codeActions = (
  id: number,
  range: TextRange,
  automatic: boolean,
  only: string | null = null,
) => call<CodeActions | null>('lsp_code_actions', { id, range, automatic, only })

export interface CodeActionApplied {
  edit: AppliedEdit
  /** A command for the editor itself that the action asked for, such as `editor.action.triggerSuggest`. */
  command: string | null
}

export const applyCodeAction = (token: number, index: number) =>
  call<CodeActionApplied>('lsp_apply_code_action', { token, index })

export interface SignatureParameter {
  /** Where it is in its signature's label. */
  start: number
  end: number
  documentation: PreviewContent | null
}

export interface Signature {
  label: string
  documentation: PreviewContent | null
  parameters: SignatureParameter[]
  /** The parameter being written, if known. */
  activeParameter: number | null
}

export interface SignatureHelp {
  signatures: Signature[]
  activeSignature: number
}

/**
 * The signatures of the call at `position`. `trigger` is the character whose
 * typing asked; `retrigger` while help is showing.
 */
export const signatureHelp = (
  id: number,
  position: Position,
  trigger: string | null,
  retrigger: boolean,
) => call<SignatureHelp | null>('lsp_signature_help', { id, position, trigger, retrigger })
