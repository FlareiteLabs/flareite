# ADR 0012: Docking

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

The workbench had fixed places: a side bar for views, an editor area for documents, and a panel
below it for terminals. Users want to arrange everything themselves, and not only within those
places as VS Code allows: any document, view, or terminal anywhere, beside or inside any other.

## Decisions

### One tree of tab groups, for everything

The layout is a tree (`dock/model.ts`). Each split is a row or column with a share for each
child, and each leaf is a group of tabs. Every item is a tab in exactly one group, and items of
any kind can share a group:

- **`document:<id>`** for open documents.
- **`view:<id>`** for the explorer, search, extensions, and extension views.
- **`terminal:<id>`** for terminals.

The side bar, editor area, and terminal panel are gone; they are groups. Split terminals and
side-by-side editors come with it.

The model's functions are pure (`placeItem`, `removeItem`, `normalize`, `parseDock`), so the
tree's rules are unit tested apart from React:

- **Placing.** An item lands among a group's tabs, beside a group (splitting its space in
  half), or along an edge of the whole dock (taking a quarter).
- **Tidying.** Groups left empty are removed, splits of one child are replaced by the child, and
  a split inside a split going the same way is merged into it.

Alternatives considered:

- **A docking library** (dockview, FlexLayout, rc-dock). They bring their own tab strips and
  styling, which would duplicate the editor tabs' behavior (unsaved marks, middle-click,
  keyboard navigation) and theme tokens. None documents keyboard docking or screen reader
  support at the level `CONTRIBUTING.md` asks for.
- **VS Code's model** (fixed side bars, panel, and editor groups). It doesn't allow what was
  asked for: a terminal beside an editor, or a view in the middle.

### The main group

One group is the main group. New documents open there when no other group has been used for
documents. It stays when empty, showing the welcome page, so there's always a place for them,
and it can't be hidden. Otherwise it's an ordinary group: anything can be docked in it or moved
out of it.

### Where new items go

- **Documents** go to the group most recently used for a document, or the main group.
- **Terminals** join the current group of terminals, else any group of terminals, else a new
  group along the bottom.
- **Views** reopen where they were closed if that group still exists. Otherwise they join the
  side bar's views, or a new group on the left.

### The side bar

The side bar is the groups along the dock's left edge, beside the activity bar
(`sidebarGroups`): those that nothing comes before in any row they're in, except the main group.
It's worked out from the tree, so nothing more is saved, and stacking groups in the left column
keeps them all in it. Groups docked along the top or bottom of the whole dock span it rather than
sit beside the activity bar, so they aren't in it.

A hidden group keeps its place in the side bar, so the activity bar can show it again. Hidden
groups don't count as coming before others, so what's beside one joins the side bar too: that's
what now lies along the edge.

- **The activity bar** has a button for each item in the side bar (views, terminals, documents),
  in tab order, then for each view not docked anywhere. An item docked elsewhere has no button:
  it's reached from its tab or its command.
- **Opening from the activity bar.** A view that isn't docked opens in the side bar: where it was
  closed, if that group is in the side bar, else with the side bar's views.
- **Dropping on the activity bar** moves any item into the side bar: among items of its kind
  there, else in a new group below the side bar's others. A new group along the left edge would
  push them out of the side bar. (Superseded: [ADR 0056](0056-one-side-bar-ordered-from-the-activity-bar.md) joins the
  side bar's group whatever the kind, where it's dropped among the buttons.)

### Hiding, not just closing

Groups can be hidden. A hidden group keeps its items and its place, and showing it puts it back
as it was:

- **Activity bar.** Selecting an item shows it. Selecting the item already showing hides its
  group, as collapsing the side bar did.
- **View: Toggle Side Bar** (`Ctrl+B`) hides or shows every group in the side bar, whatever it
  holds. Groups docked elsewhere stay as they are.
- **View: Toggle Terminals** (`Ctrl+J`) does the same for groups of terminals.
- **View: Toggle Terminal** (`` Ctrl+` ``) focuses a terminal, starting one if needed. From a
  terminal, it hides the terminal groups.

Closing a view's tab removes it. Closing a terminal's tab ends the shell. Closing a document's tab
closes the document.

### The stores own what's open; the dock owns where

The documents and terminals stores stay the source of what's open. The dock follows them: a new
document or terminal gets a tab, and a closed one loses it.

- **The current group.** The dock tracks the group focus is in. Commands such as Close Tab act on
  it.
- **The active document** is the active tab of the group last used for documents. The explorer,
  breadcrumbs, and status bar follow it.
- **Several editors at once.** Each editor view registers by document (`editorFor`), so events
  refresh the view showing a document even when it isn't the active one.

### Saved by path

The tree is saved with the rest of the layout in `state.json`:

- **Documents** are saved as `file:<path>`, since document ids change between runs. They hold
  their place until the documents reopen, and are dropped if they don't. Untitled documents
  aren't saved.
- **Terminals.** If the saved layout showed terminals and none is running (a new start of the
  app), one starts where they were.
- **Older layouts.** The side bar's saved visibility and active view seed the default layout.

`parseDock` validates the saved tree: kinds, unique ids, depth (32), node count (500), and known
item kinds. A malformed tree gives the default layout.

### Drag and drop, and the keyboard

- **Dragging.** Tabs, and activity bar buttons, can be dragged. A tab dropped on the activity bar
  joins the side bar. (They first used HTML5 drag events, with Tauri's own drop handler off;
  they're now pointer drags, so files from other programs can be dropped too: [ADR
  0022](0022-file-drops-and-pointer-drags.md).) A preview shows where the item will land:
  - over a tab strip: among those tabs;
  - over the middle of a group: into it;
  - over a quarter of a group near an edge: beside it, on that side;
  - over the outer 24 px of a group that lies along the window's edge: along that whole edge.
    Tab strips always mean joining their tabs, so the top edge is reached just below them.
- **The keyboard.** Arrow keys move between a group's tabs, and Delete closes one. The tab menu
  (right-click, `Shift+F10`, or the menu key) splits or moves the tab. F6 and Shift+F6 move
  between groups. There are commands for all of it: Move Tab Left/Right/Up/Down (into the
  neighbouring group on screen, or a new one), Split Tab Right/Down, Close Tab, and Reset Layout.
  Close Tab took `Ctrl+W` and Reopen Closed Tab was added later ([ADR 0021](0021-tab-keybindings.md)).
- **Close buttons.** The × on a tab is for the pointer only (`aria-hidden`). A control inside a
  `tab` can't be reached by screen readers, and axe reports it.

## Consequences

- **One document, one place.** A document shows in one group at a time. Showing the same
  document in two groups would need two views of it, and the backend keeps per-view state (the
  highlight mirror) per document. (Superseded: [ADR 0024](0024-a-document-in-several-groups.md)
  gives each tab of a document a view of its own.)
- **No floating or separate windows** yet. The tree has room for them as additional roots.
- **Sizes are shares.** Groups grow and shrink with the window in proportion, rather than keeping
  their width in pixels as VS Code's side bar does.
- **Extension views** whose extension isn't loaded keep their place but aren't shown. They come
  back when the extension loads.
- **Reset Layout** puts views on the left, documents in the main group, and terminals along the
  bottom, keeping everything open.
- **Bug fixed along the way.** An editor whose requests kept failing retried at once, forever.
  It now reports the failure once and retries after a delay (250 ms, doubling to 5 s). Edits
  still go out at once. Two editors in the tests' jsdom, with no backend, made this visible.
