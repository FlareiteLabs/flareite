import { describe, expect, it } from 'vitest'
import {
  allItems,
  emptyGroup,
  findGroup,
  groupOf,
  groups,
  isVisible,
  neighbourInDirection,
  parseDock,
  placeItem,
  removeItem,
  resizeSplit,
  setHidden,
  sidebarGroups,
  windowOfGroup,
  windowOfItem,
  type DockLayout,
  type DockNode,
} from './model'

/** Views on the left, the main group on the right. */
const classic = (): DockLayout => ({
  main: 'main',
  root: {
    kind: 'split',
    id: 's1',
    direction: 'row',
    sizes: [0.25, 0.75],
    children: [
      { kind: 'group', id: 'g1', items: ['view:explorer', 'view:search'], active: 'view:explorer' },
      { kind: 'group', id: 'main', items: ['document:1', 'document:2'], active: 'document:2' },
    ],
  },
})

/** A compact picture of the tree: `row(g1[a,b] main[c])`. */
function shape(node: DockNode): string {
  if (node.kind === 'group') {
    const items = node.items.map(item => item.split(':')[1]).join(',')
    return `${node.id}${node.hidden ? '(hidden)' : ''}[${items}]`
  }
  return `${node.direction}(${node.children.map(shape).join(' ')})`
}

const sizesOf = (layout: DockLayout) => (layout.root.kind === 'split' ? layout.root.sizes : [1])

describe('dock model', () => {
  it('moves a tab within its group and to another group', () => {
    let layout = placeItem(classic(), 'document:2', { kind: 'tabs', group: 'main', index: 0 })
    expect(shape(layout.root)).toBe('row(g1[explorer,search] main[2,1])')

    layout = placeItem(layout, 'view:search', { kind: 'tabs', group: 'main', index: 1 })
    expect(shape(layout.root)).toBe('row(g1[explorer] main[2,search,1])')
    expect(findGroup(layout, 'main')!.active).toBe('view:search')
  })

  it('removes a group left empty, but never the main group', () => {
    let layout = placeItem(classic(), 'view:explorer', { kind: 'tabs', group: 'main', index: 0 })
    layout = placeItem(layout, 'view:search', { kind: 'tabs', group: 'main', index: 0 })
    // The view group is gone, and the split with it.
    expect(shape(layout.root)).toBe('main[search,explorer,1,2]')

    for (const item of allItems(layout)) {
      layout = removeItem(layout, item)
    }
    expect(shape(layout.root)).toBe('main[]')
  })

  it('splits a group on any side, sharing its space', () => {
    let layout = placeItem(classic(), 'document:1', { kind: 'split', group: 'main', edge: 'right' })
    // Same direction as the root split: a new sibling taking half of main's space.
    expect(shape(layout.root)).toBe('row(g1[explorer,search] main[2] g2[1])')
    expect(sizesOf(layout)).toEqual([0.25, 0.375, 0.375])

    layout = placeItem(layout, 'document:2', { kind: 'split', group: 'g2', edge: 'top' })
    // Across: the group becomes a column of two.
    expect(shape(layout.root)).toBe('row(g1[explorer,search] main[] column(g3[2] g2[1]))')
  })

  it('does not split a group by its only tab', () => {
    const layout = placeItem(classic(), 'document:1', {
      kind: 'split',
      group: 'main',
      edge: 'left',
    })
    const once = placeItem(layout, 'document:1', { kind: 'split', group: 'g2', edge: 'bottom' })
    expect(once).toBe(layout)
  })

  it('docks along the edges of the whole layout', () => {
    let layout = placeItem(classic(), 'terminal:1', { kind: 'root', edge: 'bottom' })
    expect(shape(layout.root)).toBe('column(row(g1[explorer,search] main[1,2]) g2[1])')
    expect(sizesOf(layout)).toEqual([0.75, 0.25])

    layout = placeItem(layout, 'view:search', { kind: 'root', edge: 'top' })
    expect(shape(layout.root)).toBe('column(g3[search] row(g1[explorer] main[1,2]) g2[1])')
    expect(sizesOf(layout).reduce((a, b) => a + b)).toBeCloseTo(1)
  })

  it('flattens splits that go the same way as their parent', () => {
    let layout = placeItem(classic(), 'document:1', {
      kind: 'split',
      group: 'main',
      edge: 'bottom',
    })
    expect(shape(layout.root)).toBe('row(g1[explorer,search] column(main[2] g2[1]))')
    // Emptying the column's other group leaves one child: the column goes.
    layout = placeItem(layout, 'document:1', { kind: 'tabs', group: 'g1', index: 2 })
    expect(shape(layout.root)).toBe('row(g1[explorer,search,1] main[2])')
  })

  it('activates neighbours when the active tab is removed', () => {
    const layout = removeItem(classic(), 'document:2')
    expect(findGroup(layout, 'main')!.active).toBe('document:1')
    expect(groupOf(layout, 'document:2')).toBeUndefined()
  })

  it('hides groups but not the main group', () => {
    let layout = setHidden(classic(), 'g1', true)
    expect(findGroup(layout, 'g1')!.hidden).toBe(true)
    expect(isVisible(layout.root)).toBe(true)
    layout = setHidden(layout, 'main', true)
    expect(findGroup(layout, 'main')!.hidden).toBeFalsy()
    // Placing into a hidden group shows it.
    layout = placeItem(layout, 'document:1', { kind: 'tabs', group: 'g1', index: 0 })
    expect(findGroup(layout, 'g1')!.hidden).toBe(false)
  })

  it('finds the side bar: the groups along the left edge, but not the main group', () => {
    const ids = (layout: DockLayout) => sidebarGroups(layout).map(group => group.id)
    expect(ids(classic())).toEqual(['g1'])

    // A view moved to the right leaves it; one stacked in the left column joins it.
    let layout = placeItem(classic(), 'view:explorer', { kind: 'root', edge: 'right' })
    expect(ids(layout)).toEqual(['g1'])
    expect(ids(placeItem(layout, 'view:search', { kind: 'tabs', group: 'g2', index: 1 }))).toEqual(
      [],
    )
    layout = placeItem(classic(), 'view:search', { kind: 'split', group: 'g1', edge: 'bottom' })
    expect(ids(layout)).toEqual(['g1', 'g2'])

    // Docked along the bottom or top, a group spans the window rather than sitting beside it.
    expect(ids(placeItem(classic(), 'terminal:1', { kind: 'root', edge: 'bottom' }))).toEqual([
      'g1',
    ])
    expect(ids(placeItem(classic(), 'view:search', { kind: 'root', edge: 'top' }))).toEqual(['g1'])

    // Only the main group on the left: no side bar.
    const mainFirst = removeItem(removeItem(classic(), 'view:explorer'), 'view:search')
    expect(ids(mainFirst)).toEqual([])
  })

  it('keeps hidden groups in the side bar, joined by what is beside them', () => {
    const ids = (layout: DockLayout) => sidebarGroups(layout).map(group => group.id)
    const terminalsLeft = placeItem(classic(), 'terminal:1', { kind: 'root', edge: 'left' })
    expect(ids(terminalsLeft)).toEqual(['g2'])
    const hidden = setHidden(terminalsLeft, 'g2', true)
    expect(ids(hidden)).toEqual(['g2', 'g1'])
    // Never the main group, even when it's what lies along the edge.
    expect(ids(setHidden(hidden, 'g1', true))).toEqual(['g2', 'g1'])
  })

  it('resizes splits, keeping sizes summing to 1', () => {
    const layout = resizeSplit(classic(), 's1', [1, 3])
    expect(sizesOf(layout)).toEqual([0.25, 0.75])
    expect(sizesOf(resizeSplit(classic(), 's1', [0, 0]))).toEqual([0.5, 0.5])
  })

  it('finds the neighbouring group in a direction', () => {
    const rect = (left: number, top: number, right: number, bottom: number) => ({
      rect: { left, top, right, bottom },
    })
    const left = rect(0, 0, 100, 400)
    const topRight = rect(100, 0, 400, 200)
    const bottomRight = rect(100, 200, 400, 400)
    const all = [left, topRight, bottomRight]
    expect(neighbourInDirection(topRight.rect, 'left', all)).toBe(left)
    expect(neighbourInDirection(topRight.rect, 'bottom', all)).toBe(bottomRight)
    expect(neighbourInDirection(left.rect, 'right', all)).toBe(topRight)
    expect(neighbourInDirection(topRight.rect, 'top', all)).toBeNull()
  })

  it('reads a saved layout, dropping what it does not recognize', () => {
    const saved = JSON.parse(JSON.stringify(classic()))
    saved.root.children[1].items.push('document:1', 'bogus:1', 42)
    const parsed = parseDock(
      saved,
      item => item.startsWith('view:') || item.startsWith('document:'),
    )
    // Duplicates and unknown items are dropped.
    expect(parsed).toEqual(classic())

    // A tree that lost its main group gets one back.
    const noMain = { main: 'main', root: emptyGroup('g1', ['view:explorer']) }
    expect(groups(parseDock(noMain, () => true)!).map(g => g.id)).toEqual(['g1', 'main'])

    for (const bad of [null, 'x', {}, { main: 'main', root: { kind: 'split', id: 's1' } }]) {
      expect(parseDock(bad, () => true)).toBeNull()
    }
    // Repeated ids are rejected.
    const repeated = { main: 'main', root: { ...classic().root, id: 'main' } }
    expect(parseDock(repeated, () => true)).toBeNull()
  })

  describe('windows', () => {
    /** The window tabs went to, as a compact picture. */
    const windowShapes = (layout: DockLayout) =>
      (layout.windows ?? []).map(window => shape(window.root))

    it('moves a tab into a window of its own, and back', () => {
      let layout = placeItem(classic(), 'document:2', { kind: 'window' })
      expect(shape(layout.root)).toBe('row(g1[explorer,search] main[1])')
      expect(windowShapes(layout)).toEqual(['g2[2]'])
      const window = layout.windows![0]!.id
      expect(windowOfItem(layout, 'document:2')).toBe(window)
      expect(windowOfItem(layout, 'document:1')).toBeNull()
      expect(windowOfGroup(layout, 'g2')).toBe(window)
      expect(findGroup(layout, 'g2')!.active).toBe('document:2')
      expect(allItems(layout)).toContain('document:2')

      // Its last tab leaving closes the window.
      layout = placeItem(layout, 'document:2', { kind: 'tabs', group: 'main', index: 0 })
      expect(shape(layout.root)).toBe('row(g1[explorer,search] main[2,1])')
      expect(layout.windows).toEqual([])
    })

    it('splits and docks within a window, and gives every group a fresh id', () => {
      let layout = placeItem(classic(), 'document:2', { kind: 'window' })
      const window = layout.windows![0]!.id
      layout = placeItem(layout, 'view:search', { kind: 'split', group: 'g2', edge: 'right' })
      expect(windowShapes(layout)).toEqual(['row(g2[2] g3[search])'])
      layout = placeItem(layout, 'document:1', { kind: 'root', edge: 'bottom', window })
      expect(windowShapes(layout)).toEqual(['column(row(g2[2] g3[search]) g4[1])'])
      // The main window keeps its main group, empty.
      expect(shape(layout.root)).toBe('row(g1[explorer] main[])')
      const ids = groups(layout).map(group => group.id)
      expect(new Set(ids).size).toBe(ids.length)

      // Docking at the root of a window that's gone does nothing.
      expect(
        placeItem(layout, 'view:explorer', { kind: 'root', edge: 'left', window: 'w999' }),
      ).toBe(layout)
    })

    it('opens each window with an id of its own', () => {
      let layout = placeItem(classic(), 'document:1', { kind: 'window' })
      layout = placeItem(layout, 'document:2', { kind: 'window' })
      const [first, second] = layout.windows!.map(window => window.id)
      expect(first).not.toBe(second)
      // A closed window's id isn't handed out again.
      layout = removeItem(layout, 'document:2')
      layout = placeItem(layout, 'view:search', { kind: 'window' })
      expect(layout.windows!.map(window => window.id)).not.toContain(second)
    })

    it('hides and removes tabs in windows', () => {
      let layout = placeItem(classic(), 'document:2', { kind: 'window' })
      layout = placeItem(layout, 'document:1', { kind: 'tabs', group: 'g2', index: 1 })
      layout = removeItem(layout, 'document:2')
      expect(windowShapes(layout)).toEqual(['g2[1]'])
      expect(findGroup(layout, 'g2')!.active).toBe('document:1')
      // The side bar is the main window's.
      expect(sidebarGroups(layout).map(group => group.id)).toEqual(['g1'])
    })

    it('reads saved windows, dropping those that do not read', () => {
      const layout = placeItem(classic(), 'document:2', { kind: 'window' })
      const saved = JSON.parse(JSON.stringify(layout))
      expect(parseDock(saved, () => true)).toEqual(layout)

      saved.windows.push(
        { id: 'bad id', root: emptyGroup('g7', ['document:7']) },
        { id: 'w50', root: { kind: 'split', id: 's9' } },
        // An item already in another window stays there.
        { id: 'w51', root: emptyGroup('g8', ['document:2', 'document:8']) },
        // A group id already used makes the window unreadable.
        { id: 'w52', root: emptyGroup('g2', ['document:9']) },
      )
      const parsed = parseDock(saved, () => true)!
      expect(parsed.windows!.map(window => window.id)).toEqual([layout.windows![0]!.id, 'w51'])
      expect(windowShapes(parsed)).toEqual(['g2[2]', 'g8[8]'])
    })
  })
})
