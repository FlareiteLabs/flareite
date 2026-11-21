import { Fragment, useState, type KeyboardEvent } from 'react'
import { useViews } from '../app/views'
import {
  activityBarItems,
  dockInSidebar,
  dockStore,
  moveInSidebar,
  toggleItem,
  type SidebarPlace,
} from '../dock/dock'
import { startItemDrag } from '../dock/DockArea'
import {
  ItemIcon,
  itemLabel,
  itemTitle,
  itemTooltip,
  useResolver,
  type ResolvedItem,
} from '../dock/items'
import { groups, sidebarGroups, type ItemId } from '../dock/model'
import { extensionsStore } from '../state/extensions'
import { useStore } from '../state/store'
import { sectionsOf, viewBadges } from '../state/views'
import { useDropTarget, type Drag } from './drag'
import './ActivityBar.css'

/**
 * Where a drag over the activity bar would drop, from the first button or
 * separator whose middle is below the pointer. A separator's middle splits
 * the gap between side bar groups: above it, the end of the upper group;
 * below it, the start of the lower one.
 */
function dropPlaceAt(drag: Drag, bar: HTMLElement): SidebarPlace {
  let previous: HTMLElement | null = null
  for (const child of bar.children as HTMLCollectionOf<HTMLElement>) {
    const r = child.getBoundingClientRect()
    if (drag.y < r.top + r.height / 2) {
      const item = child.dataset.item
      const above = previous?.dataset.item
      if (above !== undefined) {
        return { after: above }
      }
      // At the top, or just below a separator.
      return item !== undefined && previous ? { before: item } : { after: null }
    }
    previous = child
  }
  return { after: previous?.dataset.item ?? null }
}

/**
 * The button that marks where `place` is, and on which side: the one below
 * the line where possible, but the one above it at the end of a group, so a
 * drop above a separator and one below it are told apart. Places among
 * views not docked anywhere are after the side bar's last item.
 */
function dropMark(
  place: SidebarPlace,
  buttons: readonly ItemId[],
  groupOfButton: ReadonlyMap<ItemId, string>,
): { item: ItemId; side: 'before' | 'after' } | null {
  const docked = buttons.filter(item => groupOfButton.has(item))
  const last = docked.at(-1)
  if (last === undefined) {
    return null
  }
  if ('before' in place) {
    return groupOfButton.has(place.before)
      ? { item: place.before, side: 'before' }
      : { item: last, side: 'after' }
  }
  if (place.after === null) {
    return { item: docked[0]!, side: 'before' }
  }
  if (!groupOfButton.has(place.after)) {
    return { item: last, side: 'after' }
  }
  const next = docked[docked.indexOf(place.after) + 1]
  return next !== undefined && groupOfButton.get(next) === groupOfButton.get(place.after)
    ? { item: next, side: 'before' }
    : { item: place.after, side: 'after' }
}

const samePlace = (a: SidebarPlace | null, b: SidebarPlace | null) =>
  JSON.stringify(a) === JSON.stringify(b)

/**
 * A button per item in the side bar (views, terminals, documents), then per
 * view not docked anywhere: shows it, or hides its group when it's already
 * shown. A button can be dragged into the dock to move the item there (and
 * out of the side bar), and a tab dropped here moves its item into the side
 * bar, between the buttons it's dropped between: so dragging buttons up and
 * down reorders the side bar's tabs, as `Alt+Up` and `Alt+Down` do. When
 * the side bar is split into groups (docked beside each other in the dock,
 * not here), a separator divides their buttons.
 */
export function ActivityBar() {
  const layout = useStore(dockStore)
  const views = useViews()
  const resolve = useResolver()
  const badges = useStore(viewBadges)
  const extensions = useStore(extensionsStore)
  /** A view's badge, or the sum of a container's sections'. */
  const badgeOf = (resolved: ResolvedItem) => {
    if (resolved.kind !== 'view') {
      return 0
    }
    const sections = sectionsOf(resolved.view.id, extensions)
    return (sections.length > 0 ? sections : [resolved.view.id]).reduce(
      (sum, view) => sum + (badges[view] ?? 0),
      0,
    )
  }

  const items = activityBarItems(
    layout,
    views.map(view => view.id),
  )
    .map(resolve)
    .filter((resolved): resolved is ResolvedItem => resolved !== null)
  const buttons = items.map(resolved => resolved.item)
  // Side bar items come first, by group; views not docked anywhere follow them.
  const groupOfButton = new Map(
    sidebarGroups(layout).flatMap(group => group.items.map(item => [item, group.id] as const)),
  )
  const docked = new Set(groupOfButton.keys())

  /** Where a drag would drop, while one is over the bar. */
  const [dropPlace, setDropPlace] = useState<SidebarPlace | null>(null)
  const showPlace = (next: SidebarPlace | null) =>
    setDropPlace(previous => (samePlace(previous, next) ? previous : next))
  const dropRef = useDropTarget<HTMLElement>({
    over: (drag, element) => {
      const takes = drag.payload.kind === 'dock'
      showPlace(takes ? dropPlaceAt(drag, element) : null)
      return takes
    },
    drop: (drag, element) => {
      showPlace(null)
      if (drag.payload.kind === 'dock') {
        dockInSidebar(drag.payload.item, dropPlaceAt(drag, element))
      }
    },
    leave: () => showPlace(null),
  })
  const mark = dropPlace && dropMark(dropPlace, buttons, groupOfButton)
  const dropSide = (item: ItemId) => (mark?.item === item ? mark.side : undefined)

  // An item counts as shown when it's the visible tab of its group.
  const shown = new Set(groups(layout).flatMap(group => (group.hidden ? [] : [group.active])))

  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, item: string) => {
    const step = { ArrowUp: -1, ArrowDown: 1 }[event.key] as -1 | 1 | undefined
    if (step === undefined || !event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
      return
    }
    event.preventDefault()
    const button = event.currentTarget
    moveInSidebar(item, buttons, step)
    // Moving a focused element in the page can lose its focus.
    button.ownerDocument.defaultView?.requestAnimationFrame(() => button.focus())
  }

  return (
    <nav
      ref={dropRef}
      className="activitybar"
      aria-label="Views"
      data-drop={dropPlace !== null || undefined}
    >
      {items.map((resolved, index) => {
        const badge = badgeOf(resolved)
        const group = groupOfButton.get(resolved.item)
        const previous = index > 0 ? groupOfButton.get(buttons[index - 1]!) : undefined
        return (
          <Fragment key={resolved.item}>
            {group !== undefined && previous !== undefined && group !== previous && (
              <div className="activitybar__separator" role="separator" />
            )}
            <button
              type="button"
              className="activitybar__item"
              aria-label={badge > 0 ? `${itemLabel(resolved)} (${badge})` : itemLabel(resolved)}
              aria-pressed={shown.has(resolved.item)}
              aria-keyshortcuts={
                docked.has(resolved.item) ? 'Alt+ArrowUp Alt+ArrowDown' : undefined
              }
              title={itemTooltip(resolved)}
              data-kind={resolved.kind}
              data-item={resolved.item}
              data-drop={dropSide(resolved.item)}
              onPointerDown={event => startItemDrag(event, resolved.item, itemTitle(resolved))}
              onClick={() => toggleItem(resolved.item)}
              onKeyDown={event => onKeyDown(event, resolved.item)}
            >
              <ItemIcon resolved={resolved} className="activitybar__icon" size={24} />
              {badge > 0 && (
                <span className="activitybar__badge" aria-hidden="true">
                  {badge > 999 ? '999+' : badge}
                </span>
              )}
            </button>
          </Fragment>
        )
      })}
    </nav>
  )
}
