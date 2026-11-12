import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { TerminalMessage } from '../ipc/terminal'
import { FakeTerminal } from '../test/fakeXterm'

vi.mock('../ipc/terminal', () => ({
  createTerminal: vi.fn(),
  listTerminals: vi.fn(),
  attachTerminal: vi.fn(),
  writeTerminal: vi.fn(),
  resizeTerminal: vi.fn(),
  acknowledgeTerminal: vi.fn(),
  killTerminal: vi.fn(),
  terminalPlatform: vi.fn(async () => ({ windowsBuild: 26200 })),
}))

const ipc = await import('../ipc/terminal')
const terminals = await import('./terminals')
const { ACKNOWLEDGE_EVERY } = await import('./session')
const { applySettings } = await import('../state/appearance')

/** Each terminal's channel, by id, as the backend would hold it. */
const channels = new Map<number, (message: TerminalMessage) => void>()
let nextId = 1

const xtermOf = (id: number) => terminals.getSession(id)!.xterm as unknown as FakeTerminal
const tabIds = () => terminals.terminalsStore.get().tabs.map(tab => tab.id)

describe('terminals', () => {
  beforeEach(() => {
    for (const id of tabIds()) {
      terminals.closeTerminal(id)
    }
    vi.clearAllMocks()
    channels.clear()
    vi.mocked(ipc.createTerminal).mockImplementation(async (_cols, _rows, _profile, onMessage) => {
      const id = nextId++
      channels.set(id, onMessage)
      return { id, title: 'pwsh' }
    })
    vi.mocked(ipc.listTerminals).mockResolvedValue([])
    for (const command of [
      ipc.attachTerminal,
      ipc.writeTerminal,
      ipc.resizeTerminal,
      ipc.acknowledgeTerminal,
      ipc.killTerminal,
    ]) {
      vi.mocked(command).mockResolvedValue(undefined)
    }
  })

  it('starts a terminal and connects it', async () => {
    const id = await terminals.newTerminal()
    expect(terminals.terminalsStore.get()).toEqual({
      tabs: [{ id, title: 'pwsh', description: null, exited: false }],
      activeId: id,
    })
    expect(ipc.createTerminal).toHaveBeenCalledWith(80, 24, null, expect.any(Function))
    // ConPTY redraws on resize itself; xterm.js must know, or lines repeat.
    expect(xtermOf(id).options.windowsPty).toEqual({ backend: 'conpty', buildNumber: 26200 })

    channels.get(id)!({ type: 'output', data: 'PS C:\\> ' })
    expect(xtermOf(id).text()).toBe('PS C:\\> ')
    xtermOf(id).type('dir\r')
    expect(ipc.writeTerminal).toHaveBeenCalledWith(id, 'dir\r')

    xtermOf(id).setTitle('Administrator: pwsh')
    expect(terminals.terminalsStore.get().tabs[0]!.description).toBe('Administrator: pwsh')
  })

  it('sends replies made before the backend named the terminal once it has', async () => {
    // ConPTY asks for the cursor position as soon as it starts.
    vi.mocked(ipc.createTerminal).mockImplementationOnce(
      async (_cols, _rows, _profile, onMessage) => {
        onMessage({ type: 'output', data: '\x1b[6n' })
        expect(ipc.writeTerminal).not.toHaveBeenCalled()
        return { id: 99, title: 'pwsh' }
      },
    )
    await terminals.newTerminal()
    expect(ipc.writeTerminal).toHaveBeenCalledWith(99, '\x1b[1;1R')
  })

  it('acknowledges drawn output in batches', async () => {
    const id = await terminals.newTerminal()
    const send = channels.get(id)!
    const half = Math.ceil(ACKNOWLEDGE_EVERY / 2)
    send({ type: 'output', data: 'x'.repeat(half) })
    expect(ipc.acknowledgeTerminal).not.toHaveBeenCalled()
    send({ type: 'output', data: 'y'.repeat(half) })
    expect(ipc.acknowledgeTerminal).toHaveBeenCalledExactlyOnceWith(id, half * 2)
  })

  it('reattaches to running terminals without answering replayed queries', async () => {
    vi.mocked(ipc.listTerminals).mockResolvedValue([{ id: 7, title: 'bash' }])
    vi.mocked(ipc.attachTerminal).mockImplementation(async (_id, onMessage) => {
      onMessage({ type: 'replay', data: '\x1b[6n$ ls\r\nREADME.md\r\n$ ' })
    })
    await terminals.restoreTerminals()

    expect(tabIds()).toEqual([7])
    expect(xtermOf(7).text()).toContain('README.md')
    expect(ipc.writeTerminal).not.toHaveBeenCalled()
    expect(ipc.createTerminal).not.toHaveBeenCalled()

    // Live queries are answered.
    vi.mocked(ipc.attachTerminal).mock.calls[0]![1]({ type: 'output', data: '\x1b[6n' })
    expect(ipc.writeTerminal).toHaveBeenCalledWith(7, '\x1b[1;1R')
  })

  it('restores once when asked twice at the same time', async () => {
    vi.mocked(ipc.listTerminals).mockResolvedValue([{ id: 8, title: 'bash' }])
    await Promise.all([terminals.restoreTerminals(), terminals.restoreTerminals()])
    expect(tabIds()).toEqual([8])
    expect(ipc.attachTerminal).toHaveBeenCalledOnce()
  })

  it('forgets a terminal whose shell exits cleanly', async () => {
    const first = await terminals.newTerminal()
    const second = await terminals.newTerminal()
    terminals.setActiveTerminal(first)

    channels.get(first)!({ type: 'exited', code: 0 })
    expect(tabIds()).toEqual([second])
    expect(ipc.killTerminal).toHaveBeenCalledWith(first)
    expect(xtermOf(second).disposed).toBe(false)
    // The last used one is gone: the most recent remaining takes its place.
    expect(terminals.terminalsStore.get().activeId).toBe(second)
  })

  it('keeps a terminal whose shell failed until a key is pressed', async () => {
    const id = await terminals.newTerminal()
    const xterm = xtermOf(id)
    channels.get(id)!({ type: 'output', data: 'error: bad flag\r\n' })
    channels.get(id)!({ type: 'exited', code: 2 })

    expect(terminals.terminalsStore.get().tabs[0]!.exited).toBe(true)
    expect(xterm.text()).toMatch(/error: bad flag\r\n.*exited with code 2/s)

    xterm.type('q')
    expect(ipc.writeTerminal).not.toHaveBeenCalled()
    expect(tabIds()).toEqual([])
    expect(xterm.disposed).toBe(true)
  })

  it('reports a shell that cannot start and leaves nothing behind', async () => {
    vi.mocked(ipc.createTerminal).mockRejectedValueOnce(new Error("Couldn't start nope"))
    await expect(terminals.newTerminal()).rejects.toThrow('nope')
    expect(tabIds()).toEqual([])
    expect(FakeTerminal.instances.at(-1)!.disposed).toBe(true)
    expect(terminals.startingTerminals.get()).toBe(0)
  })

  it('clears the last used terminal', async () => {
    const id = await terminals.newTerminal()
    terminals.clearTerminal()
    expect(xtermOf(id).cleared).toBe(true)
  })

  it('applies font and scrollback settings to open terminals', async () => {
    const id = await terminals.newTerminal()
    applySettings({
      theme: 'dark',
      iconTheme: 'default',
      colors: {},
      keybindings: {},
      fontSize: 18,
      fontFamily: '',
      tabSize: 4,
      terminalScrollback: 5000,
      terminalScreenReader: true,
      minimapRegionHeaders: true,
      minimapMarkHeaders: true,
      wordWrap: 'off',
      wordWrapColumn: 80,
      wrappingIndent: 'same',
      openWebLinks: 'auto',
      openFileLinks: 'auto',
    })
    const { options } = xtermOf(id)
    expect(options.fontSize).toBe(18)
    expect(options.scrollback).toBe(5000)
    expect(options.screenReaderMode).toBe(true)
  })
})
