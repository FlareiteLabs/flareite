/**
 * Tells the backend which commands there are, with their labels and
 * default keys, so the settings' schema can complete and describe command
 * ids under `[keybindings]` in `settings.toml`. Sent once the registry
 * settles, and again whenever commands are registered or removed
 * (extensions loading, say).
 */
import { isDesktop } from '../ipc'
import { setSchemaCommands, type SchemaCommand } from '../ipc/workspace'
import { isMac } from '../editor/keymap'
import { commandLabel, listCommands, subscribeCommands, type Command } from './registry'

/** How long the registry must stay unchanged before it's sent. */
const SETTLE_MS = 500

/** A command's default keys on this platform, as settings write them. */
export function defaultKeys(command: Command, mac: boolean): string[] {
  const bindings = command.keybinding === undefined ? [] : [command.keybinding].flat()
  return bindings.map(binding =>
    typeof binding === 'string' ? binding : mac ? binding.mac : binding.default,
  )
}

/** Every command, as the backend wants them, in id order. */
export function schemaCommands(commands: readonly Command[], mac: boolean): SchemaCommand[] {
  return commands
    .map(command => ({
      id: command.id,
      label: commandLabel(command),
      keys: defaultKeys(command, mac),
    }))
    .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
}

/** Keeps the backend's list of commands up to date; returns a function that stops. */
export function startCommandSync(): () => void {
  if (!isDesktop()) {
    return () => {}
  }
  let timer: ReturnType<typeof setTimeout> | undefined
  let sent = ''
  const send = () => {
    const commands = schemaCommands(listCommands(), isMac())
    const text = JSON.stringify(commands)
    if (text === sent) {
      return
    }
    sent = text
    setSchemaCommands(commands).catch((error: unknown) => {
      sent = ''
      console.debug('could not send commands for the settings schema', error)
    })
  }
  const schedule = () => {
    clearTimeout(timer)
    timer = setTimeout(send, SETTLE_MS)
  }
  schedule()
  const unsubscribe = subscribeCommands(schedule)
  return () => {
    unsubscribe()
    clearTimeout(timer)
  }
}
