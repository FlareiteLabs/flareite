import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'

const openLink = vi.hoisted(() => vi.fn())
vi.mock('../state/links', async importOriginal => ({
  ...(await importOriginal<typeof import('../state/links')>()),
  openLink,
}))

const { renderMarkdown } = await import('./markdown')

const html = (markdown: string) => render(<div>{renderMarkdown(markdown)}</div>).container.innerHTML

describe('renderMarkdown', () => {
  it('renders code blocks, rules and paragraphs', () => {
    expect(html('```rust\nfn main()\n```\n---\nRuns the\nprogram.')).toBe(
      '<div><pre>fn main()</pre><hr><p>Runs the program.</p></div>',
    )
  })

  it('renders inline code, bold, and links to other places as text', () => {
    expect(html('Use `Vec::new` or **see** [docs](std::vec::Vec) \\*now\\*')).toBe(
      '<div><p>Use <code>Vec::new</code> or <strong>see</strong> docs *now*</p></div>',
    )
  })

  it('opens links to web and file addresses with a click', () => {
    render(
      <div>
        {renderMarkdown('[docs](https://example.com/a "Docs") and [src](file:///C:/code/a.rs#L12)')}
      </div>,
    )
    fireEvent.click(screen.getByRole('link', { name: 'docs' }))
    expect(openLink).toHaveBeenLastCalledWith(
      { kind: 'web', target: 'https://example.com/a' },
      false,
    )
    fireEvent.click(screen.getByRole('link', { name: 'src' }), { ctrlKey: true, shiftKey: true })
    expect(openLink).toHaveBeenLastCalledWith(
      { kind: 'file', target: 'C:/code/a.rs', line: 11 },
      true,
    )
  })

  it('never interprets HTML', () => {
    expect(html('<img src=x onerror=alert(1)>')).toBe(
      '<div><p>&lt;img src=x onerror=alert(1)&gt;</p></div>',
    )
  })
})
