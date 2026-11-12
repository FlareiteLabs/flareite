import { describe, expect, it } from 'vitest'
import {
  displayRow,
  lastRow,
  lineAtRow,
  positionAtRow,
  rowStartsOf,
  segmentOf,
  segmentRange,
  viewRows,
} from './rows'

// Lines 10, 11, and 14 (12 and 13 folded), from row 10; line 11 takes three rows.
const wrap = { line: 11, breaks: [8, 16], indent: 2 }
const lineNumbers = [10, 11, 14]
const view = {
  lineNumbers,
  rowStarts: rowStartsOf(lineNumbers, new Map([[11, wrap]])),
  lineCount: 100,
  rowCount: 100,
  startRow: 10,
}

describe('rows', () => {
  it('counts the rows wrapped lines take', () => {
    expect(view.rowStarts).toEqual([0, 1, 4, 5])
    expect(viewRows(view)).toBe(5)
  })

  it('finds the line and its row on a row', () => {
    expect(positionAtRow(view, 0)).toEqual({ line: 10, index: 0, segment: 0 })
    expect(positionAtRow(view, 2)).toEqual({ line: 11, index: 1, segment: 1 })
    expect(positionAtRow(view, 3)).toEqual({ line: 11, index: 1, segment: 2 })
    expect(positionAtRow(view, 4)).toEqual({ line: 14, index: 2, segment: 0 })
    expect(lineAtRow(view, 6)).toBe(16)
  })

  it("finds a line's first and last rows", () => {
    expect(displayRow(view, 11)).toBe(11)
    expect(lastRow(view, 11)).toBe(13)
    expect(displayRow(view, 14)).toBe(14)
    expect(lastRow(view, 10)).toBe(10)
    // A hidden line is on its region's first row: line 11's.
    expect(displayRow(view, 12)).toBe(11)
  })

  it('finds the row a column is on, a break starting the next', () => {
    expect(segmentOf(wrap, 7)).toBe(0)
    expect(segmentOf(wrap, 8)).toBe(1)
    expect(segmentOf(wrap, 20)).toBe(2)
    expect(segmentOf(undefined, 20)).toBe(0)
    expect(segmentRange(wrap, 1, 20)).toEqual([8, 16])
    expect(segmentRange(wrap, 2, 20)).toEqual([16, 20])
    expect(segmentRange(undefined, 0, 20)).toEqual([0, 20])
  })
})
