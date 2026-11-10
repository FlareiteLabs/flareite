use std::ops::Range;

use flareite_text::TextBuffer;
use streaming_iterator::StreamingIterator;
use tree_sitter::{QueryCursor, Tree};

use crate::languages::Grammar;
use crate::parse::SyntaxTree;

/// Highlight categories. The names (in this order) are part of the editor
/// protocol; the frontend maps them to theme colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TokenKind {
  Comment = 1,
  Keyword,
  String,
  Escape,
  Number,
  Constant,
  Function,
  Macro,
  Type,
  Variable,
  Property,
  Operator,
  Punctuation,
  Attribute,
  Tag,
  Label,
  Module,
}

impl TokenKind {
  /// Every kind, in order.
  const ALL: [Self; 17] = [
    Self::Comment,
    Self::Keyword,
    Self::String,
    Self::Escape,
    Self::Number,
    Self::Constant,
    Self::Function,
    Self::Macro,
    Self::Type,
    Self::Variable,
    Self::Property,
    Self::Operator,
    Self::Punctuation,
    Self::Attribute,
    Self::Tag,
    Self::Label,
    Self::Module,
  ];

  pub const NAMES: [&'static str; 17] = [
    "comment",
    "keyword",
    "string",
    "escape",
    "number",
    "constant",
    "function",
    "macro",
    "type",
    "variable",
    "property",
    "operator",
    "punctuation",
    "attribute",
    "tag",
    "label",
    "module",
  ];

  /// The kind for a highlight query capture name such as `function.method`.
  /// Plain variables are left uncolored.
  pub fn for_capture(name: &str) -> Option<Self> {
    let first = name.split('.').next().unwrap_or(name);
    Some(match (first, name) {
      (_, "string.escape" | "escape") => Self::Escape,
      (_, "function.macro") => Self::Macro,
      (_, "constant.numeric") => Self::Number,
      (_, "variable.builtin") => Self::Variable,
      ("comment", _) => Self::Comment,
      ("keyword" | "include" | "conditional" | "repeat" | "exception", _) => Self::Keyword,
      ("string" | "character", _) => Self::String,
      ("number" | "float", _) => Self::Number,
      ("constant" | "boolean", _) => Self::Constant,
      ("function" | "method" | "constructor", _) => Self::Function,
      ("macro", _) => Self::Macro,
      ("type", _) => Self::Type,
      ("property" | "field", _) => Self::Property,
      ("operator", _) => Self::Operator,
      ("punctuation", _) => Self::Punctuation,
      ("attribute", _) => Self::Attribute,
      ("tag", _) => Self::Tag,
      ("label", _) => Self::Label,
      ("module" | "namespace", _) => Self::Module,
      _ => return None,
    })
  }

  pub fn name(self) -> &'static str {
    Self::NAMES[self as usize - 1]
  }
}

/// A colored run within one line: byte columns `start..end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
  pub start: usize,
  pub end: usize,
  pub kind: TokenKind,
}

/// Highlights `lines` (each limited to its first `max_line_bytes`) using
/// `syntax`, which must match `buffer` (edited and, ideally, reparsed).
/// Injected languages are painted over the document's own. Returns one list
/// of spans per line.
pub fn highlight_lines(
  syntax: &SyntaxTree,
  buffer: &TextBuffer,
  lines: Range<usize>,
  max_line_bytes: usize,
) -> Vec<Vec<Span>> {
  let Ok(grammar) = syntax.language().grammar() else {
    return lines.map(|_| Vec::new()).collect();
  };
  let bounds: Vec<Range<usize>> = lines
    .clone()
    .map(|line| {
      let start = buffer.line_start(line);
      start..buffer.line_end(line).min(start + max_line_bytes)
    })
    .collect();
  let mut paints: Vec<Vec<u8>> = bounds.iter().map(|range| vec![0; range.len()]).collect();

  // Query runs of short lines together; long lines one at a time, cut short.
  let mut run_start = 0;
  while run_start < bounds.len() {
    let mut run_end = run_start + 1;
    let full = |index: usize| bounds[index].end == buffer.line_end(lines.start + index);
    if full(run_start) {
      while run_end < bounds.len() && full(run_end) {
        run_end += 1;
      }
    }
    let range = bounds[run_start].start..bounds[run_end - 1].end;
    paint_range(
      grammar,
      syntax.tree(),
      buffer,
      range.clone(),
      &bounds[run_start..run_end],
      &mut paints[run_start..run_end],
    );
    // Layers come outermost first, so nested injections paint last.
    for layer in &syntax.layers {
      if layer.span.start < range.end
        && range.start < layer.span.end
        && let Ok(grammar) = layer.language.grammar()
      {
        paint_range(
          grammar,
          &layer.tree,
          buffer,
          range.clone(),
          &bounds[run_start..run_end],
          &mut paints[run_start..run_end],
        );
      }
    }
    run_start = run_end;
  }

  paints.iter().map(|paint| runs(paint)).collect()
}

fn paint_range(
  grammar: &Grammar,
  tree: &Tree,
  buffer: &TextBuffer,
  range: Range<usize>,
  bounds: &[Range<usize>],
  paints: &mut [Vec<u8>],
) {
  let mut cursor = QueryCursor::new();
  cursor.set_byte_range(range.clone());
  let text = |node: tree_sitter::Node| buffer.chunks_in(node.byte_range()).map(str::as_bytes);
  let mut captures = cursor.captures(&grammar.query, tree.root_node(), text);
  // (start, end, kind, pattern) for every capture with a color.
  let mut found: Vec<(usize, usize, TokenKind, usize)> = Vec::new();
  while let Some((found_match, index)) = captures.next() {
    let capture = found_match.captures()[*index];
    if let Some(Some(kind)) = grammar.kinds.get(capture.index as usize) {
      let node = capture.node.byte_range();
      let (start, end) = (node.start.max(range.start), node.end.min(range.end));
      if start < end {
        found.push((start, end, *kind, found_match.pattern_index));
      }
    }
  }
  // Paint wide captures first so narrower (inner) ones win; for the same
  // node, earlier patterns win, as highlight queries are written to expect.
  found.sort_by(|a, b| (b.1 - b.0).cmp(&(a.1 - a.0)).then(b.3.cmp(&a.3)));
  for (start, end, kind, _) in found {
    for (bound, paint) in bounds.iter().zip(paints.iter_mut()) {
      let (from, to) = (start.max(bound.start), end.min(bound.end));
      if from < to {
        paint[from - bound.start..to - bound.start].fill(kind as u8);
      }
    }
  }
}

fn runs(paint: &[u8]) -> Vec<Span> {
  let mut spans = Vec::new();
  let mut start = 0;
  while start < paint.len() {
    let kind = paint[start];
    let mut end = start + 1;
    while end < paint.len() && paint[end] == kind {
      end += 1;
    }
    if let Some(kind) = kind_from_u8(kind) {
      spans.push(Span { start, end, kind });
    }
    start = end;
  }
  spans
}

fn kind_from_u8(value: u8) -> Option<TokenKind> {
  // Not by name: `for_capture("variable")` is none, as plain variables
  // aren't colored, but the kind (from `variable.builtin`) is.
  TokenKind::ALL
    .get(usize::from(value).checked_sub(1)?)
    .copied()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parse::parse;
  use crate::testing;

  fn parsed(language: &str, buffer: &TextBuffer) -> SyntaxTree {
    let registry = testing::registry();
    let language = registry.get(language).expect("an example");
    parse(language, &registry, buffer, None, || false)
      .expect("grammar")
      .expect("parses")
  }

  fn highlight(language: &str, text: &str) -> Vec<Vec<(String, &'static str)>> {
    let buffer = TextBuffer::from_text(text);
    let tree = parsed(language, &buffer);
    highlight_lines(&tree, &buffer, 0..buffer.line_count(), 1 << 20)
      .into_iter()
      .enumerate()
      .map(|(line, spans)| {
        let text = buffer.line_text(line);
        spans
          .into_iter()
          .map(|span| (text[span.start..span.end].to_owned(), span.kind.name()))
          .collect()
      })
      .collect()
  }

  fn has(spans: &[(String, &str)], text: &str, kind: &str) -> bool {
    spans.iter().any(|(t, k)| t == text && *k == kind)
  }

  #[test]
  fn highlights_rust_tokens() {
    let lines = highlight(
      "rust",
      "// greet\nfn main() {\n    let name = \"Flareite\\n\";\n    println!(\"{}\", 42);\n}\n",
    );
    assert!(has(&lines[0], "// greet", "comment"));
    assert!(has(&lines[1], "fn", "keyword"));
    assert!(has(&lines[1], "main", "function"));
    assert!(has(&lines[2], "let", "keyword"));
    assert!(
      lines[2]
        .iter()
        .any(|(t, k)| t.contains("Flareite") && *k == "string")
    );
    assert!(
      has(&lines[2], "\\n", "escape"),
      "inner escapes win over the string"
    );
    assert!(has(&lines[3], "println!", "macro") || has(&lines[3], "println", "macro"));
    assert!(has(&lines[3], "42", "constant") || has(&lines[3], "42", "number"));
  }

  #[test]
  fn multi_line_tokens_are_split_per_line() {
    let lines = highlight("rust", "/* one\ntwo */ fn x() {}\n");
    assert!(has(&lines[0], "/* one", "comment"));
    assert!(has(&lines[1], "two */", "comment"));
    assert!(has(&lines[1], "fn", "keyword"));
  }

  #[test]
  fn highlights_other_languages() {
    let ts = highlight("typescript", "interface A { b: number }\nconst c = `t`;\n");
    assert!(has(&ts[0], "interface", "keyword"));
    assert!(has(&ts[1], "const", "keyword"));
    let json = highlight("json", "{\"key\": true}");
    assert!(json[0].iter().any(|(t, _)| t.contains("key")));
    let toml = highlight("toml", "[package]\nname = \"x\"\n");
    assert!(toml[1].iter().any(|(_, k)| *k == "string"));
    let python = highlight("python", "def f(x):\n    return x\n");
    assert!(has(&python[0], "def", "keyword"));
  }

  #[test]
  fn highlights_luau_types_strings_and_calls() {
    let lines = highlight(
      "luau",
      concat!(
        "--!strict\n",
        "local Players = game:GetService(\"Players\")\n",
        "export type Point<T> = { x: number, y: T? }\n",
        "local s = `hi {name}!\\n`\n",
        "local function f(a: Vector3, ...: string): Foo\n",
        "  if a then continue end\n",
        "  return typeof(a) :: any, self, MAX_SIZE, math.abs(t.field)\n",
        "end\n",
      ),
    );
    assert!(has(&lines[0], "--!strict", "comment"));
    assert!(has(&lines[1], "local", "keyword"));
    assert!(has(&lines[1], "GetService", "function"));
    assert!(has(&lines[1], "\"Players\"", "string"));
    assert!(
      !lines[1].iter().any(|(t, _)| t == "Players"),
      "plain variable"
    );
    assert!(has(&lines[2], "export", "keyword"));
    assert!(has(&lines[2], "type", "keyword"));
    assert!(has(&lines[2], "Point", "type"));
    assert!(has(&lines[2], "<", "punctuation"));
    assert!(has(&lines[2], "x", "property"));
    assert!(has(&lines[2], "number", "type"));
    assert!(has(&lines[2], "T", "type"));
    // Interpolated expressions aren't string-colored; escapes are escapes.
    assert!(has(&lines[3], "`hi ", "string"));
    assert!(!lines[3].iter().any(|(t, _)| t.contains("name")));
    assert!(has(&lines[3], "{", "punctuation"));
    assert!(has(&lines[3], "\\n", "escape"));
    assert!(has(&lines[4], "f", "function"));
    assert!(has(&lines[4], "Vector3", "type"));
    assert!(has(&lines[4], "Foo", "type"));
    assert!(has(&lines[4], "...", "variable"));
    assert!(has(&lines[5], "continue", "keyword"));
    assert!(has(&lines[6], "typeof", "function"));
    assert!(has(&lines[6], "any", "type"));
    assert!(has(&lines[6], "self", "variable"));
    assert!(has(&lines[6], "MAX_SIZE", "constant"));
    assert!(has(&lines[6], "math", "module"));
    assert!(has(&lines[6], "abs", "function"));
    assert!(has(&lines[6], "field", "property"));
    assert!(has(&lines[7], "end", "keyword"));
  }

  #[test]
  fn highlights_markdown_with_inline_text_and_fenced_code() {
    let lines = highlight(
      "markdown",
      "# Title\n\nSome `code` and *words* in a [link](a.md).\n\n```rs\nfn main() {}\n```\n\n```typescript\nconst x = 1\n```\n",
    );
    assert!(has(&lines[0], "# Title", "keyword"));
    // Inline text comes from the injected markdown_inline language.
    assert!(has(&lines[2], "code", "string"));
    assert!(has(&lines[2], "words", "type"));
    assert!(has(&lines[2], "link", "function"));
    assert!(has(&lines[2], "a.md", "string"));
    // Fences name languages by file extension or by id.
    assert!(has(&lines[4], "```", "punctuation"));
    assert!(has(&lines[4], "rs", "label"));
    assert!(has(&lines[5], "fn", "keyword"));
    assert!(has(&lines[9], "const", "keyword"));
  }

  #[test]
  fn long_lines_are_highlighted_only_up_to_the_limit() {
    let line = format!("let s = \"{}\";", "x".repeat(100_000));
    let buffer = TextBuffer::from_text(&line);
    let tree = parsed("rust", &buffer);
    let spans = highlight_lines(&tree, &buffer, 0..1, 1000);
    assert!(spans[0].iter().all(|span| span.end <= 1000));
    assert!(spans[0].iter().any(|span| span.kind == TokenKind::String));
  }

  #[test]
  fn capture_names_map_to_kinds() {
    assert_eq!(
      TokenKind::for_capture("function.method"),
      Some(TokenKind::Function)
    );
    assert_eq!(
      TokenKind::for_capture("keyword.control.return"),
      Some(TokenKind::Keyword)
    );
    assert_eq!(
      TokenKind::for_capture("string.escape"),
      Some(TokenKind::Escape)
    );
    assert_eq!(TokenKind::for_capture("variable"), None);
    // Painted kinds come back as themselves, Variable included.
    for kind in TokenKind::ALL {
      assert_eq!(kind_from_u8(kind as u8), Some(kind));
      assert_eq!(kind.name(), TokenKind::NAMES[kind as usize - 1]);
    }
    assert_eq!(kind_from_u8(0), None);
    assert_eq!(TokenKind::Module.name(), "module");
  }
}
