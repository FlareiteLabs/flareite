/**
 * An extension's webview page, in a viewer's or preview's tab.
 *
 * The page runs in a frame sandboxed with `allow-scripts` only: it has an
 * origin of its own, so it can't reach this document, the backend's IPC,
 * or storage, and it can't navigate, open windows, or submit forms. The
 * backend serves it from the `flareite-webview:` protocol with a content
 * security policy that keeps it to its own files. The only way out is
 * `postMessage`, and only messages from this frame's window, in the
 * bridge's shape, are acted on.
 *
 * The page gets the theme as it changes, and the workbench's keybindings,
 * so it can pass on the shortcuts it doesn't handle itself: those arrive
 * here as key presses, which the keybinding handler gets as if pressed on
 * the frame.
 */
import { useEffect, useRef, useState } from 'react'
import {
  getWebview,
  sendWebviewMessage,
  setWebviewState,
  webviewStorage,
  type WebviewInfo,
} from '../ipc/extensions'
import { onKeybindingsChanged, pendingChord, webviewKeystrokes } from '../commands/registry'
import { reportError } from '../state/notifications'
import { promptForTrust } from '../state/trust'
import { workspaceStore } from '../state/workspace'
import { useStore } from '../state/store'
import {
  currentWebviewTheme,
  onWebviewTheme,
  registerWebviewFrame,
  setActionState,
  webviewVersions,
} from '../state/webviews'
import './WebviewFrame.css'

/** A message between the bridge (`webview.js`) and the workbench. */
interface BridgeMessage {
  flareite: 'webview'
  type: string
  data?: unknown
}

const isBridgeMessage = (data: unknown): data is BridgeMessage =>
  typeof data === 'object' && data !== null && (data as BridgeMessage).flareite === 'webview'

/** A page's storage request (`flareite.storage`). */
interface StorageRequest {
  id: number
  scope: string
  op: string
  key?: string
  value?: string | null
}

/** A key press the page passed on. */
interface KeyPress {
  key: string
  code: string
  ctrlKey: boolean
  shiftKey: boolean
  altKey: boolean
  metaKey: boolean
  repeat: boolean
}

const isKeyPress = (data: unknown): data is KeyPress => {
  const press = data as KeyPress | null
  return (
    typeof press === 'object' &&
    press !== null &&
    typeof press.key === 'string' &&
    typeof press.code === 'string' &&
    (['ctrlKey', 'shiftKey', 'altKey', 'metaKey', 'repeat'] as const).every(
      flag => typeof press[flag] === 'boolean',
    )
  )
}

const isStorageRequest = (data: unknown): data is StorageRequest => {
  const request = data as StorageRequest | null
  return (
    typeof request === 'object' &&
    request !== null &&
    typeof request.id === 'number' &&
    typeof request.scope === 'string' &&
    typeof request.op === 'string' &&
    (request.key === undefined || typeof request.key === 'string') &&
    (request.value === undefined || request.value === null || typeof request.value === 'string')
  )
}

interface WebviewFrameProps {
  /** The viewer's or preview's id. */
  id: string
  /** Names the frame for assistive technology. */
  title: string
  /** In a viewer, bumped when the file changes on disk; the page is told. */
  fileVersion?: number
}

export function WebviewFrame({ id, title, fileVersion }: WebviewFrameProps) {
  const version = useStore(webviewVersions)[id] ?? 0
  const [info, setInfo] = useState<WebviewInfo | null>(null)
  const frame = useRef<HTMLIFrameElement>(null)
  /** Whether the page has loaded the bridge. */
  const ready = useRef(false)

  useEffect(() => {
    let cancelled = false
    getWebview(id).then(
      next => {
        if (!cancelled) {
          setInfo(next)
        }
      },
      (error: unknown) => reportError('Loading the page', error),
    )
    return () => {
      cancelled = true
    }
  }, [id, version])

  const post = (type: string, data?: unknown) =>
    // The page's origin is opaque, so it can't be named.
    frame.current?.contentWindow?.postMessage({ flareite: 'webview', type, data }, '*')

  // Passes the page's messages on, and the extension's to the page once
  // it's ready. A new page (a new version) starts over.
  useEffect(() => {
    ready.current = false
    let stop: (() => void) | null = null
    const onMessage = (event: MessageEvent) => {
      if (!frame.current || event.source !== frame.current.contentWindow) {
        return
      }
      const message: unknown = event.data
      if (!isBridgeMessage(message)) {
        return
      }
      if (message.type === 'ready') {
        ready.current = true
        stop?.()
        const theme = currentWebviewTheme()
        if (theme) {
          post('theme', theme)
        }
        post('keybindings', webviewKeystrokes())
        stop = registerWebviewFrame(id, post)
      } else if (message.type === 'keydown' && isKeyPress(message.data)) {
        frame.current.dispatchEvent(
          new KeyboardEvent('keydown', { ...message.data, bubbles: true, cancelable: true }),
        )
      } else if (message.type === 'actionState' && typeof message.data === 'object') {
        const { action, state } = message.data as { action?: unknown; state?: unknown }
        if (typeof action === 'string') {
          setActionState(id, action, state)
        }
      } else if (message.type === 'message' && typeof message.data === 'string') {
        sendWebviewMessage(id, message.data).catch((error: unknown) =>
          reportError('Sending a message from the page', error),
        )
      } else if (message.type === 'setState' && typeof message.data === 'string') {
        setWebviewState(id, message.data).catch((error: unknown) =>
          reportError('Saving the page’s state', error),
        )
      } else if (message.type === 'storage' && isStorageRequest(message.data)) {
        // The page gets the answer, or the error, from its promise.
        const request = message.data
        webviewStorage(id, request.scope, request.op, request.key ?? null, request.value ?? null)
          .then(
            value => ({ id: request.id, value }),
            (error: unknown) => ({
              id: request.id,
              error: error instanceof Error ? error.message : String(error),
            }),
          )
          .then(result => post('storageResult', result))
      }
    }
    window.addEventListener('message', onMessage)
    return () => {
      window.removeEventListener('message', onMessage)
      stop?.()
    }
  }, [id, info?.url, info?.version])

  // Keeps the page's copy of the keybindings, and whether a chord is being
  // typed (its next key is passed on whatever it is), up to date.
  useEffect(() => {
    const keybindings = () => ready.current && post('keybindings', webviewKeystrokes())
    const chord = () => ready.current && post('chord', pendingChord.get() !== null)
    const stopKeybindings = onKeybindingsChanged(keybindings)
    const stopChord = pendingChord.subscribe(chord)
    return () => {
      stopKeybindings()
      stopChord()
    }
  }, [])

  // Tells the page its file changed, not when it first loads.
  const shownFile = useRef(fileVersion)
  useEffect(() => {
    if (fileVersion !== shownFile.current) {
      shownFile.current = fileVersion
      if (ready.current) {
        post('fileChanged')
      }
    }
  }, [fileVersion])

  if (!info) {
    return <div className="webview" aria-busy="true" aria-label={title} />
  }
  return (
    <div className="webview">
      <iframe
        key={info.version}
        ref={frame}
        className="webview__frame"
        title={title}
        src={info.version === 0 ? info.url : `${info.url}?v=${info.version}`}
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
      />
    </div>
  )
}

/** Where each file page was scrolled to, by webview, so it stays there when shown again. */
const pageScrolls = new Map<string, { x: number; y: number }>()

/** Most values a file page's stand-in storage keeps per scope, and the largest. */
const MAX_STAND_IN_KEYS = 1_000
const MAX_STAND_IN_VALUE = 1024 * 1024

/**
 * What file pages' stand-in `flareite` objects store (JSON text), by
 * webview, then scope, then key: kept while the window is open, so a page
 * finds its values again when it shows again after an edit. It's never the
 * extension's storage: the page is the user's.
 */
const standInStores = new Map<string, Map<string, Map<string, string>>>()

/** Answers a stand-in storage request: `{ id, value }`, or `{ id, error }`. */
function standInStorage(page: string, request: StorageRequest): unknown {
  const scopes = standInStores.get(page) ?? new Map<string, Map<string, string>>()
  standInStores.set(page, scopes)
  if (request.scope !== 'global' && request.scope !== 'workspace') {
    return { id: request.id, error: `no storage scope ${request.scope}` }
  }
  const store = scopes.get(request.scope) ?? new Map<string, string>()
  scopes.set(request.scope, store)
  switch (request.op) {
    case 'get':
      return { id: request.id, value: store.get(request.key ?? '') ?? null }
    case 'keys':
      return { id: request.id, value: [...store.keys()].sort() }
    case 'set': {
      const key = request.key ?? ''
      if (request.value === null || request.value === undefined) {
        store.delete(key)
      } else if (request.value.length > MAX_STAND_IN_VALUE) {
        return { id: request.id, error: 'a stored value is at most 1 MiB' }
      } else if (!store.has(key) && store.size >= MAX_STAND_IN_KEYS) {
        return { id: request.id, error: 'too many stored values' }
      } else {
        store.set(key, request.value)
      }
      return { id: request.id, value: null }
    }
    default:
      return { id: request.id, error: `no storage operation ${request.op}` }
  }
}

interface FilePageFrameProps {
  /** The pane's webview id. */
  id: string
  title: string
  /** Bumped when the page should show again (its files changed). */
  revision: number
}

/**
 * A file of the open folder shown as a web page, in a layout's pane (API
 * 1.15). The page is the user's, served from the folder with the same
 * sandbox and policy as webview pages, but it gets only a small script of
 * Flareite's (`file-page.js`): no messages to or from an extension, and no
 * extension's storage. Of its messages, only these are acted on: it's
 * ready, a shortcut's key press, where it's scrolled to, and (from a page
 * that uses the kit, whose stand-in `flareite` object stores values here)
 * storage requests.
 */
export function FilePageFrame({ id, title, revision }: FilePageFrameProps) {
  const [info, setInfo] = useState<WebviewInfo | null>(null)
  const frame = useRef<HTMLIFrameElement>(null)
  const ready = useRef(false)
  const trust = useStore(workspaceStore).info?.trust

  useEffect(() => {
    let cancelled = false
    getWebview(id).then(
      next => {
        if (!cancelled) {
          setInfo(next)
        }
      },
      (error: unknown) => reportError('Loading the page', error),
    )
    return () => {
      cancelled = true
    }
  }, [id, trust])

  const post = (type: string, data?: unknown) =>
    frame.current?.contentWindow?.postMessage({ flareite: 'webview', type, data }, '*')

  useEffect(() => {
    ready.current = false
    let stop: (() => void) | null = null
    const onMessage = (event: MessageEvent) => {
      if (!frame.current || event.source !== frame.current.contentWindow) {
        return
      }
      const message: unknown = event.data
      if (!isBridgeMessage(message)) {
        return
      }
      if (message.type === 'ready') {
        ready.current = true
        post('keybindings', webviewKeystrokes())
        const scroll = pageScrolls.get(id)
        if (scroll) {
          post('restoreScroll', scroll)
        }
        // A page that uses the kit follows the theme, as it changes. (It's
        // not registered as a webview: it hears nothing else, such as the
        // extension's stored values.)
        const theme = currentWebviewTheme()
        if (theme) {
          post('theme', theme)
        }
        stop?.()
        stop = onWebviewTheme(next => post('theme', next))
      } else if (message.type === 'storage' && isStorageRequest(message.data)) {
        // The stand-in `flareite.storage`: kept here, for this pane.
        post('storageResult', standInStorage(id, message.data))
      } else if (message.type === 'keydown' && isKeyPress(message.data)) {
        frame.current.dispatchEvent(
          new KeyboardEvent('keydown', { ...message.data, bubbles: true, cancelable: true }),
        )
      } else if (message.type === 'scroll' && isScroll(message.data)) {
        pageScrolls.set(id, message.data)
      }
    }
    window.addEventListener('message', onMessage)
    return () => {
      window.removeEventListener('message', onMessage)
      stop?.()
    }
  }, [id, info?.url, revision])

  useEffect(() => {
    const keybindings = () => ready.current && post('keybindings', webviewKeystrokes())
    const chord = () => ready.current && post('chord', pendingChord.get() !== null)
    const stopKeybindings = onKeybindingsChanged(keybindings)
    const stopChord = pendingChord.subscribe(chord)
    return () => {
      stopKeybindings()
      stopChord()
    }
  }, [])

  if (!info) {
    return <div className="webview" aria-busy="true" aria-label={title} />
  }
  if (info.blocked) {
    return (
      <div className="webview webview--blocked" role="status">
        <p>{info.blocked}</p>
        {trust !== undefined && trust !== 'trusted' && (
          <button
            type="button"
            className="layout-button"
            data-text
            onClick={() => promptForTrust()}
          >
            Trust the Folder…
          </button>
        )}
      </div>
    )
  }
  const version = `${info.version}.${revision}`
  return (
    <div className="webview">
      <iframe
        key={version}
        ref={frame}
        className="webview__frame"
        title={title}
        src={`${info.url}?v=${version}`}
        sandbox="allow-scripts"
        referrerPolicy="no-referrer"
      />
    </div>
  )
}

const isScroll = (data: unknown): data is { x: number; y: number } => {
  const scroll = data as { x?: unknown; y?: unknown } | null
  return (
    typeof scroll === 'object' &&
    scroll !== null &&
    typeof scroll.x === 'number' &&
    typeof scroll.y === 'number'
  )
}
