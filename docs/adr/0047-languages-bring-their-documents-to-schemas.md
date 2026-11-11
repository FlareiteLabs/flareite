# ADR 0047: Languages bring their documents to schemas

- **Status:** Accepted
- **Date:** 2026-11-10

## Context

Schemas ([ADR 0041](0041-schemas-and-folder-settings.md), [ADR 0044](0044-yaml-documents.md))
were built for three languages: `flareite-schema` parsed TOML, JSON, and YAML itself, found the
cursor's place in each with a lexer of its own, and wrote keys and values in each one's syntax.
The host chose a format by the document's language or file extension. Another configuration
language (KDL, HCL, RON, one that doesn't exist yet) would need changes to the editor.

The user asked for the opposite split: the editor takes in the schema formats, TOML Schema and
JSON Schema, and exposes an API, and each language's extension applies schemas to its own
documents, so a new configuration language needs only a new extension. They chose:

- **No fallback.** Without the TOML extension, `settings.toml`, `extension.toml`, and `.tosd`
  files aren't checked or completed. Users install what they want; nothing is bundled.
- **Both schema engines stay.** The schemas aren't translated into a format of Flareite's own.
- **Completion and hover are a general extension API**, not one for schemas only.
- **The YAML work moves into the YAML extension**, and TOML's and JSON's into theirs.

## Decisions

### The document is what's shared, not the schema

Translating JSON Schema and TOML Schema into one Flareite schema would mean writing a JSON Schema
validator again (`if`/`then`/`else`, `$dynamicRef`, `unevaluatedProperties`), losing the
`jsonschema` crate's conformance and the vendored TOML Schema implementation's corpus. A new
language doesn't need to understand schemas anyway: it needs to say what its document holds,
and to write in its own syntax what the schemas say.

So what's standardized is the **document**: `flareite-schema-document` (a crate with no
dependencies but `serde_json`, shared by Flareite and extensions) is a tree of tables, arrays,
and scalars, each with its span and its key's, in parts (a YAML stream's documents), with the
schema the document names and its parser's problems. Scalars are typed as TOML's are, which
covers JSON's and YAML's: null, booleans, integers, floats, strings, and dates and times.

Both engines take the tree. A JSON Schema checks it as the JSON it stands for (dates and times
as strings); a TOML Schema as TOML values, which have no null, so a null is a problem rather
than a crash. TOML Schemas therefore apply to JSON and YAML documents too, where before they
were for TOML only. The engines' answers about a path (properties, values, what's known of a
value) were already the same for both; they're the API's.

### `flareite-schema` keeps only schemas

The crate keeps both engines, Flareite's own schemas, checking a tree (`check`, `check_tosd`,
`parse_problems`), and the queries (`properties`, `choices`, `info`). Problems are still marked
where they are: on a key for a key's problem, a scalar as written, a table's key, or the first
line of a value or part. It reads JSON Schemas written with comments or in YAML, which is
loading schemas, not reading documents. The TOML, JSON, and YAML parsers, the three cursor
lexers, and writing values in each syntax moved to the extensions.

### API 1.18: documents for schemas

`schemas` gains:

- **`set-document(id, version, tree)`**: what an open document holds, as the extension parsed
  it. WIT has no recursive types, so the tree is a list of nodes, each naming the table or array
  it's in (before it); each root is a part, with the part's span. Flareite drops a tree for a
  version the document has moved past (a newer one is coming), checks the latest once typing
  pauses, and shows the parser's problems (labeled with the language's name) and the schemas'.
  A parser's error stops checking, as syntax errors did. `clear-document` forgets one; an
  extension's trees go when it stops.
- **`properties`, `values`, and `describe(id, part, path)`**: what the schemas that apply to the
  document say at a path in one of its parts, following a JSON Schema's `if` through the latest
  tree. Values come as JSON. `values` adds what's installed for Flareite's own settings (themes,
  servers), as completion did; `describe` names the schemas, for a hover's "From …" line.

Which schemas apply is unchanged, except that the document's named schema comes from the
extension (each language has its own way: `$schema`, `#:schema`, a modeline), and Flareite's
own schemas match `settings.toml` and `extension.toml` files by name, whatever extension parses
them. All of it needs `documents-read`.

### API 1.18: completion and hover from extensions

The new `language-features` interface is general: the manifest's `[[completion_providers]]`
(languages, a command, trigger characters) and `[[hover_providers]]` (languages, a command)
declare them, and need `documents_read`. When the user asks for completions or hovers in a
document of one of those languages, Flareite runs the provider's command; while it runs,
`completing` or `hovering` says what it's asked (the document, version, and position), and it
answers with `set-completions` (at most 1,000 items, each replacing text on the request's line
up to the cursor, with a kind, detail, filter, and sort text) or `set-hover` (Markdown). This
follows the payload pattern of provided documents and link handlers: the editor waits for the
answer up to two seconds, and the extension has been told of every change up to the request's
version, since its queue keeps order. Completions join a language server's; hover Markdown goes
above the server's. A language with completion providers gets their trigger characters, and the
editor's completion, without a language server, as TOML, JSON, and YAML got from schemas.

### A crate for languages' extensions

`flareite-extension-schemas` holds what every configuration language needs besides its syntax:
the `Language` trait (parse, the place at the cursor, decoding and writing keys and values),
`Documents` (following open documents' changes with the editor's own line breaks, parsing each
change, setting trees, and answering both kinds of request), completion and hover put together
from the queries (keys a table has left out, required keys and defaults first, details), and
the hover card's Markdown. With its `testing` feature it answers from `flareite-schema`
natively, so an extension's tests check, complete, and describe documents as Flareite will. It
and `flareite-schema-document` take no workspace inheritance, so extensions in other
repositories depend on them by path, as on `flareite-extension-api`.

### The language extensions have code

TOML (`flareite.toml` 0.2.0), JSON (0.2.0), and YAML (0.2.0) each implement `Language` with what
moved out of the editor, activate for their languages, ask for `documents_read`, and declare a
completion and a hover provider with the trigger characters the editor had for them. Their
tests are the editor's former tests of each format, against the real engine.

## Consequences

- A configuration language is an extension: a parser into the tree, a cursor lexer, and how to
  write keys and values. Schemas, associations, the catalog, downloads, and checking are
  Flareite's.
- Without the TOML extension, Flareite's own files aren't checked as they're typed; settings are
  still checked when saved, and manifests when extensions load.
- The three extensions now run code and ask for `documents_read`, and their releases have a
  module; their grammars are unchanged.
- Each keystroke in a large configuration file is parsed in WebAssembly and sent as a tree; the
  extension's queue merges changes while it's busy, and documents over 4 MiB aren't parsed.
- Messages about TOML Schemas' allowed values write them as TOML, whichever language the
  document is in.
