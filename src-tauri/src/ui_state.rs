//! Window and session state restored on the next launch: the open folder,
//! open files, files' folded regions, and the frontend's layout. Stored as JSON in the app's local
//! data directory and written atomically whenever it changes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::editor::EditorState;
use crate::editor::fold_memory::FileFolds;
use crate::error::CommandError;

const FILE_NAME: &str = "state.json";
/// Largest layout object accepted from the frontend, in bytes of JSON.
const MAX_LAYOUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UiState {
  pub workspace: Option<PathBuf>,
  pub open_files: Vec<PathBuf>,
  pub active_file: Option<PathBuf>,
  /// Files' folded regions, most recently recorded first.
  pub folds: Vec<FileFolds>,
  /// Owned by the frontend (side bar width, active view, ...).
  pub layout: serde_json::Value,
  /// When Flareite last looked for an update of itself at startup, in
  /// seconds since 1970.
  pub last_update_check: Option<u64>,
  /// Why the update installed as Flareite last exited failed, to tell the
  /// user once.
  pub failed_update: Option<String>,
}

pub struct UiStateStore {
  path: Option<PathBuf>,
  state: Mutex<UiState>,
  /// When set (benchmark runs), changes are kept in memory only.
  ephemeral: AtomicBool,
}

impl UiStateStore {
  pub fn load(data_dir: Option<PathBuf>) -> Self {
    let path = data_dir.map(|dir| dir.join(FILE_NAME));
    let state = path
      .as_deref()
      .and_then(|path| std::fs::read_to_string(path).ok())
      .and_then(|text| {
        serde_json::from_str(&text)
          .inspect_err(|error| tracing::warn!(%error, "ignoring invalid saved state"))
          .ok()
      })
      .unwrap_or_default();
    Self {
      path,
      state: Mutex::new(state),
      ephemeral: AtomicBool::new(false),
    }
  }

  /// Stops writing to disk, so a benchmark run leaves the saved session alone.
  pub fn make_ephemeral(&self) {
    self.ephemeral.store(true, Ordering::Relaxed);
  }

  pub fn get(&self) -> UiState {
    self
      .state
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  /// Changes the state and writes it to disk if it changed.
  pub fn update(&self, change: impl FnOnce(&mut UiState)) {
    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
    let before = state.clone();
    change(&mut state);
    if *state != before
      && let Some(path) = &self.path
      && !self.ephemeral.load(Ordering::Relaxed)
      && let Err(error) = write_atomically(path, &state)
    {
      tracing::warn!(%error, "could not save window state");
    }
  }
}

fn write_atomically(path: &std::path::Path, state: &UiState) -> std::io::Result<()> {
  let bytes = serde_json::to_vec_pretty(state)?;
  flareite_workspace::write_file_atomically(path, &bytes)
}

/// Records which files are open, after documents are opened, closed, or saved
/// to a new path.
pub fn record_open_files(app: &AppHandle) {
  let state = app.state::<EditorState>();
  let files: Vec<PathBuf> = state
    .lock()
    .documents()
    .filter(|document| !state.is_hidden(document.id()))
    .filter_map(|document| document.path().map(PathBuf::from))
    .collect();
  app
    .state::<UiStateStore>()
    .update(|state| state.open_files = files);
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedUi {
  pub layout: serde_json::Value,
  pub active_file: Option<String>,
}

#[tauri::command]
pub fn ui_state_get(store: State<'_, UiStateStore>) -> SavedUi {
  let state = store.get();
  SavedUi {
    // Nothing saved yet (or something unusable): an empty layout.
    layout: if state.layout.is_object() {
      state.layout
    } else {
      serde_json::Value::Object(serde_json::Map::new())
    },
    active_file: state.active_file.map(|path| path.display().to_string()),
  }
}

#[tauri::command]
pub fn ui_state_save(
  store: State<'_, UiStateStore>,
  layout: serde_json::Value,
  active_file: Option<String>,
) -> Result<(), CommandError> {
  if !layout.is_object() || layout.to_string().len() > MAX_LAYOUT_BYTES {
    return Err(CommandError::new(
      "invalidLayout",
      "Layout must be a JSON object of at most 64 KiB.",
    ));
  }
  store.update(|state| {
    state.layout = layout;
    state.active_file = active_file.map(PathBuf::from);
  });
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn state_round_trips_through_disk() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = UiStateStore::load(Some(dir.path().to_path_buf()));
    assert_eq!(store.get(), UiState::default());
    store.update(|state| {
      state.workspace = Some(PathBuf::from("/projects/demo"));
      state.layout = serde_json::json!({ "sidebarWidth": 300 });
    });
    let reloaded = UiStateStore::load(Some(dir.path().to_path_buf()));
    assert_eq!(reloaded.get(), store.get());
  }

  #[test]
  fn invalid_state_files_are_ignored() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join(FILE_NAME), "{ not json").expect("write");
    let store = UiStateStore::load(Some(dir.path().to_path_buf()));
    assert_eq!(store.get(), UiState::default());
  }
}
