//! The client against a scripted server (`src/bin/fake_lsp.rs`).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flareite_lsp::FileChange;
use flareite_lsp::convert::{content_changes, path_to_uri, to_offsets, to_position};
use flareite_lsp::lsp_types::{
  self, DidChangeTextDocumentParams, DidOpenTextDocumentParams, HoverContents, HoverParams,
  PublishDiagnosticsParams, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams,
  Uri, VersionedTextDocumentIdentifier,
};
use flareite_lsp::{LanguageServer, LspError, PositionEncoding, ServerCommand, ServerEvent};
use flareite_text::{ChangeSet, Edit, TextBuffer};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Harness {
  server: LanguageServer,
  events: mpsc::Receiver<ServerEvent>,
  uri: Uri,
  _root: tempfile::TempDir,
}

fn start(args: &[&str]) -> Harness {
  let root = tempfile::tempdir().expect("temp dir");
  let (sender, events) = mpsc::channel();
  let sender = Mutex::new(sender);
  let command = ServerCommand {
    program: env!("CARGO_BIN_EXE_flareite-fake-lsp").to_owned(),
    args: args.iter().map(|&arg| arg.to_owned()).collect(),
  };
  let server = LanguageServer::start(
    "fake",
    &command,
    &json!({ "fake.answer": 1 }),
    root.path(),
    Arc::new(move |event| {
      let _ = sender.lock().expect("lock").send(event);
    }),
  )
  .expect("starts");
  let uri = path_to_uri(&root.path().join("main.rs")).expect("uri");
  Harness {
    server,
    events,
    uri,
    _root: root,
  }
}

impl Harness {
  fn open(&self, text: &str) {
    self
      .server
      .notify::<lsp_types::notification::DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(self.uri.clone(), "rust".into(), 1, text.into()),
      });
  }

  fn server_text(&self) -> Option<String> {
    let text = self
      .server
      .request_raw("fake/text", json!({ "uri": self.uri }), TIMEOUT, || false)
      .expect("answers");
    serde_json::from_value(text).expect("string or null")
  }

  /// The params of the next notification of `method`.
  fn next_notification(&self, method: &str) -> Value {
    loop {
      match self.events.recv_timeout(TIMEOUT).expect("an event") {
        ServerEvent::Notification { method: m, params } if m == method => return params,
        _ => {}
      }
    }
  }

  fn next_diagnostics(&self) -> PublishDiagnosticsParams {
    loop {
      match self.events.recv_timeout(TIMEOUT).expect("an event") {
        ServerEvent::Notification { method, params }
          if method == "textDocument/publishDiagnostics" =>
        {
          return serde_json::from_value(params).expect("diagnostics");
        }
        _ => {}
      }
    }
  }

  fn hover(&self, buffer: &TextBuffer, offset: usize) -> Result<String, LspError> {
    let hover = self.server.request::<lsp_types::request::HoverRequest>(
      HoverParams {
        text_document_position_params: TextDocumentPositionParams::new(
          TextDocumentIdentifier::new(self.uri.clone()),
          to_position(buffer, offset, self.server.encoding()),
        ),
        work_done_progress_params: Default::default(),
      },
      TIMEOUT,
      || false,
    )?;
    match hover.map(|hover| hover.contents) {
      Some(HoverContents::Markup(markup)) => Ok(markup.value),
      other => panic!("unexpected hover {other:?}"),
    }
  }
}

/// Edits `buffer` and sends the change incrementally.
fn edit_and_sync(harness: &Harness, buffer: &mut TextBuffer, version: i32, edits: Vec<Edit>) {
  let synced = buffer.snapshot();
  buffer
    .apply(&ChangeSet::new(edits).expect("valid"))
    .expect("applies");
  let edits = buffer.edits_since(synced.version()).expect("edits");
  harness
    .server
    .notify::<lsp_types::notification::DidChangeTextDocument>(DidChangeTextDocumentParams {
      text_document: VersionedTextDocumentIdentifier::new(harness.uri.clone(), version),
      content_changes: content_changes(&synced, &edits, harness.server.encoding()),
    });
}

fn check_sync(args: &[&str], encoding: PositionEncoding) {
  let harness = start(args);
  assert_eq!(harness.server.encoding(), encoding);
  let mut buffer = TextBuffer::from_text("fn 🦀() {}\nlet é = 1;\n");
  harness.open(&buffer.contents());
  assert!(harness.next_diagnostics().diagnostics.is_empty());

  edit_and_sync(
    &harness,
    &mut buffer,
    2,
    vec![
      Edit::insert(3, "a"),
      Edit::new(7..8, "_b("),
      Edit::insert(13, " error"),
    ],
  );
  edit_and_sync(&harness, &mut buffer, 3, vec![Edit::insert(0, "é\n")]);
  assert_eq!(
    harness.server_text().as_deref(),
    Some(buffer.contents().as_str())
  );

  harness.next_diagnostics();
  let diagnostics = harness.next_diagnostics();
  assert_eq!(diagnostics.version, Some(3));
  assert_eq!(diagnostics.diagnostics.len(), 1);
  let range = to_offsets(&buffer, diagnostics.diagnostics[0].range, encoding);
  assert_eq!(buffer.text_in(range), "error");

  let crab = buffer.contents().find("_b").expect("found");
  assert_eq!(
    harness.hover(&buffer, crab + 1).expect("hovers"),
    "word `_b`"
  );
  harness.server.shutdown();
  let after = harness
    .server
    .request_raw("fake/text", json!({}), TIMEOUT, || false);
  assert!(matches!(after, Err(LspError::Exited)));
}

#[test]
fn syncs_documents_with_utf8_positions() {
  check_sync(&[], PositionEncoding::Utf8);
}

#[test]
fn syncs_documents_with_utf16_positions() {
  check_sync(&["--utf16"], PositionEncoding::Utf16);
}

#[test]
fn lists_document_and_workspace_symbols() {
  let harness = start(&[]);
  harness.open("fn é_ok() {}\nfn main() {}\n");
  harness.next_diagnostics();
  let document = harness
    .server
    .request::<lsp_types::request::DocumentSymbolRequest>(
      lsp_types::DocumentSymbolParams {
        text_document: TextDocumentIdentifier::new(harness.uri.clone()),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
      },
      TIMEOUT,
      || false,
    )
    .expect("answers");
  let Some(lsp_types::DocumentSymbolResponse::Nested(symbols)) = document else {
    panic!("unexpected symbols {document:?}");
  };
  let names: Vec<_> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
  assert_eq!(names, ["é_ok", "main"]);
  assert_eq!(
    symbols[1].selection_range.start,
    lsp_types::Position::new(1, 3)
  );

  let workspace = harness
    .server
    .request::<lsp_types::request::WorkspaceSymbolRequest>(
      lsp_types::WorkspaceSymbolParams {
        query: "ma".into(),
        ..Default::default()
      },
      TIMEOUT,
      || false,
    )
    .expect("answers");
  let Some(lsp_types::WorkspaceSymbolResponse::Flat(symbols)) = workspace else {
    panic!("unexpected symbols {workspace:?}");
  };
  assert_eq!(symbols.len(), 1);
  assert_eq!(symbols[0].name, "main");
  assert_eq!(symbols[0].location.uri, harness.uri);
}

#[test]
fn requests_time_out_or_are_cancelled_without_breaking_the_connection() {
  let harness = start(&[]);
  let slow = json!({ "ms": 400 });
  let timed_out = harness.server.request_raw(
    "fake/sleep",
    slow.clone(),
    Duration::from_millis(50),
    || false,
  );
  assert!(matches!(timed_out, Err(LspError::Timeout)));
  let cancelled = harness
    .server
    .request_raw("fake/sleep", slow, TIMEOUT, || true);
  assert!(matches!(cancelled, Err(LspError::Cancelled)));
  // The late responses are ignored and later requests still work.
  assert_eq!(harness.server_text(), None);
  let unknown = harness
    .server
    .request_raw("fake/unknown", json!({}), TIMEOUT, || false);
  assert!(matches!(
    unknown,
    Err(LspError::Server { code: -32601, .. })
  ));
}

#[test]
fn crashes_fail_pending_requests_and_are_reported() {
  let harness = start(&["--crash-on-hover"]);
  let buffer = TextBuffer::from_text("x");
  harness.open("x");
  assert!(matches!(harness.hover(&buffer, 0), Err(LspError::Exited)));
  let exited = std::iter::from_fn(|| harness.events.recv_timeout(TIMEOUT).ok())
    .any(|event| matches!(event, ServerEvent::Exited));
  assert!(exited);
  assert!(!harness.server.is_alive());
}

#[test]
fn shutdown_closes_the_input_of_servers_that_ignore_exit() {
  let harness = start(&["--ignore-exit"]);
  let started = Instant::now();
  harness.server.shutdown();
  // Well before shutdown gives up and kills it.
  assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn killing_a_server_stops_the_processes_it_started() {
  let harness = start(&["--stubborn"]);
  harness.server.shutdown();
  // The server's output closes only once its lingering process is gone too.
  let exited = std::iter::from_fn(|| harness.events.recv_timeout(TIMEOUT).ok())
    .any(|event| matches!(event, ServerEvent::Exited));
  assert!(exited);
}

#[test]
fn missing_programs_are_reported_as_not_installed() {
  let root = tempfile::tempdir().expect("temp dir");
  let command = ServerCommand {
    program: "flareite-no-such-language-server".into(),
    args: Vec::new(),
  };
  let error = LanguageServer::start(
    "missing",
    &command,
    &Value::Null,
    root.path(),
    Arc::new(|_| {}),
  )
  .err()
  .expect("fails");
  assert!(error.is_not_found(), "{error}");
}

#[test]
fn settings_and_watched_files_reach_the_server() {
  let harness = start(&[]);
  // A round trip: the server registered its watchers before answering.
  assert_eq!(harness.server_text(), None);

  harness
    .server
    .set_settings(&json!({ "fake": { "answer": 2 }, "other": true }));
  assert_eq!(
    harness.next_notification("fake/configured"),
    json!([{ "answer": 2 }])
  );

  let root = harness._root.path();
  harness.server.files_changed(&[
    (root.join("a.txt"), FileChange::Changed),
    (root.join("src").join("b.watched"), FileChange::Created),
    (root.join("c.watched"), FileChange::Deleted),
  ]);
  let changes = harness.next_notification("fake/watched")["changes"].clone();
  let changes: Vec<(String, u64)> = changes
    .as_array()
    .expect("changes")
    .iter()
    .map(|change| {
      let uri = change["uri"].as_str().expect("uri");
      (
        uri.rsplit('/').next().expect("name").to_owned(),
        change["type"].as_u64().expect("type"),
      )
    })
    .collect();
  assert_eq!(
    changes,
    [("b.watched".to_owned(), 1), ("c.watched".to_owned(), 3)]
  );
}
