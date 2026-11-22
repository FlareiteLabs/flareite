//! Test fixture: a Markdown preview and a webview viewer.
//!
//! - `test.pages.preview` and `test.pages.previewToSide` show the document
//!   they're run for in a preview, rendered by the editor
//!   (`workbench::render-markdown`), and keep it up to date as it changes.
//! - The `file` viewer's page asks for its file's size (`{"type":"size"}`);
//!   the extension answers with `{"size":<bytes>}`.
//! - The editable `bytes` viewer's page asks it to change the file (2.2):
//!   `edit` sets the second byte to 0xAB (`{"version":<v>,"dirty":<bool>}`),
//!   `stale` edits at a version long gone (`{"error":"stale <current>"}`),
//!   `undo` undoes (`{"undone":[<offset>,<length>]}` or `{"undone":null}`),
//!   and `read` answers with the bytes in hex (`{"bytes":"00ab"}`).

use std::cell::RefCell;
use std::collections::BTreeSet;

use flareite_extension_api::events::Event;
use flareite_extension_api::types::{
  Activation, ApiError, CommandContext, Diagnostic, DocumentId, DocumentInfo, Position,
  PreviewOptions, Range, Severity, Webview, WebviewMessage,
};
use flareite_extension_api::{Extension, host, register, viewers, webviews, workbench};

const PREVIEW: &str = "preview";

thread_local! {
  /// Documents whose previews are open.
  static PREVIEWED: RefCell<BTreeSet<DocumentId>> = const { RefCell::new(BTreeSet::new()) };
}

fn describe(error: ApiError) -> String {
  format!("{error:?}")
}

fn file_name(info: &DocumentInfo) -> &str {
  info
    .path
    .as_deref()
    .and_then(|path| path.rsplit(['/', '\\']).next())
    .unwrap_or("Untitled")
}

/// Renders a document into its preview, and forgets the preview if it was
/// closed.
fn update(id: DocumentId) -> Result<(), ApiError> {
  let info = host::document(id)?;
  // Test documents are small enough for one read.
  let mut text = String::new();
  for line in host::read_lines(id, 0, info.line_count)?.lines {
    text.push_str(&line);
    text.push('\n');
  }
  let nodes = workbench::render_markdown(&text)?;
  if !workbench::set_preview_content(PREVIEW, &id.to_string(), &nodes)? {
    PREVIEWED.with_borrow_mut(|previewed| previewed.remove(&id));
  }
  Ok(())
}

fn preview(context: &CommandContext, beside: bool) -> Result<(), ApiError> {
  let Some(id) = context.document else {
    return Err(ApiError::InvalidArgument("no document".into()));
  };
  let info = host::document(id)?;
  let options = PreviewOptions {
    title: format!("Preview {}", file_name(&info)),
    document: Some(id),
    beside,
  };
  workbench::show_preview(PREVIEW, &id.to_string(), &options)?;
  PREVIEWED.with_borrow_mut(|previewed| previewed.insert(id));
  update(id)
}

/// Answers a viewer page's request: its file's size, or a change to it.
fn answer(message: &WebviewMessage) -> Result<(), ApiError> {
  let Webview::Viewer(viewed) = &message.source else {
    return Err(ApiError::InvalidArgument("not a viewer".into()));
  };
  let (viewer, path) = (&viewed.viewer, &viewed.path);
  let kind = |name: &str| message.data.contains(&format!(r#""type":"{name}""#));
  let reply = if kind("edit") {
    let version = viewers::viewed_file_status(viewer, path)?.version;
    let change = viewers::ByteChange {
      offset: 1,
      removed: 1,
      inserted: vec![0xAB],
    };
    let state = viewers::edit_viewed_file(viewer, path, version, &[change])?;
    format!(r#"{{"version":{},"dirty":{}}}"#, state.version, state.dirty)
  } else if kind("stale") {
    match viewers::edit_viewed_file(viewer, path, 1000, &[]) {
      Err(ApiError::StaleVersion(current)) => format!(r#"{{"error":"stale {current}"}}"#),
      other => format!(r#"{{"error":"{other:?}"}}"#),
    }
  } else if kind("undo") {
    match viewers::undo_viewed_file(viewer, path)? {
      Some(step) => format!(r#"{{"undone":[{},{}]}}"#, step.offset, step.length),
      None => r#"{"undone":null}"#.into(),
    }
  } else if kind("read") {
    let file = viewers::read_viewed_file(viewer, path, 0, 64)?;
    let hex: String = file
      .bytes
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect();
    format!(r#"{{"bytes":"{hex}"}}"#)
  } else {
    let file = viewers::read_viewed_file(viewer, path, 0, 0)?;
    format!(r#"{{"size":{}}}"#, file.size)
  };
  webviews::post_message(&message.source, &reply)?;
  Ok(())
}

struct Pages;

impl Extension for Pages {
  fn activate(_context: Activation) -> Result<Self, String> {
    Ok(Pages)
  }

  fn run_command(&mut self, command: &str, context: CommandContext) -> Result<(), String> {
    match command {
      "test.pages.preview" => preview(&context, false),
      "test.pages.previewToSide" => preview(&context, true),
      "test.pages.fileProblems" => {
        let path = context.argument.as_deref().unwrap_or_default();
        let problem = Diagnostic {
          range: Range {
            start: Position { line: 1, column: 2 },
            end: Position {
              line: 1,
              column: 400,
            },
          },
          severity: Severity::Error,
          message: "built badly".into(),
          code: Some("e1".into()),
        };
        host::publish_file_diagnostics(path, &[problem])
      }
      "test.pages.clearProblems" => {
        host::clear_file_diagnostics();
        Ok(())
      }
      other => return Err(format!("unknown command {other}")),
    }
    .map_err(describe)
  }

  fn handle_event(&mut self, event: Event) -> Result<(), String> {
    match event {
      Event::DocumentChanged(change) => {
        if PREVIEWED.with_borrow(|previewed| previewed.contains(&change.document)) {
          let _ = update(change.document);
        }
      }
      Event::DocumentClosed(document) => {
        PREVIEWED.with_borrow_mut(|previewed| previewed.remove(&document));
      }
      Event::WebviewMessage(message) => answer(&message).map_err(describe)?,
      _ => {}
    }
    Ok(())
  }
}

register!(Pages);
