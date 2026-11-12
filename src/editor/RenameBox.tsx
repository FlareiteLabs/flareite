/** The box Rename Symbol (F2) shows over the symbol, as in VS Code. */
import { useEffect, useRef, useState } from 'react'
import type { Placement } from './LanguagePopups'
import './RenameBox.css'

interface RenameBoxProps {
  /** The name to start from, selected. */
  placeholder: string
  placement: Placement
  /** Renames to `name` (Enter). */
  onSubmit: (name: string) => void
  /** Closes without renaming (Escape, or focus moving away). */
  onCancel: () => void
}

export function RenameBox({ placeholder, placement, onSubmit, onCancel }: RenameBoxProps) {
  const inputRef = useRef<HTMLInputElement>(null)
  const [name, setName] = useState(placeholder)
  const done = useRef(false)

  useEffect(() => {
    const input = inputRef.current
    input?.focus()
    input?.select()
  }, [])

  const finish = (submit: boolean) => {
    if (done.current) {
      return
    }
    done.current = true
    if (submit && name.trim() && name !== placeholder) {
      onSubmit(name)
    } else {
      onCancel()
    }
  }

  return (
    <div className="rename-box" style={placement} onMouseDown={event => event.stopPropagation()}>
      <input
        ref={inputRef}
        className="rename-box__input"
        aria-label="New name"
        spellCheck={false}
        value={name}
        onChange={event => setName(event.target.value)}
        onKeyDown={event => {
          if (event.key === 'Enter') {
            event.preventDefault()
            finish(true)
          } else if (event.key === 'Escape') {
            event.preventDefault()
            finish(false)
          }
          // Keys typed here aren't the editor's or the workbench's.
          event.stopPropagation()
        }}
        onBlur={() => finish(false)}
      />
      <div className="rename-box__hint">Enter to rename, Escape to cancel</div>
    </div>
  )
}
