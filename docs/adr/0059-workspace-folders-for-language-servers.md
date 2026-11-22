# ADR 0059: Workspace folders for language servers (API 2.3)

- **Status:** Accepted
- **Date:** 2026-11-22

## Context

A language server runs per root: the open folder for files inside it, or the nearest folder with
a root marker for files outside it (`flareite_lsp::find_root`). It got that one folder as its
only workspace folder.

That loses projects a server only finds from their own folder. rust-analyzer loads the Cargo
workspace of the open folder (or, with no `Cargo.toml` there, the packages one level down). A
package the workspace `exclude`s is a workspace of its own, which rust-analyzer never loads. The
user's operating system project is like this: its root workspace has `members = ["xtask"]` and
`exclude = ["kernel"]`, since the kernel builds for `x86_64-unknown-none` from its own folder.
In the kernel's files, Go to Definition, hover, and completion found nothing. Run directly
against rust-analyzer 1.99, the same request answered once `kernel/` was a second workspace
folder (at `initialize`, or added later with `workspace/didChangeWorkspaceFolders`), or once
`rust-analyzer.linkedProjects` listed both manifests.

Which projects a server misses is the language's business: Cargo's rules for `members`,
`exclude`, and `package.workspace` decide it for Rust. So the language's extension should decide,
and the host should give it a way to tell the server.

## Decisions

### `language-servers::set-folders`

**`set-folders(server, folders)`** replaces the workspace folders one of the extension's servers
gets besides its root. Like `set-arguments` and `set-settings`, it needs `language_servers`,
works on servers the manifest declares, and lasts until the extension stops.

- **Folders are inside the open folder.** Each must be an absolute path to an existing folder
  in it, at most 64; they're resolved as the open folder is (`normalize_path`). Trust is per
  open folder, and a server runs the project's code, so an extension can't point one elsewhere.
- **An instance gets the folders inside its root** (`flareite_lsp::folders_in`). The instance
  for the open folder gets them all; one for a file outside it (such as the standard library's
  sources after Go to Definition) gets none, and keeps finding its own project.
- **Running instances are told.** Flareite now says it supports workspace folders
  (`workspace.workspaceFolders`), answers `workspace/workspaceFolders` with the instance's
  folders (it used to answer null), and sends `workspace/didChangeWorkspaceFolders` with what was
  added and removed. A server that doesn't announce `changeNotifications` and hasn't registered
  for them restarts with the new folders. Extensions declaring the same server share it, and
  their folders are merged.
- It's a WIT addition, so the package is 2.3.0; extensions built against 2.0 to 2.2 load
  unchanged.

### Rust finds the workspaces with Cargo

The Rust extension gains code. For each Rust file opened in the folder it runs
`cargo locate-project --workspace --message-format plain` in the file's folder (once per
folder, about 0.1 s), which names the root manifest of the file's workspace exactly as Cargo
would build it. It does the same in the open folder for the workspace rust-analyzer loads there.
A workspace rust-analyzer loads anyway (the folder's own, or, with none, one in a direct
subfolder) is left alone; any other inside the folder becomes a workspace folder. Files open
when the extension activates are looked at before it returns, since rust-analyzer waits for it,
so the server starts with their workspaces. Later files are looked at in the background.

The extension now asks for `documents_read` and `processes` (to run `cargo`) as well.

## Alternatives considered

- **`rust-analyzer.linkedProjects` through `set-settings`.** No API change, but settings apply
  to every instance of a server by name. Once set, an instance rooted outside the open folder
  would load the open folder's projects instead of its own, and relative paths don't help: a
  linked manifest that doesn't exist fails the whole load. linkedProjects also turns off
  rust-analyzer's own discovery, so the extension would have to reproduce it.
- **The host adding a folder for each opened file's nearest root marker.** Generic, but wrong
  for most files: in a Cargo workspace every member has a `Cargo.toml`, and each would become a
  folder that rust-analyzer loads and reloads to find the same workspace. Only the language knows
  which projects are separate.
- **Parsing `Cargo.toml` files in the extension.** Avoids running a program, but means
  reimplementing Cargo's member globs, path dependencies, and `package.workspace`. Cargo answers
  exactly, quickly, and is installed wherever rust-analyzer is useful.
