import { resizeObserverFor } from '../app/windows'
import {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type MouseEvent,
  type RefObject,
} from 'react'
import { openContextMenu, type MenuEntry } from '../components/contextMenuState'
import { Sash } from '../components/Sash'
import { entryIcon } from '../components/fileIcons'
import { FileIcon } from '../components/FileIcon'
import { Icon, isActionIcon } from '../components/Icon'
import { isMac } from '../editor/keymap'
import {
  type GraphRow,
  expandViewItem,
  runViewCommand,
  setViewInput,
  viewOpened,
  type ItemIcon,
  type ViewContent,
  type ViewEntry,
} from '../ipc/extensions'
import { extensionsStore, runContributed } from '../state/extensions'
import { useColorResolver } from '../state/fileMarks'
import { iconThemeStore } from '../state/iconTheme'
import { layoutStore, updateSections } from '../state/layout'
import { reportError } from '../state/notifications'
import { useStore } from '../state/store'
import {
  forContext,
  sectionsOf,
  useContributedViews,
  useShownSections,
  useViewContent,
  viewMenus,
  whenHolds,
  type ViewMenuItem,
} from '../state/views'
import { themeKind } from '../themes/theme'
import { GraphCell, graphShape, LANE_WIDTH, passingShape, type GraphShape } from './viewGraph'
import './ExtensionViewPanel.css'

interface Row {
  item: ViewEntry
  depth: number
  hasChildren: boolean
  /** A placeholder for children being loaded (or none found). */
  pending?: boolean
}

/**
 * The rows shown, with the items `isOpen` says are open expanded: those the
 * user opened, and those the extension says start open.
 */
function visibleRows(items: readonly ViewEntry[], isOpen: (item: ViewEntry) => boolean): Row[] {
  const children = new Map<string | null, ViewEntry[]>()
  for (const item of items) {
    const siblings = children.get(item.parent) ?? []
    siblings.push(item)
    children.set(item.parent, siblings)
  }
  const rows: Row[] = []
  const visit = (parent: string | null, depth: number) => {
    for (const item of children.get(parent) ?? []) {
      const listed = children.has(item.id)
      rows.push({ item, depth, hasChildren: listed || item.expandable })
      if (!isOpen(item)) {
        continue
      }
      if (listed) {
        visit(item.id, depth + 1)
      } else if (item.expandable) {
        rows.push({
          item: { ...item, id: `${item.id}\0pending` },
          depth: depth + 1,
          hasChildren: false,
          pending: true,
        })
      }
    }
  }
  visit(null, 0)
  return rows
}

const fail = (what: string) => (error: unknown) => reportError(what, error)

/**
 * A side bar view contributed by an extension, on its own: its header's
 * buttons go in the side bar's header ({@link ExtensionViewActions}).
 */
export function ExtensionViewPanel({ view }: { view: string }) {
  return (
    <div className="extension-view">
      <ViewBody view={view} />
    </div>
  )
}

/** The buttons and "…" menu of a view's header (`view/title` menus). */
export function ExtensionViewActions({ view }: { view: string }) {
  const content = useViewContent(view)
  const extensions = useStore(extensionsStore)
  const menus = useMemo(() => viewMenus(view, 'view/title', extensions), [view, extensions])
  const flags = content?.flags ?? []
  const shown = menus.filter(menu => whenHolds(menu.when, flags))
  const run = (menu: ViewMenuItem) => runViewCommand(menu.command, null, []).catch(fail(menu.title))
  const buttons = shown.filter(menu => menu.inline)
  const more = shown.filter(menu => !menu.inline)
  return (
    <>
      {buttons.map(menu => (
        <button
          key={menu.command}
          type="button"
          className="dock-action"
          title={menu.title}
          aria-label={menu.title}
          onClick={event => {
            event.stopPropagation()
            void run(menu)
          }}
        >
          <MenuIcon name={menu.icon} />
        </button>
      ))}
      {more.length > 0 && (
        <button
          type="button"
          className="dock-action"
          title="More Actions…"
          aria-label="More Actions…"
          aria-haspopup="menu"
          onClick={event => {
            event.stopPropagation()
            const rect = event.currentTarget.getBoundingClientRect()
            openContextMenu(
              rect.left,
              rect.bottom,
              grouped(more, menu => () => void run(menu)),
            )
          }}
        >
          <Icon name="more" size={16} />
        </button>
      )}
    </>
  )
}

function MenuIcon({ name }: { name: string | null }) {
  return name && isActionIcon(name) ? <Icon name={name} size={16} /> : null
}

/** Menu entries, with a separator between groups. */
function grouped(
  menus: readonly ViewMenuItem[],
  run: (menu: ViewMenuItem) => () => void,
): MenuEntry[] {
  const entries: MenuEntry[] = []
  menus.forEach((menu, index) => {
    if (index > 0 && menus[index - 1]?.group !== menu.group) {
      entries.push(null)
    }
    entries.push({ label: menu.title, run: run(menu) })
  })
  return entries
}

/**
 * An activity bar entry whose views are sections: each has a header with
 * its title, badge, and buttons, and can be collapsed.
 */
export function ExtensionContainerPanel({ container }: { container: string }) {
  const extensions = useStore(extensionsStore)
  const views = useContributedViews()
  const all = useMemo(() => sectionsOf(container, extensions), [container, extensions])
  const sections = useShownSections(all, views)
  const saved = useStore(layoutStore).sections[container]
  const collapsed = useMemo(() => new Set(saved?.collapsed ?? []), [saved])
  const open = sections.filter(section => !collapsed.has(section))
  const root = useRef<HTMLDivElement>(null)
  const heights = useSectionHeights(root, sections, open)

  // Open sections share the height in proportion to their saved sizes; one
  // without a size gets the others' average.
  const savedSizes = saved?.sizes ?? {}
  const known = open.map(section => savedSizes[section]).filter(size => size !== undefined)
  const average = known.length > 0 ? known.reduce((a, b) => a + b, 0) / known.length : 1
  const share = (section: string) => savedSizes[section] ?? average

  const toggle = (section: string) =>
    updateSections(container, current => ({
      ...current,
      collapsed: collapsed.has(section)
        ? current.collapsed.filter(view => view !== section)
        : [...current.collapsed, section],
    }))

  /**
   * Moves the boundary above `section`, between it and the open section
   * before it. Only those two change: the shares are scaled to the pair's
   * pixels (which keeps every section's height), and the pair splits its
   * total. Taking the others' measured heights as their shares instead would
   * shrink them a little on every move, as heights include borders that
   * shares don't.
   */
  const resize = (section: string, above: string, height: number) => {
    const total = (heights.get(section) ?? 0) + (heights.get(above) ?? 0)
    const scale = total / (share(section) + share(above))
    updateSections(container, current => ({
      ...current,
      sizes: {
        ...current.sizes,
        ...Object.fromEntries(open.map(view => [view, share(view) * scale])),
        [section]: height,
        [above]: total - height,
      },
    }))
  }

  return (
    <div ref={root} className="extension-container">
      {sections.map(section => {
        const isOpen = !collapsed.has(section)
        const index = open.indexOf(section)
        const above = index > 0 ? open[index - 1] : undefined
        const height = heights.get(section) ?? 0
        const title = views.get(section)?.title ?? section
        return (
          <section
            key={section}
            className="extension-section"
            data-open={isOpen || undefined}
            data-section={section}
            aria-label={title}
            style={isOpen ? { flexGrow: share(section) } : undefined}
          >
            {above !== undefined && height > 0 && (
              <Sash
                label={`Resize ${title}`}
                orientation="horizontal"
                value={height}
                min={MIN_SECTION_HEIGHT}
                max={Math.max(
                  MIN_SECTION_HEIGHT,
                  height + (heights.get(above) ?? 0) - MIN_SECTION_HEIGHT,
                )}
                onChange={next => resize(section, above, next)}
              />
            )}
            <SectionHeader
              view={section}
              title={title}
              open={isOpen}
              toggle={() => toggle(section)}
            />
            {isOpen && (
              <div className="extension-view extension-section__body">
                <ViewBody view={section} />
              </div>
            )}
          </section>
        )
      })}
    </div>
  )
}

/** The smallest an open section gets by resizing: its header and two rows. */
const MIN_SECTION_HEIGHT = 66

/** The heights of a container's open sections (in `root`), measured as they change. */
function useSectionHeights(
  root: RefObject<HTMLDivElement | null>,
  sections: readonly string[],
  open: readonly string[],
): ReadonlyMap<string, number> {
  const [heights, setHeights] = useState<ReadonlyMap<string, number>>(new Map())
  const key = open.join('\n')
  useLayoutEffect(() => {
    const elements = [
      ...(root.current?.querySelectorAll<HTMLElement>(':scope > [data-section]') ?? []),
    ]
    const measure = () =>
      setHeights(current => {
        const next = new Map<string, number>()
        for (const element of elements) {
          const section = element.dataset.section
          if (section && element.dataset.open !== undefined) {
            next.set(section, element.getBoundingClientRect().height)
          }
        }
        const same =
          next.size === current.size &&
          [...next].every(([view, height]) => current.get(view) === height)
        return same ? current : next
      })
    measure()
    const observer = new (resizeObserverFor(elements[0]))(measure)
    for (const element of elements) {
      observer.observe(element)
    }
    return () => observer.disconnect()
  }, [root, sections, key])
  return heights
}

function SectionHeader({
  view,
  title,
  open,
  toggle,
}: {
  view: string
  title: string
  open: boolean
  toggle: () => void
}) {
  const content = useViewContent(view)
  const badge = content?.badge ?? 0
  return (
    <div className="extension-section__header">
      <button
        type="button"
        className="extension-section__toggle"
        aria-expanded={open}
        onClick={toggle}
      >
        <span className="extension-view__chevron" aria-hidden="true">
          {open ? '▾' : '▸'}
        </span>
        <span className="extension-section__title">{title}</span>{' '}
        {badge > 0 && <span className="extension-view__badge">{badge}</span>}
      </button>
      {open && (
        <span className="extension-section__actions">
          <ExtensionViewActions view={view} />
        </span>
      )}
    </div>
  )
}

/** A view's text box, welcome message, message, and items. */
function ViewBody({ view }: { view: string }) {
  const content = useViewContent(view)
  const declared = useContributedViews().get(view)

  useEffect(() => {
    void viewOpened(view).catch(fail('Opening the view'))
  }, [view])

  if (!content) {
    return null
  }
  const empty = content.items.length === 0
  return (
    <>
      {declared?.input && (
        <ViewInput
          view={view}
          content={content}
          placeholder={declared.input.placeholder}
          command={declared.input.command}
        />
      )}
      {content.message && <p className="extension-view__message">{content.message}</p>}
      {empty && content.welcome && (
        <div className="extension-view__welcome">
          {content.welcome.text.split(/\n\s*\n/).map((paragraph, index) => (
            <p key={index}>{paragraph}</p>
          ))}
          {content.welcome.buttons.map(button => (
            <button
              key={button.label}
              type="button"
              className="panel-button extension-view__welcome-button"
              onClick={() =>
                void runContributed(button.command, button.argument).catch(fail(button.label))
              }
            >
              {button.label}
            </button>
          ))}
        </div>
      )}
      {!empty && (
        <ViewTree
          view={view}
          items={content.items}
          flags={content.flags}
          graph={content.graph ?? NO_GRAPH}
        />
      )}
    </>
  )
}

/** How long typing pauses before the text box's text is sent. */
const INPUT_DELAY = 150

function ViewInput({
  view,
  content,
  placeholder,
  command,
}: {
  view: string
  content: ViewContent
  placeholder: string | null
  command: string | null
}) {
  const [text, setText] = useState(content.input)
  const sent = useRef(content.input)
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)

  // The extension changed it (e.g. cleared it after a commit). Checked on
  // every new content, not only when its text differs from the last
  // content's: typing isn't sent back, so the last content may already
  // have had the text the extension set.
  useEffect(() => {
    if (content.input !== sent.current) {
      sent.current = content.input
      setText(content.input)
    }
  }, [content])
  useEffect(() => () => clearTimeout(timer.current), [])

  const send = (value: string) => {
    clearTimeout(timer.current)
    sent.current = value
    return setViewInput(view, value)
  }

  return (
    <div className="extension-view__input">
      <textarea
        className="extension-view__textarea"
        rows={Math.min(6, Math.max(1, text.split('\n').length))}
        value={text}
        placeholder={placeholder ?? undefined}
        aria-label={placeholder ?? 'Text'}
        spellCheck
        onChange={event => {
          const value = event.target.value
          setText(value)
          clearTimeout(timer.current)
          timer.current = setTimeout(() => void send(value).catch(fail('Typing')), INPUT_DELAY)
        }}
        onKeyDown={event => {
          if (event.key === 'Enter' && (isMac() ? event.metaKey : event.ctrlKey) && command) {
            event.preventDefault()
            event.stopPropagation()
            send(text)
              .then(() => runContributed(command))
              .catch(fail(placeholder ?? 'The command'))
          }
        }}
      />
    </div>
  )
}

function ItemIconView({ icon, open }: { icon: ItemIcon; open: boolean }) {
  const iconTheme = useStore(iconThemeStore)
  const light = useStore(themeKind) === 'light'
  if (icon.kind === 'named') {
    return isActionIcon(icon.name) ? (
      <span className="extension-view__icon">
        <Icon name={icon.name} size={16} />
      </span>
    ) : null
  }
  const name = icon.path.split(/[/\\]/).pop() ?? icon.path
  const folder = icon.kind === 'folder' ? { expanded: open } : null
  return (
    <FileIcon
      icon={entryIcon(name, folder, iconTheme, light)}
      kind={folder ? 'folder' : 'file'}
      className="extension-view__icon"
    />
  )
}

const NO_GRAPH: readonly GraphRow[] = []

function ViewTree({
  view,
  items,
  flags,
  graph,
}: {
  view: string
  items: readonly ViewEntry[]
  flags: readonly string[]
  graph: readonly GraphRow[]
}) {
  const extensions = useStore(extensionsStore)
  const color = useColorResolver()
  const itemMenus = useMemo(() => viewMenus(view, 'view/item', extensions), [view, extensions])
  /** Items the user opened (true) or closed (false); others are as the extension says. */
  const [toggled, setToggled] = useState<ReadonlyMap<string, boolean>>(new Map())
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set())
  const [anchor, setAnchor] = useState<string | null>(null)
  const [focused, setFocused] = useState(0)
  /**
   * Items whose children were asked for, with the content they were missing
   * from (asked again only if newer content still lacks them), and whether
   * the answer came.
   */
  const [requested, setRequested] = useState<
    ReadonlyMap<string, { items: readonly ViewEntry[]; done: boolean }>
  >(new Map())
  const listRef = useRef<HTMLUListElement>(null)

  useEffect(() => {
    const ids = new Set(items.map(item => item.id))
    setSelected(current => new Set([...current].filter(id => ids.has(id))))
  }, [items])

  const isOpen = (item: ViewEntry) => toggled.get(item.id) ?? item.expanded
  const rows = useMemo(
    () => visibleRows(items, item => toggled.get(item.id) ?? item.expanded),
    [items, toggled],
  )
  const current = Math.min(focused, rows.length - 1)

  // A history graph beside the rows: its rows' shapes, and lanes carried
  // through the rows under an opened item.
  const shapes = useMemo(() => {
    if (graph.length === 0) {
      return null
    }
    const byItem = new Map(graph.map(row => [row.item, row]))
    let last: GraphRow | null = null
    const result = rows.map((row): GraphShape | null => {
      const own = byItem.get(row.item.id)
      if (own) {
        last = own
        return graphShape(own)
      }
      return last && row.depth > 0 ? passingShape(last.outputs) : null
    })
    const lanes = Math.max(0, ...result.map(shape => shape?.lanes ?? 0))
    return { result, width: lanes * LANE_WIDTH }
  }, [graph, rows])
  const graphWidth = shapes?.width ?? 0

  // Opened items whose children aren't listed: ask for them.
  useEffect(() => {
    for (const row of rows) {
      if (!row.pending) {
        continue
      }
      const id = row.item.id.slice(0, -'\0pending'.length)
      if (requested.get(id)?.items === items) {
        continue
      }
      setRequested(previous => new Map(previous).set(id, { items, done: false }))
      expandViewItem(view, id)
        .catch(fail('Loading items'))
        .finally(() =>
          setRequested(previous => {
            const at = previous.get(id)
            return at?.items === items ? new Map(previous).set(id, { items, done: true }) : previous
          }),
        )
    }
  }, [rows, requested, items, view])

  const toggle = (id: string, expand?: boolean) => {
    const item = items.find(candidate => candidate.id === id)
    if (item) {
      setToggled(previous => new Map(previous).set(id, expand ?? !isOpen(item)))
    }
  }

  const select = (
    index: number,
    event: { shiftKey: boolean; ctrlKey: boolean; metaKey: boolean },
  ) => {
    const row = rows[index]
    if (!row || row.pending) {
      return
    }
    setFocused(index)
    const id = row.item.id
    if (event.shiftKey && anchor !== null) {
      const from = rows.findIndex(candidate => candidate.item.id === anchor)
      const [start, end] = from < index ? [from, index] : [index, from]
      setSelected(
        new Set(
          rows
            .slice(Math.max(0, start), end + 1)
            .filter(candidate => !candidate.pending)
            .map(candidate => candidate.item.id),
        ),
      )
      return
    }
    if (isMac() ? event.metaKey : event.ctrlKey) {
      setSelected(current => {
        const next = new Set(current)
        if (next.has(id)) {
          next.delete(id)
        } else {
          next.add(id)
        }
        return next
      })
    } else {
      setSelected(new Set([id]))
    }
    setAnchor(id)
  }

  const activate = (row: Row) => {
    if (row.item.command) {
      runContributed(row.item.command, row.item.argument).catch(fail(row.item.label))
    } else if (row.hasChildren) {
      toggle(row.item.id)
    }
  }

  /** The arguments a menu command gets: the selection's, if the row is in it. */
  const selectionFor = (row: Row) => {
    const chosen = selected.has(row.item.id) ? rows.filter(r => selected.has(r.item.id)) : [row]
    return chosen.flatMap(r => (r.item.argument === null ? [] : [r.item.argument]))
  }
  const menusFor = (row: Row) =>
    itemMenus.filter(menu => forContext(menu, row.item.context) && whenHolds(menu.when, flags))
  const runMenu = (menu: ViewMenuItem, row: Row) =>
    runViewCommand(menu.command, row.item.argument, selectionFor(row)).catch(fail(menu.title))

  const openMenu = (row: Row, index: number, x: number, y: number) => {
    if (!selected.has(row.item.id)) {
      setSelected(new Set([row.item.id]))
      setAnchor(row.item.id)
    }
    setFocused(index)
    const entries = menusFor(row).filter(menu => !menu.inline)
    if (entries.length > 0) {
      openContextMenu(
        x,
        y,
        grouped(entries, menu => () => void runMenu(menu, row)),
      )
    }
  }

  const onKeyDown = (event: KeyboardEvent<HTMLUListElement>) => {
    const row = rows[current]
    if (!row) {
      return
    }
    const move = (index: number) => {
      event.preventDefault()
      const next = Math.max(0, Math.min(rows.length - 1, index))
      select(next, { shiftKey: event.shiftKey, ctrlKey: false, metaKey: false })
    }
    switch (event.key) {
      case 'ArrowDown':
        move(current + 1)
        break
      case 'ArrowUp':
        move(current - 1)
        break
      case 'Home':
        move(0)
        break
      case 'End':
        move(rows.length - 1)
        break
      case 'ArrowRight':
        event.preventDefault()
        if (row.hasChildren) {
          toggle(row.item.id, true)
        }
        break
      case 'ArrowLeft':
        event.preventDefault()
        if (row.hasChildren && isOpen(row.item)) {
          toggle(row.item.id, false)
        } else if (row.item.parent) {
          move(rows.findIndex(candidate => candidate.item.id === row.item.parent))
        }
        break
      case 'Enter':
        event.preventDefault()
        activate(row)
        break
      case 'a':
        if (isMac() ? event.metaKey : event.ctrlKey) {
          event.preventDefault()
          setSelected(new Set(rows.filter(r => !r.pending).map(r => r.item.id)))
        }
        break
    }
  }

  useEffect(() => {
    listRef.current?.children[current]?.scrollIntoView({ block: 'nearest' })
  }, [current])

  const label = useContributedViews().get(view)?.title ?? 'Items'
  return (
    <ul
      ref={listRef}
      className="extension-view__tree"
      role="tree"
      aria-label={label}
      aria-multiselectable="true"
      tabIndex={0}
      aria-activedescendant={rows[current] ? `${view}-${current}` : undefined}
      onKeyDown={onKeyDown}
      onContextMenu={event => {
        // From the keyboard (Shift+F10, the menu key): the focused row's menu.
        const row = rows[current]
        if (event.target !== event.currentTarget || !row || row.pending) {
          return
        }
        event.preventDefault()
        const rect = listRef.current?.children[current]?.getBoundingClientRect()
        if (rect) {
          openMenu(row, current, rect.left + rect.width / 3, rect.bottom)
        }
      }}
    >
      {rows.map((row, index) => {
        if (row.pending) {
          const id = row.item.id.slice(0, -'\0pending'.length)
          const done = requested.get(id)?.items === items && requested.get(id)?.done
          return (
            <li
              key={row.item.id}
              id={`${view}-${index}`}
              role="treeitem"
              aria-level={row.depth + 1}
              aria-selected={false}
              className="extension-view__row extension-view__row--pending"
              style={{ paddingLeft: 8 + graphWidth + row.depth * 12 + 16 }}
            >
              {done ? 'No items' : 'Loading…'}
            </li>
          )
        }
        const { item } = row
        const mark = item.mark
        const markColor = mark?.color ? color(mark.color) : null
        const inline = menusFor(row).filter(menu => menu.inline)
        return (
          <li
            key={item.id}
            id={`${view}-${index}`}
            role="treeitem"
            aria-level={row.depth + 1}
            aria-expanded={row.hasChildren ? isOpen(item) : undefined}
            aria-selected={selected.has(item.id)}
            aria-description={mark?.badge && item.tooltip ? item.tooltip : undefined}
            className="extension-view__row"
            data-focused={index === current || undefined}
            data-faded={mark?.faded || undefined}
            data-graph={graphWidth > 0 || undefined}
            style={
              {
                paddingLeft: 8 + graphWidth + row.depth * 12,
                ...(markColor && { '--view-mark-color': markColor }),
              } as CSSProperties
            }
            title={item.tooltip ?? undefined}
            onClick={event => {
              select(index, event)
              if (event.shiftKey || (isMac() ? event.metaKey : event.ctrlKey)) {
                return
              }
              if (row.hasChildren && !item.command) {
                toggle(item.id)
              } else {
                activate(row)
              }
            }}
            onContextMenu={(event: MouseEvent) => {
              event.preventDefault()
              event.stopPropagation()
              openMenu(row, index, event.clientX, event.clientY)
            }}
          >
            {shapes?.result[index] && <GraphCell shape={shapes.result[index]} width={graphWidth} />}
            <span
              className="extension-view__chevron"
              onClick={event => {
                if (row.hasChildren) {
                  event.stopPropagation()
                  toggle(item.id)
                }
              }}
            >
              {row.hasChildren ? (isOpen(item) ? '▾' : '▸') : ''}
            </span>
            {item.icon && <ItemIconView icon={item.icon} open={isOpen(item)} />}
            <span
              className="extension-view__label"
              data-marked={markColor ? true : undefined}
              data-strikethrough={mark?.strikethrough || undefined}
            >
              {item.label}
            </span>
            {item.description && (
              <span className="extension-view__description">{item.description}</span>
            )}
            {inline.length > 0 && (
              <span className="extension-view__actions">
                {inline.map(menu => (
                  <button
                    key={menu.command}
                    type="button"
                    className="extension-view__action"
                    title={menu.title}
                    aria-label={menu.title}
                    tabIndex={-1}
                    onClick={event => {
                      event.stopPropagation()
                      void runMenu(menu, row)
                    }}
                  >
                    <MenuIcon name={menu.icon} />
                  </button>
                ))}
              </span>
            )}
            {mark?.badge && (
              <span className="extension-view__mark" data-marked={markColor ? true : undefined}>
                {mark.badge}
              </span>
            )}
          </li>
        )
      })}
    </ul>
  )
}
