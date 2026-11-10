# Architecture

This document describes Flareite's architecture as it currently exists and how it is expected to
grow. The goals and constraints come from [`DESIGN.md`](DESIGN.md); decisions with lasting
consequences are recorded as ADRs in [`adr/`](adr/).

## Current state (Milestone 8)

```text
┌──────────────────────────── Desktop process (flareite) ─────────────────────────────┐
│                                                                                    │
│  ┌─────────── WebView ───────────┐        ┌──────────── flareite-app ────────────┐  │
│  │ React workbench (src/)        │        │ commands.rs   app info, logging     │  │
│  │  EditorView ─▶ EditorSession ─┼─ IPC ─▶│ editor/       documents, editing,   │  │
│  │  (visible lines, highlights,  │◀─view──│               syntax state          │  │
│  │   squiggles, hover, suggest)  │        │ lsp/          servers, sync, hover, │  │
│  │  palette, explorer, search,   │◀─events│               definition, complete  │  │
│  │  extensions, status bar       │ channel│ extensions.rs, workspace.rs, ...    │  │
│  └───────────────────────────────┘        └──┬─────────┬──────────┬─────────┬───┘  │
│                                              │         │          │         │      │
│        ┌─────────────────┐  ┌────────────────▼─┐ ┌─────▼────────┐ │  ┌──────▼────┐ │
│        │ flareite-syntax  │◀─┤ flareite-core     │ │ flareite-lsp  │ │  │ flareite-  │ │
│        │ Tree-sitter     │  │ documents, ops,  │ │ JSON-RPC,    │ │  │ workspace │ │
│        │ parse, highlight│  │ find, undo       │ │ positions    │ │  │           │ │
│        └───────┬─────────┘  └────────┬─────────┘ └──────┬───────┘ │  └─────┬─────┘ │
│                └─────────────────────▼──────────────────▼─────────▼────────▼       │
│                                   flareite-text: rope, change sets, edit log        │
└──────────────────────────────────────────────────────────┬─────────────────────────┘
                                                            │ stdio
                                           language servers (rust-analyzer, ...)
```

Extensions run beside this, in `flareite-extension-host`, each on its own thread (see
[Extensions](#extensions)).

## Crates

Crates use the `flareite-` prefix. Dependencies point downward only. Only `flareite-app` depends on
Tauri, and nothing in Rust depends on the frontend.

| Crate                        | Responsibility                                                                                                                                                                                                                                                      | Depends on                                                                      |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- |
| `flareite-text`              | Rope-backed `TextBuffer`, validated `ChangeSet`s, versions, the line-splice log, UTF-16 conversion, grapheme/word boundaries, line endings, regex/literal search over the rope                                                                                      | `ropey`, `unicode-segmentation`, `regex-cursor`                                 |
| `flareite-workspace`         | Reading and atomically writing files, directory listings with ignore rules, the Go to File index, find in files, file watching, backups of unsaved documents                                                                                                        | `flareite-text`, `ignore`, `globset`, `nucleo-matcher`, `notify-debouncer-mini` |
| `flareite-core`              | `Editor` (open documents), `Document`, selections, `EditorOp`s (including find/replace), undo history, dirty state, reload from disk, incremental viewport updates. No I/O.                                                                                         | `flareite-text`, `flareite-workspace` (for `FileStamp`)                         |
| `flareite-syntax`            | The language registry of extensions' WebAssembly Tree-sitter grammars (none built in); language detection, incremental and interruptible parsing with injected layers, per-line highlight spans                                                                     | `flareite-text`, `tree-sitter` (with `wasm`), grammar crates                    |
| `flareite-lsp`               | Language server processes and JSON-RPC over stdio, UTF-8/UTF-16 position conversion, incremental document changes from the edit log, server configuration and workspace roots, a scripted test server                                                               | `flareite-process`, `flareite-text`, `lsp-types`, `serde_json`                  |
| `flareite-diff`              | Line and character diffs of two texts, for change markers (and, compiled into the Git extension, its diff tabs): histogram line diff, then words within changed blocks, optionally ignoring whitespace at the ends of lines                                         | `imara-diff`                                                                    |
| `flareite-process`           | Starting programs (language servers, extensions' programs) in a process group with no console window, so stopping one stops what it started; finding programs on the `PATH`                                                                                         | `win32job` (Windows), `rustix` (Unix)                                           |
| `flareite-extension-api`     | The extension API: the WIT interface (`wit/extension.wit`) and the Rust bindings extensions build against (`Guest`, `export_extension!`). Standalone, so extensions outside the repository can use it                                                               | `wit-bindgen`                                                                   |
| `flareite-extension-install` | Installing extensions from GitHub releases: releases and catalogs from the API, downloads checked against GitHub's digests, unpacking and validating `extension.zip`, and swapping extensions into the extensions folder with a record of their source              | `flareite-extension-host` (manifests), `reqwest` (rustls), `zip`, `sha2`        |
| `flareite-extension-host`    | Extension discovery, manifest validation, permission decisions, a worker thread and bounded event queue per extension, Wasmtime with deadlines and memory limits, the host side of the API (validated reads, edits, diagnostics)                                    | `flareite-core`, `flareite-text`, `wasmtime`, `wit-component`, `semver`, `toml` |
| `flareite-schema`            | Schemas for TOML, JSON, and YAML documents: TOML Schema (the reference implementation, vendored) and JSON Schema checking, documents parsed with spans to place problems, and completion and hover from a schema at the cursor; Flareite's own schemas (`schemas/`) | `jsonschema`, `toml`, `saphyr-parser`, `serde_json`, `regex`, `globset`         |
| `flareite-tasks`             | Bounded background scheduler (priorities, keyed replacement, cancellation) and timing histograms                                                                                                                                                                    | `tracing`                                                                       |
| `flareite-terminal`          | Shells in pseudoterminals (ConPTY, Unix ptys), UTF-8 output decoding, output history for replay, flow control, default shell detection                                                                                                                              | `portable-pty`                                                                  |
| `flareite-app`               | Process lifecycle, Tauri commands, dialogs, clipboard, settings, session state, workspace coordination, syntax and language server state per document, backend events, instrumentation, benchmark mode, logging                                                     | all of the above, Tauri, `toml`, `toml_edit`, `memory-stats`                    |
| `flareite-benches`           | Criterion benchmarks and deterministic fixtures (`benches/`), not part of the app                                                                                                                                                                                   | `flareite-text`, `flareite-core`, `flareite-workspace`, `criterion`             |

Extensions themselves (the examples and test fixtures) build to WebAssembly in a separate
workspace, `extensions/`. The official extensions (`flareite.*`: Git, Markdown, Images, and the
languages) have repositories of their own; nothing in this one depends on them
([ADR 0034](adr/0034-official-extensions-in-their-own-repositories.md)). Tests that need real
grammars use copies in `extensions/test-fixtures/languages/`. The design brief's `editor-app`
and `src-tauri` are one crate ([ADR 0001](adr/0001-application-skeleton.md)). Workspace, settings, and command decisions are in
[ADR 0003](adr/0003-workbench-services.md), and performance decisions in
[ADR 0004](adr/0004-performance-foundations.md), and syntax and language server decisions in
[ADR 0005](adr/0005-syntax-and-language-servers.md), and extension decisions in
[ADR 0006](adr/0006-wasm-extensions.md) and [ADR 0007](adr/0007-workbench-contributions.md),
and hardening decisions (crash recovery, accessibility, regression checks) in
[ADR 0008](adr/0008-hardening.md), and the integrated terminal in
[ADR 0011](adr/0011-integrated-terminal.md), and docking in
[ADR 0012](adr/0012-docking.md), and encodings, binary files, and extensions' viewers in
[ADR 0023](adr/0023-encodings-and-viewers.md). Measurements are in
[`performance.md`](performance.md).

## Background work

All background work runs on one `flareite-tasks` scheduler owned by the app: a fixed pool of
worker threads (CPU cores − 1, between one and four) with three priorities and a queue of at most
256 tasks. When the queue is full, the least important, oldest task is dropped.

- Tasks are **keyed**. A new task with the same key cancels a queued or running one, so only the
  latest search, match count, or reload of a file runs.
- Cancellation is cooperative through a `CancelToken`, which long loops (walks, scans) check.
- A panic in a task is caught and counted, and the worker keeps running.
- On exit, the scheduler is shut down: tasks are cancelled and workers joined.

| Task                  | Priority | Key                  |
| --------------------- | -------- | -------------------- |
| `open-startup-files`  | High     | none                 |
| `workspace-search`    | High     | one search at a time |
| `count-matches`       | High     | one per document     |
| `reload-changed-file` | High     | one per path         |
| `workspace-index`     | Normal   | one index at a time  |

Find in files additionally uses up to four walker threads of its own inside its task, because it
is dominated by opening many small files (3.4× faster in the benchmark). The editor commands
themselves stay synchronous: they take under a millisecond and must apply in order.

## Text model

See [ADR 0002](adr/0002-text-model-and-editor-protocol.md) for the reasoning.

- **Positions are UTF-8 byte offsets** everywhere in Rust. Other units exist only at boundaries,
  as distinct types: `Utf16Position` (line + UTF-16 column) for the frontend now and LSP later.
- **Lines** are separated by LF, CRLF, or CR (Ropey with the `cr_lines` feature), matching LSP.
  Unicode separators such as U+2028 are ordinary characters.
- **Content is preserved byte-for-byte.** Mixed line endings are left alone. Each document detects
  its dominant line ending, and new line breaks (Enter, pasted or inserted text) use it. A UTF-8
  BOM is stripped on load and written back on save.
- **Edits** are `ChangeSet`s: sorted, non-overlapping `Edit { range, text }` in pre-edit
  coordinates, applied atomically. Ranges that are out of bounds or split a character are rejected
  with a structured `EditError`, and the buffer is left unchanged. Applying a change set returns
  its inverse, which history stores for undo.
- **Versions** increase with every content change, including undo and redo, and are never reused.
  They identify stale requests (frontend views now, extension edits and LSP later).
- **Cursor movement and deletion step over grapheme clusters**, so combining marks, emoji ZWJ
  sequences, and CRLF move as one unit. Word movement uses character classes (word, punctuation,
  whitespace), and a line break counts as a stop.

## Editor core

- **Selections**: each document has one or more `Selection { anchor, head, goal }`, kept sorted
  and merged when they overlap, with one primary. Every operation applies to all selections.
  Vertical movement keeps a visual goal column, with tabs expanded to tab stops.
- **Operations** (`EditorOp`): insert, newline (keeps indentation), tab (spaces to the next tab
  stop by default), delete by grapheme or word, motions, select all/word, set or add cursors, add
  a cursor above or below, undo, redo, find next/previous, select all matches, replace, replace
  all (one undo step, with `$1`/`${name}` expansion for regex queries), add next occurrence
  (Ctrl+D), and select all occurrences. Per-selection edits are merged into a single `ChangeSet`.
- **Line operations** (`lines.rs`) work on the blocks of whole lines the selections cover; a
  selection ending at the start of a line doesn't cover it. They insert a line above or below,
  move or copy lines up or down, delete lines, indent and outdent to tab stops, toggle comments
  (with the language's `CommentTokens`, falling back to a block comment), select lines, and put
  cursors at line ends. With nothing selected, copying takes the cursors' whole lines
  (`cursor_lines_text`); the backend remembers that text, and a `paste` of the same text inserts
  it above the cursors' lines. Go to bracket (`brackets.rs`) scans the text for the matching
  bracket, so brackets in strings and comments count.
- **Cursor undo** keeps up to 100 earlier selections, each tagged with the text version; editing
  the text makes them stale.
- **Folding** (`folds.rs`, [ADR 0015](adr/0015-code-folding.md)) keeps the folded regions as
  byte offsets that follow edits. The fold operations take the document's foldable regions as line
  ranges from the caller: the language's fold query (`flareite_syntax::fold_ranges`) or
  indentation (`Document::indentation_folds`). Display rows skip hidden lines. Up and Down count
  rows, other movement steps over hidden lines, and any other operation that leaves a selection
  in hidden lines unfolds their region. Line operations take a folded region with its first
  line.
- **Region comments** (`regions.rs`): `RegionMarkers` finds `#region`/`#endregion` and `MARK:`
  comments, written with the language's `CommentTokens`. After an edit it reads only the changed
  lines, mapping the other markers through the line splices. Marked regions join the foldable
  regions, and named regions and `MARK:` comments are the minimap's section headers.
- **Find** compiles a `SearchQuery` (literal or regex; case-sensitive; whole word) with
  `regex-cursor`, which searches the rope's chunks directly without copying the text. `^`/`$`
  match per line, including before CRLF. Counts stop at 20,000 matches and are reported as
  truncated.
- **Undo history** stores each change as a forward/inverse pair in groups. Consecutive typing or
  deleting merges into one group. A group ends on cursor movement, a save, or whitespace typed
  after a word. Retention is bounded (`HistoryLimits`: 10,000 groups and 64 MiB of stored text by
  default), and the oldest groups are dropped first.
- **Dirty state** compares the current history state id with the saved one, so undoing back to
  the saved text makes the document clean again.
- **Lifecycle**: `Editor` owns documents by stable `DocumentId`s that are never reused. A file
  already open is found by path, so it isn't opened twice. Untitled documents are numbered
  `Untitled-N`.
- **Reload** replaces only the differing middle of the text (common prefix and suffix kept), so
  cursors before and after the change stay on their text. The reload is recorded in history, so
  it can be undone, and the document is marked saved at the new disk state.

## Files and saving

`flareite-workspace` reads and writes files. `flareite-app` coordinates, so the core stays free of
I/O.

- **Loading** reads the whole file, of any size, and decodes it
  (`flareite-workspace/src/encoding.rs`, [ADR 0023](adr/0023-encodings-and-viewers.md)). A byte
  order mark picks UTF-8 or UTF-16. Without one, NUL bytes in the first 8 KiB mean a binary file
  (`Binary`, which the workbench asks about), unless every other byte is NUL (UTF-16 without a
  BOM). Valid UTF-8 comes next, then the legacy encoding `chardetng` guesses, if the file decodes
  in it without errors and encodes back to the same bytes, and otherwise Windows-1252, which gives
  every byte a character of its own. **Open Anyway** reads a binary file as UTF-8 if it's valid,
  and byte for byte otherwise. Nothing is ever decoded lossily: a file read in an encoding the user
  chose must be valid in it (`InvalidText`) and round-trip (`LossyEncoding`).
- **Encodings** are a `TextEncoding` (an `encoding_rs` encoding and whether there's a BOM) that the
  document keeps. **Reopen with Encoding** reloads a clean document in another one (undoably, like
  a reload), and **Save with Encoding** writes it in another one, which the document takes once
  the file is written. A reload after an external change reads the file in the document's
  encoding, falling back to detecting one. Crash recovery backups record it too.
- **Saving** takes a `SavePoint` (an O(1) rope snapshot plus the history state) under the editor
  lock, then writes it without the lock, so editing can continue. The file is written to a
  temporary file in the same directory, flushed with `fsync`, then renamed over the target. A
  crash therefore leaves either the old or the new file, never a partial one. The text is encoded
  as it's written; a character the encoding lacks fails the save (`Unencodable`) and leaves the
  file as it was. Symlinks are
  followed. Read-only files are refused. Unix permission bits are copied to the new file. On
  Windows, the rename is retried for up to about 600 ms while another program (a virus scanner,
  an indexer, a sync client) briefly has the file open. The app's own files (settings edits,
  `state.json`, permission decisions) are written the same way (`write_file_atomically`).
- **External modification policy**: each document remembers the file's modification time and
  length from when it was loaded or saved. If they differ at save time, nothing is written and the
  user is asked whether to overwrite. A file deleted externally is recreated on save. While a
  workspace is open, its files are also watched (see below).
- **Unsaved changes are never discarded silently.** Closing a dirty tab asks Save / Don't Save /
  Cancel. Closing the window with dirty documents asks once (Save All / Don't Save / Cancel); Save
  All prompts for a path for each untitled document, and any cancellation keeps the window open.
- **Save As** to a path that's open in another tab is refused, so two documents never share a
  path.

Hard links are not preserved (the rename replaces the link with a new file), and on Windows the
replacement file gets default attributes. A file changed externally within the filesystem's
timestamp granularity and with the same length isn't detected.

## Workspace

`flareite-app/src/workspace.rs` owns the open folder (one at a time). Opening one starts:

- **Exclusions and ignore rules.** `files.exclude` globs (default `.git`, `.DS_Store`,
  `Thumbs.db`) are hidden everywhere. `.gitignore` (in git repositories), `.ignore`, and global
  git excludes come from ripgrep's `ignore` crate. Ignored entries are **shown dimmed** in the
  explorer but skipped by the file index and search. A listing marks the entries that rules
  match; the explorer also dims everything inside an ignored folder, as Git never looks inside
  one (a rule like `!target/keep.txt` doesn't un-ignore it).
- **Problem marks.** The explorer colors files with errors or warnings, with their count, and
  the folders above them, with a dot. It derives them from the Problems view's file problems
  (`problemsByPath`), so they cover what's listed there.
- **Explorer listings** are fetched one directory at a time when a folder is expanded, never by
  walking the whole tree. Directories are listed first, sorted case-insensitively. Listing
  requests outside the workspace root are refused.
- **Creating files and folders** (`workspace_create`, `flareite-workspace/src/create.rs`) takes a
  parent folder inside the workspace and a typed name, which may contain `/` or `\` to create
  subfolders on the way. Names that would leave the parent (`..`) or that the platform can't
  store (on Windows: `<>:"|?*`, trailing periods or spaces, device names like `nul`) are refused,
  and nothing existing is ever replaced. The frontend re-lists the changed folders right away
  instead of waiting for the watcher.
- **Renaming, moving, copying, and deleting** ([ADR 0019](adr/0019-explorer-file-operations.md),
  `src-tauri/src/entries.rs`, `flareite-workspace/src/entries.rs`) take paths inside the open
  folder (not the folder itself, and no `..`). Nothing existing is replaced: a rename or move onto a taken name fails, and a
  copy takes the first free `name copy.ext`, `name copy 2.ext`, …. A move across drives copies,
  then deletes. Deleting asks with a native dialog, then moves to the system's trash (the
  `trash` crate) or, with Shift, deletes permanently; what can't go to the trash can be deleted
  permanently after asking again. Renamed or moved open documents follow
  (`Editor::follow_move`): they're synced with their language servers and extensions under the
  new path, and the frontend gets `documentMoved`. Documents of deleted files stay open.
  **Reveal** runs `explorer /select,` on Windows and `open -R` on macOS, and opens the folder
  elsewhere.
- **The Go to File index** is built on the scheduler (`workspace-index`) and is cancelled if
  the workspace changes. It holds relative paths only, up to 200,000 files (beyond that it's
  marked truncated). Queries are fuzzy-matched with `nucleo-matcher` and return the best 100,
  with match positions for highlighting.
- **File watching** (`notify` with a 250 ms debounce) keeps everything current incrementally. The
  index adds and removes only the changed paths, the explorer re-lists only the loaded directories
  that changed, and open documents react:
  - unchanged stamp (Flareite's own save): ignored;
  - **clean** document: reloaded in place;
  - **dirty** document: left untouched, and the user is told that saving will ask before
    overwriting.
- **Find in files** runs on the scheduler, searches files in parallel, and streams results file
  by file through a Tauri `Channel`. Starting a new search cancels the previous one. Open documents are searched as edited
  (including unsaved changes). Binary files (NUL in the first 8 KiB), non-UTF-8 files, and files
  over `search.max_file_bytes` (4 MiB) are skipped. A search stops at 20,000 matches, or 1,000 per
  file. **Replace across files is not implemented yet** ([ADR 0003](adr/0003-workbench-services.md)).

## Settings and session state

- **`settings.toml`** lives in the platform config directory (e.g. `%APPDATA%\dev.flareite.editor`)
  and is edited in Flareite itself, in the settings editor (**Preferences: Open Settings**,
  `Ctrl+,`) or as a document (**Open Settings (TOML)**). It's created from a commented template
  the first time it's needed. The settings editor ([ADR 0017](adr/0017-settings-editor.md)) is a
  page (`page:settings`) that lists `settings_schema`: every built-in setting
  (`settings/schema.rs`) and every setting extensions declare, with its value, default, and
  whether the file sets it. It writes one key at a time with `settings_set`, which keeps the
  file's comments and refuses changes that would make it invalid. Saving the file, or a change in
  the settings editor, validates and applies it immediately: tab size and spaces apply to open
  documents, font and theme to the frontend, and exclusions rebuild the index. Invalid settings (unknown keys, out-of-range values, bad globs) are reported
  with the parser's message and the previous settings stay in effect. **Color Theme** commands
  edit the file with `toml_edit`, preserving comments and formatting. `workbench.theme` names a
  built-in theme or a contributed one; `workbench.colors` overrides theme tokens; `[keybindings]`
  replaces commands' keys.
- **A folder's settings**, `.flareite/settings.toml` in the open folder (`settings/folder.rs`),
  apply over the user's while the folder is trusted, merged table by table. Only `[editor]`,
  `[files]`, `[search]`, `[schemas]`, `lsp.enabled`, and `[extensions.settings]` apply; the rest
  is reported and ignored. `SettingsState` keeps the user's settings, the folder's, and the result
  in effect, which `get()` returns. They're read again when the folder opens or closes, its trust
  changes, or its file is saved or changed on disk. The settings editor's Workspace tab is
  `settings_schema`/`settings_set` with `scope: "workspace"`
  ([ADR 0041](adr/0041-schemas-and-folder-settings.md)).
- **`state.json`** in the local data directory holds the open folder, open files, the active file,
  and the frontend's layout (the dock's tree, expanded folders, the minimap). It's
  written atomically on every change; the frontend debounces its layout updates by 500 ms. On
  launch the folder and files are reopened, then any files named on the command line, the last of
  which becomes active. Unsaved changes are backed up only for crash recovery (see below), not
  kept across a normal close, so closing still asks about them.
  It also records when Flareite last looked for an update of itself, and why an update failed to
  install, to report once.
- **Updating Flareite.** `src-tauri/src/app_update.rs` finds the newest release past the one the
  build was made from (its tag is compiled in), in the channel `update.channel` chooses, and has
  Tauri's updater fetch its `latest.json` and installer, which must be signed with the key in
  `tauri.conf.json`. The download is held in memory; **Restart to Update** closes the window as
  closing it does and installs the update last in `RunEvent::Exit`, then starts Flareite again.
  Release builds check at startup at most once a day unless `update.check` is off
  ([ADR 0040](adr/0040-updating-flareite-from-github-releases.md)).
- **`extension-permissions.json`** next to `settings.toml` holds the user's permission decisions
  and enabled state for each extension. Installed extensions live in the `extensions` folder
  there too.

## Crash recovery and error handling

### Backups of unsaved changes

A background thread (`src-tauri/src/backups.rs`) keeps a backup of every document with unsaved
changes, so a crash, a forced shutdown, or a power cut loses at most the last second of work.

- **Where.** Each running instance has a session folder,
  `<local data dir>/backups/<pid>-<time>/`. It holds one file per unsaved document: a format
  line, JSON metadata (path, title, cursor), then the text as is. The folder's `lock` file stays
  exclusively locked while the instance runs (`File::try_lock`). The operating system releases
  the lock when the process ends, however it ends.
- **When.** Edits only signal the thread. It waits 750 ms for a burst of edits to settle, then
  takes O(1) rope snapshots under the editor lock and writes them without it. Each file is
  written next to its final name and renamed into place.
  - Large documents are backed up less often: a document isn't written again sooner than its
    size at 4 MB/s, so a 100 MB file is backed up every 25 s while it's being edited.
  - Backups of documents that are saved, closed, or undone back to their saved text are removed.
    A document closed with **Don't Save** loses its backup immediately.
- **Restoring.** At startup, a session folder whose lock can be taken belongs to an instance that
  didn't exit cleanly, and its backups are restored before the previous session's files reopen.
  - A file-backed document opens with the file's text, and the backup is applied as one undoable
    change. It's dirty, its undo history leads back to the file on disk, and saving still checks
    for external changes.
  - A backup whose text matches the file is skipped. A backup whose file was deleted becomes an
    untitled document.
  - The restored documents are backed up in the new session before the old folder is removed.
    If any backup can't be restored, the old folder is kept.
  - A warning lists what was restored.
- **Exiting.** A clean exit removes the session folder: all changes were saved, or the user chose
  **Don't Save**. Otherwise (an exit that skipped the prompt), the backups are flushed and kept
  for the next start.
- **Quit without the window closing.** Quit on macOS (`Cmd+Q`) asks about unsaved changes like
  closing the window does (`RunEvent::ExitRequested`).
- **Benchmark runs** keep no backups and restore none.

### Errors

- **Backend.** Non-test code has no `unwrap` (a Clippy lint) or `expect`. Poisoned locks are
  recovered rather than propagated. Panics are caught in scheduler tasks and around each
  extension's thread, and logged by the panic hook. Tauri commands return typed
  `CommandError`s, which the frontend shows.
- **Problems found in the background** (files that can't be reopened at startup, restored
  changes) reach the user as `notice` events. Events sent before the frontend subscribes are kept
  (up to 256) and delivered when it does.
- **Frontend.** React error boundaries surround the workbench, the dock, each view, the editor
  (reset when switching documents), and the status bar. A rendering error is logged and replaced
  by a message with **Try Again** and **Reload Window**, and the rest stays usable. Documents,
  unsaved changes, and extensions live in the backend, so **Developer: Reload Window** loses
  nothing. Uncaught errors and unhandled rejections are forwarded to the backend log.
- **Notifications** disappear after a while (information after 10 s, warnings and errors after
  20 s), but not while hovered or focused. Only sticky ones, such as the permission prompt,
  stay until answered.
- **Problems** ([ADR 0018](adr/0018-output-and-problems.md), `panels/ProblemsPanel`,
  `state/problems.ts`) lists what's wrong with Flareite and extensions, grouped by source. Current
  problems come from state (an extension that stopped or couldn't be loaded, ignored extension
  settings, a `settings.toml` that isn't in effect) and go away when fixed. Reported problems
  are the warnings and errors notifications showed, counted when repeated, kept until cleared
  (at most 200). File problems follow, by file: `problems_list` returns every open document's
  diagnostics (language servers' and extensions') and those servers published for files that
  aren't open, most severe first, without hints, at most 10,000. The frontend fetches them at
  startup, when a document opens or closes, and 250 ms after `diagnosticsChanged` or
  `problemsChanged`. Choosing one reveals it, or opens its file there. The status bar always
  shows the errors and warnings of all of them, and opens Problems.

## Commands and keybindings

All workbench actions are entries in a frontend **command registry** (`src/commands/`). Each
has an id, a title, an optional category, optional keybindings (`mod` means Ctrl, or Cmd on
macOS, with per-platform overrides), an `enabled` predicate, and an `editorFocus` flag for
editor-only keys. One capture-phase listener runs bound commands. While a terminal has focus,
only commands flagged `terminal` (the palette, Go to File, showing views, switching tabs,
settings, terminal commands) run; other keys go to the shell. Bindings with Shift and
punctuation (`` ctrl+shift+` ``) also match by the physical key, since the browser reports the
shifted character, as do bindings with Alt and a letter or punctuation, since Option changes the
character on macOS. A binding can be a **chord** of two key combinations (`mod+k mod+0`): the
first key press waits (the status bar says so), and the next one runs the command or cancels the
chord. The **command palette** lists
enabled commands with their shortcuts (`>`), finds files (no prefix), goes to a line (`:`), to
a symbol in the active editor (`@`, or `@:` grouped by kind), and to a symbol in the workspace
(`#`). Going to a line or to a symbol in the editor previews it as in VS Code: the selected
item's lines are highlighted and scrolled to the middle without moving the cursor, and closing
the palette without choosing scrolls back. Workspace symbols are searched 200 ms after typing
pauses; a newer search cancels the older one on the servers.
Editing keys that need the editor's textarea (arrows, Backspace, clipboard) stay in
`editor/keymap.ts`. Extension commands, declared in their manifests, are registered into the
same registry, so they appear in the palette before their extension starts.

When a key matches several commands, the user's `[keybindings]` settings win (they replace a
command's keys, or remove them with `""`), then built-in defaults, then extension defaults. The
backend validates and normalizes key syntax (`flareite_extension_host::keys`) for both
manifests and settings. **Preferences: Keyboard Shortcuts** opens a list of every command with
its id and keys.

## The frontend–backend boundary

- **Rust owns authoritative state.** The frontend never holds a document's text; it renders views
  of the lines near the visible area.
- **Commands are the only entry point.** Tauri commands are defined in `src-tauri/src/`
  (`commands.rs`, `editor/commands.rs`, `lsp/commands.rs`, `extensions.rs`, `workspace.rs`,
  `settings/`, `ui_state.rs`, `events.rs`). They are mirrored by typed wrappers in `src/ipc/`, the only frontend code that
  calls `invoke`.
- **Backend events** (workspace changed, files changed, document reloaded or changed on disk,
  settings changed, index status, match counts, syntax parsed, diagnostics changed, language
  server state, extension state, messages, failures, and edits) are pushed through one Tauri `Channel` that the frontend
  registers at startup, so the webview needs no event permissions.
- **Payloads are explicit DTOs** (`src-tauri/src/editor/protocol.rs`) with camelCase fields, not
  core types.
- **Inputs are validated and bounded in Rust.** Positions are clamped and snapped to grapheme
  boundaries, batches are limited to 1,000 operations, and views to 1,000 lines and 10,000
  selections.
- **Errors** reach the frontend as `{ code, message }`, with stable codes such as `binary`,
  `modifiedOnDisk`, and `unknownDocument`. The frontend shows them as notifications, except
  `binary` when opening a file, which opens the tab that asks about it.
- **Browser fallback.** Outside the desktop shell (`pnpm dev`, tests) commands fail with code
  `unavailable` and the workbench still renders.

### Editor protocol

The frontend sends **operations**, not text diffs. Each response is the **view** for the caller's
viewport:

```text
editor_apply(id, ops[], viewport, known?, search?) → EditorView
viewport   = { startRow, endRow }                      display rows (folded lines skipped)
known      = { version, ranges: [[start, end]] }       lines the client holds
EditorView = { version, lineCount, rowCount, startRow, startLine, endLine,
               visible: [[start, end]], folded[], foldable[], foldGeneration,
               lines: { base, splices[], chunks[{ start, lines[] }] },
               selections[], selectionCount, primary: { position, visualColumn, row }, dirty,
               search?: { matches[] (visible only), total | null, truncated, current, error },
               highlights: [{ line, spans: [start, end, kind, ...] }]  changed lines only,
               diagnostics: [{ start, end, severity }]                  visible lines,
               language?: { language, server?: { name, state, message }, errors, warnings,
                            completionTriggers } }
```

- Positions on the wire are `{ line, column }` with UTF-16 columns, which are JavaScript string
  indices into the rendered line. Rust converts them against the document as it is when each
  operation applies.
- `EditorSession` (`src/editor/session.ts`) keeps at most one request in flight. Operations
  issued in the meantime are sent together as the next batch, so ordering is preserved and fast
  typing costs few round trips. Consecutive drag positions collapse to the latest one.
- **Viewports are display rows.** Lines in folded regions have no row. A view covers the
  `visible` line ranges of its rows, and the client places each line by its row. `folded` and
  `foldable` list the visible lines with gutter fold markers. `foldGeneration` changes whenever
  the folded regions do.
- **Views are incremental.** The viewport is the visible lines plus 20 lines of overscan above
  and below. Each request says which lines the client holds (`known`). The backend replays the
  buffer's line splices since that version (`base`) and sends only the lines the client is
  missing (`chunks`). The client replays the same splices on its cache (`src/editor/lineCache.ts`)
  and fills in the chunks. A keystroke sends one line, Enter shifts cached lines instead of
  resending them, and scrolling sends only newly exposed lines. A client too far behind (the log
  keeps 512 changes) gets a full update. An update the client can't apply makes it drop its cache
  and refetch, so a mismatch costs a round trip, never wrong text.
- Lines longer than 16 KiB arrive **shortened for display**, with their full length, and render
  with a "…" badge. Editing, saving, and search always use the whole line.
- Scrolling requests a new view only when the visible lines aren't already held.
- Before saving or closing, the workbench waits for the session to go idle, so the save includes
  every keystroke typed before it.
- While the find widget is open, its query rides along with every request, so highlights and the
  "3 of 17" count stay current as you type. For documents over 1 MiB, the total is counted on the
  scheduler from a snapshot. The view shows visible matches immediately and `total: null`
  ("Counting…") until a `searchCounted` event triggers a refresh. Find operations with an invalid regex are skipped,
  and the view reports the error instead.
- **Highlights are incremental too.** Spans use UTF-16 columns and token kinds numbered as in
  `TOKEN_KINDS`. Lines sent as chunks start with no spans; other held lines keep theirs. The
  backend remembers a hash of the spans it sent for each line the client holds, replays the
  line splices on those hashes, and sends spans only for lines whose hash changed. Typing on a
  line resends that line's spans; opening a block comment resends the lines it now covers.
- **Every view syncs the document with its language server first**, so diagnostics, hover, and
  completion requests always refer to text the server has.

| Command                                                       | Purpose                                                |
| ------------------------------------------------------------- | ------------------------------------------------------ |
| `app_info`, `frontend_log`, `frontend_ready`                  | Process metadata, logging, startup timing              |
| `logs_read`, `logs_file`                                      | Recent log records by channel; today's log file        |
| `document_list`                                               | Open documents (restores tabs after a webview reload)  |
| `document_new`                                                | New untitled document                                  |
| `document_open`, `document_open_dialog`                       | Open a path, or show the native open dialog            |
| `document_save`, `document_save_as`                           | Save, prompting for a path or an overwrite when needed |
| `document_close`                                              | Close, asking about unsaved changes                    |
| `editor_view`                                                 | View for a viewport (after scrolling)                  |
| `editor_minimap`                                              | Lines drawn small for the minimap                      |
| `editor_apply`                                                | Apply a batch of operations, return the new view       |
| `editor_clipboard`                                            | Copy or cut the selection to the system clipboard      |
| `workspace_get`, `workspace_open[_dialog]`, `workspace_close` | The open folder                                        |
| `workspace_list_dir`                                          | One explorer directory listing                         |
| `workspace_create`                                            | New empty file or folder from the explorer             |
| `workspace_rename`, `workspace_move`, `workspace_copy`        | Rename, move, or copy files and folders                |
| `workspace_delete`                                            | Ask, then delete to the trash or permanently           |
| `workspace_reveal`                                            | Show a file or folder in the system's file manager     |
| `clipboard_write_text`                                        | Put text on the clipboard (Copy Path)                  |
| `workspace_find_files`                                        | Fuzzy Go to File over the index                        |
| `workspace_search`, `workspace_search_cancel`                 | Streaming find in files                                |
| `settings_get`, `settings_file`, `settings_set_theme`         | Frontend settings, the settings file path, theme       |
| `settings_set_icon_theme`                                     | File icon theme                                        |
| `settings_schema`, `settings_set`                             | Every setting for the settings editor; set one         |
| `settings_problem`                                            | Why `settings.toml` isn't in effect, for Problems      |
| `problems_list`                                               | Every file's diagnostics, for Problems                 |
| `ui_state_get`, `ui_state_save`                               | Saved layout and active file                           |
| `events_subscribe`                                            | Register the backend event channel                     |
| `perf_stats`                                                  | Backend timings, scheduler statistics, memory          |
| `lsp_hover`, `lsp_definition`, `lsp_completion`               | Language server requests at a position                 |
| `lsp_references`                                              | A symbol's references, with their lines                |
| `lsp_document_symbols`, `lsp_workspace_symbols`               | A document's symbols; symbols in the workspace by name |
| `lsp_resolve_symbol`                                          | Where a workspace symbol is in its file now            |
| `editor_peek_lines`                                           | A file's highlighted lines, for the references peek    |
| `terminal_create`, `terminal_list`, `terminal_attach`         | Start a terminal, or reattach after a reload           |
| `terminal_platform`                                           | The Windows build, for the emulator's ConPTY handling  |
| `terminal_profiles`, `settings_set_default_profile`           | Installed shells and profiles; choose the default      |
| `terminal_write`, `terminal_resize`, `terminal_acknowledge`   | Input, size, and flow control for a terminal           |
| `terminal_kill`                                               | End a terminal's shell and forget it                   |
| `bench_config`, `bench_generate`, `bench_report`              | Benchmark mode only (`--bench <name>`)                 |

### Permissions, dialogs, and clipboard

The main window's capability grants **no** Tauri core or plugin permissions. File dialogs,
message dialogs, and clipboard writes (`tauri-plugin-dialog`, `tauri-plugin-clipboard-manager`)
are called from Rust, so the webview can't open arbitrary dialogs or read the clipboard. Copied
text goes from the backend straight to the clipboard. Paste uses the webview's native `paste`
event, which needs no permission.

The production Content Security Policy allows only same-origin scripts, IPC, and inline styles.
The development CSP additionally allows Vite's HMR socket and inline scripts.

## Syntax highlighting

`flareite-syntax` holds the languages in a `Registry`. All of them are grammars that extensions
contribute as WebAssembly ([ADR 0013](adr/0013-extension-languages.md),
[ADR 0014](adr/0014-languages-only-from-extensions.md)); none is built in. The app
rebuilds the registry when extensions change (`languages.rs`) and keeps one `SyntaxTree` per
document of a known language (`editor/syntax.rs`), detected from the file name and re-detected
after Save As or when the languages change.

- **Injections are layers.** A language's injection query marks text in other languages (CSS
  in an HTML `<style>`). After parsing a document, each injection is parsed as a layer over its
  ranges, with Tree-sitter's included ranges, and layers' own injections after them. Edits are
  applied to every layer, and an old layer overlapping a new injection of the same language is
  reused for an incremental parse. Layers share the parse's budget and cancellation, and are
  painted over the document's own highlights, outermost first.
- **WebAssembly grammars** run on Tree-sitter's own Wasmtime store. Each thread keeps one parser
  with a store, so a grammar is instantiated once per thread instead of on every parse.

- **Edits update the tree immediately.** On each view, the byte edits since the tree's version
  (from the buffer's edit log, with row/column points) are applied with `Tree::edit`, so the old
  tree's spans stay aligned with the text right away.
- **Reparsing is incremental and budgeted.** With a tree, an incremental reparse runs inline
  under the editor lock, stopped by Tree-sitter's progress callback after 3 ms. If it doesn't
  finish (or there is no tree yet), the parse runs on the scheduler (keyed `parse:<id>`, high
  priority) over a snapshot, with the edited old tree, and can be cancelled. Its result is caught
  up with any edits made meanwhile, stored, and announced with a `syntaxUpdated` event that makes
  the editor refetch. Only one background parse per document runs at a time, so continuous typing
  in a large file costs at most one wasted 3 ms attempt per background parse.
- **Highlighting covers only the viewport.** The highlight query runs over the visible lines'
  byte range (each line limited to the 16 KiB display limit), reading the rope through a
  `TextProvider` without copying the document. Overlapping captures resolve by painting wide
  captures first and narrow ones over them, with earlier query patterns winning for the same
  node, which is the convention highlight queries are written for. Capture names map to a fixed
  set of token kinds (`TokenKind`), styled by `--color-syntax-*` tokens.
- **The minimap highlights from a snapshot.** It covers many more lines than the viewport, so
  `editor_minimap` copies the rope and the edited tree under the editor lock and runs the query
  after releasing it ([ADR 0010](adr/0010-minimap.md)). It doesn't touch the highlights mirror
  that keeps the editor's views incremental. The snapshot also carries the section headers on
  its rows (`headers`), which the minimap draws as labels. They stack when they would overlap,
  and the frontend leaves out the kinds the `editor.minimap_*_headers` settings turn off.
- Grammars and queries are compiled once, on first use of each language. Documents over 16 MiB
  aren't highlighted.

## Language servers

`flareite-lsp` runs servers and speaks the protocol; the app decides which documents go to which
server (`lsp/`).

- **Servers start on demand.** The first view of a document whose language has a configured
  server opens it on the server for its root (the workspace folder if it contains the file, else
  the nearest ancestor with a root marker such as `Cargo.toml`, else its directory). The server
  is started on its own thread; until it is up, the status bar shows "starting", and when it is
  up every waiting document is opened. Servers come from language extensions (none is built in);
  settings (`[lsp]`) can change or add servers or turn them off. A server that isn't
  installed is reported once in the status bar, not retried.
- **One connection, three threads.** A writer thread drains a queue to the server's stdin, so
  sending never blocks the caller (or the editor lock); a reader thread parses
  `Content-Length`-framed messages (at most 64 MiB) and routes responses to waiting requests and
  notifications to the app; a third forwards stderr to the log. The client answers the requests
  servers send (configuration, progress tokens, capability registration) with defaults.
- **Positions.** The client offers UTF-8 and UTF-16 position encodings, preferring UTF-8, which is
  Flareite's native unit, so rust-analyzer needs no conversion. For UTF-16 servers, positions are
  converted with the rope's indices.
- **Document sync is incremental.** Each view sends the buffer's edits since the last sync as
  `didChange` ranges (for UTF-8, straight from the edit log's points; for UTF-16, by replaying
  the edits on a copy of the last synced snapshot, which is cheap with a rope). Servers that only
  accept full text get full text; documents over 8 MiB aren't sent. `didSave` and `didClose`
  follow the document's lifecycle.
- **Diagnostics are anchored to text.** A `publishDiagnostics` notification is interpreted
  against the snapshot of the version it names (the last 16 synced snapshots are kept), stored
  as byte ranges, and mapped through later edits when shown, so squiggles move with the text
  while the server catches up. Each view carries the diagnostics on its lines.
- **Files that aren't open.** Diagnostics a server publishes for a file that isn't open (a
  workspace-wide check) are kept by server and path, at the server's positions, for the
  Problems view, and announced with `problemsChanged`. A closed document's diagnostics are kept
  the same way until the server publishes others (servers that only check open files clear
  them). A document's own diagnostics replace them when it opens, and a server that exits or
  restarts loses them.
- **Requests never hold locks while waiting.** `lsp_hover`, `lsp_definition`,
  `lsp_references`, `lsp_document_symbols`, and `lsp_completion` sync the document, capture the position and a snapshot under the locks,
  then wait on a blocking thread (5 s timeout). Completion results are filtered and ranked in
  Rust (prefix, case-insensitive prefix, then letters in order; at most 200) and their edit
  positions are mapped to the current text. The frontend narrows them further as typing
  continues, asking again only for incomplete lists. Requests the server cancels or invalidates
  (`ContentModified`) are dropped quietly. Abandoned requests are cancelled with
  `$/cancelRequest`. `lsp_workspace_symbols` asks every running server that can search
  symbols, at once, and leaves their positions as the servers put them; `lsp_resolve_symbol`
  maps the chosen one into its file's text, so a search doesn't read every file it names.
- **Failures are contained.** A server that exits fails its pending requests, drops its
  documents' diagnostics, and is restarted on demand up to three times, then marked failed.
  Opening or closing a folder, or changing `[lsp]` settings, restarts all servers. On exit, the
  app asks every server to shut down and kills those that don't within 2 seconds. Killing a
  server kills every process it started too: each server runs in its own job object on Windows
  (which also kills it if Flareite crashes) and its own process group on Unix.

## Schemas

`src-tauri/src/schemas` decides which schemas apply to a TOML, JSON, or YAML document, loads them,
and checks documents on a thread of its own; `flareite-schema` does the work on a document
([ADR 0041](adr/0041-schemas-and-folder-settings.md), [ADR 0044](adr/0044-yaml-documents.md)).

- **Which apply:** the schema a document names (`$schema`, `#:schema`, `[toml-schema] location`,
  `# yaml-language-server: $schema=`);
  else every match among the folder's and the user's `[[schemas.associations]]`, extensions'
  `[[schemas]]` and those their code set, and Flareite's own (`settings.toml`, `extension.toml`,
  `.tosd` files); else the first match in SchemaStore's catalog.
- **Loading** by address: `file:` URLs, `flareite://schemas/<name>` (built in; the settings'
  schema is made with the settings extensions declare), `flareite://extensions/<id>/schemas/<id>`,
  and `https:` addresses, downloaded in the background into the cache folder (`schemas/fetch.rs`)
  and downloaded again after a day. A JSON Schema's references are fetched before it's loaded.
- **Checking:** building a view (`SchemaState::sync`) queues a document whose problems are out of
  date; the thread waits for typing to pause, checks the latest version, keeps the problems with
  it, and announces `diagnosticsChanged`. Kept problems move with edits, like a language server's.
  Changes to settings, extensions, or schema files bump a generation that checks every document
  again.
- **Completion and hover** go through `lsp_completion` and `lsp_hover`, which add the schemas'
  items and descriptions to a language server's. Views of TOML, JSON, and YAML documents get
  completion triggers even without a server.

## Terminals

`flareite-terminal` runs shells; `src-tauri/src/terminal.rs` exposes them as commands; xterm.js
emulates the terminal in the frontend (`src/terminal/`). The decisions are in
[ADR 0011](adr/0011-integrated-terminal.md).

```text
 frontend (xterm.js)                     flareite-app / flareite-terminal          shell
 ───────────────────                     ──────────────────────────────          ─────
 keys, paste, replies ── terminal_write ─▶ input queue ── writer thread ───────▶ pty
 drawn output ───── terminal_acknowledge ─▶ flow control ◀─┐
 Channel ◀── output, replay, exited ◀──── history ◀── reader thread (UTF-8) ◀── pty
                                                       waiter thread ◀── exit
```

- **Profiles.** `flareite-terminal/src/profiles.rs` finds the installed shells where their
  installers put them. On Windows, that's PowerShell 7, Windows PowerShell, Command Prompt, Git
  Bash, Cygwin, MSYS2, and each WSL distribution (`wsl.exe --list --quiet`, without Docker
  Desktop's). Elsewhere, it's `/etc/shells` and `$SHELL`. The system is reached through a
  `Probe` trait, so tests use a fake one. Settings add profiles, change found ones, or hide them
  (`[terminal.profiles]`), and name the default (`terminal.default_profile`). What was found is
  reused for 30 seconds (`terminal_profiles`, which the frontend's profile menus list).
- **Starting.** `terminal_create(cols, rows, profile, channel)` runs the named profile (or the
  default) in the workspace folder, or the home folder, on a blocking thread. The tab shows the
  profile's name. At most 32 terminals are open at once.
- **Threads.** Each terminal has a writer draining a bounded input queue, a reader decoding
  output as UTF-8, and a waiter that closes the pseudoterminal when the shell exits and reports
  the exit after the last output.
- **Flow control.** The frontend acknowledges output in steps of 50,000 UTF-16 units. Reading
  pauses at 500,000 unacknowledged units and resumes below 100,000, so the program blocks
  instead of flooding the webview.
- **Reload.** Terminals outlive the webview. Each keeps its last 256 KiB of output. A reloaded
  window attaches to the running terminals (`terminal_list`, `terminal_attach`), the kept output
  is replayed first, and xterm.js's replies to replayed queries are dropped.
- **Exit.** A shell that exits with code 0 closes its tab. Any other exit leaves the tab with a
  message until a key is pressed. Killing a terminal, or quitting the app, ends the shell and
  the programs it started.
- **ConPTY on Windows.** Flareite ships the ConPTY from Windows Terminal (`conpty.dll` and
  `OpenConsole.exe`), which `src-tauri/build.rs` downloads (pinned and hash-checked) and
  `tauri.windows.conf.json` installs beside the executable. The one in Windows rewraps and
  repaints its screen on resize, which duplicated or lost lines. The emulator is told it's
  talking to ConPTY (`windowsPty`, with the build from `terminal_platform`)
  ([ADR 0011](adr/0011-integrated-terminal.md#resizing-on-windows-2026-10-30)).

## Extensions

`flareite-extension-host` loads and runs WebAssembly extensions; `src-tauri/src/extensions.rs`
connects it to the editor, settings, and frontend. The developer guide is
[`extensions.md`](extensions.md); the decisions are in [ADR 0006](adr/0006-wasm-extensions.md).

```text
 editor side                        flareite-extension-host                  extension thread
 ───────────                        ──────────────────────                  ────────────────
 view, open, close ── sync ───────▶ document records ── push ─▶ bounded queue ─▶ document-*
 palette command ──── run ────────▶ start if needed ── push ─▶ (merges changes)  run-command
                                                                                │ host calls
 editor core ◀──── apply-edits: validated, versioned, one undo step ◀───────────┤
 views ◀────────── publish-diagnostics: stored, mapped through edits ◀───────────┘
```

- **Installing.** `src-tauri/src/extension_install.rs` runs `flareite-extension-install` when the
  user asks: it downloads a release's `extension.zip` into `<config dir>/extension-staging`,
  unpacks and validates it there, confirms with a dialog that shows the manifest's name,
  version, and permissions, moves it into `<config dir>/extensions/<id>` (moving any older
  version aside first, so a failure leaves it), and reloads extensions. Uninstalling moves the
  folder out the same way, then forgets the extension's permission decisions and storage. The
  catalog and update checks are cached for the session; nothing uses the network unasked
  ([ADR 0036](adr/0036-installing-extensions-from-github-releases.md)).
- **Discovery.** Installed extensions are the subfolders of `<config dir>/extensions` that hold
  an `extension.toml`. Development extensions are the folders listed in
  `extensions.development`, loaded in place. Discovery and manifest validation happen on a
  background thread at startup and on **Reload Extensions**. Invalid manifests are listed with
  their error.
- **Development builds.** A development extension whose manifest has a `[build]` gets a builder
  (`development.rs`): a watcher on its folder (skipping `.git`, `.gitignore`d files, and the entry
  point) and a thread that runs the build once when watching starts and again after changes,
  coalescing changes made during a build into one more. Output goes to the extension's log. A
  successful build reloads just that extension (`reload_development`), or rediscovers all of
  them when it becomes valid or invalid
  ([ADR 0037](adr/0037-building-development-extensions.md)).
- **Permissions.** An extension that requests permissions the user hasn't decided about isn't
  started; the frontend asks with a notification. Effective permissions (requested ∩ granted)
  are passed to `activate` and checked on every host call.
- **Lifecycle.** Extensions start on their activation event (`on-startup`, a document of a given
  language, or one of their commands). Each runs on its own thread, owning its Wasmtime store.
  The thread compiles the module (componentizing a core module with `wit-component`),
  instantiates, activates, and then processes its queue one event at a time. Stopping deactivates
  the extension (1 s); shutdown waits up to 1.5 s in total, then cancels.
- **Notifications.** Every view, open, and close syncs the document with the host under the
  editor lock. New documents are announced; for changed ones, the edit log since the last sync
  becomes a list of UTF-8 changes queued to each extension that reads documents. Queued changes to
  the same document merge.
- **Host calls** run on the extension's thread and take the editor lock only to read or apply.
  Lock order: the editor lock, then the host's lock. Notifications to the frontend are sent after
  the host's lock is released.
- **Edits** go through `Document::apply_edits`: one `ChangeSet`, one undo step, line endings
  normalized, selections mapped. A successful edit syncs the document (so extensions see it) and
  tells the frontend to refresh.
- **Diagnostics** from extensions are stored per extension and document, like LSP diagnostics,
  and merged into views, hover cards, Problems, and the status bar's counts.
- **Renamed documents.** When a document's path changes (renamed or moved from the explorer, or
  saved under another name), extensions get `document-closed` and then `document-opened` with
  the new path and language, and its extension diagnostics are dropped until they publish again.
- **Files** (API 1.3, `files.rs`). The `files` interface reads, lists, and writes files in the
  open folder, which the app provides with its `files.exclude` filter
  (`Embedder::open_folder`). Paths are resolved and must be inside the folder, not excluded, and
  not in `.git`. A file open in the editor goes through its document: reads include unsaved
  changes, and writes become one undoable change, saved (`Embedder::save`, which never asks or
  overwrites a file changed on disk) only if the document had no unsaved changes.
- **Programs** (API 1.6, `processes.rs`; [ADR 0026](adr/0026-programs-trust-and-explorer-marks.md)).
  An extension runs only the programs its manifest declares (`[[programs]]`), with the
  `processes` permission, while the open folder is trusted. Each runs in a `flareite-process`
  group with its output collected on threads (32 MiB). `run` waits within the call's deadline;
  `spawn` returns at once, and a thread waits and queues the result as a background command
  (`Event::background`, which a full queue still takes). The worker's `ProcessTable` stops what's
  left when the instance ends.
- **Watching** (`workspace.rs`). Watches inside the open folder use the app's workspace watcher
  (`ExtensionHost::files_changed`); a path outside it gets a `WorkspaceWatcher` of its own.
  Changes for a watch whose command is still queued merge into it.
- **Explorer marks** (`marks.rs`) are kept per extension, replaced whole, and fetched by the
  frontend on `FileMarksChanged`; colors are sent as theme tokens (`<extension id>.<name>`) the
  frontend resolves for the theme.
- **Views and prompts** (API 1.7, `views.rs`, `prompts.rs`;
  [ADR 0027](adr/0027-tree-views-and-prompts.md)). Tree items, children loaded on demand, text
  boxes, welcome messages, badges, and flags live in each view's `ViewContent`; the frontend
  fetches it once per change for a view's header and items (`src/state/views.ts`). Commands from
  view menus carry the selection (`Payload::Selection`), `on_expand` the item
  (`Payload::Expand`), and a prompt's command the answer (`Payload::Answer`). One prompt is
  pending at a time; a new one cancels it.
- **Provided documents** (API 1.8, `documents.rs`;
  [ADR 0028](adr/0028-diffs-and-provided-documents.md)). `ExtensionHost::provide_text` runs a
  text provider's command with `Payload::Provide` and waits for its answer; the app opens the
  result as a read-only core document (`src-tauri/src/editor/provided.rs`) and asks again on
  `ProvidedChanged`.
- **Layouts** (API 1.15, `layouts.rs`, `layout_tabs.rs`; [ADR 0038](adr/0038-layouts.md)). The
  host checks what `set-layout` and `set-pane-decorations` give (`LayoutSpec`,
  `PaneDecorations`, converting columns to UTF-16 with the tab's documents) and keeps it per open
  tab (`TabId`: the layout's full id and the key). Webview and file page panes are webviews kept
  with the previews' (`PreviewRecord::pane`), so serving, state, and messages work as for other
  pages; a file page is served by `webview::serve_file_page` from the open folder, with open
  documents' text (`Shared::live_text`), only when it's trusted. The layout's command runs with
  `Payload::Layout` (`Fill`, `Invoked`, `Message`). In the workbench a `layout:` dock item
  (`src/state/layouts.ts`) fetches the tab's layout or asks for it (`extension_layout_fill`),
  opens its documents hidden and reports them (`extension_layout_ready`), and keeps them open
  while it shows them; `src/editor/LayoutView.tsx` draws splits, headers, editor panes
  (`EditorView` with zones, `decorations`, and folds, in views from 2^30 whose folds aren't
  remembered), webview panes, and file pages (`FilePageFrame`). The editor pane in use is
  reported for `layouts::active-pane` (`extension_layout_focus`).
- **Links and web pages** (API 1.15, `links.rs`; [ADR 0039](adr/0039-links-and-web-pages.md)).
  `src/state/linkFinder.ts` finds links in text (paths checked with `path_kinds`, batched across
  callers and cached briefly) for the terminal (`src/terminal/links.ts`), the editor's text, and
  `LinkedText` (the Output view, notifications, hover diagnostics;
  [ADR 0045](adr/0045-links-everywhere.md)). `src/state/links.ts` opens every link (those, and
  links in hover cards and previews) with an extension's `[[link_handlers]]`
  (`ExtensionHost::open_link`, `Payload::Link`) or Flareite's way, as the `workbench.open_*_links`
  settings say. Web page panes are child webviews of the main window
  (`src-tauri/src/web_pages.rs`, Tauri `unstable`), placed by `WebPagePane` over their pane and
  hidden while an overlay (`state/overlays.ts`) is open; navigation goes to the layout's
  `on_navigate` (`ExtensionHost::web_page_navigated`), and `navigate-pane` comes back as
  `Notification::WebPageAction`.
- **Line annotations, the cursor's line, and hidden views** (API 1.13;
  [ADR 0033](adr/0033-blame-repositories-and-performance.md)). The host keeps line annotations
  like code lenses and sends those on a view's lines (`EditorView.annotations`);
  `src/dock/activeFile.ts` reports the active editor's cursor line
  (`extension_selection_changed`), which runs `on_selection_changed` commands; a view's content
  carries `visible`, which `useShownSections` follows.
- **Rendering Markdown** (API 1.14;
  [ADR 0034](adr/0034-official-extensions-in-their-own-repositories.md)).
  `workbench::render-markdown` runs the host's renderer (`markdown.rs`, pulldown-cmark), the
  one READMEs use, and returns preview nodes; it touches no state, so it needs no permission.
- **Code lenses** (API 1.12; [ADR 0032](adr/0032-merge-conflicts.md)).
  The host keeps code lenses with each document like decorations (mapped through edits); views
  carry them (`EditorView.lenses`) and the editor draws them as zones above their lines. (The
  merge editor of that ADR is the Git extension's layout since ADR 0038.)
- **History graphs and the active file** (API 1.11; [ADR 0031](adr/0031-graph-stashes-and-file-history.md)).
  `views::set-graph` keeps graph rows in a view's content, drawn by `src/panels/viewGraph.tsx`.
  The workbench reports the file of the editor in use (`src/dock/activeFile.ts`,
  `extension_active_file`); `ExtensionHost::active_file_changed` runs `on_active_file_changed`
  commands. `prompts::pick-folder` is a prompt the frontend answers from the system's dialog.
- **Signing in and timers** (`askpass.rs`, `timers.rs`;
  [ADR 0030](adr/0030-branches-remotes-and-signing-in.md)). Programs declared with `askpass` run
  with `GIT_ASKPASS`/`SSH_ASKPASS` naming the app's own executable, which, finding
  `FLAREITE_ASKPASS_*` in its environment, sends the question over a local connection to the host
  (`askpass::client_main`, first in `main`). The host asks with a prompt answered to the waiting
  thread (`Shared::ask_user`). Timers wait on threads of their own and run their command with
  `Payload::Timer`.
- **Quick diff** (API 1.9, `quick_diff.rs`;
  [ADR 0029](adr/0029-quick-diff-and-tab-marks.md)). `ExtensionHost::original_of` asks
  `[quick_diff]` extensions for a file's original (`Payload::Original`); the app keeps its text
  (`src-tauri/src/editor/quick_diff.rs`), diffs each document version in the background, and puts
  the changes on view lines as `changes` for the gutter. Reverting edits the document;
  applying runs the extension's `apply` command with the original's new text
  (`ExtensionHost::apply_to_original`, `Payload::Apply`), using `flareite_diff::splice`.
- **Workspace Trust** (`src-tauri/src/trust.rs`). The app decides whether the open folder is
  trusted (`OpenFolder::trusted`), asks the user, and gates language servers itself; trust
  changes restart language servers and run extensions' `on_folder_changed` commands.
- **Limits and failures:**
  - Memory, stack, and table limits come from a `ResourceLimiter`. Besides each instance's limit
    (128 MiB), all running extensions together may use 512 MiB: instances reserve what their
    memories grow to in a shared pool, and return it when they stop.
  - Call deadlines (5 s activation, 10 s commands, 1 s notifications) use epoch interruption
    with a 10 ms ticker.
  - Output is limited (log and message rate limits; read, edit, and diagnostic sizes).
  - A failure stops the instance, removes its diagnostics, marks the extension failed, and shows
    the reason with a **Restart** action.
- **Resource accounting.** The host counts, per extension (`usage.rs`): time spent in calls,
  call count, slowest call, current and peak memory, events dropped from a full queue, and
  compile time. The counters are atomics, kept across restarts. The Extensions view and the
  performance overlay show them. A call that takes more than a quarter of its deadline is logged
  as slow.
- **Benchmarks** run without extensions unless `--extensions` is passed to `scripts/bench.mjs`.

### Workbench contributions

Extensions customize the workbench declaratively ([ADR 0007](adr/0007-workbench-contributions.md)).
The manifest declares contributions (validated in `contributions.rs`), and the workbench renders
them. Extensions fill in content through the `workbench` interface (API 1.1; previews in 1.2), and
viewers through the `viewers` interface (API 1.4).

| Contribution     | Declared                               | Filled in by the extension                          | Rendered by                        |
| ---------------- | -------------------------------------- | --------------------------------------------------- | ---------------------------------- |
| Themes           | base kind + theme file of token colors | (nothing: no code needed)                           | `themes/theme.ts`, `appearance.ts` |
| File icon themes | theme file of icons and associations   | (nothing: no code needed)                           | `components/fileIcons.ts`          |
| Keybindings      | key, macOS key, `editorFocus`          | —                                                   | the command registry               |
| Menu items       | location, command, group, targets      | — (explorer items get the path as argument)         | `components/ContextMenu`           |
| Status bar items | alignment, priority, initial text      | `set-status-item`                                   | `StatusBar`                        |
| Side bar views   | title, icon                            | `set-view-items`, `set-view-message`, `show-view`   | `panels/ExtensionViewPanel`        |
| Decorations      | colors (dark/light), underline, weight | `set-decorations` (ranges per document)             | `EditorView` overlay and line runs |
| Previews         | icon                                   | `show-preview`, `set-preview-content` (nodes)       | `panels/PreviewPane` in a dock tab |
| Viewers          | title, file patterns, command, default | `set-viewer-content` (nodes), when the command runs | `panels/ViewerPane` in a dock tab  |

- **Host state.** Status and view content live in the host's state, keyed by extension. They are
  compared on update (unchanged content sends no event) and removed when the extension stops.
  The frontend fetches them on `extensionStatusItemsChanged` and `extensionViewChanged` events.
- **Decorations** are stored per document like extension diagnostics, mapped through edits, and
  sent with each view for its lines (`decorations: [{ start, end, key }]`). The frontend draws
  backgrounds, borders, and underlines as overlay rectangles, and text color and font styles as
  styled runs. Colors are validated hex colors, applied through CSS variables and inline styles.
- **Themes** apply after their extension loads. The frontend re-resolves `workbench.theme`
  whenever the extension list changes, falling back to the system theme while it isn't loaded.
- **File icon themes** ([ADR 0009](adr/0009-file-icon-themes.md)) are loaded and checked with
  the manifest (`flareite-extension-host/src/icons.rs`). They're left out of the extension list,
  and the frontend fetches the one named by `workbench.icon_theme` with `extensions_icon_theme`
  (`state/iconTheme.ts`). Icons arrive as `data:` URLs and are drawn with `<img>`. Until the
  theme loads, and for anything it doesn't match, the explorer and Go to File use the built-in Lucide icons.
- **Command arguments.** View items and explorer menu items run their command with an argument,
  which the extension reads with `workbench::command-argument()` during that command.
- **Previews** ([ADR 0016](adr/0016-previews.md)) are formatted text sent as a flat list of typed
  nodes. The host checks the tree and resolves links and pictures against the document's folder
  (`flareite-extension-host/src/preview.rs`). Open previews are dock items (`preview:<id>`) that
  follow `state/previews.ts`, as terminals follow theirs. `extension_preview_content` highlights
  code blocks with the editor's grammars, and pictures load through `extension_preview_image`,
  only for files the content names. Links to web addresses open through `open_external`, which
  accepts only `http:`, `https:`, and `mailto:`.
- **Viewers** ([ADR 0023](adr/0023-encodings-and-viewers.md)) show files that aren't opened as
  text, such as images. `openPath` (`state/documents.ts`) shows a file already open as text there,
  else in the tab it already has, else in the viewer that's the default for its name
  (`state/viewers.ts`), else as text. A file that's binary gets the same kind of tab, asking
  first. These are dock items, `viewer:<viewer id>:<path>` (no viewer id for a binary file), saved
  with the layout. When one shows, `extension_viewer_open` creates a preview record for the file
  in the host (`<viewer id>/<path>`, left out of the preview list) and runs the viewer's command
  with the path as its argument. The extension reads the file (`read-viewed-file`, only files open
  in its viewers) and sets the content (`set-viewer-content`), which the tab fetches and draws like
  a preview's, pictures resolved from the file's folder. The tab asks again when the file changes
  on disk (`filesChanged`) or the extension becomes usable again, and the host does when a
  stopped extension starts: its viewers' content went when it stopped, but their records stay
  until their tabs close. **Reopen Editor With** swaps a tab between text and viewers in place.
- **Webviews** ([ADR 0025](adr/0025-webviews.md)) are viewers and previews that show a page of
  the extension's own. `WebviewFrame` loads it in a frame sandboxed with `allow-scripts` from the
  `flareite-webview:` protocol, which `extensions::serve_webview` answers from the host
  (`flareite-extension-host/src/webview.rs`): the extension's files and the viewed file, under a
  random token per tab, with a content security policy, the theme as `--flareite-*` variables,
  and the bridge script (`webview.js`) that gives pages the `flareite` object. Messages, storage
  requests, tab state, action states, and passed-on shortcuts arrive as `postMessage` from the
  frame, which `WebviewFrame` checks and turns into commands (`extension_webview_message`, …);
  posts to pages wait in `state/webviews.ts` until a page loads. Extension storage and saved tab
  states are JSON files under `extension-storage` (`storage.rs`). Tab actions are drawn beside
  the tab (`ExtensionTabActions` in `dock/items.tsx`), and webviews that ask to stay loaded keep
  their frame, hidden, in their group (`useKeptItems`).
- **`reveal`** asks the frontend to activate a document and select a range; **`show-view`** opens
  a view.

## Frontend structure

- **`ipc/`**: typed command wrappers. `call()` normalizes failures to `CommandError`.
- **`state/`**: small observable stores (`createStore` + `useSyncExternalStore`): `documents`
  (tab summaries, order, the active id, and actions), `workspace` (folder and loaded listings),
  `search` (find-in-files results), `layout` (persisted), `appearance` (theme and font),
  `editorStatus`, `extensions` (statuses, the commands they contribute, permission prompts), and
  `notifications` (which can carry action buttons, and record warnings and errors in
  `problems`), and `output` (the log channel shown). Each component subscribes only to what it reads, so a
  keystroke re-renders the editor view and status bar, not the workbench.
- **`dock/`**: where everything shows ([ADR 0012](adr/0012-docking.md)).
  - `model.ts`: the tree of splits and tab groups, as pure functions.
  - `dock.ts`: the store. It follows the documents and terminals stores, places new items, tracks
    the current group, hides and shows groups, and saves the tree (documents by path).
  - `DockArea.tsx` and `items.tsx`: rendering, with splitters (`Sash`), tab strips, drop targets
    with a drop preview, the tab menu, and each item kind's tab, content, and buttons.

  Dragging (`components/drag.ts`, [ADR 0022](adr/0022-file-drops-and-pointer-drags.md)) is on
  pointer events, not HTML5 drag and drop: tabs, activity bar buttons, and explorer rows, and
  files dragged in from other programs, which the backend reports (`file_drop.rs`). Elements
  register as drop targets (`useDropTarget`), and the one under the pointer takes the drag.

  Views and pages (`app/pages.ts`, such as the settings editor) are shown through
  `state/viewRequests.ts`, so state modules needn't import the dock.

- **`commands/`**: the command registry, keybinding matching, fuzzy matching, and the built-in
  commands. `editor/activeEditor.ts` exposes the active editor to commands (find, go to line,
  next match) without coupling them to React. Several editors can be on screen (one per group):
  each registers by document (`editorFor`), and the active editor is the one showing the active
  document.
- **`editor/EditorView`**: a scroll container sized to `lineCount × lineHeight`, plus the view's
  height less one line so the last line can scroll to the top. Only the
  viewport's lines are in the DOM, positioned with a single `translateY`. Each line is a run of
  text nodes, plain or in `tok-*` spans for highlighting. Carets, selection rectangles, find
  matches, and diagnostic squiggles are measured from the DOM with `Range` after each render
  (`editor/measure.ts` maps UTF-16 columns across a line's text nodes), and clicks map back to
  columns with `caretPositionFromPoint`, so proportional glyphs, tabs, and wide characters line
  up without a custom text-layout engine.
- **`editor/Breadcrumbs`**: the file's path above each editor. Inside the workspace, each
  part is a button that reveals it in the explorer and focuses the tree. Other files show their
  full path, and untitled ones their title.
- **Explorer auto-reveal** (`panels/explorerState.ts`): when the active file or the workspace
  changes, the explorer expands the file's folders and loads their listings (`revealPath`). It
  then makes the file the focused row and scrolls it into view once it is listed. It reacts only
  to a different file, so collapsing a folder or editing doesn't undo what the user did.
- **`editor/Minimap`**: a canvas over the editor's right edge
  ([ADR 0010](adr/0010-minimap.md)). It fetches the lines it shows, plus 100 on each side, from
  `editor_minimap`, at once when scrolled past them and at most every 100 ms while the document
  changes. `editor/minimapGeometry.ts` holds VS Code's geometry: the proportional scroll, the
  slider, click-to-center, and drag scale.
- **`terminal/`**: the terminals (`terminals.ts`), and one `TerminalSession` per terminal
  wrapping xterm.js (`session.ts`, loaded with the first terminal). Each terminal is a dock item.
  Terminal colors and fonts follow the theme tokens and editor settings.
- **Language features**: resting the pointer on text shows a hover card (diagnostics at that
  point plus the server's Markdown, rendered as React elements, never HTML); F12 or Ctrl/Cmd+click
  goes to the definition; Shift+F12 (or Ctrl/Cmd+click on the definition itself) goes to a lone
  reference or opens the references peek (`editor/PeekView`, state in `editor/references.ts`),
  as in VS Code: a zone between the editor's lines, which the row geometry in `EditorView`
  makes room for, with a read-only preview of the selected reference's file (lines from
  `editor_peek_lines`: the open document's, or the file on disk) and a tree of references by
  file. F4 and Shift+F4 move the cursor and the peek to the next and previous reference,
  following it into another file's editor; going somewhere (a definition, a reference, a search
  result, a problem, a symbol) scrolls it to the middle of the editor if it's off screen, as in
  VS Code (`selectRange`'s `centerIfOutside`); completion opens on the server's trigger characters, on typing a word,
  or with Ctrl+Space, and is navigated with the arrow keys and accepted with Enter or Tab. The
  status bar shows the language and the server's state (and the counts of every file's
  problems).
- **Input**: a hidden textarea at the primary caret receives keys, text, IME composition (shown
  inline while composing), and paste. `editor/keymap.ts` maps keys to operations per platform
  (Ctrl on Windows/Linux, Cmd/Option on macOS). Commands' keybindings are handled in the capture
  phase before the editor sees the key.
- **Extensions view** (`panels/ExtensionsPanel`, `Ctrl+Shift+X`): a row per extension, all the
  same height: icon, name, description, author, start time, and state when it needs attention,
  with the rest in the row's tooltip and its actions in a gear menu. Choosing one opens its page
  (`panels/ExtensionPage`, the dock item `page:extension:<id>`): its README, drawn by the
  preview renderer (`PreviewDocument`) from content the backend made with the host's Markdown
  renderer (`markdown.rs`, which extensions also call as `workbench::render-markdown`;
  [ADR 0020](adr/0020-extension-pages.md),
  [ADR 0034](adr/0034-official-extensions-in-their-own-repositories.md)), its permissions
  (checkboxes that grant or deny), commands, contributions, resource use, folder, and errors. The views are the built-in ones followed by those of enabled
  extensions (`app/views.ts`). Problems and Output are panel views: they open along the bottom
  with the terminals, and show and hide with them (**View: Toggle Panel**). The activity bar
  lists everything in the side bar (the groups along the dock's left edge: views, terminals,
  documents), then the views not docked anywhere except panel views (`activityBarItems`). Its buttons show an item, hide the group of an item already showing,
  and can be dragged into the dock; a tab dropped on it joins the side bar.
- **Quick picks** (`openQuickPick` in `components/paletteState.ts`) show a list of choices in
  the palette, filtered by typing: encodings, and the ways to reopen a file.
- **Context menus** (`components/ContextMenu`) are a generic keyboard-navigable menu, opened by
  the editor (clipboard, Go to Definition and References, extension items, Command Palette) and by explorer
  file rows (Open, extension items).
- **Tabs** can be dragged within and between groups, cycled with Ctrl+Tab and
  Ctrl+PageUp/PageDown within their group, and closed with Delete or Ctrl+W (the current group's
  tab). The dock keeps recently closed files, views, and pages with their places, for Reopen
  Closed Tab (`closedTabs`, [ADR 0021](adr/0021-tab-keybindings.md)). Editor tabs remember their
  scroll position while the app runs. Clicking a tab focuses its content. A document can show in
  several groups, once in each, each tab with its own selections, folds, and scroll position
  (split off with Split Tab, or dragged with Ctrl held; [ADR
  0024](adr/0024-a-document-in-several-groups.md)).
- **Theming**: `styles/tokens.css` defines semantic tokens for dark and light themes (including
  find matches, widgets, inputs, lists, and buttons), selected by `data-theme` on `<html>`.
  Components use tokens only. The `workbench.theme` setting picks dark, light, the OS scheme, or
  an extension's theme, which overrides tokens with inline custom properties on `<html>` (as do
  the user's `workbench.colors`). The allowed tokens are listed in `themes/tokens.json`.
  Editor font size, family, line height, and tab width are CSS variables set from settings.
- **Accessibility**:
  - **Roles.** Each dock group is a labelled region, with ARIA tab roles for its tabs, and
    unsaved tabs say so in their accessible name. The explorer and extension views are ARIA `tree`s with arrow-key navigation,
    and the palette is a combobox with `aria-activedescendant`. Find options are toggle buttons,
    errors are announced through `role="alert"`, and the sash is keyboard adjustable.
  - **Screen readers** read the editor's text through its input textarea
    (`editor/inputMirror.ts`). The textarea holds the cursor's line, or the selected lines (up to
    100), with the editor's selection. It's laid over the rendered line, so its text lines up and
    IME windows appear at the cursor. It never edits itself: `beforeinput` is cancelled and
    applied as editor operations, composition text is applied when composition ends, and the
    mirror follows each new view.
  - **Context menus** open from the keyboard (`Shift+F10`, the menu key) at the cursor or the
    focused explorer row.
  - **System settings.** `styles/accessibility.css` handles forced colors (Windows contrast
    themes): carets, selections, matches, focused and selected items, and popup edges get system
    colors or outlines. It also turns off motion when `prefers-reduced-motion` is set.
  - **Automated checks.** axe-core runs over the workbench, its views, the palette, menus,
    notifications, and the editor in the test suite (`app/accessibility.test.tsx`).

## Logging and diagnostics

`flareite-app` installs a `tracing` subscriber during Tauri's `setup` hook, once the platform log
directory is known:

- Output goes to stderr (colored) and a daily rolling file through a non-blocking writer. Seven
  files are retained.
- `FLAREITE_LOG` overrides the default `info` filter. Invalid directives fall back to the default
  with a warning.
- If the log directory can't be created, logging continues on stderr only. Logging problems never
  prevent startup.
- Panics are logged through `tracing` before the default hook runs.
- The file writer is flushed on `RunEvent::Exit`.
- **The Output view's records** ([ADR 0018](adr/0018-output-and-problems.md)) are kept by
  another layer, in memory, in channels: `flareite`, `extension:<id>` (records with an
  `extension` field), and `server:<name>` (a `server` field). Each keeps its newest 2,000
  records. Its per-layer filter keeps `info` and above from Flareite and every level from
  extensions and servers. It looks only at call-site metadata, so Flareite's own debug call sites
  stay disabled. The frontend polls `logs_read(channel, after)` once a second while the view is
  shown, and `logs_file` names today's file for **Open Log File**.
- Document lifecycle events (open, save, close) are logged at `info`, file errors at `warn`, and
  frontend errors are forwarded to the backend log.

## Instrumentation

- **Backend:** named timing histograms (`flareite_tasks::Metrics`; power-of-two buckets, O(1)
  memory) for `editor.apply`, `editor.view`, `document.load`, `document.save`,
  `workspace.index`, `workspace.search`, and `search.count`. The scheduler also keeps wait and run
  times per task type, with completed, cancelled, dropped, and panicked counts.
- **Frontend** (`src/state/perf.ts`): series for IPC round trips, render and measure time,
  input-to-paint latency, and lines received per update.
- **Overlay:** **Developer: Toggle Performance Overlay** shows both, plus backend and JS heap
  memory and open document sizes.
- **Benchmarks:** see [`performance.md`](performance.md) for the benchmark suites and the
  measured baseline.

## Startup measurement

`run()` records an `Instant` on entry. The backend logs `elapsed_ms` when setup finishes and when
the frontend first calls `frontend_ready`. `scripts/bench.mjs startup` measures this
repeatably; see [`performance.md`](performance.md) for the current numbers. Files from the
previous session and the command line now open on the scheduler, so they no longer delay the
window.

Single, warm-cache runs on the development machine (Windows 11, x86_64), for orientation only:

| Build / scenario                                        | `backend initialized` | `frontend ready` |
| ------------------------------------------------------- | --------------------- | ---------------- |
| M1 debug                                                | 490 ms                | 673 ms           |
| M1 release                                              | 332 ms                | 372 ms           |
| M2 debug, 11 MB / 200,000-line file on the command line | 333 ms                | 629 ms           |
| M3 debug, this repository restored as the workspace     | 422 ms                | 584 ms           |

Indexing this repository (149 non-ignored files) took 13 ms on the index thread, after the
window was already interactive.

The M1 release `flareite.exe` process had a working set of about 30 MB. WebView2 runs in separate
processes, which this figure excludes.

## Testing and CI

| Layer                  | What runs                                                                                    |
| ---------------------- | -------------------------------------------------------------------------------------------- |
| Rust unit tests        | Every crate; `cargo test --workspace`                                                        |
| Extension host         | `crates/flareite-extension-host/tests/host.rs` builds the WebAssembly fixtures and runs them |
| Performance budgets    | Work counted, not timed (lines and bytes per keystroke and page in a 200,000-line document)  |
| Frontend               | Vitest with jsdom: components, state, the editor session, axe accessibility checks           |
| Benchmarks, end to end | `scripts/bench.mjs` against the release build; `scripts/perf-check.mjs` against a baseline   |
| Microbenchmarks        | Criterion; `perf-check.mjs micro` reads its comparison with a saved baseline                 |

- **CI** (`.github/workflows/ci.yml`) runs formatting, Clippy with warnings as errors, and all
  tests on Windows, macOS, and Linux, plus the frontend checks. Each Criterion benchmark runs
  once there to keep it working. Timing comparisons don't run in CI, because shared runners are
  too noisy; the work-counting budgets do.
- **Platform-specific code** is small and listed here:
  - `paths.rs`: Windows `\\?\` prefixes.
  - `file.rs`: Unix permission bits; the Windows rename retry.
  - `keymap.ts` and the keybinding registry: `mod` means Cmd on macOS.
  - `bench.mjs`: process memory through `ps` or CIM.
  - `shell.rs` (terminal): the default shell per platform; `terminal.rs` sets `TERM` outside
    Windows. ConPTY and Unix ptys are behind `portable-pty`.
  - `session.ts` (terminal): clipboard keys per platform.
  - Lock files use the standard library's portable locks.

## Known limitations

These are tracked for later milestones:

- Lines longer than 16 KiB are shown only up to that point (with a badge). The cursor can't be
  seen beyond it, though editing there works. Visual columns past 4 KiB count characters, not
  tab stops or grapheme clusters.
- Files load completely into memory, whatever their size, taking about twice their size while
  they load; there's no lazy or partial loading. The editor scrolls through one element sized
  to all its lines, and browsers limit an element's height (to about 33 million pixels in
  WebView2), so lines past roughly 1.6 million (at the default line height) can't be scrolled
  into view, though the cursor, editing, search, and saving reach them.
- Encodings are detected from the whole file's bytes (the guess from its first MiB); there's no
  setting for a default encoding, and UTF-32 isn't supported.
- Replace across files, include/exclude globs in the search panel, and search-result context
  lines aren't implemented.
- The explorer can't rename, move, or delete files yet. Large directories aren't
  virtualized.
- Only files inside the open folder are watched. Open files outside it don't reload on external
  changes, but saving still detects conflicts.
- Single folder only; multi-root workspaces aren't supported.
- Scroll positions are remembered per tab only while the app runs. Unsaved changes aren't kept
  across a normal close (closing asks about them); they're backed up only for crash recovery.
- Language features are limited to diagnostics, hover, go to definition, references, symbols,
  and completion. The references peek's preview is read only (VS Code's can be edited), and its
  references and anchor are where they were when it opened, not moved by later edits; there's
  no Find All References view. Rename, code actions, formatting, signature help, and semantic tokens aren't
  implemented, nor are completion's additional edits (such as auto-imports) or snippets
  (completion asks servers for plain text). There's no problems panel; diagnostics for files that
  aren't open aren't shown.
- Highlighting uses each grammar's own query. Of the example languages only HTML and Markdown
  have injection queries, and there's no language detection beyond file names. A
  WebAssembly grammar's external scanner can't be interrupted mid-call.
- Extensions have no storage API and can't depend on other extensions. Components that import
  WASI are rejected. There's no installer; extensions are installed by copying their folder.
- Contributed views are trees of text items, and previews and viewers are formatted text from a
  fixed set of elements; viewers can't edit their files, and there are no custom interactive views, inline text (inlays), gutter icons, or
  decoration hover text. Menus exist only in the editor and the
  explorer (files), and keybindings support only the `editorFocus` condition.
- There's no keybindings UI; keybindings are edited as TOML. The settings editor doesn't edit
  tables of entries (theme colors, language servers, terminal profiles, keybindings).
- Screen readers read the cursor's line and selection. Diagnostics under the cursor, hover cards,
  and completion items aren't announced yet. Forced-colors mode shows syntax in the system's
  text color.
- Multi-line Tab/Shift+Tab indentation, auto-closing brackets, and copying the whole line when
  nothing is selected aren't implemented. Alt+drag doesn't create column selections.
- Terminals survive a window reload but not quitting the app. There is no shell integration,
  and extensions can't create terminals. Replaying a full-screen program after a reload can look
  garbled until it redraws.
- A document shows in one dock group at a time. There are no floating or separate windows, and
  groups keep shares of the window rather than fixed widths.
- On Linux, watching very large trees can exhaust the inotify watch limit; watching then fails
  with a logged warning and the workspace works without live updates.
