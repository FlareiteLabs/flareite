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
