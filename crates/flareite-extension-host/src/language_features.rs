//! Completion and hover from extensions' code (API 1.18), and text inserted
//! as characters are typed (1.19): the host functions of the
//! `language-features` interface, and running providers' commands.
//!
//! When the user asks for completions, or hovers, in a document whose
//! language extensions' `[[completion_providers]]` or `[[hover_providers]]`
//! name, the editor runs each provider's command with the request as its
//! payload ([`ExtensionHost::complete`], [`ExtensionHost::hover`]), and
//! waits a moment for the answers, which the command gives with
//! `set-completions` or `set-hover`. Likewise, typing one of an
//! `[[auto_insert_providers]]`'s trigger characters asks it for text to
//! insert at the cursor ([`ExtensionHost::auto_insert`], `set-insert`), as
//! HTML's closing tags are.

use std::time::Duration;

use flareite_core::DocumentId;

use crate::bindings::flareite::extension::language_features::{
  AutoInsertRequest, CompletionItem as WitItem, CompletionKind as WitKind, CompletionRequest, Host,
  HoverRequest,
};
use crate::bindings::flareite::extension::types::{
  ApiError, Permission as WitPermission, Position,
};
use crate::runtime::StoreState;
use crate::worker::Payload;
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

/// What a provider's command is run for.
#[derive(Debug, Clone, PartialEq)]
pub enum FeatureCall {
  Complete { id: u64, request: CompletionRequest },
  Hover { id: u64, request: HoverRequest },
  AutoInsert { id: u64, request: AutoInsertRequest },
}

/// Where a request is: a document at a version, and a place in it (a line,
/// and a column in UTF-8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureRequest {
  pub document: DocumentId,
  pub version: u64,
  pub line: u32,
  pub column: u32,
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

/// An answer, kept until the request reads it.
#[derive(Debug, Clone, PartialEq)]
pub enum FeatureAnswer {
  Completions(Vec<Completion>),
  Hover(String),
  Insert(AutoInsert),
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

impl Host for StoreState {
  fn completing(&mut self) -> Option<CompletionRequest> {
    match &self.api.payload {
      Payload::Feature(FeatureCall::Complete { request, .. }) => Some(request.clone()),
      _ => None,
    }
  }

  fn set_completions(&mut self, items: Vec<WitItem>) -> Result<(), ApiError> {
    let Payload::Feature(FeatureCall::Complete { id, request }) = &self.api.payload else {
      return Err(invalid(
        "set-completions answers a request: call it from a completion provider's command, while completing() returns it",
      ));
    };
    let (id, at) = (*id, request.position);
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    if items.len() > MAX_COMPLETIONS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_COMPLETIONS} completions answer a request"
      )));
    }
    let items = items
      .into_iter()
      .map(|item| completion(item, at))
      .collect::<Result<Vec<_>, _>>()?;
    shared.answer_feature(id, FeatureAnswer::Completions(items));
    Ok(())
  }

  fn hovering(&mut self) -> Option<HoverRequest> {
    match &self.api.payload {
      Payload::Feature(FeatureCall::Hover { request, .. }) => Some(*request),
      _ => None,
    }
  }

  fn set_hover(&mut self, markdown: String) -> Result<(), ApiError> {
    let Payload::Feature(FeatureCall::Hover { id, .. }) = &self.api.payload else {
      return Err(invalid(
        "set-hover answers a request: call it from a hover provider's command, while hovering() returns it",
      ));
    };
    let id = *id;
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    if markdown.len() > MAX_HOVER_BYTES {
      return Err(ApiError::TooLarge(format!(
        "a hover is at most {} KiB",
        MAX_HOVER_BYTES / 1024
      )));
    }
    shared.answer_feature(id, FeatureAnswer::Hover(markdown));
    Ok(())
  }

  fn inserting(&mut self) -> Option<AutoInsertRequest> {
    match &self.api.payload {
      Payload::Feature(FeatureCall::AutoInsert { request, .. }) => Some(request.clone()),
      _ => None,
    }
  }

  fn set_insert(&mut self, text: String, cursor: u32) -> Result<(), ApiError> {
    let Payload::Feature(FeatureCall::AutoInsert { id, .. }) = &self.api.payload else {
      return Err(invalid(
        "set-insert answers a request: call it from an auto-insert provider's command, while inserting() returns it",
      ));
    };
    let id = *id;
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    let answer = auto_insert(text, cursor)?;
    shared.answer_feature(id, FeatureAnswer::Insert(answer));
    Ok(())
  }
}

/// Checks an auto-insert answer.
fn auto_insert(text: String, cursor: u32) -> Result<AutoInsert, ApiError> {
  if text.len() > MAX_AUTO_INSERT_BYTES {
    return Err(ApiError::TooLarge(format!(
      "set-insert inserts at most {} KiB",
      MAX_AUTO_INSERT_BYTES / 1024
    )));
  }
  let cursor = cursor as usize;
  if !text.is_char_boundary(cursor) {
    return Err(invalid(format!(
      "set-insert's cursor ({cursor}) must be a character boundary of its {}-byte text",
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
    let mut commands = Vec::new();
    self.each_usable(|manifest| {
      commands.extend(
        manifest
          .contributions
          .auto_insert_providers_for(language)
          .filter(|provider| provider.trigger_characters.iter().any(|t| t == trigger))
          .map(|provider| provider.command.clone()),
      );
    });
    commands.into_iter().find_map(|command| {
      let id = self.shared.next_request();
      let request = AutoInsertRequest {
        document: at.document.get(),
        version: at.version,
        position: Position {
          line: at.line,
          column: at.column,
        },
        trigger: trigger.to_owned(),
        indent: indent.to_owned(),
      };
      let call = FeatureCall::AutoInsert { id, request };
      match self.ask_within(&command, id, call, AUTO_INSERT_TIME) {
        Some(FeatureAnswer::Insert(answer)) => Some(answer),
        _ => None,
      }
    })
  }

  /// Whether a loaded extension provides hovers for `language`.
  pub fn hovers(&self, language: &str) -> bool {
    let mut any = false;
    self.each_usable(|manifest| {
      any |= manifest
        .contributions
        .hover_providers_for(language)
        .next()
        .is_some();
    });
    any
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
    let mut commands = Vec::new();
    self.each_usable(|manifest| {
      commands.extend(
        manifest
          .contributions
          .completion_providers_for(language)
          .map(|provider| provider.command.clone()),
      );
    });
    let mut items = Vec::new();
    for command in commands {
      let id = self.shared.next_request();
      let request = CompletionRequest {
        document: at.document.get(),
        version: at.version,
        position: Position {
          line: at.line,
          column: at.column,
        },
        trigger: trigger.clone(),
      };
      if let Some(FeatureAnswer::Completions(answer)) =
        self.ask(&command, id, FeatureCall::Complete { id, request })
      {
        items.extend(answer);
      }
    }
    items
  }

  /// Asks every hover provider for documents of `language`: their Markdown.
  /// Blocks as [`Self::complete`] does.
  pub fn hover(&self, language: &str, at: FeatureRequest) -> Vec<String> {
    let mut commands = Vec::new();
    self.each_usable(|manifest| {
      commands.extend(
        manifest
          .contributions
          .hover_providers_for(language)
          .map(|provider| provider.command.clone()),
      );
    });
    let mut hovers = Vec::new();
    for command in commands {
      let id = self.shared.next_request();
      let request = HoverRequest {
        document: at.document.get(),
        version: at.version,
        position: Position {
          line: at.line,
          column: at.column,
        },
      };
      if let Some(FeatureAnswer::Hover(markdown)) =
        self.ask(&command, id, FeatureCall::Hover { id, request })
        && !markdown.trim().is_empty()
      {
        hovers.push(markdown);
      }
    }
    hovers
  }

  /// Runs a provider's command for `call`, and takes its answer.
  fn ask(&self, command: &str, id: u64, call: FeatureCall) -> Option<FeatureAnswer> {
    self.ask_within(command, id, call, ANSWER_TIME)
  }

  fn ask_within(
    &self,
    command: &str,
    id: u64,
    call: FeatureCall,
    time: Duration,
  ) -> Option<FeatureAnswer> {
    let result = self
      .run_with_payload(command, Payload::Feature(call))
      .and_then(|ticket| ticket.wait_within(time));
    let answer = self.shared.take_feature_answer(id);
    if let Err(message) = result {
      tracing::debug!(%command, %message, "a completion or hover provider failed");
      // Its answer is still shown if it gave one before failing.
    }
    answer
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
