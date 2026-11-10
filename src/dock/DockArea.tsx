import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
  type ReactNode,
} from 'react'
import { resizeObserverFor, type WindowId } from '../app/windows'
import { executeCommand } from '../commands/registry'
import { openContextMenu, type MenuEntry } from '../components/contextMenuState'
import { startDrag, useDropTarget, type Drag } from '../components/drag'
import { Sash } from '../components/Sash'
import { WelcomePage } from '../editor/WelcomePage'
import { isDesktop, type AppInfo } from '../ipc'
import { extensionsStore } from '../state/extensions'
import { rowMark, useFileMarks } from '../state/fileMarks'
import { createStore, useStore } from '../state/store'
import {
  activateTab,
  closeItem,
  copyItem,
  currentGroup,
  dockStore,
  focusItem,
  groupFocused,
  moveItem,
  openDroppedFiles,
  resize,
} from './dock'
import {
  ItemActions,
  ItemContent,
  ItemIcon,
  itemDirty,
  itemLabel,
  itemPath,
  itemTitle,
  itemTooltip,
  keepsLoaded,
  useResolver,
  type ResolvedItem,
} from './items'
import {
  itemKind,
  rootOf,
  type DockGroup,
  type DockNode,
  type DockSplit,
  type DropTarget,
  type Edge,
  type ItemId,
} from './model'
import './Dock.css'

/** Groups are never squeezed smaller than this, in pixels. */
const MIN_GROUP_SIZE = 100
/** Dropping this close to the dock's edge docks along the whole edge. */
const ROOT_EDGE_DISTANCE = 24
/** Dropping within this share of a group's edge splits it. */
const SPLIT_ZONE = 0.25

/** What a drop here would do, for the preview. */
type DropHint =
  | { kind: 'split'; group: string; zone: Edge | 'center' }
  | { kind: 'root'; edge: Edge; window: WindowId }
  | { kind: 'tabs'; group: string; index: number }

const dropHint = createStore<DropHint | null>(null)

/** Starts dragging a tab or activity bar button (from `onPointerDown`). */
export function startItemDrag(event: PointerEvent<HTMLElement>, item: ItemId, label: string): void {
  startDrag(event, () => ({ kind: 'dock', item, label }))
}

/** What the dock takes: its own items, and files from other programs. */
const isDockDrag = (drag: Drag) => drag.payload.kind === 'dock' || drag.payload.kind === 'files'

function sameHint(a: DropHint | null, b: DropHint | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

function hint(next: DropHint | null): void {
  if (!sameHint(dropHint.get(), next)) {
    dropHint.set(next)
  }
}

const clearHint = () => hint(null)

/**
 * The edge of the whole dock the pointer is near, over a group's content: the
 * outer strip of a group that lies along that edge. Tab strips are always for
 * joining tabs, so the top edge is reached below them.
 */
function outerEdgeAt(drag: Drag, content: HTMLElement): Edge | null {
  const dock = content.closest('[data-dock]')?.getBoundingClientRect()
  const group = content.closest('[data-group]')?.getBoundingClientRect()
  if (!dock || !group) {
    return null
  }
  const r = content.getBoundingClientRect()
  const candidates: [Edge, boolean, number][] = [
    ['left', Math.abs(group.left - dock.left) < 2, drag.x - r.left],
    ['right', Math.abs(dock.right - group.right) < 2, r.right - drag.x],
    ['top', Math.abs(group.top - dock.top) < 2, drag.y - r.top],
    ['bottom', Math.abs(dock.bottom - group.bottom) < 2, r.bottom - drag.y],
  ]
  let best: Edge | null = null
  let bestDistance = ROOT_EDGE_DISTANCE
  for (const [edge, outer, distance] of candidates) {
    if (outer && distance <= bestDistance) {
      best = edge
      bestDistance = distance
    }
  }
  return best
}

/** The edge of the whole dock the pointer is near, for drags over its gaps (splitters). */
function rootEdgeAt(drag: Drag, dock: HTMLElement): Edge | null {
  const r = dock.getBoundingClientRect()
  const distances: [Edge, number][] = [
    ['left', drag.x - r.left],
    ['right', r.right - drag.x],
    ['top', drag.y - r.top],
    ['bottom', r.bottom - drag.y],
  ]
  const [edge, distance] = distances.reduce((a, b) => (b[1] < a[1] ? b : a))
  return distance <= ROOT_EDGE_DISTANCE ? edge : null
}

/**
 * Drops on the dock: moves an item there (or, with the copy key held, shows
 * a document there too), or opens files from another program there
 * (`null`: where documents open).
 */
function drop(drag: Drag, target: DropTarget | null): void {
  clearHint()
  const { payload } = drag
  if (payload.kind === 'dock' && target) {
    if (drag.copy) {
      focusItem(copyItem(payload.item, target))
    } else {
      moveItem(payload.item, target)
      focusItem(payload.item)
    }
  } else if (payload.kind === 'files') {
    void openDroppedFiles(payload.files, payload.folders, target)
  }
}

interface DockAreaProps {
  appInfo: AppInfo | null
  /** The window whose groups these are: `null` for the main one. */
  window: WindowId
}

/** The target for docking along an edge of window `window`'s whole dock. */
const rootTarget = (edge: Edge, window: WindowId): DropTarget =>
  window === null ? { kind: 'root', edge } : { kind: 'root', edge, window }

/**
 * Every group and split of a window, filling the space beside the activity
 * bar (in the main window) or the whole window (in another).
 */
export function DockArea({ appInfo, window }: DockAreaProps) {
  const layout = useStore(dockStore)
  const current = useStore(currentGroup)
  const resolve = useResolver()
  const root = rootOf(layout, window)

  const shown = (node: DockNode): boolean =>
    node.kind === 'group'
      ? !node.hidden && (node.id === layout.main || node.items.some(item => resolve(item)))
      : node.children.some(shown)

  const render = (node: DockNode): ReactNode =>
    node.kind === 'group' ? (
      <GroupView
        key={node.id}
        group={node}
        main={node.id === layout.main}
        current={node.id === current}
        resolve={resolve}
        appInfo={appInfo}
        window={window}
      />
    ) : (
      <SplitView key={node.id} split={node} shown={shown} render={render} />
    )

  // Over the gaps between groups (splitters) near the dock's edge: along that edge.
  const dropRef = useDropTarget<HTMLElement>({
    over: (drag, element) => {
      const edge = isDockDrag(drag) ? rootEdgeAt(drag, element) : null
      if (edge) {
        hint({ kind: 'root', edge, window })
      }
      return edge !== null
    },
    drop: (drag, element) => {
      const edge = rootEdgeAt(drag, element)
      if (edge) {
        drop(drag, rootTarget(edge, window))
      }
    },
    leave: clearHint,
  })

  const content =
    window === null
      ? shown(layout.root)
        ? render(layout.root)
        : render(groupNode(layout.main))
      : root && shown(root)
        ? render(root)
        : null
  return (
    <main
      ref={dropRef}
      className="dock"
      aria-label={window === null ? 'Workbench' : 'Window'}
      data-dock
    >
      {content}
      <RootDropPreview window={window} />
    </main>
  )
}

const groupNode = (id: string): DockGroup => ({ kind: 'group', id, items: [], active: null })

function RootDropPreview({ window }: { window: WindowId }) {
  const current = useStore(dropHint)
  return current?.kind === 'root' && current.window === window ? (
    <div className="dock-drop dock-drop--root" data-zone={current.edge} />
  ) : null
}

interface SplitViewProps {
  split: DockSplit
  shown: (node: DockNode) => boolean
  render: (node: DockNode) => ReactNode
}

/** Children side by side (a row) or stacked (a column), with splitters between them. */
function SplitView({ split, shown, render }: SplitViewProps) {
  // The element, not a ref: with one child or none showing, the split renders
  // no element of its own, and a new one when its children show again. Each
  // is measured; one that left the page would measure 0, leaving no splitters.
  const [element, setElement] = useState<HTMLDivElement | null>(null)
  const [extent, setExtent] = useState(0)
  const row = split.direction === 'row'

  useLayoutEffect(() => {
    if (!element) {
      return
    }
    const measure = () => setExtent(row ? element.clientWidth : element.clientHeight)
    measure()
    const observer = new (resizeObserverFor(element))(measure)
    observer.observe(element)
    return () => observer.disconnect()
  }, [element, row])

  const visible = split.children
    .map((node, index) => ({ node, index, size: split.sizes[index] ?? 0 }))
    .filter(entry => shown(entry.node))
  if (visible.length === 0) {
    return null
  }
  if (visible.length === 1) {
    return <>{render(visible[0]!.node)}</>
  }
  const total = visible.reduce((sum, entry) => sum + entry.size, 0) || 1
  const pixels = (entry: { size: number }) => Math.round((entry.size / total) * extent)

  /** Moves the boundary between visible children `k` and `k + 1`. */
  const resizePair = (k: number, firstPixels: number) => {
    const a = visible[k]!
    const b = visible[k + 1]!
    const pair = pixels(a) + pixels(b)
    const first = Math.min(pair - MIN_GROUP_SIZE, Math.max(MIN_GROUP_SIZE, firstPixels))
    const sizes = [...split.sizes]
    const share = (a.size + b.size) / pair
    sizes[a.index] = first * share
    sizes[b.index] = (pair - first) * share
    resize(split.id, sizes)
  }

  return (
    <div ref={setElement} className={`dock-split dock-split--${split.direction}`}>
      {visible.map((entry, k) => {
        const next = visible[k + 1]
        const previous = visible[k - 1]
        return (
          <div
            key={entry.node.id}
            className="dock-split__cell"
            style={{ flex: `${entry.size / total} 1 0px` }}
          >
            {render(entry.node)}
            {/* A row's splitter sits on a cell's right edge; a column's on the top edge of the next cell. */}
            {row && next && extent > 0 && (
              <Sash
                label="Resize group"
                value={pixels(entry)}
                min={MIN_GROUP_SIZE}
                max={pixels(entry) + pixels(next) - MIN_GROUP_SIZE}
                onChange={value => resizePair(k, value)}
              />
            )}
            {!row && previous && extent > 0 && (
              <Sash
                label="Resize group"
                orientation="horizontal"
                value={pixels(entry)}
                min={MIN_GROUP_SIZE}
                max={pixels(previous) + pixels(entry) - MIN_GROUP_SIZE}
                onChange={value => resizePair(k - 1, pixels(previous) + pixels(entry) - value)}
              />
            )}
          </div>
        )
      })}
    </div>
  )
}

interface GroupViewProps {
  group: DockGroup
  main: boolean
  current: boolean
  resolve: (item: ItemId) => ResolvedItem | null
  appInfo: AppInfo | null
  window: WindowId
}

/** One group: its tabs, the active item's buttons, and the active item. */
function GroupView({ group, main, current, resolve, appInfo, window }: GroupViewProps) {
  const items = group.items
    .map(item => resolve(item))
    .filter((resolved): resolved is ResolvedItem => resolved !== null)
  const active = items.find(resolved => resolved.item === group.active) ?? items[0] ?? null
  const kept = useKeptItems(items, active)
  const hintHere = useStore(dropHint)
  const zone = hintHere?.kind === 'split' && hintHere.group === group.id ? hintHere.zone : null
  const label = active ? itemTitle(active) : main ? 'Welcome' : 'Empty group'
  const panelId = `dock-panel-${group.id}`

  /**
   * Whether files from other programs dropped in the middle join this group:
   * when documents go here (it holds editors, or it's the main group).
   * Dropped on the explorer's group, they open where documents open.
   */
  const takesFiles =
    main ||
    group.items.some(item => {
      const kind = itemKind(item)
      return kind === 'document' || kind === 'page' || kind === 'preview'
    })

  const contentRef = useDropTarget<HTMLDivElement>({
    over: (drag, element) => {
      if (!isDockDrag(drag)) {
        return false
      }
      const outer = outerEdgeAt(drag, element)
      if (outer) {
        hint({ kind: 'root', edge: outer, window })
        return true
      }
      const r = element.getBoundingClientRect()
      const x = (drag.x - r.left) / r.width
      const y = (drag.y - r.top) / r.height
      const edges: [Edge, number][] = [
        ['left', x],
        ['right', 1 - x],
        ['top', y],
        ['bottom', 1 - y],
      ]
      const [edge, distance] = edges.reduce((a, b) => (b[1] < a[1] ? b : a))
      hint({ kind: 'split', group: group.id, zone: distance < SPLIT_ZONE ? edge : 'center' })
      return true
    },
    drop: (drag, element) => {
      const outer = outerEdgeAt(drag, element)
      const current = dropHint.get()
      const target: DropTarget | null = outer
        ? rootTarget(outer, window)
        : current?.kind === 'split' && current.group === group.id && current.zone !== 'center'
          ? { kind: 'split', group: group.id, edge: current.zone }
          : drag.payload.kind === 'files' && !takesFiles
            ? null
            : { kind: 'tabs', group: group.id, index: group.items.length }
      drop(drag, target)
    },
    leave: clearHint,
  })

  const barRef = useDropTarget<HTMLDivElement>({
    over: drag => {
      if (isDockDrag(drag)) {
        hint({ kind: 'tabs', group: group.id, index: group.items.length })
      }
      return isDockDrag(drag)
    },
    drop: drag => {
      const current = dropHint.get()
      const index =
        current?.kind === 'tabs' && current.group === group.id ? current.index : group.items.length
      drop(drag, { kind: 'tabs', group: group.id, index })
    },
    leave: clearHint,
  })

  return (
    <section
      className="dock-group"
      data-group={group.id}
      data-current={current || undefined}
      data-kind={active?.kind}
      aria-label={label}
      onFocus={() => groupFocused(group.id)}
    >
      {items.length > 0 && (
        <div ref={barRef} className="dock-group__bar">
          <div className="dock-tabs" role="tablist" aria-label={`${label} tabs`}>
            {items.map(resolved => (
              <Tab
                key={resolved.item}
                resolved={resolved}
                group={group}
                active={resolved === active}
                panelId={panelId}
              />
            ))}
          </div>
          {active && (
            <div className="dock-group__actions">
              <ItemActions resolved={active} />
            </div>
          )}
        </div>
      )}
      <div
        className="dock-group__content"
        data-group-content={group.id}
        id={active ? panelId : undefined}
        role={active ? 'tabpanel' : undefined}
        aria-labelledby={active ? tabId(group.id, active.item) : undefined}
        ref={contentRef}
        tabIndex={-1}
      >
        {kept.map(resolved => (
          <div
            key={resolved.item}
            className="dock-group__kept"
            hidden={resolved.item !== active?.item}
          >
            <ItemContent resolved={resolved} />
          </div>
        ))}
        {active ? (
          !kept.includes(active) && <ItemContent key={active.item} resolved={active} />
        ) : (
          <div className="dock-group__welcome">
            <WelcomePage appInfo={appInfo} />
          </div>
        )}
        {zone && <div className="dock-drop" data-zone={zone} />}
      </div>
    </section>
  )
}

/**
 * The items of a group that stay loaded while hidden (`keepsLoaded`), once
 * they've been shown, in the order they were first shown: an item moved in
 * the page would load again.
 */
function useKeptItems(items: ResolvedItem[], active: ResolvedItem | null): ResolvedItem[] {
  const extensions = useStore(extensionsStore)
  const [shown, setShown] = useState<readonly ItemId[]>([])
  const keeps = (resolved: ResolvedItem) => keepsLoaded(resolved, extensions)
  const kept = shown
    .map(item => items.find(resolved => resolved.item === item))
    .filter((resolved): resolved is ResolvedItem => resolved !== undefined && keeps(resolved))
  if (active && keeps(active) && !shown.includes(active.item)) {
    kept.push(active)
  }
  const ids = kept.map(resolved => resolved.item)
  useEffect(() => {
    setShown(previous =>
      previous.length === ids.length && previous.every((item, index) => item === ids[index])
        ? previous
        : ids,
    )
  })
  return kept
}

const tabId = (group: string, item: ItemId) => `dock-tab-${group}-${item.replace(/[^\w-]/g, '_')}`

interface TabProps {
  resolved: ResolvedItem
  group: DockGroup
  active: boolean
  panelId: string
}

function Tab({ resolved, group, active, panelId }: TabProps) {
  const ref = useRef<HTMLDivElement>(null)
  const { item } = resolved
  const index = group.items.indexOf(item)
  const hintHere = useStore(dropHint)
  const dropSide =
    hintHere?.kind === 'tabs' && hintHere.group === group.id
      ? hintHere.index === index
        ? 'before'
        : hintHere.index === index + 1 && index === group.items.length - 1
          ? 'after'
          : null
      : null
  const title = itemTitle(resolved)

  useEffect(() => {
    if (active) {
      ref.current?.scrollIntoView({ block: 'nearest', inline: 'nearest' })
    }
  }, [active])

  const menu = (x: number, y: number) => openContextMenu(x, y, tabMenu(item, title))

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowLeft: -1, ArrowRight: 1 }[event.key]
    if (step !== undefined) {
      // Arrow keys move between tabs, keeping focus on the tabs.
      event.preventDefault()
      const tabs = [...(event.currentTarget.parentElement?.children ?? [])] as HTMLElement[]
      const next = tabs[(tabs.indexOf(event.currentTarget) + step + tabs.length) % tabs.length]
      const nextItem = next?.dataset.item
      if (nextItem) {
        activateTab(nextItem, false)
        const page = event.currentTarget.ownerDocument
        page.defaultView?.requestAnimationFrame(() =>
          page.getElementById(tabId(group.id, nextItem))?.focus(),
        )
      }
    } else if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault()
      activateTab(item)
    } else if (event.key === 'Delete') {
      event.preventDefault()
      closeItem(item)
    } else if (event.key === 'ContextMenu' || (event.key === 'F10' && event.shiftKey)) {
      event.preventDefault()
      const r = event.currentTarget.getBoundingClientRect()
      menu(r.left, r.bottom)
    }
  }

  const dropRef = useDropTarget<HTMLDivElement>({
    over: (drag, element) => {
      if (!isDockDrag(drag)) {
        return false
      }
      const r = element.getBoundingClientRect()
      const before = drag.x < r.left + r.width / 2
      hint({ kind: 'tabs', group: group.id, index: before ? index : index + 1 })
      return true
    },
    drop: drag => {
      const current = dropHint.get()
      drop(drag, {
        kind: 'tabs',
        group: group.id,
        index: current?.kind === 'tabs' && current.group === group.id ? current.index : index,
      })
    },
    leave: clearHint,
  })

  // Extensions' marks on its file, as in the explorer (Git's status, say).
  const marks = useFileMarks()
  const path = itemPath(resolved)
  const mark = path ? rowMark(marks.index, path, false, marks.color) : null
  const tooltip = itemTooltip(resolved)

  return (
    <div
      ref={element => {
        ref.current = element
        dropRef(element)
      }}
      className="dock-tab"
      role="tab"
      id={tabId(group.id, item)}
      aria-selected={active}
      aria-controls={active ? panelId : undefined}
      aria-label={itemLabel(resolved)}
      tabIndex={active ? 0 : -1}
      title={mark?.tooltip ? `${tooltip}\n${mark.tooltip}` : tooltip}
      data-kind={resolved.kind}
      data-faded={mark?.faded || undefined}
      data-item={item}
      data-dirty={itemDirty(resolved) || undefined}
      data-exited={(resolved.kind === 'terminal' && resolved.terminal.exited) || undefined}
      data-drop={dropSide ?? undefined}
      onPointerDown={event => startItemDrag(event, item, title)}
      onClick={() => activateTab(item)}
      onAuxClick={event => {
        if (event.button === 1) {
          closeItem(item)
        }
      }}
      onContextMenu={(event: MouseEvent) => {
        event.preventDefault()
        menu(event.clientX, event.clientY)
      }}
      onKeyDown={onKeyDown}
    >
      <ItemIcon resolved={resolved} />
      <span className="dock-tab__title" style={mark?.color ? { color: mark.color } : undefined}>
        {title}
      </span>
      {mark?.badge && (
        <span
          className="dock-tab__badge"
          style={mark.color ? { color: mark.color } : undefined}
          aria-hidden="true"
        >
          {mark.badge}
        </span>
      )}
      {/* A pointer shortcut: a control inside a tab can't be reached by screen
          readers, so keyboards close tabs with Delete or the tab's menu. */}
      <span
        className="dock-tab__close"
        aria-hidden="true"
        title={`Close ${title}`}
        onPointerDown={event => event.stopPropagation()}
        onMouseDown={event => event.stopPropagation()}
        onClick={event => {
          event.stopPropagation()
          closeItem(item)
        }}
      >
        <span className="dock-tab__dirty" />
        <span className="dock-tab__x">×</span>
      </span>
    </div>
  )
}

function tabMenu(item: ItemId, title: string): MenuEntry[] {
  // The commands act on the current group's active tab: this one.
  const run = (command: string) => () => {
    activateTab(item, false)
    void executeCommand(command)
  }
  return [
    { label: `Close ${title}`, run: () => closeItem(item) },
    null,
    { label: 'Split Right', run: run('view.splitRight') },
    { label: 'Split Down', run: run('view.splitDown') },
    // Windows open only in the desktop app.
    { label: 'Move into New Window', disabled: !isDesktop(), run: run('view.moveTabToNewWindow') },
    null,
    { label: 'Move Left', run: run('view.moveTabLeft') },
    { label: 'Move Right', run: run('view.moveTabRight') },
    { label: 'Move Up', run: run('view.moveTabUp') },
    { label: 'Move Down', run: run('view.moveTabDown') },
  ]
}
