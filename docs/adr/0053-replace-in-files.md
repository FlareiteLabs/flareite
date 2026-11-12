# ADR 0053: Replace in files, and which files to search

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

Search in Files (ADR 0003) finds matches across the open folder, but can't replace them, and
searches every file that isn't ignored: there's no way to look only in `src`, or to leave out
tests. ADR 0003 deferred replacing. The 2026-11-11 review ranked both next after word wrap,
and they share the panel.

## Decisions

### Which files: patterns to include and exclude

Below the search box (**Toggle Search Details**, `…`) are **files to include** and **files to
exclude**: comma-separated globs, as in VS Code. A comma inside braces (`*.{ts,tsx}`) doesn't
split. Patterns follow `files.exclude`'s rules (`ExcludeFilter`): one without a `/` matches a
file or folder name at any depth; one with a `/` matches the path from the folder's root (a
leading `./` or `/` is dropped). A pattern that matches a folder takes everything in it, so
`src` includes the `src` folders and `tests` excludes the `tests` folders.

- **Include** keeps a file if it, or a folder it's in, matches; with none, every file is kept.
- **Exclude** leaves out what matches, and the walk doesn't go into a folder that does.
- **Use Exclude Settings and Ignore Files** (on by default) applies `files.exclude` and
  `.gitignore` files as before; off, only the panel's patterns apply. `.git` is left out either
  way.

A pattern that doesn't parse is reported where an invalid regex is: under the search box.

### Replacing

A **replace** box (shown with the chevron beside the search box, or Ctrl+Shift+H) replaces the
results: **Replace All** (Ctrl+Alt+Enter in the box), a file's, or one match's, from buttons
on their rows. Rows can also be dismissed, which leaves them out of Replace All. While there's a
replacement, each result shows the match struck out and what replaces it. For a regex query,
`$1`, `${name}`, and `$$` expand as in the editor's Replace (the preview expands them with
JavaScript's regex engine; the backend's is what counts).

**Replace All asks first**, saying how many matches in how many files, since files that aren't
open can't be undone.

### What's replaced is what was shown

A replace sends each file's results as the panel shows them (line and UTF-16 columns). The
backend searches the file's current text again and replaces the matches at those places. A
result whose text has moved or changed since is left alone and counted, so a replace never
lands somewhere the user didn't see. Afterwards the search runs again.

Searching again rather than sending byte ranges also means the replacement's capture groups
come from the match as it is now.

### Where it's written

Like a rename (ADR 0050):

- **Open documents** are edited as one undo step each, and left unsaved.
- **Files that aren't open** are read in their encoding, edited, and written back, refusing a
  file that changed on disk since it was read.

Read-only (provided) documents aren't touched. The result says how many matches were replaced
in how many files, and what was skipped or failed.

Search now drops a UTF-8 byte order mark before searching, as the editor does, so columns on a
file's first line agree between the results, the editor, and a replace.

## Consequences

- A project-wide rename of a word, or of a pattern with groups, no longer needs a terminal.
- Files that aren't open are written at once, with no undo, as after a rename; the confirmation
  says so.
- Results don't follow edits made after the search; a replace skips what moved, and says so.
- There's no side-by-side preview of a file's replacements (VS Code opens a diff); the
  struck-out text in the results is the preview. No case-preserving replace yet.
