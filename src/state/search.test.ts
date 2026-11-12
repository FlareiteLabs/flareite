import { afterEach, describe, expect, it, vi } from 'vitest'
import type { LineMatch } from '../ipc/workspace'

const ipc = vi.hoisted(() => ({
  replaceInFiles: vi.fn(),
  searchWorkspace: vi.fn(() => Promise.resolve()),
  cancelSearch: vi.fn(() => Promise.resolve()),
}))
vi.mock('../ipc/workspace', async importOriginal => ({
  ...(await importOriginal<typeof import('../ipc/workspace')>()),
  ...ipc,
}))
const notify = vi.hoisted(() => vi.fn())
vi.mock('./notifications', async importOriginal => ({
  ...(await importOriginal<typeof import('./notifications')>()),
  notify,
}))

import { dismissResult, replacedText, replaceResults, searchStore } from './search'

const query = { pattern: 'cat', regex: false, caseSensitive: false, wholeWord: false }

function match(line: number, startColumn: number): LineMatch {
  return {
    line,
    startColumn,
    endColumn: startColumn + 3,
    preview: 'cat',
    previewStart: 0,
    previewEnd: 3,
  }
}

function withResults() {
  searchStore.set({
    ...searchStore.get(),
    query,
    replacement: 'dog',
    files: [
      { path: '/w/a.txt', relative: 'a.txt', matches: [match(0, 0), match(2, 4)] },
      { path: '/w/b.txt', relative: 'b.txt', matches: [match(1, 1)] },
    ],
  })
}

describe('search', () => {
  afterEach(() => {
    vi.clearAllMocks()
  })

  it('replaces what the results show, and searches again', async () => {
    withResults()
    dismissResult('/w/a.txt', 0)
    ipc.replaceInFiles.mockResolvedValue({
      replaced: 2,
      documents: 1,
      files: 1,
      skipped: 0,
      failures: [],
    })
    await replaceResults()
    expect(ipc.replaceInFiles).toHaveBeenCalledWith(query, 'dog', [
      { path: '/w/a.txt', matches: [[2, 4, 7]] },
      { path: '/w/b.txt', matches: [[1, 1, 4]] },
    ])
    expect(notify).toHaveBeenCalledWith('info', 'Replaced 2 matches in 2 files.')
    expect(ipc.searchWorkspace).toHaveBeenCalled()
  })

  it("replaces one file's results, or one match, and says what was skipped", async () => {
    withResults()
    ipc.replaceInFiles.mockResolvedValue({
      replaced: 0,
      documents: 0,
      files: 0,
      skipped: 1,
      failures: [],
    })
    await replaceResults('/w/a.txt', 1)
    expect(ipc.replaceInFiles).toHaveBeenCalledWith(query, 'dog', [
      { path: '/w/a.txt', matches: [[2, 4, 7]] },
    ])
    expect(notify).toHaveBeenCalledWith(
      'info',
      'Replaced 0 matches in 0 files. 1 result had changed since the search and was left alone.',
    )
  })

  it('dismisses a file, or one of its results', () => {
    withResults()
    dismissResult('/w/b.txt')
    dismissResult('/w/a.txt', 1)
    expect(searchStore.get().files).toEqual([
      { path: '/w/a.txt', relative: 'a.txt', matches: [match(0, 0)] },
    ])
  })

  it('previews regex replacements with their groups', () => {
    const regex = { ...query, pattern: String.raw`let (\w+) = (?<value>\d)`, regex: true }
    expect(replacedText('let a = 1', regex, 'const $1 = ${value}')).toBe('const a = 1')
    expect(replacedText('cat', query, '$1')).toBe('$1')
    expect(replacedText('x', { ...regex, pattern: '(' }, 'y')).toBe('y')
  })
})
