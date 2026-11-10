import { beforeEach, describe, expect, it, vi } from 'vitest'

const existing = new Map<string, 'file' | 'folder'>()
vi.mock('../ipc/extensions', () => ({
  pathKinds: vi.fn(async (paths: string[]) => paths.map(path => existing.get(path) ?? null)),
}))

const { pathKinds } = await import('../ipc/extensions')
const {
  fileUrlPath,
  findCandidates,
  findLinks,
  folderOf,
  forgetPathKinds,
  joinPath,
  resolveCandidates,
} = await import('./linkFinder')

const found = (line: string) =>
  findCandidates(line).map(({ kind, target, line, column }) => ({ kind, target, line, column }))

describe('finding links', () => {
  it('finds web addresses without the punctuation after them', () => {
    expect(found('see https://example.com/a?b=1, or (http://x.org/wiki/Foo_(bar)).')).toEqual([
      { kind: 'web', target: 'https://example.com/a?b=1', line: undefined, column: undefined },
      { kind: 'web', target: 'http://x.org/wiki/Foo_(bar)', line: undefined, column: undefined },
    ])
  })

  it('finds paths with where in the file they point', () => {
    expect(found('error: src/main.rs:12:5: oops')).toEqual([
      { kind: 'path', target: 'src/main.rs', line: 12, column: 5 },
    ])
    expect(found('C:\\code\\app\\Program.cs(30,7): warning')).toEqual([
      { kind: 'path', target: 'C:\\code\\app\\Program.cs', line: 30, column: 7 },
    ])
    expect(found('  Cargo.toml  README.md')).toEqual([
      { kind: 'path', target: 'Cargo.toml', line: undefined, column: undefined },
      { kind: 'path', target: 'README.md', line: undefined, column: undefined },
    ])
  })

  it('leaves out version numbers and plain words', () => {
    expect(found('version 1.2.3 is ready')).toEqual([])
  })

  it('reads file addresses as paths', () => {
    expect(fileUrlPath('file:///C:/code/a%20b.txt')).toBe('C:/code/a b.txt')
    expect(fileUrlPath('file:///home/me/a.txt')).toBe('/home/me/a.txt')
    expect(found('open file:///tmp/x.log now')[0]).toMatchObject({
      kind: 'path',
      target: '/tmp/x.log',
    })
  })

  it('finds relative paths from folders', () => {
    expect(joinPath('C:\\code', '..\\other\\a.rs')).toBe('C:\\other\\a.rs')
    expect(joinPath('/home/me/', './a.rs')).toBe('/home/me/a.rs')
    expect(resolveCandidates('src/a.rs', ['C:\\code', 'C:\\code'], null)).toEqual([
      'C:\\code\\src\\a.rs',
    ])
    expect(resolveCandidates('/etc/hosts', ['C:\\code'], null)).toEqual(['/etc/hosts'])
  })

  it('finds the folder a file is in', () => {
    expect(folderOf('C:\\code\\src\\a.rs')).toBe('C:\\code\\src')
    expect(folderOf('C:\\a.rs')).toBe('C:\\')
    expect(folderOf('/a.rs')).toBe('/')
    expect(folderOf('/home/me/a.rs')).toBe('/home/me')
    expect(folderOf('a.rs')).toBeNull()
  })
})

describe('links in text', () => {
  beforeEach(() => {
    existing.clear()
    forgetPathKinds()
    vi.mocked(pathKinds).mockClear()
  })

  it('links web addresses and paths that lead somewhere', async () => {
    existing.set('/code/src/main.rs', 'file')
    existing.set('/other/docs', 'folder')
    const text = 'see https://example.com, src/main.rs:3:7, ./docs, and gone.txt'
    expect(await findLinks(text, ['/code', '/other'])).toEqual([
      { start: 4, end: 23, link: { kind: 'web', target: 'https://example.com' } },
      {
        start: 25,
        end: 40,
        link: { kind: 'file', target: '/code/src/main.rs', line: 2, column: 6 },
      },
      {
        start: 42,
        end: 48,
        link: { kind: 'file', target: '/other/docs', line: null, column: null },
      },
    ])
  })

  it('asks about the paths of text shown at once in one call, and remembers them', async () => {
    existing.set('/code/a.rs', 'file')
    const [first, second] = await Promise.all([
      findLinks('a.rs', ['/code']),
      findLinks('b.rs and a.rs', ['/code']),
    ])
    expect(first).toHaveLength(1)
    expect(second.map(found => found.link.target)).toEqual(['/code/a.rs'])
    expect(pathKinds).toHaveBeenCalledTimes(1)
    expect(vi.mocked(pathKinds).mock.calls[0]![0]).toEqual(['/code/a.rs', '/code/b.rs'])
    await findLinks('a.rs', ['/code'])
    expect(pathKinds).toHaveBeenCalledTimes(1)
  })

  it("doesn't ask the backend about text without paths", async () => {
    expect(await findLinks('just words, mailto:me@example.com', ['/code'])).toHaveLength(1)
    expect(pathKinds).not.toHaveBeenCalled()
  })
})
