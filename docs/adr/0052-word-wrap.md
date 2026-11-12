# ADR 0052: Word wrap

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

Flareite has no soft wrap: a long line runs off to the right and is reached by scrolling
sideways. That is fine for code and poor for prose, Markdown, and long single-line files. A
review on 2026-11-11 ranked it next after the extension API overhaul.

The backend holds the text; the frontend holds only the lines of its viewport (ADR 0002), and
since folding (ADR 0015) a viewport is a range of **display rows**. The backend knows how many
rows there are and which line is on each; the frontend places lines by row. Wrapping makes one
line take several rows, so whoever decides where lines break must also be who counts rows. The
frontend can't count rows of lines it doesn't hold.

## Decisions

### The core wraps, by columns

`flareite-core` computes where each line breaks, from a width in **columns** that the frontend
sends, as VS Code's default `simple` wrapping strategy does. The editor's font is monospace, so a
column is the width of one character (`1ch`): a grapheme cluster takes its Unicode width (CJK and
most emoji take 2, combining marks nothing), and a tab advances to the next tab stop.

- **Where.** A row may end after a run of spaces or tabs, after one of `})]?|/&.,;`, before one
  of `([{+`, and on either side of a wide character. A row ends at the last such place that
  fits; with none, at the last grapheme that fits. Spaces and tabs at the end of a row hang past
  the width, so a continuation row never starts with them.
- **Indentation.** Continuation rows are indented like the line (`same`, the default), one level
  more (`indent`), two more (`deep_indent`), or not at all (`none`), as VS Code's
  `wrappingIndent`. An indent of more than half the width is dropped. Tab stops in a
  continuation row count from its first character, as the browser lays out a row of its own.
- **Long lines.** A line cut for display (16 KiB, see `MAX_DISPLAY_LINE_BYTES`) wraps only the
  part shown, so its rows are bounded.

Alternatives considered:

- **Measuring in the frontend** (VS Code's `advanced` strategy). It handles proportional fonts
  and ligatures exactly, but the frontend holds only the viewport's lines, so it can't know how
  many rows the rest of the document takes, which scrolling, the minimap, and Up and Down need.
- **CSS wrapping (`white-space: pre-wrap`).** No break points to compute, but the browser and the
  backend could disagree on the number of rows, putting every row below a disagreement in the
  wrong place.

### Display rows count wrapped segments

A view keeps the lines that wrap, with how many extra rows each takes, in a sorted list (most
lines don't wrap, so it is short), like the hidden lines of folds. A second list, rebuilt when
either changes, has each visible wrapped line's first row and the extra rows before it. The row
of a line, the line on a row, and the row count combine the two with binary searches.

Edits keep the list up to date through the buffer's line splices: lines an edit replaced are
wrapped again, and those after it shift. A new width or indent, or tab size, wraps everything
again. Lines no longer than the width in bytes (and without tabs) need no measuring, since a
character's UTF-8 length is never less than its width; only longer lines are measured, so
wrapping a large file costs a scan of its line lengths.

Wrapping belongs to a view, like its folds and selections (each editor of a document has its own
width). A view without a width doesn't wrap.

### Moving by rows

Up and Down move by display rows, keeping the cursor's position within the row (its column from
the row's start, plus the continuation indent) as the goal. A position at a break shows at the
start of the next row. Home and End still go to the line's start (or its text's) and end: a
row's end is the same position as the next row's start, so End couldn't stay on its row without
the cursor knowing which of the two it is (VS Code keeps such an affinity; this doesn't, yet).
Clicking past the end of a row puts the cursor before the row's last character, so it stays on
that row.

### The protocol and the frontend

The view carries, for each line of the viewport that wraps, its break columns (UTF-16) and its
continuation indent. `primary.row` is the cursor's row, and `startRow` the first row of the first
line, which can be before the requested viewport when it begins inside a wrapped line. The line
cache is unchanged: it holds lines, and rows are derived from the wraps.

The frontend renders a wrapped line as one element with a block per row, the continuation rows
padded by the indent (in `ch`). Measuring carets and selections finds the row block of a column.
The gutter shows the line's number on its first row. Zones (code lenses, peeks) sit below a
line's last row. The minimap draws rows, so wrapped lines take several, as in VS Code.

### Settings and the command

- `editor.word_wrap`: `off` (the default), `on` (at the editor's width), `column` (at
  `editor.word_wrap_column`, 80 by default), or `bounded` (at whichever is less), as VS Code's
  `editor.wordWrap`.
- `editor.wrapping_indent`: `none`, `same`, `indent`, or `deep_indent`.
- **View: Toggle Word Wrap** (Alt+Z) turns wrapping on or off for the document, until it closes,
  over the setting.

The frontend works out the width (the text area's width over the width of `0` in the editor's
font, or the column setting) and sends it with every request, in the viewport; a request
without one doesn't wrap. An operation would have been sent once, but a view outlives its
editor (a hidden tab's, say) and would keep a width nobody wants after the settings change;
sent with each request, the backend is never out of date. A new width keeps the line at the top
of the editor in place.

## Consequences

- Prose, Markdown, and minified files read without scrolling sideways.
- Layout editor panes (Git's diff and merge editors) don't wrap: their extension lines up panes
  by line, with filler rows it computes, which wrapping would break.
- A font that isn't monospace, or one whose wide characters aren't twice as wide, wraps at the
  wrong place; the row count stays right, but rows may end early or overflow. (VS Code's
  default has the same limit.)
- Home and End work on lines, not rows.
- There are no per-language settings, so Markdown doesn't wrap by default as it does in VS Code;
  Alt+Z or the setting turns it on.
