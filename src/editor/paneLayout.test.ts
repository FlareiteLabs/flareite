import { describe, expect, it } from 'vitest'
import { foldRegions, hiddenRanges, rowOfLine, zoneRow } from './paneLayout'

describe('hiddenRanges', () => {
  it('sorts and merges folds, keeping the first line', () => {
    expect(
      hiddenRanges([
        { start: 10, end: 20 },
        { start: 0, end: 4 },
        { start: 15, end: 25 },
        { start: 30, end: 30 },
      ]),
    ).toEqual([
      [1, 4],
      [10, 25],
    ])
  })

  it('folds from the line before the hidden lines', () => {
    expect(foldRegions([[5, 9]])).toEqual([[4, 9]])
  })
})

describe('rows', () => {
  const hidden: [number, number][] = [
    [3, 6],
    [10, 12],
  ]

  it('moves lines up past hidden ones', () => {
    expect(rowOfLine(hidden, 2)).toBe(2)
    expect(rowOfLine(hidden, 6)).toBe(3)
    expect(rowOfLine(hidden, 12)).toBe(7)
  })

  it('puts a hidden line on the row of the line before', () => {
    expect(rowOfLine(hidden, 4)).toBe(2)
  })

  it('puts a zone below the row of the line before its line', () => {
    expect(zoneRow(hidden, 0)).toBe(-1)
    expect(zoneRow(hidden, 3)).toBe(2)
    expect(zoneRow(hidden, 6)).toBe(2)
    expect(zoneRow(hidden, 7)).toBe(3)
  })
})
