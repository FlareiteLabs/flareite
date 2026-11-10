import { RotateCcw, X } from 'lucide-react'
import { useEffect, useId, useMemo, useState, type KeyboardEvent } from 'react'
import { executeCommand } from '../commands/registry'
import type { FolderSettingsInfo, Setting, SettingValue, SettingsScope } from '../ipc/workspace'
import {
  changeSetting,
  refreshSettings,
  settingsQuery,
  settingsSchemaStore,
  settingsScope,
  showSettingsScope,
} from '../state/settings'
import { useStore } from '../state/store'
import './SettingsPage.css'

/** A setting's place in `settings.toml`, as written there: `editor.tab_size`. */
export function settingKey(setting: Setting): string {
  return setting.path.map(key => (/^[\w-]+$/.test(key) ? key : JSON.stringify(key))).join('.')
}

/**
 * Whether `setting` matches every word of `query`. `@ext:<id>` keeps an
 * extension's settings, and `@modified` those `settings.toml` sets.
 */
export function matchesQuery(setting: Setting, query: string): boolean {
  const text = [setting.section, setting.title, settingKey(setting), setting.description]
    .join(' ')
    .toLowerCase()
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every(word => {
      if (word.startsWith('@ext:')) {
        return setting.extension === word.slice(5)
      }
      if (word === '@modified') {
        return setting.modified
      }
      return text.includes(word)
    })
}

interface Section {
  title: string
  extension: string | null
  settings: Setting[]
}

/** Settings grouped by section, in order. */
function sectionsOf(settings: readonly Setting[]): Section[] {
  const sections: Section[] = []
  for (const setting of settings) {
    const last = sections.at(-1)
    if (last && last.title === setting.section && last.extension === setting.extension) {
      last.settings.push(setting)
    } else {
      sections.push({ title: setting.section, extension: setting.extension, settings: [setting] })
    }
  }
  return sections
}

const openFile = (scope: SettingsScope) =>
  void executeCommand(
    scope === 'workspace'
      ? 'preferences.openWorkspaceSettingsFile'
      : 'preferences.openSettingsFile',
  )

/** What the Workspace tab says about the folder's settings, above them. */
function FolderNotes({ folder }: { folder: FolderSettingsInfo }) {
  return (
    <>
      {folder.error !== null && (
        <div className="settings__error" role="alert">
          <p>
            This folder&apos;s settings file has a problem, so none of its settings apply. Fix it to
            change them.
          </p>
          <pre className="settings__error-detail">{folder.error.trim()}</pre>
        </div>
      )}
      {!folder.trusted && (
        <div className="settings__note" role="status">
          <p>You haven&apos;t trusted this folder, so its settings don&apos;t apply yet.</p>
          <button
            type="button"
            className="settings__button"
            onClick={() => void executeCommand('workspace.manageTrust')}
          >
            Manage Trust…
          </button>
        </div>
      )}
      {!folder.exists && (
        <p className="settings__note" role="status">
          {folder.name} has no settings of its own. Changing one below creates
          .flareite/settings.toml in it; the rest stay yours.
        </p>
      )}
      {folder.ignored.length > 0 && (
        <p className="settings__note" role="status">
          Only your own settings can change {folder.ignored.join(', ')}; this folder&apos;s values
          are ignored.
        </p>
      )}
    </>
  )
}

/**
 * The settings editor: every setting, built in or declared by an extension,
 * with its value, and a control to change it in `settings.toml`. Settings
 * that are tables of entries are edited in the file.
 */
export function SettingsPage() {
  const schema = useStore(settingsSchemaStore)
  const query = useStore(settingsQuery)
  const scope = useStore(settingsScope)
  const idPrefix = useId()
  const folder = schema?.folder ?? null
  const inFolder = scope === 'workspace' && folder !== null

  useEffect(() => {
    void refreshSettings()
  }, [])

  const shown = useMemo(
    () => (schema?.settings ?? []).filter(setting => matchesQuery(setting, query)),
    [schema, query],
  )
  const sections = useMemo(() => sectionsOf(shown), [shown])
  const sectionId = (index: number) => `${idPrefix}-section-${index}`
  const firstExtension = sections.findIndex(section => section.extension !== null)
  // Changes can't be written until the file is fixed.
  const readOnly = inFolder ? folder.error !== null : schema?.error != null

  return (
    <div className="settings">
      <div className="settings__header">
        {folder !== null && (
          <div className="settings__scopes" role="tablist" aria-label="Whose settings">
            <button
              type="button"
              role="tab"
              className="settings__scope"
              aria-selected={!inFolder}
              onClick={() => showSettingsScope('user')}
            >
              User
            </button>
            <button
              type="button"
              role="tab"
              className="settings__scope"
              aria-selected={inFolder}
              title={folder.file}
              onClick={() => showSettingsScope('workspace')}
            >
              Workspace
              <span className="settings__scope-name">{folder.name}</span>
            </button>
          </div>
        )}
        <input
          type="search"
          className="settings__search"
          aria-label="Search settings"
          placeholder="Search settings"
          spellCheck={false}
          value={query}
          onChange={event => settingsQuery.set(event.currentTarget.value)}
        />
        <button
          type="button"
          className="settings__button"
          onClick={() => openFile(inFolder ? 'workspace' : 'user')}
        >
          {inFolder ? 'Open Folder Settings (TOML)' : 'Open settings.toml'}
        </button>
      </div>
      {inFolder && <FolderNotes folder={folder} />}
      {readOnly && !inFolder && (
        <div className="settings__error" role="alert">
          <p>
            settings.toml has a problem, so the settings below are the last ones that applied. Fix
            it to change them.
          </p>
          {/* The parser's message points at the mistake with a drawing. */}
          <pre className="settings__error-detail">{schema?.error?.trim()}</pre>
        </div>
      )}
      {schema === null ? (
        <p className="panel-empty">Loading settings…</p>
      ) : (
        <div className="settings__body">
          {sections.length > 1 && (
            <nav className="settings__nav" aria-label="Settings sections">
              <ul>
                {sections.map((section, index) => (
                  <li key={sectionId(index)}>
                    {index === firstExtension && (
                      <span className="settings__nav-heading">Extensions</span>
                    )}
                    <button
                      type="button"
                      className="settings__nav-item"
                      onClick={() =>
                        document
                          .getElementById(sectionId(index))
                          ?.scrollIntoView({ block: 'start' })
                      }
                    >
                      {section.title}
                    </button>
                  </li>
                ))}
              </ul>
            </nav>
          )}
          <div className="settings__list">
            {sections.length === 0 && (
              <p className="panel-empty" role="status">
                No settings match “{query.trim()}”.
              </p>
            )}
            {sections.map((section, index) => (
              <section
                key={sectionId(index)}
                id={sectionId(index)}
                className="settings__section"
                aria-labelledby={`${sectionId(index)}-title`}
              >
                <h2 id={`${sectionId(index)}-title`} className="settings__section-title">
                  {section.title}
                  {section.extension && (
                    <span className="settings__section-id">{section.extension}</span>
                  )}
                </h2>
                {section.settings.map(setting => (
                  <SettingRow
                    key={settingKey(setting)}
                    setting={setting}
                    disabled={readOnly || (inFolder && !setting.folder)}
                    inFolder={inFolder}
                  />
                ))}
              </section>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

/** A value as shown to the user, e.g. in "Reset to default: 4". */
export function formatValue(setting: Setting, value: SettingValue): string {
  if (Array.isArray(value)) {
    return value.length > 0 ? value.join(', ') : 'none'
  }
  if (typeof value === 'boolean') {
    return value ? 'on' : 'off'
  }
  if (setting.type === 'string') {
    const label = setting.choices?.find(choice => choice.value === value)?.label
    return label ?? (value === '' ? 'empty' : `“${value}”`)
  }
  return String(value)
}

interface ControlProps {
  setting: Setting
  disabled: boolean
  /** The ids of the title and description, which label the control. */
  labelledBy: string
  describedBy: string
}

interface RowProps {
  setting: Setting
  disabled: boolean
  /** On the Workspace tab: "modified" means the folder's file sets it. */
  inFolder?: boolean
}

function SettingRow({ setting, disabled, inFolder = false }: RowProps) {
  const id = useId()
  const titleId = `${id}-title`
  const descriptionId = `${id}-description`
  const props: ControlProps = { setting, disabled, labelledBy: titleId, describedBy: descriptionId }
  const resettable = setting.modified && setting.type !== 'table'

  return (
    <div className="setting" data-modified={setting.modified || undefined}>
      <div className="setting__header">
        <span id={titleId} className="setting__title">
          {setting.title}
        </span>
        {setting.modified && (
          <span className="setting__badge">{inFolder ? 'Set in Folder' : 'Modified'}</span>
        )}
        {inFolder && !setting.folder && <span className="setting__badge">User Only</span>}
        {resettable && (
          <button
            type="button"
            className="setting__reset"
            title={`Reset to default: ${formatValue(setting, setting.default)}`}
            aria-label={`Reset ${setting.title} to default`}
            disabled={disabled}
            onClick={() => void changeSetting(setting.path, null)}
          >
            <RotateCcw size={14} strokeWidth={1.5} aria-hidden="true" />
          </button>
        )}
      </div>
      <code className="setting__key">{settingKey(setting)}</code>
      {setting.type === 'boolean' ? (
        <BooleanControl {...props} />
      ) : (
        <>
          <p id={descriptionId} className="setting__description">
            {setting.description}
          </p>
          <Control {...props} />
        </>
      )}
      {setting.problem &&
        (inFolder && !setting.folder ? (
          <p className="setting__problem">{setting.problem}</p>
        ) : (
          <p className="setting__problem">
            The value in {inFolder ? 'the settings' : 'settings.toml'}{' '}
            {setting.problem.replace(/; using the default$/, '')}, so the default is used.
          </p>
        ))}
    </div>
  )
}

function Control(props: ControlProps) {
  switch (props.setting.type) {
    case 'integer':
    case 'number':
      return <NumberControl {...props} />
    case 'string':
      return props.setting.choices ? <ChoiceControl {...props} /> : <TextControl {...props} />
    case 'list':
      return <ListControl {...props} />
    default:
      return <TableControl {...props} />
  }
}

function BooleanControl({ setting, disabled, labelledBy, describedBy }: ControlProps) {
  const id = useId()
  return (
    <div className="setting__checkbox">
      <input
        id={id}
        type="checkbox"
        aria-labelledby={labelledBy}
        aria-describedby={describedBy}
        checked={setting.value === true}
        disabled={disabled}
        onChange={event => void changeSetting(setting.path, event.currentTarget.checked)}
      />
      <label id={describedBy} htmlFor={id} className="setting__description">
        {setting.description}
      </label>
    </div>
  )
}

/** The value in effect for the setting at `path`, as last loaded. */
function latestValue(path: readonly string[]): SettingValue | undefined {
  return settingsSchemaStore
    .get()
    ?.settings.find(setting => setting.path.join('\0') === path.join('\0'))?.value
}

/**
 * A text box's draft: what's typed, until it's committed (Enter, or leaving
 * the box) or abandoned (Escape). After committing, it shows the value in
 * effect, which is the old one if the change was refused.
 */
function useDraft(setting: Setting) {
  const [draft, setDraft] = useState(String(setting.value))
  useEffect(() => setDraft(String(setting.value)), [setting.value])
  const commit = (value: SettingValue) => {
    void changeSetting(setting.path, value).then(() => {
      setDraft(String(latestValue(setting.path) ?? setting.value))
    })
  }
  const revert = () => setDraft(String(setting.value))
  return { draft, setDraft, commit, revert }
}

const onEditKeys =
  (commit: () => void, revert: () => void) => (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Enter') {
      event.preventDefault()
      commit()
    } else if (event.key === 'Escape') {
      event.preventDefault()
      revert()
    }
  }

/** Why `text` isn't a valid value of a number setting, if it isn't. */
export function numberProblem(setting: Setting, text: string): string | null {
  if (setting.type !== 'integer' && setting.type !== 'number') {
    return null
  }
  const value = Number(text)
  if (text.trim() === '' || !Number.isFinite(value)) {
    return 'Enter a number.'
  }
  if (setting.type === 'integer' && !Number.isInteger(value)) {
    return 'Enter a whole number.'
  }
  if (setting.minimum !== null && value < setting.minimum) {
    return `Must be at least ${setting.minimum}.`
  }
  if (setting.maximum !== null && value > setting.maximum) {
    return `Must be at most ${setting.maximum}.`
  }
  return null
}

function NumberControl({ setting, disabled, labelledBy, describedBy }: ControlProps) {
  const { draft, setDraft, commit, revert } = useDraft(setting)
  const problemId = useId()
  const problem = numberProblem(setting, draft)
  const range = setting.type === 'integer' || setting.type === 'number' ? setting : null
  const save = () => {
    if (!problem && Number(draft) !== setting.value) {
      commit(Number(draft))
    }
  }
  return (
    <>
      <input
        type="number"
        className="setting__input setting__input--number"
        aria-labelledby={labelledBy}
        aria-describedby={problem ? `${describedBy} ${problemId}` : describedBy}
        aria-invalid={problem !== null || undefined}
        min={range?.minimum ?? undefined}
        max={range?.maximum ?? undefined}
        step={setting.type === 'integer' ? 1 : 'any'}
        value={draft}
        disabled={disabled}
        onChange={event => setDraft(event.currentTarget.value)}
        onBlur={save}
        onKeyDown={onEditKeys(save, revert)}
      />
      {problem && (
        <p id={problemId} className="setting__invalid">
          {problem}
        </p>
      )}
    </>
  )
}

function TextControl({ setting, disabled, labelledBy, describedBy }: ControlProps) {
  const { draft, setDraft, commit, revert } = useDraft(setting)
  const save = () => {
    if (draft !== setting.value) {
      commit(draft)
    }
  }
  return (
    <input
      type="text"
      className="setting__input"
      aria-labelledby={labelledBy}
      aria-describedby={describedBy}
      spellCheck={false}
      value={draft}
      disabled={disabled}
      onChange={event => setDraft(event.currentTarget.value)}
      onBlur={save}
      onKeyDown={onEditKeys(save, revert)}
    />
  )
}

function ChoiceControl({ setting, disabled, labelledBy, describedBy }: ControlProps) {
  if (setting.type !== 'string' || !setting.choices) {
    return null
  }
  const value = String(setting.value)
  const known = setting.choices.some(choice => choice.value === value)
  return (
    <select
      className="setting__input setting__select"
      aria-labelledby={labelledBy}
      aria-describedby={describedBy}
      value={value}
      disabled={disabled}
      onChange={event => void changeSetting(setting.path, event.currentTarget.value)}
    >
      {setting.choices.map(choice => (
        <option key={choice.value} value={choice.value}>
          {choice.label}
        </option>
      ))}
      {/* Set in the file to something not installed, e.g. a theme whose extension is off. */}
      {!known && <option value={value}>{value} (not available)</option>}
    </select>
  )
}

function ListControl({ setting, disabled, labelledBy, describedBy }: ControlProps) {
  const [draft, setDraft] = useState('')
  const items = Array.isArray(setting.value) ? setting.value : []
  const add = () => {
    const item = draft.trim()
    if (item && !items.includes(item)) {
      void changeSetting(setting.path, [...items, item])
    }
    setDraft('')
  }
  return (
    <div className="setting__list" role="group" aria-labelledby={labelledBy}>
      {items.length > 0 && (
        <ul className="setting__items">
          {items.map((item, index) => (
            <li key={item} className="setting__item">
              <span className="setting__item-text">{item}</span>
              <button
                type="button"
                className="setting__remove"
                aria-label={`Remove ${item}`}
                title="Remove"
                disabled={disabled}
                onClick={() =>
                  void changeSetting(
                    setting.path,
                    items.filter((_, other) => other !== index),
                  )
                }
              >
                <X size={14} strokeWidth={1.5} aria-hidden="true" />
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="setting__add">
        <input
          type="text"
          className="setting__input"
          aria-label={`Add to ${setting.title}`}
          aria-describedby={describedBy}
          placeholder="Add an item"
          spellCheck={false}
          value={draft}
          disabled={disabled}
          onChange={event => setDraft(event.currentTarget.value)}
          onKeyDown={onEditKeys(add, () => setDraft(''))}
        />
        <button
          type="button"
          className="settings__button"
          disabled={disabled || draft.trim() === ''}
          onClick={add}
        >
          Add
        </button>
      </div>
    </div>
  )
}

function TableControl({ setting, labelledBy }: ControlProps) {
  const names = Array.isArray(setting.value) ? setting.value : []
  return (
    <div className="setting__table">
      <p className="setting__entries">
        {names.length > 0 ? `Set for ${names.join(', ')}.` : 'None set.'}
      </p>
      <button
        type="button"
        className="settings__button"
        aria-describedby={labelledBy}
        onClick={() => openFile(settingsScope.get())}
      >
        Edit in settings.toml
      </button>
    </div>
  )
}
