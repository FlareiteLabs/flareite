/**
 * A layout tab's content (API 1.15): the splits and panes its extension
 * set, with headers and buttons.
 *
 * - **Splits** lay their children out side by side (`row`) or stacked
 *   (`column`), sized by their shares; the user can drag the line between
 *   two to resize them, which lasts while the window does.
 * - **Editor panes** are `EditorView`s of their documents, each in a view of
 *   its own (see `state/layouts.ts`), so a file shown here can be edited
 *   here. They show what the extension sets besides the text: ranges
 *   decorated with its decoration types, zones (filler, a row of text or
 *   links, another document's lines), folds, and marks beside the scroll
 *   bar. Panes in the same scroll group scroll together.
 * - **Webview panes** are the extension's pages, as viewers' and previews'
 *   are; **file pages** show a file of the open folder as a web page, shown
 *   again as the tab's editors change it.
 *
 * Buttons and zones run the extension's commands, which learn the tab and
 * pane from `layouts::invoked`.
 */
import { ownerWindow, resizeObserverFor, windowIdOf } from '../app/windows'
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from 'react'
import * as ipc from '../ipc/editor'
import type { DecorationView, EditorView as View, PeekLines } from '../ipc/editor'
import type {
  DecorationType,
  LayoutButton,
  LayoutNode,
  LayoutSpec,
  PaneDecorations,
  PaneSpec,
  PaneZone,
} from '../ipc/extensions'
import { Icon, isActionIcon } from '../components/Icon'
import { decorationTypes, extensionsStore } from '../state/extensions'
import {
  findLayout,
  layoutResized,
  layoutsStore,
  loadLayout,
  paneFocused,
  paneView,
  parseLayoutItem,
  reloadsStore,
  retryLayout,
  runLayoutButton,
  setPaneDirty,
  type LayoutTabState,
} from '../state/layouts'
import { useStore } from '../state/store'
import { fileVersions } from '../state/viewers'
import { documentVersions, documentsStore } from '../state/documents'
import { FilePageFrame, WebviewFrame } from '../panels/WebviewFrame'
import { hideWebPage, moveWebPage, showWebPage, type WebPageBounds } from '../ipc/extensions'
import { layoutStore } from '../state/layout'
import { openOverlays } from '../state/overlays'
import { reportError } from '../state/notifications'
import { themeKind, type ThemeKind } from '../themes/theme'
import { editorsFor, setActiveOverride, type EditorController } from './activeEditor'
import { colorsFor, color } from './decorations'
import { EditorView, type ViewZone } from './EditorView'
import { highlighted } from './highlighted'
import { foldRegions, hiddenRanges, zoneRow } from './paneLayout'
import type { TextOverlay } from './segments'
import './LayoutView.css'

/** How long after the tab's editors change its file pages show again. */
const PAGE_DELAY = 300

/** Whether extensions have loaded: they're listed, and none is still starting. */
const extensionsLoaded = (extensions: readonly { state: string }[]) =>
  extensions.length > 0 && !extensions.some(extension => extension.state === 'starting')

/** The width each tab last told its extension about, to tell it when a breakpoint is crossed. */
const reportedWidths = new Map<string, number>()

/** Whether going from `before` to `after` crosses one of `breakpoints`. */
const crosses = (breakpoints: readonly number[], before: number, after: number) =>
  breakpoints.some(point => before < point !== after < point)

/** A layout tab: fills it in when needed, then shows it. */
export function LayoutPane({ item }: { item: string }) {
  const state = useStore(layoutsStore)[item]
  const frameRef = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(0)

  useEffect(() => {
    const frame = frameRef.current
    if (!frame) {
      return
    }
    const observer = new (resizeObserverFor(frame))(() => setWidth(frame.clientWidth))
    observer.observe(frame)
    setWidth(frame.clientWidth)
    return () => observer.disconnect()
  }, [])

  // Restored with the window, a tab waits for its extension to load.
  const extensions = useStore(extensionsStore)
  const tab = parseLayoutItem(item)
  const declared = useMemo(() => (tab ? findLayout(tab.layout) : null), [extensions, item])
  const spec = state?.view?.spec ?? null
  useEffect(() => {
    if (width > 0 && declared && !spec && !state?.error) {
      reportedWidths.set(item, width)
      void loadLayout(item, width)
    }
  }, [item, width > 0, declared, spec, state?.error])

  // Crossing a breakpoint runs the layout's command again.
  useEffect(() => {
    if (!spec || width === 0 || spec.breakpoints.length === 0) {
      return
    }
    const before = reportedWidths.get(item)
    if (before === undefined || crosses(spec.breakpoints, before, width)) {
      reportedWidths.set(item, width)
      layoutResized(item, width)
    }
  }, [item, spec, width])

  return (
    <div className="layout-view" ref={frameRef}>
      {state?.error ? (
        <div className="layout-view__status" role="alert">
          <p>{state.error}</p>
          <button type="button" className="layout-button" onClick={() => retryLayout(item, width)}>
            Try Again
          </button>
        </div>
      ) : spec && state ? (
        <LayoutBody item={item} spec={spec} state={state} />
      ) : (
        <div className="layout-view__status" role="status">
          {declared || !extensionsLoaded(extensions)
            ? 'Opening…'
            : `No extension that's turned on shows this tab (${tab?.layout ?? item}).`}
        </div>
      )}
    </div>
  )
}

/** Handles on a tab's editor panes, so panes in a scroll group follow each other. */
interface Scrolling {
  syncing: boolean
}

function LayoutBody({
  item,
  spec,
  state,
}: {
  item: string
  spec: LayoutSpec
  state: LayoutTabState
}) {
  const children = useMemo(() => {
    const byParent = new Map<string, LayoutNode[]>()
    for (const node of spec.nodes) {
      if (node.parent !== null) {
        byParent.set(node.parent, [...(byParent.get(node.parent) ?? []), node])
      }
    }
    return byParent
  }, [spec])
  const scrolling = useRef<Scrolling>({ syncing: false })

  // File pages show again as files in their folders are edited (in this tab
  // or any other) and as they change on disk.
  const [revision, setRevision] = useState(0)
  const timer = useRef(0)
  const bump = () => {
    window.clearTimeout(timer.current)
    timer.current = window.setTimeout(() => setRevision(current => current + 1), PAGE_DELAY)
  }
  useEffect(() => () => window.clearTimeout(timer.current), [])
  const pageFolders = useMemo(
    () =>
      spec.nodes.flatMap(node =>
        node.kind === 'pane' && node.content.type === 'filePage'
          ? [node.content.path.replace(/[/\\][^/\\]*$/, '')]
          : [],
      ),
    [spec],
  )
  const versions = useStore(fileVersions)
  const edits = useStore(documentVersions)
  const { documents: listed } = useStore(documentsStore)
  const watched = useMemo(() => {
    if (pageFolders.length === 0) {
      return ''
    }
    const inFolders = (path: string | null) =>
      path !== null && pageFolders.some(folder => path.startsWith(folder))
    // Documents with tabs of their own, and this tab's (hidden) ones.
    const paths = new Map<number, string | null>(
      listed.map(document => [document.id, document.path]),
    )
    for (const document of Object.values(state.documents)) {
      paths.set(document.id, document.path)
    }
    const edited = [...paths]
      .filter(([id, path]) => inFolders(path) && edits[id] !== undefined)
      .map(([id]) => `${id}@${edits[id]}`)
    const changed = Object.entries(versions)
      .filter(([path]) => inFolders(path))
      .map(([path, version]) => `${path}:${version}`)
    return [...edited, ...changed].join('|')
  }, [versions, edits, listed, state.documents, pageFolders])
  const firstWatched = useRef(watched)
  useEffect(() => {
    if (watched !== firstWatched.current) {
      bump()
    }
  }, [watched])

  const context: PaneContext = {
    item,
    spec,
    state,
    children,
    scrolling: scrolling.current,
    revision,
  }
  const root = spec.nodes[0]
  return root ? <NodeView node={root} context={context} /> : null
}

interface PaneContext {
  item: string
  spec: LayoutSpec
  state: LayoutTabState
  children: ReadonlyMap<string, LayoutNode[]>
  scrolling: Scrolling
  /** Bumped when file pages should show again. */
  revision: number
}

function NodeView({ node, context }: { node: LayoutNode; context: PaneContext }): ReactNode {
  if (node.kind === 'split') {
    return (
      <SplitView
        id={node.id}
        direction={node.direction}
        nodes={context.children.get(node.id) ?? []}
        context={context}
      />
    )
  }
  return <PaneView id={node.id} pane={node} context={context} />
}

/** Sizes the user dragged, by tab and split, while the window lasts. */
const draggedSizes = new Map<string, { ids: string; sizes: number[] }>()

function SplitView({
  id,
  direction,
  nodes,
  context,
}: {
  id: string
  direction: 'row' | 'column'
  nodes: readonly LayoutNode[]
  context: PaneContext
}) {
  const key = `${context.item}\u0000${id}`
  const ids = nodes.map(node => node.id).join('\u0000')
  const declared = useMemo(() => nodes.map(node => node.size), [nodes])
  const [sizes, setSizes] = useState(() => {
    const dragged = draggedSizes.get(key)
    return dragged?.ids === ids ? dragged.sizes : declared
  })
  // A new set of children starts from their declared sizes.
  const shown = useRef(ids)
  useEffect(() => {
    if (shown.current !== ids) {
      shown.current = ids
      setSizes(declared)
    }
  }, [ids, declared])
  const ref = useRef<HTMLDivElement>(null)
  const resize = (next: number[]) => {
    setSizes(next)
    draggedSizes.set(key, { ids, sizes: next })
  }
  const total = sizes.reduce((sum, size) => sum + size, 0) || 1
  return (
    <div className="layout-split" data-direction={direction} ref={ref}>
      {nodes.map((node, index) => (
        <SplitChild
          key={node.id}
          share={(sizes[index] ?? 1) / total}
          divider={
            index > 0 ? (
              <Divider
                direction={direction}
                container={ref}
                sizes={sizes}
                index={index}
                onResize={resize}
              />
            ) : null
          }
        >
          <NodeView node={node} context={context} />
        </SplitChild>
      ))}
    </div>
  )
}

function SplitChild({
  share,
  divider,
  children,
}: {
  share: number
  divider: ReactNode
  children: ReactNode
}) {
  return (
    <>
      {divider}
      <div className="layout-split__child" style={{ flexGrow: share, flexBasis: 0 }}>
        {children}
      </div>
    </>
  )
}

/** Smallest share a pane can be dragged to, of its split. */
const MIN_SHARE = 0.05

/** The line between two children of a split, which resizes them. */
function Divider({
  direction,
  container,
  sizes,
  index,
  onResize,
}: {
  direction: 'row' | 'column'
  container: { current: HTMLDivElement | null }
  sizes: readonly number[]
  index: number
  onResize: (sizes: number[]) => void
}) {
  const drag = useRef<{ start: number; sizes: readonly number[] } | null>(null)
  const total = sizes.reduce((sum, size) => sum + size, 0) || 1
  const length = () => {
    const rect = container.current?.getBoundingClientRect()
    return (direction === 'row' ? rect?.width : rect?.height) || 1
  }
  /** Moves the line by `delta`, a share of the split. */
  const move = (from: readonly number[], delta: number) => {
    const before = from[index - 1]!
    const after = from[index]!
    const pair = before + after
    const min = MIN_SHARE * total
    const next = Math.min(Math.max(before + delta * total, min), pair - min)
    const result = [...from]
    result[index - 1] = next
    result[index] = pair - next
    onResize(result)
  }
  const position = (event: ReactPointerEvent) =>
    direction === 'row' ? event.clientX : event.clientY
  const onKeyDown = (event: ReactKeyboardEvent) => {
    const back = direction === 'row' ? 'ArrowLeft' : 'ArrowUp'
    const forward = direction === 'row' ? 'ArrowRight' : 'ArrowDown'
    const step = (event.shiftKey ? 64 : 16) / length()
    if (event.key === back || event.key === forward) {
      event.preventDefault()
      move(sizes, event.key === back ? -step : step)
    }
  }
  const share = Math.round(((sizes[index - 1] ?? 0) / total) * 100)
  return (
    <div
      className="layout-split__divider"
      role="separator"
      aria-orientation={direction === 'row' ? 'vertical' : 'horizontal'}
      aria-valuenow={share}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label="Resize panes"
      tabIndex={0}
      onKeyDown={onKeyDown}
      onPointerDown={event => {
        if (event.button !== 0) {
          return
        }
        event.preventDefault()
        event.currentTarget.setPointerCapture(event.pointerId)
        drag.current = { start: position(event), sizes }
      }}
      onPointerMove={event => {
        if (drag.current) {
          move(drag.current.sizes, (position(event) - drag.current.start) / length())
        }
      }}
      onPointerUp={() => {
        drag.current = null
      }}
      onPointerCancel={() => {
        drag.current = null
      }}
    />
  )
}

function PaneView({ id, pane, context }: { id: string; pane: PaneSpec; context: PaneContext }) {
  const { content, header } = pane
  const title = header?.title ?? (content.type === 'editor' ? content.uri : context.spec.title)
  return (
    <section className="layout-pane" data-pane={id} aria-label={title}>
      {header && (
        <div className="layout-pane__header">
          <span className="layout-pane__title">{header.title}</span>
          {header.description && (
            <span className="layout-pane__description" title={header.description}>
              {header.description}
            </span>
          )}
          {header.buttons.length > 0 && (
            <span className="layout-pane__buttons">
              {header.buttons.map(button => (
                <ButtonView
                  key={button.id}
                  button={button}
                  className="layout-button"
                  onClick={() =>
                    runLayoutButton(context.item, id, button.command, button.argument, button.title)
                  }
                />
              ))}
            </span>
          )}
        </div>
      )}
      <div className="layout-pane__body">
        {content.type === 'editor' ? (
          <EditorPane id={id} pane={pane} title={title} context={context} />
        ) : content.type === 'webview' ? (
          <WebviewFrame id={content.id} title={title} />
        ) : content.type === 'webPage' ? (
          <WebPagePane id={content.id} url={content.url} title={title} />
        ) : (
          <FilePageFrame id={content.id} title={title} revision={context.revision} />
        )}
      </div>
    </section>
  )
}

/** A button of a layout: an icon, or its title as text. */
export function ButtonView({
  button,
  className,
  onClick,
}: {
  button: LayoutButton
  className: string
  onClick: () => void
}) {
  const icon = button.icon !== null && isActionIcon(button.icon) ? button.icon : null
  return (
    <button
      type="button"
      className={className}
      data-primary={button.primary || undefined}
      data-text={icon === null || undefined}
      title={button.title}
      aria-label={button.title}
      aria-pressed={button.pressed ?? undefined}
      disabled={button.disabled}
      onClick={onClick}
    >
      {icon ? <Icon name={icon} size={16} /> : button.title}
    </button>
  )
}

const NO_DECORATIONS: PaneDecorations = { decorations: [], zones: [], folds: [], ruler: [] }

function EditorPane({
  id,
  pane,
  title,
  context,
}: {
  id: string
  pane: PaneSpec
  title: string
  context: PaneContext
}) {
  const { item, state } = context
  const content = pane.content.type === 'editor' ? pane.content : null
  const document = content ? state.documents[content.uri] : undefined
  const view = paneView(item, id)
  const decorations = state.view?.decorations[id] ?? NO_DECORATIONS
  const extensions = useStore(extensionsStore)
  const types = useMemo(() => decorationTypes(extensions), [extensions])
  const kind = useStore(themeKind)
  const [lineCount, setLineCount] = useState(0)

  const controller = (): EditorController | undefined =>
    document && editorsFor(document.id).find(candidate => candidate.view === view)

  // The pane in use is the active editor (Save and the like act on it). Its
  // input can get focus before its editor has registered (when the tab is
  // shown), so it's claimed here too, once the editor has.
  const frameRef = useRef<HTMLDivElement>(null)
  const claim = () => {
    const editor = controller()
    if (editor) {
      setActiveOverride(editor)
    }
    paneFocused(item, id)
  }
  useEffect(() => {
    const frame = frameRef.current
    if (document && frame?.contains(frame.ownerDocument.activeElement)) {
      claim()
    }
  }, [document?.id])

  const hidden = useMemo(() => hiddenRanges(decorations.folds), [decorations.folds])

  // The extension's folds, in this pane's view.
  useEffect(() => {
    if (!document) {
      return
    }
    let tries = 0
    let frame = 0
    const view = ownerWindow(frameRef.current)
    const apply = () => {
      const editor = controller()
      if (editor) {
        editor.dispatch({ type: 'setFolds', regions: foldRegions(hidden) }, 'none')
      } else if (tries++ < 10) {
        frame = view.requestAnimationFrame(apply)
      }
    }
    apply()
    return () => view.cancelAnimationFrame(frame)
  }, [hidden, document?.id])

  const zones = useMemo(
    () =>
      decorations.zones.map((zone, index): ViewZone => {
        const background = zone.decoration
          ? backgroundOf(types.get(zone.decoration), kind)
          : undefined
        return {
          key: `${zone.type}:${zone.line}:${index}`,
          row: zoneRow(hidden, zone.line),
          lines:
            zone.type === 'filler'
              ? zone.rows
              : zone.type === 'lines'
                ? zone.lines.end - zone.lines.start
                : 1,
          className: `layout-zone layout-zone--${zone.type}`,
          content: (
            <ZoneContent
              zone={zone}
              item={item}
              pane={id}
              documents={state.documents}
              types={types}
              kind={kind}
              background={background}
              numberWidth={String(Math.max(lineCount, 1)).length}
            />
          ),
        }
      }),
    [decorations.zones, hidden, types, kind, state.documents, item, id, lineCount],
  )

  const ranges = useMemo(
    (): DecorationView[] =>
      decorations.decorations.map(({ key, start, end }) => ({ key, start, end })),
    [decorations.decorations],
  )

  if (!content) {
    return null
  }
  if (!document) {
    return (
      <div className="layout-view__status" role="status">
        Opening…
      </div>
    )
  }

  const onView = (next: View) => {
    setPaneDirty(item, id, next.dirty)
    setLineCount(next.lineCount)
  }

  // Panes in the same group scroll together.
  const onScroll = (scroller: HTMLElement) => {
    const group = pane.scrollGroup
    if (group === null || context.scrolling.syncing) {
      return
    }
    context.scrolling.syncing = true
    for (const node of context.spec.nodes) {
      if (
        node.kind === 'pane' &&
        node.id !== id &&
        node.scrollGroup === group &&
        node.content.type === 'editor'
      ) {
        const other = state.documents[node.content.uri]
        const otherView = paneView(item, node.id)
        other &&
          editorsFor(other.id)
            .find(candidate => candidate.view === otherView)
            ?.restoreScroll({ top: scroller.scrollTop, left: scroller.scrollLeft })
      }
    }
    context.scrolling.syncing = false
  }

  return (
    <div className="layout-pane__editor" ref={frameRef} onFocus={claim}>
      <EditorView
        documentId={document.id}
        view={view}
        title={title}
        zones={zones}
        decorations={ranges}
        folding={content.folding}
        minimap={content.minimap}
        quickDiff={content.quickDiff}
        onView={onView}
        onScroll={onScroll}
      />
      {decorations.ruler.length > 0 && lineCount > 0 && (
        <div className="layout-pane__ruler" aria-hidden="true">
          {decorations.ruler.map((mark, index) => {
            const type = types.get(mark.decoration)
            const colors = type && colorsFor(type, kind)
            const fill = colors && (color(colors.border) ?? color(colors.background))
            const top = (mark.lines.start / lineCount) * 100
            const height = (Math.max(mark.lines.end - mark.lines.start, 1) / lineCount) * 100
            return (
              <div
                key={index}
                className="layout-pane__ruler-mark"
                style={{ top: `${top}%`, height: `max(${height}%, 2px)`, background: fill }}
              />
            )
          })}
        </div>
      )}
    </div>
  )
}

/** A decoration type's background, for a zone. */
function backgroundOf(type: DecorationType | undefined, kind: ThemeKind): string | undefined {
  return type ? color(colorsFor(type, kind).background) : undefined
}

function ZoneContent({
  zone,
  item,
  pane,
  documents,
  types,
  kind,
  background,
  numberWidth,
}: {
  zone: PaneZone
  item: string
  pane: string
  documents: LayoutTabState['documents']
  types: ReadonlyMap<string, DecorationType>
  kind: ThemeKind
  background: string | undefined
  /** Digits in the editor's line numbers, to line numbers up with them. */
  numberWidth: number
}): ReactNode {
  const style: CSSProperties | undefined = background ? { background } : undefined
  // Clicks in a zone don't move the editor's cursor.
  const stop = (event: { stopPropagation(): void }) => event.stopPropagation()
  switch (zone.type) {
    case 'filler':
      return <div className="layout-zone__filler" style={style} />
    case 'text':
      return zone.command ? (
        <button
          type="button"
          className="layout-zone__text"
          data-clickable
          style={style}
          title={zone.tooltip ?? undefined}
          onMouseDown={stop}
          onClick={() => runLayoutButton(item, pane, zone.command!, zone.argument, zone.text)}
        >
          {zone.text}
        </button>
      ) : (
        <div className="layout-zone__text" style={style} title={zone.tooltip ?? undefined}>
          {zone.text}
        </div>
      )
    case 'links':
      return (
        <div className="layout-zone__links" style={style} onMouseDown={stop}>
          {zone.label && <span className="layout-zone__label">{zone.label}</span>}
          {zone.links.map((link, index) => (
            <span key={index}>
              {(index > 0 || zone.label) && (
                <span className="layout-zone__separator" aria-hidden="true">
                  |
                </span>
              )}
              <button
                type="button"
                className="layout-zone__link"
                title={link.tooltip ?? undefined}
                onClick={() => runLayoutButton(item, pane, link.command, link.argument, link.title)}
              >
                {link.title}
              </button>
            </span>
          ))}
        </div>
      )
    case 'lines':
      return (
        <LinesZone
          zone={zone}
          document={documents[zone.uri]?.id ?? null}
          types={types}
          kind={kind}
          background={background}
          numberWidth={numberWidth}
        />
      )
  }
}

/** Lines of another of the tab's documents, highlighted, in a zone. */
function LinesZone({
  zone,
  document,
  types,
  kind,
  background,
  numberWidth,
}: {
  zone: Extract<PaneZone, { type: 'lines' }>
  document: number | null
  types: ReadonlyMap<string, DecorationType>
  kind: ThemeKind
  background: string | undefined
  numberWidth: number
}) {
  const { start, end } = zone.lines
  const reloads = useStore(reloadsStore)[document ?? -1] ?? 0
  const [lines, setLines] = useState<PeekLines | null>(null)
  useEffect(() => {
    if (document === null) {
      return
    }
    let cancelled = false
    ipc
      .documentLines(document, start, end)
      .then(fetched => !cancelled && setLines(fetched))
      .catch(() => {
        // Closed meanwhile; the zone goes with the next decorations.
      })
    return () => {
      cancelled = true
    }
  }, [document, start, end, reloads])
  return (
    <div className="layout-zone__lines" style={background ? { background } : undefined}>
      {Array.from({ length: end - start }, (_, offset) => {
        const line = start + offset
        const shown = lines?.lines[line - lines.start]
        const overlays: TextOverlay[] = shown
          ? zone.decorations.flatMap(decorated => {
              if (decorated.start.line > line || decorated.end.line < line) {
                return []
              }
              const type = types.get(decorated.key)
              const fill = type && color(colorsFor(type, kind).background)
              const from = decorated.start.line === line ? decorated.start.column : 0
              const to = decorated.end.line === line ? decorated.end.column : shown.text.length
              return fill && to > from ? [{ from, to, style: { background: fill } }] : []
            })
          : []
        return (
          <div key={line} className="layout-zone__line">
            {zone.numbers && (
              <span
                className="layout-zone__number"
                style={{
                  width: `calc(${numberWidth}ch + 32px + 20px)`,
                  background: background ?? 'var(--color-editor-background)',
                }}
              >
                {line + 1}
              </span>
            )}
            <span className="layout-zone__line-text">
              {shown ? highlighted(shown.text, shown.spans, overlays) : ''}
            </span>
          </div>
        )
      })}
    </div>
  )
}

/**
 * A page from the web in a browser view of its own (API 1.15). The view is
 * native, laid over this placeholder: the workbench sends it the
 * placeholder's place as it moves (checked each frame, since moving
 * doesn't resize it), and hides it while an overlay (the palette, a menu,
 * a dialog) is open, which it would cover, and while the tab is hidden.
 */
function WebPagePane({ id, url, title }: { id: string; url: string; title: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const overlays = useStore(openOverlays)
  const { zoom } = useStore(layoutStore)

  useEffect(() => {
    const element = ref.current
    if (!element) {
      return
    }
    if (overlays > 0) {
      hideWebPage(id).catch(() => {})
      return
    }
    let shown = false
    let last = ''
    let frame = 0
    // The view goes in the window the pane is in; its frames are that window's.
    const view = ownerWindow(element)
    const windowId = windowIdOf(element) ?? null
    const place = () => {
      const rect = element.getBoundingClientRect()
      // CSS pixels at the workbench's zoom are logical pixels times it.
      const bounds: WebPageBounds = {
        x: rect.left * zoom,
        y: rect.top * zoom,
        width: rect.width * zoom,
        height: rect.height * zoom,
      }
      const key = `${bounds.x},${bounds.y},${bounds.width},${bounds.height}`
      if (rect.width > 0 && rect.height > 0 && key !== last) {
        last = key
        const done = shown ? moveWebPage(id, bounds) : showWebPage(id, url, bounds, windowId)
        shown = true
        done.catch((error: unknown) => reportError('Showing the web page', error))
      }
      frame = view.requestAnimationFrame(place)
    }
    place()
    return () => {
      view.cancelAnimationFrame(frame)
      hideWebPage(id).catch(() => {})
    }
  }, [id, url, overlays > 0, zoom])

  return <div ref={ref} className="layout-pane__web-page" role="document" aria-label={title} />
}
