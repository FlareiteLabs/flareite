import type { Indentation, LanguageStatus } from '../ipc/editor'
import { createStore } from './store'

/** Cursor information for the active editor, shown in the status bar. */
export interface EditorStatus {
  /** One-based line and visual column of the primary cursor. */
  line: number
  column: number
  selectionCount: number
  language: LanguageStatus | null
  /** How the document indents, if known. */
  indentation?: Indentation | null
}

export const editorStatus = createStore<EditorStatus | null>(null)
