# ADR 0032: Merge conflicts

- **Status:** Accepted; the merge editor became the Git extension's layout in [ADR 0038](0038-layouts.md)
- **Date:** 2026-11-04

## Context

The seventh stage of the Git extension resolves merge conflicts. The user chose to work like VS
Code: links above each conflict (Accept Current, Accept Incoming, Accept Both, Compare) with its
sides in color, as general code lenses any extension can show; VS Code's three-way merge editor
(incoming and current above the result); marking a file resolved by staging it, with Continue and
Abort while a merge, rebase, cherry-pick, or revert is in progress; and Rebase Branch beside
Merge Branch.

Extensions could decorate ranges of text, but not whole lines, nor show anything clickable in
the editor; and there was no way to show more than two documents in one tab.

## Decisions

### Code lenses and whole-line decorations (API 1.12)

`workbench::set-code-lenses(document, version, lenses)` replaces an extension's links in a
document, each on a line with a title, a tooltip, and one of the extension's commands with an
argument. The host keeps them as it keeps decorations: given at a version, mapped through later
edits (by their line's start), dropped with the extension. Views carry all of a document's
lenses, few as they are, so the editor can give each lensed line a one-row zone above it, the
same zones diff tabs use; a click runs the command with the document as its context.

Decoration types gain `whole_line`, so a conflict's sides color whole lines as VS Code's do.
Colors stay hex values in the manifest (VS Code's merge colors for Git's).

### Git finds conflicts in open documents

Git watches the documents of files its status calls conflicted (and those it already marked),
reading them again as they change; it marks each side and the marker lines, and puts its five
lenses on each conflict's first line. A lens's argument is that line: the command reads the
document again, finds the conflict there, and edits it (needing `documents_write`), so an edit
made in between can't make it resolve the wrong lines. Compare Changes diffs the file with every
conflict resolved to each side (`git:~current:` and `git:~incoming:`, worked out from the open
document).

### The merge editor is the workbench's (API 1.12)

`documents::open-merge` names a base, two versions, and a result file, so any extension can open
one; the comparing lives in `flareite_diff::merge`, next to the diff it builds on. Each version is
compared with the base; changes that overlap or touch make one range, a conflict if both sides
changed it differently (as Git counts them). Where each range is in the result comes from
comparing the result with the base, not from tracking edits, so the result can be edited by hand
and is understood again each time: its lines are compared with what accepting either side, both,
or neither gives, or else are "edited". Accepting replaces the range's lines in the result, if
it's still at the version shown. As in VS Code, the three editors line up (each range and each
change to the result elsewhere takes the height of its tallest side, plus a row of buttons) and
scroll together; the result starts from the base with the changes that don't conflict made when
the extension asks (`reset-result`, Git's choice when the file has conflict markers); and
Complete Merge saves the result and runs the extension's command (Git stages the file). Git's
versions are the index stages, `git:~1:` (base), `git:~2:` (ours, Current), and `git:~3:`
(theirs, Incoming); a missing one is empty.

Merge tabs aren't restored after a restart: restoring one would start its result again from the
file. A document becoming unsaved no longer brings its own tab to the front over a diff or merge
tab showing it; the dock shows the active document's tab only when it's opened or chosen.

### What's in progress

Git's pseudo-refs say what's in progress, and the extension can't read the Git folder as files,
so after each status it asks `git cat-file --batch-check` about `MERGE_HEAD`, `REBASE_HEAD`,
`CHERRY_PICK_HEAD`, and `REVERT_HEAD`. `REBASE_HEAD` outlives a rebase, so a rebase also needs
its folder (`rebase-merge` or `rebase-apply`), which `git -C <folder> version` finds or fails on.
While one is in progress, the Changes view says so and shows Continue and Abort (and Skip in a
rebase), the branch item adds "(Merging)" and the like, and a commit with an empty message takes
the one Git prepared. Continue refuses while files still have conflicts; it commits a merge
(`commit --no-edit`) or runs `--continue` with `core.editor=true`. Abort asks first. An operation
that stops for conflicts says so as a warning, not as a failure.

## Consequences

- Any extension can show code lenses, color whole lines, and open merge editors.
- Conflicts are found only in files Git calls conflicted, not in any file with markers.
- The merge editor has no base pane and no character-level marks yet; whole-line colors span the
  text's width, not the editor's.
- A rebase stopped without `REBASE_HEAD` (at a failed `exec`) isn't noticed.
