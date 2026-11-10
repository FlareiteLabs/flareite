import { query } from '../app/windows'
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type RefObject,
} from 'react'
import { CopyMinus, FilePlus, FolderOpen, FolderPlus } from 'lucide-react'
import { executeCommand, primaryKeybinding, getCommand } from '../commands/registry'
import type { DirectoryEntry } from '../ipc/workspace'
import { openContextMenu, type MenuEntry } from '../components/contextMenuState'
import { elementAt, startDrag, useDropTarget, type Drag } from '../components/drag'
import { isMac } from '../editor/keymap'
import { documentsStore, openPath } from '../state/documents'
import { iconThemeStore, type LoadedIconTheme } from '../state/iconTheme'
import { contributedMenuItems, runContributed } from '../state/extensions'
import { rowMark, useFileMarks } from '../state/fileMarks'
import { layoutStore } from '../state/layout'
import { reportError } from '../state/notifications'
import { fileProblemsStore, problemsByPath, type PathProblems } from '../state/problems'
import { useStore } from '../state/store'
import { themeKind } from '../themes/theme'
import {
  createEntry,
  isWithin,
  openFolder,
  parentOf,
  toggleFolder,
  workspaceStore,
  type Listing,
} from '../state/workspace'
import {
  EXPLORER_TREE_ID,
  beginCreate,
  beginRename,
  canMoveInto,
  canPaste,
  cancelCreate,
  cancelRename,
  copyDroppedInto,
  copyEntry,
  copyPath,
  cutEntry,
  deleteEntry,
  explorerStore,
  moveOrCopy,
  paste,
  rename,
  revealInSystem,
  revealLabel,
  setFocused,
  type PendingCreate,
} from './explorerState'
import { FileIcon } from '../components/FileIcon'
import { entryIcon } from '../components/fileIcons'
import './ExplorerPanel.css'

const createEntries = (target: string): MenuEntry[] => [
  { label: 'New File…', run: () => beginCreate(false, target) },
  { label: 'New Folder…', run: () => beginCreate(true, target) },
]

/** The keys the tree handles, as menus show them. */
function keyHints() {
  const mac = isMac()
  return {
    cut: mac ? '⌘X' : 'Ctrl+X',
    copy: mac ? '⌘C' : 'Ctrl+C',
    paste: mac ? '⌘V' : 'Ctrl+V',
    copyPath: mac ? '⌥⌘C' : 'Shift+Alt+C',
    copyRelativePath: mac ? '⌥⇧⌘C' : 'Ctrl+Shift+Alt+C',
    reveal: mac ? '⌥⌘R' : 'Shift+Alt+R',
    rename: mac ? '↩' : 'F2',
    delete: mac ? '⌘⌫' : 'Delete',
  }
}

/**
 * The context menu of a row: opening, creating, cut, copy, and paste,
 * copying its path, renaming, deleting, and extensions' items, which get
 * its path.
 */
function openRowMenu(
  x: number,
  y: number,
  entry: DirectoryEntry,
  focusAfterDelete: string | null,
): void {
  const keys = keyHints()
  const target = { path: entry.path, directory: entry.directory }
  openContextMenu(x, y, [
    ...(entry.directory ? [] : [{ label: 'Open', run: () => void openPath(entry.path) }, null]),
    ...createEntries(entry.path),
    { label: revealLabel(), hint: keys.reveal, run: () => revealInSystem(entry.path) },
    null,
    { label: 'Cut', hint: keys.cut, run: () => cutEntry(entry.path) },
    { label: 'Copy', hint: keys.copy, run: () => copyEntry(entry.path) },
    { label: 'Paste', hint: keys.paste, disabled: !canPaste(), run: () => paste(entry.path) },
    null,
    { label: 'Copy Path', hint: keys.copyPath, run: () => copyPath(entry.path, false) },
    {
      label: 'Copy Relative Path',
      hint: keys.copyRelativePath,
      run: () => copyPath(entry.path, true),
    },
    null,
    { label: 'Rename…', hint: keys.rename, run: () => beginRename(entry.path) },
    {
      label: 'Delete',
      hint: keys.delete,
      run: () => deleteEntry(entry.path, false, focusAfterDelete),
    },
    null,
    ...contributedMenuItems('explorer/context', target).map(item => ({
      label: item.title,
      run: () => runContributed(item.command, entry.path),
    })),
  ])
}

/** The menu of the space below the rows, for the open folder itself. */
function openFolderMenu(x: number, y: number, root: string): void {
  const keys = keyHints()
  openContextMenu(x, y, [
    ...createEntries(root),
    { label: revealLabel(), hint: keys.reveal, run: () => revealInSystem(root) },
    null,
    { label: 'Paste', hint: keys.paste, disabled: !canPaste(), run: () => paste(root) },
    null,
    ...contributedMenuItems('explorer/context', { path: root, directory: true }).map(item => ({
      label: item.title,
      run: () => runContributed(item.command, root),
    })),
  ])
}

/**
 * The folder a drag over the tree is over: a folder's row, the folder holding
 * a file's row, or `root` between rows. `closed` is a closed folder's row,
 * which opens after a moment.
 */
function folderAt(drag: Drag, root: string): { folder: string; closed: string | null } {
  const row = elementAt(drag)?.closest<HTMLElement>('[data-drop-folder]')
  return row
    ? { folder: row.dataset.dropFolder!, closed: row.dataset.closedFolder ?? null }
    : { folder: root, closed: null }
}

/** How long a dragged item hovers over a closed folder before it opens. */
const OPEN_ON_HOVER_MS = 600

interface Row {
  entry: DirectoryEntry
  depth: number
  expanded: boolean
  /**
   * Ignored by Git: matched by an ignore rule, or inside a folder that is
   * (Git doesn't look inside an ignored folder, so nothing in it is
   * committed, whatever rules match it).
   */
  ignored: boolean
}

/** Folders and files of the open workspace, loaded as folders expand. */
export function ExplorerPanel() {
  const { info, listings } = useStore(workspaceStore)
  const { expanded } = useStore(layoutStore)
  const { documents, activeId } = useStore(documentsStore)
  const { focused, creating, renaming, clipboard, revealed } = useStore(explorerStore)
  const iconTheme = useStore(iconThemeStore)
  const light = useStore(themeKind) === 'light'
  const fileProblems = useStore(fileProblemsStore)
  const problems = useMemo(() => problemsByPath(fileProblems), [fileProblems])
  const marks = useFileMarks()
  const treeRef = useRef<HTMLUListElement>(null)
  const activePath = documents.find(document => document.id === activeId)?.path ?? null
  /** The folder a drag would drop into. */
  const [dropTarget, setDropTarget] = useState<string | null>(null)
  const hoverTimer = useRef<{ path: string; timer: ReturnType<typeof setTimeout> } | null>(null)
  useEffect(() => () => clearTimeout(hoverTimer.current?.timer), [])

  const endDrag = () => {
    setDropTarget(null)
    clearTimeout(hoverTimer.current?.timer)
    hoverTimer.current = null
  }
  /**
   * Takes a drag over a folder: files and folders from the tree (moving
   * needs a new place; copying can go anywhere but into itself), and files
   * from other programs, which are copied. A closed folder's row opens after
   * a moment.
   */
  const dropHandlers = (fromTree: boolean) => ({
    over: (drag: Drag) => {
      const root = info?.root
      if (!root) {
        return false
      }
      const { folder, closed } = fromTree ? folderAt(drag, root) : { folder: root, closed: null }
      const { payload } = drag
      const takes =
        payload.kind === 'files' ||
        (payload.kind === 'entries' &&
          (drag.copy
            ? !payload.paths.some(path => isWithin(folder, path) && path !== folder)
            : canMoveInto(payload.paths, folder)))
      if (!takes) {
        endDrag()
        return false
      }
      setDropTarget(folder)
      if (hoverTimer.current?.path !== closed) {
        clearTimeout(hoverTimer.current?.timer)
        hoverTimer.current = closed
          ? { path: closed, timer: setTimeout(() => toggleFolder(closed), OPEN_ON_HOVER_MS) }
          : null
      }
      return true
    },
    drop: (drag: Drag) => {
      const root = info?.root
      endDrag()
      if (!root) {
        return
      }
      const { folder } = fromTree ? folderAt(drag, root) : { folder: root }
      const { payload } = drag
      if (payload.kind === 'entries') {
        if (drag.copy || canMoveInto(payload.paths, folder)) {
          void moveOrCopy(payload.paths, folder, drag.copy)
        }
      } else if (payload.kind === 'files' && payload.dropId !== null) {
        void copyDroppedInto(payload.dropId, folder)
      }
    },
    leave: endDrag,
  })
  const treeDropRef = useDropTarget<HTMLUListElement>(dropHandlers(true))
  const blankDropRef = useDropTarget<HTMLDivElement>(dropHandlers(false))

  const rows = useMemo(
    () => (info ? flatten(info.root, listings, new Set(expanded), 0, false) : []),
    [info, listings, expanded],
  )

  // Scroll a revealed row into view once it is listed, once per reveal.
  const scrolledSeq = useRef(0)
  useEffect(() => {
    if (revealed && revealed.seq !== scrolledSeq.current) {
      const row = query(`#${CSS.escape(rowId(revealed.path))}`)
      if (row) {
        scrolledSeq.current = revealed.seq
        row.scrollIntoView({ block: 'nearest' })
      }
    }
  }, [revealed, rows])

  if (!info) {
    return (
      <div className="panel-empty">
        <p>No folder open.</p>
        <button type="button" className="panel-button" onClick={() => void openFolder()}>
          Open Folder…
        </button>
      </div>
    )
  }
  const rootListing = listings.get(info.root)
  if (rootListing && 'error' in rootListing) {
    return (
      <p className="panel-empty">
        Could not read {info.name}: {rootListing.error}
      </p>
    )
  }

  const focusIndex = Math.max(
    0,
    rows.findIndex(row => row.entry.path === focused),
  )
  const activate = (row: Row) => {
    setFocused(row.entry.path)
    if (row.entry.directory) {
      toggleFolder(row.entry.path)
    } else {
      void openPath(row.entry.path)
    }
  }

  // The name field goes first among the new entry's siblings.
  let inputIndex = -1
  let inputDepth = 0
  if (creating) {
    const parentIndex = rows.findIndex(row => row.entry.path === creating.parent)
    inputIndex = parentIndex + 1
    inputDepth = parentIndex >= 0 ? (rows[parentIndex]?.depth ?? 0) + 1 : 0
  }
  const nameInput = creating && (
    <NameInput
      key={`${creating.parent}:${creating.directory}`}
      pending={creating}
      depth={inputDepth}
      siblings={listings.get(creating.parent)}
      iconTheme={iconTheme}
      light={light}
      treeRef={treeRef}
    />
  )

  const onKeyDown = (event: KeyboardEvent<HTMLUListElement>) => {
    const row = rows[focusIndex]
    if (!row || event.target !== event.currentTarget) {
      return
    }
    if (onFileKey(event, row.entry, info.root, rowAfterDeleting(rows, focusIndex))) {
      event.preventDefault()
      return
    }
    const move = (index: number) => {
      const target = rows[Math.max(0, Math.min(rows.length - 1, index))]
      if (target) {
        setFocused(target.entry.path)
        event.currentTarget.ownerDocument
          .getElementById(rowId(target.entry.path))
          ?.scrollIntoView({ block: 'nearest' })
      }
    }
    switch (event.key) {
      case 'ArrowDown':
        move(focusIndex + 1)
        break
      case 'ArrowUp':
        move(focusIndex - 1)
        break
      case 'Home':
        move(0)
        break
      case 'End':
        move(rows.length - 1)
        break
      case 'ArrowRight':
        if (row.entry.directory && !row.expanded) {
          toggleFolder(row.entry.path)
        } else {
          move(focusIndex + 1)
        }
        break
      case 'ArrowLeft':
        if (row.entry.directory && row.expanded) {
          toggleFolder(row.entry.path)
        } else {
          const parent = rows
            .slice(0, focusIndex)
            .reverse()
            .find(candidate => candidate.depth < row.depth)
          if (parent) {
            move(rows.indexOf(parent))
          }
        }
        break
      case 'Enter':
        // As in Finder, Return renames on macOS.
        if (isMac()) {
          beginRename(row.entry.path)
        } else {
          activate(row)
        }
        break
      case ' ':
        activate(row)
        break
      default:
        return
    }
    event.preventDefault()
  }

  return (
    <div className="explorer">
      <ul
        ref={element => {
          treeRef.current = element
          treeDropRef(element)
        }}
        id={EXPLORER_TREE_ID}
        className="explorer-tree"
        data-drop-target={dropTarget === info.root || undefined}
        role="tree"
        aria-label={info.name}
        tabIndex={0}
        aria-activedescendant={rows[focusIndex] ? rowId(rows[focusIndex].entry.path) : undefined}
        onKeyDown={onKeyDown}
        onContextMenu={event => {
          // From the keyboard (Shift+F10, the menu key): the focused row's menu.
          const row = rows[focusIndex]
          if (event.target !== event.currentTarget || !row) {
            return
          }
          event.preventDefault()
          const rect = event.currentTarget.ownerDocument
            .getElementById(rowId(row.entry.path))
            ?.getBoundingClientRect()
          if (rect) {
            const after = rowAfterDeleting(rows, focusIndex)
            openRowMenu(rect.left + rect.width / 3, rect.bottom, row.entry, after)
          }
        }}
      >
        {inputIndex === 0 && nameInput}
        {rows.map((row, index) => {
          const folder = row.entry.directory ? { expanded: row.expanded } : null
          const icon = entryIcon(row.entry.name, folder, iconTheme, light)
          const counts = problems.get(row.entry.path)
          const mark = rowMark(marks.index, row.entry.path, row.entry.directory, marks.color)
          const described =
            [counts && describeProblems(counts, row.entry.directory), mark?.tooltip]
              .filter(Boolean)
              .join('. ') || undefined
          // A folder's problem dot says enough; a file shows both counts and badges.
          const badge = counts && row.entry.directory ? null : mark?.badge
          const isRenaming = renaming === row.entry.path
          const folderPath = row.entry.directory ? row.entry.path : parentOf(row.entry.path)
          return [
            <li
              key={row.entry.path}
              id={rowId(row.entry.path)}
              className={`explorer-tree__row${isRenaming ? ' explorer-tree__row--input' : ''}`}
              role="treeitem"
              data-cut={(clipboard?.cut && clipboard.paths.includes(row.entry.path)) || undefined}
              data-drop-target={
                (dropTarget !== null &&
                  dropTarget !== info.root &&
                  isWithin(row.entry.path, dropTarget)) ||
                undefined
              }
              data-drop-folder={folderPath}
              data-closed-folder={row.entry.directory && !row.expanded ? row.entry.path : undefined}
              onPointerDown={event => {
                if (!isRenaming) {
                  startDrag(event, () => ({
                    kind: 'entries',
                    paths: [row.entry.path],
                    label: row.entry.name,
                  }))
                }
              }}
              aria-level={row.depth + 1}
              aria-expanded={row.entry.directory ? row.expanded : undefined}
              aria-selected={row.entry.path === activePath}
              data-focused={index === focusIndex || undefined}
              data-ignored={row.ignored || mark?.faded || undefined}
              data-problems={counts ? (counts.errors > 0 ? 'error' : 'warning') : undefined}
              data-marked={mark?.color ? true : undefined}
              aria-description={described}
              style={
                {
                  ...indent(row.depth),
                  // A variable, so a problem's color still wins (ExplorerPanel.css).
                  ...(mark?.color && { '--explorer-mark-color': mark.color }),
                } as CSSProperties
              }
              title={described ? `${row.entry.path}\n${described}` : row.entry.path}
              onClick={() => !isRenaming && activate(row)}
              onContextMenu={event => {
                event.preventDefault()
                setFocused(row.entry.path)
                const after = rowAfterDeleting(rows, index)
                openRowMenu(event.clientX, event.clientY, row.entry, after)
              }}
            >
              <span className="explorer-tree__chevron" aria-hidden="true">
                {row.entry.directory ? (row.expanded ? '▾' : '▸') : ''}
              </span>
              <FileIcon
                icon={icon}
                kind={folder ? 'folder' : 'file'}
                className="explorer-tree__icon"
              />
              {isRenaming ? (
                <RenameInput
                  entry={row.entry}
                  siblings={listings.get(parentOf(row.entry.path))}
                  treeRef={treeRef}
                />
              ) : (
                <span className="explorer-tree__name">{row.entry.name}</span>
              )}
              {(counts || badge) && !isRenaming && (
                <span className="explorer-tree__badges" aria-hidden="true">
                  {counts && (
                    // A file's count; a folder's dot says something inside has problems.
                    <span className="explorer-tree__problems">
                      {row.entry.directory ? '●' : counts.errors + counts.warnings}
                    </span>
                  )}
                  {badge && <span className="explorer-tree__mark">{badge}</span>}
                </span>
              )}
            </li>,
            index + 1 === inputIndex && nameInput,
          ]
        })}
        {rows.length === 0 && !creating && (
          <li className="panel-empty" role="none">
            {rootListing ? 'This folder is empty.' : 'Loading…'}
          </li>
        )}
      </ul>
      {/* The space below the rows: its menu creates at the top level. The
          header actions and commands do the same from the keyboard. */}
      <div
        ref={blankDropRef}
        className="explorer__blank"
        aria-hidden="true"
        onContextMenu={event => {
          event.preventDefault()
          openFolderMenu(event.clientX, event.clientY, info.root)
        }}
      />
    </div>
  )
}

interface NameInputProps {
  pending: PendingCreate
  depth: number
  /** The parent's listing, to warn about a name that is taken. */
  siblings: Listing | undefined
  treeRef: RefObject<HTMLUListElement | null>
  iconTheme: LoadedIconTheme | null
  light: boolean
}

/**
 * The field for naming a new file or folder. Enter creates it, keeping the
 * field open if that fails; Escape cancels. Leaving the field creates it if
 * a name was typed.
 */
function NameInput({ pending, depth, siblings, treeRef, iconTheme, light }: NameInputProps) {
  const [name, setName] = useState('')
  const [error, setError] = useState<string | null>(null)
  // Set once the field has been submitted or cancelled, so a blur as it
  // unmounts does nothing.
  const done = useRef(false)
  const kind = pending.directory ? 'folder' : 'file'

  const finish = (path: string | null) => {
    done.current = true
    explorerStore.update(state => ({
      ...state,
      creating: null,
      focused: path ?? state.focused,
    }))
    treeRef.current?.focus()
    if (path && !pending.directory) {
      void openPath(path)
    }
  }

  const taken = takenName(name, siblings)
  const message = error ?? (taken ? `A file or folder named ${taken} already exists here.` : null)

  const submit = async (fromBlur: boolean) => {
    if (done.current) {
      return
    }
    done.current = true
    // Leaving an empty field, or one already showing a problem, cancels.
    if (!name.trim() || (fromBlur && message)) {
      cancelCreate()
      return
    }
    try {
      finish(await createEntry(pending.parent, name, pending.directory))
    } catch (failure) {
      if (fromBlur) {
        cancelCreate()
        reportError(`Creating the ${kind}`, failure)
      } else {
        done.current = false
        setError(failure instanceof Error ? failure.message : String(failure))
      }
    }
  }
  const folder = pending.directory ? { expanded: false } : null
  const icon = entryIcon(name.trim(), folder, iconTheme, light)
  return (
    <li
      className="explorer-tree__row explorer-tree__row--input"
      role="treeitem"
      aria-level={depth + 1}
      aria-selected={false}
      aria-label={`New ${kind}`}
      style={indent(depth)}
    >
      <span className="explorer-tree__chevron" aria-hidden="true" />
      <FileIcon icon={icon} kind={kind} className="explorer-tree__icon" />
      <input
        className="explorer-tree__input"
        aria-label={`${pending.directory ? 'Folder' : 'File'} name`}
        aria-invalid={message ? true : undefined}
        aria-describedby={message ? 'explorer-name-message' : undefined}
        autoFocus
        spellCheck={false}
        autoComplete="off"
        value={name}
        onChange={event => {
          setName(event.target.value)
          setError(null)
        }}
        onKeyDown={event => {
          // Keep the tree's navigation keys away from the field.
          event.stopPropagation()
          if (event.key === 'Enter') {
            event.preventDefault()
            void submit(false)
          } else if (event.key === 'Escape') {
            event.preventDefault()
            finish(null)
          }
        }}
        onBlur={() => void submit(true)}
      />
      {message && (
        <div id="explorer-name-message" className="explorer-tree__message" role="alert">
          {message}
        </div>
      )}
    </li>
  )
}

/**
 * Handles the tree's keys for the focused file or folder: rename, delete,
 * cut, copy, paste, copy path, and reveal. Returns whether it did.
 */
function onFileKey(
  event: KeyboardEvent,
  entry: DirectoryEntry,
  root: string,
  focusAfterDelete: string | null,
): boolean {
  const mac = isMac()
  const mod = mac ? event.metaKey : event.ctrlKey
  const key = event.key.toLowerCase()
  const only = (wanted: { mod?: boolean; shift?: boolean; alt?: boolean }) =>
    mod === Boolean(wanted.mod) &&
    event.shiftKey === Boolean(wanted.shift) &&
    event.altKey === Boolean(wanted.alt)
  // Shortcuts with Alt give other characters on some layouts; use the key's code.
  const code = event.code.replace(/^Key/, '').toLowerCase()
  if (!mac && key === 'f2' && only({})) {
    beginRename(entry.path)
  } else if (
    (!mac && key === 'delete' && (only({}) || only({ shift: true }))) ||
    (mac && key === 'backspace' && (only({ mod: true }) || only({ mod: true, alt: true })))
  ) {
    void deleteEntry(entry.path, mac ? event.altKey : event.shiftKey, focusAfterDelete)
  } else if (only({ mod: true }) && key === 'x') {
    cutEntry(entry.path)
  } else if (only({ mod: true }) && key === 'c') {
    copyEntry(entry.path)
  } else if (only({ mod: true }) && key === 'v') {
    void paste(entry.path)
  } else if (
    code === 'c' &&
    (mac ? only({ mod: true, alt: true }) : only({ shift: true, alt: true }))
  ) {
    void copyPath(entry.path, false)
  } else if (
    code === 'c' &&
    (mac
      ? only({ mod: true, alt: true, shift: true })
      : only({ mod: true, shift: true, alt: true }))
  ) {
    void copyPath(entry.path, true)
  } else if (
    code === 'r' &&
    (mac ? only({ mod: true, alt: true }) : only({ shift: true, alt: true }))
  ) {
    void revealInSystem(entry.path === root ? root : entry.path)
  } else {
    return false
  }
  return true
}

/**
 * The field for renaming a file or folder, in place of its name. The name
 * without its extension is selected at first. Enter renames it, keeping
 * the field open if that fails; Escape cancels, as does leaving the field
 * unchanged or with a problem.
 */
function RenameInput({
  entry,
  siblings,
  treeRef,
}: {
  entry: DirectoryEntry
  siblings: Listing | undefined
  treeRef: RefObject<HTMLUListElement | null>
}) {
  const [name, setName] = useState(entry.name)
  const [error, setError] = useState<string | null>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const done = useRef(false)
  useEffect(() => {
    const input = inputRef.current
    if (input) {
      input.focus()
      const dot = entry.directory ? -1 : entry.name.lastIndexOf('.')
      input.setSelectionRange(0, dot > 0 ? dot : entry.name.length)
    }
  }, [entry])

  // Renaming to the same name in another case is allowed.
  const taken =
    name.trim().toLowerCase() === entry.name.toLowerCase() ? null : takenName(name, siblings)
  const message = error ?? (taken ? `A file or folder named ${taken} already exists here.` : null)

  const finish = () => {
    done.current = true
    cancelRename()
    treeRef.current?.focus()
  }
  const submit = async (fromBlur: boolean) => {
    if (done.current) {
      return
    }
    if (!name.trim() || name.trim() === entry.name || (fromBlur && message)) {
      finish()
      return
    }
    done.current = true
    try {
      await rename(entry.path, name)
      treeRef.current?.focus()
    } catch (failure) {
      if (fromBlur) {
        cancelRename()
        reportError('Renaming', failure)
      } else {
        done.current = false
        setError(failure instanceof Error ? failure.message : String(failure))
      }
    }
  }
  return (
    <>
      <input
        ref={inputRef}
        className="explorer-tree__input"
        aria-label={`Rename ${entry.name}`}
        aria-invalid={message ? true : undefined}
        aria-describedby={message ? 'explorer-rename-message' : undefined}
        spellCheck={false}
        autoComplete="off"
        value={name}
        onClick={event => event.stopPropagation()}
        onChange={event => {
          setName(event.target.value)
          setError(null)
        }}
        onKeyDown={event => {
          // Keep the tree's keys away from the field.
          event.stopPropagation()
          if (event.key === 'Enter') {
            event.preventDefault()
            void submit(false)
          } else if (event.key === 'Escape') {
            event.preventDefault()
            finish()
          }
        }}
        onBlur={() => void submit(true)}
      />
      {message && (
        <div id="explorer-rename-message" className="explorer-tree__message" role="alert">
          {message}
        </div>
      )}
    </>
  )
}

/** The existing sibling that `name` would clash with, if it names one directly. */
function takenName(name: string, siblings: Listing | undefined): string | null {
  const trimmed = name.trim().toLowerCase()
  if (!trimmed || !siblings || 'error' in siblings || /[/\\]/.test(trimmed)) {
    return null
  }
  return siblings.find(entry => entry.name.toLowerCase() === trimmed)?.name ?? null
}

/** Explorer actions shown in the side bar header. */
export function ExplorerActions() {
  const { info } = useStore(workspaceStore)
  const collapse = getCommand('explorer.collapseAll')
  const open = getCommand('file.openFolder')
  return (
    <>
      {info && (
        <>
          <button
            type="button"
            className="dock-action"
            title="New File…"
            aria-label="New File"
            onClick={() => beginCreate(false)}
          >
            <FilePlus size={16} strokeWidth={1.5} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="dock-action"
            title="New Folder…"
            aria-label="New Folder"
            onClick={() => beginCreate(true)}
          >
            <FolderPlus size={16} strokeWidth={1.5} aria-hidden="true" />
          </button>
        </>
      )}
      <button
        type="button"
        className="dock-action"
        title={`Open Folder…${primaryKeybinding(open) ? ` (${primaryKeybinding(open)})` : ''}`}
        aria-label="Open Folder"
        onClick={() => void executeCommand('file.openFolder')}
      >
        <FolderOpen size={16} strokeWidth={1.5} aria-hidden="true" />
      </button>
      {info && collapse && (
        <button
          type="button"
          className="dock-action"
          title="Collapse Folders"
          aria-label="Collapse Folders"
          onClick={() => void executeCommand('explorer.collapseAll')}
        >
          <CopyMinus size={16} strokeWidth={1.5} aria-hidden="true" />
        </button>
      )}
    </>
  )
}

/** The row to focus after deleting `rows[index]`: the next one outside it, or the one before. */
function rowAfterDeleting(rows: readonly Row[], index: number): string | null {
  const deleted = rows[index]?.entry.path
  if (deleted === undefined) {
    return null
  }
  const next = rows.slice(index + 1).find(row => !isWithin(row.entry.path, deleted))
  return (next ?? rows[index - 1])?.entry.path ?? null
}

function flatten(
  directory: string,
  listings: ReadonlyMap<string, Listing>,
  expanded: ReadonlySet<string>,
  depth: number,
  inIgnored: boolean,
): Row[] {
  const listing = listings.get(directory)
  if (!listing || 'error' in listing) {
    return []
  }
  return listing.flatMap(entry => {
    const isOpen = entry.directory && expanded.has(entry.path)
    const ignored = inIgnored || entry.ignored
    const row = { entry, depth, expanded: isOpen, ignored }
    return isOpen ? [row, ...flatten(entry.path, listings, expanded, depth + 1, ignored)] : [row]
  })
}

const plural = (count: number, noun: string) => `${count} ${noun}${count === 1 ? '' : 's'}`

/** `2 errors, 1 warning`, or for a folder, `Contains 2 errors, 1 warning`. */
function describeProblems({ errors, warnings }: PathProblems, folder: boolean): string {
  const counts = [errors && plural(errors, 'error'), warnings && plural(warnings, 'warning')]
    .filter(Boolean)
    .join(', ')
  return folder ? `Contains ${counts}` : counts
}

function indent(depth: number) {
  return { paddingLeft: `calc(${depth} * var(--space-3) + var(--space-2))` }
}

function rowId(path: string): string {
  return `explorer-${encodeURIComponent(path)}`
}
