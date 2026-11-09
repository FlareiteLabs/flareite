# ADR 0038: Layouts

- **Status:** Accepted
- **Date:** 2026-11-09

## Context

The diff and merge editors were built into the workbench ([ADR 0028](0028-diffs-and-provided-documents.md),
[ADR 0032](0032-merge-conflicts.md)): `diff:` and `merge:` tabs, `DiffView` and `MergeView`, and
`open-diff` and `open-merge` for extensions to open them. That kept Git's knowledge in the Git
extension but made the views themselves the workbench's, and nothing else could put two things
in one tab. The user wanted an HTML extension that renders a page beside its editor in the same
tab, which no extension could build, and asked for extensions to define tab layouts instead, so
the diff and merge editors could live entirely in the Git extension, with buttons above editors
(such as Complete Merge) as part of the same API.

The user decided:

- **The diff engine lives in the extension.** Git compiles `flareite-diff` (and the three-way
  merge, moved out of it) into its WebAssembly. The core keeps `flareite-diff` only for change
  markers beside the text (quick diff).
- **Layouts for files** are declared with patterns and a priority, as viewers are: a `default`
  layout opens matching files, an `option` one is chosen; either way the user can switch a
  file's tab between the text editor and the layout, both ways.
- **A file shown as a page** is served from the open folder, unsaved changes included, with the
  page's own scripts running in the webview sandbox; only in a trusted folder; the network only
  with the extension's `network` permission.
- **HTML & CSS gets the preview**, becoming an extension with code.
- Work autonomously after the questions (no stops between parts).

## Decisions

### A layout is a tree of splits and panes (API 1.15)

A new `layouts` interface. A manifest's `[[layouts]]` declares each with the command that fills a
tab in. A tab is named by its layout and a key the extension chooses (a file's path for a layout
with patterns); `open` opens one, `set-layout` says what it shows:

- **Nodes**, each a split (`row` or `column`) or a pane, listed parents first, since WIT has no
  recursive types. Siblings share their split by `size`; the user can drag the line between
  them, which lasts while the window does.
- **Panes**: an **editor** of a file or a provided address (`minimap`, `folding`, and `quick-diff`
  as the extension chooses), a **webview** (a page of the extension's, or HTML it sets, as viewers'
  and previews' pages are), or a **file page** (below). Each can have a **header**: a title, a
  description, and buttons.
- **Buttons**, beside the tab or in a header: a named icon or text, a command and argument,
  `primary`, a `pressed` state for toggles, and `disabled`.
- **Scroll groups**: editor panes in the same group scroll together.
- **Breakpoints**: widths at which the command runs again, so a diff goes inline when narrow.
- **Main pane**: focused first, and the tab's file (the active file, the explorer mark on the tab).
- **Other documents** to keep open, such as a diff's original when only the modified side shows.

The workbench owns the tabs, as it owns viewers': they're saved with the window's layout and
restored, stay when their extension stops, and close only when the user (or the extension)
closes them. The host keeps each tab's layout and decorations, so a tab shown again draws at once.

### One command fills a tab, told why

The layout's command runs with `filling()` saying which tab and why: `opened` (set its layout),
`restored` (it came back with the window, or the extension started again), `resized` (it crossed
a breakpoint, with its width), `ready` (the workbench has opened its documents: `documents(tab)`
gives their ids), or `closed`. One command keeps the manifest small and, as with earlier
callbacks, adds no export that would break older components. `ready` runs only when the set of
documents changed, so a command that answers `ready` with `set-layout` doesn't loop.

### Editor panes show what the extension draws

`set-pane-decorations` replaces what an editor pane shows besides its text, in that pane only:

- **Ranges** decorated with the extension's decoration types (whole-line ones color lines).
- **Zones**, rows between lines: `filler` (hatched, to line rows up across panes), `text` (a bar,
  clickable), `links` (a label and links, drawn as code lenses; none leaves an empty row), and
  `lines` of another of the tab's documents, highlighted, with their own decorations (an inline
  diff's removed lines).
- **Folds**: lines hidden as folded regions, the extension's rather than the user's, which aren't
  remembered with the file's (views numbered from 2^30, as diff views were).
- **Ruler marks** beside the scroll bar.

These aren't moved by edits, unlike document decorations: they're the extension's picture of the
text when it set them, and it sets them again as the text changes (it hears every change with
`documents_read`). Diffs and merges are recomputed per change anyway; mapping zones through edits
would only show stale ones in moved places. Columns are UTF-8 bytes as everywhere in the API; the
host converts them for the workbench using the tab's documents.

Decoration types can now name a **theme color token** (`diff-inserted-line`) instead of a hex
color, so an extension's diff follows the theme as the built-in one did.

`reveal`, `active-pane` (the pane in use and its selections, for commands like Stage Selected
Ranges and F7), and `invoked` (the tab and pane a button or zone was clicked in) complete it.
Keybindings can be limited to layouts (`layouts = ["diff", "merge"]`), so F7 is Git's in its tabs
without taking the key elsewhere.

### File pages

A `file-page` pane shows a file of the open folder as a web page. It's a webview with a token of
its own, served by the host from the open folder: each file as the editor has it if it's open
(unsaved changes included), else from disk; confined to the folder after following symlinks; under
the same content security policy as webview pages (its own address, `data:`, `blob:`, inline
scripts and styles; `https:`/`wss:` only with the extension's `network` permission). Only in a
trusted folder: otherwise the pane says why, with Trust the Folder.

The page is the user's, so it gets no `flareite` object, only a small script of Flareite's
(`file-page.js`) that passes on the workbench's shortcuts and reports where it's scrolled. The
workbench acts only on `ready`, `keydown`, and `scroll` from it.

For extension development, a page that links the kit (`__flareite/kit.css`) gets the kit, the
theme variables, and the webview defaults, as webview pages do, and its script applies theme
changes the workbench broadcasts. Pages that don't link it get none, so an ordinary site looks as
it would in a browser. Linking the kit is the signal because only a page meant for a webview
would; the theme variables alone would be harmless, but the defaults restyle the page.

Such a page also gets a stand-in `flareite` object (`kit-page.js`: the file page script with the
stand-in turned on), with the real one's API, so a webview page's scripts run in a preview. The
theme works as in a webview. Storage is kept by the workbench per pane while the window is open,
so it survives the page showing again after edits; it's never the extension's storage, which a
user's page mustn't reach, and for the same reason a file page isn't registered as a webview (it
hears theme changes only). State lasts while the page is loaded: `getState` is synchronous, and a
page that shows again after every edit couldn't get it back in time. What would reach the
extension is logged, and `flareite.standIn` lets the developer play the extension's part.

Layout webview panes' ids carry their extension (`<extension id>.pane/<n>`), as previews' do, so
their pages hear `onStorageChanged`.

The page shows again 300 ms after a file in its folder is edited (in any tab), or a file there
changes on disk, scrolled back to where it was.

### The diff and merge editors are Git's

The workbench's `diff:` and `merge:` tabs, `DiffView`, `MergeView`, their layout code, the
`diff_compute`, `merge_*`, and `diff_apply`/`diff_revert` commands, and the diff settings saved
with the layout are gone. `open-diff` and `open-merge` stay in the WIT, so older components still
load, but fail. Git 0.6 has a `diff` and a `merge` layout:

- **Diff**: the sides' editors side by side in one scroll group, or the modified side alone with
  the removed lines in zones, inline by choice or under 900 pixels; changed lines and characters,
  filler, unchanged lines folded behind a bar that shows them, ruler marks. Its buttons stage and
  revert selected ranges, move between changes, and toggle inline, whitespace, and collapsing,
  which are now Git's settings.
- **Merge**: incoming and current above the result, each with a header; links above each range in
  every editor; filler so every block takes its tallest side's height; the result's header shows
  how many conflicts remain, with previous/next and a primary **Complete Merge**, which asks while
  conflicts remain, saves the result (`documents::save`, also new), stages it, and closes the tab.
  Merge tabs are now restored with the window; a result with conflict markers starts again from
  the base.

### HTML & CSS's preview

HTML & CSS 0.2 has code: two layouts for `*.html`, `*.htm`, and `*.xhtml` files, both options
with a button on their tabs. `preview` is the file's editor and its file page side by side;
`page` is the file page alone, in a tab of its own, as a viewer would be. (A `[[viewers]]` entry
couldn't do it: a viewer's page is the extension's, which can load only the viewed file, so the
HTML's relative style sheets, scripts, and pictures wouldn't load, nor would unsaved changes
show.)

So that the page tab can be edited beside, an `option` layout's tab doesn't stand for its file
the way a viewer's tab does: opening the file opens its text. A `default` layout's tab does. And
file pages show again when files in their folder are edited in any tab (the editors report the
versions they see), not only in the layout's own editors.

## Consequences

- Extensions can put editors, pages, and files' pages in one tab, with buttons, and draw in
  editors much as the built-in diff did. Previews beside their source (Markdown, SVG) and
  formatter previews can follow.
- The workbench has no diff or merge view of its own; Git's work only with Git installed.
  Comparing two arbitrary files needs an extension.
- Old saved layouts lose their `diff:` tabs (they no longer resolve), and the diff settings saved
  in the layout reset to the defaults (now Git's settings).
- Pane decorations aren't mapped through edits: until the extension recomputes (one notification
  later), zones can sit a line off while typing.
- Line numbers of changed lines are no longer colored in diffs: whole-line decorations color the
  text area only.
- Root-relative addresses (`/style.css`) in a file page don't resolve: pages are served under
  their token.
