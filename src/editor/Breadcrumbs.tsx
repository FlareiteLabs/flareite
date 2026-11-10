import { afterFrame, query } from '../app/windows'
import { ChevronRight } from 'lucide-react'
import { useLayoutEffect, useRef } from 'react'
import { FileIcon } from '../components/FileIcon'
import { entryIcon } from '../components/fileIcons'
import type { DocumentSummary } from '../ipc/editor'
import { EXPLORER_TREE_ID, revealInExplorer } from '../panels/explorerState'
import { iconThemeStore } from '../state/iconTheme'
import { showView } from '../state/viewRequests'
import { useStore } from '../state/store'
import { foldersBetween, pathSeparator, workspaceStore } from '../state/workspace'
import { themeKind } from '../themes/theme'
import './Breadcrumbs.css'

export interface Crumb {
  label: string
  /** The folder or file, if it can be revealed in the explorer. */
  path: string | null
}

/**
 * A document's breadcrumbs: its path within the workspace, else its full
 * path (which can't be revealed), else just its title.
 */
export function crumbsFor(path: string | null, title: string, root: string | null): Crumb[] {
  if (!path) {
    return [{ label: title, path: null }]
  }
  const separator = pathSeparator(path)
  const name = (of: string) => of.slice(of.lastIndexOf(separator) + 1)
  const folders = root ? foldersBetween(root, path) : null
  if (folders) {
    return [...folders, path].map(crumb => ({ label: name(crumb), path: crumb }))
  }
  return path
    .split(separator)
    .filter(part => part !== '')
    .map(label => ({ label, path: null }))
}

/**
 * The active document's path above the editor, as in VS Code. Choosing a
 * part reveals that folder or file in the explorer and moves focus there.
 */
export function Breadcrumbs({ document }: { document: DocumentSummary }) {
  const { info } = useStore(workspaceStore)
  const iconTheme = useStore(iconThemeStore)
  const light = useStore(themeKind) === 'light'
  const listRef = useRef<HTMLOListElement>(null)
  const crumbs = crumbsFor(document.path, document.title, info?.root ?? null)

  // Keep the file name in view when the path is wider than the editor.
  useLayoutEffect(() => {
    const list = listRef.current
    if (list) {
      list.scrollLeft = list.scrollWidth
    }
  }, [document.path, document.title])

  const reveal = (path: string) => {
    showView('explorer')
    // The explorer may be in another window.
    void revealInExplorer(path).then(() =>
      afterFrame(() => query(`#${CSS.escape(EXPLORER_TREE_ID)}`)?.focus()),
    )
  }

  return (
    <nav className="breadcrumbs" aria-label={`Breadcrumbs, ${document.title}`}>
      <ol className="breadcrumbs__list" ref={listRef}>
        {crumbs.map((crumb, index) => {
          const last = index === crumbs.length - 1
          const content = (
            <>
              {last && (
                <FileIcon
                  icon={entryIcon(crumb.label, null, iconTheme, light)}
                  kind="file"
                  className="breadcrumbs__icon"
                />
              )}
              <span className="breadcrumbs__label">{crumb.label}</span>
            </>
          )
          const { path } = crumb
          return (
            <li key={`${index}:${crumb.label}`} className="breadcrumbs__item">
              {index > 0 && (
                <ChevronRight className="breadcrumbs__separator" size={14} aria-hidden="true" />
              )}
              {path ? (
                <button
                  type="button"
                  className="breadcrumbs__crumb"
                  title={`${path}\nReveal in the Explorer`}
                  aria-current={last ? 'location' : undefined}
                  onClick={() => reveal(path)}
                >
                  {content}
                </button>
              ) : (
                <span className="breadcrumbs__crumb" aria-current={last ? 'location' : undefined}>
                  {content}
                </span>
              )}
            </li>
          )
        })}
      </ol>
    </nav>
  )
}
