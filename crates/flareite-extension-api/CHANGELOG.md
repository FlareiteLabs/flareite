# Extension API changelog

The versions of the `flareite:extension` WIT package, and the `api_version` manifests declare.
Minor versions add to a major one; components built against an older minor version link. A new
major version is a new `api_version`, and Flareite loads one. See `docs/extensions.md`
(Versioning).

## 2.2.0

[ADR 0058](../../docs/adr/0058-editable-viewers-file-problems-and-language-indentation.md). Adds
only: extensions built against 2.0 and 2.1 load unchanged.

- **Editable viewers** (`editable = true` in a `[[viewers]]` entry): `viewers::edit-viewed-file`,
  `viewed-file-status`, `undo-viewed-file`, and `redo-viewed-file`, with the `byte-change`,
  `viewed-file-state`, and `viewed-file-step` records. Flareite keeps the changes until the user
  saves or reverts them; `read-viewed-file` reads them.
- **Problems in files by path:** `host::publish-file-diagnostics` and `clear-file-diagnostics`,
  for files that may not be open, such as a build's errors.
- (Manifest only, no API change: `indentation = "tabs"` or `"spaces"` in `[[languages]]`.)

## 2.1.0

[ADR 0057](../../docs/adr/0057-interactive-programs-and-terminals.md). Adds only: extensions built
against 2.0 load unchanged.

- **Interactive programs:** `processes::spawn-interactive`, `write-input`, and `close-input`. The
  program's input stays open; its output comes as `process-output` events without dropping
  lines (cut past 32 MiB), Flareite waiting to read more while the extension is behind.
- **The `terminals` interface:** `run` starts a declared program in the integrated terminal.
- **`host::diagnostics`** reads the problems the editor shows in a document, from every source
  (the `reported-diagnostic` record).

## 2.0.0 (`api_version = "2"`)

[ADR 0051](../../docs/adr/0051-extension-api-2.md). Not compatible with 1.x.

- **Events.** A new export, `handle-event(event)`, with the `events` interface's variant:
  document opened, changed, and closed (formerly the `document-opened`, `document-changed`, and
  `document-closed` exports); `folder-changed`, `active-file-changed`, `selection-changed`, and
  `configuration-changed` (for those the manifest's `events` lists); `process-finished`,
  `process-output`, `files-changed`, `download-finished`, `timer-fired`, and `prompt-answered`;
  `viewer-opened`, `webview-message`, `pane-message`, `pane-navigated`, `layout-fill`, and
  `view-item-expanding`.
- **Providers** are exports that return their answers: `provide-completions`, `provide-hover`,
  `provide-auto-insert`, `provide-text`, `provide-original`, `change-original`, and `open-link`;
  and new, `provide-formatting` and `provide-code-actions` (with `[[formatting_providers]]` and
  `[[code_action_providers]]` in the manifest).
- **Removed getters and setters:** `language-features::completing`, `hovering`, `inserting`,
  `set-completions`, `set-hover`, and `set-insert`; `processes::finished` and `lines`;
  `prompts::answer`; `timers::fired`; `downloads::downloaded`; `workspace::changes`;
  `workbench::command-argument`; `views::selection` and `expanding`; `webviews::message`;
  `layouts::filling`, `invoked`, `message`, and `navigated`; `links::opening` and
  `open-default`; `documents::requested`, `provide`, `open-diff`, and `open-merge`;
  `quick-diff::requested-file`, `set-original`, and `to-apply`.
- **No callback commands:** `processes::spawn` and `spawn-streaming`, `workspace::watch`,
  `timers::set-timer`, `downloads::download`, and the prompts take no command; the id they
  return comes back in the event.
- **`command-context`** has the command's `argument`, the view items `selection`, and
  `invoked-from` (the layout tab and pane a button or zone was clicked in).
- **`permission`** names all eight permissions.
- **One file per interface** in `wit/`, with the world in `world.wit`.
- **Manifest:** `api_version = "2"`; `events = [...]` replaces `on_folder_changed`,
  `on_active_file_changed`, `on_selection_changed`, and `on_configuration_changed`; the
  callback `command` keys of providers, viewers, layouts, link handlers, and text providers go,
  as do layouts' `on_message` and `on_navigate`, webviews' `on_message`, views' `on_expand`, and
  `[quick_diff]`'s `original` and `apply` (`apply_title` alone says changes can be applied).

## 1.x (`api_version = "1"`)

- **1.19** `language-features::inserting` and `set-insert` (`[[auto_insert_providers]]`).
- **1.18** The `language-features` interface (completion and hover providers);
  `schemas::set-document`, `clear-document`, `properties`, `values`, and `describe`.
- **1.17** The `downloads` and `secrets` interfaces; `processes::spawn-streaming`;
  `prompts::input-password`; `workspace::open-externally`; `on-folder-contains` activation.
- **1.16** The `schemas` and `language-servers` interfaces; language servers' `settings` and
  `${download:...}` arguments; `on_configuration_changed`.
- **1.15** The `layouts` and `links` interfaces; `documents::save`; the `web_pages` permission;
  theme tokens as decoration colors. `documents::open-diff` and `open-merge` failed from here on.
- **1.14** `workbench::render-markdown`.
- **1.13** `workbench::set-line-annotations`; `views::set-visible`; `on_selection_changed`.
- **1.12** `workbench::set-code-lenses`; `documents::open-merge`; whole-line decorations.
- **1.11** `views::set-graph`; `prompts::pick-folder`; `workspace::open-folder`;
  `workbench::active-file`.
- **1.10** The `timers` interface.
- **1.9** The `quick-diff` interface.
- **1.8** The `documents` interface (provided documents and diffs).
- **1.7** The `views` and `prompts` interfaces; `files::delete`; `workspace::open-file`;
  `host::set-configuration`.
- **1.6** The `workspace`, `processes`, and `explorer` interfaces.
- **1.5** The `webviews` and `storage` interfaces.
- **1.4** The `viewers` interface.
- **1.3** The `files` interface.
- **1.2** Previews.
- **1.1** The `workbench` interface.
- **1.0** Documents, edits, diagnostics, commands, settings, and logging.
