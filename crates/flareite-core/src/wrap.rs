//! Word wrap: long lines broken into several display rows.
//!
//! A view wraps at a width in columns ([`WrapConfig`]), which the frontend
//! works out from its width and the editor's monospace font. A grapheme
//! cluster takes its Unicode width (CJK characters and most emoji 2,
//! combining marks nothing) and a tab advances to the next tab stop, so a
//! column is one `ch`. [`wrap_line`] says where a line's rows start.
//!
//! Most lines don't wrap, so a view keeps only those that do, with how many
//! extra rows each takes, like the hidden lines of folds. Edits wrap again
//! only the lines they replaced (through the buffer's line splices). Display
//! rows count both: a visible line takes one row plus its extra rows, and a
//! hidden line none (see [`crate::folds`]).

use std::ops::Range;

use flareite_text::{TextBuffer, Version, map_line_range};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::document::Document;
use crate::folds::Folds;
use crate::view::MAX_DISPLAY_LINE_BYTES;

/// How continuation rows are indented, as VS Code's `wrappingIndent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapIndent {
  /// At the start of the row.
  None,
  /// Like the line's first row.
  #[default]
  Same,
  /// One indentation level more than the line.
  Indent,
  /// Two levels more.
  DeepIndent,
}

/// Where a view wraps lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrapConfig {
  /// The width of a row, in columns. At least 1.
  pub columns: usize,
  pub indent: WrapIndent,
}

/// Where a line's rows start, after the first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineWrap {
  /// Byte offsets into the line, ascending; each starts a row.
  pub breaks: Vec<usize>,
  /// The indent of continuation rows, in columns.
  pub indent: usize,
}

impl LineWrap {
  /// The row of the line that byte offset `column` is on. A position at a
  /// break is on the row the break starts.
  pub fn row_of(&self, column: usize) -> usize {
    self.breaks.partition_point(|&start| start <= column)
  }

  /// Where row `row` of `text` (the line) starts and ends, in bytes.
  pub fn row_range(&self, row: usize, text_len: usize) -> Range<usize> {
    let start = row.checked_sub(1).map_or(0, |index| self.breaks[index]);
    start..self.breaks.get(row).copied().unwrap_or(text_len)
  }
}

/// Characters a row may end after (besides whitespace), and before.
const BREAK_AFTER: &[char] = &['}', ')', ']', '?', '|', '/', '&', '.', ',', ';'];
const BREAK_BEFORE: &[char] = &['(', '[', '{', '+'];

fn is_space(grapheme: &str) -> bool {
  grapheme.chars().all(char::is_whitespace)
}

/// The width of `grapheme` at visual column `column` (for tab stops).
fn width(grapheme: &str, column: usize, tab_size: usize) -> usize {
  if grapheme == "\t" {
    return tab_size - column % tab_size;
  }
  // Printable ASCII and control characters both take one column.
  if grapheme.len() == 1 {
    return 1;
  }
  match grapheme.width() {
    // Control characters still take room where they're drawn.
    0 if grapheme.chars().any(char::is_control) => 1,
    width => width,
  }
}

/// The width of `text`, with tab stops counted from its start.
pub(crate) fn text_width(text: &str, tab_size: usize) -> usize {
  let tab_size = tab_size.max(1);
  text.graphemes(true).fold(0, |column, grapheme| {
    column + width(grapheme, column, tab_size)
  })
}

/// The byte offset in `text` whose width from the start (tab stops counted
/// from it) is closest to `x` without passing it.
pub(crate) fn offset_at_width(text: &str, x: usize, tab_size: usize) -> usize {
  let tab_size = tab_size.max(1);
  let mut column = 0;
  for (offset, grapheme) in text.grapheme_indices(true) {
    let next = column + width(grapheme, column, tab_size);
    if next > x {
      return offset;
    }
    column = next;
  }
  text.len()
}

/// Whether a row may start at a grapheme after `previous`.
fn may_break_between(previous: &str, next: &str) -> bool {
  if is_space(next) {
    return false;
  }
  if is_space(previous) {
    return true;
  }
  let ends =
    |grapheme: &str, set: &[char]| grapheme.chars().next().is_some_and(|c| set.contains(&c));
  ends(previous, BREAK_AFTER)
    || ends(next, BREAK_BEFORE)
    || previous.width() > 1
    || next.width() > 1
}

/// Whether `text` might be wider than `columns`: a character's UTF-8 length
/// is never less than its width, and a tab adds at most `tab_size - 1`.
fn might_overflow(text: &str, columns: usize, tab_size: usize) -> bool {
  let tabs = text.bytes().filter(|&byte| byte == b'\t').count();
  text.len() + tabs * (tab_size - 1) > columns
}

/// Where `text` (a line, without its line break) wraps at `columns`.
///
/// A row ends after spaces or tabs, after one of `})]?|/&.,;`, before one of
/// `([{+`, or beside a wide character: at the last such place that fits, or
/// else after the last grapheme that fits. Spaces and tabs at the end of a
/// row hang past the width. Continuation rows are indented as `indent`
/// says, unless that would take more than half the width, and their tab
/// stops count from their first character.
pub fn wrap_line(text: &str, columns: usize, tab_size: usize, indent: WrapIndent) -> LineWrap {
  let columns = columns.max(1);
  let tab_size = tab_size.max(1);
  if !might_overflow(text, columns, tab_size) {
    return LineWrap::default();
  }
  let leading = text
    .bytes()
    .take_while(|&byte| byte == b' ' || byte == b'\t')
    .fold(0, |column, byte| {
      column
        + if byte == b'\t' {
          tab_size - column % tab_size
        } else {
          1
        }
    });
  let wanted = match indent {
    WrapIndent::None => 0,
    WrapIndent::Same => leading,
    WrapIndent::Indent => leading + tab_size,
    WrapIndent::DeepIndent => leading + 2 * tab_size,
  };
  let indent = if wanted * 2 > columns { 0 } else { wanted };

  // In ASCII, each byte is a grapheme (a line holds no CRLF), so rows are
  // found byte by byte; segmenting is most of the work otherwise.
  let ascii = text.is_ascii();
  let graphemes: Vec<(usize, &str)> = if ascii {
    Vec::new()
  } else {
    text.grapheme_indices(true).collect()
  };
  let mut breaks = Vec::new();
  let mut start = 0;
  loop {
    // The first row has the whole width; the rest less their indent (at
    // most half of it).
    let room = if breaks.is_empty() {
      columns
    } else {
      columns - indent
    };
    let next = if ascii {
      next_ascii_row(text.as_bytes(), start, room, tab_size)
    } else {
      next_row(&graphemes, start, room, tab_size)
    };
    let Some(next) = next else {
      break;
    };
    breaks.push(if ascii { next } else { graphemes[next].0 });
    start = next;
  }
  LineWrap { breaks, indent }
}

/// [`next_row`] for ASCII text, by byte.
fn next_ascii_row(bytes: &[u8], start: usize, room: usize, tab_size: usize) -> Option<usize> {
  let space = |byte: u8| matches!(byte, b' ' | b'\t' | b'\x0b' | b'\x0c');
  let ascii = |set: &[char], byte: u8| set.contains(&char::from(byte));
  let mut column = 0;
  let mut opportunity = None;
  for (index, &byte) in bytes.iter().enumerate().skip(start) {
    if index > start && !space(byte) {
      let previous = bytes[index - 1];
      if space(previous) || ascii(BREAK_AFTER, previous) || ascii(BREAK_BEFORE, byte) {
        opportunity = Some(index);
      }
    }
    let w = if byte == b'\t' {
      tab_size - column % tab_size
    } else {
      1
    };
    if index > start && column + w > room && !space(byte) {
      return Some(opportunity.unwrap_or(index));
    }
    column += w;
  }
  None
}

/// Where the row of `graphemes` starting at `start` ends, `room` columns
/// wide: the index of the grapheme that starts the next row, or `None` if
/// the rest fits.
fn next_row(
  graphemes: &[(usize, &str)],
  start: usize,
  room: usize,
  tab_size: usize,
) -> Option<usize> {
  let mut column = 0;
  let mut opportunity = None;
  for (index, &(_, grapheme)) in graphemes.iter().enumerate().skip(start) {
    if index > start && may_break_between(graphemes[index - 1].1, grapheme) {
      opportunity = Some(index);
    }
    let w = width(grapheme, column, tab_size);
    if index > start && column + w > room && !is_space(grapheme) {
      return Some(opportunity.unwrap_or(index));
    }
    column += w;
  }
  None
}

/// A visible wrapped line, placed among the display rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
  line: usize,
  extra: usize,
  /// The display row of its first row.
  row: usize,
  /// Extra rows of the visible wrapped lines before it.
  before: usize,
}

/// A view's wrapped lines.
#[derive(Debug, Clone)]
pub(crate) struct Wraps {
  config: Option<WrapConfig>,
  /// The tab size the lines were wrapped with.
  tab_size: usize,
  /// The text version `wrapped` refers to.
  version: Version,
  /// Lines that wrap and their extra rows, sorted by line. Hidden lines
  /// are included.
  wrapped: Vec<(usize, usize)>,
  /// The visible ones among the display rows; see [`Self::place`].
  placed: Vec<Placed>,
  /// The folds' generation `placed` was built with, and whether `wrapped`
  /// changed since.
  placed_for: Option<u64>,
}

impl Wraps {
  pub(crate) fn new(version: Version) -> Self {
    Self {
      config: None,
      tab_size: 4,
      version,
      wrapped: Vec::new(),
      placed: Vec::new(),
      placed_for: None,
    }
  }

  pub(crate) fn config(&self) -> Option<WrapConfig> {
    self.config
  }

  fn wrap(&self, buffer: &TextBuffer, line: usize) -> LineWrap {
    match self.config {
      Some(config) => {
        let (text, _) = buffer.line_prefix(line, MAX_DISPLAY_LINE_BYTES);
        wrap_line(&text, config.columns, self.tab_size, config.indent)
      }
      None => LineWrap::default(),
    }
  }

  /// Wraps at `config` (or not at all), with `tab_size`: everything again,
  /// if either changed.
  pub(crate) fn configure(
    &mut self,
    buffer: &TextBuffer,
    config: Option<WrapConfig>,
    tab_size: usize,
  ) {
    let config = config.map(|config| WrapConfig {
      columns: config.columns.max(1),
      ..config
    });
    if config == self.config && tab_size == self.tab_size {
      return;
    }
    self.config = config;
    self.tab_size = tab_size.max(1);
    self.rewrap_all(buffer);
  }

  fn rewrap_all(&mut self, buffer: &TextBuffer) {
    self.version = buffer.version();
    self.wrapped.clear();
    self.placed_for = None;
    let Some(config) = self.config else {
      return;
    };
    for line in long_lines(buffer, config.columns, self.tab_size) {
      let extra = self.wrap(buffer, line).breaks.len();
      if extra > 0 {
        self.wrapped.push((line, extra));
      }
    }
  }

  /// Follows the edits made since the last call: wraps the lines they
  /// replaced again, and moves the rest.
  pub(crate) fn sync(&mut self, buffer: &TextBuffer, tab_size: usize) {
    if tab_size.max(1) != self.tab_size {
      self.tab_size = tab_size.max(1);
      self.rewrap_all(buffer);
      return;
    }
    let since = std::mem::replace(&mut self.version, buffer.version());
    if since == self.version || self.config.is_none() {
      return;
    }
    let Some(splices) = buffer.line_changes_since(since) else {
      self.rewrap_all(buffer);
      return;
    };
    let mut dirty: Vec<Range<usize>> = Vec::new();
    for splice in &splices {
      let removed_end = splice.start + splice.removed;
      self.wrapped.retain_mut(|(line, _)| {
        if *line < splice.start {
          true
        } else if *line < removed_end {
          false
        } else {
          *line = *line + splice.inserted - splice.removed;
          true
        }
      });
      dirty = dirty
        .into_iter()
        .flat_map(|range| map_line_range(range, std::slice::from_ref(splice)))
        .collect();
      dirty.push(splice.start..splice.start + splice.inserted);
    }
    let line_count = buffer.line_count();
    let mut lines: Vec<usize> = dirty
      .into_iter()
      .flat_map(|range| range.start.min(line_count)..range.end.min(line_count))
      .collect();
    lines.sort_unstable();
    lines.dedup();
    for line in lines {
      let extra = self.wrap(buffer, line).breaks.len();
      let index = self.wrapped.partition_point(|&(wrapped, _)| wrapped < line);
      match self.wrapped.get(index) {
        Some(&(wrapped, _)) if wrapped == line => {
          if extra > 0 {
            self.wrapped[index].1 = extra;
          } else {
            self.wrapped.remove(index);
          }
        }
        _ if extra > 0 => self.wrapped.insert(index, (line, extra)),
        _ => {}
      }
    }
    self.placed_for = None;
  }

  /// Places the visible wrapped lines among the display rows, if the
  /// wrapped lines or the folds changed since the last time.
  pub(crate) fn place(&mut self, folds: &Folds) {
    if self.placed_for == Some(folds.generation()) {
      return;
    }
    self.placed.clear();
    let mut before = 0;
    for &(line, extra) in &self.wrapped {
      if folds.hidden_at(line).is_some() {
        continue;
      }
      self.placed.push(Placed {
        line,
        extra,
        row: folds.row_of(line) + before,
        before,
      });
      before += extra;
    }
    self.placed_for = Some(folds.generation());
  }

  /// Extra rows of the visible wrapped lines before `line`.
  fn extra_before(&self, line: usize) -> usize {
    let index = self.placed.partition_point(|placed| placed.line < line);
    index
      .checked_sub(1)
      .map_or(0, |last| self.placed[last].before + self.placed[last].extra)
  }

  fn total_extra(&self) -> usize {
    self
      .placed
      .last()
      .map_or(0, |last| last.before + last.extra)
  }

  /// Whether `line` is a visible line that wraps.
  fn wraps(&self, line: usize) -> bool {
    self
      .placed
      .binary_search_by_key(&line, |placed| placed.line)
      .is_ok()
  }
}

/// Lines of `buffer` that might be wider than `columns`, in order: a scan of
/// the text's line lengths (see [`might_overflow`]).
fn long_lines(buffer: &TextBuffer, columns: usize, tab_size: usize) -> Vec<usize> {
  let mut long = Vec::new();
  let mut line = 0;
  let mut length = 0;
  let mut tabs = 0;
  let mut after_cr = false;
  let mut end_line = |line: &mut usize, length: &mut usize, tabs: &mut usize| {
    if *length + *tabs * (tab_size - 1) > columns {
      long.push(*line);
    }
    *line += 1;
    *length = 0;
    *tabs = 0;
  };
  for chunk in buffer.chunks_in(0..buffer.len()) {
    for byte in chunk.bytes() {
      match byte {
        // The LF of a CRLF ends nothing more.
        b'\n' if after_cr => {}
        b'\n' | b'\r' => end_line(&mut line, &mut length, &mut tabs),
        b'\t' => {
          length += 1;
          tabs += 1;
        }
        _ => length += 1,
      }
      after_cr = byte == b'\r';
    }
  }
  end_line(&mut line, &mut length, &mut tabs);
  long
}

impl Document {
  /// Wraps lines at `config`, or stops wrapping (`None`), in the view in
  /// use. Nothing changes if it already does.
  pub fn set_wrap(&mut self, config: Option<WrapConfig>) {
    self.wraps.configure(&self.buffer, config, self.tab_size);
    self.wraps.place(&self.folds);
  }

  /// Where the view in use wraps lines, if it does.
  pub fn wrap_config(&self) -> Option<WrapConfig> {
    self.wraps.config()
  }

  /// Where `line` wraps, if it is a visible line that does.
  pub fn line_wrap(&self, line: usize) -> Option<LineWrap> {
    self
      .wraps
      .wraps(line)
      .then(|| self.wraps.wrap(&self.buffer, line))
  }

  /// Display rows: visible lines, plus the extra rows of wrapped ones.
  pub fn row_count(&self) -> usize {
    self.folds.row_count(self.buffer.line_count()) + self.wraps.total_extra()
  }

  /// The display row of `line`'s first row. A hidden line is on its
  /// region's first row.
  pub fn row_of(&self, line: usize) -> usize {
    let line = line.min(self.buffer.line_count() - 1);
    let shown = self
      .folds
      .hidden_at(line)
      .map_or(line, |hidden| hidden.start - 1);
    self.folds.row_of(shown) + self.wraps.extra_before(shown)
  }

  /// The display row of `offset`: its line's row, or the row of the line
  /// it's on if the line wraps.
  pub fn row_of_offset(&self, offset: usize) -> usize {
    let line = self.buffer.line_of(offset.min(self.buffer.len()));
    let row = self.row_of(line);
    match self.line_wrap(line) {
      Some(wrap) => row + wrap.row_of(offset - self.buffer.line_start(line)),
      None => row,
    }
  }

  /// The line shown on display row `row` (clamped to the last row), and
  /// which of its rows it is.
  pub fn position_at_row(&self, row: usize) -> (usize, usize) {
    let row = row.min(self.row_count() - 1);
    let placed = &self.wraps.placed;
    let index = placed.partition_point(|placed| placed.row <= row);
    if let Some(last) = index.checked_sub(1).map(|index| placed[index]) {
      if row <= last.row + last.extra {
        return (last.line, row - last.row);
      }
      return (self.folds.line_at_row(row - last.before - last.extra), 0);
    }
    (self.folds.line_at_row(row), 0)
  }

  /// The line shown on display row `row` (clamped to the last row).
  pub fn line_at_row(&self, row: usize) -> usize {
    self.position_at_row(row).0
  }

  /// The horizontal position of `offset` in its row, in columns: from the
  /// row's start, plus a continuation row's indent. Positions past what's
  /// shown of a long line count as its end.
  pub(crate) fn row_x(&self, offset: usize) -> usize {
    let line = self.buffer.line_of(offset);
    let (text, _) = self.buffer.line_prefix(line, MAX_DISPLAY_LINE_BYTES);
    let column = (offset - self.buffer.line_start(line)).min(text.len());
    let column = (0..=column)
      .rev()
      .find(|&at| text.is_char_boundary(at))
      .unwrap_or(0);
    let wrap = self.line_wrap(line).unwrap_or_default();
    let row = wrap.row_of(column);
    let start = wrap.row_range(row, text.len()).start;
    let indent = if row > 0 { wrap.indent } else { 0 };
    indent + text_width(&text[start..column], self.tab_size)
  }

  /// The offset on display row `row` closest to horizontal position `x`
  /// (see [`Self::row_x`]) without passing it. On a row that isn't its
  /// line's last, that's at most its last character's start, so the
  /// position stays on the row.
  pub(crate) fn offset_at_row_x(&self, row: usize, x: usize) -> usize {
    let (line, row) = self.position_at_row(row);
    let line_start = self.buffer.line_start(line);
    let (text, _) = self.buffer.line_prefix(line, MAX_DISPLAY_LINE_BYTES);
    let wrap = self.line_wrap(line).unwrap_or_default();
    let range = wrap.row_range(row, text.len());
    let indent = if row > 0 { wrap.indent } else { 0 };
    let row_text = &text[range.clone()];
    let mut offset = offset_at_width(row_text, x.saturating_sub(indent), self.tab_size);
    if row < wrap.breaks.len() && offset == row_text.len() {
      offset = row_text
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(last, _)| last);
    }
    if row == wrap.breaks.len() && offset == row_text.len() {
      // The last row's end is the line's, even past what's shown.
      return self.buffer.line_end(line);
    }
    line_start + range.start + offset
  }
}

#[cfg(test)]
// Line ranges are naturally single-element arrays here.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
  use super::*;
  use crate::document::{DocumentId, EditorConfig};
  use crate::ops::{EditorOp, Motion};
  use crate::selection::Selections;

  fn rows(text: &str, columns: usize, indent: WrapIndent) -> Vec<String> {
    let wrap = wrap_line(text, columns, 4, indent);
    (0..=wrap.breaks.len())
      .map(|row| text[wrap.row_range(row, text.len())].to_owned())
      .collect()
  }

  fn doc(text: &str, columns: usize) -> Document {
    let mut document = Document::new(DocumentId(1), text, None, 1, &EditorConfig::default());
    document
      .apply(EditorOp::SetWrap(Some(WrapConfig {
        columns,
        indent: WrapIndent::None,
      })))
      .expect("applies");
    document
  }

  #[test]
  fn lines_wrap_after_spaces_with_the_spaces_hanging() {
    assert_eq!(
      rows("the quick brown fox", 10, WrapIndent::None),
      ["the quick ", "brown fox"]
    );
    // Spaces past the width stay on the row they end.
    assert_eq!(
      rows("aaaa bbbb      cccc", 9, WrapIndent::None),
      ["aaaa bbbb      ", "cccc"]
    );
    assert_eq!(rows("short", 10, WrapIndent::None), ["short"]);
  }

  #[test]
  fn words_longer_than_a_row_break_where_they_must() {
    assert_eq!(
      rows("abcdefghijkl", 5, WrapIndent::None),
      ["abcde", "fghij", "kl"]
    );
    // After punctuation, and before an opening bracket.
    assert_eq!(
      rows("foo.bar.baz", 8, WrapIndent::None),
      ["foo.bar.", "baz"]
    );
    assert_eq!(
      rows("call(argument)", 8, WrapIndent::None),
      ["call", "(argumen", "t)"]
    );
  }

  #[test]
  fn wide_characters_take_two_columns_and_break_anywhere() {
    // Six CJK characters are twelve columns.
    assert_eq!(
      rows("日本語の文章", 8, WrapIndent::None),
      ["日本語の", "文章"]
    );
    assert_eq!(text_width("日本", 4), 4);
    assert_eq!(
      text_width("e\u{301}", 4),
      1,
      "a combining mark adds nothing"
    );
  }

  #[test]
  fn continuation_rows_are_indented_as_asked() {
    let text = "    one two three four";
    let wrap = wrap_line(text, 12, 4, WrapIndent::Same);
    assert_eq!(wrap.indent, 4);
    assert_eq!(
      rows(text, 12, WrapIndent::Same),
      ["    one two ", "three ", "four"]
    );
    assert_eq!(wrap_line(text, 20, 4, WrapIndent::Indent).indent, 8);
    assert_eq!(wrap_line(text, 20, 4, WrapIndent::None).indent, 0);
    // More than half the width is dropped.
    assert_eq!(wrap_line(text, 12, 4, WrapIndent::DeepIndent).indent, 0);
  }

  #[test]
  fn tabs_advance_to_tab_stops() {
    assert_eq!(text_width("\tx", 4), 5);
    assert_eq!(text_width("ab\tc", 4), 5);
    assert_eq!(rows("a\tb\tc\td", 8, WrapIndent::None), ["a\tb\t", "c\td"]);
  }

  #[test]
  fn rows_count_wrapped_lines() {
    let document = doc("one two three\nshort\nfour five six\n", 8);
    // "one two " "three" / "short" / "four " "five six" / ""
    assert_eq!(document.row_count(), 6);
    assert_eq!(document.row_of(0), 0);
    assert_eq!(document.row_of(1), 2);
    assert_eq!(document.row_of(2), 3);
    assert_eq!(document.row_of(3), 5);
    assert_eq!(document.position_at_row(1), (0, 1));
    assert_eq!(document.position_at_row(2), (1, 0));
    assert_eq!(document.position_at_row(4), (2, 1));
    assert_eq!(document.position_at_row(5), (3, 0));
    assert_eq!(
      document.position_at_row(99),
      (3, 0),
      "clamped to the last row"
    );
    // "three" starts row 1 of line 0.
    assert_eq!(document.row_of_offset(8), 1);
    assert_eq!(document.row_of_offset(7), 0);
  }

  #[test]
  fn folded_lines_take_no_rows() {
    let mut document = doc("head\none two three\nlast word here\nend\n", 8);
    assert_eq!(document.row_count(), 8);
    document.set_folded([0..2]);
    // "head" / "last " "word " "here" / "end" / ""
    assert_eq!(document.row_count(), 6);
    assert_eq!(
      document.row_of(1),
      0,
      "a hidden line is on its region's first row"
    );
    assert_eq!(document.row_of(2), 1);
    assert_eq!(document.position_at_row(3), (2, 2));
    assert_eq!(document.position_at_row(4), (3, 0));
  }

  #[test]
  fn edits_wrap_only_the_lines_they_touch() {
    let mut document = doc("a\nb\nc\n", 8);
    assert_eq!(document.row_count(), 4);
    document.selections = Selections::cursor(document.buffer().line_start(1));
    document
      .apply(EditorOp::Insert("one two three ".into()))
      .expect("applies");
    assert_eq!(document.row_count(), 5);
    assert_eq!(document.row_of(2), 3);
    // A new line above moves the wrapped line down.
    document.selections = Selections::cursor(0);
    document.apply(EditorOp::Newline).expect("applies");
    assert_eq!(document.row_of(2), 2);
    assert_eq!(document.row_of(3), 4);
    assert_eq!(document.row_count(), 6);
    // Undoing both leaves no wrapped line.
    document.apply(EditorOp::Undo).expect("applies");
    document.apply(EditorOp::Undo).expect("applies");
    assert_eq!(document.row_count(), 4);
  }

  #[test]
  fn changing_the_width_wraps_everything_again() {
    let mut document = doc("one two three four\n", 8);
    assert_eq!(document.row_count(), 4);
    document
      .apply(EditorOp::SetWrap(Some(WrapConfig {
        columns: 40,
        indent: WrapIndent::None,
      })))
      .expect("applies");
    assert_eq!(document.row_count(), 2);
    document.apply(EditorOp::SetWrap(None)).expect("applies");
    assert_eq!(document.row_count(), 2);
    assert_eq!(document.line_wrap(0), None);
  }

  #[test]
  fn up_and_down_move_by_rows() {
    let mut document = doc("one two three\nx\n", 8);
    document.selections = Selections::cursor(2);
    let down = EditorOp::Move {
      motion: Motion::Down,
      extend: false,
    };
    document.apply(down.clone()).expect("applies");
    // Row 1 is "three": column 2 is its "r".
    assert_eq!(document.selections.primary().head, 10);
    document.apply(down.clone()).expect("applies");
    assert_eq!(document.selections.primary().head, 15, "the end of `x`");
    let up = EditorOp::Move {
      motion: Motion::Up,
      extend: false,
    };
    document.apply(up.clone()).expect("applies");
    document.apply(up).expect("applies");
    assert_eq!(document.selections.primary().head, 2, "back at the goal");
  }

  #[test]
  fn the_end_of_a_row_stays_on_it() {
    let document = doc("one two three\n", 8);
    // Past the end of row 0 ("one two "): before its last character.
    assert_eq!(document.offset_at_row_x(0, 50), 7);
    assert_eq!(document.offset_at_row_x(1, 50), 13);
  }

  #[test]
  fn a_viewport_inside_a_wrapped_line_starts_at_its_first_row() {
    // "one two " "three " "four " "five" / "x" / ""
    let document = doc("one two three four five\nx\n", 8);
    let update = document.viewport_update(None, 2..5, 1000);
    assert_eq!(update.start_row, 0);
    assert_eq!(update.lines, [0..2]);
  }

  #[test]
  fn each_view_wraps_on_its_own() {
    let mut document = doc("one two three\n", 8);
    document.use_view(crate::document::ViewId::from_raw(1));
    assert_eq!(document.row_count(), 3, "a new view starts as a copy");
    document.apply(EditorOp::SetWrap(None)).expect("applies");
    assert_eq!(document.row_count(), 2);
    // An edit in this view reaches the other's wrapped lines.
    document.selections = Selections::cursor(0);
    document
      .apply(EditorOp::Insert("four five six\n".into()))
      .expect("applies");
    document.use_view(crate::document::ViewId::from_raw(0));
    assert_eq!(document.row_count(), 5);
    assert_eq!(document.row_of(1), 2);
  }

  #[test]
  fn long_lines_are_found_by_scanning() {
    let buffer = TextBuffer::from_text("short\r\nlong line here\rx\n\tabcdef\n");
    assert_eq!(long_lines(&buffer, 8, 4), [1, 3]);
  }
}
