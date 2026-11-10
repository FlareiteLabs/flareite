# ADR 0046: Tabs in windows of their own

- **Status:** Accepted
- **Date:** 2026-11-10

## Context

Flareite had one window. The user asked to drag tabs out of it, each tab dropped outside the
window opening a new window that holds only that tab, as in VS Code. Decisions the user made:

- **Any tab** can leave the main window: documents, terminals, views, pages, previews, viewers,
  and layouts.
- A new window has **its own dock and a slim status bar**, with no activity bar or side bar.
- **Closing a window closes its tabs**, as in VS Code: unsaved changes are asked about, and
  terminals end. **View: Move Tabs into Main Window** puts them back instead.
- Windows **come back after a restart**, where they were, with their tabs.

Everything the workbench shows lives in the main window's page: the stores (documents, the dock,
terminals, extensions' state), editor sessions, xterm.js emulators, and the one backend event
channel. A second full workbench per window would split all of that, and each window would load
and restore the app again.

## Decisions

### The main window's page draws every window

As VS Code's floating windows do, another window is a page the main window scripts. The workbench
calls `window.open('/window.html#<id>')`. Tauri's `on_new_window` handler on the main webview
(`src-tauri/src/windows.rs`, Tauri 2.12's `NewWindowResponse::Create`) makes it a native window,
labelled `dock-<id>`, whose webview is the target of `window.open`. The main page gets its
`Window` and draws into it. Nothing runs in `public/window.html` itself: it's an empty
same-origin page in standards mode (`about:blank` would be in quirks mode).

- The handler refuses everything else: other pages, other origins, ids it doesn't hand out, and
  windows opened from extensions' webviews (their origin differs).
- `src/app/windows.ts` keeps the open windows. It copies the main page's style sheets into each
  window and keeps them in step (Vite replaces them in development). It also mirrors the `<html>`
  element's attributes (theme, colors, fonts). React renders each window's content through a
  portal, so the stores and contexts are shared.
- Code that touches the page uses the element's own document and window: `ownerWindow`,
  `afterFrame` (frames of the window being drawn, which go on while another is minimized),
  `resizeObserverFor`, `onEveryWindow` (keybindings, modifier keys, menus closing on a press),
  `activeElement` (the window the user is in), and `query` (any window). Another window's nodes
  and events come from its realm, so `instanceof Element` fails for them: `isNode`, `isElement`,
  and `isHTMLElement` test them in any window.

### One layout, a tree per window

The dock's layout (`dock/model.ts`) gains `windows`: each has an id (`w1`, `w2`, ...) and a tree
of groups like the main window's. Finding, moving, splitting, and closing work the same way in
every window, and a tab moves between windows as it moves between groups. A window whose last tab
leaves closes. New drop targets: `{ kind: 'window' }` (a new window) and `{ kind: 'root', window }`
(along an edge of a window's dock). The side bar and the bottom panel stay the main window's.

`dock/windows.ts` follows the layout. It opens a native window for each window the layout gains
and closes those it loses. If one can't open, its tabs go back to the main window. The layout is
saved as before. Where each window is, in physical pixels, is saved beside it (`layout.windows`):
the backend reports a window's bounds once it stops moving, and whether it's maximized. Saved
bounds that are off every monitor are ignored.

### Dragging between windows

A drag stays a pointer drag ([ADR 0022](0022-file-drops-and-pointer-drags.md)). The pointer is
captured, so its events keep coming to the window it was pressed in, even outside it. When a drag
starts, the backend reports where each window's page is on screen (`windows_geometry`). From the
client position and `devicePixelRatio` (which includes the zoom), the pointer's place on screen
tells which window it's over. The window the drag started in counts as in front. Elsewhere the
most recently focused window wins, since the system's stacking order isn't available. Drop
targets are hit-tested in that window's document, and the drag ghost shows there.

Dropped outside every window, a tab goes to a new window at that spot, sized like the group it
left. Dragging a window's only tab out moves its window instead. With the copy key held, a
document shows in the new window too, in a view of its own.

### Where things go

- **New documents** open in the group last used for documents in the window the user is in (the
  main window's if that window has none). Focusing a window makes its last current group current
  again, so commands such as Close Tab act there.
- **The palette, context menus, and dialogs** show in the window the user is in. Notifications
  stay in the main window.
- **A window's status bar** shows the editor's line and column, language, line ending, and
  encoding while the user works in that window.
- **Terminals** move with their tab: xterm.js follows its element into another window when opened
  again there. **Web page panes** are reparented to the native window their tab is in. **Native
  dialogs** belong to the focused window.
- A window is titled after the tab it shows (`on_document_title_changed`), and zooms with the main
  window (`windows_set_zoom`).

### Closing and reloading

- Closing a window asks the workbench (`windowCloseRequested`). It closes the window's tabs, last
  first, stopping if one is cancelled; once the window has none, it closes.
- Closing the main window closes the others without asking about them separately: unsaved
  changes in every window were asked about with the main window's. The layout keeps them for the
  next start.
- When the main page loads (or reloads), it closes the windows it left open (`windows_reset`) and
  opens the saved ones again.

### Commands

**View: Move Tab into New Window**, **View: Copy Tab into New Window** (documents), and **View:
Move Tabs into Main Window**. The tab menu has **Move into New Window**.

## Consequences

- One page does all the work, so a window costs a webview and its DOM, not another copy of the
  app. A long task in the main page also stalls the other windows.
- Another app's window over a Flareite window isn't seen: a drag over it counts as over the
  Flareite window beneath.
- Timers (`setTimeout`) are the main page's. wry doesn't hide a minimized window's webview, so
  they aren't throttled when the main window is minimized. Frames and resize observers are each
  window's own.
- Outside the desktop shell (tests, `pnpm dev`), no window opens: a tab dropped outside the window
  stays where it was.
