# ADR 0049: Text inserted as the user types

- **Status:** Accepted
- **Date:** 2026-11-11

## Context

The user asked for HTML's closing tags to be typed for them, as VS Code does: typing `<div>`
adds `</div>` after the cursor. They asked for it in the HTML extension specifically, and
extensions stay self-contained: what one language needs is a host capability that extension
uses, not HTML knowledge in the editor. VS Code does it the same way: its HTML extension asks
its language service (`html/autoInsert`) after `>` or `/` is typed and inserts the answer as a
snippet.

Brackets and quotes ([ADR 0048](0048-brackets-and-quotes-in-pairs.md)) are declared in the
manifest and need no code. Tags can't be: whether `>` ends a start tag, whether the element is
void (`<br>`), and which element `</` ends depend on the document's text before the cursor.

## Decisions

### An extension API: auto-insert providers (1.19)

`[[auto_insert_providers]]` declare languages, trigger characters, and a command, as completion
providers do (API 1.18). When a trigger is typed, Flareite runs the command;
`language-features::inserting` says what it's asked (the document, version, cursor position just
after the typed character, the character, and one level of indentation as the editor inserts it),
and `set-insert(text, cursor)` answers with the text to insert at the cursor and where in it to
leave the cursor. This adds functions to the interface, so it is API 1.19. Providers need
`documents_read`, to read the text before the cursor.

Enter is a trigger too, as `"\n"`: the user then asked for Luau's `end` after `function f()` and
`if x then`, which Roblox Studio and luau-lsp add when Enter opens a block. The request comes after
Enter's own edit, with the position on the new line after the indentation Enter kept, and the
indentation unit lets the answer indent the line the cursor is left on.

The answer is a plain string with a cursor offset, not a snippet: closing tags need one cursor
position, and snippets with placeholders are a feature of their own.

### Asked after the edit, applied only if nothing moved

Asking during the typing operation would make every keystroke wait on an extension's thread.
Instead the frontend, once the view reflects the typed trigger, calls `editor_auto_insert`, which
asks the providers off the main thread (up to half a second; the first answer wins) and returns
the answer with the version and cursor it was asked for. The frontend sends it back as an
`autoInsert` operation through its queue, so it's ordered with the typing around it, and the core
applies it only if the document is still at that version with a single cursor at that place.
An answer that arrives after more typing is dropped, as VS Code drops a late `autoInsert`.

Only a single cursor is asked: the provider answers for one position, and closing tags at
several cursors would need several answers.

### Its own undo step

The inserted text is an undo step apart from the typing before it, as VS Code's snippet
insertion is: undo takes back `</div>` and leaves `<div>`, which is how to refuse it.

### HTML's tags in the HTML extension

The HTML & CSS extension reads the document up to the cursor and scans it as HTML is tokenized,
closely enough for this: comments, doctypes, quoted attribute values, raw text elements
(`<script>`, `<style>`, `<textarea>`, `<title>`), void elements, and self-closing tags. After `>`
that ends a start tag of an element with an end tag, it inserts `</name>` with the cursor before
it; after `</`, the innermost open element's name and `>`, with the cursor after it. Names keep
their case. As in VS Code, a closing tag is added even if one follows already. SVG, in the same
extension, is scanned as XML: names keep their case, nothing is void or raw text, and CDATA
sections are text. The XML extension, which had no code, gets its own copy of the XML scanner,
since extensions don't share code with each other.

### Luau's `end` in the Luau extension

After Enter, the Luau extension reads the document and answers with an indented line for the
cursor and `end` at the opener's indentation, if the line Enter was pressed on opens a block and
ends its header (`then`, `do`, or a function's parameters or return type), the document opens
more blocks than it ends (`function`, `if` statements, and `do` against `end`, past strings,
comments, and `if ... then ... else` expressions), and nothing indented follows yet. Closing
brackets after the cursor end up after `end`, as in `end)` for `pcall(function() ... end)`.
Counting the whole document is a heuristic, as Studio's is: an `end` missing elsewhere makes a
closed block look open, which the check for a body below mostly covers. `repeat` (closed by
`until`) isn't handled.

## Consequences

- The HTML & CSS extension asks for `documents_read`, so users allow it once more.
- A closing tag appears a moment after the `>`, once the extension answers, not in the same
  frame.
- Other languages (JSX, Vue, Ruby's `end`) can close tags or blocks, or insert anything else after
  a trigger, from their own extensions.
- The XML extension now has code, built like the others' and asking for `documents_read`; the
  Luau extension asks for `documents_read` too.
