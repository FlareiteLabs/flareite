/**
 * What the Extensions view and an extension's page both show: its state,
 * what it adds, its icon, and the actions on it.
 */
import { Blocks } from 'lucide-react'
import type { MenuEntry } from '../components/contextMenuState'
import {
  buildExtension,
  restartExtension,
  setExtensionEnabled,
  type ExtensionState,
  type ExtensionStatus,
} from '../ipc/extensions'
import { writeClipboardText } from '../ipc/workspace'
import {
  chooseRelease,
  installBusy,
  sourcesStore,
  uninstall,
  update,
} from '../state/extensionInstall'
import { openExtension, useExtensionIcon } from '../state/extensions'
import { reportError } from '../state/notifications'
import { extensionChannel, showOutput } from '../state/output'
import { extensionQuery, openSettings } from '../state/settings'

export const stateLabels: Record<ExtensionState, string> = {
  inactive: 'Inactive',
  awaitingPermission: 'Needs permission',
  starting: 'Starting',
  active: 'Active',
  failed: 'Stopped',
  disabled: 'Disabled',
  invalid: 'Invalid',
  ready: 'Enabled',
}

/** States that need the user's attention. */
export const troubled = (state: ExtensionState) =>
  state === 'failed' || state === 'invalid' || state === 'awaitingPermission'

/** What an extension adds to the workbench, e.g. ["Rust", "2 themes", "1 view"]. */
export function contributionList(extension: ExtensionStatus): string[] {
  const { themes, iconThemes, keybindings, menus, views, decorations, previews, languages } =
    extension.contributions
  const parts = [
    [themes.length, 'color theme'],
    [iconThemes.length, 'file icon theme'],
    [views.length, 'view'],
    [previews.length, 'preview'],
    [keybindings.length, 'keybinding'],
    [menus.length, 'menu item'],
    [decorations.length, 'decoration type'],
    [extension.contributions.viewers.length, 'viewer'],
    [extension.contributions.layouts.length, 'layout'],
    [extension.contributions.linkHandlers.length, 'link handler'],
    [extension.contributions.languageServers.length, 'language server'],
    [extension.contributions.colors.length, 'color'],
  ] as const
  const { programs } = extension.contributions
  return [
    // Named, as a list of languages says more than a count.
    ...(languages.length > 0
      ? [
          `${languages.length === 1 ? 'Language' : 'Languages'}: ${languages.map(l => l.name).join(', ')}`,
        ]
      : []),
    // Named too: they run on the user's computer.
    ...(programs.length > 0
      ? [
          `${programs.length === 1 ? 'Program' : 'Programs'}: ${programs.map(p => p.command).join(', ')}`,
        ]
      : []),
    ...parts
      .filter(([count]) => count > 0)
      .map(([count, noun]) => `${count} ${noun}${count === 1 ? '' : 's'}`),
  ]
}

/** A duration in milliseconds, e.g. "12 ms" or "1.4 s". */
export function duration(ms: number): string {
  if (ms >= 1000) {
    return `${(ms / 1000).toFixed(1)} s`
  }
  return `${ms < 10 ? ms.toFixed(1) : Math.round(ms)} ms`
}

/** The extension's icon, or a plain one if it has none. */
export function ExtensionIcon({ extension, size }: { extension: ExtensionStatus; size: number }) {
  const icon = useExtensionIcon(extension)
  return icon ? (
    <img
      className="extension-icon"
      src={icon}
      alt=""
      width={size}
      height={size}
      draggable={false}
    />
  ) : (
    <span
      className="extension-icon extension-icon--none"
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <Blocks size={Math.round(size * 0.55)} strokeWidth={1.25} />
    </span>
  )
}

function act(action: string, run: () => Promise<void>): void {
  run().catch((error: unknown) => reportError(action, error))
}

/** Whether the extension has code that can be restarted. */
export const canRestart = (extension: ExtensionStatus) =>
  extension.state !== 'invalid' && extension.state !== 'ready' && extension.state !== 'disabled'

export function toggleEnabled(extension: ExtensionStatus): void {
  const disabled = extension.state === 'disabled'
  act(disabled ? 'Enabling the extension' : 'Disabling the extension', () =>
    setExtensionEnabled(extension.id, disabled),
  )
}

export function restart(extension: ExtensionStatus): void {
  act('Restarting the extension', () => restartExtension(extension.id))
}

/** Builds a development extension that has a `[build]`, and loads it after. */
export function build(extension: ExtensionStatus): void {
  act('Building the extension', () => buildExtension(extension.path))
}

/** The build's state in a few words, or `null` when there's nothing to say. */
export function buildLabel(extension: ExtensionStatus): string | null {
  switch (extension.build?.state) {
    case 'building':
      return 'Building…'
    case 'failed':
      return 'Build failed'
    default:
      return null
  }
}

/** Builds it, and shows the build's output: for development extensions with a `[build]`. */
function buildEntries(extension: ExtensionStatus): MenuEntry[] {
  const status = extension.build
  if (!status) {
    return []
  }
  return [
    { label: 'Build', run: () => build(extension) },
    { label: 'Show Build Log', run: () => showOutput(extensionChannel(status.log)) },
  ]
}

/** Updating, choosing another release, and uninstalling: for installed extensions. */
function installEntries(extension: ExtensionStatus): MenuEntry[] {
  if (extension.development) {
    return []
  }
  const source = sourcesStore.get()[extension.id]
  const busy = installBusy.get()
  return [
    null,
    ...(source?.update
      ? [{ label: `Update to ${source.update}`, run: () => void update(source), disabled: busy }]
      : []),
    ...(source
      ? [
          {
            label: 'Install Another Version…',
            run: () => void chooseRelease(source.repository),
            disabled: busy,
          },
        ]
      : []),
    { label: 'Uninstall…', run: () => void uninstall(extension), disabled: busy },
  ]
}

/** The extension's actions, for its manage menu. */
export function manageEntries(extension: ExtensionStatus, onPage = false): MenuEntry[] {
  if (extension.state === 'invalid') {
    return [
      ...(onPage ? [] : [{ label: 'Show Details', run: () => openExtension(extension.id) }]),
      ...buildEntries(extension),
      { label: 'Copy Folder Path', run: () => copy(extension.path) },
      ...installEntries(extension),
    ]
  }
  const disabled = extension.state === 'disabled'
  return [
    ...(onPage ? [] : [{ label: 'Show Details', run: () => openExtension(extension.id) }, null]),
    { label: disabled ? 'Enable' : 'Disable', run: () => toggleEnabled(extension) },
    ...(canRestart(extension) ? [{ label: 'Restart', run: () => restart(extension) }] : []),
    ...(extension.build ? [{ label: 'Build', run: () => build(extension) }] : []),
    null,
    ...(extension.hasSettings
      ? [{ label: 'Settings', run: () => openSettings(extensionQuery(extension.id)) }]
      : []),
    { label: 'Show Log', run: () => showOutput(extensionChannel(extension.id)) },
    null,
    { label: 'Copy Extension ID', run: () => copy(extension.id) },
    ...installEntries(extension),
  ]
}

function copy(text: string): void {
  writeClipboardText(text).catch((error: unknown) => reportError('Copying', error))
}
