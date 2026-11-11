# ADR 0044: YAML documents, and the YAML extension

- **Status:** Accepted; documents are parsed by their languages' extensions since
  [ADR 0047](0047-languages-bring-their-documents-to-schemas.md)
- **Date:** 2026-11-10

## Context

The user asked for a YAML extension with schema validation by the editor's own schema crate,
`flareite-schema` ([ADR 0041](0041-schemas-and-folder-settings.md)), which understood TOML and
JSON documents only. YAML is where schemas matter most day to day: CI workflows, Compose files,
Kubernetes manifests, and tools' configuration, almost all of which SchemaStore's catalog already
knows by file name.

Extensions can publish diagnostics, but nothing lets their code complete or describe a document.
The TOML and JSON extensions are grammars only, and the host does schemas for their languages, so
YAML follows them.

## Decisions

### The schema crate reads YAML

`Format::Yaml` joins TOML and JSON. Documents are parsed with `saphyr-parser` (YAML 1.2, MIT or
Apache-2.0), an event parser whose events carry spans, into the same trees of values with where
each is, and the JSON each document stands for, which JSON Schemas check. TOML Schemas don't apply
to YAML.

- **Values** resolve by YAML 1.2's core schema, as the YAML language server and current parsers
  read them: `yes` and `on` are strings, `0o17` and `0x1F` integers. `!!str` and the other tags of
  YAML's own make a value what they say; other tags (`!Ref`) don't change it. Infinity and not a
  number, which JSON can't hold, stay as written.
- **Streams.** Each document of a file (separated by `---`) is checked on its own, as Kubernetes
  manifests need. Completion and hover use the cursor's document.
- **Aliases** are expanded, and **`<<` merge keys** merged (YAML 1.1's, which Compose files and CI
  configuration rely on): keys written out win over merged ones, and earlier merges over later
  ones. Problems in a merged or aliased value are marked where it's written. Expansion stops at
  100,000 values, so a "billion laughs" file is an error rather than a hang.
- **Duplicate keys** are errors, as YAML says; JSON's stay warnings.
- **Syntax errors** are reported alone, as for TOML and JSON. The parser stops at the first, so a
  file has at most one; completion still uses what was read before it.
- **Where the cursor is** comes from a lexer of its own (`yaml::place`): YAML's structure is its
  indentation, so the lines of the cursor's document are read up to it, keeping the mappings and
  sequences open at each column, the key whose value is on the next lines, and block scalars to
  skip. Flow collections are read as JSON is. On an empty line, the column says which mapping a
  key goes in. After a sequence's `-`, what's typed may be a key of the item or the item itself,
  so both are offered.
- **Completion** inserts `key: ` for a value, and for a mapping or sequence the key with its value
  started on the next line, indented two spaces past the key (`key:\n  - `). Strings are inserted
  plain when they'd read back the same (`dark`, not `"dark"`), else double-quoted; in quotes
  already, in those quotes.

### Which files are YAML, and which schemas they name

Documents of the `yaml` language, and files ending in `.yaml` or `.yml`, as for TOML and JSON. A
document names its schema with the YAML language server's comment,
`# yaml-language-server: $schema=<address>`, or a `$schema` key at its top. JSON Schemas at an
address or path may be written in YAML, as the YAML language server allows.

### The extension

`flareite.yaml` is a grammar and queries, like JSON's and TOML's, in its own repository
(`FlareiteLabs/flareite-yaml-ext`, [ADR 0034](0034-official-extensions-in-their-own-repositories.md)):

- the grammar is `tree-sitter-yaml.wasm` from npm's `@tree-sitter-grammars/tree-sitter-yaml@0.7.1`
  (MIT), language ABI 14;
- its highlight query is the grammar's, with the patterns for keys moved first: a key's scalar is
  a string too, and the earlier pattern wins for the same node;
- mappings' and sequences' entries fold, with flow collections;
- `.yaml` and `.yml` files, and `.clang-format`, `.clang-tidy`, and `.clangd`, which are YAML
  without saying so.

No API changed: schemas for YAML are the host's, so extensions' `[[schemas]]` and `set-schema`
already apply to YAML files on a Flareite that checks them, and are ignored for them on one that
doesn't.

## Consequences

- GitHub workflows, Compose files, and every other YAML file SchemaStore's catalog knows are
  checked and completed when they're opened, with `schemas.catalog` on.
- Custom tags aren't configurable as the YAML language server's `yaml.customTags` is. They're
  accepted silently, and the value under one is checked as if untagged, so CloudFormation's `!Ref`
  and `!GetAtt` short forms are checked as strings and sequences where the schema expects the long
  form.
- Completion assumes two-space indentation for what it starts on the next line.
- Formatting YAML is left to a formatter extension.
