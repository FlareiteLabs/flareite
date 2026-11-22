# ADR 0060: Symbol queries, files' bytes, and file prompts (API 2.4)

- **Status:** Accepted
- **Date:** 2026-11-22

## Context

The official Linker Script extension (`flareite.linker-script`, its own repository) needed three
things the host didn't have:

- **Go to Symbol and Go to Definition for a language without a language server.** Both came only
  from servers (`lsp_document_symbols`, `lsp_definition`). There's no widely used server for
  linker scripts, and the extension API has no symbol or definition providers. Adding them as
  world exports would break every extension built against 2.0 to 2.3 (Wasmtime checks the
  exports a component has), as ADR 0057 found for variant cases.
- **A program's bytes.** Its Memory Layout view reads the ELF file a build linked.
  `files::read-file` reads text only (binary files are `not-text`), and
  `viewers::read-viewed-file` reads only a file open in one of the extension's viewers.
- **Asking for a file.** The view asks which program to show. `prompts` had `pick-folder` (the
  system's dialog) but nothing for a file, and a list of the folder's files can't offer a build's
  output, which `list-files` leaves out as Git ignores it.

## Decisions

### A language's symbol query (manifest only)

A `[[languages]]` entry may name **`symbols = "queries/symbols.scm"`**, a Tree-sitter query in
the tags convention:

- `@definition.<kind>` captures a whole definition and `@name`, in the same match, its name. The
  kind is one of the symbol kinds the palette knows (`function`, `struct`, `module`, `variable`,
  ...); others show as `property`.
- `@reference` captures a name that may refer to a definition; `@reference.<kind>` one that
  refers to a definition of that kind (`> FLASH` is a memory region, never a symbol named
  FLASH). Other captures, such as `@_function` for a predicate, are ignored.

`flareite-syntax` runs it (`document_symbols`, `definition_at`, `references_at`, at most 10,000
symbols) on the document's tree, the one kept for highlighting (parsed on the spot, within
500 ms, if the first parse hasn't finished). The app uses it **when no language server can
answer**: no server for the document, or one without the capability. Go to Symbol lists the
definitions with the innermost one each is in; Go to Definition goes to the first definition
with the name under the cursor (one of the kind a reference names first); Find References lists
the definitions' and references' names that match. All within the document. A server that
answers wins, so a language can have both.

It's manifest-only: no WIT change, so no version of its own (see the API version rule); it came
with 2.4. The schema (`extension.tosd`) has the key.

### `files::read-bytes` (2.4)

**`read-bytes(path, offset, length)`** returns a `byte-slice`: the file's size and up to
`length` bytes from `offset` (fewer at the end, none past it), as they are on disk. The same
rules as `read-file`: `files_read`, inside the open folder, not excluded, not in `.git`. At most
16 MiB a call; larger files are read in parts. A folder is `wrong-kind` (checked before opening,
which Windows refuses for folders).

### `prompts::pick-file` (2.4)

**`pick-file(title)`** shows the system's file dialog, starting in the open folder, and answers
`text(path)` or `cancelled`, as `pick-folder` does. The workbench shows it like `pick-folder`
(`dialog_pick_file`).

Both are additions to existing interfaces, so the package is 2.4.0 and extensions built against
2.0 to 2.3 load unchanged.

## Alternatives considered

- **Symbol and definition providers in the API.** Exact for every language, but new exports
  break older extensions, and a query covers what a script language needs. A provider can still
  come with a major version.
- **Running `readelf` for the layout.** No API change, but it isn't installed everywhere (lld
  users on Windows often have no binutils), its output differs between binutils and LLVM, and
  parsing ELF is short.
- **Offering the folder's files in a pick list.** Builds' outputs are what Git ignores, which
  `list-files` leaves out, and listing `target/` for executables means reading their headers.
