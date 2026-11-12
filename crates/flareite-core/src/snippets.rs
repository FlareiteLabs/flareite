//! Snippets: text with tab stops, as language servers' completions insert
//! them, in the LSP's (TextMate's) syntax: `$1`, `${1:placeholder}`,
//! `${1|one,two|}`, `$0` for the final cursor, and variables such as
//! `$TM_FILENAME` or `${TM_SELECTED_TEXT:default}`.
//!
//! Inserting one ([`Document::insert_snippet`]) inserts its text at every
//! selection and selects its first tab stop, at each of them. Tab and
//! Shift+Tab then move between the stops (a stop that appears several times
//! is selected everywhere, so typing changes them all), until the final
//! one; Escape, undo, or moving away ends it. The stops are byte ranges that
//! follow edits through the edit log.
//!
//! Not supported: transformations (`${1/(.*)/${1:/upcase}/}`, which insert
//! nothing), filling mirrors of a placeholder with its text before it's
//! edited, and offering a choice's other options (its first is inserted).

use std::ops::Range;

use flareite_text::{LineEnding, TextBuffer, Version};

use crate::document::Document;
use crate::error::CoreError;
use crate::history::EditKind;
use crate::selection::{Selection, Selections};

/// A parsed snippet: its text, and its tab stops in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
  pub text: String,
  /// Tab stops by number, each a byte range of `text`; `0` is the final
  /// cursor. A number may appear more than once.
  pub stops: Vec<(u32, Range<usize>)>,
}

struct Parser<'a> {
  chars: Vec<char>,
  at: usize,
  variable: &'a dyn Fn(&str) -> Option<String>,
  out: String,
  stops: Vec<(u32, Range<usize>)>,
  /// Unknown variables, inserted by name, which become tab stops.
  unknown: Vec<Range<usize>>,
}

impl Parser<'_> {
  fn peek(&self) -> Option<char> {
    self.chars.get(self.at).copied()
  }

  fn next(&mut self) -> Option<char> {
    let ch = self.peek()?;
    self.at += 1;
    Some(ch)
  }

  fn eat(&mut self, expected: char) -> bool {
    if self.peek() == Some(expected) {
      self.at += 1;
      true
    } else {
      false
    }
  }

  fn number(&mut self) -> Option<u32> {
    let start = self.at;
    while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
      self.at += 1;
    }
    self.chars[start..self.at]
      .iter()
      .collect::<String>()
      .parse()
      .ok()
  }

  fn name(&mut self) -> String {
    let start = self.at;
    while self
      .peek()
      .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
      self.at += 1;
    }
    self.chars[start..self.at].iter().collect()
  }

  /// Text up to the end of the snippet, or (`nested`) the `}` closing a
  /// placeholder, which is consumed.
  fn text(&mut self, nested: bool) {
    while let Some(ch) = self.next() {
      match ch {
        '\\' => match self.next() {
          Some(escaped @ ('$' | '}' | '\\')) => self.out.push(escaped),
          Some(other) => {
            self.out.push('\\');
            self.out.push(other);
          }
          None => self.out.push('\\'),
        },
        '}' if nested => return,
        '$' => self.dollar(),
        _ => self.out.push(ch),
      }
    }
  }

  /// What follows a `$`.
  fn dollar(&mut self) {
    match self.peek() {
      Some(ch) if ch.is_ascii_digit() => {
        let number = self.number().unwrap_or(0);
        let at = self.out.len();
        self.stops.push((number, at..at));
      }
      Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
        let name = self.name();
        self.variable_value(&name);
      }
      Some('{') => {
        let open = self.at;
        self.at += 1;
        match self.peek() {
          Some(ch) if ch.is_ascii_digit() => self.braced_stop(),
          Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => self.braced_variable(),
          _ => {
            // Not a snippet construct: kept as written.
            self.at = open;
            self.out.push('$');
          }
        }
      }
      _ => self.out.push('$'),
    }
  }

  /// `${1}`, `${1:placeholder}`, `${1|one,two|}`, or `${1/.../.../}`.
  fn braced_stop(&mut self) {
    let number = self.number().unwrap_or(0);
    let start = self.out.len();
    match self.next() {
      Some(':') => self.text(true),
      Some('|') => self.choice(),
      Some('/') => self.skip_transform(),
      // `}`, or the end of a broken snippet.
      _ => {}
    }
    self.stops.push((number, start..self.out.len()));
  }

  /// `${name}`, `${name:default}`, or `${name/.../.../}`.
  fn braced_variable(&mut self) {
    let name = self.name();
    match self.next() {
      Some(':') => {
        if let Some(value) = (self.variable)(&name) {
          // The default isn't used, but its stops are dropped with it.
          let (out, stops, unknown) = (
            std::mem::take(&mut self.out),
            std::mem::take(&mut self.stops),
            std::mem::take(&mut self.unknown),
          );
          self.text(true);
          (self.out, self.stops, self.unknown) = (out, stops, unknown);
          self.out.push_str(&value);
        } else {
          self.text(true);
        }
      }
      Some('/') => {
        self.skip_transform();
        self.variable_value(&name);
      }
      _ => self.variable_value(&name),
    }
  }

  fn variable_value(&mut self, name: &str) {
    match (self.variable)(name) {
      Some(value) => self.out.push_str(&value),
      None => {
        // As the LSP asks: the name, as a placeholder.
        let start = self.out.len();
        self.out.push_str(name);
        self.unknown.push(start..self.out.len());
      }
    }
  }

  /// A choice's options after `|`, up to `|}`: the first is inserted.
  fn choice(&mut self) {
    let mut first = true;
    while let Some(ch) = self.next() {
      match ch {
        '\\' => {
          if let Some(escaped) = self.next()
            && first
          {
            self.out.push(escaped);
          }
        }
        ',' => first = false,
        '|' if self.eat('}') => return,
        _ if first => self.out.push(ch),
        _ => {}
      }
    }
  }

  /// Skips a transformation after its first `/`, through its closing `}`.
  /// Its format may hold `${1:/upcase}` and the like.
  fn skip_transform(&mut self) {
    let mut slashes = 1;
    let mut depth = 0;
    while let Some(ch) = self.next() {
      match ch {
        '\\' => {
          self.next();
        }
        '$' if self.eat('{') => depth += 1,
        '}' if depth > 0 => depth -= 1,
        '/' if depth == 0 && slashes < 3 => slashes += 1,
        '}' if slashes >= 3 => return,
        _ => {}
      }
    }
  }
}

/// Parses `snippet`, with `variable` giving variables' values (`None` for
/// those it doesn't know, which are inserted by name as placeholders).
pub fn parse_snippet(snippet: &str, variable: &dyn Fn(&str) -> Option<String>) -> Snippet {
  let mut parser = Parser {
    chars: snippet.chars().collect(),
    at: 0,
    variable,
    out: String::new(),
    stops: Vec::new(),
    unknown: Vec::new(),
  };
  parser.text(false);
  let mut stops = parser.stops;
  let first = stops.iter().map(|(number, _)| *number).max().unwrap_or(0) + 1;
  stops.extend((first..).zip(parser.unknown));
  if !stops.iter().any(|(number, _)| *number == 0) {
    let end = parser.out.len();
    stops.push((0, end..end));
  }
  Snippet {
    text: parser.out,
    stops,
  }
}

impl Snippet {
  /// The snippet as inserted on a line indented with `indent`: its line
  /// breaks as `ending`, and (with `adjust`) each of its lines after the
  /// first indented like the line, and its tabs as `unit`, one level of
  /// indentation.
  pub fn adjusted(&self, indent: &str, unit: &str, ending: LineEnding, adjust: bool) -> Self {
    let mut text = String::with_capacity(self.text.len());
    // Where each byte offset of the snippet's text went.
    let mut moved = vec![0; self.text.len() + 1];
    let mut chars = self.text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
      moved[at] = text.len();
      match ch {
        '\r' | '\n' => {
          if ch == '\r'
            && let Some(&(next, '\n')) = chars.peek()
          {
            moved[next] = text.len();
            chars.next();
          }
          text.push_str(ending.as_str());
          if adjust {
            text.push_str(indent);
          }
        }
        '\t' if adjust => text.push_str(unit),
        _ => {
          moved[at + 1..at + ch.len_utf8()].fill(text.len());
          text.push(ch);
        }
      }
    }
    moved[self.text.len()] = text.len();
    Self {
      text,
      stops: self
        .stops
        .iter()
        .map(|(number, range)| (*number, moved[range.start]..moved[range.end]))
        .collect(),
    }
  }

  /// Where the final cursor goes.
  fn final_offset(&self) -> usize {
    self
      .stops
      .iter()
      .find(|(number, _)| *number == 0)
      .map_or(self.text.len(), |(_, range)| range.start)
  }

  /// The stops' numbers in the order Tab visits them: ascending, then `0`.
  fn order(&self) -> Vec<u32> {
    let mut numbers: Vec<u32> = self.stops.iter().map(|(number, _)| *number).collect();
    numbers.sort_unstable_by_key(|&number| (number == 0, number));
    numbers.dedup();
    numbers
  }
}

/// A snippet being filled in: its stops in the order Tab visits them, each
/// with its places in the document.
#[derive(Debug, Clone)]
pub(crate) struct SnippetSession {
  /// The text version `groups` is in.
  version: Version,
  groups: Vec<Vec<Range<usize>>>,
  /// The stop selected.
  current: usize,
}

impl SnippetSession {
  /// Follows the edits made since the session last looked.
  fn sync(&mut self, buffer: &TextBuffer) -> bool {
    let since = std::mem::replace(&mut self.version, buffer.version());
    if since == self.version {
      return true;
    }
    let Some(edits) = buffer.edits_since(since) else {
      return false;
    };
    for range in self.groups.iter_mut().flatten() {
      let start = edits
        .iter()
        .fold(range.start, |at, edit| edit.map_offset(at, false));
      let end = edits
        .iter()
        .fold(range.end, |at, edit| edit.map_offset(at, true));
      *range = start..end.max(start);
    }
    true
  }
}

/// The values of the snippet variables the editor knows, at `selection`.
fn variable(document: &Document, selection: &Range<usize>, name: &str) -> Option<String> {
  let buffer = document.buffer();
  let line = buffer.line_of(selection.start);
  let path = document.path();
  Some(match name {
    "TM_SELECTED_TEXT" => buffer.text_in(selection.clone()),
    "TM_CURRENT_LINE" => buffer.text_in(buffer.line_start(line)..buffer.line_end(line)),
    "TM_CURRENT_WORD" => buffer.text_in(buffer.word_at(selection.start)),
    "TM_LINE_INDEX" => line.to_string(),
    "TM_LINE_NUMBER" => (line + 1).to_string(),
    "TM_FILENAME" => path.and_then(|path| path.file_name()).map_or_else(
      || document.title(),
      |name| name.to_string_lossy().into_owned(),
    ),
    "TM_FILENAME_BASE" => path.and_then(|path| path.file_stem()).map_or_else(
      || document.title(),
      |name| name.to_string_lossy().into_owned(),
    ),
    "TM_DIRECTORY" => path?.parent()?.display().to_string(),
    "TM_FILEPATH" => path?.display().to_string(),
    _ => return None,
  })
}

impl Document {
  /// Inserts a snippet at every selection, replacing it, and selects its
  /// first tab stop (see the module's documentation). With
  /// `adjust_indentation`, its lines are indented like each selection's
  /// line and its tabs become the document's indentation.
  pub(crate) fn insert_snippet(
    &mut self,
    source: &str,
    adjust_indentation: bool,
  ) -> Result<bool, CoreError> {
    self.snippet = None;
    let unit = self.indent_unit();
    let ending = self.line_ending;
    let mut edits = Vec::new();
    let mut snippets = Vec::new();
    for selection in self.selections.iter() {
      let range = selection.range();
      let snippet = parse_snippet(source, &|name| variable(self, &range, name));
      let line = self.buffer.line_of(range.start);
      let line_start = self.buffer.line_start(line);
      let line_text = self.buffer.text_in(line_start..self.buffer.line_end(line));
      let indent: String = line_text
        .chars()
        .take_while(|ch| *ch == ' ' || *ch == '\t')
        .collect();
      let snippet = snippet.adjusted(&indent, &unit, ending, adjust_indentation);
      edits.push((range.clone(), snippet.text.clone(), snippet.final_offset()));
      snippets.push((range, snippet));
    }
    if !self.edit_selections_at(edits, EditKind::Other, false)? {
      return Ok(false);
    }
    // Where each snippet starts now: selections don't overlap, so each moves
    // by what the edits before it added.
    snippets.sort_by_key(|(range, _)| range.start);
    let mut shift: isize = 0;
    let mut placed = Vec::with_capacity(snippets.len());
    for (range, snippet) in snippets {
      let start = range.start.saturating_add_signed(shift);
      shift += snippet.text.len() as isize - range.len() as isize;
      placed.push((start, snippet));
    }
    let Some((_, first)) = placed.first() else {
      return Ok(true);
    };
    let order = first.order();
    // Only a final cursor: nothing to step through.
    if order.len() > 1 {
      let groups = order
        .iter()
        .map(|&number| {
          placed
            .iter()
            .flat_map(|(start, snippet)| {
              snippet
                .stops
                .iter()
                .filter(move |(n, _)| *n == number)
                .map(move |(_, range)| start + range.start..start + range.end)
            })
            .collect()
        })
        .collect();
      self.snippet = Some(SnippetSession {
        version: self.buffer.version(),
        groups,
        current: 0,
      });
      self.select_snippet_stop(0);
    }
    Ok(true)
  }

  /// Selects tab stop `index` of the session, ending it at the final stop.
  fn select_snippet_stop(&mut self, index: usize) {
    let Some(session) = &mut self.snippet else {
      return;
    };
    session.current = index;
    let ranges = session.groups[index].clone();
    let last = index + 1 == session.groups.len();
    if last {
      self.snippet = None;
    }
    if !ranges.is_empty() {
      let selections = ranges
        .iter()
        .map(|range| Selection::new(range.start, range.end))
        .collect();
      self.set_selections(Selections::new(selections, 0));
    }
  }

  /// Moves to the next (or previous) tab stop of the snippet being filled
  /// in, if the cursor is still in its current one. Returns whether it did,
  /// so Tab and Shift+Tab do what they otherwise do when not.
  pub(crate) fn step_snippet(&mut self, backward: bool) -> bool {
    let Some(session) = &mut self.snippet else {
      return false;
    };
    let head = self.selections.primary().head;
    let inside = session.sync(&self.buffer)
      && session.groups[session.current]
        .iter()
        .any(|range| range.start <= head && head <= range.end);
    if !inside {
      self.snippet = None;
      return false;
    }
    let current = session.current;
    if backward {
      if current > 0 {
        self.select_snippet_stop(current - 1);
      }
    } else if current + 1 < session.groups.len() {
      self.select_snippet_stop(current + 1);
    }
    true
  }

  /// Whether a snippet is being filled in.
  pub fn in_snippet(&self) -> bool {
    self.snippet.is_some()
  }

  /// Stops filling in a snippet; returns whether one was.
  pub(crate) fn end_snippet(&mut self) -> bool {
    self.snippet.take().is_some()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ops::{EditorOp, Motion};
  use crate::testing::{doc, render};

  fn known(name: &str) -> Option<String> {
    (name == "TM_FILENAME").then(|| "main.rs".into())
  }

  /// The snippet's text, and each stop's number and text.
  fn parsed(snippet: &str) -> (String, Vec<(u32, String)>) {
    let parsed = parse_snippet(snippet, &known);
    let stops = parsed
      .stops
      .iter()
      .map(|(number, range)| (*number, parsed.text[range.clone()].to_owned()))
      .collect();
    (parsed.text, stops)
  }

  fn stops(list: &[(u32, &str)]) -> Vec<(u32, String)> {
    list
      .iter()
      .map(|(number, text)| (*number, (*text).to_owned()))
      .collect()
  }

  fn insert(snippet: &str) -> EditorOp {
    EditorOp::InsertSnippet {
      snippet: snippet.into(),
      adjust_indentation: true,
    }
  }

  #[test]
  fn parses_tab_stops_and_placeholders() {
    assert_eq!(
      parsed("fn ${1:name}(${2:args}) {\n\t$0\n}"),
      (
        "fn name(args) {\n\t\n}".into(),
        stops(&[(1, "name"), (2, "args"), (0, "")])
      )
    );
    // A nested placeholder, and a mirror of the outer one (left empty).
    assert_eq!(
      parsed("${1:outer ${2:inner}}$1"),
      (
        "outer inner".into(),
        stops(&[(2, "inner"), (1, "outer inner"), (1, ""), (0, "")])
      )
    );
  }

  #[test]
  fn parses_choices_variables_and_escapes() {
    assert_eq!(parsed("${1|one,two|}").0, "one");
    assert_eq!(
      parsed("cost: \\$5 \\} $TM_FILENAME").0,
      "cost: $5 } main.rs"
    );
    // A known variable's default isn't used, nor the stops in it.
    assert_eq!(
      parsed("${TM_FILENAME:${1:unused}}"),
      ("main.rs".into(), stops(&[(0, "")]))
    );
    // Unknown variables are inserted by name, as placeholders after the
    // numbered stops; with a default, the default is.
    assert_eq!(
      parsed("$1 $NOPE ${MAYBE:default}"),
      (
        " NOPE default".into(),
        stops(&[(1, ""), (2, "NOPE"), (0, "")])
      )
    );
    // Transformations insert nothing; stray dollars are text.
    assert_eq!(parsed("${1/(.*)/${1:/upcase}/g}x").0, "x");
    assert_eq!(parsed("a $ b ${ c").0, "a $ b ${ c");
  }

  #[test]
  fn adjusts_indentation_and_line_endings() {
    let snippet = parse_snippet("if $1 {\n\t$0\n}", &known);
    let adjusted = snippet.adjusted("    ", "  ", LineEnding::CrLf, true);
    assert_eq!(adjusted.text, "if  {\r\n      \r\n    }");
    assert_eq!(adjusted.stops, vec![(1, 3..3), (0, 13..13)]);
    let plain = snippet.adjusted("    ", "  ", LineEnding::Lf, false);
    assert_eq!(plain.text, "if  {\n\t\n}");
  }

  #[test]
  fn tab_steps_through_the_stops_at_every_cursor() {
    let mut document = doc("let a = |\nlet b = |");
    document
      .apply(insert("f(${1:x}, ${2:y})$0"))
      .expect("applies");
    assert_eq!(render(&document), "let a = f([x], y)\nlet b = f([x], y)");
    assert!(document.in_snippet());

    // Typing replaces the stop everywhere; Tab moves on.
    document
      .apply(EditorOp::Insert("left".into()))
      .expect("applies");
    document.apply(EditorOp::Tab).expect("applies");
    assert_eq!(
      render(&document),
      "let a = f(left, [y])\nlet b = f(left, [y])"
    );
    // Shift+Tab goes back, to what was typed.
    document.apply(EditorOp::Outdent).expect("applies");
    assert_eq!(
      render(&document),
      "let a = f([left], y)\nlet b = f([left], y)"
    );
    document.apply(EditorOp::Tab).expect("applies");
    document.apply(EditorOp::Tab).expect("applies");
    // At the final stop the snippet is done, and Tab is a tab again.
    assert_eq!(
      render(&document),
      "let a = f(left, y)|\nlet b = f(left, y)|"
    );
    assert!(!document.in_snippet());
    document.apply(EditorOp::Tab).expect("applies");
    // Spaces to the next tab stop.
    assert_eq!(
      render(&document),
      "let a = f(left, y)  |\nlet b = f(left, y)  |"
    );
  }

  #[test]
  fn snippets_are_indented_like_their_line() {
    let mut document = doc("  call(|)");
    document.apply(insert("{\n\t$0\n}")).expect("applies");
    assert_eq!(render(&document), "  call({\n      |\n  })");
  }

  #[test]
  fn escape_or_moving_away_ends_the_snippet() {
    let mut document = doc("|");
    document
      .apply(insert("a(${1:x}, ${2:y})"))
      .expect("applies");
    assert!(document.in_snippet());
    document
      .apply(EditorOp::CollapseSelections)
      .expect("applies");
    assert!(!document.in_snippet(), "Escape ends it");
    assert_eq!(render(&document), "a([x], y)", "and keeps the selection");

    let mut document = doc("|");
    document
      .apply(insert("b(${1:x}, ${2:y})"))
      .expect("applies");
    document
      .apply(EditorOp::Move {
        motion: Motion::DocumentStart,
        extend: false,
      })
      .expect("applies");
    document.apply(EditorOp::Tab).expect("applies");
    assert!(!document.in_snippet());
    assert_eq!(render(&document), "    |b(x, y)", "Tab indents there");
  }

  #[test]
  fn undo_takes_back_the_whole_snippet_and_ends_it() {
    let mut document = doc("|");
    document.apply(insert("f(${1:x})")).expect("applies");
    document.apply(EditorOp::Undo).expect("applies");
    assert_eq!(render(&document), "|");
    assert!(!document.in_snippet());
  }
}
