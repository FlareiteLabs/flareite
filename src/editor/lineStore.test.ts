import { describe, expect, it } from 'vitest'
import type { DisplayLine, EditorView, LinesView } from '../ipc/editor'
import {
  addFetched,
  CHUNK_LINES,
  emptyStore,
  followView,
  FULL_LINES,
  nextFetch,
  WINDOW_LINES,
} from './lineStore'

const line = (text: string): DisplayLine => ({ text, fullLength: null, spans: [] })

/** A view of `lines` from `start`, at `version`, after `edits`. */
function view(
  version: number,
  lineCount: number,
  start: number,
  texts: string[],
  edits: EditorView['edits'] = { base: null, splices: [] },
): EditorView {
  return {
    documentId: 1,
    version,
    lineCount,
    lineNumbers: texts.map((_, index) => start + index),
    lines: texts.map(line),
    edits,
  } as unknown as EditorView
}

function fetched(version: number, start: number, texts: string[]): LinesView {
  return {
    documentId: 1,
    version,
    lineCount: 10,
    start,
    lines: texts,
    spans: texts.map(() => [0, 1, 2]),
  }
}

const texts = (store: ReturnType<typeof emptyStore>) => store.lines.map(held => held?.text)

describe("the editor's copy of the lines", () => {
  it("takes a view's lines, and fetched lines of the same version", () => {
    const store = followView(null, view(1, 10, 2, ['c', 'd']))
    expect(addFetched(store, fetched(1, 4, ['e', 'f']))).toBe(true)
    expect(texts(store).slice(0, 6)).toEqual([undefined, undefined, 'c', 'd', 'e', 'f'])
    expect(store.lines[4]?.spans).toEqual([0, 1, 2])
    // Fetched before or after an edit: not taken.
    expect(addFetched(store, fetched(2, 0, ['a', 'b']))).toBe(false)
    expect(store.lines[0]).toBeUndefined()
  })

  it("follows edits through a view's splices", () => {
    const store = followView(null, view(1, 5, 0, ['a', 'b', 'c', 'd', 'e']))
    // Enter at the end of line 1: line 1 changed and a line was inserted after it.
    const after = followView(
      store,
      view(2, 6, 0, ['a', 'b'], { base: 1, splices: [{ start: 1, removed: 1, inserted: 2 }] }),
    )
    expect(after).toBe(store)
    expect(texts(after)).toEqual(['a', 'b', undefined, 'c', 'd', 'e'])
    expect(after.version).toBe(2)
  })

  it("starts over when it can't follow", () => {
    const store = followView(null, view(1, 3, 0, ['a', 'b', 'c']))
    const after = followView(store, view(5, 3, 1, ['x'], { base: 4, splices: [] }))
    expect(after).not.toBe(store)
    expect(texts(after)).toEqual([undefined, 'x', undefined])
  })

  it('handles splices larger than a call takes arguments', () => {
    const store = followView(null, view(1, 2, 0, ['a', 'b']))
    const inserted = 300_000
    followView(
      store,
      view(2, inserted + 2, 0, ['a'], { base: 1, splices: [{ start: 1, removed: 0, inserted }] }),
    )
    expect(store.lines.length).toBe(inserted + 2)
    expect(store.lines[inserted + 1]?.text).toBe('b')
  })

  it('fetches the whole of a small document, nearest the view first', () => {
    const store = emptyStore(1, 1, 4_000)
    store.lines.fill(line('x'), 1_000, 1_050)
    // The view's lines are held; the next missing line below them is nearer.
    expect(nextFetch(store, 1_000, 1_050)).toEqual([1_050, 1_050 + CHUNK_LINES])
    store.lines.fill(line('x'), 1_050, 3_100)
    expect(nextFetch(store, 1_000, 1_050)).toEqual([0, 1_000])
    store.lines.fill(line('x'), 0, 1_000)
    expect(nextFetch(store, 1_000, 1_050)).toEqual([3_100, 4_000])
    store.lines.fill(line('x'), 3_100, 4_000)
    expect(nextFetch(store, 1_000, 1_050)).toBeNull()
  })

  it('fetches a window around the view of a large document', () => {
    const store = emptyStore(1, 1, FULL_LINES * 10)
    const [viewStart, viewEnd] = [100_000, 100_050]
    store.lines.fill(line('x'), viewStart - WINDOW_LINES, viewEnd + WINDOW_LINES)
    expect(nextFetch(store, viewStart, viewEnd)).toBeNull()
  })
})
