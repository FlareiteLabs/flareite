//! Renaming, moving, copying, and deleting files and folders from the
//! explorer, and showing them in the system's file manager.
//!
//! Every path must be inside the open folder, except what's copied in from a
//! drop from another program, which the backend itself names (see
//! `file_drop`). Open documents follow their
//! files when they're renamed or moved (their language servers and
//! extensions see the new path); documents of deleted files stay open, as
//! when another program deletes a file.

use std::path::{Component, Path, PathBuf};

use flareite_workspace::{EntryError, copy_entry, delete_entry, move_entry, rename_entry};
use serde::Serialize;
use tauri::{AppHandle, Manager, Window};

use crate::dialogs;
use crate::editor::EditorState;
use crate::editor::protocol::DocumentSummary;
use crate::error::CommandError;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::file_drop::DropState;
use crate::lsp::LspState;
use crate::ui_state;
use crate::workspace::WorkspaceState;

#[cfg(windows)]
const TRASH: &str = "Recycle Bin";
#[cfg(not(windows))]
const TRASH: &str = "Trash";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamedDto {
  pub path: String,
  /// Folders whose listings changed, outermost first.
  pub directories: Vec<String>,
}

fn root(app: &AppHandle) -> Result<PathBuf, CommandError> {
  app
    .state::<WorkspaceState>()
    .root()
    .ok_or_else(|| CommandError::new("noWorkspace", "No folder is open."))
}

/// `path` if it's in the open folder: inside it, or (with `root_allowed`)
/// the folder itself.
fn inside(root: &Path, path: &str, root_allowed: bool) -> Result<PathBuf, CommandError> {
  let path = PathBuf::from(path);
  let plain = path
    .components()
    .all(|c| !matches!(c, Component::ParentDir | Component::CurDir));
  if plain && path.starts_with(root) && (root_allowed || path != root) {
    Ok(path)
  } else {
    Err(CommandError::new(
      "outsideWorkspace",
      "Only files and folders in the open folder can be changed.",
    ))
  }
}

pub(crate) fn entry_error(error: EntryError) -> CommandError {
  match error {
    EntryError::InvalidName(message) => CommandError::new("invalidName", message),
    EntryError::NotADirectory(_) => CommandError::new("notADirectory", error.to_string()),
    EntryError::IntoItself(_) => CommandError::new("intoItself", error.to_string()),
    EntryError::Trash { .. } => CommandError::new("trash", error.to_string()),
    EntryError::File(error) => error.into(),
  }
}

async fn blocking<T: Send + 'static>(
  f: impl FnOnce() -> Result<T, EntryError> + Send + 'static,
) -> Result<T, CommandError> {
  tauri::async_runtime::spawn_blocking(f)
    .await
    .map_err(CommandError::internal)?
    .map_err(entry_error)
}

/// Points the documents of a renamed or moved file (or of the files in a
/// The files of documents `ids`.
fn moved_paths(app: &AppHandle, ids: &[flareite_core::DocumentId]) -> Vec<String> {
  let state = app.state::<EditorState>();
  let editor = state.lock();
  ids
    .iter()
    .filter_map(|id| Some(editor.document(*id).ok()?.path()?.display().to_string()))
    .collect()
}

/// folder) at their new paths, and tells their language servers,
/// extensions, and the frontend.
fn follow(app: &AppHandle, from: &Path, to: &Path) {
  let moved = {
    let state = app.state::<EditorState>();
    let mut editor = state.lock();
    let moved = editor.follow_move(from, to);
    for id in &moved {
      if let Ok(document) = editor.document(*id) {
        app.state::<LspState>().sync(app, document);
        app.state::<ExtensionsState>().sync(document);
        events::emit(
          app,
          AppEvent::DocumentMoved {
            document: DocumentSummary::of(document),
          },
        );
      }
    }
    moved
  };
  if !moved.is_empty() {
    ui_state::record_open_files(app);
    let paths: Vec<String> = moved_paths(app, &moved);
    if !paths.is_empty() {
      crate::editor::quick_diff::track_open(app, &paths);
    }
  }
}

/// Renames a file or folder; `name` may name new subfolders to move it into.
#[tauri::command]
pub async fn workspace_rename(
  app: AppHandle,
  path: String,
  name: String,
) -> Result<RenamedDto, CommandError> {
  let from = inside(&root(&app)?, &path, false)?;
  let source = from.clone();
  let renamed = blocking(move || rename_entry(&source, &name)).await?;
  tracing::info!(from = %from.display(), to = %renamed.path.display(), "renamed from the explorer");
  follow(&app, &from, &renamed.path);
  Ok(RenamedDto {
    path: renamed.path.display().to_string(),
    directories: renamed
      .directories
      .iter()
      .map(|dir| dir.display().to_string())
      .collect(),
  })
}

/// Moves files and folders into `target`, a folder, returning their new
/// paths. Stops at the first that can't be moved.
#[tauri::command]
pub async fn workspace_move(
  app: AppHandle,
  paths: Vec<String>,
  target: String,
) -> Result<Vec<String>, CommandError> {
  let root = root(&app)?;
  let into = inside(&root, &target, true)?;
  let mut moved = Vec::new();
  for path in paths {
    let from = inside(&root, &path, false)?;
    let (source, folder) = (from.clone(), into.clone());
    let to = blocking(move || move_entry(&source, &folder)).await?;
    if to != from {
      tracing::info!(from = %from.display(), to = %to.display(), "moved from the explorer");
      follow(&app, &from, &to);
    }
    moved.push(to.display().to_string());
  }
  Ok(moved)
}

/// Copies files and folders into `target`, a folder, returning the copies'
/// paths. A name that's taken gets ` copy` added.
#[tauri::command]
pub async fn workspace_copy(
  app: AppHandle,
  paths: Vec<String>,
  target: String,
) -> Result<Vec<String>, CommandError> {
  let root = root(&app)?;
  let into = inside(&root, &target, true)?;
  let mut copies = Vec::new();
  for path in paths {
    let (source, folder) = (inside(&root, &path, false)?, into.clone());
    let copy = blocking(move || copy_entry(&source, &folder)).await?;
    tracing::info!(copy = %copy.display(), "copied from the explorer");
    copies.push(copy.display().to_string());
  }
  Ok(copies)
}

/// Copies the files and folders dropped from another program (the last
/// drop, `drop_id`) into `target`, a folder, returning the copies' paths.
/// They can come from anywhere, but only the backend knows what they are.
#[tauri::command]
pub async fn workspace_copy_dropped(
  app: AppHandle,
  drop_id: u64,
  target: String,
) -> Result<Vec<String>, CommandError> {
  let root = root(&app)?;
  let into = inside(&root, &target, true)?;
  let sources = app
    .state::<DropState>()
    .dropped(drop_id)
    .ok_or_else(|| CommandError::new("staleDrop", "Those files are no longer being dropped."))?;
  let mut copies = Vec::new();
  for source in sources {
    let folder = into.clone();
    let copy = blocking(move || copy_entry(&source, &folder)).await?;
    tracing::info!(copy = %copy.display(), "copied a file dropped from another program");
    copies.push(copy.display().to_string());
  }
  Ok(copies)
}

/// Deletes files and folders after asking: to the system's trash, or
/// `permanent`ly. Offers to delete permanently what can't go to the trash.
/// Returns false if the user cancelled.
#[tauri::command]
pub async fn workspace_delete(
  window: Window,
  paths: Vec<String>,
  permanent: bool,
) -> Result<bool, CommandError> {
  let root = root(window.app_handle())?;
  let paths = paths
    .iter()
    .map(|path| inside(&root, path, false))
    .collect::<Result<Vec<_>, _>>()?;
  let what = match paths.as_slice() {
    [path] => {
      let name = path.file_name().unwrap_or_default().to_string_lossy();
      if path.is_dir() {
        format!("\"{name}\" and everything in it")
      } else {
        format!("\"{name}\"")
      }
    }
    _ => format!("these {} items", paths.len()),
  };
  let (message, action) = if permanent {
    (
      format!("Are you sure you want to permanently delete {what}?\n\nThis can't be undone."),
      "Delete".to_owned(),
    )
  } else {
    (
      format!("Are you sure you want to delete {what}?\n\nYou can restore it from the {TRASH}."),
      format!("Move to {TRASH}"),
    )
  };
  if !dialogs::confirm(&window, message, &action).await {
    return Ok(false);
  }
  for path in paths {
    let target = path.clone();
    match blocking(move || delete_entry(&target, !permanent)).await {
      Ok(()) => tracing::info!(path = %path.display(), permanent, "deleted from the explorer"),
      Err(error) if error.code == "trash" => {
        let message = format!("{}\n\nDelete it permanently instead?", error.message);
        if !dialogs::confirm(&window, message, "Delete Permanently").await {
          return Ok(false);
        }
        let target = path.clone();
        blocking(move || delete_entry(&target, false)).await?;
        tracing::info!(path = %path.display(), "deleted permanently from the explorer");
      }
      Err(error) => return Err(error),
    }
  }
  Ok(true)
}

/// Shows a file or folder in the system's file manager (selected, where
/// the system can do that).
#[tauri::command]
pub async fn workspace_reveal(app: AppHandle, path: String) -> Result<(), CommandError> {
  let path = inside(&root(&app)?, &path, true)?;
  reveal(&path).map_err(|error| CommandError::new("reveal", error.to_string()))
}

#[cfg(windows)]
fn reveal(path: &Path) -> std::io::Result<()> {
  use std::os::windows::process::CommandExt;
  // Windows paths can't contain quotes, so quoting it this way is safe.
  std::process::Command::new("explorer")
    .raw_arg(format!("/select,\"{}\"", path.display()))
    .spawn()
    .map(drop)
}

#[cfg(target_os = "macos")]
fn reveal(path: &Path) -> std::io::Result<()> {
  std::process::Command::new("open")
    .arg("-R")
    .arg(path)
    .spawn()
    .map(drop)
}

/// Other systems have no common way to select a file: open its folder.
#[cfg(not(any(windows, target_os = "macos")))]
fn reveal(path: &Path) -> std::io::Result<()> {
  let folder = if path.is_dir() {
    path
  } else {
    path.parent().unwrap_or(path)
  };
  open::that_detached(folder)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_paths_in_the_open_folder_are_accepted() {
    let root = std::env::temp_dir().join("flareite-root");
    let at = |path: PathBuf| path.display().to_string();
    let file = root.join("src").join("a.rs");
    assert_eq!(inside(&root, &at(file.clone()), false).ok(), Some(file));
    assert!(inside(&root, &at(root.clone()), false).is_err());
    assert!(inside(&root, &at(root.clone()), true).is_ok());
    for outside in [
      root.join("..").join("x"),
      root.join("src").join("..").join("..").join("x"),
      std::env::temp_dir().join("flareite-rooted"),
      PathBuf::from("src"),
    ] {
      assert!(
        inside(&root, &at(outside.clone()), true).is_err(),
        "{}",
        outside.display()
      );
    }
  }
}
