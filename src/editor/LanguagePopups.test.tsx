import { fireEvent, render, screen } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { PreviewContent, PreviewElement, PreviewNode } from '../ipc/extensions'

const openLink = vi.hoisted(() => vi.fn())
vi.mock('../state/links', async importOriginal => ({
  ...(await importOriginal<typeof import('../state/links')>()),
  openLink,
}))

const { HoverCard } = await import('./LanguagePopups')

/** Nodes from `[parent, element]` pairs. */
const content = (
  nodes: [number | null, PreviewElement][],
  highlights: PreviewContent['highlights'] = {},
): PreviewContent => ({
  nodes: nodes.map(([parent, element]) => ({ ...element, parent }) as PreviewNode),
  highlights,
  highlightsPending: false,
})

const text = (value: string): PreviewElement => ({ type: 'text', text: value })

const card = (hover: Parameters<typeof HoverCard>[0]['hover']) =>
  render(<HoverCard hover={hover} placement={{ top: 0, left: 0 }} onMouseLeave={() => {}} />)

describe('HoverCard', () => {
  beforeEach(() => vi.clearAllMocks())

  it('draws the Markdown with highlighted code, lists, and emphasis', () => {
    const { container } = card({
      diagnostics: [{ severity: 2, message: 'unused', source: 'luau' }],
      content: content(
        [
          [null, { type: 'codeBlock', language: 'luau', text: 'local x = 1' }],
          [null, { type: 'rule' }],
          [null, { type: 'paragraph' }],
          [2, text('Adds ')],
          [2, { type: 'emphasis' }],
          [4, text('two')],
          [null, { type: 'bulletedList' }],
          [6, { type: 'listItem', checked: null }],
          [7, { type: 'code', text: 'first' }],
        ],
        { 0: [[0, 5, 1]] },
      ),
    })
    expect(container.querySelector('[data-severity="warning"]')?.textContent).toBe('unused luau')
    expect(container.querySelector('.preview__code-line > span[class]')?.textContent).toBe('local')
    expect(container.querySelector('hr')).not.toBeNull()
    expect(container.querySelector('p em')?.textContent).toBe('two')
    expect(container.querySelector('ul > li > code')?.textContent).toBe('first')
  })

  it('opens Markdown links with a click, and addresses in text with Ctrl+click', () => {
    card({
      diagnostics: [],
      content: content([
        [null, { type: 'paragraph' }],
        [0, { type: 'link', target: { kind: 'web', url: 'https://docs.example' }, title: null }],
        [1, text('the docs at https://inside.example')],
        [0, text(' or see https://example.com/api for more')],
        [null, { type: 'paragraph' }],
        [4, { type: 'link', target: { kind: 'unsupported', destination: 'Vec' }, title: null }],
        [5, text('Vec')],
      ]),
    })
    fireEvent.click(screen.getByRole('link', { name: /the docs/ }))
    expect(openLink).toHaveBeenLastCalledWith(
      { kind: 'web', target: 'https://docs.example' },
      false,
    )

    // An address in a link's text isn't a link of its own.
    expect(document.querySelectorAll('.linked-text__link')).toHaveLength(1)
    const address = screen.getByText('https://example.com/api')
    expect(address.className).toBe('linked-text__link')
    fireEvent.click(address)
    expect(openLink).toHaveBeenCalledTimes(1)
    fireEvent.click(address, { ctrlKey: true })
    expect(openLink).toHaveBeenLastCalledWith(
      { kind: 'web', target: 'https://example.com/api' },
      false,
    )

    // Links the backend couldn't follow, such as to symbols, are text.
    expect(screen.queryByRole('link', { name: 'Vec' })).toBeNull()
  })
})
