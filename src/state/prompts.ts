/**
 * Prompts extensions show: lists to choose from and lines of text to type
 * (in the palette), and confirmations (a dialog). Each answer, or closing
 * without one, goes back to the extension, which runs a command for it.
 */
import { closePalette, openQuickPick, paletteStore } from '../components/paletteState'
import {
  answerPrompt,
  pickFileDialog,
  pickFolderDialog,
  type Prompt,
  type PromptReply,
} from '../ipc/extensions'
import { reportError } from './notifications'
import { createStore } from './store'
import { workspaceStore } from './workspace'

export type ConfirmPrompt = Extract<Prompt, { type: 'confirm' }>

/** The confirmation dialog showing, if any. */
export const confirmPrompt = createStore<ConfirmPrompt | null>(null)

/** The prompt showing in the palette, if any. */
let inPalette: number | null = null

function reply(id: number, answer: PromptReply): void {
  if (inPalette === id) {
    inPalette = null
  }
  if (confirmPrompt.get()?.id === id) {
    confirmPrompt.set(null)
  }
  answerPrompt(id, answer).catch((error: unknown) => reportError('Answering', error))
}

/** Shows a prompt, replacing any showing (the backend cancelled it). */
export function showPrompt(prompt: Prompt): void {
  confirmPrompt.set(null)
  const cancel = () => reply(prompt.id, { type: 'cancelled' })
  switch (prompt.type) {
    case 'pick':
      inPalette = prompt.id
      openQuickPick({
        placeholder: prompt.title,
        items: prompt.items.map(item => ({
          label: item.label,
          detail: item.description ?? undefined,
          run: () => reply(prompt.id, { type: 'picked', value: item.value }),
        })),
        onCancel: cancel,
      })
      break
    case 'input':
      inPalette = prompt.id
      openQuickPick({
        placeholder: prompt.placeholder ? `${prompt.title} (${prompt.placeholder})` : prompt.title,
        items: [],
        input: {
          value: prompt.value,
          accept: text => reply(prompt.id, { type: 'text', text }),
          password: prompt.password,
        },
        onCancel: cancel,
      })
      break
    case 'folder':
      pickFolderDialog(prompt.title)
        .then(path =>
          reply(prompt.id, path === null ? { type: 'cancelled' } : { type: 'text', text: path }),
        )
        .catch((error: unknown) => {
          reportError('Choosing a folder', error)
          cancel()
        })
      break
    case 'file':
      pickFileDialog(prompt.title, workspaceStore.get().info?.root ?? null)
        .then(path =>
          reply(prompt.id, path === null ? { type: 'cancelled' } : { type: 'text', text: path }),
        )
        .catch((error: unknown) => {
          reportError('Choosing a file', error)
          cancel()
        })
      break
    case 'confirm':
      if (inPalette !== null && paletteStore.get().open) {
        closePalette()
      }
      inPalette = null
      confirmPrompt.set(prompt)
      break
  }
}

/** The user chose a confirmation's button (by index), or closed it (`null`). */
export function answerConfirm(prompt: ConfirmPrompt, button: number | null): void {
  reply(prompt.id, button === null ? { type: 'cancelled' } : { type: 'button', index: button })
}

/** A prompt was withdrawn: its extension stopped. */
export function promptClosed(id: number): void {
  if (confirmPrompt.get()?.id === id) {
    confirmPrompt.set(null)
  }
  if (inPalette === id) {
    inPalette = null
    closePalette()
  }
}
