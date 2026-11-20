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
import { noteMinimapDraw } from '../state/perf'
import { useStore } from '../state/store'
import { themeKind, themeRevision } from '../themes/theme'
import {
  dragScale,
  MINIMAP_CHAR_WIDTH,
  MINIMAP_LINE_HEIGHT,
  MINIMAP_WIDTH,
  minimapLayout,
  scrollTopFor,
  type EditorScroll,
} from './minimapGeometry'
import {
  headersOf,
  holds,
  MAX_REQUEST_ROWS,
  nextRequest,
  restRequest,
  runsAt,
  withRows,
  type MinimapRows,
} from './minimapRows'
import { marginsFor, neededFor, nextMotion, type Margins, type Motion } from './scrollAhead'
import './Minimap.css'

/** Rows kept beyond those shown, so scrolling a little needs no request. */
const OVERSCAN = 100
/** Least time between requests while the document changes (typing). */
const CHANGE_THROTTLE_MS = 100
/** How long after a request for an edited document the rest of it waits, so typing doesn't. */
const REST_AFTER_CHANGE_MS = 500
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

interface Size {
  clientHeight: number
  /** The editor's vertical scrollbar, which the minimap sits beside. */
  scrollbarWidth: number
}

/** Requests for rows: one at a time, throttled while the document changes. */
interface Requests {
  inFlight: boolean
  /** When the last request was sent. */
  last: number
  /** The request that last failed, not repeated until something changes. */
  failed: string
  timer: number
  disposed: boolean
}

/**
 * The document drawn small beside the editor, with a slider over the lines
 * the editor shows. Clicking centers a line; dragging scrolls. It is a
 * pointer shortcut only, so it is hidden from assistive technology.
 *
 * It draws as the editor's scroll events arrive, rather than through a
 * render, so it keeps up with the editor frame by frame. Rows are fetched
 * as the minimap scrolls to them (see `minimapRows.ts`); until they arrive,
 * the rows held for an older version of the document are drawn.
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
  const sliderRef = useRef<HTMLDivElement>(null)
  const size = useSize(scroller)
  const [dragging, setDragging] = useState(false)
  const kind = useStore(themeKind)
  const theme = useStore(themeRevision)
  /** The theme's colors by run kind, read once per theme (reading them forces a style pass). */
  const colorsRef = useRef<{ key: string; colors: string[] }>({ key: '', colors: [] })
  const { fontSize, minimapRegionHeaders, minimapMarkHeaders } = useStore(appearance)
  const rowsRef = useRef<MinimapRows | null>(null)
  const motionRef = useRef<Motion | null>(null)
  const requestsRef = useRef<Requests>({
    inFlight: false,
    last: 0,
    failed: '',
    timer: 0,
    disposed: false,
  })

  /** Draws the rows held at the editor's scroll position, and moves the slider. */
  const draw = () => {
    const editor = scroller.current
    const scroll = editor ? readScroll(editor) : null
    if (!scroll || scroll.clientHeight <= 0) {
      return
    }
    const height = scroll.clientHeight
    const layout = minimapLayout(scroll, lineHeight, height)
    const slider = sliderRef.current
    if (slider) {
      slider.style.top = `${layout.sliderTop}px`
      slider.style.height = `${layout.sliderHeight}px`
    }
    const canvas = canvasRef.current
    const context = canvas?.getContext('2d')
    if (!canvas || !context) {
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
    const last = Math.min(rowCount, layout.firstLine + layout.lineCount)
    const store = rowsRef.current
    if (!store || store.documentId !== documentId) {
      noteMinimapDraw({
        documentId,
        scrollTop: scroll.scrollTop,
        missingRows: Math.max(0, last - layout.firstLine),
      })
      return
    }
    const colorsKey = `${kind}:${theme}`
    if (colorsRef.current.key !== colorsKey) {
      colorsRef.current = { key: colorsKey, colors: tokenColors(canvas) }
    }
    const { colors } = colorsRef.current
    const rowHeight = MINIMAP_LINE_HEIGHT * ratio
    // Leave a gap between rows so lines read as lines.
    const blockHeight = Math.max(1, Math.round(rowHeight) - Math.round(ratio * 0.5))
    context.globalAlpha = 0.75
    let fill = ''
    let missingRows = 0
    for (let line = layout.firstLine; line < last; line++) {
      const runs = runsAt(store, line)
      if (!runs) {
        missingRows += 1
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
    noteMinimapDraw({ documentId, scrollTop: scroll.scrollTop, missingRows })
    // Section headers: a label over the lines, and a separator above for `MARK: -`.
    context.globalAlpha = 1
    const headers = headersOf(store)
      .filter(
        header =>
          header.row >= layout.firstLine - HEADER_FONT_SIZE &&
          header.row < last &&
          (header.region ? minimapRegionHeaders : minimapMarkHeaders),
      )
      .sort((a, b) => a.row - b.row)
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
  }

  /** Fetches the rows to show or keep ahead next, if any and none is being fetched. */
  const pump = () => {
    const requests = requestsRef.current
    const editor = scroller.current
    if (requests.inFlight || requests.disposed || !editor || editor.clientHeight <= 0) {
      return
    }
    const layout = minimapLayout(readScroll(editor), lineHeight, editor.clientHeight)
    const shown = [layout.firstLine, layout.firstLine + layout.lineCount] as const
    const { margins, needed } = minimapMargins(motionRef.current, layout.lineCount)
    const key = `${documentId}:${documentView}:${version}:${refreshTick}`
    const store = rowsRef.current
    let range: [number, number] | null
    if (store?.key === key) {
      range = nextRequest(store, shown, margins, needed, rowCount)
      // Then the rest of a small document, so dragging through it never
      // waits; after an edit, once typing pauses.
      const rest = range ? null : restRequest(store, shown, rowCount)
      const wait =
        rest && store.stale.size > 0 ? requests.last + REST_AFTER_CHANGE_MS - performance.now() : 0
      if (wait > 0) {
        requests.timer ||= window.setTimeout(() => {
          requests.timer = 0
          pumpRef.current()
        }, wait)
        return
      }
      range ??= rest
    } else {
      // The document changed. Its old rows are drawn until it's fetched
      // again, which happens at most every throttle period while they still
      // fill the minimap.
      const covered = store?.documentId === documentId && holds(store, shown[0], shown[1])
      const wait = covered ? requests.last + CHANGE_THROTTLE_MS - performance.now() : 0
      if (wait > 0) {
        requests.timer ||= window.setTimeout(() => {
          requests.timer = 0
          pumpRef.current()
        }, wait)
        return
      }
      range = nextRequest(null, shown, margins, needed, rowCount)
    }
    const attempt = `${key}:${range?.join(':')}`
    if (!range || requests.failed === attempt) {
      return
    }
    requests.inFlight = true
    requests.last = performance.now()
    fetchLines(documentId, documentView, range[0], range[1])
      .then(fetched => {
        if (!requests.disposed) {
          rowsRef.current = withRows(rowsRef.current, key, fetched)
          drawRef.current()
        }
      })
      .catch(() => {
        requests.failed = attempt
      })
      .finally(() => {
        requests.inFlight = false
        // Things may have changed while this request ran.
        pumpRef.current()
      })
  }

  // The listeners below call the latest of these.
  const drawRef = useRef(draw)
  drawRef.current = draw
  const pumpRef = useRef(pump)
  pumpRef.current = pump

  const lineHeightRef = useRef(lineHeight)
  lineHeightRef.current = lineHeight

  // Draw and fetch as the editor scrolls, in the same frame.
  useEffect(() => {
    const editor = scroller.current
    if (!editor) {
      return
    }
    const onScroll = () => {
      const height = editor.clientHeight
      const layout = minimapLayout(readScroll(editor), lineHeightRef.current, height)
      motionRef.current = nextMotion(
        motionRef.current,
        layout.offset,
        performance.now(),
        MINIMAP_LINE_HEIGHT,
      )
      drawRef.current()
      pumpRef.current()
    }
    editor.addEventListener('scroll', onScroll, { passive: true })
    // The content's height (lines, zones) moves the slider and the rows shown.
    const view = ownerWindow(editor)
    let frame = 0
    const observer = new (resizeObserverFor(editor))(() => {
      frame ||= view.requestAnimationFrame(() => {
        frame = 0
        drawRef.current()
        pumpRef.current()
      })
    })
    if (editor.firstElementChild) {
      observer.observe(editor.firstElementChild)
    }
    return () => {
      editor.removeEventListener('scroll', onScroll)
      observer.disconnect()
      view.cancelAnimationFrame(frame)
    }
  }, [scroller])

  useEffect(() => {
    const requests = requestsRef.current
    requests.disposed = false
    return () => {
      requests.disposed = true
      window.clearTimeout(requests.timer)
      requests.timer = 0
    }
  }, [])

  // A changed document, size, or font fetches; it and anything drawn differently draws.
  useEffect(() => {
    pumpRef.current()
  }, [documentId, documentView, version, refreshTick, rowCount, lineHeight, size, fetchLines])
  // Not on edits: the rows fetched for them draw (at most every
  // throttle period), and so does a change of the content's height.
  useLayoutEffect(() => {
    drawRef.current()
  }, [
    documentId,
    lineHeight,
    size,
    kind,
    theme,
    fontSize,
    minimapRegionHeaders,
    minimapMarkHeaders,
  ])

  const onMouseDown = (event: ReactMouseEvent<HTMLDivElement>) => {
    const editor = scroller.current
    if (event.button !== 0 || !editor) {
      return
    }
    // Keep focus (and the cursor) in the editor.
    event.preventDefault()
    const height = editor.clientHeight
    const layout = minimapLayout(readScroll(editor), lineHeight, height)
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
    const height = size?.clientHeight ?? 0
    const unit = event.deltaMode === 1 ? lineHeight : event.deltaMode === 2 ? height : 1
    scroller.current?.scrollBy({ top: event.deltaY * unit, left: event.deltaX * unit })
  }

  if (!size) {
    return null
  }
  const height = size.clientHeight
  return (
    <div
      className="minimap"
      aria-hidden="true"
      data-dragging={dragging || undefined}
      style={{ right: size.scrollbarWidth, width: MINIMAP_WIDTH, height }}
      onMouseDown={onMouseDown}
      onWheel={onWheel}
    >
      <canvas
        ref={canvasRef}
        className="minimap__canvas"
        style={{ width: MINIMAP_WIDTH, height }}
      />
      {/* Placed as the editor scrolls (see `draw`). */}
      <div className="minimap__slider" ref={sliderRef} />
    </div>
  )
}

/**
 * The rows to keep around those shown, as the minimap moves: some on each
 * side, more ahead while scrolling (see `scrollAhead.ts`), and at most a
 * request's worth in all.
 */
function minimapMargins(
  motion: Motion | null,
  shownRows: number,
): { margins: Margins; needed: Margins } {
  const ahead = marginsFor(motion, MINIMAP_LINE_HEIGHT, shownRows)
  const room = Math.max(0, MAX_REQUEST_ROWS - shownRows)
  let above = Math.max(ahead.above, OVERSCAN)
  let below = Math.max(ahead.below, OVERSCAN)
  if (above + below > room) {
    if (below >= above) {
      below = Math.max(0, room - above)
    } else {
      above = Math.max(0, room - below)
    }
  }
  // The overscan always, and most of the rows ahead while scrolling.
  const needed = neededFor(ahead)
  return {
    margins: { above, below },
    needed: {
      above: Math.min(Math.max(needed.above, OVERSCAN), above),
      below: Math.min(Math.max(needed.below, OVERSCAN), below),
    },
  }
}

function readScroll(element: HTMLElement): EditorScroll {
  return {
    scrollTop: element.scrollTop,
    scrollHeight: element.scrollHeight,
    clientHeight: element.clientHeight,
  }
}

/** The editor's height and scrollbar width, updated once per frame as they change. */
function useSize(scroller: RefObject<HTMLDivElement | null>): Size | null {
  const [size, setSize] = useState<Size | null>(null)
  useEffect(() => {
    const element = scroller.current
    if (!element) {
      return
    }
    let frame = 0
    const measure = () => {
      frame = 0
      const next = {
        clientHeight: element.clientHeight,
        scrollbarWidth: element.offsetWidth - element.clientWidth,
      }
      setSize(current =>
        current &&
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
    const observer = new (resizeObserverFor(element))(schedule)
    observer.observe(element)
    return () => {
      view.cancelAnimationFrame(frame)
      observer.disconnect()
    }
  }, [scroller])
  return size
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
