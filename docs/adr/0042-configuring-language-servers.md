# ADR 0042: Configuring language servers, and the Luau extension

- **Status:** Accepted
- **Date:** 2026-11-09

## Context

The user asked for an official Luau extension. Highlighting was the easy part: `tree-sitter-luau`
1.2.0 ships a prebuilt grammar. The language server, [luau-lsp](https://github.com/JohnnyMorganz/luau-lsp),
is where Luau is useful, and most Luau is written for Roblox, where luau-lsp needs:

- **Roblox's type definitions and API docs**, which VS Code's luau-lsp extension downloads (from
  `luau-lsp.pages.dev`, for a chosen security level) and passes as `--definitions` and `--docs`.
  luau-lsp 1.70 loads definitions only from its command line: `types.definitionFiles` sent as a
  setting is ignored (tried).
- **Settings**, such as `luau-lsp.platform.type`, which it asks for with
  `workspace/configuration`. Without them it assumes Roblox, and with no definitions every
  `game` is an unknown global.
- **`sourcemap.json`**, which Rojo writes (`rojo sourcemap --watch`), to resolve requires and
  instance paths. luau-lsp doesn't watch files itself: it registers watchers for
  `sourcemap.json`, `.luaurc`, and `*.luau` with `client/registerCapability`, and expects
  `workspace/didChangeWatchedFiles`.

Flareite's LSP client answered `workspace/configuration` with nulls, accepted and ignored
registrations, didn't advertise either capability, and a server's command line was fixed in its
extension's manifest. So a Luau extension could highlight files and start luau-lsp in its
defaults, and nothing more.

The user chose: Luau and Roblox; Flareite (not the extension) downloads and keeps the
definitions; `.luau` and `.lua` files; Rojo sourcemaps generated automatically; `.luaurc` checked
by a schema; comment toggling.

## Decisions

### Servers have settings

`[[language_servers]]` takes a `settings` table, and users' `[lsp.servers.<name>]` a `settings`
table merged over it. They answer `workspace/configuration` by section, and changes reach running
servers as `workspace/didChangeConfiguration`. Keys are VS Code's: dotted keys
(`"luau-lsp.require.mode"`) are the same as nested tables, so settings copied from VS Code work as
they are. A folder's `.flareite/settings.toml` may set `lsp.servers.<name>.settings` but nothing
else under `lsp.servers` (the command stays the user's): a server's settings are the project's
business, as its config files are, and folder settings apply only in trusted folders, where the
server would run anyway.

### Servers watch files

The client advertises `didChangeWatchedFiles` with dynamic registration, keeps the watchers each
server registers (globs, relative patterns, kinds), and passes on the file changes Flareite
watches for anyway, under the server's root. Changes are `Changed` for files that exist and
`Deleted` for those that don't: the watcher doesn't say which files are new, and the servers tried
treat both alike.

### Downloads in arguments

An argument may contain `${download:https://...}`: Flareite downloads the file into its cache
folder (`downloads/`, named by a hash of the address and its file name), puts its path there, and
gets it again in the background once it's a day old (as VS Code's luau-lsp does), so the next start
uses the new copy. The first start waits for the download; an argument whose file can't be had is
left out, so the server still starts. Only `https:` addresses, at most 64 MiB.

This adds no capability: an extension that may declare language servers can ship any program as
one. So no new permission, and no list of allowed hosts.

The schema `Fetcher` was considered and not reused: it keeps text in memory (8 MiB) and returns
it, and luau-lsp's docs file is 7.4 MB that only the server reads. The new `downloads` module
keeps files on disk and returns paths, with the same refresh and retry rules.

### The `language-servers` interface

Arguments depend on settings (the security level is in the address), so they can't be static.
`language-servers::set-arguments`, `set-settings`, and `restart` (API 1.16) let an extension's code
configure the servers it declares, over its manifest's, until it stops. Changes are applied by
difference: a server whose command line changed restarts; one whose settings alone changed gets
them; others are untouched. That also replaces the restart of every server when the `[lsp]`
settings change.

### Servers wait for their extension

Otherwise luau-lsp would start with the manifest's arguments, and restart a moment later when
`activate` set others. A server declared by an extension with code waits, if the extension
activates on startup or for one of the server's languages and is starting (or about to). The
wait ends when the extension is active, has failed, or after 6 seconds, whichever is first.
The manifest's arguments are the default settings' ones, so with the defaults `activate`
changes nothing and the server starts once.

### `on_configuration_changed`

A manifest command run when the user changes one of the extension's settings. Extensions read
settings when they need them; one that configures something else (a server, a process) needs to
hear when they change.

### API 1.16, still

API 1.16 (schemas) hasn't been released (`v0.1.0-alpha.3` has 1.15), so these join it, as links
joined 1.15 in [ADR 0039](0039-links-and-web-pages.md). Nothing in 1.16's existing types changed.

### The extension

`flareite.luau`, in its own repository (`D:\Projects\Ferrite\luau`, like the others, per
[ADR 0034](0034-official-extensions-in-their-own-repositories.md)). Its settings: platform
(`roblox` or `standard`), Roblox security level, sourcemaps on or off, the Rojo project file, and
whether sourcemaps include non-scripts. In a trusted folder with the project file, it runs
`rojo sourcemap --watch` (a declared program). It provides a JSON Schema for `.luaurc`.

### Files of another extension's language

`.luaurc` is JSON, and the JSON extension is no place for Luau's file names: the user wanted the
Luau extension to say so itself, without declaring JSON's grammar again. As in VS Code, where
extensions add `filenames` to a language another one contributes, a `[[languages]]` entry without
a grammar now adds files to the language of its id (API 1.16). It takes only `id`,
`file_extensions`, and `file_names`; `name` and `highlights` stay required with a grammar. The
files go to the first language of the id, the one the registry keeps. A separate key
(`[[file_associations]]`) was considered; reusing `[[languages]]` keeps one place where files
get languages, as VS Code does.

The grammar's Neovim queries assume the later pattern wins, as Neovim does, and don't color type
names after `:`; Flareite's earlier pattern wins, so they were reordered and extended. Function
attributes (`@native`) postdate the grammar and parse as errors.

### Bugs found on the way

- `@variable.builtin` (`self` in Rust, Python, and Luau) never showed: painted kinds were turned
  back into kinds by name, and the name `variable` maps to no kind, as plain variables aren't
  colored. Kinds now map back by number.
- Opening a file that starts an extension (one that activates for the file's language) could
  freeze the editor: `open_path` holds the editor lock while it tells the extension host, whose
  start of the extension sends `ExtensionsChanged`, whose handler (`quick_diff::track_open`)
  took the editor lock again. The handler's own comment says it may be called with the lock
  held. It now runs on a thread of its own, as does the new `RestartLanguageServer` handler.
  Found by the Luau extension, which activates on `.luau` files; extensions that activate on
  startup never hit it.

## Consequences

- Any language server can be given settings, by its extension or by users, as VS Code users are
  used to; servers that rely on file watching (luau-lsp, and others that reread configuration
  files) now see changes.
- A settings-only change no longer restarts servers.
- Downloads happen on a server's first start, which can take seconds; later starts use the kept
  copy. Offline, a server without its kept copy starts without that argument.
- An extension stopping takes back what its code set, which restarts its servers with the
  manifest's configuration.
