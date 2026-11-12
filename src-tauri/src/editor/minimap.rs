//! The minimap: a document's lines drawn small beside the editor.
//!
//! A line is sent as the shape of its text rather than the text itself: runs
//! of non-blank columns, colored by token kind. Tabs advance to tab stops,
//! as in the editor, and only the first [`MINIMAP_COLUMNS`] columns are kept.
//! Highlights come from the document's syntax tree without touching what
//! the editor's view has been sent.
//!
//! A minimap covers many more lines than the editor shows, so it is built
//! from a [`Snapshot`] taken under the editor lock, and the lock is released
//! while lines are highlighted. Like the editor, it shows display rows: the
//! hidden lines of folded regions are left out, and a wrapped line takes a
//! row for each of its rows. Named regions and `MARK:` comments come with the
//! lines as section headers, which the minimap labels.

use std::collections::HashMap;
use std::ops::Range;

use flareite_core::{Document, LineWrap, MAX_DISPLAY_LINE_BYTES, SectionHeader};
use flareite_syntax::{Span, SyntaxTree, highlight_lines};
use flareite_text::TextBuffer;
use serde::Serialize;

/// Columns drawn per line.
pub const MINIMAP_COLUMNS: usize = 120;
/// Bytes of each line read: its columns, even if every character is wide.
pub const MINIMAP_LINE_BYTES: usize = MINIMAP_COLUMNS * 4;
/// Most lines sent at once.
pub const MAX_MINIMAP_LINES: usize = 1000;

/// Display rows `start_row..start_row + lines.len()` of a document, each as
/// runs `[start, end, kind, ...]` in visual columns. Kind 0 is text without
/// a highlight; others are token kinds as in the view's highlights.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinimapDto {
  pub document_id: u64,
  pub version: u64,
  pub row_count: usize,
  pub start_row: usize,
  pub lines: Vec<Vec<u32>>,
  /// Section headers on these rows.
  pub headers: Vec<MinimapHeaderDto>,
}

/// A labelled row: a named region (`region`) or a `MARK:` comment, which may
/// draw a separator above it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MinimapHeaderDto {
  pub row: usize,
  pub name: String,
  pub separator: bool,
  pub region: bool,
}

/// What a minimap needs from a document. Cheap to take: the text is a rope
/// and the tree is reference counted.
pub struct Snapshot {
  document_id: u64,
  version: u64,
  tab_size: usize,
  buffer: TextBuffer,
  /// The syntax tree, edited to match `buffer`.
  syntax: Option<SyntaxTree>,
  row_count: usize,
  start_row: usize,
  /// The visible lines of the rows to draw, as sorted ranges.
  lines: Vec<Range<usize>>,
  /// Rows of the first line before `start_row`, which aren't drawn, and how
  /// many rows are.
  skip: usize,
  rows: usize,
  /// Where the lines that wrap do.
  wraps: HashMap<usize, LineWrap>,
  headers: Vec<MinimapHeaderDto>,
}

impl Snapshot {
  /// What a minimap of display rows `rows` needs, clamped to the document
  /// and to [`MAX_MINIMAP_LINES`]. `headers` gives the section headers on a
  /// range of lines.
  pub fn of(
    document: &Document,
    syntax: Option<SyntaxTree>,
    rows: Range<usize>,
    headers: impl FnOnce(Range<usize>) -> Vec<SectionHeader>,
  ) -> Self {
    let row_count = document.row_count();
    let start = rows.start.min(row_count);
    let end = rows
      .end
      .clamp(start, row_count)
      .min(start + MAX_MINIMAP_LINES);
    let lines = if start < end {
      document.visible_lines(document.line_at_row(start)..document.line_at_row(end - 1) + 1)
    } else {
      Vec::new()
    };
    let wraps = if document.wrap_config().is_some() {
      lines
        .iter()
        .flat_map(Clone::clone)
        .filter_map(|line| Some((line, document.line_wrap(line)?)))
        .collect()
    } else {
      HashMap::new()
    };
    let skip = lines
      .first()
      .map_or(0, |first| start - document.row_of(first.start));
    let headers = match (lines.first(), lines.last()) {
      (Some(first), Some(last)) => headers(first.start..last.end)
        .into_iter()
        .filter(|header| lines.iter().any(|range| range.contains(&header.line)))
        .map(|header| MinimapHeaderDto {
          row: document.row_of(header.line),
          name: header.name,
          separator: header.separator,
          region: header.region,
        })
        .collect(),
      _ => Vec::new(),
    };
    Self {
      document_id: document.id().get(),
      version: document.version().get(),
      tab_size: document.tab_size(),
      buffer: document.buffer().clone(),
      syntax,
      row_count,
      start_row: start,
      lines,
      skip,
      rows: end - start,
      wraps,
      headers,
    }
  }

  pub fn build(&self) -> MinimapDto {
    self.build_with(|range, max_bytes| match &self.syntax {
      Some(tree) => highlight_lines(tree, &self.buffer, range, max_bytes),
      None => vec![Vec::new(); range.len()],
    })
  }

  /// [`Self::build`], with `spans` highlighting a range of lines, each cut
  /// to a number of bytes: [`MINIMAP_LINE_BYTES`], or what's shown of a
  /// wrapped line, which takes several rows.
  fn build_with(&self, mut spans: impl FnMut(Range<usize>, usize) -> Vec<Vec<Span>>) -> MinimapDto {
    let mut rows = Vec::new();
    for range in &self.lines {
      let unwrapped = spans(range.clone(), MINIMAP_LINE_BYTES);
      for line in range.clone() {
        match self.wraps.get(&line) {
          Some(wrap) => {
            let line_spans = spans(line..line + 1, MAX_DISPLAY_LINE_BYTES);
            let line_spans = line_spans.first().map_or(&[][..], Vec::as_slice);
            let (text, _) = self.buffer.line_prefix(line, MAX_DISPLAY_LINE_BYTES);
            rows.extend(wrapped_runs(&text, line_spans, wrap, self.tab_size));
          }
          None => {
            let (text, _) = self.buffer.line_prefix(line, MINIMAP_LINE_BYTES);
            let line_spans = unwrapped
              .get(line - range.start)
              .map_or(&[][..], Vec::as_slice);
            rows.push(line_runs(&text, line_spans, self.tab_size));
          }
        }
      }
    }
    let lines = rows.into_iter().skip(self.skip).take(self.rows).collect();
    MinimapDto {
      document_id: self.document_id,
      version: self.version,
      row_count: self.row_count,
      start_row: self.start_row,
      lines,
      headers: self.headers.clone(),
    }
  }
}

/// The runs of each row of a wrapped line, continuation rows moved right by
/// their indent.
fn wrapped_runs(text: &str, spans: &[Span], wrap: &LineWrap, tab_size: usize) -> Vec<Vec<u32>> {
  (0..=wrap.breaks.len())
    .map(|row| {
      let range = wrap.row_range(row, text.len());
      let row_spans: Vec<Span> = spans
        .iter()
        .filter(|span| span.end > range.start && span.start < range.end)
        .map(|span| Span {
          start: span.start.max(range.start) - range.start,
          end: span.end.min(range.end) - range.start,
          kind: span.kind,
        })
        .collect();
      let mut runs = line_runs(&text[range], &row_spans, tab_size);
      let indent = if row > 0 { wrap.indent as u32 } else { 0 };
      let last = MINIMAP_COLUMNS as u32;
      for (index, column) in runs.iter_mut().enumerate() {
        if index % 3 != 2 {
          *column = (*column + indent).min(last);
        }
      }
      runs
    })
    .collect()
}

/// The runs of `text` as `[start, end, kind, ...]`: stretches of non-blank
/// characters of one kind, in visual columns up to [`MINIMAP_COLUMNS`].
/// `spans` are byte ranges of `text`, in order.
fn line_runs(text: &str, spans: &[Span], tab_size: usize) -> Vec<u32> {
  let tab_size = tab_size.max(1);
  let mut runs: Vec<u32> = Vec::new();
  let mut spans = spans.iter().peekable();
  let mut column = 0;
  for (byte, ch) in text.char_indices() {
    if column >= MINIMAP_COLUMNS {
      break;
    }
    let width = if ch == '\t' {
      tab_size - column % tab_size
    } else {
      1
    };
    if !ch.is_whitespace() {
      while spans.next_if(|span| span.end <= byte).is_some() {}
      let kind = spans
        .peek()
        .filter(|span| span.start <= byte)
        .map_or(0, |span| span.kind as u32);
      let (start, end) = (column as u32, (column + width).min(MINIMAP_COLUMNS) as u32);
      match runs.as_mut_slice() {
        // Continue the previous run if it ends here with the same kind.
        [.., last_end, last_kind] if *last_end == start && *last_kind == kind => *last_end = end,
        _ => runs.extend([start, end, kind]),
      }
    }
    column += width;
  }
  runs
}

#[cfg(test)]
// Line ranges are naturally single-element arrays here.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
  use flareite_core::{Editor, EditorConfig, EditorOp};
  use flareite_syntax::TokenKind;

  use super::*;

  fn span(start: usize, end: usize, kind: TokenKind) -> Span {
    Span { start, end, kind }
  }

  #[test]
  fn runs_split_at_blanks_and_kinds() {
    let spans = [
      span(0, 2, TokenKind::Keyword),
      span(3, 7, TokenKind::Function),
    ];
    // "fn" keyword, "main" function, then "()" plain, " {" plain.
    let runs = line_runs("fn main() {", &spans, 4);
    let keyword = TokenKind::Keyword as u32;
    let function = TokenKind::Function as u32;
    assert_eq!(runs, [0, 2, keyword, 3, 7, function, 7, 9, 0, 10, 11, 0]);
  }

  #[test]
  fn tabs_advance_to_tab_stops() {
    assert_eq!(line_runs("\tx\ty", &[], 4), [4, 5, 0, 8, 9, 0]);
    assert_eq!(line_runs("ab\tc", &[], 4), [0, 2, 0, 4, 5, 0]);
    assert_eq!(line_runs("\tx", &[], 0), [1, 2, 0]);
  }

  #[test]
  fn counts_characters_not_bytes_and_stops_at_the_last_column() {
    let spans = [span(4, 8, TokenKind::String)];
    // "é" is 2 bytes; the string span starts at byte 4, the third character.
    let runs = line_runs("éa \"x\"", &spans, 4);
    assert_eq!(runs, [0, 2, 0, 3, 6, TokenKind::String as u32]);

    let long = "x".repeat(MINIMAP_COLUMNS + 50);
    assert_eq!(line_runs(&long, &[], 4), [0, MINIMAP_COLUMNS as u32, 0]);
    let tab_at_edge = format!("{}\ty", "x".repeat(MINIMAP_COLUMNS - 2));
    assert_eq!(
      line_runs(&tab_at_edge, &[], 4),
      [0, MINIMAP_COLUMNS as u32 - 2, 0]
    );
  }

  #[test]
  fn builds_clamped_ranges_with_the_document_tab_size() {
    let mut editor = Editor::new(EditorConfig {
      tab_size: 2,
      ..EditorConfig::default()
    });
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("a\n\tb\n\nc".into()))
      .expect("applies");

    let snapshot = Snapshot::of(document, None, 1..99, |_| Vec::new());
    let mut asked = None;
    let minimap = snapshot.build_with(|lines, _| {
      asked = Some(lines.clone());
      lines.map(|_| Vec::new()).collect()
    });
    assert_eq!(asked, Some(1..4));
    assert_eq!(minimap.start_row, 1);
    assert_eq!(minimap.row_count, 4);
    assert_eq!(minimap.lines, vec![vec![2, 3, 0], vec![], vec![0, 1, 0]]);

    let past_the_end = Snapshot::of(document, None, 10..20, |_| Vec::new()).build();
    assert_eq!(past_the_end.start_row, 4);
    assert!(past_the_end.lines.is_empty());
  }

  #[test]
  fn leaves_out_folded_lines() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert(
        "a
 b
 c
d"
        .into(),
      ))
      .expect("applies");
    document.set_folded([0..3]);
    let header = |line| SectionHeader {
      line,
      name: "d".into(),
      separator: false,
      region: false,
    };
    let minimap = Snapshot::of(document, None, 0..10, |_| vec![header(1), header(3)]).build();
    assert_eq!(minimap.row_count, 2);
    assert_eq!(minimap.lines, vec![vec![0, 1, 0], vec![0, 1, 0]], "a and d");
    assert_eq!(
      minimap.headers.iter().map(|h| h.row).collect::<Vec<_>>(),
      [1],
      "headers on hidden lines are left out; d is on row 1"
    );
  }

  #[test]
  fn wrapped_lines_take_a_row_each() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("  one two three\nx".into()))
      .expect("applies");
    document
      .apply(EditorOp::SetWrap(Some(flareite_core::WrapConfig {
        columns: 10,
        indent: flareite_core::WrapIndent::Same,
      })))
      .expect("applies");
    // "  one two " / "three", indented 2 / "x"
    let minimap = Snapshot::of(document, None, 0..10, |_| Vec::new()).build();
    assert_eq!(minimap.row_count, 3);
    assert_eq!(
      minimap.lines,
      vec![vec![2, 5, 0, 6, 9, 0], vec![2, 7, 0], vec![0, 1, 0]]
    );
    // From the middle of the wrapped line.
    let from_its_second_row = Snapshot::of(document, None, 1..10, |_| Vec::new()).build();
    assert_eq!(from_its_second_row.start_row, 1);
    assert_eq!(
      from_its_second_row.lines,
      vec![vec![2, 7, 0], vec![0, 1, 0]]
    );
  }

  #[test]
  fn sends_at_most_the_line_limit() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("x\n".repeat(MAX_MINIMAP_LINES * 2)))
      .expect("applies");
    let minimap = Snapshot::of(document, None, 0..usize::MAX, |_| Vec::new()).build();
    assert_eq!(minimap.lines.len(), MAX_MINIMAP_LINES);
  }
}
