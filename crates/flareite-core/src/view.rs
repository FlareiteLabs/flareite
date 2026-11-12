//! Incremental viewport updates.
//!
//! A client tells the document which lines it holds (ranges at some
//! version). The update replays the line splices since that version so the
//! client can keep its still-valid lines, and carries text only for the
//! requested lines it is missing. A typical keystroke therefore transfers one
//! line, and scrolling transfers only newly exposed lines.
//!
//! Viewports are display rows, which skip the hidden lines of folded
//! regions (see [`crate::folds`]), so a client holds the visible lines of
//! its viewport: one range per run of visible lines.

use std::ops::Range;

use flareite_text::{LineSplice, Version, map_line_range, missing_ranges};

use crate::document::Document;

/// Longest line text sent for display, in bytes. Longer lines are cut (and
/// marked as cut); the document itself is never changed.
pub const MAX_DISPLAY_LINE_BYTES: usize = 16 * 1024;

/// Lines a client holds as of `version`: sorted, separate ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownLines {
  pub version: Version,
  pub ranges: Vec<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineText {
  pub text: String,
  /// The line's full length in UTF-16 units, when `text` was cut for display.
  pub full_utf16_len: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChunk {
  pub start: usize,
  pub lines: Vec<LineText>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportUpdate {
  /// The client version the splices start from, or `None` when the client
  /// must discard what it holds (unknown or too old).
  pub base: Option<Version>,
  pub splices: Vec<LineSplice>,
  /// The lines the client holds afterwards: the visible lines of the
  /// (clamped) viewport, as sorted ranges.
  pub lines: Vec<Range<usize>>,
  /// The display row of the first line (its first row, when it wraps, which
  /// can be before the viewport's start).
  pub start_row: usize,
  /// Text for viewport lines the client is missing.
  pub chunks: Vec<LineChunk>,
}

impl Document {
  /// What a client holding `known` needs to show display rows `rows`
  /// (clamped to the document and to `max_rows`).
  pub fn viewport_update(
    &self,
    known: Option<&KnownLines>,
    rows: Range<usize>,
    max_rows: usize,
  ) -> ViewportUpdate {
    let row_count = self.row_count();
    let start = rows.start.min(row_count - 1);
    let end = rows
      .end
      .clamp(start + 1, row_count)
      .min(start + max_rows.max(1));
    let first = self.line_at_row(start);
    let lines = self.visible_lines(first..self.line_at_row(end - 1) + 1);

    let replay = known.and_then(|known| {
      let splices = self.buffer.line_changes_since(known.version)?;
      Some((known, splices))
    });
    let (base, splices, missing) = match replay {
      Some((known, splices)) => {
        let mut have: Vec<Range<usize>> = known
          .ranges
          .iter()
          .flat_map(|range| map_line_range(range.clone(), &splices))
          .collect();
        have.sort_by_key(|range| range.start);
        let missing = lines
          .iter()
          .flat_map(|range| missing_ranges(range.clone(), &have))
          .collect();
        (Some(known.version), splices, missing)
      }
      None => (None, Vec::new(), lines.clone()),
    };
    let chunks = missing
      .into_iter()
      .map(|range| LineChunk {
        start: range.start,
        lines: range.map(|line| self.display_line(line)).collect(),
      })
      .collect();
    ViewportUpdate {
      base,
      splices,
      lines,
      start_row: self.row_of(first),
      chunks,
    }
  }

  fn display_line(&self, line: usize) -> LineText {
    let (text, cut) = self.buffer.line_prefix(line, MAX_DISPLAY_LINE_BYTES);
    LineText {
      text,
      full_utf16_len: cut.then(|| self.buffer.line_utf16_len(line)),
    }
  }
}

#[cfg(test)]
// Line ranges are naturally single-element arrays here.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
  use super::*;
  use crate::document::{DocumentId, EditorConfig};
  use crate::ops::EditorOp;
  use crate::selection::Selections;

  fn doc(lines: usize) -> Document {
    let text: String = (0..lines).map(|i| format!("line {i}\n")).collect();
    Document::new(DocumentId(1), &text, None, 1, &EditorConfig::default())
  }

  fn texts(update: &ViewportUpdate) -> Vec<(usize, String)> {
    update
      .chunks
      .iter()
      .flat_map(|chunk| {
        chunk
          .lines
          .iter()
          .enumerate()
          .map(move |(i, line)| (chunk.start + i, line.text.clone()))
      })
      .collect()
  }

  fn known(document: &Document, update: &ViewportUpdate) -> KnownLines {
    KnownLines {
      version: document.version(),
      ranges: update.lines.clone(),
    }
  }

  #[test]
  fn a_client_without_lines_gets_the_whole_viewport() {
    let document = doc(100);
    let update = document.viewport_update(None, 10..13, 1000);
    assert_eq!(update.base, None);
    assert_eq!(update.lines, [10..13]);
    assert_eq!(
      texts(&update),
      vec![
        (10, "line 10".into()),
        (11, "line 11".into()),
        (12, "line 12".into())
      ]
    );
  }

  #[test]
  fn an_unchanged_document_sends_nothing() {
    let document = doc(100);
    let first = document.viewport_update(None, 0..20, 1000);
    let second = document.viewport_update(Some(&known(&document, &first)), 0..20, 1000);
    assert_eq!(second.base, Some(document.version()));
    assert!(second.chunks.is_empty());
  }

  #[test]
  fn scrolling_sends_only_newly_exposed_lines() {
    let document = doc(100);
    let first = document.viewport_update(None, 0..20, 1000);
    let scrolled = document.viewport_update(Some(&known(&document, &first)), 15..35, 1000);
    let lines: Vec<usize> = texts(&scrolled).into_iter().map(|(line, _)| line).collect();
    assert_eq!(lines, (20..35).collect::<Vec<_>>());
  }

  #[test]
  fn typing_sends_only_the_edited_line() {
    let mut document = doc(100);
    let first = document.viewport_update(None, 0..20, 1000);
    let before = known(&document, &first);
    document.selections = Selections::cursor(document.buffer().line_start(5));
    document
      .apply(EditorOp::Insert("x".into()))
      .expect("applies");
    let update = document.viewport_update(Some(&before), 0..20, 1000);
    assert_eq!(update.base, Some(before.version));
    assert_eq!(texts(&update), vec![(5, "xline 5".into())]);
  }

  #[test]
  fn a_new_line_shifts_cached_lines_instead_of_resending_them() {
    let mut document = doc(100);
    let first = document.viewport_update(None, 0..20, 1000);
    let before = known(&document, &first);
    document.selections = Selections::cursor(document.buffer().line_end(3));
    document.apply(EditorOp::Newline).expect("applies");
    let update = document.viewport_update(Some(&before), 0..20, 1000);
    // Line 3 is rewritten, line 4 is new; old lines 4..18 are now 5..19 and reused.
    assert_eq!(
      texts(&update),
      vec![(3, "line 3".into()), (4, String::new())]
    );
    assert_eq!(update.splices.len(), 1);
  }

  #[test]
  fn long_lines_are_cut_for_display_and_marked() {
    let long = "é".repeat(MAX_DISPLAY_LINE_BYTES);
    let document = Document::new(DocumentId(1), &long, None, 1, &EditorConfig::default());
    let update = document.viewport_update(None, 0..1, 1000);
    let line = &update.chunks[0].lines[0];
    assert!(line.text.len() <= MAX_DISPLAY_LINE_BYTES);
    assert_eq!(line.full_utf16_len, Some(MAX_DISPLAY_LINE_BYTES));
    assert_eq!(
      document.buffer().contents(),
      long,
      "the document is unchanged"
    );
  }

  #[test]
  #[allow(clippy::reversed_empty_ranges)]
  fn viewports_are_clamped() {
    let document = doc(5);
    let update = document.viewport_update(None, 50..10, 1000);
    assert_eq!(update.lines, [5..6]);
    let update = document.viewport_update(None, 0..1000, 10);
    assert_eq!(update.lines, [0..6], "five lines plus the empty last line");
  }

  #[test]
  fn viewports_count_rows_and_skip_folded_lines() {
    let mut document = doc(100);
    document.set_folded([2..10]);
    let update = document.viewport_update(None, 1..4, 1000);
    assert_eq!(update.start_row, 1);
    assert_eq!(
      update.lines,
      [1..3, 10..11],
      "rows 1 to 3 are lines 1, 2, and 10"
    );
    let lines: Vec<usize> = texts(&update).into_iter().map(|(line, _)| line).collect();
    assert_eq!(lines, [1, 2, 10]);

    // Unfolding shows lines the client doesn't have yet.
    let before = known(&document, &update);
    document.apply(EditorOp::UnfoldAll).expect("applies");
    let update = document.viewport_update(Some(&before), 1..4, 1000);
    assert_eq!(update.lines, [1..4]);
    assert_eq!(texts(&update), vec![(3, "line 3".into())]);
  }
}
