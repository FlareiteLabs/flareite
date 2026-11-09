/** Applies backend notifications to frontend state. */
import { editorsFor, mountedEditors } from '../editor/activeEditor'
import { fileDrag } from '../components/drag'
import { subscribeToEvents, type AppEvent } from '../ipc/workspace'
import { applySettings } from './appearance'
import {
  documentsStore,
  openPath,
  openProvided,
  revealInDocument,
  setDirty,
  updateSummary,
} from './documents'
import {
  closeLayoutTab,
  layoutChanged,
  noteReloaded,
  openLayoutTab,
  paneDecorationsChanged,
  revealInPane,
} from './layouts'
import { applySelectedRanges } from './selectedRanges'
import {
  refreshExtensions,
  refreshStatusItems,
  showExtensionFailure,
  showExtensionMessage,
  viewChanged,
} from './extensions'
import { refreshSources } from './extensionInstall'
import { notify } from './notifications'
import { previewChanged, refreshPreviews, showPreviewRequested } from './previews'
import { fileProblemsChanged, refreshSettingsProblem } from './problems'
import { refreshSettingsIfLoaded } from './settings'
import { showView } from './viewRequests'
import { filesChanged } from './viewers'
import { storageChanged, webviewChanged, webviewMessage } from './webviews'
import { refreshFileMarks } from './fileMarks'
import { containerOf, refreshViewBadges } from './views'
import { promptClosed, showPrompt } from './prompts'
import { trustRequested } from './trust'
import {
  openFolder,
  refreshDirectories,
  setIndexing,
  setWorkspace,
  setWorkspaceTrustInfo,
} from './workspace'

export function handleEvent(event: AppEvent): void {
  switch (event.type) {
    case 'workspaceChanged':
      setWorkspace(event.workspace)
      break
    case 'workspaceTrustChanged':
      setWorkspaceTrustInfo(event.workspace)
      break
    case 'workspaceTrustRequested':
      trustRequested(event.extension)
      break
    case 'extensionFileMarksChanged':
      void refreshFileMarks()
      break
    case 'extensionPrompt':
      showPrompt(event.prompt)
      break
    case 'extensionPromptClosed':
      promptClosed(event.id)
      break
    case 'filesChanged':
      refreshDirectories(event.directories)
      // The changed files are listed too: viewers showing them reload.
      filesChanged(event.directories)
      break
    case 'documentReloaded':
    case 'documentMoved':
    case 'documentSaved':
      updateSummary(event.document)
      refreshEditors(event.document.id)
      noteReloaded(event.document.id)
      break
    case 'documentChangedOnDisk':
      notify(
        'info',
        `${event.document.title} was changed by another program. Your unsaved changes were kept; saving will ask before overwriting.`,
      )
      break
    case 'settingsChanged':
      applySettings(event.settings)
      refreshSettingsIfLoaded()
      void refreshSettingsProblem()
      if (event.announce) {
        notify('info', 'Settings applied.')
      }
      break
    case 'languageServerChanged':
    case 'languagesChanged':
      for (const editor of mountedEditors()) {
        editor.refresh()
      }
      break
    case 'diagnosticsChanged':
      refreshEditors(event.documentId)
      fileProblemsChanged()
      break
    case 'problemsChanged':
      fileProblemsChanged()
      break
    case 'searchCounted':
    case 'syntaxUpdated':
    case 'decorationsChanged':
      refreshEditors(event.documentId)
      break
    case 'documentOpened':
      documentsStore.update(({ documents, activeId }) => ({
        documents: documents.some(d => d.id === event.document.id)
          ? documents
          : [...documents, event.document],
        activeId: event.active || activeId === null ? event.document.id : activeId,
      }))
      break
    case 'openFile':
      void openPath(event.path)
      break
    case 'quickDiffChanged':
      refreshEditors(event.documentId)
      break
    case 'applySelectedRanges':
      void applySelectedRanges()
      break
    case 'openFolder':
      void openFolder(event.path)
      break
    case 'openProvided':
      void openProvided(event.uri)
      break
    case 'openLayout':
      openLayoutTab({ layout: event.layout, key: event.key }, event.beside)
      break
    case 'layoutChanged':
      void layoutChanged({ layout: event.layout, key: event.key })
      break
    case 'closeLayout':
      closeLayoutTab({ layout: event.layout, key: event.key })
      break
    case 'paneDecorationsChanged':
      void paneDecorationsChanged({ layout: event.layout, key: event.key }, event.pane)
      break
    case 'revealInPane':
      revealInPane({ layout: event.layout, key: event.key }, event.pane, event.start, event.end)
      break
    case 'indexStatus':
      setIndexing(event.indexing)
      break
    case 'extensionsChanged':
      void refreshExtensions()
      void refreshSources()
      void refreshViewBadges()
      // Their settings, themes, and icon themes.
      refreshSettingsIfLoaded()
      break
    case 'extensionMessage':
      showExtensionMessage(event.id, event.extension, event.level, event.message)
      break
    case 'extensionFailed':
      showExtensionFailure(event.id, event.name, event.reason)
      break
    case 'extensionStatusItemsChanged':
      void refreshStatusItems()
      break
    case 'extensionViewChanged':
      viewChanged(event.view)
      void refreshViewBadges()
      break
    case 'extensionShowView':
      // A section shows with its container.
      showView(containerOf(event.view) ?? event.view, true)
      break
    case 'extensionReveal':
      revealInDocument(event.documentId, event.start, event.end)
      break
    case 'extensionPreviewsChanged':
      void refreshPreviews()
      break
    case 'extensionPreviewChanged':
      previewChanged(event.preview)
      break
    case 'extensionShowPreview':
      void showPreviewRequested(event.preview, event.beside)
      break
    case 'extensionWebviewChanged':
      webviewChanged(event.preview)
      break
    case 'extensionWebviewMessage':
      webviewMessage(event.preview, event.message)
      break
    case 'extensionStorageChanged':
      storageChanged(event.extension, event.scope, event.key, event.value, event.source)
      break
    case 'notice':
      notify(event.level, event.message)
      break
    case 'fileDrag':
      fileDrag(event)
      break
    case 'documentEdited':
      setDirty(event.documentId, true)
      refreshEditors(event.documentId)
      break
  }
}

/** Refreshes every editor showing a document. */
function refreshEditors(documentId: number): void {
  for (const editor of editorsFor(documentId)) {
    editor.refresh()
  }
}

export function listenToBackend(): void {
  subscribeToEvents(handleEvent)
}
