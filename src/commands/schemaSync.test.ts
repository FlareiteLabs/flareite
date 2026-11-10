import { describe, expect, it } from 'vitest'
import type { Command } from './registry'
import { defaultKeys, schemaCommands } from './schemaSync'

const command = (overrides: Partial<Command> & Pick<Command, 'id'>): Command => ({
  title: 'Title',
  run: () => {},
  ...overrides,
})

describe('commands for the settings schema', () => {
  it("lists each command's label and default keys on this platform", () => {
    const save = command({
      id: 'file.save',
      title: 'Save',
      category: 'File',
      keybinding: [{ default: 'ctrl+s', mac: 'mod+s' }, 'f2'],
    })
    expect(defaultKeys(save, false)).toEqual(['ctrl+s', 'f2'])
    expect(defaultKeys(save, true)).toEqual(['mod+s', 'f2'])
    expect(defaultKeys(command({ id: 'x' }), false)).toEqual([])
    expect(schemaCommands([command({ id: 'z.last', title: 'Last' }), save], false)).toEqual([
      { id: 'file.save', label: 'File: Save', keys: ['ctrl+s', 'f2'] },
      { id: 'z.last', label: 'Last', keys: [] },
    ])
  })
})
