import { act, fireEvent } from '@testing-library/react'
import { fileDrag, type FileDragEvent } from '../components/drag'

interface Point {
  /** The element under the pointer: jsdom has no layout to find it. */
  element: Element
  x: number
  y: number
  ctrlKey?: boolean
}

let under: Element | null = null
// jsdom has no elementFromPoint; drags here say what's under the pointer.
document.elementFromPoint = () => under

let last = { x: 0, y: 0, ctrlKey: false }
const pointer = (init: { x: number; y: number; ctrlKey?: boolean }) => ({
  pointerId: 1,
  button: 0,
  clientX: init.x,
  clientY: init.y,
  ctrlKey: init.ctrlKey ?? false,
})

/**
 * Presses the pointer on `source`, as starting a drag does. Then `moveTo`
 * and `drop` (or `cancel`).
 */
export function pressToDrag(source: Element) {
  under = source
  last = { x: 0, y: 0, ctrlKey: false }
  fireEvent.pointerDown(source, pointer(last))
  return {
    moveTo(point: Point) {
      under = point.element
      last = { x: point.x, y: point.y, ctrlKey: point.ctrlKey ?? false }
      fireEvent.pointerMove(window, pointer(last))
    },
    drop() {
      fireEvent.pointerUp(window, pointer(last))
      under = null
    },
    cancel() {
      fireEvent.keyDown(document.activeElement ?? document.body, { key: 'Escape' })
      fireEvent.pointerUp(window, pointer(last))
      under = null
    },
  }
}

/** Drags `source` to `point` and drops it there. */
export function dragTo(source: Element, point: Point): void {
  const drag = pressToDrag(source)
  drag.moveTo(point)
  drag.drop()
}

/** A file drag from another program, as the backend reports it (`devicePixelRatio` is 1 here). */
export function dragFilesTo(
  point: Point,
  files: { files?: string[]; folders?: string[]; dropId?: number },
): void {
  under = point.element
  const event: Omit<FileDragEvent, 'phase'> = {
    window: null,
    x: point.x,
    y: point.y,
    files: files.files ?? [],
    folders: files.folders ?? [],
    dropId: null,
  }
  act(() => fileDrag({ ...event, phase: 'enter' }))
  act(() => fileDrag({ ...event, phase: 'drop', dropId: files.dropId ?? 1 }))
  under = null
}
