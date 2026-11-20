import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const save = vi.hoisted(() => vi.fn(() => Promise.resolve(true)))
vi.mock('./documents', async importOriginal => ({
  ...(await importOriginal<typeof import('./documents')>()),
  save,
}))

import { appearance } from './appearance'
import { startAutoSave } from './autoSave'
import { documentsStore, documentVersions, noteVersion } from './documents'

function withDocuments() {
  documentsStore.set({
    ...documentsStore.get(),
    documents: [
      { id: 1, title: 'a.txt', path: '/w/a.txt', dirty: true } as never,
      { id: 2, title: 'Untitled-1', path: null, dirty: true } as never,
      { id: 3, title: 'c.txt', path: '/w/c.txt', dirty: false } as never,
    ],
  })
}

describe('autoSave', () => {
  let stop: () => void
  const before = appearance.get()

  beforeEach(() => {
    vi.useFakeTimers()
    documentVersions.set({})
    withDocuments()
  })

  afterEach(() => {
    stop()
    appearance.set(before)
    vi.useRealTimers()
    vi.clearAllMocks()
  })

  it('saves as is a while after the last change, files only', () => {
    appearance.set({ ...before, autoSave: 'after_delay', autoSaveDelay: 500 })
    stop = startAutoSave()
    noteVersion(1, 2)
    noteVersion(2, 2)
    vi.advanceTimersByTime(300)
    noteVersion(1, 3)
    vi.advanceTimersByTime(300)
    expect(save).not.toHaveBeenCalled()
    vi.advanceTimersByTime(200)
    expect(save).toHaveBeenCalledTimes(1)
    expect(save).toHaveBeenCalledWith(1, false, false)
  })

  it('saves a document when its editor loses focus', () => {
    appearance.set({ ...before, autoSave: 'on_focus_change' })
    stop = startAutoSave()
    const input = document.createElement('textarea')
    input.setAttribute('data-editor-input', '')
    input.dataset.document = '1'
    document.body.append(input)
    input.dispatchEvent(new FocusEvent('focusout', { bubbles: true }))
    expect(save).toHaveBeenCalledWith(1, false, true)
    input.remove()
  })

  it('saves every document with a file when the window loses focus', () => {
    appearance.set({ ...before, autoSave: 'on_window_change' })
    stop = startAutoSave()
    window.dispatchEvent(new FocusEvent('blur'))
    expect(save.mock.calls).toEqual([[1, false, true]])
  })

  it('does nothing when off', () => {
    appearance.set({ ...before, autoSave: 'off' })
    stop = startAutoSave()
    noteVersion(1, 5)
    window.dispatchEvent(new FocusEvent('blur'))
    vi.advanceTimersByTime(5000)
    expect(save).not.toHaveBeenCalled()
  })
})
