//! Native dialogs, shown from the backend so the frontend needs no dialog
//! permissions. Blocking dialog calls run on the blocking thread pool.

use std::path::{Path, PathBuf};

use tauri::Window;
use tauri_plugin_dialog::{
  DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

/// The window a dialog belongs to: the one the user is in, which may be
/// another window than the main one that asked.
fn parent(window: &Window) -> Window {
  crate::windows::dialog_parent(window)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsavedChoice {
  Save,
  Discard,
  Cancel,
}

pub async fn pick_file_to_open(window: &Window) -> Option<PathBuf> {
  let dialog = window.dialog().file().set_parent(&parent(window));
  run_blocking(move || dialog.blocking_pick_file())
    .await
    .flatten()
    .and_then(|path| path.into_path().ok())
}

pub async fn pick_folder(window: &Window) -> Option<PathBuf> {
  let dialog = window.dialog().file().set_parent(&parent(window));
  run_blocking(move || dialog.blocking_pick_folder())
    .await
    .flatten()
    .and_then(|path| path.into_path().ok())
}

/// Asks for a folder without opening it (an extension's prompt, say), with
/// `title` as the dialog's. `None` if cancelled.
#[tauri::command]
pub async fn dialog_pick_folder(window: Window, title: String) -> Option<String> {
  let dialog = window
    .dialog()
    .file()
    .set_parent(&parent(&window))
    .set_title(title);
  run_blocking(move || dialog.blocking_pick_folder())
    .await
    .flatten()
    .and_then(|path| path.into_path().ok())
    .map(|path| crate::paths::normalize(&path).display().to_string())
}

pub async fn pick_save_path(
  window: &Window,
  current: Option<&Path>,
  title: &str,
) -> Option<PathBuf> {
  let mut dialog = window.dialog().file().set_parent(&parent(window));
  match current {
    Some(path) => {
      if let Some(directory) = path.parent() {
        dialog = dialog.set_directory(directory);
      }
      if let Some(name) = path.file_name() {
        dialog = dialog.set_file_name(name.to_string_lossy());
      }
    }
    None => dialog = dialog.set_file_name(format!("{title}.txt")),
  }
  run_blocking(move || dialog.blocking_save_file())
    .await
    .flatten()
    .and_then(|path| path.into_path().ok())
}

pub async fn ask_unsaved(window: &Window, message: String, save_label: &str) -> UnsavedChoice {
  let save_label = save_label.to_owned();
  let dialog = window
    .dialog()
    .message(format!(
      "{message}\n\nYour changes will be lost if you don't save them."
    ))
    .kind(MessageDialogKind::Warning)
    .buttons(MessageDialogButtons::YesNoCancelCustom(
      save_label.clone(),
      "Don't Save".into(),
      "Cancel".into(),
    ))
    .parent(&parent(window));
  match run_blocking(move || dialog.blocking_show_with_result()).await {
    Some(MessageDialogResult::Yes) => UnsavedChoice::Save,
    Some(MessageDialogResult::No) => UnsavedChoice::Discard,
    Some(MessageDialogResult::Custom(label)) if label == save_label => UnsavedChoice::Save,
    Some(MessageDialogResult::Custom(label)) if label == "Don't Save" => UnsavedChoice::Discard,
    _ => UnsavedChoice::Cancel,
  }
}

pub async fn confirm_overwrite(window: &Window, title: &str) -> bool {
  let dialog = window
    .dialog()
    .message(format!(
      "{title} was changed on disk by another program since you opened it.\n\nDo you want to overwrite it with your version?"
    ))
    .kind(MessageDialogKind::Warning)
    .buttons(MessageDialogButtons::OkCancelCustom(
      "Overwrite".into(),
      "Cancel".into(),
    ))
    .parent(&parent(window));
  match run_blocking(move || dialog.blocking_show_with_result()).await {
    Some(MessageDialogResult::Ok) => true,
    Some(MessageDialogResult::Custom(label)) => label == "Overwrite",
    _ => false,
  }
}

/// Asks to go ahead with something that can't easily be undone: true if
/// the user chose `action`.
pub async fn confirm(window: &Window, message: String, action: &str) -> bool {
  let action = action.to_owned();
  let dialog = window
    .dialog()
    .message(message)
    .kind(MessageDialogKind::Warning)
    .buttons(MessageDialogButtons::OkCancelCustom(
      action.clone(),
      "Cancel".into(),
    ))
    .parent(&parent(window));
  match run_blocking(move || dialog.blocking_show_with_result()).await {
    Some(MessageDialogResult::Ok) => true,
    Some(MessageDialogResult::Custom(label)) => label == action,
    _ => false,
  }
}

async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
  match tauri::async_runtime::spawn_blocking(f).await {
    Ok(value) => Some(value),
    Err(error) => {
      tracing::error!(%error, "dialog task failed");
      None
    }
  }
}
