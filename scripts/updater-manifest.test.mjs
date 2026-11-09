import { describe, expect, it } from 'vitest'
import { manifest, signedVersion } from './updater-manifest.mjs'

/** A signature as `tauri signer` writes one: base64 of minisign's text. */
const sign = (file, version) =>
  Buffer.from(
    [
      'untrusted comment: signature from tauri secret key',
      'RUSsignature=',
      `trusted comment: timestamp:1794258766\tfile:${file}${version ? `\tversion:${version}` : ''}`,
      'globalsignature==',
      '',
    ].join('\n'),
  ).toString('base64')

const installers = [
  'Flareite_0.1.0_x64-setup.exe',
  'Flareite_0.1.0_x64_en-US.msi',
  'Flareite_universal.app.tar.gz',
  'Flareite_0.1.0_amd64.AppImage',
  'Flareite_0.1.0_amd64.deb',
  'Flareite-0.1.0-1.x86_64.rpm',
]

const build = (tag, names = installers, version = '0.1.0') =>
  manifest({
    tag,
    repository: 'FlareiteLabs/flareite',
    signatures: names.map(name => ({ name, signature: `${sign(name, version)}\n` })),
    date: new Date('2026-11-09T12:00:00Z'),
  })

describe('the update manifest', () => {
  it('has an entry for every kind of installation the release workflow builds', () => {
    const result = build('v0.1.0-alpha.3')
    expect(result.version).toBe('0.1.0')
    expect(result.pub_date).toBe('2026-11-09T12:00:00.000Z')
    expect(Object.keys(result.platforms).sort()).toEqual([
      'darwin-aarch64',
      'darwin-aarch64-app',
      'darwin-x86_64',
      'darwin-x86_64-app',
      'linux-x86_64',
      'linux-x86_64-appimage',
      'linux-x86_64-deb',
      'linux-x86_64-rpm',
      'windows-x86_64',
      'windows-x86_64-msi',
      'windows-x86_64-nsis',
    ])
    expect(result.platforms['windows-x86_64']).toBe(result.platforms['windows-x86_64-msi'])
    expect(result.platforms['windows-x86_64-nsis']).toEqual({
      signature: sign('Flareite_0.1.0_x64-setup.exe', '0.1.0'),
      url: 'https://github.com/FlareiteLabs/flareite/releases/download/v0.1.0-alpha.3/Flareite_0.1.0_x64-setup.exe',
    })
  })

  it('reads the version a signature records', () => {
    expect(signedVersion(sign('a.msi', '0.1.0'))).toBe('0.1.0')
    expect(signedVersion(sign('a.msi', null))).toBeUndefined()
  })

  it('refuses installers signed as another version', () => {
    expect(() => build('v0.2.0', installers, '0.1.0')).toThrow(/signed as 0.1.0, not 0.2.0/)
    expect(build('v0.2.0', installers, null).version).toBe('0.2.0')
  })

  it('refuses a release without signed installers, or two for one platform', () => {
    expect(() => build('v0.1.0', ['notes.txt'])).toThrow(/no signed installers/)
    expect(() => build('v0.1.0', ['A_x64.msi', 'B_x64.msi'])).toThrow(/two installers/)
    expect(() => build('nightly')).toThrow(/doesn't name a version/)
  })

  it('names architectures as the updater does', () => {
    const result = build('v0.1.0', [
      'Flareite_0.1.0_arm64-setup.exe',
      'Flareite_aarch64.app.tar.gz',
      'Flareite_0.1.0_aarch64.AppImage',
    ])
    expect(Object.keys(result.platforms).sort()).toEqual([
      'darwin-aarch64',
      'darwin-aarch64-app',
      'linux-aarch64',
      'linux-aarch64-appimage',
      'windows-aarch64-nsis',
    ])
  })
})
