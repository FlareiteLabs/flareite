/**
 * Tells extensions which file the editor in use shows, as it changes, for
 * views that follow it (Git's File History). It's the current group's tab:
 * a document's file, a viewer's, or a layout's file (see `layoutFile`). A tab that
 * isn't an editor's (a view, a terminal) leaves it as it was, so moving to
 * the side bar keeps the file, as in VS Code. It also tells them when the
 * active editor's cursor moves to another line (for Git's blame).
 */
import { activeEditor } from '../editor/activeEditor'
import { setActiveFile, setSelectionChanged } from '../ipc/extensions'
import { editorStatus } from '../state/editorStatus'
import { layoutFile, layoutsStore } from '../state/layouts'
import { documentsStore } from '../state/documents'
import { reportError } from '../state/notifications'
import { currentGroup, dockStore, parseDocumentItem, parseViewerItem } from './dock'
import { findGroup, itemKind } from './model'

/** How long the file must stay before extensions are told. */
const SETTLE_MS = 150

/** The file the current tab shows; `undefined` for a tab that isn't an editor's. */
function currentFile(): string | null | undefined {
  const item = findGroup(dockStore.get(), currentGroup.get())?.active
  if (!item) {
    return null
  }
  switch (itemKind(item)) {
    case 'document': {
      const { document } = parseDocumentItem(item)
      return (
        documentsStore.get().documents.find(candidate => candidate.id === document)?.path ?? null
      )
    }
    case 'viewer':
      return parseViewerItem(item).path
    case 'layout':
      return layoutFile(item)
    case 'preview':
    case 'page':
      return null
    default:
      return undefined
  }
}

let sent: string | null = null
let timer: ReturnType<typeof setTimeout> | undefined

function check(): void {
  clearTimeout(timer)
  timer = setTimeout(() => {
    const file = currentFile()
    if (file === undefined || file === sent) {
      return
    }
    sent = file
    setActiveFile(file).catch((error: unknown) => reportError('Following the active file', error))
  }, SETTLE_MS)
}

/** The document and line of the active editor's cursor, as last sent. */
let sentLine: string | null = null
let lineTimer: ReturnType<typeof setTimeout> | undefined

/**
 * Tells extensions when the active editor's cursor moves to another line,
 * or another document is in use, for what follows the line (Git's blame).
 */
function checkLine(): void {
  clearTimeout(lineTimer)
  lineTimer = setTimeout(() => {
    const editor = activeEditor.get()
    const line = editor?.cursor()?.line
    const key = editor ? `${editor.documentId}:${line ?? -1}` : 'none'
    if (key === sentLine) {
      return
    }
    sentLine = key
    setSelectionChanged(editor?.documentId ?? null).catch((error: unknown) =>
      reportError('Following the cursor', error),
    )
  }, SETTLE_MS)
}

/** Starts telling extensions; returns a function that stops. */
export function followActiveFile(): () => void {
  const stops = [
    dockStore.subscribe(check),
    currentGroup.subscribe(check),
    documentsStore.subscribe(check),
    layoutsStore.subscribe(check),
    activeEditor.subscribe(checkLine),
    editorStatus.subscribe(checkLine),
  ]
  check()
  checkLine()
  return () => {
    clearTimeout(timer)
    clearTimeout(lineTimer)
    for (const stop of stops) {
      stop()
    }
  }
}
