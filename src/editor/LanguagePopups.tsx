/** The hover card and completion list shown over the editor. */
import { memo, useEffect, useRef, type CSSProperties } from 'react'
import { LinkedText } from '../components/LinkedText'
import type { CompletionItem, Hover } from '../ipc/lsp'
import { PreviewDocument } from '../panels/PreviewPane'
import { SymbolKindIcon } from './symbolKinds'
import './LanguagePopups.css'

const SEVERITY = ['', 'error', 'warning', 'info', 'hint'] as const

export function severityName(severity: number): string {
  return SEVERITY[severity] ?? 'error'
}

/** Where a popup goes, relative to the editor frame. */
export type Placement = Pick<CSSProperties, 'top' | 'bottom' | 'left'>

interface HoverCardProps {
  hover: Hover
  placement: Placement
  onMouseLeave: () => void
}

export const HoverCard = memo(function HoverCard({
  hover,
  placement,
  onMouseLeave,
}: HoverCardProps) {
  return (
    <div
      className="hover-card"
      role="tooltip"
      style={placement}
      onMouseLeave={onMouseLeave}
      onMouseDown={event => event.stopPropagation()}
    >
      {hover.diagnostics.map((diagnostic, index) => (
        <div
          key={index}
          className="hover-card__diagnostic"
          data-severity={severityName(diagnostic.severity)}
        >
          <LinkedText text={diagnostic.message} />
          {diagnostic.source && <span className="hover-card__source"> {diagnostic.source}</span>}
        </div>
      ))}
      {hover.content && (
        <div className="hover-card__body">
          <PreviewDocument content={hover.content} loadImage={noPictures} linkText />
        </div>
      )}
    </div>
  )
})

/** Hover cards have no folder, so they show no picture files (only descriptions). */
const noPictures = () => Promise.reject(new Error('Hover cards show no pictures.'))

interface CompletionListProps {
  items: CompletionItem[]
  selected: number
  placement: Placement
  onPick: (item: CompletionItem) => void
}

/** Most rows shown at once; the list scrolls for more. */
export const COMPLETION_ROWS = 10
/** Row height and width of the completion list, as in the CSS. */
export const COMPLETION_ROW_HEIGHT = 22
export const COMPLETION_WIDTH = 360

export const CompletionList = memo(function CompletionList({
  items,
  selected,
  placement,
  onPick,
}: CompletionListProps) {
  const listRef = useRef<HTMLUListElement>(null)
  useEffect(() => {
    listRef.current?.children[selected]?.scrollIntoView({ block: 'nearest' })
  }, [selected])
  return (
    <ul
      ref={listRef}
      className="completion-list"
      role="listbox"
      aria-label="Suggestions"
      style={placement}
      // Keep focus in the editor.
      onMouseDown={event => event.preventDefault()}
    >
      {items.map((item, index) => (
        <li
          key={`${item.label}\u0000${index}`}
          role="option"
          aria-selected={index === selected}
          className="completion-list__item"
          onClick={() => onPick(item)}
        >
          <SymbolKindIcon kind={item.kind} />
          <span className="completion-list__label">{item.label}</span>
          {item.detail && <span className="completion-list__detail">{item.detail}</span>}
        </li>
      ))}
    </ul>
  )
})
