# ADR 0055: Keeping up with fast scrolling

- **Status:** Accepted
- **Date:** 2026-11-20

## Context

Scrolling fast left the editor blank, and the minimap partly blank, until scrolling slowed down,
with text flickering in and out in between. Dragging the minimap's slider from the top of a
4,000-line file to the bottom made all the text disappear and come back. VS Code scrolls through
thousands of lines without either.

VS Code holds the whole document in the renderer, so every frame can draw the lines at its new
position. Flareite's text lives in the backend (ADR 0002), and the editor rendered only its view:
the visible lines plus 20 on each side, fetched when the visible lines weren't held. A scroll of
more than 20 lines a frame outran that: the frame was blank, and with one request at a time, each
view arrived for a position the scroll had already passed. A wheel makes this worse, as the
WebView scrolls on its compositor thread, a frame or so before the page hears of it.

The minimap had the same problem. It fetched its window (the rows shown plus 100 on each side,
about 600 rows, 12 ms of highlighting in a Rust file) again whenever it scrolled past it, drew only
the rows of the last reply, and drew a frame after the editor, through a React render.

Two end-to-end benchmarks measure this (`pnpm bench:app drag` and `fling`, see `performance.md`).

### A first attempt: rows ahead in views

The first fix made views reach further in the direction of scrolling, up to 600 rows. Wheel
flings in the benchmark improved, but dragging the minimap didn't: a drag moves about 200 lines a
frame, and the views then brought in hundreds of highlighted lines to render in React in the
middle of the drag, taking frames to 48 ms (p95). Fetching ahead through views can't keep up with
a jump of hundreds of lines, and rendering what it fetches made scrolling lag.

## Decisions

### The editor keeps a copy of the lines

Besides its view, the editor keeps a copy of the document's lines with their highlights
(`src/editor/lineStore.ts`):

- **What.** The whole document when it has at most 20,000 lines, otherwise 5,000 lines on each
  side of the view.
- **Fetching.** A new command, `editor_lines`, returns lines with their highlights. Like
  `editor_minimap`, it takes a snapshot under the editor lock (the rope and the syntax tree, both
  cheap copies) and highlights on a worker thread, so editing never waits for it. The copy fetches
  1,000 lines at a time, nearest the view first, one request at a time; after an edit's view, only
  once typing has paused for 300 ms.
- **Edits.** Each view now also carries its line splices, and the copy replays them as the view's
  own line cache does (ADR 0004), so typing doesn't fetch it again. The lines of each view replace
  their copies. Lines fetched for another text version are dropped.
- **Stale highlights.** Highlights of lines outside the view can be out of date after an edit
  that changes them (opening a block comment, say). They're drawn as they were until those lines
  are in a view again, which draws them correctly.

### Rows the view lacks are drawn from the copy, in the same frame

On every scroll event, and after every view, the editor draws the rows near the visible ones that
its view doesn't cover from the copy (`src/editor/fill.ts`):

- **Plain HTML, not React.** Rows are built as one string and inserted at once, positioned
  absolutely by row, with the text, its highlights, and the line number. Selections, diagnostics,
  indent guides and the like appear once the view covers those rows, usually a frame or two later.
- **Only new rows.** Rows already drawn stay; a frame adds the rows it brings in and removes those
  out of range, covered by the view, or whose line changed. The layer is clipped and contained
  (`contain: strict`), so it lays out nothing else and can't change the horizontal scroll range.
- **Where the next frames land.** Besides a screen on each side, while scrolling it draws a screen
  where each of the next three frames will be, from the distance the last scroll event moved
  (scroll events come once a frame), with a quarter of that as slack. That covers a wheel's
  compositor lead. A frame that moves more than a screen never shows the rows in between, so they
  aren't drawn. A move of more than 300 rows in one event is a jump (Go to Line, the scrollbar
  dragged through a large file) and draws nothing ahead.
- **Not with folds or word wrap.** Rows are then not lines, and the view alone draws, as before.

Views go back to the visible lines plus 20 on each side, so React renders few lines per view.

Alternatives considered:

- **Rows ahead in views** (above): fetching can't keep up with jumps, and rendering lags.
- **Rendering the copy's rows through React:** each highlighted line costs about 40 µs to render
  as React elements, which is what made the first attempt lag.
- **Holding the whole document's text in the frontend as the source of truth,** as VS Code does.
  That reverses ADR 0002. The copy is only drawn; editing, the cursor, and everything else keep
  going through views.

### Highlight only the lines a view brings in

The backend counts changes to each document's syntax tree (a generation). Each view's highlight
mirror records the generation and text version its spans were found with. When a view's client
holds lines at the same version and generation, their remembered spans are still current and
those lines aren't highlighted again, so a scroll highlights only the lines it brings in.

### The horizontal scroll range keeps to the widest line seen

The content was as wide as the widest line rendered, so the horizontal scroll range grew and
shrank as lines scrolled in and out. Now the editor remembers the eight widest lines it has
rendered, their line numbers following edits through each view's splices, and keeps the content
at least as wide as the widest (`src/editor/widestLines.ts`):

- **Grows** when a wider line is rendered, and stays as wide after scrolling past it.
- **Shrinks** only when the widest line gets narrower or is removed (even out of view), to the
  next widest remembered.
- **Starts over** when the font, the tab size, or word wrap changes, and doesn't apply while lines
  wrap to the editor's width.
- **Cheap.** Only lines whose length could reach the narrowest remembered are measured, each line
  once, with a single range kept off the page between measurements. A range per line made every
  following render slower: the page keeps each range up to date as the DOM changes until it's
  collected (page jumps went from 27 to 50–60 ms at p95).

`pnpm bench:app width` checks it in the release build: the range grows from 886 to 3,519 px at a
400-character line, stays after scrolling 1,400 lines past it, and returns to 886 px once that line
is shortened out of view.

### Lines and gutter rows render only when they change

Indent guide styles are shared by lines with the same guides, and gutter rows are a memoized
component with plain props. Before, every view rendered every line again.

### The minimap keeps its rows and draws as the editor scrolls

- **Rows by state.** The minimap keeps the rows it has fetched for one state of the document (its
  version and refresh) and fetches only those it lacks (`src/editor/minimapRows.ts`): the rows
  shown first, then 100 on each side and more ahead while scrolling (`src/editor/scrollAhead.ts`).
- **Small documents whole.** A document of at most 20,000 rows is then fetched whole, 1,000 rows
  at a time, so dragging through it never waits. After an edit, the rest waits until typing has
  paused for 500 ms.
- **Old rows until new ones arrive.** After an edit, the rows of the document as it was are drawn
  until their replacements arrive, so the minimap is never blank.
- **Drawn from scroll events.** The canvas and slider are drawn from the editor's scroll events,
  not through a React render, so the minimap moves in the same frame as the editor. The theme's
  colors are read once per theme instead of on every draw, which forced a style pass each frame.

## Consequences

- **Memory.** The frontend holds a second copy of up to 20,000 lines (or 10,000 around the view)
  with their highlights, a few megabytes for a large source file.
- **Background work.** Opening a document fetches its copy in the background: for 20,000 lines of
  Rust, about 20 requests of 1,000 lines highlighted on a worker thread.
- **Briefly incomplete rows.** For a frame or two during fast scrolling, rows show no selection,
  diagnostics or indent guides, and their colors may be stale after an edit.
- **Folds and word wrap** keep the old behavior during fast scrolling until rows can be mapped to
  lines in the copy.
- **Benchmarks exit without asking** about unsaved changes (`EditorState::discard_on_exit`): the
  typing benchmark used to leave a prompt open that someone had to answer.
- **Measured** in `performance.md`, "Fast scrolling".
