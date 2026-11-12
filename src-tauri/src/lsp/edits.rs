//! Workspace edits from language servers: what renaming a symbol, a code
//! action, or a server's `workspace/applyEdit` changes, in open documents and
//! in files that aren't open.
//!
//! - **Open documents** are edited as one undo step each, and stay unsaved.
//!   A server's positions refer to the text it was sent (the version the
//!   edit names, or the last one), so they're mapped through any edits made
//!   since.
//! - **Files that aren't open** are read, edited, and written back in their
//!   encoding, as VS Code does by default (`files.refactoring.autoSave`).
//!   Writing checks the file hasn't changed since it was read.
//! - **Files created, renamed, or deleted**: open documents follow a renamed
//!   file, and deleting moves to the trash.
//!
//! An edit of text alone is checked whole before anything changes, so either
//! every document and file is edited or none is (barring a file that can't
//! be written at the end). One with files created, renamed, or deleted is
//! applied in order and stops at the first change that fails.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use flareite_core::DocumentId;
use flareite_lsp::convert::{to_offsets, uri_to_path};
use flareite_lsp::lsp_types::{
  DocumentChangeOperation, DocumentChanges, OneOf, ResourceOp, TextDocumentEdit, TextEdit, Uri,
  WorkspaceEdit,
};
use flareite_lsp::{LanguageServer, PositionEncoding};
use flareite_text::{
  ChangeSet, Edit, LineEnding, TextBuffer, Version, map_offset_through, normalize_line_endings,
};
use flareite_workspace::{FileStamp, TextEncoding, load_text_file, save_text_file};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use super::LspState;
use crate::editor::EditorState;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::paths;

/// Most text edits in one workspace edit.
const MAX_EDITS: usize = 100_000;

/// What a workspace edit changed.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedEditDto {
  /// Open documents edited, which are left unsaved.
  pub documents: usize,
  /// Files that weren't open, edited and written, created, renamed, or
  /// deleted.
  pub files: Vec<String>,
  /// What couldn't be done, after the rest was.
  pub failures: Vec<String>,
}

/// One change of a workspace edit, with its paths.
#[derive(Debug)]
enum Change {
  Text {
    path: PathBuf,
    /// The version of the document the server's positions refer to.
    version: Option<i32>,
    edits: Vec<TextEdit>,
  },
  Create {
    path: PathBuf,
    overwrite: bool,
    ignore_if_exists: bool,
  },
  Rename {
    from: PathBuf,
    to: PathBuf,
    overwrite: bool,
    ignore_if_exists: bool,
  },
  Delete {
    path: PathBuf,
    ignore_if_not_exists: bool,
  },
}

fn path_of(uri: &Uri) -> Result<PathBuf, String> {
  uri_to_path(uri)
    .map(|path| paths::normalize(&path))
    .ok_or_else(|| format!("{} isn't a file", uri.as_str()))
}

fn text_change(edit: TextDocumentEdit) -> Result<Change, String> {
  Ok(Change::Text {
    path: path_of(&edit.text_document.uri)?,
    version: edit.text_document.version,
    edits: edit
      .edits
      .into_iter()
      .map(|edit| match edit {
        OneOf::Left(edit) => edit,
        OneOf::Right(annotated) => annotated.text_edit,
      })
      .collect(),
  })
}

fn resource_change(operation: ResourceOp) -> Result<Change, String> {
  Ok(match operation {
    ResourceOp::Create(create) => {
      let options = create.options.as_ref();
      Change::Create {
        path: path_of(&create.uri)?,
        overwrite: options.and_then(|o| o.overwrite).unwrap_or(false),
        ignore_if_exists: options.and_then(|o| o.ignore_if_exists).unwrap_or(false),
      }
    }
    ResourceOp::Rename(rename) => {
      let options = rename.options.as_ref();
      Change::Rename {
        from: path_of(&rename.old_uri)?,
        to: path_of(&rename.new_uri)?,
        overwrite: options.and_then(|o| o.overwrite).unwrap_or(false),
        ignore_if_exists: options.and_then(|o| o.ignore_if_exists).unwrap_or(false),
      }
    }
    ResourceOp::Delete(delete) => Change::Delete {
      path: path_of(&delete.uri)?,
      ignore_if_not_exists: delete
        .options
        .and_then(|options| options.ignore_if_not_exists)
        .unwrap_or(false),
    },
  })
}

/// A workspace edit's changes, in order: its document changes if it has
/// them (servers send one or the other), else its changes by file.
fn changes(edit: WorkspaceEdit) -> Result<Vec<Change>, String> {
  let changes: Vec<Change> = match edit.document_changes {
    Some(DocumentChanges::Edits(edits)) => edits
      .into_iter()
      .map(text_change)
      .collect::<Result<_, _>>()?,
    Some(DocumentChanges::Operations(operations)) => operations
      .into_iter()
      .map(|operation| match operation {
        DocumentChangeOperation::Edit(edit) => text_change(edit),
        DocumentChangeOperation::Op(operation) => resource_change(operation),
      })
      .collect::<Result<_, _>>()?,
    None => {
      let mut by_file: Vec<(PathBuf, Vec<TextEdit>)> = edit
        .changes
        .unwrap_or_default()
        .into_iter()
        .map(|(uri, edits)| Ok((path_of(&uri)?, edits)))
        .collect::<Result<_, String>>()?;
      by_file.sort_by(|a, b| a.0.cmp(&b.0));
      by_file
        .into_iter()
        .map(|(path, edits)| Change::Text {
          path,
          version: None,
          edits,
        })
        .collect()
    }
  };
  let edits: usize = changes
    .iter()
    .map(|change| match change {
      Change::Text { edits, .. } => edits.len(),
      _ => 1,
    })
    .sum();
  if edits > MAX_EDITS {
    return Err(format!("the edit makes more than {MAX_EDITS} changes"));
  }
  Ok(changes)
}

/// `edits` as byte ranges of `buffer`, sorted (keeping the order of
/// insertions at one place), with line breaks as `ending`.
fn convert(
  buffer: &TextBuffer,
  edits: &[TextEdit],
  encoding: PositionEncoding,
  ending: LineEnding,
) -> Vec<Edit> {
  let mut converted: Vec<Edit> = edits
    .iter()
    .map(|edit| {
      Edit::new(
        to_offsets(buffer, edit.range, encoding),
        normalize_line_endings(&edit.new_text, ending).into_owned(),
      )
    })
    .collect();
  converted.sort_by_key(|edit| (edit.range.start, edit.range.end));
  converted
}

/// Applies `edits` to `buffer`, refusing overlapping ones.
fn apply_to(buffer: &mut TextBuffer, edits: Vec<Edit>, path: &Path) -> Result<(), String> {
  let changes = ChangeSet::new(edits).map_err(|error| overlapping(path, &error))?;
  buffer
    .apply(&changes)
    .map(|_| ())
    .map_err(|error| overlapping(path, &error))
}

fn overlapping(path: &Path, error: &impl std::fmt::Display) -> String {
  format!(
    "the edits of {} don't fit its text ({error})",
    path.display()
  )
}

/// A file that isn't open, as read, and its text after the edits.
struct Unopened {
  path: PathBuf,
  encoding: TextEncoding,
  stamp: FileStamp,
  text: TextBuffer,
}

/// Applies a server's workspace edit; `server` is the one that made it,
/// whose positions it uses.
pub(super) fn apply(
  app: &AppHandle,
  server: &LanguageServer,
  edit: WorkspaceEdit,
) -> Result<AppliedEditDto, String> {
  let changes = changes(edit)?;
  let mut applied = AppliedEditDto::default();
  if changes
    .iter()
    .all(|change| matches!(change, Change::Text { .. }))
  {
    apply_text(app, server, changes, &mut applied)?;
    return Ok(applied);
  }
  for change in changes {
    match change {
      Change::Text { .. } => apply_text(app, server, vec![change], &mut applied)?,
      other => apply_resource(app, other, &mut applied)?,
    }
    // A file that couldn't be written stops the rest too.
    if !applied.failures.is_empty() {
      break;
    }
  }
  Ok(applied)
}

/// Applies `server`'s edits of open document `id`, made for its text at
/// `version` (a formatting), as one undo step. Returns whether the text
/// changed; it doesn't if the document has changed since `version`.
pub(super) fn apply_to_document(
  app: &AppHandle,
  server: &LanguageServer,
  id: DocumentId,
  version: Version,
  edits: &[TextEdit],
) -> Result<bool, String> {
  apply_at_version(app, id, version, |buffer, ending| {
    Ok(convert(buffer, edits, server.encoding(), ending))
  })
}

/// Applies an extension's edits of open document `id`, made for its text at
/// `version`, as one undo step: positions are lines and columns in UTF-8
/// bytes. Returns whether the text changed, as [`apply_to_document`] does.
pub(super) fn apply_provided(
  app: &AppHandle,
  id: DocumentId,
  version: Version,
  edits: &[flareite_extension_host::language_features::ProvidedEdit],
) -> Result<bool, String> {
  apply_at_version(app, id, version, |buffer, ending| {
    let offset = |(line, column): (u32, u32)| {
      let line = line as usize;
      if line >= buffer.line_count() {
        return Err(format!("line {} is past the end", line + 1));
      }
      let offset = buffer.line_start(line) + column as usize;
      if offset > buffer.line_end(line) || !buffer.is_char_boundary(offset) {
        return Err(format!(
          "column {column} of line {} isn't in the text",
          line + 1
        ));
      }
      Ok(offset)
    };
    let mut converted = edits
      .iter()
      .map(|edit| {
        Ok(Edit::new(
          offset(edit.range.start)?..offset(edit.range.end)?,
          normalize_line_endings(&edit.text, ending).into_owned(),
        ))
      })
      .collect::<Result<Vec<_>, String>>()?;
    converted.sort_by_key(|edit| (edit.range.start, edit.range.end));
    Ok(converted)
  })
}

/// Applies the edits `convert` makes of document `id`'s text, if it's still
/// at `version`, as one undo step.
fn apply_at_version(
  app: &AppHandle,
  id: DocumentId,
  version: Version,
  convert: impl FnOnce(&TextBuffer, LineEnding) -> Result<Vec<Edit>, String>,
) -> Result<bool, String> {
  let state = app.state::<EditorState>();
  let mut editor = state.lock();
  let document = editor.document_mut(id).map_err(|error| error.to_string())?;
  if document.buffer().version() != version {
    tracing::debug!(document = %id, "the document changed meanwhile");
    return Ok(false);
  }
  let converted = convert(document.buffer(), document.line_ending())?;
  let changed = document
    .apply_edits(converted)
    .map_err(|error| error.to_string())?;
  if changed {
    app.state::<LspState>().sync(app, document);
    app.state::<ExtensionsState>().sync(document);
    events::emit(
      app,
      AppEvent::DocumentEdited {
        document_id: id.get(),
      },
    );
    drop(editor);
    app.state::<crate::backups::BackupState>().touch();
  }
  Ok(changed)
}

/// The text edits of one file, each with the document version they're for.
type FileEdits = (PathBuf, Vec<(Option<i32>, Vec<TextEdit>)>);

/// Applies text changes (`Change::Text`), checking them all first.
fn apply_text(
  app: &AppHandle,
  server: &LanguageServer,
  changes: Vec<Change>,
  applied: &mut AppliedEditDto,
) -> Result<(), String> {
  // By file, in the order they first appear.
  let mut files: Vec<FileEdits> = Vec::new();
  for change in changes {
    if let Change::Text {
      path,
      version,
      edits,
    } = change
    {
      match files.iter_mut().find(|(file, _)| *file == path) {
        Some((_, list)) => list.push((version, edits)),
        None => files.push((path, vec![(version, edits)])),
      }
    }
  }
  let encoding = server.encoding();
  let state = app.state::<EditorState>();
  let open: HashMap<PathBuf, DocumentId> = {
    let editor = state.lock();
    files
      .iter()
      .filter_map(|(path, _)| Some((path.clone(), editor.find_by_path(path)?)))
      .collect()
  };

  // Files that aren't open: read and edited without the lock.
  let mut unopened: Vec<Unopened> = Vec::new();
  for (path, list) in &files {
    if open.contains_key(path) {
      continue;
    }
    let loaded = load_text_file(path).map_err(|error| error.to_string())?;
    let mut text = TextBuffer::from_text(&loaded.text);
    let ending = text.detect_line_ending().unwrap_or_default();
    for (_, edits) in list {
      let converted = convert(&text, edits, encoding, ending);
      apply_to(&mut text, converted, path)?;
    }
    unopened.push(Unopened {
      path: path.clone(),
      encoding: loaded.encoding,
      stamp: loaded.stamp,
      text,
    });
  }

  // Open documents: checked, then edited, under the lock.
  {
    let mut editor = state.lock();
    let lsp = app.state::<LspState>();
    let mut prepared: Vec<(DocumentId, Vec<Vec<Edit>>)> = Vec::new();
    for (path, list) in &files {
      let Some(&id) = open.get(path) else {
        continue;
      };
      let document = editor.document(id).map_err(|error| error.to_string())?;
      if document.is_read_only() {
        return Err(format!("{} is read only", path.display()));
      }
      let buffer = document.buffer();
      let ending = document.line_ending();
      // What the server's positions refer to.
      let base = lsp
        .sent_text(id, server, list[0].0)
        .unwrap_or_else(|| buffer.snapshot());
      let since = if base.version() == buffer.version() {
        Vec::new()
      } else {
        buffer
          .edits_since(base.version())
          .ok_or_else(|| format!("{} has changed too much since", path.display()))?
      };
      let mut scratch = TextBuffer::from_snapshot(&buffer.snapshot());
      let mut steps = Vec::new();
      for (index, (_, edits)) in list.iter().enumerate() {
        let converted = if index == 0 {
          convert(&TextBuffer::from_snapshot(&base), edits, encoding, ending)
            .into_iter()
            .map(|edit| {
              let start = map_offset_through(&since, edit.range.start, true);
              let end = map_offset_through(&since, edit.range.end, false).max(start);
              Edit::new(start..end, edit.text)
            })
            .collect()
        } else {
          // Later changes refer to the text the earlier ones leave.
          convert(&scratch, edits, encoding, ending)
        };
        apply_to(&mut scratch, converted.clone(), path)?;
        steps.push(converted);
      }
      prepared.push((id, steps));
    }
    for (id, steps) in prepared {
      let document = editor.document_mut(id).map_err(|error| error.to_string())?;
      let mut changed = false;
      for edits in steps {
        changed |= document
          .apply_edits(edits)
          .map_err(|error| error.to_string())?;
      }
      if changed {
        lsp.sync(app, document);
        app.state::<ExtensionsState>().sync(document);
        events::emit(
          app,
          AppEvent::DocumentEdited {
            document_id: id.get(),
          },
        );
        applied.documents += 1;
      }
    }
  }
  if applied.documents > 0 {
    app.state::<crate::backups::BackupState>().touch();
  }

  // Files that aren't open, written once the documents are edited.
  for file in unopened {
    match save_text_file(
      &file.path,
      &file.text.snapshot(),
      file.encoding,
      Some(&file.stamp),
    ) {
      Ok(_) => applied.files.push(file.path.display().to_string()),
      Err(error) => applied.failures.push(error.to_string()),
    }
  }
  Ok(())
}

/// Creates, renames, or deletes a file.
fn apply_resource(
  app: &AppHandle,
  change: Change,
  applied: &mut AppliedEditDto,
) -> Result<(), String> {
  let failed = |path: &Path, error: std::io::Error| format!("{}: {error}", path.display());
  match change {
    Change::Text { .. } => {}
    Change::Create {
      path,
      overwrite,
      ignore_if_exists,
    } => {
      if path.exists() && !overwrite {
        if ignore_if_exists {
          return Ok(());
        }
        return Err(format!("{} already exists", path.display()));
      }
      if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| failed(parent, error))?;
      }
      std::fs::write(&path, b"").map_err(|error| failed(&path, error))?;
      tracing::info!(path = %path.display(), "a language server created a file");
      applied.files.push(path.display().to_string());
    }
    Change::Rename {
      from,
      to,
      overwrite,
      ignore_if_exists,
    } => {
      if to.exists() && !overwrite {
        if ignore_if_exists {
          return Ok(());
        }
        return Err(format!("{} already exists", to.display()));
      }
      if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|error| failed(parent, error))?;
      }
      std::fs::rename(&from, &to).map_err(|error| failed(&from, error))?;
      tracing::info!(from = %from.display(), to = %to.display(), "a language server renamed a file");
      crate::entries::follow(app, &from, &to);
      applied.files.push(to.display().to_string());
    }
    Change::Delete {
      path,
      ignore_if_not_exists,
    } => {
      if !path.exists() {
        if ignore_if_not_exists {
          return Ok(());
        }
        return Err(format!("{} doesn't exist", path.display()));
      }
      flareite_workspace::delete_entry(&path, true).map_err(|error| error.to_string())?;
      tracing::info!(path = %path.display(), "a language server deleted a file");
      applied.files.push(path.display().to_string());
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use flareite_lsp::lsp_types::{Position as LspPosition, Range as LspRange};

  use super::*;

  fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextEdit {
    TextEdit::new(
      LspRange::new(
        LspPosition::new(start.0, start.1),
        LspPosition::new(end.0, end.1),
      ),
      text.into(),
    )
  }

  fn applied(text: &str, edits: &[TextEdit]) -> Result<String, String> {
    let mut buffer = TextBuffer::from_text(text);
    let ending = buffer.detect_line_ending().unwrap_or_default();
    let converted = convert(&buffer, edits, PositionEncoding::Utf16, ending);
    apply_to(&mut buffer, converted, Path::new("a.rs"))?;
    Ok(buffer.contents())
  }

  #[test]
  fn edits_apply_in_any_order_with_the_files_line_endings() {
    let text = "let a = 1;\r\nlet b = a;\r\n";
    let edits = [
      edit((1, 8), (1, 9), "total"),
      edit((0, 4), (0, 5), "total"),
      edit((2, 0), (2, 0), "// end\n"),
    ];
    assert_eq!(
      applied(text, &edits).as_deref(),
      Ok("let total = 1;\r\nlet b = total;\r\n// end\r\n")
    );
  }

  #[test]
  fn insertions_at_one_place_keep_their_order() {
    let edits = [edit((0, 0), (0, 0), "a"), edit((0, 0), (0, 0), "b")];
    assert_eq!(applied("x", &edits).as_deref(), Ok("abx"));
  }

  #[test]
  fn overlapping_edits_are_refused() {
    let edits = [edit((0, 0), (0, 3), "a"), edit((0, 2), (0, 4), "b")];
    assert!(applied("hello", &edits).is_err());
  }

  #[test]
  fn changes_come_from_document_changes_first() {
    let uri: Uri = "file:///c%3A/code/a.rs".parse().expect("uri");
    let other: Uri = "file:///c%3A/code/b.rs".parse().expect("uri");
    let edit = WorkspaceEdit {
      changes: Some(HashMap::from([(other, vec![])])),
      document_changes: Some(DocumentChanges::Operations(vec![
        DocumentChangeOperation::Op(ResourceOp::Create(flareite_lsp::lsp_types::CreateFile {
          uri: uri.clone(),
          options: None,
          annotation_id: None,
        })),
        DocumentChangeOperation::Edit(TextDocumentEdit {
          text_document: flareite_lsp::lsp_types::OptionalVersionedTextDocumentIdentifier {
            uri,
            version: Some(3),
          },
          edits: vec![OneOf::Left(edit((0, 0), (0, 0), "fn a() {}"))],
        }),
      ])),
      change_annotations: None,
    };
    let changes = changes(edit).expect("changes");
    assert_eq!(changes.len(), 2);
    assert!(matches!(
      changes[0],
      Change::Create {
        overwrite: false,
        ..
      }
    ));
    assert!(matches!(
      &changes[1],
      Change::Text { version: Some(3), edits, .. } if edits.len() == 1
    ));
  }

  #[test]
  fn files_that_are_not_open_are_edited_on_disk() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("lib.rs");
    std::fs::write(&path, "fn old() {}\nfn main() { old(); }\n").expect("writes");
    let loaded = load_text_file(&path).expect("loads");
    let mut text = TextBuffer::from_text(&loaded.text);
    let edits = [edit((0, 3), (0, 6), "new"), edit((1, 12), (1, 15), "new")];
    let converted = convert(&text, &edits, PositionEncoding::Utf8, LineEnding::Lf);
    apply_to(&mut text, converted, &path).expect("applies");
    save_text_file(
      &path,
      &text.snapshot(),
      loaded.encoding,
      Some(&loaded.stamp),
    )
    .expect("saves");
    assert_eq!(
      std::fs::read_to_string(&path).expect("reads"),
      "fn new() {}\nfn main() { new(); }\n"
    );
  }
}
