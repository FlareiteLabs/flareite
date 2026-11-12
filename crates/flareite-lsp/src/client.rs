//! A running language server: its process, the JSON-RPC connection over
//! stdio, and initialization.
//!
//! Three threads serve each server: one writes queued messages to stdin (so
//! callers never block on a slow server), one reads stdout and routes
//! responses to waiting requests and notifications to the event callback,
//! and one forwards stderr to the log. Requests block their caller until the
//! response arrives, the timeout passes, the caller cancels, or the server
//! exits; abandoned requests are cancelled with `$/cancelRequest`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use lsp_types::notification::Notification;
use lsp_types::request::Request;
use lsp_types::{
  ClientCapabilities, ClientInfo, CodeActionCapabilityResolveSupport, CodeActionClientCapabilities,
  CodeActionKind, CodeActionKindLiteralSupport, CodeActionLiteralSupport,
  CompletionClientCapabilities, CompletionItemCapability, DidChangeWatchedFilesClientCapabilities,
  DocumentSymbolClientCapabilities, DynamicRegistrationClientCapabilities, FailureHandlingKind,
  GeneralClientCapabilities, GotoCapability, HoverClientCapabilities, InitializeParams,
  InsertTextMode, InsertTextModeSupport, MarkupKind, ParameterInformationSettings,
  PositionEncodingKind, PublishDiagnosticsClientCapabilities, RenameClientCapabilities,
  ResourceOperationKind, ServerCapabilities, SignatureHelpClientCapabilities,
  SignatureInformationSettings, SymbolKind, SymbolKindCapability, SymbolTag, TagSupport,
  TextDocumentClientCapabilities, TextDocumentSyncCapability, TextDocumentSyncClientCapabilities,
  TextDocumentSyncKind, WindowClientCapabilities, WorkspaceClientCapabilities,
  WorkspaceEditClientCapabilities, WorkspaceFolder, WorkspaceSymbolClientCapabilities,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::convert::{PositionEncoding, path_to_uri};
use crate::session::{FileChange, Session};
use flareite_process::{ProcessGroup as ServerProcess, resolve_program};

use crate::framing::{read_message, write_message};

/// How long a server may take to answer `initialize`.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long shutdown waits for a server to exit before killing it.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// How often a waiting request checks whether its caller gave up.
const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Longest stderr line forwarded to the log.
const MAX_LOG_LINE: usize = 2_000;

/// JSON-RPC error code for a request the server cancelled.
pub const REQUEST_CANCELLED: i64 = -32800;
/// JSON-RPC error code for a request invalidated by a document change.
pub const CONTENT_MODIFIED: i64 = -32801;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
  #[error("could not start {program}: {source}")]
  Spawn {
    program: String,
    source: std::io::Error,
  },
  #[error("the language server exited")]
  Exited,
  #[error("the language server did not respond in time")]
  Timeout,
  #[error("the request was cancelled")]
  Cancelled,
  #[error("the language server reported an error ({code}): {message}")]
  Server { code: i64, message: String },
  #[error("unexpected message from the language server: {0}")]
  Protocol(String),
}

impl LspError {
  /// Whether the program could not be found, i.e. is not installed.
  pub fn is_not_found(&self) -> bool {
    matches!(self, Self::Spawn { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
  }
}

/// What a server tells the client outside of responses.
#[derive(Debug, Clone)]
pub enum ServerEvent {
  Notification {
    method: String,
    params: Value,
  },
  /// A request only the application can answer, such as
  /// `workspace/applyEdit`. It must answer with
  /// [`LanguageServer::respond`], once, with `id`; the server may be waiting
  /// for it to answer one of the client's own requests.
  Request {
    id: Value,
    method: String,
    params: Value,
  },
  /// The server's output closed: it exited or crashed.
  Exited,
}

/// Requests from servers that are passed on as [`ServerEvent::Request`].
const APPLICATION_REQUESTS: &[&str] = &["workspace/applyEdit"];

pub type EventHandler = Arc<dyn Fn(ServerEvent) + Send + Sync>;

/// How to start a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommand {
  pub program: String,
  pub args: Vec<String>,
}

type Reply = Result<Value, LspError>;

#[derive(Default)]
struct Pending(Mutex<HashMap<i64, mpsc::Sender<Reply>>>);

impl Pending {
  fn lock(&self) -> MutexGuard<'_, HashMap<i64, mpsc::Sender<Reply>>> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Messages for the server's stdin. Its one sender is shared, so closing it
/// closes stdin once the queued messages are written.
struct Outgoing(Mutex<Option<mpsc::Sender<Vec<u8>>>>);

impl Outgoing {
  fn send(&self, body: Vec<u8>) -> Result<(), LspError> {
    match self
      .0
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
    {
      Some(sender) => sender.send(body).map_err(|_| LspError::Exited),
      None => Err(LspError::Exited),
    }
  }

  fn close(&self) {
    self.0.lock().unwrap_or_else(PoisonError::into_inner).take();
  }
}

pub struct LanguageServer {
  name: String,
  outgoing: Arc<Outgoing>,
  pending: Arc<Pending>,
  next_id: AtomicI64,
  alive: Arc<AtomicBool>,
  process: Mutex<ServerProcess>,
  capabilities: ServerCapabilities,
  encoding: PositionEncoding,
  session: Arc<Session>,
}

impl LanguageServer {
  /// Starts a server for the workspace at `root` with `settings` (a JSON
  /// object, or null for none) and initializes it. Blocks until the server
  /// answers `initialize`.
  pub fn start(
    name: &str,
    command: &ServerCommand,
    settings: &Value,
    root: &Path,
    on_event: EventHandler,
  ) -> Result<Self, LspError> {
    let spawn_error = |source| LspError::Spawn {
      program: command.program.clone(),
      source,
    };
    let program = resolve_program(&command.program);
    let mut process = Command::new(&program);
    process
      .args(&command.args)
      .current_dir(root)
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());
    let mut server_process = ServerProcess::spawn(&mut process).map_err(spawn_error)?;
    let child = server_process.child();
    let missing = || spawn_error(std::io::Error::other("missing stdio pipe"));
    let stdin = child.stdin.take().ok_or_else(missing)?;
    let stdout = child.stdout.take().ok_or_else(missing)?;
    let stderr = child.stderr.take().ok_or_else(missing)?;

    let (sender, queue) = mpsc::channel::<Vec<u8>>();
    let outgoing = Arc::new(Outgoing(Mutex::new(Some(sender))));
    let pending = Arc::new(Pending::default());
    let alive = Arc::new(AtomicBool::new(true));
    let session = Arc::new(Session::new(settings));
    spawn_thread(format!("lsp-write-{name}"), move || {
      write_loop(stdin, queue)
    });
    {
      let (name, pending, alive, outgoing, session) = (
        name.to_owned(),
        pending.clone(),
        alive.clone(),
        outgoing.clone(),
        session.clone(),
      );
      spawn_thread(format!("lsp-read-{name}"), move || {
        read_loop(&name, stdout, &pending, &outgoing, &session, &on_event);
        alive.store(false, Ordering::SeqCst);
        for (_, reply) in pending.lock().drain() {
          let _ = reply.send(Err(LspError::Exited));
        }
        on_event(ServerEvent::Exited);
      });
    }
    {
      let name = name.to_owned();
      spawn_thread(format!("lsp-log-{name}"), move || {
        for line in BufReader::new(stderr).lines() {
          let Ok(mut line) = line else {
            break;
          };
          if line.len() > MAX_LOG_LINE {
            let mut end = MAX_LOG_LINE;
            while !line.is_char_boundary(end) {
              end -= 1;
            }
            line.truncate(end);
          }
          tracing::debug!(server = %name, "{line}");
        }
      });
    }

    let mut server = Self {
      name: name.to_owned(),
      outgoing,
      pending,
      next_id: AtomicI64::new(1),
      alive,
      process: Mutex::new(server_process),
      capabilities: ServerCapabilities::default(),
      encoding: PositionEncoding::Utf16,
      session,
    };
    let result = match server.request::<lsp_types::request::Initialize>(
      initialize_params(root),
      INITIALIZE_TIMEOUT,
      || false,
    ) {
      Ok(result) => result,
      Err(error) => {
        server.kill();
        return Err(error);
      }
    };
    server.encoding = match result.capabilities.position_encoding {
      Some(ref kind) if *kind == PositionEncodingKind::UTF8 => PositionEncoding::Utf8,
      _ => PositionEncoding::Utf16,
    };
    server.capabilities = result.capabilities;
    server.notify::<lsp_types::notification::Initialized>(lsp_types::InitializedParams {});
    tracing::info!(
      server = %name,
      version = ?result.server_info.and_then(|info| info.version),
      encoding = server.encoding.label(),
      "language server initialized"
    );
    Ok(server)
  }

  pub fn name(&self) -> &str {
    &self.name
  }

  pub fn capabilities(&self) -> &ServerCapabilities {
    &self.capabilities
  }

  pub fn encoding(&self) -> PositionEncoding {
    self.encoding
  }

  pub fn is_alive(&self) -> bool {
    self.alive.load(Ordering::SeqCst)
  }

  /// Replaces the server's settings, telling it if they changed
  /// (`workspace/didChangeConfiguration`; most servers then ask for the
  /// sections they read).
  pub fn set_settings(&self, settings: &Value) {
    if let Some(expanded) = self.session.set_settings(settings) {
      self.notify_raw(
        "workspace/didChangeConfiguration",
        json!({ "settings": expanded }),
      );
    }
  }

  /// Tells the server about changed files it watches, if any.
  pub fn files_changed(&self, changes: &[(std::path::PathBuf, FileChange)]) {
    if let Some(params) = self.session.watched_changes(changes) {
      self.notify_raw("workspace/didChangeWatchedFiles", params);
    }
  }

  /// How the server wants document changes: `None` if it does not track
  /// documents at all.
  pub fn sync_kind(&self) -> Option<TextDocumentSyncKind> {
    let kind = match &self.capabilities.text_document_sync {
      None => TextDocumentSyncKind::NONE,
      Some(TextDocumentSyncCapability::Kind(kind)) => *kind,
      Some(TextDocumentSyncCapability::Options(options)) => {
        options.change.unwrap_or(TextDocumentSyncKind::NONE)
      }
    };
    (kind != TextDocumentSyncKind::NONE).then_some(kind)
  }

  /// Whether the server wants the text included in `didSave`.
  pub fn wants_text_on_save(&self) -> bool {
    match &self.capabilities.text_document_sync {
      Some(TextDocumentSyncCapability::Options(options)) => match &options.save {
        Some(lsp_types::TextDocumentSyncSaveOptions::SaveOptions(save)) => {
          save.include_text.unwrap_or(false)
        }
        _ => false,
      },
      _ => false,
    }
  }

  fn send(&self, message: &Value) -> Result<(), LspError> {
    let body =
      serde_json::to_vec(message).map_err(|error| LspError::Protocol(error.to_string()))?;
    if !self.is_alive() {
      return Err(LspError::Exited);
    }
    self.outgoing.send(body)
  }

  /// Sends a notification. Failures (the server has exited) are reported
  /// through the event handler, so they are only logged here.
  pub fn notify<N: Notification>(&self, params: N::Params) {
    self.notify_raw(N::METHOD, json!(params));
  }

  pub fn notify_raw(&self, method: &str, params: Value) {
    let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
    if let Err(error) = self.send(&message) {
      tracing::debug!(server = %self.name, method, %error, "notification not sent");
    }
  }

  /// Answers a request the server sent ([`ServerEvent::Request`]): with a
  /// result, or an error code and message.
  pub fn respond(&self, id: Value, reply: Result<Value, (i64, String)>) {
    if let Err(error) = self.send(&response(id, reply)) {
      tracing::debug!(server = %self.name, %error, "response not sent");
    }
  }

  /// Sends a request and waits for its result, until `timeout` passes or
  /// `cancelled` returns true.
  pub fn request<R: Request>(
    &self,
    params: R::Params,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
  ) -> Result<R::Result, LspError>
  where
    R::Result: DeserializeOwned,
  {
    let value = self.request_raw(R::METHOD, json!(params), timeout, cancelled)?;
    serde_json::from_value(value)
      .map_err(|error| LspError::Protocol(format!("invalid {} result: {error}", R::METHOD)))
  }

  pub fn request_raw(
    &self,
    method: &str,
    params: Value,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
  ) -> Result<Value, LspError> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let (reply, response) = mpsc::channel();
    self.pending.lock().insert(id, reply);
    let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    if let Err(error) = self.send(&message) {
      self.pending.lock().remove(&id);
      return Err(error);
    }
    let deadline = Instant::now() + timeout;
    let outcome = loop {
      match response.recv_timeout(POLL_INTERVAL) {
        Ok(reply) => return reply,
        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(LspError::Exited),
        Err(mpsc::RecvTimeoutError::Timeout) => {
          if cancelled() {
            break LspError::Cancelled;
          }
          if Instant::now() >= deadline {
            break LspError::Timeout;
          }
        }
      }
    };
    self.pending.lock().remove(&id);
    self.notify_raw("$/cancelRequest", json!({ "id": id }));
    tracing::debug!(server = %self.name, method, error = %outcome, "request abandoned");
    Err(outcome)
  }

  /// Asks the server to shut down and exit, killing it if it does not.
  pub fn shutdown(&self) {
    let acknowledged = self.is_alive() && {
      let answer = self.request::<lsp_types::request::Shutdown>((), SHUTDOWN_TIMEOUT, || false);
      self.notify::<lsp_types::notification::Exit>(());
      answer.is_ok()
    };
    // Closing stdin also tells the server to go away.
    self.outgoing.close();
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    let mut process = self.process.lock().unwrap_or_else(PoisonError::into_inner);
    while Instant::now() < deadline {
      if process.has_exited() {
        return;
      }
      thread::sleep(POLL_INTERVAL);
    }
    if acknowledged {
      // It has nothing left to do but free its memory, which takes
      // rust-analyzer several seconds on a large workspace.
      tracing::info!(server = %self.name, "language server shut down but is still exiting; stopping it");
    } else {
      tracing::warn!(server = %self.name, "language server did not exit; killing it");
    }
    process.kill();
  }

  fn kill(&self) {
    self
      .process
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .kill();
  }
}

fn spawn_thread(name: String, work: impl FnOnce() + Send + 'static) {
  if let Err(error) = thread::Builder::new().name(name).spawn(work) {
    tracing::error!(%error, "could not start a language server thread");
  }
}

fn write_loop(mut stdin: ChildStdin, queue: mpsc::Receiver<Vec<u8>>) {
  for body in queue {
    if let Err(error) = write_message(&mut stdin, &body) {
      tracing::debug!(%error, "language server input closed");
      break;
    }
  }
}

fn read_loop(
  name: &str,
  stdout: std::process::ChildStdout,
  pending: &Pending,
  outgoing: &Outgoing,
  session: &Session,
  on_event: &EventHandler,
) {
  let mut reader = BufReader::new(stdout);
  loop {
    let body = match read_message(&mut reader) {
      Ok(Some(body)) => body,
      Ok(None) => break,
      Err(error) => {
        tracing::warn!(server = %name, %error, "language server output was unreadable");
        break;
      }
    };
    let message: Value = match serde_json::from_slice(&body) {
      Ok(message) => message,
      Err(error) => {
        tracing::warn!(server = %name, %error, "language server sent invalid JSON");
        continue;
      }
    };
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").filter(|id| !id.is_null());
    match (method, id) {
      // A response to one of our requests.
      (None, Some(id)) => {
        let Some(reply) = id.as_i64().and_then(|id| pending.lock().remove(&id)) else {
          continue;
        };
        let result = match message.get("error") {
          Some(error) => Err(LspError::Server {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
              .get("message")
              .and_then(Value::as_str)
              .unwrap_or_default()
              .to_owned(),
          }),
          None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = reply.send(result);
      }
      // A request for the application, which answers it later: never here,
      // as it may need what this thread delivers.
      (Some(method), Some(id)) if APPLICATION_REQUESTS.contains(&method) => {
        on_event(ServerEvent::Request {
          id: id.clone(),
          method: method.to_owned(),
          params: message.get("params").cloned().unwrap_or(Value::Null),
        });
      }
      // A request from the server.
      (Some(method), Some(id)) => {
        let reply = answer_server_request(session, method, message.get("params"));
        if let Ok(body) = serde_json::to_vec(&response(id.clone(), reply)) {
          let _ = outgoing.send(body);
        }
      }
      (Some(method), None) => on_event(ServerEvent::Notification {
        method: method.to_owned(),
        params: message.get("params").cloned().unwrap_or(Value::Null),
      }),
      (None, None) => {}
    }
  }
}

/// The response to a server's request `id`.
fn response(id: Value, reply: Result<Value, (i64, String)>) -> Value {
  let mut response = json!({ "jsonrpc": "2.0", "id": id });
  match reply {
    Ok(result) => response["result"] = result,
    Err((code, text)) => response["error"] = json!({ "code": code, "message": text }),
  }
  response
}

/// Answers to requests servers send to clients.
fn answer_server_request(
  session: &Session,
  method: &str,
  params: Option<&Value>,
) -> Result<Value, (i64, String)> {
  match method {
    // The configured settings, by section (null where there are none, so
    // the server uses its defaults).
    "workspace/configuration" => Ok(session.configuration(params)),
    // File watchers are kept; other registrations are accepted and ignored.
    "client/registerCapability" => {
      session.register(params);
      Ok(Value::Null)
    }
    "client/unregisterCapability" => {
      session.unregister(params);
      Ok(Value::Null)
    }
    "window/workDoneProgress/create"
    | "window/showMessageRequest"
    | "workspace/semanticTokens/refresh"
    | "workspace/inlayHint/refresh"
    | "workspace/codeLens/refresh"
    | "workspace/diagnostic/refresh" => Ok(Value::Null),
    "workspace/workspaceFolders" => Ok(Value::Null),
    _ => Err((-32601, format!("{method} is not supported"))),
  }
}

#[allow(deprecated)] // `root_uri` is deprecated but still read by some servers.
fn initialize_params(root: &Path) -> InitializeParams {
  let root_uri = path_to_uri(root);
  let markup = vec![MarkupKind::Markdown, MarkupKind::PlainText];
  InitializeParams {
    process_id: Some(std::process::id()),
    root_uri: root_uri.clone(),
    workspace_folders: root_uri.map(|uri| {
      vec![WorkspaceFolder {
        uri,
        name: root.file_name().map_or_else(
          || root.display().to_string(),
          |name| name.to_string_lossy().into_owned(),
        ),
      }]
    }),
    client_info: Some(ClientInfo {
      name: "Flareite".into(),
      version: Some(env!("CARGO_PKG_VERSION").into()),
    }),
    capabilities: ClientCapabilities {
      general: Some(GeneralClientCapabilities {
        position_encodings: Some(vec![
          PositionEncodingKind::UTF8,
          PositionEncodingKind::UTF16,
        ]),
        ..Default::default()
      }),
      window: Some(WindowClientCapabilities {
        work_done_progress: Some(true),
        ..Default::default()
      }),
      workspace: Some(WorkspaceClientCapabilities {
        apply_edit: Some(true),
        workspace_edit: Some(WorkspaceEditClientCapabilities {
          document_changes: Some(true),
          resource_operations: Some(vec![
            ResourceOperationKind::Create,
            ResourceOperationKind::Rename,
            ResourceOperationKind::Delete,
          ]),
          // Every edit is checked before any is made; files then change
          // one at a time.
          failure_handling: Some(FailureHandlingKind::TextOnlyTransactional),
          normalizes_line_endings: Some(true),
          ..Default::default()
        }),
        execute_command: Some(DynamicRegistrationClientCapabilities::default()),
        configuration: Some(true),
        did_change_configuration: Some(DynamicRegistrationClientCapabilities::default()),
        did_change_watched_files: Some(DidChangeWatchedFilesClientCapabilities {
          dynamic_registration: Some(true),
          relative_pattern_support: Some(true),
        }),
        symbol: Some(WorkspaceSymbolClientCapabilities {
          symbol_kind: Some(symbol_kinds()),
          tag_support: Some(symbol_tags()),
          ..Default::default()
        }),
        ..Default::default()
      }),
      text_document: Some(TextDocumentClientCapabilities {
        synchronization: Some(TextDocumentSyncClientCapabilities {
          did_save: Some(true),
          ..Default::default()
        }),
        hover: Some(HoverClientCapabilities {
          content_format: Some(markup.clone()),
          ..Default::default()
        }),
        completion: Some(CompletionClientCapabilities {
          completion_item: Some(CompletionItemCapability {
            snippet_support: Some(true),
            documentation_format: Some(markup.clone()),
            insert_text_mode_support: Some(InsertTextModeSupport {
              value_set: vec![InsertTextMode::AS_IS, InsertTextMode::ADJUST_INDENTATION],
            }),
            ..Default::default()
          }),
          context_support: Some(true),
          ..Default::default()
        }),
        signature_help: Some(SignatureHelpClientCapabilities {
          signature_information: Some(SignatureInformationSettings {
            documentation_format: Some(markup),
            parameter_information: Some(ParameterInformationSettings {
              label_offset_support: Some(true),
            }),
            active_parameter_support: Some(true),
          }),
          context_support: Some(true),
          ..Default::default()
        }),
        formatting: Some(DynamicRegistrationClientCapabilities::default()),
        range_formatting: Some(DynamicRegistrationClientCapabilities::default()),
        rename: Some(RenameClientCapabilities {
          prepare_support: Some(true),
          ..Default::default()
        }),
        code_action: Some(CodeActionClientCapabilities {
          code_action_literal_support: Some(CodeActionLiteralSupport {
            code_action_kind: CodeActionKindLiteralSupport {
              value_set: [
                CodeActionKind::EMPTY,
                CodeActionKind::QUICKFIX,
                CodeActionKind::REFACTOR,
                CodeActionKind::REFACTOR_EXTRACT,
                CodeActionKind::REFACTOR_INLINE,
                CodeActionKind::REFACTOR_REWRITE,
                CodeActionKind::SOURCE,
                CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
              ]
              .into_iter()
              .map(|kind| kind.as_str().to_owned())
              .collect(),
            },
          }),
          is_preferred_support: Some(true),
          disabled_support: Some(true),
          data_support: Some(true),
          resolve_support: Some(CodeActionCapabilityResolveSupport {
            properties: vec!["edit".into()],
          }),
          ..Default::default()
        }),
        definition: Some(GotoCapability {
          link_support: Some(false),
          ..Default::default()
        }),
        references: Some(DynamicRegistrationClientCapabilities::default()),
        document_symbol: Some(DocumentSymbolClientCapabilities {
          symbol_kind: Some(symbol_kinds()),
          hierarchical_document_symbol_support: Some(true),
          tag_support: Some(symbol_tags()),
          ..Default::default()
        }),
        publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
          version_support: Some(true),
          ..Default::default()
        }),
        ..Default::default()
      }),
      ..Default::default()
    },
    ..Default::default()
  }
}

/// Every symbol kind: servers fall back to older ones for clients that don't
/// say which they handle.
fn symbol_kinds() -> SymbolKindCapability {
  SymbolKindCapability {
    value_set: Some(vec![
      SymbolKind::FILE,
      SymbolKind::MODULE,
      SymbolKind::NAMESPACE,
      SymbolKind::PACKAGE,
      SymbolKind::CLASS,
      SymbolKind::METHOD,
      SymbolKind::PROPERTY,
      SymbolKind::FIELD,
      SymbolKind::CONSTRUCTOR,
      SymbolKind::ENUM,
      SymbolKind::INTERFACE,
      SymbolKind::FUNCTION,
      SymbolKind::VARIABLE,
      SymbolKind::CONSTANT,
      SymbolKind::STRING,
      SymbolKind::NUMBER,
      SymbolKind::BOOLEAN,
      SymbolKind::ARRAY,
      SymbolKind::OBJECT,
      SymbolKind::KEY,
      SymbolKind::NULL,
      SymbolKind::ENUM_MEMBER,
      SymbolKind::STRUCT,
      SymbolKind::EVENT,
      SymbolKind::OPERATOR,
      SymbolKind::TYPE_PARAMETER,
    ]),
  }
}

fn symbol_tags() -> TagSupport<SymbolTag> {
  TagSupport {
    value_set: vec![SymbolTag::DEPRECATED],
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn answers_configuration_requests_from_settings() {
    let session = Session::new(&json!({ "a.x": 1 }));
    let params = json!({ "items": [{ "section": "a" }, { "section": "b" }] });
    assert_eq!(
      answer_server_request(&session, "workspace/configuration", Some(&params)),
      Ok(json!([{ "x": 1 }, null]))
    );
    assert!(answer_server_request(&session, "custom/unknown", None).is_err());
  }

  #[test]
  fn initialization_prefers_utf8_positions() {
    let params = serde_json::to_value(initialize_params(Path::new("/tmp/project"))).expect("json");
    assert_eq!(
      params["capabilities"]["general"]["positionEncodings"],
      json!(["utf-8", "utf-16"])
    );
    assert_eq!(params["workspaceFolders"][0]["name"], "project");
    assert_eq!(params["capabilities"]["workspace"]["configuration"], true);
    assert_eq!(
      params["capabilities"]["workspace"]["didChangeWatchedFiles"]["dynamicRegistration"],
      true
    );
  }
}
