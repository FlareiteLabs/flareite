/**
 * Changing a document's indentation, as the status bar offers (ADR 0054):
 * spaces or tabs and how wide, chosen for the document, or detected again
 * from its content. A choice holds for the document whatever the settings
 * say until it closes.
 */
import { detectIndentation, setIndentation, type Indentation } from '../ipc/editor'
import { openQuickPick } from '../components/paletteState'
import { editorStatus } from '../state/editorStatus'
import { reportError } from '../state/notifications'
import { activeEditor, activeOverride, type EditorController } from './activeEditor'

/** Tab sizes offered. */
const SIZES = [1, 2, 3, 4, 5, 6, 7, 8]

/** The editor the change is for: the active one, or a layout's pane in use. */
function target(): EditorController | null {
  return activeOverride() ?? activeEditor.get()
}

/** How the status bar says it: `Spaces: 2`, or `Tab Size: 4`. */
export function describeIndentation(indentation: Indentation): string {
  return `${indentation.insertSpaces ? 'Spaces' : 'Tab Size'}: ${indentation.tabSize}`
}

/** Offers the ways to change the active document's indentation. */
export function changeIndentation(): void {
  openQuickPick({
    placeholder: 'Change the indentation',
    items: [
      { label: 'Indent Using Spaces', run: () => chooseTabSize(true) },
      { label: 'Indent Using Tabs', run: () => chooseTabSize(false) },
      { label: 'Detect Indentation from Content', run: () => void detectAgain() },
    ],
  })
}

/** Asks how wide, then indents the active document with spaces (or tabs). */
export function chooseTabSize(insertSpaces: boolean): void {
  const editor = target()
  if (!editor) {
    return
  }
  const current = editorStatus.get()?.indentation
  openQuickPick({
    placeholder: insertSpaces ? 'Spaces per indentation level' : 'Width of a tab, in columns',
    items: SIZES.map(size => ({
      label: String(size),
      detail:
        current && current.tabSize === size && current.insertSpaces === insertSpaces
          ? 'Current'
          : undefined,
      run: () => void apply(editor, () => setIndentation(editor.documentId, size, insertSpaces)),
    })),
  })
}

/** Indents the active document as its content does. */
export async function detectAgain(): Promise<void> {
  const editor = target()
  if (editor) {
    await apply(editor, () => detectIndentation(editor.documentId))
  }
}

async function apply(editor: EditorController, change: () => Promise<Indentation>): Promise<void> {
  try {
    await change()
    editor.refresh()
  } catch (error) {
    reportError('Changing the indentation', error)
  }
}
