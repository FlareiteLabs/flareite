import { describe, expect, it } from 'vitest'
import type { MinimapView } from '../ipc/editor'
import {
  FULL_ROWS,
  headersOf,
  holds,
  MAX_KEPT_ROWS,
  MAX_REQUEST_ROWS,
  nextRequest,
  restRequest,
  runsAt,
  withRows,
} from './minimapRows'

const STILL = { above: 100, below: 100 }
const NONE = { above: 0, below: 0 }

function fetched(startRow: number, endRow: number, rowCount = 10_000): MinimapView {
  return {
    documentId: 1,
    version: 1,
    rowCount,
    startRow,
    lines: Array.from({ length: endRow - startRow }, (_, index) => [0, startRow + index, 0]),
    headers: [],
  }
}

describe('minimap rows', () => {
  it('fetches what is shown and the margins around it at first', () => {
    // Nothing held: only the rows shown, the rest once they're drawn.
    expect(nextRequest(null, [1000, 1400], STILL, NONE, 10_000)).toEqual([1000, 1400])
  })

  it('adds rows as they arrive, and asks only for those missing', () => {
    let store = withRows(null, 'a', fetched(1000, 1400))
    expect(nextRequest(store, [1000, 1400], STILL, NONE, 10_000)).toBeNull()
    expect(nextRequest(store, [1000, 1400], STILL, STILL, 10_000)).toEqual([900, 1500])
    store = withRows(store, 'a', fetched(900, 1500))
    expect(holds(store, 900, 1500)).toBe(true)
    expect(nextRequest(store, [1000, 1400], STILL, STILL, 10_000)).toBeNull()
    // Scrolled down by 50 rows: only the margin's new rows.
    expect(nextRequest(store, [1050, 1450], STILL, { above: 0, below: 100 }, 10_000)).toEqual([
      1500, 1550,
    ])
  })

  it('asks ahead once the rows held ahead run low', () => {
    const store = withRows(null, 'a', fetched(900, 1800))
    const ahead = { above: 100, below: 400 }
    // 400 rows held below what's shown: more than the 200 needed.
    expect(nextRequest(store, [1000, 1400], ahead, { above: 0, below: 200 }, 10_000)).toBeNull()
    // 150 held below: fetch the rest of the margin.
    expect(nextRequest(store, [1250, 1650], ahead, { above: 0, below: 200 }, 10_000)).toEqual([
      1800, 2050,
    ])
  })

  it('stops at the end of the document', () => {
    const store = withRows(null, 'a', fetched(0, 120, 120))
    expect(nextRequest(store, [0, 401], STILL, STILL, 120)).toBeNull()
    expect(nextRequest(null, [0, 401], STILL, NONE, 120)).toEqual([0, 120])
  })

  it('starts over for a new state of the document, drawing the old rows until then', () => {
    const header = { row: 5, name: 'Old', separator: false, region: true }
    const store = withRows(null, 'a', { ...fetched(0, 100), headers: [header] })
    const next = withRows(store, 'b', fetched(50, 60))
    expect(next.key).toBe('b')
    expect(next.rows.size).toBe(10)
    // Rows not fetched again still draw, from the old state.
    expect(runsAt(next, 10)).toEqual([0, 10, 0])
    expect(headersOf(next)).toEqual([header])
    expect(holds(next, 0, 100)).toBe(true)
    // Fetched again: the old ones go.
    const again = withRows(next, 'b', fetched(0, 50))
    expect(again.stale.size).toBe(40)
    expect(headersOf(again)).toEqual([])
  })

  it('fetches the rest of a small document, nearest what is shown', () => {
    const store = withRows(null, 'a', fetched(1000, 1500, 4000))
    // Row 999 is nearer the middle of those shown (1200) than row 1500.
    expect(restRequest(store, [1000, 1400], 4000)).toEqual([0, 1000])
    const rest = withRows(store, 'a', fetched(0, 1000, 4000))
    expect(restRequest(rest, [1000, 1400], 4000)).toEqual([1500, 1500 + MAX_REQUEST_ROWS])
    expect(
      restRequest(withRows(rest, 'a', fetched(1500, 4000, 4000)), [1000, 1400], 4000),
    ).toBeNull()
    // A large document: only what's shown and around it.
    expect(restRequest(store, [1000, 1400], FULL_ROWS + 1)).toBeNull()
  })

  it('keeps at most a bounded number of rows, dropping the farthest', () => {
    let store = withRows(null, 'a', fetched(0, 1000))
    const end = MAX_KEPT_ROWS + 2000
    for (let start = 1000; start < end; start += 1000) {
      store = withRows(store, 'a', fetched(start, start + 1000, end))
    }
    expect(store.rows.size).toBe(MAX_KEPT_ROWS)
    expect(store.rows.has(end - 1)).toBe(true)
    expect(store.rows.has(0)).toBe(false)
  })

  it('replaces the headers of the rows fetched again', () => {
    const header = { row: 10, name: 'Old', separator: false, region: true }
    let store = withRows(null, 'a', { ...fetched(0, 100), headers: [header] })
    store = withRows(store, 'a', fetched(0, 50))
    expect(store.headers.size).toBe(0)
  })
})
