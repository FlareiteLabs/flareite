# ADR 0051: Extension API 2.0

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

The extension API grew from 1.0 to 1.19 by adding interfaces, and every feature that needed the
extension to react or answer used the same mechanism: the host ran one of the extension's
commands, and the command asked the host what it was run for. A completion provider named a
command in the manifest; while it ran, `language-features::completing()` returned the request,
and `set-completions` answered it, once, inside the command. Programs, prompts, timers,
downloads, watches, webview messages, layouts, link handlers, provided documents, quick diff, and
the folder, active file, selection, and settings changing all worked this way: about 20 getters,
each with its setter or manifest key.

The consequences, found by a review on 2026-11-11:

- **Commands that aren't commands.** The Git extension declared 103 commands, 46 of them hidden
  callbacks (`jobFinished`, `filesChanged`, `answered`, `folderChanged`, `diffLayout`), and its
  `run_command` was a long string match mixing what the user ran with what the host reported.
- **Rules enforced at run time.** Calling `set-completions` outside a completion command, or
  twice, failed with `invalid-argument`; nothing in the types said so.
- **Every feature added a pair.** Each new callback meant another getter, another setter or
  manifest key, and another kind of hidden command.
- **`permission` couldn't name most permissions.** The WIT enum had `documents-read` and
  `documents-write`; the host had eight, so `activation.granted` and `permission-denied` couldn't
  report the other six.
- **A 2,043-line `extension.wit`**, with its history in a doc comment.
- **No SDK.** Each extension kept its state in a `static Mutex<Option<State>>` and implemented
  `Guest`'s raw exports, including three document notifications most didn't use.

The user asked for the overhaul as a clean break: every official extension is ported and bumped
to its first major version, 1.0.0, since none works with version 1 any more.

## Decisions

### Events, providers, and commands

The world exports:

- `activate` and `deactivate`;
- `run-command(command, context)`, for the extension's commands only: what the user runs, and
  what its buttons, menus, view items, code lenses, and zones run;
- `handle-event(event)`, for everything that happens: a variant (`events.wit`) of document
  opened, changed, and closed (formerly exports of their own); the folder, active file,
  selection, and settings changing; a program finishing or writing output, a watch's changes, a
  download, a timer, a prompt's answer; a viewer opening, a webview's or pane's message, a web
  page pane navigating, a layout tab needing its layout, and a tree item opening;
- providers that return their answers: `provide-completions`, `provide-hover`,
  `provide-auto-insert`, `provide-formatting`, `provide-code-actions`, `provide-text(uri)`,
  `provide-original(path)`, `change-original(applied)`, and `open-link(request) -> bool`.

What a request needs answered now is a typed return value. What can be answered later stays a
host function the extension calls when it's ready: `layouts::set-layout` after `layout-fill`,
`views::set-children` after `view-item-expanding`, `viewers::set-viewer-content` after
`viewer-opened`.

Functions that started something (`spawn`, `spawn-streaming`, `watch`, `set-timer`, `download`,
the prompts) lose their callback command: they return an id, and the event carries it. All the
getters go, and so do the manifest keys naming callbacks (`on_folder_changed`,
`on_active_file_changed`, `on_selection_changed`, `on_configuration_changed`, `on_expand`,
`on_message`, `on_navigate`, the providers', viewers', layouts', link handlers', and text
providers' `command`, and `[quick_diff]`'s `original` and `apply`). The four workbench events go
only to extensions whose manifest lists them in `events = [...]`, as before only extensions that
named a command got them. `command-context` carries what the getters returned: the `argument`,
the view `selection`, and the layout tab and pane a button was clicked in (`invoked-from`).

`selection-changed` has an optional document: version 1 told extensions when no document was in
use, and Git's blame clears on it.

### Formatting and code actions from extensions

`[[formatting_providers]]` and `[[code_action_providers]]` let an extension format, and offer
code actions for, languages without a language server that does. The app asks a language server
first and an extension otherwise, and lists both sources' code actions together. An extension's
action is edits plus an optional command of its own, run with the action's argument.

### Permissions

`permission` has all eight: `documents-read`, `documents-write`, `files-read`, `files-write`,
`language-servers`, `network`, `processes`, and `web-pages`. A download without `network` fails
with `permission-denied(network)`, not `invalid-argument`.

### One file per interface

`wit/` has a file per interface and `world.wit`; the package is `flareite:extension@2.0.0`. The
version history moves to `crates/flareite-extension-api/CHANGELOG.md`. Removed with it:
`documents::open-diff` and `open-merge`, which have failed since 1.15, and
`links::open-default` (`open-link` returns `false` instead).

### An SDK in the Rust crate

`flareite-extension-api` adds an `Extension` trait: `activate` returns the extension's state,
and every other method takes `&mut self` and has a default (events ignored, providers answering
with nothing). `register!(Type)` keeps the state in a thread-local and exports the world. Calls
never overlap, so there are no locks. `flareite-extension-schemas`' `Documents` takes events with
`handle_event` and answers `completions` and `hover`.

### The host

The worker queue has three kinds of entry: a command (with its context), a notice (an event,
optionally with a reply for those the caller waits on, such as a viewer's), and a call (a
provider's request, answered by a typed `Answer`). `ExtensionHost::call` returns a `Ticket` that
the caller waits on, with a deadline, or chains (`then`), as link handling does to fall back to
Flareite's own way. Background events merge as changes did: a watch's changes and a program's
output lines.

Only API 2 loads. A manifest's `api_version` is read before the rest is parsed, so a version 1
manifest is refused with "was built for extension API version 1; this version of Flareite
supports 2. Update the extension", not with its first unknown field.

## Consequences

- Git declares 89 commands, 32 hidden, all of them actions behind its views' menus and layouts'
  buttons; its callbacks are `handle_event`'s match arms and four provider methods. Its diff and
  merge views take the command context instead of asking for it.
- Every official extension is 1.0.0 and needs Flareite with API 2; their releases from before
  can't be installed in it, and theirs can't in older Flareite.
- New callbacks are a variant case or an export, not a getter, a setter, and a manifest key.
- Extensions written in other languages export more functions, but each is typed.
- The 1.x compatibility path (loading both worlds) was not built: Flareite is pre-1.0 and every
  extension is official.
