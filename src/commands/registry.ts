/**
 * The command registry: every workbench action is a named command with an
 * optional keybinding. The command palette lists them, the keybinding
 * dispatcher runs them, and extension commands register into the same
 * registry.
 *
 * Keybindings come from three places, in order of precedence: the user's
 * `[keybindings]` settings (which replace a command's defaults), built-in
 * defaults, and extension defaults. Extensions can't take a key from a
 * built-in command, but the user can reassign anything.
 *
 * A keybinding can be a chord of two key combinations, such as
 * `"mod+k mod+0"`: after the first, the next key press completes it (or
 * cancels it, if no chord continues that way), as in VS Code.
 */
import { useEffect } from 'react'
import type { KeyInput } from '../editor/keymap'
import { isMac } from '../editor/keymap'
import { reportError } from '../state/notifications'
import { createStore, useStore } from '../state/store'

/**
 * A key combination such as `"mod+shift+p"` or `"f3"`, or a chord of two
 * separated by a space (`"mod+k mod+0"`). `mod` is Ctrl on Windows and
 * Linux and Cmd on macOS; `ctrl`, `alt`, `shift`, and `meta` name keys
 * literally.
 */
export type Keybinding = string | { default: string; mac: string }

export interface Command {
  id: string
  title: string
  category?: string
  keybinding?: Keybinding | Keybinding[]
  /** Whether the command can run now. Defaults to always. */
  enabled?: () => boolean
  /** Only run from the keyboard while the editor has focus. */
  editorFocus?: boolean
  /** Its keys run it only while this holds (e.g. a layout's tab is active). */
  keysWhen?: () => boolean
  /**
   * Also runs from the keyboard while a terminal has focus. Other commands'
   * keys go to the shell there (Ctrl+W deletes a word, Ctrl+P recalls
   * history, and so on).
   */
  terminal?: boolean
  /** Omitted from the command palette. */
  hidden?: boolean
  /** Who registered it; extension keybindings yield to built-in ones. */
  source?: 'builtin' | 'extension'
  run: () => void | Promise<void>
}

const registry = createStore<ReadonlyMap<string, Command>>(new Map())
/** The first key combination of a chord being typed, for display; `null` otherwise. */
export const pendingChord = createStore<string | null>(null)
/** The user's keybindings, by command id; an empty list removes the defaults. */
const overrides = createStore<Readonly<Record<string, readonly string[]>>>({})

export function setKeybindingOverrides(
  keybindings: Readonly<Record<string, readonly string[]>>,
): void {
  overrides.set(keybindings)
}

/** Registers commands; the returned function unregisters them. */
export function registerCommands(commands: readonly Command[]): () => void {
  registry.update(current => {
    const next = new Map(current)
    for (const command of commands) {
      next.set(command.id, command)
    }
    return next
  })
  return () =>
    registry.update(current => {
      const next = new Map(current)
      for (const command of commands) {
        if (next.get(command.id) === command) {
          next.delete(command.id)
        }
      }
      return next
    })
}

/** Calls `listener` whenever commands are registered or removed; returns a function that stops. */
export function subscribeCommands(listener: () => void): () => void {
  return registry.subscribe(listener)
}

export function getCommand(id: string): Command | undefined {
  return registry.get().get(id)
}

export function listCommands(): Command[] {
  return [...registry.get().values()]
}

export function useCommands(): Command[] {
  useStore(overrides)
  return [...useStore(registry).values()]
}

export function commandLabel(command: Command): string {
  return command.category ? `${command.category}: ${command.title}` : command.title
}

export async function executeCommand(id: string): Promise<void> {
  const command = getCommand(id)
  if (!command || (command.enabled && !command.enabled())) {
    return
  }
  try {
    await command.run()
  } catch (error) {
    reportError(commandLabel(command), error)
  }
}

/** The keys bound to `command` on this platform, after the user's overrides. */
export function bindingsOf(command: Command, mac: boolean): string[] {
  const override = overrides.get()[command.id]
  if (override) {
    return [...override]
  }
  const list = command.keybinding === undefined ? [] : [command.keybinding].flat()
  return list.map(binding =>
    typeof binding === 'string' ? binding : mac ? binding.mac : binding.default,
  )
}

interface ParsedBinding {
  key: string
  ctrl: boolean
  meta: boolean
  alt: boolean
  shift: boolean
}

/** A keybinding's key combinations: one, or two for a chord. */
function stepsOf(binding: string): string[] {
  return binding.trim().split(/\s+/)
}

/** One key combination, parsed. */
function parse(step: string, mac: boolean): ParsedBinding {
  // Split on "+" but keep a literal "+" key ("mod++").
  const parts = step.toLowerCase().split(/\+(?!$)/)
  const key = parts.pop() ?? ''
  const has = (name: string) => parts.includes(name)
  return {
    key,
    ctrl: has('ctrl') || (!mac && has('mod')),
    meta: has('meta') || (mac && has('mod')),
    alt: has('alt'),
    shift: has('shift'),
  }
}

/**
 * The unshifted character of punctuation and digit keys, by physical key
 * (US layout). With Shift, `key` reports the shifted character ("~" for the
 * backquote key), but bindings name the unshifted one ("ctrl+shift+`").
 */
function unshiftedKey(code: string | undefined): string | undefined {
  const punctuation: Record<string, string> = {
    Backquote: '`',
    Minus: '-',
    Equal: '=',
    BracketLeft: '[',
    BracketRight: ']',
    Backslash: '\\',
    Semicolon: ';',
    Quote: "'",
    Comma: ',',
    Period: '.',
    Slash: '/',
  }
  return code === undefined ? undefined : (punctuation[code] ?? /^Digit(\d)$/.exec(code)?.[1])
}

/**
 * The letter on a physical key. With Option on macOS, `key` reports a
 * special character ("ˆ" for Option+Shift+I), but bindings name the letter.
 */
function letterKey(code: string | undefined): string | undefined {
  return code === undefined ? undefined : /^Key([A-Z])$/.exec(code)?.[1]?.toLowerCase()
}

/** Whether `event` presses `binding`; a chord never matches a single key press. */
export function matchesKeybinding(binding: string, event: KeyInput, mac: boolean): boolean {
  const steps = stepsOf(binding)
  return steps.length === 1 && matchesStep(steps[0]!, event, mac)
}

function matchesStep(step: string, event: KeyInput, mac: boolean): boolean {
  const parsed = parse(step, mac)
  const key = event.key === ' ' ? 'space' : event.key.toLowerCase()
  return (
    (parsed.key === key ||
      (event.shiftKey && parsed.key === unshiftedKey(event.code)) ||
      (event.altKey &&
        (parsed.key === letterKey(event.code) || parsed.key === unshiftedKey(event.code)))) &&
    parsed.ctrl === event.ctrlKey &&
    parsed.meta === event.metaKey &&
    parsed.alt === event.altKey &&
    parsed.shift === event.shiftKey
  )
}

const keyNames: Record<string, string> = {
  arrowup: '↑',
  arrowdown: '↓',
  arrowleft: '←',
  arrowright: '→',
  pageup: 'PageUp',
  pagedown: 'PageDown',
  tab: 'Tab',
  enter: 'Enter',
  escape: 'Esc',
  space: 'Space',
}

/** Whether two key combinations name the same keys on this platform. */
function sameStep(a: string, b: string, mac: boolean): boolean {
  const [x, y] = [parse(a, mac), parse(b, mac)]
  return (
    x.key === y.key &&
    x.ctrl === y.ctrl &&
    x.meta === y.meta &&
    x.alt === y.alt &&
    x.shift === y.shift
  )
}

/** A keybinding for display, e.g. "Ctrl+Shift+P", "⇧⌘P", or "Ctrl+K Ctrl+0". */
export function formatKeybinding(binding: string, mac: boolean): string {
  return stepsOf(binding)
    .map(step => formatStep(step, mac))
    .join(' ')
}

function formatStep(step: string, mac: boolean): string {
  const parsed = parse(step, mac)
  const key = keyNames[parsed.key] ?? parsed.key.toUpperCase()
  if (mac) {
    return `${parsed.ctrl ? '⌃' : ''}${parsed.alt ? '⌥' : ''}${parsed.shift ? '⇧' : ''}${parsed.meta ? '⌘' : ''}${key}`
  }
  return [
    parsed.ctrl && 'Ctrl',
    parsed.meta && 'Win',
    parsed.alt && 'Alt',
    parsed.shift && 'Shift',
    key,
  ]
    .filter(Boolean)
    .join('+')
}

/** The first keybinding of `command` on this platform, for display. */
export function primaryKeybinding(command: Command | undefined, mac = isMac()): string | null {
  const first = command ? bindingsOf(command, mac)[0] : undefined
  return first ? formatKeybinding(first, mac) : null
}

/** A key combination as a webview page matches it (see `webview.js`). */
export type Keystroke = ParsedBinding

/**
 * The key combinations that run a command, or start a chord, outside the
 * editor and terminal: what a focused webview page passes to the workbench
 * unless it handles them itself.
 */
export function webviewKeystrokes(mac = isMac()): Keystroke[] {
  const seen = new Set<string>()
  const keystrokes: Keystroke[] = []
  for (const command of keyCommands(false, false)) {
    for (const binding of bindingsOf(command, mac)) {
      const keystroke = parse(stepsOf(binding)[0]!, mac)
      const key = JSON.stringify(keystroke)
      if (!seen.has(key)) {
        seen.add(key)
        keystrokes.push(keystroke)
      }
    }
  }
  return keystrokes
}

/** Calls `listener` when commands or their keybindings change. Returns a function that stops. */
export function onKeybindingsChanged(listener: () => void): () => void {
  const stopRegistry = registry.subscribe(listener)
  const stopOverrides = overrides.subscribe(listener)
  return () => {
    stopRegistry()
    stopOverrides()
  }
}

/** The commands keys can run where focus is, in order of precedence. */
function keyCommands(editorFocused: boolean, terminalFocused: boolean): Command[] {
  const user = overrides.get()
  const rank = (command: Command) =>
    command.id in user ? 0 : command.source === 'extension' ? 2 : 1
  return [...registry.get().values()]
    .filter(
      command =>
        !(command.editorFocus && !editorFocused) && !(terminalFocused && !command.terminal),
    )
    .sort((a, b) => rank(a) - rank(b))
}

/** The first enabled command with a keybinding `matches` accepts. */
function firstBound(
  editorFocused: boolean,
  terminalFocused: boolean,
  mac: boolean,
  matches: (steps: string[]) => boolean,
): Command | undefined {
  return keyCommands(editorFocused, terminalFocused).find(
    command =>
      bindingsOf(command, mac).some(binding => matches(stepsOf(binding))) &&
      (!command.enabled || command.enabled()) &&
      (!command.keysWhen || command.keysWhen()),
  )
}

/** The enabled command bound to `event` (a single key combination), if any. */
export function commandForKey(
  event: KeyInput,
  mac: boolean,
  editorFocused: boolean,
  terminalFocused = false,
): Command | undefined {
  return firstBound(
    editorFocused,
    terminalFocused,
    mac,
    steps => steps.length === 1 && matchesStep(steps[0]!, event, mac),
  )
}

/** The first key combination of an enabled command's chord that `event` starts, if any. */
export function chordStart(
  event: KeyInput,
  mac: boolean,
  editorFocused: boolean,
  terminalFocused = false,
): string | undefined {
  const command = firstBound(
    editorFocused,
    terminalFocused,
    mac,
    steps => steps.length === 2 && matchesStep(steps[0]!, event, mac),
  )
  return command
    ? bindingsOf(command, mac)
        .map(stepsOf)
        .find(steps => steps.length === 2 && matchesStep(steps[0]!, event, mac))?.[0]
    : undefined
}

/** The enabled command whose chord is `first` (from `chordStart`) then `event`. */
export function commandForChord(
  first: string,
  event: KeyInput,
  mac: boolean,
  editorFocused: boolean,
  terminalFocused = false,
): Command | undefined {
  return firstBound(
    editorFocused,
    terminalFocused,
    mac,
    steps =>
      steps.length === 2 && sameStep(steps[0]!, first, mac) && matchesStep(steps[1]!, event, mac),
  )
}

/** Keys that only modify another key; pressing one doesn't finish a chord. */
const MODIFIER_KEYS = new Set(['Control', 'Shift', 'Alt', 'AltGraph', 'Meta', 'CapsLock', 'OS'])

/** Runs bound commands for key presses anywhere in the window. */
export function useKeybindings(): void {
  useEffect(() => {
    const mac = isMac()
    /** The first step of a chord being typed, and where focus was then. */
    let pending: { first: string; editorFocused: boolean; terminalFocused: boolean } | null = null
    const endChord = () => {
      pending = null
      pendingChord.set(null)
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.isComposing) {
        return
      }
      if (pending) {
        if (MODIFIER_KEYS.has(event.key)) {
          return
        }
        // The second key finishes the chord, or cancels it; either way it's used up.
        const { first, editorFocused, terminalFocused } = pending
        endChord()
        event.preventDefault()
        event.stopPropagation()
        const command = commandForChord(first, event, mac, editorFocused, terminalFocused)
        if (command) {
          void executeCommand(command.id)
        }
        return
      }
      const target = event.target as HTMLElement | null
      const editorFocused = target?.dataset.editorInput !== undefined
      const terminalFocused =
        target instanceof Element && target.closest('[data-terminal]') !== null
      const command = commandForKey(event, mac, editorFocused, terminalFocused)
      if (command) {
        event.preventDefault()
        event.stopPropagation()
        void executeCommand(command.id)
        return
      }
      const first = chordStart(event, mac, editorFocused, terminalFocused)
      if (first) {
        event.preventDefault()
        event.stopPropagation()
        pending = { first, editorFocused, terminalFocused }
        pendingChord.set(formatKeybinding(first, mac))
      }
    }
    window.addEventListener('keydown', onKeyDown, true)
    window.addEventListener('blur', endChord)
    return () => {
      window.removeEventListener('keydown', onKeyDown, true)
      window.removeEventListener('blur', endChord)
      endChord()
    }
  }, [])
}
