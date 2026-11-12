import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent,
  type CompositionEvent,
  type FormEvent,
  type KeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from 'react'
import {
  autoInsert,
  quickDiffApply,
  quickDiffHunk,
  quickDiffRevert,
  type AutoInsert,
  type CodeLens,
  type DecorationView,
  type DisplayLine,
  type LineAnnotation,
  type EditorOp,
  type EditorView as View,
  type GutterChange,
  type Hunk,
  type Position,
  type SearchQuery,
  type Viewport,
} from '../ipc/editor'
import { ChangePeek, changePeekLines } from './ChangePeek'
import { CommandError } from '../ipc'
import * as lsp from '../ipc/lsp'
import { appearance } from '../state/appearance'
import {
  clearPendingSelection,
  documentsStore,
  openPath,
  pendingSelection,
  registerBeforeSave,
  setDirty,
  noteVersion,
} from '../state/documents'
import { editorStatus } from '../state/editorStatus'
import { layoutStore } from '../state/layout'
import { notify, reportError } from '../state/notifications'
import { notifyPainted, recordTiming } from '../state/perf'
import { useStore } from '../state/store'
import {
  activeEditor,
  editorsFor,
  registerEditor,
  savedScroll,
  saveScroll,
  type EditorController,
  type LinePreview,
  type Reveal,
} from './activeEditor'
import { isWordChar, visibleCompletions, wordAround } from './completion'
import { PeekView } from './PeekView'
import {
  closePeek,
  closePeekIn,
  openPeek,
  peekHeightInLines,
  peekIsIn,
  peekLayout,
  peekStore,
} from './references'
import { FindWidget, type FindState } from './FindWidget'
import { inputMirror, typedText, type InputMirror } from './inputMirror'
import { isMac, keyToCommand } from './keymap'
import { Minimap } from './Minimap'
import { MINIMAP_WIDTH } from './minimapGeometry'
import {
  COMPLETION_ROW_HEIGHT,
  COMPLETION_ROWS,
  COMPLETION_WIDTH,
  CompletionList,
  HoverCard,
  severityName,
  SignatureCard,
  type Placement,
} from './LanguagePopups'
import { RenameBox } from './RenameBox'
import {
  columnAt,
  columnX,
  EMPTY_OVERLAY,
  lineLength,
  measureOverlay,
  type Overlay,
  type Rect,
} from './measure'
import { ChevronDown, ChevronRight, Lightbulb } from 'lucide-react'
import { openContextMenu, type MenuEntry } from '../components/contextMenuState'
import { openPalette } from '../components/paletteState'
import { getCommand, primaryKeybinding } from '../commands/registry'
import {
  contributedMenuItems,
  decorationTypes,
  extensionsStore,
  runContributed,
} from '../state/extensions'
import { themeKind } from '../themes/theme'
import { runCodeLens } from '../ipc/extensions'
import { decorationRectStyle, lineBackgrounds, textOverlays } from './decorations'
import { displayRow, lineAtRow, rowOf } from './rows'
import { highlighted } from './highlighted'
import { LINK_CLASS, type TextOverlay } from './segments'
import { findCandidates, folderOf, resolvePath, type Candidate } from '../state/linkFinder'
import { workspaceStore } from '../state/workspace'
import { isNode, onEveryWindow, ownerWindow, resizeObserverFor } from '../app/windows'
import { openLink } from '../state/links'
import { EditorSession, ipcTransport, type EditorTransport } from './session'
import './EditorView.css'

/** Extra lines fetched above and below the visible area. */
const OVERSCAN = 20
/** A column beyond any line end; the backend clamps it to the line end. */
const END_OF_LINE = Number.MAX_SAFE_INTEGER
const EMPTY_QUERY: SearchQuery = {
  pattern: '',
  regex: false,
  caseSensitive: false,
  wholeWord: false,
}

/** Keys that only modify others. */
const MODIFIER_KEYS = new Set(['Control', 'Meta', 'Shift', 'Alt', 'AltGraph'])

/** How long the pointer rests on a symbol before its hover card shows. */
const HOVER_DELAY = 500
/** How far the pointer may drift along a line while a hover card stays. */
const HOVER_SLACK = 40

interface HoverState {
  hover: lsp.Hover
  line: number
  x: number
  placement: Placement
}

interface CompletionState {
  items: lsp.CompletionItem[]
  incomplete: boolean
  selected: number
}

/** Rename Symbol's box: what it renames, and where it was asked. */
interface RenameState {
  target: lsp.RenameTarget
  position: Position
}

/** Signature help, for the call at `position`. */
interface SignatureState {
  help: lsp.SignatureHelp
  /** The signature shown, of several. */
  selected: number
  position: Position
}

/** How long the cursor rests before the editor looks for code actions there. */
const LIGHTBULB_DELAY = 250
/** Height of the signature help card, for placing it. */
const SIGNATURE_HEIGHT = 80

/** A word under the pointer, as UTF-16 columns `[from, to)` of `line`. */
interface Word {
  line: number
  from: number
  to: number
}

/**
 * The symbol drawn as a link while the go-to-definition modifier is held,
 * or a web address in the text (`url`) or a path to a file or folder,
 * which Ctrl+click opens.
 */
interface LinkState extends Word {
  /** The document version it was found in; edits hide it. */
  version: number
  url?: string
}

/**
 * Space between rows, such as a diff's filler lines: below display row
 * `row` (-1 for above the first), `lines` lines tall.
 */
export interface ViewZone {
  key: string
  row: number
  lines: number
  className?: string
  content?: ReactNode
}

/** Zones in row order, with the height of those before each (`before`, one longer). */
interface Zones {
  list: readonly ViewZone[]
  /** `before[i]`: the height in pixels of the zones before the `i`th. */
  before: readonly number[]
  lineHeight: number
}

const NO_ZONES: readonly ViewZone[] = []
const NO_DECORATIONS: readonly DecorationView[] = []

/** A view's decorations, with those of `extra` on its lines. */
function withPaneDecorations(
  view: View,
  extra: readonly DecorationView[],
): readonly DecorationView[] {
  if (extra.length === 0) {
    return view.decorations
  }
  const shown = extra.filter(
    decoration => decoration.end.line >= view.startLine && decoration.start.line < view.endLine,
  )
  return shown.length === 0 ? view.decorations : [...view.decorations, ...shown]
}
const NO_LENSES: readonly CodeLens[] = []
const LENS_ZONE = 'editor-view__lens-zone'

/** A line's code lenses: links that run their extension's commands. */
function CodeLenses({ documentId, lenses }: { documentId: number; lenses: readonly CodeLens[] }) {
  return (
    <div className="editor-view__lenses" onMouseDown={event => event.stopPropagation()}>
      {lenses.map((lens, index) => (
        <span key={index}>
          {index > 0 && (
            <span className="editor-view__lens-separator" aria-hidden="true">
              |
            </span>
          )}
          <button
            type="button"
            className="editor-view__lens"
            title={lens.tooltip ?? undefined}
            onClick={() =>
              runCodeLens(documentId, lens.command, lens.argument).catch((error: unknown) =>
                reportError(lens.title, error),
              )
            }
          >
            {lens.title}
          </button>
        </span>
      ))}
    </div>
  )
}

function zonesOf(list: readonly ViewZone[], lineHeight: number): Zones {
  const sorted = list.length > 1 ? [...list].sort((a, b) => a.row - b.row) : list
  const before = [0]
  for (const zone of sorted) {
    before.push(before.at(-1)! + zone.lines * lineHeight)
  }
  return { list: sorted, before, lineHeight }
}

/** How many of the zones are below rows before `row`. */
function zonesBefore(zones: Zones, row: number): number {
  let low = 0
  let high = zones.list.length
  while (low < high) {
    const middle = (low + high) >> 1
    if (zones.list[middle]!.row < row) {
      low = middle + 1
    } else {
      high = middle
    }
  }
  return low
}

/** The height of the zones above display row `row`. */
function zonesAbove(zones: Zones, row: number): number {
  return zones.before[zonesBefore(zones, row)]!
}

/** The height of the zones right below display row `row`. */
function zonesBelowRow(zones: Zones, row: number): number {
  return zonesAbove(zones, row + 1) - zonesAbove(zones, row)
}

/** The top of display row `row`, below the zones above it. */
function rowTop(row: number, lineHeight: number, zones: Zones): number {
  return row * lineHeight + zonesAbove(zones, row)
}

/** The display row at `y` from the top of the rows; a zone counts as the row above it. */
function rowAtY(y: number, lineHeight: number, zones: Zones): number {
  // The zones that start at or above `y`.
  let low = 0
  let high = zones.list.length
  while (low < high) {
    const middle = (low + high) >> 1
    if ((zones.list[middle]!.row + 1) * lineHeight + zones.before[middle]! <= y) {
      low = middle + 1
    } else {
      high = middle
    }
  }
  if (low > 0) {
    const zone = zones.list[low - 1]!
    if (y < (zone.row + 1) * lineHeight + zones.before[low]!) {
      return zone.row
    }
  }
  return Math.floor((y - zones.before[low]!) / lineHeight)
}

/** Whether `event` holds the go-to-definition modifier (Ctrl, or ⌘ on macOS) alone. */
function definitionModifier(event: {
  ctrlKey: boolean
  metaKey: boolean
  shiftKey: boolean
  altKey: boolean
}) {
  return (isMac() ? event.metaKey : event.ctrlKey) && !event.shiftKey && !event.altKey
}

/** Characters drawn with a class of their own, such as a diff's changed words. */
export interface MarkedRange {
  start: Position
  end: Position
  className: string
}

interface EditorViewProps {
  documentId: number
  /** The document's view to show: each editor of a document has its own. */
  view?: number
  title: string
  transport?: EditorTransport
  /** Space between rows, besides a peek's. */
  zones?: readonly ViewZone[]
  /** A class for a line and its line number, such as a diff's inserted lines. */
  lineClass?: (line: number) => string | undefined
  marks?: readonly MarkedRange[]
  /**
   * Ranges decorated with extensions' decoration types besides the
   * document's own, in this editor only: a layout pane's (API 1.15).
   */
  decorations?: readonly DecorationView[]
  /** Whether the user can fold regions (the default). */
  folding?: boolean
  /** Whether the minimap shows, when it's turned on (the default). */
  minimap?: boolean
  /**
   * Whether lines changed from the file's original are marked in the gutter
   * (the default), each change opening inline when its marker is clicked.
   */
  quickDiff?: boolean
  /** Called with each view as it arrives. */
  onView?: (view: View) => void
  /** Called as the editor scrolls. */
  onScroll?: (scroller: HTMLElement) => void
}

/** Operations that fold or unfold, which editors without folding ignore. */
const FOLD_OPS: ReadonlySet<EditorOp['type']> = new Set([
  'fold',
  'unfold',
  'toggleFold',
  'toggleFoldAt',
  'foldAll',
  'unfoldAll',
  'foldAllRegions',
  'unfoldAllRegions',
  'foldRecursively',
  'unfoldRecursively',
  'foldLevel',
])

/**
 * Renders one document. Only lines near the visible area exist in the DOM;
 * the backend owns the text, so every edit and cursor move is an operation
 * sent through an `EditorSession`.
 */
export function EditorView({
  documentId,
  view: documentView = 0,
  title,
  transport = ipcTransport,
  zones: extraZones = NO_ZONES,
  lineClass,
  marks,
  decorations: paneDecorations = NO_DECORATIONS,
  folding = true,
  minimap = true,
  quickDiff = true,
  onView,
  onScroll: onScrolled,
}: EditorViewProps) {
  const frameRef = useRef<HTMLDivElement>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const gutterRef = useRef<HTMLDivElement>(null)
  const linesRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLTextAreaElement>(null)
  const sessionRef = useRef<EditorSession | null>(null)
  const viewRef = useRef<View | null>(null)
  const findRef = useRef<FindState | null>(null)
  const composingRef = useRef(false)
  /** How to scroll the cursor into view once a view reflects request `after`. */
  const revealRef = useRef<{ mode: Reveal; after: number } | null>(null)
  /** The last request the latest view reflects. */
  const reflectedRef = useRef(0)
  const restoreScrollRef = useRef(savedScroll(documentId, documentView))
  /** This editor's handle, once registered. */
  const controllerRef = useRef<EditorController | null>(null)
  const isActive = useStore(activeEditor) === controllerRef.current

  const { lineHeight } = useStore(appearance)
  const minimapVisible = useStore(layoutStore).minimapVisible && minimap
  const onViewRef = useRef(onView)
  onViewRef.current = onView
  const onScrolledRef = useRef(onScrolled)
  onScrolledRef.current = onScrolled
  const foldingRef = useRef(folding)
  foldingRef.current = folding
  /** Bumped when the view refreshes without an edit, for the minimap. */
  const [refreshTick, setRefreshTick] = useState(0)
  const lineHeightRef = useRef(lineHeight)
  lineHeightRef.current = lineHeight
  const [view, setView] = useState<View | null>(null)
  /** Carets and highlights as measured, without room for a peek. */
  const [measuredOverlay, setOverlay] = useState<Overlay>(EMPTY_OVERLAY)
  const [composing, setComposing] = useState(false)
  const [find, setFind] = useState<FindState | null>(null)
  const [hover, setHover] = useState<HoverState | null>(null)
  const [completion, setCompletion] = useState<CompletionState | null>(null)
  /** Lines highlighted by a Go to Line or Go to Symbol preview. */
  const [preview, setPreview] = useState<LinePreview | null>(null)
  /** Re-renders popups anchored to the text when it scrolls. */
  const [, setScrollTick] = useState(0)
  viewRef.current = view
  findRef.current = find
  const mirror = useMemo(() => (view ? inputMirror(view) : null) ?? EMPTY_MIRROR, [view])
  const mirrorRef = useRef(mirror)
  mirrorRef.current = mirror
  useLayoutEffect(() => {
    if (inputRef.current && !composing) {
      showMirror(inputRef.current, mirror)
    }
  }, [mirror, composing])
  const hoverRef = useRef(hover)
  hoverRef.current = hover
  const hoverTimerRef = useRef<number | undefined>(undefined)
  /** Bumped to discard hover answers that arrive too late. */
  const hoverSeqRef = useRef(0)
  const completionRef = useRef(completion)
  completionRef.current = completion
  const completionSeqRef = useRef(0)
  /**
   * A completion request waiting for the view to reflect the edit that asked
   * for it (request `after`), so it's made at the cursor after the edit.
   */
  const completionPendingRef = useRef<{ trigger: string | null; after: number } | null>(null)
  /** An auto-insert trigger typed, waiting for the view to reflect it. */
  const autoInsertPendingRef = useRef<{ trigger: string; after: number } | null>(null)
  const [renaming, setRenaming] = useState<RenameState | null>(null)
  const [signature, setSignature] = useState<SignatureState | null>(null)
  const signatureRef = useRef(signature)
  signatureRef.current = signature
  const signatureSeqRef = useRef(0)
  /** Signature help to ask for once the view reflects the edit that asked (request `after`). */
  const signaturePendingRef = useRef<{ trigger: string | null; after: number } | null>(null)
  /** The cursor's line, when there are code actions where it rests. */
  const [lightbulb, setLightbulb] = useState<number | null>(null)
  const lightbulbTimerRef = useRef<number | undefined>(undefined)
  const lightbulbSeqRef = useRef(0)
  /** Where the editor last looked for code actions, to look once per place. */
  const lightbulbKeyRef = useRef('')
  const [link, setLink] = useState<LinkState | null>(null)
  /** The last pointer position over the text; `null` once it leaves. */
  const pointerRef = useRef<{ x: number; y: number } | null>(null)
  /** The word whose definition was last asked for, to ask once per word. */
  const linkKeyRef = useRef<string | null>(null)
  const linkSeqRef = useRef(0)
  /** When the view being rendered arrived, and when the oldest unpainted input happened. */
  const receivedAtRef = useRef(0)
  const inputStartRef = useRef<number | null>(null)
  const referencesSeqRef = useRef(0)

  // A references peek shown in this editor, and the room it takes.
  const peekState = useStore(peekStore)
  const peek = peekState && peekIsIn(peekState, documentId, documentView) ? peekState : null
  useStore(peekLayout)
  const peekRow = peek && view ? displayRow(view, peek.anchor.line) : null
  const peekLines =
    peekRow === null
      ? 0
      : peekHeightInLines(Math.floor((scrollRef.current?.clientHeight ?? 0) / lineHeight))
  // A change from the file's original, shown inline below its lines.
  const [hunk, setHunk] = useState<Hunk | null>(null)
  const hunkRef = useRef(hunk)
  hunkRef.current = hunk
  const hunkSeqRef = useRef(0)
  const hunkRow = useMemo(() => {
    if (!hunk || !view) {
      return null
    }
    const [start, end] = hunk.modified
    const below = end > start ? end - 1 : start - 1
    return below < 0 ? -1 : displayRow(view, below)
  }, [hunk, view])
  // Extensions' code lenses: a row of links above their lines.
  const lenses = view?.lenses ?? NO_LENSES
  const lensKey = lenses.length > 0 ? JSON.stringify(lenses) : ''
  const lensZones = useMemo((): ViewZone[] => {
    if (!view || lenses.length === 0) {
      return []
    }
    const byLine = new Map<number, CodeLens[]>()
    for (const lens of lenses) {
      byLine.set(lens.line, [...(byLine.get(lens.line) ?? []), lens])
    }
    return [...byLine].map(([line, shown]) => ({
      key: `lens:${line}`,
      row: displayRow(view, line) - 1,
      lines: 1,
      className: LENS_ZONE,
      content: <CodeLenses documentId={documentId} lenses={shown} />,
    }))
    // The lenses' content (`lensKey`), not each view, changes them.
  }, [lensKey, documentId, view?.foldGeneration])
  const zones = useMemo(() => {
    const all = [...extraZones, ...lensZones]
    if (peekRow !== null) {
      all.push({ key: 'peek', row: peekRow, lines: peekLines })
    }
    if (hunk && hunkRow !== null) {
      all.push({ key: 'change', row: hunkRow, lines: changePeekLines(hunk) })
    }
    return zonesOf(all, lineHeight)
  }, [extraZones, lensZones, peekRow, peekLines, hunk, hunkRow, lineHeight])
  const zoneRef = useRef(zones)
  zoneRef.current = zones
  const peekShownRef = useRef(false)
  peekShownRef.current = peekRow !== null || hunk !== null
  /** The peek's place, if one shows. */
  const zone = peekRow === null ? null : { row: peekRow, height: peekLines * lineHeight }
  const peekZoneRef = useRef(zone)
  peekZoneRef.current = zone
  /** The editor's visible width, which a peek spans. */
  const [scrollerWidth, setScrollerWidth] = useState(0)

  // Focusing must not scroll: the input spans the cursor's whole line.
  const focusInput = () => inputRef.current?.focus({ preventScroll: true })

  /** Sends `op`; returns its request number, which views reflect once applied. */
  const dispatch = (op: EditorOp, reveal: Reveal = 'nearest'): number | null => {
    const session = sessionRef.current
    if (session && (foldingRef.current || !FOLD_OPS.has(op.type))) {
      inputStartRef.current ??= performance.now()
      const after = session.dispatch(op)
      if (reveal !== 'none') {
        revealRef.current = { mode: reveal, after }
      }
      return after
    }
    return null
  }

  /**
   * Shows the change at `line` inline, or the next or previous one from it
   * (moving the cursor there); closes it if there's none.
   */
  const showChange = async (line: number, direction: 'at' | 'next' | 'previous') => {
    const seq = ++hunkSeqRef.current
    let found: Hunk | null = null
    try {
      found = await quickDiffHunk(documentId, line, direction)
    } catch (error) {
      reportError('Showing the change', error)
    }
    if (seq !== hunkSeqRef.current) {
      return
    }
    setHunk(found)
    if (found && direction !== 'at') {
      dispatch(
        {
          type: 'setCursor',
          position: { line: found.modified[0], column: 0 },
          extend: false,
          add: false,
        },
        'centerIfOutside',
      )
    }
  }

  const closeChange = () => {
    hunkSeqRef.current++
    setHunk(null)
  }

  /** Reverts or applies the change shown, then closes it. */
  const actOnChange = async (action: 'revert' | 'apply') => {
    if (!hunk) {
      return
    }
    const [start, end] = hunk.modified
    const lines: [number, number][] = [[start, Math.max(start, end - 1)]]
    closeChange()
    focusInput()
    try {
      await (action === 'revert'
        ? quickDiffRevert(documentId, documentView, lines)
        : quickDiffApply(documentId, documentView, lines))
    } catch (error) {
      reportError(action === 'revert' ? 'Reverting the change' : 'Applying the change', error)
    }
  }

  const sendClipboard = (action: 'cut' | 'copy' | 'paste') => {
    const session = sessionRef.current
    if (session) {
      revealRef.current = { mode: 'nearest', after: session.clipboard(action) }
    }
  }

  useEffect(() => {
    // A selection requested when the document was opened (e.g. from a search
    // result). It is cleared only once a view reflecting it has arrived, so
    // a mount that is torn down early (React StrictMode) does not lose it.
    const pending = pendingSelection(documentId)
    const session = new EditorSession(
      documentId,
      documentView,
      viewportFor(scrollRef.current, lineHeightRef.current, zoneRef.current),
      (next, reflected) => {
        receivedAtRef.current = performance.now()
        const previous = viewRef.current
        if (reflected > reflectedRef.current && previous && previous.version !== next.version) {
          // This editor changed the text: the document's other editors show it too.
          for (const other of editorsFor(documentId)) {
            if (other !== controllerRef.current) {
              other.refresh()
            }
          }
        }
        reflectedRef.current = reflected
        // Current at once, before React renders it: what's awaited the
        // session's idle (Rename, Format Selection) reads the cursor next.
        viewRef.current = next
        setView(next)
        if (pending) {
          clearPendingSelection(documentId, pending)
        }
      },
      error => reportError('Editing', error),
      transport,
    )
    sessionRef.current = session
    const unregister = registerBeforeSave(documentId, () => session.idle())
    if (pending) {
      restoreScrollRef.current = null
      session.dispatch({ type: 'setCursor', position: pending.start, extend: false, add: false })
      revealRef.current = {
        mode: 'centerIfOutside',
        after: session.dispatch({
          type: 'setCursor',
          position: pending.end,
          extend: true,
          add: false,
        }),
      }
    }
    focusInput()
    return () => {
      const scroller = scrollRef.current
      if (scroller) {
        saveScroll(documentId, documentView, {
          top: scroller.scrollTop,
          left: scroller.scrollLeft,
        })
      }
      unregister()
      closePeekIn(documentId, documentView)
      session.dispose()
      sessionRef.current = null
      if (activeEditor.get() === controllerRef.current) {
        editorStatus.set(null)
      }
    }
  }, [documentId, documentView, transport])

  // Commands reach this editor through the active-editor handle.
  useEffect(() => {
    const controller: EditorController = {
      documentId,
      view: documentView,
      dispatch,
      openFind: replace => openFind(replace),
      findNext: backward => findNext(backward),
      refresh: () => {
        sessionRef.current?.refresh()
        setRefreshTick(tick => tick + 1)
      },
      focus: () => focusInput(),
      lineCount: () => viewRef.current?.lineCount ?? 0,
      scrollToLine: line => {
        const scroller = scrollRef.current
        const current = viewRef.current
        if (scroller) {
          const row = current ? displayRow(current, line) : line
          scroller.scrollTop = rowTop(row, lineHeightRef.current, zoneRef.current)
        }
      },
      scrollBy: lines => {
        const scroller = scrollRef.current
        if (scroller) {
          scroller.scrollTop += lines * lineHeightRef.current
        }
      },
      goToDefinition: () => void goToDefinition(),
      goToReferences: () => void findReferences(false),
      peekReferences: () => void findReferences(true),
      triggerSuggest: () => requestCompletion(null, null),
      showHover: () => void showHoverAtCursor(),
      format: selection => void formatDocument(selection),
      rename: () => void startRename(),
      codeActions: only => void showCodeActions(only),
      triggerParameterHints: () => void fetchSignature(null, false),
      showChange: backward =>
        void showChange(
          viewRef.current?.primary.position.line ?? 0,
          backward ? 'previous' : 'next',
        ),
      cursor: () => viewRef.current?.primary.position ?? null,
      previewLines: next => {
        setPreview(next)
        const scroller = scrollRef.current
        const current = viewRef.current
        if (next && scroller && current) {
          const lineHeight = lineHeightRef.current
          scroller.scrollTop =
            rowTop(displayRow(current, next.center), lineHeight, zoneRef.current) -
            (scroller.clientHeight - lineHeight) / 2
        }
      },
      scrollPosition: () => ({
        top: scrollRef.current?.scrollTop ?? 0,
        left: scrollRef.current?.scrollLeft ?? 0,
      }),
      restoreScroll: ({ top, left }) => {
        const scroller = scrollRef.current
        if (scroller) {
          scroller.scrollTop = top
          scroller.scrollLeft = left
        }
      },
    }
    controllerRef.current = controller
    const unregister = registerEditor(controller)
    return () => {
      unregister()
      controllerRef.current = null
    }
    // The controller reads current state through refs, so it only needs to
    // change with the document.
  }, [documentId, documentView])

  // A different font size changes which lines are visible.
  useEffect(() => {
    sessionRef.current?.setViewport(viewportFor(scrollRef.current, lineHeight, zoneRef.current))
  }, [lineHeight])

  const findQuery = find?.query ?? null
  useEffect(() => {
    sessionRef.current?.setSearch(findQuery)
  }, [findQuery])

  useEffect(() => {
    if (!view) {
      return
    }
    onViewRef.current?.(view)
    setDirty(documentId, view.dirty)
    noteVersion(documentId, view.version)
    // Another editor of the document refreshes as that one is edited; the
    // status bar is the active editor's.
    if (isActive) {
      editorStatus.set({
        line: view.primary.position.line + 1,
        column: view.primary.visualColumn + 1,
        selectionCount: view.selectionCount,
        language: view.language,
      })
    }
    const pending = completionPendingRef.current
    // Not a view from before the edit that asked (typing outran the backend).
    if (pending && reflectedRef.current >= pending.after) {
      completionPendingRef.current = null
      void fetchCompletion(view.primary.position, pending.trigger)
    }
    const inserting = autoInsertPendingRef.current
    if (inserting && reflectedRef.current >= inserting.after) {
      autoInsertPendingRef.current = null
      void fetchAutoInsert(inserting.trigger)
    }
    const signing = signaturePendingRef.current
    if (signing && reflectedRef.current >= signing.after) {
      signaturePendingRef.current = null
      void fetchSignature(signing.trigger, signatureRef.current !== null)
    } else if (
      !signing &&
      signatureRef.current &&
      !samePosition(signatureRef.current.position, view.primary.position)
    ) {
      // The cursor moved: the help follows it, or closes.
      void fetchSignature(null, true)
    }
    lookForCodeActions(view)
  }, [view, documentId, isActive])

  useEffect(
    () => () => {
      window.clearTimeout(hoverTimerRef.current)
      window.clearTimeout(lightbulbTimerRef.current)
    },
    [],
  )

  // Folding changes which lines the minimap shows without an edit.
  const foldGeneration = view?.foldGeneration
  useEffect(() => {
    if (foldGeneration !== undefined) {
      setRefreshTick(tick => tick + 1)
    }
  }, [foldGeneration])

  useLayoutEffect(() => {
    const lines = linesRef.current
    const scroller = scrollRef.current
    if (!view || !lines || !scroller) {
      return
    }
    const shownMarks = (marks ?? []).filter(
      mark => mark.end.line >= view.startLine && mark.start.line < view.endLine,
    )
    const decorations = withPaneDecorations(view, paneDecorations)
    const measured = measureOverlay(
      lines,
      view.lineNumbers,
      view.selections,
      view.search?.matches ?? [],
      lineHeight,
      view.diagnostics,
      shownMarks.length > 0
        ? [
            ...decorations,
            ...shownMarks.map(({ start, end, className }) => ({
              start,
              end,
              key: MARK_KEY + className,
            })),
          ]
        : decorations,
    )
    setOverlay(measured)
    recordTiming('editor.render', performance.now() - receivedAtRef.current)
    const painted = {
      documentId,
      version: view.version,
      startLine: view.startLine,
      endLine: view.endLine,
    }
    const page = ownerWindow(scroller)
    const frame = page.requestAnimationFrame(() => {
      if (inputStartRef.current !== null) {
        recordTiming('editor.inputToPaint', performance.now() - inputStartRef.current)
        inputStartRef.current = null
      }
      notifyPainted(painted)
    })
    const saved = restoreScrollRef.current
    if (saved) {
      restoreScrollRef.current = null
      scroller.scrollTop = saved.top
      scroller.scrollLeft = saved.left
    } else if (revealRef.current && reflectedRef.current >= revealRef.current.after) {
      const { mode } = revealRef.current
      revealRef.current = null
      const caret = measured.carets.find(c => c.primary)
      reveal(
        scroller,
        rowTop(view.primary.row, lineHeight, zoneRef.current),
        lineHeight,
        caret?.left,
        (gutterRef.current?.offsetWidth ?? 0) + (minimapVisible ? MINIMAP_WIDTH : 0),
        mode,
      )
    }
    return () => page.cancelAnimationFrame(frame)
  }, [view, lineHeight, documentId, marks, paneDecorations])

  const primarySelection = () => viewRef.current?.selections.find(s => s.primary)

  const openFind = (replace: boolean) => {
    const current = viewRef.current
    const primary = primarySelection()
    let prefill: string | null = null
    if (current && primary && primary.anchor.line === primary.head.line) {
      const text = current.lines[rowOf(current.lineNumbers, primary.head.line)]?.text
      const from = Math.min(primary.anchor.column, primary.head.column)
      const to = Math.max(primary.anchor.column, primary.head.column)
      if (text !== undefined && to > from) {
        prefill = text.slice(from, to)
      }
    }
    setFind(state => ({
      query: {
        ...(state?.query ?? EMPTY_QUERY),
        ...(prefill !== null ? { pattern: prefill } : {}),
      },
      replacement: state?.replacement ?? '',
      showReplace: replace || (state?.showReplace ?? false),
      focusRequest: (state?.focusRequest ?? 0) + 1,
    }))
  }

  const findNext = (backward: boolean) => {
    const state = findRef.current
    if (!state?.query.pattern) {
      openFind(false)
      return
    }
    dispatch({ type: 'findNext', query: state.query, backward })
  }

  const onFindQueryChange = (change: Partial<SearchQuery>) => {
    const state = findRef.current
    if (!state) {
      return
    }
    const query = { ...state.query, ...change }
    setFind({ ...state, query })
    // Find as you type: jump to the first match at or after the selection start.
    const primary = primarySelection()
    if (query.pattern && primary) {
      dispatch({
        type: 'setCursor',
        position: earlier(primary.anchor, primary.head),
        extend: false,
        add: false,
      })
      dispatch({ type: 'findNext', query, backward: false })
    }
  }

  const closeFind = () => {
    setFind(null)
    focusInput()
  }

  // Scroll events already arrive at most once per frame; requesting lines
  // right away (instead of on the next animation frame) saves a frame per
  // page of scrolling. The session ignores repeated identical viewports.
  const onScroll = () => {
    if (hoverRef.current) {
      setHover(null)
    }
    // The text moved under the pointer; the next pointer move looks again.
    clearLink()
    // Popups follow the text, and a peek's arrow its anchor.
    if (completionRef.current || signatureRef.current || renaming || peekShownRef.current) {
      setScrollTick(tick => tick + 1)
    }
    const scroller = scrollRef.current
    if (scroller && !covers(viewRef.current, scroller, lineHeight, zoneRef.current)) {
      sessionRef.current?.setViewport(viewportFor(scroller, lineHeight, zoneRef.current))
    }
    if (scroller) {
      onScrolledRef.current?.(scroller)
    }
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.nativeEvent.isComposing || composingRef.current) {
      return
    }
    // A modifier alone leaves the hover card, so its links can be Ctrl+clicked.
    if (!MODIFIER_KEYS.has(event.key)) {
      hideHover()
    }
    if (shownCompletions && shownCompletions.length > 0 && completion) {
      const plain = !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey
      const count = shownCompletions.length
      const selected = Math.min(completion.selected, count - 1)
      const select = (index: number) => {
        event.preventDefault()
        setCompletion({ ...completion, selected: Math.max(0, Math.min(count - 1, index)) })
      }
      switch (plain ? event.key : '') {
        case 'ArrowDown':
          select((selected + 1) % count)
          return
        case 'ArrowUp':
          select((selected - 1 + count) % count)
          return
        case 'PageDown':
          select(selected + COMPLETION_ROWS)
          return
        case 'PageUp':
          select(selected - COMPLETION_ROWS)
          return
        case 'Enter':
        case 'Tab':
          event.preventDefault()
          acceptCompletion(shownCompletions[selected]!)
          return
        case 'Escape':
          event.preventDefault()
          closeCompletion()
          return
      }
    }
    const shownSignature = signatureRef.current
    if (shownSignature && !event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey) {
      const count = shownSignature.help.signatures.length
      if (event.key === 'Escape') {
        event.preventDefault()
        closeSignature()
        return
      }
      if (count > 1 && (event.key === 'ArrowUp' || event.key === 'ArrowDown')) {
        event.preventDefault()
        stepSignature(event.key === 'ArrowUp' ? -1 : 1)
        return
      }
    }
    if (
      event.key === 'Escape' &&
      !event.ctrlKey &&
      !event.altKey &&
      !event.metaKey &&
      peekIsIn(peekStore.get(), documentId, documentView)
    ) {
      event.preventDefault()
      closePeek()
      return
    }
    if (event.key === 'Escape' && hunkRef.current) {
      event.preventDefault()
      closeChange()
      return
    }
    if (event.key === 'Escape' && findRef.current) {
      event.preventDefault()
      closeFind()
      return
    }
    const pageLines = Math.max(
      1,
      Math.floor((scrollRef.current?.clientHeight ?? 0) / lineHeight) - 1,
    )
    const command = keyToCommand(event, isMac(), pageLines)
    if (!command) {
      return
    }
    event.preventDefault()
    if (command.kind === 'op') {
      const edit = dispatch(command.op)
      // Enter can ask extensions for text too (Luau's `end`).
      if (command.op.type === 'newline' && edit !== null) {
        askAutoInsert('\n', edit)
      }
    } else if (command.kind === 'clipboard') {
      sendClipboard(command.action)
    }
  }

  const insertTyped = (text: string) => {
    const edit = dispatch({ type: 'type', text })
    if (edit !== null) {
      afterTyping(text, edit)
    }
  }

  // Typing is applied to the document, not the input: the input mirrors the
  // view (see inputMirror.ts). Keys and clipboard events are handled
  // before they reach `beforeinput`; composition text can't be cancelled.
  useEffect(() => {
    const input = inputRef.current
    if (!input) {
      return
    }
    const onBeforeInput = (event: InputEvent) => {
      if (event.isComposing || composingRef.current) {
        return
      }
      if (event.inputType === 'insertCompositionText') {
        return
      }
      event.preventDefault()
      if (
        (event.inputType === 'insertText' || event.inputType === 'insertReplacementText') &&
        event.data
      ) {
        insertTyped(event.data)
      }
    }
    input.addEventListener('beforeinput', onBeforeInput)
    return () => input.removeEventListener('beforeinput', onBeforeInput)
  })

  // Input the browser applied anyway: apply the difference, then restore.
  const onInput = (event: FormEvent<HTMLTextAreaElement>) => {
    if ((event.nativeEvent as InputEvent).isComposing || composingRef.current) {
      return
    }
    const input = event.currentTarget
    const current = mirrorRef.current
    const typed = typedText(current, input.value)
    showMirror(input, current)
    if (typed) {
      insertTyped(typed)
    }
  }

  const onCompositionStart = () => {
    composingRef.current = true
    setComposing(true)
  }

  const onCompositionEnd = (event: CompositionEvent<HTMLTextAreaElement>) => {
    composingRef.current = false
    setComposing(false)
    showMirror(event.currentTarget, mirrorRef.current)
    if (event.data) {
      insertTyped(event.data)
    }
  }

  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    event.preventDefault()
    const text = event.clipboardData.getData('text/plain')
    if (text) {
      dispatch({ type: 'paste', text })
    }
  }

  const onClipboardEvent = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    event.preventDefault()
    sessionRef.current?.clipboard(event.type === 'cut' ? 'cut' : 'copy')
  }

  const positionAt = (clientX: number, clientY: number): Position | null => {
    const lines = linesRef.current
    const current = viewRef.current
    if (!lines || !current) {
      return null
    }
    const top = lines.getBoundingClientRect().top
    const zone = zoneRef.current
    const layerTop = rowTop(current.startRow, lineHeight, zone)
    const row = rowAtY(clientY - top + layerTop, lineHeight, zone) - current.startRow
    if (current.startRow + row < 0) {
      return { line: 0, column: 0 }
    }
    if (current.startRow + row >= current.rowCount) {
      return { line: current.lineCount - 1, column: END_OF_LINE }
    }
    const element = lines.children[row]
    const middle =
      top + rowTop(current.startRow + row, lineHeight, zone) - layerTop + lineHeight / 2
    const column = element ? columnAt(element, clientX, middle) : 0
    return { line: lineAtRow(current, row), column }
  }

  /**
   * Whether a point is below the last line, in the space that fills the
   * view when the document is short. Clicks there go to the end of the last
   * line, but nothing is hovered or linked.
   */
  const belowText = (clientY: number): boolean => {
    const lines = linesRef.current
    const current = viewRef.current
    if (!lines || !current) {
      return false
    }
    const zone = zoneRef.current
    const textBottom =
      lines.getBoundingClientRect().top +
      rowTop(current.rowCount, lineHeight, zone) -
      rowTop(current.startRow, lineHeight, zone)
    return clientY >= textBottom
  }

  const onMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0) {
      return
    }
    event.preventDefault()
    focusInput()
    hideHover()
    closeCompletion()
    const position = positionAt(event.clientX, event.clientY)
    if (!position) {
      return
    }
    // The marker after a folded line unfolds it.
    if ((event.target as Element).closest('.editor-view__folded')) {
      dispatch({ type: 'toggleFoldAt', line: position.line })
      return
    }
    // A web address or a path opens through the link handlers;
    // Ctrl+Shift+click asks who opens it.
    const linkModifier = (isMac() ? event.metaKey : event.ctrlKey) && !event.altKey
    const address = linkModifier ? webAddressAt(event.clientX, event.clientY) : null
    if (address) {
      openLink({ kind: 'web', target: address.url }, event.shiftKey)
      return
    }
    const path = linkModifier ? pathAt(event.clientX, event.clientY) : null
    if (path) {
      // Usually known from hovering with Ctrl held; otherwise asked now.
      const choose = event.shiftKey
      const definition = definitionModifier(event) && !belowText(event.clientY)
      dispatch({ type: 'setCursor', position, extend: choose, add: false })
      void resolvePath(path.candidate, linkFolders()).then(file => {
        if (file) {
          openLink(file, choose)
        } else if (definition) {
          void goToDefinition(position)
        }
      })
      return
    }
    if (definitionModifier(event)) {
      dispatch({ type: 'setCursor', position, extend: false, add: false })
      if (!belowText(event.clientY)) {
        void goToDefinition(position)
      }
      return
    }
    if (event.detail === 2) {
      dispatch({ type: 'selectWord', position })
      return
    }
    dispatch({ type: 'setCursor', position, extend: event.shiftKey, add: event.altKey })

    const onMove = (move: MouseEvent) => {
      const to = positionAt(move.clientX, move.clientY)
      if (to) {
        dispatch({ type: 'setCursor', position: to, extend: true, add: false })
      }
    }
    // The editor's window, which may not be the main one.
    const view = ownerWindow(event.currentTarget)
    const onUp = () => {
      view.removeEventListener('mousemove', onMove)
      view.removeEventListener('mouseup', onUp)
    }
    view.addEventListener('mousemove', onMove)
    view.addEventListener('mouseup', onUp)
  }

  const hideHover = () => {
    window.clearTimeout(hoverTimerRef.current)
    hoverSeqRef.current++
    if (hoverRef.current) {
      setHover(null)
    }
  }

  /**
   * Where a popup of `height` goes: below `line`, or above it if there is no
   * room; with `above`, above it if there's room there.
   */
  const placeAt = (line: number, left: number, height: number, above = false): Placement | null => {
    const frame = frameRef.current
    const lines = linesRef.current
    const current = viewRef.current
    if (!frame || !lines || !current) {
      return null
    }
    const row = rowOf(current.lineNumbers, line)
    if (row < 0) {
      return null
    }
    const frameBox = frame.getBoundingClientRect()
    const zone = zoneRef.current
    const lineTop =
      lines.getBoundingClientRect().top -
      frameBox.top +
      rowTop(current.startRow + row, lineHeight, zone) -
      rowTop(current.startRow, lineHeight, zone)
    const x = Math.max(0, Math.min(left, frameBox.width - COMPLETION_WIDTH))
    const fitsBelow = lineTop + lineHeight + height <= frameBox.height
    const showAbove = above ? lineTop >= height : !fitsBelow && lineTop >= height
    return showAbove
      ? { bottom: frameBox.height - lineTop, left: x }
      : { top: lineTop + lineHeight, left: x }
  }

  const showHoverAt = async (position: Position, x: number) => {
    const seq = ++hoverSeqRef.current
    let result: lsp.Hover | null = null
    try {
      result = await lsp.hover(documentId, position)
    } catch (error) {
      console.debug('hover failed', error)
    }
    const frame = frameRef.current
    if (seq !== hoverSeqRef.current || !result || !frame) {
      return
    }
    const left = x - frame.getBoundingClientRect().left
    // Hover cards vary in height: show below the line in the upper half.
    const placement = placeAt(position.line, left, frame.clientHeight / 2)
    if (placement) {
      setHover({ hover: result, line: position.line, x, placement })
    }
  }

  const showHoverAtCursor = async () => {
    const current = viewRef.current
    const caret = overlay.carets.find(c => c.primary)
    const lines = linesRef.current
    if (current && caret && lines) {
      await showHoverAt(current.primary.position, lines.getBoundingClientRect().left + caret.left)
    }
  }

  const onContentMouseMove = (event: ReactMouseEvent<HTMLDivElement>) => {
    const { clientX, clientY } = event
    pointerRef.current = { x: clientX, y: clientY }
    updateLink(event.buttons === 0 && definitionModifier(event))
    if (event.buttons !== 0) {
      return
    }
    const position = positionAt(clientX, clientY)
    const shown = hoverRef.current
    if (shown && position?.line === shown.line && Math.abs(clientX - shown.x) < HOVER_SLACK) {
      return
    }
    hideHover()
    // Hover shows language information and diagnostics (which extensions
    // may publish for any file).
    const current = viewRef.current
    if (!position || !(current?.language || current?.diagnostics.length) || belowText(clientY)) {
      return
    }
    hoverTimerRef.current = window.setTimeout(() => {
      // Only over text, not the empty space after a line.
      const lines = linesRef.current
      const element = lines?.children[rowOf(viewRef.current?.lineNumbers ?? [], position.line)]
      if (!lines || !element) {
        return
      }
      const end = lines.getBoundingClientRect().left + columnX(element, lineLength(element))
      if (clientX <= end) {
        void showHoverAt(position, clientX)
      }
    }, HOVER_DELAY)
  }

  /** The editor's context menu: clipboard, language, extension, and palette entries. */
  const onContextMenu = (event: ReactMouseEvent<HTMLDivElement>) => {
    event.preventDefault()
    hideHover()
    closeCompletion()
    focusInput()
    // Shift+F10 and the menu key target the focused input (the mouse can't
    // reach it): open the menu at the cursor, without moving it.
    const fromKeyboard = event.target === inputRef.current
    const position = fromKeyboard ? null : positionAt(event.clientX, event.clientY)
    const current = viewRef.current
    let [x, y] = [event.clientX, event.clientY]
    const caret = overlay.carets.find(candidate => candidate.primary)
    const lines = linesRef.current?.getBoundingClientRect()
    if (fromKeyboard && caret && lines) {
      ;[x, y] = [lines.left + caret.left, lines.top + caret.top + lineHeight]
    }
    // Right-clicking outside the selection moves the cursor there first.
    if (position && current) {
      const inside = current.selections.some(selection => {
        const [start, end] = [
          earlier(selection.anchor, selection.head),
          later(selection.anchor, selection.head),
        ]
        return (
          !samePosition(start, end) && !earlierThan(position, start) && !earlierThan(end, position)
        )
      })
      if (!inside) {
        dispatch({ type: 'setCursor', position, extend: false, add: false })
      }
    }
    const hint = (id: string) => primaryKeybinding(getCommand(id))
    const clipboard = (action: 'cut' | 'copy' | 'paste') => () => sendClipboard(action)
    const entries: MenuEntry[] = []
    if (current?.language) {
      entries.push(
        {
          label: 'Go to Definition',
          hint: hint('editor.goToDefinition'),
          run: () => goToDefinition(position ?? undefined),
        },
        {
          label: 'Go to References',
          hint: hint('editor.goToReferences'),
          run: () => findReferences(false, position ?? undefined),
        },
        null,
        { label: 'Rename Symbol', hint: hint('editor.rename'), run: () => startRename() },
        {
          label: 'Code Actions…',
          hint: hint('editor.codeActions'),
          run: () => showCodeActions(null),
        },
        null,
        {
          label: 'Format Document',
          hint: hint('editor.formatDocument'),
          run: () => formatDocument(false),
        },
        {
          label: 'Format Selection',
          hint: hint('editor.formatSelection'),
          disabled: !current.selections.some(
            selection => !samePosition(selection.anchor, selection.head),
          ),
          run: () => formatDocument(true),
        },
        null,
      )
    }
    entries.push(
      { label: 'Cut', hint: isMac() ? '⌘X' : 'Ctrl+X', run: clipboard('cut') },
      { label: 'Copy', hint: isMac() ? '⌘C' : 'Ctrl+C', run: clipboard('copy') },
      { label: 'Paste', hint: isMac() ? '⌘V' : 'Ctrl+V', run: clipboard('paste') },
      null,
      ...contributedMenuItems('editor/context', {
        path: documentsStore.get().documents.find(d => d.id === documentId)?.path ?? null,
        directory: false,
      }).map(item => ({
        label: item.title,
        hint: hint(item.command),
        run: () => runContributed(item.command),
      })),
      null,
      {
        label: 'Command Palette…',
        hint: hint('workbench.commandPalette'),
        run: () => openPalette('>'),
      },
    )
    openContextMenu(x, y, entries)
  }

  const onContentMouseLeave = (event: ReactMouseEvent<HTMLDivElement>) => {
    pointerRef.current = null
    clearLink()
    const to = event.relatedTarget
    const card = frameRef.current?.querySelector('.hover-card')
    if (!(isNode(to) && card?.contains(to))) {
      hideHover()
    }
  }

  const goToDefinition = async (position?: Position) => {
    const at = position ?? viewRef.current?.primary.position
    if (!at) {
      return
    }
    try {
      const location = await lsp.definition(documentId, at)
      if (!location) {
        notify('info', 'No definition found.')
      } else if (
        location.path === documentPath() &&
        !earlierThan(at, location.start) &&
        !earlierThan(location.end, at)
      ) {
        // Already at the definition: show its references instead, as VS Code does.
        await findReferences(false, at)
      } else {
        await openPath(location.path, { start: location.start })
      }
    } catch (error) {
      reportError('Going to the definition', error)
    }
  }

  const documentPath = () =>
    documentsStore.get().documents.find(document => document.id === documentId)?.path ?? null

  /**
   * Finds the references to the symbol at `position` (or the cursor). As
   * in VS Code, a lone reference is gone to unless `peekAlways`; several
   * open a peek below the line.
   */
  const findReferences = async (peekAlways: boolean, position?: Position) => {
    const at = position ?? viewRef.current?.primary.position
    if (!at) {
      return
    }
    const seq = ++referencesSeqRef.current
    let found: lsp.Reference[] | null
    try {
      found = await lsp.references(documentId, at)
    } catch (error) {
      reportError('Finding references', error)
      return
    }
    if (seq !== referencesSeqRef.current) {
      return
    }
    if (!found || found.length === 0) {
      const word = wordAtPosition(at)
      notify('info', word ? `No references found for '${word}'.` : 'No references found.')
    } else if (found.length === 1 && !peekAlways) {
      await openPath(found[0]!.path, { start: found[0]!.start })
    } else {
      openPeek(documentId, documentView, documentPath(), at, 'References', found)
    }
  }

  /** The word at or just before `position`, if the view has its line. */
  const wordAtPosition = (position: Position): string | null => {
    const current = viewRef.current
    const text = current?.lines[rowOf(current.lineNumbers, position.line)]?.text
    if (text === undefined) {
      return null
    }
    const range = wordAround(text, position.column) ?? wordAround(text, position.column - 1)
    return range && text.slice(range[0], range[1])
  }

  /** The web address under a point of the text, if any. */
  const webAddressAt = (clientX: number, clientY: number): (Word & { url: string }) | null => {
    const position = positionAt(clientX, clientY)
    const current = viewRef.current
    if (!position || !current || belowText(clientY)) {
      return null
    }
    const text = current.lines[rowOf(current.lineNumbers, position.line)]?.text
    if (text === undefined) {
      return null
    }
    const found = findCandidates(text).find(
      candidate =>
        candidate.kind === 'web' &&
        candidate.start <= position.column &&
        position.column <= candidate.end,
    )
    return found
      ? { line: position.line, from: found.start, to: found.end, url: found.target }
      : null
  }

  /** A path in the text under a point (which may not lead anywhere), if any. */
  const pathAt = (clientX: number, clientY: number): (Word & { candidate: Candidate }) | null => {
    const position = positionAt(clientX, clientY)
    const current = viewRef.current
    if (!position || !current || belowText(clientY)) {
      return null
    }
    const text = current.lines[rowOf(current.lineNumbers, position.line)]?.text
    if (text === undefined) {
      return null
    }
    const found = findCandidates(text).find(
      candidate =>
        candidate.kind === 'path' &&
        candidate.start <= position.column &&
        position.column <= candidate.end,
    )
    return found
      ? { line: position.line, from: found.start, to: found.end, candidate: found }
      : null
  }

  /** Where relative paths in the text are looked for: the document's folder, then the open folder. */
  const linkFolders = (): string[] => {
    const path = documentPath()
    const root = workspaceStore.get().info?.root ?? null
    return [path && folderOf(path), root].filter((folder): folder is string => !!folder)
  }

  /** The word under a point of the text, if any. */
  const wordAt = (clientX: number, clientY: number): Word | null => {
    const position = positionAt(clientX, clientY)
    const lines = linesRef.current
    const current = viewRef.current
    if (!position || !lines || !current || belowText(clientY)) {
      return null
    }
    const row = rowOf(current.lineNumbers, position.line)
    const element = lines.children[row]
    const text = current.lines[row]?.text
    if (!element || text === undefined) {
      return null
    }
    // `positionAt` finds the nearest caret boundary; left of it, the
    // pointer is over the character before it.
    const boundary = lines.getBoundingClientRect().left + columnX(element, position.column)
    const index = clientX < boundary ? position.column - 1 : position.column
    const word = wordAround(text, index)
    return word && { line: position.line, from: word[0], to: word[1] }
  }

  const clearLink = () => {
    linkSeqRef.current++
    linkKeyRef.current = null
    setLink(null)
  }

  /**
   * Draws the word under the pointer as a link while `held` (the
   * go-to-definition modifier is down), once the server confirms it has a
   * definition, as VS Code does; or the web address or path under it, a
   * path once it's found to lead to a file or folder.
   */
  const updateLink = (held: boolean) => {
    const pointer = pointerRef.current
    const current = viewRef.current
    // A web address is a link at once: Ctrl+click opens it.
    const address = held && pointer && current ? webAddressAt(pointer.x, pointer.y) : null
    if (address && current) {
      const key = `${current.version}:url:${address.line}:${address.from}`
      if (key !== linkKeyRef.current) {
        clearLink()
        linkKeyRef.current = key
        setLink({ ...address, version: current.version })
      }
      return
    }
    const path = held && pointer && current ? pathAt(pointer.x, pointer.y) : null
    // Moving along a path already drawn as a link keeps it.
    if (
      path &&
      link &&
      link.version === current?.version &&
      link.line === path.line &&
      link.from === path.from &&
      link.to === path.to
    ) {
      return
    }
    const word = held && pointer && current?.language ? wordAt(pointer.x, pointer.y) : null
    const key =
      (path || word) && current
        ? `${current.version}:${path ? `path:${path.line}:${path.from}:` : ''}${word ? `${word.line}:${word.from}` : ''}`
        : null
    if (key === linkKeyRef.current) {
      return
    }
    clearLink()
    if ((!word && !path) || !current) {
      return
    }
    linkKeyRef.current = key
    const seq = linkSeqRef.current
    const version = current.version
    const lookUpDefinition = () => {
      if (!word) {
        return
      }
      lsp.definition(documentId, { line: word.line, column: word.from }).then(
        location => {
          if (location && seq === linkSeqRef.current) {
            setLink({ ...word, version })
          }
        },
        error => console.debug('definition lookup failed', error),
      )
    }
    if (!path) {
      lookUpDefinition()
      return
    }
    void resolvePath(path.candidate, linkFolders()).then(file => {
      if (seq !== linkSeqRef.current) {
        return
      }
      if (file) {
        setLink({ line: path.line, from: path.from, to: path.to, version })
      } else {
        lookUpDefinition()
      }
    })
  }
  const updateLinkRef = useRef(updateLink)
  updateLinkRef.current = updateLink

  // Pressing or releasing the modifier with the pointer over a symbol shows
  // or hides its link, wherever focus is.
  useEffect(() => {
    const onKey = (event: globalThis.KeyboardEvent) =>
      updateLinkRef.current(definitionModifier(event))
    const onBlur = () => updateLinkRef.current(false)
    return onEveryWindow(view => {
      view.addEventListener('keydown', onKey)
      view.addEventListener('keyup', onKey)
      view.addEventListener('blur', onBlur)
      return () => {
        view.removeEventListener('keydown', onKey)
        view.removeEventListener('keyup', onKey)
        view.removeEventListener('blur', onBlur)
      }
    })
  }, [])

  const closeCompletion = () => {
    completionSeqRef.current++
    completionPendingRef.current = null
    if (completionRef.current) {
      setCompletion(null)
    }
  }

  /**
   * Asks for completions at the cursor: right away, or (`afterEdit`) once
   * the view reflects the edit just dispatched.
   */
  const requestCompletion = (trigger: string | null, afterEdit: number | null) => {
    const current = viewRef.current
    if (!current?.language?.completionTriggers) {
      return
    }
    if (afterEdit !== null) {
      completionPendingRef.current = { trigger, after: afterEdit }
    } else {
      void fetchCompletion(current.primary.position, trigger)
    }
  }

  const fetchCompletion = async (position: Position, trigger: string | null) => {
    const seq = ++completionSeqRef.current
    let result: lsp.Completions | null = null
    try {
      result = await lsp.completion(documentId, position, trigger)
    } catch (error) {
      console.debug('completion failed', error)
    }
    if (seq !== completionSeqRef.current) {
      return
    }
    setCompletion(
      result && result.items.length > 0
        ? { items: result.items, incomplete: result.incomplete, selected: 0 }
        : null,
    )
  }

  /**
   * Asks extensions what to insert after `trigger` was typed (an HTML
   * closing tag), and inserts it. The operation applies only if nothing
   * moved in the meantime.
   */
  const fetchAutoInsert = async (trigger: string) => {
    let answer: AutoInsert | null = null
    try {
      answer = await autoInsert(documentId, documentView, trigger)
    } catch (error) {
      console.debug('auto insert failed', error)
    }
    if (answer) {
      dispatch({ type: 'autoInsert', ...answer })
    }
  }

  /**
   * Asks extensions for text to insert once the view reflects edit `edit`,
   * if `text` (one character, `\n` for Enter) is one of their triggers.
   */
  const askAutoInsert = (text: string, edit: number) => {
    const inserts = viewRef.current?.language?.autoInsertTriggers
    if (inserts && text.length === 1 && inserts.includes(text)) {
      autoInsertPendingRef.current = { trigger: text, after: edit }
    }
  }

  /**
   * Opens, refines, or closes completion after `text` was typed, by the edit
   * with request number `edit`, and asks extensions for text to insert after
   * their trigger characters.
   */
  const afterTyping = (text: string, edit: number) => {
    askAutoInsert(text, edit)
    askSignature(text, edit)
    const triggers = viewRef.current?.language?.completionTriggers
    const last = text.slice(-1)
    if (!triggers || !last) {
      return
    }
    const open = completionRef.current
    if (triggers.includes(last)) {
      requestCompletion(last, edit)
    } else if (isWordChar(last)) {
      if (!open || open.incomplete) {
        requestCompletion(null, edit)
      }
    } else {
      closeCompletion()
    }
  }

  /**
   * Asks for signature help once the view reflects edit `edit`: when `text`
   * ends with a character that opens or updates it, or (while it shows) any
   * typing.
   */
  const askSignature = (text: string, edit: number) => {
    const triggers = viewRef.current?.language?.signatureTriggers
    const last = text.slice(-1)
    if (!triggers || !last) {
      return
    }
    if (triggers.open.includes(last) || (signatureRef.current && triggers.update.includes(last))) {
      signaturePendingRef.current = { trigger: last, after: edit }
    } else if (signatureRef.current) {
      signaturePendingRef.current = { trigger: null, after: edit }
    }
  }

  /** Shows signature help for the call at the cursor, or closes it if there's none. */
  const fetchSignature = async (trigger: string | null, retrigger: boolean) => {
    const position = viewRef.current?.primary.position
    if (!position) {
      return
    }
    const seq = ++signatureSeqRef.current
    let help: lsp.SignatureHelp | null = null
    try {
      help = await lsp.signatureHelp(documentId, position, trigger, retrigger)
    } catch (error) {
      console.debug('signature help failed', error)
    }
    if (seq !== signatureSeqRef.current) {
      return
    }
    setSignature(help ? { help, selected: help.activeSignature, position } : null)
  }

  const closeSignature = () => {
    signatureSeqRef.current++
    signaturePendingRef.current = null
    if (signatureRef.current) {
      setSignature(null)
    }
  }

  /** Shows another of several signatures. */
  const stepSignature = (by: number) => {
    const shown = signatureRef.current
    if (shown) {
      const count = shown.help.signatures.length
      setSignature({ ...shown, selected: (shown.selected + by + count) % count })
    }
  }

  /** The primary selection, start first. */
  const selectedRange = (): lsp.TextRange | null => {
    const selection = primarySelection()
    return selection
      ? {
          start: earlier(selection.anchor, selection.head),
          end: later(selection.anchor, selection.head),
        }
      : null
  }

  /** Tells the user what a language server's edit couldn't do, or what it did beyond this editor. */
  const reportApplied = (applied: lsp.AppliedEdit) => {
    if (applied.failures.length > 0) {
      notify('warning', `Some changes couldn't be made: ${applied.failures.join('; ')}`)
    } else if (applied.files.length > 0) {
      const count = applied.files.length
      notify(
        'info',
        `${count} ${count === 1 ? 'file' : 'files'} that weren't open changed too, and were saved.`,
      )
    }
  }

  /** Formats the document, or the selected lines (the cursor's line without a selection). */
  const formatDocument = async (selection: boolean) => {
    await sessionRef.current?.idle()
    const current = viewRef.current
    let range: lsp.TextRange | null = null
    if (selection) {
      range = selectedRange()
      if (!range) {
        return
      }
      if (samePosition(range.start, range.end)) {
        range = {
          start: { line: range.start.line, column: 0 },
          end: { line: range.start.line, column: END_OF_LINE },
        }
      }
    }
    try {
      const outcome = await lsp.format(documentId, range)
      if (outcome === 'noFormatter') {
        const language = current?.language?.language ?? 'plain text'
        notify(
          'info',
          selection
            ? `There's no formatter for selections in ${language} files.`
            : `There's no formatter for ${language} files.`,
        )
      } else if (outcome === 'failed') {
        notify('warning', "Formatting failed; the language server's log in Output may say why.")
      }
    } catch (error) {
      reportError('Formatting', error)
    }
  }

  /** Opens the box that renames the symbol at the cursor. */
  const startRename = async () => {
    await sessionRef.current?.idle()
    const at = viewRef.current?.primary.position
    if (!at) {
      return
    }
    closeCompletion()
    hideHover()
    let target: lsp.RenameTarget | null
    try {
      target = await lsp.prepareRename(documentId, at)
    } catch (error) {
      if (error instanceof CommandError && error.code === 'cannotRename') {
        notify('info', error.message)
      } else {
        reportError('Renaming', error)
      }
      return
    }
    if (!target) {
      notify('info', 'No language server renames symbols in this file.')
      return
    }
    setRenaming({ target, position: at })
  }

  const submitRename = async (state: RenameState, name: string) => {
    setRenaming(null)
    focusInput()
    try {
      reportApplied(await lsp.rename(documentId, state.position, name))
    } catch (error) {
      if (error instanceof CommandError && error.code === 'cannotRename') {
        notify('info', error.message)
      } else {
        reportError('Renaming', error)
      }
    }
  }

  /** Where a menu at the cursor opens, in the window. */
  const cursorPoint = (): [number, number] => {
    const caret = overlay.carets.find(candidate => candidate.primary)
    const lines = linesRef.current?.getBoundingClientRect()
    return caret && lines
      ? [lines.left + caret.left, lines.top + caret.top + lineHeight]
      : [lines?.left ?? 0, lines?.top ?? 0]
  }

  /**
   * Shows the code actions for the selection in a menu at the cursor, or with
   * `only` (Organize Imports), applies the first of that kind.
   */
  const showCodeActions = async (only: string | null) => {
    await sessionRef.current?.idle()
    const range = selectedRange()
    if (!range) {
      return
    }
    let list: lsp.CodeActions | null
    try {
      list = await lsp.codeActions(documentId, range, false, only)
    } catch (error) {
      reportError('Finding code actions', error)
      return
    }
    if (!list || list.actions.length === 0) {
      notify(
        'info',
        only === lsp.ORGANIZE_IMPORTS
          ? 'There are no imports to organize.'
          : 'No code actions are available here.',
      )
      return
    }
    const token = list.token
    if (only) {
      void applyCodeAction(token, 0)
      return
    }
    const [x, y] = cursorPoint()
    const entries: MenuEntry[] = []
    let group: string | null = null
    list.actions.forEach((action, index) => {
      const kind = action.kind?.split('.')[0] ?? ''
      if (group !== null && kind !== group) {
        entries.push(null)
      }
      group = kind
      entries.push({
        label: action.title,
        disabled: action.disabled !== null,
        run: () => applyCodeAction(token, index),
      })
    })
    openContextMenu(x, y, entries)
  }

  const applyCodeAction = async (token: number, index: number) => {
    try {
      const applied = await lsp.applyCodeAction(token, index)
      reportApplied(applied.edit)
      // Commands for the editor that actions ask for.
      switch (applied.command) {
        case 'editor.action.triggerSuggest':
          requestCompletion(null, null)
          break
        case 'editor.action.triggerParameterHints':
          void fetchSignature(null, false)
          break
        case 'editor.action.rename':
          void startRename()
          break
        case null:
          break
        default:
          console.debug('a code action asked for an unknown editor command', applied.command)
      }
    } catch (error) {
      reportError('Applying the code action', error)
    }
    focusInput()
  }

  /**
   * Looks for code actions where the cursor rests in `current`, a moment
   * after it stops, to show the lightbulb.
   */
  const lookForCodeActions = (current: View) => {
    const range = selectedRange()
    if (!current.language?.codeActions || !range) {
      lightbulbKeyRef.current = ''
      window.clearTimeout(lightbulbTimerRef.current)
      setLightbulb(null)
      return
    }
    const key = [
      current.version,
      current.diagnostics.length,
      range.start.line,
      range.start.column,
      range.end.line,
      range.end.column,
    ].join(':')
    if (key === lightbulbKeyRef.current) {
      return
    }
    lightbulbKeyRef.current = key
    window.clearTimeout(lightbulbTimerRef.current)
    const seq = ++lightbulbSeqRef.current
    const line = current.primary.position.line
    setLightbulb(shown => (shown === line ? shown : null))
    lightbulbTimerRef.current = window.setTimeout(() => {
      lsp
        .codeActions(documentId, range, true)
        .catch(() => null)
        .then(list => {
          if (seq === lightbulbSeqRef.current) {
            setLightbulb(list && list.actions.length > 0 ? line : null)
          }
        })
    }, LIGHTBULB_DELAY)
  }

  const acceptCompletion = (item: lsp.CompletionItem) => {
    // Select from the item's start to the cursor, then replace it.
    dispatch({ type: 'setCursor', position: item.from, extend: true, add: false })
    dispatch(
      item.snippet
        ? {
            type: 'insertSnippet',
            snippet: item.insert,
            adjustIndentation: item.adjustIndentation,
          }
        : { type: 'insert', text: item.insert },
    )
    closeCompletion()
    focusInput()
  }

  const startRow = view?.startRow ?? 0
  /** The overlay with rows moved down past the zones above them. */
  const overlay = useMemo(() => {
    const above = zonesAbove(zones, startRow)
    if (zones.before.at(-1) === above) {
      return measuredOverlay
    }
    const shift = <T extends Rect>(rects: T[]) =>
      rects.map(rect => {
        const by = zonesAbove(zones, startRow + Math.floor(rect.top / lineHeight)) - above
        return by === 0 ? rect : { ...rect, top: rect.top + by }
      })
    return {
      carets: shift(measuredOverlay.carets),
      selections: shift(measuredOverlay.selections),
      matches: shift(measuredOverlay.matches),
      diagnostics: shift(measuredOverlay.diagnostics),
      decorations: shift(measuredOverlay.decorations),
    }
  }, [measuredOverlay, zones, startRow, lineHeight])

  // An edit closes a change shown inline: its lines may be different now.
  const version = view?.version
  useEffect(() => {
    hunkSeqRef.current++
    setHunk(null)
  }, [version, documentId])

  const hasPeek = peek !== null || hunk !== null
  useLayoutEffect(() => {
    const scroller = scrollRef.current
    if (!hasPeek || !scroller) {
      return
    }
    const measure = () => setScrollerWidth(scroller.clientWidth)
    measure()
    const observer = new (resizeObserverFor(scroller))(measure)
    observer.observe(scroller)
    return () => observer.disconnect()
  }, [hasPeek])

  // A peek opening (or moving) scrolls so it shows below its line.
  const peekRequest = peek && zone ? `${zone.row}:${peek.focusRequest}` : null
  useLayoutEffect(() => {
    const scroller = scrollRef.current
    const current = peekZoneRef.current
    if (!peekRequest || !scroller || !current) {
      return
    }
    const anchorTop = rowTop(current.row, lineHeight, zoneRef.current)
    const bottom = anchorTop + 2 * lineHeight + current.height
    if (bottom > scroller.scrollTop + scroller.clientHeight) {
      scroller.scrollTop = Math.min(anchorTop, bottom - scroller.clientHeight)
    }
    if (anchorTop < scroller.scrollTop) {
      scroller.scrollTop = anchorTop
    }
  }, [peekRequest, lineHeight])

  // The rows on screen change as zones (a peek) open, close, or are resized.
  useEffect(() => {
    sessionRef.current?.setViewport(viewportFor(scrollRef.current, lineHeight, zones))
  }, [zones, lineHeight])

  const primaryCaret: Rect | undefined = overlay.carets.find(c => c.primary)
  const cursorLine = view?.primary.position.line ?? 0
  const shownCompletions =
    completion && view
      ? visibleCompletions(
          completion.items,
          view.primary.position,
          view.lines[rowOf(view.lineNumbers, cursorLine)]?.text ?? '',
        )
      : null
  const completionLeft =
    (linesRef.current?.getBoundingClientRect().left ?? 0) -
    (frameRef.current?.getBoundingClientRect().left ?? 0) +
    (primaryCaret?.left ?? 0)
  const completionPlacement =
    shownCompletions && shownCompletions.length > 0 && primaryCaret
      ? placeAt(
          cursorLine,
          completionLeft,
          Math.min(shownCompletions.length, COMPLETION_ROWS) * COMPLETION_ROW_HEIGHT + 2,
        )
      : null

  const signaturePlacement =
    signature && primaryCaret ? placeAt(cursorLine, completionLeft, SIGNATURE_HEIGHT, true) : null
  let renamePlacement: Placement | null = null
  if (renaming && view) {
    const { start } = renaming.target
    const element = linesRef.current?.children[rowOf(view.lineNumbers, start.line)]
    const frame = frameRef.current
    if (element && frame) {
      const left =
        element.getBoundingClientRect().left -
        frame.getBoundingClientRect().left +
        columnX(element, start.column)
      renamePlacement = placeAt(start.line, left, 56)
    }
  }

  const rowCount = view?.rowCount ?? 1
  const lineCount = view?.lineCount ?? 1
  const lineNumbers = view?.lineNumbers ?? NO_LINES
  const foldedLines = useMemo(() => new Set(folding ? view?.folded : NO_LINES), [view, folding])
  /** Gutter markers of changes from the original, by line. */
  const changeMarks = useMemo(() => {
    const marks = new Map<number, ChangeMark>()
    if (!quickDiff || !view) {
      return marks
    }
    for (const change of view.changes) {
      if (change.start === change.end) {
        // Removed lines: below the line before them, or above the first.
        marks.set(
          Math.max(0, change.start - 1),
          change.start === 0 ? 'removedAbove' : 'removedBelow',
        )
      } else {
        for (let line = change.start; line < change.end; line++) {
          marks.set(line, change.kind)
        }
      }
    }
    return marks
  }, [view, quickDiff])
  /** Classes for lines: the caller's, or the shown change's. */
  const classOf = (line: number) =>
    lineClass?.(line) ??
    (hunk && line >= hunk.modified[0] && line < hunk.modified[1] ? 'diff-inserted' : undefined)
  const foldableLines = useMemo(() => new Set(folding ? view?.foldable : NO_LINES), [view, folding])
  const extensions = useStore(extensionsStore)
  const kind = useStore(themeKind)
  const types = useMemo(() => decorationTypes(extensions), [extensions])
  const annotations = useMemo(
    () => new Map((view?.annotations ?? []).map(annotation => [annotation.line, annotation])),
    [view],
  )
  const shownDecorations = useMemo(
    () => (view ? withPaneDecorations(view, paneDecorations) : NO_DECORATIONS),
    [view, paneDecorations],
  )
  const backgrounds = useMemo(
    () =>
      view && shownDecorations.length > 0
        ? lineBackgrounds(shownDecorations, types, kind)
        : new Map<number, string>(),
    [view, shownDecorations, types, kind],
  )
  const lineOverlays = useMemo(
    () =>
      view && shownDecorations.length > 0
        ? textOverlays(
            shownDecorations,
            types,
            kind,
            view.lineNumbers,
            view.lines.map(line => line.text.length),
          )
        : new Map<number, TextOverlay[]>(),
    [view, shownDecorations, types, kind],
  )
  const primary = primaryCaret
  const activeLine = view?.primary.position.line
  const layerStyle = { transform: `translateY(${rowTop(startRow, lineHeight, zones)}px)` }
  /** The room below each of the view's rows for zones. */
  const spacerAt = (index: number) => zonesBelowRow(zones, startRow + index)
  /** The zones near the view's rows, which are drawn. */
  const shownZones = zones.list.flatMap((shown, index) =>
    shown.key !== 'peek' &&
    (shown.content !== undefined || shown.className !== undefined) &&
    shown.row >= startRow - 1 &&
    shown.row < startRow + (view?.lines.length ?? 0)
      ? [{ zone: shown, top: (shown.row + 1) * lineHeight + zones.before[index]! }]
      : [],
  )
  /** Where a peek's arrow points, from the editor's left edge. */
  let arrowLeft: number | null = null
  if (peek && view) {
    const element = linesRef.current?.children[rowOf(view.lineNumbers, peek.anchor.line)]
    const scroller = scrollRef.current
    if (element && scroller) {
      arrowLeft =
        element.getBoundingClientRect().left -
        scroller.getBoundingClientRect().left +
        columnX(element, peek.anchor.column)
    }
  }
  const peekWidth = Math.max(0, scrollerWidth - (minimapVisible ? MINIMAP_WIDTH : 0))

  /** A click on a fold marker in the gutter folds or unfolds its region. */
  const onGutterMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (event.button !== 0) {
      return
    }
    event.preventDefault()
    focusInput()
    const change = (event.target as Element).closest<HTMLElement>('[data-change-line]')
    if (change?.dataset.changeLine !== undefined) {
      const line = Number(change.dataset.changeLine)
      if (hunk && line >= hunk.modified[0] - 1 && line <= hunk.modified[1]) {
        closeChange()
      } else {
        // The marker of removed lines below a line is the change after it.
        void showChange(change.dataset.kind === 'removedBelow' ? line + 1 : line, 'at')
      }
      return
    }
    if ((event.target as Element).closest('[data-lightbulb]')) {
      void showCodeActions(null)
      return
    }
    const marker = (event.target as Element).closest<HTMLElement>('[data-fold-line]')
    if (marker?.dataset.foldLine !== undefined) {
      dispatch({ type: 'toggleFoldAt', line: Number(marker.dataset.foldLine) })
    }
  }

  return (
    <div className="editor-view-frame" ref={frameRef}>
      {find && (
        <FindWidget
          state={find}
          result={view?.search ?? null}
          onQueryChange={onFindQueryChange}
          onReplacementChange={replacement => setFind({ ...find, replacement })}
          onToggleReplace={() => setFind({ ...find, showReplace: !find.showReplace })}
          onNext={findNext}
          onReplace={() =>
            dispatch({ type: 'replace', query: find.query, replacement: find.replacement })
          }
          onReplaceAll={() =>
            dispatch({ type: 'replaceAll', query: find.query, replacement: find.replacement })
          }
          onClose={closeFind}
        />
      )}
      <div className="editor-view" ref={scrollRef} onScroll={onScroll}>
        {/* The lines plus a view's height less one line, so the last line can
            scroll to the top (as in VS Code). Clicks in that space reach the
            content and go to the end of the last line. */}
        <div
          className="editor-view__sizer"
          style={{
            minHeight: `max(${rowTop(rowCount, lineHeight, zones)}px, 100% + ${rowTop(rowCount - 1, lineHeight, zones)}px)`,
          }}
        >
          <div
            className="editor-view__gutter"
            ref={gutterRef}
            style={{ width: `calc(${String(lineCount).length}ch + 32px + 20px)` }}
            aria-hidden="true"
            onMouseDown={onGutterMouseDown}
          >
            <div style={layerStyle}>
              {lineNumbers.map((line, index) => {
                const folded = foldedLines.has(line)
                const foldable = folded || foldableLines.has(line)
                const spacer = spacerAt(index)
                const extra = classOf(line)
                const mark = changeMarks.get(line)
                return (
                  <div
                    key={line}
                    className={
                      extra ? `editor-view__line-number ${extra}` : 'editor-view__line-number'
                    }
                    data-active={line === activeLine || undefined}
                    style={spacer ? { marginBottom: spacer } : undefined}
                  >
                    {line === lightbulb && (
                      <span
                        className="editor-view__lightbulb"
                        data-lightbulb=""
                        title="Show Code Actions"
                      >
                        <Lightbulb size={14} strokeWidth={1.75} />
                      </span>
                    )}
                    {line + 1}
                    {mark && (
                      <span
                        className="editor-view__change"
                        data-kind={mark}
                        data-change-line={line}
                        title="Show the change"
                      />
                    )}
                    <span
                      className="editor-view__fold"
                      data-fold-line={foldable ? line : undefined}
                      data-folded={folded || undefined}
                    >
                      {foldable &&
                        (folded ? (
                          <ChevronRight size={14} strokeWidth={1.75} />
                        ) : (
                          <ChevronDown size={14} strokeWidth={1.75} />
                        ))}
                    </span>
                  </div>
                )
              })}
            </div>
          </div>
          <div
            className="editor-view__content"
            // Room to scroll the ends of long lines out from under the minimap.
            style={
              minimapVisible
                ? { paddingRight: `calc(var(--space-8) + ${MINIMAP_WIDTH}px)` }
                : undefined
            }
            onMouseDown={onMouseDown}
            onMouseMove={onContentMouseMove}
            onMouseLeave={onContentMouseLeave}
            onContextMenu={onContextMenu}
          >
            <div className="editor-view__layer" style={layerStyle}>
              <div className="editor-view__selections" aria-hidden="true">
                {preview &&
                  lineNumbers.map(
                    (line, index) =>
                      line >= preview.start &&
                      line <= preview.end && (
                        <div
                          key={`p${line}`}
                          className="editor-view__range-highlight"
                          style={{
                            top:
                              rowTop(startRow + index, lineHeight, zones) -
                              rowTop(startRow, lineHeight, zones),
                          }}
                        />
                      ),
                  )}
                {overlay.matches.map((rect, index) => (
                  <div
                    key={`m${index}`}
                    className="editor-view__match"
                    data-current={rect.current || undefined}
                    style={{ top: rect.top, left: rect.left, width: rect.width }}
                  />
                ))}
                {overlay.decorations.map((rect, index) => {
                  if (rect.key.startsWith(MARK_KEY)) {
                    return (
                      <div
                        key={`x${index}`}
                        className={`editor-view__mark ${rect.key.slice(MARK_KEY.length)}`}
                        style={{ top: rect.top, left: rect.left, width: rect.width }}
                      />
                    )
                  }
                  const type = types.get(rect.key)
                  return (
                    type &&
                    !type.wholeLine && (
                      <div
                        key={`x${index}`}
                        className="editor-view__decoration"
                        data-underline={type.underline ?? undefined}
                        style={{
                          ...decorationRectStyle(type, kind),
                          top: rect.top,
                          left: rect.left,
                          width: rect.width,
                        }}
                      />
                    )
                  )
                })}
                {overlay.selections.map((rect, index) => (
                  <div
                    key={index}
                    className="editor-view__selection"
                    style={{ top: rect.top, left: rect.left, width: rect.width }}
                  />
                ))}
                {overlay.diagnostics.map((rect, index) => (
                  <div
                    key={`d${index}`}
                    className="editor-view__diagnostic"
                    data-severity={severityName(rect.severity)}
                    style={{ top: rect.top, left: rect.left, width: rect.width }}
                  />
                ))}
              </div>
              <div className="editor-view__lines" ref={linesRef}>
                {view?.lines.map((line, index) => {
                  const lineNumber = lineNumbers[index]!
                  return (
                    <Line
                      key={lineNumber}
                      line={line}
                      link={
                        link?.line === lineNumber && link.version === view.version ? link : null
                      }
                      overlays={lineOverlays.get(lineNumber) ?? NO_OVERLAYS}
                      folded={foldedLines.has(lineNumber)}
                      spacer={spacerAt(index)}
                      className={classOf(lineNumber)}
                      background={backgrounds.get(lineNumber)}
                      annotation={annotations.get(lineNumber)}
                    />
                  )
                })}
              </div>
              <div className="editor-view__carets" aria-hidden="true">
                {overlay.carets.map((caret, index) => (
                  <div
                    key={index}
                    className="editor-view__caret"
                    style={{ top: caret.top, left: caret.left }}
                  />
                ))}
              </div>
              <textarea
                ref={inputRef}
                className="editor-view__input"
                data-editor-input=""
                data-composing={composing || undefined}
                data-mirror={mirror.line >= 0 || undefined}
                style={
                  mirror.line >= 0
                    ? {
                        top:
                          rowTop(startRow + rowOf(lineNumbers, mirror.line), lineHeight, zones) -
                          rowTop(startRow, lineHeight, zones),
                        left: 0,
                        width: '100%',
                        height: mirror.value.split('\n').length * lineHeight,
                      }
                    : { top: primary?.top ?? 0, left: primary?.left ?? 0 }
                }
                aria-label={`Editor for ${title}`}
                aria-multiline="true"
                autoComplete="off"
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                wrap="off"
                onKeyDown={onKeyDown}
                onInput={onInput}
                onCompositionStart={onCompositionStart}
                onCompositionEnd={onCompositionEnd}
                onPaste={onPaste}
                onCopy={onClipboardEvent}
                onCut={onClipboardEvent}
              />
            </div>
          </div>
          {shownZones.map(({ zone: shown, top }) => (
            <div
              key={shown.key}
              className={
                shown.className ? `editor-view__zone ${shown.className}` : 'editor-view__zone'
              }
              style={{
                top,
                height: shown.lines * lineHeight,
                // Code lenses start where the text does.
                paddingLeft:
                  shown.className === LENS_ZONE ? (gutterRef.current?.offsetWidth ?? 0) : undefined,
              }}
            >
              {shown.content}
            </div>
          ))}
          {hunk && hunkRow !== null && (
            <div
              className="editor-view__zone editor-view__zone--change"
              style={{
                top: rowTop(hunkRow + 1, lineHeight, zones) - changePeekLines(hunk) * lineHeight,
                height: changePeekLines(hunk) * lineHeight,
              }}
            >
              <ChangePeek
                hunk={hunk}
                width={peekWidth}
                lineHeight={lineHeight}
                numberWidth={String(lineCount).length}
                onApply={() => void actOnChange('apply')}
                onRevert={() => void actOnChange('revert')}
                onMove={backward =>
                  void showChange(hunk.modified[0], backward ? 'previous' : 'next')
                }
                onClose={() => {
                  closeChange()
                  focusInput()
                }}
              />
            </div>
          )}
          {peek && zone && (
            <div
              className="editor-view__zone"
              style={{
                top: rowTop(zone.row + 1, lineHeight, zones) - zone.height,
                height: zone.height,
              }}
            >
              <div className="editor-view__zone-content" style={{ width: peekWidth }}>
                <PeekView
                  state={peek}
                  lineHeight={lineHeight}
                  width={peekWidth}
                  height={zone.height}
                  arrowLeft={arrowLeft}
                />
              </div>
            </div>
          )}
        </div>
      </div>
      {minimapVisible && view && (
        <Minimap
          documentId={documentId}
          view={documentView}
          version={view.version}
          rowCount={view.rowCount}
          refreshTick={refreshTick}
          scroller={scrollRef}
          lineHeight={lineHeight}
        />
      )}
      {hover && (
        <HoverCard hover={hover.hover} placement={hover.placement} onMouseLeave={hideHover} />
      )}
      {shownCompletions && completion && completionPlacement && (
        <CompletionList
          items={shownCompletions}
          selected={Math.min(completion.selected, shownCompletions.length - 1)}
          placement={completionPlacement}
          onPick={acceptCompletion}
        />
      )}
      {signature && signaturePlacement && (
        <SignatureCard
          help={signature.help}
          selected={signature.selected}
          placement={signaturePlacement}
          onStep={stepSignature}
        />
      )}
      {renaming && renamePlacement && (
        <RenameBox
          key={`${renaming.position.line}:${renaming.position.column}`}
          placeholder={renaming.target.placeholder}
          placement={renamePlacement}
          onSubmit={name => void submitRename(renaming, name)}
          onCancel={() => {
            setRenaming(null)
            focusInput()
          }}
        />
      )}
    </div>
  )
}

function earlier(a: Position, b: Position): Position {
  return a.line < b.line || (a.line === b.line && a.column <= b.column) ? a : b
}

/** A gutter marker: lines added or modified, or lines removed below or above a line. */
type ChangeMark = GutterChange['kind'] | 'removedAbove' | 'removedBelow'

const NO_OVERLAYS: readonly TextOverlay[] = []
/** Marks are measured as decorations with keys starting with this. */
const MARK_KEY = 'mark:'
const NO_LINES: readonly number[] = []

/** The input's content when nothing is mirrored: empty, at the caret. */
const EMPTY_MIRROR: InputMirror = {
  line: -1,
  value: '',
  selectionStart: 0,
  selectionEnd: 0,
  direction: 'forward',
}

function showMirror(input: HTMLTextAreaElement, mirror: InputMirror): void {
  if (input.value !== mirror.value) {
    input.value = mirror.value
  }
  if (
    input.selectionStart !== mirror.selectionStart ||
    input.selectionEnd !== mirror.selectionEnd ||
    input.selectionDirection !== mirror.direction
  ) {
    input.setSelectionRange(mirror.selectionStart, mirror.selectionEnd, mirror.direction)
  }
}

interface LineProps {
  line: DisplayLine
  className?: string
  link: Word | null
  /** Decoration styles on this line. */
  overlays: readonly TextOverlay[]
  /** Whether the region starting on this line is folded. */
  folded: boolean
  /** Room below the line for zones, in pixels. */
  spacer: number
  /** A whole-line decoration's background. */
  background?: string
  /** An extension's faint text after the line. */
  annotation?: LineAnnotation
}

const Line = memo(function Line({
  line,
  className,
  link,
  overlays,
  folded,
  spacer,
  background,
  annotation,
}: LineProps) {
  const all = link
    ? [...overlays, { from: link.from, to: link.to, className: LINK_CLASS }]
    : overlays
  const text =
    line.spans.length === 0 && all.length === 0
      ? line.text
      : highlighted(line.text, line.spans, all)
  const marker = (
    <>
      {folded && (
        <span className="editor-view__folded" title="Unfold">
          ⋯
        </span>
      )}
      {annotation && (
        <span className="editor-view__annotation" title={annotation.tooltip ?? undefined}>
          {annotation.text}
        </span>
      )}
    </>
  )
  const style =
    spacer || background
      ? { marginBottom: spacer || undefined, background: background ?? undefined }
      : undefined
  const lineClassName = className ? `editor-view__line ${className}` : 'editor-view__line'
  if (line.fullLength === null) {
    return (
      <div className={lineClassName} style={style}>
        {text}
        {marker}
      </div>
    )
  }
  // A very long line shortened for display; the document keeps all of it.
  return (
    <div className={lineClassName} style={style}>
      {text}
      <span
        className="editor-view__cut"
        title={`Line shortened for display (${line.fullLength.toLocaleString()} characters)`}
      >
        …
      </span>
      {marker}
    </div>
  )
})

function later(a: Position, b: Position): Position {
  return earlier(a, b) === a ? b : a
}

function earlierThan(a: Position, b: Position): boolean {
  return a.line < b.line || (a.line === b.line && a.column < b.column)
}

function samePosition(a: Position, b: Position): boolean {
  return a.line === b.line && a.column === b.column
}

function visibleRange(scroller: HTMLElement, lineHeight: number, zone: Zones): [number, number] {
  const first = rowAtY(scroller.scrollTop, lineHeight, zone)
  const last = rowAtY(scroller.scrollTop + scroller.clientHeight, lineHeight, zone)
  return [first, last + 2]
}

function viewportFor(scroller: HTMLElement | null, lineHeight: number, zone: Zones): Viewport {
  if (!scroller) {
    return { startRow: 0, endRow: OVERSCAN * 2 }
  }
  const [first, last] = visibleRange(scroller, lineHeight, zone)
  return { startRow: Math.max(0, first - OVERSCAN), endRow: last + OVERSCAN }
}

/** Whether `view` already contains every visible line. */
function covers(
  view: View | null,
  scroller: HTMLElement,
  lineHeight: number,
  zone: Zones,
): boolean {
  if (!view) {
    return false
  }
  const [first, last] = visibleRange(scroller, lineHeight, zone)
  return (
    view.startRow <= first && view.startRow + view.lines.length >= Math.min(last, view.rowCount)
  )
}

/** Scrolls to show the primary caret: just enough, or to the middle. */
function reveal(
  scroller: HTMLElement,
  top: number,
  lineHeight: number,
  left: number | undefined,
  /** The width the gutter and the minimap cover. */
  coveredWidth: number,
  mode: Reveal,
): void {
  const outside =
    top < scroller.scrollTop || top + lineHeight > scroller.scrollTop + scroller.clientHeight
  if (mode === 'center' || (mode === 'centerIfOutside' && outside)) {
    scroller.scrollTop = top - (scroller.clientHeight - lineHeight) / 2
  } else if (top < scroller.scrollTop) {
    scroller.scrollTop = top
  } else if (top + lineHeight > scroller.scrollTop + scroller.clientHeight) {
    scroller.scrollTop = top + lineHeight - scroller.clientHeight
  }
  if (left === undefined) {
    return
  }
  const margin = 2 * lineHeight
  const textWidth = scroller.clientWidth - coveredWidth
  if (left < scroller.scrollLeft) {
    scroller.scrollLeft = Math.max(0, left - margin)
  } else if (left > scroller.scrollLeft + textWidth - margin) {
    scroller.scrollLeft = left - textWidth + margin
  }
}
