# ADR 0057: Interactive programs, terminals, and reading diagnostics (API 2.1)

- **Status:** Accepted
- **Date:** 2026-11-21

## Context

Claude Code, Anthropic's coding agent, is a command-line program. Editors embed it the way the
Claude Agent SDK does: they start `claude --print --input-format stream-json --output-format
stream-json --permission-prompt-tool stdio` and talk to it in JSON lines for as long as the
conversation lasts. The user's messages go to its input, along with answers to its permission
requests ("may I edit `main.rs`?"), and its replies stream from its output. A Flareite extension
for it needs more of the extension API than version 2.0 had:

- **`spawn-streaming` closes the program's input** once the optional `stdin` is written, so
  nothing can be sent after the first message.
- **Streamed output is lossy by design**: lines are cut at 16 KiB, and when more than 1 MiB waits
  for the extension, the oldest lines are dropped. That suits a watcher's log. In a protocol
  where a file the agent read arrives as one JSON line, a cut or dropped line is a broken
  conversation.
- **Signing in is interactive** (`claude auth login` asks questions in a terminal), and so is
  using Claude Code by hand, but extensions can't start anything in the integrated terminal.
- **The editor's problems** (language servers', schemas', other extensions') can't be read, so
  "fix the problems in this file" isn't possible.

The obvious home for a chat is the side bar, as in VS Code, but side bar views are trees.
Adding a webview kind of view means a new case in `types::webview` and in `events::event`. While
designing this, we found that Wasmtime type-checks a component's imports and exports exactly
(`typecheck_variant` compares case counts): a 2.0 extension's `handle-event` export would no
longer match a host whose `event` has one more case, and it would fail to load. Wasmtime's
semantic-version matching links a 2.0 component to a 2.1 host only for items whose types haven't
changed.

## Decisions

### Minor versions only add

Version 2.1 adds functions, an interface, and a record, and changes no existing type. This is
now the rule for every 2.x (docs/extensions.md, Versioning): a type that would gain a field or a
case gets a new name instead, and new happenings come through existing events or new functions.
We confirmed that extensions built against 2.0 (Git, Rojo, Images, Luau) start in the 2.1 host.
The chat goes in an editor tab (a layout with a webview pane), which needs no new type; a side
bar webview waits for version 3.

### Interactive programs

- **`processes::spawn-interactive`** starts a declared program with its input left open. Its
  output comes as the existing `process-output` and `process-finished` events (no new event
  case), with different guarantees from `spawn-streaming`'s: **nothing is dropped**, and lines
  are cut only past 32 MiB. Instead of dropping, the line reader **waits**. After queueing lines
  it blocks while more than 1 MiB of that program's lines wait in the extension's queue, and the
  queue wakes it as events are taken (`Queue::push_output`, `wait_for_output`). The program then
  blocks on its full pipe, which is the backpressure a protocol wants.
- **`write-input`** never blocks the extension. Bytes go to a thread per program that writes
  them, with at most 16 MiB waiting. Otherwise an extension writing to a program that is itself
  waiting for its output to be read would deadlock with it.
- **`close-input`** ends the input once what's written is written.

### Terminals

**`terminals::run(program, args, options)`** checks the program exactly as `processes` does: it
must be declared, the `processes` permission granted, and the folder trusted. The embedder then
starts it in a new integrated terminal and shows it (`Embedder::run_in_terminal`; the app emits
`terminalOpened`, and the workbench attaches as it does to terminals after a reload). The
terminal is the user's: the extension gets no handle, output, or events, so a terminal is never
an unchecked way to read what a program prints.

### Reading diagnostics

**`host::diagnostics(document)`** returns the problems the editor shows: the embedder's (language
servers and schemas, `Embedder::diagnostics`) and every extension's, with their sources, in the
current text and sorted, at most 10,000. It needs `documents-read`, like reading the text.

### The Claude Code extension

`flareite.claude-code` (its own repository) is the first user. Each chat tab runs a `claude`
program, shows the conversation in its page, and keeps it so the page can load again. It shows
permission requests in the chat: proposed edits as diffs, with a side-by-side `edit` layout tab;
commands; plans; and questions. It sends the editor's context (file and selection) along when
asked, offers @-files and the program's slash commands, keeps past chats in the folder's storage
to resume, signs in with `terminals::run("claude", ["auth", "login"])`, and can use an API key
from `secrets`.

## Consequences

- **The event queue knows interactive output.** `process-output` events of interactive programs
  merge without trimming; those of streaming programs still trim. A slow extension slows its
  own programs, never the editor.
- **One thread more per interactive program** (the writer), on top of the two readers.
- **Version 2.x can't grow `event`.** Something needing a new kind of event in 2.x needs another
  route; version 3 can restructure (for example, open-ended event payloads).
- **The chat isn't in the side bar.** Users can drag the chat tab into any dock group, including
  beside the side bar.
