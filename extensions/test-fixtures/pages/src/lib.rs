//! Test fixture: a Markdown preview and a webview viewer.
//!
//! - `test.pages.preview` and `test.pages.previewToSide` show the document
//!   they're run for in a preview, rendered by the editor
//!   (`workbench::render-markdown`), and keep it up to date as it changes.
//! - The `file` viewer's page asks for its file's size; `test.pages.size`
//!   answers with `{"size":<bytes>}`.

use std::cell::RefCell;
use std::collections::BTreeSet;

use flareite_extension_api::events::Event;
use flareite_extension_api::types::{
  Activation, ApiError, CommandContext, DocumentId, DocumentInfo, PreviewOptions, Webview,
  WebviewMessage,
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

/// Answers a viewer page's request for its file's size.
fn size(message: &WebviewMessage) -> Result<(), ApiError> {
  let Webview::Viewer(viewed) = &message.source else {
    return Err(ApiError::InvalidArgument("not a viewer".into()));
  };
  let file = viewers::read_viewed_file(&viewed.viewer, &viewed.path, 0, 0)?;
  webviews::post_message(&message.source, &format!(r#"{{"size":{}}}"#, file.size))?;
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
      Event::WebviewMessage(message) => size(&message).map_err(describe)?,
      _ => {}
    }
    Ok(())
  }
}

register!(Pages);
