import { act, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'

const ipc = vi.hoisted(() => ({
  replaceInFiles: vi.fn(() =>
    Promise.resolve({ replaced: 2, documents: 0, files: 2, skipped: 0, failures: [] }),
  ),
  searchWorkspace: vi.fn(() => Promise.resolve()),
  cancelSearch: vi.fn(() => Promise.resolve()),
}))
vi.mock('../ipc/workspace', async importOriginal => ({
  ...(await importOriginal<typeof import('../ipc/workspace')>()),
  ...ipc,
}))

import { searchStore } from '../state/search'
import { workspaceStore } from '../state/workspace'
import { SearchPanel } from './SearchPanel'

const match = (line: number) => ({
  line,
  startColumn: 0,
  endColumn: 3,
  preview: 'cat food',
  previewStart: 0,
  previewEnd: 3,
})

describe('SearchPanel', () => {
  afterEach(() => vi.clearAllMocks())

  it('previews replacements and replaces all once confirmed', async () => {
    workspaceStore.set({
      ...workspaceStore.get(),
      info: { root: '/w' } as never,
    })
    searchStore.set({
      ...searchStore.get(),
      query: { pattern: 'cat', regex: false, caseSensitive: false, wholeWord: false },
      showReplace: true,
      replacement: 'dog',
      files: [
        { path: '/w/a.txt', relative: 'a.txt', matches: [match(0)] },
        { path: '/w/b/c.txt', relative: 'b/c.txt', matches: [match(4)] },
      ],
    })
    render(<SearchPanel />)
    expect(screen.getAllByText('dog')).toHaveLength(2)

    fireEvent.click(screen.getByRole('button', { name: 'Replace All' }))
    expect(screen.getByText('Replace 2 matches across 2 files with “dog”?')).toBeInTheDocument()
    expect(ipc.replaceInFiles).not.toHaveBeenCalled()
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: 'Replace' }))
    })
    expect(ipc.replaceInFiles).toHaveBeenCalledWith(
      expect.objectContaining({ pattern: 'cat' }),
      'dog',
      [
        { path: '/w/a.txt', matches: [[0, 0, 3]] },
        { path: '/w/b/c.txt', matches: [[4, 0, 3]] },
      ],
    )
  })

  it('renders only the rows near the screen, however many results there are', () => {
    workspaceStore.set({ ...workspaceStore.get(), info: { root: '/w' } as never })
    searchStore.set({
      ...searchStore.get(),
      query: { pattern: 'cat', regex: true, caseSensitive: false, wholeWord: false },
      showReplace: true,
      replacement: 'dog',
      files: Array.from({ length: 100 }, (_, file) => ({
        path: `/w/${file}.txt`,
        relative: `${file}.txt`,
        matches: Array.from({ length: 200 }, (_, line) => match(line)),
      })),
    })
    const { container } = render(<SearchPanel />)
    const rows = container.querySelectorAll('.search-results__row')
    expect(rows.length).toBeLessThan(100)
    // The list is as tall as all 20,100 rows.
    const list = container.querySelector<HTMLElement>('.search-results__rows')!
    expect(list.style.height).toBe(`${20_100 * 22}px`)
  })

  it('shows the boxes for files to include and exclude', () => {
    workspaceStore.set({ ...workspaceStore.get(), info: { root: '/w' } as never })
    render(<SearchPanel />)
    expect(screen.queryByLabelText('files to include')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'Toggle Search Details' }))
    fireEvent.change(screen.getByLabelText('files to include'), { target: { value: 'src' } })
    expect(searchStore.get().scope.include).toBe('src')
  })
})
