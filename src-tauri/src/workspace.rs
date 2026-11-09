//! The open workspace folder: explorer listings, creating files and folders
//! (the explorer's other changes are in `entries`), the Go to File index,
//! find in files, and reacting to changes on disk.
//!
//! Indexing and searching run on background threads and can be cancelled;
//! the editor stays responsive while they run. File-change notifications
//! update the index incrementally, refresh the explorer, and reload open
//! documents that have no unsaved changes.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use flareite_tasks::{Priority, TaskHandle};
use flareite_text::{SearchQuery, TextSnapshot};
use flareite_workspace::{
  DecodeOptions, DirEntry, EntryKind, FileIndex, FileMatches, FileStamp, SearchLimits,
  SearchSummary, WorkspaceWatcher, create_entry, list_directory, load_text_file_with, scan_files,
  search_workspace,
};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State, Window};

use crate::editor::EditorState;
use crate::editor::protocol::{DocumentSummary, SearchQueryDto};
use crate::entries::entry_error;
use crate::error::CommandError;
use crate::events::{self, AppEvent, WorkspaceInfo};
use crate::extensions::ExtensionsState;
use crate::lsp::LspState;
use crate::perf::Perf;
use crate::settings::SettingsState;
use crate::ui_state::UiStateStore;
use crate::{dialogs, paths, trust};

/// Most Go to File results returned.
const MAX_FILE_RESULTS: usize = 100;

struct Active {
  root: PathBuf,
  name: String,
  index: Arc<RwLock<FileIndex>>,
  indexing: Arc<AtomicBool>,
  /// Set to stop this workspace's background work when it is replaced.
  cancel: Arc<AtomicBool>,
  _watcher: Option<WorkspaceWatcher>,
}

impl Active {
  fn info(&self, app: &AppHandle) -> WorkspaceInfo {
    WorkspaceInfo {
      root: self.root.display().to_string(),
      name: self.name.clone(),
      trust: trust::trust_of(app, &self.root),
    }
  }
}

/// The open folder, and whether the user trusts it.
pub fn info(app: &AppHandle) -> Option<WorkspaceInfo> {
  app
    .state::<WorkspaceState>()
    .lock()
    .as_ref()
    .map(|active| active.info(app))
}

#[derive(Default)]
pub struct WorkspaceState {
  active: Mutex<Option<Active>>,
  /// The running find-in-files search, for cancellation.
  search: Mutex<Option<TaskHandle>>,
}

impl WorkspaceState {
  fn lock(&self) -> MutexGuard<'_, Option<Active>> {
    self.active.lock().unwrap_or_else(PoisonError::into_inner)
  }

  pub(crate) fn root(&self) -> Option<PathBuf> {
    self.lock().as_ref().map(|active| active.root.clone())
  }
}

/// Opens `path` as the workspace, replacing any previous one.
pub fn open(app: &AppHandle, path: &Path) -> Result<WorkspaceInfo, CommandError> {
  let root = paths::normalize(path);
  if !root.is_dir() {
    return Err(CommandError::new(
      "notADirectory",
      format!("{} is not a folder.", root.display()),
    ));
  }
  let name = root
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_else(|| root.display().to_string());

  let cancel = Arc::new(AtomicBool::new(false));
  let watcher = {
    let app = app.clone();
    let cancel = cancel.clone();
    WorkspaceWatcher::new(&root, move |changed| {
      if !cancel.load(Ordering::Relaxed) {
        on_files_changed(&app, changed);
      }
    })
    .inspect_err(|error| tracing::warn!(%error, "file watching unavailable"))
    .ok()
  };
  let active = Active {
    index: Arc::new(RwLock::new(FileIndex::default())),
    indexing: Arc::new(AtomicBool::new(true)),
    cancel,
    _watcher: watcher,
    root: root.clone(),
    name,
  };
  let info = active.info(app);
  start_indexing(app, &active);
  let previous = app.state::<WorkspaceState>().lock().replace(active);
  if let Some(previous) = &previous {
    previous.cancel.store(true, Ordering::Relaxed);
  }
  if previous.is_some_and(|previous| previous.root != root) {
    // Language servers are per workspace root.
    app.state::<LspState>().restart_all(app);
  }

  app
    .state::<UiStateStore>()
    .update(|state| state.workspace = Some(root.clone()));
  tracing::info!(root = %root.display(), trust = ?info.trust, "opened workspace");
  events::emit(
    app,
    AppEvent::WorkspaceChanged {
      workspace: Some(info.clone()),
    },
  );
  if let Some(host) = app.state::<ExtensionsState>().host() {
    host.folder_changed();
  }
  Ok(info)
}

/// Rebuilds the index and refreshes the explorer after `files.exclude`
/// changed.
pub fn exclusions_changed(app: &AppHandle) {
  let root = app.state::<WorkspaceState>().root();
  if let Some(root) = root
    && let Err(error) = open(app, &root)
  {
    tracing::warn!(message = %error.message, "could not reopen workspace");
  }
}

/// Builds the Go to File index on the worker pool. Reopening a workspace
/// replaces (cancels) a still-running build.
fn start_indexing(app: &AppHandle, active: &Active) {
  let exclude = app.state::<SettingsState>().get().exclude_filter();
  let (root, index, indexing, cancel, task_app) = (
    active.root.clone(),
    active.index.clone(),
    active.indexing.clone(),
    active.cancel.clone(),
    app.clone(),
  );
  app.state::<Perf>().scheduler.spawn(
    "workspace-index",
    Priority::Normal,
    Some("workspace-index".into()),
    move |token| {
      let started = std::time::Instant::now();
      let built = scan_files(&root, &exclude, token.as_atomic());
      if token.is_cancelled() || cancel.load(Ordering::Relaxed) {
        return;
      }
      let (files, truncated) = (built.len(), built.is_truncated());
      *index.write().unwrap_or_else(PoisonError::into_inner) = built;
      indexing.store(false, Ordering::Relaxed);
      task_app
        .state::<Perf>()
        .record("workspace.index", started.elapsed());
      tracing::info!(
        files,
        truncated,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "indexed workspace"
      );
      events::emit(
        &task_app,
        AppEvent::IndexStatus {
          indexing: false,
          files,
          truncated,
        },
      );
    },
  );
}

/// Handles a batch of file-change notifications (on the watcher's thread).
fn on_files_changed(app: &AppHandle, changed: Vec<PathBuf>) {
  let exclude = app.state::<SettingsState>().get().exclude_filter();
  let (root, index) = {
    let workspace = app.state::<WorkspaceState>();
    let guard = workspace.lock();
    let Some(active) = guard.as_ref() else {
      return;
    };
    (active.root.clone(), active.index.clone())
  };

  index
    .write()
    .unwrap_or_else(PoisonError::into_inner)
    .update(&changed, &exclude);
  if let Some(host) = app.state::<ExtensionsState>().host() {
    host.files_changed(&changed);
  }

  let directories: BTreeSet<String> = changed
    .iter()
    .flat_map(|path| [Some(path.as_path()), path.parent()])
    .flatten()
    .filter(|path| path.starts_with(&root))
    .map(|path| path.display().to_string())
    .collect();
  events::emit(
    app,
    AppEvent::FilesChanged {
      directories: directories.into_iter().collect(),
    },
  );

  // Reloading reads files, so it runs on the worker pool rather than the
  // watcher's thread. A newer change to the same file replaces a queued reload.
  for path in changed {
    let task_app = app.clone();
    app.state::<Perf>().scheduler.spawn(
      "reload-changed-file",
      Priority::High,
      Some(format!("reload:{}", path.display())),
      move |_| reload_if_open(&task_app, &path),
    );
  }
}

/// Reloads an open document whose file changed on disk, unless the change
/// is Flareite's own save. Documents with unsaved changes are left alone and
/// the user is told.
fn reload_if_open(app: &AppHandle, path: &Path) {
  let editor_state = app.state::<EditorState>();
  let (id, dirty, stamp, encoding) = {
    let editor = editor_state.lock();
    let Some(id) = editor.find_by_path(path) else {
      return;
    };
    let Ok(document) = editor.document(id) else {
      return;
    };
    (
      id,
      document.is_dirty(),
      document.disk_stamp().cloned(),
      document.encoding(),
    )
  };
  let current = match FileStamp::read(path) {
    Ok(Some(current)) => current,
    // Deleted or unreadable: keep the document as it is.
    _ => return,
  };
  if stamp.as_ref() == Some(&current) {
    return;
  }
  if dirty {
    let editor = editor_state.lock();
    if let Ok(document) = editor.document(id) {
      tracing::info!(document = %id, "open document with unsaved changes changed on disk");
      events::emit(
        app,
        AppEvent::DocumentChangedOnDisk {
          document: DocumentSummary::of(document),
        },
      );
    }
    return;
  }
  // In the encoding it has (which the user may have chosen), else whatever
  // the new contents are in. It's open already, so binary or not.
  let read = |encoding| {
    load_text_file_with(
      path,
      DecodeOptions {
        encoding,
        binary: true,
      },
    )
  };
  let loaded = match read(Some(encoding)).or_else(|_| read(None)) {
    Ok(loaded) => loaded,
    Err(error) => {
      tracing::warn!(%error, "could not reload changed file");
      return;
    }
  };
  let mut editor = editor_state.lock();
  let Ok(document) = editor.document_mut(id) else {
    return;
  };
  // Edited while the file was being read: leave it for the user.
  if document.is_dirty() {
    return;
  }
  match document.reload(&loaded.text, loaded.encoding, loaded.stamp) {
    Ok(_) => {
      tracing::info!(document = %id, "reloaded document changed on disk");
      events::emit(
        app,
        AppEvent::DocumentReloaded {
          document: DocumentSummary::of(document),
        },
      );
    }
    Err(error) => tracing::error!(%error, "reload failed"),
  }
}

/// Reopens the workspace saved from the previous session, if it still exists.
pub fn restore(app: &AppHandle) {
  if let Some(root) = app.state::<UiStateStore>().get().workspace
    && root.is_dir()
    && let Err(error) = open(app, &root)
  {
    tracing::warn!(message = %error.message, "could not restore workspace");
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDto {
  pub name: String,
  pub path: String,
  pub directory: bool,
  pub ignored: bool,
}

impl From<DirEntry> for EntryDto {
  fn from(entry: DirEntry) -> Self {
    Self {
      name: entry.name,
      path: entry.path.display().to_string(),
      directory: entry.kind == EntryKind::Directory,
      ignored: entry.ignored,
    }
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedDto {
  pub path: String,
  /// Folders whose listings gained an entry, outermost first.
  pub directories: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResult {
  pub path: String,
  pub relative: String,
  pub indices: Vec<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindFilesResult {
  pub files: Vec<FileResult>,
  pub indexing: bool,
  pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchDto {
  pub line: usize,
  pub start_column: usize,
  pub end_column: usize,
  pub preview: String,
  pub preview_start: usize,
  pub preview_end: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
  tag = "type",
  rename_all = "camelCase",
  rename_all_fields = "camelCase"
)]
pub enum SearchEvent {
  File {
    path: String,
    relative: String,
    matches: Vec<MatchDto>,
  },
  Done {
    files_searched: usize,
    files_with_matches: usize,
    matches: usize,
    truncated: bool,
    cancelled: bool,
  },
}

impl From<FileMatches> for SearchEvent {
  fn from(file: FileMatches) -> Self {
    Self::File {
      path: file.path.display().to_string(),
      relative: file.relative,
      matches: file
        .matches
        .into_iter()
        .map(|found| MatchDto {
          line: found.line,
          start_column: found.start_column,
          end_column: found.end_column,
          preview: found.preview,
          preview_start: found.preview_start,
          preview_end: found.preview_end,
        })
        .collect(),
    }
  }
}

impl From<SearchSummary> for SearchEvent {
  fn from(summary: SearchSummary) -> Self {
    Self::Done {
      files_searched: summary.files_searched,
      files_with_matches: summary.files_with_matches,
      matches: summary.matches,
      truncated: summary.truncated,
      cancelled: summary.cancelled,
    }
  }
}

#[tauri::command]
pub fn workspace_get(app: AppHandle) -> Option<WorkspaceInfo> {
  info(&app)
}

#[tauri::command]
pub fn workspace_open(app: AppHandle, path: String) -> Result<WorkspaceInfo, CommandError> {
  open(&app, Path::new(&path))
}

/// Shows a folder picker and opens the chosen folder. `None` if cancelled.
#[tauri::command]
pub async fn workspace_open_dialog(
  app: AppHandle,
  window: Window,
) -> Result<Option<WorkspaceInfo>, CommandError> {
  match dialogs::pick_folder(&window).await {
    Some(path) => open(&app, &path).map(Some),
    None => Ok(None),
  }
}

#[tauri::command]
pub fn workspace_close(app: AppHandle, state: State<'_, WorkspaceState>) {
  let previous = state.lock().take();
  if let Some(previous) = previous {
    previous.cancel.store(true, Ordering::Relaxed);
    tracing::info!(root = %previous.root.display(), "closed workspace");
    app.state::<LspState>().restart_all(&app);
    if let Some(host) = app.state::<ExtensionsState>().host() {
      host.folder_changed();
    }
  }
  app.state::<UiStateStore>().update(|ui| ui.workspace = None);
  events::emit(&app, AppEvent::WorkspaceChanged { workspace: None });
}

/// Lists a directory inside the workspace for the explorer.
#[tauri::command]
pub async fn workspace_list_dir(
  app: AppHandle,
  path: String,
) -> Result<Vec<EntryDto>, CommandError> {
  let root = app
    .state::<WorkspaceState>()
    .root()
    .ok_or_else(|| CommandError::new("noWorkspace", "No folder is open."))?;
  let dir = PathBuf::from(path);
  if !dir.starts_with(&root) {
    return Err(CommandError::new(
      "outsideWorkspace",
      "Only folders inside the workspace can be listed.",
    ));
  }
  let exclude = app.state::<SettingsState>().get().exclude_filter();
  let entries = tauri::async_runtime::spawn_blocking(move || list_directory(&root, &dir, &exclude))
    .await
    .map_err(CommandError::internal)??;
  Ok(entries.into_iter().map(EntryDto::from).collect())
}

/// Creates an empty file or a folder called `name` (which may include
/// subfolders) in `parent`, a folder inside the workspace.
#[tauri::command]
pub async fn workspace_create(
  app: AppHandle,
  parent: String,
  name: String,
  directory: bool,
) -> Result<CreatedDto, CommandError> {
  let root = app
    .state::<WorkspaceState>()
    .root()
    .ok_or_else(|| CommandError::new("noWorkspace", "No folder is open."))?;
  let parent = PathBuf::from(parent);
  if !parent.starts_with(&root) {
    return Err(CommandError::new(
      "outsideWorkspace",
      "Files can only be created inside the workspace.",
    ));
  }
  let kind = if directory {
    EntryKind::Directory
  } else {
    EntryKind::File
  };
  let created = tauri::async_runtime::spawn_blocking(move || create_entry(&parent, &name, kind))
    .await
    .map_err(CommandError::internal)?
    .map_err(entry_error)?;
  tracing::info!(path = %created.path.display(), directory, "created from the explorer");
  Ok(CreatedDto {
    path: created.path.display().to_string(),
    directories: created
      .directories
      .iter()
      .map(|dir| dir.display().to_string())
      .collect(),
  })
}

/// Fuzzy-matches workspace files by path for Go to File.
#[tauri::command]
pub async fn workspace_find_files(
  app: AppHandle,
  query: String,
) -> Result<FindFilesResult, CommandError> {
  let Some((root, index, indexing)) = app.state::<WorkspaceState>().lock().as_ref().map(|active| {
    (
      active.root.clone(),
      active.index.clone(),
      active.indexing.load(Ordering::Relaxed),
    )
  }) else {
    return Ok(FindFilesResult {
      files: Vec::new(),
      indexing: false,
      truncated: false,
    });
  };
  tauri::async_runtime::spawn_blocking(move || {
    let index = index.read().unwrap_or_else(PoisonError::into_inner);
    let files = index
      .query(&query, MAX_FILE_RESULTS)
      .into_iter()
      .map(|found| FileResult {
        path: root.join(&found.relative).display().to_string(),
        relative: found.relative,
        indices: found.indices,
      })
      .collect();
    FindFilesResult {
      files,
      indexing,
      truncated: index.is_truncated(),
    }
  })
  .await
  .map_err(CommandError::internal)
}

/// Searches workspace files, streaming results file by file through
/// `channel` and finishing with a `done` event. Starting a search cancels
/// the previous one.
#[tauri::command]
pub fn workspace_search(
  app: AppHandle,
  query: SearchQueryDto,
  channel: Channel<SearchEvent>,
) -> Result<(), CommandError> {
  let searcher = SearchQuery::from(query)
    .compile()
    .map_err(|error| CommandError::new("invalidSearch", error.to_string()))?;
  let state = app.state::<WorkspaceState>();
  let root = state
    .root()
    .ok_or_else(|| CommandError::new("noWorkspace", "No folder is open."))?;
  let settings = app.state::<SettingsState>().get();
  let exclude = settings.exclude_filter();
  let limits = SearchLimits {
    max_file_bytes: settings.search.max_file_bytes,
    ..SearchLimits::default()
  };
  let open: HashMap<PathBuf, TextSnapshot> = app
    .state::<EditorState>()
    .lock()
    .documents()
    .filter_map(|document| Some((document.path()?.to_path_buf(), document.buffer().snapshot())))
    .collect();

  // Runs on the worker pool; a new search replaces (cancels) the previous one.
  let task_app = app.clone();
  let handle = app.state::<Perf>().scheduler.spawn(
    "workspace-search",
    Priority::High,
    Some("workspace-search".into()),
    move |token| {
      let started = std::time::Instant::now();
      let summary = search_workspace(
        &root,
        &exclude,
        &searcher,
        &open,
        limits,
        token.as_atomic(),
        |file| {
          if channel.send(SearchEvent::from(file)).is_err() {
            token.cancel();
          }
        },
      );
      task_app
        .state::<Perf>()
        .record("workspace.search", started.elapsed());
      tracing::debug!(
        matches = summary.matches,
        files = summary.files_searched,
        cancelled = summary.cancelled,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "workspace search finished"
      );
      let _ = channel.send(SearchEvent::from(summary));
    },
  );
  *state.search.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
  Ok(())
}

#[tauri::command]
pub fn workspace_search_cancel(state: State<'_, WorkspaceState>) {
  if let Some(search) = state
    .search
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .take()
  {
    search.cancel();
  }
}
