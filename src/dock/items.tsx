/**
 * What each kind of dock item looks like: its tab (title, icon, unsaved or
 * exited state), its content, and the buttons shown beside the tabs while
 * it's active.
 */
import { ChevronDown, FileText, Plus, SquareTerminal, Trash2 } from 'lucide-react'
import { useEffect, useRef, type ReactNode } from 'react'
import { findPage, type PageDescriptor } from '../app/pages'
import { useViews, type ViewDescriptor } from '../app/views'
import { executeCommand, getCommand, primaryKeybinding } from '../commands/registry'
import { ErrorBoundary } from '../components/ErrorBoundary'
import { FileIcon } from '../components/FileIcon'
import { Icon, isActionIcon, type IconName } from '../components/Icon'
import { entryIcon } from '../components/fileIcons'
import { Breadcrumbs } from '../editor/Breadcrumbs'
import { EditorView } from '../editor/EditorView'
import { ButtonView, LayoutPane } from '../editor/LayoutView'
import type { DocumentSummary } from '../ipc/editor'
import {
  runExtensionCommand,
  type ExtensionStatus,
  type PreviewInfo,
  type TabAction,
} from '../ipc/extensions'
import { PreviewPane } from '../panels/PreviewPane'
import { ViewerPane } from '../panels/ViewerPane'
import { documentsStore } from '../state/documents'
import { extensionsStore } from '../state/extensions'
import {
  findLayout,
  layoutFile,
  layoutsFor,
  layoutsStore,
  parseLayoutItem,
  runLayoutButton,
  type LayoutTab,
  type LayoutTabState,
} from '../state/layouts'
import { iconThemeStore } from '../state/iconTheme'
import { reportError } from '../state/notifications'
import { previewsStore } from '../state/previews'
import { useStore } from '../state/store'
import { viewerIds } from '../state/viewers'
import { postToWebview, webviewActionStates } from '../state/webviews'
import { openNewTerminalMenu } from '../terminal/profileMenu'
import { closeTerminal, getSession, terminalsStore, type TerminalTab } from '../terminal/terminals'
import { themeKind } from '../themes/theme'
import {
  documentTab,
  itemId,
  parseDocumentItem,
  parseViewerItem,
  reopenWith,
  showItem,
} from './dock'
import { itemKind, type ItemId } from './model'

export type ResolvedItem =
  /** A document's tab, showing its view `view`. */
  | { kind: 'document'; item: ItemId; document: DocumentSummary; view: number }
  | { kind: 'view'; item: ItemId; view: ViewDescriptor }
  | { kind: 'terminal'; item: ItemId; terminal: TerminalTab }
  | { kind: 'preview'; item: ItemId; preview: PreviewInfo }
  | { kind: 'page'; item: ItemId; page: PageDescriptor }
  /** A file in an extension's viewer, or (`viewer: null`) a binary file to open as text. */
  | { kind: 'viewer'; item: ItemId; viewer: string | null; path: string }
  /** An extension's layout; `state` is undefined until it's first shown. */
  | { kind: 'layout'; item: ItemId; tab: LayoutTab; state: LayoutTabState | undefined }

const fileName = (path: string) => path.split(/[/\\]/).pop() || path

/** The items that exist now (open documents, known views, running terminals, open previews, pages). */
export function useResolver(): (item: ItemId) => ResolvedItem | null {
  const { documents } = useStore(documentsStore)
  const { tabs } = useStore(terminalsStore)
  const previews = useStore(previewsStore)
  const extensions = useStore(extensionsStore)
  const views = useViews()
  const layouts = useStore(layoutsStore)
  return (item: ItemId) => {
    const id = itemId(item)
    switch (itemKind(item)) {
      case 'document': {
        const { document: documentId, view } = parseDocumentItem(item)
        const document = documents.find(candidate => candidate.id === documentId)
        return document ? { kind: 'document', item, document, view } : null
      }
      case 'view': {
        const view = views.find(candidate => candidate.id === id)
        return view ? { kind: 'view', item, view } : null
      }
      case 'terminal': {
        const terminal = tabs.find(candidate => candidate.id === Number(id))
        return terminal ? { kind: 'terminal', item, terminal } : null
      }
      case 'preview': {
        const preview = previews.find(candidate => candidate.id === id)
        return preview ? { kind: 'preview', item, preview } : null
      }
      case 'page': {
        const page = findPage(id, extensions)
        return page ? { kind: 'page', item, page } : null
      }
      case 'viewer':
        return { kind: 'viewer', item, ...parseViewerItem(item) }
      case 'layout': {
        const tab = parseLayoutItem(item)
        return tab ? { kind: 'layout', item, tab, state: layouts[item] } : null
      }
      default:
        return null
    }
  }
}

export function itemTitle(resolved: ResolvedItem): string {
  switch (resolved.kind) {
    case 'document':
      return resolved.document.title
    case 'view':
      return resolved.view.title
    case 'terminal':
      return resolved.terminal.title
    case 'preview':
      return resolved.preview.title
    case 'page':
      return resolved.page.title
    case 'viewer':
      return fileName(resolved.path)
    case 'layout': {
      const title = resolved.state?.view?.spec?.title
      if (title) {
        return title
      }
      const file = layoutFile(resolved.item)
      return file ? fileName(file) : (findLayout(resolved.tab.layout)?.title ?? resolved.tab.key)
    }
  }
}

/** The file a tab shows, for its explorer marks: a document's, a viewer's, or a layout's. */
export function itemPath(resolved: ResolvedItem): string | null {
  switch (resolved.kind) {
    case 'document':
      return resolved.document.path
    case 'viewer':
      return resolved.path
    case 'layout':
      return layoutFile(resolved.item)
    default:
      return null
  }
}

/** Whether a tab has unsaved changes. */
export function itemDirty(resolved: ResolvedItem): boolean {
  return (
    (resolved.kind === 'document' && resolved.document.dirty) ||
    (resolved.kind === 'layout' && (resolved.state?.dirty.length ?? 0) > 0)
  )
}

/** The tab's accessible name: the title, and whether it's unsaved or exited. */
export function itemLabel(resolved: ResolvedItem): string {
  const title = itemTitle(resolved)
  if (itemDirty(resolved)) {
    return `${title}, unsaved`
  }
  if (resolved.kind === 'terminal' && resolved.terminal.exited) {
    return `${title}, exited`
  }
  return title
}

export function itemTooltip(resolved: ResolvedItem): string {
  switch (resolved.kind) {
    case 'document':
      return resolved.document.path ?? resolved.document.title
    case 'view':
      return resolved.view.title
    case 'terminal':
      return resolved.terminal.description ?? resolved.terminal.title
    case 'preview': {
      const { document } = resolved.preview
      const path = documentsStore.get().documents.find(d => d.id === document)?.path
      return path ? `${resolved.preview.title} (${path})` : resolved.preview.title
    }
    case 'page':
      return resolved.page.title
    case 'viewer':
      return resolved.path
    case 'layout':
      return layoutFile(resolved.item) ?? itemTitle(resolved)
  }
}

interface ItemIconProps {
  resolved: ResolvedItem
  className?: string
  size?: number
}

export function ItemIcon({ resolved, className = 'dock-tab__icon', size = 16 }: ItemIconProps) {
  const iconTheme = useStore(iconThemeStore)
  const light = useStore(themeKind) === 'light'
  switch (resolved.kind) {
    case 'document':
      return (
        <FileIcon
          className={className}
          size={size}
          kind="file"
          icon={entryIcon(resolved.document.title, null, iconTheme, light)}
        />
      )
    case 'view':
      return (
        <span className={className} aria-hidden="true">
          <Icon name={resolved.view.icon} size={size} />
        </span>
      )
    case 'terminal':
      return (
        <SquareTerminal className={className} size={size} strokeWidth={1.5} aria-hidden="true" />
      )
    case 'preview':
      return (
        <span className={className} aria-hidden="true">
          {/* The backend allows only the view icons. */}
          <Icon name={resolved.preview.icon as IconName} size={size} />
        </span>
      )
    case 'page': {
      const PageIcon = resolved.page.icon
      return <PageIcon className={className} size={size} strokeWidth={1.5} aria-hidden="true" />
    }
    case 'viewer':
      return (
        <FileIcon
          className={className}
          size={size}
          kind="file"
          icon={entryIcon(fileName(resolved.path), null, iconTheme, light)}
        />
      )
    case 'layout': {
      const shown = layoutMainUri(resolved) ?? layoutFile(resolved.item) ?? itemTitle(resolved)
      return (
        <FileIcon
          className={className}
          size={size}
          kind="file"
          icon={entryIcon(sideName(shown), null, iconTheme, light)}
        />
      )
    }
  }
}

/** The file name at the end of a path or an address. */
const sideName = (side: string) => side.split(/[/\\:]/).pop() || side

/** The document of a layout's main editor pane, or its first editor's. */
function layoutMainUri(resolved: Extract<ResolvedItem, { kind: 'layout' }>): string | null {
  const spec = resolved.state?.view?.spec
  if (!spec) {
    return null
  }
  const editors = spec.nodes.flatMap(node =>
    node.kind === 'pane' && node.content.type === 'editor'
      ? [{ id: node.id, uri: node.content.uri }]
      : [],
  )
  return (editors.find(editor => editor.id === spec.main) ?? editors[0])?.uri ?? null
}

/** What the active tab shows. */
export function ItemContent({ resolved }: { resolved: ResolvedItem }): ReactNode {
  switch (resolved.kind) {
    case 'document':
      return (
        <>
          <Breadcrumbs document={resolved.document} />
          <ErrorBoundary name="The editor" resetKey={resolved.item}>
            <EditorView
              key={resolved.item}
              documentId={resolved.document.id}
              view={resolved.view}
              title={resolved.document.title}
            />
          </ErrorBoundary>
        </>
      )
    case 'view': {
      const View = resolved.view.component
      return (
        <div className="dock-view">
          <ErrorBoundary name={`The ${resolved.view.title} view`} resetKey={resolved.view.id}>
            <View />
          </ErrorBoundary>
        </div>
      )
    }
    case 'terminal':
      return <TerminalPane id={resolved.terminal.id} />
    case 'preview':
      return (
        <div className="dock-view">
          <ErrorBoundary name={`The preview ${resolved.preview.title}`} resetKey={resolved.item}>
            <PreviewPane key={resolved.preview.id} preview={resolved.preview} />
          </ErrorBoundary>
        </div>
      )
    case 'page': {
      const Page = resolved.page.component
      return (
        <div className="dock-view">
          <ErrorBoundary name={`The ${resolved.page.title} page`} resetKey={resolved.item}>
            <Page />
          </ErrorBoundary>
        </div>
      )
    }
    case 'layout':
      return (
        <ErrorBoundary name={`The tab ${itemTitle(resolved)}`} resetKey={resolved.item}>
          <LayoutPane key={resolved.item} item={resolved.item} />
        </ErrorBoundary>
      )
    case 'viewer':
      return (
        <div className="dock-view">
          <ErrorBoundary name={`The viewer of ${fileName(resolved.path)}`} resetKey={resolved.item}>
            <ViewerPane
              key={resolved.item}
              item={resolved.item}
              viewer={resolved.viewer}
              path={resolved.path}
            />
          </ErrorBoundary>
        </div>
      )
  }
}

/**
 * Whether an item stays loaded while its tab is hidden: a webview whose
 * extension asks for it (`retain_when_hidden`). Other items are unmounted
 * and drawn again when shown.
 */
export function keepsLoaded(
  resolved: ResolvedItem,
  extensions: readonly ExtensionStatus[],
): boolean {
  const contributions = extensions.map(extension => extension.contributions)
  switch (resolved.kind) {
    case 'viewer':
      return contributions.some(contribution =>
        contribution.viewers.some(
          viewer => viewer.fullId === resolved.viewer && viewer.webview?.retainWhenHidden,
        ),
      )
    case 'preview': {
      // `<extension id>.<preview id>/<key>`.
      const declared = resolved.preview.id.split('/')[0]
      return (
        resolved.preview.webview &&
        contributions.some(contribution =>
          contribution.previews.some(
            preview => preview.fullId === declared && preview.webview?.retainWhenHidden,
          ),
        )
      )
    }
    default:
      return false
  }
}

/** Buttons beside the tabs for the active item. */
export function ItemActions({ resolved }: { resolved: ResolvedItem }): ReactNode {
  if (resolved.kind === 'view') {
    const Actions = resolved.view.actions
    return Actions ? <Actions /> : null
  }
  if (resolved.kind === 'viewer' && resolved.viewer !== null) {
    return <ExtensionTabActions resolved={resolved} />
  }
  if (resolved.kind === 'preview') {
    const { document } = resolved.preview
    return (
      <>
        <ExtensionTabActions resolved={resolved} />
        {document !== null && (
          <button
            type="button"
            className="dock-action"
            title="Show Source"
            aria-label="Show Source"
            onClick={() => showItem(documentTab(document))}
          >
            <FileText size={16} strokeWidth={1.5} aria-hidden="true" />
          </button>
        )}
      </>
    )
  }
  if (resolved.kind === 'layout') {
    return <LayoutActions resolved={resolved} />
  }
  if (resolved.kind === 'document' && resolved.document.path) {
    return <SwitchToLayout item={resolved.item} path={resolved.document.path} />
  }
  if (resolved.kind === 'terminal') {
    const keys = primaryKeybinding(getCommand('terminal.new'))
    return (
      <>
        <button
          type="button"
          className="dock-action"
          title={keys ? `New Terminal (${keys})` : 'New Terminal'}
          aria-label="New Terminal"
          onClick={() => void executeCommand('terminal.new')}
        >
          <Plus size={16} strokeWidth={1.5} aria-hidden="true" />
        </button>
        <button
          type="button"
          className="dock-action dock-action--narrow"
          title="New Terminal With Profile…"
          aria-label="New Terminal With Profile"
          aria-haspopup="menu"
          onClick={event => {
            const r = event.currentTarget.getBoundingClientRect()
            void openNewTerminalMenu(r.left, r.bottom)
          }}
        >
          <ChevronDown size={14} strokeWidth={1.5} aria-hidden="true" />
        </button>
        <button
          type="button"
          className="dock-action"
          title="Kill Terminal"
          aria-label="Kill Terminal"
          onClick={() => closeTerminal(resolved.terminal.id)}
        >
          <Trash2 size={16} strokeWidth={1.5} aria-hidden="true" />
        </button>
      </>
    )
  }
  return null
}

/**
 * The buttons an extension declares for a viewer's or preview's tabs. A
 * click runs the action's command, with the tab as its argument (the
 * viewer's file, or the preview's key), or goes to the webview's page,
 * which can set each button's state.
 */
function ExtensionTabActions({
  resolved,
}: {
  resolved: Extract<ResolvedItem, { kind: 'viewer' | 'preview' }>
}): ReactNode {
  const extensions = useStore(extensionsStore)
  const ids = useStore(viewerIds)
  const states = useStore(webviewActionStates)
  // `<extension id>.<preview id>/<key>`, or `<viewer id>/<path>` once opened.
  const id = resolved.kind === 'preview' ? resolved.preview.id : ids[resolved.item]
  const declared = resolved.kind === 'preview' ? resolved.preview.id.split('/')[0] : resolved.viewer
  const actions = extensions
    .flatMap<{ fullId: string; actions: TabAction[] }>(extension =>
      resolved.kind === 'preview'
        ? extension.contributions.previews
        : extension.contributions.viewers,
    )
    .find(contribution => contribution.fullId === declared)?.actions
  if (!actions || actions.length === 0) {
    return null
  }
  const argument =
    resolved.kind === 'preview'
      ? resolved.preview.id.slice(resolved.preview.id.indexOf('/') + 1)
      : resolved.path
  const run = (action: TabAction) => {
    if (action.command) {
      runExtensionCommand(action.command, null, argument).catch((error: unknown) =>
        reportError(action.title, error),
      )
    } else if (id) {
      postToWebview(id, 'action', action.id)
    }
  }
  return actions.map(action => {
    const state = (id && states[id]?.[action.id]) || {}
    if (state.hidden) {
      return null
    }
    const title = state.title ?? action.title
    return (
      <button
        key={action.id}
        type="button"
        className="dock-action"
        title={title}
        aria-label={title}
        aria-pressed={state.pressed}
        disabled={state.disabled || (!action.command && !id)}
        onClick={() => run(action)}
      >
        {action.icon.kind === 'named' && isActionIcon(action.icon.name) ? (
          <Icon name={action.icon.name} size={16} />
        ) : action.icon.kind === 'image' ? (
          <img className="dock-action__image" src={action.icon.url} alt="" />
        ) : null}
      </button>
    )
  })
}

/**
 * A layout tab's buttons, as its extension set them; and for a layout of
 * files, a button that shows the file as text again.
 */
function LayoutActions({ resolved }: { resolved: Extract<ResolvedItem, { kind: 'layout' }> }) {
  const spec = resolved.state?.view?.spec
  const forFiles = (findLayout(resolved.tab.layout)?.patterns.length ?? 0) > 0
  return (
    <>
      {spec?.buttons.map(button => (
        <ButtonView
          key={button.id}
          button={button}
          className="dock-action"
          onClick={() =>
            runLayoutButton(resolved.item, null, button.command, button.argument, button.title)
          }
        />
      ))}
      {forFiles && (
        <button
          type="button"
          className="dock-action"
          title="Open in Text Editor"
          aria-label="Open in Text Editor"
          onClick={() => void reopenWith(resolved.item, null)}
        >
          <FileText size={16} strokeWidth={1.5} aria-hidden="true" />
        </button>
      )}
    </>
  )
}

/** Buttons beside a file's tab that show it in a layout for its kind of file, such as a preview. */
function SwitchToLayout({ item, path }: { item: ItemId; path: string }) {
  useStore(extensionsStore)
  return layoutsFor(path)
    .filter(layout => layout.icon !== null && isActionIcon(layout.icon))
    .map(layout => (
      <button
        key={layout.fullId}
        type="button"
        className="dock-action"
        title={layout.title}
        aria-label={layout.title}
        onClick={() => void reopenWith(item, { layout: layout.fullId })}
      >
        <Icon name={layout.icon as Parameters<typeof Icon>[0]['name']} size={16} />
      </button>
    ))
}

/** Shows a terminal's emulator, moving its element here. */
function TerminalPane({ id }: { id: number }) {
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const session = getSession(id)
    const element = ref.current
    if (!session || !element) {
      return
    }
    session.mount(element)
    const observer = new ResizeObserver(() => session.fit())
    observer.observe(element)
    return () => observer.disconnect()
  }, [id])

  return <div ref={ref} className="terminal-view" data-terminal />
}
