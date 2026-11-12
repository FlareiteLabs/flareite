//! TODOs: an example Flareite extension that customizes the workbench.
//!
//! It finds TODO and FIXME markers in open documents and shows them with
//! every kind of workbench contribution:
//!
//! - a **decoration type** that highlights the markers;
//! - a **side bar view** listing them by document (selecting one reveals
//!   it, through a hidden command that receives the item's argument);
//! - a **status bar item** with the total, which opens the view;
//! - a **keybinding** (Ctrl+Alt+T) that jumps to the next marker;
//! - an **editor context menu item** that inserts a TODO comment.
//!
//! Like the Tidy example, it keeps per-line state and re-reads only the
//! lines a change touches.

use std::cell::RefCell;
use std::collections::BTreeMap;

use flareite_extension_api::types::{
  Activation, ApiError, CommandContext, ConfigValue, DocumentChange, DocumentId, DocumentInfo,
  LogLevel, Position, Range, StatusItem, TextEdit, ViewItem,
};
use flareite_extension_api::events::Event;
use flareite_extension_api::{Extension, host, register, workbench};

const CHUNK_LINES: u32 = 2_000;
/// Items listed per document in the view.
const MAX_LISTED: usize = 200;

/// A marker: where it is and the rest of its line.
#[derive(Debug, Clone, PartialEq)]
struct Todo {
  column: u32,
  length: u32,
  text: String,
}

struct Document {
  info: DocumentInfo,
  /// Markers by line; `None` for lines not checked since they changed.
  lines: Vec<Option<Vec<Todo>>>,
}

#[derive(Default)]
struct State {
  documents: BTreeMap<DocumentId, Document>,
  /// What the view and status bar last showed, to skip unchanged updates.
  shown: Option<(Vec<ViewItem>, usize)>,
}

thread_local! {
  static STATE: RefCell<State> = RefCell::new(State::default());
}

fn to_u32(value: usize) -> u32 {
  u32::try_from(value).unwrap_or(u32::MAX)
}

fn keywords() -> Vec<String> {
  let configured = match host::configuration("keywords") {
    Some(ConfigValue::Text(text)) => text,
    _ => "TODO,FIXME".into(),
  };
  configured
    .split(',')
    .map(|word| word.trim().to_owned())
    .filter(|word| !word.is_empty())
    .collect()
}

/// Markers in one line: a keyword as a whole word.
fn find_todos(line: &str, keywords: &[String]) -> Vec<Todo> {
  let mut found = Vec::new();
  for keyword in keywords {
    let mut from = 0;
    while let Some(offset) = line[from..].find(keyword.as_str()) {
      let start = from + offset;
      let end = start + keyword.len();
      let word = |c: char| c.is_alphanumeric() || c == '_';
      let before = line[..start].chars().next_back().is_some_and(word);
      let after = line[end..].chars().next().is_some_and(word);
      if !before && !after {
        found.push(Todo {
          column: to_u32(start),
          length: to_u32(keyword.len()),
          text: line[start..].trim().chars().take(120).collect(),
        });
      }
      from = end;
    }
  }
  found.sort_by_key(|todo| todo.column);
  found
}

fn line_breaks(text: &str) -> usize {
  let bytes = text.as_bytes();
  let mut count = 0;
  let mut i = 0;
  while i < bytes.len() {
    match bytes[i] {
      b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
        count += 1;
        i += 1;
      }
      b'\r' | b'\n' => count += 1,
      _ => {}
    }
    i += 1;
  }
  count
}

fn describe(error: &ApiError) -> String {
  match error {
    ApiError::PermissionDenied(_) => {
      "TODOs is missing a permission (see the Extensions view)".into()
    }
    ApiError::UnknownDocument => "the document is no longer open".into(),
    ApiError::StaleVersion(_) => "the document changed; try again".into(),
    ApiError::InvalidRange(message)
    | ApiError::TooLarge(message)
    | ApiError::InvalidArgument(message) => message.clone(),
  }
}

fn range(line: u32, todo: &Todo) -> Range {
  Range {
    start: Position {
      line,
      column: todo.column,
    },
    end: Position {
      line,
      column: todo.column + todo.length,
    },
  }
}

/// Checks the document's unchecked lines, then updates its decorations.
fn refresh(id: DocumentId) {
  let keywords = keywords();
  STATE.with_borrow_mut(|state| {
    let Some(document) = state.documents.get_mut(&id) else {
      return;
    };
    let mut line = 0;
    while line < document.lines.len() {
      if document.lines[line].is_some() {
        line += 1;
        continue;
      }
      let start = line;
      while line < document.lines.len()
        && document.lines[line].is_none()
        && line - start < CHUNK_LINES as usize
      {
        line += 1;
      }
      let slice = match host::read_lines(id, to_u32(start), to_u32(line)) {
        Ok(slice) => slice,
        Err(error) => {
          host::log(LogLevel::Warn, &describe(&error));
          return;
        }
      };
      if slice.version != document.info.version {
        return; // Newer changes are on their way.
      }
      for (offset, text) in slice.lines.iter().enumerate() {
        if let Some(state) = document.lines.get_mut(start + offset) {
          *state = Some(find_todos(text, &keywords));
        }
      }
    }
    let ranges: Vec<Range> = document
      .lines
      .iter()
      .enumerate()
      .flat_map(|(line, todos)| {
        todos
          .iter()
          .flatten()
          .map(move |todo| range(to_u32(line), todo))
      })
      .collect();
    if let Err(error) = workbench::set_decorations(id, document.info.version, "keyword", &ranges) {
      host::log(LogLevel::Debug, &describe(&error));
    }
  });
  update_workbench();
}

/// A short name for a document in the view.
fn title(info: &DocumentInfo) -> String {
  info
    .path
    .as_deref()
    .and_then(|path| path.rsplit(['/', '\\']).next())
    .map_or_else(|| format!("Untitled ({})", info.id), str::to_owned)
}

/// Updates the view and the status bar item from all documents.
fn update_workbench() {
  STATE.with_borrow_mut(|state| {
    let mut items = Vec::new();
    let mut total = 0;
    for (id, document) in &state.documents {
      let todos: Vec<(usize, &Todo)> = document
        .lines
        .iter()
        .enumerate()
        .flat_map(|(line, todos)| todos.iter().flatten().map(move |todo| (line, todo)))
        .collect();
      if todos.is_empty() {
        continue;
      }
      total += todos.len();
      let parent = format!("doc-{id}");
      items.push(ViewItem {
        id: parent.clone(),
        parent: None,
        label: title(&document.info),
        description: Some(todos.len().to_string()),
        tooltip: document.info.path.clone(),
        expanded: true,
        command: None,
        argument: None,
      });
      for (line, todo) in todos.into_iter().take(MAX_LISTED) {
        items.push(ViewItem {
          id: format!("{id}:{line}:{}", todo.column),
          parent: Some(parent.clone()),
          label: todo.text.clone(),
          description: Some(format!("line {}", line + 1)),
          tooltip: None,
          expanded: false,
          command: Some("example.todos.reveal".into()),
          argument: Some(format!("{id}:{line}:{}:{}", todo.column, todo.length)),
        });
      }
    }
    if state.shown.as_ref() == Some(&(items.clone(), total)) {
      return;
    }
    let message = (total == 0).then(|| "No TODOs in open documents.".to_owned());
    let results = [
      workbench::set_view_items("list", &items),
      workbench::set_view_message("list", message.as_deref()),
      workbench::set_status_item(
        "count",
        Some(&StatusItem {
          text: format!("☐ {total} TODO{}", if total == 1 { "" } else { "s" }),
          tooltip: Some("Show TODOs".into()),
          command: Some("example.todos.show".into()),
        }),
      ),
    ];
    for result in results {
      if let Err(error) = result {
        host::log(LogLevel::Warn, &describe(&error));
      }
    }
    state.shown = Some((items, total));
  });
}

/// Reveals the marker described by a view item's argument,
/// `document:line:column:length`.
fn reveal(argument: &str) -> Result<(), String> {
  let parts: Vec<u64> = argument
    .split(':')
    .map(|part| {
      part
        .parse()
        .map_err(|_| format!("bad argument {argument:?}"))
    })
    .collect::<Result<_, _>>()?;
  let [document, line, column, length] = parts[..] else {
    return Err(format!("bad argument {argument:?}"));
  };
  let (line, column, length) = (
    to_u32(line as usize),
    to_u32(column as usize),
    to_u32(length as usize),
  );
  let todo = Todo {
    column,
    length,
    text: String::new(),
  };
  workbench::reveal(document, range(line, &todo)).map_err(|error| describe(&error))
}

/// Jumps to the first marker after the cursor, wrapping around.
fn next(context: &CommandContext) -> Result<(), String> {
  let document = context.document.ok_or("Open a document first.")?;
  let cursor = context.selections.first().map_or((0, 0), |selection| {
    (selection.end.line, selection.end.column)
  });
  let target = STATE.with_borrow(|state| {
    let todos: Vec<(u32, Todo)> = state
      .documents
      .get(&document)?
      .lines
      .iter()
      .enumerate()
      .flat_map(|(line, todos)| {
        todos
          .iter()
          .flatten()
          .map(move |todo| (to_u32(line), todo.clone()))
      })
      .collect();
    todos
      .iter()
      .find(|(line, todo)| (*line, todo.column) > cursor)
      .or_else(|| todos.first())
      .cloned()
  });
  match target {
    Some((line, todo)) => {
      workbench::reveal(document, range(line, &todo)).map_err(|error| describe(&error))
    }
    None => {
      host::show_message(
        flareite_extension_api::types::MessageLevel::Info,
        "No TODOs in this document.",
      );
      Ok(())
    }
  }
}

/// Inserts a TODO comment at the cursor, in the document's comment syntax.
fn insert(context: &CommandContext) -> Result<(), String> {
  let document = context.document.ok_or("Open a document first.")?;
  let info = host::document(document).map_err(|error| describe(&error))?;
  let at = context
    .selections
    .first()
    .map_or(Position { line: 0, column: 0 }, |selection| selection.start);
  let marker = keywords()
    .into_iter()
    .next()
    .unwrap_or_else(|| "TODO".into());
  let comment = match info.language.as_deref() {
    Some("python" | "toml") => format!("# {marker}: "),
    Some(_) => format!("// {marker}: "),
    None => format!("{marker}: "),
  };
  let edit = TextEdit {
    range: Range { start: at, end: at },
    text: comment,
  };
  host::apply_edits(document, info.version, &[edit])
    .map(|_| ())
    .map_err(|error| describe(&error))
}

struct Todos;

fn opened(info: DocumentInfo) {
  let id = info.id;
  STATE.with_borrow_mut(|state| {
    let lines = vec![None; info.line_count as usize];
    state.documents.insert(id, Document { info, lines });
  });
  refresh(id);
}

fn changed(change: DocumentChange) {
  let fits = STATE.with_borrow_mut(|state| {
    let Some(document) = state.documents.get_mut(&change.document) else {
      return true;
    };
    document.info.version = change.version;
    let Some(changes) = &change.changes else {
      return false;
    };
    for edit in changes {
      let (start, end) = (edit.range.start.line as usize, edit.range.end.line as usize);
      if end < start || end >= document.lines.len() {
        return false;
      }
      let added = line_breaks(&edit.text) + 1;
      document
        .lines
        .splice(start..=end, std::iter::repeat_n(None, added));
    }
    true
  });
  if !fits {
    match host::document(change.document) {
      Ok(info) => opened(info),
      Err(error) => host::log(LogLevel::Warn, &describe(&error)),
    }
    return;
  }
  refresh(change.document);
}

fn closed(document: DocumentId) {
  STATE.with_borrow_mut(|state| state.documents.remove(&document));
  update_workbench();
}

impl Extension for Todos {
  fn activate(_context: Activation) -> Result<Self, String> {
    update_workbench();
    Ok(Todos)
  }

  fn run_command(&mut self, command: &str, context: CommandContext) -> Result<(), String> {
    match command {
      "example.todos.next" => next(&context),
      "example.todos.insert" => insert(&context),
      "example.todos.show" => workbench::show_view("list").map_err(|error| describe(&error)),
      "example.todos.reveal" => {
        let argument = context
          .argument
          .ok_or("Choose a TODO in the TODOs view.")?;
        reveal(&argument)
      }
      _ => Err(format!("unknown command {command}")),
    }
  }

  fn handle_event(&mut self, event: Event) -> Result<(), String> {
    match event {
      Event::DocumentOpened(info) => opened(info),
      Event::DocumentChanged(change) => changed(change),
      Event::DocumentClosed(document) => closed(document),
      _ => {}
    }
    Ok(())
  }
}

register!(Todos);

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn finds_whole_word_markers() {
    let keywords = vec!["TODO".to_owned(), "FIXME".to_owned()];
    let found = find_todos("// TODO: tidy up, FIXME later; TODOS no", &keywords);
    assert_eq!(found.len(), 2);
    assert_eq!((found[0].column, found[0].length), (3, 4));
    assert_eq!(found[0].text, "TODO: tidy up, FIXME later; TODOS no");
    assert_eq!(found[1].column, 18);
    assert!(find_todos("mytodo TODO_LIST", &keywords).is_empty());
  }

  #[test]
  fn parses_reveal_arguments() {
    assert!(reveal("1:2").is_err());
    assert!(reveal("a:b:c:d").is_err());
  }
}
