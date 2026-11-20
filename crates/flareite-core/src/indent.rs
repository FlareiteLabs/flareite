//! A document's indentation, where it came from, and what saving tidies.
//!
//! Each document has its own tab size and choice of spaces or tabs. They
//! come from the settings unless something more specific said otherwise:
//! the file's `.editorconfig`, its content ([`detect_indentation`]), or the
//! user (the status bar's indentation picker). Settings changes reach only
//! the documents whose indentation is still the settings' (ADR 0054).

use std::ops::Range;

use flareite_text::{Edit, TextBuffer};

use crate::document::Document;
use crate::error::CoreError;

/// Lines looked at to detect indentation.
const DETECT_LINES: usize = 10_000;
/// Widest indentation step that's counted, in spaces.
const MAX_STEP: usize = 8;
/// Tab sizes a detected step can be, the usual ones first (they win ties).
const SIZES: [usize; 7] = [4, 2, 8, 3, 6, 5, 7];

/// Where a document's indentation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndentSource {
  /// `editor.tab_size` and `editor.insert_spaces`.
  #[default]
  Settings,
  /// The file's content.
  Detected,
  /// The file's `.editorconfig`.
  EditorConfig,
  /// The user chose it for this document.
  Chosen,
}

/// How a document indents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indentation {
  /// Columns between tab stops, and spaces Tab inserts when inserting spaces.
  pub tab_size: usize,
  pub insert_spaces: bool,
  pub source: IndentSource,
}

/// What a file's content says about its indentation: whether it indents
/// with spaces, and if so by how many (`None` if no step stood out, or for
/// tabs, whose width the content doesn't tell).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetectedIndent {
  pub insert_spaces: bool,
  pub tab_size: Option<usize>,
}

/// Guesses how `buffer` is indented from its first lines, as VS Code's
/// `detectIndentation` does: by whether more lines start with tabs or with
/// spaces, and by the most common step between the space indentation of
/// neighbouring lines. `None` if no line is indented.
pub fn detect_indentation(buffer: &TextBuffer) -> Option<DetectedIndent> {
  let mut tab_lines = 0usize;
  let mut space_lines = 0usize;
  let mut steps = [0usize; MAX_STEP + 1];
  let mut previous: Option<usize> = None;
  for line in 0..buffer.line_count().min(DETECT_LINES) {
    let start = buffer.line_start(line);
    let end = buffer.line_end(line);
    let mut spaces = 0usize;
    let mut tab = false;
    let mut blank = true;
    for ch in buffer.chars_at(start).take(end - start) {
      match ch {
        ' ' if !tab => spaces += 1,
        '\t' if spaces == 0 => tab = true,
        ' ' | '\t' => {}
        _ => {
          blank = false;
          break;
        }
      }
    }
    if blank {
      // Blank lines don't break a run of indentation.
      continue;
    }
    if tab {
      tab_lines += 1;
      previous = None;
      continue;
    }
    if spaces > 1 {
      space_lines += 1;
    }
    if let Some(before) = previous {
      let step = spaces.abs_diff(before);
      // One space is usually alignment (a comment's ` *`), not a level.
      if (2..=MAX_STEP).contains(&step) {
        steps[step] += 1;
      }
    }
    previous = Some(spaces);
  }
  if tab_lines == 0 && space_lines == 0 {
    return None;
  }
  if tab_lines > space_lines {
    return Some(DetectedIndent {
      insert_spaces: false,
      tab_size: None,
    });
  }
  // The commonest step; ties go to the usual sizes first.
  let tab_size = SIZES
    .iter()
    .enumerate()
    .filter(|&(_, &step)| steps[step] > 0)
    .max_by_key(|&(order, &step)| (steps[step], std::cmp::Reverse(order)))
    .map(|(_, &step)| step);
  Some(DetectedIndent {
    insert_spaces: true,
    tab_size,
  })
}

/// What saving tidies in a document (ADR 0054).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SaveActions {
  /// Removes spaces and tabs at the ends of lines.
  pub trim_trailing_whitespace: bool,
  /// Ends the text with a line break, if it has any text.
  pub insert_final_newline: bool,
  /// Removes empty lines after the last line break.
  pub trim_final_newlines: bool,
}

impl SaveActions {
  pub fn any(self) -> bool {
    self.trim_trailing_whitespace || self.insert_final_newline || self.trim_final_newlines
  }
}

/// The edits `actions` make to `text`, in order, ending lines with `ending`.
fn save_edits(text: &str, actions: SaveActions, ending: &str) -> Vec<Edit> {
  let mut edits = Vec::new();
  if actions.trim_trailing_whitespace {
    let bytes = text.as_bytes();
    let mut line_start = 0;
    let mut at = 0;
    while at <= bytes.len() {
      let end_of_line = at == bytes.len() || bytes[at] == b'\n' || bytes[at] == b'\r';
      if end_of_line {
        let mut trimmed = at;
        while trimmed > line_start && matches!(bytes[trimmed - 1], b' ' | b'\t') {
          trimmed -= 1;
        }
        if trimmed < at {
          edits.push(Edit::new(trimmed..at, String::new()));
        }
        if at < bytes.len() && bytes[at] == b'\r' && bytes.get(at + 1) == Some(&b'\n') {
          at += 1;
        }
        line_start = at + 1;
      }
      at += 1;
    }
  }
  let content_end = text.trim_end_matches(['\n', '\r', ' ', '\t']).len();
  // Where the text's last line break, after its last character, ends.
  let breaks = &text[content_end..];
  let first_break = breaks.find(['\n', '\r']).map(|at| {
    let end = content_end + at;
    end
      + if text[end..].starts_with("\r\n") {
        2
      } else {
        1
      }
  });
  if actions.trim_final_newlines
    && let Some(keep) = first_break
    && keep < text.len()
    && content_end > 0
  {
    let removed: Range<usize> = keep..text.len();
    // Trailing whitespace on the removed lines is already being removed.
    edits.retain(|edit| edit.range.start < removed.start);
    edits.push(Edit::new(removed, String::new()));
  }
  if actions.insert_final_newline && content_end > 0 && first_break.is_none() {
    edits.push(Edit::new(text.len()..text.len(), ending.to_owned()));
  }
  edits
}

impl Document {
  /// How the document indents, and where that came from.
  pub fn indent_style(&self) -> Indentation {
    Indentation {
      tab_size: self.tab_size,
      insert_spaces: self.insert_spaces,
      source: self.indent_source,
    }
  }

  /// Indents with `tab_size` and spaces or tabs from now on, as `source`
  /// said. Settings changes then reach the document only if `source` is
  /// [`IndentSource::Settings`].
  pub fn set_indentation_from(
    &mut self,
    tab_size: usize,
    insert_spaces: bool,
    source: IndentSource,
  ) {
    self.indent_source = source;
    self.set_indentation(tab_size, insert_spaces);
  }

  /// What the content says about the document's indentation.
  pub fn detect_indentation(&self) -> Option<DetectedIndent> {
    detect_indentation(&self.buffer)
  }

  /// Tidies the text as `actions` say, as one undo step. Returns whether it
  /// changed. A read-only document isn't changed.
  pub fn apply_save_actions(&mut self, actions: SaveActions) -> Result<bool, CoreError> {
    if !actions.any() || self.is_read_only() {
      return Ok(false);
    }
    let edits = save_edits(&self.buffer.contents(), actions, self.line_ending.as_str());
    if edits.is_empty() {
      return Ok(false);
    }
    self.apply_edits(edits)
  }
}

#[cfg(test)]
mod tests {
  use flareite_text::ChangeSet;

  use super::*;

  fn detect(text: &str) -> Option<DetectedIndent> {
    detect_indentation(&TextBuffer::from_text(text))
  }

  fn spaces(tab_size: Option<usize>) -> Option<DetectedIndent> {
    Some(DetectedIndent {
      insert_spaces: true,
      tab_size,
    })
  }

  #[test]
  fn detects_spaces_by_their_commonest_step() {
    assert_eq!(
      detect("fn a() {\n  b();\n  if c {\n    d();\n  }\n}\n"),
      spaces(Some(2))
    );
    assert_eq!(
      detect("class A:\n    def b(self):\n        pass\n\n    def c(self):\n        pass\n"),
      spaces(Some(4))
    );
  }

  #[test]
  fn detects_tabs_without_a_size() {
    assert_eq!(
      detect("fn a() {\n\tb();\n\tif c {\n\t\td();\n\t}\n}\n"),
      Some(DetectedIndent {
        insert_spaces: false,
        tab_size: None
      })
    );
  }

  #[test]
  fn alignment_by_one_space_and_unindented_text_say_nothing() {
    assert_eq!(detect("/*\n * a\n * b\n */\n"), None);
    assert_eq!(detect("one\ntwo\n"), None);
  }

  fn tidied(text: &str, actions: SaveActions) -> String {
    let mut buffer = TextBuffer::from_text(text);
    let edits = save_edits(text, actions, "\n");
    buffer
      .apply(&ChangeSet::new(edits).expect("in order"))
      .expect("applies");
    buffer.contents()
  }

  const ALL: SaveActions = SaveActions {
    trim_trailing_whitespace: true,
    insert_final_newline: true,
    trim_final_newlines: true,
  };

  #[test]
  fn trims_trailing_whitespace_in_any_line_ending() {
    let trim = SaveActions {
      trim_trailing_whitespace: true,
      ..SaveActions::default()
    };
    assert_eq!(tidied("a  \r\nb\t\n c \rd ", trim), "a\r\nb\n c\rd");
  }

  #[test]
  fn ends_with_exactly_one_line_break() {
    assert_eq!(tidied("a", ALL), "a\n");
    assert_eq!(tidied("a\n\n\n", ALL), "a\n");
    assert_eq!(tidied("a  \n  \n\n", ALL), "a\n");
    assert_eq!(tidied("a\r\n\r\n", ALL), "a\r\n");
    assert_eq!(tidied("", ALL), "", "an empty file stays empty");
    assert_eq!(tidied("\n\n", ALL), "\n\n", "as does one of blank lines");
  }

  #[test]
  fn each_action_alone() {
    let final_newline = SaveActions {
      insert_final_newline: true,
      ..SaveActions::default()
    };
    assert_eq!(tidied("a \n\n", final_newline), "a \n\n");
    assert_eq!(tidied("a ", final_newline), "a \n");
    let trim_final = SaveActions {
      trim_final_newlines: true,
      ..SaveActions::default()
    };
    assert_eq!(tidied("a\n\n", trim_final), "a\n");
    assert_eq!(tidied("a", trim_final), "a");
  }
}
