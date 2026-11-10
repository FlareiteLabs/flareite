/**
 * An extension's page, opened from the Extensions view: who made it, its
 * state and actions, its README (drawn like a preview, so nothing in it
 * becomes markup), and its permissions, commands, contributions, resource
 * use, and folder.
 */
import { CircleAlert, Ellipsis } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { formatBytes, formatUsage } from '../app/format'
import { executeCommand } from '../commands/registry'
import { openContextMenu } from '../components/contextMenuState'
import {
  clearExtensionStorage,
  getExtensionReadme,
  getExtensionReadmeImage,
  setExtensionPermissions,
  type ExtensionStatus,
  type Permission,
  type PreviewContent,
} from '../ipc/extensions'
import { installBusy, sourcesStore, update } from '../state/extensionInstall'
import { extensionsStore, useExtensionUsage } from '../state/extensions'
import { reportError } from '../state/notifications'
import { extensionChannel, showOutput } from '../state/output'
import { extensionQuery, openSettings } from '../state/settings'
import { useStore } from '../state/store'
import {
  ExtensionIcon,
  build,
  canRestart,
  contributionList,
  duration,
  manageEntries,
  restart,
  stateLabels,
  toggleEnabled,
  troubled,
} from './extensionInfo'
import { PreviewDocument } from './PreviewPane'
import './ExtensionPage.css'
import './PreviewPane.css'

type Readme =
  | { state: 'loading' }
  | { state: 'none' }
  | { state: 'failed'; message: string }
  | { state: 'loaded'; content: PreviewContent }

/** The README of an extension, read again when it's reloaded or updated. */
function useReadme(extension: ExtensionStatus | undefined): Readme {
  const [readme, setReadme] = useState<Readme>({ state: 'loading' })
  const key = extension && `${extension.id}\n${extension.path}\n${extension.version}`
  const has = extension?.hasReadme ?? false
  const id = extension?.id
  useEffect(() => {
    if (!id || !has) {
      setReadme({ state: 'none' })
      return
    }
    let cancelled = false
    setReadme({ state: 'loading' })
    getExtensionReadme(id).then(
      content =>
        !cancelled && setReadme(content ? { state: 'loaded', content } : { state: 'none' }),
      (error: unknown) =>
        !cancelled &&
        setReadme({
          state: 'failed',
          message: error instanceof Error ? error.message : String(error),
        }),
    )
    return () => {
      cancelled = true
    }
  }, [id, has, key])
  return readme
}

/** A development extension's build: how it went, building again, and its output. */
function BuildSection({ extension }: { extension: ExtensionStatus }) {
  const status = extension.build
  if (!status) {
    return null
  }
  const took = status.durationMs === null ? '' : ` in ${duration(status.durationMs)}`
  const summary =
    status.state === 'building'
      ? 'Building…'
      : status.state === 'failed'
        ? `Build failed${took}: ${status.error ?? 'see its log'}`
        : `Built${took}`
  return (
    <div className="extension-page__build">
      <p
        className="extension-page__state"
        data-state={status.state === 'failed' ? 'failed' : undefined}
        role={status.state === 'failed' ? 'alert' : 'status'}
      >
        {status.state === 'failed' && (
          <CircleAlert size={14} strokeWidth={1.75} aria-hidden="true" />
        )}
        {summary}
        <span className="extension-page__muted">
          {' '}
          · rebuilt and reloaded when a file in its folder changes
        </span>
      </p>
      <div className="extension-page__actions">
        <button type="button" className="extension-page__button" onClick={() => build(extension)}>
          Build
        </button>
        <button
          type="button"
          className="extension-page__button"
          onClick={() => showOutput(extensionChannel(status.log))}
        >
          Show Build Log
        </button>
      </div>
    </div>
  )
}

export function ExtensionPage({ id }: { id: string }) {
  const extension = useStore(extensionsStore).find(candidate => candidate.id === id)
  const usage = useExtensionUsage().get(id) ?? extension?.usage
  const readme = useReadme(extension)
  const source = useStore(sourcesStore)[id]
  const busy = useStore(installBusy)
  const loadImage = useCallback((path: string) => getExtensionReadmeImage(id, path), [id])
  if (!extension || !usage) {
    return <p className="panel-empty">This extension is no longer loaded.</p>
  }
  const invalid = extension.state === 'invalid'
  const disabled = extension.state === 'disabled'
  const commands = extension.commands.filter(command => !command.hidden)
  const contributions = contributionList(extension)
  const usageText = formatUsage(usage)

  return (
    <article className="extension-page" aria-label={extension.name}>
      <header className="extension-page__header">
        <ExtensionIcon extension={extension} size={88} />
        <div className="extension-page__summary">
          <h1 className="extension-page__name">
            {extension.name}
            {extension.version && (
              <span className="extension-page__version">{extension.version}</span>
            )}
            {extension.development && <span className="extension-page__badge">development</span>}
          </h1>
          <p className="extension-page__meta">
            {extension.author && (
              <>
                <span className="extension-page__author">{extension.author}</span>
                <span aria-hidden="true"> · </span>
              </>
            )}
            <span className="extension-page__id">{extension.id}</span>
          </p>
          {source && !extension.development && (
            <p className="extension-page__meta">
              Installed from {source.repository} (release {source.tag})
            </p>
          )}
          {extension.description && (
            <p className="extension-page__description">{extension.description}</p>
          )}
          <p className="extension-page__state" data-state={extension.state}>
            {troubled(extension.state) && (
              <CircleAlert size={14} strokeWidth={1.75} aria-hidden="true" />
            )}
            {stateLabels[extension.state]}
            {usage.activationMs !== null && !disabled && (
              <span className="extension-page__muted">
                {' '}
                · started in {duration(usage.activationMs)}
              </span>
            )}
          </p>
          {!invalid && (
            <div className="extension-page__actions">
              {source?.update && !extension.development && (
                <button
                  type="button"
                  className="extension-page__button extension-page__button--primary"
                  disabled={busy}
                  onClick={() => void update(source)}
                >
                  Update to {source.update}
                </button>
              )}
              <button
                type="button"
                className={`extension-page__button${source?.update && !extension.development ? '' : ' extension-page__button--primary'}`}
                onClick={() => toggleEnabled(extension)}
              >
                {disabled ? 'Enable' : 'Disable'}
              </button>
              {canRestart(extension) && (
                <button
                  type="button"
                  className="extension-page__button"
                  onClick={() => restart(extension)}
                >
                  Restart
                </button>
              )}
              {extension.hasSettings && (
                <button
                  type="button"
                  className="extension-page__button"
                  onClick={() => openSettings(extensionQuery(extension.id))}
                >
                  Settings
                </button>
              )}
              <button
                type="button"
                className="extension-page__button"
                onClick={() => showOutput(extensionChannel(extension.id))}
              >
                Show Log
              </button>
              <button
                type="button"
                className="extension-page__button extension-page__button--icon"
                title="More Actions"
                aria-label="More Actions"
                aria-haspopup="menu"
                onClick={event => {
                  const rect = event.currentTarget.getBoundingClientRect()
                  openContextMenu(rect.left, rect.bottom, manageEntries(extension, true))
                }}
              >
                <Ellipsis size={16} strokeWidth={1.5} aria-hidden="true" />
              </button>
            </div>
          )}
          {extension.build && <BuildSection extension={extension} />}
          {extension.error && (
            <p className="extension-page__error" role="alert">
              {extension.error}
            </p>
          )}
          {extension.configErrors.map(error => (
            <p key={error} className="extension-page__warning">
              Setting {error}
            </p>
          ))}
        </div>
      </header>

      <div className="extension-page__body">
        <section className="extension-page__readme preview" aria-label="README" tabIndex={0}>
          {readme.state === 'loaded' && (
            <PreviewDocument content={readme.content} loadImage={loadImage} />
          )}
          {readme.state === 'none' && (
            <p className="extension-page__muted">This extension has no README.</p>
          )}
          {readme.state === 'failed' && (
            <p className="extension-page__muted">The README couldn't be shown: {readme.message}</p>
          )}
        </section>

        <aside className="extension-page__details" aria-label="Details">
          {extension.permissions.length > 0 && <Permissions extension={extension} />}
          {commands.length > 0 && (
            <section className="extension-page__section">
              <h2>Commands</h2>
              <ul className="extension-page__commands">
                {commands.map(command => (
                  <li key={command.id}>
                    <button
                      type="button"
                      className="extension-page__link"
                      disabled={disabled}
                      title={`Run ${command.category}: ${command.title}`}
                      onClick={() => void executeCommand(command.id)}
                    >
                      {command.title}
                    </button>
                  </li>
                ))}
              </ul>
            </section>
          )}
          {contributions.length > 0 && (
            <section className="extension-page__section">
              <h2>Contributes</h2>
              <ul className="extension-page__list">
                {contributions.map(item => (
                  <li key={item}>{item}</li>
                ))}
              </ul>
            </section>
          )}
          {!invalid && (
            <section className="extension-page__section">
              <h2>Resource use</h2>
              <p>{usageText ?? 'It hasn’t run this session.'}</p>
              {usage.compileMs > 0 && (
                <p className="extension-page__muted">Compiled in {duration(usage.compileMs)}</p>
              )}
            </section>
          )}
          {extension.storedBytes > 0 && (
            <section className="extension-page__section">
              <h2>Stored data</h2>
              <p>{formatBytes(extension.storedBytes)}</p>
              {extension.secrets > 0 && (
                <p className="extension-page__muted">
                  {extension.secrets === 1 ? '1 secret' : `${extension.secrets} secrets`} in the
                  system’s credential store, removed with it
                </p>
              )}
              <button
                type="button"
                className="extension-page__button"
                onClick={() =>
                  clearExtensionStorage(extension.id).catch((error: unknown) =>
                    reportError('Clearing stored data', error),
                  )
                }
              >
                Clear Stored Data
              </button>
            </section>
          )}
          <section className="extension-page__section">
            <h2>Folder</h2>
            <p className="extension-page__path">{extension.path}</p>
          </section>
        </aside>
      </div>
    </article>
  )
}

function Permissions({ extension }: { extension: ExtensionStatus }) {
  const granted = extension.permissions.filter(p => p.granted).map(p => p.permission)
  const toggle = (permission: Permission, allow: boolean) => {
    const next = allow ? [...granted, permission] : granted.filter(p => p !== permission)
    setExtensionPermissions(extension.id, next).catch((error: unknown) =>
      reportError('Changing permissions', error),
    )
  }
  return (
    <fieldset className="extension-page__section extension-page__permissions">
      <legend>
        <h2>Permissions</h2>
      </legend>
      {extension.permissions.map(permission => (
        <label key={permission.permission} className="extension-page__permission">
          <input
            type="checkbox"
            checked={permission.granted}
            onChange={event => toggle(permission.permission, event.currentTarget.checked)}
          />
          <span>
            {permission.description}
            {!permission.decided && <em> (not decided)</em>}
          </span>
        </label>
      ))}
    </fieldset>
  )
}
