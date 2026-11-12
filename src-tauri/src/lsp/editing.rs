//! Editing through language servers: formatting, renaming, code actions,
//! and signature help. As with hover and completion, each request syncs the
//! document and takes what it needs under the locks, then waits for the
//! server with no lock held. Edits are applied by `edits`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use flareite_core::DocumentId;
use flareite_lsp::convert::{to_offsets, to_range};
use flareite_lsp::lsp_types::request::{
  CodeActionRequest, CodeActionResolveRequest, ExecuteCommand, Formatting, PrepareRenameRequest,
  RangeFormatting, Rename, SignatureHelpRequest,
};
use flareite_lsp::lsp_types::{
  CodeAction, CodeActionContext, CodeActionKind, CodeActionOrCommand, CodeActionParams,
  CodeActionProviderCapability, CodeActionTriggerKind, Command, DocumentFormattingParams,
  DocumentRangeFormattingParams, Documentation, ExecuteCommandParams, FormattingOptions,
  MarkupKind, OneOf, ParameterLabel, PrepareRenameResponse, RenameParams, SignatureHelpContext,
  SignatureHelpParams, SignatureHelpTriggerKind, TextDocumentIdentifier,
  TextDocumentPositionParams,
};
use flareite_lsp::{LanguageServer, LspError};
use flareite_text::TextBuffer;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use super::commands::{REQUEST_TIMEOUT, blocking, hover_content, log_failure, word_range};
use super::edits::{self, AppliedEditDto};
use super::{LspState, Target};
use crate::editor::EditorState;
use crate::editor::protocol::Position;
use crate::error::CommandError;
use crate::extensions::PreviewContentDto;

/// How long a code action's command may take: it may edit many files.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Most code actions offered at once.
const MAX_CODE_ACTIONS: usize = 200;

/// A range of a document, from the frontend.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct TextRange {
  pub start: Position,
  pub end: Position,
}

/// What a request needs from a document, taken under the locks.
struct Prepared {
  target: Option<Target>,
  /// The text the server has, which its answer refers to.
  text: TextBuffer,
  tab_size: u32,
  insert_spaces: bool,
}

fn prepare(app: &AppHandle, id: DocumentId) -> Result<Prepared, CommandError> {
  let state = app.state::<EditorState>();
  let editor = state.lock();
  let document = editor.document(id)?;
  let lsp = app.state::<LspState>();
  lsp.sync(app, document);
  Ok(Prepared {
    target: lsp.target(id),
    text: TextBuffer::from_snapshot(&document.buffer().snapshot()),
    tab_size: u32::try_from(document.tab_size()).unwrap_or(4),
    insert_spaces: document.indent_unit() != "\t",
  })
}

impl Prepared {
  fn identifier(target: &Target) -> TextDocumentIdentifier {
    TextDocumentIdentifier::new(target.uri.clone())
  }

  fn position_params(&self, target: &Target, position: Position) -> TextDocumentPositionParams {
    let offset = position.to_offset(&self.text);
    TextDocumentPositionParams::new(
      Self::identifier(target),
      flareite_lsp::convert::to_position(&self.text, offset, target.server.encoding()),
    )
  }
}

fn provides<T>(capability: Option<&OneOf<bool, T>>) -> bool {
  matches!(capability, Some(OneOf::Left(true) | OneOf::Right(_)))
}

/// What formatting did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FormatOutcome {
  Formatted,
  /// The server had nothing to change, or the text changed meanwhile.
  Unchanged,
  /// No server formats the document (or its selections, for a range).
  NoFormatter,
  Failed,
}

/// Formats document `id`, or `range` of it, with its language server,
/// waiting at most `timeout`.
pub(crate) async fn format(
  app: &AppHandle,
  id: DocumentId,
  range: Option<TextRange>,
  timeout: Duration,
) -> Result<FormatOutcome, CommandError> {
  let prepared = prepare(app, id)?;
  let Some(target) = &prepared.target else {
    return Ok(FormatOutcome::NoFormatter);
  };
  let capabilities = target.server.capabilities();
  let options = FormattingOptions {
    tab_size: prepared.tab_size,
    insert_spaces: prepared.insert_spaces,
    ..Default::default()
  };
  let server = target.server.clone();
  let text_document = Prepared::identifier(target);
  let result = match range {
    None => {
      if !provides(capabilities.document_formatting_provider.as_ref()) {
        return Ok(FormatOutcome::NoFormatter);
      }
      let params = DocumentFormattingParams {
        text_document,
        options,
        work_done_progress_params: Default::default(),
      };
      blocking(move || server.request::<Formatting>(params, timeout, || false)).await?
    }
    Some(range) => {
      if !provides(capabilities.document_range_formatting_provider.as_ref()) {
        return Ok(FormatOutcome::NoFormatter);
      }
      let offsets = range.start.to_offset(&prepared.text)..range.end.to_offset(&prepared.text);
      let params = DocumentRangeFormattingParams {
        text_document,
        range: to_range(&prepared.text, offsets, server.encoding()),
        options,
        work_done_progress_params: Default::default(),
      };
      blocking(move || server.request::<RangeFormatting>(params, timeout, || false)).await?
    }
  };
  let edits = match result {
    Ok(edits) => edits.unwrap_or_default(),
    Err(error) => {
      log_failure("formatting", &error);
      return Ok(FormatOutcome::Failed);
    }
  };
  if edits.is_empty() {
    return Ok(FormatOutcome::Unchanged);
  }
  let (app, server, version) = (app.clone(), target.server.clone(), prepared.text.version());
  let applied =
    blocking(move || edits::apply_to_document(&app, &server, id, version, &edits)).await?;
  Ok(match applied {
    Ok(true) => FormatOutcome::Formatted,
    Ok(false) => FormatOutcome::Unchanged,
    Err(error) => {
      tracing::warn!(document = %id, %error, "could not apply formatting");
      FormatOutcome::Failed
    }
  })
}

/// Formats a document, or `range` of it.
#[tauri::command]
pub async fn lsp_format(
  app: AppHandle,
  id: u64,
  range: Option<TextRange>,
) -> Result<FormatOutcome, CommandError> {
  format(&app, DocumentId::from_raw(id), range, REQUEST_TIMEOUT).await
}

/// What renaming at a place renames.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameTargetDto {
  pub start: Position,
  pub end: Position,
  /// The name to start editing from.
  pub placeholder: String,
}

fn cannot_rename(message: impl Into<String>) -> CommandError {
  CommandError::new("cannotRename", message)
}

/// An error from a request that the user asked for, to show them.
fn request_error(what: &str, error: &LspError) -> CommandError {
  log_failure(what, error);
  match error {
    LspError::Server { message, .. } if !message.is_empty() => {
      CommandError::new("languageServer", message.clone())
    }
    _ => CommandError::new("languageServer", error.to_string()),
  }
}

/// What renaming the symbol at `position` would rename; `None` if its
/// language server can't rename. Fails with `cannotRename` and the
/// server's reason when there's nothing to rename there.
#[tauri::command]
pub async fn lsp_prepare_rename(
  app: AppHandle,
  id: u64,
  position: Position,
) -> Result<Option<RenameTargetDto>, CommandError> {
  let prepared = prepare(&app, DocumentId::from_raw(id))?;
  let Some(target) = &prepared.target else {
    return Ok(None);
  };
  let prepare_support = match &target.server.capabilities().rename_provider {
    None | Some(OneOf::Left(false)) => return Ok(None),
    Some(OneOf::Left(true)) => false,
    Some(OneOf::Right(options)) => options.prepare_provider == Some(true),
  };
  let text = &prepared.text;
  let offset = position.to_offset(text);
  let mut range = word_range(text, offset);
  let mut placeholder = None;
  if prepare_support {
    let params = prepared.position_params(target, position);
    let server = target.server.clone();
    let answer =
      blocking(move || server.request::<PrepareRenameRequest>(params, REQUEST_TIMEOUT, || false))
        .await?;
    let encoding = target.server.encoding();
    match answer {
      Ok(Some(PrepareRenameResponse::Range(found))) => range = to_offsets(text, found, encoding),
      Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: found,
        placeholder: name,
      })) => {
        range = to_offsets(text, found, encoding);
        placeholder = Some(name);
      }
      Ok(Some(PrepareRenameResponse::DefaultBehavior { .. })) => {}
      Ok(None) => return Err(cannot_rename("The element can't be renamed.")),
      Err(LspError::Server { message, .. }) if !message.is_empty() => {
        return Err(cannot_rename(message));
      }
      Err(error) => return Err(request_error("prepare rename", &error)),
    }
  }
  if range.is_empty() && placeholder.is_none() {
    return Err(cannot_rename("There's nothing to rename here."));
  }
  Ok(Some(RenameTargetDto {
    start: text.offset_to_utf16(range.start).into(),
    end: text.offset_to_utf16(range.end).into(),
    placeholder: placeholder.unwrap_or_else(|| text.text_in(range)),
  }))
}

/// Renames the symbol at `position` to `name` everywhere its language
/// server finds it.
#[tauri::command]
pub async fn lsp_rename(
  app: AppHandle,
  id: u64,
  position: Position,
  name: String,
) -> Result<AppliedEditDto, CommandError> {
  if name.trim().is_empty() {
    return Err(cannot_rename("The new name is empty."));
  }
  let prepared = prepare(&app, DocumentId::from_raw(id))?;
  let Some(target) = &prepared.target else {
    return Err(cannot_rename("No language server renames here."));
  };
  let params = RenameParams {
    text_document_position: prepared.position_params(target, position),
    new_name: name,
    work_done_progress_params: Default::default(),
  };
  let server = target.server.clone();
  let answer =
    blocking(move || server.request::<Rename>(params, COMMAND_TIMEOUT, || false)).await?;
  let edit = match answer {
    Ok(Some(edit)) => edit,
    Ok(None) => return Err(cannot_rename("Nothing was renamed.")),
    Err(LspError::Server { message, .. }) if !message.is_empty() => {
      return Err(cannot_rename(message));
    }
    Err(error) => return Err(request_error("rename", &error)),
  };
  let server = target.server.clone();
  blocking(move || edits::apply(&app, &server, edit))
    .await?
    .map_err(cannot_rename)
}

/// The code actions last offered, for choosing one by `token` and index.
pub(super) struct OfferedActions {
  token: u64,
  server: Arc<LanguageServer>,
  actions: Vec<CodeActionOrCommand>,
}

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeActionDto {
  pub title: String,
  /// Such as `quickfix` or `refactor.extract`.
  pub kind: Option<String>,
  /// The fix to apply when one is chosen without looking.
  pub preferred: bool,
  /// Why it can't be applied now, if it can't.
  pub disabled: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeActionsDto {
  /// Names this list for `lsp_apply_code_action`.
  pub token: u64,
  pub actions: Vec<CodeActionDto>,
}

fn is_quick_fix(action: &CodeActionOrCommand) -> bool {
  matches!(action, CodeActionOrCommand::CodeAction(CodeAction { kind: Some(kind), .. })
    if kind.as_str().starts_with(CodeActionKind::QUICKFIX.as_str()))
}

fn is_preferred(action: &CodeActionOrCommand) -> bool {
  matches!(
    action,
    CodeActionOrCommand::CodeAction(CodeAction {
      is_preferred: Some(true),
      ..
    })
  )
}

/// The code actions for `range` of a document: quick fixes for its problems
/// there, refactorings, and so on, preferred fixes first. `automatic` when
/// the editor looks for them itself (where the cursor rests), which leaves
/// out those that can't be applied. `only` limits them to a kind, such as
/// `source.organizeImports`. `None` without a server that offers them.
#[tauri::command]
pub async fn lsp_code_actions(
  app: AppHandle,
  id: u64,
  range: TextRange,
  automatic: bool,
  only: Option<String>,
) -> Result<Option<CodeActionsDto>, CommandError> {
  let id = DocumentId::from_raw(id);
  let (target, text, diagnostics) = {
    let state = app.state::<EditorState>();
    let editor = state.lock();
    let document = editor.document(id)?;
    let lsp = app.state::<LspState>();
    lsp.sync(&app, document);
    let Some(target) = lsp.target(id) else {
      return Ok(None);
    };
    let text = TextBuffer::from_snapshot(&document.buffer().snapshot());
    let offsets = range.start.to_offset(&text)..range.end.to_offset(&text);
    let diagnostics = lsp.diagnostics_in(document, offsets, target.server.encoding());
    (target, text, diagnostics)
  };
  if matches!(
    target.server.capabilities().code_action_provider,
    None | Some(CodeActionProviderCapability::Simple(false))
  ) {
    return Ok(None);
  }
  let offsets = range.start.to_offset(&text)..range.end.to_offset(&text);
  let params = CodeActionParams {
    text_document: Prepared::identifier(&target),
    range: to_range(&text, offsets, target.server.encoding()),
    context: CodeActionContext {
      diagnostics,
      only: only.map(|kind| vec![CodeActionKind::from(kind)]),
      trigger_kind: Some(if automatic {
        CodeActionTriggerKind::AUTOMATIC
      } else {
        CodeActionTriggerKind::INVOKED
      }),
    },
    work_done_progress_params: Default::default(),
    partial_result_params: Default::default(),
  };
  let server = target.server.clone();
  let answer =
    blocking(move || server.request::<CodeActionRequest>(params, REQUEST_TIMEOUT, || false))
      .await?;
  let mut actions = match answer {
    Ok(actions) => actions.unwrap_or_default(),
    Err(error) if automatic => {
      log_failure("code actions", &error);
      return Ok(None);
    }
    Err(error) => return Err(request_error("code actions", &error)),
  };
  if automatic {
    actions.retain(|action| {
      !matches!(
        action,
        CodeActionOrCommand::CodeAction(CodeAction {
          disabled: Some(_),
          ..
        })
      )
    });
  }
  // Preferred fixes, then other fixes, then the rest, each in the server's
  // order.
  actions.sort_by_key(|action| (!is_preferred(action), !is_quick_fix(action)));
  actions.truncate(MAX_CODE_ACTIONS);
  let listed = actions
    .iter()
    .map(|action| match action {
      CodeActionOrCommand::Command(command) => CodeActionDto {
        title: command.title.clone(),
        kind: None,
        preferred: false,
        disabled: None,
      },
      CodeActionOrCommand::CodeAction(action) => CodeActionDto {
        title: action.title.clone(),
        kind: action.kind.as_ref().map(|kind| kind.as_str().to_owned()),
        preferred: action.is_preferred == Some(true),
        disabled: action.disabled.as_ref().map(|why| why.reason.clone()),
      },
    })
    .collect();
  let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
  app.state::<LspState>().lock().code_actions = Some(OfferedActions {
    token,
    server: target.server.clone(),
    actions,
  });
  Ok(Some(CodeActionsDto {
    token,
    actions: listed,
  }))
}

/// What applying a code action did.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeActionAppliedDto {
  pub edit: AppliedEditDto,
  /// A command for the editor itself that the action asked for (such as
  /// `editor.action.triggerSuggest`), which the server can't run.
  pub command: Option<String>,
}

/// Runs a command on `server` if it offers it; otherwise returns it, for
/// the editor.
async fn execute(
  server: Arc<LanguageServer>,
  command: Command,
) -> Result<Option<String>, CommandError> {
  let offered = server
    .capabilities()
    .execute_command_provider
    .as_ref()
    .is_some_and(|provider| provider.commands.contains(&command.command));
  if !offered {
    tracing::debug!(command = %command.command, "code action command left for the editor");
    return Ok(Some(command.command));
  }
  let params = ExecuteCommandParams {
    command: command.command,
    arguments: command.arguments.unwrap_or_default(),
    work_done_progress_params: Default::default(),
  };
  // Edits the command makes arrive as `workspace/applyEdit` meanwhile.
  match blocking(move || server.request::<ExecuteCommand>(params, COMMAND_TIMEOUT, || false))
    .await?
  {
    Ok(_) => Ok(None),
    Err(error) => Err(request_error("execute command", &error)),
  }
}

/// Applies code action `index` of the list `token` names: its edit, then
/// its command.
#[tauri::command]
pub async fn lsp_apply_code_action(
  app: AppHandle,
  token: u64,
  index: usize,
) -> Result<CodeActionAppliedDto, CommandError> {
  let offered = {
    let lsp = app.state::<LspState>();
    let inner = lsp.lock();
    inner
      .code_actions
      .as_ref()
      .filter(|offered| offered.token == token)
      .and_then(|offered| Some((offered.server.clone(), offered.actions.get(index)?.clone())))
  };
  let Some((server, action)) = offered else {
    return Err(CommandError::new(
      "stale",
      "The code actions have changed. Try again.",
    ));
  };
  let mut applied = CodeActionAppliedDto::default();
  let mut action = match action {
    CodeActionOrCommand::Command(command) => {
      applied.command = execute(server, command).await?;
      return Ok(applied);
    }
    CodeActionOrCommand::CodeAction(action) => action,
  };
  if let Some(disabled) = &action.disabled {
    return Err(CommandError::new("disabled", disabled.reason.clone()));
  }
  let resolves = matches!(
    &server.capabilities().code_action_provider,
    Some(CodeActionProviderCapability::Options(options)) if options.resolve_provider == Some(true)
  );
  if action.edit.is_none() && resolves {
    let (resolving, params) = (server.clone(), action.clone());
    match blocking(move || {
      resolving.request::<CodeActionResolveRequest>(params, REQUEST_TIMEOUT, || false)
    })
    .await?
    {
      Ok(resolved) => action = resolved,
      Err(error) => return Err(request_error("resolve code action", &error)),
    }
  }
  if let Some(edit) = action.edit.take() {
    let (app, server) = (app.clone(), server.clone());
    applied.edit = blocking(move || edits::apply(&app, &server, edit))
      .await?
      .map_err(|error| CommandError::new("languageServer", error))?;
  }
  if let Some(command) = action.command.take() {
    applied.command = execute(server, command).await?;
  }
  Ok(applied)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParameterDto {
  /// Where it is in its signature's label, in UTF-16 units.
  pub start: usize,
  pub end: usize,
  pub documentation: Option<PreviewContentDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureDto {
  pub label: String,
  pub documentation: Option<PreviewContentDto>,
  pub parameters: Vec<ParameterDto>,
  /// The parameter being written, if known.
  pub active_parameter: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureHelpDto {
  pub signatures: Vec<SignatureDto>,
  pub active_signature: usize,
}

fn documentation_markdown(documentation: Documentation) -> Option<String> {
  let text = match documentation {
    Documentation::String(text) => text,
    Documentation::MarkupContent(markup) => match markup.kind {
      MarkupKind::Markdown => markup.value,
      MarkupKind::PlainText => format!("```text\n{}\n```", markup.value),
    },
  };
  let text = text.trim();
  (!text.is_empty()).then(|| text.to_owned())
}

/// Where each parameter is in `label`, in UTF-16 units: from its offsets,
/// or found after the one before.
fn parameter_ranges(label: &str, parameters: &[ParameterLabel]) -> Vec<(usize, usize)> {
  let utf16 = |bytes: usize| label[..bytes].encode_utf16().count();
  let length = label.encode_utf16().count();
  let mut from = label.find('(').map_or(0, |open| open + 1);
  parameters
    .iter()
    .map(|parameter| match parameter {
      ParameterLabel::LabelOffsets([start, end]) => {
        let end = (*end as usize).min(length);
        ((*start as usize).min(end), end)
      }
      ParameterLabel::Simple(name) => {
        match label.get(from..).and_then(|rest| rest.find(name.as_str())) {
          Some(found) => {
            let start = from + found;
            from = start + name.len();
            (utf16(start), utf16(from))
          }
          None => (0, 0),
        }
      }
    })
    .collect()
}

/// The signatures of the call at `position`, with the parameter being
/// written. `trigger` is the character whose typing asked; `retrigger`
/// when help is already showing.
#[tauri::command]
pub async fn lsp_signature_help(
  app: AppHandle,
  id: u64,
  position: Position,
  trigger: Option<String>,
  retrigger: bool,
) -> Result<Option<SignatureHelpDto>, CommandError> {
  let prepared = prepare(&app, DocumentId::from_raw(id))?;
  let Some(target) = &prepared.target else {
    return Ok(None);
  };
  if target
    .server
    .capabilities()
    .signature_help_provider
    .is_none()
  {
    return Ok(None);
  }
  let params = SignatureHelpParams {
    context: Some(SignatureHelpContext {
      trigger_kind: if trigger.is_some() {
        SignatureHelpTriggerKind::TRIGGER_CHARACTER
      } else if retrigger {
        SignatureHelpTriggerKind::CONTENT_CHANGE
      } else {
        SignatureHelpTriggerKind::INVOKED
      },
      trigger_character: trigger,
      is_retrigger: retrigger,
      active_signature_help: None,
    }),
    text_document_position_params: prepared.position_params(target, position),
    work_done_progress_params: Default::default(),
  };
  let server = target.server.clone();
  let help = match blocking(move || {
    server.request::<SignatureHelpRequest>(params, REQUEST_TIMEOUT, || false)
  })
  .await?
  {
    Ok(Some(help)) if !help.signatures.is_empty() => help,
    Ok(_) => return Ok(None),
    Err(error) => {
      log_failure("signature help", &error);
      return Ok(None);
    }
  };
  let active_signature =
    (help.active_signature.unwrap_or(0) as usize).min(help.signatures.len() - 1);
  let shared_active = help.active_parameter;
  blocking(move || {
    let signatures = help
      .signatures
      .into_iter()
      .map(|signature| {
        let labels: Vec<ParameterLabel> = signature
          .parameters
          .iter()
          .flatten()
          .map(|parameter| parameter.label.clone())
          .collect();
        let ranges = parameter_ranges(&signature.label, &labels);
        let parameters = signature
          .parameters
          .into_iter()
          .flatten()
          .zip(ranges)
          .map(|(parameter, (start, end))| ParameterDto {
            start,
            end,
            documentation: parameter
              .documentation
              .and_then(documentation_markdown)
              .map(|markdown| hover_content(&app, &markdown)),
          })
          .collect();
        SignatureDto {
          documentation: signature
            .documentation
            .and_then(documentation_markdown)
            .map(|markdown| hover_content(&app, &markdown)),
          active_parameter: signature
            .active_parameter
            .or(shared_active)
            .map(|active| active as usize),
          label: signature.label,
          parameters,
        }
      })
      .collect();
    Some(SignatureHelpDto {
      signatures,
      active_signature,
    })
  })
  .await
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parameters_are_found_in_signature_labels() {
    let label = "fn add(a: u8, b: u8) -> u8";
    let ranges = parameter_ranges(
      label,
      &[
        ParameterLabel::Simple("a: u8".into()),
        ParameterLabel::Simple("b: u8".into()),
      ],
    );
    assert_eq!(ranges, [(7, 12), (14, 19)]);
    // Names that appear earlier (`u8` in the name) are found after the
    // parameter before.
    let ranges = parameter_ranges(
      "u8(u8, u8)",
      &[
        ParameterLabel::Simple("u8".into()),
        ParameterLabel::Simple("u8".into()),
      ],
    );
    assert_eq!(ranges, [(3, 5), (7, 9)]);
    // UTF-16 units, and offsets kept within the label.
    let ranges = parameter_ranges(
      "é(x)",
      &[
        ParameterLabel::Simple("x".into()),
        ParameterLabel::LabelOffsets([2, 99]),
      ],
    );
    assert_eq!(ranges, [(2, 3), (2, 4)]);
  }

  #[test]
  fn plain_documentation_is_shown_as_is() {
    let plain = Documentation::MarkupContent(flareite_lsp::lsp_types::MarkupContent {
      kind: MarkupKind::PlainText,
      value: "a *b*".into(),
    });
    assert_eq!(
      documentation_markdown(plain).as_deref(),
      Some("```text\na *b*\n```")
    );
    assert_eq!(
      documentation_markdown(Documentation::String("  ".into())),
      None
    );
  }
}
