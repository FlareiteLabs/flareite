import type { WindowId } from '../app/windows'
import { useStore } from '../state/store'
import { dragStore } from './drag'
import './DragGhost.css'

/**
 * What's being dragged, beside the pointer, in the window the pointer is
 * over (`window`). Files from other programs have the system's own drag
 * image instead.
 */
export function DragGhost({ window }: { window: WindowId }) {
  const drag = useStore(dragStore)
  if (!drag || drag.payload.kind === 'files' || drag.window !== window) {
    return null
  }
  return (
    <div
      className="drag-ghost"
      aria-hidden="true"
      data-copy={drag.copy || undefined}
      style={{ transform: `translate(${drag.x + 14}px, ${drag.y + 10}px)` }}
    >
      {drag.payload.label}
    </div>
  )
}
