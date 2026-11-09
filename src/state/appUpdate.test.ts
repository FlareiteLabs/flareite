import { beforeEach, describe, expect, it, vi } from 'vitest'

vi.mock('../ipc/appUpdate', () => ({
  checkAppUpdate: vi.fn(),
  downloadAppUpdate: vi.fn(),
  getAppUpdateStatus: vi.fn(),
  restartToUpdate: vi.fn(() => Promise.resolve(true)),
}))
vi.mock('../ipc', async importOriginal => ({
  ...(await importOriginal<typeof import('../ipc')>()),
  openExternal: vi.fn(() => Promise.resolve()),
  log: vi.fn(),
}))

const ipc = await import('../ipc/appUpdate')
const { CommandError, openExternal } = await import('../ipc')
const { notifications } = await import('./notifications')
const flows = await import('./appUpdate')

type Status = Awaited<ReturnType<typeof ipc.checkAppUpdate>>

const status = (extra: Partial<Status> = {}): Status => ({
  current: '0.1.0-alpha.2',
  updatable: true,
  releasesUrl: 'https://github.com/FlareiteLabs/flareite/releases',
  update: null,
  downloading: false,
  ready: false,
  failed: null,
  ...extra,
})

const update = {
  version: '0.1.0-alpha.3',
  tag: 'v0.1.0-alpha.3',
  prerelease: true,
  url: 'https://github.com/FlareiteLabs/flareite/releases/tag/v0.1.0-alpha.3',
}

const messages = () => notifications.get().map(n => n.message)

describe('Flareite updating itself', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    notifications.set([])
    flows.appUpdateStore.set({ status: null, progress: null })
  })

  it('says so when asked and there is no update', async () => {
    vi.mocked(ipc.checkAppUpdate).mockResolvedValue(status())
    await flows.checkForAppUpdate()
    expect(ipc.checkAppUpdate).toHaveBeenCalledWith(false)
    expect(messages()).toEqual(['Flareite 0.1.0-alpha.2 is the newest version.'])
  })

  it('says nothing at startup unless it finds an update', async () => {
    vi.mocked(ipc.checkAppUpdate).mockResolvedValue(status())
    await flows.checkForAppUpdate(true)
    vi.mocked(ipc.checkAppUpdate).mockRejectedValue(new CommandError('network', 'offline'))
    await flows.checkForAppUpdate(true)
    expect(messages()).toEqual([])
  })

  it('offers an update found, downloads it, and offers to restart', async () => {
    vi.mocked(ipc.checkAppUpdate).mockResolvedValue(status({ update }))
    await flows.checkForAppUpdate(true)
    const offer = notifications.get()[0]
    expect(offer?.message).toBe('Flareite 0.1.0-alpha.3 is available. You have 0.1.0-alpha.2.')
    expect(offer?.sticky).toBe(true)
    expect(offer?.actions?.map(action => action.label)).toEqual(['Download', 'Release Notes'])

    await offer?.actions?.[1]?.run()
    expect(openExternal).toHaveBeenCalledWith(update.url)

    vi.mocked(ipc.downloadAppUpdate).mockImplementation(async onProgress => {
      onProgress({ downloaded: 50, total: 100 })
      expect(flows.appUpdateStore.get().progress).toEqual({ downloaded: 50, total: 100 })
      return status({ update, ready: true })
    })
    await offer?.actions?.[0]?.run()
    expect(flows.appUpdateStore.get().progress).toBeNull()
    const restart = notifications.get().at(-1)
    expect(restart?.message).toBe(
      'Flareite 0.1.0-alpha.3 is downloaded. Restart Flareite to install it.',
    )
    await restart?.actions?.[0]?.run()
    expect(ipc.restartToUpdate).toHaveBeenCalled()
  })

  it("doesn't offer the same update twice unasked", async () => {
    // Offered tags are remembered for the session, so this one is new here.
    const later = { ...update, version: '0.1.0-alpha.4', tag: 'v0.1.0-alpha.4' }
    vi.mocked(ipc.checkAppUpdate).mockResolvedValue(status({ update: later }))
    await flows.checkForAppUpdate(true)
    await flows.checkForAppUpdate(true)
    expect(messages()).toHaveLength(1)
  })

  it('links to the releases when the update has nothing for this installation', async () => {
    vi.mocked(ipc.checkAppUpdate).mockRejectedValue(
      new CommandError('noInstaller', 'The newer release has no update for this installation.'),
    )
    flows.appUpdateStore.set({ status: status(), progress: null })
    await flows.checkForAppUpdate()
    const warning = notifications.get()[0]
    expect(warning?.kind).toBe('warning')
    await warning?.actions?.[0]?.run()
    expect(openExternal).toHaveBeenCalledWith('https://github.com/FlareiteLabs/flareite/releases')
  })

  it('explains that a build not from a release does not update', async () => {
    vi.mocked(ipc.checkAppUpdate).mockRejectedValue(
      new CommandError('notRelease', "This build of Flareite isn't from a release."),
    )
    await flows.checkForAppUpdate()
    expect(notifications.get()[0]?.kind).toBe('info')
  })

  it('reports an update that failed to install, then checks', async () => {
    vi.useFakeTimers()
    try {
      vi.mocked(ipc.getAppUpdateStatus).mockResolvedValue(
        status({ failed: 'Flareite 0.1.0-alpha.3 couldn’t be installed: denied.' }),
      )
      vi.mocked(ipc.checkAppUpdate).mockResolvedValue(status())
      await flows.startAppUpdates()
      expect(notifications.get()[0]?.kind).toBe('error')
      expect(ipc.checkAppUpdate).not.toHaveBeenCalled()
      await vi.runAllTimersAsync()
      expect(ipc.checkAppUpdate).toHaveBeenCalledWith(true)

      // The startup effect runs twice in development; the check runs once.
      await flows.startAppUpdates()
      await vi.runAllTimersAsync()
      expect(ipc.getAppUpdateStatus).toHaveBeenCalledTimes(1)
      expect(ipc.checkAppUpdate).toHaveBeenCalledTimes(1)
    } finally {
      vi.useRealTimers()
    }
  })

  it('restarts only with a downloaded update', async () => {
    await flows.restartToUpdate()
    expect(ipc.restartToUpdate).not.toHaveBeenCalled()
    expect(messages()).toEqual(['No update is downloaded yet. Check for updates first.'])
  })
})
