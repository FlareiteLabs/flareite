# Flareite

A fast, memory-efficient, extensible code editor built with Rust, Tauri, React, and a sandboxed
WebAssembly extension system.

> **Status:** All eight milestones of the initial plan are complete. Milestone 8 (hardening)
> added recovery of unsaved changes after a crash, screen reader and high-contrast support,
> per-extension resource accounting, CI on Windows, macOS, and Linux, and performance regression
> checks. See [`docs/DESIGN.md`](docs/DESIGN.md) for the brief and milestone plan,
> [`docs/performance.md`](docs/performance.md) for measurements, and
> [`CONTRIBUTING.md`](CONTRIBUTING.md) for how to work on Flareite.

## Downloading

Each [release](https://github.com/FlareiteLabs/flareite/releases) has installers for Windows
(`.msi`, or the `.exe` setup), macOS (a `.dmg` for both Apple silicon and Intel), and Linux
(`.deb`, `.rpm`, or an `.AppImage`). They aren't signed yet, so the system warns before the first
start:

- **Windows.** SmartScreen says it protected your PC: choose **More info**, then **Run anyway**.
- **macOS.** Gatekeeper refuses to open an app from an unidentified developer, or says it's
  damaged: after moving Flareite to Applications, run `xattr -cr /Applications/Flareite.app`
  once, or allow it under **System Settings → Privacy & Security**.
- **Linux.** Make the AppImage executable (`chmod +x Flareite_*.AppImage`) before running it.

To build it yourself instead, see below.

## Prerequisites

| Tool                                                | Version used                                 |
| --------------------------------------------------- | -------------------------------------------- |
| [Rust](https://rustup.rs) (stable, MSVC on Windows) | 1.99.0                                       |
| [Node.js](https://nodejs.org)                       | 26.x                                         |
| [pnpm](https://pnpm.io)                             | 10.27.0 (pinned via `packageManager`)        |
| Platform dependencies for Tauri 2                   | see the [Tauri prerequisites][tauri-prereqs] |

On Windows this means the Visual Studio C++ build tools and the WebView2 runtime (preinstalled on
Windows 11). Linux needs WebKitGTK 4.1 development packages. Building extensions, and the
extension host's tests, also need `rustup target add wasm32-unknown-unknown`.

[tauri-prereqs]: https://v2.tauri.app/start/prerequisites/

## Getting started

```sh
pnpm install
pnpm tauri dev
```

`pnpm tauri dev` starts the Vite dev server on port 1420, builds the Rust backend, and opens the
desktop window. Frontend changes hot-reload; Rust changes trigger a rebuild and restart.

Files passed on the command line open as tabs, for example
`target/debug/flareite path/to/file.rs` (the debug binary needs `pnpm dev` running).

## Editing

Flareite opens text files of any size, in UTF-8, UTF-16, or a legacy encoding such as Windows-1252
or Shift JIS, which it detects and shows in the status bar. Click the encoding to reopen or save the
file with another one. Files are saved in the encoding they were read in, and line endings and a
byte order mark are preserved exactly; new lines use the file's existing line ending. A file that
looks binary opens in a tab that asks first (**Open Anyway** opens it as text), unless an
extension's viewer shows that kind of file: the official Images extension shows PNG,
JPEG, GIF, WebP, BMP, ICO, and AVIF files as pictures. **View: Reopen Editor With…** switches a
file between text and the viewers that can show it.

| Action                              | Windows / Linux                  | macOS                    |
| ----------------------------------- | -------------------------------- | ------------------------ |
| Command palette                     | Ctrl+Shift+P, F1                 | ⇧⌘P, F1                  |
| Go to file / Go to line             | Ctrl+P / Ctrl+G                  | ⌘P / ⌃G                  |
| Go to symbol in editor / workspace  | Ctrl+Shift+O / Ctrl+T            | ⇧⌘O / ⌘T                 |
| New file / Open / Open folder       | Ctrl+N / Ctrl+O / Ctrl+K Ctrl+O  | ⌘N / ⌘O / ⌘K ⌘O          |
| Close tab / Reopen closed tab       | Ctrl+W / Ctrl+Shift+T            | ⌘W / ⇧⌘T                 |
| Close saved / all tabs in group     | Ctrl+K U / Ctrl+K W              | ⌘K U / ⌘K W              |
| Close all editors / Close folder    | Ctrl+K Ctrl+W / Ctrl+K F         | ⌘K ⌘W / ⌘K F             |
| Save / Save As / Save all           | Ctrl+S / Ctrl+Shift+S / Ctrl+K S | ⌘S / ⇧⌘S / ⌘K S          |
| Find / Replace                      | Ctrl+F / Ctrl+H                  | ⌘F / ⌥⌘F                 |
| Next / previous match               | F3 / Shift+F3 (Enter in find)    | F3 / ⇧F3                 |
| Add next occurrence                 | Ctrl+D                           | ⌘D                       |
| Find in files                       | Ctrl+Shift+F                     | ⇧⌘F                      |
| Extensions view                     | Ctrl+Shift+X                     | ⇧⌘X                      |
| Problems / Output                   | Ctrl+Shift+M / Ctrl+Shift+U      | ⇧⌘M / ⇧⌘U                |
| Explorer / Toggle side bar          | Ctrl+Shift+E / Ctrl+B            | ⇧⌘E / ⌘B                 |
| Terminal / New / Toggle panel       | Ctrl+\` / Ctrl+Shift+\` / Ctrl+J | ⌃\` / ⌃⇧\` / ⌘J          |
| Next / previous tab (in its group)  | Ctrl+Tab, Ctrl+PageDown / PageUp | ⌃Tab, ⌃PageDown / PageUp |
| Tab 1 to 9 / last tab (in group)    | Alt+1 … Alt+9 / Alt+0            | ⌃1 … ⌃9 / ⌃0             |
| Next / previous group               | F6 / Shift+F6                    | F6 / ⇧F6                 |
| Group 1 to 9 (outside the side bar) | Ctrl+1 … Ctrl+9                  | ⌘1 … ⌘9                  |
| Split tab right                     | Ctrl+\                           | ⌘\                       |
| Move tab to the left / right group  | Ctrl+Alt+← / →                   | ⌃⌘← / →                  |
| Zoom in / out / reset               | Ctrl+= / Ctrl+- / Ctrl+0         | ⌘= / ⌘- / ⌘0             |
| Settings / Keyboard shortcuts       | Ctrl+, / Ctrl+K Ctrl+S           | ⌘, / ⌘K ⌘S               |
| Undo / Redo                         | Ctrl+Z / Ctrl+Y, Ctrl+Shift+Z    | ⌘Z / ⇧⌘Z                 |
| Cut / Copy / Paste / Select all     | Ctrl+X / C / V / A               | ⌘X / C / V / A           |
| Move by word                        | Ctrl+←/→                         | ⌥←/→                     |
| Line start (smart) / end            | Home / End                       | ⌘← / ⌘→                  |
| Document start / end                | Ctrl+Home / Ctrl+End             | ⌘↑ / ⌘↓                  |
| Delete word                         | Ctrl+Backspace / Ctrl+Delete     | ⌥Backspace / ⌥Delete     |
| Insert line below / above           | Ctrl+Enter / Ctrl+Shift+Enter    | ⌘Enter / ⇧⌘Enter         |
| Move line up / down                 | Alt+↑ / ↓                        | ⌥↑ / ↓                   |
| Copy line up / down                 | Shift+Alt+↑ / ↓                  | ⌥⇧↑ / ↓                  |
| Delete line                         | Ctrl+Shift+K                     | ⇧⌘K                      |
| Indent / outdent line               | Ctrl+] / Ctrl+[, Tab / Shift+Tab | ⌘] / ⌘[, Tab / ⇧Tab      |
| Toggle line comment                 | Ctrl+/                           | ⌘/                       |
| Go to matching bracket              | Ctrl+Shift+\                     | ⇧⌘\                      |
| Fold / unfold                       | Ctrl+Shift+[ / Ctrl+Shift+]      | ⌥⌘[ / ⌥⌘]                |
| Toggle fold                         | Ctrl+K Ctrl+L                    | ⌘K ⌘L                    |
| Fold / unfold recursively           | Ctrl+K Ctrl+[ / Ctrl+K Ctrl+]    | ⌘K ⌘[ / ⌘K ⌘]            |
| Fold all / unfold all               | Ctrl+K Ctrl+0 / Ctrl+K Ctrl+J    | ⌘K ⌘0 / ⌘K ⌘J            |
| Fold all / unfold all regions       | Ctrl+K Ctrl+8 / Ctrl+K Ctrl+9    | ⌘K ⌘8 / ⌘K ⌘9            |
| Fold level 1 to 7                   | Ctrl+K Ctrl+1 … Ctrl+K Ctrl+7    | ⌘K ⌘1 … ⌘K ⌘7            |
| Add cursor above / below / at click | Ctrl+Alt+↑ / ↓, Alt+click        | ⌘⌥↑ / ↓, ⌥click          |
| Select word / one cursor            | Double-click / Escape            | Double-click / Escape    |
| Select line / all occurrences       | Ctrl+L / Ctrl+Shift+L            | ⌘L / ⇧⌘L                 |
| Cursors at line ends / cursor undo  | Shift+Alt+I / Ctrl+U             | ⌥⇧I / ⌘U                 |
| Scroll a line up / down             | Ctrl+↑ / ↓                       | ⌃↑ / ↓                   |
| Go to definition                    | F12, Ctrl+click                  | F12, ⌘click              |
| Go to references / next, previous   | Shift+F12 / F4, Shift+F4         | ⇧F12 / F4, ⇧F4           |
| Trigger suggestions                 | Ctrl+Space                       | ⌃Space                   |

With nothing selected, cut and copy take the whole line, and pasting it inserts it as a line
above the cursor, as in VS Code. Tab indents when the selection spans lines. Toggle Line Comment
uses the comment syntax the language's extension declares; languages without line comments (HTML,
CSS) get a block comment around the lines.

Code folds where the language's fold query says (blocks, bodies, long argument lists, comments),
or by indentation for languages without one. Fold markers appear in the gutter when the pointer
is over it; click one to fold, or click the ⋯ after a folded line to unfold. A folded region
moves, copies, and deletes with its first line, and searching or undoing into it unfolds it.
Flareite remembers what was folded in each file.

Region comments mark foldable regions of your own, written with the language's comment syntax:

```rust
// #region Helpers
fn a() {}
// #endregion
```

(`# region` in Python and TOML, `/* #region */` in CSS, `<!-- #region -->` in HTML.) Folding one
hides it through its `#endregion` line. The minimap labels named regions, and `// MARK: Name`
comments too (`// MARK: - Name` adds a separator line). `editor.minimap_region_headers` and
`editor.minimap_mark_headers` turn the labels off.

A minimap beside the scrollbar shows the whole document small, in its syntax colors. Click it to
jump to that part of the file, or drag its slider to scroll. **View: Toggle Minimap** hides or
shows it. The editor scrolls past the last line until it's the only one visible, and clicking
below the text moves the cursor to the end of the last line.

Breadcrumbs above the editor show where the file is, such as `src › components › Palette.tsx`.
Click a part to show it in the Explorer.

In the find widget, Alt+C, Alt+W, and Alt+R toggle Match Case, Whole Word, and Regular
Expression; in the replace field, Enter replaces and Ctrl+Enter replaces all. Regex
replacements can use `$1` and `${name}`.

Hold Shift with any movement key to extend the selection. Closing a tab or the window with
unsaved changes asks whether to save them. If another program changes an open file, Flareite
reloads it when you have no unsaved changes, and otherwise keeps your version and asks before
overwriting when you save.

## Languages

Flareite has no languages built in: each comes from an extension, so you add exactly the ones you
edit. Without any, files are plain text. A file's language comes from its name and is shown in the
status bar; click it (or run **View: Change Language Mode**, `Ctrl+K M`) to choose another one,
plain text, or to go back to detecting it. The choice lasts while the file is open. A language
extension brings a Tree-sitter grammar compiled to WebAssembly for highlighting, and usually
declares a language server. The official ones (`flareite.*`) each have a repository of their
own:

| Extension               | Languages                        | Language server                   | Install the server with                                |
| ----------------------- | -------------------------------- | --------------------------------- | ------------------------------------------------------ |
| Rust                    | Rust                             | `rust-analyzer`                   | `rustup component add rust-analyzer`                   |
| TypeScript & JavaScript | TypeScript, TSX, JavaScript, JSX | `typescript-language-server`      | `npm install -g typescript-language-server typescript` |
| JSON                    | JSON                             | —                                 |                                                        |
| TOML                    | TOML, `Cargo.lock`               | —                                 |                                                        |
| Python                  | Python                           | `pyright-langserver`              | `npm install -g pyright`                               |
| HTML & CSS              | HTML (with CSS, JS), CSS, SVG    | `vscode-html/css-language-server` | `npm install -g vscode-langservers-extracted`          |
| XML                     | XML                              | —                                 |                                                        |
| Markdown                | Markdown                         | —                                 |                                                        |

Install one from the **Official Extensions** in the Extensions view (see
[Installing extensions](#installing-extensions)). Flareite asks before running an extension's
language server; highlighting works either way.

With a language server, Flareite also shows errors and warnings as you type (squiggles, and in
**View: Problems** with every file's, counted in the status bar), shows type information and documentation when the pointer rests on a
symbol, goes to definitions (F12 or Ctrl/⌘+click; holding Ctrl/⌘ underlines the symbol under the
pointer when it has one), finds references (Shift+F12, or Ctrl/⌘+click on a definition: one is
gone to, several open a peek below the line with a preview and a list by file; F4 and Shift+F4
step through them, Enter or a double-click opens one, Escape closes it), goes to symbols
(Ctrl/⌘+Shift+O lists the file's, previewing each in the editor, and `@:` groups them by kind;
Ctrl/⌘+T searches the whole workspace), and offers completions (as you type, after `.` and similar
characters, or with Ctrl+Space). Servers start when you open a file that needs one. The status
bar shows the server's state; hover it for details if a server is missing or failed. Servers can
be changed, added, or turned off in the `[lsp]` section of the settings.

## Extensions

Extensions are WebAssembly modules that run in a sandbox: they can only use Flareite's
[extension API](crates/flareite-extension-api/wit/extension.wit). With it they add commands to
the palette, read open documents, edit them, and show problems as squiggles. They can also
customize the workbench: color themes, file icon themes, keybindings, context menu items, status
bar items, side bar views, highlights in the text, previews (formatted pages, such as rendered
Markdown), and tabs split into editors and pages with buttons above them (layouts, such as Git's
diff and merge editors), mark files in the explorer, and add languages. They have no access to files, the
network, or anything else on your computer beyond the permissions they request. Before an
extension starts, Flareite asks you to allow them: reading or editing open documents or the open
folder's files, or running the language servers or programs (such as Git) it declares, which
run only in folders you [trust](#workspace). The
Extensions view (`Ctrl+Shift+X`) lists each extension with its icon, name, description, author,
and how long it took to start (hover over one for the rest). Choose one to open its page: its
README, permissions (change them there), commands, what it adds, and its resource use, with
buttons to restart it, turn it off, or open its settings or log. The gear on each row has the
same actions. An extension that crashes, loops, or uses too
much memory is stopped without affecting the editor.

Besides the [language extensions](#languages), the official extensions, each in a repository of
its own, are:

- **Git** adds a Source Control view: stage, unstage, and discard changes, commit them
  (Ctrl+Enter in the message box), amend or undo the last commit, and browse the current branch's
  commits and the files each changed. It shows the Git status of files in the explorer, and the
  branch you're on in the status bar, with `*` when there are changes and how far it is ahead of
  or behind its upstream. Its diff tabs (side by side or inline) and merge editor are its own
  layouts. It needs [Git](https://git-scm.com) installed.
- **HTML & CSS** adds HTML, CSS, and SVG, and shows HTML files as live web pages: beside the
  file's editor in one tab (**HTML Preview**), or in a tab of their own (**HTML Page**), with the
  buttons on an HTML file's tab or **View: Reopen Editor With…**. Pages show again as you type,
  in [trusted](#workspace) folders.
- **Markdown** adds Markdown highlighting, and **Markdown: Open Preview** (`Ctrl+Shift+V`) and
  **Open Preview to the Side** (`Ctrl+K V`), which show the page rendered and update it as you
  type.
- **Images** shows image files as pictures you can zoom, on the background you choose.
- **Browser** opens web links you Ctrl+click in tabs that show the page, with back, forward,
  reload, and an address to go to, instead of in your browser (it asks to show pages from the
  web).

Four examples are included here:

- [Tidy](extensions/tidy/) flags trailing whitespace and long lines, and can trim them.
- [TODOs](extensions/todos/) highlights TODO and FIXME comments and lists them in a side bar
  view. It shows the count in the status bar, jumps to the next one with `Ctrl+Alt+T`, and adds
  **Insert TODO Comment** to the editor's context menu.
- [Example Themes](extensions/themes/) adds the Midnight and Paper color themes, with no code.
- [Example File Icons](extensions/icons/) adds the Colorful file icon theme, with no code.

To try them:

```sh
rustup target add wasm32-unknown-unknown
cd extensions && cargo build --release
```

Then run **Extensions: Load Development Extension…** and choose an extension's folder, such as
`extensions/todos`. The official extensions with code build the same way in their own
repositories (`cargo build --release`), which expect this one checked out beside them as
`editor`, for the extension API. See [`docs/extensions.md`](docs/extensions.md) for writing your
own.

### Installing extensions

Extensions are installed from their GitHub releases:

- **Official Extensions**, below the installed ones in the Extensions view, lists the official
  extensions you don't have, each with an **Install** button. They're the repositories of the
  owners in `extensions.catalog` (`["FlareiteLabs"]` unless you change it) that have the
  `flareite-extension` topic. The list is fetched from GitHub when the view first opens in a
  session; `catalog = []` turns it off.
- **Extensions: Install Extension from GitHub…** (the ⤓ button in the view's header) asks for a
  repository (`owner/name` or its address), then lists its releases to choose one, newest first.

Flareite downloads the release's `extension.zip`, checks it against the digest GitHub publishes
for it, unpacks it, and checks its manifest. Then it asks, showing the extension's name, version,
author, and the permissions it requests, before installing it into the `extensions` folder next
to `settings.toml`. It still asks before the extension uses any of those permissions.

- **Updates.** **Extensions: Check for Extension Updates** looks for newer releases of the
  extensions installed from GitHub, marks them in the Extensions view, and offers to install
  them. An extension's gear menu and page have **Update to …** once one is found, and
  **Install Another Version…** to choose any release.
- **Uninstalling.** **Uninstall…** in an extension's gear menu deletes its folder, with the
  values it stored and the permissions you gave it; its settings stay in `settings.toml`.
- **By hand.** Copying an extension's folder (manifest and `.wasm`) into the `extensions` folder
  installs it too.

Flareite uses the network only for these, never in the background. GitHub allows 60 requests an
hour without signing in; listing the official extensions takes one for each of them.

## Customizing

- **Themes.** **Preferences: Color Theme: …** in the command palette switches between Dark, Light,
  Follow System, and themes from extensions. You can override any theme color in the settings.
- **File icons.** **Preferences: File Icon Theme: …** switches the file icons in the explorer
  and Go to File between the built-in ones and icon themes from extensions.
- **Keybindings.** Set keybindings in the settings by command id. **Preferences: Keyboard
  Shortcuts** lists every command with its id and keys. Your keybindings take precedence over the
  built-in ones, which take precedence over extensions'.

```toml
[workbench]
theme = "example.themes.midnight"
icon_theme = "example.icons.colorful"

[workbench.colors]
editor-background = "#101018"            # any token in src/themes/tokens.json

[keybindings]
"workbench.showSearch" = "mod+shift+h"   # "mod" is Ctrl, or Cmd on macOS
"example.todos.next" = ["f8", "ctrl+alt+t"]
"workbench.toggleSidebar" = ""           # remove a command's keys
```

## Recovering unsaved changes

Unsaved changes are backed up in the background while you work. If Flareite crashes, is killed,
or the computer loses power, the next start reopens them as unsaved tabs and says so. Closing
Flareite normally still asks about unsaved changes, and changes you choose not to save are not
brought back.

If part of the window shows "stopped working because of an internal error", choose **Try
Again**, or **Reload Window** (also **Developer: Reload Window** in the command palette).
Reloading keeps open documents and unsaved changes. See
[`docs/troubleshooting.md`](docs/troubleshooting.md) for more.

## Accessibility

- **Screen readers.** The editor exposes the cursor's line and the selection to screen readers,
  so they can read the line, character, word, or selection as you move. Errors and
  notifications are announced.
- **Keyboard.** Commands, views, lists, and menus work from the keyboard. Context menus open
  with `Shift+F10` or the menu key. Groups are resized with the arrow keys on their edges, `F6`
  moves between them, and every docking action has a command (see [Layout](#layout)).
  `` Ctrl+` `` moves between the editor and the terminal.
- **Terminal output.** Set `terminal.screen_reader = true` to have screen readers announce
  terminal output.
- **System settings.** Windows contrast themes (forced colors) and "reduce motion" are honored.
- **Fonts and colors.** The editor font is a setting (`editor.font_size`, `editor.font_family`),
  and any theme color can be overridden (see [Customizing](#customizing)).

## Workspace

Open a folder with **File: Open Folder…** (command palette or the Explorer). The explorer shows
`.gitignore`d entries dimmed, along with everything inside an ignored folder. Files with errors or
warnings (those listed in **View: Problems**) are colored and show how many, and so are the
folders that contain them, with a dot. Extensions can mark files too: the official Git
extension colors changed files and shows a letter (**M** modified, **U** untracked, and so on).
It follows the file you're editing: switching tabs, going to a
definition, or opening a search result expands the file's folders and selects it.

**Workspace Trust.** Programs that work on a folder can run code from it: a language server
builds the project, and Git runs hooks. So the first time you open a folder, Flareite asks
whether you trust its files (you can trust every folder in its parent instead). Until you do, it
runs in Restricted Mode, shown in the status bar: language servers don't start and extensions
can't run programs, while editing and everything else work. Click **Restricted Mode**, or run
**Workspaces: Manage Workspace Trust**, to change your mind. Decisions are kept in
`trusted-folders.json` next to `settings.toml`; `security.workspace_trust = false` trusts every
folder.

To add a file or folder, use the **New File** and **New Folder** buttons in the Explorer header,
right-click a folder or file, or run **Explorer: New File…** / **Explorer: New Folder…**. The new
entry goes inside the selected folder, or beside the selected file. Right-clicking the empty space
below the files creates at the top level. Type a name and press
`Enter` (or `Escape` to cancel); a name like `src/net/http.rs` creates the missing folders too.
New files open in the editor. Go to File and find in files skip them, along with paths matching
`files.exclude`. The folder, open files, and layout are restored on the next launch.

Right-click a file or folder for the rest, or use these keys while the explorer has focus:

| Action                                      | Keys (Windows and Linux)                      |
| ------------------------------------------- | --------------------------------------------- |
| Rename                                      | `F2` (`Enter` on macOS)                       |
| Delete to the Recycle Bin / Trash           | `Delete` (`Cmd+Backspace` on macOS)           |
| Delete permanently                          | `Shift+Delete` (`Cmd+Alt+Backspace` on macOS) |
| Cut, copy, paste                            | `Ctrl+X`, `Ctrl+C`, `Ctrl+V`                  |
| Copy path, copy path relative to the folder | `Shift+Alt+C`, `Ctrl+Shift+Alt+C`             |
| Reveal in File Explorer (or Finder)         | `Shift+Alt+R`                                 |

Deleting asks first. Pasting where the name is taken adds ` copy` (`notes copy.md`). Drag a file or
folder onto a folder to move it, or hold `Ctrl` (`Alt` on macOS) to copy it; hovering over a
closed folder opens it. Files and folders dragged in from another program (File Explorer,
Finder) are copied into the folder they're dropped on. Open files follow when they're renamed or moved, and their language
servers and extensions see the new name. Files of a deleted folder stay open, as when another
program deletes them. Extensions can add their own items, for files, folders, or files with
certain names, such as Tidy's **Trim Trailing Whitespace in Files**.

## Layout

Everything is a tab: documents, the explorer, search, extensions, extension views, and terminals.
Tabs sit in groups, and any tab can go in any group, beside any other.

- **Docking.** Drag a tab (or an activity bar button) onto another group's tabs to join it, onto
  a side of a group to split it, or near an edge of the window to dock along that edge. A
  highlight shows where it will land. `Escape` cancels a drag.
- **Opening files by dropping them.** Files dragged in from another program open where they're
  dropped, the same way: among a group's tabs, beside a group, or along an edge. They don't need to
  be in the open folder. Dropped in the middle of a group that isn't for documents (the side bar's
  views, say), they open where documents open; dropped on the explorer, they're copied into it
  instead. A dropped folder becomes the open folder.
- **From the keyboard.** The tab menu (right-click, `Shift+F10`, or the menu key) splits or moves
  a tab. The same actions are commands: **View: Split Tab Right/Down**, **View: Move Tab
  Left/Right/Up/Down** (into the neighbouring group, or a new one), **View: Close Tab**, and
  **View: Reset Layout**. In a tab strip, the arrow keys move between tabs and `Delete` closes one.
- **Closing and reopening.** `Ctrl+W` closes the tab you're working in, whatever it is: a
  document, a view, Settings. In a terminal, `Ctrl+W` goes to the shell; close a terminal from its
  tab. **View: Reopen Closed Tab** (`Ctrl+Shift+T`) brings back the last closed file, view, or
  page where it was. Terminals (their shell has ended), previews, and untitled files don't come
  back. **Close Other Tabs**, **Close Saved Tabs in Group**, and **Close All Tabs in Group** act on
  the current group; **Close All Editors** closes every document, preview, and page, leaving views
  and terminals.
- **The side bar** is whatever is docked along the left edge, beside the activity bar. The
  activity bar has a button for each tab there (views, terminals, documents), and for views that
  aren't open; something docked elsewhere has no button. Drop a tab on the activity bar to move
  it into the side bar.
- **Hiding.** An activity bar button shows its tab, or hides its group when it's already
  showing. **View: Toggle Side Bar** (`Ctrl+B`) hides or shows the side bar, and **View: Toggle
  Panel** (`Ctrl+J`) the groups along the bottom (terminals, Problems, Output), keeping their
  places.
- **Problems and Output** open along the bottom, with the terminals. **Problems**
  (`Ctrl+Shift+M`, or the counts in the status bar) lists what's wrong. First, problems with
  Flareite and its extensions: an extension that stopped, settings that were ignored, and the
  warnings and errors notifications showed (notifications disappear after a while, so this is
  where to find them again). Then the errors, warnings, and information in files, by file:
  those in open files, from language servers and extensions, and those a language server
  reports for files that aren't open (rust-analyzer checks the whole folder, for example).
  Choose one to go to it. Groups collapse, and the arrow keys move between rows. **Output** (`Ctrl+Shift+U`) shows recent log messages from Flareite, from each extension,
  and from each language server (including what the server prints), with **Open Log File** for
  the full log. Each extension's page and its gear menu in the Extensions view have **Show
  Log**.
- **Documents** open in the group last used for documents. The main group (on the right at
  first) stays when empty and shows the welcome page.

The layout is restored on the next launch, with documents found again by path.

## Terminal

**View: Toggle Terminal** (`` Ctrl+` ``) opens a terminal, docked along the bottom at first, in
the workspace folder; pressed in the terminal, it returns to the editor. **Terminal: New
Terminal** (`` Ctrl+Shift+` ``) or the **+** button beside a terminal's tabs opens another. Like
any tab, a terminal can be docked anywhere.

- **Shells.** Flareite finds the shells installed on your computer:
  - **Windows:** PowerShell 7, Windows PowerShell, Command Prompt, Git Bash, Cygwin, MSYS2, and
    each WSL distribution, such as "Ubuntu (WSL)".
  - **macOS and Linux:** the shells listed in `/etc/shells`, such as bash, zsh, and fish.

  The arrow beside the **+** button, or **Terminal: New Terminal With Profile…**, starts a
  terminal with any of them. New terminals otherwise use the default profile: PowerShell on
  Windows (PowerShell 7 if installed), `$SHELL` elsewhere. Change it with **Terminal: Select
  Default Profile…**, or `terminal.default_profile`. In settings, you can also add profiles,
  change a found one's arguments or environment, or hide one (see [Settings](#settings)).

- **Keys.** While the terminal has focus, keys go to the shell, except the command palette, Go
  to File, showing views, switching tabs and groups, settings, and terminal commands.
- **Links.** Ctrl+click (Cmd+click on macOS) a web address or a file path in the output: a web
  address opens in your browser, a file in the editor (at the line and column a path like
  `src/main.rs:12:5` or `Program.cs(12,5)` names), a folder in your file manager. Relative paths
  are found from the shell's folder. Web addresses in the editor's text and links in previews
  open the same way. Extensions can open links their own way (the **Browser** extension opens web
  pages in tabs); `workbench.open_web_links` and `open_file_links` choose, and Ctrl+Shift+click
  asks.
- **Clipboard.** On Windows, `Ctrl+C` copies when text is selected and `Ctrl+V` pastes; on Linux
  use `Ctrl+Shift+C` and `Ctrl+Shift+V`; on macOS, `Cmd+C` and `Cmd+V`.
- **Lifetime.** `exit` closes the tab. A shell that fails leaves its tab open until you press a
  key. Terminals keep running when hidden or when the window reloads, and end when Flareite
  quits.
- **On Windows**, Flareite uses the console host from Windows Terminal, which it installs beside
  `flareite.exe`, rather than the one built into Windows, which loses or repeats lines when a
  terminal is resized. The first Windows build downloads it (see
  [ADR 0011](docs/adr/0011-integrated-terminal.md#resizing-on-windows-2026-10-30)).
- **Colors.** The ANSI colors are theme tokens (`terminal-ansi-red`, and so on), so themes and
  `workbench.colors` can change them.

## Settings

Run **Preferences: Open Settings** (`Ctrl+,`) to see every setting, including those your
extensions declare, with its value and description. Changes are written to `settings.toml` and
apply at once. Search by name, key, or description; `@modified` shows what you've set, and
`@ext:<id>` shows one extension's settings (the **Settings** button on an extension's page
opens them).

**Preferences: Open Settings (TOML)** opens the file itself; saving applies it. Theme color
overrides, language servers, terminal profiles, and keybindings are edited there. Every available
setting is listed, commented out, in a new file. Invalid settings are reported and the previous
ones stay in effect.

```toml
[editor]
tab_size = 2
font_size = 15

[workbench]
theme = "light"   # "system", "dark", "light", or an extension's theme id
icon_theme = "default"   # or an extension's file icon theme id

[files]
exclude = [".git", "dist"]

[lsp.servers.rust-analyzer]
command = "C:/tools/rust-analyzer.exe"   # use a specific build

[terminal]
default_profile = "Ubuntu (WSL)"         # a profile's name; empty: PowerShell, or $SHELL
scrollback = 5000
screen_reader = true                     # announce terminal output (slower)

[terminal.profiles."Nushell"]
command = "C:/tools/nu.exe"              # add a profile
args = []

[terminal.profiles."Command Prompt"]
enabled = false                          # hide one that was found

[extensions.settings."example.tidy"]
max_line_length = 120                    # settings an extension declares
```

| Platform | Settings                                             | Session state                                        |
| -------- | ---------------------------------------------------- | ---------------------------------------------------- |
| Windows  | `%APPDATA%\dev.flareite.editor\settings.toml`        | `%LOCALAPPDATA%\dev.flareite.editor\state.json`      |
| macOS    | `~/Library/Application Support/dev.flareite.editor/` | `~/Library/Application Support/dev.flareite.editor/` |
| Linux    | `~/.config/dev.flareite.editor/`                     | `~/.local/share/dev.flareite.editor/`                |

## Commands

| Command                                                 | What it does                                                    |
| ------------------------------------------------------- | --------------------------------------------------------------- |
| `pnpm tauri dev`                                        | Run the desktop app in development mode                         |
| `pnpm tauri build`                                      | Build a release binary and platform installers                  |
| `pnpm tauri build --no-bundle`                          | Build the release binary only (`target/release/flareite[.exe]`) |
| `pnpm dev`                                              | Run only the frontend in a browser (no backend; IPC is stubbed) |
| `pnpm test`                                             | Run frontend tests (Vitest + Testing Library)                   |
| `pnpm typecheck`                                        | Type-check the frontend                                         |
| `pnpm format`                                           | Format frontend (Prettier) and Rust (`cargo fmt`) sources       |
| `pnpm format:check`                                     | Check formatting without writing                                |
| `pnpm bench`                                            | Criterion microbenchmarks (`benches/`)                          |
| `pnpm bench:app all`                                    | End-to-end startup, typing, and scrolling benchmarks (release)  |
| `pnpm bench:app typing --file F`                        | Typing benchmark, also typing into file `F` (e.g. a `.rs` file) |
| `pnpm bench:app typing --extensions`                    | Typing benchmark with your extensions loaded                    |
| `pnpm bench:check`                                      | Compare the last end-to-end run with `benches/baseline.json`    |
| `node scripts/perf-check.mjs micro`                     | Check Criterion's comparison with a saved baseline              |
| `cd extensions && cargo build --release`                | Build the example and test extensions to WebAssembly            |
| `cargo test --workspace`                                | Run Rust tests                                                  |
| `cargo clippy --workspace --all-targets -- -D warnings` | Lint Rust sources                                               |

## Logging

The backend logs through [`tracing`](https://docs.rs/tracing) to stderr and to a daily rolling
file (seven files retained) in the platform log directory:

| Platform | Log directory                             |
| -------- | ----------------------------------------- |
| Windows  | `%LOCALAPPDATA%\dev.flareite.editor\logs` |
| macOS    | `~/Library/Logs/dev.flareite.editor`      |
| Linux    | `~/.local/share/dev.flareite.editor/logs` |

Set `FLAREITE_LOG` to change the filter, using
[`EnvFilter` syntax](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html).
The default is `info`. Frontend messages are logged under the `frontend` target:

```sh
FLAREITE_LOG=debug,frontend=trace pnpm tauri dev
```

**View: Output** (`Ctrl+Shift+U`) shows recent messages without opening the file: Flareite's,
each extension's, and each language server's, in separate logs. Those keep every level from
extensions and servers, including their debug messages and a server's stderr, whatever the
filter.

Startup timing is logged as `backend initialized` and `frontend ready`, each with `elapsed_ms`
measured from the start of `flareite_app::run`.

## Repository layout

```text
Cargo.toml            Cargo workspace
package.json          Frontend package and scripts
benches/              Criterion benchmarks and fixtures
scripts/              End-to-end benchmark harness and performance regression checks
.github/workflows/    CI: formatting, lints, and tests on Windows, macOS, and Linux
crates/
  flareite-extension-api/   Extension API: WIT interface and Rust bindings for extensions
  flareite-extension-host/  Extension loading, permissions, sandbox, limits, host functions
  flareite-tasks/      Background scheduler and timing metrics
  flareite-terminal/   Shells in pseudoterminals, output streaming, flow control
  flareite-syntax/     Tree-sitter grammars, incremental parsing, highlighting
  flareite-lsp/        Language server client: processes, JSON-RPC, positions, document sync
  flareite-process/    Starting programs, and stopping them with everything they started
  flareite-diff/       Line and character diffs (change markers; Git's diff tabs)
  flareite-text/       Rope-backed text buffer, change sets, positions, Unicode boundaries
  flareite-workspace/  Files, explorer listings, Go to File index, find in files, watching
  flareite-core/       Documents, selections, editing operations, undo, dirty state
src-tauri/            flareite-app crate: Tauri shell, lifecycle, IPC commands, dialogs, logging
src/                  React + TypeScript workbench
  app/                Root component, workbench layout, view registry
  commands/           Command registry, keybindings, fuzzy matching, built-in commands
  components/         Workbench chrome (activity bar, palette, notifications, ...)
  dock/               The layout: groups of tabs in splits, drag and drop, saving
  editor/             Editor view, find widget, hover and completion, tabs, keymap, session
  panels/             Views (explorer, search, extensions)
  terminal/           Terminal panel, tabs, and xterm.js sessions
  state/              Small observable stores (documents, workspace, search, layout, ...)
  ipc/                Typed wrappers for Tauri commands (the only place that calls invoke)
  themes/             Theme selection
  styles/             Design tokens and base styles
extensions/           Extensions built to WebAssembly: examples (tidy, todos, markdown, themes, icons) and test fixtures
docs/                 Architecture notes, ADRs, extension guide, troubleshooting
```

See [`docs/architecture.md`](docs/architecture.md) for how these pieces fit together and how the
layout will grow.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
