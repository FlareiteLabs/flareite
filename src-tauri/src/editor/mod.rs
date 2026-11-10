//! Integration of the editor core with the desktop shell: shared editor
//! state, document lifecycle (open, save, close) with file dialogs and
//! unsaved-changes prompts, and the editing commands the frontend calls.

mod commands;
mod counts;
pub(crate) mod fold_memory;
mod folding;
mod minimap;
mod peek;
pub(crate) mod protocol;
mod provided;
pub(crate) mod quick_diff;
pub(crate) mod syntax;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use std::time::Instant;

use flareite_core::{DocumentId, Editor, ViewId};
use flareite_tasks::Priority;
use flareite_workspace::{Abandoned, FileError, load_text_file};
use tauri::{AppHandle, Manager, Window};

use crate::events::{self, AppEvent, NoticeLevel};
use crate::perf::Perf;
use crate::ui_state::UiStateStore;
use protocol::DocumentSummary;

pub use commands::*;
pub use peek::*;
pub use provided::*;
pub use quick_diff::*;

/// Views numbered from here are layout tabs' editor panes
/// (`state/layouts.ts`): their folds are the extension's (unchanged lines in
/// a diff), so they aren't remembered.
pub const PANE_VIEWS: u32 = 1 << 30;

pub fn is_pane_view(view: ViewId) -> bool {
  view.get() >= PANE_VIEWS
}

/// The authoritative editor state. Commands hold the lock only briefly; file
/// I/O and dialogs run without it.
#[derive(Default)]
pub struct EditorState {
  editor: Arc<Mutex<Editor>>,
  /// Background find-match counts for large documents.
  pub(crate) counts: counts::MatchCounts,
  /// Syntax trees and highlighting state.
  pub(crate) syntax: syntax::SyntaxStates,
  /// Folded regions remembered for documents, until they are restored.
  pub(crate) pending_folds: fold_memory::PendingFolds,
  /// Region comments of documents.
  pub(crate) regions: folding::RegionStates,
  /// The text last copied as whole lines (copy with nothing selected), so
  /// pasting it can insert whole lines too.
  copied_lines: Mutex<Option<String>>,
  /// Documents open only for diff tabs, with no tab of their own: the
  /// workbench doesn't list them, and they aren't reopened next time.
  hidden: Mutex<HashSet<DocumentId>>,
  /// Open files' originals and changes from them.
  pub(crate) quick: quick_diff::QuickDiffs,
  /// Set once the user has confirmed closing the window with unsaved changes.
  close_confirmed: AtomicBool,
  close_prompt_open: AtomicBool,
}

impl EditorState {
  pub(crate) fn lock(&self) -> MutexGuard<'_, Editor> {
    // A panic while holding the lock must not make every document inaccessible.
    self.editor.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn copied_lines(&self) -> Option<String> {
    self
      .copied_lines
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  fn set_copied_lines(&self, text: Option<String>) {
    *self
      .copied_lines
      .lock()
      .unwrap_or_else(PoisonError::into_inner) = text;
  }

  pub(crate) fn is_hidden(&self, id: DocumentId) -> bool {
    self
      .hidden
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .contains(&id)
  }

  /// Hides a document (`true`) or lists it again.
  pub(crate) fn set_hidden(&self, id: DocumentId, hidden: bool) {
    let mut set = self.hidden.lock().unwrap_or_else(PoisonError::into_inner);
    if hidden {
      set.insert(id);
    } else {
      set.remove(&id);
    }
  }

  fn has_unsaved_changes(&self) -> bool {
    self.lock().documents().any(|document| document.is_dirty())
  }

  /// Whether the app is exiting with every unsaved change saved or
  /// deliberately discarded.
  pub(crate) fn exit_is_clean(&self) -> bool {
    self.close_confirmed.load(Ordering::SeqCst) || !self.has_unsaved_changes()
  }
}

/// Opens the previous session's files and those named on the command line,
/// on the worker pool so the window does not wait for them, after restoring
/// unsaved changes from crashed sessions. Each opened file is announced to
/// the frontend; the one matching the saved active file becomes the active
/// tab. Files that can't be opened are skipped and reported together.
pub fn open_startup_files(app: &AppHandle, files: Vec<PathBuf>, crashed: Option<Abandoned>) {
  let task_app = app.clone();
  app
    .state::<Perf>()
    .scheduler
    .spawn("open-startup-files", Priority::High, None, move |token| {
      if let Some(crashed) = crashed {
        crate::backups::restore(&task_app, crashed);
      }
      let active = task_app.state::<UiStateStore>().get().active_file;
      let mut failed = Vec::new();
      for file in files {
        if token.is_cancelled() {
          break;
        }
        let path = crate::paths::normalize(&file);
        // Already open (restored from a backup): only activate it if wanted.
        let open = {
          let state = task_app.state::<EditorState>();
          let editor = state.lock();
          editor
            .find_by_path(&path)
            .and_then(|id| editor.document(id).ok().map(DocumentSummary::of))
        };
        if let Some(document) = open {
          if active.as_deref() == Some(path.as_path()) {
            events::emit(
              &task_app,
              AppEvent::DocumentOpened {
                document,
                active: true,
              },
            );
          }
          continue;
        }
        let started = Instant::now();
        let loaded = match load_text_file(&path) {
          Ok(loaded) => loaded,
          // An image, say: the workbench opens it in an extension's viewer,
          // or asks whether to open it as text.
          Err(FileError::Binary { .. }) => {
            events::emit(
              &task_app,
              AppEvent::OpenFile {
                path: path.display().to_string(),
                active: active.as_deref() == Some(path.as_path()),
              },
            );
            continue;
          }
          Err(error) => {
            tracing::warn!(%error, "could not open file at startup");
            failed.push(error.to_string());
            continue;
          }
        };
        task_app
          .state::<Perf>()
          .record("document.load", started.elapsed());
        let summary = {
          let state = task_app.state::<EditorState>();
          let mut editor = state.lock();
          if editor.find_by_path(&path).is_some() {
            continue;
          }
          let id = editor.open(path.clone(), loaded);
          tracing::info!(document = %id, path = %path.display(), "opened file at startup");
          editor.document(id).ok().map(|document| {
            fold_memory::recall(&task_app, document);
            task_app
              .state::<crate::extensions::ExtensionsState>()
              .sync(document);
            DocumentSummary::of(document)
          })
        };
        if let Some(document) = summary {
          let is_active = active.as_deref() == Some(path.as_path());
          events::emit(
            &task_app,
            AppEvent::DocumentOpened {
              document,
              active: is_active,
            },
          );
        }
      }
      crate::ui_state::record_open_files(&task_app);
      if !failed.is_empty() {
        let message = match failed.as_slice() {
          [one] => format!("A file from the last session could not be reopened: {one}."),
          many => format!(
            "{} files from the last session could not be reopened: {}.",
            many.len(),
            many.join("; ")
          ),
        };
        events::emit(
          &task_app,
          AppEvent::Notice {
            level: NoticeLevel::Warning,
            message,
          },
        );
      }
    });
}

/// Decides whether the window may close. With unsaved documents, prevents
/// the close and asks the user (asynchronously) whether to save them.
pub fn on_close_requested(window: &Window, api: &tauri::CloseRequestApi) {
  if window.state::<EditorState>().exit_is_clean() {
    return;
  }
  api.prevent_close();
  confirm_then_close(window);
}

/// Decides whether the app may exit when that's requested other than by
/// closing the window (Quit on macOS). With unsaved documents, prevents the
/// exit and asks first, as when closing the window.
pub fn on_exit_requested(app: &AppHandle, api: &tauri::ExitRequestApi) {
  if app.state::<EditorState>().exit_is_clean() {
    return;
  }
  // `get_window`, not `get_webview_window`, which leaves out a window
  // holding a web page pane's view.
  match app.get_window(crate::windows::MAIN) {
    Some(window) => {
      api.prevent_exit();
      confirm_then_close(&window);
    }
    // No window to ask in; the backups keep the changes.
    None => tracing::warn!("exiting with unsaved changes; they are kept as backups"),
  }
}

/// Asks about unsaved changes (once at a time) and closes the window if the
/// user saves or discards them.
fn confirm_then_close(window: &Window) {
  let window = window.clone();
  tauri::async_runtime::spawn(async move {
    if confirm_exit(&window).await
      && let Err(error) = window.destroy()
    {
      tracing::error!(%error, "failed to close the window");
    }
  });
}

/// Asks about unsaved changes, if there are any, and whether the user saves
/// or discards them, lets the app exit without asking again. False if the
/// user cancelled, or the question is already being asked.
pub async fn confirm_exit(window: &Window) -> bool {
  let state = window.state::<EditorState>();
  if state.close_prompt_open.swap(true, Ordering::SeqCst) {
    return false;
  }
  let close = commands::confirm_close_with_unsaved(window).await;
  state.close_prompt_open.store(false, Ordering::SeqCst);
  if close {
    state.close_confirmed.store(true, Ordering::SeqCst);
  }
  close
}
