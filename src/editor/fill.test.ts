import { describe, expect, it } from 'vitest'
import { fillHtml } from './fill'

describe('rows drawn from the copy of the lines', () => {
  it('draws highlighted, escaped text and numbers at their rows', () => {
    // "let a<b" with "let" as a keyword (kind 1).
    const { text, gutter } = fillHtml(
      [{ line: { text: 'let a<b', fullLength: null, spans: [0, 3, 1] }, number: 9, top: 200 }],
      line => String(line + 1),
    )
    const element = document.createElement('div')
    element.innerHTML = text
    const row = element.firstElementChild as HTMLElement
    expect(row.className).toBe('editor-view__line')
    expect(row.style.top).toBe('200px')
    expect(row.textContent).toBe('let a<b')
    expect(row.querySelector('span')?.textContent).toBe('let')
    expect(row.querySelector('span')?.className).toMatch(/^tok-/)

    element.innerHTML = gutter
    expect(element.firstElementChild?.textContent).toBe('10')
  })
})
