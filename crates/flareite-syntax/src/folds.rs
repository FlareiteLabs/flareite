//! Folding: the regions a language's fold query (`folds.scm`) captures as
//! `@fold`, such as blocks, bodies, and long argument lists.
//!
//! A region is the lines of the captured node: the first stays visible when
//! the region is folded and the rest are hidden, except a last line that
//! closes the first, such as `}` under `fn main() {` or `</div>` under
//! `<div>`. That line stays visible too, so folded code reads
//! `fn main() {⋯` followed by `}`.

use std::ops::Range;

use flareite_text::TextBuffer;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Point, Query, QueryCursor, Tree};

use crate::parse::SyntaxTree;

/// The foldable regions of `syntax` that touch `lines`, or `None` if its
/// language has no fold query. Each region is lines `start..end`: `start`
/// stays visible and `start + 1..end` are hidden when it is folded. Regions
/// are sorted by their first line, one per line (the largest); injected
/// languages' regions are included.
pub fn fold_ranges(
  syntax: &SyntaxTree,
  buffer: &TextBuffer,
  lines: Range<usize>,
) -> Option<Vec<Range<usize>>> {
  let query = syntax.language().grammar().ok()?.folds.as_ref()?;
  let line_count = buffer.line_count();
  let lines = lines.start.min(line_count)..lines.end.min(line_count);
  let mut found = Vec::new();
  if lines.is_empty() {
    return Some(found);
  }
  collect(query, syntax.tree(), buffer, lines.clone(), &mut found);
  let bytes = buffer.line_start(lines.start)..buffer.line_end(lines.end - 1);
  for layer in &syntax.layers {
    if layer.span.start <= bytes.end
      && bytes.start <= layer.span.end
      && let Some(query) = layer
        .language
        .grammar()
        .ok()
        .and_then(|grammar| grammar.folds.as_ref())
    {
      collect(query, &layer.tree, buffer, lines.clone(), &mut found);
    }
  }
  found.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
  found.dedup_by_key(|range| range.start);
  Some(found)
}

/// Adds the regions `query` captures in `tree` that touch `lines`.
fn collect(
  query: &Query,
  tree: &Tree,
  buffer: &TextBuffer,
  lines: Range<usize>,
  found: &mut Vec<Range<usize>>,
) {
  let Some(fold) = query.capture_index_for_name("fold") else {
    return;
  };
  let mut cursor = QueryCursor::new();
  cursor.set_point_range(Point::new(lines.start, 0)..Point::new(lines.end, 0));
  let text = |node: Node| buffer.chunks_in(node.byte_range()).map(str::as_bytes);
  let mut matches = cursor.matches(query, tree.root_node(), text);
  while let Some(found_match) = matches.next() {
    for capture in found_match.captures() {
      if capture.index == fold
        && let Some(region) = region(buffer, capture.node)
      {
        found.push(region);
      }
    }
  }
}

/// The region of a captured node, if it hides at least one line.
fn region(buffer: &TextBuffer, node: Node) -> Option<Range<usize>> {
  let first = node.start_position().row;
  let end = node.end_position();
  // A node ending at the start of a line ends on the line before.
  let last = if end.column == 0 {
    end.row.checked_sub(1)?
  } else {
    end.row
  };
  let mut last = last.min(buffer.line_count().checked_sub(1)?);
  // Blank lines after the node (a TOML table's) aren't part of it.
  while last > first && leading(buffer, last).1.next().is_none() {
    last -= 1;
  }
  if last <= first {
    return None;
  }
  let end = if closes(buffer, first, last) {
    last
  } else {
    last + 1
  };
  (end > first + 1).then_some(first..end)
}

/// Whether line `last` closes what line `first` opens: `first` ends with an
/// opening bracket (or a tag's `>`) and `last` starts with a closing one (or
/// `</`), indented no deeper.
fn closes(buffer: &TextBuffer, first: usize, last: usize) -> bool {
  let (indent, mut start) = leading(buffer, last);
  let opener = last_char(buffer, first);
  let closing = match start.next() {
    Some('}' | ')' | ']') => matches!(opener, Some('{' | '(' | '[')),
    Some('<') => start.next() == Some('/') && opener == Some('>'),
    _ => false,
  };
  closing && indent <= leading(buffer, first).0
}

/// The width of a line's indentation, and its characters after that.
fn leading(buffer: &TextBuffer, line: usize) -> (usize, impl Iterator<Item = char> + '_) {
  let mut chars = buffer.chars_at(buffer.line_start(line)).peekable();
  let mut indent = 0;
  while chars.next_if(|&c| c == ' ' || c == '\t').is_some() {
    indent += 1;
  }
  (indent, chars.take_while(|&c| c != '\n' && c != '\r'))
}

/// A line's last character that isn't whitespace.
fn last_char(buffer: &TextBuffer, line: usize) -> Option<char> {
  let start = buffer.line_start(line);
  let mut chars = buffer.chars_at(buffer.line_end(line));
  let mut offset = buffer.line_end(line);
  while offset > start {
    let c = chars.prev()?;
    offset -= c.len_utf8();
    if !c.is_whitespace() {
      return Some(c);
    }
  }
  None
}

#[cfg(test)]
// Line ranges are naturally single-element arrays here.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
  use super::*;
  use crate::parse::parse;
  use crate::testing;

  /// The regions of `text` as `(first line, last hidden line)`, one-based
  /// like an editor's line numbers.
  fn folds(language: &str, text: &str) -> Vec<(usize, usize)> {
    let registry = testing::registry();
    let buffer = TextBuffer::from_text(text);
    let tree = parse(
      registry.get(language).expect("an example"),
      &registry,
      &buffer,
      None,
      || false,
    )
    .expect("grammar")
    .expect("parses");
    fold_ranges(&tree, &buffer, 0..buffer.line_count())
      .expect("a fold query")
      .into_iter()
      .map(|range| (range.start + 1, range.end))
      .collect()
  }

  #[test]
  fn closing_lines_stay_visible() {
    let rust = "fn main() {\n    let v = vec![\n        1,\n    ];\n    if v.is_empty() {\n        return;\n    } else {\n        go();\n    }\n}\n";
    assert_eq!(folds("rust", rust), [(1, 9), (2, 3), (5, 6), (7, 8)]);
  }

  #[test]
  fn regions_without_a_closing_line_hide_their_last_line() {
    let python = "class A:\n    def f(self):\n        return 1\n\n    def g(self):\n        pass\n";
    assert_eq!(folds("python", python), [(1, 6), (2, 3), (5, 6)]);
    let toml = "[a]\nx = 1\ny = 2\n\n[b]\nz = 3\n";
    assert_eq!(folds("toml", toml), [(1, 3), (5, 6)]);
    let yaml = "jobs:\n  build:\n    steps:\n      - run: |\n          make\n          test\n";
    assert_eq!(folds("yaml", yaml), [(1, 6), (2, 6), (3, 6), (4, 6)]);
  }

  #[test]
  fn markdown_headings_fold_their_sections() {
    let markdown = "# A\n\ntext\n\n## B\n\n```\ncode\n```\n\n# C\nmore\n";
    assert_eq!(
      folds("markdown", markdown),
      [(1, 9), (5, 9), (7, 9), (11, 12)]
    );
  }

  #[test]
  fn single_line_and_empty_regions_do_not_fold() {
    assert!(folds("rust", "fn f() {}\nfn g() {\n}\n").is_empty());
    assert!(folds("json", "{\"a\": [1, 2]}").is_empty());
  }

  #[test]
  fn injected_languages_fold_too() {
    let html = "<html>\n<style>\na {\n  color: red;\n}\n</style>\n</html>\n";
    let found = folds("html", html);
    assert!(found.contains(&(1, 6)), "{found:?}");
    assert!(found.contains(&(3, 4)), "the CSS rule: {found:?}");
  }

  #[test]
  fn languages_without_a_fold_query_have_none() {
    let registry = testing::registry_without_folds();
    let buffer = TextBuffer::from_text("fn f() {\n}\n");
    let tree = parse(
      registry.get("rust").expect("an example"),
      &registry,
      &buffer,
      None,
      || false,
    )
    .expect("grammar")
    .expect("parses");
    assert_eq!(fold_ranges(&tree, &buffer, 0..2), None);
  }

  #[test]
  fn only_regions_touching_the_lines_are_found() {
    let registry = testing::registry();
    let text = "fn a() {\n    1;\n}\nfn b() {\n    2;\n}\n";
    let buffer = TextBuffer::from_text(text);
    let tree = parse(
      registry.get("rust").expect("an example"),
      &registry,
      &buffer,
      None,
      || false,
    )
    .expect("grammar")
    .expect("parses");
    assert_eq!(fold_ranges(&tree, &buffer, 4..5), Some(vec![3..5]));
  }
}
