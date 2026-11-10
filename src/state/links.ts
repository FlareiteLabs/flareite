/**
 * Opening links (API 1.15): web addresses and file paths the user
 * Ctrl+clicks in the terminal, in the editor's text, and in previews.
 *
 * Flareite's own way opens web addresses with the system's browser, files
 * in the editor (at the line and column the link names), and folders in
 * the system's file manager. Extensions' `[[link_handlers]]` can open links
 * instead; the `workbench.open_web_links` and `open_file_links` settings
 * choose: `auto` (the first installed handler, else Flareite's way),
 * `default` (Flareite's way), or a handler's id. Ctrl+Shift+click asks.
 */
import { openExternal } from '../ipc'
import {
  openFolderExternal,
  openLinkWith,
  pathKinds,
  type LinkHandlerContribution,
} from '../ipc/extensions'
import { openQuickPick } from '../components/paletteState'
import { openPath } from './documents'
import { extensionsStore, patternMatcher, usableExtensions } from './extensions'
import { reportError } from './notifications'
import { createStore } from './store'

export interface Link {
  kind: 'web' | 'file'
  /** A web address, or a file's absolute path. */
  target: string
  /** For a file: the line the link names, from 0. */
  line?: number | null
  /** For a file: the column the link names, from 0. */
  column?: number | null
}

/** The user's choice of who opens links: `auto`, `default`, or a handler's id. */
export const linkSettings = createStore<{ web: string; file: string }>({
  web: 'auto',
  file: 'auto',
})

const onMac = () => typeof navigator !== 'undefined' && /Mac/.test(navigator.userAgent)

/** What a link found in text says when hovered. */
export const LINK_TOOLTIP = `Follow link (${onMac() ? 'Cmd' : 'Ctrl'}+click; with Shift, choose how)`

/** Whether a click follows a link found in text: Ctrl (Cmd on macOS), with or without Shift. */
export const followsLink = (event: { ctrlKey: boolean; metaKey: boolean; altKey: boolean }) =>
  (onMac() ? event.metaKey : event.ctrlKey) && !event.altKey

const fileName = (path: string) => path.split(/[/\\]/).pop() ?? path
const schemeOf = (url: string) => /^([a-z][a-z0-9+.-]*):/i.exec(url)?.[1]?.toLowerCase() ?? ''

/** The handlers of usable extensions that can open `link`, in the order extensions load. */
export function handlersFor(link: Link): LinkHandlerContribution[] {
  return usableExtensions(extensionsStore.get()).flatMap(extension =>
    extension.contributions.linkHandlers.filter(handler => {
      if (handler.kind !== link.kind) {
        return false
      }
      if (link.kind === 'web') {
        return handler.schemes.includes(schemeOf(link.target))
      }
      const name = fileName(link.target)
      return (
        handler.patterns.length === 0 ||
        handler.patterns.some(pattern => patternMatcher(pattern).test(name))
      )
    }),
  )
}

/** Who opens a link: a handler, or `null` for Flareite's way. */
function chosenHandler(link: Link): LinkHandlerContribution | null {
  const handlers = handlersFor(link)
  const setting = link.kind === 'web' ? linkSettings.get().web : linkSettings.get().file
  if (setting === 'default') {
    return null
  }
  return handlers.find(handler => handler.fullId === setting) ?? handlers[0] ?? null
}

/**
 * Opens a link as the settings say; with `choose`, asks who should open it
 * (Ctrl+Shift+click).
 */
export function openLink(link: Link, choose = false): void {
  if (choose) {
    chooseHow(link)
    return
  }
  const handler = chosenHandler(link)
  if (handler) {
    runHandler(handler, link)
  } else {
    void openDefault(link)
  }
}

function runHandler(handler: LinkHandlerContribution, link: Link): void {
  openLinkWith(handler.fullId, {
    kind: link.kind,
    target: link.target,
    line: link.line ?? null,
    column: link.column ?? null,
  }).catch((error: unknown) => reportError(handler.title, error))
}

/** Asks who should open a link, this once. */
function chooseHow(link: Link): void {
  const own = link.kind === 'web' ? 'Open in Your Browser' : 'Open in Flareite'
  const chosen = chosenHandler(link)
  openQuickPick({
    placeholder: `Open ${link.target} with`,
    items: [
      {
        label: own,
        detail: chosen === null ? 'Default' : undefined,
        run: () => void openDefault(link),
      },
      ...handlersFor(link).map(handler => ({
        label: handler.title,
        detail: chosen?.fullId === handler.fullId ? 'Default' : undefined,
        run: () => runHandler(handler, link),
      })),
    ],
  })
}

/**
 * Opens a link Flareite's own way: a web address with the system's
 * browser, a file in the editor, a folder in the system's file manager.
 */
export async function openDefault(link: Link): Promise<void> {
  try {
    if (link.kind === 'web') {
      await openExternal(link.target)
      return
    }
    const [kind] = await pathKinds([link.target])
    if (kind === 'folder') {
      await openFolderExternal(link.target)
    } else if (kind === 'file') {
      const line = link.line ?? null
      await openPath(
        link.target,
        line === null ? undefined : { start: { line, column: link.column ?? 0 } },
      )
    } else {
      reportError('Opening the link', new Error(`${link.target} doesn't exist.`))
    }
  } catch (error) {
    reportError('Opening the link', error)
  }
}
