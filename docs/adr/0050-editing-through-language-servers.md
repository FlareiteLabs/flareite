# ADR 0050: Editing through language servers

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

A review of Flareite (2026-11-11) found that the language server client asked servers for
diagnostics, hover, definitions, references, symbols, and completion only. With rust-analyzer,
pyright, and the TypeScript server already running, what users miss most from VS Code is what
those servers do to the text: formatting, renaming a symbol everywhere, quick fixes, signature
help while typing a call, and completions whose snippets have tab stops (they were flattened to
plain text). The user asked for these next.

## Decisions

### The client says it can do them

`initialize` now offers formatting and range formatting, rename with `prepareRename`, code
actions (literals with kinds, `isPreferred`, `disabled`, `data`, and `codeAction/resolve` for
`edit`), signature help (Markdown, label offsets, the active parameter), snippet completions with
both insert text modes, `workspace/applyEdit`, `workspace/executeCommand`, and workspace edits
with document changes and file creates, renames, and deletes (`textOnlyTransactional`).

### Servers' requests that need the editor are answered later

A server sends `workspace/applyEdit` while one of the client's requests (`executeCommand`) waits
for it: TypeScript's code actions work this way. The reader thread used to answer server
requests itself, and couldn't apply edits. Now it passes such requests to the application as
`ServerEvent::Request`, which answers on a thread of its own with `LanguageServer::respond`. The
reader keeps delivering meanwhile, so the waiting request still gets its answer. Other requests
are still answered on the reader thread.

### One way to apply a workspace edit (`lsp/edits.rs`)

Rename, code actions, and `applyEdit` all apply a `WorkspaceEdit`:

- **Open documents** get one undo step each and stay unsaved. Positions refer to the text the
  server was sent: the version a document change names (from the last 16 sent, as for
  diagnostics), or the last one. They're mapped through edits made since then.
- **Files that aren't open** are read, edited, and written back in their encoding, checking the
  file hasn't changed since. This is what VS Code does by default (`files.refactoring.autoSave`).
  Opening each one as an unsaved tab would flood the dock on a large rename, and Flareite has no
  documents without tabs. The cost is that undo in the editor doesn't take these back. The
  editor says how many files changed.
- **Creates, renames, and deletes** go through the same code the explorer uses: open documents
  follow a renamed file, and deleting moves to the trash.
- An edit of text alone is checked whole first, so either everything is edited or nothing is
  (barring a file that can't be written at the end, which is reported). One with file operations
  is applied in order and stops at the first change that fails, as LSP's `textOnlyTransactional`
  says.

### Formatting, and formatting on save

**Format Document** (Shift+Alt+F) and **Format Selection** (Ctrl+K Ctrl+F; the cursor's line
without a selection) ask the document's server, with its tab size and spaces. The edits apply as
one undo step only if the document is still at the version that was formatted. `editor.format_on_save`
(off by default, also allowed in a folder's settings) formats before saving, waiting at most
3 seconds. A formatter that fails or is slow doesn't stop the save. **File: Save without
Formatting** (Ctrl+K Ctrl+Shift+S) skips it. Extensions can't format yet; formatting and code
action providers belong with the API 2.0 rework of callbacks, so they aren't added as yet another
command-and-getter pair.

### Rename (F2)

`prepareRename` gives the range and placeholder, or the server's reason that nothing can be renamed
there, shown as a notification. Servers without it get the word at the cursor. A box over the
symbol takes the new name: Enter renames, Escape cancels.

### Code actions (Ctrl+.) and the lightbulb

A request sends the server's own diagnostics that touch the selection, as it published them
(with their `code` and `data`) at their current positions. The answer is kept in the backend with
a token, and the editor chooses one by token and index. Applying one resolves its edit if needed,
applies it, then runs its command. A command the server doesn't offer is passed back to the
editor, which knows `editor.action.triggerSuggest`, `triggerParameterHints`, and `rename`. The
menu groups fixes, refactorings, and source actions, with the preferred fixes first.

Where the cursor rests (250 ms), the editor asks again with `trigger: automatic`. If any action
can be applied, a lightbulb shows in the gutter on the cursor's line, and clicking it opens the
menu. **Organize Imports** (Shift+Alt+O) applies the first `source.organizeImports` action
directly.

### Signature help

The view's language status carries the server's trigger and retrigger characters. Typing one opens
signature help above the cursor, with the parameter being written in bold and its
documentation. While it's open, typing and moving the cursor update it, and it closes when the
server has no answer. Up and Down choose among overloads, Escape closes it, and Ctrl+Shift+Space
opens it.

### Snippets in the core (`flareite-core/src/snippets.rs`)

Completion items in snippet format are no longer flattened. `EditorOp::InsertSnippet` parses
the LSP snippet syntax (tab stops, placeholders, nested placeholders, choices, variables such as
`TM_FILENAME` and `TM_SELECTED_TEXT`, escapes) and inserts it at every selection. With
`adjustIndentation` (unless the item says `asIs`), its lines are indented like the cursor's line
and its tabs become the document's indentation. The first stop is selected at every cursor. While
a snippet is active, **Tab** and **Shift+Tab** (the existing `Tab` and `Outdent` operations) move
between stops. A stop used several times is selected everywhere, so typing changes all of them.
The snippet ends at its final stop (`$0`), on Escape (the first Escape keeps the selections),
undo, a switch of view, or a Tab when the cursor has left the current stop, which then indents as
usual. Stops are byte ranges that follow edits through the edit log, as auto-closed pairs do
([ADR 0048](0048-brackets-and-quotes-in-pairs.md)).

Not supported: transformations (they insert nothing), filling a placeholder's mirrors with its
text before it's edited, and a list for a choice's other options.

## Consequences

- Rust, Python, TypeScript, and the other languages with servers get the editing VS Code users
  expect, and code actions that work through commands (TypeScript's) work too.
- Renames change files that aren't open on disk, and undo doesn't reach them.
- No extension API changed; the API stays at 1.19.
- Inlay hints, semantic tokens, document highlights, type definitions, implementations, call
  hierarchy, and extensions' formatters and code actions are still to come.
