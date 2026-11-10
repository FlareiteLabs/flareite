/**
 * The references peek view, as in VS Code: a symbol's references listed
 * inside the editor, below the line they were asked from, with a preview of
 * the selected one. Go to References goes straight to a lone reference and
 * peeks at several; Peek References always peeks. F4 and Shift+F4 move the
 * editor (and the peek) to the next and previous reference. One peek is open
 * at a time; the state lives here so it can follow a reference into another
 * file's editor.
 */
import type { Position } from '../ipc/editor'
import type { Reference } from '../ipc/lsp'
import { openPath } from '../state/documents'
import { createStore } from '../state/store'
import { activeElement, isHTMLElement } from '../app/windows'
import { isWithin } from '../state/workspace'
import { editorsFor, shownView } from './activeEditor'
import { isWordChar } from './completion'

/** A file's references, in order. */
export interface ReferenceGroup {
  path: string
  references: readonly Reference[]
}

/** A reference, by group and index within it. */
export interface ReferenceIndex {
  group: number
  index: number
}

export interface PeekState {
  /** The document whose editor shows the peek, and that editor's view of it. */
  documentId: number
  view: number
  /** Where the peek opens: below this line, its arrow pointing here. */
  anchor: Position
  /** The kind of results, for the header, e.g. "References". */
  title: string
  groups: readonly ReferenceGroup[]
  /** The reference shown in the preview. */
  selected: ReferenceIndex
  /** Files whose references are hidden in the list. */
  collapsed: ReadonlySet<string>
  /** Where focus goes when this state is shown. */
  focus: 'list' | 'preview' | 'editor'
  /** Bumped whenever focus should move again. */
  focusRequest: number
}

export const peekStore = createStore<PeekState | null>(null)

/** The peek's size: its height in lines, and the preview's share of its width. */
export interface PeekLayout {
  heightInLines: number
  ratio: number
}

/** Kept while the app runs, as the peek is resized. */
export const peekLayout = createStore<PeekLayout>({ heightInLines: 18, ratio: 0.7 })

/**
 * The peek's height in lines for an editor showing `visibleLines`: as
 * resized, but at most 80% of the editor (and no less than 12 lines), as in
 * VS Code.
 */
export function peekHeightInLines(visibleLines: number): number {
  return Math.min(peekLayout.get().heightInLines, Math.max(12, Math.floor(visibleLines * 0.8)))
}

/** Groups references (sorted by path) by file. */
export function groupReferences(references: readonly Reference[]): ReferenceGroup[] {
  const groups: { path: string; references: Reference[] }[] = []
  for (const reference of references) {
    const last = groups.at(-1)
    if (last?.path === reference.path) {
      last.references.push(reference)
    } else {
      groups.push({ path: reference.path, references: [reference] })
    }
  }
  return groups
}

/**
 * The reference nearest `position` in `path`: in the file whose path shares
 * the longest prefix with it, then the closest by line, then by column (as
 * VS Code picks the reference to start on).
 */
export function nearestReference(
  groups: readonly ReferenceGroup[],
  path: string,
  position: Position,
): ReferenceIndex {
  let best: ReferenceIndex = { group: 0, index: 0 }
  let bestPrefix = -1
  let bestDistance = Infinity
  groups.forEach((group, groupIndex) => {
    const prefix = commonPrefixLength(group.path, path)
    group.references.forEach((reference, index) => {
      const distance =
        Math.abs(reference.start.line - position.line) * 100 +
        Math.abs(reference.start.column - position.column)
      if (prefix > bestPrefix || (prefix === bestPrefix && distance < bestDistance)) {
        best = { group: groupIndex, index }
        bestPrefix = prefix
        bestDistance = distance
      }
    })
  })
  return best
}

function commonPrefixLength(a: string, b: string): number {
  let length = 0
  while (length < a.length && length < b.length && a[length] === b[length]) {
    length++
  }
  return length
}

/** The reference after (or before) `at`, wrapping around at either end. */
export function stepReference(
  groups: readonly ReferenceGroup[],
  at: ReferenceIndex,
  forward: boolean,
): ReferenceIndex {
  const flat = groups.flatMap((group, groupIndex) =>
    group.references.map((_, index) => ({ group: groupIndex, index })),
  )
  const current = flat.findIndex(item => item.group === at.group && item.index === at.index)
  const next = (current + (forward ? 1 : -1) + flat.length) % flat.length
  return flat[next] ?? at
}

export function referenceAt(state: PeekState, at: ReferenceIndex = state.selected): Reference {
  return state.groups[at.group]!.references[at.index]!
}

export function referenceCount(groups: readonly ReferenceGroup[]): number {
  return groups.reduce((sum, group) => sum + group.references.length, 0)
}

/** Whether the peek shows in the editor of `documentId` showing its view `view`. */
export function peekIsIn(state: PeekState | null, documentId: number, view: number): boolean {
  return state?.documentId === documentId && state.view === view
}

/**
 * Opens a peek in the editor of `documentId` showing its view `view`,
 * below `anchor`, starting on the reference nearest it.
 */
export function openPeek(
  documentId: number,
  view: number,
  path: string | null,
  anchor: Position,
  title: string,
  references: readonly Reference[],
): void {
  const groups = groupReferences(references)
  const selected = nearestReference(groups, path ?? '', anchor)
  peekStore.set({
    documentId,
    view,
    anchor,
    title,
    groups,
    selected,
    // With several files, only the one being looked at starts expanded.
    collapsed: new Set(
      groups.map(group => group.path).filter(other => other !== groups[selected.group]!.path),
    ),
    focus: 'list',
    focusRequest: (peekStore.get()?.focusRequest ?? 0) + 1,
  })
}

/** Closes the peek, returning focus to its editor unless `focusEditor` is false. */
export function closePeek(focusEditor = true): void {
  const state = peekStore.get()
  if (!state) {
    return
  }
  peekStore.set(null)
  if (focusEditor) {
    editorsFor(state.documentId)
      .find(editor => editor.view === state.view)
      ?.focus()
  }
}

/** The document whose editor the peek is leaving for another file's. */
let movingFrom: number | null = null

/** Closes the peek in the editor of a document's view, which is going away. */
export function closePeekIn(documentId: number, view: number): void {
  if (peekIsIn(peekStore.get(), documentId, view) && movingFrom !== documentId) {
    peekStore.set(null)
  }
}

/** Shows a reference in the preview, expanding its file in the list. */
export function selectReference(at: ReferenceIndex): void {
  peekStore.update(state => {
    if (!state) {
      return state
    }
    const path = state.groups[at.group]!.path
    const collapsed = new Set(state.collapsed)
    collapsed.delete(path)
    return { ...state, selected: at, collapsed }
  })
}

/** Shows or hides a file's references in the list. */
export function toggleGroup(path: string): void {
  peekStore.update(state => {
    if (!state) {
      return state
    }
    const collapsed = new Set(state.collapsed)
    if (!collapsed.delete(path)) {
      collapsed.add(path)
    }
    return { ...state, collapsed }
  })
}

/** Closes the peek and opens a reference with its range selected. */
export async function openReference(reference: Reference, at?: Position): Promise<void> {
  closePeek(false)
  await openPath(
    reference.path,
    at ? { start: at } : { start: reference.start, end: reference.end },
  )
}

/**
 * Moves to the next (or previous) reference: the editor's cursor goes to
 * it, in its file's editor, and the peek follows, keeping focus where it
 * was (VS Code's F4 and Shift+F4).
 */
export async function goToNextReference(forward: boolean): Promise<void> {
  const state = peekStore.get()
  if (!state) {
    return
  }
  const active = activeElement()
  const focus =
    isHTMLElement(active) && active.dataset.editorInput !== undefined
      ? 'editor'
      : active?.closest('.peek-preview')
        ? 'preview'
        : 'list'
  const selected = stepReference(state.groups, state.selected, forward)
  const reference = referenceAt(state, selected)
  // Its editor may unmount as another file's opens; the peek moves instead.
  movingFrom = state.documentId
  let opened
  try {
    opened = await openPath(reference.path, { start: reference.start })
  } finally {
    movingFrom = null
  }
  const current = peekStore.get()
  if (!opened) {
    // It showed in a viewer, or couldn't be opened.
    closePeek(false)
    return
  }
  if (current?.focusRequest !== state.focusRequest) {
    // Closed or moved meanwhile.
    return
  }
  const collapsed = new Set(current.collapsed)
  collapsed.delete(reference.path)
  peekStore.set({
    ...current,
    documentId: opened.id,
    // The view the dock shows the file in, as it opened.
    view: shownView(opened.id),
    anchor: reference.start,
    selected,
    collapsed,
    focus,
    focusRequest: current.focusRequest + 1,
  })
}

/**
 * A reference's line for the list: a few characters before it (from the
 * start of a word), it, and the rest of the line, trimmed (as VS Code
 * shows them).
 */
export function referencePreview(reference: Reference): {
  before: string
  inside: string
  after: string
} {
  const { line, start, end } = reference
  const to = end.line === start.line ? Math.min(end.column, line.length) : line.length
  let from = Math.max(0, start.column - 8)
  while (from > 0 && isWordChar(line[from - 1]!)) {
    from--
  }
  return {
    before: line.slice(from, start.column).trimStart(),
    inside: line.slice(start.column, to),
    after: line.slice(to).trimEnd(),
  }
}

/** The file name of `path`, and its folder relative to the workspace (or in full outside it). */
export function describePath(path: string, root: string | null): { name: string; folder: string } {
  const cut = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
  const name = path.slice(cut + 1)
  const parent = cut > 0 ? path.slice(0, cut) : ''
  if (root && isWithin(parent, root)) {
    return { name, folder: parent.slice(root.length + 1).replace(/\\/g, '/') }
  }
  return { name, folder: parent }
}
