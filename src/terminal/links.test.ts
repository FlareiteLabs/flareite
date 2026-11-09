import { describe, expect, it } from 'vitest'
import { fileUrlPath, findCandidates, joinPath, promptFolder, resolveCandidates } from './links'

const found = (line: string) =>
  findCandidates(line).map(({ kind, target, line, column }) => ({ kind, target, line, column }))

describe('terminal links', () => {
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

  it('finds relative paths from the shell’s folders', () => {
    expect(joinPath('C:\\code', '..\\other\\a.rs')).toBe('C:\\other\\a.rs')
    expect(joinPath('/home/me/', './a.rs')).toBe('/home/me/a.rs')
    expect(resolveCandidates('src/a.rs', ['C:\\code', 'C:\\code'], null)).toEqual([
      'C:\\code\\src\\a.rs',
    ])
    expect(resolveCandidates('/etc/hosts', ['C:\\code'], null)).toEqual(['/etc/hosts'])
  })

  it('reads the folder from a PowerShell prompt', () => {
    expect(promptFolder('PS C:\\code\\app> cargo build')).toBe('C:\\code\\app')
    expect(promptFolder('just output')).toBeNull()
  })
})
