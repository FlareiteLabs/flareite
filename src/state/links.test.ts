import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { LinkHandlerContribution } from '../ipc/extensions'

vi.mock('../ipc/extensions', () => ({
  openLinkWith: vi.fn(async () => {}),
  pathKinds: vi.fn(async () => ['file']),
  openFolderExternal: vi.fn(async () => {}),
}))
vi.mock('../ipc', () => ({ openExternal: vi.fn(async () => {}) }))
vi.mock('./documents', () => ({ openPath: vi.fn(async () => null) }))

const handlers: LinkHandlerContribution[] = []
vi.mock('./extensions', async importOriginal => ({
  ...(await importOriginal<typeof import('./extensions')>()),
  usableExtensions: () => [{ contributions: { linkHandlers: handlers } }],
}))

const ipc = await import('../ipc/extensions')
const { openExternal } = await import('../ipc')
const { openPath } = await import('./documents')
const { handlersFor, linkSettings, openLink } = await import('./links')

const handler = (
  fullId: string,
  kind: 'web' | 'file',
  match: string[],
): LinkHandlerContribution => ({
  id: fullId.split('.').pop()!,
  fullId,
  title: fullId,
  kind,
  schemes: kind === 'web' ? match : [],
  patterns: kind === 'file' ? match : [],
  command: `${fullId}.open`,
})

describe('opening links', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    handlers.length = 0
    linkSettings.set({ web: 'auto', file: 'auto' })
  })

  it('opens web links with the browser and files in the editor by default', async () => {
    openLink({ kind: 'web', target: 'https://example.com' })
    expect(openExternal).toHaveBeenCalledWith('https://example.com')
    openLink({ kind: 'file', target: 'C:\\code\\a.rs', line: 11, column: 4 })
    await vi.waitFor(() =>
      expect(openPath).toHaveBeenCalledWith('C:\\code\\a.rs', {
        start: { line: 11, column: 4 },
      }),
    )
  })

  it('opens folders in the file manager', async () => {
    vi.mocked(ipc.pathKinds).mockResolvedValueOnce(['folder'])
    openLink({ kind: 'file', target: 'C:\\code' })
    await vi.waitFor(() => expect(ipc.openFolderExternal).toHaveBeenCalledWith('C:\\code'))
  })

  it('lets an installed handler take over the links it declares', () => {
    handlers.push(handler('example.web.tab', 'web', ['https']))
    expect(handlersFor({ kind: 'web', target: 'http://example.com' })).toEqual([])
    openLink({ kind: 'web', target: 'https://example.com' })
    expect(ipc.openLinkWith).toHaveBeenCalledWith('example.web.tab', {
      kind: 'web',
      target: 'https://example.com',
      line: null,
      column: null,
    })
    expect(openExternal).not.toHaveBeenCalled()
  })

  it('follows the setting: Flareite’s own way, or a chosen handler', () => {
    handlers.push(handler('a.web.one', 'web', ['https']), handler('b.web.two', 'web', ['https']))
    linkSettings.set({ web: 'default', file: 'auto' })
    openLink({ kind: 'web', target: 'https://example.com' })
    expect(openExternal).toHaveBeenCalled()
    linkSettings.set({ web: 'b.web.two', file: 'auto' })
    openLink({ kind: 'web', target: 'https://example.com' })
    expect(ipc.openLinkWith).toHaveBeenCalledWith('b.web.two', expect.anything())
  })

  it('matches file handlers by name', () => {
    handlers.push(handler('x.files.images', 'file', ['*.png']))
    expect(handlersFor({ kind: 'file', target: '/a/logo.PNG' })).toHaveLength(1)
    expect(handlersFor({ kind: 'file', target: '/a/main.rs' })).toHaveLength(0)
  })
})
