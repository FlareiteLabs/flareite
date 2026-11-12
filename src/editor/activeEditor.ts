/**
 * Handles on the editor views on screen, so commands (find, go to line, add
 * next match) and events can act on them without knowing about React.
 * Several editors can be visible at once (one per dock group), and several
 * can show the same document, each with a view of its own (see
 * `ipc/editor.ts`). The active editor is the one showing the active
 * document in the view the dock last used for it.
 */
import type { EditorOp, Position } from '../ipc/editor'
import { createStore } from '../state/store'

/**
 * How an operation's cursor is scrolled into view: just enough (`nearest`,
 * as when typing), to the middle (`center`), or to the middle only when it
 * is off screen (`centerIfOutside`, as when navigating), as in VS Code; or
 * not at all (`none`).
 */
export type Reveal = 'nearest' | 'center' | 'centerIfOutside' | 'none'

/** Whole lines highlighted and scrolled to without moving the cursor. */
export interface LinePreview {
  start: number
  end: number
  /** The line scrolled to the middle. */
  center: number
}

export interface ScrollPosition {
  top: number
  left: number
}

export interface EditorController {
  documentId: number
  /** The document's view it shows. */
  view: number
  /** Applies `op`, then reveals the cursor (`nearest` by default). */
  dispatch(op: EditorOp, reveal?: Reveal): void
  /** Opens the find widget, prefilled from a single-line selection. */
  openFind(replace: boolean): void
  /** Moves to the next or previous match of the find widget's query. */
  findNext(backward: boolean): void
  /** Re-fetches the view, e.g. after the document was reloaded from disk. */
  refresh(): void
  focus(): void
  lineCount(): number
  /** Scrolls so `line` is at the top of the viewport. */
  scrollToLine(line: number): void
  /** Scrolls by `lines` lines (negative for up) without moving the cursor. */
  scrollBy(lines: number): void
  /** Goes to the definition of the symbol at the cursor. */
  goToDefinition(): void
  /** Goes to the one reference to the symbol at the cursor, or peeks at several. */
  goToReferences(): void
  /** Peeks at the references to the symbol at the cursor. */
  peekReferences(): void
  /** Opens completion at the cursor. */
  triggerSuggest(): void
  /** Shows hover information for the symbol at the cursor. */
  showHover(): void
  /** Formats the document with its language server, or the selected lines. */
  format(selection: boolean): void
  /** Opens the box that renames the symbol at the cursor everywhere. */
  rename(): void
  /** Shows the code actions for the selection; with `only` (a kind), applies the first of that kind. */
  codeActions(only: string | null): void
  /** Shows signature help for the call at the cursor. */
  triggerParameterHints(): void
  /** Shows the next (or previous) change from the file's original inline. */
  showChange(backward: boolean): void
  /** The primary cursor, once the view has arrived. */
  cursor(): Position | null
  /** Highlights lines and scrolls to them (Go to Line and Go to Symbol), or stops. */
  previewLines(preview: LinePreview | null): void
  scrollPosition(): ScrollPosition
  /** Scrolls back to a position from `scrollPosition`. */
  restoreScroll(position: ScrollPosition): void
}

export const activeEditor = createStore<EditorController | null>(null)

const key = (documentId: number, view: number) => `${documentId}:${view}`

/** Mounted editors by document and view; the last one mounted wins. */
const mounted = new Map<string, EditorController>()
/** The view of each document the dock last used, where it isn't view 0. */
const shownViews = new Map<number, number>()
let activeDocument: number | null = null
let lastUsed: EditorController | null = null
/** The editor in use in a tab that isn't a document's, such as a diff's. */
let override: EditorController | null = null

function update(): void {
  if (override && mounted.get(key(override.documentId, override.view)) === override) {
    activeEditor.set(override)
    return
  }
  const active = activeDocument === null ? null : editorFor(activeDocument)
  activeEditor.set(
    active ??
      (lastUsed && mounted.get(key(lastUsed.documentId, lastUsed.view)) === lastUsed
        ? lastUsed
        : null),
  )
}

/** Registers a mounted editor view; returns the function that unregisters it. */
export function registerEditor(controller: EditorController): () => void {
  const at = key(controller.documentId, controller.view)
  mounted.set(at, controller)
  lastUsed = controller
  update()
  return () => {
    if (mounted.get(at) === controller) {
      mounted.delete(at)
    }
    if (lastUsed === controller) {
      lastUsed = null
    }
    update()
  }
}

/**
 * The editor showing a document in the view the dock last used for it, if
 * it's mounted: the one to select in or focus. `null` while that view's
 * tab is about to mount, so a selection waits for it (`pendingSelection`).
 */
export function editorFor(documentId: number): EditorController | null {
  return mounted.get(key(documentId, shownView(documentId))) ?? null
}

/** The view of a document the dock last used. */
export function shownView(documentId: number): number {
  return shownViews.get(documentId) ?? 0
}

/** Every mounted editor showing a document, in any view. */
export function editorsFor(documentId: number): EditorController[] {
  return mountedEditors().filter(editor => editor.documentId === documentId)
}

export function mountedEditors(): EditorController[] {
  return [...mounted.values()]
}

/** Called by the dock when it uses another view of a document (`null` when it has none). */
export function setShownView(documentId: number, view: number | null): void {
  if ((shownViews.get(documentId) ?? 0) === (view ?? 0)) {
    return
  }
  if (view) {
    shownViews.set(documentId, view)
  } else {
    shownViews.delete(documentId)
  }
  update()
}

/** Scroll positions of editors that were unmounted (tab switches), by document and view. */
const scrollPositions = new Map<string, ScrollPosition>()

export function savedScroll(documentId: number, view: number): ScrollPosition | null {
  return scrollPositions.get(key(documentId, view)) ?? null
}

export function saveScroll(
  documentId: number,
  view: number,
  position: ScrollPosition | null,
): void {
  if (position) {
    scrollPositions.set(key(documentId, view), position)
  } else {
    scrollPositions.delete(key(documentId, view))
  }
}

/**
 * Makes `editor` the active one while its tab (a diff's) is in use, whatever
 * the active document; `null` when another tab is.
 */
export function setActiveOverride(editor: EditorController | null): void {
  if (override !== editor) {
    override = editor
    update()
  }
}

/** The editor `setActiveOverride` set, while it's mounted. */
export function activeOverride(): EditorController | null {
  return override && mounted.get(key(override.documentId, override.view)) === override
    ? override
    : null
}

/** Called when the active document changes. */
export function setActiveDocument(id: number | null): void {
  activeDocument = id
  update()
}

/**
 * Selects `start..end` (or places the cursor at `start`) and scrolls to it,
 * to the middle of the editor if it's off screen.
 */
export function selectRange(
  editor: EditorController,
  start: Position,
  end = start,
  reveal: Reveal = 'centerIfOutside',
): void {
  editor.dispatch({ type: 'setCursor', position: start, extend: false, add: false }, reveal)
  if (end.line !== start.line || end.column !== start.column) {
    editor.dispatch({ type: 'setCursor', position: end, extend: true, add: false }, reveal)
  }
  editor.focus()
}
