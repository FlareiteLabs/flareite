import { describe, expect, it } from 'vitest'
import { promptFolder } from './links'

describe('terminal links', () => {
  it('reads the folder from a PowerShell prompt', () => {
    expect(promptFolder('PS C:\\code\\app> cargo build')).toBe('C:\\code\\app')
    expect(promptFolder('just output')).toBeNull()
  })
})
