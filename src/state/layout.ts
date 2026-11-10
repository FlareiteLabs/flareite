/**
 * Workbench layout, restored on the next launch. The backend stores it as
 * opaque JSON; this module owns its shape and validates what comes back.
 * The dock's part is validated by the dock (`dock/model.ts`).
 */
import { getSavedUi, saveUi } from '../ipc/workspace'
import type { WindowBounds } from '../ipc/windows'
import { isDesktop } from '../ipc'
import { documentsStore } from './documents'
import { createStore } from './store'

const SAVE_DELAY_MS = 500
const MAX_EXPANDED = 200
const MAX_CONTAINERS = 50
const MAX_SECTIONS = 50
const MAX_WINDOWS = 16
export const MIN_ZOOM = 0.5
export const MAX_ZOOM = 3

export interface Layout {
  /** Expanded explorer folders (absolute paths). */
  expanded: string[]
  minimapVisible: boolean
  /** The window's zoom factor (1 is 100%). */
  zoom: number
  /**
   * Where everything is docked, as saved (see `dock/dock.ts`). Layouts from
   * before the dock hold `{ sidebarVisible, activeView }` here instead.
   */
  dock: unknown
  /** Extension view containers' sections, by container id. */
  sections: Readonly<Record<string, SectionLayout>>
  /** Where the windows tabs were dragged into are, by id (`dock.windows`). */
  windows: Readonly<Record<string, SavedWindow>>
}

/** Where a window is on screen. */
export interface SavedWindow {
  /** In physical pixels; `null` if the system placed it and it hasn't moved. */
  bounds: WindowBounds | null
  maximized: boolean
}

/** How a view container's sections are laid out. */
export interface SectionLayout {
  /** Sections the user collapsed (full view ids). */
  collapsed: readonly string[]
  /**
   * Open sections' shares of the height, by full view id: in proportion to
   * one another, so they keep their split when the side bar's height
   * changes. A section without one gets the others' average.
   */
  sizes: Readonly<Record<string, number>>
}

export const DEFAULT_LAYOUT: Layout = {
  expanded: [],
  minimapVisible: true,
  zoom: 1,
  dock: null,
  sections: {},
  windows: {},
}

export const layoutStore = createStore<Layout>(DEFAULT_LAYOUT)

export function updateLayout(change: Partial<Layout>): void {
  layoutStore.update(current => ({ ...current, ...change }))
}

/** Validates a saved layout, falling back to defaults field by field. */
export function parseLayout(input: unknown): Layout {
  const saved: Record<string, unknown> =
    typeof input === 'object' && input !== null && !Array.isArray(input)
      ? (input as Record<string, unknown>)
      : {}
  return {
    expanded: Array.isArray(saved.expanded)
      ? saved.expanded
          .filter((path): path is string => typeof path === 'string')
          .slice(0, MAX_EXPANDED)
      : [],
    minimapVisible:
      typeof saved.minimapVisible === 'boolean'
        ? saved.minimapVisible
        : DEFAULT_LAYOUT.minimapVisible,
    zoom:
      typeof saved.zoom === 'number' && saved.zoom >= MIN_ZOOM && saved.zoom <= MAX_ZOOM
        ? saved.zoom
        : DEFAULT_LAYOUT.zoom,
    sections: parseSections(saved.sections),
    windows: parseWindows(saved.windows),
    dock:
      saved.dock ??
      // Before the dock: the side bar's state seeds the default layout.
      (saved.sidebarVisible !== undefined || saved.activeView !== undefined
        ? { sidebarVisible: saved.sidebarVisible, activeView: saved.activeView }
        : null),
  }
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

/** Validates saved window places, dropping what doesn't fit. */
function parseWindows(input: unknown): Record<string, SavedWindow> {
  if (!isRecord(input)) {
    return {}
  }
  const coordinate = (value: unknown) =>
    typeof value === 'number' && Number.isInteger(value) && Math.abs(value) < 1_000_000
  const extent = (value: unknown) => coordinate(value) && (value as number) > 0
  const windows: Record<string, SavedWindow> = {}
  for (const [id, saved] of Object.entries(input).slice(0, MAX_WINDOWS)) {
    if (!/^w\d{1,6}$/.test(id) || !isRecord(saved)) {
      continue
    }
    const bounds = isRecord(saved.bounds) ? saved.bounds : null
    windows[id] = {
      bounds:
        bounds &&
        coordinate(bounds.x) &&
        coordinate(bounds.y) &&
        extent(bounds.width) &&
        extent(bounds.height)
          ? {
              x: bounds.x as number,
              y: bounds.y as number,
              width: bounds.width as number,
              height: bounds.height as number,
            }
          : null,
      maximized: saved.maximized === true,
    }
  }
  return windows
}

/** Validates saved section layouts, dropping what doesn't fit. */
function parseSections(input: unknown): Record<string, SectionLayout> {
  if (!isRecord(input)) {
    return {}
  }
  const sections: Record<string, SectionLayout> = {}
  for (const [container, saved] of Object.entries(input).slice(0, MAX_CONTAINERS)) {
    if (!isRecord(saved)) {
      continue
    }
    const collapsed = Array.isArray(saved.collapsed)
      ? saved.collapsed
          .filter((view): view is string => typeof view === 'string')
          .slice(0, MAX_SECTIONS)
      : []
    const sizes = Object.fromEntries(
      Object.entries(isRecord(saved.sizes) ? saved.sizes : {})
        .filter(
          (entry): entry is [string, number] =>
            typeof entry[1] === 'number' && Number.isFinite(entry[1]) && entry[1] > 0,
        )
        .slice(0, MAX_SECTIONS),
    )
    sections[container] = { collapsed, sizes }
  }
  return sections
}

/** Changes how a view container's sections are laid out. */
export function updateSections(
  container: string,
  change: (current: SectionLayout) => SectionLayout,
): void {
  layoutStore.update(layout => ({
    ...layout,
    sections: {
      ...layout.sections,
      [container]: change(layout.sections[container] ?? { collapsed: [], sizes: {} }),
    },
  }))
}

/**
 * Loads the saved layout, then saves changes to it (and to the active file)
 * after they settle. Returns the saved active file path, if any.
 */
export async function restoreLayout(): Promise<string | null> {
  if (!isDesktop()) {
    return null
  }
  const saved = await getSavedUi()
  layoutStore.set(parseLayout(saved.layout))

  let timer: ReturnType<typeof setTimeout> | undefined
  const scheduleSave = () => {
    clearTimeout(timer)
    timer = setTimeout(() => {
      const { documents, activeId } = documentsStore.get()
      const active = documents.find(document => document.id === activeId)?.path ?? null
      const layout = layoutStore.get()
      void saveUi({ ...layout, expanded: layout.expanded.slice(0, MAX_EXPANDED) }, active).catch(
        () => {},
      )
    }, SAVE_DELAY_MS)
  }
  layoutStore.subscribe(scheduleSave)
  documentsStore.subscribe(scheduleSave)
  return saved.activeFile
}
