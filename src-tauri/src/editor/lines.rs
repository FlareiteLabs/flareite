//! Lines with their highlights, for the editor's copy of a document's lines
//! (ADR 0055). While the editor scrolls faster than its views arrive, it
//! draws the rows its view doesn't cover from that copy. Like the minimap's,
//! they're made from a snapshot taken under the editor lock and highlighted
//! after releasing it, so editing never waits for them.

use std::ops::Range;

use flareite_core::{Document, LineText, MAX_DISPLAY_LINE_BYTES};
use flareite_syntax::SyntaxTree;
use flareite_text::TextBuffer;
use serde::Serialize;

use super::protocol::LineDto;
use super::syntax::highlight_buffer_lines;

/// Most lines one request returns.
pub const MAX_LINES: usize = 2_000;

/// Document lines `start..start + lines.len()`, as views send them.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinesDto {
  pub document_id: u64,
  /// The text version the lines are of.
  pub version: u64,
  pub line_count: usize,
  pub start: usize,
  pub lines: Vec<LineDto>,
  /// Each line's highlights, flattened as in views (UTF-16 columns).
  pub spans: Vec<Vec<u32>>,
}

/// What [`LinesDto`] needs, copied cheaply under the editor lock.
pub struct Snapshot {
  document_id: u64,
  version: u64,
  buffer: TextBuffer,
  syntax: Option<SyntaxTree>,
  lines: Range<usize>,
}

impl Snapshot {
  /// Lines `lines` of `document`, clamped to it and to [`MAX_LINES`], with
  /// `syntax` (its tree, edited to its current text) to highlight them.
  pub fn of(document: &Document, syntax: Option<SyntaxTree>, lines: Range<usize>) -> Self {
    let buffer = document.buffer();
    let count = buffer.line_count();
    let start = lines.start.min(count);
    let end = lines.end.clamp(start, count).min(start + MAX_LINES);
    Self {
      document_id: document.id().get(),
      version: document.version().get(),
      buffer: buffer.clone(),
      syntax,
      lines: start..end,
    }
  }

  pub fn build(&self) -> LinesDto {
    let lines = self
      .lines
      .clone()
      .map(|line| {
        let (text, cut) = self.buffer.line_prefix(line, MAX_DISPLAY_LINE_BYTES);
        LineDto::from(LineText {
          text,
          full_utf16_len: cut.then(|| self.buffer.line_utf16_len(line)),
        })
      })
      .collect();
    LinesDto {
      document_id: self.document_id,
      version: self.version,
      line_count: self.buffer.line_count(),
      start: self.lines.start,
      lines,
      spans: highlight_buffer_lines(self.syntax.as_ref(), &self.buffer, self.lines.clone()),
    }
  }
}

#[cfg(test)]
mod tests {
  use flareite_core::{Editor, EditorOp};

  use super::*;

  #[test]
  fn sends_lines_clamped_to_the_document() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("a\nbb\nccc".into()))
      .expect("applies");

    let lines = Snapshot::of(document, None, 1..99).build();
    assert_eq!(lines.start, 1);
    assert_eq!(lines.line_count, 3);
    assert_eq!(lines.version, document.version().get());
    assert_eq!(
      serde_json::to_value(&lines.lines).expect("serializes"),
      serde_json::json!(["bb", "ccc"])
    );
    assert_eq!(lines.spans, vec![Vec::<u32>::new(); 2]);

    let past_the_end = Snapshot::of(document, None, 10..20).build();
    assert_eq!(past_the_end.start, 3);
    assert!(past_the_end.lines.is_empty());
  }
}
