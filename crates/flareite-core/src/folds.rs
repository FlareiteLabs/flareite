//! Folding: hiding the lines of collapsed regions.
//!
//! The core doesn't know where a document's regions are. Callers pass the
//! foldable regions (from a language's fold query, or from
//! [`Document::indentation_folds`]) to the fold operations as line ranges
//! `start..end`: line `start` stays visible and `start + 1..end` are hidden
//! while the region is folded.
//!
//! Folded regions are kept as byte offsets, so they follow edits. An edit
//! that removes the line break after a region's first or last line unfolds
//! it, as does a new line typed at the end of its first line, or a selection
//! reaching into its hidden lines (after a search, say). Cursor movement steps over hidden lines instead, and line
//! operations treat a folded region as part of its first line.
//!
//! Display rows number the visible lines, so a document with folded regions
//! has fewer rows than lines.

use std::ops::Range;

use flareite_text::{TextBuffer, Version};

use crate::document::Document;
use crate::selection::Selection;

/// Leading whitespace beyond this many characters counts as this much.
const MAX_INDENT_CHARS: usize = 1000;

/// A folded region: the end of its first line (before the line break) and
/// the end of its last hidden line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Fold {
  header: usize,
  end: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct Folds {
  /// Sorted, without duplicates.
  folds: Vec<Fold>,
  /// The text version `folds` refer to.
  version: Version,
  /// The hidden lines as sorted, merged ranges, each with the number of
  /// hidden lines before it.
  hidden: Vec<(Range<usize>, usize)>,
  /// Counts changes to the folded regions.
  generation: u64,
}

impl Folds {
  pub(crate) fn new(version: Version) -> Self {
    Self {
      folds: Vec::new(),
      version,
      hidden: Vec::new(),
      generation: 0,
    }
  }

  /// Follows the edits made since the last call.
  pub(crate) fn sync(&mut self, buffer: &TextBuffer) {
    let since = std::mem::replace(&mut self.version, buffer.version());
    if since == self.version || self.folds.is_empty() {
      return;
    }
    let Some(edits) = buffer.edits_since(since) else {
      self.folds.clear();
      self.hidden.clear();
      return;
    };
    self.folds.retain_mut(|fold| {
      for edit in &edits {
        let removes = |offset: usize| edit.start <= offset && offset < edit.old_end;
        let new_line = edit.text.contains(['\n', '\r']);
        if removes(fold.header) || removes(fold.end) || (new_line && edit.start == fold.header) {
          return false;
        }
        // The offsets stay at their line breaks, except that new lines
        // inserted right before the last one stay out of the region.
        fold.header = edit.map_offset(fold.header, true);
        fold.end = edit.map_offset(fold.end, !new_line);
      }
      true
    });
    self.rebuild(buffer);
  }

  fn rebuild(&mut self, buffer: &TextBuffer) {
    self.generation += 1;
    self
      .folds
      .retain(|fold| buffer.line_of(fold.end) > buffer.line_of(fold.header));
    self.folds.sort();
    self.folds.dedup();
    let mut merged: Vec<Range<usize>> = Vec::new();
    for fold in &self.folds {
      let lines = buffer.line_of(fold.header) + 1..buffer.line_of(fold.end) + 1;
      match merged.last_mut() {
        Some(last) if lines.start <= last.end => last.end = last.end.max(lines.end),
        _ => merged.push(lines),
      }
    }
    let mut before = 0;
    self.hidden = merged
      .into_iter()
      .map(|lines| {
        let count = before;
        before += lines.len();
        (lines, count)
      })
      .collect();
  }

  fn is_empty(&self) -> bool {
    self.folds.is_empty()
  }

  /// The folded regions as line ranges, in order.
  fn regions(&self, buffer: &TextBuffer) -> Vec<Range<usize>> {
    self
      .folds
      .iter()
      .map(|fold| buffer.line_of(fold.header)..buffer.line_of(fold.end) + 1)
      .collect()
  }

  /// Folds `regions` (line ranges); ones that hide nothing are skipped.
  fn add(&mut self, buffer: &TextBuffer, regions: impl IntoIterator<Item = Range<usize>>) {
    let line_count = buffer.line_count();
    for region in regions {
      if region.start + 1 < region.end && region.end <= line_count {
        self.folds.push(Fold {
          header: buffer.line_end(region.start),
          end: buffer.line_end(region.end - 1),
        });
      }
    }
    self.rebuild(buffer);
  }

  /// Unfolds the regions `unfold` picks.
  fn remove(&mut self, buffer: &TextBuffer, mut unfold: impl FnMut(&Range<usize>) -> bool) {
    let before = self.folds.len();
    self
      .folds
      .retain(|fold| !unfold(&(buffer.line_of(fold.header)..buffer.line_of(fold.end) + 1)));
    if self.folds.len() != before {
      self.rebuild(buffer);
    }
  }

  /// Unfolds the regions inside `range` (bytes) and returns them as
  /// `(header, end)` offsets, to fold again where their text moves.
  pub(crate) fn take_within(&mut self, range: Range<usize>) -> Vec<(usize, usize)> {
    let mut taken = Vec::new();
    self.folds.retain(|fold| {
      let inside = range.start <= fold.header && fold.end <= range.end;
      if inside {
        taken.push((fold.header, fold.end));
      }
      !inside
    });
    taken
  }

  /// Folds regions given as `(header, end)` offsets (see
  /// [`Self::take_within`]). The folds must be up to date.
  pub(crate) fn put(
    &mut self,
    buffer: &TextBuffer,
    folds: impl IntoIterator<Item = (usize, usize)>,
  ) {
    self
      .folds
      .extend(folds.into_iter().map(|(header, end)| Fold { header, end }));
    self.rebuild(buffer);
  }

  /// The hidden lines `line` is among, if it is hidden.
  pub(crate) fn hidden_at(&self, line: usize) -> Option<Range<usize>> {
    let index = self.hidden.partition_point(|(lines, _)| lines.end <= line);
    self
      .hidden
      .get(index)
      .map(|(lines, _)| lines.clone())
      .filter(|lines| lines.start <= line)
  }

  /// Hidden lines before the `index`th hidden range.
  fn hidden_before(&self, index: usize) -> usize {
    match index.checked_sub(1).and_then(|last| self.hidden.get(last)) {
      Some((lines, before)) => before + lines.len(),
      None => 0,
    }
  }

  /// Counts changes to the folded regions.
  pub(crate) fn generation(&self) -> u64 {
    self.generation
  }

  /// Visible lines among `line_count`.
  pub(crate) fn row_count(&self, line_count: usize) -> usize {
    line_count - self.hidden_before(self.hidden.len())
  }

  /// The visible line `line` is, counting from 0, or its region's first
  /// line's if it's hidden.
  pub(crate) fn row_of(&self, line: usize) -> usize {
    let index = self.hidden.partition_point(|(lines, _)| lines.end <= line);
    match self.hidden.get(index) {
      // A hidden line shows as its region's first line.
      Some((lines, before)) if lines.start <= line => lines.start - 1 - before,
      _ => line - self.hidden_before(index),
    }
  }

  /// The `row`th visible line.
  pub(crate) fn line_at_row(&self, row: usize) -> usize {
    let index = self
      .hidden
      .partition_point(|(lines, before)| lines.start - before <= row);
    row + self.hidden_before(index)
  }
}

impl Document {
  /// The visible lines of `lines`, as sorted ranges.
  pub fn visible_lines(&self, lines: Range<usize>) -> Vec<Range<usize>> {
    let mut visible = Vec::new();
    let mut start = lines.start;
    let first = self
      .folds
      .hidden
      .partition_point(|(hidden, _)| hidden.end <= lines.start);
    for (hidden, _) in &self.folds.hidden[first..] {
      if hidden.start >= lines.end {
        break;
      }
      if hidden.start > start {
        visible.push(start..hidden.start);
      }
      start = start.max(hidden.end);
    }
    if start < lines.end {
      visible.push(start..lines.end);
    }
    visible
  }

  /// A number that changes whenever the folded regions do (including when
  /// edits move them), so a cache of what is shown can tell it is stale.
  pub fn fold_generation(&self) -> u64 {
    self.folds.generation
  }

  /// The folded regions, as line ranges in order. Nested regions are
  /// included.
  pub fn folded(&self) -> Vec<Range<usize>> {
    self.folds.regions(&self.buffer)
  }

  /// The visible lines in `lines` whose region is folded, in order.
  pub fn folded_lines(&self, lines: Range<usize>) -> Vec<usize> {
    self
      .folds
      .hidden
      .iter()
      .map(|(hidden, _)| hidden.start - 1)
      .filter(|line| lines.contains(line))
      .collect()
  }

  /// Folds `regions` (line ranges), e.g. ones remembered from an earlier
  /// session. Ones that hide nothing or reach past the end are skipped.
  pub fn set_folded(&mut self, regions: impl IntoIterator<Item = Range<usize>>) {
    self.folds.sync(&self.buffer);
    self.folds.add(&self.buffer, regions);
    self.leave_folds();
    self.sync_wraps();
  }

  /// Unfolds every region a selection reaches into.
  pub(crate) fn reveal_selections(&mut self) {
    if self.folds.is_empty() {
      return;
    }
    let lines: Vec<usize> = self
      .selections
      .iter()
      .flat_map(|s| [s.anchor, s.head])
      .map(|offset| self.buffer.line_of(offset))
      .filter(|&line| self.folds.hidden_at(line).is_some())
      .collect();
    if !lines.is_empty() {
      self.folds.remove(&self.buffer, |region| {
        lines
          .iter()
          .any(|&line| region.start < line && line < region.end)
      });
    }
  }

  /// Moves selections out of hidden lines, to the end of their region's
  /// first line.
  fn leave_folds(&mut self) {
    let out = |offset: usize| match self.folds.hidden_at(self.buffer.line_of(offset)) {
      Some(hidden) => self.buffer.line_end(hidden.start - 1),
      None => offset,
    };
    let selections = self.selections.map(|s| Selection {
      goal: s.goal,
      ..Selection::new(out(s.anchor), out(s.head))
    });
    self.selections = selections;
  }

  /// `moved` (the result of moving `old`), stepped over any hidden lines it
  /// landed in: forward to the next visible line, back to the end of the
  /// region's first line.
  pub(crate) fn step_over_folds(&self, old: Selection, moved: Selection) -> Selection {
    let Some(hidden) = self.folds.hidden_at(self.buffer.line_of(moved.head)) else {
      return moved;
    };
    let head = if moved.head >= old.head && hidden.end < self.buffer.line_count() {
      self.buffer.line_start(hidden.end)
    } else {
      self.buffer.line_end(hidden.start - 1)
    };
    let anchor = if moved.is_empty() { head } else { moved.anchor };
    Selection {
      goal: moved.goal,
      ..Selection::new(anchor, head)
    }
  }

  /// The last line of `line`'s folded region if it starts one, else `line`.
  pub(crate) fn through_fold(&self, line: usize) -> usize {
    self
      .folds
      .hidden_at(line + 1)
      .map_or(line, |hidden| hidden.end - 1)
  }

  pub(crate) fn is_hidden(&self, line: usize) -> bool {
    self.folds.hidden_at(line).is_some()
  }

  /// The lines with a selection's head, in order, without repeats.
  fn head_lines(&self) -> Vec<usize> {
    let mut lines: Vec<usize> = self
      .selections
      .iter()
      .map(|s| self.buffer.line_of(s.head))
      .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
  }

  /// Folds `regions`, moving selections out of the hidden lines.
  fn collapse(&mut self, regions: impl IntoIterator<Item = Range<usize>>) {
    self.folds.add(&self.buffer, regions);
    self.leave_folds();
  }

  /// Folds the innermost unfolded region around each cursor.
  pub(crate) fn fold(&mut self, regions: &[Range<usize>]) {
    let folded = self.folded();
    let picked: Vec<Range<usize>> = self
      .head_lines()
      .into_iter()
      .filter_map(|line| {
        innermost(
          regions.iter().filter(|region| !folded.contains(region)),
          line,
        )
      })
      .collect();
    self.collapse(picked);
  }

  /// Unfolds the folded regions starting on a cursor's line.
  pub(crate) fn unfold(&mut self) {
    let lines = self.head_lines();
    self
      .folds
      .remove(&self.buffer, |region| lines.contains(&region.start));
  }

  /// Unfolds the region starting on each cursor's line if it is folded, and
  /// otherwise folds the innermost region around the cursor.
  pub(crate) fn toggle_fold(&mut self, regions: &[Range<usize>]) {
    let folded = self.folded();
    let (unfold, fold): (Vec<usize>, Vec<usize>) = self
      .head_lines()
      .into_iter()
      .partition(|line| folded.iter().any(|region| region.start == *line));
    self
      .folds
      .remove(&self.buffer, |region| unfold.contains(&region.start));
    let picked: Vec<Range<usize>> = fold
      .into_iter()
      .filter_map(|line| innermost(regions.iter(), line))
      .collect();
    self.collapse(picked);
  }

  /// Unfolds the regions starting on `line`, or if none is folded, folds
  /// the largest region starting there (a click on its marker).
  pub(crate) fn toggle_fold_at(&mut self, line: usize, regions: &[Range<usize>]) {
    if self.folded().iter().any(|region| region.start == line) {
      self
        .folds
        .remove(&self.buffer, |region| region.start == line);
      return;
    }
    let largest = regions
      .iter()
      .filter(|region| region.start == line)
      .max_by_key(|region| region.end);
    self.collapse(largest.cloned());
  }

  pub(crate) fn fold_all(&mut self, regions: &[Range<usize>]) {
    self.collapse(regions.iter().cloned());
  }

  pub(crate) fn unfold_all(&mut self) {
    self.folds.remove(&self.buffer, |_| true);
  }

  pub(crate) fn unfold_regions(&mut self, regions: &[Range<usize>]) {
    self
      .folds
      .remove(&self.buffer, |region| regions.contains(region));
  }

  /// Folds the innermost region around each cursor and every region in it.
  pub(crate) fn fold_recursively(&mut self, regions: &[Range<usize>]) {
    let outer: Vec<Range<usize>> = self
      .head_lines()
      .into_iter()
      .filter_map(|line| innermost(regions.iter(), line))
      .collect();
    let picked: Vec<Range<usize>> = regions
      .iter()
      .filter(|region| outer.iter().any(|outer| contains(outer, region)))
      .cloned()
      .collect();
    self.collapse(picked);
  }

  /// Unfolds the innermost region around each cursor (or the folded region
  /// starting on its line) and every region in it.
  pub(crate) fn unfold_recursively(&mut self, regions: &[Range<usize>]) {
    let folded = self.folded();
    let outer: Vec<Range<usize>> = self
      .head_lines()
      .into_iter()
      .filter_map(|line| {
        let starting = folded
          .iter()
          .filter(|region| region.start == line)
          .max_by_key(|region| region.end);
        starting
          .cloned()
          .or_else(|| innermost(regions.iter(), line))
      })
      .collect();
    self.folds.remove(&self.buffer, |region| {
      outer.iter().any(|outer| contains(outer, region))
    });
  }

  /// Folds the regions nested `level` deep (1 is outermost), except those
  /// around a cursor.
  pub(crate) fn fold_level(&mut self, level: usize, regions: &[Range<usize>]) {
    let mut sorted: Vec<&Range<usize>> = regions.iter().collect();
    sorted.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let lines = self.head_lines();
    let mut open: Vec<&Range<usize>> = Vec::new();
    let mut picked = Vec::new();
    for region in sorted {
      while open.last().is_some_and(|outer| !contains(outer, region)) {
        open.pop();
      }
      open.push(region);
      let around_cursor = lines
        .iter()
        .any(|&line| region.start <= line && line < region.end);
      if open.len() == level && !around_cursor {
        picked.push(region.clone());
      }
    }
    self.collapse(picked);
  }

  /// The indentation of `line` in columns (tabs to tab stops), or `None`
  /// if it is blank.
  fn indent_width(&self, line: usize) -> Option<usize> {
    let mut width = 0;
    let mut chars = 0;
    for c in self
      .buffer
      .chars_at(self.buffer.line_start(line))
      .take(MAX_INDENT_CHARS)
    {
      chars += 1;
      match c {
        ' ' => width += 1,
        '\t' => width += self.tab_size - width % self.tab_size,
        '\n' | '\r' => return None,
        _ => return Some(width),
      }
    }
    (chars == MAX_INDENT_CHARS).then_some(width)
  }

  /// The first line from `line` on that isn't blank, and its indentation.
  fn next_text_line(&self, line: usize) -> Option<(usize, usize)> {
    (line..self.buffer.line_count()).find_map(|line| Some((line, self.indent_width(line)?)))
  }

  /// The end of the indentation region starting on `line` (indented
  /// `indent`): after its last line indented deeper, before the next line
  /// indented as little.
  fn indentation_end(&self, line: usize, indent: usize) -> usize {
    let mut last = line;
    for next in line + 1..self.buffer.line_count() {
      match self.indent_width(next) {
        Some(width) if width <= indent => break,
        Some(_) => last = next,
        None => {}
      }
    }
    last + 1
  }

  /// Whether an indentation region starts on `line`: the next line that
  /// isn't blank is indented deeper.
  fn starts_indentation_region(&self, line: usize) -> Option<usize> {
    let indent = self.indent_width(line)?;
    let (_, next) = self.next_text_line(line + 1)?;
    (next > indent).then_some(indent)
  }

  /// Foldable regions by indentation, for languages without a fold query:
  /// a line followed by lines indented deeper, up to the next line that
  /// isn't. With `line`, only the regions starting on or around it;
  /// otherwise all of them, in order.
  pub fn indentation_folds(&self, line: Option<usize>) -> Vec<Range<usize>> {
    let Some(line) = line else {
      return self.all_indentation_folds();
    };
    let mut found = Vec::new();
    if line >= self.buffer.line_count() {
      return found;
    }
    if let Some(indent) = self.starts_indentation_region(line) {
      found.push(line..self.indentation_end(line, indent));
    }
    // A blank line belongs with the next line that isn't.
    let Some(mut indent) = self
      .indent_width(line)
      .or_else(|| self.next_text_line(line).map(|(_, indent)| indent))
    else {
      return found;
    };
    let mut above = line;
    while indent > 0 && above > 0 {
      above -= 1;
      if let Some(width) = self.indent_width(above)
        && width < indent
      {
        found.push(above..self.indentation_end(above, width));
        indent = width;
      }
    }
    found.retain(|region| region.start + 1 < region.end);
    found
  }

  fn all_indentation_folds(&self) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    // Lines that may start a region, with their indentation.
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut last_text = 0;
    for line in 0..self.buffer.line_count() {
      let Some(indent) = self.indent_width(line) else {
        continue;
      };
      while let Some(&(start, width)) = open.last()
        && width >= indent
      {
        open.pop();
        if last_text > start {
          found.push(start..last_text + 1);
        }
      }
      open.push((line, indent));
      last_text = line;
    }
    for (start, _) in open {
      if last_text > start {
        found.push(start..last_text + 1);
      }
    }
    found.sort_by_key(|region| region.start);
    found
  }

  /// The lines in `lines` that start an indentation region.
  pub fn indentation_fold_starts(&self, lines: Range<usize>) -> Vec<usize> {
    let end = lines.end.min(self.buffer.line_count());
    (lines.start..end)
      .filter(|&line| self.starts_indentation_region(line).is_some())
      .collect()
  }
}

/// The smallest of `regions` around `line` (its first line or a hidden one).
fn innermost<'a>(
  regions: impl Iterator<Item = &'a Range<usize>>,
  line: usize,
) -> Option<Range<usize>> {
  regions
    .filter(|region| region.start <= line && line < region.end)
    .min_by_key(|region| region.len())
    .cloned()
}

/// Whether `outer` contains `inner` (or is the same region).
fn contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
  outer.start <= inner.start && inner.end <= outer.end
}

#[cfg(test)]
// Line ranges are naturally single-element arrays here.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
  use super::*;
  use crate::ops::{EditorOp, Motion, VerticalDirection};
  use crate::testing::{doc, render, run};

  const RUST: &str =
    "fn a() {\n    one();\n    two();\n}\nfn b() {\n    if x {\n        three();\n    }\n}\n";

  /// The regions of `RUST`, as its fold query would find them.
  fn regions() -> Vec<Range<usize>> {
    vec![0..3, 4..8, 5..7]
  }

  fn folded(text: &str, regions: &[Range<usize>]) -> Document {
    let mut document = doc(text);
    document.set_folded(regions.iter().cloned());
    document
  }

  #[test]
  fn rows_skip_hidden_lines() {
    let document = folded(RUST, &[0..3, 5..7]);
    assert_eq!(document.row_count(), 10 - 3);
    assert_eq!(document.visible_lines(0..10), [0..1, 3..6, 7..10]);
    assert_eq!(document.row_of(3), 1);
    assert_eq!(
      document.row_of(2),
      0,
      "hidden lines are on their first line's row"
    );
    assert_eq!(document.line_at_row(1), 3);
    assert_eq!(document.line_at_row(4), 7);
    assert_eq!(document.line_at_row(100), 9, "clamped");
    assert_eq!(document.folded_lines(0..10), [0, 5]);
  }

  #[test]
  fn vertical_movement_steps_over_folds() {
    let mut document = folded(&format!("|{RUST}"), &[0..3]);
    assert_eq!(
      run(
        &mut document,
        [EditorOp::Move {
          motion: Motion::Down,
          extend: false
        }]
      ),
      "fn a() {\n    one();\n    two();\n|}\nfn b() {\n    if x {\n        three();\n    }\n}\n"
    );
    run(
      &mut document,
      [EditorOp::Move {
        motion: Motion::Up,
        extend: false,
      }],
    );
    assert_eq!(document.selections.primary().head, 0);
    assert_eq!(document.folded(), [0..3], "still folded");
  }

  #[test]
  fn horizontal_movement_steps_over_folds() {
    let mut document = folded("fn a() {|\n    one();\n}\n", &[0..2]);
    let right = EditorOp::Move {
      motion: Motion::Right,
      extend: false,
    };
    assert_eq!(run(&mut document, [right]), "fn a() {\n    one();\n|}\n");
    let left = EditorOp::Move {
      motion: Motion::Left,
      extend: false,
    };
    assert_eq!(run(&mut document, [left]), "fn a() {|\n    one();\n}\n");
    assert_eq!(document.folded(), [0..2]);
  }

  #[test]
  fn selections_reaching_into_a_fold_unfold_it() {
    let mut document = folded(&format!("|{RUST}"), &[0..3, 4..8]);
    let find = EditorOp::FindNext {
      query: flareite_text::SearchQuery {
        pattern: "three".into(),
        regex: false,
        case_sensitive: true,
        whole_word: false,
      },
      backward: false,
    };
    document.apply(find).expect("applies");
    assert_eq!(document.folded(), [0..3]);
  }

  #[test]
  fn folding_moves_cursors_out_of_the_region() {
    let mut document = doc("fn a() {\n    on|e();\n}\n");
    document.apply(EditorOp::Fold(vec![0..2])).expect("applies");
    assert_eq!(render(&document), "fn a() {|\n    one();\n}\n");
    assert_eq!(document.folded(), [0..2]);
  }

  #[test]
  fn fold_picks_the_innermost_unfolded_region() {
    let mut document = doc(&RUST.replace("three", "|three"));
    document.apply(EditorOp::Fold(regions())).expect("applies");
    assert_eq!(document.folded(), [5..7]);
    document.apply(EditorOp::Fold(regions())).expect("applies");
    assert_eq!(document.folded(), [4..8, 5..7], "then the one around it");
    document.apply(EditorOp::Unfold).expect("applies");
    assert_eq!(document.folded(), [5..7], "unfolds where the cursor is");
  }

  #[test]
  fn toggling_folds() {
    let mut document = doc(&format!("|{RUST}"));
    document
      .apply(EditorOp::ToggleFold(regions()))
      .expect("applies");
    assert_eq!(document.folded(), [0..3]);
    document
      .apply(EditorOp::ToggleFold(regions()))
      .expect("applies");
    assert!(document.folded().is_empty());
    document
      .apply(EditorOp::ToggleFoldAt {
        line: 4,
        regions: regions(),
      })
      .expect("applies");
    assert_eq!(document.folded(), [4..8]);
    document
      .apply(EditorOp::ToggleFoldAt {
        line: 4,
        regions: regions(),
      })
      .expect("applies");
    assert!(document.folded().is_empty());
  }

  #[test]
  fn folding_all_recursively_and_by_level() {
    let mut document = doc(&format!("|{RUST}"));
    document
      .apply(EditorOp::FoldAll(regions()))
      .expect("applies");
    assert_eq!(document.folded(), regions());
    assert_eq!(document.row_count(), 5);
    document
      .apply(EditorOp::UnfoldRegions(vec![4..8, 5..7]))
      .expect("applies");
    assert_eq!(document.folded(), [0..3], "only those regions");
    document.apply(EditorOp::UnfoldAll).expect("applies");
    assert!(document.folded().is_empty());

    document
      .apply(EditorOp::FoldLevel {
        level: 2,
        regions: regions(),
      })
      .expect("applies");
    assert_eq!(document.folded(), [5..7]);
    document
      .apply(EditorOp::FoldLevel {
        level: 1,
        regions: regions(),
      })
      .expect("applies");
    assert_eq!(
      document.folded(),
      [4..8, 5..7],
      "not the region around the cursor"
    );

    let mut document = doc(&RUST.replace("fn b", "|fn b"));
    document
      .apply(EditorOp::FoldRecursively(regions()))
      .expect("applies");
    assert_eq!(document.folded(), [4..8, 5..7]);
    document
      .apply(EditorOp::UnfoldRecursively(regions()))
      .expect("applies");
    assert!(document.folded().is_empty());
  }

  #[test]
  fn folds_follow_edits() {
    let mut document = folded(&format!("|{RUST}"), &[4..8]);
    run(&mut document, [EditorOp::Insert("// top\n".into())]);
    assert_eq!(document.folded(), [5..9], "moved down a line");
    // Typing at the end of the first line keeps it folded.
    document.selections = crate::selection::Selections::cursor(document.buffer.line_end(5));
    run(&mut document, [EditorOp::Insert(" // b".into())]);
    assert_eq!(document.folded(), [5..9]);
    // Joining the first line with the next unfolds.
    run(
      &mut document,
      [EditorOp::Delete {
        direction: crate::ops::Direction::Forward,
        unit: crate::ops::Unit::Grapheme,
      }],
    );
    assert!(document.folded().is_empty());
  }

  #[test]
  fn line_operations_take_folded_regions_whole() {
    let text = "x\nfn a() {|\n    one();\n}\ny\n";
    let mut document = folded(text, &[1..3]);
    run(&mut document, [EditorOp::MoveLines(VerticalDirection::Up)]);
    assert_eq!(render(&document), "fn a() {|\n    one();\nx\n}\ny\n");
    assert_eq!(document.folded(), [0..2], "the fold moves with its lines");

    let mut document = folded(text, &[1..3]);
    run(
      &mut document,
      [EditorOp::MoveLines(VerticalDirection::Down)],
    );
    assert_eq!(render(&document), "x\n}\nfn a() {|\n    one();\ny\n");
    assert_eq!(document.folded(), [2..4]);

    let mut document = folded(text, &[1..3]);
    run(&mut document, [EditorOp::DeleteLines]);
    assert_eq!(render(&document), "x\n}|\ny\n");
    assert!(document.folded().is_empty());

    // Moving past a folded region moves over all of it.
    let mut document = folded("fn a() {\n    one();\n}\n|y\n", &[0..2]);
    run(&mut document, [EditorOp::MoveLines(VerticalDirection::Up)]);
    assert_eq!(render(&document), "fn a() {\n    one();\n|y\n}\n");
    assert_eq!(document.folded(), [0..2]);
  }

  #[test]
  fn moving_over_a_folded_neighbour_keeps_it_folded() {
    let mut document = folded("|x\nfn a() {\n    one();\n}\ny\n", &[1..3]);
    run(
      &mut document,
      [EditorOp::MoveLines(VerticalDirection::Down)],
    );
    assert_eq!(render(&document), "fn a() {\n    one();\n|x\n}\ny\n");
    assert_eq!(document.folded(), [0..2], "it moved up as one line");
  }

  #[test]
  fn indentation_regions() {
    let python = doc(
      "class A:\n    def f(self):\n        return 1\n\n    def g(self):\n        pass\nx = 1\n",
    );
    assert_eq!(python.indentation_folds(None), [0..6, 1..3, 4..6]);
    assert_eq!(python.indentation_folds(Some(2)), [1..3, 0..6]);
    assert_eq!(python.indentation_folds(Some(3)), [0..6], "a blank line");
    assert_eq!(python.indentation_fold_starts(0..7), [0, 1, 4]);
    let braces = doc("fn a() {\n    one();\n}\n");
    assert_eq!(
      braces.indentation_folds(None),
      [0..2],
      "the closing line stays visible"
    );
  }
}
