/**
 * A change from a file's original shown between the editor's lines, as
 * when its gutter marker is clicked: the original lines it replaced, with
 * buttons to apply it to the original (stage it, for Git), revert it, move
 * to the previous or next change, or close.
 */
import { ArrowDown, ArrowUp, Plus, Undo2, X } from 'lucide-react'
import type { Hunk, LineChange } from '../ipc/editor'
import { RemovedLines } from './RemovedLines'
import './ChangePeek.css'

/** Most original lines shown at once; more scroll. */
export const MAX_CHANGE_LINES = 10

/** The lines a change's peek takes: its header and original lines. */
export function changePeekLines(hunk: Hunk): number {
  return 1 + Math.min(hunk.original[1] - hunk.original[0], MAX_CHANGE_LINES)
}

interface ChangePeekProps {
  hunk: Hunk
  /** The editor's visible width, which the peek keeps to. */
  width: number
  lineHeight: number
  /** Digits in the editor's line numbers, for the original lines' gutter. */
  numberWidth: number
  onApply: () => void
  onRevert: () => void
  onMove: (backward: boolean) => void
  onClose: () => void
}

export function ChangePeek({
  hunk,
  width,
  lineHeight,
  numberWidth,
  onApply,
  onRevert,
  onMove,
  onClose,
}: ChangePeekProps) {
  const change: LineChange = { original: hunk.original, modified: hunk.modified, chars: hunk.chars }
  const removed = hunk.original[1] - hunk.original[0]
  const button = (label: string, icon: React.ReactNode, onClick: () => void) => (
    <button
      type="button"
      className="change-peek__button"
      title={label}
      aria-label={label}
      // The editor would take the click as a cursor move.
      onMouseDown={event => event.stopPropagation()}
      onClick={onClick}
    >
      {icon}
    </button>
  )
  return (
    <div
      className="change-peek"
      role="region"
      aria-label={`Change ${hunk.index + 1} of ${hunk.total}`}
      style={{ width }}
      onMouseDown={event => event.stopPropagation()}
    >
      <div className="change-peek__header" style={{ height: lineHeight }}>
        <span className="change-peek__title">
          Change {hunk.index + 1} of {hunk.total}
          {removed === 0 && ' (added lines)'}
        </span>
        {hunk.applyTitle &&
          button(`${hunk.applyTitle} Change`, <Plus size={14} strokeWidth={1.75} />, onApply)}
        {button('Revert Change', <Undo2 size={14} strokeWidth={1.75} />, onRevert)}
        {button('Previous Change', <ArrowUp size={14} strokeWidth={1.75} />, () => onMove(true))}
        {button('Next Change', <ArrowDown size={14} strokeWidth={1.75} />, () => onMove(false))}
        {button('Close', <X size={14} strokeWidth={1.75} />, onClose)}
      </div>
      {removed > 0 && (
        <div
          className="change-peek__lines"
          style={{
            maxHeight: Math.min(removed, MAX_CHANGE_LINES) * lineHeight,
            overflowY: removed > MAX_CHANGE_LINES ? 'auto' : 'hidden',
          }}
        >
          <RemovedLines change={change} lines={hunk.lines} numberWidth={numberWidth} />
        </div>
      )}
    </div>
  )
}
