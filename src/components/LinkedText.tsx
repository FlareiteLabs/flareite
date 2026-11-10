/**
 * Text with its links: web addresses and paths that lead to a file or
 * folder, opened with Ctrl+click (Cmd+click on macOS) as in the terminal,
 * or Ctrl+Shift+click to choose who opens them. A plain click leaves the
 * text to be selected.
 *
 * Web addresses are links at once; paths once the backend says they lead
 * somewhere (checked together for everything shown at once).
 */
import { useEffect, useState, type MouseEvent } from 'react'
import { findCandidates, findLinks, findWebLinks, type FoundLink } from '../state/linkFinder'
import { followsLink, LINK_TOOLTIP, openLink } from '../state/links'
import { workspaceStore } from '../state/workspace'
import './LinkedText.css'

interface Props {
  text: string
  /**
   * The folders relative paths are looked for in, in turn; the open folder
   * when not given.
   */
  folders?: readonly string[]
}

const NO_FOLDERS: readonly string[] = []

export function LinkedText({ text, folders }: Props) {
  const [found, setFound] = useState<{ text: string; links: FoundLink[] } | null>(null)
  const folderKey = (folders ?? NO_FOLDERS).join('\n')

  useEffect(() => {
    if (!findCandidates(text).some(candidate => candidate.kind === 'path')) {
      return
    }
    let live = true
    const root = workspaceStore.get().info?.root
    const within = folders ?? (root ? [root] : [])
    void findLinks(text, within).then(links => {
      if (live) {
        setFound({ text, links })
      }
    })
    return () => {
      live = false
    }
    // `folderKey` stands for `folders`, which callers may build each render.
  }, [text, folderKey])

  const links = found?.text === text ? found.links : findWebLinks(text)
  if (links.length === 0) {
    return <>{text}</>
  }
  const parts: (string | FoundLink)[] = []
  let at = 0
  for (const link of links) {
    if (link.start > at) {
      parts.push(text.slice(at, link.start))
    }
    parts.push(link)
    at = link.end
  }
  if (at < text.length) {
    parts.push(text.slice(at))
  }
  return (
    <>
      {parts.map((part, index) =>
        typeof part === 'string' ? (
          part
        ) : (
          <span
            key={index}
            className="linked-text__link"
            title={`${part.link.target}\n${LINK_TOOLTIP}`}
            // A Shift+mousedown would extend the selection instead.
            onMouseDown={(event: MouseEvent) => {
              if (event.button === 0 && followsLink(event)) {
                event.preventDefault()
              }
            }}
            onClick={(event: MouseEvent) => {
              if (followsLink(event)) {
                event.preventDefault()
                event.stopPropagation()
                openLink(part.link, event.shiftKey)
              }
            }}
          >
            {text.slice(part.start, part.end)}
          </span>
        ),
      )}
    </>
  )
}
