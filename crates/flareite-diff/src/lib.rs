//! Text diffs, as a diff editor shows them: which lines changed, and within
//! changed lines, which characters.
//!
//! Lines are compared with the histogram algorithm (`imara-diff`), then
//! moved to where a person would expect them (by indentation, as Git
//! does). Optionally, lines that differ only in leading or trailing
//! whitespace count as the same. Within a change that replaces lines with
//! others, words and punctuation are compared to find the changed
//! characters, unless the change is too large for that to help.
//!
//! Lines are split on `\n` (a `\r` before it isn't part of the line), and
//! columns are UTF-16 code units, as the editor counts them.

mod splice;

pub use splice::{Splice, apply, revert, splices, touching};

use std::collections::HashMap;
use std::ops::Range;

use imara_diff::{
  Algorithm, Diff, IndentHeuristic, IndentLevel, InternedInput, Interner, NoSliderHeuristic, Token,
};

/// Most lines a side may have; larger texts aren't compared.
pub const MAX_LINES: usize = 500_000;
/// Most lines on a side of a change whose characters are compared.
const MAX_INNER_LINES: usize = 500;
/// Most characters on a side of a change whose characters are compared.
const MAX_INNER_CHARS: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffOptions {
  /// Lines that differ only in leading or trailing whitespace are the same.
  pub ignore_trim_whitespace: bool,
}

/// A place in a text: a line and a UTF-16 column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
  pub line: usize,
  pub column: usize,
}

/// Characters that changed: `original` in the original text became
/// `modified` in the modified one. Either may be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharChange {
  pub original: Range<Point>,
  pub modified: Range<Point>,
}

/// Lines that changed: `original` lines became `modified` lines. An empty
/// range is a place between lines (an insertion or a removal there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChange {
  pub original: Range<usize>,
  pub modified: Range<usize>,
  /// The characters that changed, when lines were replaced by others and
  /// the change is small enough to compare them.
  pub chars: Vec<CharChange>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffError {
  /// A side has more than [`MAX_LINES`] lines.
  TooLarge,
}

/// A text's lines, without their line breaks.
pub(crate) fn lines(text: &str) -> Vec<&str> {
  text
    .split('\n')
    .map(|line| line.strip_suffix('\r').unwrap_or(line))
    .collect()
}

/// Compares two texts.
pub fn diff<'a>(
  original: &'a str,
  modified: &'a str,
  options: DiffOptions,
) -> Result<Vec<LineChange>, DiffError> {
  let before = lines(original);
  let after = lines(modified);
  if before.len() > MAX_LINES || after.len() > MAX_LINES {
    return Err(DiffError::TooLarge);
  }
  let key = |line: &'a str| -> &'a str {
    if options.ignore_trim_whitespace {
      line.trim()
    } else {
      line
    }
  };
  let mut interner = Interner::new(before.len() + after.len());
  let input = InternedInput {
    before: before
      .iter()
      .map(|line| interner.intern(key(line)))
      .collect(),
    after: after
      .iter()
      .map(|line| interner.intern(key(line)))
      .collect(),
    interner,
  };
  let mut diff = Diff::compute(Algorithm::Histogram, &input);
  diff.postprocess_with(
    &input.before,
    &input.after,
    IndentHeuristic::new(|token| IndentLevel::for_ascii_line(input.interner[token].bytes(), 4)),
  );
  Ok(
    diff
      .hunks()
      .map(|hunk| {
        let original = hunk.before.start as usize..hunk.before.end as usize;
        let modified = hunk.after.start as usize..hunk.after.end as usize;
        let chars = if original.is_empty() || modified.is_empty() {
          Vec::new()
        } else {
          char_changes(
            &before[original.clone()],
            original.start,
            &after[modified.clone()],
            modified.start,
            options,
          )
        };
        LineChange {
          original,
          modified,
          chars,
        }
      })
      .collect(),
  )
}

/// A word, a run of whitespace, a punctuation character, or a line break,
/// and where it is.
struct Piece<'a> {
  text: &'a str,
  start: Point,
  end: Point,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
  Word,
  Space,
  Other,
}

fn class(c: char) -> Class {
  if c.is_alphanumeric() || c == '_' {
    Class::Word
  } else if c.is_whitespace() {
    Class::Space
  } else {
    Class::Other
  }
}

/// The pieces of `lines` (starting at line `first`), with a line break
/// piece between lines. Leading and trailing whitespace is left out when
/// it's ignored.
fn pieces<'a>(lines: &[&'a str], first: usize, options: DiffOptions) -> Vec<Piece<'a>> {
  let mut pieces = Vec::new();
  for (index, line) in lines.iter().enumerate() {
    let number = first + index;
    if index > 0 {
      let end = lines[index - 1].encode_utf16().count();
      pieces.push(Piece {
        text: "\n",
        start: Point {
          line: number - 1,
          column: end,
        },
        end: Point {
          line: number,
          column: 0,
        },
      });
    }
    let (skip, keep) = if options.ignore_trim_whitespace {
      let trimmed = line.trim_start();
      (line.len() - trimmed.len(), trimmed.trim_end().len())
    } else {
      (0, line.len())
    };
    let text = &line[skip..skip + keep];
    let mut column = line[..skip].encode_utf16().count();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
      let kind = class(c);
      let mut end = at + c.len_utf8();
      // Words and whitespace run on; punctuation is one character each.
      if kind != Class::Other {
        while let Some(&(next_at, next)) = chars.peek() {
          if class(next) != kind {
            break;
          }
          end = next_at + next.len_utf8();
          chars.next();
        }
      }
      let piece = &text[start.max(at)..end];
      let width = piece.encode_utf16().count();
      pieces.push(Piece {
        text: piece,
        start: Point {
          line: number,
          column,
        },
        end: Point {
          line: number,
          column: column + width,
        },
      });
      column += width;
      start = end;
    }
  }
  pieces
}

/// The characters that changed between lines that replaced others.
fn char_changes<'a>(
  before: &[&'a str],
  before_first: usize,
  after: &[&'a str],
  after_first: usize,
  options: DiffOptions,
) -> Vec<CharChange> {
  let size = |lines: &[&str]| lines.iter().map(|line| line.len()).sum::<usize>();
  if before.len() > MAX_INNER_LINES
    || after.len() > MAX_INNER_LINES
    || size(before) > MAX_INNER_CHARS
    || size(after) > MAX_INNER_CHARS
  {
    return Vec::new();
  }
  let old = pieces(before, before_first, options);
  let new = pieces(after, after_first, options);
  let mut ids: HashMap<&str, u32> = HashMap::new();
  let mut token = |text: &'a str| {
    let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
    Token(*ids.entry(text).or_insert(next))
  };
  let old_tokens: Vec<Token> = old.iter().map(|piece| token(piece.text)).collect();
  let new_tokens: Vec<Token> = new.iter().map(|piece| token(piece.text)).collect();
  let count = u32::try_from(ids.len()).unwrap_or(u32::MAX);
  let mut diff = Diff::default();
  diff.compute_with(Algorithm::Myers, &old_tokens, &new_tokens, count);
  diff.postprocess_with(&old_tokens, &new_tokens, NoSliderHeuristic);

  // Where an empty range sits: at the start of the piece after it, or the
  // end of the last.
  let at = |pieces: &[Piece<'_>], index: usize, fallback: Point| {
    pieces
      .get(index)
      .map(|piece| piece.start)
      .or_else(|| pieces.last().map(|piece| piece.end))
      .unwrap_or(fallback)
  };
  let span = |pieces: &[Piece<'_>], range: Range<u32>, fallback: Point| {
    let (start, end) = (range.start as usize, range.end as usize);
    if start == end {
      let point = at(pieces, start, fallback);
      point..point
    } else {
      pieces[start].start..pieces[end - 1].end
    }
  };
  let old_start = Point {
    line: before_first,
    column: 0,
  };
  let new_start = Point {
    line: after_first,
    column: 0,
  };
  diff
    .hunks()
    .filter(|hunk| {
      // Whitespace alone between words isn't worth showing when ignored.
      !(options.ignore_trim_whitespace
        && hunk
          .before
          .clone()
          .all(|i| old[i as usize].text.trim().is_empty())
        && hunk
          .after
          .clone()
          .all(|i| new[i as usize].text.trim().is_empty()))
    })
    .map(|hunk| CharChange {
      original: span(&old, hunk.before, old_start),
      modified: span(&new, hunk.after, new_start),
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn point(line: usize, column: usize) -> Point {
    Point { line, column }
  }

  fn line_ranges(changes: &[LineChange]) -> Vec<(Range<usize>, Range<usize>)> {
    changes
      .iter()
      .map(|change| (change.original.clone(), change.modified.clone()))
      .collect()
  }

  #[test]
  fn finds_added_removed_and_changed_lines() {
    let original = "fn main() {\n  one();\n  two();\n}\n";
    let modified = "// start\nfn main() {\n  one();\n  three();\n}\n";
    let changes = diff(original, modified, DiffOptions::default()).expect("diffs");
    assert_eq!(line_ranges(&changes), vec![(0..0, 0..1), (2..3, 3..4)]);
    assert!(
      changes[0].chars.is_empty(),
      "an insertion has no character changes"
    );
    assert_eq!(
      changes[1].chars,
      vec![CharChange {
        original: point(2, 2)..point(2, 5),
        modified: point(3, 2)..point(3, 7),
      }]
    );
  }

  #[test]
  fn identical_texts_have_no_changes() {
    assert!(
      diff("a\nb", "a\nb", DiffOptions::default())
        .expect("diffs")
        .is_empty()
    );
    assert!(
      diff("", "", DiffOptions::default())
        .expect("diffs")
        .is_empty()
    );
  }

  #[test]
  fn line_endings_dont_count() {
    assert!(
      diff("a\r\nb\r\n", "a\nb\n", DiffOptions::default())
        .expect("diffs")
        .is_empty()
    );
  }

  #[test]
  fn leading_and_trailing_whitespace_can_be_ignored() {
    let original = "if x {\n  go();\n}\n";
    let modified = "if x {\n    go();   \n}\n";
    let shown = diff(original, modified, DiffOptions::default()).expect("diffs");
    assert_eq!(line_ranges(&shown), vec![(1..2, 1..2)]);
    let ignored = diff(
      original,
      modified,
      DiffOptions {
        ignore_trim_whitespace: true,
      },
    )
    .expect("diffs");
    assert!(ignored.is_empty());

    // A real change on such a line shows only its characters.
    let changed = diff(
      "  call(a);\n",
      "    call(b);\n",
      DiffOptions {
        ignore_trim_whitespace: true,
      },
    )
    .expect("diffs");
    assert_eq!(
      changed[0].chars,
      vec![CharChange {
        original: point(0, 7)..point(0, 8),
        modified: point(0, 9)..point(0, 10),
      }]
    );
  }

  #[test]
  fn columns_count_utf16_units() {
    let changes = diff("é😀 a\n", "é😀 b\n", DiffOptions::default()).expect("diffs");
    // é is one unit, 😀 two, the space one: "a" is at column 4.
    assert_eq!(
      changes[0].chars,
      vec![CharChange {
        original: point(0, 4)..point(0, 5),
        modified: point(0, 4)..point(0, 5),
      }]
    );
  }

  #[test]
  fn changes_across_lines_and_insertions_inside_a_line() {
    let changes = diff(
      "let a = 1;\nlet b = 2;\n",
      "let a = 10;\n",
      DiffOptions::default(),
    )
    .expect("diffs");
    assert_eq!(line_ranges(&changes), vec![(0..2, 0..1)]);
    // "1;", the line break, and "let b = 2" became "10" (the ";" left is
    // the second line's): one change across the lines.
    assert_eq!(
      changes[0].chars,
      vec![CharChange {
        original: point(0, 8)..point(1, 9),
        modified: point(0, 8)..point(0, 10),
      }]
    );
  }

  #[test]
  fn large_changes_keep_only_their_lines() {
    let original: String = (0..600).map(|i| format!("old {i}\n")).collect();
    let modified: String = (0..600).map(|i| format!("new {i}\n")).collect();
    let changes = diff(&original, &modified, DiffOptions::default()).expect("diffs");
    assert_eq!(line_ranges(&changes), vec![(0..600, 0..600)]);
    assert!(changes[0].chars.is_empty());
  }
}
