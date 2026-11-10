import { describe, expect, it } from 'vitest'
import { DEFAULT_LAYOUT, parseLayout } from './layout'

describe('parseLayout', () => {
  it('restores valid saved values', () => {
    const dock = { main: 'main', root: { kind: 'group', id: 'main', items: [], active: null } }
    expect(parseLayout({ expanded: ['/a'], minimapVisible: false, zoom: 1.25, dock })).toEqual({
      expanded: ['/a'],
      minimapVisible: false,
      zoom: 1.25,
      dock,
      sections: {},
      windows: {},
    })
  })

  it('restores where windows were, dropping what is not valid', () => {
    const windows = {
      'w1': { bounds: { x: -1200, y: 40, width: 800, height: 600 }, maximized: false },
      'w2': { bounds: { x: 0, y: 0, width: 0, height: 600 }, maximized: true },
      'w3': { bounds: 'here' },
      'not a window': { bounds: null, maximized: false },
      'w4': 'nothing',
    }
    expect(parseLayout({ windows }).windows).toEqual({
      w1: { bounds: { x: -1200, y: 40, width: 800, height: 600 }, maximized: false },
      w2: { bounds: null, maximized: true },
      w3: { bounds: null, maximized: false },
    })
    expect(parseLayout({ windows: [1] }).windows).toEqual({})
  })

  it("restores view containers' sections, dropping what isn't valid", () => {
    const sections = {
      'flareite.git.scm': {
        collapsed: ['flareite.git.commits', 7],
        sizes: { 'flareite.git.changes': 320, 'flareite.git.commits': -1, 'bad': 'x' },
      },
      'broken': 'not an object',
    }
    expect(parseLayout({ sections }).sections).toEqual({
      'flareite.git.scm': {
        collapsed: ['flareite.git.commits'],
        sizes: { 'flareite.git.changes': 320 },
      },
    })
    expect(parseLayout({ sections: [1] }).sections).toEqual({})
  })

  it('falls back to defaults for missing or invalid fields', () => {
    expect(parseLayout({})).toEqual(DEFAULT_LAYOUT)
    expect(parseLayout({ expanded: [1, '/b'], minimapVisible: 'no', zoom: 40 })).toEqual({
      ...DEFAULT_LAYOUT,
      expanded: ['/b'],
    })
  })

  it('keeps the side bar settings of layouts from before the dock', () => {
    expect(
      parseLayout({ sidebarVisible: false, activeView: 'search', sidebarWidth: 300 }).dock,
    ).toEqual({ sidebarVisible: false, activeView: 'search' })
  })

  it('treats anything but an object as no saved layout', () => {
    expect(parseLayout(null)).toEqual(DEFAULT_LAYOUT)
    expect(parseLayout([1, 2])).toEqual(DEFAULT_LAYOUT)
    expect(parseLayout('layout')).toEqual(DEFAULT_LAYOUT)
  })
})
