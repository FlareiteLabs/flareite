//! Completion, hover, text inserted as characters are typed, formatting,
//! and code actions from extensions' code: calling the world's `provide-*`
//! functions of the extensions whose manifests declare providers for a
//! document's language, and checking their answers.
//!
//! The editor waits a moment for each answer ([`ExtensionHost::complete`],
//! [`ExtensionHost::hover`], [`ExtensionHost::auto_insert`],
//! [`ExtensionHost::format`], [`ExtensionHost::code_actions`]); one that
//! comes later is dropped, and the user can ask again.

use std::time::Duration;

use flareite_core::DocumentId;

use crate::bindings::flareite::extension::language_features::{
  AutoInsertRequest, CodeActionRequest, CompletionItem as WitItem, CompletionKind as WitKind,
  CompletionRequest, FormattingRequest, Host, HoverRequest,
};
use crate::bindings::flareite::extension::types::{
  ApiError, Position, Range as WitRange, TextEdit as WitEdit,
};
use crate::manifest::Manifest;
use crate::runtime::StoreState;
use crate::worker::{Answer, Request};
use crate::{ExtensionHost, Notification};

/// Most completions one request answers with.
pub const MAX_COMPLETIONS: usize = 1_000;
const MAX_LABEL_CHARS: usize = 200;
const MAX_INSERT_BYTES: usize = 4 * 1024;
/// Longest hover answer.
pub const MAX_HOVER_BYTES: usize = 64 * 1024;
/// Longest text an auto-insert provider inserts.
pub const MAX_AUTO_INSERT_BYTES: usize = 4 * 1024;
/// How long typing waits for an auto-insert provider: past this, typing has
/// probably moved on.
const AUTO_INSERT_TIME: Duration = Duration::from_millis(500);
/// How long the editor waits for each provider. Its answer is dropped
/// after this; the user can ask again.
const ANSWER_TIME: Duration = Duration::from_secs(2);
/// How long formatting waits for a provider.
const FORMAT_TIME: Duration = Duration::from_secs(5);
/// Most edits in one formatting or code action.
pub const MAX_EDITS: usize = 10_000;
/// Most code actions one provider offers at once.
const MAX_CODE_ACTIONS: usize = 100;

/// Where a request is: a document at a version, and a place in it (a line,
/// and a column in UTF-8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureRequest {
  pub document: DocumentId,
  pub version: u64,
  pub line: u32,
  pub column: u32,
}

impl FeatureRequest {
  fn position(&self) -> Position {
    Position {
      line: self.line,
      column: self.column,
    }
  }
}

/// A range of a document: lines, and columns in UTF-8 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProvidedRange {
  pub start: (u32, u32),
  pub end: (u32, u32),
}

impl ProvidedRange {
  fn to_wit(self) -> WitRange {
    WitRange {
      start: Position {
        line: self.start.0,
        column: self.start.1,
      },
      end: Position {
        line: self.end.0,
        column: self.end.1,
      },
    }
  }
}

/// An edit an extension made: a range, and its new text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidedEdit {
  pub range: ProvidedRange,
  pub text: String,
}

impl ProvidedEdit {
  fn from_wit(edit: WitEdit) -> Self {
    Self {
      range: ProvidedRange {
        start: (edit.range.start.line, edit.range.start.column),
        end: (edit.range.end.line, edit.range.end.column),
      },
      text: edit.text,
    }
  }
}

/// A document, or a range of it, to format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatRequest {
  pub document: DocumentId,
  pub version: u64,
  pub range: Option<ProvidedRange>,
  pub tab_size: u32,
  pub insert_spaces: bool,
}

/// A code action an extension offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvidedCodeAction {
  /// The extension's id.
  pub extension: String,
  pub title: String,
  pub kind: Option<String>,
  pub preferred: bool,
  /// Edits of the request's document, at its version.
  pub edits: Vec<ProvidedEdit>,
  /// One of the extension's commands, run after the edits.
  pub command: Option<String>,
  pub argument: Option<String>,
}

/// A completion an extension made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
  pub label: String,
  /// As language servers' kinds are named (`property`, `enumMember`).
  pub kind: &'static str,
  pub detail: Option<String>,
  /// Where the inserted text starts: on the request's line, at a column in
  /// UTF-8 bytes at or before the request's.
  pub column: u32,
  pub insert: String,
  pub filter: String,
  pub sort: String,
  pub deprecated: bool,
}

/// Text an auto-insert provider inserts at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoInsert {
  pub text: String,
  /// Where the cursor is left, in bytes into `text`: a character boundary.
  pub cursor: usize,
}

fn kind_name(kind: WitKind) -> &'static str {
  match kind {
    WitKind::Text => "text",
    WitKind::Method => "method",
    WitKind::Function => "function",
    WitKind::Constructor => "constructor",
    WitKind::Field => "field",
    WitKind::Variable => "variable",
    WitKind::Class => "class",
    WitKind::Interface => "interface",
    WitKind::Module => "module",
    WitKind::Property => "property",
    WitKind::Enum => "enum",
    WitKind::Keyword => "keyword",
    WitKind::Snippet => "snippet",
    WitKind::File => "file",
    WitKind::Folder => "folder",
    WitKind::EnumMember => "enumMember",
    WitKind::Constant => "constant",
    WitKind::Struct => "struct",
    WitKind::Operator => "operator",
    WitKind::TypeParameter => "typeParameter",
  }
}

fn invalid(message: impl Into<String>) -> ApiError {
  ApiError::InvalidArgument(message.into())
}

/// Checks a completion, for a request at `at`.
fn completion(item: WitItem, at: Position) -> Result<Completion, ApiError> {
  if item.start.line != at.line || item.start.column > at.column {
    return Err(invalid(format!(
      "completion {:?} must start on the request's line ({}), at or before its column ({})",
      item.label, at.line, at.column
    )));
  }
  if item.label.is_empty() || item.label.chars().count() > MAX_LABEL_CHARS {
    return Err(invalid(format!(
      "a completion's label is 1 to {MAX_LABEL_CHARS} characters"
    )));
  }
  if item
    .detail
    .as_ref()
    .is_some_and(|detail| detail.chars().count() > MAX_LABEL_CHARS)
  {
    return Err(invalid(format!(
      "a completion's detail is at most {MAX_LABEL_CHARS} characters"
    )));
  }
  if item.insert.len() > MAX_INSERT_BYTES {
    return Err(ApiError::TooLarge(format!(
      "a completion inserts at most {} KiB",
      MAX_INSERT_BYTES / 1024
    )));
  }
  Ok(Completion {
    kind: kind_name(item.kind),
    detail: item.detail,
    column: item.start.column,
    insert: item.insert,
    filter: item.filter.unwrap_or_else(|| item.label.clone()),
    sort: item.sort.unwrap_or_else(|| item.label.clone()),
    deprecated: item.deprecated,
    label: item.label,
  })
}

impl Host for StoreState {}

/// Checks an auto-insert answer.
fn auto_insert(text: String, cursor: u32) -> Result<AutoInsert, ApiError> {
  if text.len() > MAX_AUTO_INSERT_BYTES {
    return Err(ApiError::TooLarge(format!(
      "an auto-insert inserts at most {} KiB",
      MAX_AUTO_INSERT_BYTES / 1024
    )));
  }
  let cursor = cursor as usize;
  if !text.is_char_boundary(cursor) {
    return Err(invalid(format!(
      "an auto-insert's cursor ({cursor}) must be a character boundary of its {}-byte text",
      text.len()
    )));
  }
  Ok(AutoInsert { text, cursor })
}

impl ExtensionHost {
  /// The characters whose typing asks for completions in documents of
  /// `language`, from loaded extensions' providers: `None` if none provide
  /// completions for it.
  pub fn completion_triggers(&self, language: &str) -> Option<Vec<String>> {
    let mut triggers: Option<Vec<String>> = None;
    self.each_usable(|manifest| {
      for provider in manifest.contributions.completion_providers_for(language) {
        let known = triggers.get_or_insert_with(Vec::new);
        for trigger in &provider.trigger_characters {
          if !known.contains(trigger) {
            known.push(trigger.clone());
          }
        }
      }
    });
    triggers
  }

  /// The characters whose typing asks auto-insert providers in documents
  /// of `language`; `None` if none insert for it.
  pub fn auto_insert_triggers(&self, language: &str) -> Option<Vec<String>> {
    let mut triggers: Option<Vec<String>> = None;
    self.each_usable(|manifest| {
      for provider in manifest.contributions.auto_insert_providers_for(language) {
        let known = triggers.get_or_insert_with(Vec::new);
        for trigger in &provider.trigger_characters {
          if !known.contains(trigger) {
            known.push(trigger.clone());
          }
        }
      }
    });
    triggers
  }

  /// The extensions whose manifests pass `test`, in load order.
  fn providers(&self, test: impl Fn(&Manifest) -> bool) -> Vec<String> {
    let mut found = Vec::new();
    self.each_usable(|manifest| {
      if test(manifest) {
        found.push(manifest.id.clone());
      }
    });
    found
  }

  /// Calls `extension`'s export for `request`, waiting at most `time`:
  /// its answer, or `None` (logged) if it fails or takes too long.
  fn ask(&self, extension: &str, request: Request, time: Duration) -> Option<Answer> {
    match self
      .call(extension, request)
      .and_then(|ticket| ticket.wait_within(time))
    {
      Ok(answer) => Some(answer),
      Err(message) => {
        tracing::debug!(%extension, %message, "a language feature provider failed");
        None
      }
    }
  }

  /// Asks the auto-insert providers for documents of `language` whose
  /// triggers include `trigger`, in turn, for text to insert at the cursor:
  /// the first answer. `indent` is one level of indentation in the
  /// document. Blocks as [`Self::complete`] does, for less long.
  pub fn auto_insert(
    &self,
    language: &str,
    at: FeatureRequest,
    trigger: &str,
    indent: &str,
  ) -> Option<AutoInsert> {
    let extensions = self.providers(|manifest| {
      manifest
        .contributions
        .auto_insert_providers_for(language)
        .any(|provider| provider.trigger_characters.iter().any(|t| t == trigger))
    });
    extensions.into_iter().find_map(|extension| {
      let request = AutoInsertRequest {
        document: at.document.get(),
        version: at.version,
        position: at.position(),
        trigger: trigger.to_owned(),
        indent: indent.to_owned(),
      };
      match self.ask(&extension, Request::AutoInsert(request), AUTO_INSERT_TIME)? {
        Answer::AutoInsert(Some(insertion)) => auto_insert(insertion.text, insertion.cursor)
          .inspect_err(
            |error| tracing::debug!(%extension, ?error, "an auto-insert answer was refused"),
          )
          .ok(),
        _ => None,
      }
    })
  }

  /// Whether a loaded extension provides hovers for `language`.
  pub fn hovers(&self, language: &str) -> bool {
    !self
      .providers(|manifest| manifest.contributions.hovers(language))
      .is_empty()
  }

  /// Whether a loaded extension formats documents of `language`.
  pub fn formats(&self, language: &str) -> bool {
    !self
      .providers(|manifest| manifest.contributions.formats(language))
      .is_empty()
  }

  /// Whether a loaded extension offers code actions for `language`.
  pub fn offers_code_actions(&self, language: &str) -> bool {
    !self
      .providers(|manifest| manifest.contributions.offers_code_actions(language))
      .is_empty()
  }

  /// Asks every completion provider for documents of `language`, starting
  /// their extensions if needed. Blocks until each answers, or a moment
  /// passes, so call it off the UI thread.
  pub fn complete(
    &self,
    language: &str,
    at: FeatureRequest,
    trigger: Option<String>,
  ) -> Vec<Completion> {
    let extensions = self.providers(|manifest| {
      manifest
        .contributions
        .completion_providers_for(language)
        .next()
        .is_some()
    });
    let mut items = Vec::new();
    for extension in extensions {
      let request = CompletionRequest {
        document: at.document.get(),
        version: at.version,
        position: at.position(),
        trigger: trigger.clone(),
      };
      if let Some(Answer::Completions(answer)) =
        self.ask(&extension, Request::Completions(request), ANSWER_TIME)
      {
        for item in answer.into_iter().take(MAX_COMPLETIONS) {
          match completion(item, at.position()) {
            Ok(item) => items.push(item),
            Err(error) => tracing::debug!(%extension, ?error, "a completion was refused"),
          }
        }
      }
    }
    items
  }

  /// Asks every hover provider for documents of `language`: their Markdown.
  /// Blocks as [`Self::complete`] does.
  pub fn hover(&self, language: &str, at: FeatureRequest) -> Vec<String> {
    let extensions = self.providers(|manifest| manifest.contributions.hovers(language));
    let mut hovers = Vec::new();
    for extension in extensions {
      let request = HoverRequest {
        document: at.document.get(),
        version: at.version,
        position: at.position(),
      };
      if let Some(Answer::Hover(Some(markdown))) =
        self.ask(&extension, Request::Hover(request), ANSWER_TIME)
        && !markdown.trim().is_empty()
        && markdown.len() <= MAX_HOVER_BYTES
      {
        hovers.push(markdown);
      }
    }
    hovers
  }

  /// Asks the formatting providers for documents of `language`, in turn,
  /// for edits formatting the document (or a range of it): the first that
  /// answers with any. Blocks as [`Self::complete`] does.
  pub fn format(&self, language: &str, request: FormatRequest) -> Option<Vec<ProvidedEdit>> {
    let extensions = self.providers(|manifest| manifest.contributions.formats(language));
    extensions.into_iter().find_map(|extension| {
      let call = FormattingRequest {
        document: request.document.get(),
        version: request.version,
        range: request.range.map(ProvidedRange::to_wit),
        tab_size: request.tab_size,
        insert_spaces: request.insert_spaces,
      };
      match self.ask(&extension, Request::Formatting(call), FORMAT_TIME)? {
        Answer::Formatting(edits) if !edits.is_empty() && edits.len() <= MAX_EDITS => {
          Some(edits.into_iter().map(ProvidedEdit::from_wit).collect())
        }
        _ => None,
      }
    })
  }

  /// Asks every code action provider for documents of `language` for the
  /// actions at `range`. Blocks as [`Self::complete`] does.
  pub fn code_actions(
    &self,
    language: &str,
    document: DocumentId,
    version: u64,
    range: ProvidedRange,
    only: Option<String>,
  ) -> Vec<ProvidedCodeAction> {
    let extensions =
      self.providers(|manifest| manifest.contributions.offers_code_actions(language));
    let mut actions = Vec::new();
    for extension in extensions {
      let request = CodeActionRequest {
        document: document.get(),
        version,
        range: range.to_wit(),
        only: only.clone(),
      };
      let Some(Answer::CodeActions(answer)) =
        self.ask(&extension, Request::CodeActions(request), ANSWER_TIME)
      else {
        continue;
      };
      for action in answer.into_iter().take(MAX_CODE_ACTIONS) {
        let title_ok =
          !action.title.trim().is_empty() && action.title.chars().count() <= MAX_LABEL_CHARS;
        if !title_ok || action.edits.len() > MAX_EDITS {
          tracing::debug!(%extension, title = %action.title, "a code action was refused");
          continue;
        }
        actions.push(ProvidedCodeAction {
          extension: extension.clone(),
          title: action.title,
          kind: action.kind,
          preferred: action.preferred,
          edits: action
            .edits
            .into_iter()
            .map(ProvidedEdit::from_wit)
            .collect(),
          command: action.command,
          argument: action.argument,
        });
      }
    }
    actions
  }
}

/// Tells the editor that an extension's documents for schemas are gone,
/// when it stops.
pub(crate) fn forgotten(extension: &str) -> Notification {
  Notification::SchemaDocumentsForgotten {
    extension: extension.to_owned(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn item(line: u32, column: u32) -> WitItem {
    WitItem {
      label: "name".into(),
      kind: WitKind::Property,
      detail: Some("string".into()),
      start: Position { line, column },
      insert: "name = ".into(),
      filter: None,
      sort: Some("0name".into()),
      deprecated: false,
    }
  }

  #[test]
  fn completions_are_checked() {
    let at = Position { line: 2, column: 4 };
    let ok = completion(item(2, 1), at).expect("valid");
    assert_eq!(ok.kind, "property");
    assert_eq!(ok.filter, "name");
    assert_eq!(ok.sort, "0name");
    // Elsewhere than before the cursor on its line.
    assert!(completion(item(1, 0), at).is_err());
    assert!(completion(item(2, 5), at).is_err());
    let mut long = item(2, 0);
    long.label = "x".repeat(201);
    assert!(completion(long, at).is_err());
    let mut empty = item(2, 0);
    empty.label.clear();
    assert!(completion(empty, at).is_err());
  }

  #[test]
  fn auto_inserts_are_checked() {
    let ok = auto_insert("</div>".into(), 0).expect("valid");
    assert_eq!((ok.text.as_str(), ok.cursor), ("</div>", 0));
    assert!(
      auto_insert("\u{e9}>".into(), 1).is_err(),
      "inside a character"
    );
    assert!(auto_insert("a".into(), 2).is_err(), "past the end");
    assert!(matches!(
      auto_insert("x".repeat(MAX_AUTO_INSERT_BYTES + 1), 0),
      Err(ApiError::TooLarge(_))
    ));
  }
}
