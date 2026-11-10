/**
 * Menus of terminal profiles (the shells found on this computer, and those
 * in settings): start a terminal with one, or make one the default.
 */
import { focusedDocument } from '../app/windows'
import { executeCommand } from '../commands/registry'
import { openContextMenu, type MenuEntry } from '../components/contextMenuState'
import { openTerminal } from '../dock/dock'
import { setDefaultProfile, terminalProfiles, type TerminalProfile } from '../ipc/terminal'
import { reportError } from '../state/notifications'

/** Where a menu opened from the command palette goes: near the top, centered. */
export function menuOrigin(): { x: number; y: number } {
  // In the window the user is in, where menus show.
  const view = focusedDocument().defaultView ?? window
  return { x: Math.max(0, Math.round(view.innerWidth / 2 - 120)), y: 72 }
}

async function profiles(): Promise<TerminalProfile[] | null> {
  try {
    return await terminalProfiles()
  } catch (error) {
    reportError('Finding shells', error)
    return null
  }
}

const label = (profile: TerminalProfile) =>
  profile.default ? `${profile.name} (default)` : profile.name

const none: MenuEntry = { label: 'No shells were found', disabled: true, run: () => {} }

/** Lists the profiles; choosing one starts a terminal with it. */
export async function openNewTerminalMenu(x: number, y: number): Promise<void> {
  const list = await profiles()
  if (!list) {
    return
  }
  const entries: MenuEntry[] = list.map(profile => ({
    label: label(profile),
    run: () => openTerminal(profile.name).catch(error => reportError('Starting a terminal', error)),
  }))
  openContextMenu(x, y, [
    ...(entries.length > 0 ? entries : [none]),
    null,
    { label: 'Select Default Profile…', run: () => openDefaultProfileMenu(x, y) },
    { label: 'Configure Profiles…', run: () => executeCommand('preferences.openSettingsFile') },
  ])
}

/** Lists the profiles; choosing one makes it the default (in settings). */
export async function openDefaultProfileMenu(x: number, y: number): Promise<void> {
  const list = await profiles()
  if (!list) {
    return
  }
  const entries: MenuEntry[] = list.map(profile => ({
    label: label(profile),
    run: () =>
      setDefaultProfile(profile.name).catch(error =>
        reportError('Setting the default profile', error),
      ),
  }))
  openContextMenu(x, y, entries.length > 0 ? entries : [none])
}
