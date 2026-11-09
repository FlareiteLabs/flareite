# ADR 0028: Diff tabs and provided documents

- **Status:** Accepted; diff tabs became the Git extension's layout in [ADR 0038](0038-layouts.md)
- **Date:** 2026-11-03

## Context

The third stage of the Git extension shows changes the way VS Code does: clicking a changed file
in Source Control opens a tab comparing it with the index, a staged file compares the index with
`HEAD`, and a commit's file compares the commit with its parent. The versions that aren't files
on disk come from Git. Diffing could have lived in the extension, which would send the editor
finished decorations, but the view that lines two documents up, collapses unchanged lines, and
lets one side be edited is the editor's either way, and comparing texts is useful beyond Git
(comparing two files, an extension's formatter preview). The user preferred keeping the core
generic, with Git's knowledge in the extension.

## Decisions

### A diff crate in the core

`flareite-diff` compares two texts with imara-diff: lines with the histogram algorithm (and the
indent heuristic, so changes line up with code blocks), then, within each changed block of at
most 500 lines and 50,000 characters, words and punctuation with Myers. Columns are UTF-16, as
everywhere in the protocol. By default whitespace at the ends of lines doesn't count, as in VS
Code; the user can turn that off. Texts over 500,000 lines aren't compared. The app computes a
diff off the UI thread from the documents' current texts (`diff_compute`), so an edited side is
compared as it is, not as saved.

### Extensions provide read-only documents by address (API 1.8)

A manifest's `[[text_providers]]` declares schemes (`git`) and the command that provides a text.
When the editor needs the text at an address (`git:HEAD:src/main.rs`), the host runs that
command; `documents::requested()` names the address and `provide` answers it. The answer is tied
to the request (by id, through the command's payload), so two requests can't take each other's
answers, and a command that doesn't answer fails the request. This follows the earlier
callbacks: a new export would break components built against older versions.

Provided documents are core documents marked read-only: operations that change text do nothing,
`apply_edits` fails, and they are never dirty. `changed(uris)` (with `*` prefixes) makes the
editor ask again for open ones, replacing their text in place. The address's last segment names
the tab and picks the language, so a provided Rust file is highlighted. Language servers don't
see them (they have no file).

### Diff tabs name their sides

`open-diff(original, modified, title)` names each side by a file's absolute path or a provided
address, so a diff tab can be saved with the layout and restored, and the same diff shows in one
tab. The workbench opens each side's document _hidden_ if it isn't open: the backend keeps it but
doesn't list it, record it as an open file, or give it a tab. A document's own tab closing while
a diff shows it only hides it. The last diff tab showing a hidden document closes it, asking to
save changes made in the diff first. Each diff tab shows its documents in views of their own
(numbered from 2^30), whose folds aren't remembered with the file's.

### The diff view is two editors with zones

Rather than a new kind of editor, a diff tab is one or two `EditorView`s. Editors gained general
features the diff uses:

- **Zones**: room between rows, as the references peek already had, now any number. Filler lines
  level the sides after a change with fewer lines on one side; a bar stands for collapsed lines;
  inline, the removed lines show in zones above the lines that replaced them.
- **Line classes and marked ranges** color changed lines and characters.
- **`setFolds`** folds exactly the given regions; collapsed unchanged lines are folds in both
  sides' views, which have as many lines in each unchanged stretch, so rows stay level. Folding by
  the user is off in diff editors, so the folds stay the diff's.

The sides scroll together. Narrower than 900 pixels, a diff shows inline, as in VS Code. Side by
side or inline, ignoring whitespace, and collapsing unchanged lines are remembered with the
layout. F7 and Shift+F7 move between changes. Save and other editor commands act on the side in
use, through an override of the active editor while a diff tab's editor has focus.

### Git's addresses

`git:<revision>:<path>` is a file's version in a commit (`HEAD`, a hash, `<hash>^`) or, with no
revision, in the index; `git:~empty:<path>` is empty (Git forbids `~` in names). The extension
reads them with `git cat-file blob`, announces `git::*` and `git:HEAD:*` changed when `.git`
changes, and asks for `documents_read` to find the active document's file for **Open Changes**.

## Consequences

- Any extension can show versions of files and open diffs, without the workbench knowing why.
- Hidden documents are a new state the workbench and backend keep in step; `document_list`,
  `ui_state` open files, closing, and saving each account for it.
- A selection or search that reaches into collapsed lines unfolds them in the backend without the
  diff knowing, until it next lays out; the rows can be off by the hidden lines until then.
- Gutter markers for changes in ordinary editors, staging hunks, and conflicts (stages 4 and 7)
  can build on `flareite-diff` and provided documents.
