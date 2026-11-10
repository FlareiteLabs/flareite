import { ownerWindow, resizeObserverFor } from '../app/windows'
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type RefObject,
  type WheelEvent,
} from 'react'
import { getMinimap, TOKEN_KINDS, type MinimapView } from '../ipc/editor'
import { appearance } from '../state/appearance'
import { useStore } from '../state/store'
import { themeKind } from '../themes/theme'
import {
  dragScale,
  MINIMAP_CHAR_WIDTH,
  MINIMAP_LINE_HEIGHT,
  MINIMAP_WIDTH,
  minimapLayout,
  scrollTopFor,
  type EditorScroll,
} from './minimapGeometry'
import './Minimap.css'

/** Lines fetched beyond those shown, so scrolling a little needs no request. */
const OVERSCAN = 100
/** Least time between requests while the document changes (typing). */
const CHANGE_THROTTLE_MS = 100
/** Font size of section header labels, in CSS pixels (as in VS Code). */
const HEADER_FONT_SIZE = 9

interface MinimapProps {
  documentId: number
  /** The document's view the editor shows, whose folds the minimap follows. */
  view?: number
  /** The document version and row count of the editor's latest view. */
  version: number
  rowCount: number
  /** Bumped when the editor refreshes without an edit (e.g. new highlights). */
  refreshTick: number
  /** The editor's scroll container. */
  scroller: RefObject<HTMLDivElement | null>
  lineHeight: number
  fetchLines?: (id: number, view: number, startRow: number, endRow: number) => Promise<MinimapView>
}

interface Measured extends EditorScroll {
  /** The editor's vertical scrollbar, which the minimap sits beside. */
  scrollbarWidth: number
}

interface Fetched {
  view: MinimapView
  refreshTick: number
}

/**
 * The document drawn small beside the editor, with a slider over the lines
 * the editor shows. Clicking centers a line; dragging scrolls. It is a
 * pointer shortcut only, so it is hidden from assistive technology.
 */
export function Minimap({
  documentId,
  view: documentView = 0,
  version,
  rowCount,
  refreshTick,
  scroller,
  lineHeight,
  fetchLines = getMinimap,
}: MinimapProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const measured = useMeasured(scroller)
  const [fetched, setFetched] = useState<Fetched | null>(null)
  const [dragging, setDragging] = useState(false)
  const kind = useStore(themeKind)
  const { fontSize, minimapRegionHeaders, minimapMarkHeaders } = useStore(appearance)

  const height = measured?.clientHeight ?? 0
  const layout = measured ? minimapLayout(measured, lineHeight, height) : null
  const wantStart = layout?.firstLine ?? 0
  const wantEnd = Math.min(rowCount, wantStart + (layout?.lineCount ?? 0))

  useLineRequests(
    {
      documentId,
      view: documentView,
      version,
      refreshTick,
      wantStart,
      wantEnd,
      enabled: layout !== null,
    },
    fetched,
    setFetched,
    fetchLines,
  )

  // Draw. Colors are read from the theme each time, so theme and color
  // changes show on the next draw.
  useLayoutEffect(() => {
    const canvas = canvasRef.current
    const context = canvas?.getContext('2d')
    if (!canvas || !context || !layout) {
      return
    }
    const ratio = ownerWindow(canvas).devicePixelRatio || 1
    const width = Math.round(MINIMAP_WIDTH * ratio)
    const pixelHeight = Math.round(height * ratio)
    if (canvas.width !== width || canvas.height !== pixelHeight) {
      canvas.width = width
      canvas.height = pixelHeight
    }
    context.clearRect(0, 0, width, pixelHeight)
    const view = fetched?.view
    if (!view || view.documentId !== documentId) {
      return
    }
    const colors = tokenColors(canvas)
    const rowHeight = MINIMAP_LINE_HEIGHT * ratio
    // Leave a gap between rows so lines read as lines.
    const blockHeight = Math.max(1, Math.round(rowHeight) - Math.round(ratio * 0.5))
    context.globalAlpha = 0.75
    let fill = ''
    for (let line = layout.firstLine; line < layout.firstLine + layout.lineCount; line++) {
      const runs = view.lines[line - view.startRow]
      if (!runs) {
        continue
      }
      const y = Math.round((line * MINIMAP_LINE_HEIGHT - layout.offset) * ratio)
      for (let index = 0; index + 2 < runs.length; index += 3) {
        const start = runs[index] ?? 0
        const end = runs[index + 1] ?? 0
        const token = runs[index + 2] ?? 0
        const color = colors[token] ?? colors[0] ?? ''
        if (color !== fill) {
          context.fillStyle = color
          fill = color
        }
        const x = Math.round(start * MINIMAP_CHAR_WIDTH * ratio)
        context.fillRect(x, y, Math.round(end * MINIMAP_CHAR_WIDTH * ratio) - x, blockHeight)
      }
    }
    // Section headers: a label over the lines, and a separator above for `MARK: -`.
    context.globalAlpha = 1
    const headers = view.headers.filter(header =>
      header.region ? minimapRegionHeaders : minimapMarkHeaders,
    )
    if (headers.length > 0) {
      const style = ownerWindow(canvas).getComputedStyle(canvas)
      const background = style.getPropertyValue('--color-editor-background').trim()
      const labelHeight = Math.round((HEADER_FONT_SIZE + 2) * ratio)
      const pad = Math.round(2 * ratio)
      context.font = `600 ${Math.round(HEADER_FONT_SIZE * ratio)}px ${style.fontFamily || 'sans-serif'}`
      context.textBaseline = 'top'
      // Labels are taller than a row, so close headers stack instead of overlapping.
      let free = -Infinity
      for (const header of headers) {
        const y = Math.max(
          free,
          Math.round((header.row * MINIMAP_LINE_HEIGHT - layout.offset) * ratio),
        )
        free = y + (header.name ? labelHeight : 0) + (header.separator ? pad : 0)
        if (y > pixelHeight || y + labelHeight < 0) {
          continue
        }
        if (header.separator) {
          context.fillStyle = colors[0] ?? ''
          context.globalAlpha = 0.6
          context.fillRect(0, y, width, Math.max(1, Math.round(ratio)))
          context.globalAlpha = 1
        }
        if (header.name) {
          const top = header.separator ? y + pad : y
          context.fillStyle = background
          context.fillRect(0, top, width, labelHeight)
          context.fillStyle = colors[0] ?? ''
          context.fillText(header.name, pad, top + Math.round(ratio), width - 2 * pad)
        }
      }
    }
  }, [
    fetched,
    layout?.offset,
    layout?.firstLine,
    height,
    kind,
    fontSize,
    documentId,
    minimapRegionHeaders,
    minimapMarkHeaders,
  ])

  const onMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    const editor = scroller.current
    if (event.button !== 0 || !editor || !layout) {
      return
    }
    // Keep focus (and the cursor) in the editor.
    event.preventDefault()
    const y = event.clientY - event.currentTarget.getBoundingClientRect().top
    const onSlider = y >= layout.sliderTop && y < layout.sliderTop + layout.sliderHeight
    if (!onSlider) {
      editor.scrollTop = scrollTopFor(y, layout, readScroll(editor), lineHeight)
    }
    const [startY, startTop] = [event.clientY, editor.scrollTop]
    const scale = dragScale(readScroll(editor), lineHeight, height)
    setDragging(true)
    const onMove = (move: MouseEvent) => {
      editor.scrollTop = startTop + (move.clientY - startY) * scale
    }
    const view = ownerWindow(editor)
    const onUp = () => {
      view.removeEventListener('mousemove', onMove)
      view.removeEventListener('mouseup', onUp)
      setDragging(false)
    }
    view.addEventListener('mousemove', onMove)
    view.addEventListener('mouseup', onUp)
  }

  // The minimap lies over the editor, so pass the wheel on to it.
  const onWheel = (event: WheelEvent<HTMLDivElement>) => {
    const unit = event.deltaMode === 1 ? lineHeight : event.deltaMode === 2 ? height : 1
    scroller.current?.scrollBy({ top: event.deltaY * unit, left: event.deltaX * unit })
  }

  if (!measured || !layout) {
    return null
  }
  return (
    <div
      className="minimap"
      aria-hidden="true"
      data-dragging={dragging || undefined}
      style={{ right: measured.scrollbarWidth, width: MINIMAP_WIDTH, height }}
      onMouseDown={onMouseDown}
      onWheel={onWheel}
    >
      <canvas
        ref={canvasRef}
        className="minimap__canvas"
        style={{ width: MINIMAP_WIDTH, height }}
      />
      <div
        className="minimap__slider"
        style={{ top: layout.sliderTop, height: layout.sliderHeight }}
      />
    </div>
  )
}

function readScroll(element: HTMLElement): EditorScroll {
  return {
    scrollTop: element.scrollTop,
    scrollHeight: element.scrollHeight,
    clientHeight: element.clientHeight,
  }
}

/** The editor's scroll position and size, updated once per frame as they change. */
function useMeasured(scroller: RefObject<HTMLDivElement | null>): Measured | null {
  const [measured, setMeasured] = useState<Measured | null>(null)
  useEffect(() => {
    const element = scroller.current
    if (!element) {
      return
    }
    let frame = 0
    const measure = () => {
      frame = 0
      const next = {
        ...readScroll(element),
        scrollbarWidth: element.offsetWidth - element.clientWidth,
      }
      setMeasured(current =>
        current &&
        current.scrollTop === next.scrollTop &&
        current.scrollHeight === next.scrollHeight &&
        current.clientHeight === next.clientHeight &&
        current.scrollbarWidth === next.scrollbarWidth
          ? current
          : next,
      )
    }
    const view = ownerWindow(element)
    const schedule = () => {
      frame ||= view.requestAnimationFrame(measure)
    }
    measure()
    element.addEventListener('scroll', schedule, { passive: true })
    const observer = new (resizeObserverFor(element))(schedule)
    observer.observe(element)
    // The content's height changes with the line count.
    if (element.firstElementChild) {
      observer.observe(element.firstElementChild)
    }
    return () => {
      view.cancelAnimationFrame(frame)
      element.removeEventListener('scroll', schedule)
      observer.disconnect()
    }
  }, [scroller])
  return measured
}

interface Wanted {
  documentId: number
  view: number
  version: number
  refreshTick: number
  /** The lines the minimap shows. */
  wantStart: number
  wantEnd: number
  enabled: boolean
}

/**
 * Fetches the lines the minimap shows, with some to spare. Scrolling past
 * them fetches right away; edits and refreshes at most every
 * `CHANGE_THROTTLE_MS`, keeping the old lines until then. One request runs
 * at a time.
 */
function useLineRequests(
  wanted: Wanted,
  fetched: Fetched | null,
  setFetched: (fetched: Fetched) => void,
  fetchLines: NonNullable<MinimapProps['fetchLines']>,
): void {
  const inFlight = useRef(false)
  const lastRequest = useRef(0)
  /** The request that last failed, not repeated until something changes. */
  const failed = useRef('')
  const [retry, setRetry] = useState(0)
  const {
    documentId,
    view: documentView,
    version,
    refreshTick,
    wantStart,
    wantEnd,
    enabled,
  } = wanted

  useEffect(() => {
    const key = `${documentId}:${version}:${refreshTick}:${wantStart}:${wantEnd}`
    if (!enabled || inFlight.current || failed.current === key) {
      return
    }
    const view = fetched?.view
    const sameDocument = view?.documentId === documentId
    const current = sameDocument && view.version === version && fetched?.refreshTick === refreshTick
    const covers =
      sameDocument &&
      view.startRow <= wantStart &&
      view.startRow + view.lines.length >= Math.min(wantEnd, view.rowCount)
    if (current && covers) {
      return
    }
    // A changed document waits out the throttle if the old lines still cover the view.
    const wait = covers ? lastRequest.current + CHANGE_THROTTLE_MS - performance.now() : 0
    if (wait > 0) {
      const timer = setTimeout(() => setRetry(count => count + 1), wait)
      return () => clearTimeout(timer)
    }
    inFlight.current = true
    lastRequest.current = performance.now()
    const tick = refreshTick
    fetchLines(documentId, documentView, Math.max(0, wantStart - OVERSCAN), wantEnd + OVERSCAN)
      .then(view => setFetched({ view, refreshTick: tick }))
      .catch(() => {
        failed.current = key
      })
      .finally(() => {
        inFlight.current = false
        // Check again: things may have changed while this request ran.
        setRetry(count => count + 1)
      })
  }, [
    documentId,
    documentView,
    version,
    refreshTick,
    wantStart,
    wantEnd,
    enabled,
    fetched,
    retry,
    fetchLines,
  ])
}

/** Colors by run kind: plain text first, then each token kind's theme color. */
function tokenColors(element: Element): string[] {
  const style = ownerWindow(element).getComputedStyle(element)
  const text = style.getPropertyValue('--color-editor-foreground').trim()
  return [
    text,
    ...TOKEN_KINDS.map(kind => style.getPropertyValue(`--color-syntax-${kind}`).trim() || text),
  ]
}
