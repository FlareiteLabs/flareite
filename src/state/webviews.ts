/**
 * Webviews: extension pages that viewer and preview tabs show in sandboxed
 * frames (`WebviewFrame`). This is how the rest of the workbench reaches
 * them:
 *
 * - **Posts to one page** (an extension's message, a tab action's click)
 *   wait here until its frame has loaded the page, since an extension often
 *   sends one as the tab opens, and while the tab is in the background (its
 *   frame unmounted).
 * - **Broadcasts** (the theme, stored values that changed) go to the pages
 *   loaded now. Pages loaded later get the theme in their HTML, and read
 *   stored values themselves.
 * - **Tab action states** that pages set (`flareite.setActionState`), for
 *   the buttons beside their tabs.
 */
import { setWebviewTheme } from '../ipc/extensions'
import { THEME_TOKENS, themeKind, themeRevision } from '../themes/theme'
import { reportError } from './notifications'
import { createStore } from './store'

/** Bumped per webview when its extension replaces the page, so its frame loads it again. */
export const webviewVersions = createStore<Readonly<Record<string, number>>>({})

/** Sends a page one of the bridge's message types (`webview.js`). */
export type Post = (type: string, data?: unknown) => void

/** Most posts kept for a page that isn't loaded; older ones are dropped. */
const MAX_WAITING = 1_000

const waiting = new Map<string, [string, unknown][]>()
const frames = new Map<string, Post>()

export function webviewChanged(id: string): void {
  webviewVersions.update(versions => ({ ...versions, [id]: (versions[id] ?? 0) + 1 }))
}

/** Posts to a webview's page now, or once it has loaded. */
export function postToWebview(id: string, type: string, data?: unknown): void {
  const post = frames.get(id)
  if (post) {
    post(type, data)
    return
  }
  const queue = waiting.get(id) ?? []
  queue.push([type, data])
  if (queue.length > MAX_WAITING) {
    queue.shift()
  }
  waiting.set(id, queue)
}

/** An extension sent a message (JSON text) to a webview's page. */
export function webviewMessage(id: string, message: string): void {
  postToWebview(id, 'message', message)
}

/**
 * A webview's page has loaded: `post` gets what waited for it, then what
 * comes. Returns a function that stops it.
 */
export function registerWebviewFrame(id: string, post: Post): () => void {
  frames.set(id, post)
  const queued = waiting.get(id)
  waiting.delete(id)
  queued?.forEach(([type, data]) => post(type, data))
  return () => {
    if (frames.get(id) === post) {
      frames.delete(id)
    }
  }
}

/** A webview's tab closed. */
export function webviewClosed(id: string): void {
  waiting.delete(id)
  frames.delete(id)
  webviewActionStates.update(states => {
    if (!(id in states)) {
      return states
    }
    const next = { ...states }
    delete next[id]
    return next
  })
}

/** Whether a webview (`<extension id>.<viewer or preview id>/<path or key>`) is an extension's. */
const ownedBy = (id: string, extension: string) =>
  (id.split('/')[0] ?? '').startsWith(`${extension}.`)

/**
 * A value an extension stores changed (`value` JSON text, or `null` when
 * removed), or all were cleared (`scope` and `key` `null`): its loaded pages
 * hear of it, except the one that changed it.
 */
export function storageChanged(
  extension: string,
  scope: string | null,
  key: string | null,
  value: string | null,
  source: string | null,
): void {
  for (const [id, post] of frames) {
    if (id !== source && ownedBy(id, extension)) {
      post('storageChanged', { scope, key, value })
    }
  }
}

/** What a page set for one of its tab's actions. */
export interface ActionState {
  pressed?: boolean
  disabled?: boolean
  hidden?: boolean
  title?: string
}

/** Action states by webview, then action id. */
export const webviewActionStates = createStore<
  Readonly<Record<string, Readonly<Record<string, ActionState>>>>
>({})

/** A page set an action's state; only known fields of the right types are kept. */
export function setActionState(id: string, action: string, state: unknown): void {
  if (typeof state !== 'object' || state === null) {
    return
  }
  const given = state as Record<string, unknown>
  const next: ActionState = {}
  for (const flag of ['pressed', 'disabled', 'hidden'] as const) {
    if (typeof given[flag] === 'boolean') {
      next[flag] = given[flag]
    }
  }
  if (typeof given.title === 'string') {
    next.title = given.title.slice(0, 100)
  }
  webviewActionStates.update(states => ({
    ...states,
    [id]: { ...states[id], [action]: { ...states[id]?.[action], ...next } },
  }))
}

/** The theme as pages get it: `--flareite-<name>` variables. */
export interface WebviewTheme {
  kind: 'dark' | 'light'
  variables: Record<string, string>
}

/** Workbench tokens pages get besides the colors (`--color-<token>`). */
const OTHER_TOKENS = [
  'font-ui',
  'font-mono',
  'font-size-ui',
  'font-size-ui-small',
  'font-size-editor',
  'radius-control',
]

let theme: WebviewTheme | null = null

/** The theme pages have, once the workbench has one. */
export const currentWebviewTheme = (): WebviewTheme | null => theme

/**
 * Reads the workbench's theme and gives it to pages: to the backend, which
 * puts it in pages it serves from now on, and to loaded pages.
 */
export function syncWebviewTheme(): void {
  const style = getComputedStyle(document.documentElement)
  const variables: Record<string, string> = {}
  const read = (name: string, as: string) => {
    const value = style.getPropertyValue(`--${name}`).trim()
    if (value) {
      variables[as] = value
    }
  }
  for (const token of THEME_TOKENS) {
    read(`color-${token}`, token)
  }
  for (const token of OTHER_TOKENS) {
    read(token, token)
  }
  const next: WebviewTheme = { kind: themeKind.get(), variables }
  // The workbench applies its theme again when settings change.
  if (JSON.stringify(next) === JSON.stringify(theme)) {
    return
  }
  theme = next
  setWebviewTheme(theme.kind, variables).catch((error: unknown) =>
    reportError('Theming extension pages', error),
  )
  for (const post of frames.values()) {
    post('theme', theme)
  }
  for (const listener of themeListeners) {
    listener(theme)
  }
}

const themeListeners = new Set<(theme: WebviewTheme) => void>()

/**
 * Calls `listener` with the theme pages get, as it changes: for file pages,
 * which follow the theme but aren't webviews (they hear nothing else).
 * Returns a function that stops.
 */
export function onWebviewTheme(listener: (theme: WebviewTheme) => void): () => void {
  themeListeners.add(listener)
  return () => themeListeners.delete(listener)
}

themeRevision.subscribe(syncWebviewTheme)
