# ADR 0048: Brackets and quotes in pairs

- **Status:** Accepted
- **Date:** 2026-11-10

## Context

The user asked for auto-closing quotes and brackets that depend on the language and work as in
VS Code: typing an opener adds its closer, and typing the closer then moves over it instead of
adding another. VS Code's behavior is the reference: `editor.autoClosingBrackets`,
`autoClosingQuotes`, `autoClosingOvertype`, `autoClosingDelete`, and `autoSurround`, driven by
each language's `language-configuration.json` (`brackets`, `autoClosingPairs` with `notIn`,
`surroundingPairs`, `autoCloseBefore`).

Flareite has no languages of its own ([ADR 0014](0014-languages-only-from-extensions.md)), so
the pairs have to come from language extensions, as comment syntax does. Typing is applied in
the backend and the frontend shows the view it returns; there is no local prediction to keep in
step.

## Decisions

### The manifest takes VS Code's keys

`[[languages]]` entries take `brackets`, `auto_closing_pairs`, `surrounding_pairs`, and
`auto_close_before`: VS Code's keys, in snake case, with its defaults (the auto-closing pairs are
the brackets unless given, and the surrounding pairs are the auto-closing ones). An auto-closing
pair is `["(", ")"]` or `{ open, close, not_in = ["string", "comment"] }`. Language authors can
copy a VS Code language configuration across, and the official extensions do. Openers and closers
are 1 to 8 characters, at most 32 pairs a list. A language that declares none has no pairs, as in
VS Code; plain text has none.

These are manifest keys only, with no new functions, so the API version doesn't change for them.
(An older Flareite's loader rejects keys it doesn't know, so an extension that uses them needs a
Flareite that has them.)

### The core does the typing; the app says what's at each cursor

`flareite-core` gets `EditorOp::Type` (keyboard text, separate from `Insert`, which completions
and pastes use), `Enter`, and `Backspace`, each with a `PairContext`: the settings, and for each
selection its language's pairs and whether it's in a string or comment. The core stays free of
syntax; the app builds the context before each such operation, as it does fold regions, so
operations later in a batch see the cursors earlier ones left.

The language at a cursor is the innermost injected language there that declares pairs, else the
document's: JavaScript's in an HTML `<script>`, Rust's in a Markdown fence, and Markdown's in its
inline text, which declares none. Whether a cursor is in a string or comment comes from the
highlight query's `@string` and `@comment` captures (by `TokenKind`), the innermost containing it
in any layer there. VS Code asks its tokenizer what typing a neutral character would produce; a
Tree-sitter tree can't be asked that without parsing again, so an offset at the end of a string
counts as outside it, and at the end of a comment as inside only if it runs to the end of the
line. The tree is used as it is, edited to the current text but not parsed again, so building the
context doesn't add a parse to every keystroke.

An unterminated string is an `ERROR` node, not a string, in the grammars tried (Rust, Python,
JavaScript), so the tree can't say a quote would end it. The core counts instead: after an odd
number of the same unescaped quote on the line, a typed quote isn't closed.

### Only closers Flareite added are typed over

As VS Code's `auto` defaults do, typing a closer steps over it, and Backspace deletes it with its
opener, only if Flareite added it. The core keeps the pairs it added (`AutoClosed`) as byte ranges
of the view in use, which follow edits through the edit log and are dropped when an edit touches
the opener or closer, no cursor is between them any more, undo or redo runs, or another view
takes over. `always` settings skip the check. Stepping over a closer moves the cursors without
ending the undo group, so `f(x)` is still undone at once.

### Rules as VS Code's, all cursors or none

A pair closes, steps over, wraps, or deletes only if every cursor allows it; otherwise the
character is typed plainly at all of them. With `language_defined`, a pair closes before
whitespace, the end of the line, a closer that isn't also an opener, or an `auto_close_before`
character, and a one-character quote not right after a word character. Enter after an opener
indents the new line, and between an opener and its closer puts the closer on a line of its own.
Typing a closer on a line of whitespace doesn't outdent it yet (VS Code's electric characters),
and VS Code's `onEnterRules` (continuing doc comments) aren't taken.

## Consequences

- Each language extension declares its pairs; the official ones have VS Code's, and an extension
  without them types plainly until it adds them.
- Typing reads the syntax tree before each keystroke's operation (one query per cursor, over
  the captures around it), not only when building the view.
- The string and comment checks are as good as the highlight queries: a grammar that doesn't
  capture its strings as `@string` gets pairs closed inside them.
