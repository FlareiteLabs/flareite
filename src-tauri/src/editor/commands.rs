//! Document and editing commands.
//!
//! Lifecycle commands (`document_*`) may show dialogs and touch the disk, so
//! they are async and never hold the editor lock across an await. Editing
//! commands (`editor_*`) are fast and synchronous; each returns the updated
//! view for the caller's viewport, so a keystroke costs one round trip and
//! transfers only visible lines.

use std::path::Path;

use flareite_core::{CoreError, Document, DocumentId, EditorOp, LanguageChoice, ViewId};
use flareite_text::SearchQuery;
use flareite_workspace::{
  DecodeOptions, ENCODING_CHOICES, FileError, TextEncoding, load_text_file_with, save_text_file,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, Window};
use tauri_plugin_clipboard_manager::ClipboardExt;

use super::EditorState;
use super::minimap::{MinimapDto, Snapshot};
use super::protocol::{
  DocumentSummary, EditorView, KnownDto, MAX_OPS_PER_BATCH, OpContext, OpDto, Position,
  SearchQueryDto, SearchView, Viewport, same_text,
};
use super::{fold_memory, folding, pairs};
use crate::backups::BackupState;
use crate::dialogs::{self, UnsavedChoice};
use crate::error::CommandError;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::languages::LanguagesState;
use crate::lsp::LspState;
use crate::paths;
use crate::perf::Perf;
use crate::{settings, ui_state};

#[tauri::command]
pub fn document_list(state: State<'_, EditorState>) -> Vec<DocumentSummary> {
  state
    .lock()
    .documents()
    .filter(|document| !state.is_hidden(document.id()))
    .map(DocumentSummary::of)
    .collect()
}

/// Creates an untitled document, optionally containing `text` (e.g. the
/// keyboard shortcuts list), inserted as its first edit.
#[tauri::command]
pub fn document_new(
  app: AppHandle,
  state: State<'_, EditorState>,
  text: Option<String>,
) -> Result<DocumentSummary, CommandError> {
  let mut editor = state.lock();
  let id = editor.create_untitled();
  tracing::info!(document = %id, "created untitled document");
  if let Some(text) = text.filter(|text| !text.is_empty()) {
    let document = editor.document_mut(id)?;
    document.apply(EditorOp::Insert(text))?;
    document.apply(EditorOp::SetCursor {
      offset: 0,
      extend: false,
      add: false,
    })?;
  }
  let document = editor.document(id)?;
  app.state::<ExtensionsState>().sync(document);
  Ok(DocumentSummary::of(document))
}

/// Opens a file as a text document. A file that looks binary fails with
/// `binary`, unless `binary` is set (the user chose to open it anyway).
/// With `hidden`, a file not open yet opens for a diff tab only (see
/// [`EditorState::is_hidden`]); without, a hidden one gets listed.
#[tauri::command]
pub async fn document_open(
  app: AppHandle,
  path: String,
  binary: Option<bool>,
  hidden: Option<bool>,
) -> Result<DocumentSummary, CommandError> {
  let options = DecodeOptions {
    encoding: None,
    binary: binary.unwrap_or(false),
  };
  let hidden = hidden.unwrap_or(false);
  let summary = open_path(&app, Path::new(&path), options, hidden).await?;
  if !hidden {
    show_hidden(&app, DocumentId::from_raw(summary.id));
  }
  Ok(summary)
}

/// Lists a hidden document: it got a tab of its own.
pub(super) fn show_hidden(app: &AppHandle, id: DocumentId) {
  let state = app.state::<EditorState>();
  if state.is_hidden(id) {
    state.set_hidden(id, false);
    ui_state::record_open_files(app);
  }
}

/// Stops listing a document whose last tab closed while a diff tab still
/// shows it.
#[tauri::command]
pub fn document_hide(app: AppHandle, id: u64) -> Result<(), CommandError> {
  let id = DocumentId::from_raw(id);
  app.state::<EditorState>().lock().document(id)?;
  app.state::<EditorState>().set_hidden(id, true);
  ui_state::record_open_files(&app);
  Ok(())
}

/// Shows an open dialog and returns the chosen file, which the workbench
/// then opens (as text, or in an extension's viewer). `None` if cancelled.
#[tauri::command]
pub async fn document_pick_file(window: Window) -> Option<String> {
  dialogs::pick_file_to_open(&window)
    .await
    .map(|path| paths::normalize(&path).display().to_string())
}

/// The encodings a document can be reopened or saved with.
#[tauri::command]
pub fn document_encodings() -> Vec<EncodingChoice> {
  ENCODING_CHOICES
    .iter()
    .map(|(id, label)| EncodingChoice { id, label })
    .collect()
}

#[derive(Serialize)]
pub struct EncodingChoice {
  id: &'static str,
  label: &'static str,
}

/// Sets a document's language: a language's id, `plaintext` for none, or
/// `None` to go by its file name again. Its language server and extensions
/// see it close and open again as the new language; its view is refreshed
/// by the event this sends.
#[tauri::command]
pub fn document_set_language(
  app: AppHandle,
  state: State<'_, EditorState>,
  id: u64,
  language: Option<String>,
) -> Result<(), CommandError> {
  let choice = match language {
    None => None,
    Some(language) if language == "plaintext" => Some(LanguageChoice::PlainText),
    Some(language) => {
      if app
        .state::<LanguagesState>()
        .registry()
        .get(&language)
        .is_none()
      {
        return Err(CommandError::new(
          "unknownLanguage",
          format!("There's no language {language:?}."),
        ));
      }
      Some(LanguageChoice::Language(language))
    }
  };
  let id = DocumentId::from_raw(id);
  {
    let mut editor = state.lock();
    let document = editor.document_mut(id)?;
    tracing::info!(document = %id, language = ?choice, "set document language");
    document.set_language_choice(choice);
    let document = &*document;
    app.state::<LspState>().sync(&app, document);
    app.state::<ExtensionsState>().sync(document);
  }
  // Its old server's diagnostics are gone.
  events::emit(
    &app,
    AppEvent::DiagnosticsChanged {
      document_id: id.get(),
    },
  );
  Ok(())
}

fn encoding_named(id: &str) -> Result<TextEncoding, CommandError> {
  TextEncoding::from_id(id)
    .ok_or_else(|| CommandError::new("unknownEncoding", format!("There's no encoding {id:?}.")))
}

/// Reads a document's file again in another encoding. Refused while it has
/// unsaved changes, which reading it again would replace. The reload can be
/// undone, like a reload after the file changed on disk.
#[tauri::command]
pub async fn document_reopen_with_encoding(
  app: AppHandle,
  id: u64,
  encoding: String,
) -> Result<DocumentSummary, CommandError> {
  let id = DocumentId::from_raw(id);
  let encoding = encoding_named(&encoding)?;
  let state = app.state::<EditorState>();
  let path = {
    let editor = state.lock();
    let document = editor.document(id)?;
    if document.is_dirty() {
      return Err(CommandError::new(
        "unsaved",
        format!(
          "{} has unsaved changes. Save or undo them before reopening it with another encoding.",
          document.title()
        ),
      ));
    }
    document
      .path()
      .ok_or_else(|| CommandError::new("untitled", "An untitled file has no encoding yet."))?
      .to_path_buf()
  };
  let options = DecodeOptions {
    encoding: Some(encoding),
    binary: true,
  };
  let to_load = path.clone();
  let loaded = blocking(move || load_text_file_with(&to_load, options)).await??;
  let mut editor = state.lock();
  let document = editor.document_mut(id)?;
  if document.is_dirty() {
    return Err(CommandError::new(
      "unsaved",
      format!("{} was edited while it was being read.", document.title()),
    ));
  }
  document.reload(&loaded.text, loaded.encoding, loaded.stamp)?;
  tracing::info!(document = %id, encoding = %loaded.encoding.id(), "reopened with an encoding");
  app.state::<ExtensionsState>().sync(document);
  Ok(DocumentSummary::of(document))
}

/// Saves a document in another encoding, which it keeps from then on.
/// `None` if the user cancelled.
#[tauri::command]
pub async fn document_save_with_encoding(
  window: Window,
  id: u64,
  encoding: String,
) -> Result<Option<DocumentSummary>, CommandError> {
  let encoding = encoding_named(&encoding)?;
  save_in(&window, DocumentId::from_raw(id), false, Some(encoding)).await
}

/// Saves a document, asking for a path if it has none. `None` if the user
/// cancelled.
///
/// With `editor.format_on_save`, the document is formatted first, unless
/// `format` is false (File: Save without Formatting).
#[tauri::command]
pub async fn document_save(
  window: Window,
  id: u64,
  format: Option<bool>,
) -> Result<Option<DocumentSummary>, CommandError> {
  let id = DocumentId::from_raw(id);
  if format.unwrap_or(true) {
    format_on_save(&window, id).await;
  }
  save(&window, id, false).await
}

/// How long saving waits for formatting.
const FORMAT_ON_SAVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Formats a document about to be saved, if `editor.format_on_save` says
/// to. A formatter that fails or is slow doesn't stop the save.
async fn format_on_save(window: &Window, id: DocumentId) {
  let app = window.app_handle();
  if !app
    .state::<settings::SettingsState>()
    .get()
    .editor
    .format_on_save
  {
    return;
  }
  let has_path = {
    let state = app.state::<EditorState>();
    let editor = state.lock();
    editor
      .document(id)
      .is_ok_and(|document| document.path().is_some() && !document.is_read_only())
  };
  if !has_path {
    return;
  }
  match crate::lsp::format(app, id, None, FORMAT_ON_SAVE_TIMEOUT).await {
    Ok(outcome) => tracing::debug!(document = %id, ?outcome, "formatted before saving"),
    Err(error) => tracing::debug!(document = %id, ?error, "could not format before saving"),
  }
}

#[tauri::command]
pub async fn document_save_as(
  window: Window,
  id: u64,
) -> Result<Option<DocumentSummary>, CommandError> {
  save(&window, DocumentId::from_raw(id), true).await
}

/// Closes a document, first asking whether to save unsaved changes. Returns
/// whether it was closed.
#[tauri::command]
pub async fn document_close(window: Window, id: u64) -> Result<bool, CommandError> {
  let id = DocumentId::from_raw(id);
  let (dirty, title) = {
    let state = window.state::<EditorState>();
    let editor = state.lock();
    let document = editor.document(id)?;
    (document.is_dirty(), document.title())
  };
  if dirty {
    let message = format!("Do you want to save the changes you made to {title}?");
    match dialogs::ask_unsaved(&window, message, "Save").await {
      UnsavedChoice::Save => {
        if save(&window, id, false).await?.is_none() {
          return Ok(false);
        }
      }
      UnsavedChoice::Discard => {}
      UnsavedChoice::Cancel => return Ok(false),
    }
  }
  let state = window.state::<EditorState>();
  let mut closed = state.lock().close(id)?;
  // The folds of its first tab are the ones to remember, not a diff's.
  if let Some(first) = closed
    .views()
    .into_iter()
    .filter(|view| !super::is_pane_view(*view))
    .min()
  {
    closed.use_view(first);
  }
  if !state.is_hidden(id) {
    fold_memory::remember(window.app_handle(), &closed);
  }
  state.set_hidden(id, false);
  fold_memory::forget(window.app_handle(), id);
  window.state::<BackupState>().forget(id);
  state.counts.forget(id);
  state.syntax.forget(id);
  state.regions.forget(id);
  state.quick.forget(id);
  window.state::<LspState>().close(&closed);
  window.state::<ExtensionsState>().closed(id);
  window.state::<crate::schemas::SchemaState>().closed(id);
  tracing::info!(document = %id, "closed document");
  ui_state::record_open_files(window.app_handle());
  Ok(true)
}

/// Syncs the document with its language server and builds a view,
/// including find matches when `search` is given, syntax highlights, fold
/// markers, and diagnostics. Match counts for large documents and syntax
/// parses that take too long are scheduled in the background. Folds
/// remembered for the document are restored first, once they can be.
fn build_view(
  app: &AppHandle,
  document: &mut Document,
  viewport: Viewport,
  known: Option<KnownDto>,
  search: Option<SearchQueryDto>,
) -> EditorView {
  let state = app.state::<EditorState>();
  let lsp = app.state::<LspState>();
  let extensions = app.state::<ExtensionsState>();
  fold_memory::restore(app, document);
  let document = &*document;
  lsp.sync(app, document);
  extensions.sync(document);
  app.state::<BackupState>().touch();
  let mut view = EditorView::of(
    document,
    viewport,
    known.as_ref(),
    |start, end| {
      let query = SearchQuery::from(search?);
      let counted = state.counts.get(document.id());
      let (view, needs_count) = SearchView::build(document, &query, start, end, counted.as_deref());
      if needs_count {
        state.counts.schedule(app, document, &query);
      }
      Some(view)
    },
    |ranges, chunks| {
      state
        .syntax
        .highlights(app, document, ranges, known.as_ref(), chunks)
    },
  );
  let visible: Vec<_> = view
    .visible
    .iter()
    .map(|&[start, end]| start..end)
    .collect();
  view.foldable = folding::foldable_lines(app, document, &visible);
  let lines = view.start_line..view.end_line;
  (view.diagnostics, view.language) = lsp.view(app, document, lines.clone());
  schema_view(app, document, lines.clone(), &mut view);
  view.decorations = extensions.add_to_view(document, lines.clone(), &mut view.diagnostics);
  view.lenses = extensions.lenses(document);
  view.annotations = extensions.annotations(document, lines.clone());
  view.changes = super::quick_diff::view_changes(app, document, lines);
  view
}

/// Adds what schemas find in a document to its view, and lets it ask the
/// completion and auto-insert providers extensions declare for its
/// language. Call with the editor locked.
fn schema_view(
  app: &AppHandle,
  document: &flareite_core::Document,
  lines: std::ops::Range<usize>,
  view: &mut EditorView,
) {
  let schemas = app.state::<crate::schemas::SchemaState>();
  schemas.sync(document);
  view.diagnostics.extend(schemas.view(document, lines));
  let Some(language) = app
    .state::<crate::languages::LanguagesState>()
    .for_document(document)
  else {
    return;
  };
  let extensions = app.state::<ExtensionsState>();
  let Some(host) = extensions.host() else {
    return;
  };
  let completion = host.completion_triggers(language.id());
  let auto_insert = host.auto_insert_triggers(language.id());
  let code_actions = host.offers_code_actions(language.id());
  if completion.is_none() && auto_insert.is_none() && !code_actions {
    return;
  }
  let status = view
    .language
    .get_or_insert_with(|| crate::lsp::LanguageStatusDto {
      id: language.id().to_owned(),
      language: language.name().to_owned(),
      server: None,
      completion_triggers: None,
      auto_insert_triggers: None,
      signature_triggers: None,
      code_actions: false,
    });
  if let Some(triggers) = completion {
    let known = status.completion_triggers.get_or_insert_with(Vec::new);
    for trigger in triggers {
      if !known.contains(&trigger) {
        known.push(trigger);
      }
    }
  }
  status.auto_insert_triggers = auto_insert;
  status.code_actions |= code_actions;
}

/// Text an extension inserts after typing, at the view's cursor (see
/// `language-features::set-insert`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoInsertDto {
  /// The document version it's for: the `autoInsert` operation does
  /// nothing at another.
  pub version: u64,
  pub position: Position,
  pub text: String,
  /// Where to leave the cursor, in UTF-8 bytes into `text`.
  pub cursor: usize,
}

/// Asks the auto-insert providers of the document's language what to
/// insert at its view's cursor, now that `trigger` was typed there. `None`
/// if there's nothing to insert, or several cursors or a selection.
#[tauri::command]
pub async fn editor_auto_insert(
  app: AppHandle,
  state: State<'_, EditorState>,
  id: u64,
  view: Option<u32>,
  trigger: String,
) -> Result<Option<AutoInsertDto>, CommandError> {
  let (language, request, position, indent) = {
    let mut editor = state.lock();
    let document = document_in_view(&mut editor, id, view)?;
    let selections = document.selections();
    let cursor = selections.primary();
    if selections.len() != 1 || !cursor.is_empty() {
      return Ok(None);
    }
    let Some(language) = app
      .state::<crate::languages::LanguagesState>()
      .for_document(document)
    else {
      return Ok(None);
    };
    let buffer = document.buffer();
    let line = buffer.line_of(cursor.head);
    let request = flareite_extension_host::language_features::FeatureRequest {
      document: document.id(),
      version: buffer.version().get(),
      line: u32::try_from(line).unwrap_or(u32::MAX),
      column: u32::try_from(cursor.head - buffer.line_start(line)).unwrap_or(u32::MAX),
    };
    let position: Position = buffer.offset_to_utf16(cursor.head).into();
    (
      language.id().to_owned(),
      request,
      position,
      document.indent_unit(),
    )
  };
  let app = app.clone();
  let answer = tauri::async_runtime::spawn_blocking(move || {
    app
      .state::<ExtensionsState>()
      .host()
      .and_then(|host| host.auto_insert(&language, request, &trigger, &indent))
  })
  .await
  .map_err(CommandError::internal)?;
  Ok(answer.map(|answer| AutoInsertDto {
    version: request.version,
    position,
    text: answer.text,
    cursor: answer.cursor,
  }))
}

/// The document `id` in its view `view`, wrapping lines as `viewport` says
/// (see [`document_in_view`]).
fn document_for<'a>(
  editor: &'a mut flareite_core::Editor,
  id: u64,
  view: Option<u32>,
  viewport: &Viewport,
) -> Result<&'a mut Document, CoreError> {
  let document = document_in_view(editor, id, view)?;
  document.set_wrap(viewport.wrap_config());
  Ok(document)
}

/// The document `id` in its view `view` (view 0 if not given): each editor
/// showing a document has its own view, with its own selections and folds.
fn document_in_view(
  editor: &mut flareite_core::Editor,
  id: u64,
  view: Option<u32>,
) -> Result<&mut Document, CoreError> {
  let document = editor.document_mut(DocumentId::from_raw(id))?;
  document.use_view(ViewId::from_raw(view.unwrap_or(0)));
  Ok(document)
}

/// The view for `viewport`. `known` describes the lines the client already
/// holds, so only missing lines are sent. With `search`, the view includes
/// find matches.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn editor_view(
  app: AppHandle,
  state: State<'_, EditorState>,
  perf: State<'_, Perf>,
  id: u64,
  view: Option<u32>,
  viewport: Viewport,
  known: Option<KnownDto>,
  search: Option<SearchQueryDto>,
) -> Result<EditorView, CommandError> {
  let _timer = perf.metrics.time("editor.view");
  let mut editor = state.lock();
  let document = document_for(&mut editor, id, view, &viewport)?;
  Ok(build_view(&app, document, viewport, known, search))
}

/// Makes view `to` of a document a copy of its view `from`, for an editor
/// split off another: the same selections and folds.
#[tauri::command]
pub fn editor_copy_view(
  state: State<'_, EditorState>,
  id: u64,
  from: u32,
  to: u32,
) -> Result<(), CommandError> {
  let mut editor = state.lock();
  let document = editor.document_mut(DocumentId::from_raw(id))?;
  document.copy_view(ViewId::from_raw(from), ViewId::from_raw(to));
  Ok(())
}

/// Forgets a view of a document that no editor shows any more, while
/// others still show the document.
#[tauri::command]
pub fn editor_close_view(
  state: State<'_, EditorState>,
  id: u64,
  view: u32,
) -> Result<(), CommandError> {
  let mut editor = state.lock();
  let document = editor.document_mut(DocumentId::from_raw(id))?;
  let view = ViewId::from_raw(view);
  document.close_view(view);
  state.syntax.forget_view(document.id(), view);
  Ok(())
}

/// The minimap for display rows `start_row..end_row`, clamped to the
/// document and to `MAX_MINIMAP_LINES`. Runs on a worker thread, and
/// highlights from a snapshot after releasing the editor lock, so editing
/// never waits for it.
#[tauri::command]
pub async fn editor_minimap(
  app: AppHandle,
  id: u64,
  view: Option<u32>,
  start_row: usize,
  end_row: usize,
) -> Result<MinimapDto, CommandError> {
  tauri::async_runtime::spawn_blocking(move || {
    let started = std::time::Instant::now();
    let state = app.state::<EditorState>();
    let snapshot = {
      let mut editor = state.lock();
      let document = &*document_in_view(&mut editor, id, view)?;
      Snapshot::of(
        document,
        state.syntax.tree(&app, document),
        start_row..end_row,
        |lines| folding::section_headers(&app, document, lines),
      )
    };
    let perf = app.state::<Perf>();
    perf.record("editor.minimap.lock", started.elapsed());
    let minimap = snapshot.build();
    perf.record("editor.minimap", started.elapsed());
    Ok(minimap)
  })
  .await
  .map_err(CommandError::internal)?
}

/// Applies a batch of operations in order and returns the resulting view.
/// Find operations with an invalid pattern are skipped; the view's search
/// section reports the problem.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn editor_apply(
  app: AppHandle,
  state: State<'_, EditorState>,
  perf: State<'_, Perf>,
  id: u64,
  view: Option<u32>,
  ops: Vec<OpDto>,
  viewport: Viewport,
  known: Option<KnownDto>,
  search: Option<SearchQueryDto>,
) -> Result<EditorView, CommandError> {
  let _timer = perf.metrics.time("editor.apply");
  if ops.len() > MAX_OPS_PER_BATCH {
    return Err(CommandError::new(
      "tooManyOps",
      format!("At most {MAX_OPS_PER_BATCH} operations can be applied at once."),
    ));
  }
  let mut editor = state.lock();
  let document = document_for(&mut editor, id, view, &viewport)?;
  let mut context = op_context(&app, &state, document, &ops);
  let changes_folds = ops.iter().any(OpDto::changes_folds);
  for op in ops {
    // Looked up for each fold operation, as the ops before it may have
    // moved the cursors.
    if let Some(scope) = folding::scope(&op, document) {
      context.folds = folding::regions(&app, document, scope);
    }
    // Likewise for each cursor's pairs.
    if let Some(need) = pairs::need(&op) {
      context.pairs = pairs::context(&app, document, need);
    }
    let op = op.into_core(document.buffer(), &context);
    match document.apply(op) {
      Ok(_) | Err(CoreError::Search(_)) => {}
      Err(error) => return Err(error.into()),
    }
  }
  if changes_folds {
    fold_memory::remember(&app, document);
  }
  Ok(build_view(&app, document, viewport, known, search))
}

/// The context the operations in `ops` need, looked up only if they need it.
fn op_context(
  app: &AppHandle,
  state: &EditorState,
  document: &Document,
  ops: &[OpDto],
) -> OpContext {
  let mut context = OpContext::default();
  if ops.iter().any(|op| matches!(op, OpDto::ToggleComment)) {
    context.comments = app.state::<LanguagesState>().comments_for(document);
  }
  if ops.iter().any(|op| matches!(op, OpDto::Paste { .. })) {
    context.copied_lines = state.copied_lines();
  }
  context
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClipboardAction {
  Copy,
  Cut,
  Paste,
}

/// Copies (or cuts) the selected text to the system clipboard, or pastes
/// from it. With nothing selected, copies (or cuts) the cursors' whole
/// lines, which then paste as whole lines. The text goes straight between
/// the backend and the clipboard without crossing IPC. (Keyboard paste
/// arrives as a `paste` operation instead.)
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn editor_clipboard(
  app: AppHandle,
  state: State<'_, EditorState>,
  id: u64,
  view: Option<u32>,
  action: ClipboardAction,
  viewport: Viewport,
  known: Option<KnownDto>,
  search: Option<SearchQueryDto>,
) -> Result<EditorView, CommandError> {
  if let ClipboardAction::Paste = action {
    // Read before taking the editor lock: the clipboard can be slow.
    let text = app
      .clipboard()
      .read_text()
      .map_err(|error| CommandError::new("clipboard", format!("Could not paste: {error}")))?;
    let whole_lines = state
      .copied_lines()
      .is_some_and(|copied| same_text(&copied, &text));
    let mut editor = state.lock();
    let document = document_for(&mut editor, id, view, &viewport)?;
    if !text.is_empty() {
      document.apply(EditorOp::Paste { text, whole_lines })?;
    }
    return Ok(build_view(&app, document, viewport, known, search));
  }
  let mut editor = state.lock();
  let document = document_for(&mut editor, id, view, &viewport)?;
  let (text, whole_lines) = match document.selected_text() {
    Some(text) => (text, false),
    None => (document.cursor_lines_text(), true),
  };
  let remembered = whole_lines.then(|| text.clone());
  app
    .clipboard()
    .write_text(text)
    .map_err(|error| CommandError::new("clipboard", format!("Could not copy: {error}")))?;
  state.set_copied_lines(remembered);
  if let ClipboardAction::Cut = action {
    document.apply(if whole_lines {
      EditorOp::DeleteLines
    } else {
      EditorOp::DeleteSelection
    })?;
  }
  Ok(build_view(&app, document, viewport, known, search))
}

/// Asks whether to save unsaved documents before the window closes, saving
/// them if requested. Returns whether the window should close.
pub async fn confirm_close_with_unsaved(window: &Window) -> bool {
  let dirty: Vec<(DocumentId, String)> = window
    .state::<EditorState>()
    .lock()
    .documents()
    .filter(|document| document.is_dirty())
    .map(|document| (document.id(), document.title()))
    .collect();
  let message = match dirty.as_slice() {
    [] => return true,
    [(_, title)] => format!("Do you want to save the changes you made to {title}?"),
    many => format!("Do you want to save the changes to {} files?", many.len()),
  };
  match dialogs::ask_unsaved(window, message, "Save All").await {
    UnsavedChoice::Discard => true,
    UnsavedChoice::Cancel => false,
    UnsavedChoice::Save => {
      for (id, title) in dirty {
        match save(window, id, false).await {
          Ok(Some(_)) => {}
          Ok(None) => return false,
          Err(error) => {
            tracing::error!(document = %id, %title, message = %error.message, "save before close failed");
            return false;
          }
        }
      }
      true
    }
  }
}

async fn open_path(
  app: &AppHandle,
  path: &Path,
  options: DecodeOptions,
  hidden: bool,
) -> Result<DocumentSummary, CommandError> {
  let path = paths::normalize(path);
  let state = app.state::<EditorState>();
  if let Some(summary) = find_open(&state, &path) {
    return Ok(summary);
  }

  let to_load = path.clone();
  let started = std::time::Instant::now();
  let loaded = blocking(move || load_text_file_with(&to_load, options)).await??;
  app
    .state::<Perf>()
    .record("document.load", started.elapsed());

  let mut editor = state.lock();
  // Another request may have opened the same file while this one was loading.
  if let Some(id) = editor.find_by_path(&path) {
    return Ok(DocumentSummary::of(editor.document(id)?));
  }
  let bytes = loaded.text.len();
  let encoding = loaded.encoding;
  let id = editor.open(path.clone(), loaded);
  state.set_hidden(id, hidden);
  tracing::info!(document = %id, path = %path.display(), bytes, ?encoding, hidden, "opened file");
  let document = editor.document(id)?;
  fold_memory::recall(app, document);
  super::quick_diff::track(app, id, path.clone());
  app.state::<ExtensionsState>().sync(document);
  let summary = DocumentSummary::of(document);
  drop(editor);
  ui_state::record_open_files(app);
  Ok(summary)
}

fn find_open(state: &EditorState, path: &Path) -> Option<DocumentSummary> {
  let editor = state.lock();
  let id = editor.find_by_path(path)?;
  editor.document(id).ok().map(DocumentSummary::of)
}

/// Saves `id`, showing a Save As dialog when it has no path or `choose_path`
/// is set, and asking before overwriting a file changed by another program.
async fn save(
  window: &Window,
  id: DocumentId,
  choose_path: bool,
) -> Result<Option<DocumentSummary>, CommandError> {
  save_in(window, id, choose_path, None).await
}

/// Saves `id` as [`save`] does, in `encoding` rather than its own if given.
/// The document takes the encoding only once the file is written.
async fn save_in(
  window: &Window,
  id: DocumentId,
  choose_path: bool,
  encoding: Option<TextEncoding>,
) -> Result<Option<DocumentSummary>, CommandError> {
  let state = window.state::<EditorState>();
  let (mut point, current_path, title) = {
    let editor = state.lock();
    let document = editor.document(id)?;
    (
      document.save_point(),
      document.path().map(Path::to_path_buf),
      document.title(),
    )
  };
  if let Some(encoding) = encoding {
    point.encoding = encoding;
  }

  let path = match (&current_path, choose_path) {
    (Some(path), false) => path.clone(),
    _ => match dialogs::pick_save_path(window, current_path.as_deref(), &title).await {
      Some(path) => paths::normalize(&path),
      None => return Ok(None),
    },
  };
  let same_file = current_path.as_deref() == Some(path.as_path());
  if !same_file {
    let editor = state.lock();
    if editor.find_by_path(&path).is_some_and(|other| other != id) {
      return Err(CommandError::new(
        "alreadyOpen",
        format!("{} is open in another tab. Close it first.", path.display()),
      ));
    }
  }
  // Save As to another path: the dialog already confirmed any overwrite.
  let mut expected = if same_file {
    point.expected_stamp.clone()
  } else {
    None
  };

  let stamp = loop {
    let (target, snapshot, encoding, check) = (
      path.clone(),
      point.snapshot.clone(),
      point.encoding,
      expected.clone(),
    );
    let started = std::time::Instant::now();
    let result =
      blocking(move || save_text_file(&target, &snapshot, encoding, check.as_ref())).await?;
    window
      .state::<Perf>()
      .record("document.save", started.elapsed());
    match result {
      Ok(stamp) => break stamp,
      Err(FileError::ModifiedOnDisk { .. }) if expected.is_some() => {
        tracing::warn!(document = %id, path = %path.display(), "file changed on disk before save");
        if !dialogs::confirm_overwrite(window, &title).await {
          return Ok(None);
        }
        expected = None;
      }
      Err(error) => return Err(error.into()),
    }
  };

  let saved_path = paths::normalize(&path);
  let summary = {
    let mut editor = state.lock();
    let document = editor.document_mut(id)?;
    document.mark_saved(&point, saved_path.clone(), stamp);
    window.state::<LspState>().saved(document);
    fold_memory::remember(window.app_handle(), document);
    tracing::info!(document = %id, path = %path.display(), bytes = point.snapshot.len(), "saved file");
    DocumentSummary::of(document)
  };
  let app = window.app_handle();
  app.state::<BackupState>().touch();
  if !same_file {
    ui_state::record_open_files(app);
    super::quick_diff::track(app, id, saved_path.clone());
  }
  crate::schemas::files_changed(app, std::slice::from_ref(&saved_path));
  // The file is saved either way; report invalid settings to the user.
  settings::saved(app, &saved_path)?;
  Ok(Some(summary))
}

/// Saves a document to its file without asking anything, for an extension
/// that changed the file: a file changed on disk since it was read isn't
/// overwritten.
pub fn save_quietly(app: &AppHandle, id: DocumentId) -> Result<(), String> {
  let state = app.state::<EditorState>();
  let (point, path) = {
    let editor = state.lock();
    let document = editor.document(id).map_err(|error| error.to_string())?;
    let path = document.path().ok_or("it has no file")?.to_path_buf();
    (document.save_point(), path)
  };
  let stamp = save_text_file(
    &path,
    &point.snapshot,
    point.encoding,
    point.expected_stamp.as_ref(),
  )
  .map_err(|error| error.to_string())?;
  let summary = {
    let mut editor = state.lock();
    let document = editor.document_mut(id).map_err(|error| error.to_string())?;
    document.mark_saved(&point, path.clone(), stamp);
    app.state::<LspState>().saved(document);
    DocumentSummary::of(document)
  };
  app.state::<BackupState>().touch();
  tracing::info!(document = %id, path = %path.display(), "saved a file an extension changed");
  events::emit(app, AppEvent::DocumentSaved { document: summary });
  if let Err(error) = settings::saved(app, &path) {
    tracing::warn!(message = %error.message, "settings an extension changed are invalid");
  }
  Ok(())
}

async fn blocking<T: Send + 'static>(
  f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, CommandError> {
  tauri::async_runtime::spawn_blocking(f)
    .await
    .map_err(CommandError::internal)
}
