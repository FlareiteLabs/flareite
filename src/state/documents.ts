/**
 * Open documents as the workbench sees them: summaries for tabs (the text
 * itself stays in the backend) and which document is active.
 */
import { editorFor, editorsFor, selectRange, setActiveDocument } from '../editor/activeEditor'
import { CommandError } from '../ipc'
import * as ipc from '../ipc/editor'
import type { DocumentSummary, Position } from '../ipc/editor'
import { reportError } from './notifications'
import { refreshSettingsProblem } from './problems'
import { refreshSettingsIfLoaded } from './settings'
import { createStore } from './store'
import { defaultViewerFor, showExistingFileTab, showFileTab } from './viewers'

export interface DocumentsState {
  documents: readonly DocumentSummary[]
  activeId: number | null
}

export const documentsStore = createStore<DocumentsState>({ documents: [], activeId: null })
documentsStore.subscribe(() => setActiveDocument(documentsStore.get().activeId))

/**
 * Hooks for work that must finish before a document is saved, such as edits
 * still in flight from its editor views. Registered by editor sessions.
 */
const beforeSave = new Map<number, Set<() => Promise<void>>>()

export function registerBeforeSave(id: number, flush: () => Promise<void>): () => void {
  const flushes = beforeSave.get(id) ?? new Set()
  beforeSave.set(id, flushes.add(flush))
  return () => {
    flushes.delete(flush)
    if (flushes.size === 0 && beforeSave.get(id) === flushes) {
      beforeSave.delete(id)
    }
  }
}

/** Waits for the edits in flight from every editor of a document. */
async function flush(id: number): Promise<void> {
  await Promise.all([...(beforeSave.get(id) ?? [])].map(flush => flush()))
}

/**
 * Documents just opened (rather than switched to): the dock shows each in
 * the group documents open in, in a tab of its own there if it's showing
 * elsewhere, as VS Code does.
 */
const openRequests = new Set<number>()

/** Whether `id` was just opened; the request is taken. */
export function takeOpenRequest(id: number): boolean {
  return openRequests.delete(id)
}

/**
 * Selections to apply when a document's editor view next mounts, for
 * documents opened at a location (search results, Go to File).
 */
const pendingSelections = new Map<number, { start: Position; end: Position }>()

export function pendingSelection(id: number): { start: Position; end: Position } | undefined {
  return pendingSelections.get(id)
}

/** Marks a pending selection as applied (if it is still the pending one). */
export function clearPendingSelection(
  id: number,
  applied: { start: Position; end: Position },
): void {
  if (pendingSelections.get(id) === applied) {
    pendingSelections.delete(id)
  }
}

/** Updates a document's summary (for example after a reload from disk). */
export function updateSummary(summary: DocumentSummary): void {
  if (documentsStore.get().documents.some(d => d.id === summary.id)) {
    upsert(summary, false)
  }
}

/**
 * Counts requests to make a document active (opening or choosing one),
 * so the dock shows its tab then, and not on every change to the list
 * (a document becoming unsaved while a diff or merge tab shows).
 */
let activations = 0
export const activationCount = (): number => activations

function upsert(summary: DocumentSummary, activate: boolean): void {
  if (activate) {
    activations++
  }
  documentsStore.update(({ documents, activeId }) => {
    const exists = documents.some(d => d.id === summary.id)
    return {
      documents: exists
        ? documents.map(d => (d.id === summary.id ? summary : d))
        : [...documents, summary],
      activeId: activate ? summary.id : activeId,
    }
  })
}

function activeId(): number | null {
  return documentsStore.get().activeId
}

/** Restores tabs for documents the backend already has open (e.g. after a reload). */
export async function loadDocuments(activePath: string | null = null): Promise<void> {
  try {
    const documents = await ipc.listDocuments()
    const active = documents.find(d => d.path === activePath) ?? documents.at(-1)
    activations++
    documentsStore.set({ documents, activeId: active?.id ?? null })
  } catch (error) {
    reportError('Loading open documents', error)
  }
}

export function activate(id: number): void {
  activations++
  documentsStore.update(state => ({ ...state, activeId: id }))
}

/**
 * Opens a file in the layout its kind of file opens in by default, if one
 * does (`layouts.ts` sets it, which imports this module). Returns whether it did.
 */
let layoutOpener: ((path: string) => boolean) | null = null

export function setLayoutOpener(open: (path: string) => boolean): void {
  layoutOpener = open
}

/**
 * Opens (or switches to) the file at `path`, optionally selecting a range
 * once its editor is showing. Resolves to the document, or `null` if it
 * didn't open as one: it showed in a viewer, or in a tab asking whether to
 * open it as text (a binary file), or it couldn't be opened (the error is
 * reported).
 *
 * A file already open as text shows there. Otherwise one already in a tab
 * of its own shows there, and a file an extension's viewer is the default
 * for opens in it. With `asText`, it opens as text even if it's binary.
 */
export async function openPath(
  path: string,
  selection?: { start: Position; end?: Position },
  { asText = false }: { asText?: boolean } = {},
): Promise<DocumentSummary | null> {
  const isOpen = documentsStore.get().documents.some(document => document.path === path)
  if (!isOpen && !asText) {
    if (showExistingFileTab(path)) {
      return null
    }
    const viewer = defaultViewerFor(path)
    if (viewer) {
      showFileTab({ viewer: viewer.fullId, path })
      return null
    }
    if (layoutOpener?.(path)) {
      return null
    }
  }
  try {
    const summary = await ipc.openDocument(path, asText)
    // The dock picks (or adds) the tab first, so the selection goes to its editor.
    openRequests.add(summary.id)
    upsert(summary, true)
    openRequests.delete(summary.id)
    const editor = editorFor(summary.id)
    if (selection) {
      const range = { start: selection.start, end: selection.end ?? selection.start }
      if (editor) {
        selectRange(editor, range.start, range.end)
      } else {
        pendingSelections.set(summary.id, range)
      }
    }
    return summary
  } catch (error) {
    if (error instanceof CommandError && error.code === 'binary') {
      showFileTab({ viewer: null, path })
    } else {
      reportError('Opening the file', error)
    }
    return null
  }
}

/**
 * Opens (or switches to) the read-only document an extension provides at
 * `uri`. Resolves to it, or `null` if it couldn't be opened (the error is
 * reported).
 */
export async function openProvided(uri: string): Promise<DocumentSummary | null> {
  try {
    const summary = await ipc.openProvidedDocument(uri)
    openRequests.add(summary.id)
    upsert(summary, true)
    openRequests.delete(summary.id)
    return summary
  } catch (error) {
    reportError('Opening the document', error)
    return null
  }
}

/** Creates an untitled document, optionally containing `text`. */
export async function newFile(text?: string): Promise<void> {
  try {
    upsert(await ipc.newDocument(text), true)
  } catch (error) {
    reportError('Creating a file', error)
  }
}

/** Makes an open document active and selects `start..end` in it. */
export function revealInDocument(id: number, start: Position, end: Position): void {
  const editor = editorFor(id)
  if (editor) {
    activate(id)
    selectRange(editor, start, end)
    return
  }
  if (documentsStore.get().documents.some(document => document.id === id)) {
    pendingSelections.set(id, { start, end })
    activate(id)
  }
}

export async function openFile(): Promise<void> {
  try {
    const path = await ipc.pickFile()
    if (path) {
      await openPath(path)
    }
  } catch (error) {
    reportError('Opening the file', error)
  }
}

/** Reads a document's file again in another encoding (it must have no unsaved changes). */
export async function reopenWithEncoding(id: number, encoding: string): Promise<void> {
  try {
    await flush(id)
    const summary = await ipc.reopenWithEncoding(id, encoding)
    upsert(summary, false)
    for (const editor of editorsFor(id)) {
      editor.refresh()
    }
  } catch (error) {
    reportError('Reopening the file', error)
  }
}

/** Saves a document in another encoding, which it keeps from then on. */
export async function saveWithEncoding(id: number, encoding: string): Promise<void> {
  try {
    await flush(id)
    const summary = await ipc.saveWithEncoding(id, encoding)
    if (summary) {
      upsert(summary, false)
    }
  } catch (error) {
    reportError('Saving', error)
  }
}

/**
 * Saves a document; resolves to whether it was saved. With `format: false`
 * it isn't formatted first, whatever `editor.format_on_save` says.
 */
export async function save(id = activeId(), as = false, format = true): Promise<boolean> {
  if (id === null) {
    return false
  }
  try {
    await flush(id)
    const summary = await (as ? ipc.saveDocumentAs(id) : ipc.saveDocument(id, format))
    if (summary) {
      // A document only a diff tab shows isn't listed; its editors show it's saved.
      updateSummary(summary)
      if (!documentsStore.get().documents.some(document => document.id === id)) {
        for (const editor of editorsFor(id)) {
          editor.refresh()
        }
      }
      return !summary.dirty
    }
    return false
  } catch (error) {
    reportError('Saving', error)
    if (error instanceof CommandError && error.code === 'invalidSettings') {
      // settings.toml was saved with a mistake; the settings editor shows it.
      refreshSettingsIfLoaded()
      void refreshSettingsProblem()
    }
    return false
  }
}

/** Saves every document with unsaved changes, one at a time (untitled ones ask for a path). */
export async function saveAll(): Promise<void> {
  for (const document of documentsStore.get().documents.filter(d => d.dirty)) {
    await save(document.id)
  }
}

/** Tests whether a document stays open when its last tab closes (a diff or merge tab shows it). */
const keepers: ((id: number) => boolean)[] = []
const keptOpen = (id: number) => keepers.some(test => test(id))

export function setKeptOpen(test: (id: number) => boolean): void {
  keepers.push(test)
}

export async function close(id = activeId()): Promise<void> {
  if (id === null) {
    return
  }
  try {
    await flush(id)
    if (keptOpen(id)) {
      await ipc.hideDocument(id)
    } else if (!(await ipc.closeDocument(id))) {
      return
    }
  } catch (error) {
    reportError('Closing the file', error)
    return
  }
  documentsStore.update(({ documents, activeId }) => {
    const index = documents.findIndex(d => d.id === id)
    const remaining = documents.filter(d => d.id !== id)
    const neighbour = remaining[Math.min(index, remaining.length - 1)]
    return {
      documents: remaining,
      activeId: activeId === id ? (neighbour?.id ?? null) : activeId,
    }
  })
}

/** Records a dirty-state change reported by the editor view. */
/**
 * The text version of each document an editor shows, as editors last saw
 * it: what follows edits made anywhere, such as file pages showing a file
 * edited in another tab.
 */
export const documentVersions = createStore<Readonly<Record<number, number>>>({})

/** An editor saw document `id` at `version`. */
export function noteVersion(id: number, version: number): void {
  if (documentVersions.get()[id] !== version) {
    documentVersions.update(versions => ({ ...versions, [id]: version }))
  }
}

export function setDirty(id: number, dirty: boolean): void {
  const current = documentsStore.get().documents.find(d => d.id === id)
  if (current && current.dirty !== dirty) {
    upsert({ ...current, dirty }, false)
  }
}
