// The script Flareite puts first in a file page: a file of the open folder
// shown as a web page in a layout's pane (API 1.15). Unlike an extension's
// webview page, the page is the user's, so it gets no `flareite` object:
// this only passes on the workbench's shortcuts, as webview pages do, keeps
// the page's scroll position when it shows again after an edit, and, in a
// page that uses the kit, follows the workbench's theme.
//
// A page that uses the kit is being made for an extension's webview, so it
// also gets a stand-in `flareite` object (`FLAREITE_STAND_IN` is set before
// this script for it), with the real one's API, so the page's scripts run:
// the theme works as in a webview; storage is kept by the workbench for the
// pane while the window is open (never the extension's real storage); state
// lasts while the page is loaded; and what would reach the extension
// (messages, action states) is logged to the console, where
// `flareite.standIn` can also play the extension's part (`receive`, `action`,
// `fileChanged`).
;(() => {
  // As in webview.js: the frame can't reach the editor; drop the fallback.
  if (window.chrome?.webview) {
    delete window.chrome.webview
  }
  const parent = window.parent
  const send = (type, data) => parent.postMessage({ flareite: 'webview', type, data }, '*')

  let keystrokes = []
  let chordPending = false
  const MODIFIERS = new Set(['Control', 'Shift', 'Alt', 'AltGraph', 'Meta', 'CapsLock', 'OS'])
  const PUNCTUATION = {
    Backquote: '`',
    Minus: '-',
    Equal: '=',
    BracketLeft: '[',
    BracketRight: ']',
    Backslash: '\\',
    Semicolon: ';',
    Quote: "'",
    Comma: ',',
    Period: '.',
    Slash: '/',
  }
  const unshifted = code => PUNCTUATION[code] ?? /^Digit(\d)$/.exec(code)?.[1]
  const letter = code => /^Key([A-Z])$/.exec(code)?.[1]?.toLowerCase()
  const matches = (stroke, event) => {
    const key = event.key === ' ' ? 'space' : event.key.toLowerCase()
    return (
      (stroke.key === key ||
        (event.shiftKey && stroke.key === unshifted(event.code)) ||
        (event.altKey &&
          (stroke.key === letter(event.code) || stroke.key === unshifted(event.code)))) &&
      stroke.ctrl === event.ctrlKey &&
      stroke.meta === event.metaKey &&
      stroke.alt === event.altKey &&
      stroke.shift === event.shiftKey
    )
  }
  window.addEventListener('keydown', event => {
    if (event.defaultPrevented || event.isComposing || MODIFIERS.has(event.key)) {
      return
    }
    if (!chordPending && !keystrokes.some(stroke => matches(stroke, event))) {
      return
    }
    event.preventDefault()
    const { key, code, ctrlKey, shiftKey, altKey, metaKey, repeat } = event
    send('keydown', { key, code, ctrlKey, shiftKey, altKey, metaKey, repeat })
  })

  // Where the page was scrolled to, before it showed again.
  let restore = null
  const scrollBack = () => {
    if (restore) {
      window.scrollTo(restore.x, restore.y)
    }
  }
  let scrollTimer = 0
  window.addEventListener(
    'scroll',
    () => {
      clearTimeout(scrollTimer)
      scrollTimer = setTimeout(() => send('scroll', { x: window.scrollX, y: window.scrollY }), 100)
    },
    { passive: true },
  )

  const standIn = typeof FLAREITE_STAND_IN === 'undefined' ? null : makeStandIn(FLAREITE_STAND_IN)

  window.addEventListener('message', event => {
    const message = event.data
    if (event.source !== parent || message?.flareite !== 'webview') {
      return
    }
    switch (message.type) {
      case 'storageResult':
        standIn?.storageResult(message.data)
        break
      case 'keybindings':
        keystrokes = message.data
        break
      case 'chord':
        chordPending = message.data
        break
      case 'theme': {
        // Only a page that uses the kit has the theme (`flareite-theme`): it
        // follows the workbench's, as webview pages do.
        const style = document.getElementById('flareite-theme')
        if (!style) {
          break
        }
        const { kind, variables } = message.data
        let css = `:root { --flareite-color-scheme: ${kind};`
        for (const name of Object.keys(variables).sort()) {
          css += ` --flareite-${name}: ${variables[name]};`
        }
        style.textContent = css + ' }'
        document.documentElement.dataset.flareiteTheme = kind
        standIn?.themeChanged(kind)
        break
      }
      case 'restoreScroll':
        restore = message.data
        if (document.readyState === 'complete') {
          scrollBack()
        } else {
          window.addEventListener('load', scrollBack, { once: true })
        }
        break
    }
  })

  /** The stand-in `flareite` object, defined as `window.flareite`. */
  function makeStandIn(options) {
    const log = (...args) => console.info('[flareite stand-in]', ...args)
    const json = (value, what) => {
      const text = JSON.stringify(value)
      if (text === undefined) {
        throw new TypeError(`${what} needs a value JSON can represent`)
      }
      return text
    }
    const listen = (listeners, listener) => {
      listeners.add(listener)
      return () => listeners.delete(listener)
    }
    const notify = (listeners, value) => {
      for (const listener of listeners) {
        listener(value)
      }
    }
    const messageListeners = new Set()
    const fileListeners = new Set()
    const themeListeners = new Set()
    const storageListeners = new Set()
    const actionListeners = new Set()
    let waiting = []
    let themeKind = options.theme || 'dark'
    let state
    document.documentElement.dataset.flareiteTheme = themeKind

    // Storage requests waiting for the workbench's answer, by id.
    const requests = new Map()
    let nextRequest = 0
    const request = (scope, op, key, value) =>
      new Promise((resolve, reject) => {
        const id = nextRequest++
        requests.set(id, { resolve, reject })
        send('storage', { id, scope, op, key, value })
      })
    const storage = scope =>
      Object.freeze({
        get: key =>
          request(scope, 'get', key).then(text => (text == null ? undefined : JSON.parse(text))),
        set: (key, value) =>
          request(scope, 'set', key, value === undefined ? null : json(value, 'storage.set')),
        keys: () => request(scope, 'keys'),
      })

    const deliver = data => {
      if (messageListeners.size === 0) {
        waiting.push(data)
      } else {
        notify(messageListeners, data)
      }
    }

    const flareite = {
      kind: 'preview',
      fileUrl: null,
      fileName: undefined,
      get theme() {
        return themeKind
      },
      postMessage(data) {
        const text = json(data, 'flareite.postMessage')
        sent.push(JSON.parse(text))
        log('message to the extension:', JSON.parse(text))
      },
      onMessage(listener) {
        messageListeners.add(listener)
        if (waiting.length > 0) {
          const delivered = waiting
          waiting = []
          queueMicrotask(() => delivered.forEach(data => listener(data)))
        }
        return () => messageListeners.delete(listener)
      },
      onFileChanged: listener => listen(fileListeners, listener),
      onThemeChanged: listener => listen(themeListeners, listener),
      getState() {
        return state
      },
      setState(value) {
        state = JSON.parse(json(value, 'flareite.setState'))
      },
      storage: storage('global'),
      workspaceStorage: storage('workspace'),
      onStorageChanged: listener => listen(storageListeners, listener),
      onAction: listener => listen(actionListeners, listener),
      setActionState(action, actionState) {
        log('action state:', action, actionState)
      },
    }
    // What the page sent, and the extension's part, for trying the page out.
    const sent = []
    flareite.standIn = Object.freeze({
      /** The messages the page sent with `postMessage`, in order. */
      sent,
      /** Delivers a message to the page as if the extension sent it. */
      receive: data => deliver(JSON.parse(json(data, 'standIn.receive'))),
      /** Tells the page one of its tab's actions was clicked. */
      action: id => notify(actionListeners, id),
      /** Tells the page its file changed, as in a viewer. */
      fileChanged: () => notify(fileListeners, null),
    })
    Object.defineProperty(window, 'flareite', { value: Object.freeze(flareite) })
    log('this page has a stand-in flareite object: messages reach no extension.')

    return {
      themeChanged(kind) {
        if (kind !== themeKind) {
          themeKind = kind
          notify(themeListeners, kind)
        }
      },
      storageResult(result) {
        const pending = requests.get(result?.id)
        if (!pending) {
          return
        }
        requests.delete(result.id)
        if (typeof result.error === 'string') {
          pending.reject(new Error(result.error))
        } else {
          pending.resolve(result.value)
        }
      },
    }
  }

  send('ready')
})()
