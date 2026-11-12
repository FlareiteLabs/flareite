//! Language servers for open documents: starting them on demand, keeping
//! documents in sync, diagnostics, and (in `commands`) hover, go to
//! definition, references, symbols and completion.
//!
//! A document of a language with a configured server is opened on the
//! server for its workspace root the first time it is viewed; the server is
//! started then if needed, on its own thread. Every view syncs the document
//! first, so the server sees each edit right after it is applied (as an
//! incremental change built from the buffer's edit log). Diagnostics are
//! stored as byte ranges in the text they were computed for and mapped
//! through later edits when shown. Diagnostics a server publishes for files
//! that aren't open (a workspace-wide check, or a file just closed) are kept
//! as the server's positions, for the Problems view. A server that exits is
//! restarted on demand a few times, then given up on.
//!
//! Servers are configured by their extensions and the settings: their
//! arguments (which may name files to download), and settings they ask for.
//! When that changes, only the servers it changed for restart, and a change
//! of settings alone is sent to the running ones instead. A server waits to
//! start while the extension declaring it activates (see
//! `ExtensionHost::server_waits`), for at most a few seconds. Changed files
//! the servers watch are passed on to them.
//!
//! Lock order: the editor lock, then this state's lock. Nothing blocks on a
//! server while holding either: notifications are queued, and requests are
//! made after the locks are released.

mod commands;
mod editing;
mod edits;
mod symbols;

use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use flareite_core::{Document, DocumentId};
use flareite_lsp::convert::{content_changes, full_change, path_to_uri, to_offsets, uri_to_path};
use flareite_lsp::lsp_types::notification::{
  DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
};
use flareite_lsp::lsp_types::{
  ApplyWorkspaceEditParams, Diagnostic as LspDiagnostic, DiagnosticSeverity,
  DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
  DidSaveTextDocumentParams, NumberOrString, PublishDiagnosticsParams, TextDocumentIdentifier,
  TextDocumentItem, TextDocumentSyncKind, Uri, VersionedTextDocumentIdentifier,
};
use flareite_lsp::{FileChange, LanguageServer, ServerConfig, ServerEvent, find_root};
use flareite_syntax::Language;
use flareite_text::{TextBuffer, TextSnapshot, Version, map_offset_through};
use serde::Serialize;
use tauri::{AppHandle, Manager};

pub use commands::*;
pub use editing::*;
pub use symbols::*;

use crate::downloads::{self, DownloadsState};
use crate::editor::EditorState;
use crate::editor::protocol::Position;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::languages::LanguagesState;
use crate::settings::SettingsState;
use crate::workspace::WorkspaceState;
use flareite_extension_host::contributions::DOWNLOAD_START;

/// Larger documents are not sent to language servers.
const MAX_LSP_BYTES: usize = 8 * 1024 * 1024;
/// Automatic restarts of a server that keeps exiting.
const MAX_RESTARTS: u32 = 3;
/// Synced versions kept to interpret diagnostics computed for older text.
const SYNC_HISTORY: usize = 16;
/// Most diagnostics included in one view.
const MAX_VIEW_DIAGNOSTICS: usize = 2_000;
/// Longest a server waits for its extension to activate.
const HOLD_LIMIT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ServerKey {
  name: String,
  root: PathBuf,
}

enum Slot {
  /// Not running; started when a document needs it.
  Idle,
  Starting,
  Running(Arc<LanguageServer>),
  /// Not installed, failed to start, or exited too often.
  Failed(String),
}

struct ServerEntry {
  slot: Slot,
  restarts: u32,
  /// Titles of work in progress (e.g. indexing), by progress token.
  progress: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct Diagnostic {
  range: Range<usize>,
  severity: u8,
  message: String,
  source: Option<String>,
  /// As the server published it (its code and data, say), for asking it
  /// for code actions; its range is out of date.
  original: Box<LspDiagnostic>,
}

/// A diagnostic at a position, for the Problems view. In a file that isn't
/// open, the position is as the server gave it: its columns count the
/// server's units (usually UTF-16, as the editor's do).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiagnostic {
  pub start: Position,
  pub end: Position,
  /// 1 error, 2 warning, 3 information, 4 hint.
  pub severity: u8,
  pub message: String,
  pub source: Option<String>,
}

/// What a server's diagnostics were published for.
enum Published {
  /// An open document, by id.
  Document(u64),
  /// A file that isn't open.
  File,
  /// Neither (e.g. a version that's too old, or a URI that isn't a file).
  Nothing,
}

/// A document open on a server.
struct OpenDocument {
  key: ServerKey,
  server: Arc<LanguageServer>,
  uri: Uri,
  path: PathBuf,
  /// The id of the language it was opened as.
  language: String,
  lsp_version: i32,
  /// Texts recently sent, by LSP version, oldest first; the last is what
  /// the server has now.
  history: VecDeque<(i32, TextSnapshot)>,
  /// Diagnostics as byte ranges in the text at `diagnostics_version`.
  diagnostics: Vec<Diagnostic>,
  diagnostics_version: Version,
}

impl OpenDocument {
  fn synced(&self) -> &TextSnapshot {
    // The history is never empty: opening records the first version.
    &self.history[self.history.len() - 1].1
  }

  /// Sends the edits since the last sync.
  fn send_changes(&mut self, buffer: &TextBuffer) {
    if self.synced().version() == buffer.version() {
      return;
    }
    let encoding = self.server.encoding();
    let changes = match (
      self.server.sync_kind(),
      buffer.edits_since(self.synced().version()),
    ) {
      (None, _) => Vec::new(),
      (Some(TextDocumentSyncKind::INCREMENTAL), Some(edits)) => {
        content_changes(self.synced(), &edits, encoding)
      }
      _ => vec![full_change(buffer.contents())],
    };
    self.lsp_version += 1;
    if !changes.is_empty() {
      self
        .server
        .notify::<DidChangeTextDocument>(DidChangeTextDocumentParams {
          text_document: VersionedTextDocumentIdentifier::new(self.uri.clone(), self.lsp_version),
          content_changes: changes,
        });
    }
    if self.history.len() == SYNC_HISTORY {
      self.history.pop_front();
    }
    self
      .history
      .push_back((self.lsp_version, buffer.snapshot()));
  }

  /// Maps the diagnostics through the edits since they were stored.
  fn catch_up(&mut self, buffer: &TextBuffer) {
    if self.diagnostics_version == buffer.version() {
      return;
    }
    match buffer.edits_since(self.diagnostics_version) {
      Some(edits) => {
        for diagnostic in &mut self.diagnostics {
          let start = map_offset_through(&edits, diagnostic.range.start, true);
          let end = map_offset_through(&edits, diagnostic.range.end, false).max(start);
          diagnostic.range = start..end;
        }
      }
      // Too old to map; the server will send fresh ones.
      None => self.diagnostics.clear(),
    }
    self.diagnostics_version = buffer.version();
  }

  /// The diagnostics at positions in `buffer`, the document's text.
  fn positioned(&mut self, buffer: &TextBuffer) -> Vec<FileDiagnostic> {
    self.catch_up(buffer);
    self
      .diagnostics
      .iter()
      .map(|d| FileDiagnostic {
        start: buffer.offset_to_utf16(d.range.start).into(),
        end: buffer.offset_to_utf16(d.range.end).into(),
        severity: d.severity,
        message: d.message.clone(),
        source: d.source.clone(),
      })
      .collect()
  }

  fn close(&self) {
    if self.server.is_alive() {
      self
        .server
        .notify::<DidCloseTextDocument>(DidCloseTextDocumentParams {
          text_document: TextDocumentIdentifier::new(self.uri.clone()),
        });
    }
  }
}

/// The server a document should be open on.
struct Wanted {
  key: ServerKey,
  config: ServerConfig,
}

#[derive(Default)]
struct Inner {
  /// Configured servers; loaded from settings on first use.
  configs: Option<Vec<ServerConfig>>,
  servers: HashMap<ServerKey, ServerEntry>,
  documents: HashMap<DocumentId, OpenDocument>,
  /// Documents waiting for their server to start (or that cannot have one
  /// because it failed), with the path and language id they were checked
  /// for.
  waiting: HashMap<DocumentId, (PathBuf, String, ServerKey)>,
  /// Diagnostics for files that aren't open, by server and path.
  unopened: HashMap<ServerKey, BTreeMap<PathBuf, Vec<FileDiagnostic>>>,
  /// Servers waiting for their extension to activate, by name, as of the
  /// last change to extensions.
  held: HashSet<String>,
  /// When each held server was first wanted.
  held_since: HashMap<String, Instant>,
  /// The code actions last offered, for choosing one.
  code_actions: Option<editing::OfferedActions>,
  shutting_down: bool,
}

#[derive(Default)]
pub struct LspState(Mutex<Inner>);

/// A diagnostic in a view.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticDto {
  pub start: Position,
  pub end: Position,
  /// 1 error, 2 warning, 3 information, 4 hint.
  pub severity: u8,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatusDto {
  pub name: String,
  /// `starting`, `ready`, `busy`, or `failed`.
  pub state: &'static str,
  pub message: Option<String>,
}

/// Language features for a view's document, for the status bar and editor.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageStatusDto {
  /// The language's id, e.g. `css`.
  pub id: String,
  /// Its name, e.g. `CSS`.
  pub language: String,
  pub server: Option<ServerStatusDto>,
  /// Characters that open completion; `None` without completion support.
  pub completion_triggers: Option<Vec<String>>,
  /// Characters whose typing asks extensions for text to insert (closing
  /// tags); `None` if no extension inserts any for the language.
  pub auto_insert_triggers: Option<Vec<String>>,
  /// Characters that open signature help, and those that update it while
  /// it's open; `None` without signature help.
  pub signature_triggers: Option<SignatureTriggersDto>,
  /// Whether the server offers code actions, so the editor can look for
  /// them where the cursor rests.
  pub code_actions: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureTriggersDto {
  pub open: Vec<String>,
  pub update: Vec<String>,
}

fn severity(severity: Option<DiagnosticSeverity>) -> u8 {
  match severity {
    Some(DiagnosticSeverity::WARNING) => 2,
    Some(DiagnosticSeverity::INFORMATION) => 3,
    Some(DiagnosticSeverity::HINT) => 4,
    // Servers may omit it; clients are to treat that as an error.
    _ => 1,
  }
}

impl LspState {
  fn lock(&self) -> MutexGuard<'_, Inner> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn wanted(
    &self,
    app: &AppHandle,
    inner: &mut Inner,
    path: &Path,
    language: &Language,
  ) -> Option<Wanted> {
    // Servers build and run the project's code; trusting the folder
    // restarts them all.
    if !crate::trust::workspace_trusted(app) {
      return None;
    }
    let languages = app.state::<LanguagesState>();
    let configs = inner.configs.get_or_insert_with(|| {
      app
        .state::<SettingsState>()
        .get()
        .language_servers_with(languages.servers())
    });
    let config = configs
      .iter()
      .find(|config| config.languages.iter().any(|id| id == language.id()))?
      .clone();
    let workspace = app.state::<WorkspaceState>().root();
    let root = find_root(path, &config.root_markers, workspace.as_deref());
    Some(Wanted {
      key: ServerKey {
        name: config.name.clone(),
        root,
      },
      config,
    })
  }

  /// Brings the server's copy of `document` up to date, opening it (and
  /// starting its server) if needed. Called with the editor locked.
  pub fn sync(&self, app: &AppHandle, document: &Document) {
    let id = document.id();
    let buffer = document.buffer();
    let language = app.state::<LanguagesState>().for_document(document);
    let mut inner = self.lock();
    if inner.shutting_down {
      return;
    }
    let path = document.path();
    if let Some(open) = inner.documents.get_mut(&id) {
      if Some(open.path.as_path()) == path
        && language.as_ref().map(Language::id) == Some(open.language.as_str())
        && open.server.is_alive()
      {
        open.send_changes(buffer);
        return;
      }
      // Saved under another name, set to another language, or its server
      // exited.
      if let Some(open) = inner.documents.remove(&id) {
        open.close();
      }
    }
    let (Some(path), Some(language)) = (path.filter(|_| buffer.len() <= MAX_LSP_BYTES), language)
    else {
      inner.waiting.remove(&id);
      return;
    };
    let key = match inner.waiting.get(&id) {
      Some((waited, waited_language, key))
        if waited == path && waited_language == language.id() =>
      {
        key.clone()
      }
      _ => {
        let Some(wanted) = self.wanted(app, &mut inner, path, &language) else {
          inner.waiting.remove(&id);
          return;
        };
        let key = wanted.key.clone();
        let entry = inner
          .servers
          .entry(key.clone())
          .or_insert_with(|| ServerEntry {
            slot: Slot::Idle,
            restarts: 0,
            progress: HashMap::new(),
          });
        if let Slot::Idle = entry.slot {
          if hold(app, &mut inner, &wanted.key.name) {
            return;
          }
          let Some(entry) = inner.servers.get_mut(&key) else {
            return;
          };
          entry.slot = Slot::Starting;
          start_server(app, wanted.key, wanted.config);
          events::emit(app, AppEvent::LanguageServerChanged);
        }
        inner.waiting.insert(
          id,
          (path.to_path_buf(), language.id().to_owned(), key.clone()),
        );
        key
      }
    };
    let Some(ServerEntry {
      slot: Slot::Running(server),
      ..
    }) = inner.servers.get(&key)
    else {
      return;
    };
    let Some(uri) = path_to_uri(path) else {
      return;
    };
    let server = server.clone();
    server.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
      text_document: TextDocumentItem::new(uri.clone(), language.id().into(), 1, buffer.contents()),
    });
    inner.waiting.remove(&id);
    inner.documents.insert(
      id,
      OpenDocument {
        key,
        server,
        uri,
        path: path.to_path_buf(),
        language: language.id().to_owned(),
        lsp_version: 1,
        history: VecDeque::from([(1, buffer.snapshot())]),
        diagnostics: Vec::new(),
        diagnostics_version: buffer.version(),
      },
    );
  }

  /// Tells the server a document was saved.
  pub fn saved(&self, document: &Document) {
    let mut inner = self.lock();
    if let Some(open) = inner.documents.get_mut(&document.id()) {
      open.send_changes(document.buffer());
      let text = open
        .server
        .wants_text_on_save()
        .then(|| document.buffer().contents());
      open
        .server
        .notify::<DidSaveTextDocument>(DidSaveTextDocumentParams {
          text_document: TextDocumentIdentifier::new(open.uri.clone()),
          text,
        });
    }
  }

  /// Closes a document on its server. Its diagnostics are kept as a
  /// file's, until the server publishes others (servers that only check
  /// open files clear them).
  pub fn close(&self, document: &Document) {
    let mut inner = self.lock();
    inner.waiting.remove(&document.id());
    if let Some(mut open) = inner.documents.remove(&document.id()) {
      open.close();
      let kept = open.positioned(document.buffer());
      if !kept.is_empty() {
        inner
          .unopened
          .entry(open.key)
          .or_default()
          .insert(open.path, kept);
      }
    }
  }

  /// An open document's diagnostics, at positions in its text. Called with
  /// the editor locked.
  pub fn document_diagnostics(&self, document: &Document) -> Vec<FileDiagnostic> {
    let mut inner = self.lock();
    inner
      .documents
      .get_mut(&document.id())
      .map_or_else(Vec::new, |open| open.positioned(document.buffer()))
  }

  /// Diagnostics servers published for files that aren't open, by path,
  /// leaving out `open` files (whose diagnostics are their documents').
  pub fn file_diagnostics(&self, open: &[PathBuf]) -> Vec<(PathBuf, Vec<FileDiagnostic>)> {
    let inner = self.lock();
    let mut files: BTreeMap<PathBuf, Vec<FileDiagnostic>> = BTreeMap::new();
    for (path, diagnostics) in inner.unopened.values().flatten() {
      if !open.iter().any(|other| same_path(other, path)) {
        files
          .entry(path.clone())
          .or_default()
          .extend(diagnostics.iter().cloned());
      }
    }
    files.into_iter().collect()
  }

  /// Diagnostics on `lines` and the language status, for a view. Called with
  /// the editor locked, after [`Self::sync`].
  pub fn view(
    &self,
    app: &AppHandle,
    document: &Document,
    lines: Range<usize>,
  ) -> (Vec<DiagnosticDto>, Option<LanguageStatusDto>) {
    let Some(language) = app.state::<LanguagesState>().for_document(document) else {
      return (Vec::new(), None);
    };
    let buffer = document.buffer();
    let mut inner = self.lock();
    let mut status = LanguageStatusDto {
      id: language.id().to_owned(),
      language: language.name().to_owned(),
      server: None,
      completion_triggers: None,
      auto_insert_triggers: None,
      signature_triggers: None,
      code_actions: false,
    };
    let id = document.id();
    let key = match (inner.documents.get(&id), inner.waiting.get(&id)) {
      (Some(open), _) => open.key.clone(),
      (None, Some((_, _, key))) => key.clone(),
      (None, None) => return (Vec::new(), Some(status)),
    };
    if let Some(entry) = inner.servers.get(&key) {
      let (state, message) = match &entry.slot {
        Slot::Idle | Slot::Starting => ("starting", None),
        Slot::Failed(message) => ("failed", Some(message.clone())),
        Slot::Running(_) => match entry.progress.values().next() {
          Some(title) => ("busy", Some(title.clone())),
          None => ("ready", None),
        },
      };
      status.server = Some(ServerStatusDto {
        name: key.name.clone(),
        state,
        message,
      });
    }
    let Some(open) = inner.documents.get_mut(&id) else {
      return (Vec::new(), Some(status));
    };
    status.completion_triggers = open
      .server
      .capabilities()
      .completion_provider
      .as_ref()
      .map(|completion| completion.trigger_characters.clone().unwrap_or_default());
    let capabilities = open.server.capabilities();
    status.signature_triggers =
      capabilities
        .signature_help_provider
        .as_ref()
        .map(|help| SignatureTriggersDto {
          open: help.trigger_characters.clone().unwrap_or_default(),
          update: help.retrigger_characters.clone().unwrap_or_default(),
        });
    status.code_actions = !matches!(
      capabilities.code_action_provider,
      None | Some(flareite_lsp::lsp_types::CodeActionProviderCapability::Simple(false))
    );
    open.catch_up(buffer);
    let visible = buffer.line_start(lines.start)..buffer.line_end(lines.end.saturating_sub(1));
    let diagnostics = open
      .diagnostics
      .iter()
      .filter(|d| d.range.start <= visible.end && d.range.end >= visible.start)
      .take(MAX_VIEW_DIAGNOSTICS)
      .map(|d| DiagnosticDto {
        start: buffer.offset_to_utf16(d.range.start).into(),
        end: buffer.offset_to_utf16(d.range.end).into(),
        severity: d.severity,
      })
      .collect();
    (diagnostics, Some(status))
  }

  /// Messages of the diagnostics covering `offset`, most severe first.
  fn diagnostics_at(&self, document: &Document, offset: usize) -> Vec<DiagnosticMessageDto> {
    let mut inner = self.lock();
    let Some(open) = inner.documents.get_mut(&document.id()) else {
      return Vec::new();
    };
    open.catch_up(document.buffer());
    let mut found: Vec<DiagnosticMessageDto> = open
      .diagnostics
      .iter()
      .filter(|d| d.range.start <= offset && offset <= d.range.end)
      .map(|d| DiagnosticMessageDto {
        severity: d.severity,
        message: d.message.clone(),
        source: d.source.clone(),
      })
      .collect();
    found.sort_by_key(|d| d.severity);
    found
  }

  /// The diagnostics of `document` from its server that touch `range` (byte
  /// offsets), as the server published them with their ranges where they
  /// are now in `buffer`, the text it was last sent: for code actions.
  fn diagnostics_in(
    &self,
    document: &Document,
    range: Range<usize>,
    encoding: flareite_lsp::PositionEncoding,
  ) -> Vec<LspDiagnostic> {
    let mut inner = self.lock();
    let Some(open) = inner.documents.get_mut(&document.id()) else {
      return Vec::new();
    };
    let buffer = document.buffer();
    open.catch_up(buffer);
    open
      .diagnostics
      .iter()
      .filter(|d| d.range.start <= range.end && range.start <= d.range.end)
      .map(|d| LspDiagnostic {
        range: flareite_lsp::convert::to_range(buffer, d.range.clone(), encoding),
        ..(*d.original).clone()
      })
      .collect()
  }

  fn on_diagnostics(&self, key: &ServerKey, params: PublishDiagnosticsParams) -> Published {
    // As open files are named (servers may lowercase a drive letter, say),
    // found before locking as it reads the disk.
    let path = uri_to_path(&params.uri).map(|path| crate::paths::normalize(&path));
    let mut inner = self.lock();
    let found = inner.documents.iter_mut().find(|(_, open)| {
      open.key == *key
        && (open.uri == params.uri
          || path
            .as_deref()
            .is_some_and(|path| same_path(path, &open.path)))
    });
    let Some((&id, open)) = found else {
      let Some(path) = path else {
        return Published::Nothing;
      };
      let files = inner.unopened.entry(key.clone()).or_default();
      files.retain(|other, _| !same_path(other, &path));
      let diagnostics: Vec<FileDiagnostic> = params
        .diagnostics
        .into_iter()
        .map(|diagnostic| FileDiagnostic {
          start: Position {
            line: diagnostic.range.start.line as usize,
            column: diagnostic.range.start.character as usize,
          },
          end: Position {
            line: diagnostic.range.end.line as usize,
            column: diagnostic.range.end.character as usize,
          },
          severity: severity(diagnostic.severity),
          message: diagnostic.message,
          source: diagnostic.source,
        })
        .collect();
      if !diagnostics.is_empty() {
        files.insert(path, diagnostics);
      }
      return Published::File;
    };
    let snapshot = match params.version {
      Some(version) => match open.history.iter().find(|(v, _)| *v == version) {
        Some((_, snapshot)) => snapshot,
        None => return Published::Nothing,
      },
      None => open.synced(),
    };
    let text = TextBuffer::from_snapshot(snapshot);
    let version = snapshot.version();
    let encoding = open.server.encoding();
    open.diagnostics = params
      .diagnostics
      .into_iter()
      .map(|diagnostic| Diagnostic {
        range: to_offsets(&text, diagnostic.range, encoding),
        severity: severity(diagnostic.severity),
        message: diagnostic.message.clone(),
        source: diagnostic.source.clone(),
        original: Box::new(diagnostic),
      })
      .collect();
    open.diagnostics_version = version;
    let open_path = open.path.clone();
    // The document's diagnostics replace what was kept for the file.
    if let Some(files) = inner.unopened.get_mut(key) {
      files.retain(|other, _| !same_path(other, &open_path));
    }
    Published::Document(id.get())
  }

  fn on_progress(&self, key: &ServerKey, params: &serde_json::Value) -> bool {
    let token = match serde_json::from_value::<NumberOrString>(params["token"].clone()) {
      Ok(NumberOrString::Number(number)) => number.to_string(),
      Ok(NumberOrString::String(string)) => string,
      Err(_) => return false,
    };
    let mut inner = self.lock();
    let Some(entry) = inner.servers.get_mut(key) else {
      return false;
    };
    let value = &params["value"];
    match value["kind"].as_str() {
      Some("begin") => {
        let title = value["title"].as_str().unwrap_or("Working").to_owned();
        entry.progress.insert(token, title);
        true
      }
      Some("end") => entry.progress.remove(&token).is_some(),
      _ => false,
    }
  }

  /// Handles a server exiting. Returns the documents that lost their
  /// diagnostics.
  fn on_exit(&self, key: &ServerKey) -> Vec<u64> {
    let mut inner = self.lock();
    if inner.shutting_down {
      return Vec::new();
    }
    let Some(entry) = inner.servers.get_mut(key) else {
      return Vec::new();
    };
    let Slot::Running(server) = &entry.slot else {
      return Vec::new();
    };
    if server.is_alive() {
      // A late event from an earlier instance.
      return Vec::new();
    }
    entry.restarts += 1;
    entry.progress.clear();
    entry.slot = if entry.restarts > MAX_RESTARTS {
      tracing::error!(server = %key.name, "language server keeps exiting; giving up");
      Slot::Failed(format!("{} stopped unexpectedly too many times.", key.name))
    } else {
      tracing::warn!(server = %key.name, restarts = entry.restarts, "language server exited; restarting");
      Slot::Idle
    };
    let affected: Vec<DocumentId> = inner
      .documents
      .iter()
      .filter(|(_, open)| open.key == *key)
      .map(|(&id, _)| id)
      .collect();
    for id in &affected {
      inner.documents.remove(id);
    }
    inner.waiting.retain(|_, (_, _, waiting)| waiting != key);
    // Its replacement checks the files again.
    inner.unopened.remove(key);
    affected.into_iter().map(DocumentId::get).collect()
  }

  /// Takes up the servers as extensions and settings now configure them:
  /// restarts those whose configuration changed (stopping any no longer
  /// wanted), and gives running ones new settings when only those changed.
  /// Servers held for their extension may start now.
  pub fn configure(&self, app: &AppHandle) {
    let configs = app
      .state::<SettingsState>()
      .get()
      .language_servers_with(app.state::<LanguagesState>().servers());
    let held: HashSet<String> = match app.state::<ExtensionsState>().host() {
      Some(host) => configs
        .iter()
        .map(|config| &config.name)
        .filter(|name| host.server_waits(name))
        .cloned()
        .collect(),
      None => HashSet::new(),
    };
    let (stopped, affected, resync) = {
      let mut inner = self.lock();
      if inner.shutting_down {
        return;
      }
      inner.held_since.retain(|name, _| held.contains(name));
      let released = inner.held.difference(&held).next().is_some();
      inner.held = held;
      let previous = inner.configs.replace(configs.clone());
      let listed = previous.as_ref() != Some(&configs);
      let find =
        |list: &[ServerConfig], name: &str| list.iter().find(|config| config.name == name).cloned();
      let mut changed = HashSet::new();
      for (key, entry) in &inner.servers {
        let before = previous.as_deref().and_then(|list| find(list, &key.name));
        let after = find(&configs, &key.name);
        match (before, after) {
          (Some(before), Some(after)) if before == after => {}
          (Some(before), Some(after)) if same_but_settings(&before, &after) => {
            if let Slot::Running(server) = &entry.slot {
              server.set_settings(&after.settings);
            }
          }
          _ => {
            changed.insert(key.name.clone());
          }
        }
      }
      let (stopped, affected) = take_servers(&mut inner, |key| changed.contains(&key.name));
      (stopped, affected, released || listed)
    };
    after_stopping(app, stopped, affected, resync);
  }

  /// Restarts the running instances of the server `name`.
  pub fn restart(&self, app: &AppHandle, name: &str) {
    let (stopped, affected) = take_servers(&mut self.lock(), |key| key.name == name);
    after_stopping(app, stopped, affected, false);
  }

  /// Passes changed files on to the servers watching them (those under
  /// their roots).
  pub fn files_changed(&self, paths: &[PathBuf]) {
    let servers: Vec<(PathBuf, Arc<LanguageServer>)> = {
      let inner = self.lock();
      inner
        .servers
        .iter()
        .filter_map(|(key, entry)| match &entry.slot {
          Slot::Running(server) if server.is_alive() => Some((key.root.clone(), server.clone())),
          _ => None,
        })
        .collect()
    };
    if servers.is_empty() {
      return;
    }
    let changes: Vec<(PathBuf, FileChange)> = paths
      .iter()
      .map(|path| {
        let change = if path.exists() {
          FileChange::Changed
        } else {
          FileChange::Deleted
        };
        (path.clone(), change)
      })
      .collect();
    for (root, server) in servers {
      let under: Vec<(PathBuf, FileChange)> = changes
        .iter()
        .filter(|(path, _)| path.starts_with(&root))
        .cloned()
        .collect();
      if !under.is_empty() {
        server.files_changed(&under);
      }
    }
  }

  /// Stops every server and forgets all documents, so the next views start
  /// over with the current settings.
  pub fn restart_all(&self, app: &AppHandle) {
    let servers = {
      let mut inner = self.lock();
      inner.configs = None;
      inner.documents.clear();
      inner.waiting.clear();
      inner.unopened.clear();
      std::mem::take(&mut inner.servers)
    };
    events::emit(app, AppEvent::ProblemsChanged);
    let app = app.clone();
    spawn(String::from("lsp-restart"), move || {
      for entry in servers.into_values() {
        if let Slot::Running(server) = entry.slot {
          server.shutdown();
        }
      }
      resync_all(&app);
    });
  }

  /// Shuts every server down, waiting for them to exit.
  pub fn shutdown_all(&self) {
    let servers = {
      let mut inner = self.lock();
      inner.shutting_down = true;
      inner.documents.clear();
      std::mem::take(&mut inner.servers)
    };
    let handles: Vec<_> = servers
      .into_values()
      .filter_map(|entry| match entry.slot {
        Slot::Running(server) => Some(thread::spawn(move || server.shutdown())),
        _ => None,
      })
      .collect();
    for handle in handles {
      let _ = handle.join();
    }
  }
}

/// Whether two configurations of a server differ only in its settings,
/// which a running server can be given.
fn same_but_settings(a: &ServerConfig, b: &ServerConfig) -> bool {
  a.name == b.name
    && a.command == b.command
    && a.languages == b.languages
    && a.root_markers == b.root_markers
}

/// Whether the server `name` should go on waiting for its extension to
/// activate; the first time, makes sure documents are synced again once the
/// wait is over, whether or not the extension says so.
fn hold(app: &AppHandle, inner: &mut Inner, name: &str) -> bool {
  if !inner.held.contains(name) {
    return false;
  }
  match inner.held_since.get(name) {
    Some(since) => since.elapsed() < HOLD_LIMIT,
    None => {
      inner.held_since.insert(name.to_owned(), Instant::now());
      tracing::info!(server = %name, "language server waits for its extension to activate");
      let app = app.clone();
      spawn(String::from("lsp-hold"), move || {
        thread::sleep(HOLD_LIMIT);
        resync_all(&app);
      });
      true
    }
  }
}

/// Removes the servers whose keys `which` picks, with their documents and
/// diagnostics: the servers to shut down, and the documents that lost
/// diagnostics.
fn take_servers(
  inner: &mut Inner,
  which: impl Fn(&ServerKey) -> bool,
) -> (Vec<ServerEntry>, Vec<u64>) {
  let keys: Vec<ServerKey> = inner
    .servers
    .keys()
    .filter(|key| which(key))
    .cloned()
    .collect();
  let mut stopped = Vec::new();
  for key in &keys {
    if let Some(entry) = inner.servers.remove(key) {
      stopped.push(entry);
    }
    inner.unopened.remove(key);
  }
  inner.waiting.retain(|_, (_, _, key)| !which(key));
  let affected: Vec<DocumentId> = inner
    .documents
    .iter()
    .filter(|(_, open)| which(&open.key))
    .map(|(&id, _)| id)
    .collect();
  for id in &affected {
    inner.documents.remove(id);
  }
  (stopped, affected.into_iter().map(DocumentId::get).collect())
}

/// Shuts down servers taken out, tells the UI, and syncs documents so the
/// servers they want start (again); or only syncs them, with `resync`.
fn after_stopping(app: &AppHandle, stopped: Vec<ServerEntry>, affected: Vec<u64>, resync: bool) {
  if stopped.is_empty() && !resync {
    return;
  }
  if !stopped.is_empty() {
    events::emit(app, AppEvent::LanguageServerChanged);
    events::emit(app, AppEvent::ProblemsChanged);
    for document_id in affected {
      events::emit(app, AppEvent::DiagnosticsChanged { document_id });
    }
  }
  let app = app.clone();
  spawn(String::from("lsp-configure"), move || {
    for entry in stopped {
      if let Slot::Running(server) = entry.slot {
        server.shutdown();
      }
    }
    resync_all(&app);
  });
}

fn same_path(a: &Path, b: &Path) -> bool {
  if cfg!(windows) {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
  } else {
    a == b
  }
}

fn spawn(name: String, work: impl FnOnce() + Send + 'static) {
  if let Err(error) = thread::Builder::new().name(name).spawn(work) {
    tracing::error!(%error, "could not start a language server thread");
  }
}

/// Syncs every open document, e.g. to open them on a server that started.
fn resync_all(app: &AppHandle) {
  let state = app.state::<EditorState>();
  let editor = state.lock();
  let lsp = app.state::<LspState>();
  for document in editor.documents() {
    lsp.sync(app, document);
  }
}

fn start_server(app: &AppHandle, key: ServerKey, config: ServerConfig) {
  let app = app.clone();
  spawn(format!("lsp-start-{}", key.name), move || {
    tracing::info!(server = %key.name, root = %key.root.display(), "starting language server");
    let handler = {
      let (app, key) = (app.clone(), key.clone());
      Arc::new(move |event| on_server_event(&app, &key, event))
    };
    let mut command = config.command.clone();
    if command.args.iter().any(|arg| arg.contains(DOWNLOAD_START)) {
      let downloads = app.state::<DownloadsState>().get();
      command.args = downloads::resolve_args(&key.name, &command.args, |address| {
        downloads
          .as_ref()
          .ok_or_else(|| String::from("downloads aren't set up"))?
          .file(address, downloads::MAX_AGE)
      });
    }
    let result = LanguageServer::start(&key.name, &command, &config.settings, &key.root, handler);
    let lsp = app.state::<LspState>();
    let unwanted = {
      let mut inner = lsp.lock();
      match inner.servers.get_mut(&key) {
        Some(entry) if matches!(entry.slot, Slot::Starting) => {
          entry.slot = match result {
            Ok(server) => Slot::Running(Arc::new(server)),
            Err(error) => {
              tracing::warn!(server = %key.name, %error, "could not start language server");
              Slot::Failed(if error.is_not_found() {
                format!(
                  "{} is not installed (or not on the PATH).",
                  config.command.program
                )
              } else {
                error.to_string()
              })
            }
          };
          None
        }
        // Settings changed or the app is closing.
        _ => result.ok(),
      }
    };
    if let Some(server) = unwanted {
      server.shutdown();
      return;
    }
    events::emit(&app, AppEvent::LanguageServerChanged);
    resync_all(&app);
  });
}

fn on_server_event(app: &AppHandle, key: &ServerKey, event: ServerEvent) {
  let lsp = app.state::<LspState>();
  match event {
    ServerEvent::Notification { method, params } => match method.as_str() {
      "textDocument/publishDiagnostics" => {
        let Ok(params) = serde_json::from_value::<PublishDiagnosticsParams>(params) else {
          return;
        };
        match lsp.on_diagnostics(key, params) {
          Published::Document(document_id) => {
            events::emit(app, AppEvent::DiagnosticsChanged { document_id });
          }
          Published::File => events::emit(app, AppEvent::ProblemsChanged),
          Published::Nothing => {}
        }
      }
      "$/progress" => {
        if lsp.on_progress(key, &params) {
          events::emit(app, AppEvent::LanguageServerChanged);
        }
      }
      "window/showMessage" | "window/logMessage" => {
        let message = params["message"].as_str().unwrap_or_default();
        match params["type"].as_u64() {
          Some(1) => tracing::warn!(server = %key.name, "{message}"),
          _ => tracing::debug!(server = %key.name, "{message}"),
        }
      }
      _ => {}
    },
    ServerEvent::Request { id, method, params } => {
      let Some(server) = lsp.server(key) else {
        return;
      };
      let app = app.clone();
      spawn(format!("lsp-request-{}", key.name), move || {
        let reply = match method.as_str() {
          "workspace/applyEdit" => Ok(apply_edit_request(&app, &server, params)),
          _ => Err((-32601, format!("{method} is not supported"))),
        };
        server.respond(id, reply);
      });
    }
    ServerEvent::Exited => {
      let affected = lsp.on_exit(key);
      events::emit(app, AppEvent::LanguageServerChanged);
      events::emit(app, AppEvent::ProblemsChanged);
      for document_id in affected {
        events::emit(app, AppEvent::DiagnosticsChanged { document_id });
      }
      // Restart for the documents that were open on it.
      let app = app.clone();
      spawn(String::from("lsp-resync"), move || resync_all(&app));
    }
  }
}

/// Applies the edit a server asked for (`workspace/applyEdit`), as when a
/// code action's command runs, and returns the answer.
fn apply_edit_request(
  app: &AppHandle,
  server: &LanguageServer,
  params: serde_json::Value,
) -> serde_json::Value {
  let params: ApplyWorkspaceEditParams = match serde_json::from_value(params) {
    Ok(params) => params,
    Err(error) => {
      return serde_json::json!({ "applied": false, "failureReason": error.to_string() });
    }
  };
  let label = params.label.unwrap_or_else(|| String::from("an edit"));
  let outcome = edits::apply(app, server, params.edit);
  let failure = match &outcome {
    Ok(applied) if applied.failures.is_empty() => None,
    Ok(applied) => Some(applied.failures.join("; ")),
    Err(error) => Some(error.clone()),
  };
  match &failure {
    None => tracing::info!(server = %server.name(), %label, "applied a language server's edit"),
    Some(reason) => {
      tracing::warn!(server = %server.name(), %label, %reason, "a language server's edit failed");
      events::emit(
        app,
        AppEvent::Notice {
          level: events::NoticeLevel::Warning,
          message: format!("{} couldn't apply {label}: {reason}", server.name()),
        },
      );
    }
  }
  serde_json::json!({ "applied": failure.is_none(), "failureReason": failure })
}

/// A diagnostic's message, for hover.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticMessageDto {
  pub severity: u8,
  pub message: String,
  pub source: Option<String>,
}

/// What requests need from an open document, taken under the locks.
struct Target {
  server: Arc<LanguageServer>,
  uri: Uri,
}

impl LspState {
  /// The text of document `id` that `server` was sent as LSP version
  /// `version` (the last one sent if `None`), which its positions refer
  /// to; `None` if it isn't open on `server`, or the version is too old.
  fn sent_text(
    &self,
    id: DocumentId,
    server: &LanguageServer,
    version: Option<i32>,
  ) -> Option<TextSnapshot> {
    let inner = self.lock();
    let open = inner.documents.get(&id)?;
    if !std::ptr::eq(open.server.as_ref(), server) {
      return None;
    }
    match version {
      None => Some(open.synced().clone()),
      Some(version) => open
        .history
        .iter()
        .find(|(sent, _)| *sent == version)
        .map(|(_, text)| text.clone()),
    }
  }

  fn target(&self, id: DocumentId) -> Option<Target> {
    let inner = self.lock();
    let open = inner.documents.get(&id)?;
    open.server.is_alive().then(|| Target {
      server: open.server.clone(),
      uri: open.uri.clone(),
    })
  }

  /// The server running for `key`, if it is.
  fn server(&self, key: &ServerKey) -> Option<Arc<LanguageServer>> {
    match &self.lock().servers.get(key)?.slot {
      Slot::Running(server) if server.is_alive() => Some(server.clone()),
      _ => None,
    }
  }

  /// The servers that are running, for requests about the whole workspace.
  fn running(&self) -> Vec<Arc<LanguageServer>> {
    let inner = self.lock();
    inner
      .servers
      .values()
      .filter_map(|entry| match &entry.slot {
        Slot::Running(server) if server.is_alive() => Some(server.clone()),
        _ => None,
      })
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use flareite_lsp::lsp_types::{
    Diagnostic as LspDiagnostic, Position as LspPosition, Range as LspRange,
  };

  use super::*;

  fn key(name: &str) -> ServerKey {
    ServerKey {
      name: name.into(),
      root: std::env::temp_dir(),
    }
  }

  fn publish(path: &Path, messages: &[&str]) -> PublishDiagnosticsParams {
    PublishDiagnosticsParams {
      uri: path_to_uri(path).expect("uri"),
      diagnostics: messages
        .iter()
        .map(|&message| LspDiagnostic {
          range: LspRange::new(LspPosition::new(2, 4), LspPosition::new(2, 8)),
          severity: Some(DiagnosticSeverity::WARNING),
          message: message.into(),
          ..LspDiagnostic::default()
        })
        .collect(),
      version: None,
    }
  }

  #[test]
  fn keeps_diagnostics_published_for_files_that_are_not_open() {
    let lsp = LspState::default();
    let a = std::env::temp_dir().join("a.rs");
    let b = std::env::temp_dir().join("b.rs");
    assert!(matches!(
      lsp.on_diagnostics(&key("ra"), publish(&a, &["unused", "dead code"])),
      Published::File
    ));
    lsp.on_diagnostics(&key("ra"), publish(&b, &["unused"]));
    lsp.on_diagnostics(&key("other"), publish(&a, &["typo"]));

    let files = lsp.file_diagnostics(&[]);
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].0, a);
    let messages: Vec<&str> = files[0].1.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(messages.len(), 3, "both servers' diagnostics: {messages:?}");
    assert_eq!(
      (files[0].1[0].start, files[0].1[0].severity),
      (Position { line: 2, column: 4 }, 2)
    );

    // An open file's diagnostics are its document's, so it's left out.
    assert_eq!(lsp.file_diagnostics(std::slice::from_ref(&a)).len(), 1);
    // A new publish replaces the server's; an empty one clears them.
    lsp.on_diagnostics(&key("ra"), publish(&a, &[]));
    assert_eq!(lsp.file_diagnostics(&[])[0].1.len(), 1);
  }
}
