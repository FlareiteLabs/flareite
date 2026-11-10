//! Problems in files, for the Problems view: the diagnostics of every open
//! document (from language servers and extensions), and those language
//! servers published for files that aren't open. Hints are left out, as
//! they suggest rather than report.

use std::path::PathBuf;

use flareite_core::Document;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::editor::EditorState;
use crate::extensions::ExtensionsState;
use crate::lsp::{FileDiagnostic, LspState};

/// Most problems listed; the rest are counted as left out.
const MAX_PROBLEMS: usize = 10_000;
/// LSP severity of a hint, the least severe.
const HINT: u8 = 4;

/// One file's problems.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileProblemsDto {
  /// `None` for an untitled document.
  path: Option<String>,
  /// The open document's id; `None` for a file that isn't open.
  document: Option<u64>,
  title: String,
  /// Most severe first, then in order.
  problems: Vec<FileDiagnostic>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemsDto {
  /// Files with problems, by path; untitled documents last.
  files: Vec<FileProblemsDto>,
  /// Problems beyond the most listed.
  left_out: usize,
}

/// Every file's problems.
#[tauri::command]
pub fn problems_list(app: AppHandle) -> ProblemsDto {
  let mut files = Vec::new();
  let mut open_paths: Vec<PathBuf> = Vec::new();
  {
    let editor_state = app.state::<EditorState>();
    let editor = editor_state.lock();
    let lsp = app.state::<LspState>();
    let extensions = app.state::<ExtensionsState>();
    for document in editor.documents() {
      if let Some(path) = document.path() {
        open_paths.push(path.to_path_buf());
      }
      let mut problems = lsp.document_diagnostics(document);
      problems.extend(extension_diagnostics(&extensions, document));
      let schemas = app.state::<crate::schemas::SchemaState>();
      if schemas.sync(&app, document).is_some() {
        problems.extend(schemas.document_diagnostics(document));
      }
      files.push(FileProblemsDto {
        path: document.path().map(|path| path.display().to_string()),
        document: Some(document.id().get()),
        title: document.title(),
        problems,
      });
    }
  }
  for (path, problems) in app.state::<LspState>().file_diagnostics(&open_paths) {
    files.push(FileProblemsDto {
      title: path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
      ),
      path: Some(path.display().to_string()),
      document: None,
      problems,
    });
  }
  arrange(files)
}

fn extension_diagnostics(extensions: &ExtensionsState, document: &Document) -> Vec<FileDiagnostic> {
  let Some(host) = extensions.host() else {
    return Vec::new();
  };
  let buffer = document.buffer();
  host
    .diagnostics(document, 0..buffer.len())
    .into_iter()
    .map(|diagnostic| FileDiagnostic {
      start: buffer.offset_to_utf16(diagnostic.range.start).into(),
      end: buffer.offset_to_utf16(diagnostic.range.end).into(),
      severity: diagnostic.severity.number(),
      message: diagnostic.message,
      source: Some(diagnostic.source),
    })
    .collect()
}

/// Leaves out hints and files without problems, sorts, and keeps at most
/// [`MAX_PROBLEMS`].
fn arrange(mut files: Vec<FileProblemsDto>) -> ProblemsDto {
  for file in &mut files {
    file.problems.retain(|problem| problem.severity < HINT);
    file.problems.sort_by(|a, b| {
      (a.severity, a.start.line, a.start.column).cmp(&(b.severity, b.start.line, b.start.column))
    });
  }
  files.retain(|file| !file.problems.is_empty());
  files.sort_by(|a, b| match (&a.path, &b.path) {
    (Some(a), Some(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
    (Some(_), None) => std::cmp::Ordering::Less,
    (None, Some(_)) => std::cmp::Ordering::Greater,
    (None, None) => a.title.cmp(&b.title),
  });
  let mut room = MAX_PROBLEMS;
  let mut left_out = 0;
  for file in &mut files {
    if file.problems.len() > room {
      left_out += file.problems.len() - room;
      file.problems.truncate(room);
    }
    room -= file.problems.len();
  }
  files.retain(|file| !file.problems.is_empty());
  ProblemsDto { files, left_out }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::editor::protocol::Position;

  fn problem(severity: u8, line: usize) -> FileDiagnostic {
    FileDiagnostic {
      start: Position { line, column: 0 },
      end: Position { line, column: 1 },
      severity,
      message: format!("at {line}"),
      source: None,
    }
  }

  fn file(path: Option<&str>, problems: Vec<FileDiagnostic>) -> FileProblemsDto {
    FileProblemsDto {
      path: path.map(str::to_owned),
      document: None,
      title: path.unwrap_or("Untitled-1").to_owned(),
      problems,
    }
  }

  #[test]
  fn sorts_files_and_problems_and_leaves_out_hints() {
    let arranged = arrange(vec![
      file(None, vec![problem(1, 0)]),
      file(
        Some("/w/b.rs"),
        vec![problem(2, 9), problem(1, 5), problem(2, 1)],
      ),
      file(Some("/w/A.rs"), vec![problem(4, 0)]),
      file(Some("/w/a.rs"), vec![problem(3, 2)]),
    ]);
    let paths: Vec<Option<&str>> = arranged.files.iter().map(|f| f.path.as_deref()).collect();
    assert_eq!(
      paths,
      [Some("/w/a.rs"), Some("/w/b.rs"), None],
      "A.rs has only a hint"
    );
    let b: Vec<(u8, usize)> = arranged.files[1]
      .problems
      .iter()
      .map(|p| (p.severity, p.start.line))
      .collect();
    assert_eq!(b, [(1, 5), (2, 1), (2, 9)]);
    assert_eq!(arranged.left_out, 0);
  }

  #[test]
  fn keeps_at_most_the_limit() {
    let many = |n: usize| (0..n).map(|line| problem(1, line)).collect();
    let arranged = arrange(vec![
      file(Some("/a"), many(MAX_PROBLEMS - 10)),
      file(Some("/b"), many(25)),
      file(Some("/c"), many(5)),
    ]);
    let counts: Vec<usize> = arranged.files.iter().map(|f| f.problems.len()).collect();
    assert_eq!(counts, [MAX_PROBLEMS - 10, 10]);
    assert_eq!(arranged.left_out, 20);
  }
}
