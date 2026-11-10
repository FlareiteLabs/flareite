import { act, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Setting, SettingsSchema } from '../ipc/workspace'

vi.mock('../ipc/workspace', () => ({
  getSettingsSchema: vi.fn(),
  setSetting: vi.fn(async () => {}),
}))
vi.mock('../commands/registry', () => ({ executeCommand: vi.fn(async () => {}) }))
vi.mock('../ipc/editor', () => ({ saveDocument: vi.fn() }))

const { axeViolations } = await import('../test/axe')
const ipc = await import('../ipc/workspace')
const { executeCommand } = await import('../commands/registry')
const editorIpc = await import('../ipc/editor')
const { CommandError } = await import('../ipc')
const documents = await import('../state/documents')
const { notifications } = await import('../state/notifications')
const { settingsQuery, settingsSchemaStore, settingsScope } = await import('../state/settings')
const { SettingsPage, matchesQuery, numberProblem, settingKey } = await import('./SettingsPage')

const setting = (overrides: Partial<Setting> & Pick<Setting, 'path'>): Setting =>
  ({
    section: 'Editor',
    extension: null,
    title: 'Title',
    description: 'What it does.',
    type: 'boolean',
    default: true,
    value: true,
    modified: false,
    problem: null,
    folder: true,
    ...overrides,
  }) as Setting

const settings: Setting[] = [
  setting({
    path: ['editor', 'tab_size'],
    title: 'Tab Size',
    description: 'The width of a tab.',
    type: 'integer',
    minimum: 1,
    maximum: 16,
    default: 4,
    value: 2,
    modified: true,
  }),
  setting({
    path: ['editor', 'insert_spaces'],
    title: 'Insert Spaces',
    description: 'Insert spaces when Tab is pressed.',
  }),
  setting({
    path: ['workbench', 'theme'],
    section: 'Workbench',
    title: 'Color Theme',
    type: 'string',
    choices: [
      { value: 'system', label: 'Follow System' },
      { value: 'dark', label: 'Dark' },
    ],
    default: 'system',
    value: 'gone.theme',
    modified: true,
  }),
  setting({
    path: ['files', 'exclude'],
    section: 'Files',
    title: 'Exclude',
    type: 'list',
    default: ['.git'],
    value: ['.git', 'target'],
  }),
  setting({
    path: ['keybindings'],
    section: 'Keyboard Shortcuts',
    title: 'Keybindings',
    type: 'table',
    default: [],
    value: ['file.save'],
    modified: true,
  }),
  setting({
    path: ['extensions', 'settings', 'example.tidy', 'max_line_length'],
    section: 'Tidy',
    extension: 'example.tidy',
    title: 'Max Line Length',
    description: 'Lines longer than this are flagged.',
    type: 'integer',
    minimum: 0,
    maximum: null,
    default: 100,
    value: 100,
    modified: true,
    problem: 'must be at least 0; using the default',
  }),
]

const schema = (overrides: Partial<SettingsSchema> = {}): SettingsSchema => ({
  file: '/config/settings.toml',
  error: null,
  settings,
  folder: null,
  ...overrides,
})

async function show(value = schema()) {
  vi.mocked(ipc.getSettingsSchema).mockResolvedValue(value)
  const view = render(<SettingsPage />)
  await screen.findByRole('heading', { name: 'Editor' })
  return view
}

/** The row of the setting titled `title`. */
const row = (title: string) =>
  screen.getByText(title, { selector: '.setting__title' }).closest('.setting') as HTMLElement

describe('SettingsPage', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    settingsSchemaStore.set(null)
    settingsQuery.set('')
    settingsScope.set('user')
    notifications.set([])
  })

  it('lists every setting by section, with its key, value, and whether it is set', async () => {
    const { container } = await show()
    expect(screen.getAllByRole('heading', { level: 2 }).map(h => h.textContent)).toEqual([
      'Editor',
      'Workbench',
      'Files',
      'Keyboard Shortcuts',
      'Tidyexample.tidy',
    ])
    expect(within(row('Tab Size')).getByText('editor.tab_size')).toBeTruthy()
    expect(screen.getByRole('spinbutton', { name: 'Tab Size' })).toHaveProperty('value', '2')
    expect(screen.getByRole('checkbox', { name: 'Insert Spaces' })).toHaveProperty('checked', true)
    expect(row('Tab Size').dataset.modified).toBeDefined()
    expect(row('Insert Spaces').dataset.modified).toBeUndefined()
    expect(within(row('Keybindings')).getByText('Set for file.save.')).toBeTruthy()
    // Extension keys are quoted as in the file; ignored values are explained.
    expect(
      within(row('Max Line Length')).getByText(
        'extensions.settings."example.tidy".max_line_length',
      ),
    ).toBeTruthy()
    expect(
      within(row('Max Line Length')).getByText(/must be at least 0, so the default/),
    ).toBeTruthy()
    // A value that isn't offered (its theme isn't installed) still shows.
    expect(screen.getByRole('combobox', { name: 'Color Theme' })).toHaveProperty(
      'value',
      'gone.theme',
    )
    expect(await axeViolations(container)).toEqual([])
  })

  it('changes settings: toggles, choices, numbers on Enter, and resets', async () => {
    const user = userEvent.setup()
    await show()
    await user.click(screen.getByRole('checkbox', { name: 'Insert Spaces' }))
    expect(ipc.setSetting).toHaveBeenLastCalledWith(['editor', 'insert_spaces'], false, 'user')

    await user.selectOptions(screen.getByRole('combobox', { name: 'Color Theme' }), 'dark')
    expect(ipc.setSetting).toHaveBeenLastCalledWith(['workbench', 'theme'], 'dark', 'user')

    const tabSize = screen.getByRole('spinbutton', { name: 'Tab Size' })
    await user.clear(tabSize)
    await user.type(tabSize, '20')
    expect(screen.getByText('Must be at most 16.')).toBeTruthy()
    await user.keyboard('{Enter}')
    expect(ipc.setSetting).toHaveBeenCalledTimes(2)
    await user.keyboard('{Escape}')
    expect(tabSize).toHaveProperty('value', '2')
    await user.clear(tabSize)
    await user.type(tabSize, '8{Enter}')
    expect(ipc.setSetting).toHaveBeenLastCalledWith(['editor', 'tab_size'], 8, 'user')

    await user.click(screen.getByRole('button', { name: 'Reset Tab Size to default' }))
    expect(ipc.setSetting).toHaveBeenLastCalledWith(['editor', 'tab_size'], null, 'user')
    expect(screen.queryByRole('button', { name: 'Reset Insert Spaces to default' })).toBeNull()
    // Tables are edited in the file.
    expect(screen.queryByRole('button', { name: 'Reset Keybindings to default' })).toBeNull()
    await user.click(
      within(row('Keybindings')).getByRole('button', { name: 'Edit in settings.toml' }),
    )
    expect(executeCommand).toHaveBeenCalledWith('preferences.openSettingsFile')
  })

  it('adds to and removes from lists', async () => {
    const user = userEvent.setup()
    await show()
    await user.click(screen.getByRole('button', { name: 'Remove .git' }))
    expect(ipc.setSetting).toHaveBeenLastCalledWith(['files', 'exclude'], ['target'], 'user')
    await user.type(screen.getByRole('textbox', { name: 'Add to Exclude' }), ' dist {Enter}')
    expect(ipc.setSetting).toHaveBeenLastCalledWith(
      ['files', 'exclude'],
      ['.git', 'target', 'dist'],
      'user',
    )
  })

  it('reports a refused change and shows the value in effect', async () => {
    const user = userEvent.setup()
    await show()
    vi.mocked(ipc.setSetting).mockRejectedValueOnce(new Error('editor.tab_size is wrong'))
    const tabSize = screen.getByRole('spinbutton', { name: 'Tab Size' })
    await user.clear(tabSize)
    await user.type(tabSize, '3')
    await user.tab()
    await waitFor(() => expect(tabSize).toHaveProperty('value', '2'))
    expect(notifications.get().at(-1)?.message).toContain('editor.tab_size is wrong')
  })

  it('searches titles, keys, and descriptions, and filters by extension', async () => {
    const user = userEvent.setup()
    await show()
    await user.type(screen.getByRole('searchbox', { name: 'Search settings' }), 'tab')
    expect(screen.getAllByRole('heading', { level: 2 }).map(h => h.textContent)).toEqual(['Editor'])
    expect(screen.queryByText('Insert Spaces')).not.toBeNull()

    act(() => settingsQuery.set('@ext:example.tidy'))
    expect(screen.getAllByRole('heading', { level: 2 }).map(h => h.textContent)).toEqual([
      'Tidyexample.tidy',
    ])
    act(() => settingsQuery.set('nothing like this'))
    expect(screen.getByRole('status').textContent).toContain('No settings match')
  })

  it("switches to the open folder's settings, where only some can change", async () => {
    const user = userEvent.setup()
    const folder = {
      name: 'project',
      file: '/project/.flareite/settings.toml',
      exists: false,
      trusted: false,
      error: null,
      ignored: ['terminal'],
    }
    const workspaceSettings = settings.map(s =>
      s.path[0] === 'workbench' || s.path[0] === 'keybindings' ? { ...s, folder: false } : s,
    )
    await show(schema({ folder }))
    vi.mocked(ipc.getSettingsSchema).mockResolvedValue(
      schema({ folder, settings: workspaceSettings }),
    )
    await user.click(screen.getByRole('tab', { name: /Workspace/ }))
    await waitFor(() => expect(ipc.getSettingsSchema).toHaveBeenLastCalledWith('workspace'))
    expect(screen.getByRole('tab', { name: /Workspace/ }).getAttribute('aria-selected')).toBe(
      'true',
    )
    await waitFor(() =>
      expect(screen.getByRole('combobox', { name: 'Color Theme' })).toHaveProperty(
        'disabled',
        true,
      ),
    )
    expect(within(row('Color Theme')).getByText('User Only')).toBeTruthy()
    const notes = screen.getAllByRole('status').map(note => note.textContent)
    expect(notes.some(note => note?.includes("haven't trusted"))).toBe(true)
    expect(notes.some(note => note?.includes('creates .flareite/settings.toml'))).toBe(true)
    expect(notes.some(note => note?.includes('terminal'))).toBe(true)

    await user.click(screen.getByRole('checkbox', { name: 'Insert Spaces' }))
    expect(ipc.setSetting).toHaveBeenLastCalledWith(['editor', 'insert_spaces'], false, 'workspace')
    await user.click(screen.getByRole('button', { name: 'Open Folder Settings (TOML)' }))
    expect(executeCommand).toHaveBeenCalledWith('preferences.openWorkspaceSettingsFile')
  })

  it("can't change settings while settings.toml is invalid", async () => {
    await show(schema({ error: 'expected `=`' }))
    expect(screen.getByRole('alert').textContent).toContain('expected `=`')
    expect(screen.getByRole('checkbox', { name: 'Insert Spaces' })).toHaveProperty('disabled', true)
    expect(screen.getByRole('spinbutton', { name: 'Tab Size' })).toHaveProperty('disabled', true)
  })

  it('shows the mistake when settings.toml is saved with one', async () => {
    await show()
    vi.mocked(editorIpc.saveDocument).mockRejectedValue(
      new CommandError('invalidSettings', 'Settings not applied: unknown field `bogus`'),
    )
    vi.mocked(ipc.getSettingsSchema).mockResolvedValue(schema({ error: 'unknown field `bogus`' }))
    await act(() => documents.save(1))
    expect(screen.getByRole('alert').textContent).toContain('unknown field `bogus`')
  })
})

describe('settings helpers', () => {
  it('matches words anywhere, @modified, and @ext', () => {
    const [tabSize, insertSpaces, , , , tidy] = settings as [
      Setting,
      Setting,
      Setting,
      Setting,
      Setting,
      Setting,
    ]
    expect(matchesQuery(tabSize, 'editor WIDTH')).toBe(true)
    expect(matchesQuery(tabSize, 'tab_size')).toBe(true)
    expect(matchesQuery(tabSize, 'tab terminal')).toBe(false)
    expect(matchesQuery(insertSpaces, '@modified')).toBe(false)
    expect(matchesQuery(tidy, '@ext:example.tidy length')).toBe(true)
    expect(settingKey(tidy)).toBe('extensions.settings."example.tidy".max_line_length')
  })

  it('checks numbers before writing them', () => {
    const [tabSize] = settings as [Setting]
    expect(numberProblem(tabSize, '4')).toBeNull()
    expect(numberProblem(tabSize, '')).toBe('Enter a number.')
    expect(numberProblem(tabSize, '2.5')).toBe('Enter a whole number.')
    expect(numberProblem(tabSize, '0')).toBe('Must be at least 1.')
  })
})
