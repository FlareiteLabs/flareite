# ADR 0041: Schemas, and settings for a folder

- **Status:** Accepted; documents are parsed by their languages' extensions since
  [ADR 0047](0047-languages-bring-their-documents-to-schemas.md)
- **Date:** 2026-11-09

## Context

TOML and JSON files were highlighted and nothing more. A mistake in `settings.toml` showed when
the file was saved, as a notification with the parser's message; a mistake in an extension's
`extension.toml` showed when the extension failed to load. Neither file offered its keys as they
were typed. VS Code does this for JSON with schemas: a setting maps files to JSON Schemas,
extensions contribute more (`jsonValidation`), files name their own (`$schema`), and the settings'
schema is made from what extensions declare.

The user asked for:

- a schema for `extension.toml`, as `extension.tosd`: a TOML Schema
  ([tomlschema.org](https://tomlschema.org)), with JSON Schema supported too, as the format tools
  already use;
- schemas configured globally and per workspace, like VS Code, which needs settings per workspace;
- schemas from extensions, both in their manifests and made by their code;
- a schema for `settings.toml` that includes every extension's settings.

They chose: schemas for TOML and JSON files; a folder's settings in `.flareite/settings.toml`,
limited to settings that are safe to take from a folder's files, and applied only once the folder
is trusted; User and Workspace tabs in the settings editor; schemas downloaded from the web, with
SchemaStore's catalog matched to well-known files.

## Decisions

### A crate of its own does the work on a document

`flareite-schema` checks a document against schemas, and completes and describes its keys and
values. It knows nothing of the app: callers hand it the document's text and the schemas, already
loaded with whatever they reference.

- **TOML Schema** is the reference implementation in Rust from the TOML Schema project, vendored
  under `src/tosd` (MIT), with the project's conformance corpus run as a test. It isn't published
  as a crate. Changes to it are kept to making its model visible to the crate, so it can be updated
  from upstream. TOML Schemas apply to TOML files only.
- **JSON Schema** checking uses the `jsonschema` crate (drafts 4 to 2020-12), without its network
  and file retrieval: references resolve only to documents the app fetched first. JSON Schemas
  check TOML files too, as the JSON they convert to (dates and times become strings), as Taplo
  does.
- **Where problems are.** Both validators report paths (`$.a."b.c"[0]`, `/a/b.c/0`), so documents
  are parsed again with spans (the `toml` crate's for TOML; a parser of its own for JSON, which
  accepts comments and trailing commas) to mark the value, or, for an unexpected or missing key,
  the key or its table. Syntax errors are reported, and alone.
- **Completion and hover** don't validate: they follow a schema to the path at the cursor, through
  references, compositions, and every alternative (a document being typed hasn't chosen one).
  Where the cursor is comes from a small lexer for each format that reads the text up to it,
  tolerating anything unfinished, since the document rarely parses while it's typed.

### Flareite's own schemas

`extension.tosd`, `settings.tosd`, and TOML Schema's own schema (for `.tosd` files) are in
`crates/flareite-schema/schemas`. Tests hold them to the code: every table of the manifest and of
`Settings` has exactly the keys serde accepts (read from its unknown-field error), the example
extensions' manifests pass, and documents the settings accept the schema accepts, and the other
way round.

The settings' schema is made when it's needed (`schemas/installed.rs`): `settings.tosd` with
what's installed as known keys, each described, so every table keyed by name completes and
explains its keys:

- `[extensions.settings]`: a table for each extension's declared settings (their types, ranges,
  choices, defaults, and descriptions; a number's range goes on each of its integer and float
  alternatives, as TOML Schema puts no bounds on a union);
- `[workbench.colors]`: every theme color token, described by the comment over it in
  `tokens.css` and its colors in the dark and light themes, and extensions' colors. Other keys
  must look like an extension's color (`<extension id>.<name>`), as the settings require, so a
  misspelled token is reported, with the closest one;
- `[lsp.servers]`: the servers extensions declare, with their commands and languages;
- `[terminal.profiles]`: the profiles found on this machine;
- `[extensions.programs]`: `<extension id>.<program>` for each program an extension runs;
- `[keybindings]`: every command, with its label and default keys. The commands are the
  frontend's (its registry has the built-in ones), so it sends the list (`schemas_set_commands`)
  when it changes.

Completing a theme, an icon theme, a link handler, a terminal profile, an update channel, or a
server's languages also offers what's installed, as the settings editor does. Tables keyed by name
still take names that aren't installed (a server or a profile to add, a command an extension
not loaded yet has).

`.tosd` files are checked by loading them, not with TOML Schema's schema: loading finds more (the
reference implementation checks references, cycles, and patterns), and checking a file against
that schema takes seconds, since its unions are evaluated speculatively. The schema still
completes and describes their keys.

### Which schemas apply

Most specific first, in `src-tauri/src/schemas`:

1. **The one a document names**, alone: `"$schema"` in JSON, a `#:schema` comment before a TOML
   file's first key (as Taplo reads it), or a TOML Schema's `[toml-schema] location`.
2. **Every match** among the open folder's `[[schemas.associations]]` (once the folder is
   trusted), the user's, those extensions provide, and Flareite's own (`settings.toml` files,
   `extension.toml` files without Zed's `schema_version`, and `.tosd` files).
3. **The first match in SchemaStore's catalog**, if `schemas.catalog` is on (the default).

Patterns are matched as VS Code's `fileMatch` is: a name in any folder without '/', the end of the
path with it, and an exclusion with '!'.

### Schemas from the web are downloaded and kept

Addresses with `https:` (and `http:`, which JSON Schema's own `$schema`s use) are downloaded in the
background into the cache folder, used from there, and downloaded again a day later (the catalog,
a week); the copy kept is used meanwhile. A document waiting for one is checked without it, then
again. This adds to ADR 0036's "no network unasked": SchemaStore's catalog is downloaded when a
TOML or JSON file is first opened, unless `schemas.catalog` is off, and schemas when a file needs
one. `schemas.enabled = false` turns everything off.

### Extensions declare schemas, and their code can set more (API 1.16)

The manifest's `[[schemas]]` name `files` and a `path` (a `.tosd` or `.json` file in the extension's
folder) or a `url`. The new `schemas` interface's `set-schema` adds or replaces one the code makes
(with its format and content), which is checked when it's set, so the extension hears why it's
wrong; `remove-schema` takes one back. They last until the extension stops, at most 32 with the
manifest's, each at most 4 MiB. Documents can name them as
`flareite://extensions/<extension id>/schemas/<id>`.

### Checking runs on a thread of its own

Building a view schedules a check of a TOML or JSON document whose problems are out of date; a
thread checks documents a quarter second after typing pauses, keeping only the latest version of
each. Problems are kept with the version they're for and move with edits until the next check, as
language servers' are. Changes to settings, extensions, or schema files (saved, or changed on disk)
check every document again. They're shown as squiggles, in hover cards (with what the schema says
about the key), and in the Problems view, labeled with the schema's name. Completion adds the
schemas' keys and values to a language server's, so a document with both gets both.

### A folder's own settings

`.flareite/settings.toml` in the open folder applies over the user's settings while the folder is
open and trusted, merged table by table. It can set only `[editor]`, `[files]`, `[search]`,
`[schemas]`, `lsp.enabled`, and `[extensions.settings]`. The rest name programs to run (language
servers, terminal profiles, extensions' programs, development extensions), or are the user's own
(themes, keybindings, updates, trust), and a folder's files could be anyone's. What it sets
otherwise is ignored and marked in the file. Its `[[schemas.associations]]` are added to the
user's, with paths relative to the folder. An invalid folder file applies nothing, and says why.

The settings editor has **User** and **Workspace** tabs when a folder is open. The Workspace tab
shows the values in effect there, changes the folder's file (creating it), and marks the settings
only the user's file can change. **Preferences: Open Workspace Settings** opens it, and **Open
Workspace Settings (TOML)** the file.

## Consequences

- TOML Schema 1.0 is a release candidate; the vendored implementation follows upstream, and its
  corpus test says when an update changes behavior.
- `serde_json` keeps object keys sorted, so a JSON Schema's properties are offered alphabetically
  rather than in the schema's order.
- A JSON Schema's references are fetched before it's loaded, at most 64 documents.
- Settings added to `Settings` need a key in `settings.tosd`; the test fails until they have one.
- Extensions' settings in a folder's file apply like the user's: an extension can't yet say that a
  setting of its own shouldn't come from a folder.
