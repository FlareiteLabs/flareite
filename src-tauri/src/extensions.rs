//! WebAssembly extensions: connects the extension host to the editor, the
//! settings, and the frontend.
//!
//! Installed extensions live in `<config dir>/extensions/<name>/`;
//! development extensions are loaded in place from the folders listed in
//! `extensions.development` in the settings. The user's permission
//! decisions are kept in `<config dir>/extension-permissions.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use flareite_core::{Document, DocumentId, Editor};
use flareite_extension_host::icons::IconTheme;
use flareite_extension_host::layouts::{Fill, PaneDecorations, TabDocument, TabId};
use flareite_extension_host::preview::PreviewElement;
use flareite_extension_host::webview::WebviewInfo;
use flareite_extension_host::{
  Embedder, ExtensionHost, ExtensionMarks, ExtensionStatus, HostOptions, LayoutView, Limits,
  MANIFEST_FILE, Notification, OpenFolder, Permission, PreviewContent, PreviewInfo, PromptReply,
  StatusItem, ViewContent,
};
use serde::{Serialize, Serializer};
use tauri::{AppHandle, Manager, State, Window};

use crate::editor::EditorState;
use crate::editor::protocol::{CodeLensDto, DecorationDto, LineAnnotationDto};
use crate::editor::syntax::highlight_snippet;
use crate::error::CommandError;
use crate::events::{self, AppEvent};
use crate::languages::{self, LanguagesState};
use crate::lsp::{DiagnosticDto, DiagnosticMessageDto};
use crate::settings::{self, Settings, SettingsState};
use crate::workspace::WorkspaceState;

pub(crate) const EXTENSIONS_DIR: &str = "extensions";
const PERMISSIONS_FILE: &str = "extension-permissions.json";
/// Extensions' stored values and webview tabs' states, in the local data
/// directory.
const STORAGE_DIR: &str = "extension-storage";
/// Largest code block highlighted in a preview.
const MAX_HIGHLIGHTED_CODE_BYTES: usize = 64 * 1024;
/// Time spent parsing a preview's code blocks to highlight them, each time
/// its content is fetched.
const PREVIEW_HIGHLIGHT_BUDGET: Duration = Duration::from_millis(200);

/// The extension host, once the app is set up.
#[derive(Default)]
pub struct ExtensionsState(OnceLock<ExtensionHost>);

impl ExtensionsState {
  pub fn host(&self) -> Option<&ExtensionHost> {
    self.0.get()
  }

  /// Announces a new or changed document to extensions. Call with the
  /// editor locked.
  pub fn sync(&self, document: &Document) {
    if let Some(host) = self.host() {
      host.sync(document);
    }
  }

  pub fn closed(&self, id: DocumentId) {
    if let Some(host) = self.host() {
      host.closed(id);
    }
  }

  /// Adds extension diagnostics on `lines` to a view's, and returns the
  /// decorations there. Call with the editor locked.
  pub fn add_to_view(
    &self,
    document: &Document,
    lines: std::ops::Range<usize>,
    diagnostics: &mut Vec<DiagnosticDto>,
  ) -> Vec<DecorationDto> {
    let Some(host) = self.host() else {
      return Vec::new();
    };
    let buffer = document.buffer();
    let visible = buffer.line_start(lines.start)..buffer.line_end(lines.end.saturating_sub(1));
    let decorations = host
      .decorations(document, visible.clone())
      .into_iter()
      .map(|decoration| DecorationDto {
        start: buffer.offset_to_utf16(decoration.range.start).into(),
        end: buffer.offset_to_utf16(decoration.range.end).into(),
        key: decoration.key,
      })
      .collect();
    diagnostics.extend(
      host
        .diagnostics(document, visible)
        .into_iter()
        .map(|diagnostic| DiagnosticDto {
          start: buffer.offset_to_utf16(diagnostic.range.start).into(),
          end: buffer.offset_to_utf16(diagnostic.range.end).into(),
          severity: diagnostic.severity.number(),
        }),
    );
    decorations
  }

  /// Extensions' line annotations on `lines`. Call with the editor locked.
  pub fn annotations(
    &self,
    document: &Document,
    lines: std::ops::Range<usize>,
  ) -> Vec<LineAnnotationDto> {
    let Some(host) = self.host() else {
      return Vec::new();
    };
    let buffer = document.buffer();
    if lines.is_empty() {
      return Vec::new();
    }
    let bytes = buffer.line_start(lines.start)..buffer.line_start(lines.end - 1);
    host
      .line_annotations(document, bytes)
      .into_iter()
      .map(|annotation| LineAnnotationDto {
        line: buffer.line_of(annotation.offset),
        text: annotation.text,
        tooltip: annotation.tooltip,
      })
      .collect()
  }

  /// Extensions' code lenses in a document. Call with the editor locked.
  pub fn lenses(&self, document: &Document) -> Vec<CodeLensDto> {
    let Some(host) = self.host() else {
      return Vec::new();
    };
    let buffer = document.buffer();
    host
      .code_lenses(document)
      .into_iter()
      .map(|lens| CodeLensDto {
        line: buffer.line_of(lens.offset),
        title: lens.title,
        tooltip: lens.tooltip,
        command: lens.command,
        argument: lens.argument,
      })
      .collect()
  }

  /// Messages of extension diagnostics covering `offset`, for hover.
  pub fn messages_at(&self, document: &Document, offset: usize) -> Vec<DiagnosticMessageDto> {
    self.host().map_or_else(Vec::new, |host| {
      host
        .diagnostics_at(document, offset)
        .into_iter()
        .map(|diagnostic| DiagnosticMessageDto {
          severity: diagnostic.severity.number(),
          message: diagnostic.message,
          source: Some(diagnostic.source),
        })
        .collect()
    })
  }
}

/// Runs `work` on a thread of its own: for what notifications set off that
/// takes the editor's lock, which their caller may hold.
fn background(app: &AppHandle, name: &str, work: impl FnOnce(&AppHandle) + Send + 'static) {
  let app = app.clone();
  if let Err(error) = std::thread::Builder::new()
    .name(name.into())
    .spawn(move || work(&app))
  {
    tracing::error!(%error, "could not start a thread");
  }
}

struct AppEmbedder {
  app: AppHandle,
}

impl Embedder for AppEmbedder {
  fn with_editor(&self, f: &mut dyn FnMut(&mut Editor)) {
    let state = self.app.state::<EditorState>();
    let mut editor = state.lock();
    f(&mut editor);
  }

  fn language_of(&self, path: &Path) -> Option<String> {
    self
      .app
      .state::<LanguagesState>()
      .for_path(path)
      .map(|language| language.id().to_owned())
  }

  fn open_folder(&self) -> Option<OpenFolder> {
    let root = self.app.state::<WorkspaceState>().root()?;
    Some(OpenFolder {
      trusted: crate::trust::trust_of(&self.app, &root) == crate::trust::Trust::Trusted,
      root,
      exclude: self.app.state::<SettingsState>().get().exclude_filter(),
    })
  }

  fn save(&self, document: DocumentId) -> Result<(), String> {
    crate::editor::save_quietly(&self.app, document)
  }

  fn askpass_program(&self) -> Option<PathBuf> {
    // This executable, which answers as the helper (see `main.rs`).
    std::env::current_exe().ok()
  }

  fn open_externally(&self, path: &Path) -> Result<(), String> {
    open::that_detached(path).map_err(|error| error.to_string())
  }

  fn download(&self, address: &str) -> Result<PathBuf, String> {
    self
      .app
      .state::<crate::downloads::DownloadsState>()
      .get()
      .ok_or("downloads aren't set up")?
      .file(address, crate::downloads::MAX_AGE)
  }

  fn secret(&self, account: &str) -> Result<Option<String>, String> {
    crate::secrets::get(&self.app.config().identifier, account)
  }

  fn set_secret(&self, account: &str, value: Option<&str>) -> Result<(), String> {
    crate::secrets::set(&self.app.config().identifier, account, value)
  }

  fn set_setting(
    &self,
    extension: &str,
    key: &str,
    value: Option<toml::Value>,
  ) -> Result<(), String> {
    settings::set_extension_setting(&self.app, extension, key, value).map_err(|error| error.message)
  }

  // Called with the editor lock possibly held, so this only emits events.
  fn notify(&self, notification: Notification) {
    let event = match notification {
      Notification::Message {
        id,
        extension,
        level,
        message,
      } => AppEvent::ExtensionMessage {
        id,
        extension,
        level,
        message,
      },
      Notification::DocumentEdited(id) => {
        self.app.state::<crate::backups::BackupState>().touch();
        AppEvent::DocumentEdited {
          document_id: id.get(),
        }
      }
      Notification::DiagnosticsChanged(id) => AppEvent::DiagnosticsChanged {
        document_id: id.get(),
      },
      Notification::ExtensionsChanged => {
        // Their languages and language servers may have changed, and the
        // originals of open files.
        languages::refresh(&self.app);
        // Not here: it locks the editor, which the caller may hold (an
        // extension starting for a document being opened).
        background(&self.app, "quick-diff-track", |app| {
          crate::editor::quick_diff::track_open(app, &[]);
        });
        // Their schemas, and the settings they declare.
        crate::schemas::extensions_changed(&self.app);
        AppEvent::ExtensionsChanged
      }
      // Only the Extensions view shows it; the new build's load says
      // ExtensionsChanged itself.
      Notification::BuildChanged => AppEvent::ExtensionsChanged,
      Notification::ExtensionFailed { id, name, reason } => {
        AppEvent::ExtensionFailed { id, name, reason }
      }
      Notification::StatusItemsChanged => AppEvent::ExtensionStatusItemsChanged,
      Notification::ViewChanged(view) => AppEvent::ExtensionViewChanged { view },
      Notification::ShowView(view) => AppEvent::ExtensionShowView { view },
      Notification::DecorationsChanged(id) => AppEvent::DecorationsChanged {
        document_id: id.get(),
      },
      Notification::Reveal {
        document,
        start,
        end,
      } => AppEvent::ExtensionReveal {
        document_id: document.get(),
        start: start.into(),
        end: end.into(),
      },
      Notification::PreviewsChanged => AppEvent::ExtensionPreviewsChanged,
      Notification::PreviewChanged(preview) => AppEvent::ExtensionPreviewChanged { preview },
      Notification::ShowPreview { id, beside } => AppEvent::ExtensionShowPreview {
        preview: id,
        beside,
      },
      Notification::WebviewChanged(id) => AppEvent::ExtensionWebviewChanged { preview: id },
      Notification::WebviewMessage { id, message } => AppEvent::ExtensionWebviewMessage {
        preview: id,
        message,
      },
      Notification::StorageChanged {
        extension,
        scope,
        key,
        value,
        source,
      } => AppEvent::ExtensionStorageChanged {
        extension,
        scope,
        key,
        value,
        source,
      },
      Notification::FileMarksChanged => AppEvent::ExtensionFileMarksChanged,
      // Recomputes the servers, as extensions changed theirs.
      Notification::LanguageServersChanged => {
        languages::refresh(&self.app);
        return;
      }
      Notification::RestartLanguageServer { name } => {
        background(&self.app, "lsp-restart", move |app| {
          app.state::<crate::lsp::LspState>().restart(app, &name);
        });
        return;
      }
      Notification::SchemasChanged => {
        crate::schemas::extensions_changed(&self.app);
        return;
      }
      Notification::OpenFile { path } => AppEvent::OpenFile {
        path: path.display().to_string(),
        active: true,
      },
      Notification::OpenFolder { path } => AppEvent::OpenFolder {
        path: path.display().to_string(),
      },
      Notification::Prompt(prompt) => AppEvent::ExtensionPrompt { prompt },
      Notification::PromptClosed(id) => AppEvent::ExtensionPromptClosed { id },
      Notification::TrustRequested { extension } => AppEvent::WorkspaceTrustRequested { extension },
      Notification::ProvidedChanged(uris) => {
        crate::editor::refresh_provided(&self.app, uris);
        return;
      }
      Notification::OpenProvided(uri) => AppEvent::OpenProvided { uri },
      Notification::OriginalsChanged(paths) => {
        crate::editor::quick_diff::track_open(&self.app, &paths);
        return;
      }
      Notification::ApplySelectedRanges => AppEvent::ApplySelectedRanges,
      Notification::OpenLinkDefault(link) => AppEvent::OpenLinkDefault { link },
      Notification::WebPageAction { id, action } => {
        crate::web_pages::act(&self.app, &id, action);
        return;
      }
      Notification::OpenLayout { tab, beside } => AppEvent::OpenLayout {
        layout: tab.layout,
        key: tab.key,
        beside,
      },
      Notification::LayoutChanged(tab) => AppEvent::LayoutChanged {
        layout: tab.layout,
        key: tab.key,
      },
      Notification::CloseLayout(tab) => AppEvent::CloseLayout {
        layout: tab.layout,
        key: tab.key,
      },
      Notification::PaneDecorationsChanged { tab, pane } => AppEvent::PaneDecorationsChanged {
        layout: tab.layout,
        key: tab.key,
        pane,
      },
      Notification::RevealInPane {
        tab,
        pane,
        start,
        end,
      } => AppEvent::RevealInPane {
        layout: tab.layout,
        key: tab.key,
        pane,
        start,
        end,
      },
    };
    events::emit(&self.app, event);
  }
}

pub(crate) fn config_dir(app: &AppHandle) -> Option<PathBuf> {
  app.path().app_config_dir().ok()
}

fn options(app: &AppHandle, settings: &Settings) -> HostOptions {
  let editor_version = semver::Version::parse(&app.package_info().version.to_string())
    .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
  let enabled = settings.extensions.enabled;
  let dir = config_dir(app);
  HostOptions {
    editor_version,
    extensions_dir: dir
      .as_ref()
      .filter(|_| enabled)
      .map(|dir| dir.join(EXTENSIONS_DIR)),
    development: if enabled {
      settings
        .extensions
        .development
        .iter()
        .map(PathBuf::from)
        .collect()
    } else {
      Vec::new()
    },
    builds: true,
    permissions_file: dir.map(|dir| dir.join(PERMISSIONS_FILE)),
    settings: settings.extensions.settings.clone(),
    programs: settings.extensions.programs.clone(),
    limits: Limits::default(),
    storage_dir: app
      .path()
      .app_local_data_dir()
      .ok()
      .map(|dir| dir.join(STORAGE_DIR)),
  }
}

/// Creates the host and loads extensions in the background.
pub fn init(app: &AppHandle, load: bool) {
  let settings = app.state::<SettingsState>().get();
  let mut options = options(app, &settings);
  let bench = app.state::<crate::bench::BenchState>();
  if bench.active() && !bench.with_extensions() {
    // Only the language extensions the benchmark asks for.
    options.extensions_dir = None;
    options.permissions_file = None;
    options.development = bench.languages().to_vec();
    options.builds = false;
    options.settings.clear();
    options.storage_dir = None;
  }
  let host = ExtensionHost::new(
    std::sync::Arc::new(AppEmbedder { app: app.clone() }),
    options,
  );
  let state = app.state::<ExtensionsState>();
  if state.0.set(host).is_err() || !load {
    return;
  }
  // Documents opened before the host existed.
  {
    let editor_state = app.state::<EditorState>();
    let editor = editor_state.lock();
    for document in editor.documents() {
      state.sync(document);
    }
  }
  let app = app.clone();
  let spawned = std::thread::Builder::new()
    .name("extensions-load".into())
    .spawn(move || {
      if let Some(host) = app.state::<ExtensionsState>().host() {
        host.load();
      }
    });
  if let Err(error) = spawned {
    tracing::error!(%error, "could not load extensions");
  }
}

/// Applies changed extension settings.
pub fn settings_changed(app: &AppHandle, previous: &Settings, settings: &Settings) {
  // The catalog is the Extensions view's; the host doesn't use it.
  let unchanged = settings::ExtensionsSettings {
    catalog: settings.extensions.catalog.clone(),
    ..previous.extensions.clone()
  } == settings.extensions;
  if unchanged {
    return;
  }
  if let Some(host) = app.state::<ExtensionsState>().host() {
    host.configure(options(app, settings));
  }
}

pub fn shutdown(app: &AppHandle) {
  if let Some(host) = app.state::<ExtensionsState>().host() {
    host.shutdown();
  }
}

fn host<'a>(state: &'a State<'_, ExtensionsState>) -> Result<&'a ExtensionHost, CommandError> {
  state
    .host()
    .ok_or_else(|| CommandError::new("extensions", "Extensions are not available."))
}

#[tauri::command]
pub fn extensions_list(state: State<'_, ExtensionsState>) -> Vec<ExtensionStatus> {
  state.host().map_or_else(Vec::new, ExtensionHost::statuses)
}

/// Runs an extension's command for the active document and waits for it.
/// `argument` comes from a view item or menu.
#[tauri::command]
pub async fn extension_run_command(
  app: AppHandle,
  command: String,
  document_id: Option<u64>,
  argument: Option<String>,
) -> Result<(), CommandError> {
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .run_command(&command, document_id.map(DocumentId::from_raw), argument)
      .map_err(|message| CommandError::new("extension", message))?
  };
  tauri::async_runtime::spawn_blocking(move || ticket.wait())
    .await
    .map_err(CommandError::internal)?
    .map_err(|message| CommandError::new("extension", message))
}

/// Opens a link with an extension's link handler (`handler`, its full id),
/// and waits.
#[tauri::command]
pub async fn extension_open_link(
  app: AppHandle,
  handler: String,
  link: flareite_extension_host::links::LinkRequest,
) -> Result<(), CommandError> {
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .open_link(&handler, link)
      .map_err(|message| CommandError::new("extension", message))?
  };
  wait(ticket).await
}

/// What a layout tab shows; `None` if no tab is open with that id (the
/// workbench then fills it in, `extension_layout_fill`).
#[tauri::command]
pub fn extension_layout(
  state: State<'_, ExtensionsState>,
  layout: String,
  key: String,
) -> Option<LayoutView> {
  state.host()?.layout(&TabId { layout, key })
}

/// What an editor pane of a layout tab shows besides its text.
#[tauri::command]
pub fn extension_pane_decorations(
  state: State<'_, ExtensionsState>,
  layout: String,
  key: String,
  pane: String,
) -> Option<PaneDecorations> {
  state
    .host()?
    .pane_decorations(&TabId { layout, key }, &pane)
    .map(|decorations| (*decorations).clone())
}

/// Runs a layout's command to fill a tab in (`opened`, `restored`) or say
/// it crossed a breakpoint (`resized`), and waits.
#[tauri::command]
pub async fn extension_layout_fill(
  app: AppHandle,
  layout: String,
  key: String,
  reason: String,
  width: u32,
) -> Result<(), CommandError> {
  let reason = Fill::parse(&reason)
    .filter(|reason| matches!(reason, Fill::Opened | Fill::Restored | Fill::Resized))
    .ok_or_else(|| CommandError::new("invalidArgument", format!("no reason {reason:?}")))?;
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .fill_layout(&TabId { layout, key }, reason, width)
      .map_err(|message| CommandError::new("extension", message))?
  };
  wait(ticket).await
}

/// One of a layout tab's documents, open in the editor.
#[derive(Debug, serde::Deserialize)]
pub struct TabDocumentDto {
  uri: String,
  document: u64,
}

/// The workbench opened a layout tab's documents: tells the extension
/// (`ready`) if they're new to it, and waits.
#[tauri::command]
pub async fn extension_layout_ready(
  app: AppHandle,
  layout: String,
  key: String,
  documents: Vec<TabDocumentDto>,
) -> Result<(), CommandError> {
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .layout_ready(
        &TabId { layout, key },
        documents
          .into_iter()
          .map(|document| TabDocument {
            uri: document.uri,
            document: document.document,
          })
          .collect(),
      )
      .map_err(|message| CommandError::new("extension", message))?
  };
  match ticket {
    Some(ticket) => wait(ticket).await,
    None => Ok(()),
  }
}

/// A layout tab closed.
#[tauri::command]
pub fn extension_layout_closed(state: State<'_, ExtensionsState>, layout: String, key: String) {
  if let Some(host) = state.host() {
    host.layout_closed(&TabId { layout, key });
  }
}

/// The editor pane in use, when it's a layout tab's (`None`: it isn't).
#[tauri::command]
pub fn extension_layout_focus(
  state: State<'_, ExtensionsState>,
  layout: Option<String>,
  key: Option<String>,
  pane: Option<String>,
) {
  if let Some(host) = state.host() {
    host.set_active_pane(match (layout, key, pane) {
      (Some(layout), Some(key), Some(pane)) => Some((TabId { layout, key }, pane)),
      _ => None,
    });
  }
}

/// The user clicked a layout tab's button, a pane's header button, or a
/// zone (`pane`): runs its command with its argument, and waits.
#[tauri::command]
pub async fn extension_layout_command(
  app: AppHandle,
  layout: String,
  key: String,
  pane: Option<String>,
  command: String,
  argument: Option<String>,
  document_id: Option<u64>,
) -> Result<(), CommandError> {
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .run_layout_command(
        &TabId { layout, key },
        pane,
        &command,
        document_id.map(DocumentId::from_raw),
        argument,
      )
      .map_err(|message| CommandError::new("extension", message))?
  };
  wait(ticket).await
}

/// A file icon theme with its icons, or `None` if no enabled extension
/// contributes it.
#[tauri::command]
pub fn extensions_icon_theme(state: State<'_, ExtensionsState>, id: String) -> Option<IconTheme> {
  state
    .host()
    .and_then(|host| host.icon_theme(&id))
    .map(|theme| (*theme).clone())
}

/// Runs a command from a view's menu or button for the selected items
/// (their arguments) and waits for it. `argument` is the clicked item's.
#[tauri::command]
pub async fn extension_run_view_command(
  app: AppHandle,
  command: String,
  argument: Option<String>,
  selection: Vec<String>,
) -> Result<(), CommandError> {
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .run_view_command(&command, argument, selection)
      .map_err(|message| CommandError::new("extension", message))?
  };
  wait(ticket).await
}

/// The user clicked a code lens in document `document`: runs its command
/// with its argument, and waits.
#[tauri::command]
pub async fn extension_run_code_lens(
  app: AppHandle,
  document: u64,
  command: String,
  argument: Option<String>,
) -> Result<(), CommandError> {
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .run_command(&command, Some(DocumentId::from_raw(document)), argument)
      .map_err(|message| CommandError::new("extension", message))?
  };
  wait(ticket).await
}

/// The user opened a view item with no children listed: asks its
/// extension for them, and waits.
#[tauri::command]
pub async fn extension_view_expand(
  app: AppHandle,
  view: String,
  item: String,
) -> Result<(), CommandError> {
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .expand_view_item(&view, &item)
      .map_err(|message| CommandError::new("extension", message))?
  };
  wait(ticket).await
}

async fn wait(ticket: flareite_extension_host::CommandTicket) -> Result<(), CommandError> {
  tauri::async_runtime::spawn_blocking(move || ticket.wait())
    .await
    .map_err(CommandError::internal)?
    .map_err(|message| CommandError::new("extension", message))
}

/// The text in a view's text box changed.
#[tauri::command]
pub fn extension_view_input(state: State<'_, ExtensionsState>, view: String, text: String) {
  if let Some(host) = state.host() {
    host.set_view_input(&view, text);
  }
}

/// Views' badges, by full view id.
#[tauri::command]
pub fn extension_view_badges(state: State<'_, ExtensionsState>) -> BTreeMap<String, u32> {
  state
    .host()
    .map_or_else(BTreeMap::new, ExtensionHost::view_badges)
}

/// The user answered an extension's prompt, or closed it.
#[tauri::command]
pub fn extension_prompt_answer(state: State<'_, ExtensionsState>, id: u64, reply: PromptReply) {
  if let Some(host) = state.host() {
    host.prompt_answered(id, reply);
  }
}

/// The workbench's editor in use shows this file now (`None`: none).
#[tauri::command]
pub fn extension_active_file(state: State<'_, ExtensionsState>, path: Option<String>) {
  if let Some(host) = state.host() {
    host.active_file_changed(path.map(|path| crate::paths::normalize(Path::new(&path))));
  }
}

/// The active editor's cursor moved to another line, or another document
/// (or none) is in use.
#[tauri::command]
pub fn extension_selection_changed(state: State<'_, ExtensionsState>, document: Option<u64>) {
  if let Some(host) = state.host() {
    host.selection_changed(document.map(DocumentId::from_raw));
  }
}

/// Every running extension's explorer marks.
#[tauri::command]
pub fn extension_file_marks(state: State<'_, ExtensionsState>) -> Vec<ExtensionMarks> {
  state
    .host()
    .map_or_else(Vec::new, ExtensionHost::file_marks)
}

#[tauri::command]
pub fn extension_status_items(state: State<'_, ExtensionsState>) -> Vec<StatusItem> {
  state
    .host()
    .map_or_else(Vec::new, ExtensionHost::status_items)
}

#[tauri::command]
pub fn extension_view(state: State<'_, ExtensionsState>, view: String) -> ViewContent {
  state
    .host()
    .map_or_else(ViewContent::default, |host| host.view_content(&view))
}

/// Tells extensions a view is shown, starting its extension if needed.
#[tauri::command]
pub fn extension_view_opened(state: State<'_, ExtensionsState>, view: String) {
  if let Some(host) = state.host() {
    host.view_opened(&view);
  }
}

#[tauri::command]
pub fn extension_previews(state: State<'_, ExtensionsState>) -> Vec<PreviewInfo> {
  state.host().map_or_else(Vec::new, ExtensionHost::previews)
}

/// A preview's content, with its code blocks highlighted.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewContentDto {
  #[serde(serialize_with = "preview_nodes")]
  nodes: Arc<PreviewContent>,
  /// Each code block in a known language, by node index: its lines'
  /// highlights as flat UTF-16 `[start, end, kind]` triples, as in views.
  highlights: BTreeMap<u32, Vec<Vec<u32>>>,
  /// Whether code blocks in known languages were left plain because time
  /// ran out (e.g. compiling a grammar); fetching again highlights them.
  highlights_pending: bool,
}

fn preview_nodes<S: Serializer>(content: &Arc<PreviewContent>, s: S) -> Result<S::Ok, S::Error> {
  content.nodes.serialize(s)
}

/// A preview's content, or `None` if it isn't open. Code blocks in known
/// languages are highlighted, within a time budget; the rest show plain.
#[tauri::command]
pub async fn extension_preview_content(
  app: AppHandle,
  preview: String,
) -> Result<Option<PreviewContentDto>, CommandError> {
  tauri::async_runtime::spawn_blocking(move || {
    let content = app
      .state::<ExtensionsState>()
      .host()?
      .preview_content(&preview)?;
    Some(highlighted(&app, content))
  })
  .await
  .map_err(CommandError::internal)
}

/// Preview content with its code blocks in known languages highlighted,
/// within a time budget.
fn highlighted(app: &AppHandle, content: Arc<PreviewContent>) -> PreviewContentDto {
  let registry = app.state::<LanguagesState>().registry();
  let known = |language: &str| registry.for_name(&language.to_ascii_lowercase()).is_some();
  let started = Instant::now();
  let mut highlights = BTreeMap::new();
  let mut highlights_pending = false;
  for (index, node) in content.nodes.iter().enumerate() {
    let PreviewElement::CodeBlock {
      language: Some(language),
      text,
    } = &node.element
    else {
      continue;
    };
    if text.len() > MAX_HIGHLIGHTED_CODE_BYTES {
      continue;
    }
    let lines = PREVIEW_HIGHLIGHT_BUDGET
      .checked_sub(started.elapsed())
      .and_then(|budget| highlight_snippet(&registry, language, text, budget));
    match lines {
      Some(lines) => {
        highlights.insert(u32::try_from(index).unwrap_or(u32::MAX), lines);
      }
      None => highlights_pending |= known(language),
    }
  }
  PreviewContentDto {
    nodes: content,
    highlights,
    highlights_pending,
  }
}

/// An extension's icon as a `data:` URL; `None` if it has none.
#[tauri::command]
pub async fn extension_icon(app: AppHandle, id: String) -> Result<Option<String>, CommandError> {
  tauri::async_runtime::spawn_blocking(move || {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .icon(&id)
      .transpose()
      .map_err(|message| CommandError::new("extensionIcon", message))
  })
  .await
  .map_err(CommandError::internal)?
}

/// An extension's README as preview content; `None` if it has none.
#[tauri::command]
pub async fn extension_readme(
  app: AppHandle,
  id: String,
) -> Result<Option<PreviewContentDto>, CommandError> {
  tauri::async_runtime::spawn_blocking(move || {
    let state = app.state::<ExtensionsState>();
    let Some(readme) = host(&state)?.readme(&id) else {
      return Ok(None);
    };
    let content = readme.map_err(|message| CommandError::new("extensionReadme", message))?;
    Ok(Some(highlighted(&app, Arc::new(content))))
  })
  .await
  .map_err(CommandError::internal)?
}

/// A picture in an extension's README, as a `data:` URL.
#[tauri::command]
pub async fn extension_readme_image(
  app: AppHandle,
  id: String,
  path: String,
) -> Result<String, CommandError> {
  tauri::async_runtime::spawn_blocking(move || {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .readme_image(&id, Path::new(&path))
      .map_err(|message| CommandError::new("previewImage", message))
  })
  .await
  .map_err(CommandError::internal)?
}

/// Opens a file in an extension's viewer (`<extension id>.<viewer id>`), or
/// fills it in again, and waits for the extension to do so. Returns the
/// viewer's preview id, by which its content and pictures are fetched and
/// which `extension_preview_closed` closes.
#[tauri::command]
pub async fn extension_viewer_open(
  app: AppHandle,
  viewer: String,
  path: String,
) -> Result<String, CommandError> {
  let path = crate::paths::normalize(Path::new(&path));
  let ticket = {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .open_viewer(&viewer, &path)
      .map_err(|message| CommandError::new("extension", message))?
  };
  tauri::async_runtime::spawn_blocking(move || ticket.wait())
    .await
    .map_err(CommandError::internal)?
    .map_err(|message| CommandError::new("extension", message))?;
  Ok(flareite_extension_host::viewer_id(&viewer, &path))
}

/// Where a webview (viewer or preview) loads its page from; `None` if it
/// isn't open or isn't a webview.
#[tauri::command]
pub fn extension_webview(
  state: State<'_, ExtensionsState>,
  preview: String,
) -> Option<WebviewInfo> {
  state.host().and_then(|host| host.webview(&preview))
}

/// A webview's page sent a message (JSON text) to its extension. The
/// frontend checked that it came from that webview's frame.
#[tauri::command]
pub fn extension_webview_message(
  state: State<'_, ExtensionsState>,
  preview: String,
  message: String,
) -> Result<(), CommandError> {
  host(&state)?
    .webview_message(&preview, message)
    .map_err(|message| CommandError::new("extension", message))
}

/// The workbench's theme, as webview pages get it: `dark` or `light`, and
/// variables by name (without `--flareite-`).
#[tauri::command]
pub fn extension_webview_theme(
  state: State<'_, ExtensionsState>,
  kind: String,
  variables: BTreeMap<String, String>,
) -> Result<(), CommandError> {
  host(&state)?
    .set_webview_theme(&kind, variables)
    .map_err(|message| CommandError::new("extension", message))
}

/// A webview's page saved its state (JSON text).
#[tauri::command]
pub fn extension_webview_set_state(
  state: State<'_, ExtensionsState>,
  preview: String,
  page_state: String,
) -> Result<(), CommandError> {
  host(&state)?
    .set_webview_state(&preview, page_state)
    .map_err(|message| CommandError::new("extension", message))
}

/// A webview page's storage request: `get` a value (JSON text, or `null`),
/// `set` one (`value` `null` removes it), or list the `keys`, in the
/// `global` or `workspace` scope of the page's extension.
#[tauri::command]
pub fn extension_webview_storage(
  state: State<'_, ExtensionsState>,
  preview: String,
  scope: String,
  op: String,
  key: Option<String>,
  value: Option<String>,
) -> Result<serde_json::Value, CommandError> {
  let host = host(&state)?;
  let key = || key.clone().ok_or_else(|| "a key is needed".to_owned());
  let result = match op.as_str() {
    "get" => key()
      .and_then(|key| host.webview_storage_get(&preview, &scope, &key))
      .map(serde_json::Value::from),
    "set" => key()
      .and_then(|key| host.webview_storage_set(&preview, &scope, &key, value))
      .map(|()| serde_json::Value::Null),
    "keys" => host
      .webview_storage_keys(&preview, &scope)
      .map(serde_json::Value::from),
    other => Err(format!("no storage operation {other:?}")),
  };
  result.map_err(|message| CommandError::new("extensionStorage", message))
}

/// Removes everything an extension has stored.
#[tauri::command]
pub fn extension_clear_storage(
  state: State<'_, ExtensionsState>,
  id: String,
) -> Result<(), CommandError> {
  tracing::info!(extension = %id, "clearing extension storage");
  host(&state)?
    .clear_storage(&id)
    .map_err(|message| CommandError::new("extensionStorage", message))
}

/// Serves `flareite-webview:` requests: `/<token>/<path>` from a webview
/// page (see `flareite_extension_host::webview`).
pub fn serve_webview(
  app: &AppHandle,
  request: &tauri::http::Request<Vec<u8>>,
) -> tauri::http::Response<Vec<u8>> {
  use tauri::http::{Response, StatusCode, header};
  let path = request.uri().path().trim_start_matches('/');
  let (token, rest) = path.split_once('/').unwrap_or((path, ""));
  let resource = app
    .state::<ExtensionsState>()
    .host()
    .ok_or_else(|| "extensions are not available".to_owned())
    .and_then(|host| host.webview_resource(token, rest));
  let response = match resource {
    Ok(resource) => Response::builder()
      .status(StatusCode::OK)
      .header(header::CONTENT_TYPE, resource.mime)
      .header(header::CONTENT_SECURITY_POLICY, resource.csp)
      .body(resource.body),
    Err(message) => {
      tracing::debug!(%path, %message, "webview request refused");
      Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(message.into_bytes())
    }
  };
  // Pages have opaque origins, so reading their own files (`fetch`) is a
  // cross-origin request; the token in the address is what guards them.
  response
    .map(|mut response| {
      let headers = response.headers_mut();
      headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
      );
      headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
      );
      headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
      );
      response
    })
    .unwrap_or_else(|_| {
      let mut response = Response::new(Vec::new());
      *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
      response
    })
}

/// The user closed a preview's tab, or a viewer's.
#[tauri::command]
pub fn extension_preview_closed(state: State<'_, ExtensionsState>, preview: String) {
  if let Some(host) = state.host() {
    host.preview_closed(&preview);
  }
}

/// A picture a preview shows, as a `data:` URL.
#[tauri::command]
pub async fn extension_preview_image(
  app: AppHandle,
  preview: String,
  path: String,
) -> Result<String, CommandError> {
  tauri::async_runtime::spawn_blocking(move || {
    let state = app.state::<ExtensionsState>();
    host(&state)?
      .preview_image(&preview, Path::new(&path))
      .map_err(|message| CommandError::new("previewImage", message))
  })
  .await
  .map_err(CommandError::internal)?
}

#[tauri::command]
pub fn extension_set_permissions(
  state: State<'_, ExtensionsState>,
  id: String,
  granted: Vec<Permission>,
) -> Result<(), CommandError> {
  tracing::info!(extension = %id, ?granted, "extension permissions decided");
  host(&state)?.set_permissions(&id, &granted);
  Ok(())
}

#[tauri::command]
pub fn extension_set_enabled(
  state: State<'_, ExtensionsState>,
  id: String,
  enabled: bool,
) -> Result<(), CommandError> {
  host(&state)?.set_enabled(&id, enabled);
  Ok(())
}

#[tauri::command]
pub fn extension_restart(
  state: State<'_, ExtensionsState>,
  id: String,
) -> Result<(), CommandError> {
  host(&state)?.restart(&id);
  Ok(())
}

/// Rediscovers extensions and restarts them, e.g. after rebuilding one.
#[tauri::command]
pub async fn extensions_reload(app: AppHandle) -> Result<(), CommandError> {
  tauri::async_runtime::spawn_blocking(move || {
    let state = app.state::<ExtensionsState>();
    host(&state).map(ExtensionHost::load)
  })
  .await
  .map_err(CommandError::internal)?
}

/// Builds a development extension (`path`: its folder) with its manifest's
/// `[build]`, and loads the result. Its status shows how it went.
#[tauri::command]
pub fn extension_build(
  state: State<'_, ExtensionsState>,
  path: String,
) -> Result<(), CommandError> {
  if host(&state)?.build_development(Path::new(&path)) {
    Ok(())
  } else {
    Err(CommandError::new(
      "extensionBuild",
      "This extension has no [build] in its manifest, or isn't a development extension.",
    ))
  }
}

/// Asks for an extension folder and adds it to `extensions.development`
/// in the settings, which loads it. `None` if cancelled.
#[tauri::command]
pub async fn extension_add_development(
  app: AppHandle,
  window: Window,
) -> Result<Option<String>, CommandError> {
  let Some(folder) = crate::dialogs::pick_folder(&window).await else {
    return Ok(None);
  };
  if !folder.join(MANIFEST_FILE).is_file() {
    return Err(CommandError::new(
      "notAnExtension",
      format!("{} has no {MANIFEST_FILE}.", folder.display()),
    ));
  }
  let folder = crate::paths::normalize(&folder);
  let path = folder.to_string_lossy().into_owned();
  settings::edit(&app, |document| {
    if !document.contains_table("extensions") {
      document["extensions"] = toml_edit::table();
    }
    let list = document["extensions"]["development"]
      .or_insert(toml_edit::value(toml_edit::Array::new()))
      .as_array_mut()
      .ok_or_else(|| "extensions.development must be a list".to_owned())?;
    if !list.iter().any(|item| item.as_str() == Some(path.as_str())) {
      list.push(path.as_str());
    }
    Ok(())
  })?;
  Ok(Some(path))
}
