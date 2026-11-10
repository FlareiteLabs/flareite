import { CircleAlert, Settings } from 'lucide-react'
import { useEffect, type KeyboardEvent, type MouseEvent } from 'react'
import { executeCommand } from '../commands/registry'
import { openContextMenu } from '../components/contextMenuState'
import type { CatalogEntry } from '../ipc/extensionInstall'
import type { ExtensionStatus } from '../ipc/extensions'
import {
  catalogStore,
  installBusy,
  installRelease,
  loadCatalog,
  sourcesStore,
} from '../state/extensionInstall'
import { extensionsStore, openExtension } from '../state/extensions'
import { useStore } from '../state/store'
import {
  ExtensionIcon,
  buildLabel,
  duration,
  manageEntries,
  stateLabels,
  troubled,
} from './extensionInfo'
import './ExtensionsPanel.css'

/**
 * Installed and development extensions, one row each: icon, name,
 * description, author, and how long it took to start. Choosing one opens
 * its page (README, permissions, and the rest). Below them, the official
 * extensions not installed yet, to install from their GitHub releases.
 */
export function ExtensionsPanel() {
  const extensions = useStore(extensionsStore)
  useEffect(() => {
    void loadCatalog()
  }, [])
  return (
    <div className="extensions-panel">
      {extensions.length === 0 ? (
        <div className="extensions-panel__empty">
          <p>No extensions are installed.</p>
          <p>
            Install one below or from a GitHub repository, put extension folders in the{' '}
            <code>extensions</code> folder next to your settings file, or load one you are
            developing.
          </p>
          <p className="extensions-panel__buttons">
            <button
              type="button"
              className="extensions-panel__button"
              onClick={() => void executeCommand('extensions.installFromGitHub')}
            >
              Install from GitHub…
            </button>
            <button
              type="button"
              className="extensions-panel__button"
              onClick={() => void executeCommand('extensions.addDevelopment')}
            >
              Load Development Extension…
            </button>
          </p>
        </div>
      ) : (
        <ul className="extensions-panel__list" aria-label="Extensions" onKeyDown={onListKeys}>
          {extensions.map(extension => (
            <ExtensionRow key={extension.id} extension={extension} />
          ))}
        </ul>
      )}
      <Catalog installed={extensions} />
    </div>
  )
}

/** The official extensions that aren't installed, each with an Install button. */
function Catalog({ installed }: { installed: readonly ExtensionStatus[] }) {
  const catalog = useStore(catalogStore)
  const busy = useStore(installBusy)
  if (catalog.state === 'idle') {
    return null
  }
  const ids = new Set(installed.map(extension => extension.id))
  const available =
    catalog.state === 'loaded' ? catalog.entries.filter(entry => !ids.has(entry.id)) : []
  if (catalog.state === 'loaded' && available.length === 0) {
    return null
  }
  return (
    <section className="extensions-catalog" aria-labelledby="extensions-catalog-heading">
      <h2 id="extensions-catalog-heading" className="extensions-catalog__heading">
        Official Extensions
      </h2>
      {catalog.state === 'loading' && <p className="extensions-panel__empty">Loading…</p>}
      {catalog.state === 'failed' && (
        <div className="extensions-panel__empty">
          <p role="alert">The official extensions couldn't be listed. {catalog.message}</p>
          <button
            type="button"
            className="extensions-panel__button"
            onClick={() => void loadCatalog(true)}
          >
            Try Again
          </button>
        </div>
      )}
      {available.length > 0 && (
        <ul className="extensions-panel__list" aria-label="Official extensions to install">
          {available.map(entry => (
            <CatalogRow key={`${entry.repo} ${entry.id}`} entry={entry} busy={busy} />
          ))}
        </ul>
      )}
    </section>
  )
}

function CatalogRow({ entry, busy }: { entry: CatalogEntry; busy: boolean }) {
  const title = [
    `${entry.name} ${entry.version}`,
    entry.author ? `${entry.id} · by ${entry.author}` : entry.id,
    entry.description,
    `From ${entry.repo} (release ${entry.tag})`,
  ]
    .filter(Boolean)
    .join('\n')
  return (
    <li className="catalog-row" title={title}>
      <span className="extension-row__text">
        <span className="extension-row__line">
          <span className="extension-row__name">{entry.name}</span>
          <span className="extension-row__time">{entry.version}</span>
        </span>
        <span className="extension-row__description">{entry.description || ' '}</span>
        <span className="extension-row__line">
          <span className="extension-row__author">{entry.author ?? entry.id}</span>
        </span>
      </span>
      <button
        type="button"
        className="extensions-panel__button"
        aria-label={`Install ${entry.name}`}
        disabled={busy}
        onClick={() => void installRelease(entry.repo, entry.tag)}
      >
        Install
      </button>
    </li>
  )
}

/** Moves between rows with the arrow keys, Home, and End. */
function onListKeys(event: KeyboardEvent<HTMLElement>): void {
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
    return
  }
  const rows = [...event.currentTarget.querySelectorAll<HTMLElement>('.extension-row__open')]
  const index = rows.indexOf(event.currentTarget.ownerDocument.activeElement as HTMLElement)
  if (index === -1) {
    return
  }
  event.preventDefault()
  const next =
    event.key === 'Home'
      ? 0
      : event.key === 'End'
        ? rows.length - 1
        : Math.min(Math.max(index + (event.key === 'ArrowDown' ? 1 : -1), 0), rows.length - 1)
  rows[next]?.focus()
}

/** Everything the row shortens, for its tooltip. */
export function rowTooltip(extension: ExtensionStatus): string {
  const title = [extension.name, extension.version, extension.development ? '(development)' : '']
    .filter(Boolean)
    .join(' ')
  const by = extension.author ? `${extension.id} · by ${extension.author}` : extension.id
  const started =
    extension.usage.activationMs === null
      ? ''
      : ` · started in ${duration(extension.usage.activationMs)}`
  const build = extension.build
  return [
    title,
    by,
    extension.description,
    `${stateLabels[extension.state]}${started}`,
    extension.error ?? '',
    build?.state === 'failed'
      ? `Build failed: ${build.error ?? ''}`
      : (buildLabel(extension) ?? ''),
  ]
    .filter(Boolean)
    .join('\n')
}

function ExtensionRow({ extension }: { extension: ExtensionStatus }) {
  const { activationMs } = extension.usage
  const update = useStore(sourcesStore)[extension.id]?.update ?? null
  const showMenu = (x: number, y: number) => openContextMenu(x, y, manageEntries(extension))
  const onContextMenu = (event: MouseEvent) => {
    event.preventDefault()
    showMenu(event.clientX, event.clientY)
  }
  const shownState =
    extension.state === 'active' || extension.state === 'ready'
      ? null
      : stateLabels[extension.state]
  const stateText = [buildLabel(extension), shownState].filter(Boolean).join(' · ') || null
  const buildFailed = extension.build?.state === 'failed'
  return (
    <li
      className="extension-row"
      data-state={buildFailed && !troubled(extension.state) ? 'failed' : extension.state}
      onContextMenu={onContextMenu}
    >
      <button
        type="button"
        className="extension-row__open"
        title={rowTooltip(extension)}
        aria-label={`${extension.name}${stateText ? `, ${stateText}` : ''}`}
        aria-description={extension.description || undefined}
        onClick={() => openExtension(extension.id)}
      >
        <ExtensionIcon extension={extension} size={36} />
        <span className="extension-row__text">
          <span className="extension-row__line">
            <span className="extension-row__name">{extension.name}</span>
            {extension.development && <span className="extension-row__badge">dev</span>}
            {update && !extension.development && (
              <span className="extension-row__badge" title={`${update} is available`}>
                update
              </span>
            )}
            {activationMs !== null && (
              <span className="extension-row__time" aria-hidden="true">
                {duration(activationMs)}
              </span>
            )}
          </span>
          <span className="extension-row__description">
            {extension.description || extension.error || ' '}
          </span>
          <span className="extension-row__line">
            <span className="extension-row__author">{extension.author ?? extension.id}</span>
            {stateText && (
              <span className="extension-row__state">
                {(troubled(extension.state) || buildFailed) && (
                  <CircleAlert size={12} strokeWidth={1.75} aria-hidden="true" />
                )}
                {stateText}
              </span>
            )}
          </span>
        </span>
      </button>
      <button
        type="button"
        className="extension-row__manage"
        title="Manage"
        aria-label={`Manage ${extension.name}`}
        aria-haspopup="menu"
        onClick={event => {
          const rect = event.currentTarget.getBoundingClientRect()
          showMenu(rect.left, rect.bottom)
        }}
      >
        <Settings size={14} strokeWidth={1.5} aria-hidden="true" />
      </button>
    </li>
  )
}

/** Buttons in the side bar header. */
export function ExtensionsActions() {
  return (
    <>
      <button
        type="button"
        className="dock-action"
        title="Install Extension from GitHub…"
        aria-label="Install Extension from GitHub"
        onClick={() => void executeCommand('extensions.installFromGitHub')}
      >
        ⤓
      </button>
      <button
        type="button"
        className="dock-action"
        title="Load Development Extension…"
        aria-label="Load Development Extension"
        onClick={() => void executeCommand('extensions.addDevelopment')}
      >
        ⊕
      </button>
      <button
        type="button"
        className="dock-action"
        title="Reload Extensions"
        aria-label="Reload Extensions"
        onClick={() => void executeCommand('extensions.reload')}
      >
        ↻
      </button>
    </>
  )
}
