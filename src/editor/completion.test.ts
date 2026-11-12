import { describe, expect, it } from 'vitest'
import type { CompletionItem } from '../ipc/lsp'
import { isWordChar, matchTier, visibleCompletions, wordAround } from './completion'

const item = (label: string, column = 4): CompletionItem => ({
  label,
  kind: null,
  detail: null,
  insert: label,
  from: { line: 0, column },
  filter: label,
  snippet: false,
  adjustIndentation: false,
})

describe('completion', () => {
  it('ranks prefixes before case-insensitive prefixes before scattered letters', () => {
    expect(matchTier('push_str', 'push')).toBe(0)
    expect(matchTier('PushStr', 'push')).toBe(1)
    expect(matchTier('push_str', 'pst')).toBe(2)
    expect(matchTier('push_str', 'x')).toBeNull()
  })

  it("filters by what was typed since each item's start", () => {
    const items = [item('as_str'), item('push'), item('Pull'), item('pop')]
    // "let pu|" with items starting at column 4.
    expect(visibleCompletions(items, { line: 0, column: 6 }, 'let pu')?.map(i => i.label)).toEqual([
      'push',
      'Pull',
    ])
    expect(visibleCompletions(items, { line: 0, column: 4 }, 'let ')).toHaveLength(4)
  })

  it('closes when the cursor leaves the completed word', () => {
    const items = [item('push')]
    expect(visibleCompletions(items, { line: 0, column: 3 }, 'let pu')).toBeNull()
    expect(visibleCompletions(items, { line: 1, column: 6 }, 'let pu')).toBeNull()
  })

  it('treats letters, digits, underscores and dollars as word characters', () => {
    expect(['a', 'Z', 'é', '7', '_', '$'].every(isWordChar)).toBe(true)
    expect(['.', ' ', '(', ':'].some(isWordChar)).toBe(false)
  })

  it('finds the word around a character', () => {
    const text = 'let p = point.distance(q);'
    expect(wordAround(text, 8)).toEqual([8, 13])
    expect(wordAround(text, 12)).toEqual([8, 13])
    expect(wordAround(text, 14)).toEqual([14, 22])
    expect(wordAround(text, 13)).toBeNull()
    expect(wordAround(text, -1)).toBeNull()
    expect(wordAround(text, text.length)).toBeNull()
  })
})
