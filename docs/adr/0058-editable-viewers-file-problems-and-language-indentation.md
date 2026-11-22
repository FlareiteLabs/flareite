# ADR 0058: Editable viewers, problems in files, and indentation by language (API 2.2)

- **Status:** Accepted
- **Date:** 2026-11-21

## Context

Three official extensions were asked for together: a hex editor, NASM, and Makefiles. Each
needed something the extension API didn't have.

- **A hex editor changes binary files.** Viewers ([ADR 0023](0023-encodings-and-viewers.md),
  webviews in [ADR 0025](0025-webviews.md)) could only read the file open in them. A page could
  keep its own changes and write them with a new call, but then the workbench wouldn't know the
  tab is unsaved: no dot on the tab, Ctrl+S and Save All wouldn't save it, and closing the tab or
  the window would lose the changes without asking.
- **A build reports problems in files that aren't open.** `publish-diagnostics` takes a document
  id, so an extension could only report problems in open documents. The Makefile extension runs
  `make` and reads gcc's, clang's, and NASM's errors, which name any file in the project.
- **Makefiles need tabs.** A recipe line must start with a tab, but a document's indentation came
  from its `.editorconfig`, its content, or the user's settings ([ADR 0054](0054-editor-settings.md)),
  and a language couldn't say. VS Code's Makefile support sets `editor.insertSpaces: false` for
  the language.

Version 2's rule holds: a minor version adds functions, interfaces, and types, and never changes
an existing type ([ADR 0057](0057-interactive-programs-and-terminals.md)). In particular no new
`event` case, and no new export: a new export would be missing from every component built
before it. That also ruled out the outline and go to definition the NASM extension could have
offered itself (they would need new `provide-*` exports); asm-lsp, a language server, gives both.

## Decisions

### Editable viewers: Flareite keeps the changes

A viewer declares `editable = true` in its manifest entry. Its extension changes the file's
bytes with **`viewers::edit-viewed-file(viewer, path, version, changes)`**: byte changes
(`offset`, `removed`, `inserted`), made in order as one step the user undoes at once.
**The host keeps them**, not the extension or its page:

- The first change reads the whole file into memory (`viewer_edits::ByteBuffer`, at most
  256 MiB), and the changes are made there, with an undo history (64 MiB of it, the oldest steps
  forgotten past that). `read-viewed-file` reads through the buffer, so the page and the
  extension see the changes. The file on disk can change or go meanwhile; the changes stay.
- **The workbench owns saving.** The tab shows a dot while the buffer differs from what was last
  saved. **File: Save** (Ctrl+S, forwarded from the page like any workbench key) and **Save All**
  write it, through a temporary file beside it that replaces it. Closing the tab, reopening it as
  text or in another viewer, and closing the window ask first, as for documents. **File: Revert
  File** drops the changes.
- **Undo and redo** are the host's too: the page asks its extension, which calls
  `undo-viewed-file` and `redo-viewed-file` (they return where the change was, for the cursor),
  and **Edit: Undo** and **Redo** in the workbench work on the tab. When the bytes change other
  than by the extension's own calls (undo from the workbench, Revert File), it hears
  `viewer-opened` again, the existing event for "show this file", and shows them anew.
- **Versions** guard against stale changes. Each change, undo, redo, and revert raises the
  file's version, which `viewed-file-status` returns; a change made against an older version
  fails with `stale-version` and changes nothing. After a revert, versions go on from the
  reverted changes', so a version the extension saw is never seen again.

Undo in the extension's page instead was considered: the page would keep its changes and send
them all when saving. But the workbench saves (Ctrl+S, Save All, the prompt when quitting), and
it can't ask a page for its bytes without a new event. With the host keeping the bytes, saving
needs nothing from the extension, works while it's stopped, and a crashed page loses nothing.

There are no backups of a hex editor's changes, as there are of documents' (they can be
256 MiB): quitting asks about them instead.

### Problems in files by path

**`host::publish-file-diagnostics(path, list)`** replaces an extension's problems in a file
named by its absolute path, open or not; an empty list removes them, and
**`clear-file-diagnostics()`** removes all of the extension's. The Problems view lists them for
files that aren't open, as it lists language servers' for such files, and an open document shows
them as its own. Positions are lines and byte columns in the file as it was checked: while it's
edited they stay where they are (moved inside the text if it got shorter) until the extension
publishes again, as a build's problems do in other editors. Mapping them through edits, as
document diagnostics are, would need the text they were found in, which a build doesn't have. They
go when the extension stops. At most 5,000 per file, in at most 1,000 files per extension.

### Indentation by language

`[[languages]]` takes **`indentation = "tabs"`** (or `"spaces"`), a manifest key with no API
change. A file of the language is indented that way over what its content and the settings say;
its `.editorconfig`, and the user's choice for the document, still win. The status bar's
indentation shows where it came from (`IndentSource::Language`).

## Consequences

- WIT 2.2.0: `viewers` gains `byte-change`, `viewed-file-state`, `viewed-file-step`,
  `edit-viewed-file`, `viewed-file-status`, `undo-viewed-file`, and `redo-viewed-file`; `host`
  gains `publish-file-diagnostics` and `clear-file-diagnostics`. Extensions built against 2.0
  and 2.1 load unchanged.
- The official **Hex Editor** (`flareite.hex-editor`), **NASM** (`flareite.nasm`), and
  **Makefile** (`flareite.makefile`) extensions use them, each in its own repository.
- A viewer that isn't editable is unchanged: `edit-viewed-file` and the rest fail for it with
  `invalid-argument`.
