import { describe, expect, it } from 'vitest'
import type { LayoutSpec } from '../ipc/extensions'
import { documentsOf, isAddress, layoutItem, panesOf, parseLayoutItem } from './layouts'

const spec: LayoutSpec = {
  title: 'a.rs (Working Tree)',
  nodes: [
    { id: 'sides', parent: null, size: 1, kind: 'split', direction: 'row' },
    {
      id: 'original',
      parent: 'sides',
      size: 1,
      kind: 'pane',
      content: {
        type: 'editor',
        uri: 'git::a.rs',
        minimap: false,
        folding: false,
        quickDiff: false,
      },
      header: null,
      scrollGroup: 'sides',
    },
    {
      id: 'modified',
      parent: 'sides',
      size: 1,
      kind: 'pane',
      content: {
        type: 'editor',
        uri: 'C:\\repo\\a.rs',
        minimap: false,
        folding: false,
        quickDiff: false,
      },
      header: null,
      scrollGroup: 'sides',
    },
  ],
  documents: ['git:HEAD:a.rs', 'git::a.rs'],
  buttons: [],
  breakpoints: [900],
  main: 'modified',
}

describe('layout tabs', () => {
  it('name their layout and key in the dock item', () => {
    const tab = { layout: 'flareite.git.diff', key: '["git::a.rs","C:\\\\a.rs","a"]' }
    expect(parseLayoutItem(layoutItem(tab))).toEqual(tab)
    expect(parseLayoutItem('layout:not json')).toBeNull()
    expect(parseLayoutItem('document:1')).toBeNull()
  })

  it('keep their editors’ documents and the others open, once each', () => {
    expect(panesOf(spec).map(([id]) => id)).toEqual(['original', 'modified'])
    expect(documentsOf(spec)).toEqual(['git::a.rs', 'C:\\repo\\a.rs', 'git:HEAD:a.rs'])
  })

  it('tell addresses from paths', () => {
    expect(isAddress('git:HEAD:a.rs')).toBe(true)
    expect(isAddress('C:\\repo\\a.rs')).toBe(false)
    expect(isAddress('/home/a.rs')).toBe(false)
  })
})
