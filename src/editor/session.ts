/**
 * The frontend side of one open editor: sends operations to the backend in
 * order and delivers the resulting views.
 *
 * At most one request is in flight. Operations issued meanwhile are queued
 * and sent together as one batch, so fast typing costs few round trips and
 * never reorders. Each request says which lines the session already holds,
 * so the backend sends only the lines it is missing (see `lineCache.ts`).
 * Operations and clipboard actions are numbered, and each view says up to
 * which it reflects: one fetched for scrolling may arrive after an
 * operation was issued but before it was applied.
 */
import type {
  ClipboardAction,
  EditorOp,
  EditorView,
  KnownLines,
  SearchQuery,
  Viewport,
  WireView,
  WrapOptions,
} from '../ipc/editor'
import * as ipc from '../ipc/editor'
import { wrapKey } from '../ipc/editor'
import { log } from '../ipc'
import { recordCount, recordTiming } from '../state/perf'
import { applyUpdate, knownLines, type LineCache } from './lineCache'
import { rowStartsOf } from './rows'

export interface EditorTransport {
  apply(
    id: number,
    view: number,
    ops: EditorOp[],
    viewport: Viewport,
    known: KnownLines | null,
    search: SearchQuery | null,
  ): Promise<WireView>
  view(
    id: number,
    view: number,
    viewport: Viewport,
    known: KnownLines | null,
    search: SearchQuery | null,
  ): Promise<WireView>
  clipboard(
    id: number,
    view: number,
    action: ClipboardAction,
    viewport: Viewport,
    known: KnownLines | null,
    search: SearchQuery | null,
  ): Promise<WireView>
}

export const ipcTransport: EditorTransport = {
  apply: ipc.applyOps,
  view: ipc.getView,
  clipboard: ipc.clipboard,
}

type Job = ({ kind: 'ops'; ops: EditorOp[] } | { kind: 'clipboard'; action: ClipboardAction }) & {
  /** The number of the last request in the job. */
  last: number
}

/** The first delay before refetching after repeated failures, doubling up to the maximum. */
const RETRY_BASE_MS = 250
const RETRY_MAX_MS = 5000

export class EditorSession {
  private readonly queue: Job[] = []
  private viewport: Viewport
  private search: SearchQuery | null = null
  private cache: LineCache | null = null
  private viewportStale = true
  private busy = false
  private disposed = false
  private idleWaiters: (() => void)[] = []
  /** Requests (operations and clipboard actions) issued, and those views reflect. */
  private requested = 0
  private reflected = 0
  /** The wrap of the request in flight, which its view was made with. */
  private sentWrap = ''
  /** Requests that failed in a row. After the first, the view is refetched with growing delays. */
  private failures = 0
  private retryTimer: ReturnType<typeof setTimeout> | null = null

  constructor(
    private readonly documentId: number,
    /** The document's view this editor shows (see `ipc/editor.ts`). */
    private readonly view: number,
    viewport: Viewport,
    /** Receives each view, and the number of the last request it reflects. */
    private readonly onView: (view: EditorView, reflected: number) => void,
    private readonly onError: (error: unknown) => void,
    private readonly transport: EditorTransport = ipcTransport,
  ) {
    this.viewport = viewport
    // Start on a microtask so operations dispatched right after construction
    // (e.g. a selection to restore) go out with the first request.
    queueMicrotask(() => this.pump())
  }

  /** Queues `op`; returns its number, which views report once they reflect it. */
  dispatch(op: EditorOp): number {
    const request = ++this.requested
    const last = this.queue.at(-1)
    if (last?.kind === 'ops') {
      const previous = last.ops.at(-1)
      // While dragging, only the latest pointer position matters.
      if (isDragExtend(previous) && isDragExtend(op)) {
        last.ops[last.ops.length - 1] = op
      } else {
        last.ops.push(op)
      }
      last.last = request
    } else {
      this.queue.push({ kind: 'ops', ops: [op], last: request })
    }
    this.pump()
    return request
  }

  /** Queues a clipboard action; returns its number, as `dispatch` does. */
  clipboard(action: ClipboardAction): number {
    const request = ++this.requested
    this.queue.push({ kind: 'clipboard', action, last: request })
    this.pump()
    return request
  }

  setViewport(viewport: Viewport): void {
    if (viewport.startRow === this.viewport.startRow && viewport.endRow === this.viewport.endRow) {
      return
    }
    const { wrap } = this.viewport
    const { startRow, endRow } = viewport
    this.viewport = wrap ? { startRow, endRow, wrap } : { startRow, endRow }
    this.viewportStale = true
    this.pump()
  }

  /**
   * Wraps lines (or stops: `null`) from the next request on, which every
   * request then says. Returns whether that changed anything.
   */
  setWrap(wrap: WrapOptions | null): boolean {
    if (wrapKey(wrap) === wrapKey(this.viewport.wrap)) {
      return false
    }
    const { startRow, endRow } = this.viewport
    this.viewport = wrap ? { startRow, endRow, wrap } : { startRow, endRow }
    this.viewportStale = true
    this.pump()
    return true
  }

  /**
   * Sets the find query whose matches every view should include, or `null`
   * for none. Fetches a fresh view.
   */
  setSearch(search: SearchQuery | null): void {
    const next = search && search.pattern ? search : null
    if (JSON.stringify(next) === JSON.stringify(this.search)) {
      return
    }
    this.search = next
    this.refresh()
  }

  /** Fetches a fresh view, e.g. after the document changed elsewhere. */
  refresh(): void {
    this.viewportStale = true
    this.pump()
  }

  /** Resolves once every queued operation has been applied. */
  idle(): Promise<void> {
    if (!this.busy && this.queue.length === 0) {
      return Promise.resolve()
    }
    return new Promise(resolve => this.idleWaiters.push(resolve))
  }

  dispose(): void {
    this.disposed = true
    this.queue.length = 0
    if (this.retryTimer !== null) {
      clearTimeout(this.retryTimer)
    }
    this.resolveIdle()
  }

  private pump(): void {
    if (this.busy || this.disposed) {
      return
    }
    // A refetch waiting out a failure doesn't hold up edits, or saving.
    if (this.retryTimer !== null && this.queue.length === 0) {
      this.resolveIdle()
      return
    }
    const job = this.queue.shift()
    if (!job && !this.viewportStale) {
      this.resolveIdle()
      return
    }
    const { viewport, search } = this
    this.sentWrap = wrapKey(viewport.wrap)
    const known = knownLines(this.cache)
    this.viewportStale = false
    this.busy = true
    const sent = performance.now()
    const request = !job
      ? this.transport.view(this.documentId, this.view, viewport, known, search)
      : job.kind === 'ops'
        ? this.transport.apply(this.documentId, this.view, job.ops, viewport, known, search)
        : this.transport.clipboard(this.documentId, this.view, job.action, viewport, known, search)
    request
      .then(wire => {
        recordTiming('ipc.roundTrip', performance.now() - sent)
        this.failures = 0
        if (job) {
          this.reflected = job.last
        }
        if (!this.disposed) {
          this.receive(wire)
        }
      })
      .catch((error: unknown) => {
        if (job) {
          // Dropped: the views that follow are as up to date as they get.
          this.reflected = job.last
        }
        if (!this.disposed) {
          // Resynchronize with the backend's state after a failure.
          this.cache = null
          this.viewportStale = true
          this.failures += 1
          // Report a run of failures once, not once per retry.
          if (this.failures === 1) {
            this.onError(error)
          } else {
            log('warn', `Editor ${this.documentId}: request failed again: ${String(error)}`)
          }
        }
      })
      .finally(() => {
        this.busy = false
        if (this.failures > 1 && this.queue.length === 0 && !this.disposed) {
          // The backend keeps refusing: retry later rather than at once.
          const delay = Math.min(RETRY_MAX_MS, RETRY_BASE_MS * 2 ** (this.failures - 2))
          this.retryTimer = setTimeout(() => {
            this.retryTimer = null
            this.pump()
          }, delay)
          this.resolveIdle()
        } else {
          this.pump()
        }
      })
  }

  private receive(wire: WireView): void {
    recordCount(
      'lines.received',
      wire.lines.chunks.reduce((sum, chunk) => sum + chunk.lines.length, 0),
    )
    const cache = applyUpdate(this.cache, wire)
    if (!cache) {
      // Should not happen; recover by asking for every line again.
      log('warn', `Editor ${this.documentId}: line update did not apply; refetching`)
      this.cache = null
      this.viewportStale = true
      return
    }
    this.cache = cache
    const wrapped = new Map((wire.wraps ?? []).map(wrap => [wrap.line, wrap]))
    this.onView(
      {
        ...wire,
        lines: cache.lines,
        lineNumbers: cache.lineNumbers,
        rowStarts: rowStartsOf(cache.lineNumbers, wrapped),
        wrapped,
        wrapKey: this.sentWrap,
      },
      this.reflected,
    )
  }

  private resolveIdle(): void {
    const waiters = this.idleWaiters
    this.idleWaiters = []
    for (const resolve of waiters) {
      resolve()
    }
  }
}

function isDragExtend(op: EditorOp | undefined): boolean {
  return op?.type === 'setCursor' && op.extend && !op.add
}
