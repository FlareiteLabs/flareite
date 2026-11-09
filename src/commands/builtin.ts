/** The workbench's built-in commands and their default keybindings. */
import { openPalette, openQuickPick } from '../components/paletteState'
import { togglePerfOverlay } from '../components/PerfOverlay'
import { resetPerf } from '../state/perf'
import { activeEditor, activeOverride } from '../editor/activeEditor'
import { revertSelectedRanges } from '../state/selectedRanges'
import { findLayout, layoutsFor, parseLayoutItem } from '../state/layouts'
import { goToNextReference, peekStore } from '../editor/references'
import {
  documentsStore,
  newFile,
  openFile,
  openPath,
  reopenWithEncoding,
  save,
  saveAll,
  saveWithEncoding,
} from '../state/documents'
import { viewersFor } from '../state/viewers'
import { addDevelopmentExtension, reloadExtensions } from '../ipc/extensions'
import { checkForUpdates, installFromGitHub } from '../state/extensionInstall'
import {
  listEncodings,
  listLanguages,
  setDocumentLanguage,
  type EncodingChoice,
  type LanguageChoice,
} from '../ipc/editor'
import { editorStatus } from '../state/editorStatus'
import { setIconTheme, setTheme, settingsFilePath } from '../ipc/workspace'
import { showOutput, showProblems } from '../state/output'
import { forgetExtensionIcons } from '../state/extensions'
import { openSettings } from '../state/settings'
import { DEFAULT_ICON_THEME } from '../state/iconTheme'
import { layoutStore, updateLayout } from '../state/layout'
import { notify } from '../state/notifications'
import { resetZoom, zoomIn, zoomOut } from '../state/zoom'
import { closeFolder, collapseAll, openFolder, workspaceStore } from '../state/workspace'
import { promptForTrust } from '../state/trust'
import { isMac } from '../editor/keymap'
import {
  beginCreate,
  beginRename,
  copyPath,
  deleteEntry,
  focusedEntry,
  revealInSystem,
  revealLabel,
} from '../panels/explorerState'
import { clearTerminal, closeTerminal, terminalsStore } from '../terminal/terminals'
import { menuOrigin, openDefaultProfileMenu, openNewTerminalMenu } from '../terminal/profileMenu'
import {
  activateTabNumber,
  closeAllEditors,
  closeCurrentTab,
  closeGroupTabs,
  closeItem,
  closeOtherTabs,
  closeSavedTabs,
  currentTab,
  cycleTab,
  documentTab,
  focusActiveTerminal,
  focusGroupNumber,
  focusGroupRelative,
  hasClosedTab,
  hasCurrentTab,
  moveCurrentTab,
  openTerminal,
  parseDocumentItem,
  parseViewerItem,
  reopenClosedTab,
  reopenWith,
  resetLayout,
  showView,
  splitCurrentTab,
  toggleSidebar,
  toggleTerminal,
  toggleTerminals,
} from '../dock/dock'
import type { EditorOp } from '../ipc/editor'
import { itemKind } from '../dock/model'
import {
  bindingsOf,
  commandLabel,
  formatKeybinding,
  listCommands,
  type Command,
  type Keybinding,
} from './registry'

const hasEditor = () => activeEditor.get() !== null
const hasPeek = () => peekStore.get() !== null
const hasDocument = () => documentsStore.get().activeId !== null

/** The active document, if it has a file (and so an encoding to change). */
function activeFileDocument() {
  const { documents, activeId } = documentsStore.get()
  return documents.find(document => document.id === activeId && document.path !== null)
}

let encodings: Promise<EncodingChoice[]> | null = null

/** Asks for an encoding, then reopens or saves the active document with it. */
async function pickEncoding(action: 'reopen' | 'save'): Promise<void> {
  const document = activeFileDocument()
  if (!document) {
    return
  }
  encodings ??= listEncodings()
  let choices: EncodingChoice[]
  try {
    choices = await encodings
  } catch (error) {
    encodings = null
    notify('error', `Listing the encodings failed: ${String(error)}`)
    return
  }
  const run = action === 'reopen' ? reopenWithEncoding : saveWithEncoding
  openQuickPick({
    placeholder: action === 'reopen' ? 'Reopen with encoding' : 'Save with encoding',
    items: choices.map(choice => ({
      label: choice.label,
      detail: choice.id === document.encodingId ? 'Current' : choice.id,
      run: () => void run(document.id, choice.id),
    })),
  })
}

/** Reopening or saving the active document with another encoding, as the status bar offers. */
function changeEncoding(): void {
  openQuickPick({
    placeholder: 'Change the file encoding',
    items: [
      { label: 'Reopen with Encoding', run: () => void pickEncoding('reopen') },
      { label: 'Save with Encoding', run: () => void pickEncoding('save') },
    ],
  })
}

/** Asks for a language for the active editor's document, as the status bar's language offers. */
async function pickLanguage(): Promise<void> {
  const editor = activeEditor.get()
  if (!editor) {
    return
  }
  const id = editor.documentId
  let languages: LanguageChoice[]
  try {
    languages = await listLanguages()
  } catch (error) {
    notify('error', `Listing the languages failed: ${String(error)}`)
    return
  }
  const current = editorStatus.get()?.language?.id ?? 'plaintext'
  // The backend's event refreshes the editor.
  const set = (language: string | null) => () => {
    setDocumentLanguage(id, language).catch((error: unknown) => {
      notify('error', `Changing the language failed: ${String(error)}`)
    })
  }
  const extensions = (language: LanguageChoice) =>
    language.fileExtensions.map(extension => `.${extension}`).join(', ') || undefined
  openQuickPick({
    placeholder: 'Select a language',
    items: [
      { label: 'Auto Detect', detail: 'From the file name', run: set(null) },
      {
        label: 'Plain Text',
        detail: current === 'plaintext' ? 'Current' : undefined,
        run: set('plaintext'),
      },
      ...languages.map(language => ({
        label: language.name,
        detail: language.id === current ? 'Current' : extensions(language),
        run: set(language.id),
      })),
    ],
  })
}

/** The file of the current tab, if it's a document's, a viewer's, or a file layout's. */
function currentFile(): string | null {
  const item = currentTab()
  if (!item) {
    return null
  }
  if (itemKind(item) === 'viewer') {
    return parseViewerItem(item).path
  }
  const layout = parseLayoutItem(item)
  if (layout) {
    return (findLayout(layout.layout)?.patterns.length ?? 0) > 0 ? layout.key : null
  }
  if (itemKind(item) === 'document') {
    const id = parseDocumentItem(item).document
    return documentsStore.get().documents.find(document => document.id === id)?.path ?? null
  }
  return null
}

/** Asks how to show the current tab's file: as text, in a viewer, or in a layout that can show it. */
function pickEditor(): void {
  const item = currentTab()
  const path = currentFile()
  if (!item || !path) {
    return
  }
  const showing =
    itemKind(item) === 'viewer'
      ? parseViewerItem(item).viewer
      : (parseLayoutItem(item)?.layout ?? null)
  const current = (id: string | null) =>
    id === showing && (id !== null || itemKind(item) === 'document') ? 'Current' : undefined
  const choices = [...viewersFor(path), ...layoutsFor(path)]
  openQuickPick({
    placeholder: 'Reopen the editor with',
    items: [
      { label: 'Text Editor', detail: current(null), run: () => void reopenWith(item, null) },
      ...choices.map(choice => ({
        label: choice.title,
        detail: current(choice.fullId) ?? (choice.priority === 'default' ? 'Default' : undefined),
        run: () =>
          void reopenWith(
            item,
            'webview' in choice ? { viewer: choice.fullId } : { layout: choice.fullId },
          ),
      })),
    ],
  })
}
const hasWorkspace = () => workspaceStore.get().info !== null
/** Runs `action` on the explorer's focused file or folder. */
const onFocusedEntry = (action: (path: string) => void | Promise<void>) => () => {
  const entry = focusedEntry()
  return entry ? action(entry.path) : undefined
}
const hasFocusedEntry = () => focusedEntry() !== null
const hasTerminal = () => terminalsStore.get().activeId !== null
const editor = () => activeEditor.get()

/** A command that applies `op` in the active editor, bound while it has focus. */
const editorOp = (
  id: string,
  category: string,
  title: string,
  keybinding: Keybinding,
  op: EditorOp,
): Command => ({
  id,
  category,
  title,
  keybinding,
  editorFocus: true,
  enabled: hasEditor,
  run: () => editor()?.dispatch(op),
})

export const builtinCommands: Command[] = [
  { id: 'file.newFile', category: 'File', title: 'New File', keybinding: 'mod+n', run: newFile },
  {
    id: 'file.openFile',
    category: 'File',
    title: 'Open File…',
    keybinding: 'mod+o',
    run: openFile,
  },
  {
    id: 'file.openFolder',
    category: 'File',
    title: 'Open Folder…',
    keybinding: 'mod+k mod+o',
    run: openFolder,
  },
  {
    id: 'file.closeFolder',
    category: 'File',
    title: 'Close Folder',
    keybinding: 'mod+k f',
    enabled: hasWorkspace,
    run: closeFolder,
  },
  {
    id: 'workspace.manageTrust',
    category: 'Workspaces',
    title: 'Manage Workspace Trust',
    enabled: hasWorkspace,
    run: () => promptForTrust(),
  },
  {
    id: 'file.save',
    category: 'File',
    title: 'Save',
    keybinding: 'mod+s',
    enabled: () => hasDocument() || activeOverride() !== null,
    // In a layout tab, the editor pane in use.
    run: async () => {
      await save(activeOverride()?.documentId)
    },
  },
  {
    id: 'file.saveAs',
    category: 'File',
    title: 'Save As…',
    keybinding: 'mod+shift+s',
    enabled: hasDocument,
    run: async () => {
      await save(undefined, true)
    },
  },
  {
    id: 'file.changeEncoding',
    category: 'File',
    title: 'Change File Encoding',
    enabled: () => activeFileDocument() !== undefined,
    run: changeEncoding,
  },
  {
    id: 'file.reopenWithEncoding',
    category: 'File',
    title: 'Reopen with Encoding…',
    enabled: () => activeFileDocument() !== undefined,
    run: () => pickEncoding('reopen'),
  },
  {
    id: 'file.saveWithEncoding',
    category: 'File',
    title: 'Save with Encoding…',
    enabled: () => activeFileDocument() !== undefined,
    run: () => pickEncoding('save'),
  },
  {
    id: 'editor.changeLanguage',
    category: 'View',
    title: 'Change Language Mode',
    keybinding: 'mod+k m',
    enabled: hasEditor,
    run: pickLanguage,
  },
  {
    id: 'view.reopenWith',
    category: 'View',
    title: 'Reopen Editor With…',
    enabled: () => currentFile() !== null,
    run: pickEditor,
  },
  {
    id: 'file.saveAll',
    category: 'File',
    title: 'Save All',
    keybinding: 'mod+k s',
    enabled: () => documentsStore.get().documents.some(document => document.dirty),
    run: saveAll,
  },
  {
    // The active document, wherever focus is; Close Tab (Ctrl+W) closes the focused tab.
    id: 'file.close',
    category: 'View',
    title: 'Close Editor',
    enabled: hasDocument,
    run: () => closeItem(documentTab(documentsStore.get().activeId!)),
  },
  {
    id: 'workbench.commandPalette',
    category: 'View',
    title: 'Command Palette',
    keybinding: ['mod+shift+p', 'f1'],
    terminal: true,
    hidden: true,
    run: () => openPalette('>'),
  },
  {
    id: 'workbench.quickOpen',
    category: 'Go',
    title: 'Go to File…',
    keybinding: 'mod+p',
    terminal: true,
    run: () => openPalette(''),
  },
  {
    id: 'workbench.goToLine',
    category: 'Go',
    title: 'Go to Line/Column…',
    keybinding: 'ctrl+g',
    enabled: hasEditor,
    run: () => openPalette(':'),
  },
  {
    id: 'workbench.goToSymbol',
    category: 'Go',
    title: 'Go to Symbol in Editor…',
    keybinding: 'mod+shift+o',
    enabled: hasEditor,
    run: () => openPalette('@'),
  },
  {
    id: 'workbench.showAllSymbols',
    category: 'Go',
    title: 'Go to Symbol in Workspace…',
    keybinding: 'mod+t',
    run: () => openPalette('#'),
  },
  {
    id: 'workbench.toggleSidebar',
    category: 'View',
    title: 'Toggle Side Bar',
    keybinding: 'mod+b',
    terminal: true,
    run: toggleSidebar,
  },
  {
    id: 'view.toggleMinimap',
    category: 'View',
    title: 'Toggle Minimap',
    run: () => updateLayout({ minimapVisible: !layoutStore.get().minimapVisible }),
  },
  {
    id: 'diff.showNextChange',
    category: 'Diff',
    title: 'Show Next Change',
    keybinding: 'alt+f3',
    enabled: () => activeEditor.get() !== null,
    run: () => activeEditor.get()?.showChange(false),
  },
  {
    id: 'diff.showPreviousChange',
    category: 'Diff',
    title: 'Show Previous Change',
    keybinding: 'shift+alt+f3',
    enabled: () => activeEditor.get() !== null,
    run: () => activeEditor.get()?.showChange(true),
  },
  {
    id: 'diff.revertSelectedRanges',
    category: 'Diff',
    title: 'Revert Selected Ranges',
    enabled: () => activeEditor.get() !== null,
    run: () => revertSelectedRanges(),
  },
  {
    id: 'view.zoomIn',
    category: 'View',
    title: 'Zoom In',
    keybinding: ['mod+=', 'mod+shift+=', 'mod++'],
    terminal: true,
    run: zoomIn,
  },
  {
    id: 'view.zoomOut',
    category: 'View',
    title: 'Zoom Out',
    keybinding: ['mod+-', 'mod+shift+-'],
    terminal: true,
    run: zoomOut,
  },
  {
    id: 'view.resetZoom',
    category: 'View',
    title: 'Reset Zoom',
    keybinding: 'mod+0',
    terminal: true,
    run: resetZoom,
  },
  {
    id: 'workbench.togglePanel',
    category: 'View',
    title: 'Toggle Panel',
    keybinding: 'mod+j',
    terminal: true,
    run: toggleTerminals,
  },
  {
    id: 'terminal.toggle',
    category: 'View',
    title: 'Toggle Terminal',
    keybinding: 'ctrl+`',
    terminal: true,
    run: toggleTerminal,
  },
  {
    id: 'terminal.new',
    category: 'Terminal',
    title: 'New Terminal',
    keybinding: 'ctrl+shift+`',
    terminal: true,
    run: () => openTerminal(),
  },
  {
    id: 'terminal.newWithProfile',
    category: 'Terminal',
    title: 'New Terminal With Profile…',
    terminal: true,
    run: () => {
      const { x, y } = menuOrigin()
      return openNewTerminalMenu(x, y)
    },
  },
  {
    id: 'terminal.selectDefaultProfile',
    category: 'Terminal',
    title: 'Select Default Profile…',
    run: () => {
      const { x, y } = menuOrigin()
      return openDefaultProfileMenu(x, y)
    },
  },
  {
    id: 'terminal.focus',
    category: 'Terminal',
    title: 'Focus Terminal',
    run: focusActiveTerminal,
  },
  {
    id: 'terminal.kill',
    category: 'Terminal',
    title: 'Kill Terminal',
    enabled: hasTerminal,
    run: () => closeTerminal(),
  },
  {
    id: 'terminal.clear',
    category: 'Terminal',
    title: 'Clear',
    enabled: hasTerminal,
    run: () => clearTerminal(),
  },
  {
    id: 'view.closeTab',
    category: 'View',
    title: 'Close Tab',
    keybinding: 'mod+w',
    enabled: hasCurrentTab,
    run: closeCurrentTab,
  },
  {
    id: 'view.closeOtherTabs',
    category: 'View',
    title: 'Close Other Tabs',
    enabled: hasCurrentTab,
    run: closeOtherTabs,
  },
  {
    id: 'view.closeSavedTabs',
    category: 'View',
    title: 'Close Saved Tabs in Group',
    keybinding: 'mod+k u',
    enabled: hasCurrentTab,
    run: closeSavedTabs,
  },
  {
    id: 'view.closeGroupTabs',
    category: 'View',
    title: 'Close All Tabs in Group',
    keybinding: 'mod+k w',
    enabled: hasCurrentTab,
    run: () => closeGroupTabs(),
  },
  {
    id: 'view.closeAllEditors',
    category: 'View',
    title: 'Close All Editors',
    keybinding: 'mod+k mod+w',
    run: closeAllEditors,
  },
  {
    id: 'view.reopenClosedTab',
    category: 'View',
    title: 'Reopen Closed Tab',
    keybinding: 'mod+shift+t',
    terminal: true,
    enabled: hasClosedTab,
    run: reopenClosedTab,
  },
  ...(['left', 'right', 'up', 'down'] as const).map((direction): Command => ({
    id: `view.moveTab${direction[0]!.toUpperCase()}${direction.slice(1)}`,
    category: 'View',
    title: `Move Tab ${direction[0]!.toUpperCase()}${direction.slice(1)}`,
    // Up and down would take the editor's Add Cursor Above/Below.
    ...(direction === 'left' || direction === 'right'
      ? {
          keybinding: {
            default: `ctrl+alt+arrow${direction}`,
            mac: `ctrl+meta+arrow${direction}`,
          },
        }
      : {}),
    terminal: true,
    enabled: hasCurrentTab,
    run: () =>
      moveCurrentTab(direction === 'up' ? 'top' : direction === 'down' ? 'bottom' : direction),
  })),
  {
    id: 'view.splitRight',
    category: 'View',
    title: 'Split Tab Right',
    keybinding: 'mod+\\',
    enabled: hasCurrentTab,
    run: () => splitCurrentTab('right'),
  },
  {
    id: 'view.splitDown',
    category: 'View',
    title: 'Split Tab Down',
    enabled: hasCurrentTab,
    run: () => splitCurrentTab('bottom'),
  },
  {
    id: 'view.focusNextGroup',
    category: 'View',
    title: 'Focus Next Group',
    keybinding: 'f6',
    terminal: true,
    run: () => focusGroupRelative(1),
  },
  {
    id: 'view.focusPreviousGroup',
    category: 'View',
    title: 'Focus Previous Group',
    keybinding: 'shift+f6',
    terminal: true,
    run: () => focusGroupRelative(-1),
  },
  ...[1, 2, 3, 4, 5, 6, 7, 8, 9].map((n): Command => ({
    id: `view.focusGroup${n}`,
    category: 'View',
    title: `Focus Group ${n}`,
    keybinding: `mod+${n}`,
    terminal: true,
    run: () => focusGroupNumber(n),
  })),
  ...[1, 2, 3, 4, 5, 6, 7, 8, 9, 0].map((n): Command => ({
    id: n === 0 ? 'view.lastTab' : `view.tab${n}`,
    category: 'View',
    title: n === 0 ? 'Last Tab in Group' : `Tab ${n} in Group`,
    keybinding: { default: `alt+${n}`, mac: `ctrl+${n}` },
    enabled: hasCurrentTab,
    run: () => activateTabNumber(n),
  })),
  {
    id: 'view.resetLayout',
    category: 'View',
    title: 'Reset Layout',
    run: resetLayout,
  },
  {
    id: 'workbench.showExplorer',
    category: 'View',
    title: 'Show Explorer',
    keybinding: 'mod+shift+e',
    terminal: true,
    run: () => showView('explorer'),
  },
  {
    id: 'workbench.showSearch',
    category: 'View',
    title: 'Search in Files',
    keybinding: 'mod+shift+f',
    terminal: true,
    run: () => showView('search'),
  },
  {
    id: 'workbench.showExtensions',
    category: 'View',
    title: 'Extensions',
    keybinding: 'mod+shift+x',
    terminal: true,
    run: () => showView('extensions'),
  },
  {
    id: 'workbench.showProblems',
    category: 'View',
    title: 'Problems',
    keybinding: 'mod+shift+m',
    terminal: true,
    run: showProblems,
  },
  {
    id: 'workbench.showOutput',
    category: 'View',
    title: 'Output',
    keybinding: 'mod+shift+u',
    terminal: true,
    run: () => showOutput(),
  },
  {
    id: 'extensions.reload',
    category: 'Extensions',
    title: 'Reload Extensions',
    run: async () => {
      forgetExtensionIcons()
      await reloadExtensions()
      notify('info', 'Extensions reloaded.')
    },
  },
  {
    id: 'extensions.installFromGitHub',
    category: 'Extensions',
    title: 'Install Extension from GitHub…',
    run: installFromGitHub,
  },
  {
    id: 'extensions.checkForUpdates',
    category: 'Extensions',
    title: 'Check for Extension Updates',
    run: () => checkForUpdates(),
  },
  {
    id: 'extensions.addDevelopment',
    category: 'Extensions',
    title: 'Load Development Extension…',
    run: async () => {
      if (await addDevelopmentExtension()) {
        showView('extensions')
      }
    },
  },
  {
    id: 'explorer.newFile',
    category: 'Explorer',
    title: 'New File…',
    enabled: hasWorkspace,
    run: () => {
      showView('explorer', false)
      beginCreate(false)
    },
  },
  {
    id: 'explorer.newFolder',
    category: 'Explorer',
    title: 'New Folder…',
    enabled: hasWorkspace,
    run: () => {
      showView('explorer', false)
      beginCreate(true)
    },
  },
  {
    id: 'explorer.collapseAll',
    category: 'Explorer',
    title: 'Collapse Folders',
    enabled: hasWorkspace,
    run: collapseAll,
  },
  {
    id: 'explorer.rename',
    category: 'Explorer',
    title: 'Rename…',
    enabled: hasFocusedEntry,
    run: onFocusedEntry(path => {
      showView('explorer', false)
      beginRename(path)
    }),
  },
  {
    id: 'explorer.delete',
    category: 'Explorer',
    title: 'Delete',
    enabled: hasFocusedEntry,
    run: onFocusedEntry(path => deleteEntry(path)),
  },
  {
    id: 'explorer.deletePermanently',
    category: 'Explorer',
    title: 'Delete Permanently',
    enabled: hasFocusedEntry,
    run: onFocusedEntry(path => deleteEntry(path, true)),
  },
  {
    id: 'explorer.copyPath',
    category: 'Explorer',
    title: 'Copy Path',
    enabled: hasFocusedEntry,
    run: onFocusedEntry(path => copyPath(path, false)),
  },
  {
    id: 'explorer.copyRelativePath',
    category: 'Explorer',
    title: 'Copy Relative Path',
    enabled: hasFocusedEntry,
    run: onFocusedEntry(path => copyPath(path, true)),
  },
  {
    id: 'explorer.reveal',
    category: 'Explorer',
    title: revealLabel(),
    enabled: hasFocusedEntry,
    run: onFocusedEntry(revealInSystem),
  },
  {
    id: 'view.nextEditor',
    category: 'View',
    title: 'Next Tab',
    keybinding: ['ctrl+tab', 'ctrl+pagedown'],
    terminal: true,
    enabled: hasCurrentTab,
    run: () => cycleTab(1),
  },
  {
    id: 'view.previousEditor',
    category: 'View',
    title: 'Previous Tab',
    keybinding: ['ctrl+shift+tab', 'ctrl+pageup'],
    terminal: true,
    enabled: hasCurrentTab,
    run: () => cycleTab(-1),
  },
  {
    id: 'editor.find',
    category: 'Edit',
    title: 'Find',
    keybinding: 'mod+f',
    enabled: hasEditor,
    run: () => editor()?.openFind(false),
  },
  {
    id: 'editor.replace',
    category: 'Edit',
    title: 'Replace',
    keybinding: { default: 'mod+h', mac: 'mod+alt+f' },
    enabled: hasEditor,
    run: () => editor()?.openFind(true),
  },
  {
    id: 'editor.findNext',
    category: 'Edit',
    title: 'Find Next',
    keybinding: 'f3',
    enabled: hasEditor,
    run: () => editor()?.findNext(false),
  },
  {
    id: 'editor.findPrevious',
    category: 'Edit',
    title: 'Find Previous',
    keybinding: 'shift+f3',
    enabled: hasEditor,
    run: () => editor()?.findNext(true),
  },
  {
    id: 'editor.addNextMatch',
    category: 'Selection',
    title: 'Add Next Occurrence',
    keybinding: 'mod+d',
    editorFocus: true,
    enabled: hasEditor,
    run: () => editor()?.dispatch({ type: 'addNextMatch' }),
  },
  editorOp('editor.selectAllOccurrences', 'Selection', 'Select All Occurrences', 'mod+shift+l', {
    type: 'selectAllOccurrences',
  }),
  editorOp('editor.expandLineSelection', 'Selection', 'Expand Line Selection', 'mod+l', {
    type: 'selectLine',
  }),
  editorOp('editor.cursorsAtLineEnds', 'Selection', 'Add Cursors to Line Ends', 'alt+shift+i', {
    type: 'cursorsAtLineEnds',
  }),
  editorOp('editor.cursorUndo', 'Selection', 'Cursor Undo', 'mod+u', { type: 'cursorUndo' }),
  editorOp('editor.insertLineBelow', 'Edit', 'Insert Line Below', 'mod+enter', {
    type: 'insertLine',
    direction: 'down',
  }),
  editorOp('editor.insertLineAbove', 'Edit', 'Insert Line Above', 'mod+shift+enter', {
    type: 'insertLine',
    direction: 'up',
  }),
  editorOp('editor.moveLinesUp', 'Edit', 'Move Line Up', 'alt+arrowup', {
    type: 'moveLines',
    direction: 'up',
  }),
  editorOp('editor.moveLinesDown', 'Edit', 'Move Line Down', 'alt+arrowdown', {
    type: 'moveLines',
    direction: 'down',
  }),
  editorOp('editor.copyLinesUp', 'Edit', 'Copy Line Up', 'alt+shift+arrowup', {
    type: 'copyLines',
    direction: 'up',
  }),
  editorOp('editor.copyLinesDown', 'Edit', 'Copy Line Down', 'alt+shift+arrowdown', {
    type: 'copyLines',
    direction: 'down',
  }),
  editorOp('editor.deleteLines', 'Edit', 'Delete Line', 'mod+shift+k', { type: 'deleteLines' }),
  editorOp('editor.indentLines', 'Edit', 'Indent Line', 'mod+]', { type: 'indent' }),
  editorOp('editor.outdentLines', 'Edit', 'Outdent Line', 'mod+[', { type: 'outdent' }),
  editorOp('editor.toggleLineComment', 'Edit', 'Toggle Line Comment', 'mod+/', {
    type: 'toggleComment',
  }),
  editorOp('editor.jumpToBracket', 'Go', 'Go to Bracket', 'mod+shift+\\', {
    type: 'jumpToBracket',
  }),
  editorOp(
    'editor.fold',
    'View',
    'Fold',
    { default: 'mod+shift+[', mac: 'mod+alt+[' },
    {
      type: 'fold',
    },
  ),
  editorOp(
    'editor.unfold',
    'View',
    'Unfold',
    { default: 'mod+shift+]', mac: 'mod+alt+]' },
    {
      type: 'unfold',
    },
  ),
  editorOp('editor.toggleFold', 'View', 'Toggle Fold', 'mod+k mod+l', { type: 'toggleFold' }),
  editorOp('editor.foldRecursively', 'View', 'Fold Recursively', 'mod+k mod+[', {
    type: 'foldRecursively',
  }),
  editorOp('editor.unfoldRecursively', 'View', 'Unfold Recursively', 'mod+k mod+]', {
    type: 'unfoldRecursively',
  }),
  editorOp('editor.foldAll', 'View', 'Fold All', 'mod+k mod+0', { type: 'foldAll' }),
  editorOp('editor.unfoldAll', 'View', 'Unfold All', 'mod+k mod+j', { type: 'unfoldAll' }),
  editorOp('editor.foldAllRegions', 'View', 'Fold All Regions', 'mod+k mod+8', {
    type: 'foldAllRegions',
  }),
  editorOp('editor.unfoldAllRegions', 'View', 'Unfold All Regions', 'mod+k mod+9', {
    type: 'unfoldAllRegions',
  }),
  ...[1, 2, 3, 4, 5, 6, 7].map(level =>
    editorOp(`editor.foldLevel${level}`, 'View', `Fold Level ${level}`, `mod+k mod+${level}`, {
      type: 'foldLevel',
      level,
    }),
  ),
  {
    id: 'editor.scrollLineUp',
    category: 'View',
    title: 'Scroll Line Up',
    keybinding: 'ctrl+arrowup',
    editorFocus: true,
    enabled: hasEditor,
    run: () => editor()?.scrollBy(-1),
  },
  {
    id: 'editor.scrollLineDown',
    category: 'View',
    title: 'Scroll Line Down',
    keybinding: 'ctrl+arrowdown',
    editorFocus: true,
    enabled: hasEditor,
    run: () => editor()?.scrollBy(1),
  },
  {
    id: 'editor.goToDefinition',
    category: 'Go',
    title: 'Go to Definition',
    keybinding: 'f12',
    enabled: hasEditor,
    run: () => editor()?.goToDefinition(),
  },
  {
    id: 'editor.goToReferences',
    category: 'Go',
    title: 'Go to References',
    keybinding: 'shift+f12',
    enabled: hasEditor,
    run: () => editor()?.goToReferences(),
  },
  {
    id: 'editor.peekReferences',
    category: 'Go',
    title: 'Peek References',
    enabled: hasEditor,
    run: () => editor()?.peekReferences(),
  },
  {
    id: 'editor.goToNextReference',
    category: 'Go',
    title: 'Go to Next Reference',
    keybinding: 'f4',
    enabled: hasPeek,
    run: () => goToNextReference(true),
  },
  {
    id: 'editor.goToPreviousReference',
    category: 'Go',
    title: 'Go to Previous Reference',
    keybinding: 'shift+f4',
    enabled: hasPeek,
    run: () => goToNextReference(false),
  },
  {
    id: 'editor.triggerSuggest',
    category: 'Edit',
    title: 'Trigger Suggest',
    keybinding: 'ctrl+space',
    editorFocus: true,
    enabled: hasEditor,
    run: () => editor()?.triggerSuggest(),
  },
  {
    id: 'editor.showHover',
    category: 'Edit',
    title: 'Show Hover',
    enabled: hasEditor,
    run: () => editor()?.showHover(),
  },
  {
    id: 'editor.selectAll',
    category: 'Selection',
    title: 'Select All',
    enabled: hasEditor,
    run: () => editor()?.dispatch({ type: 'selectAll' }),
  },
  {
    id: 'editor.undo',
    category: 'Edit',
    title: 'Undo',
    enabled: hasEditor,
    run: () => editor()?.dispatch({ type: 'undo' }),
  },
  {
    id: 'editor.redo',
    category: 'Edit',
    title: 'Redo',
    enabled: hasEditor,
    run: () => editor()?.dispatch({ type: 'redo' }),
  },
  {
    id: 'preferences.openSettings',
    category: 'Preferences',
    title: 'Open Settings',
    keybinding: 'mod+,',
    terminal: true,
    run: () => openSettings(),
  },
  {
    id: 'preferences.openSettingsFile',
    category: 'Preferences',
    title: 'Open Settings (TOML)',
    terminal: true,
    run: async () => {
      await openPath(await settingsFilePath())
    },
  },
  {
    id: 'developer.togglePerformance',
    category: 'Developer',
    title: 'Toggle Performance Overlay',
    run: togglePerfOverlay,
  },
  {
    id: 'developer.resetPerformance',
    category: 'Developer',
    title: 'Reset Frontend Performance Counters',
    run: resetPerf,
  },
  {
    id: 'developer.reloadWindow',
    category: 'Developer',
    title: 'Reload Window',
    // Documents, unsaved changes, and extensions live in the backend, so
    // the reloaded workbench picks up where it was.
    run: () => window.location.reload(),
  },
  {
    id: 'preferences.themeDark',
    category: 'Preferences',
    title: 'Color Theme: Dark',
    run: () => setTheme('dark'),
  },
  {
    id: 'preferences.themeLight',
    category: 'Preferences',
    title: 'Color Theme: Light',
    run: () => setTheme('light'),
  },
  {
    id: 'preferences.themeSystem',
    category: 'Preferences',
    title: 'Color Theme: Follow System',
    run: () => setTheme('system'),
  },
  {
    id: 'preferences.iconThemeDefault',
    category: 'Preferences',
    title: 'File Icon Theme: Default',
    run: () => setIconTheme(DEFAULT_ICON_THEME),
  },
  {
    id: 'preferences.keyboardShortcuts',
    category: 'Preferences',
    title: 'Keyboard Shortcuts',
    keybinding: 'mod+k mod+s',
    run: () => newFile(keyboardShortcutsText()),
  },
]

/**
 * A plain-text list of every command with its id and keys, shown in a new
 * document so it can be searched, for writing `[keybindings]` settings.
 */
export function keyboardShortcutsText(mac = isMac()): string {
  const rows = listCommands()
    .filter(command => !command.hidden)
    .map(command => [
      commandLabel(command),
      command.id,
      bindingsOf(command, mac)
        .map(binding => formatKeybinding(binding, mac))
        .join(', '),
    ])
    .sort((a, b) => a[0]!.localeCompare(b[0]!))
  const header = ['Command', 'Id', 'Keys']
  const widths = [0, 1].map(column =>
    Math.max(header[column]!.length, ...rows.map(row => row[column]!.length)),
  )
  const line = (row: string[]) =>
    `${row[0]!.padEnd(widths[0]!)}  ${row[1]!.padEnd(widths[1]!)}  ${row[2]}`.trimEnd()
  return [
    'Keyboard shortcuts',
    '',
    'Change them in settings.toml (Preferences: Open Settings (TOML)) under [keybindings], e.g.',
    '  "workbench.showSearch" = "mod+shift+h"     "mod" is Ctrl, or Cmd on macOS; "" removes them',
    '',
    line(header),
    ...rows.map(line),
    '',
  ].join('\n')
}
