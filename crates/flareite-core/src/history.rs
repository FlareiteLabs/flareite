//! Undo and redo.
//!
//! Each applied change is stored as a forward change set and its inverse.
//! Consecutive similar edits (typing, deleting) are grouped so one undo
//! reverts a burst of typing. Retention is bounded by group count and by the
//! bytes of text the history holds; the oldest groups are dropped first.

use std::collections::VecDeque;

use flareite_text::{ChangeSet, EditError, TextBuffer};

use crate::selection::Selections;

/// Identifies a point in a document's history. Two equal ids mean equal
/// content, which is how dirty state survives undo back to the saved text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StateId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
  /// Maximum undo groups retained.
  pub max_groups: usize,
  /// Maximum bytes of text (inserted plus removed) retained across groups.
  pub max_bytes: usize,
}

impl Default for HistoryLimits {
  fn default() -> Self {
    Self {
      max_groups: 10_000,
      max_bytes: 64 * 1024 * 1024,
    }
  }
}

/// How an edit may group with its neighbours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
  Typing,
  Deleting,
  Other,
}

#[derive(Debug)]
struct Step {
  forward: ChangeSet,
  inverse: ChangeSet,
}

#[derive(Debug)]
struct Group {
  steps: Vec<Step>,
  before: Selections,
  after: Selections,
  /// State before the group's first step and after its last.
  previous: StateId,
  state: StateId,
  kind: EditKind,
  bytes: usize,
}

#[derive(Debug)]
pub struct History {
  undo: VecDeque<Group>,
  redo: Vec<Group>,
  limits: HistoryLimits,
  bytes: usize,
  current: StateId,
  next_id: u64,
  /// When set, the next edit starts a new group.
  sealed: bool,
}

impl History {
  pub fn new(limits: HistoryLimits) -> Self {
    Self {
      undo: VecDeque::new(),
      redo: Vec::new(),
      limits,
      bytes: 0,
      current: StateId(0),
      next_id: 1,
      sealed: false,
    }
  }

  pub fn current_state(&self) -> StateId {
    self.current
  }

  pub fn can_undo(&self) -> bool {
    !self.undo.is_empty()
  }

  pub fn can_redo(&self) -> bool {
    !self.redo.is_empty()
  }

  /// Ends the current group, e.g. after a save or cursor movement.
  pub fn seal(&mut self) {
    self.sealed = true;
  }

  /// Moves the selections the current group ends with, for a cursor
  /// movement that continues it (typing over a closer). Does nothing once
  /// the group has ended.
  pub(crate) fn continue_group(&mut self, after: Selections) {
    if !self.sealed
      && let Some(last) = self.undo.back_mut()
    {
      last.after = after;
    }
  }

  /// Records a change that has already been applied. With `merge`, it joins
  /// the previous group if that group has the same kind and ended with the
  /// selections this change started from.
  pub fn record(
    &mut self,
    forward: ChangeSet,
    inverse: ChangeSet,
    before: Selections,
    after: Selections,
    kind: EditKind,
    merge: bool,
  ) {
    for group in self.redo.drain(..) {
      self.bytes -= group.bytes;
    }
    let bytes = forward.text_bytes() + inverse.text_bytes();
    let state = StateId(self.next_id);
    self.next_id += 1;
    self.bytes += bytes;

    let step = Step { forward, inverse };
    match self.undo.back_mut() {
      Some(last)
        if merge
          && !self.sealed
          && kind != EditKind::Other
          && last.kind == kind
          && last.after == before =>
      {
        last.steps.push(step);
        last.after = after;
        last.state = state;
        last.bytes += bytes;
      }
      _ => self.undo.push_back(Group {
        steps: vec![step],
        before,
        after,
        previous: self.current,
        state,
        kind,
        bytes,
      }),
    }
    self.current = state;
    self.sealed = false;

    while self.undo.len() > 1
      && (self.undo.len() > self.limits.max_groups || self.bytes > self.limits.max_bytes)
    {
      if let Some(evicted) = self.undo.pop_front() {
        self.bytes -= evicted.bytes;
      }
    }
  }

  /// Reverts the most recent group. Returns the selections to restore, or
  /// `None` if there is nothing to undo.
  pub fn undo(&mut self, buffer: &mut TextBuffer) -> Result<Option<Selections>, EditError> {
    let Some(group) = self.undo.pop_back() else {
      return Ok(None);
    };
    for step in group.steps.iter().rev() {
      buffer.apply(&step.inverse)?;
    }
    self.current = group.previous;
    self.sealed = true;
    let selections = group.before.clone();
    self.redo.push(group);
    Ok(Some(selections))
  }

  /// Re-applies the most recently undone group.
  pub fn redo(&mut self, buffer: &mut TextBuffer) -> Result<Option<Selections>, EditError> {
    let Some(group) = self.redo.pop() else {
      return Ok(None);
    };
    for step in &group.steps {
      buffer.apply(&step.forward)?;
    }
    self.current = group.state;
    self.sealed = true;
    let selections = group.after.clone();
    self.undo.push_back(group);
    Ok(Some(selections))
  }
}

#[cfg(test)]
mod tests {
  use flareite_text::Edit;

  use super::*;

  fn type_text(history: &mut History, buffer: &mut TextBuffer, at: usize, text: &str, merge: bool) {
    let forward = ChangeSet::single(Edit::insert(at, text)).expect("valid");
    let inverse = buffer.apply(&forward).expect("applies");
    let end = at + text.len();
    history.record(
      forward,
      inverse,
      Selections::cursor(at),
      Selections::cursor(end),
      EditKind::Typing,
      merge,
    );
  }

  #[test]
  fn undo_and_redo_restore_text_and_state() {
    let mut buffer = TextBuffer::new();
    let mut history = History::new(HistoryLimits::default());
    let initial = history.current_state();
    type_text(&mut history, &mut buffer, 0, "hello", false);
    let typed = history.current_state();

    let selections = history.undo(&mut buffer).expect("undo");
    assert_eq!(selections, Some(Selections::cursor(0)));
    assert_eq!(buffer.contents(), "");
    assert_eq!(history.current_state(), initial);

    let selections = history.redo(&mut buffer).expect("redo");
    assert_eq!(selections, Some(Selections::cursor(5)));
    assert_eq!(buffer.contents(), "hello");
    assert_eq!(history.current_state(), typed);
    assert_eq!(history.redo(&mut buffer).expect("redo"), None);
  }

  #[test]
  fn consecutive_typing_merges_into_one_group() {
    let mut buffer = TextBuffer::new();
    let mut history = History::new(HistoryLimits::default());
    type_text(&mut history, &mut buffer, 0, "a", true);
    type_text(&mut history, &mut buffer, 1, "b", true);
    type_text(&mut history, &mut buffer, 2, "c", true);
    history.undo(&mut buffer).expect("undo");
    assert_eq!(buffer.contents(), "");
  }

  #[test]
  fn sealing_or_moving_starts_a_new_group() {
    let mut buffer = TextBuffer::new();
    let mut history = History::new(HistoryLimits::default());
    type_text(&mut history, &mut buffer, 0, "a", true);
    history.seal();
    type_text(&mut history, &mut buffer, 1, "b", true);
    // Typing somewhere else does not continue the group either.
    type_text(&mut history, &mut buffer, 0, "c", true);
    history.undo(&mut buffer).expect("undo");
    assert_eq!(buffer.contents(), "ab");
    history.undo(&mut buffer).expect("undo");
    assert_eq!(buffer.contents(), "a");
  }

  #[test]
  fn new_edits_discard_the_redo_stack() {
    let mut buffer = TextBuffer::new();
    let mut history = History::new(HistoryLimits::default());
    type_text(&mut history, &mut buffer, 0, "a", false);
    history.undo(&mut buffer).expect("undo");
    type_text(&mut history, &mut buffer, 0, "b", false);
    assert!(!history.can_redo());
    assert_eq!(history.redo(&mut buffer).expect("redo"), None);
    assert_eq!(buffer.contents(), "b");
  }

  #[test]
  fn retention_limits_drop_the_oldest_groups() {
    let mut buffer = TextBuffer::new();
    let mut history = History::new(HistoryLimits {
      max_groups: 2,
      max_bytes: usize::MAX,
    });
    for (i, text) in ["a", "b", "c"].into_iter().enumerate() {
      type_text(&mut history, &mut buffer, i, text, false);
    }
    history.undo(&mut buffer).expect("undo");
    history.undo(&mut buffer).expect("undo");
    assert!(!history.can_undo());
    assert_eq!(buffer.contents(), "a");

    let mut history = History::new(HistoryLimits {
      max_groups: usize::MAX,
      max_bytes: 8,
    });
    let mut buffer = TextBuffer::new();
    type_text(&mut history, &mut buffer, 0, "12345", false);
    type_text(&mut history, &mut buffer, 5, "6789", false);
    history.undo(&mut buffer).expect("undo");
    assert!(!history.can_undo());
    assert_eq!(buffer.contents(), "12345");
  }
}
