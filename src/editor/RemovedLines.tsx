/**
 * A change's removed lines, highlighted, with the characters that changed
 * marked: what the peek at a change from the file's original shows.
 */
import type { ReactNode } from 'react'
import type { LineChange, PeekLines } from '../ipc/editor'
import { highlighted } from './highlighted'
import type { TextOverlay } from './segments'
import './changes.css'

/** A change's removed lines, with their original line numbers: in a change's peek. */
export function RemovedLines({
  change,
  lines,
  numberWidth,
}: {
  change: LineChange
  lines: PeekLines | null
  numberWidth: number
}): ReactNode {
  const count = change.original[1] - change.original[0]
  return (
    <div className="removed-lines">
      {Array.from({ length: count }, (_, offset) => {
        const line = change.original[0] + offset
        const shown = lines?.lines[line - lines.start]
        const overlays: TextOverlay[] = shown
          ? change.chars.flatMap(({ original }) =>
              original.start.line <= line && line <= original.end.line
                ? [
                    {
                      from: original.start.line === line ? original.start.column : 0,
                      to: original.end.line === line ? original.end.column : shown.text.length,
                      className: 'diff-removed-text',
                    },
                  ]
                : [],
            )
          : []
        return (
          <div key={line} className="removed-lines__line">
            <span
              className="removed-lines__number"
              style={{ width: `calc(${numberWidth}ch + 32px + 20px)` }}
            >
              {line + 1}
            </span>
            <span className="removed-lines__text">
              {shown ? highlighted(shown.text, shown.spans, overlays) : ''}
            </span>
          </div>
        )
      })}
    </div>
  )
}
