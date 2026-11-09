import { useOverlay } from '../state/overlays'
import { useEffect, useId, useRef, type KeyboardEvent, type ReactNode } from 'react'
import './ModalDialog.css'

interface ModalDialogProps {
  title: string
  /** Shown before the title. */
  icon?: ReactNode
  /** The dialog's text, read with its title. */
  children: ReactNode
  /** Below the text, before the buttons (such as a checkbox). */
  extra?: ReactNode
  buttons: ReactNode
  /** Escape: closes without an answer. */
  onClose: () => void
}

/**
 * A dialog that asks something and waits for the answer: the rest of the
 * window can't be used meanwhile. It takes focus itself, so Enter doesn't
 * choose by accident, keeps Tab inside, and gives focus back when it closes.
 */
export function ModalDialog({ title, icon, children, extra, buttons, onClose }: ModalDialogProps) {
  useOverlay()
  const dialogRef = useRef<HTMLDivElement>(null)
  const titleId = useId()
  const descriptionId = useId()

  useEffect(() => {
    const previous = document.activeElement
    dialogRef.current?.focus()
    return () => {
      if (previous instanceof HTMLElement) {
        previous.focus()
      }
    }
  }, [])

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === 'Escape') {
      event.preventDefault()
      onClose()
      return
    }
    if (event.key === 'Tab' && dialogRef.current) {
      const controls = [...dialogRef.current.querySelectorAll<HTMLElement>('button, input')]
      const first = controls[0]
      const last = controls[controls.length - 1]
      if (
        event.shiftKey &&
        (document.activeElement === first || document.activeElement === dialogRef.current)
      ) {
        event.preventDefault()
        last?.focus()
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault()
        first?.focus()
      }
    }
  }

  return (
    <div className="dialog-backdrop">
      <div
        ref={dialogRef}
        className="dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={descriptionId}
        tabIndex={-1}
        onKeyDown={onKeyDown}
      >
        <div className="dialog__heading">
          {icon}
          <h2 id={titleId} className="dialog__title">
            {title}
          </h2>
        </div>
        <div id={descriptionId} className="dialog__text">
          {children}
        </div>
        {extra}
        <div className="dialog__buttons">{buttons}</div>
      </div>
    </div>
  )
}
