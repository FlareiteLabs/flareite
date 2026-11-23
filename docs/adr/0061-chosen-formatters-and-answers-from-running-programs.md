# ADR 0061: Chosen formatters, and answers from running programs (API 2.5)

- **Status:** Accepted
- **Date:** 2026-11-22

## Context

The official Prettier extension (`flareite.prettier`, its own repository) formats with the
project's Prettier, which is JavaScript and runs in Node.js. Two things stood in its way:

- **It would never be asked.** `lsp::format` asked the document's language server first and an
  extension's `[[formatting_providers]]` only when no server formats. TypeScript, HTML, CSS, and
  JSON all have servers that format, and those are Prettier's main languages. VS Code solves the
  same conflict with `editor.defaultFormatter` per language, a "Format Document With…" command,
  and a prompt when several formatters exist and none is chosen.
- **It would be slow.** `provide-formatting` must return its edits. The only way to get a
  program's output within a call was `processes::run`, which starts the program each time: about
  170 ms for one `prettier` run here (440 ms cold), on every Format Document and every save with
  format on save. Keeping Node.js running with Prettier loaded answers in 10–30 ms, but an
  interactive program's output (ADR 0057) arrives only as `process-output` events, and calls
  never overlap, so a provider can't wait for one.

## Decisions

### A formatter per language (app; no WIT change)

- **`[editor.formatters]`** maps a language id to the formatter that formats it: a language
  server's name (`rust-analyzer`) or an extension's id (`flareite.prettier`). Folder settings can
  set it (it's under `[editor]`).
- `lsp::format` takes a **`Pick`**: `Default` (the chosen formatter if it can format the
  document; else the language server; else the extensions in turn, as before), `Ask` (the same,
  but answering `choose` when several can and none is chosen), or `With(id)`.
- A chosen or named extension is the **only one asked** (`ExtensionHost::format_with`): its empty
  answer (an ignored file, say) leaves the document alone rather than falling through to another
  formatter.
- **Format Document** asks (`choose`), and the workbench then lists the formatters
  (`lsp_formatters`) in the palette: "Choose the default formatter for TypeScript files". The
  choice is saved (`settings_set_formatter`) and the document formatted with it. A new
  `formatters` table is written inline in `[editor]` (`formatters = { typescript = "..." }`):
  a new `[editor.formatters]` table would land after the comments that end `[editor]` in the
  settings template, or among `[keybindings]`'s. An existing table is changed in place.
- **Format Document With…** (`editor.formatDocumentWith`) formats once with one chosen from the
  list, marking the default, and has **Configure Default Formatter…**.
- **Format on save** uses `Default`: it never asks. With several formatters and no choice, the
  language server formats, as before.

### `processes::read-output` (2.5)

**`read-output(process, timeout-ms) -> result<list<output-line>, process-error>`** returns the
lines a program `spawn-interactive` started wrote that the extension hasn't had yet, taking them
out of the queue (`Queue::take_output`) so they don't also come as an event. It waits for the
first at most `timeout-ms`, and never past the running call's deadline (less the margin `run`
keeps); an empty list when none came. The program's reader, if it was waiting for room
(`wait_for_output`), is woken. It fails with `invalid-argument` for a program that wasn't started
interactive (the process table remembers which were), or that has ended with nothing left: its
last lines are queued before it leaves the running table, so once it's off, what's queued is all
there is. A cancelled call ends the wait with `killed`.

A provider writes its request with `write-input` and reads until its answer comes; lines for
other requests (status queries, a folder being checked) come along and are the extension's to
handle after the answer.

### `workbench::show-log()` (2.5)

Shows the extension's own log channel in View: Output (`Notification::ShowLog` →
`extensionShowLog`). Prettier's status bar item opens it when formatting failed. Before, an
extension could only tell users to find its log.

Both are new functions in existing interfaces, so the package is 2.5.0 and extensions built
against 2.0 to 2.4 load unchanged.

### The Prettier extension

- `server.cjs`, built into the module (`include_str!`) and run with `node -e`, so the extension
  ships no files Node.js has to find. It resolves Prettier from each file
  (`createRequire(file).resolve`, so monorepos and pnpm work), else a global install (beside
  Node.js, `%APPDATA%\npm`, or `npm root -g`), and loads each once. Requests are handled side by
  side.
- Documents are formatted with `endOfLine: "lf"`: the editor keeps `\n` and writes each file's
  own line endings when saving. Without a Prettier config, the editor's indentation is passed
  (`tabWidth`, `useTabs`), as VS Code's extension does; with one, Prettier's defaults hold, so
  the result matches `prettier --check` in CI.
- One edit per format: what's between the old and new text's common start and end, so the cursor
  and the rest of the document stay put.
- The server restarts when a `package.json` changes (Prettier installed or upgraded), and drops
  Prettier's config cache when a config, `.prettierignore`, or `.editorconfig` does.

## Alternatives considered

- **Extension formatters before language servers.** No setting or prompt, but every extension
  that formats a language would override its server for every user, with no way back short of
  disabling the extension; a second formatting extension would make the order arbitrary.
- **Run `prettier` per format.** No API change, but 0.2–0.5 s on every save with format on save.
- **Answer providers asynchronously** (a `provide-formatting` that returns a ticket, answered
  later). Every provider could wait for events then, but it changes the world's exports, which
  breaks every extension built against 2.x (Wasmtime checks them), and the editor's waiting code
  for every provider.
- **A bundled Prettier.** The user chose the project's Prettier, else a global one: a bundled copy
  would be 8 MB and need the extension to know its own folder.
