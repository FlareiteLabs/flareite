//! Replace in files (ADR 0053): replacing the search panel's results.
//!
//! Each file's results come as the panel shows them (line and UTF-16
//! columns). The file's current text is searched again and the matches at
//! those places replaced (`flareite_workspace::replacements`), so a result
//! that moved since the search is skipped, not replaced elsewhere. As for a
//! rename (ADR 0050), open documents are edited as one undo step each and
//! left unsaved, and files that aren't open are written back in their
//! encoding, refusing one that changed on disk since it was read.

use std::path::{Path, PathBuf};

use flareite_text::{ChangeSet, Edit, SearchQuery, Searcher, TextBuffer, normalize_line_endings};
use flareite_workspace::{MatchAt, load_text_file, replacements, save_text_file};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::editor::EditorState;
use crate::editor::protocol::SearchQueryDto;
use crate::error::CommandError;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::lsp::LspState;
use crate::workspace::WorkspaceState;

/// Most files one replace changes.
const MAX_FILES: usize = 10_000;

/// A file's results to replace: `[line, start column, end column]` each,
/// as the search reported them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceFileDto {
  pub path: String,
  pub matches: Vec<[usize; 3]>,
}

/// What a replace did.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaceSummaryDto {
  /// Matches replaced.
  pub replaced: usize,
  /// Open documents edited, left unsaved.
  pub documents: usize,
  /// Files that weren't open, edited and written.
  pub files: usize,
  /// Results no match was at any more (the text changed since the search).
  pub skipped: usize,
  /// Files that couldn't be changed, and why.
  pub failures: Vec<String>,
}

/// Replaces the matches of `query` at the places `files` name with
/// `replacement` (capture groups expanded for a regex), in the open
/// folder's files.
#[tauri::command]
pub async fn workspace_replace(
  app: AppHandle,
  query: SearchQueryDto,
  replacement: String,
  files: Vec<ReplaceFileDto>,
) -> Result<ReplaceSummaryDto, CommandError> {
  let searcher = SearchQuery::from(query)
    .compile()
    .map_err(|error| CommandError::new("invalidSearch", error.to_string()))?;
  let root = app
    .state::<WorkspaceState>()
    .root()
    .ok_or_else(|| CommandError::new("noWorkspace", "No folder is open."))?;
  if files.len() > MAX_FILES {
    return Err(CommandError::new(
      "tooManyFiles",
      format!("At most {MAX_FILES} files can be changed at once."),
    ));
  }
  tauri::async_runtime::spawn_blocking(move || replace(&app, &root, &searcher, &replacement, files))
    .await
    .map_err(CommandError::internal)
}

fn replace(
  app: &AppHandle,
  root: &Path,
  searcher: &Searcher,
  replacement: &str,
  files: Vec<ReplaceFileDto>,
) -> ReplaceSummaryDto {
  let mut summary = ReplaceSummaryDto::default();
  let state = app.state::<EditorState>();
  let mut unopened: Vec<(PathBuf, Vec<MatchAt>)> = Vec::new();
  {
    let mut editor = state.lock();
    let mut edited = false;
    for file in files {
      let path = PathBuf::from(&file.path);
      if !path.starts_with(root) {
        summary
          .failures
          .push(format!("{} isn't in the open folder", file.path));
        continue;
      }
      let wanted: Vec<MatchAt> = file
        .matches
        .iter()
        .map(|&[line, start_column, end_column]| MatchAt {
          line,
          start_column,
          end_column,
        })
        .collect();
      let Some(id) = editor.find_by_path(&path) else {
        unopened.push((path, wanted));
        continue;
      };
      let Ok(document) = editor.document_mut(id) else {
        continue;
      };
      if document.is_read_only() {
        summary.failures.push(format!("{} is read-only", file.path));
        continue;
      }
      let planned = replacements(document.buffer(), searcher, replacement, &wanted);
      summary.skipped += planned.missing;
      let count = planned.edits.len();
      match document.apply_edits(planned.edits) {
        Ok(true) => {
          summary.replaced += count;
          summary.documents += 1;
          edited = true;
          app.state::<LspState>().sync(app, document);
          app.state::<ExtensionsState>().sync(document);
          events::emit(
            app,
            AppEvent::DocumentEdited {
              document_id: id.get(),
            },
          );
        }
        Ok(false) => {}
        Err(error) => summary.failures.push(format!("{}: {error}", file.path)),
      }
    }
    drop(editor);
    if edited {
      app.state::<crate::backups::BackupState>().touch();
    }
  }

  // Files that aren't open: read, edited, and written without the lock.
  for (path, wanted) in unopened {
    match replace_in_file(&path, searcher, replacement, &wanted) {
      Ok((count, missing)) => {
        summary.skipped += missing;
        if count > 0 {
          summary.replaced += count;
          summary.files += 1;
        }
      }
      Err(error) => summary.failures.push(error),
    }
  }
  summary
}

/// Replaces the matches at `wanted` in a file that isn't open: the matches
/// replaced, and the results no match was at.
fn replace_in_file(
  path: &Path,
  searcher: &Searcher,
  replacement: &str,
  wanted: &[MatchAt],
) -> Result<(usize, usize), String> {
  let loaded = load_text_file(path).map_err(|error| error.to_string())?;
  let mut text = TextBuffer::from_text(&loaded.text);
  let planned = replacements(&text, searcher, replacement, wanted);
  if planned.edits.is_empty() {
    return Ok((0, planned.missing));
  }
  let ending = text.detect_line_ending().unwrap_or_default();
  let count = planned.edits.len();
  let edits: Vec<Edit> = planned
    .edits
    .into_iter()
    .map(|edit| {
      Edit::new(
        edit.range,
        normalize_line_endings(&edit.text, ending).into_owned(),
      )
    })
    .collect();
  let changes = ChangeSet::new(edits).map_err(|error| format!("{}: {error}", path.display()))?;
  text
    .apply(&changes)
    .map_err(|error| format!("{}: {error}", path.display()))?;
  save_text_file(path, &text.snapshot(), loaded.encoding, Some(&loaded.stamp))
    .map_err(|error| error.to_string())?;
  Ok((count, planned.missing))
}

#[cfg(test)]
mod tests {
  use std::fs;

  use flareite_text::SearchQuery;

  use super::*;

  fn searcher(pattern: &str) -> Searcher {
    SearchQuery::literal(pattern).compile().expect("compiles")
  }

  #[test]
  fn files_that_are_not_open_are_written_in_their_line_endings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("a.txt");
    fs::write(&path, "cat\r\ndog cat\r\n").expect("write");
    let wanted = [MatchAt {
      line: 1,
      start_column: 4,
      end_column: 7,
    }];
    let (count, missing) =
      replace_in_file(&path, &searcher("cat"), "big\ncat", &wanted).expect("replaces");
    assert_eq!((count, missing), (1, 0));
    assert_eq!(
      fs::read_to_string(&path).expect("read"),
      "cat\r\ndog big\r\ncat\r\n"
    );
  }

  #[test]
  fn a_result_that_moved_leaves_the_file_alone() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("a.txt");
    fs::write(&path, "x cat").expect("write");
    let wanted = [MatchAt {
      line: 0,
      start_column: 0,
      end_column: 3,
    }];
    let (count, missing) = replace_in_file(&path, &searcher("cat"), "dog", &wanted).expect("ok");
    assert_eq!((count, missing), (0, 1));
    assert_eq!(fs::read_to_string(&path).expect("read"), "x cat");
  }
}
