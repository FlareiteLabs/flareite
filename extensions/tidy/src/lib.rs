//! Tidy: an example Flareite extension.
//!
//! It flags trailing whitespace and lines longer than a configurable limit
//! (as diagnostics), and adds commands: one trims trailing whitespace (a
//! validated, versioned edit), one counts words, and one, in the explorer's
//! menu, trims trailing whitespace in a file or every file in a folder
//! (through the `files` interface).
//!
//! It shows how to keep per-document state up to date from change
//! notifications without re-reading whole documents: problems are stored
//! per line, a change marks only the lines it touches as unchecked, and only
//! those lines are read again.

use std::cell::RefCell;
use std::collections::HashMap;

use flareite_extension_api::files::{self, FileError};
use flareite_extension_api::types::{
  Activation, ApiError, CommandContext, ConfigValue, Diagnostic, DocumentChange, DocumentId,
  DocumentInfo, LogLevel, MessageLevel, Permission, Position, Range, Severity, TextEdit,
};
use flareite_extension_api::events::Event;
use flareite_extension_api::{Extension, host, register};

/// Lines read per request.
const CHUNK_LINES: u32 = 2_000;
/// Diagnostics published per document, at most.
const MAX_DIAGNOSTICS: usize = 1_000;
/// Attempts at an edit when the document changes in between.
const ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy)]
enum Kind {
  TrailingWhitespace,
  TooLong { length: usize, limit: usize },
}

/// A problem on one line, as byte columns.
#[derive(Debug, Clone, Copy)]
struct Problem {
  start: u32,
  end: u32,
  kind: Kind,
}

struct Document {
  /// Problems by line; `None` for lines not checked since they changed.
  lines: Vec<Option<Vec<Problem>>>,
  /// The version `lines` describes.
  version: u64,
}

thread_local! {
  static DOCUMENTS: RefCell<HashMap<DocumentId, Document>> = RefCell::new(HashMap::new());
}

struct Settings {
  max_line_length: usize,
  severity: Severity,
}

/// Read on every check, so changed settings apply on the next edit.
fn settings() -> Settings {
  let max_line_length = match host::configuration("max_line_length") {
    Some(ConfigValue::Integer(value)) => usize::try_from(value).unwrap_or(0),
    _ => 100,
  };
  let severity = match host::configuration("severity") {
    Some(ConfigValue::Text(value)) => match value.as_str() {
      "error" => Severity::Error,
      "information" => Severity::Information,
      "hint" => Severity::Hint,
      _ => Severity::Warning,
    },
    _ => Severity::Warning,
  };
  Settings {
    max_line_length,
    severity,
  }
}

fn column(value: usize) -> u32 {
  u32::try_from(value).unwrap_or(u32::MAX)
}

fn check_line(text: &str, settings: &Settings) -> Vec<Problem> {
  let mut problems = Vec::new();
  let trimmed = text.trim_end_matches([' ', '\t']);
  if trimmed.len() < text.len() {
    problems.push(Problem {
      start: column(trimmed.len()),
      end: column(text.len()),
      kind: Kind::TrailingWhitespace,
    });
  }
  let limit = settings.max_line_length;
  if limit > 0
    && let Some((start, _)) = text.char_indices().nth(limit)
  {
    problems.push(Problem {
      start: column(start),
      end: column(text.len()),
      kind: Kind::TooLong {
        length: text.chars().count(),
        limit,
      },
    });
  }
  problems
}

/// Line breaks in `text`: `\r\n`, `\n`, or `\r`.
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
    ApiError::PermissionDenied(permission) => format!(
      "Tidy is not allowed to {} (see the Extensions view)",
      permission.describe()
    ),
    ApiError::UnknownDocument => "the document is no longer open".into(),
    ApiError::StaleVersion(version) => format!("the document changed (now version {version})"),
    ApiError::InvalidRange(message)
    | ApiError::TooLarge(message)
    | ApiError::InvalidArgument(message) => message.clone(),
  }
}

fn position(line: u32, column: u32) -> Position {
  Position { line, column }
}

/// Checks the document's unchecked lines and publishes its problems.
fn refresh(id: DocumentId) {
  let settings = settings();
  DOCUMENTS.with_borrow_mut(|documents| {
    let Some(document) = documents.get_mut(&id) else {
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
      let slice = match host::read_lines(id, column(start), column(line)) {
        Ok(slice) => slice,
        Err(error) => {
          host::log(
            LogLevel::Warn,
            &format!("could not read lines: {}", describe(&error)),
          );
          return;
        }
      };
      // Newer changes are on their way; check again when they arrive.
      if slice.version != document.version {
        return;
      }
      for (offset, text) in slice.lines.iter().enumerate() {
        if let Some(state) = document.lines.get_mut(start + offset) {
          *state = Some(check_line(text, &settings));
        }
      }
    }
    publish(id, document, &settings);
  });
}

fn publish(id: DocumentId, document: &Document, settings: &Settings) {
  let diagnostics: Vec<Diagnostic> = document
    .lines
    .iter()
    .enumerate()
    .flat_map(|(line, problems)| {
      problems
        .iter()
        .flatten()
        .map(move |problem| (column(line), *problem))
    })
    .take(MAX_DIAGNOSTICS)
    .map(|(line, problem)| {
      let (message, code) = match problem.kind {
        Kind::TrailingWhitespace => ("Trailing whitespace".to_owned(), "trailing-whitespace"),
        Kind::TooLong { length, limit } => (
          format!("Line is {length} characters long (limit {limit})"),
          "line-too-long",
        ),
      };
      Diagnostic {
        range: Range {
          start: position(line, problem.start),
          end: position(line, problem.end),
        },
        severity: settings.severity,
        message,
        code: Some(code.to_owned()),
      }
    })
    .collect();
  if let Err(error) = host::publish_diagnostics(id, document.version, &diagnostics) {
    host::log(
      LogLevel::Debug,
      &format!("diagnostics not published: {}", describe(&error)),
    );
  }
}

/// Applies `changes` to the per-line state. Returns `false` if they don't
/// fit it, and the document should be checked from scratch.
fn apply_changes(document: &mut Document, change: &DocumentChange) -> bool {
  let Some(changes) = &change.changes else {
    return false;
  };
  for change in changes {
    let start = change.range.start.line as usize;
    let end = change.range.end.line as usize;
    if end < start || end >= document.lines.len() {
      return false;
    }
    let added = line_breaks(&change.text) + 1;
    document
      .lines
      .splice(start..=end, std::iter::repeat_n(None, added));
  }
  true
}

/// Trims trailing whitespace from every line as one edit. Returns a message
/// for the user.
fn trim(id: DocumentId) -> Result<String, String> {
  'attempt: for _ in 0..ATTEMPTS {
    let info = host::document(id).map_err(|error| describe(&error))?;
    let mut edits = Vec::new();
    let mut start = 0;
    while start < info.line_count {
      let slice =
        host::read_lines(id, start, start + CHUNK_LINES).map_err(|error| describe(&error))?;
      if slice.version != info.version {
        continue 'attempt;
      }
      for (offset, line) in slice.lines.iter().enumerate() {
        let trimmed = line.trim_end_matches([' ', '\t']);
        if trimmed.len() < line.len() {
          let line_number = start + column(offset);
          edits.push(TextEdit {
            range: Range {
              start: position(line_number, column(trimmed.len())),
              end: position(line_number, column(line.len())),
            },
            text: String::new(),
          });
        }
      }
      if slice.lines.is_empty() {
        break;
      }
      start += column(slice.lines.len());
    }
    if edits.is_empty() {
      return Ok("No trailing whitespace found.".into());
    }
    match host::apply_edits(id, info.version, &edits) {
      Ok(_) => {
        let lines = if edits.len() == 1 { "line" } else { "lines" };
        return Ok(format!(
          "Trimmed trailing whitespace from {} {lines}.",
          edits.len()
        ));
      }
      Err(ApiError::StaleVersion(_)) => continue,
      Err(error) => return Err(describe(&error)),
    }
  }
  Err("The document kept changing; try again.".into())
}

fn describe_file(error: &FileError) -> String {
  match error {
    FileError::PermissionDenied(permission) => {
      format!("Tidy is not allowed {permission} (see the Extensions view)")
    }
    FileError::OutsideFolder => "only files in the open folder can be tidied".into(),
    FileError::NotFound => "the file or folder no longer exists".into(),
    FileError::WrongKind => "not a file".into(),
    FileError::NotText => "not a text file".into(),
    FileError::TooLarge(message) | FileError::Failed(message) => message.clone(),
  }
}

/// `text` without spaces and tabs at the ends of its lines; line breaks
/// are kept as they are.
fn trim_text(text: &str) -> String {
  let mut trimmed = String::with_capacity(text.len());
  for line in text.split_inclusive('\n') {
    let body = line.trim_end_matches(['\r', '\n']);
    trimmed.push_str(body.trim_end_matches([' ', '\t']));
    trimmed.push_str(&line[body.len()..]);
  }
  trimmed
}

/// Trims trailing whitespace in a file, or in every file in a folder,
/// from the explorer's menu. Files that aren't text are skipped, and so
/// are Markdown files in a folder (trailing spaces break lines there).
fn trim_files(path: &str) -> Result<String, String> {
  let (paths, folder) = match files::list_files(path) {
    Ok(list) => (list.paths, true),
    Err(FileError::WrongKind) => (vec![path.to_owned()], false),
    Err(error) => return Err(describe_file(&error)),
  };
  let mut changed = 0;
  for file in &paths {
    let lower = file.to_lowercase();
    if folder && (lower.ends_with(".md") || lower.ends_with(".markdown")) {
      continue;
    }
    let text = match files::read_file(file) {
      Ok(text) => text,
      Err(FileError::NotText | FileError::TooLarge(_)) if folder => continue,
      Err(error) => return Err(format!("{file}: {}", describe_file(&error))),
    };
    let trimmed = trim_text(&text);
    if trimmed != text {
      files::write_file(file, &trimmed)
        .map_err(|error| format!("{file}: {}", describe_file(&error)))?;
      changed += 1;
    }
  }
  Ok(match (changed, folder) {
    (0, _) => "No trailing whitespace found.".into(),
    (_, false) => "Trimmed trailing whitespace.".into(),
    (1, true) => "Trimmed trailing whitespace in 1 file.".into(),
    (n, true) => format!("Trimmed trailing whitespace in {n} files."),
  })
}

/// Counts words in the primary selection, or the whole document.
fn count_words(id: DocumentId, context: &CommandContext) -> Result<String, String> {
  if let Some(selection) = context.selections.first()
    && (selection.start.line, selection.start.column) != (selection.end.line, selection.end.column)
  {
    let text = host::read_range(id, *selection).map_err(|error| describe(&error))?;
    let words = text.text.split_whitespace().count();
    return Ok(format!("{words} words in the selection."));
  }
  let info = host::document(id).map_err(|error| describe(&error))?;
  let mut words = 0;
  let mut start = 0;
  while start < info.line_count {
    let slice =
      host::read_lines(id, start, start + CHUNK_LINES).map_err(|error| describe(&error))?;
    words += slice
      .lines
      .iter()
      .map(|line| line.split_whitespace().count())
      .sum::<usize>();
    if slice.lines.is_empty() {
      break;
    }
    start += column(slice.lines.len());
  }
  Ok(format!("{words} words in {} lines.", info.line_count))
}

struct Tidy;

/// Starts following a document: its lines are checked as they change.
fn opened(document: DocumentInfo) {
  DOCUMENTS.with_borrow_mut(|documents| {
    documents.insert(
      document.id,
      Document {
        lines: vec![None; document.line_count as usize],
        version: document.version,
      },
    );
  });
  refresh(document.id);
}

fn changed(change: DocumentChange) {
  let fits = DOCUMENTS.with_borrow_mut(|documents| {
    let Some(document) = documents.get_mut(&change.document) else {
      return true;
    };
    document.version = change.version;
    apply_changes(document, &change)
  });
  if !fits {
    // Start over from the document as it is now.
    match host::document(change.document) {
      Ok(info) => opened(info),
      Err(error) => host::log(LogLevel::Warn, &describe(&error)),
    }
    return;
  }
  refresh(change.document);
}

fn closed(document: DocumentId) {
  DOCUMENTS.with_borrow_mut(|documents| documents.remove(&document));
}

impl Extension for Tidy {
  fn activate(context: Activation) -> Result<Self, String> {
    if !context.granted.contains(&Permission::DocumentsRead) {
      host::log(
        LogLevel::Info,
        "not allowed to read documents; problems will not be shown",
      );
    }
    Ok(Tidy)
  }

  fn deactivate(&mut self) {
    DOCUMENTS.with_borrow_mut(HashMap::clear);
  }

  fn run_command(&mut self, command: &str, context: CommandContext) -> Result<(), String> {
    if command == "example.tidy.trimFiles" {
      // From the explorer: the file's or folder's path.
      let path = context
        .argument
        .ok_or("Choose a file or folder in the explorer.")?;
      host::show_message(MessageLevel::Info, &trim_files(&path)?);
      return Ok(());
    }
    let Some(id) = context.document else {
      return Err("Open a document first.".into());
    };
    let message = match command {
      "example.tidy.trimTrailingWhitespace" => trim(id)?,
      "example.tidy.countWords" => count_words(id, &context)?,
      _ => return Err(format!("unknown command {command}")),
    };
    host::show_message(MessageLevel::Info, &message);
    Ok(())
  }

  fn handle_event(&mut self, event: Event) -> Result<(), String> {
    match event {
      Event::DocumentOpened(document) => opened(document),
      Event::DocumentChanged(change) => changed(change),
      Event::DocumentClosed(document) => closed(document),
      _ => {}
    }
    Ok(())
  }
}

register!(Tidy);

#[cfg(test)]
mod tests {
  use super::*;

  fn settings(max_line_length: usize) -> Settings {
    Settings {
      max_line_length,
      severity: Severity::Warning,
    }
  }

  #[test]
  fn finds_trailing_whitespace_and_long_lines() {
    let problems = check_line("let x = 1;  \t", &settings(10));
    assert_eq!(problems.len(), 2);
    assert_eq!((problems[0].start, problems[0].end), (10, 13));
    assert_eq!((problems[1].start, problems[1].end), (10, 13));
    assert!(check_line("fine", &settings(10)).is_empty());
    assert!(check_line("long line here", &settings(0)).is_empty());
  }

  #[test]
  fn counts_columns_in_bytes_and_limits_in_characters() {
    let problems = check_line("ééééé", &settings(3));
    assert_eq!((problems[0].start, problems[0].end), (6, 10));
  }

  #[test]
  fn trims_line_ends_and_keeps_line_breaks() {
    assert_eq!(trim_text("a  \r\nb\t\nc \n\n  "), "a\r\nb\nc\n\n");
    assert_eq!(trim_text("clean\n"), "clean\n");
  }

  #[test]
  fn counts_each_kind_of_line_break_once() {
    assert_eq!(line_breaks("a\r\nb\nc\rd"), 3);
    assert_eq!(line_breaks("no breaks"), 0);
  }
}
