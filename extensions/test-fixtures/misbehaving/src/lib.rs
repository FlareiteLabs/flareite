//! A test fixture: an extension that breaks rules and exceeds limits when
//! asked to. The host's integration tests run its commands and check that
//! the host stays healthy. Outcomes are reported with `show_message`.

use flareite_extension_api::events::Event;
use flareite_extension_api::language_features::{CodeAction, CodeActionRequest, FormattingRequest};
use flareite_extension_api::types::{
  Activation, CommandContext, ConfigValue, Diagnostic, DocumentChange, LogLevel, MessageLevel,
  Position, PreviewElement, PreviewNode, Range, Severity, StatusItem, TextEdit, ViewItem,
};
use flareite_extension_api::{Extension, host, register, viewers, workbench};

fn text_setting(key: &str) -> String {
  match host::configuration(key) {
    Some(ConfigValue::Text(text)) => text,
    other => format!("{other:?}"),
  }
}

fn report(message: impl AsRef<str>) {
  host::show_message(MessageLevel::Info, message.as_ref());
}

fn at(line: u32, column: u32) -> Position {
  Position { line, column }
}

fn edit(start: Position, end: Position, text: &str) -> TextEdit {
  TextEdit {
    range: Range { start, end },
    text: text.into(),
  }
}

#[allow(unconditional_recursion)]
fn recurse(depth: u64) -> u64 {
  recurse(depth + 1).wrapping_add(std::hint::black_box(depth))
}

fn spin() -> ! {
  loop {
    std::hint::black_box(());
  }
}

struct Misbehaving;

/// Fills the `bytes` viewer in with the file's size, as preview content.
fn show_bytes(path: &str) -> Result<(), String> {
  let read = viewers::read_viewed_file("bytes", path, 0, 0).map_err(|e| format!("{e:?}"))?;
  let nodes = [
    PreviewNode {
      parent: None,
      element: PreviewElement::Paragraph,
    },
    PreviewNode {
      parent: Some(0),
      element: PreviewElement::Text(format!("{} bytes", read.size)),
    },
  ];
  viewers::set_viewer_content("bytes", path, &nodes).map_err(|e| format!("{e:?}"))?;
  Ok(())
}

fn changed(change: DocumentChange) {
  if text_setting("on_change") == "loop" {
    spin();
  }
  let changes = match change.changes {
    Some(changes) => changes
      .iter()
      .map(|c| {
        let (start, end) = (c.range.start, c.range.end);
        format!(
          "{}:{}-{}:{}={:?}",
          start.line, start.column, end.line, end.column, c.text
        )
      })
      .collect::<Vec<_>>()
      .join(","),
    None => "reread".into(),
  };
  report(format!(
    "changed {} v{} [{changes}]",
    change.document, change.version
  ));
}

impl Extension for Misbehaving {
  fn activate(context: Activation) -> Result<Self, String> {
    match text_setting("on_activate").as_str() {
      "error" => Err("activation refused".into()),
      "loop" => spin(),
      _ => {
        report(format!("activated with {:?}", context.granted));
        Ok(Misbehaving)
      }
    }
  }

  fn run_command(&mut self, command: &str, context: CommandContext) -> Result<(), String> {
    let document = context.document.unwrap_or(0);
    let version = host::document(document).map_or(0, |info| info.version);
    match command.trim_start_matches("test.misbehaving.") {
      "loop" => spin(),
      "alloc" => {
        let mut blocks = Vec::new();
        loop {
          blocks.push(vec![1u8; 1 << 20]);
          std::hint::black_box(&blocks);
        }
      }
      "panic" => panic!("deliberate panic"),
      "recurse" => {
        std::hint::black_box(recurse(0));
      }
      "escape" => {
        let file = std::fs::read_to_string("Cargo.toml").map(|text| text.len());
        let env = std::env::var("PATH");
        let args = std::env::args().count();
        report(format!("file: {file:?}; env: {env:?}; args: {args}"));
      }
      "edit" => {
        let result = host::apply_edits(document, version, &[edit(at(0, 0), at(0, 0), "X")]);
        report(format!("edit: {result:?}"));
      }
      "editStale" => {
        let edits = [edit(at(0, 0), at(0, 0), "X")];
        let result = host::apply_edits(document, version.saturating_sub(1), &edits);
        report(format!("edit: {result:?}"));
      }
      "editBadRange" => {
        let result = host::apply_edits(document, version, &[edit(at(0, 0), at(0, 999), "X")]);
        report(format!("edit: {result:?}"));
      }
      "editOverlapping" => {
        let edits = [edit(at(0, 0), at(0, 3), "A"), edit(at(0, 2), at(0, 4), "B")];
        let result = host::apply_edits(document, version, &edits);
        report(format!("edit: {result:?}"));
      }
      "read" => {
        let result = host::read_lines(document, 0, 1).map(|slice| slice.lines);
        report(format!("read: {result:?}"));
      }
      "flood" => {
        let long = "x".repeat(100_000);
        for _ in 0..10_000 {
          host::log(LogLevel::Info, &long);
        }
        let diagnostic = Diagnostic {
          range: Range {
            start: at(0, 0),
            end: at(0, 0),
          },
          severity: Severity::Error,
          message: "too many".into(),
          code: None,
        };
        let many = vec![diagnostic; 6_000];
        let result = host::publish_diagnostics(document, version, &many);
        report(format!("diagnostics: {result:?}"));
      }
      "diagnose" => {
        let diagnostic = Diagnostic {
          range: Range {
            start: at(0, 0),
            end: at(0, 1),
          },
          severity: Severity::Warning,
          message: "first character".into(),
          code: Some("first".into()),
        };
        let result = host::publish_diagnostics(document, version, &[diagnostic]);
        report(format!("diagnostics: {result:?}"));
      }
      "config" => {
        report(format!(
          "on_change: {}; limit: {:?}; unknown: {:?}",
          text_setting("on_change"),
          host::configuration("limit"),
          host::configuration("unknown"),
        ));
      }
      "ui" => {
        let status = workbench::set_status_item(
          "state",
          Some(&StatusItem {
            text: "busy".into(),
            tooltip: Some("Working".into()),
            command: Some("test.misbehaving.argument".into()),
          }),
        );
        let item = |id: &str, parent: Option<&str>| ViewItem {
          id: id.into(),
          parent: parent.map(Into::into),
          label: id.to_uppercase(),
          description: None,
          tooltip: None,
          expanded: true,
          command: Some("test.misbehaving.argument".into()),
          argument: Some(format!("item {id}")),
        };
        let view = workbench::set_view_items("items", &[item("a", None), item("b", Some("a"))]);
        let message = workbench::set_view_message("items", Some("Two items"));
        let mark = Range {
          start: at(0, 0),
          end: at(0, 2),
        };
        let decorations = workbench::set_decorations(document, version, "mark", &[mark]);
        let reveal = workbench::reveal(document, mark);
        let shown = workbench::show_view("items");
        report(format!(
          "ui: {status:?} {view:?} {message:?} {decorations:?} {reveal:?} {shown:?}"
        ));
      }
      "argument" => report(format!("argument: {:?}", context.argument)),
      "uiErrors" => {
        let status = workbench::set_status_item("nope", None);
        let foreign = workbench::set_status_item(
          "state",
          Some(&StatusItem {
            text: "x".into(),
            tooltip: None,
            command: Some("other.command".into()),
          }),
        );
        let orphan = workbench::set_view_items(
          "items",
          &[ViewItem {
            id: "child".into(),
            parent: Some("missing".into()),
            label: "Child".into(),
            description: None,
            tooltip: None,
            expanded: false,
            command: None,
            argument: None,
          }],
        );
        let decoration = workbench::set_decorations(document, version, "nope", &[]);
        let view = workbench::show_view("nope");
        report(format!(
          "uiErrors: {status:?} {foreign:?} {orphan:?} {decoration:?} {view:?}"
        ));
      }
      other => return Err(format!("unknown command {other}")),
    }
    Ok(())
  }

  /// Adds a line saying how it was asked to format.
  fn provide_formatting(&mut self, request: FormattingRequest) -> Result<Vec<TextEdit>, String> {
    let indent = if request.insert_spaces {
      "spaces"
    } else {
      "tabs"
    };
    Ok(vec![edit(
      at(0, 0),
      at(0, 0),
      &format!("// formatted with {} {indent}\n", request.tab_size),
    )])
  }

  /// Offers to say hello where asked, then to report its argument.
  fn provide_code_actions(
    &mut self,
    request: CodeActionRequest,
  ) -> Result<Vec<CodeAction>, String> {
    Ok(vec![CodeAction {
      title: "Say Hello".into(),
      kind: Some("quickfix".into()),
      preferred: true,
      edits: vec![edit(request.range.start, request.range.start, "hello ")],
      command: Some("test.misbehaving.argument".into()),
      argument: Some("from an action".into()),
    }])
  }

  fn handle_event(&mut self, event: Event) -> Result<(), String> {
    match event {
      Event::DocumentOpened(document) => {
        report(format!("opened {} v{}", document.id, document.version));
      }
      Event::DocumentChanged(change) => changed(change),
      Event::DocumentClosed(document) => report(format!("closed {document}")),
      Event::ViewerOpened(file) => show_bytes(&file.path)?,
      _ => {}
    }
    Ok(())
  }
}

register!(Misbehaving);
