//! Requests made for the editor: hover, go to definition, references, and
//! completion. Each syncs the document, takes what it needs under the locks,
//! and waits for the server on a blocking thread with no lock held. Results
//! are mapped to the document as it is when they arrive.

use std::collections::BTreeMap;
use std::iter::Peekable;
use std::path::PathBuf;
use std::str::Chars;
use std::sync::Arc;
use std::time::Duration;

use flareite_core::{DocumentId, MAX_DISPLAY_LINE_BYTES};
use flareite_extension_host::preview::{
  self, LinkTarget, PreviewContent, PreviewElement, PreviewNode,
};
use flareite_lsp::LspError;
use flareite_lsp::convert::{PositionEncoding, to_offset, to_offsets, to_position, uri_to_path};
use flareite_lsp::lsp_types::request::{Completion, GotoDefinition, HoverRequest, References};
use flareite_lsp::lsp_types::{
  CompletionContext, CompletionItemKind, CompletionParams, CompletionResponse, CompletionTextEdit,
  CompletionTriggerKind, GotoDefinitionParams, GotoDefinitionResponse, HoverContents, HoverParams,
  HoverProviderCapability, InsertTextFormat, MarkedString, MarkupKind, OneOf, Range,
  ReferenceContext, ReferenceParams, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
};
use flareite_text::{TextBuffer, map_offset_through};
use flareite_workspace::load_text_file;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use super::{DiagnosticMessageDto, LspState, Target};
use crate::editor::EditorState;
use crate::editor::protocol::Position;
use crate::error::CommandError;
use crate::extensions::PreviewContentDto;
use crate::paths;

/// How long the editor waits for a server's answer.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Most completion items returned.
const MAX_COMPLETIONS: usize = 200;
/// Longest word before the cursor considered for completion.
const MAX_WORD_CHARS: usize = 256;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverDto {
  /// What the server and extensions say about the place, from their
  /// Markdown: drawn as previews are, with code blocks highlighted.
  pub content: Option<PreviewContentDto>,
  /// Diagnostics at the position.
  pub diagnostics: Vec<DiagnosticMessageDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocationDto {
  pub path: String,
  pub start: Position,
  pub end: Position,
}

/// A reference to a symbol.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceDto {
  pub path: String,
  pub start: Position,
  pub end: Position,
  /// The text of the line it starts on, for previews.
  pub line: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionItemDto {
  pub label: String,
  pub kind: Option<&'static str>,
  pub detail: Option<String>,
  /// Replaces the text from `from` to the cursor.
  pub insert: String,
  pub from: Position,
  /// Matched against what is typed after `from`.
  pub filter: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionDto {
  pub items: Vec<CompletionItemDto>,
  /// More typing should ask the server again rather than filter these.
  pub incomplete: bool,
}

pub(super) async fn blocking<T: Send + 'static>(
  f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, CommandError> {
  tauri::async_runtime::spawn_blocking(f)
    .await
    .map_err(CommandError::internal)
}

pub(super) fn log_failure(request: &str, error: &LspError) {
  match error {
    LspError::Server { code, .. }
      if *code == flareite_lsp::REQUEST_CANCELLED || *code == flareite_lsp::CONTENT_MODIFIED =>
    {
      tracing::debug!(request, %error, "language server request superseded");
    }
    _ => tracing::warn!(request, %error, "language server request failed"),
  }
}

/// The document's state for a request at `position`.
struct Prepared {
  target: Option<Target>,
  offset: usize,
  /// The text the server has, which its answer refers to.
  text: TextBuffer,
  diagnostics: Vec<DiagnosticMessageDto>,
  /// The document's language, for extensions' completion and hover
  /// providers.
  language: Option<String>,
}

fn prepare(app: &AppHandle, id: DocumentId, position: Position) -> Result<Prepared, CommandError> {
  let state = app.state::<EditorState>();
  let editor = state.lock();
  let document = editor.document(id)?;
  let lsp = app.state::<LspState>();
  lsp.sync(app, document);
  let buffer = document.buffer();
  let offset = position.to_offset(buffer);
  let mut diagnostics = lsp.diagnostics_at(document, offset);
  diagnostics.extend(
    app
      .state::<crate::extensions::ExtensionsState>()
      .messages_at(document, offset),
  );
  diagnostics.extend(
    app
      .state::<crate::schemas::SchemaState>()
      .messages_at(document, offset),
  );
  diagnostics.sort_by_key(|d| d.severity);
  Ok(Prepared {
    target: lsp.target(id),
    offset,
    text: TextBuffer::from_snapshot(&buffer.snapshot()),
    diagnostics,
    language: app
      .state::<crate::languages::LanguagesState>()
      .for_document(document)
      .map(|language| language.id().to_owned()),
  })
}

impl Prepared {
  /// The request as extensions' providers get it: the line, and the column
  /// in UTF-8 bytes.
  fn feature_request(
    &self,
    id: DocumentId,
  ) -> flareite_extension_host::language_features::FeatureRequest {
    let line = self.text.line_of(self.offset);
    flareite_extension_host::language_features::FeatureRequest {
      document: id,
      version: self.text.version().get(),
      line: u32::try_from(line).unwrap_or(u32::MAX),
      column: u32::try_from(self.offset - self.text.line_start(line)).unwrap_or(u32::MAX),
    }
  }

  fn position_params(&self, target: &Target) -> TextDocumentPositionParams {
    TextDocumentPositionParams::new(
      TextDocumentIdentifier::new(target.uri.clone()),
      to_position(&self.text, self.offset, target.server.encoding()),
    )
  }
}

/// The extensions' host, if extensions run: for their completion and
/// hover providers.
fn providers(app: &AppHandle) -> Option<&flareite_extension_host::ExtensionHost> {
  app
    .state::<crate::extensions::ExtensionsState>()
    .inner()
    .host()
}

/// Information about the symbol at `position`, and diagnostics there.
#[tauri::command]
pub async fn lsp_hover(
  app: AppHandle,
  id: u64,
  position: Position,
) -> Result<Option<HoverDto>, CommandError> {
  let id = DocumentId::from_raw(id);
  let prepared = prepare(&app, id, position)?;
  let mut markdown = None;
  if let Some(target) = &prepared.target
    && match &target.server.capabilities().hover_provider {
      Some(HoverProviderCapability::Simple(enabled)) => *enabled,
      Some(HoverProviderCapability::Options(_)) => true,
      None => false,
    }
  {
    let params = HoverParams {
      text_document_position_params: prepared.position_params(target),
      work_done_progress_params: Default::default(),
    };
    let server = target.server.clone();
    match blocking(move || server.request::<HoverRequest>(params, REQUEST_TIMEOUT, || false))
      .await?
    {
      Ok(hover) => markdown = hover.and_then(|hover| hover_markdown(hover.contents)),
      Err(error) => log_failure("hover", &error),
    }
  }
  // What extensions' hover providers say, first.
  if let Some(language) = prepared.language.clone()
    && providers(&app).is_some_and(|host| host.hovers(&language))
  {
    let request = prepared.feature_request(id);
    let app = app.clone();
    let described = blocking(move || {
      providers(&app).map_or_else(Vec::new, |host| host.hover(&language, request))
    })
    .await?;
    if !described.is_empty() {
      let described = described.join("\n\n---\n\n");
      markdown = Some(match markdown {
        Some(server) => format!("{described}\n\n---\n\n{server}"),
        None => described,
      });
    }
  }
  if markdown.is_none() && prepared.diagnostics.is_empty() {
    return Ok(None);
  }
  let content = match markdown {
    Some(markdown) => Some(blocking(move || hover_content(&app, &markdown)).await?),
    None => None,
  };
  Ok(Some(HoverDto {
    content,
    diagnostics: prepared.diagnostics,
  }))
}

/// A hover's Markdown as preview content, with its code blocks highlighted.
/// Links to web addresses and `file:` addresses are followed (`#L12` names
/// a line); there's no folder for relative links to be found from.
fn hover_content(app: &AppHandle, markdown: &str) -> PreviewContentDto {
  let mut content = preview::from_markdown(markdown, None).unwrap_or_else(|error| {
    // Too large or too deep to draw: shown as it is.
    tracing::debug!(%error, "hover Markdown shown as text");
    PreviewContent {
      nodes: vec![PreviewNode {
        parent: None,
        element: PreviewElement::CodeBlock {
          language: None,
          text: markdown.to_owned(),
        },
      }],
    }
  });
  for node in &mut content.nodes {
    if let PreviewElement::Link { target, .. } = &mut node.element
      && let LinkTarget::Unsupported { destination } = target
      && let Some(file) = file_link(destination)
    {
      *target = file;
    }
  }
  crate::extensions::highlighted(app, Arc::new(content))
}

/// Where a `file:` address leads, with a `#L12` (or `#12`) line as the
/// anchor `L12`.
fn file_link(destination: &str) -> Option<LinkTarget> {
  let scheme = destination.get(..5)?;
  if !scheme.eq_ignore_ascii_case("file:") {
    return None;
  }
  let (address, fragment) = match destination.split_once('#') {
    Some((address, fragment)) => (address, Some(fragment)),
    None => (destination, None),
  };
  let address = address
    .split_once('?')
    .map_or(address, |(address, _)| address);
  let uri: Uri = format!("file:{}", &address[5..]).parse().ok()?;
  let path = uri_to_path(&uri)?;
  let anchor = fragment
    .filter(|fragment| !fragment.is_empty())
    .map(|fragment| {
      let digits = fragment.strip_prefix('L').unwrap_or(fragment);
      let line: String = digits.chars().take_while(char::is_ascii_digit).collect();
      if line.is_empty() {
        fragment.to_owned()
      } else {
        format!("L{line}")
      }
    });
  Some(LinkTarget::File {
    path: path.to_string_lossy().into_owned(),
    anchor,
  })
}

fn hover_markdown(contents: HoverContents) -> Option<String> {
  let marked = |marked: MarkedString| match marked {
    MarkedString::String(text) => text,
    MarkedString::LanguageString(code) => format!("```{}\n{}\n```", code.language, code.value),
  };
  let text = match contents {
    HoverContents::Scalar(item) => marked(item),
    HoverContents::Array(items) => items
      .into_iter()
      .map(marked)
      .collect::<Vec<_>>()
      .join("\n\n"),
    HoverContents::Markup(markup) => match markup.kind {
      MarkupKind::Markdown => markup.value,
      // Shown as is, rather than interpreted as Markdown.
      MarkupKind::PlainText => format!("```text\n{}\n```", markup.value),
    },
  };
  let text = text.trim();
  (!text.is_empty()).then(|| text.to_owned())
}

/// Where the symbol at `position` is defined.
#[tauri::command]
pub async fn lsp_definition(
  app: AppHandle,
  id: u64,
  position: Position,
) -> Result<Option<LocationDto>, CommandError> {
  let prepared = prepare(&app, DocumentId::from_raw(id), position)?;
  let Some(target) = &prepared.target else {
    return Ok(None);
  };
  if matches!(
    target.server.capabilities().definition_provider,
    None | Some(OneOf::Left(false))
  ) {
    return Ok(None);
  }
  let params = GotoDefinitionParams {
    text_document_position_params: prepared.position_params(target),
    work_done_progress_params: Default::default(),
    partial_result_params: Default::default(),
  };
  let server = target.server.clone();
  let response =
    match blocking(move || server.request::<GotoDefinition>(params, REQUEST_TIMEOUT, || false))
      .await?
    {
      Ok(response) => response,
      Err(error) => {
        log_failure("definition", &error);
        return Ok(None);
      }
    };
  let location = match response {
    Some(GotoDefinitionResponse::Scalar(location)) => Some((location.uri, location.range)),
    Some(GotoDefinitionResponse::Array(locations)) => locations
      .into_iter()
      .next()
      .map(|location| (location.uri, location.range)),
    Some(GotoDefinitionResponse::Link(links)) => links
      .into_iter()
      .next()
      .map(|link| (link.target_uri, link.target_selection_range)),
    None => None,
  };
  let resolved = resolve(
    &app,
    target.server.encoding(),
    location.into_iter().collect(),
  )
  .await?;
  Ok(resolved.into_iter().next().map(|reference| LocationDto {
    path: reference.path,
    start: reference.start,
    end: reference.end,
  }))
}

/// The references to the symbol at `position`, including its declaration,
/// in order of path and position.
#[tauri::command]
pub async fn lsp_references(
  app: AppHandle,
  id: u64,
  position: Position,
) -> Result<Option<Vec<ReferenceDto>>, CommandError> {
  let prepared = prepare(&app, DocumentId::from_raw(id), position)?;
  let Some(target) = &prepared.target else {
    return Ok(None);
  };
  if matches!(
    target.server.capabilities().references_provider,
    None | Some(OneOf::Left(false))
  ) {
    return Ok(None);
  }
  let params = ReferenceParams {
    text_document_position: prepared.position_params(target),
    work_done_progress_params: Default::default(),
    partial_result_params: Default::default(),
    context: ReferenceContext {
      include_declaration: true,
    },
  };
  let server = target.server.clone();
  let locations = match blocking(move || {
    server.request::<References>(params, REQUEST_TIMEOUT, || false)
  })
  .await?
  {
    Ok(locations) => locations.unwrap_or_default(),
    Err(error) => {
      log_failure("references", &error);
      return Ok(None);
    }
  };
  let locations = locations
    .into_iter()
    .map(|location| (location.uri, location.range))
    .collect();
  Ok(Some(
    resolve(&app, target.server.encoding(), locations).await?,
  ))
}

/// `locations` as positions in their files' text: an open document's as it
/// is now, or the file's on disk. Those in files that can't be read are left
/// out. Sorted by path and position, without duplicates.
pub(super) async fn resolve(
  app: &AppHandle,
  encoding: PositionEncoding,
  locations: Vec<(Uri, Range)>,
) -> Result<Vec<ReferenceDto>, CommandError> {
  let mut by_path: BTreeMap<PathBuf, Vec<Range>> = BTreeMap::new();
  for (uri, range) in locations {
    if let Some(path) = uri_to_path(&uri) {
      by_path
        .entry(paths::normalize(&path))
        .or_default()
        .push(range);
    }
  }
  // Open documents' text now; other files are read without the lock.
  let mut texts: Vec<(PathBuf, Vec<Range>, Option<TextBuffer>)> = {
    let state = app.state::<EditorState>();
    let editor = state.lock();
    by_path
      .into_iter()
      .map(|(path, ranges)| {
        let open = editor
          .find_by_path(&path)
          .and_then(|id| editor.document(id).ok())
          .map(|document| TextBuffer::from_snapshot(&document.buffer().snapshot()));
        (path, ranges, open)
      })
      .collect()
  };
  texts = blocking(move || {
    for (path, _, text) in &mut texts {
      if text.is_none() {
        match load_text_file(path) {
          Ok(loaded) => *text = Some(TextBuffer::from_text(&loaded.text)),
          Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read a referenced file");
          }
        }
      }
    }
    texts
  })
  .await?;
  let mut resolved = Vec::new();
  for (path, ranges, text) in texts {
    let Some(buffer) = text else {
      continue;
    };
    let mut references: Vec<ReferenceDto> = ranges
      .into_iter()
      .map(|range| {
        let offsets = to_offsets(&buffer, range, encoding);
        let start = Position::from(buffer.offset_to_utf16(offsets.start));
        ReferenceDto {
          path: path.display().to_string(),
          start,
          end: buffer.offset_to_utf16(offsets.end).into(),
          line: buffer.line_prefix(start.line, MAX_DISPLAY_LINE_BYTES).0,
        }
      })
      .collect();
    references.sort_by_key(|reference| {
      (
        reference.start.line,
        reference.start.column,
        reference.end.line,
        reference.end.column,
      )
    });
    references.dedup();
    resolved.extend(references);
  }
  Ok(resolved)
}

fn is_word_char(ch: char) -> bool {
  ch.is_alphanumeric() || ch == '_' || ch == '$'
}

/// The start of the word ending at `offset`.
fn word_start(buffer: &TextBuffer, offset: usize) -> usize {
  let line_start = buffer.line_start(buffer.line_of(offset));
  let before = buffer.text_in(line_start..offset);
  let mut start = offset;
  for ch in before.chars().rev().take(MAX_WORD_CHARS) {
    if !is_word_char(ch) {
      break;
    }
    start -= ch.len_utf8();
  }
  start
}

/// Completions at `position`. `trigger` is the character that was typed to
/// ask for them, if any.
#[tauri::command]
pub async fn lsp_completion(
  app: AppHandle,
  id: u64,
  position: Position,
  trigger: Option<String>,
) -> Result<Option<CompletionDto>, CommandError> {
  let id = DocumentId::from_raw(id);
  let prepared = prepare(&app, id, position)?;
  // What extensions' completion providers offer.
  let provided = match prepared.language.clone() {
    Some(language)
      if providers(&app).is_some_and(|host| host.completion_triggers(&language).is_some()) =>
    {
      let request = prepared.feature_request(id);
      let trigger = trigger.clone();
      let app = app.clone();
      blocking(move || {
        providers(&app).map_or_else(Vec::new, |host| host.complete(&language, request, trigger))
      })
      .await?
    }
    _ => Vec::new(),
  };
  let mut items = Vec::new();
  let mut incomplete = false;
  let mut encoding = None;
  if let Some(target) = &prepared.target
    && target.server.capabilities().completion_provider.is_some()
  {
    let params = CompletionParams {
      text_document_position: prepared.position_params(target),
      work_done_progress_params: Default::default(),
      partial_result_params: Default::default(),
      context: Some(CompletionContext {
        trigger_kind: if trigger.is_some() {
          CompletionTriggerKind::TRIGGER_CHARACTER
        } else {
          CompletionTriggerKind::INVOKED
        },
        trigger_character: trigger,
      }),
    };
    let server = target.server.clone();
    match blocking(move || server.request::<Completion>(params, REQUEST_TIMEOUT, || false)).await? {
      Ok(Some(CompletionResponse::Array(array))) => items = array,
      Ok(Some(CompletionResponse::List(list))) => {
        items = list.items;
        incomplete = list.is_incomplete;
      }
      Ok(None) => {}
      Err(error) => log_failure("completion", &error),
    }
    encoding = Some(target.server.encoding());
  }
  if items.is_empty() && provided.is_empty() {
    return Ok(None);
  }

  let text = &prepared.text;
  let cursor = prepared.offset;
  let default_from = word_start(text, cursor);
  let line_start = text.line_start(text.line_of(cursor));
  let mut ranked: Vec<(u8, String, CompletionItemDto)> = Vec::new();
  let state = app.state::<EditorState>();
  let editor = state.lock();
  let document = editor.document(id)?;
  let buffer = document.buffer();
  // Offsets in the requested text, mapped to the current one.
  let edits = buffer.edits_since(text.version()).unwrap_or_default();
  let current = |offset: usize| -> Position {
    buffer
      .offset_to_utf16(map_offset_through(&edits, offset, false))
      .into()
  };
  for item in provided {
    // On the cursor's line, before it (the extension host checked).
    let from = (line_start + item.column as usize).min(cursor);
    if !text.is_char_boundary(from) {
      continue;
    }
    let typed = text.text_in(from..cursor);
    let Some(tier) = match_tier(&item.filter, &typed) else {
      continue;
    };
    let sort = item.sort;
    let detail = match (item.deprecated, item.detail) {
      (true, Some(detail)) => Some(format!("{detail} (deprecated)")),
      (true, None) => Some("deprecated".to_owned()),
      (false, detail) => detail,
    };
    ranked.push((
      tier,
      sort,
      CompletionItemDto {
        label: item.label,
        kind: Some(item.kind),
        detail,
        insert: item.insert,
        from: current(from),
        filter: item.filter,
      },
    ));
  }
  // Server items only come with a server.
  let encoding = encoding.unwrap_or(flareite_lsp::convert::PositionEncoding::Utf16);
  for item in items {
    let (from, insert) = match item.text_edit {
      Some(CompletionTextEdit::Edit(edit)) => {
        (to_offset(text, edit.range.start, encoding), edit.new_text)
      }
      Some(CompletionTextEdit::InsertAndReplace(edit)) => {
        (to_offset(text, edit.insert.start, encoding), edit.new_text)
      }
      None => (
        default_from,
        item.insert_text.unwrap_or_else(|| item.label.clone()),
      ),
    };
    // Edits must start on the cursor's line, before it.
    let from = if (line_start..=cursor).contains(&from) {
      from
    } else {
      default_from
    };
    let insert = if item.insert_text_format == Some(InsertTextFormat::SNIPPET) {
      strip_snippet(&insert)
    } else {
      insert
    };
    let filter = item.filter_text.unwrap_or_else(|| item.label.clone());
    let typed = text.text_in(from..cursor);
    let Some(tier) = match_tier(&filter, &typed) else {
      continue;
    };
    let sort = item.sort_text.unwrap_or_else(|| item.label.clone());
    ranked.push((
      tier,
      sort,
      CompletionItemDto {
        label: item.label,
        kind: item.kind.map(kind_name),
        detail: item.detail,
        insert,
        from: current(from),
        filter,
      },
    ));
  }
  ranked.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
  // Typing more asks again for what was left out (such as the rest of the
  // commands in `[keybindings]`).
  incomplete |= ranked.len() > MAX_COMPLETIONS;
  ranked.truncate(MAX_COMPLETIONS);
  Ok(Some(CompletionDto {
    items: ranked.into_iter().map(|(_, _, item)| item).collect(),
    incomplete,
  }))
}

/// How well `candidate` matches what was typed: 0 for a prefix, 1 for a
/// prefix ignoring case, 2 for its letters in order; `None` for no match.
/// Mirrors `matchTier` in the frontend, which refines results as typing
/// continues.
fn match_tier(candidate: &str, typed: &str) -> Option<u8> {
  if candidate.starts_with(typed) {
    return Some(0);
  }
  let candidate = candidate.to_lowercase();
  let typed = typed.to_lowercase();
  if candidate.starts_with(&typed) {
    return Some(1);
  }
  let mut letters = candidate.chars();
  typed
    .chars()
    .all(|wanted| letters.any(|ch| ch == wanted))
    .then_some(2)
}

fn kind_name(kind: CompletionItemKind) -> &'static str {
  match kind {
    CompletionItemKind::METHOD => "method",
    CompletionItemKind::FUNCTION => "function",
    CompletionItemKind::CONSTRUCTOR => "constructor",
    CompletionItemKind::FIELD => "field",
    CompletionItemKind::VARIABLE => "variable",
    CompletionItemKind::CLASS => "class",
    CompletionItemKind::INTERFACE => "interface",
    CompletionItemKind::MODULE => "module",
    CompletionItemKind::PROPERTY => "property",
    CompletionItemKind::ENUM => "enum",
    CompletionItemKind::KEYWORD => "keyword",
    CompletionItemKind::SNIPPET => "snippet",
    CompletionItemKind::FILE => "file",
    CompletionItemKind::FOLDER => "folder",
    CompletionItemKind::ENUM_MEMBER => "enumMember",
    CompletionItemKind::CONSTANT => "constant",
    CompletionItemKind::STRUCT => "struct",
    CompletionItemKind::OPERATOR => "operator",
    CompletionItemKind::TYPE_PARAMETER => "typeParameter",
    _ => "text",
  }
}

/// A snippet as plain text: placeholders become their default text; tab
/// stops and variables are dropped.
fn strip_snippet(snippet: &str) -> String {
  fn strip(chars: &mut Peekable<Chars<'_>>, nested: bool) -> String {
    let mut out = String::new();
    while let Some(ch) = chars.next() {
      match ch {
        '\\' => out.extend(chars.next()),
        '}' if nested => return out,
        '$' => match chars.peek().copied() {
          Some('{') => {
            chars.next();
            while chars
              .next_if(|&c| c.is_alphanumeric() || c == '_')
              .is_some()
            {}
            match chars.next() {
              Some(':') => out.push_str(&strip(chars, true)),
              // A choice: use the first option.
              Some('|') => {
                let mut first = String::new();
                let mut done = false;
                for c in chars.by_ref() {
                  match c {
                    '}' => break,
                    ',' | '|' => done = true,
                    _ if !done => first.push(c),
                    _ => {}
                  }
                }
                out.push_str(&first);
              }
              _ => {}
            }
          }
          Some(c) if c.is_alphanumeric() || c == '_' => {
            while chars
              .next_if(|&c| c.is_alphanumeric() || c == '_')
              .is_some()
            {}
          }
          _ => out.push('$'),
        },
        _ => out.push(ch),
      }
    }
    out
  }
  strip(&mut snippet.chars().peekable(), false)
}

#[cfg(test)]
mod tests {
  use flareite_lsp::lsp_types::{LanguageString, MarkupContent};

  use super::*;

  #[test]
  fn snippets_become_plain_text() {
    assert_eq!(strip_snippet("println!(\"$1\")$0"), "println!(\"\")");
    assert_eq!(
      strip_snippet("fn ${1:name}(${2:args}) {}"),
      "fn name(args) {}"
    );
    assert_eq!(strip_snippet("${1:outer ${2:inner}}"), "outer inner");
    assert_eq!(strip_snippet("${1|one,two|}"), "one");
    assert_eq!(strip_snippet("cost: \\$5 $TM_FILENAME"), "cost: $5 ");
  }

  #[test]
  fn completion_matching_prefers_prefixes() {
    assert_eq!(match_tier("push_str", "push"), Some(0));
    assert_eq!(match_tier("PushStr", "push"), Some(1));
    assert_eq!(match_tier("push_str", "pst"), Some(2));
    assert_eq!(match_tier("push_str", "x"), None);
    assert_eq!(match_tier("anything", ""), Some(0));
  }

  #[test]
  fn hover_contents_become_markdown() {
    let code = HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
      language: "rust".into(),
      value: "fn main()".into(),
    }));
    assert_eq!(
      hover_markdown(code).as_deref(),
      Some("```rust\nfn main()\n```")
    );
    let empty = HoverContents::Markup(MarkupContent {
      kind: MarkupKind::Markdown,
      value: "  ".into(),
    });
    assert_eq!(hover_markdown(empty), None);
  }

  #[test]
  fn hover_file_links_name_files_and_lines() {
    let file = |path: &str, anchor: Option<&str>| {
      Some(LinkTarget::File {
        path: path.into(),
        anchor: anchor.map(Into::into),
      })
    };
    #[cfg(windows)]
    {
      assert_eq!(
        file_link("file:///c%3A/code/a%20b.rs#L12"),
        file("c:\\code\\a b.rs", Some("L12"))
      );
      assert_eq!(
        file_link("FILE:///C:/code/a.rs#12,3"),
        file("C:\\code\\a.rs", Some("L12"))
      );
    }
    #[cfg(not(windows))]
    {
      assert_eq!(
        file_link("file:///code/a%20b.rs#L12"),
        file("/code/a b.rs", Some("L12"))
      );
      assert_eq!(
        file_link("FILE:///code/a.rs#12,3"),
        file("/code/a.rs", Some("L12"))
      );
    }
    assert_eq!(
      file_link("file:///code/a.md?x=1#usage"),
      file("/code/a.md", Some("usage"))
    );
    assert_eq!(file_link("https://example.com"), None);
    assert_eq!(file_link("std::vec::Vec"), None);
  }

  #[test]
  fn words_start_after_non_word_characters() {
    let buffer = TextBuffer::from_text("let x = foo.ba");
    assert_eq!(word_start(&buffer, 14), 12);
    assert_eq!(word_start(&buffer, 12), 12);
    assert_eq!(word_start(&buffer, 5), 4);
  }
}
