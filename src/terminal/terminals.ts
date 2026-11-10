/**
 * The integrated terminals and their sessions. Shells run in the backend
 * and survive a window reload; on startup the workbench attaches to the
 * ones still running. Where each terminal shows is up to the dock, which
 * follows this store.
 */
import { activeElement, afterFrame } from '../app/windows'
import {
  attachTerminal,
  createTerminal,
  killTerminal as killBackendTerminal,
  listTerminals,
  terminalPlatform,
} from '../ipc/terminal'
import { appearance } from '../state/appearance'
import { createStore } from '../state/store'
import { themeRevision } from '../themes/theme'
import type { ITerminalOptions } from '@xterm/xterm'
import type { SessionEvents, TerminalSession } from './session'

export interface TerminalTab {
  id: number
  /** The shell's name. */
  title: string
  /** The title the shell set, if any (shown as the tab's tooltip). */
  description: string | null
  /** Whether the shell has exited (it exited with an error, or the tab would be gone). */
  exited: boolean
}

export interface TerminalsState {
  tabs: readonly TerminalTab[]
  /** The terminal used last (for commands such as Kill Terminal). */
  activeId: number | null
}

export const terminalsStore = createStore<TerminalsState>({ tabs: [], activeId: null })
/** Terminals being started, so a second start doesn't begin while one is under way. */
export const startingTerminals = createStore(0)

const sessions = new Map<number, TerminalSession>()

export function getSession(id: number): TerminalSession | undefined {
  return sessions.get(id)
}

const DEFAULT_SIZE = { cols: 80, rows: 24 }

/**
 * The emulator (xterm.js, about as large as the rest of the frontend) loads
 * with the first terminal, so it doesn't slow down startup.
 */
let sessionModule: typeof import('./session') | null = null

let platformOptions: Promise<ITerminalOptions> | null = null

/**
 * On Windows, xterm.js must know it's talking to ConPTY, which redraws the
 * screen itself when the terminal is resized. Otherwise lines from the
 * scrollback are drawn twice.
 */
function loadPlatformOptions(): Promise<ITerminalOptions> {
  platformOptions ??= terminalPlatform().then(
    ({ windowsBuild }) =>
      windowsBuild === null ? {} : { windowsPty: { backend: 'conpty', buildNumber: windowsBuild } },
    () => ({}),
  )
  return platformOptions
}

async function createSession(): Promise<TerminalSession> {
  const [module, platform] = await Promise.all([
    sessionModule ?? import('./session'),
    loadPlatformOptions(),
  ])
  sessionModule = module
  return new module.TerminalSession(events, { ...currentOptions(module), ...platform })
}

function currentOptions({ terminalOptions }: typeof import('./session')) {
  const { fontSize, terminalScrollback, terminalScreenReader } = appearance.get()
  return terminalOptions({
    fontSize,
    scrollback: terminalScrollback,
    screenReader: terminalScreenReader,
  })
}

function updateTab(id: number, change: Partial<TerminalTab>): void {
  terminalsStore.update(state => ({
    ...state,
    tabs: state.tabs.map(tab => (tab.id === id ? { ...tab, ...change } : tab)),
  }))
}

const events: SessionEvents = {
  exited(session, code) {
    const id = session.terminalId
    if (id === null) {
      return
    }
    if (code === 0) {
      closeTerminal(id)
      return
    }
    const reason = code === null ? 'The process exited' : `The process exited with code ${code}`
    session.xterm.write(`\r\n\x1b[0m${reason}. Press any key to close the terminal.\r\n`)
    updateTab(id, { exited: true })
  },
  inputAfterExit(session) {
    if (session.terminalId !== null) {
      closeTerminal(session.terminalId)
    }
  },
  titleChanged(session, title) {
    if (session.terminalId !== null) {
      updateTab(session.terminalId, { description: title || null })
    }
  },
}

/** Whether keyboard focus is in a terminal. */
export function terminalHasFocus(): boolean {
  return activeElement()?.closest('[data-terminal]') != null
}

/** Focuses a terminal once it's rendered. */
export function focusTerminal(id: number): void {
  afterFrame(() => sessions.get(id)?.focus(), sessions.get(id)?.xterm.element)
}

/** Starts a terminal with a profile (the default if `null`) and returns its id. */
export async function newTerminal(profile: string | null = null): Promise<number> {
  startingTerminals.update(count => count + 1)
  let session: TerminalSession | undefined
  let info
  try {
    session = await createSession()
    const active = activeSession()
    const size = active ? { cols: active.xterm.cols, rows: active.xterm.rows } : DEFAULT_SIZE
    info = await createTerminal(size.cols, size.rows, profile, session.receive)
  } catch (error) {
    session?.dispose()
    throw error
  } finally {
    startingTerminals.update(count => count - 1)
  }
  sessions.set(info.id, session)
  session.connect(info.id)
  terminalsStore.update(({ tabs }) => ({
    tabs: [...tabs, { id: info.id, title: info.title, description: null, exited: false }],
    activeId: info.id,
  }))
  return info.id
}

function activeSession(): TerminalSession | undefined {
  const { activeId } = terminalsStore.get()
  return activeId === null ? undefined : sessions.get(activeId)
}

/** Records the terminal used last. */
export function setActiveTerminal(id: number): void {
  if (sessions.has(id) && terminalsStore.get().activeId !== id) {
    terminalsStore.update(state => ({ ...state, activeId: id }))
  }
}

/** Ends a terminal's shell (the last used one by default) and forgets it. */
export function closeTerminal(id = terminalsStore.get().activeId): void {
  if (id === null) {
    return
  }
  sessions.get(id)?.dispose()
  sessions.delete(id)
  void killBackendTerminal(id).catch(() => {})
  const { tabs, activeId } = terminalsStore.get()
  const remaining = tabs.filter(tab => tab.id !== id)
  terminalsStore.set({
    tabs: remaining,
    activeId: activeId !== id ? activeId : (remaining.at(-1)?.id ?? null),
  })
}

export function clearTerminal(id = terminalsStore.get().activeId): void {
  if (id !== null) {
    sessions.get(id)?.xterm.clear()
  }
}

/** Attaches to the terminals still running in the backend (after a window reload). */
export async function restoreTerminals(): Promise<void> {
  for (const info of await listTerminals()) {
    const session = await createSession()
    // Restoring twice (React runs effects twice in development) must not
    // attach twice: the second attach would take the first one's channel.
    if (sessions.has(info.id)) {
      session.dispose()
      continue
    }
    sessions.set(info.id, session)
    session.connect(info.id)
    // The tab exists before the replay, which may close it (a shell that
    // exited while the window reloaded).
    terminalsStore.update(({ tabs }) => ({
      tabs: [...tabs, { id: info.id, title: info.title, description: null, exited: false }],
      activeId: info.id,
    }))
    try {
      await attachTerminal(info.id, session.receive)
    } catch {
      closeTerminal(info.id)
    }
  }
}

/** Applies font, scrollback, and theme changes to every terminal. */
function applyOptions(): void {
  if (sessions.size === 0 || !sessionModule) {
    return
  }
  const options = currentOptions(sessionModule)
  for (const session of sessions.values()) {
    session.setOptions(options)
  }
}

appearance.subscribe(applyOptions)
themeRevision.subscribe(applyOptions)
