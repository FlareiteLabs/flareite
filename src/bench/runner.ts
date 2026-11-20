/**
 * End-to-end benchmarks that run inside the real app (IPC, rendering, and
 * painting included). Started with `--bench <name>` via `scripts/bench.mjs`;
 * results are reported to the backend, which writes them out and exits.
 */
import { activeEditor, type EditorController } from '../editor/activeEditor'
import { newDocument, openDocument, type DocumentSummary } from '../ipc/editor'
import {
  benchConfig,
  benchGenerate,
  benchReport,
  getPerfStats,
  type BenchConfig,
} from '../ipc/perf'
import { activate, documentsStore } from '../state/documents'
import { editorStatus } from '../state/editorStatus'
import {
  lastMinimapDrawn,
  nextPaint,
  perfSnapshot,
  resetPerf,
  summarize,
  type SeriesSummary,
} from '../state/perf'

type Benchmark = (config: BenchConfig) => Promise<Record<string, unknown>>

const now = () => performance.now()
const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))

/** Adds a document as the active tab and waits for its editor to paint. */
async function addDocument(summary: DocumentSummary): Promise<EditorController> {
  const painted = nextPaint(info => info.documentId === summary.id)
  documentsStore.update(state => ({
    ...state,
    documents: state.documents.some(d => d.id === summary.id)
      ? state.documents
      : [...state.documents, summary],
  }))
  activate(summary.id)
  await painted
  const editor = activeEditor.get()
  if (editor?.documentId !== summary.id) {
    throw new Error('the editor did not open')
  }
  return editor
}

/** Types `count` characters one at a time, timing each until it is painted. */
async function typeAndTime(editor: EditorController, count: number): Promise<number[]> {
  const samples: number[] = []
  let version = -1
  for (let i = 0; i < count; i++) {
    const painted = nextPaint(
      info => info.documentId === editor.documentId && info.version > version,
    )
    const started = now()
    editor.dispatch({ type: 'insert', text: i % 10 === 9 ? ' ' : 'x' })
    version = (await painted).version
    samples.push(now() - started)
  }
  return samples
}

/**
 * Waits (up to a minute) until the active document's language server, if
 * any, has started and finished its initial work, so typing is measured in
 * the steady state rather than while the server indexes.
 */
async function languageServerSettled(): Promise<void> {
  const deadline = now() + 60_000
  let readySince: number | null = null
  while (now() < deadline) {
    const state = editorStatus.get()?.language?.server?.state
    if (state === undefined || state === 'failed') {
      return
    }
    readySince = state === 'ready' ? (readySince ?? now()) : null
    if (readySince !== null && now() - readySince > 2_000) {
      return
    }
    await delay(100)
  }
}

/** Speeds of the fast-scrolling benchmark, in lines per frame (×75 for lines per second here). */
const FLING_SPEEDS = [10, 40, 100]
/** Frames in each direction at each speed. */
const FLING_FRAMES = 120

interface FlingResult {
  linesPerFrame: number
  frames: number
  /** Frames painted with part of the editor's visible area holding no lines. */
  editorBlankFrames: number
  /** The share of the visible area, over all frames, holding no lines. */
  editorBlankShare: number
  /** Rows drawn from the copy of the lines away from their line's place, over all frames. */
  misplacedRows: number
  /** Frames where the minimap's last drawing left visible rows blank. */
  minimapBlankFrames: number
  /** How far (in editor lines) the minimap's drawing trailed the editor. */
  minimapLagLines: SeriesSummary
  frameInterval: SeriesSummary
}

/** The editor's scroll container, found by its input. */
function scrollerOf(documentId: number): HTMLElement {
  const scroller = document
    .querySelector(`textarea[data-document="${documentId}"]`)
    ?.closest<HTMLElement>('.editor-view')
  if (!scroller) {
    throw new Error('the editor is not in the page')
  }
  return scroller
}

/** What one frame paints, as `measureFrame` finds it. */
interface FrameCheck {
  /** Pixels of the editor's visible text area with no line. */
  blank: number
  height: number
  /** Whether the minimap's last drawing left rows blank. */
  minimapBlank: boolean
  /** How far (in lines) the minimap's last drawing trailed the editor. */
  minimapLag: number | null
  /** Rows drawn from the copy of the lines away from where their line belongs. */
  misplaced: number
}

/**
 * Checks what the editor paints this frame: how much of its visible area
 * (up to the document's end) holds no line, among the lines rendered and
 * any drawn ahead of them, and how complete and current the minimap is.
 */
function measureFrame(
  scroller: HTMLElement,
  documentId: number,
  lineCount: number,
  lineHeight: number,
): FrameCheck {
  const box = scroller.getBoundingClientRect()
  const top = box.top
  const bottom = Math.min(
    box.top + scroller.clientHeight,
    box.top - scroller.scrollTop + lineCount * lineHeight,
  )
  const spans: [number, number][] = []
  const lines = scroller.querySelector('.editor-view__lines')
  const first = lines?.firstElementChild?.getBoundingClientRect()
  const end = lines?.lastElementChild?.getBoundingClientRect()
  if (first && end) {
    spans.push([first.top, end.bottom])
  }
  // Rows drawn from the copy of the lines must sit where their lines do:
  // at their line's top (the benchmarks have no folds, wraps, or zones),
  // and where the view's lines start sideways.
  const contentTop = box.top - scroller.scrollTop
  const left = lines?.getBoundingClientRect().left
  let misplaced = 0
  for (const line of scroller.querySelectorAll<HTMLElement>('.editor-view__fill > *')) {
    const rect = line.getBoundingClientRect()
    spans.push([rect.top, rect.bottom])
    const expected = contentTop + Number(line.dataset.line) * lineHeight
    const sideways =
      line.classList.contains('editor-view__line') && left !== undefined
        ? Math.abs(rect.left - left)
        : 0
    if (Math.abs(rect.top - expected) > 0.5 || sideways > 0.5) {
      misplaced += 1
    }
  }
  spans.sort((a, b) => a[0] - b[0])
  let covered = 0
  let reached = top
  for (const [from, to] of spans) {
    const start = Math.max(from, reached)
    const stop = Math.min(to, bottom)
    if (stop > start) {
      covered += stop - start
      reached = stop
    }
  }
  const height = Math.max(0, bottom - top)
  const drawn = lastMinimapDrawn()
  const current = drawn?.documentId === documentId ? drawn : null
  return {
    blank: Math.max(0, height - covered),
    height,
    minimapBlank: (current?.missingRows ?? 0) > 0,
    minimapLag: current ? Math.abs(scroller.scrollTop - current.scrollTop) / lineHeight : null,
    misplaced,
  }
}

/** Frame checks and frame times, summed up. */
function scrollResult(
  linesPerFrame: number,
  checks: FrameCheck[],
  intervals: number[],
): FlingResult {
  const blank = checks.filter(check => check.blank > 0.5)
  return {
    linesPerFrame,
    frames: checks.length,
    editorBlankFrames: blank.length,
    editorBlankShare:
      blank.reduce((sum, check) => sum + check.blank / Math.max(1, check.height), 0) /
      Math.max(1, checks.length),
    minimapBlankFrames: checks.filter(check => check.minimapBlank).length,
    misplacedRows: checks.reduce((sum, check) => sum + check.misplaced, 0),
    minimapLagLines: summarize(
      'minimapLagLines',
      'count',
      checks.flatMap(check => (check.minimapLag === null ? [] : [check.minimapLag])),
    ),
    frameInterval: summarize('frameInterval', 'ms', intervals),
  }
}

/** The height of the editor's lines. */
function lineHeightOf(scroller: HTMLElement): number {
  return (
    scroller.querySelector('.editor-view__lines')?.firstElementChild?.getBoundingClientRect()
      .height || 20
  )
}

/**
 * Scrolls by `linesPerFrame` every frame for `frames` frames, down then
 * back up, as a wheel or trackpad fling does, and checks each frame.
 */
function fling(
  documentId: number,
  lineCount: number,
  linesPerFrame: number,
  frames: number,
): Promise<FlingResult> {
  const scroller = scrollerOf(documentId)
  const lineHeight = lineHeightOf(scroller)
  const checks: FrameCheck[] = []
  const intervals: number[] = []
  return new Promise(resolve => {
    let frame = 0
    let last = now()
    const step = () => {
      const time = now()
      if (frame > 0) {
        intervals.push(time - last)
      }
      last = time
      if (frame === 2 * frames) {
        resolve(scrollResult(linesPerFrame, checks, intervals))
        return
      }
      const direction = frame < frames ? 1 : -1
      frame += 1
      // As a wheel scroll paints: the compositor moves the text at once,
      // and the page hears of it (the scroll event) a frame later.
      scroller.scrollTop += direction * linesPerFrame * lineHeight
      checks.push(measureFrame(scroller, documentId, lineCount, lineHeight))
      requestAnimationFrame(step)
    }
    requestAnimationFrame(step)
  })
}

/**
 * Drags the minimap's slider from the top of the document to the bottom in
 * `frames` frames and back, as the mouse does, and checks each frame. A drag
 * scrolls from the page, so the scroll event arrives in the frame it paints.
 */
function dragMinimap(documentId: number, lineCount: number, frames: number): Promise<FlingResult> {
  const scroller = scrollerOf(documentId)
  const lineHeight = lineHeightOf(scroller)
  const minimap = scroller.parentElement?.querySelector<HTMLElement>('.minimap')
  const slider = minimap?.querySelector<HTMLElement>('.minimap__slider')
  if (!minimap || !slider) {
    throw new Error('the minimap is not shown')
  }
  scroller.scrollTop = 0
  const map = minimap.getBoundingClientRect()
  const thumb = slider.getBoundingClientRect()
  const x = map.left + map.width / 2
  const from = thumb.top + thumb.height / 2
  const to = map.bottom - thumb.height / 2
  const mouse = (type: string, y: number) =>
    new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, clientX: x, clientY: y })
  const checks: FrameCheck[] = []
  const intervals: number[] = []
  return new Promise(resolve => {
    let frame = 0
    let last = now()
    minimap.dispatchEvent(mouse('mousedown', from))
    const step = () => {
      const time = now()
      if (frame > 0) {
        intervals.push(time - last)
      }
      last = time
      if (frame === 2 * frames) {
        window.dispatchEvent(mouse('mouseup', from))
        resolve(scrollResult(Math.round(lineCount / frames), checks, intervals))
        return
      }
      frame += 1
      const progress = frame <= frames ? frame / frames : 2 - frame / frames
      window.dispatchEvent(mouse('mousemove', from + (to - from) * progress))
      scroller.dispatchEvent(new Event('scroll'))
      checks.push(measureFrame(scroller, documentId, lineCount, lineHeight))
      requestAnimationFrame(step)
    }
    requestAnimationFrame(step)
  })
}

const benchmarks: Record<string, Benchmark> = {
  /** Time to an interactive window, measured once the app is idle. */
  async startup() {
    await delay(500)
    return { navigationToIdleMs: now(), frontend: perfSnapshot() }
  },

  /**
   * Keystroke-to-paint latency in a small and a large document, and a typing
   * burst. With a file, also typing in the middle of that file (which, for a
   * source file, includes syntax highlighting and language server sync).
   */
  async typing(config) {
    const small = await addDocument(await newDocument())
    await typeAndTime(small, 20)
    resetPerf()
    const smallSamples = await typeAndTime(small, 300)

    const burstPainted = nextPaint(info => info.documentId === small.documentId)
    const burstStart = now()
    for (let i = 0; i < 300; i++) {
      small.dispatch({ type: 'insert', text: 'y' })
    }
    await burstPainted
    // Wait until the last batch has been painted.
    let settled = now()
    for (;;) {
      const next = await Promise.race([nextPaint().then(() => true), delay(200).then(() => false)])
      if (!next) {
        break
      }
      settled = now()
    }
    const burstMs = settled - burstStart

    const large = await addDocument(await benchGenerate(200_000, 80))
    large.dispatch({
      type: 'setCursor',
      position: { line: 100_000, column: 10 },
      extend: false,
      add: false,
    })
    await nextPaint(info => info.documentId === large.documentId && info.startLine > 1000)
    await typeAndTime(large, 20)
    const largeSamples = await typeAndTime(large, 300)

    let fileSamples: number[] | null = null
    if (config.file) {
      const file = await addDocument(await openDocument(config.file))
      const middle = Math.floor(file.lineCount() / 2)
      file.dispatch({
        type: 'setCursor',
        position: { line: middle, column: 0 },
        extend: false,
        add: false,
      })
      await languageServerSettled()
      await typeAndTime(file, 20)
      fileSamples = await typeAndTime(file, 300)
    }

    return {
      smallDocumentKeystroke: summarize('keystrokeToPaint', 'ms', smallSamples),
      largeDocumentKeystroke: summarize('keystrokeToPaint', 'ms', largeSamples),
      fileKeystroke: fileSamples && summarize('keystrokeToPaint', 'ms', fileSamples),
      burst300CharactersMs: burstMs,
      frontend: perfSnapshot(),
      backend: await getPerfStats(),
    }
  },

  /** Page-jump scrolling through a 200,000-line document. */
  async scroll() {
    const generateStart = now()
    const summary = await benchGenerate(200_000, 80)
    const generateMs = now() - generateStart
    const editor = await addDocument(summary)
    resetPerf()
    const samples: number[] = []
    for (let page = 1; page <= 200; page++) {
      const target = page * 997
      const painted = nextPaint(
        info =>
          info.documentId === editor.documentId &&
          info.startLine <= target &&
          target < info.endLine,
      )
      const started = now()
      editor.scrollToLine(target)
      await painted
      samples.push(now() - started)
    }
    const endPainted = nextPaint(
      info => info.documentId === editor.documentId && info.endLine > 199_000,
    )
    const endStart = now()
    editor.dispatch({ type: 'move', motion: 'documentEnd', extend: false })
    await endPainted
    return {
      generateAndOpen200kLinesMs: generateMs,
      pageJumpToPaint: summarize('scrollToPaint', 'ms', samples),
      jumpToEndMs: now() - endStart,
      frontend: perfSnapshot(),
      backend: await getPerfStats(),
    }
  },

  /**
   * Fast scrolling, as a wheel or trackpad fling: a fixed number of lines
   * every frame through a 200,000-line document (or the file given, which
   * may be highlighted), counting frames painted with lines missing.
   */
  async fling(config) {
    const editor = await addDocument(
      config.file ? await openDocument(config.file) : await benchGenerate(200_000, 80),
    )
    const scroller = scrollerOf(editor.documentId)
    // Background parsing of a large file settles first.
    await delay(config.file ? 3000 : 500)
    resetPerf()
    const results: FlingResult[] = []
    for (const speed of FLING_SPEEDS) {
      // Room for the whole fling, in lines, past the first screen.
      const frames = Math.min(FLING_FRAMES, Math.floor((editor.lineCount() - 200) / speed))
      if (frames < 10) {
        continue
      }
      scroller.scrollTop = 0
      await delay(500)
      results.push(await fling(editor.documentId, editor.lineCount(), speed, frames))
    }
    return {
      lineCount: editor.lineCount(),
      flings: results,
      frontend: perfSnapshot(),
      backend: await getPerfStats(),
    }
  },

  /**
   * Dragging the minimap's slider from the top of a document to the bottom
   * and back, fast (20 frames each way) and slower (60): a 4,000-line
   * document, or the file given (which may be highlighted).
   */
  async drag(config) {
    const editor = await addDocument(
      config.file ? await openDocument(config.file) : await benchGenerate(4_000, 80),
    )
    // Background parsing and anything fetched once a document opens settle first.
    await delay(config.file ? 3000 : 1000)
    resetPerf()
    const results: FlingResult[] = []
    for (const frames of [20, 60]) {
      results.push(await dragMinimap(editor.documentId, editor.lineCount(), frames))
      await delay(500)
    }
    return {
      lineCount: editor.lineCount(),
      drags: results,
      frontend: perfSnapshot(),
      backend: await getPerfStats(),
    }
  },

  /**
   * The horizontal scroll range (not a timing): it grows to the widest line
   * scrolled to, stays as wide after scrolling past it, and shrinks once that
   * line gets shorter. Line 1,500 of 3,000 is 400 characters, the rest 20.
   */
  async width() {
    const long = 1_500
    const text = Array.from({ length: 3_000 }, (_, line) =>
      line === long ? 'y'.repeat(400) : 'x'.repeat(20),
    ).join('\n')
    const editor = await addDocument(await newDocument(text))
    const scroller = scrollerOf(editor.documentId)
    const settle = () => new Promise(resolve => setTimeout(resolve, 300))
    const scrollTo = async (line: number) => {
      const painted = nextPaint(
        info =>
          info.documentId === editor.documentId && info.startLine <= line && line < info.endLine,
      )
      editor.scrollToLine(line)
      await painted
      await settle()
      return scroller.scrollWidth
    }
    await settle()
    const atTop = scroller.scrollWidth
    const atLongLine = await scrollTo(long)
    const pastIt = await scrollTo(2_900)
    // Shorten the long line to 30 characters, from where it's out of view.
    editor.dispatch({
      type: 'setCursor',
      position: { line: long, column: 30 },
      extend: false,
      add: false,
    })
    editor.dispatch({
      type: 'setCursor',
      position: { line: long, column: 400 },
      extend: true,
      add: false,
    })
    editor.dispatch({ type: 'deleteSelection' })
    await settle()
    const afterShortening = await scrollTo(long)
    const backPastIt = await scrollTo(2_900)
    return {
      clientWidth: scroller.clientWidth,
      atTop,
      atLongLine,
      pastIt,
      afterShortening,
      backPastIt,
      ok:
        atLongLine > atTop &&
        pastIt === atLongLine &&
        afterShortening < atLongLine &&
        backPastIt === afterShortening,
    }
  },

  /** Opening the file given on the command line, until its first paint. */
  async open(config) {
    if (!config.file) {
      throw new Error('the open benchmark needs a file: --bench open <file>')
    }
    const started = now()
    const summary = await openDocument(config.file)
    const loadedMs = now() - started
    await addDocument(summary)
    return {
      openToLoadedMs: loadedMs,
      openToPaintMs: now() - started,
      backend: await getPerfStats(),
    }
  },
}

/** Runs the benchmark requested on the command line, if any. */
export async function runRequestedBenchmark(): Promise<void> {
  const config = await benchConfig()
  if (!config) {
    return
  }
  const benchmark = benchmarks[config.name]
  try {
    if (!benchmark) {
      throw new Error(
        `unknown benchmark "${config.name}"; try ${Object.keys(benchmarks).join(', ')}`,
      )
    }
    await benchReport({ ok: true, ...(await benchmark(config)) })
  } catch (error) {
    await benchReport({ ok: false, error: String(error) })
  }
}
