import { afterFrame } from '../app/windows'
import { ChevronRight, ChevronsDownUp, CircleX, Info, ListX, TriangleAlert, X } from 'lucide-react'
import { useId, type KeyboardEvent, type ReactNode } from 'react'
import { executeCommand } from '../commands/registry'
import { FileIcon } from '../components/FileIcon'
import { entryIcon } from '../components/fileIcons'
import { activeEditor, editorFor } from '../editor/activeEditor'
import { restartExtension } from '../ipc/extensions'
import type { FileProblem, FileProblems } from '../ipc/problems'
import { openPath, revealInDocument } from '../state/documents'
import { iconThemeStore } from '../state/iconTheme'
import { reportError } from '../state/notifications'
import { APP_CHANNEL, extensionChannel, showOutput } from '../state/output'
import {
  clearReported,
  fileProblemsStore,
  reportedProblems,
  useProblems,
  type Problem,
} from '../state/problems'
import { extensionQuery, openSettings } from '../state/settings'
import { createStore, useStore } from '../state/store'
import { workspaceStore } from '../state/workspace'
import { themeKind } from '../themes/theme'
import './ProblemsPanel.css'

interface Group {
  source: string
  problems: Problem[]
}

/** Problems grouped by source, in the order they're listed. */
function groupsOf(problems: readonly Problem[]): Group[] {
  const groups = new Map<string, Problem[]>()
  for (const problem of problems) {
    groups.set(problem.source, [...(groups.get(problem.source) ?? []), problem])
  }
  return [...groups].map(([source, list]) => ({ source, problems: list }))
}

/** Groups the user collapsed, by key; kept while the view is closed. */
export const collapsedGroups = createStore<ReadonlySet<string>>(new Set())

function toggleGroup(key: string): void {
  collapsedGroups.update(current => {
    const next = new Set(current)
    if (!next.delete(key)) {
      next.add(key)
    }
    return next
  })
}

const fileKey = (file: FileProblems) => file.path ?? `untitled:${file.document}`

const timeFormat = new Intl.DateTimeFormat(undefined, {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
})

/**
 * The folder of `path`, relative to the open folder when it's inside it
 * (`src/state`), as shown beside a file's name.
 */
export function folderOf(path: string, root: string | null): string {
  const normalized = path.replace(/\\/g, '/')
  const folder = normalized.slice(0, Math.max(normalized.lastIndexOf('/'), 0))
  if (root) {
    const base = root.replace(/\\/g, '/').replace(/\/$/, '')
    // Paths on Windows and macOS usually ignore case.
    if (folder.toLowerCase() === base.toLowerCase()) {
      return ''
    }
    if (folder.toLowerCase().startsWith(`${base.toLowerCase()}/`)) {
      return folder.slice(base.length + 1)
    }
  }
  return folder
}

/** Moves focus between the view's rows with the arrow keys, Home, and End. */
function onListKeys(event: KeyboardEvent<HTMLElement>): void {
  const keys = ['ArrowDown', 'ArrowUp', 'Home', 'End']
  if (!keys.includes(event.key)) {
    return
  }
  const rows = [...event.currentTarget.querySelectorAll<HTMLElement>('[data-problems-row]')]
  const index = rows.indexOf(event.currentTarget.ownerDocument.activeElement as HTMLElement)
  if (index === -1) {
    return
  }
  event.preventDefault()
  const next =
    event.key === 'Home'
      ? 0
      : event.key === 'End'
        ? rows.length - 1
        : Math.min(Math.max(index + (event.key === 'ArrowDown' ? 1 : -1), 0), rows.length - 1)
  rows[next]?.focus()
}

/**
 * Problems with Flareite and with extensions (an extension that stopped,
 * settings that were ignored, warnings and errors notifications reported),
 * then problems in files, by file. Choosing a file's problem shows it in
 * the editor.
 */
export function ProblemsPanel() {
  const problems = useProblems()
  const { files, leftOut } = useStore(fileProblemsStore)
  if (problems.length === 0 && files.length === 0) {
    return (
      <p className="panel-empty">
        No problems have been found. Files are checked when they're open, and some language servers
        check the whole folder.
      </p>
    )
  }
  return (
    <>
      <ul className="problems" aria-label="Problems" onKeyDown={onListKeys}>
        {groupsOf(problems).map(group => (
          <ProblemGroup key={`source:${group.source}`} group={group} />
        ))}
        {files.map(file => (
          <FileGroup key={fileKey(file)} file={file} />
        ))}
      </ul>
      {leftOut > 0 && (
        <p className="panel-empty">
          {leftOut} more {leftOut === 1 ? 'problem isn’t' : 'problems aren’t'} listed.
        </p>
      )}
    </>
  )
}

function act(action: string, run: () => Promise<void> | void): void {
  Promise.resolve(run()).catch((error: unknown) => reportError(action, error))
}

interface GroupProps {
  /** Remembers whether it's collapsed. */
  groupKey: string
  icon?: ReactNode
  name: string
  /** Shown after the name, e.g. a file's folder. */
  detail?: string
  count: number
  actions?: ReactNode
  children: ReactNode
}

/** A group of problems, with a header that collapses it. */
function ProblemsGroup({ groupKey, icon, name, detail, count, actions, children }: GroupProps) {
  const id = useId()
  const expanded = !useStore(collapsedGroups).has(groupKey)
  return (
    <li className="problems__group" aria-labelledby={`${id}-name`}>
      <div className="problems__source">
        <button
          type="button"
          className="problems__toggle"
          aria-expanded={expanded}
          aria-controls={`${id}-list`}
          data-problems-row
          onClick={() => toggleGroup(groupKey)}
        >
          <ChevronRight
            className="problems__chevron"
            size={14}
            strokeWidth={1.5}
            aria-hidden="true"
          />
          {icon}
          <span id={`${id}-name`} className="problems__name">
            {name}
          </span>
          {detail && <span className="problems__detail">{detail}</span>}
          <span className="problems__count" aria-label={`${count} problems`}>
            {count}
          </span>
        </button>
        {actions && <span className="problems__actions">{actions}</span>}
      </div>
      {expanded && (
        <ul id={`${id}-list`} className="problems__list">
          {children}
        </ul>
      )}
    </li>
  )
}

function ProblemGroup({ group }: { group: Group }) {
  const { source, problems } = group
  const extension = problems.find(problem => problem.extension)?.extension ?? null
  const failed = problems.some(problem => problem.key.startsWith('failed:'))
  const settings = problems.some(problem => problem.key.startsWith('config:'))
  return (
    <ProblemsGroup
      groupKey={`source:${source}`}
      name={source}
      count={problems.length}
      actions={
        <>
          {failed && extension && (
            <button
              type="button"
              className="problems__button"
              onClick={() => act('Restarting the extension', () => restartExtension(extension))}
            >
              Restart
            </button>
          )}
          {settings && extension && (
            <button
              type="button"
              className="problems__button"
              onClick={() => openSettings(extensionQuery(extension))}
            >
              Settings
            </button>
          )}
          {source === 'Settings' && (
            <button
              type="button"
              className="problems__button"
              onClick={() => void executeCommand('preferences.openSettingsFile')}
            >
              Open settings.toml
            </button>
          )}
          {(extension || source === 'Flareite') && (
            <button
              type="button"
              className="problems__button"
              aria-label={`Show ${source} Log`}
              onClick={() => showOutput(extension ? extensionChannel(extension) : APP_CHANNEL)}
            >
              Show Log
            </button>
          )}
        </>
      }
    >
      {problems.map(problem => (
        <ProblemItem key={problem.key} problem={problem} />
      ))}
    </ProblemsGroup>
  )
}

const SEVERITY = {
  error: { Icon: CircleX, label: 'Error' },
  warning: { Icon: TriangleAlert, label: 'Warning' },
  information: { Icon: Info, label: 'Information' },
} as const

function SeverityIcon({ severity }: { severity: keyof typeof SEVERITY }) {
  const { Icon, label } = SEVERITY[severity]
  return (
    <Icon className="problem__icon" size={14} strokeWidth={1.75} role="img" aria-label={label} />
  )
}

function ProblemItem({ problem }: { problem: Problem }) {
  return (
    <li className="problem" data-severity={problem.severity}>
      <SeverityIcon severity={problem.severity} />
      <span className="problem__text">
        <span className="problem__message">{problem.message}</span>
        {problem.count > 1 && (
          <span className="problem__meta" title={`Reported ${problem.count} times`}>
            ×{problem.count}
          </span>
        )}
        {problem.time !== null && (
          <time className="problem__meta" dateTime={new Date(problem.time).toISOString()}>
            {timeFormat.format(problem.time)}
          </time>
        )}
      </span>
      {!problem.current && (
        <button
          type="button"
          className="problem__clear"
          aria-label={`Clear: ${problem.message}`}
          title="Clear"
          onClick={() => clearReported(problem.key)}
        >
          <X size={14} strokeWidth={1.5} aria-hidden="true" />
        </button>
      )}
    </li>
  )
}

const severityName = (severity: FileProblem['severity']) =>
  severity === 1 ? 'error' : severity === 2 ? 'warning' : 'information'

function FileGroup({ file }: { file: FileProblems }) {
  const iconTheme = useStore(iconThemeStore)
  const light = useStore(themeKind) === 'light'
  const root = useStore(workspaceStore).info?.root ?? null
  return (
    <ProblemsGroup
      groupKey={`file:${fileKey(file)}`}
      icon={
        <FileIcon
          className="problems__file-icon"
          size={16}
          kind="file"
          icon={entryIcon(file.title, null, iconTheme, light)}
        />
      }
      name={file.title}
      detail={file.path ? folderOf(file.path, root) : undefined}
      count={file.problems.length}
    >
      {file.problems.map((problem, index) => (
        <FileProblemItem key={index} file={file} problem={problem} />
      ))}
    </ProblemsGroup>
  )
}

/** Shows a problem in the editor, opening its file if needed, and focuses it. */
async function showProblem(file: FileProblems, problem: FileProblem): Promise<void> {
  if (file.document !== null) {
    revealInDocument(file.document, problem.start, problem.end)
  } else if (file.path) {
    await openPath(file.path, { start: problem.start, end: problem.end })
  }
  const id = file.document
  afterFrame(() => ((id !== null ? editorFor(id) : null) ?? activeEditor.get())?.focus())
}

function FileProblemItem({ file, problem }: { file: FileProblems; problem: FileProblem }) {
  const position = `Ln ${problem.start.line + 1}, Col ${problem.start.column + 1}`
  return (
    <li className="problem" data-severity={severityName(problem.severity)}>
      <button
        type="button"
        className="problem__open"
        data-problems-row
        title={`${problem.message}\n${file.path ?? file.title}, ${position}`}
        onClick={() => void showProblem(file, problem)}
      >
        <SeverityIcon severity={severityName(problem.severity)} />
        {/* One run of text, so it wraps in a narrow side bar. */}
        <span className="problem__text">
          <span className="problem__message">{problem.message}</span>
          {problem.source && <span className="problem__meta">{problem.source}</span>}
          <span className="problem__meta">[{position}]</span>
        </span>
      </button>
    </li>
  )
}

/** Buttons beside the Problems tab. */
export function ProblemsActions() {
  const reported = useStore(reportedProblems)
  const problems = useProblems()
  const { files } = useStore(fileProblemsStore)
  return (
    <>
      <button
        type="button"
        className="dock-action"
        title="Collapse All"
        aria-label="Collapse All"
        onClick={() =>
          collapsedGroups.set(
            new Set([
              ...problems.map(problem => `source:${problem.source}`),
              ...files.map(file => `file:${fileKey(file)}`),
            ]),
          )
        }
      >
        <ChevronsDownUp size={16} strokeWidth={1.5} aria-hidden="true" />
      </button>
      <button
        type="button"
        className="dock-action"
        title="Clear Reported Problems"
        aria-label="Clear Reported Problems"
        disabled={reported.length === 0}
        onClick={() => clearReported()}
      >
        <ListX size={16} strokeWidth={1.5} aria-hidden="true" />
      </button>
    </>
  )
}
