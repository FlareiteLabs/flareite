import { useOverlay } from '../state/overlays'
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { fuzzyFilter, fuzzyMatch, type FuzzyMatch } from '../commands/fuzzy'
import { commandLabel, executeCommand, primaryKeybinding, useCommands } from '../commands/registry'
import {
  activeEditor,
  selectRange,
  type EditorController,
  type LinePreview,
} from '../editor/activeEditor'
import { describePath } from '../editor/references'
import { SymbolKindIcon, symbolKindGroup, symbolKindName } from '../editor/symbolKinds'
import {
  documentSymbols,
  resolveSymbol,
  workspaceSymbols,
  type DocumentSymbol,
  type SymbolLocation,
  type WorkspaceSymbol,
} from '../ipc/lsp'
import { findFiles, type FileResult } from '../ipc/workspace'
import { openPath } from '../state/documents'
import { iconThemeStore } from '../state/iconTheme'
import { useStore } from '../state/store'
import { workspaceStore } from '../state/workspace'
import { themeKind } from '../themes/theme'
import { entryIcon, type EntryIcon } from './fileIcons'
import { activeElement, isHTMLElement } from '../app/windows'
import { FileIcon } from './FileIcon'
import { closePalette, paletteStore, type QuickPick } from './paletteState'
import './Palette.css'

interface Item {
  key: string
  label: ReactNode
  detail?: ReactNode
  hint?: string | null
  /** Shown before the label (files). */
  icon?: EntryIcon
  /** A symbol's kind, shown before the label. */
  symbol?: string
  /** Struck through, as deprecated symbols are. */
  deprecated?: boolean
  /** Heads a group of items: shown at the end of the item, with a rule above. */
  separator?: string
  /** Shown in the editor while the item is selected. */
  preview?: LinePreview
  /** Runs when chosen; an item without it is a message. */
  run?: () => void
}

interface Items {
  items: Item[]
  /** Shown when there are no items; `null` while they are being found. */
  empty: string | null
}

const MAX_ITEMS = 100
const MAX_SYMBOL_ITEMS = 2_000
/** How long typing pauses before workspace symbols are searched, as in VS Code. */
const SYMBOL_SEARCH_DELAY = 200
const PLACEHOLDER =
  'Search files by name (> for commands, : for a line, @ for a symbol, # for a symbol in the workspace)'

/**
 * The command palette. With a `>` prefix it lists commands, with `:` it goes
 * to a line, with `@` to a symbol in the active editor, with `#` to a symbol
 * anywhere in the workspace, and otherwise it finds workspace files by fuzzy
 * path. It also shows quick picks: lists of choices, such as encodings.
 *
 * Going to a line or to a symbol in the editor previews it there, as in VS
 * Code: the selected item's lines are highlighted and scrolled to, and
 * closing without choosing scrolls back.
 */
export function Palette() {
  const { open, initial, session, pick } = useStore(paletteStore)
  if (!open) {
    return null
  }
  return <PaletteDialog key={session} initial={initial} pick={pick} />
}

function PaletteDialog({ initial, pick }: { initial: string; pick: QuickPick | null }) {
  useOverlay()
  const [value, setValue] = useState(initial)
  const [selected, setSelected] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLUListElement>(null)
  const returnFocus = useRef<Element | null>(activeElement())
  const [editor] = useState(() => activeEditor.get())
  const { items, empty } = usePaletteItems(value, pick, editor)

  useEffect(() => {
    inputRef.current?.focus()
  }, [])

  // Previews in the editor, undone when the palette closes without choosing.
  const previewing = useRef(false)
  /** Set once an item is chosen, after which nothing is previewed. */
  const chosen = useRef(false)
  const preview = items[selected]?.preview
  const [start, end, center] = preview ? [preview.start, preview.end, preview.center] : []
  useEffect(() => {
    if (!editor || chosen.current) {
      return
    }
    if (start !== undefined && end !== undefined && center !== undefined) {
      editor.previewLines({ start, end, center })
      previewing.current = true
    } else if (previewing.current) {
      editor.previewLines(null)
    }
  }, [editor, start, end, center])
  useEffect(() => {
    if (!editor) {
      return
    }
    const scroll = editor.scrollPosition()
    return () => {
      if (previewing.current) {
        editor.previewLines(null)
        editor.restoreScroll(scroll)
      }
    }
  }, [editor])

  useEffect(() => setSelected(0), [value])

  useEffect(() => {
    listRef.current?.children[selected]?.scrollIntoView({ block: 'nearest' })
  }, [selected])

  const dismiss = (restoreFocus: boolean) => {
    closePalette()
    if (!chosen.current) {
      chosen.current = true
      pick?.onCancel?.()
    }
    if (restoreFocus && isHTMLElement(returnFocus.current)) {
      // Not scrolled to: an editor's input sits at its cursor, which choosing may move.
      returnFocus.current.focus({ preventScroll: true })
    }
  }

  const choose = (item: Item | undefined) => {
    if (!item?.run) {
      return
    }
    chosen.current = true
    if (item.preview) {
      // Choosing what's previewed keeps the editor where the preview put it.
      previewing.current = false
      editor?.previewLines(null)
    }
    dismiss(true)
    item.run()
  }

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault()
        setSelected(index => Math.min(items.length - 1, index + 1))
        break
      case 'ArrowUp':
        event.preventDefault()
        setSelected(index => Math.max(0, index - 1))
        break
      case 'Enter':
        event.preventDefault()
        if (pick?.input) {
          const accept = pick.input.accept
          chosen.current = true
          dismiss(true)
          accept(value)
        } else {
          choose(items[selected])
        }
        break
      case 'Escape':
        event.preventDefault()
        dismiss(true)
        break
    }
  }

  const activeId = items[selected] ? `palette-item-${selected}` : undefined
  return (
    <div className="palette-backdrop" onMouseDown={() => dismiss(true)}>
      <div
        className="palette"
        role="dialog"
        aria-label={pick ? pick.placeholder : 'Command palette'}
        onMouseDown={event => event.stopPropagation()}
      >
        <input
          ref={inputRef}
          className="palette__input"
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-list"
          aria-activedescendant={activeId}
          aria-label={pick ? pick.placeholder : PLACEHOLDER}
          placeholder={pick ? pick.placeholder : PLACEHOLDER}
          value={value}
          type={pick?.input?.password ? 'password' : 'text'}
          autoComplete="off"
          spellCheck={false}
          onChange={event => setValue(event.target.value)}
          onKeyDown={onKeyDown}
          onBlur={() => dismiss(false)}
        />
        <ul className="palette__list" id="palette-list" role="listbox" ref={listRef}>
          {items.map((item, index) => (
            <li
              key={item.key}
              id={`palette-item-${index}`}
              className="palette__item"
              role="option"
              aria-selected={index === selected}
              data-separator={item.separator !== undefined || undefined}
              onMouseMove={() => setSelected(index)}
              onMouseDown={event => {
                event.preventDefault()
                choose(item)
              }}
            >
              {item.icon && <FileIcon icon={item.icon} kind="file" className="palette__icon" />}
              {item.symbol && <SymbolKindIcon kind={item.symbol} />}
              <span className="palette__label" data-deprecated={item.deprecated || undefined}>
                {item.label}
              </span>
              {item.detail && <span className="palette__detail">{item.detail}</span>}
              {item.hint && <kbd className="palette__hint">{item.hint}</kbd>}
              {item.separator && <span className="palette__separator">{item.separator}</span>}
            </li>
          ))}
          {items.length === 0 && empty !== null && <li className="palette__empty">{empty}</li>}
        </ul>
      </div>
    </div>
  )
}

function usePaletteItems(
  value: string,
  pick: QuickPick | null,
  editor: EditorController | null,
): Items {
  const mode = pick ? null : value[0]
  const commands = useCommands()
  const files = useFileResults(pick || /^[>:@#]/.test(value) ? null : value)
  const symbols = useDocumentSymbols(mode === '@' ? editor : null)
  const workspaceQuery = mode === '#' ? splitQuery(value.slice(1)).name : null
  const found = useWorkspaceSymbols(workspaceQuery)
  const { info } = useStore(workspaceStore)
  const iconTheme = useStore(iconThemeStore)
  const light = useStore(themeKind) === 'light'

  return useMemo((): Items => {
    if (pick?.input) {
      return {
        items: [{ key: 'input', label: 'Press Enter to confirm, or Escape to cancel.' }],
        empty: null,
      }
    }
    if (pick) {
      return {
        items: fuzzyFilter(pick.items, value, item => item.label).map(({ item, match }, index) => ({
          key: `${index}:${item.label}`,
          label: highlight(item.label, match.indices),
          detail: item.detail,
          run: item.run,
        })),
        empty: 'No matches',
      }
    }
    if (mode === '>') {
      const visible = commands.filter(
        command => !command.hidden && (!command.enabled || command.enabled()),
      )
      return {
        items: fuzzyFilter(visible, value.slice(1), commandLabel)
          .slice(0, MAX_ITEMS)
          .map(({ item: command, match }) => ({
            key: command.id,
            label: highlight(commandLabel(command), match.indices),
            hint: primaryKeybinding(command),
            run: () => void executeCommand(command.id),
          })),
        empty: 'No matching commands',
      }
    }
    if (mode === ':') {
      return goToLineItems(value.slice(1), editor)
    }
    if (mode === '@') {
      if (!editor) {
        return {
          items: [],
          empty: 'To go to a symbol, first open a text editor with symbol information.',
        }
      }
      if (symbols === 'loading') {
        return { items: [], empty: null }
      }
      if (symbols === null) {
        return {
          items: [],
          empty: 'The active text editor does not provide symbol information.',
        }
      }
      const filter = value.slice(1)
      return {
        items: documentSymbolItems(symbols, filter, editor),
        empty: filter.trim() ? 'No matching editor symbols' : 'No editor symbols',
      }
    }
    if (mode === '#') {
      return {
        items: workspaceSymbolItems(found.symbols, value.slice(1), info?.root ?? null),
        empty: found.searching ? null : 'No matching workspace symbols',
      }
    }
    return {
      items: files.map(file => {
        const slash = file.relative.lastIndexOf('/')
        const name = file.relative.slice(slash + 1)
        return {
          key: file.path,
          icon: entryIcon(name, null, iconTheme, light),
          label: highlight(name, shift(file.indices, slash + 1)),
          detail: slash > 0 ? highlight(file.relative.slice(0, slash), file.indices) : null,
          run: () => void openPath(file.path),
        }
      }),
      empty: info ? 'No matching files' : 'Open a folder to search its files',
    }
  }, [value, mode, pick, editor, commands, files, symbols, found, info, iconTheme, light])
}

/**
 * Go to Line, as in VS Code: `42` goes to line 42, `42:5` (or `42,5`, `42#5`)
 * to its fifth character, and `-1` to the last line. Without a line in
 * range, the one item says where the cursor is.
 */
function goToLineItems(text: string, editor: EditorController | null): Items {
  if (!editor) {
    return { items: [], empty: 'Open a text editor first to go to a line.' }
  }
  const lineCount = editor.lineCount()
  const [first, column] = text
    .split(/[,:#]/)
    .map(part => parseInt(part, 10))
    .filter(number => !Number.isNaN(number))
  const line = first !== undefined && first < 0 ? lineCount + first + 1 : first
  if (line === undefined || line < 1 || line > lineCount) {
    const cursor = editor.cursor()
    return {
      items: [
        {
          key: 'current',
          label: `${cursor ? `Current Line: ${cursor.line + 1}, Character: ${cursor.column + 1}. ` : ''}Type a line number between 1 and ${lineCount} to navigate to.`,
        },
      ],
      empty: null,
    }
  }
  const position = { line: line - 1, column: column !== undefined && column > 0 ? column - 1 : 0 }
  return {
    items: [
      {
        key: 'line',
        label:
          column !== undefined && column > 0
            ? `Go to line ${line} and character ${column}.`
            : `Go to line ${line}.`,
        preview: { start: position.line, end: position.line, center: position.line },
        run: () => selectRange(editor, position, position, 'center'),
      },
    ],
    empty: null,
  }
}

/**
 * A symbol query's parts, as in VS Code: the first word matches names and
 * the rest the names of the symbols they're in.
 */
function splitQuery(filter: string): { name: string; container: string } {
  const [name = '', ...container] = filter.trim().split(/\s+/)
  return { name, container: container.join('') }
}

/** The active editor's symbols: `'loading'`, or `null` when its server can't list them. */
function useDocumentSymbols(editor: EditorController | null): DocumentSymbol[] | 'loading' | null {
  const documentId = editor?.documentId ?? null
  const [result, setResult] = useState<{
    documentId: number
    symbols: DocumentSymbol[] | null
  } | null>(null)
  useEffect(() => {
    if (documentId === null) {
      return
    }
    let stale = false
    documentSymbols(documentId)
      .then(symbols => !stale && setResult({ documentId, symbols }))
      .catch(() => !stale && setResult({ documentId, symbols: [] }))
    return () => {
      stale = true
    }
  }, [documentId])
  return result?.documentId === documentId ? result.symbols : 'loading'
}

/**
 * Go to Symbol in Editor, as in VS Code: symbols in order of position, or
 * best match first, under a "symbols (N)" heading; after `:`, grouped by
 * kind under headings such as "functions (3)".
 */
function documentSymbolItems(
  symbols: readonly DocumentSymbol[],
  filter: string,
  editor: EditorController,
): Item[] {
  const byKind = filter.startsWith(':')
  const query = splitQuery(byKind ? filter.slice(1) : filter)
  const matched: {
    symbol: DocumentSymbol
    order: number
    name: FuzzyMatch
    container?: FuzzyMatch
  }[] = []
  symbols.forEach((symbol, order) => {
    const name = fuzzyMatch(query.name, symbol.name)
    const container =
      query.container && symbol.container ? fuzzyMatch(query.container, symbol.container) : null
    if (name && (!query.container || container)) {
      matched.push({ symbol, order, name, container: container ?? undefined })
    }
  })
  matched.sort(
    (a, b) =>
      (byKind ? symbolKindName(a.symbol.kind).localeCompare(symbolKindName(b.symbol.kind)) : 0) ||
      (query.name ? b.name.score - a.name.score : 0) ||
      a.order - b.order,
  )
  const counts = new Map<string, number>()
  for (const { symbol } of matched) {
    counts.set(symbol.kind, (counts.get(symbol.kind) ?? 0) + 1)
  }
  return matched.slice(0, MAX_SYMBOL_ITEMS).map(({ symbol, order, name, container }, index) => {
    const previous = matched[index - 1]?.symbol
    let separator: string | undefined
    if (byKind) {
      if (previous?.kind !== symbol.kind) {
        separator = symbolKindGroup(symbol.kind, counts.get(symbol.kind)!)
      }
    } else if (index === 0) {
      separator = `symbols (${matched.length})`
    }
    return {
      key: String(order),
      symbol: symbol.kind,
      label: highlight(symbol.name, name.indices),
      detail: symbol.container && highlight(symbol.container, container?.indices ?? []),
      deprecated: symbol.deprecated,
      separator,
      preview: { start: symbol.start.line, end: symbol.end.line, center: symbol.selection.line },
      run: () => selectRange(editor, symbol.selection, symbol.selection, 'center'),
    }
  })
}

/** Workspace symbols for `query`, searched as typing pauses; earlier results until they arrive. */
function useWorkspaceSymbols(query: string | null): {
  symbols: WorkspaceSymbol[]
  searching: boolean
} {
  const [result, setResult] = useState<{ query: string; symbols: WorkspaceSymbol[] } | null>(null)
  useEffect(() => {
    if (query === null) {
      return
    }
    let stale = false
    const timer = setTimeout(() => {
      workspaceSymbols(query)
        .then(symbols => !stale && setResult({ query, symbols }))
        .catch(() => !stale && setResult({ query, symbols: [] }))
    }, SYMBOL_SEARCH_DELAY)
    return () => {
      stale = true
      clearTimeout(timer)
    }
  }, [query])
  return useMemo(
    () => ({
      symbols: query === null ? [] : (result?.symbols ?? []),
      searching: query !== null && result?.query !== query,
    }),
    [query, result],
  )
}

/**
 * Go to Symbol in Workspace, as in VS Code: servers find symbols by the
 * query's first word; they're filtered by it (or by the whole query) and
 * by the rest against "container • path", best match first.
 */
function workspaceSymbolItems(
  symbols: readonly WorkspaceSymbol[],
  filter: string,
  root: string | null,
): Item[] {
  const query = splitQuery(filter)
  const whole = filter.trim()
  const matched: {
    symbol: WorkspaceSymbol
    where: string
    name: FuzzyMatch
    container?: FuzzyMatch
  }[] = []
  for (const symbol of symbols) {
    const { name: file, folder } = describePath(symbol.location.path, root)
    const path = folder ? `${folder}/${file}` : file
    const where = symbol.container ? `${symbol.container} • ${path}` : path
    // A name can match the whole query ("change log"), and then it's all used.
    const exact = query.container ? fuzzyMatch(whole, symbol.name) : null
    const name = exact ?? fuzzyMatch(query.name, symbol.name)
    const container = !exact && query.container ? fuzzyMatch(query.container, where) : null
    if (name && (exact || !query.container || container)) {
      matched.push({ symbol, where, name, container: container ?? undefined })
    }
  }
  matched.sort(
    (a, b) =>
      b.name.score - a.name.score ||
      a.symbol.name.localeCompare(b.symbol.name) ||
      a.where.localeCompare(b.where),
  )
  return matched.slice(0, MAX_SYMBOL_ITEMS).map(({ symbol, where, name, container }, index) => ({
    key: `${index}:${symbol.location.path}:${symbol.name}`,
    symbol: symbol.kind,
    label: highlight(symbol.name, name.indices),
    detail: highlight(where, container?.indices ?? []),
    deprecated: symbol.deprecated,
    run: () => void openSymbol(symbol.location),
  }))
}

async function openSymbol(location: SymbolLocation): Promise<void> {
  const resolved = await resolveSymbol(location).catch(() => null)
  await openPath(location.path, resolved ? { start: resolved.start } : undefined)
}

/** Workspace files matching `query`, fetched from the backend as it changes. */
function useFileResults(query: string | null): FileResult[] {
  const [results, setResults] = useState<FileResult[]>([])
  const { info } = useStore(workspaceStore)
  useEffect(() => {
    if (query === null || !info) {
      setResults([])
      return
    }
    let stale = false
    const timer = setTimeout(() => {
      findFiles(query)
        .then(found => !stale && setResults(found.files))
        .catch(() => !stale && setResults([]))
    }, 30)
    return () => {
      stale = true
      clearTimeout(timer)
    }
  }, [query, info])
  return results
}

function shift(indices: readonly number[], by: number): number[] {
  return indices.map(index => index - by).filter(index => index >= 0)
}

/** `text` with the characters at `indices` emphasized. */
function highlight(text: string, indices: readonly number[]): ReactNode {
  if (indices.length === 0) {
    return text
  }
  const marked = new Set(indices)
  const parts: ReactNode[] = []
  let run = ''
  let runMarked = false
  const flush = (key: number) => {
    if (run) {
      parts.push(runMarked ? <mark key={key}>{run}</mark> : run)
    }
    run = ''
  }
  for (let index = 0; index < text.length; index++) {
    const isMarked = marked.has(index)
    if (isMarked !== runMarked) {
      flush(index)
      runMarked = isMarked
    }
    run += text[index]
  }
  flush(text.length)
  return parts
}
