import { FileText, ListX } from 'lucide-react'
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { LinkedText } from '../components/LinkedText'
import type { ExtensionStatus } from '../ipc/extensions'
import { logFilePath, readLogs, type LogLevel, type LogRecord } from '../ipc/logs'
import { openPath } from '../state/documents'
import { extensionsStore } from '../state/extensions'
import { notify, reportError } from '../state/notifications'
import { APP_CHANNEL, outputChannel } from '../state/output'
import { createStore, useStore } from '../state/store'
import './OutputPanel.css'

/** How often the shown channel is checked for new records. */
const POLL_MS = 1000
/** Records shown per channel; the backend keeps as many. */
const MAX_SHOWN = 2000

/** Per channel, the `seq` the user cleared the view at. */
export const clearedAt = createStore<Readonly<Record<string, number>>>({})

const LEVELS: { value: LogLevel; label: string }[] = [
  { value: 'trace', label: 'All levels' },
  { value: 'debug', label: 'Debug and above' },
  { value: 'info', label: 'Info and above' },
  { value: 'warn', label: 'Warnings and errors' },
  { value: 'error', label: 'Errors' },
]
const RANK: Record<LogLevel, number> = { trace: 0, debug: 1, info: 2, warn: 3, error: 4 }

/** A channel's name: Flareite, an extension's name, or a language server's. */
export function channelLabel(id: string, extensions: readonly ExtensionStatus[]): string {
  if (id === APP_CHANNEL) {
    return 'Flareite'
  }
  if (id.startsWith('extension:')) {
    const extension = id.slice('extension:'.length)
    return extensions.find(candidate => candidate.id === extension)?.name ?? extension
  }
  if (id.startsWith('server:')) {
    return `Language Server: ${id.slice('server:'.length)}`
  }
  return id
}

const timeFormat = new Intl.DateTimeFormat(undefined, {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  fractionalSecondDigits: 3,
  hour12: false,
})

/**
 * Recent log records of Flareite, an extension, or a language server, as
 * the backend keeps them. New records are fetched while the view is shown.
 */
export function OutputPanel() {
  const channel = useStore(outputChannel)
  const extensions = useStore(extensionsStore)
  const cleared = useStore(clearedAt)[channel] ?? 0
  const [channels, setChannels] = useState<string[]>([APP_CHANNEL])
  const [records, setRecords] = useState<LogRecord[]>([])
  const [level, setLevel] = useState<LogLevel>('trace')
  const [filter, setFilter] = useState('')
  const logRef = useRef<HTMLDivElement>(null)
  const atBottom = useRef(true)

  useEffect(() => {
    let cancelled = false
    let last = 0
    setRecords([])
    const poll = async () => {
      try {
        const logs = await readLogs(channel, last)
        if (cancelled) {
          return
        }
        setChannels(logs.channels.map(c => c.id))
        if (logs.records.length > 0) {
          last = logs.records.at(-1)!.seq
          setRecords(current => [...current, ...logs.records].slice(-MAX_SHOWN))
        }
      } catch {
        // Outside the desktop app; nothing to show.
      }
    }
    void poll()
    const timer = setInterval(() => void poll(), POLL_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [channel])

  const shown = useMemo(() => {
    const words = filter.toLowerCase()
    return records.filter(
      record =>
        record.seq > cleared &&
        RANK[record.level] >= RANK[level] &&
        (!words ||
          `${record.message} ${record.fields} ${record.target}`.toLowerCase().includes(words)),
    )
  }, [records, cleared, level, filter])

  // Follow new records while scrolled to the bottom.
  useLayoutEffect(() => {
    const log = logRef.current
    if (log && atBottom.current) {
      log.scrollTop = log.scrollHeight
    }
  }, [shown])

  const options = channels.includes(channel) ? channels : [...channels, channel]
  const label = channelLabel(channel, extensions)
  const withTarget = channel === APP_CHANNEL

  return (
    <div className="output">
      <div className="output__toolbar">
        <select
          className="output__control"
          aria-label="Log"
          value={channel}
          onChange={event => outputChannel.set(event.currentTarget.value)}
        >
          {options.map(id => (
            <option key={id} value={id}>
              {channelLabel(id, extensions)}
            </option>
          ))}
        </select>
        <select
          className="output__control"
          aria-label="Levels shown"
          value={level}
          onChange={event => setLevel(event.currentTarget.value as LogLevel)}
        >
          {LEVELS.map(option => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
        <input
          type="search"
          className="output__control output__filter"
          aria-label="Filter log"
          placeholder="Filter"
          spellCheck={false}
          value={filter}
          onChange={event => setFilter(event.currentTarget.value)}
        />
      </div>
      <div
        ref={logRef}
        className="output__log"
        role="log"
        aria-label={`${label} log`}
        // Announcing every line would drown out everything else.
        aria-live="off"
        tabIndex={0}
        onScroll={event => {
          const log = event.currentTarget
          atBottom.current = log.scrollHeight - log.scrollTop - log.clientHeight < 4
        }}
      >
        {shown.length === 0 ? (
          <p className="output__empty">
            {records.length === 0 ? `Nothing logged by ${label} yet.` : 'No lines match.'}
          </p>
        ) : (
          shown.map(record => (
            <div key={record.seq} className="output__line" data-level={record.level}>
              <span className="output__time">{timeFormat.format(record.time)}</span>{' '}
              <span className="output__level">{record.level.toUpperCase().padEnd(5)}</span>{' '}
              {withTarget && <span className="output__target">{record.target}: </span>}
              <span className="output__message">
                <LinkedText text={record.message} />
              </span>
              {record.fields && (
                <span className="output__fields">
                  {' '}
                  <LinkedText text={record.fields} />
                </span>
              )}
            </div>
          ))
        )}
      </div>
    </div>
  )
}

/** Opens today's log file, which has more than the Output view keeps. */
async function openLogFile(): Promise<void> {
  try {
    const path = await logFilePath()
    if (path) {
      await openPath(path)
    } else {
      notify('info', 'Flareite has no log file this session.')
    }
  } catch (error) {
    reportError('Opening the log file', error)
  }
}

/** Buttons beside the Output tab. */
export function OutputActions() {
  const channel = useStore(outputChannel)
  return (
    <>
      <button
        type="button"
        className="dock-action"
        title="Open Log File"
        aria-label="Open Log File"
        onClick={() => void openLogFile()}
      >
        <FileText size={16} strokeWidth={1.5} aria-hidden="true" />
      </button>
      <button
        type="button"
        className="dock-action"
        title="Clear Output"
        aria-label="Clear Output"
        onClick={() => {
          readLogs(channel, Number.MAX_SAFE_INTEGER)
            .then(logs => {
              const last = logs.channels.find(c => c.id === channel)?.last ?? 0
              clearedAt.update(current => ({ ...current, [channel]: last }))
            })
            .catch((error: unknown) => reportError('Clearing the output', error))
        }}
      >
        <ListX size={16} strokeWidth={1.5} aria-hidden="true" />
      </button>
    </>
  )
}
