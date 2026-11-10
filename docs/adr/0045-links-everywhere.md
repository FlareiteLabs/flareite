# ADR 0045: Links wherever text is shown

- **Status:** Accepted
- **Date:** 2026-11-10

## Context

[ADR 0039](0039-links-and-web-pages.md) made web addresses and file paths in the terminal's output
open with Ctrl+click, but elsewhere only web addresses in the editor's text did. The user asked
for the same links across the app: the editor, the Output view, and the rest.

## Decisions

### One way to find links

Finding links moved from the terminal into `src/state/linkFinder.ts`: the same patterns (web
addresses, `file:` addresses, and paths with `:line:column` or `(line,column)`), and the same rule
that a path is a link only if it leads to a file or folder. The backend is asked about paths
(`path_kinds`) in batches: every caller asking in the same moment, as a view showing many lines
does, shares one call (200 paths at a time), and answers are trusted for ten seconds.

Where relative paths are looked for depends on the text: the shell's folder in the terminal (as
before), the document's folder and then the open folder in the editor, and the open folder
elsewhere.

### Where links are

- **The editor's text.** A path, like a web address, is a link while Ctrl is held over it, once
  the backend says it leads somewhere, and wins over Go to Definition there (an import's path
  opens the file either way). Hovering with Ctrl asks; a Ctrl+click asks again if it hasn't
  answered yet, and goes to the definition if the path leads nowhere. Paths are links in every
  document, with or without a language server.
- **The Output view, notifications, and hover cards' diagnostics** show text through
  `LinkedText`: web addresses are links at once, paths when the backend answers. They're drawn
  with a faint dotted underline, so they can be found without holding Ctrl, and open with
  Ctrl+click (a plain click still selects text), or Ctrl+Shift+click to choose. Language servers'
  and extensions' logs are in the Output view, so links in a build's or a server's output open
  there too.
- **Hover cards' Markdown links** to web and `file:` addresses (a `#L12` suffix names a line)
  open with a plain click, as links in previews do: they look like links. Other links (such as
  rust-analyzer's links to symbols) are still shown as their text.

Every link opens through `state/links.ts`, so extensions' `[[link_handlers]]` and the
`workbench.open_*_links` settings apply everywhere, with no change to the API.

### Not linked

The Problems view's rows are buttons that open the problem's place already. Extensions' views
and webviews show their own content; a webview can open a web address with
`links::open-in-browser`, or through its extension.

## Consequences

- Paths in compiler output, logs, and error messages open from wherever they're shown.
- Holding Ctrl over the editor's text asks the backend about the path-like word under the pointer
  (`a.b` included), at most once per path every ten seconds.
- The Output view asks about every path it shows, in batches, when lines arrive.
