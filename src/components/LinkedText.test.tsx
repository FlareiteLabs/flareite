import { fireEvent, render, screen } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const pathKinds = vi.hoisted(() =>
  vi.fn(async (paths: string[]) => paths.map(path => (path.endsWith('a.rs') ? 'file' : null))),
)
vi.mock('../ipc/extensions', async importOriginal => ({
  ...(await importOriginal<typeof import('../ipc/extensions')>()),
  pathKinds,
}))
const openLink = vi.hoisted(() => vi.fn())
vi.mock('../state/links', async importOriginal => ({
  ...(await importOriginal<typeof import('../state/links')>()),
  openLink,
}))

const { forgetPathKinds } = await import('../state/linkFinder')
const { LinkedText } = await import('./LinkedText')

describe('LinkedText', () => {
  beforeEach(() => {
    forgetPathKinds()
    vi.clearAllMocks()
  })

  it('links web addresses at once and paths that lead somewhere', async () => {
    const { container } = render(
      <p>
        <LinkedText text="see https://example.com, /code/a.rs:4 and /code/b.rs" />
      </p>,
    )
    expect(container.querySelectorAll('.linked-text__link')).toHaveLength(1)
    const path = await screen.findByText('/code/a.rs:4')
    expect(container.textContent).toBe('see https://example.com, /code/a.rs:4 and /code/b.rs')
    expect(container.querySelectorAll('.linked-text__link')).toHaveLength(2)
    expect(screen.getByText('/code/b.rs', { exact: false }).className).not.toBe('linked-text__link')

    // A plain click is for selecting; Ctrl+click opens, with Shift to choose.
    fireEvent.click(path)
    expect(openLink).not.toHaveBeenCalled()
    fireEvent.click(path, { ctrlKey: true })
    expect(openLink).toHaveBeenLastCalledWith(
      { kind: 'file', target: '/code/a.rs', line: 3, column: null },
      false,
    )
    fireEvent.click(screen.getByText('https://example.com'), { ctrlKey: true, shiftKey: true })
    expect(openLink).toHaveBeenLastCalledWith({ kind: 'web', target: 'https://example.com' }, true)
  })

  it('checks the paths of many lines in one call', async () => {
    render(
      <div>
        <LinkedText text="error at /code/a.rs:1" />
        <LinkedText text="error at /code/x/a.rs:2" />
        <LinkedText text="no paths here" />
      </div>,
    )
    await screen.findByText('/code/x/a.rs:2')
    expect(pathKinds).toHaveBeenCalledTimes(1)
  })
})
