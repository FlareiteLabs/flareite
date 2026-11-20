import { describe, expect, it } from 'vitest'
import { followWidest, KEPT_LINES, widestWidth } from './widestLines'

const NONE = { base: null, splices: [] }
const widths = (entries: [number, number][]) => new Map(entries)

describe('the widest lines shown', () => {
  it('starts as the widest line rendered and grows to wider ones scrolled to', () => {
    let widest = followWidest(
      null,
      1,
      NONE,
      widths([
        [0, 100],
        [1, 300],
        [2, 200],
      ]),
    )
    expect(widestWidth(widest)).toBe(300)
    // Scrolled down: narrower lines don't shrink it, a wider one grows it.
    widest = followWidest(
      widest,
      1,
      NONE,
      widths([
        [50, 120],
        [51, 90],
      ]),
    )
    expect(widestWidth(widest)).toBe(300)
    widest = followWidest(widest, 1, NONE, widths([[100, 500]]))
    expect(widestWidth(widest)).toBe(500)
  })

  it('shrinks when the widest line gets narrower, to the next widest seen', () => {
    let widest = followWidest(
      null,
      1,
      NONE,
      widths([
        [0, 100],
        [1, 300],
      ]),
    )
    widest = followWidest(widest, 1, NONE, widths([[80, 500]]))
    // Line 80 edited shorter, rendered again.
    const edit = { base: 1, splices: [{ start: 80, removed: 1, inserted: 1 }] }
    widest = followWidest(widest, 2, edit, widths([[80, 150]]))
    expect(widestWidth(widest)).toBe(300)
  })

  it('forgets a widest line that is removed, even off screen', () => {
    let widest = followWidest(
      null,
      1,
      NONE,
      widths([
        [10, 500],
        [20, 300],
      ]),
    )
    // Lines 5 to 14 deleted while showing others.
    const edit = { base: 1, splices: [{ start: 5, removed: 10, inserted: 0 }] }
    widest = followWidest(widest, 2, edit, widths([[0, 100]]))
    expect(widestWidth(widest)).toBe(300)
    // Line 20 moved up to 10.
    expect(widest.lines[0]).toEqual({ line: 10, width: 300 })
  })

  it('follows lines inserted above the widest', () => {
    let widest = followWidest(null, 1, NONE, widths([[10, 500]]))
    const enter = { base: 1, splices: [{ start: 3, removed: 1, inserted: 2 }] }
    widest = followWidest(
      widest,
      2,
      enter,
      widths([
        [3, 50],
        [4, 60],
      ]),
    )
    expect(widest.lines[0]).toEqual({ line: 11, width: 500 })
  })

  it('starts over when the edits are unknown', () => {
    let widest = followWidest(null, 1, NONE, widths([[10, 500]]))
    widest = followWidest(widest, 5, { base: 3, splices: [] }, widths([[0, 100]]))
    expect(widestWidth(widest)).toBe(100)
  })

  it('remembers a bounded number of lines', () => {
    const many = widths(Array.from({ length: 50 }, (_, line): [number, number] => [line, line]))
    expect(followWidest(null, 1, NONE, many).lines).toHaveLength(KEPT_LINES)
  })
})
