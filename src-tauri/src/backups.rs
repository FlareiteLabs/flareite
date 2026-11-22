//! Crash recovery: backups of unsaved documents while the app runs, and
//! restoring them after an instance ended without shutting down cleanly.
//!
//! A background thread writes a backup of each document with unsaved
//! changes shortly after it changes (see [`flareite_workspace::BackupSession`]
//! for the files and how abandoned sessions are recognized). Editing paths
//! only signal the thread; snapshots are taken under the editor lock, which
//! is cheap (ropes are shared), and written without it. Large documents are
//! backed up less often. Backups of documents that are saved, closed, or
//! undone back to their saved text are removed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use flareite_core::{DocumentId, Editor, EditorOp};
use flareite_text::{TextSnapshot, Version};
use flareite_workspace::{
  Abandoned, BackupMeta, BackupSession, DecodeOptions, FileError, RecoveredBackup, TextEncoding,
  load_text_file_with,
};
use tauri::{AppHandle, Manager};

use crate::editor::EditorState;
use crate::editor::protocol::DocumentSummary;
use crate::events::{self, AppEvent, NoticeLevel};
use crate::paths;

/// Edits within this long of each other are written in one backup.
const DEBOUNCE: Duration = Duration::from_millis(750);
/// Backup throughput to plan for: a document is backed up again at most
/// once per its size at this rate (plus [`DEBOUNCE`]), so typing in a
/// 100 MB file writes a backup every 25 seconds rather than continuously.
const BYTES_PER_SECOND: usize = 4 * 1024 * 1024;

fn interval_for(bytes: usize) -> Duration {
  DEBOUNCE + Duration::from_secs_f64(bytes as f64 / BYTES_PER_SECOND as f64)
}

/// When a document's last backup was written, and of which version.
struct Written {
  version: Version,
  at: Instant,
}

/// Tracks which documents are backed up and writes and removes backups.
pub struct Writer {
  session: BackupSession,
  written: HashMap<DocumentId, Written>,
  /// Whether the last write failed (logged once until a write succeeds).
  failing: bool,
}

struct Job {
  id: DocumentId,
  version: Version,
  meta: BackupMeta,
  text: TextSnapshot,
}

impl Writer {
  pub fn new(session: BackupSession) -> Self {
    Self {
      session,
      written: HashMap::new(),
      failing: false,
    }
  }

  /// Brings the backups up to date with `editor`'s documents. Unless
  /// `force` is set, documents backed up recently wait for their interval.
  /// Returns when the next waiting document is due.
  pub fn sweep<'a>(
    &mut self,
    lock: impl FnOnce() -> MutexGuard<'a, Editor>,
    force: bool,
  ) -> Option<Instant> {
    let now = Instant::now();
    let mut next_due: Option<Instant> = None;
    let mut jobs = Vec::new();
    let mut removals = Vec::new();
    {
      let editor = lock();
      for document in editor.documents() {
        let id = document.id();
        if !document.is_dirty() {
          if self.written.contains_key(&id) {
            removals.push(id);
          }
          continue;
        }
        if let Some(written) = self.written.get(&id) {
          if written.version == document.version() {
            continue;
          }
          let due = written.at + interval_for(document.buffer().len());
          if !force && due > now {
            next_due = Some(next_due.map_or(due, |next| next.min(due)));
            continue;
          }
        }
        jobs.push(Job {
          id,
          version: document.version(),
          meta: BackupMeta {
            path: document.path().map(Path::to_path_buf),
            title: document.title(),
            cursor: document.selections().primary().head,
            encoding: document.path().map(|_| document.encoding().id()),
          },
          text: document.buffer().snapshot(),
        });
      }
      removals.extend(
        self
          .written
          .keys()
          .filter(|id| editor.document(**id).is_err())
          .copied(),
      );
    }

    for job in jobs {
      match self.session.write(&name(job.id), &job.meta, &job.text) {
        Ok(()) => {
          self.failing = false;
          self.written.insert(
            job.id,
            Written {
              version: job.version,
              at: now,
            },
          );
        }
        Err(error) => {
          if !self.failing {
            tracing::warn!(%error, document = %job.id, "could not write a backup");
          }
          self.failing = true;
        }
      }
    }
    for id in removals {
      self.forget(id);
    }
    next_due
  }

  /// Removes the backup of `id`.
  pub fn forget(&mut self, id: DocumentId) {
    if self.written.remove(&id).is_some()
      && let Err(error) = self.session.remove(&name(id))
    {
      tracing::warn!(%error, document = %id, "could not remove a backup");
    }
  }

  pub fn session_dir(&self) -> &Path {
    self.session.dir()
  }

  /// Removes all backups after a clean exit.
  pub fn discard(self) {
    let dir = self.session.dir().to_path_buf();
    if let Err(error) = self.session.discard() {
      tracing::warn!(%error, dir = %dir.display(), "could not remove the backups");
    }
  }
}

fn name(id: DocumentId) -> String {
  id.get().to_string()
}

#[derive(Default)]
struct Signal {
  pending: bool,
  stop: bool,
}

struct Shared {
  signal: Mutex<Signal>,
  wake: Condvar,
  writer: Mutex<Option<Writer>>,
}

impl Shared {
  fn signal(&self) -> MutexGuard<'_, Signal> {
    self.signal.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn writer(&self) -> MutexGuard<'_, Option<Writer>> {
    self.writer.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Backups for this instance. Without a backup directory (or in benchmark
/// runs) it does nothing.
#[derive(Default)]
pub struct BackupState {
  shared: Option<Arc<Shared>>,
  thread: Mutex<Option<JoinHandle<()>>>,
}

impl BackupState {
  /// Notes that documents may have changed; the backup thread catches up
  /// shortly. Cheap, and safe to call with the editor lock held.
  pub fn touch(&self) {
    if let Some(shared) = &self.shared {
      shared.signal().pending = true;
      shared.wake.notify_one();
    }
  }

  /// Removes the backup of a closed document right away, so changes the
  /// user chose to discard are not restored after a crash.
  pub fn forget(&self, id: DocumentId) {
    if let Some(shared) = &self.shared
      && let Some(writer) = shared.writer().as_mut()
    {
      writer.forget(id);
    }
  }

  fn flush(&self, app: &AppHandle) {
    if let Some(shared) = &self.shared
      && let Some(writer) = shared.writer().as_mut()
    {
      let editor = app.state::<EditorState>();
      writer.sweep(|| editor.lock(), true);
    }
  }
}

/// Sets up backups in `root` (none if `None`) and returns the sessions of
/// earlier instances that crashed, to be restored once startup files open.
pub fn init(app: &AppHandle, root: Option<PathBuf>) -> Option<Abandoned> {
  let Some(root) = root else {
    app.manage(BackupState::default());
    return None;
  };
  let session = match BackupSession::create(&root) {
    Ok(session) => session,
    Err(error) => {
      tracing::warn!(%error, root = %root.display(), "backups of unsaved changes are unavailable");
      app.manage(BackupState::default());
      return None;
    }
  };
  let abandoned = match Abandoned::find(&root, Some(session.dir())) {
    Ok(abandoned) => abandoned,
    Err(error) => {
      tracing::warn!(%error, "could not look for backups from earlier sessions");
      Abandoned::default()
    }
  };
  for (path, reason) in &abandoned.unreadable {
    tracing::warn!(path = %path.display(), %reason, "could not read a backup");
  }
  tracing::debug!(dir = %session.dir().display(), "backing up unsaved changes");

  let shared = Arc::new(Shared {
    signal: Mutex::new(Signal::default()),
    wake: Condvar::new(),
    writer: Mutex::new(Some(Writer::new(session))),
  });
  let thread = {
    let (shared, app) = (shared.clone(), app.clone());
    std::thread::Builder::new()
      .name("backups".into())
      .spawn(move || run(&shared, &app))
  };
  let thread = match thread {
    Ok(thread) => Some(thread),
    Err(error) => {
      tracing::warn!(%error, "could not start the backup thread");
      None
    }
  };
  app.manage(BackupState {
    shared: Some(shared),
    thread: Mutex::new(thread),
  });
  (!abandoned.is_empty()).then_some(abandoned)
}

fn run(shared: &Shared, app: &AppHandle) {
  let mut due: Option<Instant> = None;
  loop {
    {
      let mut signal = shared.signal();
      while !signal.pending && !signal.stop {
        signal = match due {
          Some(at) => {
            let wait = at.saturating_duration_since(Instant::now());
            if wait.is_zero() {
              break;
            }
            shared
              .wake
              .wait_timeout(signal, wait)
              .unwrap_or_else(PoisonError::into_inner)
              .0
          }
          None => shared
            .wake
            .wait(signal)
            .unwrap_or_else(PoisonError::into_inner),
        };
      }
      if signal.stop {
        return;
      }
      // Let a burst of edits settle (an exit request ends the wait).
      let settle = Instant::now() + DEBOUNCE;
      while !signal.stop {
        let wait = settle.saturating_duration_since(Instant::now());
        if wait.is_zero() {
          break;
        }
        signal = shared
          .wake
          .wait_timeout(signal, wait)
          .unwrap_or_else(PoisonError::into_inner)
          .0;
      }
      if signal.stop {
        return;
      }
      signal.pending = false;
    }
    let editor = app.state::<EditorState>();
    due = match shared.writer().as_mut() {
      Some(writer) => writer.sweep(|| editor.lock(), false),
      None => return,
    };
  }
}

/// Stops the backup thread. After a clean exit (unsaved changes were saved
/// or discarded when asked) the backups are removed; otherwise they are
/// brought up to date and kept for the next start.
pub fn shutdown(app: &AppHandle) {
  let Some(state) = app.try_state::<BackupState>() else {
    return;
  };
  let Some(shared) = &state.shared else {
    return;
  };
  shared.signal().stop = true;
  shared.wake.notify_all();
  if let Some(thread) = state
    .thread
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .take()
  {
    let _ = thread.join();
  }
  let clean = app.state::<EditorState>().exit_is_clean();
  let writer = shared.writer().take();
  if let Some(mut writer) = writer {
    if clean {
      writer.discard();
    } else {
      let editor = app.state::<EditorState>();
      writer.sweep(|| editor.lock(), true);
      tracing::info!(dir = %writer.session_dir().display(), "kept backups of unsaved changes");
    }
  }
}

/// What restoring one backup did.
enum Restored {
  /// Opened as a new tab.
  Opened(DocumentSummary),
  /// Applied to a document that was already open.
  Updated(DocumentSummary),
  /// The file already contains the backed-up text.
  Unchanged,
}

/// Restores the documents of crashed sessions as unsaved documents, backs
/// them up in this session, removes the old sessions, and tells the user.
pub fn restore(app: &AppHandle, abandoned: Abandoned) {
  let mut titles = Vec::new();
  let mut problems = Vec::new();
  for backup in &abandoned.documents {
    match restore_one(app, backup) {
      Ok(Restored::Opened(document)) => {
        titles.push(document.title.clone());
        events::emit(
          app,
          AppEvent::DocumentOpened {
            document,
            active: false,
          },
        );
      }
      Ok(Restored::Updated(document)) => {
        titles.push(document.title.clone());
        events::emit(app, AppEvent::DocumentReloaded { document });
      }
      Ok(Restored::Unchanged) => {}
      Err(error) => {
        tracing::warn!(title = %backup.meta.title, %error, "could not restore a backup");
        problems.push(backup.meta.title.clone());
      }
    }
  }
  let state = app.state::<BackupState>();
  state.flush(app);
  // Keep the old backups if any could not be restored, so nothing is lost.
  if problems.is_empty()
    && let Err(error) = abandoned.finish()
  {
    tracing::warn!(%error, "could not remove backups from an earlier session");
  }
  if !titles.is_empty() {
    tracing::info!(
      documents = titles.len(),
      "restored unsaved changes after a crash"
    );
    let message = format!(
      "Flareite didn't shut down properly last time. Unsaved changes were restored: {}.",
      list(&titles)
    );
    events::emit(
      app,
      AppEvent::Notice {
        level: NoticeLevel::Warning,
        message,
      },
    );
  }
  if !problems.is_empty() {
    events::emit(
      app,
      AppEvent::Notice {
        level: NoticeLevel::Warning,
        message: format!(
          "Unsaved changes from the last session could not be restored: {}. The backups were kept; see the log for details.",
          list(&problems)
        ),
      },
    );
  }
}

fn restore_one(app: &AppHandle, backup: &RecoveredBackup) -> Result<Restored, String> {
  let state = app.state::<EditorState>();
  let mut was_open = false;
  let mut loaded = None;
  if let Some(path) = &backup.meta.path {
    let path = paths::normalize(path);
    if state.lock().find_by_path(&path).is_some() {
      was_open = true;
    } else {
      // As it was read before, else as it reads now.
      let options = DecodeOptions {
        encoding: backup
          .meta
          .encoding
          .as_deref()
          .and_then(TextEncoding::from_id),
        binary: true,
      };
      match load_text_file_with(&path, options) {
        Ok(file) if file.text == backup.text => return Ok(Restored::Unchanged),
        Ok(file) => loaded = Some((path, file)),
        // Deleted since: restore the text as an untitled document.
        Err(FileError::NotFound { .. }) => {}
        Err(error) => return Err(error.to_string()),
      }
    }
  }

  let mut editor = state.lock();
  let id = match (&backup.meta.path, loaded) {
    (Some(path), _) if was_open => editor
      .find_by_path(&paths::normalize(path))
      .ok_or("the document was closed")?,
    (_, Some((path, file))) => {
      let config = flareite_workspace::editorconfig_for(&path);
      let id = editor.open(path, file);
      let settings = app.state::<crate::settings::SettingsState>().get();
      let language_spaces = editor.document(id).ok().and_then(|document| {
        app
          .state::<crate::languages::LanguagesState>()
          .insert_spaces_for(document)
      });
      if let Ok(document) = editor.document_mut(id) {
        crate::editor::indentation::on_open(document, &config, &settings.editor, language_spaces);
      }
      id
    }
    _ => editor.create_untitled(),
  };
  let document = editor.document_mut(id).map_err(|error| error.to_string())?;
  if !document
    .replace_text(&backup.text)
    .map_err(|error| error.to_string())?
    && !was_open
  {
    // An untitled backup with no text.
    let _ = editor.close(id);
    return Ok(Restored::Unchanged);
  }
  let document = editor.document_mut(id).map_err(|error| error.to_string())?;
  let mut cursor = backup.meta.cursor.min(document.buffer().len());
  while !document.buffer().is_char_boundary(cursor) {
    cursor -= 1;
  }
  let _ = document.apply(EditorOp::SetCursor {
    offset: cursor,
    extend: false,
    add: false,
  });
  tracing::info!(document = %id, title = %backup.meta.title, "restored unsaved changes");
  app
    .state::<crate::extensions::ExtensionsState>()
    .sync(document);
  let summary = DocumentSummary::of(document);
  Ok(if was_open {
    Restored::Updated(summary)
  } else {
    Restored::Opened(summary)
  })
}

fn list(titles: &[String]) -> String {
  const SHOWN: usize = 5;
  let mut text = titles
    .iter()
    .take(SHOWN)
    .cloned()
    .collect::<Vec<_>>()
    .join(", ");
  if titles.len() > SHOWN {
    text.push_str(&format!(" and {} more", titles.len() - SHOWN));
  }
  text
}

#[cfg(test)]
mod tests {
  use std::sync::LazyLock;

  use flareite_core::EditorConfig;

  use super::*;

  #[test]
  fn backs_up_dirty_documents_and_removes_clean_ones() {
    static EDITOR: LazyLock<Mutex<Editor>> =
      LazyLock::new(|| Mutex::new(Editor::new(EditorConfig::default())));
    let lock = || EDITOR.lock().unwrap_or_else(PoisonError::into_inner);
    let root = tempfile::tempdir().expect("temp dir");
    let mut writer = Writer::new(BackupSession::create(root.path()).expect("session"));
    let dir = writer.session_dir().to_path_buf();
    let file_of = |id: DocumentId| dir.join(format!("{}.backup", id.get()));

    let (clean, dirty) = {
      let mut editor = lock();
      let clean = editor.create_untitled();
      let dirty = editor.create_untitled();
      let document = editor.document_mut(dirty).expect("document");
      document
        .apply(EditorOp::Insert("draft".into()))
        .expect("insert");
      (clean, dirty)
    };
    assert_eq!(writer.sweep(lock, false), None);
    assert!(!file_of(clean).exists());
    assert!(file_of(dirty).is_file());

    // A change right after the backup waits for the interval...
    lock()
      .document_mut(dirty)
      .expect("document")
      .apply(EditorOp::Insert(" more".into()))
      .expect("insert");
    assert!(writer.sweep(lock, false).is_some());
    // ...unless forced.
    assert_eq!(writer.sweep(lock, true), None);
    let found = Abandoned::find(root.path(), None).expect("find");
    assert!(found.is_empty(), "the running session is not abandoned");

    // Undoing back to the saved (empty) text removes the backup.
    {
      let mut editor = lock();
      let document = editor.document_mut(dirty).expect("document");
      while document.is_dirty() {
        document.apply(EditorOp::Undo).expect("undo");
      }
    }
    writer.sweep(lock, true);
    assert!(!file_of(dirty).exists());

    // Closing removes it too.
    let other = {
      let mut editor = lock();
      let other = editor.create_untitled();
      editor
        .document_mut(other)
        .expect("document")
        .apply(EditorOp::Insert("x".into()))
        .expect("insert");
      other
    };
    writer.sweep(lock, true);
    assert!(file_of(other).is_file());
    lock().close(other).expect("close");
    writer.sweep(lock, true);
    assert!(!file_of(other).exists());

    writer.discard();
    assert!(!dir.exists());
  }

  #[test]
  fn large_documents_are_backed_up_less_often() {
    assert_eq!(interval_for(0), DEBOUNCE);
    assert!(interval_for(100 * 1024 * 1024) > Duration::from_secs(20));
  }

  #[test]
  fn lists_at_most_five_titles() {
    let titles: Vec<String> = (1..=7).map(|n| format!("f{n}")).collect();
    assert_eq!(list(&titles[..2]), "f1, f2");
    assert_eq!(list(&titles), "f1, f2, f3, f4, f5 and 2 more");
  }
}
