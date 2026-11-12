//! Editing operations: the commands a frontend sends to change a document's
//! text or selections. Every operation applies to all selections.

use std::ops::Range;

use flareite_text::{ChangeSet, Edit, SearchQuery, Version, normalize_line_endings};

use crate::document::Document;
use crate::error::CoreError;
use crate::history::EditKind;
use crate::lines::CommentTokens;
use crate::pairs::PairContext;
use crate::selection::{Selection, Selections};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
  Backward,
  Forward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
  Grapheme,
  Word,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerticalDirection {
  Up,
  Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
  Left,
  Right,
  WordLeft,
  WordRight,
  Up,
  Down,
  PageUp {
    lines: usize,
  },
  PageDown {
    lines: usize,
  },
  /// To the first non-whitespace character, or the line start if already there.
  LineStart,
  LineEnd,
  DocumentStart,
  DocumentEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorOp {
  /// Replaces each selection with the text. Line breaks are converted to the
  /// document's line ending.
  Insert(String),
  /// Text typed at the keyboard: inserted as [`Self::Insert`] is, except
  /// that a single character may step over a closer, add one, or wrap the
  /// selections in a pair (see [`crate::pairs`]).
  Type {
    text: String,
    pairs: PairContext,
  },
  /// Like [`Self::Insert`], except that text copied as whole lines (see
  /// [`Document::cursor_lines_text`]) goes above each cursor's line when
  /// nothing is selected.
  Paste {
    text: String,
    whole_lines: bool,
  },
  /// Inserts a line break, keeping the current line's indentation.
  Newline,
  /// Enter: a line break that also indents after an opening bracket, and
  /// puts the closer on a line of its own between brackets.
  Enter(PairContext),
  /// Inserts a line above or below each selection's lines and moves there.
  InsertLine(VerticalDirection),
  /// Inserts a tab, or spaces to the next tab stop. Indents the lines
  /// instead when a selection spans several.
  Tab,
  /// Indents the selected lines to the next tab stop.
  Indent,
  /// Outdents the selected lines to the previous tab stop.
  Outdent,
  /// Comments out the selected lines, or uncomments them.
  ToggleComment(CommentTokens),
  /// Swaps the selected lines with the line above or below.
  MoveLines(VerticalDirection),
  /// Duplicates the selected lines above or below.
  CopyLines(VerticalDirection),
  /// Deletes the selected lines.
  DeleteLines,
  /// Deletes each non-empty selection, or one unit next to each cursor.
  Delete {
    direction: Direction,
    unit: Unit,
  },
  /// Text an extension inserts as the user types (an HTML closing tag
  /// after `<div>`): at `offset`, leaving the cursor `cursor` bytes into
  /// it. Only if the text is still at `version` and the only selection is a
  /// cursor at `offset`; otherwise nothing happens. Its own undo step, so
  /// undo takes back only what was inserted.
  AutoInsert {
    version: Version,
    offset: usize,
    text: String,
    cursor: usize,
  },
  /// Inserts a snippet (`$1`, `${1:placeholder}`, `$0`, ...) at each
  /// selection and selects its first tab stop; Tab and Shift+Tab
  /// ([`Self::Tab`], [`Self::Outdent`]) then move between its stops (see
  /// [`crate::snippets`]). With `adjust_indentation`, its lines are
  /// indented like the line it's inserted on.
  InsertSnippet {
    snippet: String,
    adjust_indentation: bool,
  },
  /// Backspace: deletes backward by grapheme, or an opener and its closer
  /// together when the cursors are between them.
  Backspace(PairContext),
  /// Deletes each non-empty selection; cursors are left alone.
  DeleteSelection,
  Move {
    motion: Motion,
    extend: bool,
  },
  SelectAll,
  /// Selects the selections' whole lines, or extends by one more line.
  SelectLine,
  /// Puts a cursor at the end of each line in the selections.
  CursorsAtLineEnds,
  /// Leaves a single cursor at the primary selection's head.
  CollapseSelections,
  /// Restores the selections from before the last selection change, if the
  /// text hasn't changed since.
  CursorUndo,
  /// Places the cursor (snapped to a grapheme boundary). `extend` moves the
  /// primary selection's head instead; `add` adds a new cursor.
  SetCursor {
    offset: usize,
    extend: bool,
    add: bool,
  },
  /// Selects the word (or run of punctuation or spaces) at the offset.
  SelectWord {
    offset: usize,
  },
  /// Adds a cursor on the line above the first or below the last selection.
  AddCursor(VerticalDirection),
  Undo,
  Redo,
  /// Selects the next (or previous) match after the primary selection,
  /// wrapping around the document.
  FindNext {
    query: SearchQuery,
    backward: bool,
  },
  /// Selects every match, up to a limit.
  SelectAllMatches {
    query: SearchQuery,
  },
  /// Replaces the primary selection if it is a match, then selects the next
  /// match.
  Replace {
    query: SearchQuery,
    replacement: String,
  },
  /// Replaces every match as a single undoable change.
  ReplaceAll {
    query: SearchQuery,
    replacement: String,
  },
  /// Selects the word at the cursor, or adds a selection at the next
  /// occurrence of the primary selection's text.
  AddNextMatch,
  /// Selects every occurrence of the primary selection's text, or of the
  /// whole word at the cursor.
  SelectAllOccurrences,
  /// Moves each cursor to the matching bracket.
  JumpToBracket,
  /// Folds the innermost unfolded region around each cursor. Fold
  /// operations take the document's foldable regions as line ranges (see
  /// [`crate::folds`]).
  Fold(Vec<Range<usize>>),
  /// Unfolds the folded regions starting on a cursor's line.
  Unfold,
  /// Unfolds the region starting on each cursor's line, or folds the
  /// innermost region around the cursor.
  ToggleFold(Vec<Range<usize>>),
  /// Unfolds the regions starting on `line`, or folds the largest one.
  ToggleFoldAt {
    line: usize,
    regions: Vec<Range<usize>>,
  },
  FoldAll(Vec<Range<usize>>),
  UnfoldAll,
  /// Folds exactly these regions, unfolding any others: a diff's unchanged
  /// lines, say.
  SetFolds(Vec<Range<usize>>),
  /// Unfolds the folded regions among these.
  UnfoldRegions(Vec<Range<usize>>),
  /// Folds the innermost region around each cursor and the regions in it.
  FoldRecursively(Vec<Range<usize>>),
  /// Unfolds the region around each cursor and the regions in it.
  UnfoldRecursively(Vec<Range<usize>>),
  /// Folds the regions nested `level` deep (1 is outermost), except those
  /// around a cursor.
  FoldLevel {
    level: usize,
    regions: Vec<Range<usize>>,
  },
}

impl EditorOp {
  /// Whether the operation may change the text (rather than only the
  /// selections, folds, or view).
  pub fn changes_text(&self) -> bool {
    matches!(
      self,
      Self::Insert(_)
        | Self::Type { .. }
        | Self::Paste { .. }
        | Self::Newline
        | Self::Enter(_)
        | Self::InsertLine(_)
        | Self::Tab
        | Self::Indent
        | Self::Outdent
        | Self::ToggleComment(_)
        | Self::MoveLines(_)
        | Self::CopyLines(_)
        | Self::DeleteLines
        | Self::Delete { .. }
        | Self::Backspace(_)
        | Self::AutoInsert { .. }
        | Self::InsertSnippet { .. }
        | Self::DeleteSelection
        | Self::Undo
        | Self::Redo
        | Self::Replace { .. }
        | Self::ReplaceAll { .. }
    )
  }
}

impl Document {
  /// Applies an operation. Returns whether the text changed. Afterwards no
  /// selection is in a folded region's hidden lines: movement steps over
  /// them, and other operations unfold the regions selections end up in.
  /// A read-only document ignores operations that would change its text.
  pub fn apply(&mut self, op: EditorOp) -> Result<bool, CoreError> {
    if self.is_read_only() && op.changes_text() {
      return Ok(false);
    }
    // Catch up with edits made otherwise, such as extensions'.
    self.auto_closed.sync(&self.buffer);
    let restores = matches!(op, EditorOp::Undo | EditorOp::Redo);
    let result = self.apply_op(op);
    if restores {
      self.auto_closed.clear(self.buffer.version());
      self.snippet = None;
    } else {
      self.auto_closed.sync(&self.buffer);
      self.auto_closed.keep_around(&self.selections);
    }
    self.sync_folds();
    result
  }

  fn apply_op(&mut self, op: EditorOp) -> Result<bool, CoreError> {
    match op {
      EditorOp::Insert(text) => self.insert(&text),
      EditorOp::Type { text, pairs } => self.type_text(&text, &pairs),
      EditorOp::Paste { text, whole_lines } => self.paste(&text, whole_lines),
      EditorOp::Newline => self.enter(&PairContext::default()),
      EditorOp::Enter(pairs) => self.enter(&pairs),
      EditorOp::InsertLine(direction) => self.insert_line(direction),
      // In a snippet, Tab and Shift+Tab move between its stops.
      EditorOp::Tab if self.step_snippet(false) => Ok(false),
      EditorOp::Tab => self.tab(),
      EditorOp::Indent => self.shift_lines(false),
      EditorOp::Outdent if self.step_snippet(true) => Ok(false),
      EditorOp::Outdent => self.shift_lines(true),
      EditorOp::ToggleComment(tokens) => self.toggle_comment(&tokens),
      EditorOp::MoveLines(direction) => self.move_lines(direction),
      EditorOp::CopyLines(direction) => self.copy_lines(direction),
      EditorOp::DeleteLines => self.delete_lines(),
      EditorOp::Delete { direction, unit } => self.delete(direction, unit),
      EditorOp::Backspace(pairs) => self.backspace(&pairs),
      EditorOp::AutoInsert {
        version,
        offset,
        text,
        cursor,
      } => self.auto_insert(version, offset, &text, cursor),
      EditorOp::InsertSnippet {
        snippet,
        adjust_indentation,
      } => self.insert_snippet(&snippet, adjust_indentation),
      EditorOp::DeleteSelection => {
        self.edit_selections(|_, s| (s.range(), String::new()), EditKind::Other, false)
      }
      EditorOp::Move { motion, extend } => {
        let moved = self
          .selections
          .map(|s| self.step_over_folds(s, self.moved(s, motion, extend)));
        self.set_selections(moved);
        Ok(false)
      }
      EditorOp::SelectAll => {
        self.set_selections(Selections::single(Selection::new(0, self.buffer.len())));
        Ok(false)
      }
      EditorOp::SelectLine => {
        self.select_line();
        Ok(false)
      }
      EditorOp::CursorsAtLineEnds => {
        self.cursors_at_line_ends();
        Ok(false)
      }
      // Escape leaves a snippet first, keeping the selections.
      EditorOp::CollapseSelections if self.end_snippet() => Ok(false),
      EditorOp::CollapseSelections => {
        self.set_selections(Selections::cursor(self.selections.primary().head));
        Ok(false)
      }
      EditorOp::CursorUndo => {
        self.cursor_undo();
        Ok(false)
      }
      EditorOp::SetCursor {
        offset,
        extend,
        add,
      } => {
        self.set_cursor(offset, extend, add);
        Ok(false)
      }
      EditorOp::SelectWord { offset } => {
        let word = self.buffer.word_at(self.buffer.snap_to_grapheme(offset));
        self.set_selections(Selections::single(Selection::new(word.start, word.end)));
        Ok(false)
      }
      EditorOp::AddCursor(direction) => {
        self.add_cursor(direction);
        Ok(false)
      }
      EditorOp::Undo => self.restore(|document| document.history.undo(&mut document.buffer)),
      EditorOp::Redo => self.restore(|document| document.history.redo(&mut document.buffer)),
      EditorOp::FindNext { query, backward } => {
        self.find_next(&query.compile()?, backward);
        Ok(false)
      }
      EditorOp::SelectAllMatches { query } => {
        self.select_all_matches(&query.compile()?);
        Ok(false)
      }
      EditorOp::Replace { query, replacement } => {
        self.replace_current(&query.compile()?, &replacement)
      }
      EditorOp::ReplaceAll { query, replacement } => {
        self.replace_all(&query.compile()?, &replacement)
      }
      EditorOp::AddNextMatch => {
        self.add_next_match();
        Ok(false)
      }
      EditorOp::SelectAllOccurrences => {
        self.select_all_occurrences();
        Ok(false)
      }
      EditorOp::JumpToBracket => {
        self.jump_to_bracket();
        Ok(false)
      }
      EditorOp::Fold(regions) => {
        self.fold(&regions);
        Ok(false)
      }
      EditorOp::Unfold => {
        self.unfold();
        Ok(false)
      }
      EditorOp::ToggleFold(regions) => {
        self.toggle_fold(&regions);
        Ok(false)
      }
      EditorOp::ToggleFoldAt { line, regions } => {
        self.toggle_fold_at(line, &regions);
        Ok(false)
      }
      EditorOp::FoldAll(regions) => {
        self.fold_all(&regions);
        Ok(false)
      }
      EditorOp::UnfoldAll => {
        self.unfold_all();
        Ok(false)
      }
      EditorOp::SetFolds(regions) => {
        self.unfold_all();
        self.fold_all(&regions);
        Ok(false)
      }
      EditorOp::UnfoldRegions(regions) => {
        self.unfold_regions(&regions);
        Ok(false)
      }
      EditorOp::FoldRecursively(regions) => {
        self.fold_recursively(&regions);
        Ok(false)
      }
      EditorOp::UnfoldRecursively(regions) => {
        self.unfold_recursively(&regions);
        Ok(false)
      }
      EditorOp::FoldLevel { level, regions } => {
        self.fold_level(level, &regions);
        Ok(false)
      }
    }
  }

  pub(crate) fn insert(&mut self, text: &str) -> Result<bool, CoreError> {
    let text = normalize_line_endings(text, self.line_ending).into_owned();
    if text.is_empty() {
      return Ok(false);
    }
    let has_break = text.contains(['\n', '\r']);
    let kind = if has_break {
      EditKind::Other
    } else {
      EditKind::Typing
    };
    let merge = !has_break && !self.starts_new_word(&text);
    self.edit_selections(|_, s| (s.range(), text.clone()), kind, merge)
  }

  fn auto_insert(
    &mut self,
    version: Version,
    offset: usize,
    text: &str,
    cursor: usize,
  ) -> Result<bool, CoreError> {
    let at_cursor =
      self.selections.len() == 1 && self.selections.primary() == Selection::cursor(offset);
    if self.buffer.version() != version
      || !at_cursor
      || !text.is_char_boundary(cursor.min(text.len()))
    {
      return Ok(false);
    }
    // Line breaks become the document's, which may move the cursor.
    let cursor = normalize_line_endings(&text[..cursor.min(text.len())], self.line_ending).len();
    let text = normalize_line_endings(text, self.line_ending).into_owned();
    self.history.seal();
    let changed =
      self.edit_selections_at(vec![(offset..offset, text, cursor)], EditKind::Other, false)?;
    self.history.seal();
    Ok(changed)
  }

  /// Typing groups end where a new word starts: when whitespace follows a
  /// non-whitespace character, or when typing replaces a selection.
  pub(crate) fn starts_new_word(&self, text: &str) -> bool {
    if self.selections.iter().any(|s| !s.is_empty()) {
      return true;
    }
    let mut before = self.buffer.chars_at(self.selections.primary().head);
    let after_space = before.prev().is_none_or(char::is_whitespace);
    text.starts_with(char::is_whitespace) && !after_space
  }

  fn tab(&mut self) -> Result<bool, CoreError> {
    let spans_lines = self
      .selections
      .iter()
      .any(|s| self.buffer.line_of(s.start()) != self.buffer.line_of(s.end()));
    if spans_lines {
      return self.shift_lines(false);
    }
    self.edit_selections(
      |document, s| {
        let text = if document.insert_spaces {
          let column = document.visual_column(s.start());
          " ".repeat(document.tab_size - column % document.tab_size)
        } else {
          "\t".to_owned()
        };
        (s.range(), text)
      },
      EditKind::Typing,
      false,
    )
  }

  pub(crate) fn delete(&mut self, direction: Direction, unit: Unit) -> Result<bool, CoreError> {
    self.edit_selections(
      |document, s| {
        if !s.is_empty() {
          return (s.range(), String::new());
        }
        let buffer = &document.buffer;
        let head = s.head;
        let range = match (direction, unit) {
          (Direction::Backward, Unit::Grapheme) => buffer.prev_grapheme_boundary(head)..head,
          (Direction::Backward, Unit::Word) => buffer.prev_word_boundary(head)..head,
          (Direction::Forward, Unit::Grapheme) => head..buffer.next_grapheme_boundary(head),
          (Direction::Forward, Unit::Word) => head..buffer.next_word_boundary(head),
        };
        (range, String::new())
      },
      EditKind::Deleting,
      unit == Unit::Grapheme,
    )
  }

  /// Computes one edit per selection, merges overlapping edits, applies them
  /// as a single change, and leaves a cursor after each edit.
  fn edit_selections(
    &mut self,
    edit_for: impl Fn(&Document, Selection) -> (Range<usize>, String),
    kind: EditKind,
    merge: bool,
  ) -> Result<bool, CoreError> {
    let edits = self
      .selections
      .iter()
      .map(|&selection| {
        let (range, text) = edit_for(self, selection);
        let cursor = text.len();
        (range, text, cursor)
      })
      .collect();
    self.edit_selections_at(edits, kind, merge)
  }

  /// Applies one edit per selection, in order, as a single change: each a
  /// range, its new text, and where in that text to leave the cursor.
  /// Overlapping edits are merged, leaving their cursors after them.
  pub(crate) fn edit_selections_at(
    &mut self,
    per_selection: Vec<(Range<usize>, String, usize)>,
    kind: EditKind,
    merge: bool,
  ) -> Result<bool, CoreError> {
    let before = self.selections.clone();
    let mut pending: Vec<(Range<usize>, String, usize, usize)> = per_selection
      .into_iter()
      .enumerate()
      .map(|(index, (range, text, cursor))| (range, text, cursor, index))
      .collect();
    pending.sort_by_key(|(range, _, _, _)| (range.start, range.end));

    let mut edits: Vec<Edit> = Vec::with_capacity(pending.len());
    // Where to leave the cursor in each edit's text; `None` once merged.
    let mut cursors: Vec<Option<usize>> = Vec::with_capacity(pending.len());
    let mut owner = vec![0; before.len()];
    for (range, text, cursor, index) in pending {
      match edits.last_mut() {
        Some(last) if range.start < last.range.end => {
          last.range.end = last.range.end.max(range.end);
          last.text.push_str(&text);
          if let Some(last) = cursors.last_mut() {
            *last = None;
          }
        }
        _ => {
          edits.push(Edit::new(range, text));
          cursors.push(Some(cursor));
        }
      }
      owner[index] = edits.len() - 1;
    }

    let changes = ChangeSet::new(edits)?;
    if changes.is_empty() {
      return Ok(false);
    }
    let inverse = self.buffer.apply(&changes)?;
    let post = changes.post_ranges();
    let after = Selections::new(
      owner
        .iter()
        .map(|&edit| {
          let range = &post[edit];
          Selection::cursor(cursors[edit].map_or(range.end, |cursor| range.start + cursor))
        })
        .collect(),
      before.primary_index(),
    );
    self.selections = after.clone();
    self
      .history
      .record(changes, inverse, before, after, kind, merge);
    Ok(true)
  }

  fn restore(
    &mut self,
    step: impl FnOnce(&mut Self) -> Result<Option<Selections>, flareite_text::EditError>,
  ) -> Result<bool, CoreError> {
    match step(self)? {
      Some(selections) => {
        self.selections = selections;
        Ok(true)
      }
      None => Ok(false),
    }
  }

  /// Replaces the selections; any change ends the current typing group.
  pub(crate) fn set_selections(&mut self, selections: Selections) {
    if selections != self.selections {
      self.history.seal();
      let previous = std::mem::replace(&mut self.selections, selections);
      self.remember_selections(previous);
    }
  }

  fn moved(&self, s: Selection, motion: Motion, extend: bool) -> Selection {
    let buffer = &self.buffer;
    let collapse = !extend && !s.is_empty();
    match motion {
      Motion::Left if collapse => Selection::cursor(s.start()),
      Motion::Right if collapse => Selection::cursor(s.end()),
      Motion::Left => s.moved_to(buffer.prev_grapheme_boundary(s.head), extend),
      Motion::Right => s.moved_to(buffer.next_grapheme_boundary(s.head), extend),
      Motion::WordLeft => s.moved_to(buffer.prev_word_boundary(s.head), extend),
      Motion::WordRight => s.moved_to(buffer.next_word_boundary(s.head), extend),
      Motion::Up => self.vertical(s, VerticalDirection::Up, 1, extend),
      Motion::Down => self.vertical(s, VerticalDirection::Down, 1, extend),
      Motion::PageUp { lines } => self.vertical(s, VerticalDirection::Up, lines.max(1), extend),
      Motion::PageDown { lines } => self.vertical(s, VerticalDirection::Down, lines.max(1), extend),
      Motion::LineStart => {
        let line = buffer.line_of(s.head);
        let start = buffer.line_start(line);
        let indent = buffer
          .chars_at(start)
          .take_while(|&c| c == ' ' || c == '\t')
          .count();
        let first_text = start + indent;
        s.moved_to(
          if s.head == first_text {
            start
          } else {
            first_text
          },
          extend,
        )
      }
      Motion::LineEnd => s.moved_to(buffer.line_end(buffer.line_of(s.head)), extend),
      Motion::DocumentStart => s.moved_to(0, extend),
      Motion::DocumentEnd => s.moved_to(buffer.len(), extend),
    }
  }

  /// Moves `lines` visible lines up or down, aiming for the selection's goal
  /// column. Moving past the first or last line goes to the start or end of
  /// the text.
  fn vertical(
    &self,
    s: Selection,
    direction: VerticalDirection,
    lines: usize,
    extend: bool,
  ) -> Selection {
    let goal = s.goal.unwrap_or_else(|| self.visual_column(s.head));
    let row = self.row_of(self.buffer.line_of(s.head));
    let target = match direction {
      VerticalDirection::Up => row.checked_sub(lines),
      VerticalDirection::Down => Some(row + lines).filter(|&t| t < self.row_count()),
    };
    let head = match (target, direction) {
      (Some(target), _) => self.offset_at_visual_column(self.line_at_row(target), goal),
      (None, VerticalDirection::Up) => 0,
      (None, VerticalDirection::Down) => self.buffer.len(),
    };
    Selection {
      goal: Some(goal),
      ..s.moved_to(head, extend)
    }
  }

  fn set_cursor(&mut self, offset: usize, extend: bool, add: bool) {
    let offset = self.buffer.snap_to_grapheme(offset);
    let selections = if add {
      self.selections.with_added(Selection::cursor(offset))
    } else if extend {
      Selections::single(self.selections.primary().moved_to(offset, true))
    } else {
      Selections::cursor(offset)
    };
    self.set_selections(selections);
  }

  fn add_cursor(&mut self, direction: VerticalDirection) {
    let edge = match direction {
      VerticalDirection::Up => self.selections.as_slice().first(),
      VerticalDirection::Down => self.selections.as_slice().last(),
    };
    let Some(&edge) = edge else {
      return;
    };
    let row = self.row_of(self.buffer.line_of(edge.head));
    let at_edge = match direction {
      VerticalDirection::Up => row == 0,
      VerticalDirection::Down => row + 1 >= self.row_count(),
    };
    if at_edge {
      return;
    }
    let cursor = Selection {
      goal: edge.goal,
      ..Selection::cursor(edge.head)
    };
    let added = self.vertical(cursor, direction, 1, false);
    self.set_selections(self.selections.with_added(added));
  }
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use flareite_workspace::FileStamp;

  use super::*;
  use crate::testing::{doc, render, run};

  fn insert(text: &str) -> EditorOp {
    EditorOp::Insert(text.to_owned())
  }

  fn move_by(motion: Motion, extend: bool) -> EditorOp {
    EditorOp::Move { motion, extend }
  }

  const BACKSPACE: EditorOp = EditorOp::Delete {
    direction: Direction::Backward,
    unit: Unit::Grapheme,
  };

  fn temp_stamp() -> (tempfile::TempDir, PathBuf, FileStamp) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("file.txt");
    std::fs::write(&path, "x").expect("write");
    let stamp = FileStamp::read(&path).expect("stat").expect("exists");
    (dir, path, stamp)
  }

  #[test]
  fn typing_applies_at_every_cursor() {
    assert_eq!(run(&mut doc("a|b|c"), [insert("X")]), "aX|bX|c");
    assert_eq!(run(&mut doc("a[bc]d"), [insert("X")]), "aX|d");
  }

  #[test]
  fn backspace_deletes_whole_graphemes() {
    assert_eq!(run(&mut doc("e\u{301}|x"), [BACKSPACE]), "|x");
    assert_eq!(run(&mut doc("a🦀|"), [BACKSPACE]), "a|");
    assert_eq!(run(&mut doc("a\r\n|b"), [BACKSPACE]), "a|b");
  }

  #[test]
  fn adjacent_cursors_merge_after_deleting() {
    assert_eq!(run(&mut doc("ab|c|"), [BACKSPACE]), "a|");
  }

  #[test]
  fn word_deletion_removes_runs() {
    let delete_word = EditorOp::Delete {
      direction: Direction::Backward,
      unit: Unit::Word,
    };
    assert_eq!(run(&mut doc("foo bar|"), [delete_word.clone()]), "foo |");
    let forward = EditorOp::Delete {
      direction: Direction::Forward,
      unit: Unit::Word,
    };
    assert_eq!(run(&mut doc("|foo bar"), [forward]), "| bar");
  }

  #[test]
  fn deleting_at_the_edges_changes_nothing() {
    let mut document = doc("|abc");
    assert!(!document.apply(BACKSPACE).expect("applies"));
    assert_eq!(document.version().get(), 0);
    assert!(!document.is_dirty());
  }

  #[test]
  fn newline_uses_the_document_line_ending_and_keeps_indentation() {
    assert_eq!(
      run(&mut doc("    foo|\r\nbar"), [EditorOp::Newline]),
      "    foo\r\n    |\r\nbar"
    );
    assert_eq!(run(&mut doc("\tx|"), [EditorOp::Newline]), "\tx\n\t|");
    // A cursor inside the indentation copies only the part before it.
    assert_eq!(run(&mut doc("  |  x"), [EditorOp::Newline]), "  \n  |  x");
  }

  #[test]
  fn inserted_line_breaks_are_normalized() {
    assert_eq!(
      run(&mut doc("a|\r\nb"), [insert("1\n2\r3")]),
      "a1\r\n2\r\n3|\r\nb"
    );
  }

  #[test]
  fn tab_inserts_spaces_to_the_next_tab_stop() {
    assert_eq!(run(&mut doc("ab|"), [EditorOp::Tab]), "ab  |");
    assert_eq!(run(&mut doc("|x"), [EditorOp::Tab]), "    |x");
  }

  #[test]
  fn horizontal_movement_collapses_or_extends_selections() {
    assert_eq!(
      run(&mut doc("a[bc]d"), [move_by(Motion::Left, false)]),
      "a|bcd"
    );
    assert_eq!(
      run(&mut doc("a[bc]d"), [move_by(Motion::Right, false)]),
      "abc|d"
    );
    assert_eq!(
      run(&mut doc("|🦀x"), [move_by(Motion::Right, true)]),
      "[🦀]x"
    );
    assert_eq!(
      run(&mut doc("foo bar|"), [move_by(Motion::WordLeft, true)]),
      "foo [bar]"
    );
  }

  #[test]
  fn vertical_movement_keeps_the_goal_column() {
    let mut document = doc("abcdef|\nab\nabcdefgh");
    assert_eq!(
      run(&mut document, [move_by(Motion::Down, false)]),
      "abcdef\nab|\nabcdefgh"
    );
    assert_eq!(
      run(&mut document, [move_by(Motion::Down, false)]),
      "abcdef\nab\nabcdef|gh"
    );
    assert_eq!(
      run(&mut document, [move_by(Motion::Down, false)]),
      "abcdef\nab\nabcdefgh|"
    );
    assert_eq!(
      run(&mut doc("ab|c\nd"), [move_by(Motion::Up, false)]),
      "|abc\nd"
    );
  }

  #[test]
  fn vertical_movement_counts_tabs_as_tab_stops() {
    assert_eq!(
      run(&mut doc("\tx|\nabcdefgh"), [move_by(Motion::Down, false)]),
      "\tx\nabcde|fgh"
    );
  }

  #[test]
  fn line_start_toggles_between_indentation_and_column_zero() {
    let mut document = doc("  foo|");
    assert_eq!(
      run(&mut document, [move_by(Motion::LineStart, false)]),
      "  |foo"
    );
    assert_eq!(
      run(&mut document, [move_by(Motion::LineStart, false)]),
      "|  foo"
    );
    assert_eq!(
      run(&mut document, [move_by(Motion::LineEnd, true)]),
      "[  foo]"
    );
  }

  #[test]
  fn added_cursors_edit_together() {
    let mut document = doc("ab|c\nx\nabcd");
    assert_eq!(
      run(
        &mut document,
        [EditorOp::AddCursor(VerticalDirection::Down)]
      ),
      "ab|c\nx|\nabcd"
    );
    assert_eq!(
      run(
        &mut document,
        [EditorOp::AddCursor(VerticalDirection::Down)]
      ),
      "ab|c\nx|\nab|cd"
    );
    assert_eq!(run(&mut document, [insert("Z")]), "abZ|c\nxZ|\nabZ|cd");
    assert_eq!(
      run(&mut document, [EditorOp::CollapseSelections]),
      "abZc\nxZ\nabZ|cd"
    );
  }

  #[test]
  fn mouse_style_cursor_placement() {
    let mut document = doc("|hello world");
    let set = |offset, extend, add| EditorOp::SetCursor {
      offset,
      extend,
      add,
    };
    assert_eq!(run(&mut document, [set(6, false, false)]), "hello |world");
    assert_eq!(run(&mut document, [set(9, true, false)]), "hello [wor]ld");
    assert_eq!(run(&mut document, [set(1, false, true)]), "h|ello [wor]ld");
    assert_eq!(
      run(&mut document, [EditorOp::SelectWord { offset: 2 }]),
      "[hello] world"
    );
    // Offsets inside a grapheme snap to its start.
    assert_eq!(
      run(&mut doc("|e\u{301}x"), [set(2, false, false)]),
      "|e\u{301}x"
    );
  }

  #[test]
  fn cursor_undo_restores_earlier_selections_until_the_text_changes() {
    let mut document = doc("|foo bar");
    run(
      &mut document,
      [
        move_by(Motion::WordRight, false),
        move_by(Motion::WordRight, true),
      ],
    );
    assert_eq!(run(&mut document, [EditorOp::CursorUndo]), "foo| bar");
    assert_eq!(run(&mut document, [EditorOp::CursorUndo]), "|foo bar");
    assert_eq!(run(&mut document, [EditorOp::CursorUndo]), "|foo bar");

    let mut document = doc("|ab");
    run(
      &mut document,
      [
        move_by(Motion::Right, false),
        insert("x"),
        EditorOp::CursorUndo,
      ],
    );
    assert_eq!(render(&document), "ax|b");
  }

  #[test]
  fn select_all_and_selected_text() {
    let mut document = doc("one\ntwo|");
    assert_eq!(document.selected_text(), None);
    run(&mut document, [EditorOp::SelectAll]);
    assert_eq!(document.selected_text().as_deref(), Some("one\ntwo"));
    let document = doc("[a]b[c]");
    assert_eq!(document.selected_text().as_deref(), Some("a\nc"));
  }

  #[test]
  fn undo_and_redo_restore_text_and_selections() {
    let mut document = doc("a[bc]d");
    run(&mut document, [insert("X")]);
    assert_eq!(run(&mut document, [EditorOp::Undo]), "a[bc]d");
    assert_eq!(run(&mut document, [EditorOp::Redo]), "aX|d");
    assert!(!document.apply(EditorOp::Redo).expect("applies"));
  }

  #[test]
  fn typing_groups_break_at_word_starts_and_cursor_moves() {
    let mut document = doc("|");
    for c in ["f", "o", "o", " ", "b", "a", "r"] {
      run(&mut document, [insert(c)]);
    }
    assert_eq!(run(&mut document, [EditorOp::Undo]), "foo|");
    assert_eq!(run(&mut document, [EditorOp::Undo]), "|");

    let mut document = doc("|");
    run(
      &mut document,
      [insert("a"), move_by(Motion::Left, false), insert("b")],
    );
    assert_eq!(run(&mut document, [EditorOp::Undo]), "|a");
  }

  #[test]
  fn backspaces_group_together() {
    let mut document = doc("abc|");
    run(&mut document, [BACKSPACE, BACKSPACE]);
    assert_eq!(run(&mut document, [EditorOp::Undo]), "abc|");
  }

  #[test]
  fn dirty_state_follows_the_saved_text() {
    let (_dir, path, stamp) = temp_stamp();
    let mut document = doc("|");
    assert!(!document.is_dirty());

    run(&mut document, [insert("a")]);
    assert!(document.is_dirty());
    run(&mut document, [EditorOp::Undo]);
    assert!(!document.is_dirty(), "undo back to the saved text is clean");
    run(&mut document, [EditorOp::Redo]);
    assert!(document.is_dirty());

    let point = document.save_point();
    document.mark_saved(&point, path.clone(), stamp.clone());
    assert!(!document.is_dirty());
    assert_eq!(document.title(), "file.txt");

    // Typing after a save starts a new undo group, so undo returns to the saved text.
    run(&mut document, [insert("b")]);
    assert!(document.is_dirty());
    run(&mut document, [EditorOp::Undo]);
    assert!(!document.is_dirty());
    assert_eq!(document.buffer().contents(), "a");
    run(&mut document, [EditorOp::Undo]);
    assert!(document.is_dirty(), "undo past the saved text is dirty");

    // Edits made while a save is in flight remain dirty afterwards.
    let point = document.save_point();
    run(&mut document, [insert("late")]);
    document.mark_saved(&point, path, stamp);
    assert!(document.is_dirty());
  }

  #[test]
  fn versions_increase_with_every_change_including_undo() {
    let mut document = doc("|");
    run(&mut document, [insert("a")]);
    let after_insert = document.version();
    run(&mut document, [EditorOp::Undo]);
    assert!(document.version() > after_insert);
    run(&mut document, [move_by(Motion::Right, false)]);
    assert_eq!(
      document.version().get(),
      2,
      "movement does not change the version"
    );
  }
}
