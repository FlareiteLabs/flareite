/**
 * Find and replace in files. Starting a search supersedes the previous one;
 * results from superseded searches are dropped. Results can be dismissed,
 * which leaves them out of a replace, and replaced (ADR 0053): what's
 * replaced is what the results show, and the search runs again afterwards.
 */
import { afterFrame } from '../app/windows'
import type { SearchQuery } from '../ipc/editor'
import * as ipc from '../ipc/workspace'
import type { LineMatch, ReplaceFile, SearchEvent, SearchScope } from '../ipc/workspace'
import { notify, reportError } from './notifications'
import { createStore } from './store'

export interface FileResults {
  path: string
  relative: string
  matches: LineMatch[]
}

export interface SearchState {
  query: SearchQuery
  /** Which files to look in. */
  scope: SearchScope
  /** What replaces the matches; the replace box shows while `showReplace`. */
  replacement: string
  showReplace: boolean
  /** Whether the boxes for files to include and exclude show. */
  showDetails: boolean
  /** Bumped to focus the replace box. */
  replaceFocus: number
  running: boolean
  /** A replace is under way. */
  replacing: boolean
  files: readonly FileResults[]
  summary: Extract<SearchEvent, { type: 'done' }> | null
  error: string | null
}

export const searchStore = createStore<SearchState>({
  query: { pattern: '', regex: false, caseSensitive: false, wholeWord: false },
  scope: { include: '', exclude: '', useIgnoreFiles: true },
  replacement: '',
  showReplace: false,
  showDetails: false,
  replaceFocus: 0,
  running: false,
  replacing: false,
  files: [],
  summary: null,
  error: null,
})

let generation = 0

export function setSearchQuery(change: Partial<SearchQuery>): void {
  searchStore.update(state => ({ ...state, query: { ...state.query, ...change } }))
}

export function setSearchScope(change: Partial<SearchScope>): void {
  searchStore.update(state => ({ ...state, scope: { ...state.scope, ...change } }))
}

/** Changes what the panel shows: the replace box, the details, the replacement. */
export function setSearchPanel(
  change: Partial<Pick<SearchState, 'replacement' | 'showReplace' | 'showDetails'>>,
): void {
  searchStore.update(state => ({ ...state, ...change }))
}

/** Shows the replace box and focuses it: Replace in Files. */
export function openReplace(): void {
  searchStore.update(state => ({
    ...state,
    showReplace: true,
    replaceFocus: state.replaceFocus + 1,
  }))
}

/** Leaves a file's results out, or one of them (`index`). */
export function dismissResult(path: string, index?: number): void {
  searchStore.update(state => ({
    ...state,
    files: state.files.flatMap(file => {
      if (file.path !== path) {
        return [file]
      }
      if (index === undefined) {
        return []
      }
      const matches = file.matches.filter((_, at) => at !== index)
      return matches.length > 0 ? [{ ...file, matches }] : []
    }),
  }))
}

/** The results' matches, by file, for a replace. */
function toReplace(files: readonly FileResults[]): ReplaceFile[] {
  return files.map(file => ({
    path: file.path,
    matches: file.matches.map(match => [match.line, match.startColumn, match.endColumn]),
  }))
}

/**
 * Replaces results: all of them, a file's (`path`), or one match of it
 * (`index`). Then searches again.
 */
export async function replaceResults(path?: string, index?: number): Promise<void> {
  const { query, replacement, files, replacing } = searchStore.get()
  if (replacing || !query.pattern) {
    return
  }
  const chosen = files
    .filter(file => path === undefined || file.path === path)
    .map(file => (index === undefined ? file : { ...file, matches: [file.matches[index]!] }))
    .filter(file => file.matches.length > 0)
  if (chosen.length === 0) {
    return
  }
  searchStore.update(state => ({ ...state, replacing: true }))
  try {
    const done = await ipc.replaceInFiles(query, replacement, toReplace(chosen))
    const parts = [
      `Replaced ${plural(done.replaced, 'match', 'matches')} in ${plural(
        done.documents + done.files,
        'file',
        'files',
      )}.`,
    ]
    if (done.skipped > 0) {
      parts.push(
        `${plural(done.skipped, 'result had', 'results had')} changed since the search and ${
          done.skipped === 1 ? 'was' : 'were'
        } left alone.`,
      )
    }
    if (done.failures.length > 0) {
      parts.push(`Couldn't change ${done.failures.join('; ')}.`)
    }
    // A single match replaced says nothing, unless something went wrong.
    if (index === undefined || done.skipped > 0 || done.failures.length > 0) {
      notify(done.failures.length > 0 ? 'warning' : 'info', parts.join(' '))
    }
  } catch (error) {
    reportError('Replacing', error)
  } finally {
    searchStore.update(state => ({ ...state, replacing: false }))
  }
  await runSearch()
}

function plural(count: number, one: string, many: string): string {
  return `${count.toLocaleString()} ${count === 1 ? one : many}`
}

/**
 * What a match becomes, for the results' preview: the replacement, with a
 * regex's groups expanded as well as JavaScript's engine can (the backend's
 * is what counts), or else the replacement as typed.
 */
export function replacedText(matched: string, query: SearchQuery, replacement: string): string {
  if (!query.regex) {
    return replacement
  }
  try {
    const pattern = query.wholeWord ? `\\b(?:${query.pattern})\\b` : query.pattern
    const regex = new RegExp(`^(?:${pattern})$`, query.caseSensitive ? 'u' : 'iu')
    if (!regex.test(matched)) {
      return replacement
    }
    // `${name}` is `$<name>` in JavaScript; `$1` and `$$` are the same.
    return matched.replace(regex, replacement.replace(/\$\{(\w+)\}/g, '$$<$1>'))
  } catch {
    return replacement
  }
}

export async function runSearch(): Promise<void> {
  const current = ++generation
  const { query, scope } = searchStore.get()
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
    await ipc.searchWorkspace(query, scope, event => {
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
