/**
 * One terminal in the frontend: an xterm.js emulator connected to a shell
 * in the backend.
 *
 * The emulator exists before it's shown, so output that arrives while the
 * panel is hidden isn't lost; showing the terminal moves its element into
 * the panel. Input typed (or replies the emulator generates) before the
 * backend has named the terminal are queued.
 */
import { FitAddon } from '@xterm/addon-fit'
import { Terminal, type ITerminalOptions, type ITheme } from '@xterm/xterm'
import '@xterm/xterm/css/xterm.css'
import { isMac } from '../editor/keymap'
import {
  acknowledgeTerminal,
  resizeTerminal,
  writeTerminal,
  type TerminalMessage,
} from '../ipc/terminal'
import { reportError } from '../state/notifications'
import { linkProvider, osc7Folder } from './links'

/**
 * Output drawn before telling the backend, which pauses a shell that gets
 * too far ahead. At most the backend's low water mark (100,000), or it could
 * stay paused (see `flareite_terminal::flow`).
 */
export const ACKNOWLEDGE_EVERY = 50_000

export interface SessionEvents {
  /** The shell exited, after all its output was drawn. */
  exited(session: TerminalSession, code: number | null): void
  /** A key was pressed after the shell exited. */
  inputAfterExit(session: TerminalSession): void
  /** The shell set the window title (OSC 0 or 2). */
  titleChanged(session: TerminalSession, title: string): void
}

const ANSI_NAMES = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white'] as const

/** The terminal's colors from the theme tokens in effect. */
export function terminalTheme(
  style: CSSStyleDeclaration = getComputedStyle(document.documentElement),
): ITheme {
  const token = (name: string) =>
    style.getPropertyValue(`--color-terminal-${name}`).trim() || undefined
  const theme: Record<string, string | undefined> = {
    background: token('background'),
    foreground: token('foreground'),
    cursor: token('cursor'),
    cursorAccent: token('background'),
    selectionBackground: token('selection'),
  }
  for (const name of ANSI_NAMES) {
    theme[name] = token(`ansi-${name}`)
    theme[`bright${name[0]!.toUpperCase()}${name.slice(1)}`] = token(`ansi-bright-${name}`)
  }
  return theme as ITheme
}

export interface AppearanceOptions {
  fontSize: number
  scrollback: number
  screenReader: boolean
}

/** Emulator options from settings and the theme. */
export function terminalOptions({
  fontSize,
  scrollback,
  screenReader,
}: AppearanceOptions): ITerminalOptions {
  const style = getComputedStyle(document.documentElement)
  return {
    fontFamily: style.getPropertyValue('--font-mono').trim() || 'monospace',
    fontSize,
    scrollback,
    screenReaderMode: screenReader,
    theme: terminalTheme(style),
  }
}

/**
 * Keys left to the browser, whose clipboard events xterm.js handles: paste,
 * and copy when text is selected. Ctrl+C without a selection interrupts, as
 * in any terminal. On Linux, Ctrl+V stays a key for the shell, as there.
 */
export function isClipboardKey(event: KeyboardEvent, hasSelection: boolean, mac: boolean): boolean {
  if (event.type !== 'keydown' || event.altKey) {
    return false
  }
  const key = event.key.toLowerCase()
  if (mac) {
    return event.metaKey && !event.ctrlKey && (key === 'v' || (key === 'c' && hasSelection))
  }
  if (!event.ctrlKey || event.metaKey) {
    return false
  }
  const windows = /Windows/.test(navigator.userAgent)
  if (key === 'v') {
    return event.shiftKey || windows
  }
  return key === 'c' && (event.shiftKey || (windows && hasSelection))
}

export class TerminalSession {
  readonly xterm: Terminal
  private readonly fitAddon = new FitAddon()
  private readonly events: SessionEvents
  /** Set once the backend has created or attached the terminal. */
  private id: number | null = null
  private unacknowledged = 0
  private queuedInput: string[] = []
  /** Replayed output being drawn; the emulator's replies to it are stale. */
  private replaying = 0
  private exitCode: number | null | undefined = undefined
  /** The shell's folder, when it says (OSC 7), for relative paths in links. */
  private cwd: string | null = null

  constructor(events: SessionEvents, options: ITerminalOptions) {
    this.events = events
    this.xterm = new Terminal({
      ...options,
      // Keep text readable whatever colors programs pick.
      minimumContrastRatio: 4.5,
      allowProposedApi: false,
    })
    this.xterm.loadAddon(this.fitAddon)
    this.xterm.attachCustomKeyEventHandler(
      event => !isClipboardKey(event, this.xterm.hasSelection(), isMac()),
    )
    this.xterm.onData(data => this.input(data))
    this.xterm.onResize(({ cols, rows }) => {
      if (this.id !== null) {
        void resizeTerminal(this.id, cols, rows).catch(() => {})
      }
    })
    this.xterm.onTitleChange(title => this.events.titleChanged(this, title))
    // Web addresses and file paths in the output open with Ctrl+click.
    this.xterm.parser.registerOscHandler(7, data => {
      this.cwd = osc7Folder(data) ?? this.cwd
      return true
    })
    this.xterm.registerLinkProvider(linkProvider(this.xterm, () => this.cwd))
  }

  get terminalId(): number | null {
    return this.id
  }

  /** Whether the shell has exited (and with what code). */
  get exited(): boolean {
    return this.exitCode !== undefined
  }

  /** Handles a message from the backend's channel. */
  readonly receive = (message: TerminalMessage): void => {
    switch (message.type) {
      case 'output':
        this.draw(message.data)
        break
      case 'replay':
        this.replaying += 1
        this.draw(message.data, () => {
          this.replaying -= 1
        })
        break
      case 'exited':
        // Written after everything before it is drawn.
        this.xterm.write('', () => {
          this.exitCode = message.code
          this.events.exited(this, message.code)
        })
        break
    }
  }

  /** The backend's id for this terminal: sends anything queued until now. */
  connect(id: number): void {
    this.id = id
    for (const data of this.queuedInput) {
      this.send(data)
    }
    this.queuedInput = []
    this.acknowledge(0)
    if (this.xterm.element) {
      void resizeTerminal(id, this.xterm.cols, this.xterm.rows).catch(() => {})
    }
  }

  /** Shows the terminal in `parent`, moving it there if it was shown elsewhere. */
  mount(parent: HTMLElement): void {
    const element = this.xterm.element
    if (element) {
      parent.appendChild(element)
      // Opening again after the move lets the emulator follow it into
      // another window (its frames and pixel ratio are that window's).
      this.xterm.open(parent)
    } else {
      this.xterm.open(parent)
    }
    this.fit()
  }

  /** Resizes the terminal to fill its element, if it's visible. */
  fit(): void {
    const element = this.xterm.element
    if (element && element.offsetWidth > 0 && element.offsetHeight > 0) {
      this.fitAddon.fit()
    }
  }

  focus(): void {
    this.xterm.focus()
  }

  setOptions(options: ITerminalOptions): void {
    this.xterm.options = options
    this.fit()
  }

  dispose(): void {
    this.xterm.dispose()
  }

  private draw(data: string, done?: () => void): void {
    this.xterm.write(data, () => {
      done?.()
      this.acknowledge(data.length)
    })
  }

  private acknowledge(units: number): void {
    this.unacknowledged += units
    if (this.id !== null && this.unacknowledged >= ACKNOWLEDGE_EVERY) {
      void acknowledgeTerminal(this.id, this.unacknowledged).catch(() => {})
      this.unacknowledged = 0
    }
  }

  private input(data: string): void {
    if (this.replaying > 0) {
      return
    }
    if (this.exited) {
      this.events.inputAfterExit(this)
    } else if (this.id === null) {
      this.queuedInput.push(data)
    } else {
      this.send(data)
    }
  }

  private send(data: string): void {
    if (this.id !== null) {
      void writeTerminal(this.id, data).catch(error => reportError('Sending input', error))
    }
  }
}
