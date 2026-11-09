# ADR 0039: Links and web pages

- **Status:** Accepted
- **Date:** 2026-11-09

## Context

The terminal showed web addresses and file paths as plain text. The user asked for them to open
with Ctrl+click: web addresses in the browser, file paths in the file explorer. They also asked
for that to be extensible, with an example in mind: a web extension that opens web links in a tab
that loads the page in the editor, instead of in the browser. No extension could show a page from
the web: webview pages are sandboxed to the extension's own files, and many sites refuse to be
framed anyway.

The user decided:

- **File paths** open in the editor at the line and column they name (`main.rs:12:5`), as in VS
  Code; folders open in the system's file manager.
- **Extensions declare link handlers.** An installed handler takes over its links unless a
  setting names another (or Flareite's way); Ctrl+Shift+click asks who opens a link, once.
- **The same handlers open the other links** the app opens: web addresses Ctrl+clicked in the
  editor's text, and links in previews and READMEs.
- **A native web view pane** is built now, behind a permission the user grants, so any site works
  in a tab.

## Decisions

### Terminal links

An xterm.js link provider (`src/terminal/links.ts`) finds web addresses and paths in each line of
output. Paths can end with `:line`, `:line:column`, or `(line,column)` (compilers' styles), and
`file:` addresses are paths. Only paths that lead to a file or folder are links: the provider asks
the backend (`path_kinds`, cached for ten seconds) about each candidate, made absolute from the
shell's folder as it reports it (OSC 7), or else the nearest PowerShell prompt above the line, or
else the open folder. Hovering shows a tooltip; Ctrl+click opens; Ctrl+Shift+click asks.
xterm.js takes Shift+mousedown for extending a selection, so the provider catches Ctrl+Shift
mousedowns on a hovered link first, in the capture phase.

### One way to open links

`src/state/links.ts` opens every link. Who opens it comes from usable extensions'
`[[link_handlers]]` (web links by scheme, `https`, `http`, or `mailto`; file links by name
pattern) and the `workbench.open_web_links` and `open_file_links` settings: `auto` (the first
handler that takes the link, else Flareite's way), `default` (Flareite's way), or a handler's id.
An extension's handler runs its command with the link as its payload (`links::opening`); it can
hand the link back (`open-default`). Flareite's way is the system's browser for web addresses
(`open_external`), the editor for files, and the file manager for folders
(`open_folder_external`).

Previews (and READMEs, which use them) and Ctrl+click on a web address in the editor's text go
through it too, so a web extension catches every web link the user follows. In the editor, a web
address under the pointer is underlined as soon as Ctrl is held, without asking a language server,
and wins over Go to Definition.

`links::open-in-browser(url)` lets an extension open a web address in the browser itself (for an
"Open in Browser" button), at most once a second so none can flood it.

### Web page panes

Layouts gain a `web-page(url)` pane: a child webview of the main window (Tauri's multiwebview,
behind its `unstable` feature), which the workbench lays over the pane. Alternatives: an iframe
can't show the many sites that refuse to be framed, and a separate window wouldn't be a tab.

- **Isolation.** The view navigates only to `https:` and `http:` addresses (`on_navigation`), so
  it can't load Flareite's pages or files. Flareite's commands are granted only to its own local
  pages (the default capability), so a remote page's IPC calls are refused; verified in the real
  app (`path_kinds not allowed`). A page that opens a window goes there in the same view.
- **The `web_pages` permission**, granted by the user, is needed to show one: a site in a tab
  can collect what the user types there, which a page of the extension's own couldn't.
- **Placement.** The workbench sends the pane's bounds, in logical pixels (CSS pixels times the
  workbench's zoom), checking each frame because moving a pane (a side bar opening) doesn't
  resize it. Native views sit above the workbench's page, so a view hides while an overlay is
  open: the palette, a context menu, or a dialog (each counts itself with `useOverlay`), and while
  its tab is hidden. It closes with its tab, or when its layout drops the pane.
- **Navigation.** Page loads and title changes go to the layout's `on_navigate` command
  (`layouts::navigated`: address, title, loading). The extension moves the view with
  `navigate-pane`: go, back, forward, reload, stop (back, forward, and stop run script in the
  page, which Tauri's API has no calls for).

### The Browser extension

Browser (`flareite.browser`) takes `https` and `http` links and opens each in a tab with a web
page pane, with Back, Forward, Reload, Go to an Address, and Open in Your Browser in the pane's
header, and the page's title as the tab's. It began as an example in `extensions/` and became an
official extension with a repository of its own, as the others have
([ADR 0034](0034-official-extensions-in-their-own-repositories.md)).

### API 1.15, still

API 1.15 (layouts) hadn't been released, so the `links` interface, the `web-page` pane content,
`navigated`, `navigate-pane`, and `[[link_handlers]]` join it rather than starting 1.16. Adding a
case to `pane-content` changes a 1.15 type, which components built against an earlier 1.15
checkout must be rebuilt for (Git and HTML & CSS were).

## Consequences

- Links in the terminal, the editor, and previews open with Ctrl+click, and extensions can open
  them their own way; the user chooses with a setting, or per link.
- Extensions can show any web page in a tab, with the user's permission.
- A web page pane hides whenever an overlay opens, even one that wouldn't cover it, and while it's
  hidden the page doesn't draw (its scripts keep running).
- Only links within one terminal row are found: a path wrapped across rows isn't.
- The Tauri `unstable` feature is enabled for the whole app.
