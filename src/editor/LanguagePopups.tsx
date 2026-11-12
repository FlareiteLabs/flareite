/** The hover card, completion list, and signature help shown over the editor. */
import { memo, useEffect, useRef, type CSSProperties } from 'react'
import { LinkedText } from '../components/LinkedText'
import type { CompletionItem, Hover, SignatureHelp } from '../ipc/lsp'
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

interface SignatureCardProps {
  help: SignatureHelp
  /** The signature shown, of several (overloads). */
  selected: number
  placement: Placement
  /** Shows another signature, by offset (Up and Down, or the arrows). */
  onStep: (by: number) => void
}

/** Signature help: the call's signature with the parameter being written in bold. */
export const SignatureCard = memo(function SignatureCard({
  help,
  selected,
  placement,
  onStep,
}: SignatureCardProps) {
  const count = help.signatures.length
  const signature = help.signatures[Math.min(selected, count - 1)]
  if (!signature) {
    return null
  }
  const active =
    signature.activeParameter !== null ? signature.parameters[signature.activeParameter] : undefined
  const label = signature.label
  return (
    <div
      className="signature-card"
      role="tooltip"
      style={placement}
      // Keep focus in the editor.
      onMouseDown={event => event.preventDefault()}
    >
      <div className="signature-card__header">
        {count > 1 && (
          <span className="signature-card__overloads">
            <button type="button" aria-label="Previous signature" onClick={() => onStep(-1)}>
              ‹
            </button>
            {Math.min(selected, count - 1) + 1}/{count}
            <button type="button" aria-label="Next signature" onClick={() => onStep(1)}>
              ›
            </button>
          </span>
        )}
        <code className="signature-card__label">
          {active && active.end > active.start ? (
            <>
              {label.slice(0, active.start)}
              <strong className="signature-card__active">
                {label.slice(active.start, active.end)}
              </strong>
              {label.slice(active.end)}
            </>
          ) : (
            label
          )}
        </code>
      </div>
      {active?.documentation && (
        <div className="signature-card__body">
          <PreviewDocument content={active.documentation} loadImage={noPictures} linkText />
        </div>
      )}
      {signature.documentation && (
        <div className="signature-card__body">
          <PreviewDocument content={signature.documentation} loadImage={noPictures} linkText />
        </div>
      )}
    </div>
  )
})
