//! A document's indentation, and the tidying saving does (ADR 0054).
//!
//! A file opened in the editor is indented as its `.editorconfig` says,
//! else as its content does (`editor.detect_indentation`), else as the
//! settings say; the user can choose otherwise for a document from the
//! status bar. Saving trims trailing whitespace and ends the file with one
//! line break as `files.*` say, which the file's `.editorconfig` can
//! override.

use flareite_core::{Document, DocumentId, IndentSource, Indentation, SaveActions};
use flareite_workspace::{EditorConfig, editorconfig_for};
use serde::Serialize;
use tauri::{AppHandle, Manager, Window};

use super::EditorState;
use crate::error::CommandError;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::lsp::LspState;
use crate::settings::{EditorSettings, FilesSettings, SettingsState};

/// How a document indents, for the status bar.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndentationDto {
  pub tab_size: usize,
  pub insert_spaces: bool,
  /// `settings`, `detected`, `editorConfig`, or `chosen`.
  pub source: &'static str,
}

impl From<Indentation> for IndentationDto {
  fn from(indentation: Indentation) -> Self {
    Self {
      tab_size: indentation.tab_size,
      insert_spaces: indentation.insert_spaces,
      source: match indentation.source {
        IndentSource::Settings => "settings",
        IndentSource::Detected => "detected",
        IndentSource::EditorConfig => "editorConfig",
        IndentSource::Chosen => "chosen",
      },
    }
  }
}

/// Indents a document just opened from a file: as its `.editorconfig`
/// says, else as its content does (if `editor.detect_indentation`), else as
/// the settings do. What `config` leaves out comes from the content or the
/// settings.
pub(crate) fn on_open(document: &mut Document, config: &EditorConfig, settings: &EditorSettings) {
  let mut tab_size = settings.tab_size;
  let mut insert_spaces = settings.insert_spaces;
  let mut source = IndentSource::Settings;
  if settings.detect_indentation
    && let Some(detected) = document.detect_indentation()
  {
    insert_spaces = detected.insert_spaces;
    tab_size = detected.tab_size.unwrap_or(tab_size);
    source = IndentSource::Detected;
  }
  if let Some(spaces) = config.insert_spaces() {
    insert_spaces = spaces;
    source = IndentSource::EditorConfig;
  }
  if let Some(size) = config.tab_size() {
    tab_size = size;
    source = IndentSource::EditorConfig;
  }
  if source != IndentSource::Settings {
    document.set_indentation_from(tab_size, insert_spaces, source);
  }
}

/// What saving a file tidies: `files.*`, with what its `.editorconfig` says
/// over them.
pub(crate) fn save_actions(files: &FilesSettings, config: &EditorConfig) -> SaveActions {
  SaveActions {
    trim_trailing_whitespace: config
      .trim_trailing_whitespace()
      .unwrap_or(files.trim_trailing_whitespace),
    insert_final_newline: config
      .insert_final_newline()
      .unwrap_or(files.insert_final_newline),
    trim_final_newlines: files.trim_final_newlines,
  }
}

/// Tidies a document about to be saved, as [`save_actions`] says, as one
/// undo step.
pub(crate) async fn tidy_before_save(window: &Window, id: DocumentId) {
  let app = window.app_handle();
  let path = {
    let state = app.state::<EditorState>();
    let editor = state.lock();
    let Ok(document) = editor.document(id) else {
      return;
    };
    if document.is_read_only() {
      return;
    }
    document.path().map(std::path::Path::to_path_buf)
  };
  let files = app.state::<SettingsState>().get().files.clone();
  let config = match path {
    Some(path) => tauri::async_runtime::spawn_blocking(move || editorconfig_for(&path))
      .await
      .unwrap_or_default(),
    None => EditorConfig::default(),
  };
  let actions = save_actions(&files, &config);
  if !actions.any() {
    return;
  }
  let state = app.state::<EditorState>();
  let mut editor = state.lock();
  let Ok(document) = editor.document_mut(id) else {
    return;
  };
  match document.apply_save_actions(actions) {
    Ok(true) => {
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
    Err(error) => tracing::debug!(document = %id, %error, "could not tidy before saving"),
  }
}

/// Indents a document with `tab_size` spaces, or with tabs `tab_size`
/// wide, from now on: the user's choice, which settings changes don't undo.
#[tauri::command]
pub fn document_set_indentation(
  app: AppHandle,
  id: u64,
  tab_size: usize,
  insert_spaces: bool,
) -> Result<IndentationDto, CommandError> {
  let state = app.state::<EditorState>();
  let mut editor = state.lock();
  let document = editor.document_mut(DocumentId::from_raw(id))?;
  document.set_indentation_from(tab_size.clamp(1, 16), insert_spaces, IndentSource::Chosen);
  Ok(document.indent_style().into())
}

/// Indents a document as its content does, or as the settings say if it
/// doesn't tell.
#[tauri::command]
pub fn document_detect_indentation(
  app: AppHandle,
  id: u64,
) -> Result<IndentationDto, CommandError> {
  let settings = app.state::<SettingsState>().get().editor.clone();
  let state = app.state::<EditorState>();
  let mut editor = state.lock();
  let document = editor.document_mut(DocumentId::from_raw(id))?;
  match document.detect_indentation() {
    Some(detected) => document.set_indentation_from(
      detected.tab_size.unwrap_or(settings.tab_size),
      detected.insert_spaces,
      IndentSource::Detected,
    ),
    None => document.set_indentation_from(
      settings.tab_size,
      settings.insert_spaces,
      IndentSource::Settings,
    ),
  }
  Ok(document.indent_style().into())
}

#[cfg(test)]
mod tests {
  use flareite_core::{Editor, EditorConfig as CoreConfig, EditorOp};

  use super::*;

  fn document_with(editor: &mut Editor, text: &str) -> DocumentId {
    let id = editor.create_untitled();
    editor
      .document_mut(id)
      .expect("open")
      .apply(EditorOp::Insert(text.into()))
      .expect("applies");
    id
  }

  fn config(text: &str) -> EditorConfig {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join(".editorconfig"), text).expect("write");
    editorconfig_for(&dir.path().join("a.py"))
  }

  #[test]
  fn editorconfig_wins_over_the_content_which_wins_over_the_settings() {
    let mut editor = Editor::new(CoreConfig::default());
    let settings = EditorSettings::default();
    let id = document_with(&mut editor, "def a():\n  b()\n  if c:\n    d()\n");
    let document = editor.document_mut(id).expect("open");
    on_open(document, &EditorConfig::default(), &settings);
    let detected = document.indent_style();
    assert_eq!((detected.tab_size, detected.insert_spaces), (2, true));
    assert_eq!(detected.source, IndentSource::Detected);

    on_open(
      document,
      &config("root = true\n[*.py]\nindent_size = 3\n"),
      &settings,
    );
    let configured = document.indent_style();
    assert_eq!((configured.tab_size, configured.insert_spaces), (3, true));
    assert_eq!(configured.source, IndentSource::EditorConfig);
  }

  #[test]
  fn without_detection_or_a_config_the_settings_stay() {
    let mut editor = Editor::new(CoreConfig::default());
    let settings = EditorSettings {
      detect_indentation: false,
      ..EditorSettings::default()
    };
    let id = document_with(&mut editor, "a\n\tb\n");
    let document = editor.document_mut(id).expect("open");
    on_open(document, &EditorConfig::default(), &settings);
    assert_eq!(document.indent_style().source, IndentSource::Settings);
    assert!(document.indent_style().insert_spaces);
  }

  #[test]
  fn editorconfig_overrides_the_save_settings() {
    let files = FilesSettings {
      trim_trailing_whitespace: true,
      ..FilesSettings::default()
    };
    let actions = save_actions(
      &files,
      &config("[*]\ntrim_trailing_whitespace = false\ninsert_final_newline = true\n"),
    );
    assert!(!actions.trim_trailing_whitespace);
    assert!(actions.insert_final_newline);
    assert!(save_actions(&files, &EditorConfig::default()).trim_trailing_whitespace);
  }
}
