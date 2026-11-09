import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { PreviewContent, PreviewElement, PreviewInfo, PreviewNode } from '../ipc/extensions'

vi.mock('../ipc/extensions', () => ({
  getPreviewContent: vi.fn(),
  getPreviewImage: vi.fn(async () => 'data:image/png;base64,AAAA'),
  listPreviews: vi.fn(async () => []),
  previewClosed: vi.fn(async () => {}),
}))
vi.mock('../state/links', () => ({ openLink: vi.fn() }))

const { axeViolations } = await import('../test/axe')
const ipc = await import('../ipc/extensions')
const { openLink } = await import('../state/links')
const { previewChanged } = await import('../state/previews')
const { PreviewPane, slug } = await import('./PreviewPane')

const info: PreviewInfo = {
  id: 'flareite.markdown.preview/1',
  extension: 'Markdown',
  title: 'Preview README.md',
  icon: 'book',
  document: 1,
  webview: false,
}

/** Nodes from `[parent, element]` pairs. */
const content = (
  nodes: [number | null, PreviewElement][],
  highlights: PreviewContent['highlights'] = {},
  highlightsPending = false,
): PreviewContent => ({
  nodes: nodes.map(([parent, element]) => ({ ...element, parent }) as PreviewNode),
  highlights,
  highlightsPending,
})

const text = (value: string): PreviewElement => ({ type: 'text', text: value })

async function show(value: PreviewContent) {
  vi.mocked(ipc.getPreviewContent).mockResolvedValue(value)
  const view = render(<PreviewPane preview={info} />)
  await waitFor(() => expect(ipc.getPreviewContent).toHaveBeenCalled())
  return view
}

describe('PreviewPane', () => {
  beforeEach(() => vi.clearAllMocks())

  it('draws blocks, lists, tables, and code from nodes', async () => {
    const { container } = await show(
      content(
        [
          [null, { type: 'heading', level: 2 }],
          [0, text('Hello, World!')],
          [null, { type: 'numberedList', start: 3 }],
          [2, { type: 'listItem', checked: true }],
          [3, { type: 'strong' }],
          [4, text('done')],
          [null, { type: 'table', alignments: ['none', 'right'] }],
          [6, { type: 'tableHead' }],
          [7, { type: 'tableCell' }],
          [8, text('a')],
          [7, { type: 'tableCell' }],
          [10, text('b')],
          [6, { type: 'tableRow' }],
          [12, { type: 'tableCell' }],
          [12, { type: 'tableCell' }],
          [14, { type: 'code', text: 'x' }],
          [null, { type: 'codeBlock', language: 'rust', text: 'fn main() {}\nplain' }],
        ],
        { 16: [[0, 2, 2], []] },
      ),
    )
    const heading = await screen.findByRole('heading', { level: 2, name: 'Hello, World!' })
    expect(heading.dataset.anchor).toBe('hello-world')
    expect(container.querySelector('ol')!.getAttribute('start')).toBe('3')
    expect(screen.getByRole('checkbox', { name: 'Done' })).toBeChecked()
    expect(screen.getAllByRole('columnheader').map(cell => cell.textContent)).toEqual(['a', 'b'])
    expect(screen.getAllByRole('columnheader')[1]!.style.textAlign).toBe('right')
    expect(screen.getAllByRole('cell')[1]!.innerHTML).toBe('<code>x</code>')
    const code = container.querySelector('pre code')!
    expect(code.querySelector('.tok-keyword')!.textContent).toBe('fn')
    expect(code.textContent).toBe('fn main() {}\nplain')
  })

  it('shows text as text, never as markup', async () => {
    const { container } = await show(
      content([
        [null, { type: 'paragraph' }],
        [0, text('<img src=x onerror=alert(1)><script>alert(2)</script>')],
      ]),
    )
    await screen.findByText(/<script>/)
    expect(container.querySelector('img, script')).toBeNull()
  })

  it('follows links by kind, and shows those it cannot follow as text', async () => {
    const user = userEvent.setup()
    const scrolled = vi.fn()
    Element.prototype.scrollIntoView = scrolled
    await show(
      content([
        [null, { type: 'heading', level: 1 }],
        [0, text('Getting Started')],
        [null, { type: 'paragraph' }],
        [2, { type: 'link', target: { kind: 'web', url: 'https://example.com' }, title: null }],
        [3, text('web')],
        [2, { type: 'link', target: { kind: 'file', path: '/w/a.md', anchor: null }, title: 'A' }],
        [5, text('file')],
        [2, { type: 'link', target: { kind: 'heading', anchor: 'Getting-Started' }, title: null }],
        [7, text('heading')],
        [2, { type: 'link', target: { kind: 'unsupported', destination: 'ftp://x' }, title: null }],
        [9, text('ftp')],
      ]),
    )
    // Through the link handlers, as the terminal's links are.
    await user.click(await screen.findByRole('link', { name: 'web' }))
    expect(openLink).toHaveBeenCalledWith({ kind: 'web', target: 'https://example.com' })
    screen.getByRole('link', { name: 'file' }).focus()
    await user.keyboard('{Enter}')
    expect(openLink).toHaveBeenCalledWith({ kind: 'file', target: '/w/a.md' })
    await user.click(screen.getByRole('link', { name: 'heading' }))
    expect(scrolled).toHaveBeenCalled()
    expect(screen.queryByRole('link', { name: 'ftp' })).toBeNull()
    expect(screen.getByText('ftp').title).toBe('ftp://x')
  })

  it('loads pictures from files through the backend, but not from the web', async () => {
    await show(
      content([
        [null, { type: 'paragraph' }],
        [
          0,
          {
            type: 'image',
            source: { kind: 'file', path: '/w/logo.png' },
            description: 'Logo',
            title: null,
          },
        ],
        [
          0,
          {
            type: 'image',
            source: { kind: 'web', url: 'https://example.com/badge.svg' },
            description: 'Badge',
            title: null,
          },
        ],
      ]),
    )
    // A placeholder with the description shows while it loads.
    await screen.findByRole('img', { name: 'Logo' })
    await waitFor(() =>
      expect(screen.getByRole('img', { name: 'Logo' }).getAttribute('src')).toBe(
        'data:image/png;base64,AAAA',
      ),
    )
    expect(ipc.getPreviewImage).toHaveBeenCalledTimes(1)
    expect(ipc.getPreviewImage).toHaveBeenCalledWith(info.id, '/w/logo.png')
    expect(screen.getByText('Badge').title).toBe('https://example.com/badge.svg')
  })

  it('reloads when its content changes', async () => {
    await show(
      content([
        [null, { type: 'paragraph' }],
        [0, text('first')],
      ]),
    )
    await screen.findByText('first')
    vi.mocked(ipc.getPreviewContent).mockResolvedValue(
      content([
        [null, { type: 'paragraph' }],
        [0, text('second')],
      ]),
    )
    act(() => previewChanged(info.id))
    await screen.findByText('second')
  })

  it('has no accessibility problems', async () => {
    const { container } = await show(
      content([
        [null, { type: 'heading', level: 1 }],
        [0, text('Title')],
        [null, { type: 'bulletedList' }],
        [2, { type: 'listItem', checked: false }],
        [3, { type: 'link', target: { kind: 'heading', anchor: 'title' }, title: null }],
        [4, text('task')],
        [null, { type: 'table', alignments: ['none'] }],
        [6, { type: 'tableHead' }],
        [7, { type: 'tableCell' }],
        [8, text('a')],
        [null, { type: 'paragraph' }],
        [
          10,
          {
            type: 'image',
            source: { kind: 'web', url: 'https://example.com/a.png' },
            description: 'A',
            title: null,
          },
        ],
      ]),
    )
    await screen.findByRole('heading', { name: 'Title' })
    expect(await axeViolations(container)).toEqual([])
  })

  it('fetches once more when code was left plain for lack of time', async () => {
    const code: [number | null, PreviewElement][] = [
      [null, { type: 'codeBlock', language: 'ts', text: 'const x = 1' }],
    ]
    vi.mocked(ipc.getPreviewContent)
      .mockResolvedValueOnce(content(code, {}, true))
      .mockResolvedValueOnce(content(code, { 0: [[0, 5, 2]] }, true))
    const { container } = render(<PreviewPane preview={info} />)
    await waitFor(() => expect(container.querySelector('.tok-keyword')?.textContent).toBe('const'))
    await new Promise(resolve => setTimeout(resolve, 500))
    expect(ipc.getPreviewContent).toHaveBeenCalledTimes(2)
  })

  it('makes anchors as GitHub does', () => {
    expect(slug('Hello, World!')).toBe('hello-world')
    expect(slug('  API: v1.2 (beta) ')).toBe('api-v12-beta')
    expect(slug('Café_au lait')).toBe('café_au-lait')
  })
})
