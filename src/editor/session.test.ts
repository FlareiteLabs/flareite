import { describe, expect, it, vi } from 'vitest'
import type { EditorOp, Viewport, WireView } from '../ipc/editor'
import { EditorSession, type EditorTransport } from './session'

/** A view of no lines (the tests here are about request flow, not text). */
function view(version: number, viewport: Viewport): WireView {
  return {
    documentId: 1,
    version,
    lineCount: 100,
    rowCount: 100,
    startRow: viewport.startRow,
    startLine: viewport.startRow,
    endLine: viewport.startRow,
    visible: [],
    folded: [],
    foldable: [],
    foldGeneration: 0,
    lines: { base: null, splices: [], chunks: [] },
    selections: [],
    selectionCount: 1,
    primary: { position: { line: 0, column: 0 }, visualColumn: 0, row: 0 },
    dirty: false,
    search: null,
    highlights: [],
    diagnostics: [],
    language: null,
    decorations: [],
    changes: [],
    lenses: [],
    annotations: [],
  }
}

/** A transport whose requests resolve only when the test says so. */
function controlledTransport() {
  const calls: { kind: string; ops?: EditorOp[]; viewport: Viewport }[] = []
  const resolvers: (() => void)[] = []
  let version = 0
  const respond = (kind: string, viewport: Viewport, ops?: EditorOp[]) => {
    calls.push({ kind, viewport, ...(ops ? { ops } : {}) })
    return new Promise<WireView>(resolve => {
      resolvers.push(() => resolve(view(++version, viewport)))
    })
  }
  const transport: EditorTransport = {
    apply: (_id, _view, ops, viewport) => respond('apply', viewport, [...ops]),
    view: (_id, _view, viewport) => respond('view', viewport),
    clipboard: (_id, _view, _action, viewport) => respond('clipboard', viewport),
  }
  const next = async () => {
    resolvers.shift()?.()
    await new Promise(r => setTimeout(r, 0))
  }
  return { transport, calls, next }
}

const insert = (text: string): EditorOp => ({ type: 'insert', text })
const drag = (column: number): EditorOp => ({
  type: 'setCursor',
  position: { line: 0, column },
  extend: true,
  add: false,
})

describe('EditorSession', () => {
  it('fetches the initial view', async () => {
    const { transport, calls, next } = controlledTransport()
    const onView = vi.fn()
    new EditorSession(1, 0, { startRow: 0, endRow: 10 }, onView, vi.fn(), transport)
    await Promise.resolve()
    expect(calls).toEqual([{ kind: 'view', viewport: { startRow: 0, endRow: 10 } }])
    await next()
    expect(onView).toHaveBeenCalledOnce()
  })

  it('sends operations dispatched right after construction with the first request', async () => {
    const { transport, calls } = controlledTransport()
    const session = new EditorSession(
      1,
      0,
      { startRow: 0, endRow: 10 },
      vi.fn(),
      vi.fn(),
      transport,
    )
    session.dispatch(insert('a'))
    await Promise.resolve()
    expect(calls).toHaveLength(1)
    expect(calls[0]).toMatchObject({ kind: 'apply', ops: [insert('a')] })
  })

  it('batches operations sent while a request is in flight', async () => {
    const { transport, calls, next } = controlledTransport()
    const onView = vi.fn()
    const session = new EditorSession(1, 0, { startRow: 0, endRow: 10 }, onView, vi.fn(), transport)
    await Promise.resolve()

    expect(session.dispatch(insert('a'))).toBe(1)
    session.dispatch(insert('b'))
    expect(session.dispatch(insert('c'))).toBe(3)
    expect(calls).toHaveLength(1)
    expect(calls[0]?.kind).toBe('view')

    await next()
    expect(calls).toHaveLength(2)
    expect(calls[1]?.ops).toEqual([insert('a'), insert('b'), insert('c')])
    // The view that was in flight doesn't reflect them yet.
    expect(onView).toHaveBeenLastCalledWith(expect.anything(), 0)

    await next()
    expect(onView).toHaveBeenCalledTimes(2)
    expect(onView).toHaveBeenLastCalledWith(expect.anything(), 3)
    expect(calls).toHaveLength(2)
  })

  it('keeps only the latest drag position', async () => {
    const { transport, calls, next } = controlledTransport()
    const session = new EditorSession(
      1,
      0,
      { startRow: 0, endRow: 10 },
      vi.fn(),
      vi.fn(),
      transport,
    )
    session.dispatch(drag(1))
    session.dispatch(drag(2))
    session.dispatch(drag(3))
    await next()
    expect(calls[1]?.ops).toEqual([drag(3)])
  })

  it('sends the latest viewport with the next request', async () => {
    const { transport, calls, next } = controlledTransport()
    const session = new EditorSession(
      1,
      0,
      { startRow: 0, endRow: 10 },
      vi.fn(),
      vi.fn(),
      transport,
    )
    session.setViewport({ startRow: 5, endRow: 15 })
    session.setViewport({ startRow: 20, endRow: 30 })
    session.dispatch(insert('x'))
    await next()
    expect(calls[1]).toMatchObject({ kind: 'apply', viewport: { startRow: 20, endRow: 30 } })
    await next()
    expect(calls).toHaveLength(2)
  })

  it('reports errors, resynchronizes, and keeps processing', async () => {
    const onError = vi.fn()
    const onView = vi.fn()
    const transport: EditorTransport = {
      apply: vi.fn().mockRejectedValueOnce(new Error('stale')),
      view: vi.fn((_id, _view, viewport: Viewport) => Promise.resolve(view(1, viewport))),
      clipboard: vi.fn(),
    }
    const session = new EditorSession(1, 0, { startRow: 0, endRow: 10 }, onView, onError, transport)
    await session.idle()
    session.dispatch(insert('x'))
    await session.idle()
    expect(onError).toHaveBeenCalledOnce()
    expect(transport.view).toHaveBeenCalledTimes(2)
    expect(onView).toHaveBeenCalledTimes(2)
  })

  it('backs off when the backend keeps failing, and reports it once', async () => {
    vi.useFakeTimers()
    try {
      const onError = vi.fn()
      const transport: EditorTransport = {
        apply: vi.fn(),
        view: vi.fn().mockRejectedValue(new Error('unknown document')),
        clipboard: vi.fn(),
      }
      const session = new EditorSession(
        1,
        0,
        { startRow: 0, endRow: 10 },
        vi.fn(),
        onError,
        transport,
      )
      await vi.advanceTimersByTimeAsync(0)
      // The first failure is retried at once; the second waits.
      expect(transport.view).toHaveBeenCalledTimes(2)
      await vi.advanceTimersByTimeAsync(249)
      expect(transport.view).toHaveBeenCalledTimes(2)
      await vi.advanceTimersByTimeAsync(1)
      expect(transport.view).toHaveBeenCalledTimes(3)
      // Then 500 ms, 1 s, ...
      await vi.advanceTimersByTimeAsync(500)
      expect(transport.view).toHaveBeenCalledTimes(4)
      expect(onError).toHaveBeenCalledOnce()
      // Waiting out a failure doesn't block saving.
      await expect(session.idle()).resolves.toBeUndefined()
      session.dispose()
    } finally {
      vi.useRealTimers()
    }
  })

  it('sends the find query with every request', async () => {
    const { transport, calls, next } = controlledTransport()
    const search = { pattern: 'x', regex: false, caseSensitive: false, wholeWord: false }
    const apply = vi.spyOn(transport, 'apply')
    const session = new EditorSession(
      1,
      0,
      { startRow: 0, endRow: 10 },
      vi.fn(),
      vi.fn(),
      transport,
    )
    await Promise.resolve()
    await next()
    session.setSearch(search)
    expect(calls).toHaveLength(2)
    await next()
    session.dispatch(insert('a'))
    await next()
    expect(apply).toHaveBeenCalledWith(
      1,
      0,
      [insert('a')],
      expect.anything(),
      expect.anything(),
      search,
    )
    session.setSearch({ ...search, pattern: '' })
    await next()
    expect(calls).toHaveLength(4)
  })

  it('tells the backend which lines it holds', async () => {
    const known: unknown[] = []
    const transport: EditorTransport = {
      view: vi.fn((_id, _view, viewport: Viewport, held) => {
        known.push(held)
        return Promise.resolve(view(5, viewport))
      }),
      apply: vi.fn((_id, _view, _ops, viewport: Viewport, held) => {
        known.push(held)
        return Promise.resolve(view(6, viewport))
      }),
      clipboard: vi.fn(),
    }
    const session = new EditorSession(
      1,
      0,
      { startRow: 3, endRow: 10 },
      vi.fn(),
      vi.fn(),
      transport,
    )
    await session.idle()
    session.dispatch(insert('a'))
    await session.idle()
    expect(known).toEqual([null, { version: 5, ranges: [] }])
  })

  it('refetches everything when an update cannot be applied', async () => {
    const onView = vi.fn()
    let calls = 0
    const transport: EditorTransport = {
      view: vi.fn((_id, _view, viewport: Viewport) => {
        calls++
        // The first response claims a base version the session never had.
        const wire = view(calls, viewport)
        return Promise.resolve(
          calls === 1 ? { ...wire, lines: { base: 99, splices: [], chunks: [] } } : wire,
        )
      }),
      apply: vi.fn(),
      clipboard: vi.fn(),
    }
    const session = new EditorSession(1, 0, { startRow: 0, endRow: 10 }, onView, vi.fn(), transport)
    await session.idle()
    await new Promise(r => setTimeout(r, 0))
    await session.idle()
    expect(transport.view).toHaveBeenCalledTimes(2)
    expect(onView).toHaveBeenCalledOnce()
  })

  it('resolves idle once queued work is done', async () => {
    const { transport, next } = controlledTransport()
    const session = new EditorSession(
      1,
      0,
      { startRow: 0, endRow: 10 },
      vi.fn(),
      vi.fn(),
      transport,
    )
    session.dispatch(insert('a'))
    session.dispatch(insert('b'))
    let idle = false
    void session.idle().then(() => (idle = true))
    // "a" is in flight and "b" is queued behind it.
    await next()
    expect(idle).toBe(false)
    await next()
    expect(idle).toBe(true)
  })

  it('says where lines wrap with every request, once set, and which wrap a view has', async () => {
    const { transport, calls, next } = controlledTransport()
    const onView = vi.fn()
    const session = new EditorSession(1, 0, { startRow: 0, endRow: 10 }, onView, vi.fn(), transport)
    // The first request goes out, then is answered.
    await next()
    await next()
    const wrap = { columns: 80, indent: 'same' as const }
    expect(session.setWrap(wrap)).toBe(true)
    expect(session.setWrap({ ...wrap })).toBe(false)
    expect(calls[1]).toEqual({ kind: 'view', viewport: { startRow: 0, endRow: 10, wrap } })
    await next()
    expect(onView.mock.calls.at(-1)![0].wrapKey).toBe('80:same')
    // Scrolling keeps it; stopping leaves it out.
    session.setViewport({ startRow: 5, endRow: 15 })
    expect(calls[2]!.viewport).toEqual({ startRow: 5, endRow: 15, wrap })
    await next()
    session.setWrap(null)
    expect(calls[3]!.viewport).toEqual({ startRow: 5, endRow: 15 })
  })

  it('places wrapped lines on the rows they take', async () => {
    const onView = vi.fn()
    const wire: WireView = {
      ...view(1, { startRow: 0, endRow: 10 }),
      visible: [[0, 3]],
      lines: { base: null, splices: [], chunks: [{ start: 0, lines: ['a', 'long line', 'b'] }] },
      wraps: [{ line: 1, breaks: [5], indent: 0 }],
    }
    const transport: EditorTransport = {
      apply: () => Promise.resolve(wire),
      view: () => Promise.resolve(wire),
      clipboard: () => Promise.resolve(wire),
    }
    new EditorSession(1, 0, { startRow: 0, endRow: 10 }, onView, vi.fn(), transport)
    await new Promise(r => setTimeout(r, 0))
    const shown = onView.mock.calls[0]![0]
    expect(shown.rowStarts).toEqual([0, 1, 3, 4])
    expect(shown.wrapped.get(1)).toEqual({ line: 1, breaks: [5], indent: 0 })
  })
})
