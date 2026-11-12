//! What's at a position: the language there, and whether it's in a string or
//! a comment, for typing brackets and quotes in pairs.

use flareite_text::TextBuffer;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, QueryCursor, Tree};

use crate::highlight::TokenKind;
use crate::languages::{Grammar, Language};
use crate::parse::SyntaxTree;

/// The language at `offset`: the innermost injected language there that
/// `wanted` accepts, else the document's own. Layers are found by the bytes
/// they span.
pub fn language_at(
  syntax: &SyntaxTree,
  offset: usize,
  wanted: impl Fn(&Language) -> bool,
) -> &Language {
  syntax
    .layers
    .iter()
    .rev()
    .filter(|layer| layer.span.start <= offset && offset <= layer.span.end)
    .map(|layer| &layer.language)
    .find(|language| wanted(language))
    .unwrap_or(syntax.language())
}

/// Whether `offset` is in a string or a comment, as the highlight queries
/// of the document's language and of the languages injected there capture
/// them: [`TokenKind::String`], [`TokenKind::Comment`], or `None`. The
/// innermost such capture counts. An offset at the end of a string counts
/// as inside only if the string is unterminated, and one at the end of a
/// comment only if the comment runs to the end of its line.
pub fn scope_at(syntax: &SyntaxTree, buffer: &TextBuffer, offset: usize) -> Option<TokenKind> {
  let mut trees: Vec<(&Language, &Tree)> = vec![(syntax.language(), syntax.tree())];
  trees.extend(
    syntax
      .layers
      .iter()
      .filter(|layer| layer.span.start <= offset && offset <= layer.span.end)
      .map(|layer| (&layer.language, &layer.tree)),
  );
  let mut innermost: Option<(usize, TokenKind)> = None;
  for (language, tree) in trees {
    let Ok(grammar) = language.grammar() else {
      continue;
    };
    if let Some((len, kind)) = innermost_in(grammar, tree, buffer, offset)
      && innermost.is_none_or(|(shortest, _)| len < shortest)
    {
      innermost = Some((len, kind));
    }
  }
  innermost.map(|(_, kind)| kind)
}

/// The shortest string or comment captured around `offset` in one tree.
fn innermost_in(
  grammar: &Grammar,
  tree: &Tree,
  buffer: &TextBuffer,
  offset: usize,
) -> Option<(usize, TokenKind)> {
  let mut cursor = QueryCursor::new();
  cursor.set_byte_range(offset.saturating_sub(1)..(offset + 1).min(buffer.len()).max(offset));
  let text = |node: Node| buffer.chunks_in(node.byte_range()).map(str::as_bytes);
  let mut captures = cursor.captures(&grammar.query, tree.root_node(), text);
  let mut found: Option<(usize, TokenKind)> = None;
  while let Some((found_match, index)) = captures.next() {
    let capture = found_match.captures()[*index];
    let Some(Some(kind @ (TokenKind::String | TokenKind::Comment))) =
      grammar.kinds.get(capture.index as usize)
    else {
      continue;
    };
    let node = capture.node;
    if contains(node, *kind, buffer, offset) {
      let len = node.byte_range().len();
      if found.is_none_or(|(shortest, _)| len < shortest) {
        found = Some((len, *kind));
      }
    }
  }
  found
}

fn contains(node: Node, kind: TokenKind, buffer: &TextBuffer, offset: usize) -> bool {
  let range = node.byte_range();
  if range.start < offset && offset < range.end {
    return true;
  }
  if offset != range.end || range.is_empty() {
    return false;
  }
  match kind {
    // Typing the quote would end it.
    TokenKind::String => node.has_error(),
    // A line comment, which typing at its end continues.
    _ => {
      node.start_position().row == node.end_position().row
        && buffer.line_end(buffer.line_of(offset)) == offset
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parse::parse;
  use crate::testing;

  /// The scope at `|` in `marked`, parsed as `language`.
  fn scope(language: &str, marked: &str) -> Option<&'static str> {
    let offset = marked.find('|').expect("a cursor");
    let buffer = TextBuffer::from_text(&marked.replace('|', ""));
    let registry = testing::registry();
    let language = registry.get(language).expect("an example");
    let tree = parse(language, &registry, &buffer, None, || false)
      .expect("grammar")
      .expect("parses");
    scope_at(&tree, &buffer, offset).map(TokenKind::name)
  }

  #[test]
  fn finds_strings_and_comments() {
    assert_eq!(scope("rust", "let x = \"a|b\";"), Some("string"));
    assert_eq!(scope("rust", "let x = |\"ab\";"), None);
    assert_eq!(scope("rust", "let x = \"ab\"|;"), None);
    assert_eq!(scope("rust", "// note|\nfn f() {}"), Some("comment"));
    assert_eq!(scope("rust", "// no|te\nfn f() {}"), Some("comment"));
    assert_eq!(scope("rust", "fn f() {}\n|"), None);
    assert_eq!(scope("rust", "/* a */|"), Some("comment"));
    assert_eq!(scope("rust", "/* a */ |x"), None);
    assert_eq!(scope("python", "x = 'ab|'"), Some("string"));
    assert_eq!(scope("python", "x = f(|)"), None);
  }

  #[test]
  fn finds_strings_in_injected_languages() {
    assert_eq!(
      scope("html", "<script>let x = 'a|b';</script>"),
      Some("string")
    );
    assert_eq!(scope("html", "<script>let x = f(|);</script>"), None);
    assert_eq!(scope("html", "<a href=\"x|\">"), Some("string"));
  }

  #[test]
  fn finds_the_innermost_wanted_language() {
    let text = "<p></p><script>let x = 1;</script>";
    let buffer = TextBuffer::from_text(text);
    let registry = testing::registry();
    let html = registry.get("html").expect("an example");
    let tree = parse(html, &registry, &buffer, None, || false)
      .expect("grammar")
      .expect("parses");
    let script = text.find("let").expect("script");
    assert_eq!(language_at(&tree, script, |_| true).id(), "javascript");
    assert_eq!(
      language_at(&tree, script, |l| l.id() != "javascript").id(),
      "html"
    );
    assert_eq!(language_at(&tree, 1, |_| true).id(), "html");
  }
}
