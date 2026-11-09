//! Remembering which regions of a file are folded, across closing and
//! reopening it and across restarts. The regions are kept in the UI state
//! with the file's line count, recorded after fold operations, on save, and
//! on close.
//!
//! A reopened file gets back the remembered regions that are still foldable
//! regions, and only if its line count is unchanged; the rest are dropped.
//! That waits for the first view after the regions are known, which for a
//! language with a fold query is once its syntax tree has been parsed.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use flareite_core::{Document, DocumentId};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use super::EditorState;
use super::folding::{self, Scope};
use super::is_pane_view;
use crate::ui_state::UiStateStore;

/// Files whose folds are remembered; the least recently recorded go first.
const MAX_FILES: usize = 100;
/// Most regions remembered for a file.
const MAX_REGIONS: usize = 5_000;

/// A file's folded regions, as kept in the UI state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFolds {
  pub path: PathBuf,
  /// The file's line count when the regions were recorded.
  pub line_count: usize,
  /// Line ranges `[start, end)`.
  pub regions: Vec<[usize; 2]>,
}

/// Remembered regions of opened documents, waiting to be restored.
#[derive(Default)]
pub struct PendingFolds(Mutex<HashMap<DocumentId, Vec<Range<usize>>>>);

impl PendingFolds {
  fn lock(&self) -> MutexGuard<'_, HashMap<DocumentId, Vec<Range<usize>>>> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Looks up the regions remembered for a document just opened from its
/// file, to restore when it is shown.
pub fn recall(app: &AppHandle, document: &Document) {
  let Some(path) = document.path() else {
    return;
  };
  let line_count = document.buffer().line_count();
  let remembered = app
    .state::<UiStateStore>()
    .get()
    .folds
    .into_iter()
    .find(|file| file.path == path && file.line_count == line_count);
  if let Some(file) = remembered {
    let regions = file
      .regions
      .iter()
      .map(|&[start, end]| start..end)
      .collect();
    app
      .state::<EditorState>()
      .pending_folds
      .lock()
      .insert(document.id(), regions);
  }
}

/// Folds the remembered regions of `document` that are still foldable,
/// once its foldable regions are known. Called with the editor locked.
pub fn restore(app: &AppHandle, document: &mut Document) {
  let state = app.state::<EditorState>();
  if is_pane_view(document.view())
    || !state.pending_folds.lock().contains_key(&document.id())
    || !state.syntax.folds_ready(app, document)
  {
    return;
  }
  let Some(remembered) = state.pending_folds.lock().remove(&document.id()) else {
    return;
  };
  let foldable: HashSet<(usize, usize)> = folding::regions(app, document, Scope::All)
    .into_iter()
    .map(|region| (region.start, region.end))
    .collect();
  document.set_folded(
    remembered
      .into_iter()
      .filter(|region| foldable.contains(&(region.start, region.end))),
  );
}

/// Records which regions of `document` are folded. Called with the editor
/// locked.
pub fn remember(app: &AppHandle, document: &Document) {
  let Some(path) = document.path() else {
    return;
  };
  // A diff's folds hide its unchanged lines, not regions the user folded.
  if is_pane_view(document.view()) {
    return;
  }
  // Not restored yet: what is remembered still stands.
  if app
    .state::<EditorState>()
    .pending_folds
    .lock()
    .contains_key(&document.id())
  {
    return;
  }
  let file = FileFolds {
    path: path.to_path_buf(),
    line_count: document.buffer().line_count(),
    regions: document
      .folded()
      .into_iter()
      .take(MAX_REGIONS)
      .map(|region| [region.start, region.end])
      .collect(),
  };
  app.state::<UiStateStore>().update(|state| {
    state
      .folds
      .retain(|remembered| remembered.path != file.path);
    if !file.regions.is_empty() {
      state.folds.insert(0, file);
      state.folds.truncate(MAX_FILES);
    }
  });
}

/// Drops what is waiting to be restored for a closed document.
pub fn forget(app: &AppHandle, id: DocumentId) {
  app.state::<EditorState>().pending_folds.lock().remove(&id);
}
