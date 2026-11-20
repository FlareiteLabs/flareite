/**
 * Auto save (`files.auto_save`, ADR 0054): documents saved without being
 * asked, once they have a file (new, untitled ones are left alone).
 *
 * - `after_delay`: a while (`files.auto_save_delay`) after the last change.
 *   It saves as is: formatting and tidying on save would change the line
 *   being typed, so they wait for a save that asks for them.
 * - `on_focus_change`: when an editor loses focus (to another editor, a
 *   view, or another window).
 * - `on_window_change`: when a window loses focus.
 */
import { onEveryWindow } from '../app/windows'
import { appearance } from './appearance'
import { documentsStore, documentVersions, save } from './documents'

const timers = new Map<number, ReturnType<typeof setTimeout>>()

/** Whether `id` has unsaved changes and a file to save them in. */
function savable(id: number): boolean {
  const document = documentsStore.get().documents.find(candidate => candidate.id === id)
  return document !== undefined && document.dirty && document.path !== null
}

/** Saves `id` if it's savable; `asIs` skips formatting and tidying. */
function saveIfNeeded(id: number, asIs: boolean): void {
  if (savable(id)) {
    void save(id, false, !asIs)
  }
}

function saveAllNeeded(): void {
  for (const document of documentsStore.get().documents) {
    saveIfNeeded(document.id, false)
  }
}

/** The document an editor's input belongs to, if `target` is one. */
function editorDocument(target: EventTarget | null): number | null {
  const element = target as HTMLElement | null
  const id = element?.dataset?.document
  return element?.matches?.('textarea[data-editor-input]') && id !== undefined ? Number(id) : null
}

/** Starts saving documents as `files.auto_save` says; returns a function that stops. */
export function startAutoSave(): () => void {
  let versions = documentVersions.get()
  const stopVersions = documentVersions.subscribe(() => {
    const next = documentVersions.get()
    const { autoSave, autoSaveDelay } = appearance.get()
    for (const [key, version] of Object.entries(next)) {
      const id = Number(key)
      if (versions[id] === version || autoSave !== 'after_delay') {
        continue
      }
      clearTimeout(timers.get(id))
      timers.set(
        id,
        setTimeout(() => {
          timers.delete(id)
          if (appearance.get().autoSave === 'after_delay') {
            saveIfNeeded(id, true)
          }
        }, autoSaveDelay),
      )
    }
    versions = next
  })

  const stopWindows = onEveryWindow(view => {
    const onFocusOut = (event: FocusEvent) => {
      const id = editorDocument(event.target)
      // Focus moving between two editors of the same document isn't leaving it.
      if (
        id === null ||
        appearance.get().autoSave !== 'on_focus_change' ||
        editorDocument(event.relatedTarget) === id
      ) {
        return
      }
      saveIfNeeded(id, false)
    }
    const onBlur = () => {
      const mode = appearance.get().autoSave
      if (mode === 'on_window_change' || mode === 'on_focus_change') {
        saveAllNeeded()
      }
    }
    view.addEventListener('focusout', onFocusOut, true)
    view.addEventListener('blur', onBlur)
    return () => {
      view.removeEventListener('focusout', onFocusOut, true)
      view.removeEventListener('blur', onBlur)
    }
  })

  return () => {
    stopVersions()
    stopWindows()
    for (const timer of timers.values()) {
      clearTimeout(timer)
    }
    timers.clear()
  }
}
