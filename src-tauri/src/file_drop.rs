//! Files and folders dragged onto the window from other programs.
//!
//! Tauri's drop handler reports the drag (`WindowEvent::DragDrop`), and it
//! goes to the frontend as `FileDrag` events: the frontend follows it as it
//! follows its own drags (`src/components/drag.ts`), opening the files where
//! they're dropped or copying them into the explorer's folder. HTML5 drag
//! and drop isn't used, because on Windows Tauri's handler replaces
//! WebView2's.
//!
//! The last drop is remembered, so copying it (`workspace_copy_dropped`)
//! names it by id: the frontend never names files outside the open folder
//! to copy.

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, DragDropEvent, Manager};

use crate::events::{self, AppEvent};
use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileDragPhase {
  Enter,
  Over,
  Drop,
  Leave,
}

/// The last drop: its id and what was dropped.
#[derive(Default)]
pub struct DropState(Mutex<(u64, Vec<PathBuf>)>);

impl DropState {
  fn remember(&self, paths: Vec<PathBuf>) -> u64 {
    let mut last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
    last.0 += 1;
    last.1 = paths;
    last.0
  }

  /// What drop `id` dropped, if it's the last one.
  pub fn dropped(&self, id: u64) -> Option<Vec<PathBuf>> {
    let last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
    (last.0 == id).then(|| last.1.clone())
  }
}

/// Forwards a drag from another program over `window` to the frontend.
pub fn on_drag_drop(app: &AppHandle, window: &str, event: &DragDropEvent) {
  let (phase, position, paths) = match event {
    DragDropEvent::Enter { paths, position } => (FileDragPhase::Enter, Some(*position), paths),
    DragDropEvent::Over { position } => (FileDragPhase::Over, Some(*position), &Vec::new()),
    DragDropEvent::Drop { paths, position } => (FileDragPhase::Drop, Some(*position), paths),
    DragDropEvent::Leave => (FileDragPhase::Leave, None, &Vec::new()),
    _ => return,
  };
  let paths: Vec<PathBuf> = paths.iter().map(|path| paths::normalize(path)).collect();
  let (folders, files): (Vec<_>, Vec<_>) = paths.iter().partition(|path| path.is_dir());
  let text = |paths: Vec<&PathBuf>| {
    paths
      .iter()
      .map(|path| path.display().to_string())
      .collect()
  };
  let (files, folders) = (text(files), text(folders));
  let drop_id = (phase == FileDragPhase::Drop).then(|| app.state::<DropState>().remember(paths));
  let (x, y) = position.map_or((0.0, 0.0), |position| (position.x, position.y));
  events::emit(
    app,
    AppEvent::FileDrag {
      window: crate::windows::window_id(window).map(str::to_owned),
      phase,
      x,
      y,
      files,
      folders,
      drop_id,
    },
  );
}
