import {
  memo,
  useCallback,
  useDeferredValue,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react'
import { ChevronDown, ChevronRight, Ellipsis, Replace, ReplaceAll, X } from 'lucide-react'
import type { SearchQuery } from '../ipc/editor'
import type { LineMatch } from '../ipc/workspace'
import { resizeObserverFor } from '../app/windows'
import { ModalDialog } from '../components/ModalDialog'
import { openPath } from '../state/documents'
import {
  dismissResult,
  replacer,
  replaceResults,
  runSearch,
  searchStore,
  setSearchPanel,
  setSearchQuery,
  setSearchScope,
  type FileResults,
} from '../state/search'
import { useStore } from '../state/store'
import { openFolder, workspaceStore } from '../state/workspace'
import './SearchPanel.css'

const SEARCH_DELAY_MS = 250
/** The height of a result's row, in pixels: rows are placed by it. */
const ROW_HEIGHT = 22
/** Rows rendered beyond those on screen, above and below. */
const OVERSCAN = 20

const toggles: { key: keyof Omit<SearchQuery, 'pattern'>; label: string; glyph: string }[] = [
  { key: 'caseSensitive', label: 'Match Case', glyph: 'Aa' },
  { key: 'wholeWord', label: 'Match Whole Word', glyph: 'ab' },
  { key: 'regex', label: 'Use Regular Expression', glyph: '.*' },
]

/** Find and replace in files across the open workspace (ADR 0053). */
export function SearchPanel() {
  const { info } = useStore(workspaceStore)
  const state = useStore(searchStore)
  const { query, scope, replacement, showReplace, showDetails, replaceFocus } = state
  const { running, replacing, files, summary, error } = state
  const replaceRef = useRef<HTMLInputElement>(null)
  /** Replace All, waiting for the user to confirm it. */
  const [confirming, setConfirming] = useState(false)
  /** Files whose results are folded away. */
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set())
  // Typing in the replace box stays quick: the results' previews follow
  // when there's time, and their regex is compiled once per replacement.
  const shownReplacement = useDeferredValue(showReplace ? replacement : null)
  const preview = useMemo(
    () => (shownReplacement === null ? null : replacer(query, shownReplacement)),
    [query, shownReplacement],
  )
  const rows = useMemo(() => resultRows(files, collapsed), [files, collapsed])
  const toggleFile = useCallback(
    (path: string) =>
      setCollapsed(current => {
        const next = new Set(current)
        if (!next.delete(path)) {
          next.add(path)
        }
        return next
      }),
    [],
  )

  // Search as the query and the files to look in settle.
  useEffect(() => {
    if (!info) {
      return
    }
    const timer = setTimeout(() => void runSearch(), SEARCH_DELAY_MS)
    return () => clearTimeout(timer)
  }, [query, scope, info])

  // Replace in Files focuses the replace box.
  useEffect(() => {
    if (replaceFocus > 0) {
      replaceRef.current?.focus()
      replaceRef.current?.select()
    }
  }, [replaceFocus])

  if (!info) {
    return (
      <div className="panel-empty">
        <p>Open a folder to search its files.</p>
        <button type="button" className="panel-button" onClick={() => void openFolder()}>
          Open Folder…
        </button>
      </div>
    )
  }

  const matchCount = files.reduce((sum, file) => sum + file.matches.length, 0)
  const canReplace = !replacing && !running && matchCount > 0
  const askReplaceAll = () => {
    if (canReplace) {
      setConfirming(true)
    }
  }

  return (
    <div className="search-panel">
      <div className="search-panel__boxes">
        <button
          type="button"
          className="search-panel__expand"
          aria-label="Toggle Replace"
          aria-expanded={showReplace}
          title="Toggle Replace"
          onClick={() => setSearchPanel({ showReplace: !showReplace })}
        >
          {showReplace ? (
            <ChevronDown size={14} aria-hidden="true" />
          ) : (
            <ChevronRight size={14} aria-hidden="true" />
          )}
        </button>
        <div className="search-panel__fields">
          <div className="search-panel__query" data-invalid={error ? true : undefined}>
            <input
              id="search-panel-query"
              className="search-panel__input"
              aria-label="Search in files"
              placeholder="Search"
              spellCheck={false}
              value={query.pattern}
              title={error ?? undefined}
              onChange={event => setSearchQuery({ pattern: event.target.value })}
              onKeyDown={event => {
                if (event.key === 'Enter') {
                  void runSearch()
                }
              }}
            />
            {toggles.map(toggle => (
              <button
                key={toggle.key}
                type="button"
                className="search-panel__option"
                aria-label={toggle.label}
                aria-pressed={query[toggle.key]}
                title={toggle.label}
                onClick={() => setSearchQuery({ [toggle.key]: !query[toggle.key] })}
              >
                {toggle.glyph}
              </button>
            ))}
          </div>
          {showReplace && (
            <div className="search-panel__replace">
              <div className="search-panel__query">
                <input
                  ref={replaceRef}
                  className="search-panel__input"
                  aria-label="Replace"
                  placeholder="Replace"
                  spellCheck={false}
                  value={replacement}
                  onChange={event => setSearchPanel({ replacement: event.target.value })}
                  onKeyDown={event => {
                    if (event.key === 'Enter' && event.ctrlKey && event.altKey) {
                      event.preventDefault()
                      askReplaceAll()
                    }
                  }}
                />
              </div>
              <button
                type="button"
                className="search-panel__option search-panel__icon"
                aria-label="Replace All"
                title="Replace All (Ctrl+Alt+Enter)"
                disabled={!canReplace}
                onClick={askReplaceAll}
              >
                <ReplaceAll size={14} aria-hidden="true" />
              </button>
            </div>
          )}
        </div>
      </div>
      <div className="search-panel__details-toggle">
        <button
          type="button"
          className="search-panel__option search-panel__icon"
          aria-label="Toggle Search Details"
          aria-expanded={showDetails}
          title="Toggle Search Details"
          onClick={() => setSearchPanel({ showDetails: !showDetails })}
        >
          <Ellipsis size={14} aria-hidden="true" />
        </button>
      </div>
      {showDetails && (
        <div className="search-panel__details">
          <label className="search-panel__label" htmlFor="search-panel-include">
            files to include
          </label>
          <div className="search-panel__query">
            <input
              id="search-panel-include"
              className="search-panel__input"
              placeholder="e.g. src, *.ts, docs/**"
              spellCheck={false}
              value={scope.include}
              onChange={event => setSearchScope({ include: event.target.value })}
            />
          </div>
          <label className="search-panel__label" htmlFor="search-panel-exclude">
            files to exclude
          </label>
          <div className="search-panel__query">
            <input
              id="search-panel-exclude"
              className="search-panel__input"
              placeholder="e.g. tests, *.min.js"
              spellCheck={false}
              value={scope.exclude}
              onChange={event => setSearchScope({ exclude: event.target.value })}
            />
            <button
              type="button"
              className="search-panel__option"
              aria-label="Use Exclude Settings and Ignore Files"
              aria-pressed={scope.useIgnoreFiles}
              title="Use Exclude Settings and Ignore Files"
              onClick={() => setSearchScope({ useIgnoreFiles: !scope.useIgnoreFiles })}
            >
              <span aria-hidden="true">⊘</span>
            </button>
          </div>
        </div>
      )}
      <p className="search-panel__summary" role="status">
        {replacing ? 'Replacing…' : describe(query.pattern, running, files, summary, error)}
      </p>
      <ResultList
        rows={rows}
        collapsed={collapsed}
        preview={preview}
        replacing={showReplace}
        canReplace={canReplace}
        onToggle={toggleFile}
      />
      {confirming && (
        <ModalDialog
          title={`Replace ${plural(matchCount, 'match', 'matches')} across ${plural(
            files.length,
            'file',
            'files',
          )} with “${replacement}”?`}
          onClose={() => setConfirming(false)}
          buttons={
            <>
              <button type="button" className="dialog__button" onClick={() => setConfirming(false)}>
                Cancel
              </button>
              <button
                type="button"
                className="dialog__button dialog__button--primary"
                onClick={() => {
                  setConfirming(false)
                  void replaceResults()
                }}
              >
                Replace
              </button>
            </>
          }
        >
          <p>
            Open documents can be undone. Files that aren't open are changed on disk, which can't be
            undone.
          </p>
        </ModalDialog>
      )}
    </div>
  )
}

function plural(count: number, one: string, many: string): string {
  return `${count.toLocaleString()} ${count === 1 ? one : many}`
}

/** A row of the results: a file's header, or one of its matches. */
type Row =
  | { kind: 'file'; file: FileResults }
  | { kind: 'match'; file: FileResults; match: LineMatch; index: number }

/** The results as rows, leaving out the matches of collapsed files. */
function resultRows(files: readonly FileResults[], collapsed: ReadonlySet<string>): Row[] {
  const rows: Row[] = []
  for (const file of files) {
    rows.push({ kind: 'file', file })
    if (!collapsed.has(file.path)) {
      file.matches.forEach((match, index) => rows.push({ kind: 'match', file, match, index }))
    }
  }
  return rows
}

interface ResultListProps {
  rows: readonly Row[]
  collapsed: ReadonlySet<string>
  /** What a match becomes, while the replace box shows. */
  preview: ((matched: string) => string) | null
  /** Whether the replace box shows, so rows offer replacing. */
  replacing: boolean
  canReplace: boolean
  onToggle: (path: string) => void
}

/**
 * The results, which scroll on their own. Only the rows on screen (and a
 * few around them) are rendered, so tens of thousands of results cost no
 * more to show, or to preview a replacement in, than a screenful.
 */
function ResultList({
  rows,
  collapsed,
  preview,
  replacing,
  canReplace,
  onToggle,
}: ResultListProps) {
  const scrollRef = useRef<HTMLDivElement>(null)
  const [scroll, setScroll] = useState({ top: 0, height: 0 })
  useLayoutEffect(() => {
    const scroller = scrollRef.current
    if (!scroller) {
      return
    }
    const measure = () => setScroll({ top: scroller.scrollTop, height: scroller.clientHeight })
    measure()
    const observer = new (resizeObserverFor(scroller))(measure)
    observer.observe(scroller)
    return () => observer.disconnect()
  }, [])
  // Before it's laid out (or where there's no layout), a screenful.
  const height = scroll.height || 40 * ROW_HEIGHT
  const first = Math.max(0, Math.floor(scroll.top / ROW_HEIGHT) - OVERSCAN)
  const last = Math.min(rows.length, Math.ceil((scroll.top + height) / ROW_HEIGHT) + OVERSCAN)
  return (
    <div
      className="search-results"
      ref={scrollRef}
      onScroll={event =>
        setScroll({ top: event.currentTarget.scrollTop, height: event.currentTarget.clientHeight })
      }
    >
      <ul
        className="search-results__rows"
        aria-label="Search results"
        style={{ height: rows.length * ROW_HEIGHT }}
      >
        {rows.slice(first, last).map((row, offset) => {
          const style = { top: (first + offset) * ROW_HEIGHT, height: ROW_HEIGHT }
          return row.kind === 'file' ? (
            <FileRow
              key={row.file.path}
              file={row.file}
              open={!collapsed.has(row.file.path)}
              style={style}
              replacing={replacing}
              canReplace={canReplace}
              onToggle={onToggle}
            />
          ) : (
            <MatchRow
              key={`${row.file.path}:${row.match.line}:${row.match.startColumn}`}
              path={row.file.path}
              match={row.match}
              index={row.index}
              style={style}
              preview={preview}
              replacing={replacing}
              canReplace={canReplace}
            />
          )
        })}
      </ul>
    </div>
  )
}

interface RowStyle {
  top: number
  height: number
}

const FileRow = memo(function FileRow({
  file,
  open,
  style,
  replacing,
  canReplace,
  onToggle,
}: {
  file: FileResults
  open: boolean
  style: RowStyle
  replacing: boolean
  canReplace: boolean
  onToggle: (path: string) => void
}) {
  const slash = file.relative.lastIndexOf('/')
  return (
    <li className="search-results__row search-results__file" style={style}>
      <button
        type="button"
        className="search-results__file-header"
        aria-expanded={open}
        title={file.path}
        onClick={() => onToggle(file.path)}
      >
        <span className="search-results__chevron" aria-hidden="true">
          {open ? '▾' : '▸'}
        </span>
        <span className="search-results__name">{file.relative.slice(slash + 1)}</span>
        {slash > 0 && <span className="search-results__dir">{file.relative.slice(0, slash)}</span>}
        <span className="search-results__count">{file.matches.length}</span>
      </button>
      <RowActions
        replaceLabel={`Replace All in ${file.relative}`}
        dismissLabel={`Dismiss ${file.relative}`}
        replacing={replacing}
        canReplace={canReplace}
        onReplace={() => void replaceResults(file.path)}
        onDismiss={() => dismissResult(file.path)}
        all
      />
    </li>
  )
}, sameStyle)

const MatchRow = memo(function MatchRow({
  path,
  index,
  match,
  style,
  preview,
  replacing,
  canReplace,
}: {
  path: string
  index: number
  match: LineMatch
  style: RowStyle
  preview: ((matched: string) => string) | null
  replacing: boolean
  canReplace: boolean
}) {
  const units = match.preview
  const before = units.slice(0, match.previewStart).trimStart()
  const matched = units.slice(match.previewStart, match.previewEnd)
  return (
    <li className="search-results__row" style={style}>
      <button
        type="button"
        className="search-results__match"
        title={`Line ${match.line + 1}`}
        onClick={() =>
          void openPath(path, {
            start: { line: match.line, column: match.startColumn },
            end: { line: match.line, column: match.endColumn },
          })
        }
      >
        {before}
        {preview === null ? (
          <mark>{matched}</mark>
        ) : (
          <>
            <del>{matched}</del>
            <ins>{preview(matched)}</ins>
          </>
        )}
        {units.slice(match.previewEnd)}
      </button>
      <RowActions
        replaceLabel={`Replace match on line ${match.line + 1}`}
        dismissLabel={`Dismiss match on line ${match.line + 1}`}
        replacing={replacing}
        canReplace={canReplace}
        onReplace={() => void replaceResults(path, index)}
        onDismiss={() => dismissResult(path, index)}
      />
    </li>
  )
}, sameStyle)

/** Props equal but for `style`, which is equal if it places the row alike. */
function sameStyle<P extends { style: RowStyle }>(before: P, after: P): boolean {
  for (const key of Object.keys(after) as (keyof P)[]) {
    if (key === 'style') {
      if (before.style.top !== after.style.top || before.style.height !== after.style.height) {
        return false
      }
    } else if (before[key] !== after[key]) {
      return false
    }
  }
  return true
}

/** A result row's buttons, shown on hover or focus: replace it, or dismiss it. */
function RowActions({
  replaceLabel,
  dismissLabel,
  replacing,
  canReplace,
  onReplace,
  onDismiss,
  all = false,
}: {
  replaceLabel: string
  dismissLabel: string
  /** Whether the replace box shows, so replacing is offered. */
  replacing: boolean
  canReplace: boolean
  onReplace: () => void
  onDismiss: () => void
  all?: boolean
}) {
  const Icon = all ? ReplaceAll : Replace
  return (
    <span className="search-results__actions">
      {replacing && (
        <button
          type="button"
          className="search-results__action"
          aria-label={replaceLabel}
          title={replaceLabel}
          disabled={!canReplace}
          onClick={onReplace}
        >
          <Icon size={14} aria-hidden="true" />
        </button>
      )}
      <button
        type="button"
        className="search-results__action"
        aria-label={dismissLabel}
        title={dismissLabel}
        onClick={onDismiss}
      >
        <X size={14} aria-hidden="true" />
      </button>
    </span>
  )
}

function describe(
  pattern: string,
  running: boolean,
  files: readonly FileResults[],
  summary: {
    matches: number
    filesWithMatches: number
    truncated: boolean
    cancelled: boolean
  } | null,
  error: string | null,
): string {
  if (!pattern) {
    return ''
  }
  if (error) {
    return error
  }
  if (running) {
    const found = files.reduce((sum, file) => sum + file.matches.length, 0)
    return found > 0 ? `Searching… ${found} results so far` : 'Searching…'
  }
  if (!summary || summary.cancelled) {
    return ''
  }
  if (summary.matches === 0) {
    return 'No results.'
  }
  const results = `${summary.matches} result${summary.matches === 1 ? '' : 's'}`
  const inFiles = `in ${summary.filesWithMatches} file${summary.filesWithMatches === 1 ? '' : 's'}`
  return `${results} ${inFiles}${summary.truncated ? ' (search stopped at the result limit)' : ''}`
}
