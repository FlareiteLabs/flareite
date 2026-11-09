import { useOverlay } from '../state/overlays'
import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from 'react'
import { reportError } from '../state/notifications'
import { useStore } from '../state/store'
import {
  closeContextMenu,
  contextMenuStore,
  type ContextMenuState,
  type MenuAction,
} from './contextMenuState'
import './ContextMenu.css'

/** The open context menu, if any. */
export function ContextMenu() {
  const state = useStore(contextMenuStore)
  return state ? <Menu key={`${state.x},${state.y}`} state={state} /> : null
}

function Menu({ state }: { state: ContextMenuState }) {
  useOverlay()
  const ref = useRef<HTMLUListElement>(null)
  const actions = state.entries.filter((entry): entry is MenuAction => entry !== null)
  const [selected, setSelected] = useState(() => actions.findIndex(a => !a.disabled))
  const [position, setPosition] = useState({ left: state.x, top: state.y })

  // Keep the menu inside the window.
  useLayoutEffect(() => {
    const menu = ref.current
    if (!menu) {
      return
    }
    const { width, height } = menu.getBoundingClientRect()
    setPosition({
      left: Math.max(0, Math.min(state.x, window.innerWidth - width - 4)),
      top: Math.max(0, Math.min(state.y, window.innerHeight - height - 4)),
    })
    menu.focus()
  }, [state])

  useEffect(() => {
    const close = (event: Event) => {
      if (!(event.target instanceof Node && ref.current?.contains(event.target))) {
        closeContextMenu()
      }
    }
    window.addEventListener('mousedown', close, true)
    window.addEventListener('blur', closeContextMenu)
    window.addEventListener('resize', closeContextMenu)
    return () => {
      window.removeEventListener('mousedown', close, true)
      window.removeEventListener('blur', closeContextMenu)
      window.removeEventListener('resize', closeContextMenu)
    }
  }, [])

  const dismiss = () => {
    closeContextMenu()
    if (state.returnFocus instanceof HTMLElement) {
      state.returnFocus.focus()
    }
  }

  const run = (action: MenuAction | undefined) => {
    if (!action || action.disabled) {
      return
    }
    dismiss()
    Promise.resolve(action.run()).catch((error: unknown) => reportError(action.label, error))
  }

  const step = (direction: number) => {
    if (actions.length === 0) {
      return
    }
    let index = selected
    for (let i = 0; i < actions.length; i++) {
      index = (index + direction + actions.length) % actions.length
      if (!actions[index]?.disabled) {
        setSelected(index)
        return
      }
    }
  }

  const onKeyDown = (event: KeyboardEvent<HTMLUListElement>) => {
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault()
        step(1)
        break
      case 'ArrowUp':
        event.preventDefault()
        step(-1)
        break
      case 'Enter':
      case ' ':
        event.preventDefault()
        run(actions[selected])
        break
      case 'Escape':
      case 'Tab':
        event.preventDefault()
        dismiss()
        break
    }
  }

  let actionIndex = -1
  return (
    <ul
      ref={ref}
      className="context-menu"
      role="menu"
      tabIndex={-1}
      style={position}
      onKeyDown={onKeyDown}
      onContextMenu={event => event.preventDefault()}
    >
      {state.entries.map((entry, index) => {
        if (entry === null) {
          return (
            <li key={`separator-${index}`} role="separator" className="context-menu__separator" />
          )
        }
        actionIndex++
        const mine = actionIndex
        return (
          <li
            key={`${entry.label}-${index}`}
            role="menuitem"
            aria-disabled={entry.disabled || undefined}
            className="context-menu__item"
            data-selected={mine === selected || undefined}
            onMouseEnter={() => !entry.disabled && setSelected(mine)}
            onClick={() => run(entry)}
          >
            <span className="context-menu__label">{entry.label}</span>
            {entry.hint && <span className="context-menu__hint">{entry.hint}</span>}
          </li>
        )
      })}
    </ul>
  )
}
