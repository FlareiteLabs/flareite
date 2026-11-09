/**
 * The dock layout: a tree of splits whose leaves are tab groups. Every item
 * (a document, a view such as the explorer, a terminal, an extension's
 * preview, a page such as the settings editor) is a tab in exactly one
 * group, and any item can go in any group.
 *
 * These functions are pure: they take a layout and return a new one, so the
 * store, persistence, and tests share them.
 */

/** `document:<id>`, `view:<id>`, `terminal:<id>`, `preview:<id>`, or `page:<id>`. */
export type ItemId = string

export type Edge = 'left' | 'right' | 'top' | 'bottom'

export interface DockGroup {
  kind: 'group'
  id: string
  items: ItemId[]
  active: ItemId | null
  /** Hidden groups keep their items and place; they just aren't shown. */
  hidden?: boolean
}

export interface DockSplit {
  kind: 'split'
  id: string
  direction: 'row' | 'column'
  children: DockNode[]
  /** Each child's share of the split, summing to 1. */
  sizes: number[]
}

export type DockNode = DockGroup | DockSplit

export interface DockLayout {
  root: DockNode
  /**
   * The main group: new documents go here when no other group holds
   * documents, and it stays (showing the welcome page) when empty.
   */
  main: string
}

/** Where a dragged or moved item lands. */
export type DropTarget =
  /** Among a group's tabs. */
  | { kind: 'tabs'; group: string; index: number }
  /** In a new group beside a group. */
  | { kind: 'split'; group: string; edge: Edge }
  /** In a new group along an edge of the whole dock. */
  | { kind: 'root'; edge: Edge }

/** The share a group gets when docked along the edge of the whole dock. */
export const ROOT_EDGE_SIZE = 0.25

export const MAIN_GROUP = 'main'

export type ItemKind = 'document' | 'view' | 'terminal' | 'preview' | 'page' | 'viewer' | 'layout'

const KINDS: readonly string[] = [
  'document',
  'view',
  'terminal',
  'preview',
  'page',
  'viewer',
  'layout',
]

export function itemKind(item: ItemId): ItemKind | null {
  const kind = item.slice(0, item.indexOf(':'))
  return KINDS.includes(kind) ? (kind as ItemKind) : null
}

export function emptyGroup(id: string, items: ItemId[] = []): DockGroup {
  return { kind: 'group', id, items, active: items[0] ?? null }
}

/** Every group, in reading order (left to right, top to bottom). */
export function groups(layout: DockLayout): DockGroup[] {
  const result: DockGroup[] = []
  const walk = (node: DockNode) => {
    if (node.kind === 'group') {
      result.push(node)
    } else {
      node.children.forEach(walk)
    }
  }
  walk(layout.root)
  return result
}

export function findGroup(layout: DockLayout, id: string): DockGroup | undefined {
  return groups(layout).find(group => group.id === id)
}

export function groupOf(layout: DockLayout, item: ItemId): DockGroup | undefined {
  return groups(layout).find(group => group.items.includes(item))
}

export function allItems(layout: DockLayout): ItemId[] {
  return groups(layout).flatMap(group => group.items)
}

/**
 * The side bar: the groups along the dock's left edge, beside the activity
 * bar, in reading order. The main group is never part of it. A group is
 * there when, in every row it's in, nothing shows before it. So a hidden
 * group stays (the activity bar can show it again), and what's beside it
 * joins it, since that's what now lies along the edge.
 */
export function sidebarGroups(layout: DockLayout): DockGroup[] {
  const result: DockGroup[] = []
  // `left` is null until the walk passes through a row.
  const walk = (node: DockNode, left: boolean | null) => {
    if (node.kind === 'group') {
      if (left && node.id !== layout.main) {
        result.push(node)
      }
      return
    }
    let before = false
    for (const child of node.children) {
      walk(child, node.direction === 'row' ? left !== false && !before : left)
      before ||= isVisible(child)
    }
  }
  walk(layout.root, null)
  return result
}

/** Whether a node shows anything: hidden groups, and splits of only those, don't. */
export function isVisible(node: DockNode): boolean {
  return node.kind === 'group' ? !node.hidden : node.children.some(isVisible)
}

/** An id not used by any node, such as `g4` or `s2`. */
export function freshId(layout: DockLayout, prefix: 'g' | 's'): string {
  let max = 0
  const walk = (node: DockNode) => {
    const match = new RegExp(`^${prefix}(\\d+)$`).exec(node.id)
    if (match) {
      max = Math.max(max, Number(match[1]))
    }
    if (node.kind === 'split') {
      node.children.forEach(walk)
    }
  }
  walk(layout.root)
  return `${prefix}${max + 1}`
}

/** Applies `change` to the group `id`. */
export function updateGroup(
  layout: DockLayout,
  id: string,
  change: (group: DockGroup) => DockGroup,
): DockLayout {
  const walk = (node: DockNode): DockNode => {
    if (node.kind === 'group') {
      return node.id === id ? change(node) : node
    }
    return { ...node, children: node.children.map(walk) }
  }
  return { ...layout, root: walk(layout.root) }
}

/** Makes `item` its group's active tab, and shows the group. */
export function activateItem(layout: DockLayout, item: ItemId): DockLayout {
  const group = groupOf(layout, item)
  if (!group || (group.active === item && !group.hidden)) {
    return layout
  }
  return updateGroup(layout, group.id, g => ({ ...g, active: item, hidden: false }))
}

export function setHidden(layout: DockLayout, id: string, hidden: boolean): DockLayout {
  // The main group always shows: it's where documents go.
  if (hidden && id === layout.main) {
    return layout
  }
  return updateGroup(layout, id, group =>
    Boolean(group.hidden) === hidden ? group : { ...group, hidden },
  )
}

/**
 * Removes `item`. A group left empty is removed too, except the main group,
 * and the tree is tidied up.
 */
export function removeItem(layout: DockLayout, item: ItemId): DockLayout {
  const group = groupOf(layout, item)
  if (!group) {
    return layout
  }
  const index = group.items.indexOf(item)
  const items = group.items.filter(other => other !== item)
  const active =
    group.active === item ? (items[Math.min(index, items.length - 1)] ?? null) : group.active
  return normalize(updateGroup(layout, group.id, g => ({ ...g, items, active })))
}

/**
 * Puts `item` at `target` (taking it from where it was, if anywhere) and
 * makes it active there. `size` is the new group's share for split and root
 * targets.
 */
export function placeItem(
  layout: DockLayout,
  item: ItemId,
  target: DropTarget,
  size = ROOT_EDGE_SIZE,
): DockLayout {
  const from = groupOf(layout, item)
  // Moving a group's only item beside that same group would leave it empty.
  if (from && target.kind === 'split' && target.group === from.id && from.items.length === 1) {
    return layout
  }
  if (target.kind === 'tabs') {
    const group = findGroup(layout, target.group)
    if (!group) {
      return layout
    }
    // Indexes count the tabs as they are, including the one moving.
    let index = target.index
    if (from?.id === group.id && group.items.indexOf(item) < index) {
      index -= 1
    }
    // Remove first, but keep the target group even if the item was its only one.
    const without = from ? detach(layout, item) : layout
    return normalize(
      updateGroup(without, group.id, g => {
        const items = g.items.filter(other => other !== item)
        const at = Math.max(0, Math.min(index, items.length))
        return {
          ...g,
          items: [...items.slice(0, at), item, ...items.slice(at)],
          active: item,
          hidden: false,
        }
      }),
    )
  }

  const without = from ? normalize(detach(layout, item)) : layout
  const created: DockGroup = emptyGroup(freshId(without, 'g'), [item])
  if (target.kind === 'root') {
    return normalize(dockAtRoot(without, created, target.edge, size))
  }
  if (!findGroup(without, target.group)) {
    return layout
  }
  return normalize(splitBeside(without, target.group, created, target.edge))
}

/** Takes `item` out of its group without removing the group if it empties. */
function detach(layout: DockLayout, item: ItemId): DockLayout {
  const group = groupOf(layout, item)
  if (!group) {
    return layout
  }
  const index = group.items.indexOf(item)
  const items = group.items.filter(other => other !== item)
  const active =
    group.active === item ? (items[Math.min(index, items.length - 1)] ?? null) : group.active
  return updateGroup(layout, group.id, g => ({ ...g, items, active }))
}

const directionOf = (edge: Edge) => (edge === 'left' || edge === 'right' ? 'row' : 'column')
const isLeading = (edge: Edge) => edge === 'left' || edge === 'top'

function dockAtRoot(layout: DockLayout, group: DockGroup, edge: Edge, size: number): DockLayout {
  const direction = directionOf(edge)
  const root = layout.root
  if (root.kind === 'split' && root.direction === direction) {
    const sizes = root.sizes.map(s => s * (1 - size))
    return {
      ...layout,
      root: isLeading(edge)
        ? { ...root, children: [group, ...root.children], sizes: [size, ...sizes] }
        : { ...root, children: [...root.children, group], sizes: [...sizes, size] },
    }
  }
  const split: DockSplit = {
    kind: 'split',
    id: freshId(layout, 's'),
    direction,
    children: isLeading(edge) ? [group, root] : [root, group],
    sizes: isLeading(edge) ? [size, 1 - size] : [1 - size, size],
  }
  return { ...layout, root: split }
}

/** Puts `group` beside the group `besideId`, halving its space. */
function splitBeside(
  layout: DockLayout,
  besideId: string,
  group: DockGroup,
  edge: Edge,
): DockLayout {
  const direction = directionOf(edge)
  const splitId = freshId(layout, 's')
  const walk = (node: DockNode): DockNode => {
    if (node.kind === 'group') {
      if (node.id !== besideId) {
        return node
      }
      return {
        kind: 'split',
        id: splitId,
        direction,
        children: isLeading(edge) ? [group, node] : [node, group],
        sizes: [0.5, 0.5],
      }
    }
    const index = node.children.findIndex(child => child.kind === 'group' && child.id === besideId)
    if (index >= 0 && node.direction === direction) {
      // Same direction: a new sibling, sharing the neighbour's space.
      const half = node.sizes[index]! / 2
      const sizes = [...node.sizes]
      sizes[index] = half
      const at = isLeading(edge) ? index : index + 1
      return {
        ...node,
        children: [...node.children.slice(0, at), group, ...node.children.slice(at)],
        sizes: [...sizes.slice(0, at), half, ...sizes.slice(at)],
      }
    }
    return { ...node, children: node.children.map(walk) }
  }
  return { ...layout, root: walk(layout.root) }
}

/** Sets a split's sizes (normalized to sum to 1). */
export function resizeSplit(layout: DockLayout, id: string, sizes: number[]): DockLayout {
  const walk = (node: DockNode): DockNode => {
    if (node.kind === 'group') {
      return node
    }
    if (node.id === id && sizes.length === node.children.length) {
      return { ...node, sizes: normalizeSizes(sizes) }
    }
    return { ...node, children: node.children.map(walk) }
  }
  return { ...layout, root: walk(layout.root) }
}

function normalizeSizes(sizes: number[]): number[] {
  const clean = sizes.map(size => (Number.isFinite(size) && size > 0 ? size : 0))
  const total = clean.reduce((sum, size) => sum + size, 0)
  return total > 0 ? clean.map(size => size / total) : sizes.map(() => 1 / sizes.length)
}

/**
 * Tidies the tree: removes empty groups (but not the main group), replaces
 * splits of one child with that child, merges a split into its parent when
 * they go the same way, and keeps sizes summing to 1.
 */
export function normalize(layout: DockLayout): DockLayout {
  const walk = (node: DockNode): DockNode | null => {
    if (node.kind === 'group') {
      if (node.items.length === 0 && node.id !== layout.main) {
        return null
      }
      const active = node.active !== null && node.items.includes(node.active) ? node.active : null
      return active === node.active ? node : { ...node, active: active ?? node.items[0] ?? null }
    }
    const children: DockNode[] = []
    const sizes: number[] = []
    node.children.forEach((child, index) => {
      const kept = walk(child)
      const size = node.sizes[index] ?? 0
      if (!kept) {
        return
      }
      if (kept.kind === 'split' && kept.direction === node.direction) {
        kept.children.forEach((grandchild, i) => {
          children.push(grandchild)
          sizes.push(size * kept.sizes[i]!)
        })
      } else {
        children.push(kept)
        sizes.push(size)
      }
    })
    if (children.length === 0) {
      return null
    }
    if (children.length === 1) {
      return children[0]!
    }
    return { ...node, children, sizes: normalizeSizes(sizes) }
  }
  return { ...layout, root: walk(layout.root) ?? emptyGroup(layout.main) }
}

/** The direction, among the four, of a move from one rectangle toward another. */
export interface Rect {
  left: number
  top: number
  right: number
  bottom: number
}

/**
 * The group nearest to `from` in direction `edge`, among `candidates`
 * (rectangles on screen), or null. A candidate must lie beyond `from` on
 * that side and overlap it on the other axis.
 */
export function neighbourInDirection<T extends { rect: Rect }>(
  from: Rect,
  edge: Edge,
  candidates: readonly T[],
): T | null {
  let best: T | null = null
  let bestDistance = Infinity
  for (const candidate of candidates) {
    const r = candidate.rect
    const overlapsRows = r.top < from.bottom - 1 && r.bottom > from.top + 1
    const overlapsColumns = r.left < from.right - 1 && r.right > from.left + 1
    let distance: number | null = null
    if (edge === 'left' && r.right <= from.left + 1 && overlapsRows) distance = from.left - r.right
    if (edge === 'right' && r.left >= from.right - 1 && overlapsRows) distance = r.left - from.right
    if (edge === 'top' && r.bottom <= from.top + 1 && overlapsColumns)
      distance = from.top - r.bottom
    if (edge === 'bottom' && r.top >= from.bottom - 1 && overlapsColumns)
      distance = r.top - from.bottom
    if (distance !== null && distance < bestDistance) {
      best = candidate
      bestDistance = distance
    }
  }
  return best
}

/**
 * Validates a saved layout. Anything malformed gives `null`, and the caller
 * falls back to the default. `isItem` decides which saved item ids are kept.
 */
export function parseDock(input: unknown, isItem: (item: string) => boolean): DockLayout | null {
  if (typeof input !== 'object' || input === null) {
    return null
  }
  const saved = input as Record<string, unknown>
  const main = typeof saved.main === 'string' ? saved.main : null
  if (!main) {
    return null
  }
  const ids = new Set<string>()
  const seenItems = new Set<string>()
  let nodes = 0
  const parse = (value: unknown, depth: number): DockNode | null => {
    if (typeof value !== 'object' || value === null || depth > 32 || ++nodes > 500) {
      return null
    }
    const node = value as Record<string, unknown>
    if (typeof node.id !== 'string' || node.id.length > 64 || ids.has(node.id)) {
      return null
    }
    ids.add(node.id)
    if (node.kind === 'group') {
      // Each item once, in the first place it appears.
      const items: string[] = []
      for (const item of Array.isArray(node.items) ? node.items : []) {
        if (
          typeof item === 'string' &&
          item.length < 4096 &&
          isItem(item) &&
          !seenItems.has(item)
        ) {
          seenItems.add(item)
          items.push(item)
        }
      }
      const active =
        typeof node.active === 'string' && items.includes(node.active) ? node.active : null
      const group: DockGroup = {
        kind: 'group',
        id: node.id,
        items,
        active: active ?? items[0] ?? null,
      }
      return node.hidden === true ? { ...group, hidden: true } : group
    }
    if (
      node.kind === 'split' &&
      (node.direction === 'row' || node.direction === 'column') &&
      Array.isArray(node.children) &&
      Array.isArray(node.sizes) &&
      node.children.length === node.sizes.length &&
      node.children.length > 0
    ) {
      const children = node.children.map(child => parse(child, depth + 1))
      if (children.some(child => child === null)) {
        return null
      }
      return {
        kind: 'split',
        id: node.id,
        direction: node.direction,
        children: children as DockNode[],
        sizes: normalizeSizes(node.sizes.map(Number)),
      }
    }
    return null
  }
  const root = parse(saved.root, 0)
  if (!root) {
    return null
  }
  const layout: DockLayout = { root, main }
  // The main group must exist; add it if the saved tree lost it.
  const withMain = findGroup(layout, main)
    ? layout
    : dockAtRoot(layout, emptyGroup(main), 'right', 1 - ROOT_EDGE_SIZE)
  return normalize(withMain)
}
