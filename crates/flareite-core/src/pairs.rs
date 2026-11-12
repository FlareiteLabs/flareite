//! Brackets and quotes typed in pairs, as in VS Code: typing an opening
//! bracket or quote adds its closer after the cursor, typing the closer then
//! steps over it, Backspace between the two deletes both, typing one with
//! text selected wraps the selection, and Enter between brackets puts the
//! closer on a line of its own.
//!
//! Which pairs apply comes from the language at each cursor (its
//! [`LanguagePairs`]) and whether the cursor is in a string or comment
//! ([`Scope`]), which the caller works out from the syntax tree and passes in
//! a [`PairContext`]. The core remembers the closers it added
//! ([`AutoClosed`]) so that, by default, only those are typed over or deleted
//! with their opener.

use std::ops::Range;
use std::sync::Arc;

use flareite_text::{ByteEdit, ChangeSet, Edit, TextBuffer, Version};

use crate::document::Document;
use crate::error::CoreError;
use crate::history::EditKind;
use crate::ops::{Direction, Unit};
use crate::selection::{Selection, Selections};

/// Longest opener or closer, in characters.
pub const MAX_PAIR_CHARS: usize = 8;

/// What auto-closing applies before, by default: VS Code's
/// `autoCloseBefore`. Whitespace and the end of the line always count.
pub const DEFAULT_AUTO_CLOSE_BEFORE: &str = ";:.,=}])>";

/// Longest stretch of a line before the cursor searched for an unclosed
/// quote.
const MAX_QUOTE_SCAN_BYTES: usize = 4096;

/// Most added closers remembered at once; the oldest are forgotten first.
const MAX_AUTO_CLOSED: usize = 64;

/// A language's pairs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanguagePairs {
  /// Brackets: Enter after an opener indents the new line, and Enter
  /// between an opener and its closer puts the closer on a line of its own.
  pub brackets: Vec<(String, String)>,
  /// Pairs whose closer is added when the opener is typed.
  pub auto_closing: Vec<AutoClosingPair>,
  /// Pairs that wrap the selection when the opener is typed with text
  /// selected. Only single-character openers.
  pub surrounding: Vec<(String, String)>,
  /// Characters that a pair is auto-closed before, besides whitespace and
  /// the end of the line, and closers.
  pub auto_close_before: String,
}

/// A pair whose closer is added when its opener is typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoClosingPair {
  pub open: String,
  pub close: String,
  /// Not auto-closed in strings.
  pub not_in_string: bool,
  /// Not auto-closed in comments.
  pub not_in_comment: bool,
}

impl AutoClosingPair {
  pub fn new(open: &str, close: &str) -> Self {
    Self {
      open: open.to_owned(),
      close: close.to_owned(),
      not_in_string: false,
      not_in_comment: false,
    }
  }
}

/// What a cursor is in, as far as pairs care.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
  #[default]
  Code,
  String,
  Comment,
}

/// When brackets (or quotes) are auto-closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AutoClose {
  /// Before whitespace, the end of the line, closers, and the language's
  /// `auto_close_before` characters; quotes not after a word character.
  #[default]
  LanguageDefined,
  /// Before whitespace, the end of the line, and closers.
  BeforeWhitespace,
  /// Wherever the language's pairs allow.
  Always,
  Never,
}

/// Which closers are typed over (or deleted with their opener by
/// Backspace).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Closers {
  /// Only those that were added automatically.
  #[default]
  Auto,
  /// Any closer of one of the language's pairs.
  Always,
  Never,
}

/// Which pairs wrap a selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Surround {
  /// Both the quotes and the brackets the language declares.
  #[default]
  LanguageDefined,
  Quotes,
  Brackets,
  Never,
}

/// The user's settings for pairs, as VS Code's `editor.autoClosing*` and
/// `editor.autoSurround`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PairSettings {
  pub brackets: AutoClose,
  pub quotes: AutoClose,
  pub overtype: Closers,
  pub delete: Closers,
  pub surround: Surround,
}

/// The pairs that apply at one cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorPairs {
  /// Those of the language at the cursor (an injected one's, inside it).
  pub pairs: Arc<LanguagePairs>,
  pub scope: Scope,
}

/// What the pair-aware operations need besides the document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairContext {
  pub settings: PairSettings,
  /// For each selection, in order. A selection without one has no pairs.
  pub cursors: Vec<CursorPairs>,
}

impl PairContext {
  fn cursor(&self, index: usize) -> Option<&CursorPairs> {
    self.cursors.get(index)
  }
}

/// Whether `c` is a quote: quotes and brackets have settings of their own.
fn is_quote(c: char) -> bool {
  matches!(c, '\'' | '"' | '`')
}

/// Whether `c` is part of a word, by VS Code's default word separators.
fn is_word_char(c: char) -> bool {
  !c.is_whitespace() && !"`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?".contains(c)
}

fn is_line_break(c: char) -> bool {
  c == '\n' || c == '\r'
}

/// An opener and the closer added for it, as byte ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Closed {
  open: (usize, usize),
  close: (usize, usize),
}

impl Closed {
  /// Follows an edit. Returns false if the edit touched the opener or the
  /// closer, which is then forgotten.
  fn follow(&mut self, edit: &ByteEdit) -> bool {
    let touches = |(start, end): (usize, usize)| {
      (edit.start < end && edit.old_end > start) || (edit.start > start && edit.start < end)
    };
    if touches(self.open) || touches(self.close) {
      return false;
    }
    // Text typed between the two stays between them.
    self.open = (
      edit.map_offset(self.open.0, true),
      edit.map_offset(self.open.1, false),
    );
    self.close = (
      edit.map_offset(self.close.0, true),
      edit.map_offset(self.close.1, false),
    );
    true
  }
}

/// The closers added automatically that are still as they were added and
/// still have a cursor between them and their opener.
#[derive(Debug, Clone, Default)]
pub(crate) struct AutoClosed {
  /// The text version `closed` is in.
  version: Version,
  closed: Vec<Closed>,
}

impl AutoClosed {
  pub(crate) fn new(version: Version) -> Self {
    Self {
      version,
      closed: Vec::new(),
    }
  }

  /// Follows the edits since the last sync.
  pub(crate) fn sync(&mut self, buffer: &TextBuffer) {
    let since = std::mem::replace(&mut self.version, buffer.version());
    if since == self.version || self.closed.is_empty() {
      return;
    }
    match buffer.edits_since(since) {
      Some(edits) => self
        .closed
        .retain_mut(|closed| edits.iter().all(|edit| closed.follow(edit))),
      None => self.closed.clear(),
    }
  }

  /// Forgets the pairs no cursor is in any more.
  pub(crate) fn keep_around(&mut self, selections: &Selections) {
    self.closed.retain(|closed| {
      selections
        .iter()
        .any(|s| s.head >= closed.open.1 && s.head < closed.close.1)
    });
  }

  pub(crate) fn clear(&mut self, version: Version) {
    self.version = version;
    self.closed.clear();
  }

  fn add(&mut self, closed: Closed) {
    if self.closed.len() == MAX_AUTO_CLOSED {
      self.closed.remove(0);
    }
    self.closed.push(closed);
  }

  /// Whether an added closer is at `offset`.
  fn closes_at(&self, offset: usize) -> bool {
    self
      .closed
      .iter()
      .any(|closed| offset >= closed.close.0 && offset < closed.close.1)
  }

  /// Whether `open` and `close` are an opener and the closer added for it.
  fn is_pair(&self, open: Range<usize>, close: Range<usize>) -> bool {
    self.closed.iter().any(|closed| {
      closed.open == (open.start, open.end) && closed.close == (close.start, close.end)
    })
  }
}

impl Document {
  /// Types `text` at each selection, with the language's pairs: steps over
  /// an added closer, adds a closer, or wraps the selections. Anything but a
  /// single character is inserted as it is.
  pub(crate) fn type_text(&mut self, text: &str, context: &PairContext) -> Result<bool, CoreError> {
    let mut chars = text.chars();
    let (Some(typed), None) = (chars.next(), chars.next()) else {
      return self.insert(text);
    };
    if is_line_break(typed) {
      return self.insert(text);
    }
    if self.overtype(typed, context) {
      return Ok(false);
    }
    if let Some(result) = self.auto_close(typed, context) {
      return result;
    }
    if let Some(result) = self.surround(typed, context) {
      return result;
    }
    self.insert(text)
  }

  /// Moves each cursor over the closer it's at, if every cursor is at one
  /// that may be typed over.
  fn overtype(&mut self, typed: char, context: &PairContext) -> bool {
    let policy = context.settings.overtype;
    if policy == Closers::Never {
      return false;
    }
    let all = self.selections.iter().enumerate().all(|(index, s)| {
      s.is_empty()
        && self.buffer.chars_at(s.head).next() == Some(typed)
        && context.cursor(index).is_some_and(|cursor| {
          cursor
            .pairs
            .auto_closing
            .iter()
            .any(|pair| pair.close.contains(typed))
        })
        && (policy == Closers::Always || self.auto_closed.closes_at(s.head))
    });
    if !all {
      return false;
    }
    let after = self
      .selections
      .map(|s| Selection::cursor(s.head + typed.len_utf8()));
    // Typing over a closer continues the typing: it doesn't end the undo
    // group as moving the cursor would.
    self.history.continue_group(after.clone());
    self.selections = after;
    true
  }

  /// Types an opener and adds its closer at every cursor, if every cursor
  /// has a pair to auto-close there.
  fn auto_close(&mut self, typed: char, context: &PairContext) -> Option<Result<bool, CoreError>> {
    if self.selections.iter().any(|s| !s.is_empty()) {
      return None;
    }
    let mut plans = Vec::with_capacity(self.selections.len());
    for (index, s) in self.selections.iter().enumerate() {
      let cursor = context.cursor(index)?;
      plans.push(self.closer_for(s.head, typed, cursor, &context.settings)?);
    }
    let edits = self
      .selections
      .iter()
      .zip(&plans)
      .map(|(s, (_, close))| (s.range(), format!("{typed}{close}"), typed.len_utf8()))
      .collect();
    let merge = !self.starts_new_word(&typed.to_string());
    let result = self.edit_selections_at(edits, EditKind::Typing, merge);
    if matches!(result, Ok(true)) {
      self.auto_closed.sync(&self.buffer);
      let heads: Vec<usize> = self.selections.iter().map(|s| s.head).collect();
      for (head, (open_before, close)) in heads.into_iter().zip(plans) {
        self.auto_closed.add(Closed {
          open: (head - typed.len_utf8() - open_before, head),
          close: (head, head + close.len()),
        });
      }
    }
    Some(result)
  }

  /// The closer to add when `typed` is typed at `head`, with the bytes of
  /// its opener already before the cursor; `None` to type it plainly.
  fn closer_for(
    &self,
    head: usize,
    typed: char,
    cursor: &CursorPairs,
    settings: &PairSettings,
  ) -> Option<(usize, String)> {
    let quote = is_quote(typed);
    let mode = if quote {
      settings.quotes
    } else {
      settings.brackets
    };
    if mode == AutoClose::Never {
      return None;
    }
    let before = self.line_before(head, MAX_PAIR_CHARS - 1);
    let typed_text = format!("{before}{typed}");
    let pairs = &cursor.pairs;
    let pair = pairs
      .auto_closing
      .iter()
      .filter(|pair| typed_text.ends_with(&pair.open))
      .max_by_key(|pair| pair.open.len())?;
    let excluded = match cursor.scope {
      Scope::Code => false,
      Scope::String => pair.not_in_string,
      Scope::Comment => pair.not_in_comment,
    };
    if excluded {
      return None;
    }
    let after = self.line_after(head, MAX_PAIR_CHARS);
    if mode != AutoClose::Always
      && let Some(next) = after.chars().next()
    {
      let allowed = next.is_whitespace()
        || before_closer(&after, pairs)
        || (mode == AutoClose::LanguageDefined && pairs.auto_close_before.contains(next));
      if !allowed {
        return None;
      }
    }
    // A quote right after a word ends it, or is an apostrophe.
    if quote
      && mode != AutoClose::Always
      && pair.open.chars().count() == 1
      && before.chars().next_back().is_some_and(is_word_char)
    {
      return None;
    }
    // After an odd number of these quotes on the line, it most likely ends
    // a string: one the syntax tree doesn't show, as it isn't terminated.
    // (Also for a prefixed opener, such as Python's `f"`.)
    if quote && pair.close.chars().eq([typed]) && self.ends_open_quote(head, typed) {
      return None;
    }
    // A pair inside this one whose closer is already there, as `(` in `(*`
    // after `(` added `)`: add only the rest.
    let mut close = pair.close.clone();
    if let Some(inner) = pairs.auto_closing.iter().find(|inner| {
      inner.open != pair.open
        && pair.open.contains(&inner.open)
        && pair.close.ends_with(&inner.close)
        && after.starts_with(&inner.close)
    }) {
      close.truncate(close.len() - inner.close.len());
    }
    if close.is_empty() {
      return None;
    }
    Some((pair.open.len() - typed.len_utf8(), close))
  }

  /// Wraps every selection in the pair `typed` opens, if each has one.
  fn surround(&mut self, typed: char, context: &PairContext) -> Option<Result<bool, CoreError>> {
    let quote = is_quote(typed);
    let enabled = match context.settings.surround {
      Surround::LanguageDefined => true,
      Surround::Quotes => quote,
      Surround::Brackets => !quote,
      Surround::Never => false,
    };
    if !enabled || self.selections.iter().any(Selection::is_empty) {
      return None;
    }
    let mut buf = [0; 4];
    let open = &*typed.encode_utf8(&mut buf);
    let mut wraps = Vec::with_capacity(self.selections.len());
    for (index, s) in self.selections.iter().enumerate() {
      let (_, close) = context
        .cursor(index)?
        .pairs
        .surrounding
        .iter()
        .find(|(pair_open, _)| pair_open == open)?;
      wraps.push((*s, close.clone()));
    }
    let edits = wraps
      .iter()
      .map(|(s, close)| {
        let text = self.buffer.text_in(s.range());
        Edit::new(s.range(), format!("{open}{text}{close}"))
      })
      .collect();
    Some(self.apply_wraps(edits, &wraps, open.len()))
  }

  fn apply_wraps(
    &mut self,
    edits: Vec<Edit>,
    wraps: &[(Selection, String)],
    open_len: usize,
  ) -> Result<bool, CoreError> {
    let changes = ChangeSet::new(edits)?;
    let before = self.selections.clone();
    let inverse = self.buffer.apply(&changes)?;
    let after = Selections::new(
      changes
        .post_ranges()
        .into_iter()
        .zip(wraps)
        .map(|(range, (s, close))| {
          let inner = (range.start + open_len, range.end - close.len());
          if s.head < s.anchor {
            Selection::new(inner.1, inner.0)
          } else {
            Selection::new(inner.0, inner.1)
          }
        })
        .collect(),
      before.primary_index(),
    );
    self.selections = after.clone();
    self
      .history
      .record(changes, inverse, before, after, EditKind::Other, false);
    self.history.seal();
    Ok(true)
  }

  /// Deletes backward, taking an opener and its closer together when every
  /// cursor is between a pair that may be deleted.
  pub(crate) fn backspace(&mut self, context: &PairContext) -> Result<bool, CoreError> {
    if let Some(ranges) = self.pairs_to_delete(context) {
      let edits = ranges
        .into_iter()
        .map(|range| (range, String::new(), 0))
        .collect();
      return self.edit_selections_at(edits, EditKind::Deleting, true);
    }
    self.delete(Direction::Backward, Unit::Grapheme)
  }

  fn pairs_to_delete(&self, context: &PairContext) -> Option<Vec<Range<usize>>> {
    let policy = context.settings.delete;
    if policy == Closers::Never {
      return None;
    }
    let mut ranges = Vec::with_capacity(self.selections.len());
    for (index, s) in self.selections.iter().enumerate() {
      if !s.is_empty() {
        return None;
      }
      let head = s.head;
      let before = self.line_before(head, MAX_PAIR_CHARS);
      let after = self.line_after(head, MAX_PAIR_CHARS);
      let pair = context
        .cursor(index)?
        .pairs
        .auto_closing
        .iter()
        .filter(|pair| before.ends_with(&pair.open) && after.starts_with(&pair.close))
        .filter(|pair| {
          policy == Closers::Always
            || self
              .auto_closed
              .is_pair(head - pair.open.len()..head, head..head + pair.close.len())
        })
        .max_by_key(|pair| pair.open.len())?;
      ranges.push(head - pair.open.len()..head + pair.close.len());
    }
    Some(ranges)
  }

  /// Inserts a line break at each selection, keeping the line's
  /// indentation, indenting after an opening bracket, and putting the
  /// closer on a line of its own when the cursor is between brackets.
  pub(crate) fn enter(&mut self, context: &PairContext) -> Result<bool, CoreError> {
    let ending = self.line_ending.as_str();
    let unit = self.indent_unit();
    let edits = self
      .selections
      .iter()
      .enumerate()
      .map(|(index, s)| {
        let buffer = &self.buffer;
        let line_start = buffer.line_start(buffer.line_of(s.start()));
        let indent: String = buffer
          .chars_at(line_start)
          .take_while(|&c| c == ' ' || c == '\t')
          .take(s.start() - line_start)
          .collect();
        let brackets = context
          .cursor(index)
          .map_or(&[][..], |cursor| &cursor.pairs.brackets[..]);
        let opened = if brackets.is_empty() {
          None
        } else {
          let before = self.line_before_spaces(s.start());
          brackets
            .iter()
            .filter(|(open, _)| before.ends_with(open.as_str()))
            .max_by_key(|(open, _)| open.len())
        };
        let Some((_, close)) = opened else {
          let text = format!("{ending}{indent}");
          let cursor = text.len();
          return (s.range(), text, cursor);
        };
        let (spaces, after) = self.line_after_spaces(s.end());
        if after.starts_with(close.as_str()) {
          let line = format!("{ending}{indent}{unit}");
          let cursor = line.len();
          let end = s.end() + spaces;
          (s.start()..end, format!("{line}{ending}{indent}"), cursor)
        } else {
          let text = format!("{ending}{indent}{unit}");
          let cursor = text.len();
          (s.range(), text, cursor)
        }
      })
      .collect();
    self.edit_selections_at(edits, EditKind::Other, false)
  }

  /// Whether the line before `offset` has an odd number of `quote`s not
  /// escaped with a backslash. Long lines aren't counted.
  fn ends_open_quote(&self, offset: usize, quote: char) -> bool {
    let line_start = self.buffer.line_start(self.buffer.line_of(offset));
    if offset - line_start > MAX_QUOTE_SCAN_BYTES {
      return false;
    }
    let mut open = false;
    let mut escaped = false;
    for c in self.buffer.text_in(line_start..offset).chars() {
      if escaped {
        escaped = false;
      } else if c == '\\' {
        escaped = true;
      } else if c == quote {
        open = !open;
      }
    }
    open
  }

  /// Up to `max` characters before `offset` on its line.
  fn line_before(&self, offset: usize, max: usize) -> String {
    let mut chars = self.buffer.chars_at(offset);
    let mut found = Vec::new();
    while found.len() < max {
      match chars.prev() {
        Some(c) if !is_line_break(c) => found.push(c),
        _ => break,
      }
    }
    found.into_iter().rev().collect()
  }

  /// Up to `max` characters after `offset` on its line.
  fn line_after(&self, offset: usize, max: usize) -> String {
    self
      .buffer
      .chars_at(offset)
      .take_while(|&c| !is_line_break(c))
      .take(max)
      .collect()
  }

  /// The characters before `offset` on its line, past spaces and tabs.
  fn line_before_spaces(&self, offset: usize) -> String {
    let mut chars = self.buffer.chars_at(offset);
    let mut found = Vec::new();
    let mut in_spaces = true;
    while found.len() < MAX_PAIR_CHARS {
      match chars.prev() {
        Some(' ' | '\t') if in_spaces => {}
        Some(c) if !is_line_break(c) => {
          in_spaces = false;
          found.push(c);
        }
        _ => break,
      }
    }
    found.into_iter().rev().collect()
  }

  /// The bytes of spaces and tabs after `offset`, and the characters on
  /// the line after them.
  fn line_after_spaces(&self, offset: usize) -> (usize, String) {
    let spaces = self
      .buffer
      .chars_at(offset)
      .take_while(|&c| c == ' ' || c == '\t')
      .count();
    (spaces, self.line_after(offset + spaces, MAX_PAIR_CHARS))
  }
}

/// Whether `after` starts with a closer that isn't also an opener.
fn before_closer(after: &str, pairs: &LanguagePairs) -> bool {
  let closes = pairs
    .auto_closing
    .iter()
    .any(|pair| after.starts_with(&pair.close));
  let opens = pairs
    .auto_closing
    .iter()
    .any(|pair| after.starts_with(&pair.open));
  closes && !opens
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::ops::{EditorOp, Motion};
  use crate::testing::{doc, render, run};

  fn code() -> LanguagePairs {
    let quoted = |quote: &str, comment: bool| AutoClosingPair {
      not_in_string: true,
      not_in_comment: comment,
      ..AutoClosingPair::new(quote, quote)
    };
    LanguagePairs {
      brackets: vec![
        ("{".into(), "}".into()),
        ("[".into(), "]".into()),
        ("(".into(), ")".into()),
      ],
      auto_closing: vec![
        AutoClosingPair::new("{", "}"),
        AutoClosingPair::new("[", "]"),
        AutoClosingPair::new("(", ")"),
        quoted("\"", false),
        quoted("'", true),
        AutoClosingPair {
          not_in_string: true,
          ..AutoClosingPair::new("/**", " */")
        },
      ],
      surrounding: vec![
        ("(".into(), ")".into()),
        ("\"".into(), "\"".into()),
        ("'".into(), "'".into()),
      ],
      auto_close_before: DEFAULT_AUTO_CLOSE_BEFORE.into(),
    }
  }

  fn context_with(document: &Document, settings: PairSettings, scope: Scope) -> PairContext {
    let pairs = Arc::new(code());
    PairContext {
      settings,
      cursors: (0..document.selections.len())
        .map(|_| CursorPairs {
          pairs: pairs.clone(),
          scope,
        })
        .collect(),
    }
  }

  fn context(document: &Document) -> PairContext {
    context_with(document, PairSettings::default(), Scope::Code)
  }

  fn type_text(document: &mut Document, text: &str) -> String {
    for c in text.chars() {
      let context = context(document);
      document
        .apply(EditorOp::Type {
          text: c.to_string(),
          pairs: context,
        })
        .expect("types");
    }
    render(document)
  }

  fn backspace(document: &mut Document) -> String {
    let context = context(document);
    run(document, [EditorOp::Backspace(context)])
  }

  fn enter(document: &mut Document) -> String {
    let context = context(document);
    run(document, [EditorOp::Enter(context)])
  }

  #[test]
  fn closes_brackets_and_quotes() {
    assert_eq!(type_text(&mut doc("|"), "("), "(|)");
    assert_eq!(type_text(&mut doc("|"), "{["), "{[|]}");
    assert_eq!(type_text(&mut doc("x = |;"), "\""), "x = \"|\";");
  }

  #[test]
  fn types_over_added_closers() {
    let mut document = doc("|");
    assert_eq!(type_text(&mut document, "(a)"), "(a)|");
    assert_eq!(type_text(&mut document, ")"), "(a))|");
    assert_eq!(type_text(&mut doc("|"), "\"ab\""), "\"ab\"|");
    assert_eq!(type_text(&mut doc("|"), "f(\"x\")"), "f(\"x\")|");
  }

  #[test]
  fn types_closers_it_did_not_add() {
    assert_eq!(type_text(&mut doc("(|)"), ")"), "()|)");
    let always = PairSettings {
      overtype: Closers::Always,
      ..PairSettings::default()
    };
    let mut document = doc("(|)");
    let context = context_with(&document, always, Scope::Code);
    let op = EditorOp::Type {
      text: ")".into(),
      pairs: context,
    };
    assert_eq!(run(&mut document, [op]), "()|");
  }

  #[test]
  fn forgets_closers_once_the_cursor_leaves() {
    let mut document = doc("|");
    type_text(&mut document, "(");
    run(
      &mut document,
      [
        EditorOp::Move {
          motion: Motion::Right,
          extend: false,
        },
        EditorOp::Move {
          motion: Motion::Left,
          extend: false,
        },
      ],
    );
    assert_eq!(type_text(&mut document, ")"), "()|)");
  }

  #[test]
  fn closes_only_before_whitespace_closers_and_listed_characters() {
    assert_eq!(type_text(&mut doc("|x"), "("), "(|x");
    assert_eq!(type_text(&mut doc("| x"), "("), "(|) x");
    assert_eq!(type_text(&mut doc("(|)"), "["), "([|])");
    assert_eq!(type_text(&mut doc("|;"), "("), "(|);");
    // A quote isn't a closer: it may open a string.
    assert_eq!(type_text(&mut doc("|\""), "\""), "\"|\"");
  }

  #[test]
  fn does_not_close_quotes_after_words() {
    assert_eq!(type_text(&mut doc("don|"), "'"), "don'|");
    assert_eq!(type_text(&mut doc("x|"), "\""), "x\"|");
    assert_eq!(type_text(&mut doc("(|"), "\""), "(\"|\"");
  }

  #[test]
  fn does_not_close_quotes_that_end_strings() {
    assert_eq!(type_text(&mut doc("x = \"ab |"), "\""), "x = \"ab \"|");
    assert_eq!(
      type_text(&mut doc("x = \"a\\\" |"), "\""),
      "x = \"a\\\" \"|"
    );
    assert_eq!(type_text(&mut doc("f(\"a\", |)"), "\""), "f(\"a\", \"|\")");
  }

  #[test]
  fn closes_prefixed_quotes_after_their_prefix() {
    let mut pairs = code();
    pairs.auto_closing.push(AutoClosingPair::new("f\"", "\""));
    let type_with = |marked: &str| {
      let mut document = doc(marked);
      let context = PairContext {
        settings: PairSettings::default(),
        cursors: vec![CursorPairs {
          pairs: Arc::new(pairs.clone()),
          scope: Scope::Code,
        }],
      };
      let op = EditorOp::Type {
        text: "\"".into(),
        pairs: context,
      };
      run(&mut document, [op])
    };
    assert_eq!(type_with("x = f|"), "x = f\"|\"");
    assert_eq!(type_with("x = \"a f|"), "x = \"a f\"|");
  }

  #[test]
  fn respects_strings_and_comments() {
    let mut document = doc("|");
    let context = context_with(&document, PairSettings::default(), Scope::String);
    let quote = EditorOp::Type {
      text: "\"".into(),
      pairs: context.clone(),
    };
    assert_eq!(run(&mut document, [quote]), "\"|");
    let bracket = EditorOp::Type {
      text: "(".into(),
      pairs: context,
    };
    assert_eq!(run(&mut doc("|"), [bracket]), "(|)");
    let mut document = doc("|");
    let context = context_with(&document, PairSettings::default(), Scope::Comment);
    let apostrophe = EditorOp::Type {
      text: "'".into(),
      pairs: context.clone(),
    };
    assert_eq!(run(&mut document, [apostrophe]), "'|");
    let quote = EditorOp::Type {
      text: "\"".into(),
      pairs: context,
    };
    assert_eq!(run(&mut doc("|"), [quote]), "\"|\"");
  }

  #[test]
  fn closes_openers_of_several_characters() {
    assert_eq!(type_text(&mut doc("|"), "/**"), "/**| */");
  }

  #[test]
  fn follows_settings() {
    let never = PairSettings {
      brackets: AutoClose::Never,
      ..PairSettings::default()
    };
    let mut document = doc("|");
    let context = context_with(&document, never, Scope::Code);
    let op = EditorOp::Type {
      text: "(".into(),
      pairs: context.clone(),
    };
    assert_eq!(run(&mut document, [op]), "(|");
    let quote = EditorOp::Type {
      text: "\"".into(),
      pairs: context,
    };
    assert_eq!(run(&mut doc("|"), [quote]), "\"|\"");

    let whitespace = PairSettings {
      brackets: AutoClose::BeforeWhitespace,
      ..PairSettings::default()
    };
    let mut document = doc("|;");
    let context = context_with(&document, whitespace, Scope::Code);
    let op = EditorOp::Type {
      text: "(".into(),
      pairs: context,
    };
    assert_eq!(run(&mut document, [op]), "(|;");

    let always = PairSettings {
      brackets: AutoClose::Always,
      ..PairSettings::default()
    };
    let mut document = doc("|x");
    let context = context_with(&document, always, Scope::Code);
    let op = EditorOp::Type {
      text: "(".into(),
      pairs: context,
    };
    assert_eq!(run(&mut document, [op]), "(|)x");
  }

  #[test]
  fn without_pairs_types_plainly() {
    let mut document = doc("|");
    let op = EditorOp::Type {
      text: "(".into(),
      pairs: PairContext::default(),
    };
    assert_eq!(run(&mut document, [op]), "(|");
  }

  #[test]
  fn closes_at_every_cursor_or_none() {
    assert_eq!(type_text(&mut doc("|\n|"), "("), "(|)\n(|)");
    assert_eq!(type_text(&mut doc("|\n|x"), "("), "(|\n(|x");
    let mut document = doc("|\n|");
    assert_eq!(type_text(&mut document, "(a)"), "(a)|\n(a)|");
  }

  #[test]
  fn wraps_selections() {
    assert_eq!(type_text(&mut doc("[abc]"), "("), "([abc])");
    assert_eq!(type_text(&mut doc("[a] [b]"), "\""), "\"[a]\" \"[b]\"");
    // Wrapped again, with the selection kept inside.
    assert_eq!(type_text(&mut doc("[abc]"), "(\""), "(\"[abc]\")");
    // `{` doesn't surround in these pairs: typed over the selection.
    assert_eq!(type_text(&mut doc("[abc]"), "{"), "{|");
    let never = PairSettings {
      surround: Surround::Never,
      ..PairSettings::default()
    };
    let mut document = doc("[abc]");
    let context = context_with(&document, never, Scope::Code);
    let op = EditorOp::Type {
      text: "(".into(),
      pairs: context,
    };
    assert_eq!(run(&mut document, [op]), "(|");
  }

  #[test]
  fn backspace_deletes_added_pairs() {
    let mut document = doc("|");
    type_text(&mut document, "(");
    assert_eq!(backspace(&mut document), "|");
    // Not a closer it added.
    assert_eq!(backspace(&mut doc("(|)")), "|)");
    let mut document = doc("|");
    type_text(&mut document, "/**");
    assert_eq!(backspace(&mut document), "|");
    let mut document = doc("|");
    type_text(&mut document, "(a");
    assert_eq!(backspace(&mut document), "(|)");
    assert_eq!(backspace(&mut document), "|");
  }

  #[test]
  fn backspace_follows_settings() {
    let always = PairSettings {
      delete: Closers::Always,
      ..PairSettings::default()
    };
    let mut document = doc("(|)");
    let context = context_with(&document, always, Scope::Code);
    assert_eq!(run(&mut document, [EditorOp::Backspace(context)]), "|");
    let never = PairSettings {
      delete: Closers::Never,
      ..PairSettings::default()
    };
    let mut document = doc("|");
    type_text(&mut document, "(");
    let context = context_with(&document, never, Scope::Code);
    assert_eq!(run(&mut document, [EditorOp::Backspace(context)]), "|)");
  }

  #[test]
  fn enter_between_brackets_puts_the_closer_on_its_own_line() {
    assert_eq!(enter(&mut doc("  f {|}")), "  f {\n      |\n  }");
    assert_eq!(enter(&mut doc("f(|  )")), "f(\n    |\n)");
    assert_eq!(enter(&mut doc("if x {  |")), "if x {  \n    |");
    assert_eq!(enter(&mut doc("  x|")), "  x\n  |");
    let mut document = doc("  x {|}");
    let op = EditorOp::Enter(PairContext::default());
    assert_eq!(run(&mut document, [op]), "  x {\n  |}");
  }

  #[test]
  fn undo_takes_back_typing_with_its_pairs() {
    let mut document = doc("|");
    type_text(&mut document, "f(x)");
    assert_eq!(render(&document), "f(x)|");
    assert_eq!(run(&mut document, [EditorOp::Undo]), "|");
    assert_eq!(run(&mut document, [EditorOp::Redo]), "f(x)|");
  }

  #[test]
  fn other_edits_move_added_closers_or_forget_them() {
    let mut document = doc("|");
    type_text(&mut document, "(");
    // An edit before the pair moves it.
    document
      .apply_edits(vec![Edit::new(0..0, String::from("x = "))])
      .expect("edits");
    assert_eq!(render(&document), "x = (|)");
    assert_eq!(type_text(&mut document, ")"), "x = ()|");
    // An edit of the closer forgets it.
    let mut document = doc("|");
    type_text(&mut document, "/**");
    document
      .apply_edits(vec![Edit::new(4..5, String::from("#"))])
      .expect("edits");
    assert_eq!(backspace(&mut document), "/*| #/");
  }

  #[test]
  fn auto_insert_applies_only_where_it_was_asked() {
    let mut document = doc("<div>|");
    let version = document.buffer.version();
    let insert = |version, offset| EditorOp::AutoInsert {
      version,
      offset,
      text: "</div>".into(),
      cursor: 0,
    };
    assert_eq!(run(&mut document, [insert(version, 5)]), "<div>|</div>");
    // Undo takes back only the inserted text.
    assert_eq!(run(&mut document, [EditorOp::Undo]), "<div>|");
    // Not at another version, cursor, or with several cursors.
    let mut document = doc("<div>|");
    let stale = document.buffer.version();
    type_text(&mut document, "x");
    assert_eq!(run(&mut document, [insert(stale, 5)]), "<div>x|");
    let mut document = doc("<div>|");
    let version = document.buffer.version();
    assert_eq!(run(&mut document, [insert(version, 4)]), "<div>|");
    let mut document = doc("<a>|<b>|");
    let version = document.buffer.version();
    assert_eq!(run(&mut document, [insert(version, 3)]), "<a>|<b>|");
    // The cursor can be left after the text.
    let mut document = doc("<p></|");
    let version = document.buffer.version();
    let op = EditorOp::AutoInsert {
      version,
      offset: 5,
      text: "p>".into(),
      cursor: 2,
    };
    assert_eq!(run(&mut document, [op]), "<p></p>|");
  }

  #[test]
  fn undo_forgets_added_closers() {
    let mut document = doc("|");
    type_text(&mut document, "x (");
    run(&mut document, [EditorOp::Undo, EditorOp::Redo]);
    assert_eq!(render(&document), "x (|)");
    assert_eq!(type_text(&mut document, ")"), "x ()|)");
  }
}
