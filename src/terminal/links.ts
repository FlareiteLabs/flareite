/**
 * Links in the terminal: web addresses and file paths in the output,
 * underlined on hover and opened with Ctrl+click (Cmd+click on macOS), or
 * Ctrl+Shift+click to choose who opens them (see `state/links.ts`).
 *
 * Links are found as everywhere else (`state/linkFinder.ts`). Relative
 * paths are found from the shell's folder: as the shell reports it (OSC 7),
 * else as the nearest PowerShell prompt above says (`PS C:\code>`), else
 * the open folder.
 */
import type { IBufferLine, ILink, ILinkProvider, Terminal } from '@xterm/xterm'
import { fileUrlPath, findLinks } from '../state/linkFinder'
import { LINK_TOOLTIP, openLink, type Link } from '../state/links'
import { workspaceStore } from '../state/workspace'

/** The folder a PowerShell prompt names (`PS C:\code> `), if `line` is one. */
export function promptFolder(line: string): string | null {
  const match = /^PS ([A-Za-z]:\\[^>]*|\\\\[^>]+|\/[^>]*)> /.exec(line)
  return match?.[1] ?? null
}

/** The text of buffer line `y` (from 0), `''` past the end. */
function lineText(terminal: Terminal, y: number): string {
  const line: IBufferLine | undefined = terminal.buffer.active.getLine(y)
  return line?.translateToString(true) ?? ''
}

/** The folder relative paths on line `y` are probably in: the nearest prompt above it. */
function promptAbove(terminal: Terminal, y: number): string | null {
  for (let row = y; row >= Math.max(0, y - 500); row--) {
    const folder = promptFolder(lineText(terminal, row))
    if (folder) {
      return folder
    }
  }
  return null
}

/**
 * The terminal's links. `cwd` is the shell's folder, when it has said
 * (OSC 7).
 */
export function linkProvider(terminal: Terminal, cwd: () => string | null): ILinkProvider {
  const activate = (link: Link) => (event: MouseEvent) => {
    if (!(event.ctrlKey || event.metaKey)) {
      return
    }
    openLink(link, event.shiftKey)
  }
  // Shift+mousedown extends the selection in xterm.js, so a link never sees
  // Ctrl+Shift+click: it's caught first, for the link under the pointer.
  let hovered: Link | null = null
  let listening: HTMLElement | null = null
  const onMouseDown = (event: MouseEvent) => {
    if (hovered && event.button === 0 && event.shiftKey && (event.ctrlKey || event.metaKey)) {
      event.preventDefault()
      event.stopPropagation()
      openLink(hovered, true)
    }
  }
  const decorate = (text: string, range: ILink['range'], link: Link): ILink => ({
    text,
    range,
    decorations: { underline: true, pointerCursor: true },
    activate: activate(link),
    hover: () => {
      hovered = link
      const element = terminal.element
      if (element && listening !== element) {
        listening?.removeEventListener('mousedown', onMouseDown, true)
        element.addEventListener('mousedown', onMouseDown, true)
        listening = element
      }
      element?.setAttribute('title', LINK_TOOLTIP)
    },
    leave: () => {
      hovered = null
      terminal.element?.removeAttribute('title')
    },
  })
  return {
    provideLinks(row, callback) {
      // `row` counts from 1.
      const y = row - 1
      const text = lineText(terminal, y)
      const root = workspaceStore.get().info?.root ?? null
      const folders = [cwd(), promptAbove(terminal, y), root].filter(
        (folder): folder is string => folder !== null,
      )
      void findLinks(text, folders).then(found => {
        callback(
          found.length === 0
            ? undefined
            : found.map(({ start, end, link }) =>
                decorate(
                  text.slice(start, end),
                  { start: { x: start + 1, y: row }, end: { x: end, y: row } },
                  link,
                ),
              ),
        )
      })
    },
  }
}

/** The folder an OSC 7 report names (`file://host/path`), if it's valid. */
export function osc7Folder(data: string): string | null {
  return fileUrlPath(data)
}
