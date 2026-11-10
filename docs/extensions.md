# Writing extensions

Flareite extensions are WebAssembly components. They run in a sandbox inside the editor and can
do only what the [extension API](../crates/flareite-extension-api/wit/extension.wit) offers:

- register commands;
- read open documents and get notified of changes;
- edit documents through validated, versioned edits;
- read, write, and delete files in the open folder, such as every file in a folder chosen in the
  explorer, and open files in the editor;
- publish diagnostics (squiggles, hover messages, and entries in the Problems view);
- customize the workbench: color themes, keybindings, context menu items, status bar items,
  side bar views (trees with buttons, menus, and a text box, as sections of one activity bar
  entry if they like), editor decorations, and previews (formatted pages, such as rendered
  Markdown);
- ask the user to choose from a list, type a line of text, or confirm (API 1.7);
- provide read-only documents at addresses of their own, such as a file's version in a commit,
  and open tabs comparing two documents (API 1.8);
- name the originals files are compared with as they're edited, marking changed lines beside
  the text, and take changes the user applies to them, as Git stages them (API 1.9);
- show kinds of files in viewers: tabs that show a file the extension's way, such as an image,
  instead of as text;
- show pages of their own (HTML, CSS, and JavaScript) in viewers and previews: webviews, which
  run sandboxed and talk to the extension by messages;
- split tabs into editors, pages, files shown as web pages, and pages from the web, with buttons
  above them, and draw in those editors: layouts, as Git's diff and merge editors are (API 1.15);
- open the links the user Ctrl+clicks in the terminal, the editor, the Output view, and previews
  their own way,
  instead of the browser or the editor (API 1.15);
- keep small values across sessions (storage);
- run programs the manifest declares, such as Git, in folders the user trusts (API 1.6);
- watch files for changes, and hear when the open folder changes (API 1.6);
- mark files and folders in the explorer, with colors themes can change (API 1.6);
- add languages: syntax highlighting from Tree-sitter grammars, and language servers, with
  arguments and settings their code can change (API 1.16);
- provide schemas for TOML, JSON, and YAML files, which check them as they're edited and complete
  their keys and values (API 1.16);
- read their own settings, and change them as the user chooses (API 1.7);
- log, and show messages.

There is no network, environment, or clock access, no file access beyond the open folder's files
(webview pages can reach the web only with the `network` permission), and no programs but those
the manifest declares. A manifest declares the permissions an extension needs, and the user
grants them before the extension starts.
(Language servers an extension declares are started by Flareite, not by the extension, and only
once the user allows it.)

This guide uses these examples:

- [Tidy](../extensions/tidy/) flags trailing whitespace and long lines, and has commands to trim
  trailing whitespace and to count words. Its explorer menu item trims every file in a folder.
- [TODOs](../extensions/todos/) uses every workbench contribution: it highlights TODO comments,
  lists them in a side bar view, counts them in the status bar, and adds a keybinding and a
  context menu item.
- [Example Themes](../extensions/themes/) contributes two color themes and no code at all.

It also refers to the official extensions (`flareite.*`), which have repositories of their own:

- **HTML & CSS** adds three languages (HTML, CSS and SVG), highlights CSS and JavaScript inside
  HTML and SVG, and declares language servers for HTML and CSS, with no code; its code is two
  [layouts](#layouts) that show an HTML file as a page, beside its editor or in a tab of its own. Flareite has no languages of
  its own: Rust, TypeScript & JavaScript, JSON, TOML, XML, and Python are extensions like it.
- **Markdown** is a language too, and has code: a preview that renders the document as you type.
- **Luau** is a language whose code [configures its language server](#configuring-servers)
  (luau-lsp, with Roblox's types, which Flareite downloads) from its settings, and runs Rojo to
  keep a project's sourcemap up to date.
- **Images** shows image files in a webview viewer: a page that draws the picture on a background
  you choose, fit to the tab or zoomed, with its format and size.
- **Browser** opens web links in tabs that show the page: a [link handler](#links) and a
  [layout](#layouts) with a [web page pane](#web-page-panes).
- **Git** runs Git to mark changed files in the explorer, show the branch in the status bar, and
  fill its Source Control view, where changes are staged and committed:
  [programs](#programs), [watching files](#watching-files), [explorer marks](#explorer-marks),
  [view containers](#view-containers), [tree items](#tree-items), and [prompts](#prompts). Its
  diff and merge editors are [layouts](#layouts).

## Quick start

You need Rust with the `wasm32-unknown-unknown` target:

```sh
rustup target add wasm32-unknown-unknown
cd extensions
cargo build --release          # builds every extension in the workspace
```

To load the result in Flareite:

1. Run **Extensions: Load Development Extension…** from the command palette.
2. Choose `extensions/tidy`. This adds the folder to `extensions.development` in your settings.
3. Flareite asks whether to allow the permissions Tidy requests.
4. Open a file with trailing spaces: they are underlined, and **Tidy: Trim Trailing Whitespace**
   removes them.

Tidy's manifest has a [`[build]`](#building-as-you-edit), so from then on Flareite rebuilds it
whenever a file in its folder changes, and loads the new build: edit `src/lib.rs`, save, and the
change is running a moment later. An extension without one is rebuilt by hand; click **Restart**
in the Extensions view (`Ctrl+Shift+X`) to load the new build. To pick up new or removed
extensions, use **Extensions: Reload Extensions**. There's no need to restart Flareite.

## Anatomy of an extension

```text
my-extension/
  extension.toml          the manifest
  Cargo.toml              crate-type = ["cdylib"], depends on flareite-extension-api
  src/lib.rs              implements `Guest` and calls `export_extension!`
```

```rust
use flareite_extension_api::types::*;
use flareite_extension_api::{Guest, export_extension, host};

struct Hello;

impl Guest for Hello {
  fn activate(_context: Activation) -> Result<(), String> {
    Ok(())
  }
  fn deactivate() {}
  fn run_command(_command: String, _context: CommandContext) -> Result<(), String> {
    host::show_message(MessageLevel::Info, "Hello from an extension!");
    Ok(())
  }
  fn document_opened(_document: DocumentInfo) {}
  fn document_changed(_change: DocumentChange) {}
  fn document_closed(_document: DocumentId) {}
}

export_extension!(Hello);
```

Build it as a `cdylib` for `wasm32-unknown-unknown`. The output is a core module that already
carries its interface description, embedded by wit-bindgen. Flareite turns it into a component
when it loads it, so you need no other tools.

## The manifest

```toml
id = "example.tidy"               # publisher.name: lowercase letters, digits, '-'
name = "Tidy"
version = "0.1.0"                 # semantic version
description = "Flags trailing whitespace and overlong lines."
author = "Flareite"                # optional; shown in the Extensions view
icon = "icon.svg"                 # optional: a PNG or SVG file in its folder, at most 256 KiB
api_version = "1"                 # the extension API version it was built for
entrypoint = "tidy.wasm"          # relative to this file
activation = ["on-startup"]       # see below; default: when one of its commands runs

[engines]
flareite = ">=0.1.0"               # optional semver requirement on the editor

[permissions]
documents_read = true
documents_write = true
files_read = true
files_write = true

[[commands]]
id = "example.tidy.trimTrailingWhitespace"   # must start with the extension id
title = "Trim Trailing Whitespace"
category = "Tidy"                 # optional; the extension's name by default

[configuration.max_line_length]
type = "integer"                  # boolean, integer, number, or string
default = 100
minimum = 0                       # numbers: optional minimum and maximum
description = "Lines longer than this are flagged; 0 turns the check off."

[configuration.severity]
type = "string"
default = "warning"
choices = ["error", "warning", "information", "hint"]   # strings: optional
```

While you edit `extension.toml`, Flareite checks it against its schema,
[`extension.tosd`](../crates/flareite-schema/schemas/extension.tosd) (a
[TOML Schema](https://tomlschema.org)), underlining mistakes, completing keys and values, and
describing each key on hover. The loader has the last word, and checks more (that files exist,
that commands are declared).

Flareite validates a manifest completely before loading anything. Any of these problems marks the
extension invalid in the Extensions view, with a message:

- unknown fields;
- an `api_version` other than `1`, or an unmet `engines.flareite`;
- malformed ids, or command ids outside the extension's namespace;
- an activation event naming an undeclared command;
- a configuration default of the wrong type.

**Activation events:**

| Event                          | When the extension starts                          |
| ------------------------------ | -------------------------------------------------- |
| `on-startup`                   | When Flareite starts (or the extension is loaded). |
| `on-language:<id>`             | When a document of that language is opened.        |
| `on-command:<id>`              | When that command runs.                            |
| `on-view:<id>`                 | When one of its side bar views is shown.           |
| `on-folder-contains:<pattern>` | When a file in the open folder matches (API 1.17). |

Every extension also starts when one of its commands runs, so `on-command` is only
documentation.

**`on-folder-contains`** (API 1.17) is for extensions that serve a kind of project rather than a
language, as VS Code's `workspaceContains`: a Rojo extension starts with
`on-folder-contains:*.project.json`. The pattern is a glob relative to the open folder, with `/`
between names: `*` and `?` stay within a name, `**` matches any number of folders, and `{a,b}`
either. The folder is searched when it opens or the extension loads, only as deep as the pattern
reaches (8 levels with `**`, and at most 10,000 entries), leaving out what `files.exclude`,
`.gitignore` files, and `.git` leave out. A matching file created later starts it too. Language ids are those extensions add (see [Languages](#languages)), such as
`rust`, `typescript`, `typescriptreact`, `javascript`, `json`, `toml`, `python`, `html`, `css`,
`luau`, and `markdown` in the official extensions.

**Entry point.** An installed extension's entry point must be inside its folder. A development
extension may point elsewhere, such as Tidy's
`../target/wasm32-unknown-unknown/release/tidy.wasm`. The file can be at most 32 MiB.

### Building as you edit

A development extension can say how it's built, and Flareite builds it in place: when it's loaded
(at startup, or when you add its folder), and again whenever a file in its folder changes. After a
build succeeds, the extension is loaded again with its manifest read anew, and restarted if it was
running. Other extensions are left as they are.

```toml
[build]
command = "cargo"                 # a program on the PATH, or a path relative to this folder
args = ["build", "--release", "--package", "tidy"]
watch = ["src", "Cargo.toml"]     # optional: which changes start a build
```

- **Only for development extensions.** An installed extension may keep its `[build]` (it's the
  same manifest), but it never runs. Builds run with your environment and without a console
  window, in the extension's folder, and must exit: a watch mode (`cargo watch`, `tsc --watch`)
  never finishes. Versions of Flareite from before `[build]` existed reject it as an unknown
  field, so a published extension that keeps it can't be installed in them.
- **What starts a build.** Any file in the folder, except in `.git`, ignored by a `.gitignore`
  (the folder's own, and its parents' up to the repository's root), or the entry point itself,
  so the build's own output doesn't start another. `watch` narrows that to the files it matches:
  a pattern without a `/` matches a file or folder name at any depth, one with a `/` the path
  from the extension's folder, like `files.exclude`. Editing the `[build]` itself is used by
  the next build. `.gitignore` files are read when the folder starts being watched.
- **One at a time.** Changes made while it builds start one more build after it, not one each,
  and don't stop it. Builds aren't timed out.
- **Even when invalid.** The `[build]` is read even when the rest of the manifest is invalid, so
  a fresh checkout without its `.wasm` builds, then loads. The first build after starting only
  loads the extension again if it changed the entry point, or the extension wasn't loaded.
- **Seeing it.** The build's output goes to the extension's log (**Show Build Log**). The
  Extensions view shows a build that's running or failed, and the extension's page how the last
  one went, with **Build** to run one now. A failure is announced once, until a build succeeds;
  the last good build stays loaded.

**Hidden commands.** A command with `hidden = true` is left out of the command palette, for
commands run only from view items or other contributions.

**The Extensions view** shows the `icon` (square works best; it's shown from 36 to 88 pixels),
`name`, `description` (on one line, cut if long), and `author`. Choosing the extension opens its
page, which shows the `README.md` in its folder (any case), rendered like a Markdown preview
(by the same renderer as [`render-markdown`](#previews)):
relative links open files from the extension's folder, pictures load from it, and HTML is shown
as its text. A README can be at most 1 MiB. Tidy and TODOs have both.

**Dependencies** between extensions aren't supported in API version 1.

## Permissions

| Manifest key       | Allows                                                                           |
| ------------------ | -------------------------------------------------------------------------------- |
| `documents_read`   | Listing and reading open documents, change notifications, publishing diagnostics |
| `documents_write`  | Editing open documents                                                           |
| `files_read`       | Listing and reading [files](#files) in the open folder (API 1.3)                 |
| `files_write`      | Changing and creating files in the open folder (API 1.3)                         |
| `language_servers` | Flareite running the extension's [language servers](#language-servers)           |
| `network`          | [Webviews](#webviews) loading from the web; [downloads](#downloads) (API 1.17)   |
| `processes`        | Running the [programs](#programs) it declares, in trusted folders (API 1.6)      |
| `web_pages`        | Showing pages from the web in its [layouts](#web-page-panes)' tabs (API 1.15)    |

Commands, settings, logging, messages, storage, [secrets](#secrets), languages, viewers,
webviews, explorer marks, and the open folder's path need no permission.

**Requesting a permission doesn't grant it.** When an extension requests permissions the user
hasn't decided about, Flareite asks before starting it. The user can allow or deny each
permission, now or later in the Extensions view. Decisions are stored in
`extension-permissions.json` next to `settings.toml`.

An extension with denied permissions still runs. Calls that need them fail with
`permission-denied`, and it gets no document notifications without `documents_read`.
`activate` receives the granted permissions, so the extension can adapt (except `files_read`
and `files_write`, which the API 1.0 `permission` type can't name: a call tells by failing with
`permission-denied`). Changing permissions restarts the extension.

## The API

The full interface, with comments, is
[`extension.wit`](../crates/flareite-extension-api/wit/extension.wit). Functions the extension
calls are in `host`, `workbench`, `files`, `viewers`, `webviews`, `storage`, `workspace`,
`processes`, `explorer`, `views`, `prompts`, `documents`, `quick-diff`, `timers`, `layouts`,
`links`, `schemas`, `language-servers`, `downloads`, and `secrets`; the extension exports the
rest.

### Positions and versions

**Positions.** A position is a zero-based **line** and a **column in UTF-8 bytes** from the start
of that line. Rust strings index the same way. Ranges are `start` up to (not including) `end`.

**Versions.** Every document has a **version** that increases with each change. Reads return the
version they read. Edits and diagnostics name the version they refer to.

### Reading documents

- `documents()` and `document(id)` return each document's id, version, path, language, line
  count, length in bytes, and whether it is unsaved.
- `read-lines(id, start, end)` returns lines without their line breaks. It may return fewer
  lines than asked for: at most 10,000 lines or 4 MiB at once, so loop until you have them all.
- `read-range(id, range)` returns the text of a range, up to 4 MiB.

Read only what you need. Tidy reads whole documents once, then only the lines each change
touches.

### Notifications

- `document-opened(info)` is sent for every open document when the extension starts, and for
  each document opened later.
- `document-changed(change)` lists the changes since the previous notification, in order. Each
  change is a range in the text before it, plus the text that replaced it, like LSP incremental
  sync but in UTF-8 columns. If an extension falls behind, waiting changes are merged. If they
  grow past 1 MiB, or events had to be dropped, `changes` is `none`: read the document again.
- `document-closed(id)` is sent when a document closes.

Notifications are delivered one at a time, in order, on the extension's own thread. The editor
never waits for them.

### Editing

`apply-edits(id, version, edits)` applies all the edits or none. The host checks each of these
first, and returns a specific error when one fails:

- the extension has `documents_write` (`permission-denied`);
- the document is open (`unknown-document`);
- it is still at `version` (`stale-version`, carrying the current version);
- every position is inside the document and on a character boundary (`invalid-range`);
- edits don't overlap (`invalid-range`);
- sizes are within limits: at most 10,000 edits and 16 MiB of inserted text (`too-large`).

Ranges refer to the text at `version`, so they don't shift as earlier edits apply. Line breaks
in inserted text are converted to the document's line ending. The whole call becomes **one undo
step**.

When the user is typing, edits based on a slightly old version fail with `stale-version`. Read
again and retry, as Tidy's trim command does (three attempts).

### Diagnostics

`publish-diagnostics(id, version, list)` replaces the extension's diagnostics for a document.
Each diagnostic has a range, a severity, a message, and an optional code.

- **Version.** It may be the current version or one of the last eight sent to the extension.
  Ranges are converted in that text, then move with later edits.
- **Limit.** At most 5,000 diagnostics per call.
- **Display.** They appear as squiggles, in hover cards (labeled with the extension's name), in
  the Problems view, and in the status bar's counts.
- **Removal.** They are removed when the extension stops.

### Files

The `files` interface (API 1.3) works on files in the folder open in the editor, such as every
file in a folder the user chose in the explorer. Paths are absolute.

- `folder()` returns the open folder, or `none`.
- `list-files(folder)` returns the files in a folder and its subfolders, sorted: at most 10,000,
  with `truncated` set if there were more. Like Go to File, it leaves out what `files.exclude`
  hides and what Git ignores.
- `read-file(path)` returns a file's text, line breaks included, decoded from its encoding (UTF-8,
  UTF-16, or a legacy one such as Windows-1252, detected as the editor does). A file open in the
  editor is read from its document, unsaved changes included, so the extension sees what the
  user sees.
- `delete(path, to-trash)` (API 1.7) deletes a file, or a folder and everything in it: to the
  system's trash (the Recycle Bin on Windows), where the user can restore it, or permanently.
  If the trash can't take it, nothing is deleted and the call fails. The open folder itself
  can't be deleted.
- `write-file(path, text)` replaces a file's text, or creates the file in an existing folder.
  Line breaks are converted to the file's line ending (a new file gets them as written), and the
  text is written in the file's encoding (a new file in UTF-8). A file
  open in the editor is changed in its document, as one undo step, and saved unless it had
  unsaved changes (those stay the user's to save). Writing the text a file already has changes
  nothing.

Reading needs `files_read`, and writing and deleting `files_write`. Only files inside the open folder can be
used: paths are resolved (links and `..` included) first, and files that `files.exclude` hides,
and anything in a `.git` folder, are refused too. Failures are a `file-error`:
`permission-denied` (naming the permission), `outside-folder`, `not-found`, `wrong-kind` (a
folder where a file is needed, or the other way round), `not-text` (binary), `too-large`
(more than 16 MiB), or `failed` with the reason (such as a character the file's encoding
lacks).

A command has 10 s, so a command that works through many files should do little per file. Tidy's
**Trim Trailing Whitespace in Files** trims a few thousand files well within that.

### The open folder and trust

The `workspace` interface (API 1.6) tells an extension about the open folder:

- `folder()` returns its path and whether the user trusts it, or `none`. It needs no permission.
- `on_folder_changed = "<command>"` in the manifest names a command that runs when the open
  folder changes (opened, closed, or another one opened) or the user changes whether they trust
  it. It runs only while the extension is running; an extension started later reads `folder()`
  in `activate`.
- `request-trust()` asks the user to trust the folder, naming the extension. Call it when the
  user does something that needs trust, not on its own: the user is asked when the folder opens
  anyway.
- `open-file(path)` (API 1.7) opens a file in the editor and shows it, as if the user opened it
  (in its viewer, if one is the default for it). It needs no permission.
- `open-folder(path)` (API 1.11) opens a folder in place of the open one, as if the user chose
  it: they're asked whether to trust it as usual. Git opens a repository it cloned this way.
- `open-externally(path)` (API 1.17) opens a file in the open folder with the system's default
  app for its kind, as Rojo opens a place it built in Roblox Studio. It needs the `processes`
  permission and a trusted folder, as running a program does, and refuses programs and scripts
  (`.exe`, `.bat`, `.ps1`, `.sh`, `.app`, shortcuts, and on Unix executable files), which
  opening would run.
- `workbench::active-file()` (API 1.11) returns the file of the editor in use: a document's, a
  viewer's, or a diff's modified file. `on_active_file_changed = "<command>"` in the manifest
  names a command that runs when it changes. Moving to a side bar view keeps it, so a view can
  follow the file being edited, as Git's File History does.
- `on_selection_changed = "<command>"` (API 1.13) in the manifest names a command that runs when
  the active editor's cursor moves to another line, or another document is in use, with that
  document and its primary selection as the command's context (`command-context`). Git shows
  the cursor line's blame this way.

**Workspace Trust.** Programs that work on a folder may run code from it: a language server
builds the project, and Git runs hooks and the programs its configuration names. So until the
user trusts a folder, [programs](#programs) don't run there, and neither do language servers.
Everything else works. With no folder open, nothing needs trust.

### Watching files

`workspace::watch(path, command)` watches a file, or a folder and everything in it, and returns
the watch's id. When something under it changes, `command` (one of the extension's own) runs, and
`workspace::changes()` returns the watch, the changed paths (absolute and sorted), and
`overflow` if more than 10,000 changed (then `paths` is empty: look at everything again).
Changes that arrive while the extension is busy are merged into one call. `unwatch(id)` stops a
watch, and watches stop with the extension.

- **Inside the open folder**, watching needs `files_read`, `.git` included, and lasts while the
  folder is open. It uses the changes Flareite watches for anyway.
- **Outside it** needs `processes`, whose programs could read the path anyway. Git uses this for
  the `.git` folder of a repository the open folder is inside.
- At most 32 watches.

```rust
fn run_command(command: String, _: CommandContext) -> Result<(), String> {
  match command.as_str() {
    "flareite.git.start" => {
      let folder = workspace::folder().ok_or("no folder is open")?;
      workspace::watch(&folder.path, "flareite.git.changed").map_err(|e| format!("{e:?}"))?;
    }
    "flareite.git.changed" => {
      let changes = workspace::changes().ok_or("not run for changes")?;
      // ... look again at changes.paths ...
    }
    _ => {}
  }
  Ok(())
}
```

### Programs

The `processes` interface (API 1.6) runs programs on the user's computer. The manifest declares
each one, and the `processes` permission, which the user grants seeing the list:

```toml
[permissions]
processes = true

[[programs]]
name = "git"                      # what the code calls it; also the command by default
askpass = true                    # optional: it may ask the user to sign in (see below)

[[programs]]
name = "prettier"
command = "bin/prettier"          # optional: on the PATH, or a path in the extension's folder
workspace = "node_modules/.bin/prettier"   # optional: preferred when the open folder has it
```

Users can set where a program is in `settings.toml`, by `<extension id>.<name>`:

```toml
[extensions.programs]
"flareite.git.git" = "C:/Program Files/Git/cmd/git.exe"
```

- **`run(program, args, options)`** runs a program and waits for it. It returns its exit code, its
  standard output and error (32 MiB together; `truncated` says if more was dropped). It stops the
  program before the running command's deadline, with `timed-out`.
- **`spawn(program, args, options, command)`** starts a program and returns its id at once.
  When it ends, `command` runs, and `processes::finished()` returns the id and the outcome. Use
  it for anything that may take a while. At most 16 run at once.
- **`spawn-streaming(program, args, options, command, output)`** (API 1.17) starts a program
  as `spawn` does, for programs that run until they're stopped, such as servers and watchers: its
  output is handed over line by line as it's written. `output`, another of the extension's
  commands, runs with the lines written since it last ran, which **`processes::lines()`**
  returns: each line's text (without its line ending, cut at 16 KiB) and whether it went to
  standard error. Every line is handed over before `command` runs for the end, whose output is
  then empty. Lines waiting for the extension are handed over together, and past 1 MiB the
  oldest are dropped and counted in `dropped`, so a program can't flood a slow extension.
- **`kill(id)`** stops a spawned program and everything it started; its command still runs,
  with `killed`.
- **Options**: the folder it runs in (the open folder by default), environment variables to add,
  bytes for its standard input (otherwise it reads nothing), and a timeout.
- **Failures** are a `process-error`: `permission-denied`, `untrusted` (the open folder isn't
  [trusted](#the-open-folder-and-trust)), `unknown-program` (not declared), `not-found` (not
  installed, with the reason), `timed-out`, `killed`, `too-large`, `invalid-argument`, or
  `failed`.

Programs run as the user, with Flareite's environment, in a process group with no console window.

**Signing in.** A program declared with `askpass = true` runs with `GIT_ASKPASS` and
`SSH_ASKPASS` set to Flareite's own executable (and `SSH_ASKPASS_REQUIRE=force`). When Git needs
a username or password, or SSH a key's passphrase, it runs that executable with the question,
which Flareite asks the user in the command palette, hiding what's typed for passwords,
passphrases, PINs, and tokens. The answer goes back to the program, never to the extension.
Credential helpers (Git Credential Manager, an SSH agent) still answer first when they can.
They are stopped when the extension stops. Their input is empty unless `stdin` is given, so a
program never waits for typing. Some still wait for an answer in other ways: Git, for example,
needs `GIT_TERMINAL_PROMPT=0` so that it fails rather than waits for a password.

```rust
let output = processes::run(
  "git",
  &["rev-parse".into(), "--show-toplevel".into()],
  &ProcessOptions { cwd: None, env: Vec::new(), stdin: None, timeout_ms: None },
)
.map_err(|error| format!("{error:?}"))?;
if output.exit_code == Some(0) {
  let root = String::from_utf8_lossy(&output.stdout).trim().to_owned();
}
```

### Settings, logging, and messages

**Settings.** `configuration(key)` returns the user's value for a declared setting, or its
default. Users change them in the settings editor (**Preferences: Open Settings**), which lists
each declared setting under the extension's name, titled from its key (`max_line_length` is "Max
Line Length"), with its description, and a control for its type, range, and choices. The values
are stored in `settings.toml`:

```toml
[extensions.settings."example.tidy"]
max_line_length = 120
```

Values are checked against the declaration. Invalid values are listed in the Extensions view and
the settings editor, and the default is used instead. Values are read at call time, so a changed setting applies on the
next read.

`on_configuration_changed = "<command>"` (API 1.16) in the manifest names a command that runs
when the user changes one of the extension's settings, for an extension that applies them to
something else, as Luau configures its language server.

`set-configuration(key, value)` (API 1.7) changes one of the extension's settings in the user's
`settings.toml`, or removes it there (`none`), which restores the default. It's for choices the
user makes in the extension, such as "Always" in a prompt; Git's Smart Commit setting is saved
this way. The value must fit the declaration.

**Storage** (API 1.5). The `storage` interface keeps small values across sessions, as JSON
text: `get(scope, key)`, `set(scope, key, value)` (`none` removes it), and `keys(scope)`. The
scope is `global`, or `workspace` for the folder open in the editor (nothing is kept while none
is open, and `set` fails). Each scope holds at most 1 MiB, and keys are at most 200 bytes.
The extension's [webview](#webviews) pages share the values, and hear when its code changes them.
Users see how much an extension stores on its page in the Extensions view, and can clear it
(which removes its [secrets](#secrets) too).

**Logging and messages:**

- `log(level, message)` writes to Flareite's log, tagged with the extension's id. Messages are cut
  at 4,000 characters, and at most 50 per second are kept, after a burst of 200. The extension's
  log in **View: Output** (or its **Log** button in the Extensions view) shows every level,
  including `debug` and `trace`, while the log file follows `FLAREITE_LOG` (`info` by default).
- `show-message(level, message)` shows a notification and logs it. Messages are cut at 1,000
  characters, and at most 1 per second is shown, after a burst of 10. Notifications disappear
  after a while; warnings and errors stay in **View: Problems**, under the extension's name,
  until the user clears them.

## Workbench contributions

Contributions are declared in the manifest, and the workbench draws them itself. An extension
never supplies markup or CSS: it supplies text, colors that must be hex colors, and icons that
are shown as images. The exception is [webviews](#webviews), pages that run sandboxed.

- **Ids.** Contribution ids are local to the extension (`list`, `count`). In the workbench they
  become `<extension id>.<id>`.
- **Commands.** Commands named by contributions must be the extension's own.
- **Disabled extensions.** Contributions of a disabled or invalid extension aren't shown.

### Themes

```toml
[[themes]]
id = "midnight"                   # used as "example.themes.midnight"
label = "Midnight"
kind = "dark"                     # the built-in theme it starts from: dark or light
path = "themes/midnight.toml"     # inside the extension's folder
```

The theme file sets Flareite's color tokens; any token it leaves out keeps the base theme's
color:

```toml
[colors]
editor-background = "#1a1b26"
syntax-keyword = "#bb9af7"
```

- **Tokens.** The tokens are listed in [`src/themes/tokens.json`](../src/themes/tokens.json)
  and defined in `src/styles/tokens.css`.
- **Validation.** Unknown tokens and values that aren't hex colors make the extension invalid.
- **No code needed.** An extension that only contributes themes needs no entrypoint.
- **Selecting.** Users pick a theme with **Preferences: Color Theme: …** or
  `workbench.theme = "example.themes.midnight"`.
- **User colors.** `[workbench.colors]` in the settings overrides individual tokens on top of
  any theme.

### File icon themes

An icon theme replaces the icons the explorer and Go to File show for files and folders
([ADR 0009](adr/0009-file-icon-themes.md)). [`extensions/icons`](../extensions/icons/) is a
complete example.

```toml
[[icon_themes]]
id = "colorful"                   # used as "example.icons.colorful"
label = "Colorful"
path = "icons/theme.toml"         # inside the extension's folder
```

The theme file names its icons, then says which icon goes with what. Icon paths are relative to
the theme file.

```toml
file = "file"                     # files nothing below matches
folder = "folder"                 # folders nothing below matches
folder_expanded = "folder-open"

[icons]                           # icon id = SVG or PNG file
file = "file.svg"
folder = "folder.svg"
folder-open = "folder-open.svg"
rust = "rust.svg"
typescript = "typescript.svg"
typescript-def = "typescript-def.svg"
folder-src = "folder-src.svg"
folder-src-open = "folder-src-open.svg"
folder-dark-ink = "folder-dark-ink.svg"
rust-dark-ink = "rust-dark-ink.svg"

[file_extensions]                 # without the leading dot
rs = "rust"
ts = "typescript"
"d.ts" = "typescript-def"         # the longest match wins

[file_names]                      # exact names; these beat extensions
"cargo.toml" = "rust"

[folder_names]
src = "folder-src"

[folder_names_expanded]           # open folders; falls back to folder_names
src = "folder-src-open"

[light]                           # looked up first with a light color theme
folder = "folder-dark-ink"
file_extensions = { rs = "rust-dark-ink" }
```

- **Matching.** Names and extensions are matched ignoring case. A leading dot starts a name, not
  an extension: `.eslintrc.json` matches the extension `json`.
- **Fallback.** Anything the theme doesn't match uses its `file`, `folder`, and
  `folder_expanded` icons. If it doesn't set those either, Flareite's built-in icons are used, so
  a theme can change just a few file types.
- **Icons.** Icons are SVG or PNG files, shown at 16 × 16 pixels. They are shown as images, so
  they can't use theme colors. Provide light variants under `[light]` if an icon needs one.
  Scripts and external references in an SVG are ignored.
- **Validation.** Icons outside the theme's folder, files that aren't SVG or PNG, and
  associations that name an undeclared icon make the extension invalid.
- **No code needed.** An extension that only contributes themes and icon themes needs no
  entrypoint.
- **Selecting.** Users pick an icon theme with **Preferences: File Icon Theme: …** or
  `workbench.icon_theme = "example.icons.colorful"`. **File Icon Theme: Default** returns to
  the built-in icons.

### Keybindings

```toml
[[keybindings]]
command = "example.todos.next"
key = "ctrl+alt+t"                # modifiers: mod, ctrl, alt, shift, meta
mac = "mod+alt+t"                 # optional; "mod" is Cmd on macOS
when = "editorFocus"              # optional: only while the editor has focus
```

Keys are written like `ctrl+shift+p`, `alt+f12`, or `mod++`. Named keys are `enter`, `tab`,
`escape`, `space`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, the
arrows (`arrowup`, …), and `f1` to `f24`.

**Precedence.** A built-in command's keys win over an extension's. The user's `[keybindings]`
settings win over both: they replace a command's keys, or remove them with `""`. **Preferences:
Keyboard Shortcuts** lists every command with its id and keys.

### Menus

```toml
[[menus]]
location = "editor/context"       # or "explorer/context"
command = "example.todos.insert"
group = "todos"                   # items are grouped by this, then sorted by title
patterns = ["*.rs", "*.{ts,tsx}"] # optional: only for files whose names match one

[[menus]]
location = "explorer/context"
command = "example.tidy.trimFiles"
targets = ["files", "folders"]    # explorer only; default ["files"]
```

- **`editor/context`** items appear in the editor's right-click menu and run with the usual
  command context (the active document and its selections).
- **`explorer/context`** items appear when right-clicking a file or folder in the explorer, as
  `targets` says (right-clicking the space below the files counts as the open folder). They
  receive its path as the command argument (`workbench::command-argument`). With the
  [`files`](#files) interface, a command can work on everything in a folder, as a formatter's
  **Format All** would.
- **`patterns`** match a file's name, ignoring case: `*` is any characters, `?` one, and
  `{a,b}` either. An item with patterns isn't shown for untitled documents. At most 20 patterns
  of 100 characters each.
- **`view/title`** and **`view/item`** (API 1.7) are for side bar views: see
  [view menus](#view-menus).

### Status bar items

```toml
[[status_items]]
id = "count"
alignment = "right"               # or "left" (default)
priority = 10                     # higher is further left within its side
text = "TODOs"                    # shown until the extension sets other content
tooltip = "Show TODOs"
command = "example.todos.show"    # optional: run on click
```

The item shows its manifest text even before the extension starts (clicking it starts the
extension). While running, the extension changes it with `workbench::set-status-item(id, item)`,
or hides it with `none`. Text is plain text, at most 100 characters.

### Side bar views

```toml
[[views]]
id = "list"
title = "TODOs"
icon = "check"   # list, check, bolt, book, bug, tag, star, flag, bell, source-control, history, cube
```

A view adds an activity bar entry and a side bar panel. Its content is a tree of items:

- **`set-view-items(view, items)`** sets the tree, at most 5,000 items. Each item has an id, an
  optional parent (which must come earlier in the list), a label, a description, a tooltip,
  whether it starts expanded, and an optional command and argument.
- **Activating an item** (click or Enter) runs its command. The command reads the item's
  argument with `workbench::command-argument()`.
- **`set-view-message(view, message)`** shows a line of text above the items, such as "No TODOs
  in open documents."
- **`show-view(view)`** brings the view into the side bar.

Use `on-view:<id>` to start the extension only when its view is first shown.

API 1.7 adds the `views` interface, for views like Git's Source Control view. Everything below
needs no permission.

#### View containers

Views can be sections of one activity bar entry, a container, each with a header that
collapses it. Git's Source Control entry has a Changes section and a Commits section:

```toml
[[view_containers]]
id = "scm"
title = "Source Control"
icon = "source-control"           # as for views

[[views]]
id = "changes"
title = "Changes"
container = "scm"
input = { placeholder = "Message (Ctrl+Enter to commit)", command = "flareite.git.commit" }

[[views]]
id = "commits"
title = "Commits"
container = "scm"
on_expand = "flareite.git.expandCommit"
```

A container needs at least one view. A view in a container has no activity bar entry of its
own; showing it (`show-view`) shows its container. A section with `hidden = true` (API 1.13)
doesn't show until the extension calls **`views::set-visible(view, true)`**, which also hides
one again; Git shows its Repositories section only when there's more than one repository. Users collapse sections and resize them by dragging the
boundary between two; Flareite remembers both for the next session.

#### Tree items

**`views::set-items(view, items)`** sets a view's tree, at most 10,000 items. Besides what
`set-view-items` takes, a `tree-item` has:

- **`icon`**: `named(name)`, one of the [named icons](#named-icons); or `file(path)` or
  `folder(path)`, the file icon theme's icon for that name.
- **`mark`**: a badge of one or two characters at the end of the row, a color the manifest
  declares under [`[colors]`](#explorer-marks) (for the label and the badge), `strikethrough`
  (as for a deleted file), and `faded`.
- **`context`**: which of the view's [item menus](#view-menus) apply to it.
- **`expansion`**: `none`, `collapsed`, or `expanded`. An item that can be opened but has no
  children listed gets them when the user opens it: the view's `on_expand` command runs,
  `views::expanding()` returns the view, the item's id, and its argument, and the command
  replaces the item's children with **`views::set-children(view, item, items)`** (which returns
  false if the item is gone). The view shows "Loading…" until then, and "No items" if it sets
  none. Git lists a commit's files this way.

Users select items with the mouse (Ctrl- and Shift-click for several) and the keyboard (arrows,
Shift+arrows, Ctrl+A). Clicking an item runs its command, if it has one.

#### View menus

```toml
[[menus]]
location = "view/item"            # an item's button or right-click menu
view = "changes"
contexts = ["changed", "untracked"]   # optional: items with these contexts; default every item
command = "flareite.git.stage"
inline = true                     # a button on the row (shown on hover, focus, or selection)
icon = "add"                      # one of the named icons; buttons need one

[[menus]]
location = "view/title"           # the view's header
view = "changes"
command = "flareite.git.viewAsTree"
group = "3_view"                  # entries are grouped, then sorted by title
when = "!tree"                    # optional: only while the view hasn't the flag "tree"
```

- **`view/title`** entries are buttons in the view's header (the side bar's, or the section's)
  when `inline`, and otherwise entries of its "…" menu. Their commands run with no argument.
- **`view/item`** entries are buttons on the rows when `inline`, and otherwise entries of the
  rows' right-click menu (also the menu key, or Shift+F10).
- **The command** gets the item's argument (`workbench::command-argument()`), and
  **`views::selection()`** returns the arguments of every selected item when the clicked item
  is one of them, or of the clicked item alone. Git stages all the selected files this way.
- **`when`** shows an entry only while the view has a flag, or with `!`, while it doesn't. A
  view's flags are what **`views::set-flags(view, names)`** last set.

#### Text box, welcome message, and badge

- **A text box** (`input` in the view's manifest entry) sits above the items. Ctrl+Enter (Cmd+Enter
  on macOS) runs its `command`. **`views::input(view)`** returns its text, and
  **`views::set-input(view, text)`** replaces it (Git empties it after a commit).
- **A welcome message**, **`views::set-welcome(view, welcome)`**, shows instead of the items
  while there are none: text (a blank line starts a paragraph) and up to 4 buttons, each running
  a command with an optional argument. Git offers **Initialize Repository** this way.
- **A badge**, **`views::set-badge(view, count)`**, shows a number on the view's header and on
  its activity bar entry (a container's adds up its sections'). 0 shows none.

#### History graph

**`views::set-graph(view, rows)`** (API 1.11) draws a history graph beside a view's items, as VS
Code's Source Control Graph does: one row per item it names. A row has its `node`, the
`parents` the node leads to, and the lanes at its top (`inputs`) and bottom (`outputs`), each an
`id` (what the lane leads to) and a `color` (0 to 4, by turns). Lanes at the top for the node
end at it; others carry on to the bottom lane with the same `id`; and a lane leaves the node for
each parent. Rows of items it doesn't name, such as an opened commit's files, carry the lanes
straight on. Going down the commits, the extension makes each row's `outputs` from its `inputs`:
the node's lane goes on to its first parent, and other parents get lanes of their own. Git's
Commits view does this; see `src/graph.rs` in the Git extension.

### Decorations

```toml
[decorations.keyword]
background = "#e5c07b26"          # all colors optional: hex, or a theme color token (API 1.15)
color = "#e5c07b"                 # text color
border = "#e5c07b"
underline = "wavy"                # solid, wavy, dotted, or dashed
underline_color = "#e5c07b"
bold = true
italic = false
whole_line = false                # (API 1.12) the background spans each line's width

[decorations.keyword.light]       # optional colors for light themes
background = "#9868011f"
color = "#986801"
```

`workbench::set-decorations(document, version, "keyword", ranges)` replaces the ranges decorated
with that type in a document (at most 10,000). It works like `publish-diagnostics`:

- **Version.** Ranges refer to the text at `version`, which may be the current version or a
  recently sent one.
- **Editing.** Decorations move with later edits.
- **Removal.** Decorations are removed when the extension stops.
- **Permission.** It needs the `documents_read` permission.

`workbench::reveal(document, range)` selects a range and scrolls it into view, making that
document active. TODOs uses it when a view item is chosen. It also needs `documents_read`.

A color can also be a **theme color token** (API 1.15), such as `"diff-inserted-line"` or
`"merge-current"`: the color follows the theme, as Git's diffs do. The tokens are those themes
set (`src/themes/tokens.json`).

A **whole-line** type (`whole_line = true`, API 1.12) colors the background of each line a range
covers, as VS Code's `isWholeLine` does; a range ending at the start of a line doesn't cover that
line. Git colors the sides of merge conflicts this way.

#### Code lenses

**`workbench::set-code-lenses(document, version, lenses)`** (API 1.12) shows links above lines,
as VS Code's code lenses: each lens has a `line`, a `title` (plain text, at most 100
characters), an optional `tooltip`, and one of the extension's `command`s with an optional
`argument`. A line's lenses show in one row, in order, separated by bars. Clicking one runs its
command for the document (`command-context.document`) with the argument
(`workbench::command-argument`). Like decorations, lenses refer to the text at `version`, move
with later edits (staying on their lines), are replaced as a whole (at most 1,000 per document),
go when the extension stops, and need `documents_read`. Git shows Accept Current Change, Accept
Incoming Change, Accept Both Changes, Compare Changes, and Resolve in Merge Editor above each
merge conflict.

#### Line annotations

**`workbench::set-line-annotations(document, version, annotations)`** (API 1.13) shows faint
text after the end of lines, such as who last changed them: each annotation has a `line`, its
`text` (plain, at most 200 characters), and an optional `tooltip` shown on hover (plain, at most
2,000 characters). A line shows one. Like code lenses, they refer to the text at `version`, follow
later edits, are replaced as a whole (at most 100,000 per document), go when the extension
stops, and need `documents_read`. Git annotates the cursor's line with its blame, following
it with `on_selection_changed`.

### Previews

A preview is a tab of formatted text that the extension fills in, such as a rendered Markdown
file. The official Markdown extension is the example.

```toml
[[previews]]
id = "preview"
icon = "book"                     # list, check, bolt, book, bug, tag, star, flag, bell
```

A preview type can have many open previews, told apart by a key the extension chooses (Markdown
uses the document's id).

- **`show-preview(preview, key, options)`** opens a preview, or shows it if it's open. The
  options are its title (at most 100 characters), the document it shows, if any, and whether to
  open it beside that document (or the active editor) rather than in the same group. Showing a
  document's preview needs the `documents_read` permission.
- **`set-preview-content(preview, key, nodes)`** replaces what it shows. It returns `false`
  without changing anything when the preview isn't open, because the user closed its tab, so
  the extension can stop updating it.
- **`close-preview(preview, key)`** closes it.
- **`render-markdown(text)`** (API 1.14) turns Markdown into preview content: CommonMark with
  GitHub's tables, strikethrough, and task lists, rendered by the editor as it renders
  extensions' READMEs. HTML is reduced to its text, front matter is left out, and a document too
  long for a preview is cut with a note. It takes at most 8 MiB (`too-large` otherwise) and needs
  no permission. The result can go to `set-preview-content` or a viewer's `set-viewer-content`.

Content is a list of nodes, each naming the index of the node it's inside, which must come
before it (WIT has no recursive types). The workbench draws them itself, so an extension can't
supply HTML, scripts, or styles:

| Kind   | Nodes                                                                                | May contain                         |
| ------ | ------------------------------------------------------------------------------------ | ----------------------------------- |
| Blocks | `paragraph`, `heading(level)`, `code-block`, `rule`                                  | inline nodes (paragraph, heading)   |
|        | `quote`                                                                              | blocks and inline nodes             |
|        | `bulleted-list`, `numbered-list(start)`                                              | `list-item(task)`, which holds both |
|        | `table(alignments)`                                                                  | `table-head` and `table-row`        |
|        | `table-head`, `table-row`                                                            | `table-cell`, which is inline       |
| Inline | `text`, `code`, `image`, `line-break`; `emphasis`, `strong`, `strikethrough`, `link` | inline nodes (the last four)        |

The top level holds blocks and inline nodes. Anything out of place is rejected with
`invalid-argument`, naming the node.

- **Links** go to a web address (`https:`, `http:`, or `mailto:`), which opens with the system;
  to a heading in the preview (`#name`, matched like GitHub's anchors); or to a file relative to
  the preview's document (`setup.md#install`), which opens in the editor. Other schemes and
  absolute paths are shown as text and can't be followed.
- **Pictures** are PNG, JPEG, GIF, WebP, SVG, BMP, ICO, or AVIF files (the last three since API
  1.4) relative to the preview's document, up to 64 MiB, and the workbench can read only the
  files the content names. Web pictures aren't
  loaded, so opening a preview sends no requests; their description is shown instead.
- **Code blocks** name their language by id or file extension (`rust` or `rs`). Code in a
  language Flareite has is highlighted with the editor's colors.
- **Lifetime.** A preview closes with its document, and when its extension stops. Previews
  aren't restored when Flareite restarts: their extension opens them again.
- **Updates.** Unchanged content sends no event, and the tab loads at most one update at a time,
  so an extension can render on every change. Markdown does.

### Viewers

A viewer shows a kind of file the extension's way instead of as text, such as a picture of an
image file. Viewers are API 1.4. This one fills the tab with [preview](#previews) content, here
a summary of a font file; a viewer can also show a page of its own instead
([webviews](#webviews)), as the official Images extension does.

```toml
[[commands]]
id = "example.fonts.show"
title = "Show Font"
hidden = true                     # run by the viewer, not from the palette

[[viewers]]
id = "font"                       # used as "example.fonts.font"
title = "Font Viewer"             # shown when choosing how to open a file
patterns = ["*.ttf", "*.{otf,woff}"]   # file names, as for menu items
command = "example.fonts.show"    # fills the viewer in
priority = "default"              # or "option"
```

- **Which files.** A file whose name matches a `default` viewer's patterns opens in it: from the
  explorer, Go to File, the open dialog, links, or a drop. An `option` viewer is used only when
  the user chooses it with **View: Reopen Editor With…**, which also switches back to text. A
  file already open as text stays in its text tab. A binary file that no viewer is the default
  for opens in a tab that offers **Open Anyway** and the viewers that match it.
- **Filling it in.** The workbench runs the viewer's `command` with the file's path as its
  argument (`workbench::command-argument()`): when the tab shows, when the file changes on disk
  (files in the open folder), and when the extension starts again after stopping. The command
  reads the file with **`viewers::read-viewed-file(viewer, path, offset, length)`**, which
  returns the file's size and up to 16 MiB from `offset`, and sets what the tab shows with
  **`viewers::set-viewer-content(viewer, path, nodes)`**. A command that fails shows its error in
  the tab, with **Try Again**.
- **Content** is [preview](#previews) content, checked and drawn the same way. Pictures and
  links are found from the file's folder, so the file's own picture is its name: escape `%`,
  `#`, `?`, and spaces (`%25`, `%23`, `%3F`, `%20`), and start it with `./`. In a viewer,
  pictures are centered and drawn on a checkerboard, so transparent parts show.
- **Access.** A viewer needs no permission. It can read only files open in one of its viewers,
  and `set-viewer-content` returns `false` once the user has closed the tab.
- **Lifetime.** A viewer tab is the workbench's: it's saved with the layout and stays open when
  the extension stops, which removes only its content. Viewers can't edit their files.

### Webviews

A viewer or preview can show a page of the extension's own (HTML, CSS, and JavaScript) instead
of preview content, for what content can't draw: the official Images extension shows a picture
you can zoom, on a background you choose. Webviews are API 1.5
([ADR 0025](adr/0025-webviews.md)).

```toml
[[commands]]
id = "flareite.images.details"
title = "Image Details"
hidden = true

[[viewers]]
id = "svg"
title = "Image Viewer"
patterns = ["*.svg"]
webview = "media/viewer.html"          # a page in the extension's folder; or `true`, and set it from code
on_message = "flareite.images.details"  # optional: run when the page sends a message
retain_when_hidden = false             # optional: keep the page loaded while its tab is hidden
priority = "option"

[[viewers.actions]]                    # optional: buttons beside the tab
id = "fit"
title = "Fit"
icon = "fit"                           # a named icon, or an SVG or PNG file in the folder
# command = "flareite.images.fit"       # optional: run with the tab as argument, instead of telling the page
```

A preview takes the same keys under `[[previews]]`, and opens with `show-preview` as before. A
webview viewer's `command` is optional, since the page can load the file itself; if it has one,
it runs as for any viewer.

**The page** is an ordinary page: addresses in it are found next to it in the extension's
folder, so `<script src="viewer.js">` and `<link rel="stylesheet" href="viewer.css">` work.
Before its own scripts run, it has a `flareite` object:

| Member                                      | What it's for                                                                                                                                                               |
| ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `kind`                                      | `"viewer"` or `"preview"`                                                                                                                                                   |
| `fileUrl`, `fileName`                       | In a viewer, the file open in it, for `<img src>`, `fetch`, and so on. `fileUrl` changes with the file                                                                      |
| `onFileChanged(fn)`                         | In a viewer: called with the new `fileUrl` when the file changes on disk                                                                                                    |
| `postMessage(value)`                        | Sends a value JSON can represent to the extension                                                                                                                           |
| `onMessage(fn)`                             | Called with each message the extension sends; those sent before the page listens wait for it                                                                                |
| `getState()`, `setState(value)`             | This tab's state (at most 64 KiB), back when the page loads again                                                                                                           |
| `storage`, `workspaceStorage`               | The extension's [stored values](#settings-logging-and-messages): `get(key)`, `set(key, value)` (`undefined` removes it), `keys()`, each a promise                           |
| `onStorageChanged(fn)`                      | Called with `{ scope, key, value }` when the extension's code or another of its pages changes a stored value; `scope` and `key` are `null` when the user cleared everything |
| `theme`, `onThemeChanged(fn)`               | `"dark"` or `"light"`, and a call when the theme changes                                                                                                                    |
| `onAction(fn)`, `setActionState(id, state)` | Clicks on the tab's actions without a command, and how each looks: `pressed`, `disabled`, `hidden`, `title`                                                                 |

Each `on…` function returns a function that stops listening.

**Messages.** `flareite.postMessage(value)` runs the entry's `on_message` command (starting the
extension if needed). In it, **`webviews::message()`** returns `{ source, data }`: the tab
(`viewer({ viewer, path })` or `preview({ preview, key })`) and the message as JSON text. The
extension answers with **`webviews::post-message(source, json)`**, and can message any of its
open webview tabs the same way. Messages are JSON, at most 1 MiB. **`webviews::set-webview-html(webview, html)`**
replaces a tab's page (at most 4 MiB), for `webview = true`; addresses in it are found from the
extension's folder. Both return `false` once the tab is closed. Images' page asks for the
picture's details:

```js
flareite.onMessage(({ details }) => (document.getElementById('details').textContent = details))
flareite.postMessage({ type: 'details' })
```

```rust
fn run_command(command: String, _: CommandContext) -> Result<(), String> {
  let message = webviews::message().ok_or("no message")?;
  // ... read the file named by message.source ...
  webviews::post_message(&message.source, r#"{"details":"PNG · 128 × 128 pixels"}"#)
    .map_err(|error| format!("{error:?}"))?;
  Ok(())
}
```

**Lifetime.** Only the active tab of a group is drawn, so a page is unloaded when its tab is
hidden, and loads again when it's shown: save what it needs with `setState`. A viewer's state
also comes back in the next session, since its tab is saved with the layout; closing the tab
forgets it. With `retain_when_hidden = true` the page stays loaded instead, at the cost of its
memory (moving the tab to another group still loads it again).

**Looking like Flareite.** Every page gets the theme as CSS variables, which change with it:
`--flareite-<token>` for each [theme token](#themes) (`--flareite-editor-background`,
`--flareite-text-muted`, `--flareite-focus-ring`, …), and `--flareite-font-ui`,
`--flareite-font-mono`, `--flareite-font-size-ui`, `--flareite-font-size-ui-small`,
`--flareite-font-size-editor`, `--flareite-radius-control`, and `--flareite-color-scheme`. The root
element has `data-flareite-theme="dark"` or `"light"`. Defaults for body text, links, focus
rings, selection, and scroll bars come with them, with no specificity, so any style of the page's
wins. For controls like Flareite's, link the kit, from any folder:

```html
<link rel="stylesheet" href="__flareite/kit.css" />

<div class="flareite-toolbar">
  <button class="flareite-button" aria-pressed="true" aria-label="Zoom In">
    <span class="flareite-icon" data-icon="zoom-in"></span>
  </button>
</div>
```

It has `flareite-toolbar`, `flareite-group`, `flareite-separator`, `flareite-button` (pressed with
`aria-pressed="true"`; `flareite-primary` for the main action), `flareite-input`, `flareite-label`,
and `flareite-icon`, a 16-pixel icon in the text color.

<a id="named-icons"></a>The named icons, for the kit, tab actions, view menus, and tree items:
`actual-size`, `add`, `branch`, `check`, `chevron-down`, `chevron-left`, `chevron-right`,
`chevron-up`, `close`, `code`, `commit`, `copy`, `crop`, `download`, `edit`, `error`, `eye`,
`eye-off`, `file`, `filter`, `fit`, `flip-horizontal`, `flip-vertical`, `grid`, `image`, `info`,
`layers`, `link`, `list-flat`, `list-tree`, `merge`, `moon`, `more`, `move`, `open`, `palette`, `pause`,
`play`, `refresh`, `remove`, `rotate`, `save`, `search`, `settings`, `split`, `stop`, `sun`,
`undo`, `upload`, `warning`, `zoom-in`, `zoom-out`.

**Tab actions** are buttons beside the tab while it's active, at most 10. Without a `command`, a
click goes to the page (`flareite.onAction(id)`); with one, the command runs with the tab as its
argument (`workbench::command-argument()`: the viewer's file, or the preview's key), even while
the page isn't loaded. Actions without a command need a webview; any viewer or preview can have
actions with one. An icon file is shown as an image (it can't follow the theme's colors).

**The keyboard.** Flareite's shortcuts work while a page has focus: the page passes on key
presses bound to Flareite commands, and the second key of a chord. To use such a key itself,
handle it with `event.preventDefault()` in a `keydown` listener on an element or the
`document` (a listener on `window` runs too late). Other keys, such as typing or Ctrl+C in a
field, stay in the page.

**The sandbox.** A page runs in a frame sandboxed with only `allow-scripts`, so it has an origin
of its own: it can't reach Flareite's window, IPC, cookies, or `localStorage`, navigate, open
popups, or submit forms. Its address is made for its tab, and a content security policy allows
only the extension's files, the viewed file, and `data:` and `blob:` URLs (inline scripts and
styles are fine). With the [`network` permission](#permissions), granted by the user, the policy
also allows `https:` for scripts, styles, images, media, fonts, and `fetch`, and `wss:` for
sockets.

### Prompts

The `prompts` interface (API 1.7) asks the user something. Each function returns the prompt's id
at once; the answer runs the command the call names, during which **`prompts::answer()`**
returns the id and the value:

- **`pick(title, items, command)`** offers a list in the command palette, filtered as the user
  types (at most 1,000 items, each with a label, an optional description, and a value). The
  answer is `picked(value)`.
- **`input(options, command)`** asks for a line of text in the palette, starting with
  `options.value`. The answer is `text(text)`.
- **`input-password(options, command)`** (API 1.17) asks the same, hiding what's typed, for
  passwords and API keys. Flareite doesn't log the answer; keep it with [secrets](#secrets)
  rather than storage.
- **`confirm(options, command)`** asks in a dialog: a message, optional detail, and 1 to 4
  buttons, the first being the main one (shown last, and in red when `destructive`); Cancel is
  added. The answer is `button(index)`.
- **`pick-folder(title, command)`** (API 1.11) asks for a folder in the system's dialog. The
  answer is `text(path)`, an absolute path.

Closing a prompt without answering, or another prompt replacing it (one shows at a time), answers
`cancelled`. A prompt goes away when its extension stops.

```rust
let id = prompts::confirm(
  &ConfirmOptions {
    message: "Discard the changes to main.rs?".into(),
    detail: Some("Changes that aren't staged are lost.".into()),
    buttons: vec!["Discard File".into()],
    destructive: true,
  },
  "flareite.git.answered",
)
.map_err(|e| format!("{e:?}"))?;
// ... remember what `id` was for; in flareite.git.answered:
if let Some(answer) = prompts::answer()
  && answer.value == AnswerValue::Button(0)
{
  // ... discard ...
}
```

### Provided documents

The `documents` interface (API 1.8) lets an extension provide read-only documents at addresses of
its own. It needs no permission.

The manifest declares the schemes it provides, each with the command that provides a text:

```toml
[[text_providers]]
scheme = "git"                  # 2 to 32 lowercase letters, digits, +, -, or .
command = "flareite.git.provide" # one of the extension's commands
```

An address is the scheme, a colon, and whatever the extension likes: Git's are
`git:<revision>:<path>`. The name after the last `/`, `\`, or `:` is the tab's title and picks
the language, so `git:HEAD:src/main.rs` is highlighted as Rust. Schemes the editor or the web use
(`file`, `untitled`, `http`, `https`, `data`, `blob`, `flareite`) are refused.

When the editor needs a text, it runs the provider's command, during which
**`documents::requested()`** returns the address. The command answers with
**`provide(uri, text)`**, at most 32 MiB; `none` means there's nothing there, which shows as
empty. A command that doesn't answer fails the request.

```rust
"flareite.git.provide" => {
  if let Some(uri) = documents::requested() {
    let text = read_version(&uri); // e.g. `git cat-file blob HEAD:src/main.rs` with processes::run
    documents::provide(&uri, text.as_deref()).map_err(|e| format!("{e:?}"))?;
  }
}
```

- **`changed(uris)`** says texts may have changed, so open documents showing them ask again. An
  address ending in `*` stands for every address starting with what's before it (`git:HEAD:*`).
- **`open(uri)`** opens the document at one of the extension's addresses in a tab of its own.
- **`save(document)`** (API 1.15) saves an open document to its file, as the user's Save does but
  without asking anything; a document without a file (a provided one, a new file) fails. It needs
  `documents_write`. Git's merge editor saves the result before staging it.
- **`open-diff`** and **`open-merge`** (API 1.8 and 1.12) opened the workbench's diff and merge
  tabs. Since API 1.15 those are the Git extension's [layouts](#layouts), and these fail with
  `invalid-argument`; they stay so that components built against older versions still load.

### Layouts

A layout (API 1.15, [ADR 0038](adr/0038-layouts.md)) is a tab the extension splits into panes:
editors, pages of its own, and files shown as web pages, side by side or stacked, each with an
optional header of buttons. Git's diff and merge editors are layouts, and so is HTML & CSS's
preview of an HTML file beside its editor.

```toml
[[commands]]
id = "example.preview.fill"
title = "Show a Preview Tab"
hidden = true

[[layouts]]
id = "preview"                    # used as "example.preview.preview"
title = "HTML Preview"            # shown when choosing how to open a file
command = "example.preview.fill"  # fills its tabs in (below)
on_message = "example.preview.message"  # optional: its webview panes' messages
patterns = ["*.html", "*.htm"]    # optional: the files it's for
priority = "option"               # with patterns: "default" opens matching files in it
icon = "preview"                  # optional: a named icon for a button on matching files' tabs
```

**Tabs.** A tab is named by its layout and a **key** the extension chooses (at most 4 KiB):
`layouts::open(tab, beside)` opens one or shows it. A layout with `patterns` is for files: its
tabs' keys are their files' paths, and the user opens a file in it with **View: Reopen Editor
With…** (which also goes back to the text editor), with its `icon` button on a matching file's
tab, or always, with `priority = "default"`. A file layout's tab has a button that shows the file
as text again. A `default` layout's tab stands for its file, as a viewer's does: opening the file
shows that tab. An `option` layout's tab doesn't: opening the file opens its text in a tab of its
own, so it can be edited beside the layout. Tabs are the workbench's, like viewers': they're
saved with the window and restored, and stay open when their extension stops.

**Filling a tab in.** The workbench runs the layout's `command` with **`filling()`** saying which
tab and why:

| Reason     | When                                                                       | The command              |
| ---------- | -------------------------------------------------------------------------- | ------------------------ |
| `opened`   | The tab opened                                                             | sets its layout          |
| `restored` | It came back without a layout: with the window, or the extension restarted | sets its layout          |
| `resized`  | Its width (`width`, pixels) crossed one of its `breakpoints`               | may set another          |
| `ready`    | Its documents are open: `documents(tab)` lists their ids                   | reads and decorates them |
| `closed`   | It closed                                                                  | forgets it               |

**`set-layout(tab, layout)`** says what an open tab shows (it returns `false` if the tab isn't
open):

- **`nodes`**: splits (`row`, side by side; `column`, stacked) and panes, parents first, each
  naming the split it's in (`parent`), the first filling the tab. `size` is a node's share of its
  split, relative to its siblings'; the user can drag the line between two. At most 32 nodes and
  16 panes.
- **Panes**: an **editor** of a document (a file's absolute path, which can be edited, or a
  provided address) with `minimap`, `folding`, and `quick-diff` (change markers) as you choose; a
  **webview** (a page in the extension's folder, or `none` for HTML set with `set-pane-html`;
  sandboxed and given the `flareite` object as [webviews](#webviews) are, with
  `post-message(tab, pane, message)` and `message()` in the layout's `on_message` command); or a
  **file page** (below). A `header` has a title, a description, and up to 20 buttons. Editor
  panes with the same `scroll-group` scroll together.
- **`buttons`** beside the tab while it's active, up to 20. A button has an `id`, a `title` (its
  tooltip, and its text when it has no `icon`, a named icon), a `command` of the extension's with
  an `argument`, `primary` (drawn as the main button, as Complete Merge is), `pressed` (a toggle:
  on or off) and `disabled`.
- **`documents`**: other documents to keep open, such as those a zone shows lines of.
- **`breakpoints`**: widths whose crossing runs the command again (`resized`).
- **`main`**: the pane focused first, whose file is the tab's (the active file, the explorer's mark).

A command run from a button or zone gets its `argument` from `workbench::command-argument()` and
the tab and pane from **`invoked()`**. **`active-pane()`** names the editor pane in use and its
selections, for commands such as Stage Selected Ranges. Keybindings can be limited to the
extension's layouts, so a key is theirs only in their tabs:

```toml
[[keybindings]]
command = "flareite.git.nextChange"
key = "f7"
layouts = ["diff", "merge"]
```

**Drawing in editor panes.** **`set-pane-decorations(tab, pane, decorations)`** replaces what an
editor pane shows besides its text, in that pane only:

- **`decorations`**: ranges decorated with the extension's [decoration types](#decorations),
  50,000 in all. Whole-line types color lines.
- **`zones`**: rows above a document line (the line count puts one after the last), at most
  10,000: `filler(rows)`, hatched rows that line up panes scrolling together; `text`, a row of
  text that runs a command when clicked ("⋯ 12 unchanged lines"); `links`, a label and up to 10
  links drawn as code lenses (none leaves an empty row); and `lines` of another of the tab's
  documents, highlighted, with decorations of their own and optionally their line numbers (an
  inline diff's removed lines). A zone's `decoration` gives it that type's background.
- **`folds`**: lines hidden as folded regions (line 0 can't be), at most 10,000.
- **`ruler`**: marks beside the scroll bar, in a decoration type's border color or else its
  background, at most 10,000.

Unlike document decorations, these aren't moved by edits: they're the extension's picture of the
text when it set them, so set them again as the text changes (`document-changed`, with
`documents_read`). **`reveal(tab, pane, range)`** selects a range in an editor pane and scrolls it
to the middle.

**File pages.** A `file-page(path)` pane shows a file of the open folder as a web page, as a live
preview. Flareite serves the folder's files to it as they are in the editor, unsaved changes
included, and shows it again 300 ms after a file in its folder is edited (in any tab) or a file there
changes on disk, scrolled to where it was. Its scripts run, sandboxed as webview pages are, under
the same content security policy (its own files, `data:`, and `blob:`; `https:` and `wss:` only
with the extension's `network` permission). The page is the user's, so it gets no `flareite`
object; Flareite adds only a script that passes on its shortcuts and remembers the scroll
position. A page that links the [kit](#webviews) (`__flareite/kit.css`) is taken to be one being
made for a webview: it gets the kit, the theme's `--flareite-*` variables, and the webview
defaults, and follows theme changes, so it looks as it will in an extension's webview. Other
pages get none of it, and look as they would in a browser.

A kit page also gets a **stand-in `flareite` object** with the real one's API, so its scripts run
as they will in a webview: `theme` and `onThemeChanged` work; `storage` and `workspaceStorage`
are kept for the pane while the window is open (never the extension's real storage), so values
survive the page showing again; `getState` and `setState` last while the page is loaded; and what
would reach the extension (`postMessage`, `setActionState`) is logged to the page's console.
`flareite.standIn` plays the extension's part: `receive(message)` delivers a message as if the
extension sent it, `action(id)` clicks a tab action, `fileChanged()` says the file changed, and
`sent` lists what the page posted. A page can test `flareite.standIn` to know it's in a preview.

File pages show only in a trusted folder, and only files in it; otherwise the pane says why.
Addresses from the folder's root (`/style.css`) don't resolve: use relative ones.

#### Web page panes

A `web-page(url)` pane shows a page from the web (`https:` or `http:`) in a browser view of its
own, laid over the pane, as a browser tab shows it: the user follows its links, fills in its
forms, and signs in to sites. It needs the **`web_pages` permission**, which the user grants.

- **Isolation.** The view loads only `https:` and `http:` addresses and can't reach Flareite: its
  pages can't call Flareite's commands, read files, or see the workbench. A link that would open
  a window opens in the same view.
- **Where it goes.** The layout's `on_navigate` command runs as the page goes somewhere, finishes
  loading, or gets its title; **`navigated()`** says the tab, the pane, the address, the title, and
  whether it's still loading. Set the layout again to show them (in its title or a header), and
  keep the pane's address as it started: changing it doesn't move the page.
- **Moving it.** **`navigate-pane(tab, pane, action)`**: `go(url)`, `back`, `forward`, `reload`,
  or `stop`.
- **On screen.** The view is native, so it sits above the workbench: it hides while the palette,
  a menu, or a dialog is open, and while its tab is hidden. It closes with its tab.

```toml
[permissions]
web_pages = true

[[layouts]]
id = "page"
title = "Browser"
command = "flareite.browser.fill"
on_navigate = "flareite.browser.navigated"  # where its pages go
```

The official **Browser** extension (`flareite.browser`, its own repository) opens web links in
such tabs, with back, forward, reload, and Go to Address buttons, and the page's title as the
tab's.

```rust
"example.preview.fill" => {
  if let Some(request) = layouts::filling() {
    if matches!(request.reason, FillReason::Opened | FillReason::Restored) {
      let path = request.tab.key.clone();
      let pane = |id: &str, content| LayoutNode {
        id: id.into(),
        parent: Some("split".into()),
        size: 1.0,
        kind: NodeKind::Pane(Pane { content, header: None, scroll_group: None }),
      };
      let layout = Layout {
        title: "Preview".into(),
        nodes: vec![
          LayoutNode { id: "split".into(), parent: None, size: 1.0, kind: NodeKind::Split(Direction::Row) },
          pane("source", PaneContent::Editor(EditorPane { uri: path.clone(), minimap: true, folding: true, quick_diff: true })),
          pane("page", PaneContent::FilePage(path)),
        ],
        documents: vec![],
        buttons: vec![],
        breakpoints: vec![],
        main: Some("source".into()),
      };
      layouts::set_layout(&request.tab, &layout).map_err(|e| format!("{e:?}"))?;
    }
  }
}
```

### Links

Links the user Ctrl+clicks (Cmd+click on macOS) open Flareite's way by default: web addresses
(`https:`, `http:`, `mailto:`) in the system's browser, files in the editor (at the line and
column a link like `main.rs:12:5` or `Program.cs(12,5)` names), and folders in the system's file
manager. That covers web addresses and paths in the terminal's output, the editor's text, the
Output view (your extension's log included), notifications, and hover cards, and links in
previews and READMEs. An extension can open some of them its own way (API 1.15):

```toml
[[link_handlers]]
id = "tab"
title = "Open in a Browser Tab"   # shown when choosing who opens a link
kind = "web"                      # or "file"
schemes = ["https", "http"]       # web links: https, http, mailto (default: https, http)
# patterns = ["*.png"]            # file links: file names (default: every file and folder)
command = "flareite.browser.open"
```

While the command runs, **`links::opening()`** gives the link: its kind, its `target` (the
address, or the absolute path), and for a file the `line` and `column` it names (from 0).
**`open-default()`** opens it Flareite's way instead, for a link the handler doesn't take after
all. **`open-in-browser(url)`** opens a web address with the system's browser, as an "Open in
Browser" button does (at most one a second).

Who opens a link: the `workbench.open_web_links` and `open_file_links` settings say `auto` (an
extension's handler when one takes the link, else Flareite's way), `default` (Flareite's way), or
a handler's id. **Ctrl+Shift+click** asks, this once.

In the terminal, paths relative to the shell's folder work: Flareite follows the folder the shell
reports (OSC 7), or else the PowerShell prompt above the link, or else the open folder. Only
paths that lead to a file or folder are links.

### Downloads

The `downloads` interface (API 1.17) gets files from the web for the extension's code, which
otherwise has no network access: data to build a schema from, say. It needs the `network`
permission (without it, calls fail with `invalid-argument`).

**`download(address, command)`** asks for the file at an `https://` address and returns a request
id at once. When the file is had, or can't be, `command` runs, during which
**`downloads::downloaded()`** returns the id, the address, and the file's content (at most 32 MiB)
or why it couldn't be had. At most 8 downloads are under way at once.

Flareite keeps what it downloads, sharing the copies with language servers' `${download:...}`
[arguments](#language-servers): a kept copy is handed over at once, and once it's a day old it's
downloaded again in the background, so a later request gets the newer one. A failed download
isn't tried again for a few minutes.

```rust
use flareite_extension_api::downloads;

downloads::download("https://example.com/api-dump.json", "example.api.downloaded")
  .map_err(|e| format!("{e:?}"))?;
// ... in example.api.downloaded:
let file = downloads::downloaded().ok_or("not run for a download")?;
let bytes = file.outcome?;
```

### Secrets

The `secrets` interface (API 1.17) keeps API keys, tokens, and the like in the system's
credential store (Windows Credential Manager, the macOS Keychain, or the Secret Service on Linux)
rather than in Flareite's files, where [storage](#settings-logging-and-messages) keeps values. It
needs no permission: an extension sees only its own secrets.

- **`get(key)`** returns a secret, or `none`.
- **`set(key, value)`** sets one (at most 2 KiB), or removes it with `none`. Keys are 1 to 200
  bytes, and an extension has at most 32 secrets.
- **`keys()`** lists the keys, sorted.

Failures are a `secret-error`: `invalid-argument`, `too-large`, or `unavailable`, when the store
can't be reached (on Linux, when no Secret Service such as GNOME Keyring or KWallet runs). Ask
for a secret with `prompts::input-password`, which hides what's typed.

Users clear an extension's secrets with its storage, in the Extensions view.

### Timers

The `timers` interface (API 1.10) runs one of the extension's commands later: **`set-timer(delay-ms,
command)`** waits 1 second to 1 day and returns the timer's id, which **`timers::fired()`**
returns while the command runs; **`clear-timer(id)`** cancels it. At most 16 wait at once, and
they're dropped when the extension stops. Git fetches in the background this way.

### Quick diff

The `quick-diff` interface (API 1.9) compares open files with originals the extension names,
such as Git's index. The editor marks lines changed from the original beside the text as it's
edited: a green bar for added lines, blue for changed ones, and a red wedge where lines were
removed. Clicking a marker (or **Show Next Change**, Alt+F3) shows the change's original lines
between the editor's lines, with buttons to revert it, apply it to the original, or move to the
next or previous change. **Revert Selected Ranges** reverts the changes the selections touch.

```toml
[quick_diff]
original = "flareite.git.original"    # names a file's original
apply = "flareite.git.stage"          # optional: applies changes to an original
apply_title = "Stage"                # with apply: what applying is called
```

- The **`original`** command runs when a file opens (and when the extension says originals
  changed). **`requested-file()`** returns the file's absolute path, and the command answers
  with **`set-original(path, uri)`**: an address the extension provides (see
  [Provided documents](#provided-documents)), or `none` for a file without
  one, which isn't marked. The editor reads the original's text, and reads it again when the
  extension says that address changed (`documents::changed`).
- The **`apply`** command runs when the user applies changes (a change's button, or the diff
  tab's "<apply_title> Selected Ranges"). **`to-apply()`** returns the original's address and
  its whole new text: the original with the chosen changes made. Git writes it to the index.
- **`originals-changed(paths)`** makes the editor ask again for these open files' originals,
  or all of them when the list is empty, as when files become tracked.
- **`apply-selected-ranges()`** applies the changes the selections in the active editor touch,
  for a command of the extension's own, such as Git's **Stage Selected Ranges**.

The first extension with a `[quick_diff]` that names an original for a file is used.

### Explorer marks

An extension marks files and folders in the explorer with `explorer::set-file-marks(marks)` (API
1.6), which replaces all its marks. Git marks changed files this way. Each mark has:

- **`path`**, absolute. `/` works on Windows too.
- **`badge`**: one or two characters at the end of the row, such as `M`.
- **`color`**: the name of a color the manifest declares, for the row's text (and the title of
  the file's tabs, which show the badge too).
- **`tooltip`**: plain text, also read by screen readers.
- **`faded`**: dims the row, as the explorer does for files Git ignores.
- **`inherit`**: marks everything inside a folder too, such as an untracked folder.
- **`propagate`** and **`priority`**: folders that contain the path show its color and a dot.
  Where several marks reach a folder, the highest priority wins.

A problem's color (an error or warning in the file) wins over a mark's, and both badges show.
Marks of several extensions on one path show all their badges. Marks need no permission, and
go when the extension stops. At most 100,000, one per path.

**Colors** are declared in the manifest, and are theme colors: a theme's `[colors]` or the user's
`[workbench.colors]` can change them, by `<extension id>.<name>`.

```toml
[colors.modified]
description = "Files with changes"   # optional
dark = "#e2c08d"
light = "#895503"                    # optional; the dark color otherwise
```

```toml
# A theme, or [workbench.colors] in settings.toml:
"flareite.git.modified" = "#f0c674"
```

### Limits

| Item                               | Limit                                     |
| ---------------------------------- | ----------------------------------------- |
| Themes / keybindings / menus       | 20 / 100 / 50 per extension               |
| Icon themes                        | 5 per extension                           |
| Icons                              | 2,000 per theme, 64 KiB each, 8 MiB all   |
| Icon associations                  | 10,000 per theme                          |
| Status items / views / decorations | 20 / 10 / 20 types per extension          |
| Previews                           | 10 types per extension                    |
| Viewers                            | 10 per extension, 20 patterns each        |
| Viewed file reads                  | 16 MiB per call                           |
| Tab actions                        | 10 per viewer or preview; icons 32 KiB    |
| Webview pages set from code        | 4 MiB                                     |
| Webview messages                   | 1 MiB, JSON                               |
| Webview tab state                  | 64 KiB per tab                            |
| Storage                            | 1 MiB per scope; keys 200 bytes           |
| Preview content                    | 100,000 nodes, 8 MiB of text              |
| Status text / tooltip              | 100 / 500 characters (cut, not refused)   |
| View items                         | 5,000 per view; 10,000 with `views`       |
| View containers                    | 5 per extension                           |
| Text providers / provided text     | 5 per extension / 32 MiB                  |
| Timers                             | 16 waiting per extension; 1 s to 1 day    |
| Menus                              | 100 per extension                         |
| Pick items / confirm buttons       | 1,000 / 4                                 |
| Decorated ranges                   | 10,000 per type and document              |
| Programs / colors                  | 10 / 50 per extension                     |
| Programs running (`spawn`)         | 16 at once                                |
| Program output                     | 32 MiB; input 16 MiB                      |
| Watches                            | 32 per extension                          |
| Explorer marks                     | 100,000 per extension                     |
| Layouts                            | 10 per extension, 20 patterns each        |
| Link handlers                      | 5 per extension, 20 patterns each         |
| Layout nodes / panes / buttons     | 32 / 16 / 20 per header or tab            |
| Pane decorations                   | 50,000 ranges; 10,000 zones, folds, marks |
| Zone lines                         | 10,000 per zone                           |

Misuse fails with `invalid-argument` or `too-large` and changes nothing: an undeclared id, a
command that isn't the extension's own, or an item whose parent isn't listed before it.

## Languages

Every language Flareite knows comes from an extension: which files it is for, syntax
highlighting from a [Tree-sitter](https://tree-sitter.github.io/) grammar, highlighting of other
languages embedded in it, and its language server. None of this needs code, so a language-only
extension has no entrypoint. The official HTML & CSS extension is the example used below; the
Rust, TypeScript & JavaScript (three languages: TS, TSX, and JS with JSX), JSON, TOML, YAML,
XML, Python, and Luau extensions are simpler ones. Markdown uses two grammars, one for blocks and one for the inline text it
injects into them, with a language for each; the inline one has no files of its own.

```text
html-css/
  extension.toml
  grammars/
    tree-sitter-html.wasm         Tree-sitter grammars, compiled to WebAssembly
    tree-sitter-css.wasm
    tree-sitter-xml.wasm          SVG's: SVG is XML
  queries/
    html/highlights.scm           what to color
    html/injections.scm           where CSS and JavaScript are embedded
    html/folds.scm                what folds
    css/highlights.scm
    css/folds.scm
    svg/highlights.scm
    svg/injections.scm
    svg/folds.scm
```

```toml
[[languages]]
id = "html"                       # the language id (not prefixed with the extension's id)
name = "HTML"                     # shown in the status bar
file_extensions = ["html", "htm"] # without the dot; may contain dots, like "d.ts"
file_names = []                   # whole file names, like "Makefile"
grammar = "grammars/tree-sitter-html.wasm"
grammar_name = "html"             # optional: the id, with '-' as '_'
highlights = "queries/html/highlights.scm"
injections = "queries/html/injections.scm"   # optional
folds = "queries/html/folds.scm"  # optional; without it, the language folds by indentation
block_comment = ["<!--", "-->"]   # optional; for Toggle Line Comment
# line_comment = "//"             # optional; preferred over block_comment when both are set
```

All paths must be inside the extension's folder, and the files must exist when the extension
loads. The language id is what language servers, `on-language:` activation, settings, and other
languages' injections use, so it is global: a language whose id another extension already
registered is left out (the log says so). Ids are 1 to 32 lowercase
letters, digits, `-`, and `_`.

**Comments.** **Toggle Line Comment** (Ctrl+/) uses `line_comment`, or wraps the lines in
`block_comment` for languages without line comments. Each delimiter is 1 to 8 characters without
spaces. Without either, the command does nothing for the language. The same tokens find region
comments (`// #region Name` … `// #endregion`, or `/* #region Name */` with a block comment)
and `MARK:` comments, so a language gets foldable regions and minimap labels by declaring its
comment syntax.

**Which language a file gets.** A whole file name beats an extension, and a longer extension
beats a shorter one (`d.ts` beats `ts`). When two languages claim a file equally, the one
registered first wins (extensions are loaded in order of name). Names are matched
case-insensitively.

**Files of another extension's language** (API 1.16). An entry without a grammar adds files to a
language another extension declares, as VS Code's language contributions do. It takes only `id`,
`file_extensions`, and `file_names`. Luau's configuration files are JSON:

```toml
[[languages]]
id = "json"
file_names = [".luaurc"]
```

The files get that language's highlighting, server, and schemas. Without an extension that
declares the language, they're plain text. An extension can't add files to a language it declares
itself, which it would list there.

### Grammars

A grammar is a Tree-sitter parser compiled to WebAssembly with `tree-sitter build --wasm`, which
exports `tree_sitter_<grammar_name>`. Flareite uses Tree-sitter 0.27, which loads grammars of
language ABI 13 to 15. Many grammars publish a prebuilt `.wasm`: all the official extensions' grammars are the
files from their npm packages (`tree-sitter-html@0.23.2`, `tree-sitter-css@0.25.0`,
`tree-sitter-rust@0.24.0`, and so on), except XML's, whose npm package has none: it's the
`tree-sitter-xml.wasm` attached to the
[v0.7.0 release](https://github.com/tree-sitter-grammars/tree-sitter-xml/releases/tag/v0.7.0).
Each extension's `grammars` folder has the licenses. To build one yourself:

```sh
npx tree-sitter-cli@0.27.1 build --wasm path/to/tree-sitter-html
```

A grammar runs in its own WebAssembly sandbox, separate from extensions'. It can use only
Tree-sitter's small C library and up to 128 MiB of memory per thread that parses it. It is
loaded and compiled the first time a file of its language is opened, not when Flareite starts.

**Grammars are trusted not to loop.** Flareite stops a parse that takes too long between steps,
but it can't interrupt a grammar's external scanner in the middle of a call. A scanner that
never returns blocks the thread parsing, which can freeze the editor. Use grammars from sources
you trust, as you would any program.

### Highlight queries

`highlights.scm` is a standard Tree-sitter
[highlight query](https://tree-sitter.github.io/tree-sitter/3-syntax-highlighting.html#highlights):
patterns that capture nodes with names like `@keyword` or `@function.method`. Flareite colors a
capture by the first part of its name, using the theme's `syntax-*` colors:

| Capture names                                              | Color     |
| ---------------------------------------------------------- | --------- |
| `comment`                                                  | comment   |
| `keyword`, `include`, `conditional`, `repeat`, `exception` | keyword   |
| `string`, `character`                                      | string    |
| `string.escape`, `escape`                                  | escape    |
| `number`, `float`, `constant.numeric`                      | number    |
| `constant`, `boolean`                                      | constant  |
| `function`, `method`, `constructor`                        | function  |
| `macro`, `function.macro`                                  | macro     |
| `type`                                                     | type      |
| `variable.builtin`                                         | variable  |
| `property`, `field`                                        | property  |
| `operator` / `punctuation` / `attribute` / `tag` / `label` | same name |
| `module`, `namespace`                                      | module    |

Other captures, including plain `@variable`, are left uncolored. Where captures overlap, the
narrower one wins, and for the same node the earlier pattern wins, as highlight queries
expect. Predicates such as `#eq?` and `#match?` work.

### Folding

`folds.scm` captures foldable nodes as `@fold`:

```scheme
[
  (block)
  (declaration_list)
  (arguments)
  (block_comment)
] @fold
```

A region is the captured node's lines. Its first line stays visible when it is folded, and the
rest are hidden. The exception is a last line that closes the first: it starts with `}`, `)`,
`]`, or `</`, the first line ends with an opening bracket or `>`, and it is indented no deeper.
That line stays visible too, so folded code reads `fn main() {⋯` followed by `}`. Capturing
either a body or the whole item gives this shape. Regions that would hide nothing are skipped,
and injected languages fold by their own queries. A language without `folds.scm` folds by
indentation.

### Injections

`injections.scm` marks text in another language, which Flareite parses with that language's
grammar and highlights over the outer language:

```scheme
((style_element (raw_text) @injection.content)
 (#set! injection.language "css"))
```

- **`@injection.content`** is the embedded text. The whole node is used, children included.
- **The language** is a language id, set with `#set! injection.language`, or taken from the
  text of a node captured as `@injection.language` (for example a Markdown code fence's info
  string). A name that isn't a language id is tried as a file extension, so a fence can say
  `rs` or `rust`. It may be the extension's own language or another extension's; an id no language
  has is ignored (HTML's scripts are highlighted only if the TypeScript & JavaScript extension is loaded).
- **`#set! injection.combined`** parses all of a pattern's matches together, as one document.
- **Limits.** Injections are followed 4 levels deep (CSS in HTML in Markdown, say), and a
  document has at most 1,024 injected regions.

Each injected region is parsed incrementally as you type, within the same 3 ms budget as the
document itself; anything longer finishes in the background.

### Language servers

```toml
[permissions]
language_servers = true

[[language_servers]]
name = "vscode-html-language-server"     # unique; also its name in the settings
command = "vscode-html-language-server"  # a program on the PATH, or a path in the folder
args = ["--stdio"]
languages = ["html"]                     # the language ids it serves
root_markers = ["package.json"]          # optional: files marking a project root

[language_servers.settings]              # optional (API 1.16)
"html.format.enable" = false
```

- **Permission.** Declaring language servers requires `language_servers = true`, and the other
  way around. Flareite asks the user before running them, showing each command line. Until the
  user allows it, or if they deny it, the servers don't run; the languages are highlighted
  either way.
- **Flareite runs them**: started on demand when a file of
  one of their languages is opened, with hover, go to definition, references, symbols, completion, and diagnostics.
  The extension's code, if any, has no access to them.
- **Commands.** A command without `/` or `\` is looked up on the `PATH`. A relative path runs a
  program the extension ships, inside its folder. Absolute paths aren't allowed. A server that
  isn't installed shows as failed in the status bar, with a hover explaining why.
- **Names.** Extensions that declare a server with the same name and command share one
  server for all their languages: a Vue extension could declare `typescript-language-server`
  too, and share the one TypeScript & JavaScript starts. With a different command, the first
  declaration wins. Users can
  change an extension's server or turn it off by name, for example
  `[lsp.servers.rust-analyzer]` with `enabled = false` or another `command`.
- **Settings** (API 1.16) answer the server's `workspace/configuration` requests, by section.
  Keys are as VS Code names them: dotted keys (`"luau-lsp.platform.type"`) are the same as nested
  tables. Users' `[lsp.servers.<name>] settings` go over the extension's, and so does a trusted
  folder's `.flareite/settings.toml` (which can set a server's settings, not its command). When
  only settings change, running servers get them with `workspace/didChangeConfiguration`;
  servers whose command line changes restart.
- **Files the server watches.** Servers that register watchers
  (`workspace/didChangeWatchedFiles`) hear of changes to the files they match under their root,
  as `Changed` (or `Deleted`, for files that are gone).
- **Files from the web** (API 1.16). An argument may name a file on the web as
  `${download:https://...}`: Flareite downloads it into its cache folder and puts the file's path
  in its place, so a server can be given type definitions or a database without the extension
  shipping them. A kept copy is used at once, and downloaded again in the background once it's a
  day old. Without one, the server's start waits for the download, and an argument whose file
  can't be downloaded is left out. `https:` only, at most 64 MiB.

#### Configuring servers

An extension's code can change the arguments and settings of the servers it declares, with the
`language-servers` interface (API 1.16), when they depend on its settings or on the open folder:

```rust
use flareite_extension_api::language_servers;

language_servers::set_arguments(
  "luau-lsp",
  &["lsp".into(), format!("--definitions:@roblox=${{download:{TYPES}}}")],
)?;
language_servers::set_settings("luau-lsp", r#"{"luau-lsp.platform.type": "roblox"}"#)?;
```

- **`set-arguments(server, args)`** replaces the manifest's `args`; running instances restart if
  they changed. At most 32 arguments of 1,000 characters.
- **`set-settings(server, json)`** replaces the manifest's `settings` with a JSON object (at most
  256 KiB); running instances are told.
- **`restart(server)`** restarts its running instances.
- They need `language_servers`, and work only on servers the manifest declares. What they set
  lasts until the extension stops, and the user's `[lsp.servers]` settings still go over it.
- **The server waits for `activate`.** A server declared by an extension with code, which
  activates on startup or for one of the server's languages, doesn't start until the extension
  is active (or has failed, or 6 seconds have passed), so `activate` can configure it first. Make
  the manifest's `args` and `settings` the ones for the default settings, so that by default
  nothing changes and the server starts once.

### Trying it

1. Run **Extensions: Load Development Extension…** and choose a checkout of the HTML & CSS
   extension. Nothing needs building.
2. Flareite asks whether to allow the two language servers. Either answer is fine; to get them,
   run `npm install -g vscode-langservers-extracted`.
3. Open an `.html` file: tags, attributes, and comments are colored, and so is the CSS in
   `<style>` and the JavaScript in `<script>`. An `.svg` file is colored the same way.

After editing a query, click **Restart** in the Extensions view: languages whose files changed
are loaded again, and open documents are highlighted with them. An invalid query is logged
(`syntax highlighting is unavailable`, with the query's error) and leaves the file uncolored.

### Language limits

| Item                         | Limit                    |
| ---------------------------- | ------------------------ |
| Languages / language servers | 20 / 10 per extension    |
| File extensions / names      | 50 / 50 per language     |
| Grammar / query file         | 32 MiB / 1 MiB           |
| Grammar memory               | 128 MiB per thread       |
| Injections                   | 4 deep, 1,024 a document |

## Schemas

An extension can provide schemas for TOML, JSON, and YAML files (API 1.16). Flareite checks the
files a schema is for against it as they're edited (squiggles, hover cards, and the Problems view),
and completes and describes their keys and values. Which files are which is up to the languages extensions declare: documents of the `toml`,
`json`, and `yaml` languages, and files ending in `.toml`, `.json`, `.yaml`, or `.yml`. Two kinds
are understood:

- **TOML Schema** (`.tosd`, [tomlschema.org](https://tomlschema.org)), for TOML files. Flareite's
  own [`extension.tosd`](../crates/flareite-schema/schemas/extension.tosd) and
  [`settings.tosd`](../crates/flareite-schema/schemas/settings.tosd) are examples.
- **JSON Schema** (drafts 4 to 2020-12), for files of every format: a TOML file is checked as
  the JSON it converts to, with dates and times as strings, and each document of a YAML file
  (separated by `---`) as the JSON it stands for, by YAML 1.2's core schema (`yes` is a string),
  with aliases expanded and `<<` merge keys merged. VS Code's `markdownDescription`,
  `enumDescriptions`, and `deprecationMessage` are shown. A JSON Schema at an address or path may
  be written in YAML.

Declare them in the manifest; no code or permission is needed:

```toml
[[schemas]]
files = ["*.tidy.toml", ".tidy/config.json"]   # names, or ends of paths with '/'; '!' excludes
path = "schemas/tidy.tosd"                     # a .tosd or .json file in the extension's folder

[[schemas]]
files = ["tidy-remote.json"]
url = "https://example.com/tidy.schema.json"   # or a schema on the web, downloaded and kept
```

A pattern without '/' matches a file's name in any folder (`*` and `?` wildcards, `{a,b}`
alternatives); with '/', the end of its path (`**/` is implied). Up to 32 schemas, each for up to
50 patterns.

**Schemas made by code.** The `schemas` interface's `set-schema` adds a schema the extension makes
as it runs, such as one for a project's own configuration, or replaces the one with the same id:

```rust
use flareite_extension_api::schemas::{self, Schema, SchemaFormat};

schemas::set_schema(&Schema {
  id: "config".into(),
  files: vec!["tidy.toml".into()],
  format: SchemaFormat::TomlSchema,
  content: text,
})?;
```

The schema is loaded when it's set: one that can't be is refused with `invalid-argument`, saying
why. `remove-schema(id)` takes one back. They last until the extension stops, count toward the 32,
and are at most 4 MiB. A document can name one as
`flareite://extensions/<extension id>/schemas/<id>`.

**Conditions.** Completion and hover follow a JSON Schema's `if` to its `then` or `else` (API 1.17)
when the `if` is of the simple kind schemas use to pick a branch: `const`, `enum`, `type`,
`required`, and `properties` of those, tested against the document as it's written. So an object
whose `"$className"` is `"Part"` completes only a part's properties, with this in an `allOf`:

```json
{
  "if": { "properties": { "$className": { "const": "Part" } }, "required": ["$className"] },
  "then": { "properties": { "$properties": { "$ref": "#/definitions/props/Part" } } }
}
```

An `if` of another kind, or one testing a value still being typed, takes both branches.
Checking always follows the schema exactly.

**Which schemas apply** to a file, most specific first: the one the file names itself (`"$schema"`
in JSON, a `#:schema <path or address>` comment before a TOML file's first key,
`[toml-schema] location`, or, in YAML, a `# yaml-language-server: $schema=<path or address>`
comment or a `$schema` key), alone; otherwise every match among the user's (and the open folder's)
`[[schemas.associations]]`, extensions' schemas, and Flareite's own; otherwise the first match in
the [SchemaStore](https://www.schemastore.org) catalog, unless `schemas.catalog` is off.

## Limits and failures

Each extension runs on its own thread with these limits:

| Limit                          | Value                      |
| ------------------------------ | -------------------------- |
| Memory (linear memory)         | 128 MiB                    |
| Memory of all extensions       | 512 MiB together           |
| Instantiation and `activate`   | 5 s                        |
| One command                    | 10 s                       |
| One notification, `deactivate` | 1 s                        |
| WebAssembly stack              | 1 MiB                      |
| Waiting events                 | 1,024 (changes are merged) |

**Failures.** Flareite stops an extension when it traps, panics, runs out of memory or stack, or
misses a deadline. Infinite loops are interrupted at the deadline. The extension is marked
failed, and its diagnostics, decorations, status bar items, and view contents are removed. The
user sees why, with a **Restart** button. The editor and the document are unaffected.

**Shared memory.** Running extensions share a 512 MiB memory pool, besides each one's own
limit. An extension whose memory can't grow because the pool is used up fails with a message
saying so, and a stopped extension returns its memory to the pool.

**Resource use.** Flareite counts, per extension, how often it ran and for how long, its slowest
call, its memory (current and peak), the document changes it skipped because its queue was
full, how long it took to compile, and how long it took to start (compiling, instantiating, and
`activate`). Users see this in the Extensions view (the start time on each row, the rest on its
page) and in **Developer: Toggle Performance Overlay**. A call that takes more than a quarter of its deadline is logged as
`slow extension call`. Keep handlers short: do the minimum per change, and use the changed
ranges instead of re-reading the whole document.

**Commands.** A command that returns `Err(message)` shows the message as an error; the
extension keeps running.

**Restarts.** A failed extension doesn't restart by itself. Restarting creates a fresh instance:
nothing persists between instances.

Every failure listed here is covered by the host's integration tests, which use the
[misbehaving test extension](../extensions/test-fixtures/misbehaving/).

## Installing

Flareite loads installed extensions from the `extensions` folder next to `settings.toml`, for
example `%APPDATA%\dev.flareite.editor\extensions` on Windows. Each extension is a subfolder with
its `extension.toml` and `.wasm` file:

```text
extensions/
  tidy/
    extension.toml        entrypoint = "tidy.wasm"
    tidy.wasm
```

Users install extensions from GitHub releases (see [Publishing](#publishing)), which puts each in
a folder named by its id with `.flareite-install.json`, a record of the repository and release it
came from, for updates. Copying a folder in by hand works too.

If an installed extension and a development extension share an id, the development extension
wins. Set `extensions.enabled = false` to load none.

## Publishing

Publish an extension as a GitHub release with an asset named **`extension.zip`**: the
extension's folder, zipped, with `extension.toml` at its root (or in its one top-level folder) and
its entrypoint inside it. Flareite installs it with **Extensions: Install Extension from
GitHub…** and your repository's name.

- **Versions.** Tag releases with the manifest's version (`v1.2.0` for `version = "1.2.0"`). For
  a prerelease, add its name to the tag and keep the manifest as it is: `v1.2.0-alpha.1` with
  `version = "1.2.0"`. Prereleases are listed to choose from but never offered as updates. Updates
  compare the newest release's tag with the tag the extension was installed from (or its
  manifest's version, if it was copied in by hand), so `v1.2.0` updates `v1.2.0-alpha.1`.
  Releases without an `extension.zip` are skipped.
- **Checks.** The download must match the SHA-256 digest GitHub reports for the asset, be at
  most 100 MiB, and unpack to at most 256 MiB in 10,000 files, with no links and nothing outside
  its folder. The manifest is then checked as an installed extension's is.
- **Listing.** Repositories with the `flareite-extension` topic are listed in the Extensions view
  for users who add their owner to `extensions.catalog` (the official ones are under
  `FlareiteLabs`). The list shows the name, version, description, and author from the
  `extension.toml` of each repository's newest release.

The official extensions publish with a workflow (`.github/workflows/release.yml` in each of their
repositories) that runs when a `v*` tag is pushed. It checks the tag's `major.minor.patch`
against the manifest's version, builds the extension if it has code (with the API from this repository, checked out
beside it), packs the extension's files with the `.wasm` beside the manifest, and creates the
release with `extension.zip`. Prerelease tags (`v1.3.0-alpha.1`) become prereleases. Copy it to
publish yours the same way.

## Testing

**Unit tests.** Test your logic as ordinary Rust on your host target. The host functions are
unavailable there, so keep them at the edges:

```sh
cd extensions
cargo test -p tidy --target x86_64-pc-windows-msvc    # or your host's target triple
```

**Integration tests.** The extension host's tests (`crates/flareite-extension-host/tests/host.rs`)
build the extensions workspace and run the extensions against a real editor core. They are the
model for testing an extension end to end. Run them with
`cargo test -p flareite-extension-host`.

## Other languages

Any language that can produce a WebAssembly component for the `extension` world works. The
component must import nothing but the Flareite interfaces (`flareite:extension/host`,
`flareite:extension/workbench`, `flareite:extension/files`, `flareite:extension/viewers`,
`flareite:extension/webviews`, `flareite:extension/storage`, `flareite:extension/workspace`,
`flareite:extension/processes`, `flareite:extension/explorer`, `flareite:extension/views`,
`flareite:extension/prompts`, `flareite:extension/documents`, `flareite:extension/quick-diff`, and
`flareite:extension/timers`, `flareite:extension/layouts`, `flareite:extension/links`,
`flareite:extension/schemas`, `flareite:extension/language-servers`, `flareite:extension/downloads`,
and `flareite:extension/secrets`). The host provides no WASI, so
a component that imports `wasi:*` interfaces (most toolchains' defaults) fails to start, with a
message saying so. Rust on `wasm32-unknown-unknown` is the supported path for now.

## Versioning

The WIT package is versioned: `flareite:extension@1.17.0`, `api_version = "1"`.

- **Additions.** New host functions arrive as minor versions of the package. Version 1.1 added the
  `workbench` interface, 1.2 added previews to it, 1.3 added the `files` interface, 1.4 the
  `viewers` interface, 1.5 the `webviews` and `storage` interfaces, 1.6 the `workspace`, `processes`, and `explorer`
  interfaces, 1.7 the `views` and `prompts` interfaces, with `files::delete`,
  `workspace::open-file`, and `host::set-configuration`, 1.8 the `documents` interface, and 1.9 the
  `quick-diff` interface, 1.10 the `timers` interface, 1.11 `views::set-graph`,
  `prompts::pick-folder`, `workspace::open-folder`, and `workbench::active-file`, and 1.12
  `workbench::set-code-lenses` and `documents::open-merge` (with whole-line decorations in the
  manifest), 1.13 `workbench::set-line-annotations` and `views::set-visible` (with
  `on_selection_changed` and hidden views in the manifest), 1.14
  `workbench::render-markdown`, and 1.15 the `layouts` and `links` interfaces and
  `documents::save` (with `[[layouts]]`, `[[link_handlers]]`, keybindings' `layouts`, the
  `web_pages` permission, and theme tokens as decoration colors in the manifest;
  `documents::open-diff` and `open-merge` fail since), and 1.16 the `schemas` and
  `language-servers` interfaces (with `[[schemas]]`, language servers' `settings` and
  `${download:...}` arguments, `[[languages]]` entries without a grammar, and
  `on_configuration_changed` in the manifest), and 1.17 the `downloads` and `secrets`
  interfaces, `processes::spawn-streaming`, `prompts::input-password`, and
  `workspace::open-externally` (with `on-folder-contains` activation and the `cube` view icon in
  the manifest).
  Wasmtime links components built against an older minor version.
- **The rename.** The package was `ferrite:extension` until the project became Flareite; components
  built against that name don't link and must be rebuilt
  ([ADR 0035](adr/0035-the-flareite-rename.md)).
- **Breaking changes.** Changes that break existing extensions get a new major version and a new
  `api_version`.
- **Rejection.** Manifests for unsupported versions are rejected before loading.
