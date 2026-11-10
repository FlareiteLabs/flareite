# ADR 0043: Streamed output, folder activation, downloads, secrets, and the Rojo extension (API 1.17)

- **Status:** Accepted
- **Date:** 2026-11-10

## Context

The user asked for an official Rojo extension: everything needed for Roblox development, built on
Rojo, Rokit, and Wally. They chose:

- `rojo serve` shows its output in the extension's log and its state in the status bar as it
  runs;
- Rojo starts in folders with a Rojo project, before any `.luau` file is opened, since it keeps
  the sourcemap and shows the project's instances in a view and explorer marks;
- `*.project.json`, `*.meta.json`, and `*.model.json` are checked against schemas built from
  Roblox's current API, downloaded, rather than a list frozen into the extension;
- `rojo upload` takes an Open Cloud API key, which Flareite asks for once and keeps in the
  system's credential store.

None of these could be done with API 1.16:

- `processes::spawn` hands a program's output over only when it ends, and `rojo serve` (like
  `rojo sourcemap --watch` and every watcher) runs until it's stopped.
- Extensions started on startup, for a language, a command, or a view; never for a kind of
  project.
- Extensions' code has no network access. `schemas::set-schema` (1.16) accepts schemas made at
  runtime, but the data to make them from couldn't be had; `${download:...}` exists only in
  language servers' arguments.
- `storage` keeps values as plain JSON in Flareite's folder; `prompts::input` shows what's typed.

`v0.1.0-alpha.4` shipped API 1.16, so these are 1.17.

## Decisions

### Output as it's written: `spawn-streaming`

`processes::spawn-streaming(program, args, options, command, output)` starts a program as `spawn`
does, and runs `output`, another of the extension's commands, with the lines it writes;
`processes::lines()` returns them, each with its text and whether it went to standard error.

- **Lines, not bytes.** Every consumer so far (Rojo's log, its port, its errors) reads lines, and
  lines can be handed over as text without splitting a character. Line endings and a carriage
  return before them are removed; invalid UTF-8 is replaced. A line is cut at 16 KiB, and the rest
  of it skipped.
- **One event for what's waiting.** Lines for a program whose output command hasn't run yet join
  it, as file changes for a watch do. A program writing faster than the extension reads can't
  fill its queue: past 1 MiB of waiting lines, the oldest are dropped and counted in `dropped`.
- **Then the end.** Each reading thread hands over its last line before it ends, and the program's
  end is reported after both ended, so every line arrives before `command` runs. The outcome's
  `stdout` and `stderr` are then empty.
- **A new function**, rather than an option on `spawn`: `process-options` is a record, and adding
  a field to it would break every extension built against 1.6 to 1.16.

### `on-folder-contains:<pattern>`

A new activation event, VS Code's `workspaceContains`. The pattern is a glob relative to the open
folder (`*` stays within a name, `**` crosses folders), checked when the manifest loads (relative,
no `..`, valid).

The folder is searched when it opens, when the extension loads (or is rebuilt, or its permissions
change), and the paths of files created later are matched as they're reported. The search goes only
as deep as the patterns reach (Rojo's `*.project.json` reads one folder), or 8 levels for `**`,
and stops after 10,000 entries; it leaves out `files.exclude`, `.gitignore`d files, and `.git`.
It runs without the host locked: the open folder comes from the embedder, which locks the
workspace, and the workspace calls into the host.

### Downloads for code: the `downloads` interface

`downloads::download(address, command)` returns at once; the file's content arrives in `command`
(`downloads::downloaded()`). Flareite downloads through the same cache as `${download:...}`
arguments (`src-tauri/src/downloads.rs`), so a kept copy is handed over at once and refreshed in
the background once it's a day old, and a failing address isn't tried again for a few minutes.

- **The `network` permission**, which already let webview pages load from the web, now covers
  this too, described as "Get content from the web, for itself and its pages". A new permission
  would ask users the same question twice. Without it, `download` fails with `invalid-argument`,
  as `language-servers` does without its permission: `api-error::permission-denied` takes the
  API 1.0 `permission` enum, which can't name it, and changing that enum breaks every extension.
- **Only `https://`**, with a host and no user name; at most 32 MiB handed over (the cache keeps up
  to 64 MiB for language servers), and 8 downloads under way per extension.
- **Content, not a path.** Extensions can't read files outside the open folder, and the cache is
  outside it.

### Secrets: the `secrets` interface, and password prompts

`secrets::get`, `set`, and `keys` keep values in the system's credential store, through the
[`keyring`](https://crates.io/crates/keyring) crate 3.6: Windows Credential Manager, the macOS
Keychain, and the Secret Service on Linux over D-Bus in Rust (`async-secret-service` with
`crypto-rust`), so building needs no `libdbus` and CI no new packages.

- **Isolation.** Entries belong to the service named by the app's identifier
  (`dev.flareite.editor`), and their accounts are `<extension id>/<key>`; an extension names only
  keys. The keys an extension has set are listed (without values) in its storage folder, as
  `secrets.json`: a value in the store that isn't listed isn't the extension's, which keeps one
  extension from reading another's by guessing, and the list is what clearing uses, since stores
  can't list entries by prefix.
- **Clearing.** Clearing an extension's storage in the Extensions view removes its secrets too.
- **No permission.** Like `storage`, an extension's secrets are its own business.
- **Its own error type**, `secret-error`, with `unavailable` for a store that can't be reached
  (on Linux, no Secret Service running): `api-error` has no such case, and adding one breaks
  extensions.
- **Limits:** keys 1 to 200 bytes without control characters, values at most 2 KiB, 32 per
  extension.

`prompts::input-password(options, command)` asks for a line of text hidden as it's typed, as Git's
password prompts already did (`PromptKind::Input { password }`). A new function, since
`input-options` is a record.

### Opening a file with its app: `workspace::open-externally`

**Build and Open in Roblox Studio** needs the built place opened with the system's default app.
Extensions can't find Studio themselves (it's installed per user, under a versioned folder outside
the open folder), and declaring `cmd`, `open`, or `xdg-open` as a program would let an extension
run anything. `open-externally(path)` opens a file in the open folder with its default app, as
the explorer's own commands do (`open::that_detached`). It needs `processes` and a trusted
folder, since opening a file can run code as a program can; and it refuses programs, scripts,
installers, and shortcuts by their extension (and on Unix, executable files), which opening
would run, so it can't widen what `processes` allows.

### Completion follows simple conditions

Roblox's classes each have their own properties, picked by an instance's `$className`. JSON
Schema says that with `if` and `then`, but completion and hover expanded every branch, so a
part would have offered every class's properties. They now know the document: `Navigate` gained
`properties_in`, `choices_in`, and `info_in`, which take it (the tolerant tree the parser makes
of a document still being written, and its text), and a JSON Schema's `if` picks its branch when
it's of the simple kind used to pick one: `const`, `enum`, `type`, `required`, and `properties`
of those. Anything else, or a value not yet written out whole, takes both branches, as before.
Validation was already exact. TOML Schemas ignore the document.

A schema per class would be a per-class file, or `oneOf`s that completion can't tell apart. The
Rojo extension's schemas use one `if` per class in an `allOf` (955 classes, about 2 MiB each), and
a class's properties name its superclass's with `allOf`, so each property is written once.

### A view icon for instances

None of the activity bar icons suited a Roblox project, so `cube` joins them.

### The Rojo extension (its own repository)

Built on the above, in `FlareiteLabs/flareite-rojo-ext` (`D:\Projects\Ferrite\rojo`), as other
official extensions are ([ADR 0034](0034-official-extensions-in-their-own-repositories.md)):
serving with the status bar and a Projects view, the sourcemap and an Instances view, explorer
marks (SS, LS, and MS badges, two letters so as not to read as Git's), schemas made from
Roblox-Client-Tracker's API dump and luau-lsp's documentation, build (and open), upload, syncback,
format, new projects, installing Rojo and its Studio plugin, Rokit's tools, and Wally's packages
with `wally-package-types` after.

The user chose to move sourcemaps from the Luau extension ([ADR 0042](0042-configuring-language-servers.md))
to Rojo: the Luau extension no longer runs programs or reads files, and in Rojo projects luau-lsp
reads the `sourcemap.json` Rojo keeps.

## Consequences

- Watchers and servers (Rojo's serve and sourcemap, and others later) can run under an extension
  and report as they go.
- Project tools start in their projects without waiting for a file of a language.
- Extensions can build schemas and other data from files on the web, sharing Flareite's cache.
- API keys never sit in Flareite's files.
- Schemas that pick properties by a value complete like VS Code's do.
- Components built against 1.16 and earlier still link: nothing that existed changed.
