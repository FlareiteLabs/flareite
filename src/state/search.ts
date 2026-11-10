/**
 * Find in files. Starting a search supersedes the previous one; results
 * from superseded searches are dropped.
 */
import { afterFrame } from '../app/windows'
import type { SearchQuery } from '../ipc/editor'
import * as ipc from '../ipc/workspace'
import type { LineMatch, SearchEvent } from '../ipc/workspace'
import { reportError } from './notifications'
import { createStore } from './store'

export interface FileResults {
  path: string
  relative: string
  matches: LineMatch[]
}

export interface SearchState {
  query: SearchQuery
  running: boolean
  files: readonly FileResults[]
  summary: Extract<SearchEvent, { type: 'done' }> | null
  error: string | null
}

export const searchStore = createStore<SearchState>({
  query: { pattern: '', regex: false, caseSensitive: false, wholeWord: false },
  running: false,
  files: [],
  summary: null,
  error: null,
})

let generation = 0

export function setSearchQuery(change: Partial<SearchQuery>): void {
  searchStore.update(state => ({ ...state, query: { ...state.query, ...change } }))
}

export async function runSearch(): Promise<void> {
  const current = ++generation
  const { query } = searchStore.get()
  if (!query.pattern) {
    void ipc.cancelSearch().catch(() => {})
    searchStore.update(state => ({
      ...state,
      running: false,
      files: [],
      summary: null,
      error: null,
    }))
    return
  }
  searchStore.update(state => ({
    ...state,
    running: true,
    files: [],
    summary: null,
    error: null,
  }))

  // Results arrive one file at a time; batch them into one update per frame
  // (of the window the user is in: the search view may be in another).
  let pending: FileResults[] = []
  let scheduled = false
  const flush = () => {
    scheduled = false
    const batch = pending
    pending = []
    if (current === generation && batch.length > 0) {
      searchStore.update(state => ({ ...state, files: [...state.files, ...batch] }))
    }
  }
  try {
    await ipc.searchWorkspace(query, event => {
      if (current !== generation) {
        return
      }
      if (event.type === 'file') {
        pending.push({ path: event.path, relative: event.relative, matches: event.matches })
        if (!scheduled) {
          scheduled = true
          afterFrame(flush)
        }
      } else {
        flush()
        searchStore.update(state => ({ ...state, running: false, summary: event }))
      }
    })
  } catch (error) {
    if (current === generation) {
      const message = error instanceof Error ? error.message : String(error)
      searchStore.update(state => ({ ...state, running: false, error: message }))
      if (!(error instanceof Error && 'code' in error && error.code === 'invalidSearch')) {
        reportError('Searching', error)
      }
    }
  }
}
