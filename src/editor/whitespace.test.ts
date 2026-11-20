import { describe, expect, it } from 'vitest'
import {
  guideColumns,
  guideStyle,
  indentColumns,
  selectedOnLine,
  whitespaceOverlays,
} from './whitespace'

const spans = (text: string, mode: Parameters<typeof whitespaceOverlays>[1], selected = []) =>
  whitespaceOverlays(text, mode, selected).map(
    ({ from, to, className }) => `${from}-${to}${className?.includes('tab') ? 'T' : ''}`,
  )

describe('whitespace', () => {
  it('draws runs of spaces, and each tab, as the mode says', () => {
    const text = '  a b  c\t\td  '
    expect(spans(text, 'none')).toEqual([])
    expect(spans(text, 'all')).toEqual(['0-2', '3-4', '5-7', '8-9T', '9-10T', '11-13'])
    expect(spans(text, 'boundary')).toEqual(['0-2', '5-7', '8-9T', '9-10T', '11-13'])
    expect(spans(text, 'trailing')).toEqual(['11-13'])
  })

  it('draws only what is selected in selection mode', () => {
    expect(spans('a  b  c', 'selection', [[2, 6]] as never)).toEqual(['2-3', '4-6'])
    const selections = [
      { anchor: { line: 3, column: 2 }, head: { line: 1, column: 4 }, primary: true },
    ]
    expect(selectedOnLine(selections, 1, 10)).toEqual([[4, 10]])
    expect(selectedOnLine(selections, 2, 7)).toEqual([[0, 7]])
    expect(selectedOnLine(selections, 3, 9)).toEqual([[0, 2]])
    expect(selectedOnLine(selections, 4, 9)).toEqual([])
  })

  it('measures indentation in columns, tabs to tab stops', () => {
    expect(indentColumns('    a', 4)).toBe(4)
    expect(indentColumns('\t a', 4)).toBe(5)
    expect(indentColumns(' \ta', 4)).toBe(4)
    expect(indentColumns('   ', 4)).toBeNull()
  })

  it('runs guides through blank lines at the deeper indentation around them', () => {
    expect(guideColumns(['fn a() {', '    b();', '', '    c();', '}', '', 'fn d() {'], 4)).toEqual([
      0, 4, 4, 4, 0, 0, 0,
    ])
    expect(guideColumns(['a', '', '        b'], 4)).toEqual([0, 8, 8])
  })

  it('draws a guide at each level the indentation passes', () => {
    expect(guideStyle(0, 4)).toBeUndefined()
    const style = guideStyle(8, 4)!
    expect(style.backgroundPosition).toBe('0ch 0, 4ch 0')
    expect(String(style.backgroundImage).match(/linear-gradient/g)).toHaveLength(2)
    expect(guideStyle(6, 4)!.backgroundPosition).toBe('0ch 0, 4ch 0')
  })
})
