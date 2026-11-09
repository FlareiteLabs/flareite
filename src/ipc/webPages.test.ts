import { beforeEach, describe, expect, it, vi } from 'vitest'

const calls: { command: string; done: () => void; fail: (error: Error) => void }[] = []

vi.mock('./index', () => ({
  call: (command: string, args: { id: string }) =>
    new Promise<void>((resolve, reject) => {
      calls.push({ command: `${command} ${args.id}`, done: resolve, fail: reject })
    }),
}))

const { closeWebPage, hideWebPage, moveWebPage, showWebPage } = await import('./extensions')

const bounds = { x: 0, y: 0, width: 100, height: 100 }
const sent = () => calls.map(call => call.command)
const settle = () => new Promise(resolve => setTimeout(resolve, 0))

describe('web page calls', () => {
  beforeEach(() => {
    calls.length = 0
  })

  it("sends a view's calls one at a time, in order", async () => {
    // Showing creates the view, which takes a while; the tab is hidden meanwhile.
    const shown = showWebPage('a', 'https://example.com/', bounds)
    const shownAgain = showWebPage('a', 'https://example.com/', bounds)
    const hidden = hideWebPage('a')
    await settle()
    expect(sent()).toEqual(['web_page_show a'])

    calls[0]!.done()
    await shown
    await settle()
    expect(sent()).toEqual(['web_page_show a', 'web_page_show a'])

    calls[1]!.done()
    await shownAgain
    await settle()
    expect(sent()).toEqual(['web_page_show a', 'web_page_show a', 'web_page_hide a'])
    calls[2]!.done()
    await hidden
  })

  it('goes on after a call fails', async () => {
    const shown = showWebPage('b', 'https://example.com/', bounds)
    const closed = closeWebPage('b')
    await settle()
    calls[0]!.fail(new Error('no'))
    await expect(shown).rejects.toThrow('no')
    await settle()
    expect(sent()).toEqual(['web_page_show b', 'web_page_close b'])
    calls[1]!.done()
    await closed
  })

  it("doesn't hold one view's calls for another's", async () => {
    void showWebPage('c', 'https://example.com/', bounds)
    void moveWebPage('d', bounds)
    await settle()
    expect(sent()).toEqual(['web_page_show c', 'web_page_bounds d'])
    calls.forEach(call => call.done())
  })
})
