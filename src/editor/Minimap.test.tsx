import { act, fireEvent, render, waitFor } from '@testing-library/react'
import { useRef } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { MinimapView } from '../ipc/editor'
import { Minimap } from './Minimap'

// The editor: 100 lines of 20 px with room to scroll past the last, in a
// 400 px view with a 10 px scrollbar. In the minimap, lines are 2 px.
const size = {
  scrollTop: 0,
  scrollHeight: 2380,
  clientHeight: 400,
  offsetWidth: 600,
  clientWidth: 590,
}

function lines(version: number, startRow: number, endRow: number): MinimapView {
  return {
    documentId: 7,
    version,
    rowCount: 100,
    startRow,
    lines: Array.from({ length: Math.min(endRow, 100) - startRow }, () => [0, 4, 0]),
    headers: [],
  }
}

function renderMinimap() {
  // The backend answers with the document's current version.
  let current = 1
  const fetchLines = vi.fn((_id: number, _view: number, start: number, end: number) =>
    Promise.resolve(lines(current, start, end)),
  )
  let editor: HTMLDivElement | null = null
  function Harness({ version }: { version: number }) {
    const scroller = useRef<HTMLDivElement | null>(null)
    return (
      <>
        <div
          ref={element => {
            if (element && element !== editor) {
              for (const [key, value] of Object.entries(size)) {
                Object.defineProperty(element, key, { configurable: true, writable: true, value })
              }
              element.scrollBy = vi.fn()
              editor = element
            }
            scroller.current = element
          }}
        />
        <Minimap
          documentId={7}
          version={version}
          rowCount={100}
          refreshTick={0}
          scroller={scroller}
          lineHeight={20}
          fetchLines={fetchLines}
        />
      </>
    )
  }
  const result = render(<Harness version={current} />)
  const minimap = result.container.querySelector('.minimap') as HTMLElement
  return {
    ...result,
    fetchLines,
    minimap,
    editor: () => editor as unknown as HTMLDivElement & { scrollBy: ReturnType<typeof vi.fn> },
    rerenderVersion: (next: number) => {
      current = next
      result.rerender(<Harness version={next} />)
    },
  }
}

describe('Minimap', () => {
  it('sits beside the scrollbar, hidden from assistive technology', async () => {
    const { minimap, fetchLines } = renderMinimap()
    expect(minimap).toHaveAttribute('aria-hidden', 'true')
    expect(minimap.style.right).toBe('10px')
    expect(minimap.style.height).toBe('400px')
    // 400 px shows 201 lines; the document has 100, all of them fetched.
    await waitFor(() => expect(fetchLines).toHaveBeenCalledWith(7, 0, 0, 100))
  })

  it('fetches again after an edit, at most once per throttle period', async () => {
    const { fetchLines, rerenderVersion } = renderMinimap()
    await waitFor(() => expect(fetchLines).toHaveBeenCalledTimes(1))
    rerenderVersion(2)
    rerenderVersion(3)
    await waitFor(() => expect(fetchLines).toHaveBeenCalledTimes(2))
    await act(() => new Promise(resolve => setTimeout(resolve, 150)))
    expect(fetchLines).toHaveBeenCalledTimes(2)
  })

  it('centers the clicked line and scrolls by dragging', async () => {
    const { minimap, editor } = renderMinimap()
    // y = 101 is line 50, below the slider (0 to 40 px): center it.
    fireEvent.mouseDown(minimap, { button: 0, clientY: 101 })
    expect(editor().scrollTop).toBe(1000 + 10 - 200)
    fireEvent.mouseUp(window)

    // Dragging the slider: 10 px in the minimap is 100 px in the editor.
    editor().scrollTop = 0
    fireEvent.mouseDown(minimap, { button: 0, clientY: 20 })
    expect(minimap).toHaveAttribute('data-dragging')
    fireEvent.mouseMove(window, { clientY: 30 })
    expect(editor().scrollTop).toBe(100)
    fireEvent.mouseUp(window)
    expect(minimap).not.toHaveAttribute('data-dragging')
  })

  it('passes the wheel on to the editor', () => {
    const { minimap, editor } = renderMinimap()
    fireEvent.wheel(minimap, { deltaY: 3, deltaMode: 1 })
    expect(editor().scrollBy).toHaveBeenCalledWith({ top: 60, left: 0 })
  })
})
