/**
 * Reverting or applying (staging, for Git) the changes from a file's
 * original that the active editor's selections touch, for the commands and
 * for extensions (`quick-diff::apply-selected-ranges`). The editor may be
 * a layout's pane, such as a Git diff's working tree side.
 */
import * as ipc from '../ipc/editor'
import { activeEditor } from '../editor/activeEditor'
import { reportError } from './notifications'

/** Reverts the changes the active editor's selections touch to the file's original. */
export async function revertSelectedRanges(): Promise<void> {
  const editor = activeEditor.get()
  if (!editor) {
    return
  }
  try {
    await ipc.quickDiffRevert(editor.documentId, editor.view, null)
  } catch (error) {
    reportError('Reverting the changes', error)
  }
}

/** Applies (stages) the changes the active editor's selections touch to its original. */
export async function applySelectedRanges(): Promise<void> {
  const editor = activeEditor.get()
  if (!editor) {
    return
  }
  try {
    await ipc.quickDiffApply(editor.documentId, editor.view, null)
  } catch (error) {
    reportError('Applying the changes', error)
  }
}
