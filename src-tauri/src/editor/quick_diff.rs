//! Quick diffs: open files compared with their originals as they're edited
//! (the extension API's `quick-diff` interface, API 1.9), for change markers
//! beside the text, a change's original lines on request, and reverting or
//! applying (staging) changes.
//!
//! An extension names a file's original: an address it provides, read like
//! a provided document's but kept here as text. Changes are computed in the
//! background for each document version; a view carries those on its lines,
//! and the frontend is told to fetch views again when newer ones are ready.
//! Diff tabs revert and apply between their own two documents the same way.

use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use flareite_core::{Document, DocumentId, ViewId};
use flareite_diff::{DiffOptions, LineChange, Point};
use flareite_syntax::{deadline, parse};
use flareite_text::{Edit, TextBuffer, Version};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use super::EditorState;
use super::peek::{PeekLinesDto, lines_of};
use super::protocol::{GutterChangeDto, GutterChangeKind, Position, RangeDto};
use crate::error::CommandError;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::languages::LanguagesState;

/// Longest an original may take to parse for a change's lines; past it,
/// they come without highlighting.
const PARSE_BUDGET: Duration = Duration::from_millis(300);

/// A document's original and its changes from it.
struct Entry {
  uri: String,
  original: Arc<String>,
  /// The changes, and the document version they're for.
  changes: Option<(Version, Arc<Vec<LineChange>>)>,
  computing: bool,
}

#[derive(Default)]
pub struct QuickDiffs(Mutex<HashMap<DocumentId, Entry>>);

impl QuickDiffs {
  fn lock(&self) -> MutexGuard<'_, HashMap<DocumentId, Entry>> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Forgets a closed document.
  pub fn forget(&self, id: DocumentId) {
    self.lock().remove(&id);
  }
}

fn changed(app: &AppHandle, id: DocumentId) {
  events::emit(
    app,
    AppEvent::QuickDiffChanged {
      document_id: id.get(),
    },
  );
}

/// Asks extensions for the original of document `id`'s file (at `path`),
/// and reads it, in the background.
pub fn track(app: &AppHandle, id: DocumentId, path: PathBuf) {
  let app = app.clone();
  tauri::async_runtime::spawn_blocking(move || {
    let extensions = app.state::<ExtensionsState>();
    let Some(host) = extensions.host() else {
      return;
    };
    let path = path.display().to_string();
    let state = app.state::<EditorState>();
    let found = host.original_of(&path).and_then(|uri| {
      // The same original: its text stays (changes to it come through
      // `refresh`).
      let known = state
        .quick
        .lock()
        .get(&id)
        .and_then(|entry| (entry.uri == uri).then(|| (*entry.original).clone()));
      if let Some(text) = known {
        return Some((uri, text));
      }
      match host.provide_text(&uri) {
        // Nothing there: the file is new to the original.
        Ok(text) => Some((uri, text.unwrap_or_default())),
        Err(message) => {
          tracing::warn!(%uri, %message, "reading a file's original failed");
          None
        }
      }
    });
    if state.lock().document(id).is_err() {
      return;
    }
    let mut entries = state.quick.lock();
    let unchanged = match (&found, entries.get(&id)) {
      (Some((uri, text)), Some(entry)) => entry.uri == *uri && *entry.original == *text,
      (None, None) => true,
      _ => false,
    };
    if unchanged {
      return;
    }
    match found {
      Some((uri, text)) => {
        entries.insert(
          id,
          Entry {
            uri,
            original: Arc::new(text),
            changes: None,
            computing: false,
          },
        );
      }
      None => {
        entries.remove(&id);
      }
    }
    drop(entries);
    changed(&app, id);
  });
}

/// Looks for the originals of open files again: those at `paths`, or all
/// when it's empty.
pub fn track_open(app: &AppHandle, paths: &[String]) {
  let open: Vec<(DocumentId, PathBuf)> = app
    .state::<EditorState>()
    .lock()
    .documents()
    .filter_map(|document| {
      let path = document.path()?;
      let wanted = paths.is_empty()
        || paths
          .iter()
          .any(|wanted| crate::paths::normalize(std::path::Path::new(wanted)) == path);
      wanted.then(|| (document.id(), path.to_path_buf()))
    })
    .collect();
  for (id, path) in open {
    track(app, id, path);
  }
}

/// Reads originals at addresses among `patterns` again (see
/// `provided::refresh_provided`). Called on a background thread.
pub fn refresh(app: &AppHandle, matches: impl Fn(&str) -> bool) {
  let state = app.state::<EditorState>();
  let stale: Vec<(DocumentId, String)> = state
    .quick
    .lock()
    .iter()
    .filter(|(_, entry)| matches(&entry.uri))
    .map(|(id, entry)| (*id, entry.uri.clone()))
    .collect();
  let extensions = app.state::<ExtensionsState>();
  let Some(host) = extensions.host() else {
    return;
  };
  for (id, uri) in stale {
    let Ok(text) = host.provide_text(&uri) else {
      continue;
    };
    let text = text.unwrap_or_default();
    let mut entries = state.quick.lock();
    let Some(entry) = entries.get_mut(&id).filter(|entry| entry.uri == uri) else {
      continue;
    };
    if *entry.original != text {
      entry.original = Arc::new(text);
      entry.changes = None;
      drop(entries);
      changed(app, id);
    }
  }
}

/// The changes the document has from its original, if it has one: as of
/// its current version when they're ready, else the last ones, while newer
/// ones are computed (the frontend is told when they're ready).
fn changes_of(app: &AppHandle, document: &Document) -> Option<Arc<Vec<LineChange>>> {
  let state = app.state::<EditorState>();
  let mut entries = state.quick.lock();
  let entry = entries.get_mut(&document.id())?;
  let version = document.version();
  match &entry.changes {
    Some((computed, changes)) if *computed == version => return Some(changes.clone()),
    _ => {}
  }
  if !entry.computing {
    entry.computing = true;
    let original = entry.original.clone();
    let text = document.buffer().contents();
    let id = document.id();
    let uri = entry.uri.clone();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
      let changes = compute(&original, &text);
      let state = app.state::<EditorState>();
      let mut entries = state.quick.lock();
      if let Some(entry) = entries.get_mut(&id).filter(|entry| entry.uri == uri) {
        entry.computing = false;
        if Arc::ptr_eq(&entry.original, &original) {
          entry.changes = Some((version, Arc::new(changes)));
        }
        drop(entries);
        changed(&app, id);
      }
    });
  }
  entry.changes.as_ref().map(|(_, changes)| changes.clone())
}

fn compute(original: &str, text: &str) -> Vec<LineChange> {
  // Whitespace counts: these changes are reverted and staged.
  flareite_diff::diff(original, text, DiffOptions::default()).unwrap_or_default()
}

fn kind(change: &LineChange) -> GutterChangeKind {
  if change.original.is_empty() {
    GutterChangeKind::Added
  } else if change.modified.is_empty() {
    GutterChangeKind::Removed
  } else {
    GutterChangeKind::Modified
  }
}

/// The document's changes touching `lines`, for a view.
pub fn view_changes(
  app: &AppHandle,
  document: &Document,
  lines: Range<usize>,
) -> Vec<GutterChangeDto> {
  let Some(changes) = changes_of(app, document) else {
    return Vec::new();
  };
  let line_count = document.buffer().line_count();
  changes
    .iter()
    .filter(|change| change.modified.start <= lines.end && change.modified.end >= lines.start)
    .map(|change| GutterChangeDto {
      // A removal past the last line shows on the last line.
      start: change.modified.start.min(line_count),
      end: change.modified.end.min(line_count),
      kind: kind(change),
    })
    .collect()
}

/// The document's original and changes as of its current version,
/// computed now if they aren't ready.
fn current(
  state: &EditorState,
  document: &Document,
) -> Option<(String, Arc<String>, Arc<Vec<LineChange>>)> {
  let mut entries = state.quick.lock();
  let entry = entries.get_mut(&document.id())?;
  let version = document.version();
  let changes = match &entry.changes {
    Some((computed, changes)) if *computed == version => changes.clone(),
    _ => {
      let changes = Arc::new(compute(&entry.original, &document.buffer().contents()));
      entry.changes = Some((version, changes.clone()));
      changes
    }
  };
  Some((entry.uri.clone(), entry.original.clone(), changes))
}

/// The lines the selections of the document's view in use cover, as
/// inclusive ranges.
fn selected_lines(document: &Document) -> Vec<RangeInclusive<usize>> {
  let buffer = document.buffer();
  document
    .selections()
    .iter()
    .map(|selection| {
      let range = selection.range();
      let start = buffer.line_of(range.start);
      let mut end = buffer.line_of(range.end);
      // A selection ending at the start of a line doesn't take it.
      if end > start && buffer.line_start(end) == range.end {
        end -= 1;
      }
      start..=end
    })
    .collect()
}

/// The lines asked for (`[first, last]`, inclusive), or else those selected.
fn lines_or_selected(
  document: &Document,
  lines: Option<Vec<[usize; 2]>>,
) -> Vec<RangeInclusive<usize>> {
  match lines {
    Some(lines) => lines
      .into_iter()
      .map(|[first, last]| first..=last)
      .collect(),
    None => selected_lines(document),
  }
}

/// A change and its original lines, for the inline view of it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HunkDto {
  /// Its place among the changes (from 0), and how many there are.
  pub index: usize,
  pub total: usize,
  pub original: [usize; 2],
  pub modified: [usize; 2],
  /// The original lines, highlighted as the document's language.
  pub lines: PeekLinesDto,
  pub chars: Vec<CharChangeDto>,
  /// What applying it to the original is called, if the extension can.
  pub apply_title: Option<String>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HunkDirection {
  /// The change at the line.
  At,
  /// The first change after the line (or the first of all).
  Next,
  /// The last change before the line (or the last of all).
  Previous,
}

/// A change of document `id` from its original, to show inline.
#[tauri::command]
pub fn quick_diff_hunk(
  app: AppHandle,
  state: State<'_, EditorState>,
  id: u64,
  line: usize,
  direction: HunkDirection,
) -> Result<Option<HunkDto>, CommandError> {
  let (uri, original, changes, language) = {
    let editor = state.lock();
    let document = editor.document(DocumentId::from_raw(id))?;
    let Some((uri, original, changes)) = current(&state, document) else {
      return Ok(None);
    };
    let language = app.state::<LanguagesState>().for_document(document);
    (uri, original, changes, language)
  };
  let index = match direction {
    HunkDirection::At => changes.iter().position(|change| {
      let Range { start, end } = change.modified;
      if start == end {
        line + 1 == start || line == start
      } else {
        start <= line && line < end
      }
    }),
    HunkDirection::Next => changes
      .iter()
      .position(|change| change.modified.start > line)
      .or((!changes.is_empty()).then_some(0)),
    HunkDirection::Previous => changes
      .iter()
      .rposition(|change| change.modified.start < line)
      .or(changes.len().checked_sub(1)),
  };
  let Some(index) = index else {
    return Ok(None);
  };
  let change = &changes[index];
  let buffer = TextBuffer::from_text(&original);
  let tree = language.and_then(|language| {
    let registry = app.state::<LanguagesState>().registry();
    parse(&language, &registry, &buffer, None, deadline(PARSE_BUDGET))
      .ok()
      .flatten()
  });
  let apply_title = app
    .state::<ExtensionsState>()
    .host()
    .and_then(|host| host.apply_title(&uri));
  Ok(Some(HunkDto {
    index,
    total: changes.len(),
    original: [change.original.start, change.original.end],
    modified: [change.modified.start, change.modified.end],
    lines: lines_of(
      &buffer,
      tree.as_ref(),
      change.original.start,
      change.original.end,
    ),
    chars: change.chars.iter().map(CharChangeDto::of).collect(),
    apply_title,
  }))
}

/// Reverts `chosen` in `document` to `original`'s lines, as one undoable
/// edit. Returns whether anything changed.
fn revert_in(
  app: &AppHandle,
  document: &mut Document,
  original: &str,
  changes: &[LineChange],
  lines: &[RangeInclusive<usize>],
) -> Result<bool, CommandError> {
  let chosen = flareite_diff::touching(changes, lines);
  if chosen.is_empty() {
    return Ok(false);
  }
  let text = document.buffer().contents();
  let edits = flareite_diff::revert(original, &text, &chosen)
    .into_iter()
    .map(|splice| Edit::new(splice.range, splice.text))
    .collect();
  let edited = document.apply_edits(edits)?;
  if edited {
    app.state::<ExtensionsState>().sync(document);
    events::emit(
      app,
      AppEvent::DocumentEdited {
        document_id: document.id().get(),
      },
    );
  }
  Ok(edited)
}

/// Reverts the changes from its original that the given lines (or the
/// selections of view `view`) touch, in document `id`.
#[tauri::command]
pub fn quick_diff_revert(
  app: AppHandle,
  state: State<'_, EditorState>,
  id: u64,
  view: Option<u32>,
  lines: Option<Vec<[usize; 2]>>,
) -> Result<bool, CommandError> {
  let mut editor = state.lock();
  let document = editor.document_mut(DocumentId::from_raw(id))?;
  document.use_view(ViewId::from_raw(view.unwrap_or(0)));
  let Some((_, original, changes)) = current(&state, document) else {
    return Ok(false);
  };
  let lines = lines_or_selected(document, lines);
  revert_in(&app, document, &original, &changes, &lines)
}

/// Applies the changes from its original that the given lines (or the
/// selections of view `view`) touch to the original, by its extension
/// (staging them, for Git).
#[tauri::command]
pub async fn quick_diff_apply(
  app: AppHandle,
  id: u64,
  view: Option<u32>,
  lines: Option<Vec<[usize; 2]>>,
) -> Result<bool, CommandError> {
  let job = {
    let state = app.state::<EditorState>();
    let mut editor = state.lock();
    let document = editor.document_mut(DocumentId::from_raw(id))?;
    document.use_view(ViewId::from_raw(view.unwrap_or(0)));
    let Some((uri, original, changes)) = current(&state, document) else {
      return Ok(false);
    };
    let lines = lines_or_selected(document, lines);
    let chosen = flareite_diff::touching(&changes, &lines);
    if chosen.is_empty() {
      return Ok(false);
    }
    (
      uri,
      flareite_diff::apply(&original, &document.buffer().contents(), &chosen),
    )
  };
  apply_to(&app, job.0, job.1).await
}

async fn apply_to(app: &AppHandle, uri: String, text: String) -> Result<bool, CommandError> {
  let app = app.clone();
  tauri::async_runtime::spawn_blocking(move || {
    let extensions = app.state::<ExtensionsState>();
    let host = extensions
      .host()
      .ok_or_else(|| "Extensions aren't running.".to_owned())?;
    host.apply_to_original(&uri, text)
  })
  .await
  .map_err(CommandError::internal)?
  .map(|()| true)
  .map_err(|message| CommandError::new("notApplied", message))
}

/// Characters that changed within a change's lines.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CharChangeDto {
  pub original: RangeDto,
  pub modified: RangeDto,
}

impl CharChangeDto {
  pub fn of(chars: &flareite_diff::CharChange) -> Self {
    let range = |range: Range<Point>| {
      let position = |point: Point| Position {
        line: point.line,
        column: point.column,
      };
      RangeDto {
        start: position(range.start),
        end: position(range.end),
      }
    };
    Self {
      original: range(chars.original.clone()),
      modified: range(chars.modified.clone()),
    }
  }
}

#[cfg(test)]
mod tests {
  use flareite_core::{Editor, EditorOp};

  use super::*;

  #[test]
  fn selections_cover_whole_lines() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("one\ntwo\nthree\n".into()))
      .expect("inserts");
    let cursor = |offset, extend, add| EditorOp::SetCursor {
      offset,
      extend,
      add,
    };
    // `two` and the start of `three`, then a cursor on `one`.
    document.apply(cursor(5, false, false)).expect("moves");
    document.apply(cursor(9, true, false)).expect("selects");
    document.apply(cursor(1, false, true)).expect("adds");
    let mut lines = selected_lines(document);
    lines.sort_by_key(|range| *range.start());
    assert_eq!(lines, vec![0..=0, 1..=2]);

    // A selection ending at the start of a line doesn't take that line.
    document.apply(cursor(4, false, false)).expect("moves");
    document.apply(cursor(8, true, false)).expect("selects");
    assert_eq!(selected_lines(document), vec![1..=1]);
  }
}
