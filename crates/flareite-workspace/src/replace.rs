//! Replacing search results (ADR 0053).
//!
//! A replace names the results it's for as the search reported them: a line
//! and UTF-16 columns. The text is searched again, and the matches found at
//! those places are replaced, so a result that has moved or changed since is
//! left alone rather than replaced somewhere the user didn't see.

use std::collections::HashSet;
use std::ops::Range;

use flareite_text::{Edit, Searcher, TextBuffer};

/// Most matches replaced in one file.
const MAX_MATCHES: usize = 100_000;

/// Where a result was: its line, and its columns there (UTF-16), as in
/// [`crate::LineMatch`]. A match spanning lines ends at its first line's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatchAt {
  pub line: usize,
  pub start_column: usize,
  pub end_column: usize,
}

/// The edits that replace a file's results.
#[derive(Debug, Default)]
pub struct Replacements {
  /// In order, not overlapping.
  pub edits: Vec<Edit>,
  /// Results no match is at any more.
  pub missing: usize,
}

/// Where the match at `range` of `buffer` is, as a search reports it.
pub fn match_at(buffer: &TextBuffer, range: Range<usize>) -> MatchAt {
  let start = buffer.offset_to_utf16(range.start);
  let end = buffer.offset_to_utf16(range.end);
  MatchAt {
    line: start.line,
    start_column: start.column,
    end_column: if end.line == start.line {
      end.column
    } else {
      buffer.line_utf16_len(start.line)
    },
  }
}

/// The edits replacing the matches of `searcher` in `buffer` that are at the
/// places `wanted` names, with `replacement` (its capture groups expanded,
/// for a regex). Other matches stay.
pub fn replacements(
  buffer: &TextBuffer,
  searcher: &Searcher,
  replacement: &str,
  wanted: &[MatchAt],
) -> Replacements {
  let wanted: HashSet<MatchAt> = wanted.iter().copied().collect();
  let mut found = 0;
  let edits = searcher
    .find_in_buffer(buffer, 0..buffer.len(), MAX_MATCHES)
    .into_iter()
    .filter(|range| wanted.contains(&match_at(buffer, range.clone())))
    .map(|range| {
      found += 1;
      let text = searcher.expand_replacement(buffer, range.clone(), replacement);
      Edit::new(range, text)
    })
    .collect();
  Replacements {
    edits,
    missing: wanted.len() - found.min(wanted.len()),
  }
}

#[cfg(test)]
mod tests {
  use flareite_text::{ChangeSet, SearchQuery};

  use super::*;

  fn searcher(pattern: &str, regex: bool) -> Searcher {
    SearchQuery {
      pattern: pattern.into(),
      regex,
      ..SearchQuery::default()
    }
    .compile()
    .expect("compiles")
  }

  fn at(line: usize, start_column: usize, end_column: usize) -> MatchAt {
    MatchAt {
      line,
      start_column,
      end_column,
    }
  }

  fn replaced(text: &str, plan: Replacements) -> String {
    let mut buffer = TextBuffer::from_text(text);
    buffer
      .apply(&ChangeSet::new(plan.edits).expect("in order"))
      .expect("applies");
    buffer.contents()
  }

  #[test]
  fn replaces_the_matches_at_the_places_named() {
    let text = "cat cat\n🦀 cat\n";
    let buffer = TextBuffer::from_text(text);
    let search = searcher("cat", false);
    // The second on line 0, and the one after the crab (two UTF-16 units).
    let plan = replacements(&buffer, &search, "dog", &[at(0, 4, 7), at(1, 3, 6)]);
    assert_eq!(plan.missing, 0);
    assert_eq!(replaced(text, plan), "cat dog\n🦀 dog\n");
  }

  #[test]
  fn results_that_moved_are_left_alone_and_counted() {
    let buffer = TextBuffer::from_text("x cat\n");
    let plan = replacements(
      &buffer,
      &searcher("cat", false),
      "dog",
      &[at(0, 0, 3), at(0, 2, 5)],
    );
    assert_eq!(plan.missing, 1);
    assert_eq!(plan.edits.len(), 1);
  }

  #[test]
  fn regex_replacements_expand_groups() {
    let text = "let a = 1;\nlet b = 2;\n";
    let buffer = TextBuffer::from_text(text);
    let search = searcher(r"let (\w) = (\d)", true);
    let plan = replacements(&buffer, &search, "const $1: i32 = $2", &[at(1, 0, 9)]);
    assert_eq!(replaced(text, plan), "let a = 1;\nconst b: i32 = 2;\n");
  }

  #[test]
  fn a_match_across_lines_is_named_by_its_first_line() {
    let buffer = TextBuffer::from_text("ab\r\ncd");
    let search = searcher(r"b\r\nc", true);
    let found = search.find_in_buffer(&buffer, 0..buffer.len(), 10);
    assert_eq!(match_at(&buffer, found[0].clone()), at(0, 1, 2));
  }
}
