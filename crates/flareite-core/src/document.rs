use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};

use flareite_text::{
  Bias, ChangeSet, Edit, LineEnding, TextBuffer, TextSnapshot, Version, map_offset_through,
  normalize_line_endings,
};
use flareite_workspace::{FileStamp, TextEncoding};

use crate::error::CoreError;
use crate::folds::Folds;
use crate::history::{EditKind, History, HistoryLimits, StateId};
use crate::pairs::AutoClosed;
use crate::selection::{Selection, Selections};
use crate::snippets::SnippetSession;
use crate::wrap::Wraps;

/// Stable identifier of an open document. Never reused within a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentId(pub(crate) u64);

impl DocumentId {
  pub fn get(self) -> u64 {
    self.0
  }

  pub fn from_raw(raw: u64) -> Self {
    Self(raw)
  }
}

impl fmt::Display for DocumentId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "#{}", self.0)
  }
}

/// One of a document's views. Each editor showing the document has its own
/// selections, cursor history, and folded regions; the text, undo history,
/// and everything else are shared. A document starts with view 0, and the
/// caller numbers any others.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViewId(u32);

impl ViewId {
  pub fn get(self) -> u32 {
    self.0
  }

  pub fn from_raw(raw: u32) -> Self {
    Self(raw)
  }
}

/// A view's own state, while another view is in use.
#[derive(Debug, Clone)]
struct ViewState {
  selections: Selections,
  /// The text version `selections` refer to.
  version: Version,
  cursor_history: VecDeque<(Version, Selections)>,
  folds: Folds,
  wraps: Wraps,
}

impl ViewState {
  /// Follows the edits made in other views since the state was last brought
  /// up to date. A cursor where text was inserted stays before it.
  fn catch_up(&mut self, buffer: &TextBuffer, tab_size: usize) {
    self.folds.sync(buffer);
    self.wraps.sync(buffer, tab_size);
    self.wraps.place(&self.folds);
    let since = std::mem::replace(&mut self.version, buffer.version());
    if since == self.version {
      return;
    }
    self.selections = match buffer.edits_since(since) {
      Some(edits) => self.selections.map(|s| {
        Selection::new(
          map_offset_through(&edits, s.anchor, false),
          map_offset_through(&edits, s.head, false),
        )
      }),
      // Kept up to date after every change, so never this far behind.
      None => Selections::cursor(0),
    };
  }
}

/// How far into a line visual columns are computed exactly (grapheme by
/// grapheme, with tab stops). Past this they count characters.
pub const EXACT_COLUMN_BYTES: usize = 4096;

/// Most earlier selections kept for cursor undo.
const MAX_CURSOR_HISTORY: usize = 100;

#[derive(Debug, Clone)]
pub struct EditorConfig {
  pub tab_size: usize,
  /// Whether Tab inserts spaces instead of a tab character.
  pub insert_spaces: bool,
  /// Line ending for documents that do not contain one yet.
  pub default_line_ending: LineEnding,
  pub history: HistoryLimits,
}

impl Default for EditorConfig {
  fn default() -> Self {
    Self {
      tab_size: 4,
      insert_spaces: true,
      default_line_ending: LineEnding::Lf,
      history: HistoryLimits::default(),
    }
  }
}

#[derive(Debug, Clone)]
struct FileBacking {
  path: PathBuf,
  encoding: TextEncoding,
  /// Disk state when last loaded or saved; `None` if unknown.
  stamp: Option<FileStamp>,
}

/// What to write when saving, captured at one moment. Editing may continue
/// while the snapshot is written; pass the save point back to
/// [`Document::mark_saved`] afterwards.
#[derive(Debug, Clone)]
pub struct SavePoint {
  pub snapshot: TextSnapshot,
  /// The encoding to write it in: the document's, unless changed to save
  /// it with another one.
  pub encoding: TextEncoding,
  /// Disk stamp the file is expected to still have, if known.
  pub expected_stamp: Option<FileStamp>,
  state: StateId,
}

/// A language chosen for a document, instead of the one its file name
/// suggests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageChoice {
  /// No language.
  PlainText,
  /// The language with this id, e.g. `css`.
  Language(String),
}

#[derive(Debug)]
pub struct Document {
  id: DocumentId,
  pub(crate) buffer: TextBuffer,
  /// The selections of the view in use, as are `cursor_history` and `folds`.
  pub(crate) selections: Selections,
  pub(crate) history: History,
  pub(crate) line_ending: LineEnding,
  pub(crate) tab_size: usize,
  pub(crate) insert_spaces: bool,
  file: Option<FileBacking>,
  untitled_number: u32,
  saved_state: StateId,
  /// Earlier selections, newest last, with the version they belong to.
  cursor_history: VecDeque<(Version, Selections)>,
  /// Folded regions.
  pub(crate) folds: Folds,
  /// Wrapped lines.
  pub(crate) wraps: Wraps,
  /// The closers typed pairs added, which typing steps over.
  pub(crate) auto_closed: AutoClosed,
  /// The snippet being filled in, whose tab stops Tab moves between.
  pub(crate) snippet: Option<SnippetSession>,
  /// The view in use: operations and viewport updates are its.
  view: ViewId,
  /// The other views' state.
  parked: BTreeMap<ViewId, ViewState>,
  /// The language chosen for the document, if any.
  language: Option<LanguageChoice>,
  /// For a document an extension provides (such as a file as it was in a
  /// commit), its address, e.g. `git:HEAD:src/main.rs`. Such documents are
  /// read-only and never dirty.
  pub(crate) provided: Option<String>,
}

impl Document {
  pub(crate) fn new(
    id: DocumentId,
    text: &str,
    file: Option<(PathBuf, TextEncoding, FileStamp)>,
    untitled_number: u32,
    config: &EditorConfig,
  ) -> Self {
    let buffer = TextBuffer::from_text(text);
    let line_ending = buffer
      .detect_line_ending()
      .unwrap_or(config.default_line_ending);
    let history = History::new(config.history);
    let saved_state = history.current_state();
    let folds = Folds::new(buffer.version());
    let wraps = Wraps::new(buffer.version());
    let auto_closed = AutoClosed::new(buffer.version());
    Self {
      id,
      buffer,
      selections: Selections::cursor(0),
      history,
      line_ending,
      tab_size: config.tab_size.max(1),
      insert_spaces: config.insert_spaces,
      file: file.map(|(path, encoding, stamp)| FileBacking {
        path,
        encoding,
        stamp: Some(stamp),
      }),
      untitled_number,
      saved_state,
      cursor_history: VecDeque::new(),
      folds,
      wraps,
      auto_closed,
      snippet: None,
      view: ViewId::default(),
      parked: BTreeMap::new(),
      language: None,
      provided: None,
    }
  }

  /// The address of a document an extension provides; `None` for files
  /// and untitled documents.
  pub fn provided_uri(&self) -> Option<&str> {
    self.provided.as_deref()
  }

  /// Whether the text can't be edited: a provided document's.
  pub fn is_read_only(&self) -> bool {
    self.provided.is_some()
  }

  /// The file name a provided document's address ends with (what its
  /// language is found by), e.g. `main.rs` for `git:HEAD:src/main.rs`.
  pub fn provided_name(&self) -> Option<&str> {
    let uri = self.provided.as_deref()?;
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    path
      .rsplit(['/', '\\', ':'])
      .next()
      .filter(|name| !name.is_empty())
  }

  /// Replaces a provided document's text with what its extension provides
  /// now. It stays read-only and clean.
  pub fn set_provided_text(&mut self, text: &str) -> Result<bool, CoreError> {
    let changed = self.replace_text(text)?;
    self.saved_state = self.history.current_state();
    self.history.seal();
    Ok(changed)
  }

  pub fn id(&self) -> DocumentId {
    self.id
  }

  /// The view in use.
  pub fn view(&self) -> ViewId {
    self.view
  }

  /// Uses `view` from now on: operations move its selections and fold its
  /// regions, and viewport updates show its folds. A view not seen before
  /// starts as a copy of the one in use.
  pub fn use_view(&mut self, view: ViewId) {
    if view == self.view {
      return;
    }
    let state = self
      .parked
      .remove(&view)
      .unwrap_or_else(|| self.view_state());
    let previous = std::mem::replace(&mut self.view, view);
    let parked = self.install(state);
    self.parked.insert(previous, parked);
  }

  /// Makes view `to` a copy of view `from` as it is now, as for an editor
  /// split off another. Does nothing if `from` isn't a view.
  pub fn copy_view(&mut self, from: ViewId, to: ViewId) {
    let state = if from == self.view {
      self.view_state()
    } else if let Some(state) = self.parked.get(&from) {
      state.clone()
    } else {
      return;
    };
    if to == self.view {
      self.install(state);
    } else {
      self.parked.insert(to, state);
    }
  }

  /// Forgets a view, once no editor shows it. A document always keeps one,
  /// so the view in use goes only if another can take its place.
  pub fn close_view(&mut self, view: ViewId) {
    if view != self.view {
      self.parked.remove(&view);
    } else if let Some((next, state)) = self.parked.pop_first() {
      self.view = next;
      self.install(state);
    }
  }

  /// The views, in order.
  pub fn views(&self) -> Vec<ViewId> {
    let mut views: Vec<ViewId> = self.parked.keys().copied().collect();
    views.push(self.view);
    views.sort();
    views
  }

  /// A copy of the state of the view in use.
  fn view_state(&self) -> ViewState {
    ViewState {
      selections: self.selections.clone(),
      version: self.buffer.version(),
      cursor_history: self.cursor_history.clone(),
      folds: self.folds.clone(),
      wraps: self.wraps.clone(),
    }
  }

  /// Puts `state` in use, returning the state it replaces.
  fn install(&mut self, mut state: ViewState) -> ViewState {
    state.catch_up(&self.buffer, self.tab_size);
    // Added closers, and a snippet's stops, are the cursors' in the view
    // that was in use.
    self.auto_closed.clear(self.buffer.version());
    self.snippet = None;
    ViewState {
      selections: std::mem::replace(&mut self.selections, state.selections),
      version: self.buffer.version(),
      cursor_history: std::mem::replace(&mut self.cursor_history, state.cursor_history),
      folds: std::mem::replace(&mut self.folds, state.folds),
      wraps: std::mem::replace(&mut self.wraps, state.wraps),
    }
  }

  pub fn buffer(&self) -> &TextBuffer {
    &self.buffer
  }

  pub fn selections(&self) -> &Selections {
    &self.selections
  }

  pub fn version(&self) -> Version {
    self.buffer.version()
  }

  pub fn line_ending(&self) -> LineEnding {
    self.line_ending
  }

  pub fn path(&self) -> Option<&Path> {
    self.file.as_ref().map(|file| file.path.as_path())
  }

  /// How the text is stored in its file: the encoding it was read in or
  /// last saved in. Untitled documents are saved as UTF-8.
  pub fn encoding(&self) -> TextEncoding {
    self
      .file
      .as_ref()
      .map_or(TextEncoding::UTF8, |file| file.encoding)
  }

  /// The language chosen for the document; `None` to go by its file name.
  /// It stays when the file is renamed or saved under another name.
  pub fn language_choice(&self) -> Option<&LanguageChoice> {
    self.language.as_ref()
  }

  pub fn set_language_choice(&mut self, choice: Option<LanguageChoice>) {
    self.language = choice;
  }

  /// File name for file-backed and provided documents, `Untitled-N`
  /// otherwise.
  pub fn title(&self) -> String {
    if let Some(name) = self.provided_name() {
      return name.to_owned();
    }
    match self.path().and_then(Path::file_name) {
      Some(name) => name.to_string_lossy().into_owned(),
      None => format!("Untitled-{}", self.untitled_number),
    }
  }

  /// Whether the text differs from what was last loaded or saved. Undoing
  /// back to the saved text makes the document clean again.
  pub fn is_dirty(&self) -> bool {
    self.history.current_state() != self.saved_state
  }

  pub fn can_undo(&self) -> bool {
    self.history.can_undo()
  }

  pub fn can_redo(&self) -> bool {
    self.history.can_redo()
  }

  /// Captures the current text for saving.
  pub fn save_point(&self) -> SavePoint {
    SavePoint {
      snapshot: self.buffer.snapshot(),
      encoding: self.encoding(),
      expected_stamp: self.file.as_ref().and_then(|file| file.stamp.clone()),
      state: self.history.current_state(),
    }
  }

  /// Records that `point` was written to `path`, which may differ from the
  /// current path (Save As). Edits made since the save point stay dirty.
  pub fn mark_saved(&mut self, point: &SavePoint, path: PathBuf, stamp: FileStamp) {
    self.saved_state = point.state;
    self.file = Some(FileBacking {
      path,
      encoding: point.encoding,
      stamp: Some(stamp),
    });
    self.history.seal();
  }

  /// Records that the document's file was renamed or moved to `path`. The
  /// text and whether it's saved don't change.
  pub fn moved_to(&mut self, path: PathBuf) {
    if let Some(file) = &mut self.file {
      file.path = path;
    }
  }

  /// The disk stamp recorded when the file was last loaded or saved.
  pub fn disk_stamp(&self) -> Option<&FileStamp> {
    self.file.as_ref().and_then(|file| file.stamp.as_ref())
  }

  /// Replaces the text with a newer version read from disk and marks the
  /// document saved. Only the differing middle section is replaced, so
  /// cursors before and after it stay put, and the reload can be undone.
  /// Returns whether the text changed.
  pub fn reload(
    &mut self,
    text: &str,
    encoding: TextEncoding,
    stamp: FileStamp,
  ) -> Result<bool, CoreError> {
    let changed = self.replace_text(text)?;
    self.saved_state = self.history.current_state();
    if let Some(file) = &mut self.file {
      file.encoding = encoding;
      file.stamp = Some(stamp);
    }
    self.history.seal();
    Ok(changed)
  }

  /// Replaces the whole text with `text` (taken as is, line endings
  /// included) as one undoable change, e.g. to restore a backup. Only the
  /// differing middle section is replaced, so cursors before and after it
  /// stay put. Returns whether the text changed.
  pub fn replace_text(&mut self, text: &str) -> Result<bool, CoreError> {
    let current = self.buffer.contents();
    if current == text {
      return Ok(false);
    }
    let (prefix, suffix) = common_affixes(&current, text);
    let edit = Edit::new(
      prefix..current.len() - suffix,
      &text[prefix..text.len() - suffix],
    );
    let changes = ChangeSet::single(edit)?;
    let before = self.selections.clone();
    let inverse = self.buffer.apply(&changes)?;
    let after = before.map(|s| {
      Selection::new(
        changes.map_offset(s.anchor, Bias::Before),
        changes.map_offset(s.head, Bias::Before),
      )
    });
    self.selections = after.clone();
    self
      .history
      .record(changes, inverse, before, after, EditKind::Other, false);
    self.history.seal();
    self.sync_folds();
    Ok(true)
  }

  /// Applies edits made outside the editor (e.g. by an extension) as one
  /// undoable change. Line breaks in inserted text are converted to the
  /// document's line ending; selections move with the text. Returns whether
  /// the text changed.
  pub fn apply_edits(&mut self, edits: Vec<Edit>) -> Result<bool, CoreError> {
    if self.is_read_only() {
      return Err(CoreError::ReadOnly(self.id));
    }
    let edits = edits
      .into_iter()
      .map(|edit| {
        let text = normalize_line_endings(&edit.text, self.line_ending).into_owned();
        Edit::new(edit.range, text)
      })
      .collect();
    let changes = ChangeSet::new(edits)?;
    if changes.edits().iter().all(Edit::is_noop) {
      return Ok(false);
    }
    let before = self.selections.clone();
    let inverse = self.buffer.apply(&changes)?;
    let after = before.map(|s| {
      Selection::new(
        changes.map_offset(s.anchor, Bias::After),
        changes.map_offset(s.head, Bias::After),
      )
    });
    self.selections = after.clone();
    self
      .history
      .record(changes, inverse, before, after, EditKind::Other, false);
    self.history.seal();
    self.sync_folds();
    Ok(true)
  }

  /// Brings the folded regions up to date with the text, unfolding any a
  /// selection reaches into. The other views follow the text too.
  pub(crate) fn sync_folds(&mut self) {
    self.folds.sync(&self.buffer);
    self.reveal_selections();
    self.sync_wraps();
    for state in self.parked.values_mut() {
      state.catch_up(&self.buffer, self.tab_size);
    }
  }

  /// Brings the wrapped lines up to date with the text, the tab size, and
  /// the folds.
  pub(crate) fn sync_wraps(&mut self) {
    self.wraps.sync(&self.buffer, self.tab_size);
    self.wraps.place(&self.folds);
  }

  /// Records selections that were just replaced, for cursor undo.
  pub(crate) fn remember_selections(&mut self, previous: Selections) {
    if self.cursor_history.len() == MAX_CURSOR_HISTORY {
      self.cursor_history.pop_front();
    }
    self
      .cursor_history
      .push_back((self.buffer.version(), previous));
  }

  /// Restores the selections from before the last selection change. Only
  /// selections from the current text count: editing clears the history.
  pub(crate) fn cursor_undo(&mut self) {
    let version = self.buffer.version();
    while let Some((at, selections)) = self.cursor_history.pop_back() {
      if at != version {
        self.cursor_history.clear();
        return;
      }
      if selections != self.selections {
        self.history.seal();
        self.selections = selections;
        return;
      }
    }
  }

  pub(crate) fn set_indentation(&mut self, tab_size: usize, insert_spaces: bool) {
    self.tab_size = tab_size.max(1);
    self.insert_spaces = insert_spaces;
    self.sync_wraps();
  }

  /// The selected text of every non-empty selection, joined by the
  /// document's line ending, or `None` if nothing is selected.
  pub fn selected_text(&self) -> Option<String> {
    let parts: Vec<String> = self
      .selections
      .iter()
      .filter(|selection| !selection.is_empty())
      .map(|selection| self.buffer.text_in(selection.range()))
      .collect();
    (!parts.is_empty()).then(|| parts.join(self.line_ending.as_str()))
  }

  /// Columns between tab stops.
  pub fn tab_size(&self) -> usize {
    self.tab_size
  }

  /// One level of indentation, as Tab inserts it at the start of a line:
  /// `tab_size` spaces, or a tab.
  pub fn indent_unit(&self) -> String {
    if self.insert_spaces {
      " ".repeat(self.tab_size)
    } else {
      "\t".to_owned()
    }
  }

  /// The visual column of `offset`: grapheme clusters since the start of the
  /// line, with tabs advancing to the next tab stop.
  ///
  /// Beyond [`EXACT_COLUMN_BYTES`] into a line, columns are approximated by
  /// counting characters, so cursor work on very long lines stays O(log n).
  pub fn visual_column(&self, offset: usize) -> usize {
    let mut position = self.buffer.line_start(self.buffer.line_of(offset));
    let offset = offset.min(self.buffer.len());
    if offset - position > EXACT_COLUMN_BYTES {
      return self.buffer.char_count(position..offset);
    }
    let mut column = 0;
    while position < offset {
      column = self.advance_column(column, position);
      position = self.buffer.next_grapheme_boundary(position);
    }
    column
  }

  /// The offset in `line` closest to (not past) visual column `goal`.
  pub(crate) fn offset_at_visual_column(&self, line: usize, goal: usize) -> usize {
    let mut position = self.buffer.line_start(line);
    let end = self.buffer.line_end(line);
    if goal > EXACT_COLUMN_BYTES {
      return self.buffer.offset_after_chars(position, goal, end);
    }
    let mut column = 0;
    while position < end {
      let next_column = self.advance_column(column, position);
      if next_column > goal {
        break;
      }
      column = next_column;
      position = self.buffer.next_grapheme_boundary(position);
    }
    position
  }

  fn advance_column(&self, column: usize, position: usize) -> usize {
    if self.buffer.chars_at(position).next() == Some('\t') {
      column + self.tab_size - column % self.tab_size
    } else {
      column + 1
    }
  }
}

/// Lengths of the longest common prefix and suffix of `a` and `b`, on
/// character boundaries and not overlapping.
fn common_affixes(a: &str, b: &str) -> (usize, usize) {
  let mut prefix = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
  while !a.is_char_boundary(prefix) || !b.is_char_boundary(prefix) {
    prefix -= 1;
  }
  let max_suffix = a.len().min(b.len()) - prefix;
  let mut suffix = a
    .bytes()
    .rev()
    .zip(b.bytes().rev())
    .take(max_suffix)
    .take_while(|(x, y)| x == y)
    .count();
  while !a.is_char_boundary(a.len() - suffix) || !b.is_char_boundary(b.len() - suffix) {
    suffix -= 1;
  }
  (prefix, suffix)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn columns_on_very_long_lines_count_characters() {
    let long = format!("	{}", "x".repeat(EXACT_COLUMN_BYTES * 2));
    let document = Document::new(DocumentId(1), &long, None, 0, &EditorConfig::default());
    assert_eq!(
      document.visual_column(1),
      4,
      "short distances are exact (tab stop)"
    );
    let far = EXACT_COLUMN_BYTES + 100;
    assert_eq!(document.visual_column(far), far);
    assert_eq!(document.offset_at_visual_column(0, far), far);
  }

  #[test]
  fn common_affixes_respect_character_boundaries() {
    assert_eq!(common_affixes("abcXdef", "abcYYdef"), (3, 3));
    assert_eq!(common_affixes("same", "same"), (4, 0));
    // "é" (C3 A9) and "è" (C3 A8) share a first byte that must not be split.
    assert_eq!(common_affixes("aé", "aè"), (1, 0));
    assert_eq!(common_affixes("éa", "èa"), (0, 1));
    assert_eq!(common_affixes("aa", "aaa"), (2, 0));
  }

  #[test]
  fn reload_replaces_text_keeps_cursors_and_is_clean() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("f.txt");
    std::fs::write(&path, "x").expect("write");
    let stamp = FileStamp::read(&path).expect("stat").expect("exists");

    let mut document = Document::new(
      DocumentId(1),
      "head
old middle
tail",
      Some((path, TextEncoding::UTF8, stamp.clone())),
      0,
      &EditorConfig::default(),
    );
    document.selections = Selections::new(vec![Selection::cursor(2), Selection::cursor(19)], 0);
    assert!(
      document
        .reload(
          "head
new
tail",
          TextEncoding::UTF8,
          stamp.clone()
        )
        .expect("reloads")
    );
    assert_eq!(
      document.buffer().contents(),
      "head
new
tail"
    );
    let heads: Vec<_> = document.selections().iter().map(|s| s.head).collect();
    assert_eq!(
      heads,
      vec![2, 12],
      "cursors before and after the change stay on their text"
    );
    assert!(!document.is_dirty());

    document.apply(crate::ops::EditorOp::Undo).expect("undo");
    assert_eq!(
      document.buffer().contents(),
      "head
old middle
tail"
    );
    assert!(
      document.is_dirty(),
      "undoing a reload differs from the disk contents"
    );

    assert!(
      !document
        .reload(
          "head
old middle
tail",
          TextEncoding::UTF8,
          stamp
        )
        .expect("reloads")
    );
    assert!(!document.is_dirty());
  }

  #[test]
  // Fold regions are naturally single-element arrays here.
  #[allow(clippy::single_range_in_vec_init)]
  fn views_have_their_own_selections_and_folds_and_follow_each_others_edits() {
    use crate::ops::EditorOp;
    let config = EditorConfig::default();
    let text: String = (0..10).map(|i| format!("line {i}\n")).collect();
    let mut document = Document::new(DocumentId(1), &text, None, 0, &config);
    let cursor = |offset| EditorOp::SetCursor {
      offset,
      extend: false,
      add: false,
    };
    let head = |document: &Document| document.selections().primary().head;

    document.apply(cursor(14)).expect("applies");
    document.set_folded([3..6]);
    let second = ViewId::from_raw(1);
    document.use_view(second);
    assert_eq!(head(&document), 14, "a new view starts as a copy");
    assert_eq!(document.folded(), [3..6]);

    document.apply(EditorOp::UnfoldAll).expect("applies");
    document.apply(cursor(0)).expect("applies");
    document
      .apply(EditorOp::Insert("new\n".into()))
      .expect("applies");
    assert_eq!(head(&document), 4);

    document.use_view(ViewId::default());
    assert_eq!(
      head(&document),
      18,
      "the cursor moved with the text before it"
    );
    assert_eq!(document.folded(), [4..7], "and so did the folds");
    document.use_view(second);
    assert_eq!(head(&document), 4);
    assert!(document.folded().is_empty());

    let third = ViewId::from_raw(2);
    document.copy_view(ViewId::default(), third);
    document.use_view(third);
    assert_eq!(head(&document), 18);
    assert_eq!(document.views(), [ViewId::default(), second, third]);

    document.close_view(third);
    assert_ne!(document.view(), third, "another view takes its place");
    assert_eq!(document.views(), [ViewId::default(), second]);
    document.close_view(ViewId::default());
    document.close_view(second);
    assert_eq!(document.views().len(), 1, "a document keeps one view");
  }

  #[test]
  fn replacing_the_text_is_one_undoable_dirty_change() {
    let config = EditorConfig::default();
    let mut document = Document::new(DocumentId(1), "a\r\nsaved\r\n", None, 0, &config);
    assert!(document.replace_text("a\r\nrestored\n").expect("replaces"));
    assert_eq!(document.buffer().contents(), "a\r\nrestored\n");
    assert!(document.is_dirty());
    assert!(!document.replace_text("a\r\nrestored\n").expect("replaces"));

    document.apply(crate::ops::EditorOp::Undo).expect("undo");
    assert_eq!(document.buffer().contents(), "a\r\nsaved\r\n");
    assert!(!document.is_dirty());
  }

  #[test]
  fn external_edits_are_one_undo_step_with_document_line_endings() {
    let config = EditorConfig::default();
    let mut document = Document::new(DocumentId(1), "one\r\ntwo\r\n", None, 0, &config);
    document.selections = Selections::single(Selection::cursor(5));
    let edits = vec![Edit::new(0..3, "ONE\nAND"), Edit::insert(10, "three")];
    assert!(document.apply_edits(edits).expect("applies"));
    assert_eq!(document.buffer().contents(), "ONE\r\nAND\r\ntwo\r\nthree");
    assert_eq!(
      document.selections().primary().head,
      10,
      "the cursor stays on its text"
    );
    document.apply(crate::EditorOp::Undo).expect("undoes");
    assert_eq!(document.buffer().contents(), "one\r\ntwo\r\n");

    let overlapping = vec![Edit::new(0..2, "a"), Edit::new(1..3, "b")];
    assert!(document.apply_edits(overlapping).is_err());
    assert!(
      !document
        .apply_edits(vec![Edit::new(1..1, "")])
        .expect("no-op")
    );
  }
}
