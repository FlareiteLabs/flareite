# ADR 0054: Indentation per document, tidying on save, auto save, and drawing whitespace

- **Status:** Accepted
- **Date:** 2026-11-20

## Context

The 2026-11-11 review listed small settings that VS Code users expect and Flareite lacked:
indentation detected from a file, `.editorconfig`, trimming trailing whitespace and ending files
with a line break on save, auto save, drawn whitespace, indent guides, rulers, and relative line
numbers. Every document also took the one `editor.tab_size` and `editor.insert_spaces`, so a
file indented with tabs, or with two spaces in a four-space project, was edited wrongly.

## Decisions

### Each document has its own indentation, and knows where it came from

`Document` keeps its tab size and spaces-or-tabs with a source (`IndentSource`): the settings,
the content (detected), the file's `.editorconfig`, or the user's choice. A file that opens is
indented as its `.editorconfig` says, else as its content does (`editor.detect_indentation`, on
by default), else as the settings do; what an `.editorconfig` leaves out comes from the content
or the settings. Settings changes reach only documents whose indentation is the settings'.

**Detection** (`flareite_core::detect_indentation`) looks at the first 10,000 lines, as VS
Code's does: more lines starting with a tab than with two or more spaces means tabs; otherwise
spaces, as many as the commonest step (2 to 8) between neighbouring lines' indentation, ties
going to 4, 2, then 8. One space is taken for alignment (a comment's ` *`), not a level. Blank
lines don't break a run.

The status bar shows the active document's indentation (`Spaces: 2`, `Tab Size: 4`); clicking
it offers **Indent Using Spaces**, **Indent Using Tabs** (each then asks for a size), and
**Detect Indentation from Content**, which are commands too. Each editor sets its own CSS
`tab-size`, and word wrap (ADR 0052) uses the document's tab size.

### `.editorconfig`

`flareite_workspace::editorconfig_for` reads the `.editorconfig` files in a file's folder and those
above it, up to one with `root = true`. Nearer files, and later sections, win, and `unset` takes
a property out. A glob without `/` matches a name at any depth below its file; one with `/`
matches the path from it. Globs are `globset`'s, with EditorConfig's numeric ranges (`{1..3}`)
expanded first. No crate for EditorConfig was available, and the format is small.

Flareite uses `indent_style`, `indent_size` (including `tab`), `tab_width`,
`trim_trailing_whitespace`, and `insert_final_newline`. It ignores `end_of_line` and `charset`:
changing either rewrites the whole file, which opening shouldn't do.

### Tidying on save

`files.trim_trailing_whitespace`, `files.insert_final_newline`, and `files.trim_final_newlines`
(all off by default, as in VS Code) tidy a document when it's saved, after format on save and as
one undo step (`Document::apply_save_actions`). A file's `.editorconfig` overrides the first two.
**File: Save without Formatting** skips tidying too.

### Auto save

`files.auto_save` is `off` (the default), `after_delay` (`files.auto_save_delay`, 1,000 ms after
the last change), `on_focus_change` (when an editor loses focus, or the window does), or
`on_window_change`, as VS Code's. It runs in the frontend (`state/autoSave.ts`), which knows about
focus and typing, and saves only documents with a file. A save after a delay is as is: no
formatting or tidying, which would change the line being typed (trimming the space just typed);
the other modes save as Ctrl+S does.

### Drawing whitespace, guides, rulers, and line numbers

Whitespace and guides are drawn as backgrounds, so the rendered text, and every measurement made
from it (carets, selections, wrapping), stays exactly as it is.

- `editor.render_whitespace` (`none`, `boundary`, `selection` (default), `trailing`, `all`): runs
  of spaces get a span with a dot centred in each `1ch`, and each tab one with an arrow, through
  the overlays lines already have for decorations.
- `editor.render_indent_guides` (on by default): a 1 px line at each level a line's indentation
  passes, as layers of the line's own background, on its first row only. A blank line takes the
  deeper of the lines around it (in the view), so guides run through blank lines in a block.
- `editor.rulers`: vertical lines at columns (`Nch`), the height of the text. The settings editor
  gains lists of numbers for them.
- `editor.line_numbers`: `on`, `off` (the gutter keeps its markers), or `relative` (the distance
  from the cursor's line, which shows its own number).

Three theme tokens are new: `editor-indent-guide`, `editor-ruler`, and `editor-whitespace`.

## Consequences

- Files keep their own indentation, as in VS Code, and a project's `.editorconfig` is followed.
- A tidying setting can remove whitespace someone wanted (Markdown's two-space line break); an
  `.editorconfig` section can turn it off for those files. There are no per-language settings.
- Detection sees only the file's text when it opens; reopening applies a changed `.editorconfig`.
- Sticky scroll and bracket pair colorization are still to come.
