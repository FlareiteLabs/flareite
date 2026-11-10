//! The host functions extensions call (the `host` interface).
//!
//! Every call is checked: permissions first, then sizes, then document
//! identity, version, and ranges, all before anything is changed. Edits
//! become one validated, versioned change in the editor core; nothing an
//! extension sends can reach the document storage any other way.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::Range as Span;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Instant;

use flareite_core::{Document, DocumentId, Editor, LanguageChoice};
use flareite_text::{Edit, TextBuffer};

use crate::bindings::flareite::extension::host::Host;
use crate::bindings::flareite::extension::storage::{Host as StorageHost, StorageScope};
use crate::bindings::flareite::extension::types::{
  self, ApiError, ConfigValue as WitConfigValue, Diagnostic, DocumentInfo, LineSlice, LogLevel,
  MessageLevel, Permission as WitPermission, Position, PreviewNode, PreviewOptions, Range,
  StatusItem, TextEdit, TextSlice, ViewItem, Webview, WebviewMessage,
};
use crate::bindings::flareite::extension::viewers::{FileBytes, Host as ViewersHost};
use crate::bindings::flareite::extension::webviews::Host as WebviewsHost;
use crate::bindings::flareite::extension::workbench::{
  CodeLens, Host as WorkbenchHost, LineAnnotation,
};
use crate::config::ConfigValue;
use crate::host::{Shared, StoredDiagnostic, StoredLens};
use crate::manifest::Manifest;
use crate::permissions::Permission;
use crate::preview::{self, MAX_KEY_BYTES, MAX_TITLE_CHARS, PreviewInfo};
use crate::processes::ProcessTable;
use crate::runtime::StoreState;
use crate::storage::StorageError;
use crate::worker::{Payload, Setup};
use crate::{Embedder, Notification, Severity, StatusContent, ViewEntry};

/// Most lines returned by one `read-lines` call.
pub const MAX_READ_LINES: usize = 10_000;
/// Most bytes returned by one read.
pub const MAX_READ_BYTES: usize = 4 * 1024 * 1024;
/// Most edits in one `apply-edits` call, and their total inserted text.
pub const MAX_EDITS: usize = 10_000;
pub const MAX_EDIT_BYTES: usize = 16 * 1024 * 1024;
/// Most diagnostics in one `publish-diagnostics` call.
pub const MAX_DIAGNOSTICS: usize = 5_000;
const MAX_DIAGNOSTIC_MESSAGE_CHARS: usize = 2_000;
const MAX_CODE_CHARS: usize = 64;
const MAX_LOG_CHARS: usize = 4_000;
const MAX_MESSAGE_CHARS: usize = 1_000;

/// Allows `burst` events at once and `per_second` on average after that.
struct RateLimit {
  tokens: f64,
  burst: f64,
  per_second: f64,
  last: Instant,
  dropped: u64,
}

impl RateLimit {
  fn new(burst: f64, per_second: f64) -> Self {
    Self {
      tokens: burst,
      burst,
      per_second,
      last: Instant::now(),
      dropped: 0,
    }
  }

  fn allow(&mut self) -> bool {
    let now = Instant::now();
    let elapsed = now.duration_since(self.last).as_secs_f64();
    self.last = now;
    self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
    if self.tokens >= 1.0 {
      self.tokens -= 1.0;
      true
    } else {
      self.dropped += 1;
      false
    }
  }

  /// Messages dropped since the last call.
  fn take_dropped(&mut self) -> u64 {
    std::mem::take(&mut self.dropped)
  }
}

/// What host functions know about the calling extension.
pub struct ApiContext {
  shared: Weak<Shared>,
  manifest: Arc<Manifest>,
  id: String,
  name: String,
  granted: BTreeSet<Permission>,
  config: Arc<RwLock<BTreeMap<String, ConfigValue>>>,
  logs: RateLimit,
  messages: RateLimit,
  /// Which start of the extension this is.
  generation: u64,
  /// Set when this start of the extension is stopped.
  cancel: Arc<AtomicBool>,
  processes: Arc<ProcessTable>,
  /// Downloads under way. (1.17)
  downloads: Arc<crate::downloads::DownloadTable>,
  /// The argument of the command being run.
  pub argument: Option<String>,
  /// What the running command was run for: a webview message, a finished
  /// program, or file changes.
  pub payload: Payload,
}

impl ApiContext {
  pub fn extension_id(&self) -> &str {
    &self.id
  }

  pub(crate) fn manifest(&self) -> &Manifest {
    &self.manifest
  }

  pub(crate) fn generation(&self) -> u64 {
    self.generation
  }

  pub(crate) fn processes(&self) -> Arc<ProcessTable> {
    self.processes.clone()
  }

  pub(crate) fn downloads(&self) -> Arc<crate::downloads::DownloadTable> {
    self.downloads.clone()
  }

  pub(crate) fn weak_shared(&self) -> Weak<Shared> {
    self.shared.clone()
  }
}

impl ApiContext {
  pub fn new(shared: Weak<Shared>, setup: &Setup) -> Self {
    Self {
      shared,
      manifest: setup.manifest.clone(),
      id: setup.manifest.id.clone(),
      name: setup.manifest.name.clone(),
      granted: setup.granted.clone(),
      config: setup.config.clone(),
      logs: RateLimit::new(200.0, 50.0),
      messages: RateLimit::new(10.0, 1.0),
      generation: setup.generation,
      cancel: setup.cancel.clone(),
      processes: setup.processes.clone(),
      downloads: Arc::default(),
      argument: None,
      payload: Payload::None,
    }
  }

  pub(crate) fn require(&self, permission: WitPermission) -> Result<Arc<Shared>, ApiError> {
    let needed = match permission {
      WitPermission::DocumentsRead => Permission::DocumentsRead,
      WitPermission::DocumentsWrite => Permission::DocumentsWrite,
    };
    if !self.granted.contains(&needed) {
      return Err(ApiError::PermissionDenied(permission));
    }
    self.shared()
  }

  pub(crate) fn has(&self, permission: Permission) -> bool {
    self.granted.contains(&permission)
  }

  /// The host's shared state, unless this start of the extension was
  /// stopped: what it showed is gone, and it mustn't show more over what the
  /// next start shows (it runs until its call is interrupted).
  pub(crate) fn shared(&self) -> Result<Arc<Shared>, ApiError> {
    if self.cancel.load(Ordering::Relaxed) {
      return Err(ApiError::InvalidArgument(
        "the extension was stopped".into(),
      ));
    }
    self
      .shared
      .upgrade()
      .ok_or_else(|| ApiError::InvalidArgument("the editor is closing".into()))
  }
}

fn cut(text: &str, max_chars: usize) -> String {
  match text.char_indices().nth(max_chars) {
    Some((end, _)) => format!("{}…", &text[..end]),
    None => text.to_owned(),
  }
}

fn to_u32(value: usize) -> u32 {
  u32::try_from(value).unwrap_or(u32::MAX)
}

fn to_u64(value: usize) -> u64 {
  u64::try_from(value).unwrap_or(u64::MAX)
}

/// Runs `f` with the editor locked.
pub fn with_editor<R>(shared: &Shared, f: impl FnOnce(&mut Editor) -> R) -> Option<R> {
  let mut f = Some(f);
  let mut result = None;
  shared.embedder.with_editor(&mut |editor| {
    if let Some(f) = f.take() {
      result = Some(f(editor));
    }
  });
  result
}

fn editor_unavailable() -> ApiError {
  ApiError::InvalidArgument("the editor is not available".into())
}

pub fn document_info(shared: &Shared, document: &Document) -> DocumentInfo {
  let buffer = document.buffer();
  DocumentInfo {
    id: document.id().get(),
    version: document.version().get(),
    path: document
      .path()
      .map(|path| path.to_string_lossy().into_owned()),
    language: language_of(
      shared.embedder.as_ref(),
      document.path(),
      document.language_choice(),
    ),
    line_count: to_u32(buffer.line_count()),
    length: to_u64(buffer.len()),
    dirty: document.is_dirty(),
  }
}

/// A document's language id: the one chosen for it, else its file's.
pub fn language_of(
  embedder: &dyn Embedder,
  path: Option<&Path>,
  choice: Option<&LanguageChoice>,
) -> Option<String> {
  match choice {
    Some(LanguageChoice::PlainText) => None,
    Some(LanguageChoice::Language(id)) => Some(id.clone()),
    None => path.and_then(|path| embedder.language_of(path)),
  }
}

pub fn to_position(buffer: &TextBuffer, offset: usize) -> Position {
  let point = buffer.point(offset);
  Position {
    line: to_u32(point.row),
    column: to_u32(point.column),
  }
}

/// The offset of `position`, which must be inside the document and on a
/// character boundary.
pub fn to_offset(buffer: &TextBuffer, position: Position) -> Result<usize, ApiError> {
  let line = position.line as usize;
  if line >= buffer.line_count() {
    return Err(ApiError::InvalidRange(format!(
      "line {} is past the end of the document ({} lines)",
      position.line,
      buffer.line_count()
    )));
  }
  let start = buffer.line_start(line);
  let length = buffer.line_end(line) - start;
  let column = position.column as usize;
  if column > length {
    return Err(ApiError::InvalidRange(format!(
      "column {column} is past the end of line {line} ({length} bytes)"
    )));
  }
  let offset = start + column;
  if !buffer.is_char_boundary(offset) {
    return Err(ApiError::InvalidRange(format!(
      "line {line} column {column} is inside a character"
    )));
  }
  Ok(offset)
}

pub fn to_span(buffer: &TextBuffer, range: Range) -> Result<Span<usize>, ApiError> {
  let start = to_offset(buffer, range.start)?;
  let end = to_offset(buffer, range.end)?;
  if end < start {
    return Err(ApiError::InvalidRange(
      "the range ends before it starts".into(),
    ));
  }
  Ok(start..end)
}

fn severity(severity: types::Severity) -> Severity {
  match severity {
    types::Severity::Error => Severity::Error,
    types::Severity::Warning => Severity::Warning,
    types::Severity::Information => Severity::Information,
    types::Severity::Hint => Severity::Hint,
  }
}

fn find(editor: &Editor, id: u64) -> Result<&Document, ApiError> {
  editor
    .document(DocumentId::from_raw(id))
    .map_err(|_| ApiError::UnknownDocument)
}

impl types::Host for StoreState {}

impl Host for StoreState {
  fn log(&mut self, level: LogLevel, message: String) {
    let api = &mut self.api;
    if !api.logs.allow() {
      return;
    }
    let dropped = api.logs.take_dropped();
    if dropped > 0 {
      tracing::warn!(extension = %api.id, dropped, "extension logged too much; messages dropped");
    }
    let message = cut(&message, MAX_LOG_CHARS);
    let id = api.id.as_str();
    match level {
      LogLevel::Trace => tracing::trace!(extension = id, "{message}"),
      LogLevel::Debug => tracing::debug!(extension = id, "{message}"),
      LogLevel::Info => tracing::info!(extension = id, "{message}"),
      LogLevel::Warn => tracing::warn!(extension = id, "{message}"),
      LogLevel::Error => tracing::error!(extension = id, "{message}"),
    }
  }

  fn show_message(&mut self, level: MessageLevel, message: String) {
    let api = &mut self.api;
    if !api.messages.allow() {
      tracing::debug!(extension = %api.id, "message dropped: too many messages");
      return;
    }
    let message = cut(&message, MAX_MESSAGE_CHARS);
    // Kept in its log too, where the user can find it after it's gone.
    match level {
      MessageLevel::Info => tracing::info!(extension = %api.id, "showed: {message}"),
      MessageLevel::Warning => tracing::warn!(extension = %api.id, "showed: {message}"),
      MessageLevel::Error => tracing::error!(extension = %api.id, "showed: {message}"),
    }
    if let Ok(shared) = api.shared() {
      shared.embedder.notify(Notification::Message {
        id: api.id.clone(),
        extension: api.name.clone(),
        level: match level {
          MessageLevel::Info => crate::MessageLevel::Info,
          MessageLevel::Warning => crate::MessageLevel::Warning,
          MessageLevel::Error => crate::MessageLevel::Error,
        },
        message,
      });
    }
  }

  fn configuration(&mut self, key: String) -> Option<WitConfigValue> {
    let config = self
      .api
      .config
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    config.get(&key).map(|value| match value {
      ConfigValue::Boolean(b) => WitConfigValue::Boolean(*b),
      ConfigValue::Integer(i) => WitConfigValue::Integer(*i),
      ConfigValue::Number(f) => WitConfigValue::Number(*f),
      ConfigValue::Text(s) => WitConfigValue::Text(s.clone()),
    })
  }

  fn documents(&mut self) -> Result<Vec<DocumentInfo>, ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    with_editor(&shared, |editor| {
      editor
        .documents()
        .map(|document| document_info(&shared, document))
        .collect()
    })
    .ok_or_else(editor_unavailable)
  }

  fn document(&mut self, id: u64) -> Result<DocumentInfo, ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    with_editor(&shared, |editor| {
      find(editor, id).map(|document| document_info(&shared, document))
    })
    .ok_or_else(editor_unavailable)?
  }

  fn read_lines(&mut self, id: u64, start: u32, end: u32) -> Result<LineSlice, ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    with_editor(&shared, |editor| {
      let document = find(editor, id)?;
      let buffer = document.buffer();
      let end = (end as usize).min(buffer.line_count());
      let mut lines = Vec::new();
      let mut bytes = 0;
      for line in (start as usize)..end {
        let span = buffer.line_start(line)..buffer.line_end(line);
        bytes += span.len();
        if lines.len() == MAX_READ_LINES || bytes > MAX_READ_BYTES {
          if lines.is_empty() {
            return Err(ApiError::TooLarge(format!(
              "line {line} is longer than {} MiB",
              MAX_READ_BYTES / 1024 / 1024
            )));
          }
          break;
        }
        lines.push(buffer.text_in(span));
      }
      Ok(LineSlice {
        version: document.version().get(),
        lines,
      })
    })
    .ok_or_else(editor_unavailable)?
  }

  fn read_range(&mut self, id: u64, range: Range) -> Result<TextSlice, ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    with_editor(&shared, |editor| {
      let document = find(editor, id)?;
      let span = to_span(document.buffer(), range)?;
      if span.len() > MAX_READ_BYTES {
        return Err(ApiError::TooLarge(format!(
          "ranges of more than {} MiB must be read in parts",
          MAX_READ_BYTES / 1024 / 1024
        )));
      }
      Ok(TextSlice {
        version: document.version().get(),
        text: document.buffer().text_in(span),
      })
    })
    .ok_or_else(editor_unavailable)?
  }

  fn apply_edits(&mut self, id: u64, version: u64, edits: Vec<TextEdit>) -> Result<u64, ApiError> {
    let shared = self.api.require(WitPermission::DocumentsWrite)?;
    if edits.len() > MAX_EDITS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_EDITS} edits can be applied at once"
      )));
    }
    if edits.iter().map(|edit| edit.text.len()).sum::<usize>() > MAX_EDIT_BYTES {
      return Err(ApiError::TooLarge(format!(
        "edits may insert at most {} MiB at once",
        MAX_EDIT_BYTES / 1024 / 1024
      )));
    }
    let outcome = with_editor(&shared, |editor| {
      let document = editor
        .document_mut(DocumentId::from_raw(id))
        .map_err(|_| ApiError::UnknownDocument)?;
      let current = document.version().get();
      if current != version {
        return Err(ApiError::StaleVersion(current));
      }
      let buffer = document.buffer();
      let mut converted = edits
        .into_iter()
        .map(|edit| Ok(Edit::new(to_span(buffer, edit.range)?, edit.text)))
        .collect::<Result<Vec<_>, ApiError>>()?;
      converted.sort_by_key(|edit| (edit.range.start, edit.range.end));
      if converted
        .windows(2)
        .any(|pair| pair[1].range.start < pair[0].range.end)
      {
        return Err(ApiError::InvalidRange("edits overlap".into()));
      }
      let changed = document
        .apply_edits(converted)
        .map_err(|error| ApiError::InvalidRange(error.to_string()))?;
      if changed {
        shared.sync(document);
      }
      Ok((document.version().get(), changed))
    })
    .ok_or_else(editor_unavailable)??;
    if outcome.1 {
      tracing::info!(extension = %self.api.id, document = id, "extension edited a document");
      shared
        .embedder
        .notify(Notification::DocumentEdited(DocumentId::from_raw(id)));
    }
    Ok(outcome.0)
  }

  fn publish_diagnostics(
    &mut self,
    id: u64,
    version: u64,
    diagnostics: Vec<Diagnostic>,
  ) -> Result<(), ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    if diagnostics.len() > MAX_DIAGNOSTICS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_DIAGNOSTICS} diagnostics can be published for a document"
      )));
    }
    let extension = self.api.id.clone();
    with_editor(&shared, |editor| {
      let document = find(editor, id)?;
      let convert = |buffer: &TextBuffer| {
        diagnostics
          .iter()
          .map(|diagnostic| {
            Ok(StoredDiagnostic {
              range: to_span(buffer, diagnostic.range)?,
              severity: severity(diagnostic.severity),
              message: cut(&diagnostic.message, MAX_DIAGNOSTIC_MESSAGE_CHARS),
              code: diagnostic
                .code
                .as_deref()
                .map(|code| cut(code, MAX_CODE_CHARS)),
            })
          })
          .collect::<Result<Vec<_>, ApiError>>()
      };
      shared.store_diagnostics(document, &extension, version, convert)
    })
    .ok_or_else(editor_unavailable)??;
    shared
      .embedder
      .notify(Notification::DiagnosticsChanged(DocumentId::from_raw(id)));
    Ok(())
  }

  fn set_configuration(
    &mut self,
    key: String,
    value: Option<WitConfigValue>,
  ) -> Result<(), ApiError> {
    let api = &self.api;
    let property = api
      .manifest
      .configuration
      .get(&key)
      .ok_or_else(|| ApiError::InvalidArgument(format!("no setting {key:?} is declared")))?;
    let value = value.map(|value| match value {
      WitConfigValue::Boolean(b) => toml::Value::Boolean(b),
      WitConfigValue::Integer(i) => toml::Value::Integer(i),
      WitConfigValue::Number(f) => toml::Value::Float(f),
      WitConfigValue::Text(s) => toml::Value::String(s),
    });
    let effective = match &value {
      Some(value) => property
        .check(value)
        .map_err(|problem| ApiError::InvalidArgument(format!("{key} {problem}")))?,
      None => property.default.clone(),
    };
    api
      .shared()?
      .embedder
      .set_setting(&api.id, &key, value)
      .map_err(ApiError::InvalidArgument)?;
    // In effect at once, before the settings file is read again.
    api
      .config
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(key, effective);
    Ok(())
  }
}

/// Most items in one view.
pub const MAX_VIEW_ITEMS: usize = 5_000;
/// Most decorated ranges per decoration type and document.
pub const MAX_DECORATIONS: usize = 10_000;
/// Most code lenses an extension shows in a document.
pub const MAX_LENSES: usize = 1_000;
/// Most line annotations an extension shows in a document.
pub const MAX_ANNOTATIONS: usize = 100_000;

impl ApiContext {
  pub(crate) fn own_command(&self, command: Option<&String>) -> Result<(), ApiError> {
    match command {
      Some(command) if !self.manifest.declares_command(command) => Err(ApiError::InvalidArgument(
        format!("{command:?} is not one of this extension's commands"),
      )),
      _ => Ok(()),
    }
  }

  /// The id of an instance of one of the extension's previews, and the
  /// preview's icon.
  fn preview(&self, preview: &str, key: &str) -> Result<(String, String), ApiError> {
    let declared = self
      .manifest
      .contributions
      .preview(preview)
      .ok_or_else(|| ApiError::InvalidArgument(format!("no preview {preview:?} is declared")))?;
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.contains(char::is_control) {
      return Err(ApiError::InvalidArgument(format!(
        "preview keys must be 1 to {MAX_KEY_BYTES} bytes, without control characters"
      )));
    }
    Ok((format!("{}/{key}", declared.full_id), declared.icon.clone()))
  }

  /// The preview id of `path` open in one of the extension's viewers, and
  /// the file, or an error if it isn't open there.
  fn viewed(&self, viewer: &str, path: &str) -> Result<(String, std::path::PathBuf), ApiError> {
    let declared = self
      .manifest
      .contributions
      .viewer(viewer)
      .ok_or_else(|| ApiError::InvalidArgument(format!("no viewer {viewer:?} is declared")))?;
    let id = crate::host::viewer_id(&declared.full_id, Path::new(path));
    let file = self.shared()?.viewed_file(&id).ok_or_else(|| {
      ApiError::InvalidArgument(format!("{path} isn't open in the viewer {viewer:?}"))
    })?;
    Ok((id, file))
  }

  fn is_webview_preview(&self, preview: &str) -> bool {
    self
      .manifest
      .contributions
      .preview(preview)
      .is_some_and(|declared| declared.webview.is_some())
  }

  /// The preview id of a webview tab of the extension's; `None` if it
  /// isn't open, and an error if it isn't a webview.
  fn webview(&self, webview: &Webview) -> Result<Option<String>, ApiError> {
    match webview {
      Webview::Viewer(viewed) => {
        let declared = self
          .manifest
          .contributions
          .viewer(&viewed.viewer)
          .ok_or_else(|| {
            ApiError::InvalidArgument(format!("no viewer {:?} is declared", viewed.viewer))
          })?;
        if declared.webview.is_none() {
          return Err(ApiError::InvalidArgument(format!(
            "the viewer {:?} isn't a webview",
            viewed.viewer
          )));
        }
        match self.viewed(&viewed.viewer, &viewed.path) {
          Ok((id, _)) => Ok(Some(id)),
          // Closed since the command started.
          Err(ApiError::InvalidArgument(_)) => Ok(None),
          Err(error) => Err(error),
        }
      }
      Webview::Preview(instance) => {
        let (id, _) = self.preview(&instance.preview, &instance.key)?;
        if !self.is_webview_preview(&instance.preview) {
          return Err(ApiError::InvalidArgument(format!(
            "the preview {:?} isn't a webview",
            instance.preview
          )));
        }
        Ok(self.shared()?.preview_document(&id).map(|_| id))
      }
    }
  }

  pub(crate) fn view_id(&self, view: &str) -> Result<String, ApiError> {
    self
      .manifest
      .contributions
      .view(view)
      .map(|declared| declared.full_id.clone())
      .ok_or_else(|| ApiError::InvalidArgument(format!("no view {view:?} is declared")))
  }
}

impl WorkbenchHost for StoreState {
  fn set_status_item(&mut self, id: String, item: Option<StatusItem>) -> Result<(), ApiError> {
    let api = &self.api;
    if api.manifest.contributions.status_item(&id).is_none() {
      return Err(ApiError::InvalidArgument(format!(
        "no status item {id:?} is declared"
      )));
    }
    let content = match item {
      Some(item) => {
        api.own_command(item.command.as_ref())?;
        Some(StatusContent {
          text: cut(item.text.trim(), 100),
          tooltip: item.tooltip.map(|tooltip| cut(&tooltip, 500)),
          command: item.command,
        })
      }
      None => None,
    };
    api.shared()?.set_status_item(&api.id, &id, content);
    Ok(())
  }

  fn set_view_items(&mut self, view: String, items: Vec<ViewItem>) -> Result<(), ApiError> {
    let api = &self.api;
    let full_id = api.view_id(&view)?;
    if items.len() > MAX_VIEW_ITEMS {
      return Err(ApiError::TooLarge(format!(
        "a view can show at most {MAX_VIEW_ITEMS} items"
      )));
    }
    let mut seen = HashSet::new();
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
      if item.id.is_empty() || item.id.len() > 200 {
        return Err(ApiError::InvalidArgument(
          "view item ids must be 1 to 200 bytes".into(),
        ));
      }
      if let Some(parent) = &item.parent
        && !seen.contains(parent)
      {
        return Err(ApiError::InvalidArgument(format!(
          "view item {:?}: its parent {parent:?} must come before it",
          item.id
        )));
      }
      api.own_command(item.command.as_ref())?;
      if !seen.insert(item.id.clone()) {
        return Err(ApiError::InvalidArgument(format!(
          "view item {:?} appears twice",
          item.id
        )));
      }
      entries.push(ViewEntry {
        id: item.id,
        parent: item.parent,
        label: cut(&item.label, 200),
        description: item.description.map(|text| cut(&text, 200)),
        tooltip: item.tooltip.map(|text| cut(&text, 1_000)),
        expanded: item.expanded,
        command: item.command,
        argument: item.argument.map(|text| cut(&text, 4_000)),
        icon: None,
        mark: None,
        context: None,
        expandable: false,
      });
    }
    api
      .shared()?
      .update_view(&full_id, |content| content.items = entries);
    Ok(())
  }

  fn set_view_message(&mut self, view: String, message: Option<String>) -> Result<(), ApiError> {
    let full_id = self.api.view_id(&view)?;
    let message = message.map(|text| cut(&text, 500));
    self
      .api
      .shared()?
      .update_view(&full_id, |content| content.message = message);
    Ok(())
  }

  fn show_view(&mut self, view: String) -> Result<(), ApiError> {
    let full_id = self.api.view_id(&view)?;
    self
      .api
      .shared()?
      .embedder
      .notify(Notification::ShowView(full_id));
    Ok(())
  }

  fn set_decorations(
    &mut self,
    id: u64,
    version: u64,
    decoration: String,
    ranges: Vec<Range>,
  ) -> Result<(), ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    if self
      .api
      .manifest
      .contributions
      .decoration(&decoration)
      .is_none()
    {
      return Err(ApiError::InvalidArgument(format!(
        "no decoration type {decoration:?} is declared"
      )));
    }
    if ranges.len() > MAX_DECORATIONS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_DECORATIONS} ranges can be decorated with one type"
      )));
    }
    let extension = self.api.id.clone();
    with_editor(&shared, |editor| {
      let document = find(editor, id)?;
      shared.store_decorations(document, &extension, &decoration, version, |buffer| {
        ranges.iter().map(|range| to_span(buffer, *range)).collect()
      })
    })
    .ok_or_else(editor_unavailable)??;
    shared
      .embedder
      .notify(Notification::DecorationsChanged(DocumentId::from_raw(id)));
    Ok(())
  }

  fn set_code_lenses(
    &mut self,
    id: u64,
    version: u64,
    lenses: Vec<CodeLens>,
  ) -> Result<(), ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    if lenses.len() > MAX_LENSES {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_LENSES} code lenses in a document"
      )));
    }
    for lens in &lenses {
      self.api.own_command(Some(&lens.command))?;
      if lens.title.trim().is_empty() {
        return Err(ApiError::InvalidArgument(
          "a code lens needs a title".into(),
        ));
      }
      if lens
        .argument
        .as_ref()
        .is_some_and(|argument| argument.len() > 4_096)
      {
        return Err(ApiError::TooLarge(
          "a code lens's argument is at most 4 KiB".into(),
        ));
      }
    }
    let extension = self.api.id.clone();
    with_editor(&shared, |editor| {
      let document = find(editor, id)?;
      shared.store_lenses(document, &extension, version, |buffer| {
        lenses
          .into_iter()
          .map(|lens| {
            let line = lens.line as usize;
            if line >= buffer.line_count() {
              return Err(ApiError::InvalidRange(format!(
                "line {line} is past the end of the document"
              )));
            }
            Ok((
              buffer.line_start(line),
              StoredLens {
                title: cut(lens.title.trim(), 100),
                tooltip: lens.tooltip.map(|tooltip| cut(&tooltip, 500)),
                command: lens.command,
                argument: lens.argument,
              },
            ))
          })
          .collect()
      })
    })
    .ok_or_else(editor_unavailable)??;
    shared
      .embedder
      .notify(Notification::DecorationsChanged(DocumentId::from_raw(id)));
    Ok(())
  }

  fn set_line_annotations(
    &mut self,
    id: u64,
    version: u64,
    annotations: Vec<LineAnnotation>,
  ) -> Result<(), ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    if annotations.len() > MAX_ANNOTATIONS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_ANNOTATIONS} line annotations in a document"
      )));
    }
    let extension = self.api.id.clone();
    with_editor(&shared, |editor| {
      let document = find(editor, id)?;
      shared.store_annotations(document, &extension, version, |buffer| {
        annotations
          .into_iter()
          .map(|annotation| {
            let line = annotation.line as usize;
            if line >= buffer.line_count() {
              return Err(ApiError::InvalidRange(format!(
                "line {line} is past the end of the document"
              )));
            }
            Ok((
              buffer.line_start(line),
              cut(annotation.text.trim(), 200),
              annotation.tooltip.map(|tooltip| cut(&tooltip, 2_000)),
            ))
          })
          .collect()
      })
    })
    .ok_or_else(editor_unavailable)??;
    shared
      .embedder
      .notify(Notification::DecorationsChanged(DocumentId::from_raw(id)));
    Ok(())
  }

  fn reveal(&mut self, id: u64, range: Range) -> Result<(), ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    let (start, end) = with_editor(&shared, |editor| {
      let document = find(editor, id)?;
      let buffer = document.buffer();
      let span = to_span(buffer, range)?;
      Ok::<_, ApiError>((
        buffer.offset_to_utf16(span.start),
        buffer.offset_to_utf16(span.end),
      ))
    })
    .ok_or_else(editor_unavailable)??;
    shared.embedder.notify(Notification::Reveal {
      document: DocumentId::from_raw(id),
      start,
      end,
    });
    Ok(())
  }

  fn command_argument(&mut self) -> Option<String> {
    self.api.argument.clone()
  }

  fn active_file(&mut self) -> Option<String> {
    let path = self.api.shared().ok()?.active_file()?;
    Some(path.display().to_string())
  }

  fn show_preview(
    &mut self,
    preview: String,
    key: String,
    options: PreviewOptions,
  ) -> Result<(), ApiError> {
    let (id, icon) = self.api.preview(&preview, &key)?;
    // A webview's page: the manifest's, or none until the extension sets it.
    let page = self
      .api
      .manifest
      .contributions
      .preview(&preview)
      .and_then(|declared| declared.webview.as_ref())
      .map(|webview| webview.page.clone());
    let title = options.title.trim();
    if title.is_empty() {
      return Err(ApiError::InvalidArgument("a preview needs a title".into()));
    }
    let shared = match options.document {
      Some(document) => {
        let shared = self.api.require(WitPermission::DocumentsRead)?;
        with_editor(&shared, |editor| find(editor, document).map(|_| ()))
          .ok_or_else(editor_unavailable)??;
        shared
      }
      None => self.api.shared()?,
    };
    let info = PreviewInfo {
      id,
      extension: self.api.name.clone(),
      title: cut(title, MAX_TITLE_CHARS),
      icon,
      document: options.document,
      webview: page.is_some(),
    };
    shared.show_preview(&self.api.id, info, page, options.beside);
    Ok(())
  }

  fn set_preview_content(
    &mut self,
    preview: String,
    key: String,
    nodes: Vec<PreviewNode>,
  ) -> Result<bool, ApiError> {
    let (id, _) = self.api.preview(&preview, &key)?;
    if self.api.is_webview_preview(&preview) {
      return Err(ApiError::InvalidArgument(format!(
        "the preview {preview:?} is a webview: use set-webview-html or post-message"
      )));
    }
    let shared = self.api.shared()?;
    let Some(document) = shared.preview_document(&id) else {
      return Ok(false);
    };
    // Relative links and pictures are found from the document's folder.
    let base = document.and_then(|document| {
      with_editor(&shared, |editor| {
        let document = editor.document(document).ok()?;
        document
          .path()
          .and_then(Path::parent)
          .map(Path::to_path_buf)
      })
      .flatten()
    });
    let converted = preview::convert(nodes, base.as_deref())?;
    Ok(shared.set_preview_content(&id, converted.content, converted.images))
  }

  fn close_preview(&mut self, preview: String, key: String) -> Result<(), ApiError> {
    let (id, _) = self.api.preview(&preview, &key)?;
    self.api.shared()?.close_preview(&id);
    Ok(())
  }

  fn render_markdown(&mut self, text: String) -> Result<Vec<PreviewNode>, ApiError> {
    if text.len() > crate::markdown::MAX_MARKDOWN_BYTES {
      return Err(ApiError::TooLarge(format!(
        "Markdown of at most {} MiB can be rendered",
        crate::markdown::MAX_MARKDOWN_BYTES / 1024 / 1024
      )));
    }
    Ok(crate::markdown::render(&text))
  }
}

/// Most bytes one `read-viewed-file` call returns.
pub const MAX_VIEWED_READ_BYTES: u32 = 16 * 1024 * 1024;

impl ViewersHost for StoreState {
  fn set_viewer_content(
    &mut self,
    viewer: String,
    path: String,
    nodes: Vec<PreviewNode>,
  ) -> Result<bool, ApiError> {
    let (id, file) = match self.api.viewed(&viewer, &path) {
      Ok(viewed) => viewed,
      // Closed since the command started.
      Err(ApiError::InvalidArgument(_))
        if self.api.manifest.contributions.viewer(&viewer).is_some() =>
      {
        return Ok(false);
      }
      Err(error) => return Err(error),
    };
    // Pictures and links are found from the file's folder.
    let converted = preview::convert(nodes, file.parent())?;
    Ok(
      self
        .api
        .shared()?
        .set_preview_content(&id, converted.content, converted.images),
    )
  }

  fn read_viewed_file(
    &mut self,
    viewer: String,
    path: String,
    offset: u64,
    length: u32,
  ) -> Result<FileBytes, ApiError> {
    use std::io::{Read, Seek, SeekFrom};
    let (_, file) = self.api.viewed(&viewer, &path)?;
    let failed = |error: std::io::Error| {
      ApiError::InvalidArgument(format!("{} couldn't be read: {error}", file.display()))
    };
    let mut handle = std::fs::File::open(&file).map_err(failed)?;
    let size = handle.metadata().map_err(failed)?.len();
    let length = u64::from(length.min(MAX_VIEWED_READ_BYTES)).min(size.saturating_sub(offset));
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    handle.seek(SeekFrom::Start(offset)).map_err(failed)?;
    handle
      .take(length)
      .read_to_end(&mut bytes)
      .map_err(failed)?;
    Ok(FileBytes { size, bytes })
  }
}

impl WebviewsHost for StoreState {
  fn set_webview_html(&mut self, webview: Webview, html: String) -> Result<bool, ApiError> {
    if html.len() > crate::webview::MAX_HTML_BYTES {
      return Err(ApiError::TooLarge(format!(
        "a page must be at most {} MiB",
        crate::webview::MAX_HTML_BYTES / 1024 / 1024
      )));
    }
    let Some(id) = self.api.webview(&webview)? else {
      return Ok(false);
    };
    Ok(self.api.shared()?.set_webview_html(&id, html))
  }

  fn post_message(&mut self, webview: Webview, message: String) -> Result<bool, ApiError> {
    if message.len() > crate::webview::MAX_MESSAGE_BYTES {
      return Err(ApiError::TooLarge(format!(
        "a message must be at most {} KiB",
        crate::webview::MAX_MESSAGE_BYTES / 1024
      )));
    }
    serde_json::from_str::<serde::de::IgnoredAny>(&message)
      .map_err(|error| ApiError::InvalidArgument(format!("the message isn't JSON: {error}")))?;
    let Some(id) = self.api.webview(&webview)? else {
      return Ok(false);
    };
    Ok(self.api.shared()?.post_webview_message(&id, message))
  }

  fn message(&mut self) -> Option<WebviewMessage> {
    match &self.api.payload {
      Payload::Message(message) => Some(message.clone()),
      _ => None,
    }
  }
}

impl ApiContext {
  /// A storage scope: `None` for the workspace while no folder is open.
  fn scope(&self, scope: StorageScope) -> Result<Option<crate::storage::Scope>, ApiError> {
    Ok(match scope {
      StorageScope::Global => Some(crate::storage::Scope::Global),
      StorageScope::Workspace => self
        .shared()?
        .embedder
        .open_folder()
        .map(|folder| crate::storage::Scope::Workspace(folder.root)),
    })
  }
}

impl StorageHost for StoreState {
  fn get(&mut self, scope: StorageScope, key: String) -> Option<String> {
    let scope = self.api.scope(scope).ok().flatten()?;
    self
      .api
      .shared()
      .ok()?
      .storage
      .get(&self.api.id, &scope, &key)
  }

  fn set(
    &mut self,
    scope: StorageScope,
    key: String,
    value: Option<String>,
  ) -> Result<(), ApiError> {
    let scope = self
      .api
      .scope(scope)?
      .ok_or_else(|| ApiError::InvalidArgument("no folder is open".into()))?;
    let shared = self.api.shared()?;
    shared
      .storage
      .set(&self.api.id, &scope, &key, value.clone())
      .map_err(|error| match error {
        StorageError::TooLarge(message) => ApiError::TooLarge(message),
        StorageError::Invalid(message) | StorageError::Unsaved(message) => {
          ApiError::InvalidArgument(message)
        }
      })?;
    shared.storage_changed(&self.api.id, &scope, &key, value, None);
    Ok(())
  }

  fn keys(&mut self, scope: StorageScope) -> Vec<String> {
    let Ok(Some(scope)) = self.api.scope(scope) else {
      return Vec::new();
    };
    self
      .api
      .shared()
      .map(|shared| shared.storage.keys(&self.api.id, &scope))
      .unwrap_or_default()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn positions_must_be_inside_the_text_and_on_character_boundaries() {
    let buffer = TextBuffer::from_text("ab\r\nçd");
    let at = |line, column| Position { line, column };
    assert_eq!(to_offset(&buffer, at(0, 2)), Ok(2));
    assert_eq!(to_offset(&buffer, at(1, 0)), Ok(4));
    assert!(
      to_offset(&buffer, at(0, 3)).is_err(),
      "inside the line break"
    );
    assert!(to_offset(&buffer, at(1, 1)).is_err(), "inside ç");
    assert!(to_offset(&buffer, at(2, 0)).is_err());
    assert_eq!(to_position(&buffer, 6), at(1, 2));
    let backwards = Range {
      start: at(1, 0),
      end: at(0, 0),
    };
    assert!(to_span(&buffer, backwards).is_err());
  }

  #[test]
  fn rate_limits_allow_a_burst_then_drop() {
    let mut limit = RateLimit::new(3.0, 0.0);
    assert!((0..3).all(|_| limit.allow()));
    assert!(!limit.allow());
    assert_eq!(limit.take_dropped(), 1);
  }

  #[test]
  fn long_text_is_cut_on_a_character_boundary() {
    assert_eq!(cut("ééé", 2), "éé…");
    assert_eq!(cut("short", 10), "short");
  }
}
