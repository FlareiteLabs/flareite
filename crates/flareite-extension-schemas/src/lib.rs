//! For Flareite extensions that bring a configuration language to schemas.
//!
//! Flareite checks documents against TOML Schemas and JSON Schemas, but
//! reads no configuration language itself. An extension for a language
//! implements [`Language`]: parsing a document into a tree of values
//! ([`document::Document`]), finding what's being typed at the cursor
//! ([`Place`]), and writing keys and values. This crate does the rest:
//!
//! - [`Documents`] keeps the language's open documents, parses them as they
//!   change, and hands each tree to Flareite (`schemas::set-document`),
//!   which checks it against the schemas that apply;
//! - and answers the extension's completion and hover providers
//!   (`[[completion_providers]]`, `[[hover_providers]]`) from what the
//!   schemas say ([`complete`], [`hover`]).
//!
//! An extension keeps a [`Documents`] as its state, hands it its events,
//! and answers its providers from it:
//!
//! ```ignore
//! struct IniExtension(Documents<Ini>);
//!
//! impl Extension for IniExtension {
//!   fn activate(_: Activation) -> Result<Self, String> {
//!     Ok(Self(Documents::new(Ini)))
//!   }
//!   fn handle_event(&mut self, event: Event) -> Result<(), String> {
//!     self.0.handle_event(event);
//!     Ok(())
//!   }
//!   fn provide_completions(&mut self, at: CompletionRequest) -> Result<Vec<CompletionItem>, String> {
//!     Ok(self.0.completions(at.document, at.position))
//!   }
//!   fn provide_hover(&mut self, at: HoverRequest) -> Result<Option<String>, String> {
//!     Ok(self.0.hover(at.document, at.position))
//!   }
//! }
//!
//! register!(IniExtension);
//! ```
//!
//! With the `testing` feature, [`testing`] answers from Flareite's own
//! schema engine, natively, so a language's tests check, complete, and
//! describe documents as Flareite will.

mod documents;
#[cfg(feature = "testing")]
pub mod testing;

use serde_json::Value as Json;

pub use documents::{Documents, HostSchemas, MAX_DOCUMENT_BYTES, to_tree};
pub use flareite_schema_document as document;

use crate::document::{Document, NodeKind, Segment, Span};

/// What's being typed at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
  /// A key of the table at `table`. `start` is where the key (or this
  /// part of a dotted key) starts; `end`, where it ends, for hover.
  Key {
    table: Vec<Segment>,
    start: usize,
    end: usize,
    header: Header,
  },
  /// The value at `path`, starting at `start` (where a value was begun,
  /// or the cursor).
  Value {
    path: Vec<Segment>,
    start: usize,
    end: usize,
  },
  /// A comment, or somewhere nothing can be typed.
  Nothing,
}

/// Whether a key is in a table header, as TOML's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
  /// A key in a table, object, or mapping.
  None,
  /// In `[...]`.
  Table,
  /// In `[[...]]`.
  ArrayOfTables,
}

/// How a value is written, for completing its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
  /// A string, number, boolean, date, or a value of several kinds.
  Value,
  Table,
  /// An array of tables (`[[name]]` in TOML).
  ArrayOfTables,
  Array,
}

/// A key a table may have, as the schemas say.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
  pub name: String,
  /// Markdown.
  pub description: Option<String>,
  /// The kind of value, as a schema names it (`string`, `integer | boolean`).
  pub kind: Option<String>,
  pub shape: Shape,
  pub required: bool,
  pub deprecated: bool,
}

/// A value the schemas allow or suggest.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
  pub value: Json,
  /// Markdown.
  pub description: Option<String>,
  /// The schema's default.
  pub default: bool,
}

/// What the schemas say about a value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
  pub title: Option<String>,
  pub description: Option<String>,
  pub kind: Option<String>,
  pub default: Option<Json>,
  /// The only values allowed, if the schemas list them.
  pub allowed: Vec<Json>,
  /// `Some` when it's deprecated, with why, if the schema says.
  pub deprecated: Option<String>,
  /// Other constraints, in words ("At least 1").
  pub constraints: Vec<String>,
  /// The names of the schemas that say it.
  pub schemas: Vec<String>,
}

/// What the schemas that apply to a document say at a path in one of its
/// parts. [`HostSchemas`] asks Flareite; `testing::Schemas` answers
/// natively.
pub trait Schemas {
  fn properties(&self, part: usize, path: &[Segment]) -> Vec<Property>;
  fn values(&self, part: usize, path: &[Segment]) -> Vec<Choice>;
  fn describe(&self, part: usize, path: &[Segment]) -> Option<Info>;
}

/// A configuration language.
pub trait Language {
  /// The language ids it's for, as `[[languages]]` declare them.
  fn languages(&self) -> &[&str];

  /// Parses a document. A document being typed rarely parses whole: the
  /// tree should hold what can be read, with its syntax errors among the
  /// problems. Each part's span should cover its text, from the end of the
  /// one before.
  fn parse(&self, text: &str) -> Document;

  /// What's being typed at `offset`. The text may not parse.
  fn place(&self, text: &str, offset: usize) -> Place;

  /// A key as written (`raw`, the text of a [`Place::Key`]), decoded.
  fn decode_key(&self, raw: &str) -> String;

  /// A key written as the language writes it: bare if it can be, else
  /// quoted.
  fn key(&self, name: &str) -> String;

  /// What completing `property` inserts at `start` in `text`, with its key
  /// written as `key`, in `header`.
  fn insert_property(
    &self,
    key: &str,
    property: &Property,
    header: Header,
    text: &str,
    start: usize,
  ) -> String;

  /// A value as completing it at `start` in `text` inserts it: in quotes
  /// already begun there, say.
  fn insert_value(&self, value: &Json, text: &str, start: usize) -> String {
    let _ = (text, start);
    self.render(value)
  }

  /// A value written on one line, for hover cards.
  fn render(&self, value: &Json) -> String;

  /// Whether, at a key being typed at `start` in the table at `table`,
  /// what's typed may be a value too (a YAML sequence's `- ` item).
  fn values_at_key(&self, text: &str, table: &[Segment], start: usize) -> bool {
    let _ = (text, table, start);
    false
  }
}

/// What a completion is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
  /// A key for a value.
  Property,
  /// A key for a table or array of tables.
  Table,
  /// A value the schemas allow or suggest.
  Value,
}

/// A completion.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
  pub label: String,
  pub kind: CompletionKind,
  /// The kind of value, and whether it's required.
  pub detail: Option<String>,
  pub documentation: Option<String>,
  /// Replaces the text from `start` to the cursor.
  pub insert: String,
  pub start: usize,
  /// Matched against what's typed after `start`.
  pub filter: String,
  /// Required keys and the default value first, then by label.
  pub sort: String,
  pub deprecated: bool,
}

/// `offset` within `text`, at a character boundary.
fn clamp(text: &str, offset: usize) -> usize {
  let mut offset = offset.min(text.len());
  while !text.is_char_boundary(offset) {
    offset -= 1;
  }
  offset
}

fn sort(preferred: bool, label: &str) -> String {
  format!("{}{label}", if preferred { 0 } else { 1 })
}

/// Adds `property` to `list`, unless one of its name is there, which gets
/// what it lacks.
fn add_property(list: &mut Vec<Property>, property: Property) {
  match list.iter_mut().find(|known| known.name == property.name) {
    Some(known) => {
      known.description = known.description.take().or(property.description);
      known.kind = known.kind.take().or(property.kind);
      known.required |= property.required;
      known.deprecated |= property.deprecated;
      if known.shape == Shape::Value {
        known.shape = property.shape;
      }
    }
    None => list.push(property),
  }
}

/// The keys and values the schemas offer at `offset` in `text`, parsed as
/// `document`.
pub fn complete(
  language: &impl Language,
  schemas: &impl Schemas,
  document: &Document,
  text: &str,
  offset: usize,
) -> Vec<Completion> {
  let offset = clamp(text, offset);
  let part = document.part_at(offset).unwrap_or(0);
  let empty = document::Node::table(0..0, Vec::new());
  let tree = document.parts.get(part).map_or(&empty, |part| &part.tree);
  let mut items = Vec::new();
  let values = |items: &mut Vec<Completion>, path: &[Segment], start: usize| {
    for choice in schemas.values(part, path) {
      let insert = language.insert_value(&choice.value, text, start);
      items.push(Completion {
        label: insert.clone(),
        kind: CompletionKind::Value,
        detail: choice.default.then(|| "default".to_owned()),
        documentation: choice.description,
        sort: sort(choice.default, &insert),
        filter: insert.clone(),
        insert,
        start,
        deprecated: false,
      });
    }
  };
  match language.place(text, offset) {
    Place::Nothing => {}
    Place::Value { path, start, .. } => values(&mut items, &path, start),
    Place::Key {
      table,
      start,
      header,
      ..
    } => {
      if language.values_at_key(text, &table, start) {
        values(&mut items, &table, start);
      }
      let mut properties = Vec::new();
      for property in schemas.properties(part, &table) {
        add_property(&mut properties, property);
      }
      // Keys the table already has, except the one being typed.
      let typed = &text[start..offset.max(start)];
      let (node, taken) = tree.find(&table);
      let existing: Vec<&str> = match &node.kind {
        NodeKind::Table(entries) if taken == table.len() => entries
          .iter()
          .filter(|(_, child)| child.key_span.as_ref().is_none_or(|key| key.start != start))
          .map(|(name, _)| name.as_str())
          .collect(),
        _ => Vec::new(),
      };
      for property in properties {
        let fits = match header {
          Header::None => true,
          Header::Table => property.shape == Shape::Table,
          Header::ArrayOfTables => matches!(property.shape, Shape::Table | Shape::ArrayOfTables),
        };
        if !fits || (existing.contains(&property.name.as_str()) && header == Header::None) {
          continue;
        }
        let key = language.key(&property.name);
        let insert = language.insert_property(&key, &property, header, text, start);
        let mut detail = property.kind.clone().unwrap_or_default();
        if property.required {
          detail = if detail.is_empty() {
            "required".to_owned()
          } else {
            format!("{detail}, required")
          };
        }
        items.push(Completion {
          label: property.name.clone(),
          kind: match property.shape {
            Shape::Table | Shape::ArrayOfTables => CompletionKind::Table,
            _ => CompletionKind::Property,
          },
          detail: (!detail.is_empty()).then_some(detail),
          documentation: property.description.clone(),
          insert,
          start,
          // What's typed may be quoted.
          filter: if typed.starts_with(['"', '\'']) {
            key
          } else {
            property.name.clone()
          },
          sort: sort(property.required, &property.name),
          deprecated: property.deprecated,
        });
      }
    }
  }
  items
}

/// What the schemas say about the key or value at `offset` in `text`,
/// parsed as `document`: Markdown, and the span it's about.
pub fn hover(
  language: &impl Language,
  schemas: &impl Schemas,
  document: &Document,
  text: &str,
  offset: usize,
) -> Option<(String, Span)> {
  let offset = clamp(text, offset);
  let part = document.part_at(offset).unwrap_or(0);
  let (path, span) = match language.place(text, offset) {
    Place::Key {
      mut table,
      start,
      end,
      ..
    } if end > start => {
      table.push(Segment::Key(language.decode_key(&text[start..end])));
      (table, start..end)
    }
    Place::Value { path, start, end } if !path.is_empty() => (path, start..end.max(start)),
    _ => return None,
  };
  let info = schemas.describe(part, &path)?;
  let mut markdown = info_markdown(&info, |value| language.render(value));
  if markdown.is_empty() {
    return None;
  }
  if !info.schemas.is_empty() {
    markdown.push_str(&format!("\n\n*From {}*", info.schemas.join(", ")));
  }
  Some((markdown, span))
}

/// What the schemas say about a value, as Markdown for a hover card, with
/// values written by `render`.
pub fn info_markdown(info: &Info, render: impl Fn(&Json) -> String) -> String {
  let mut parts = Vec::new();
  if let Some(title) = &info.title {
    parts.push(format!("**{}**", title.trim()));
  }
  if let Some(reason) = &info.deprecated {
    parts.push(if reason.is_empty() {
      "*Deprecated.*".to_owned()
    } else {
      format!("*Deprecated:* {reason}")
    });
  }
  if let Some(description) = &info.description {
    parts.push(description.trim().to_owned());
  }
  let mut facts = Vec::new();
  if let Some(kind) = &info.kind {
    facts.push(format!("Type: `{kind}`"));
  }
  if !info.allowed.is_empty() {
    let shown: Vec<String> = info
      .allowed
      .iter()
      .take(20)
      .map(|value| format!("`{}`", render(value)))
      .collect();
    let more = info.allowed.len().saturating_sub(20);
    facts.push(if more > 0 {
      format!("Allowed: {} and {more} more", shown.join(", "))
    } else {
      format!("Allowed: {}", shown.join(", "))
    });
  }
  if let Some(default) = &info.default {
    facts.push(format!("Default: `{}`", render(default)));
  }
  for constraint in &info.constraints {
    facts.push(constraint.clone());
  }
  if !facts.is_empty() {
    parts.push(facts.join("  \n"));
  }
  parts.join("\n\n")
}

/// Makes an empty span cover the character there, so a problem marked on
/// it can be seen.
pub fn widen(text: &str, span: Span) -> Span {
  if !span.is_empty() {
    return span;
  }
  let start = span.start.min(text.len());
  let end = text[start..].chars().next().map_or(start, |c| {
    if c == '\n' {
      start
    } else {
      start + c.len_utf8()
    }
  });
  start..end
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  #[test]
  fn info_renders_as_markdown() {
    let info = Info {
      description: Some("The width of a tab.".into()),
      kind: Some("integer".into()),
      default: Some(json!(4)),
      constraints: vec!["At least 1".into()],
      ..Info::default()
    };
    assert_eq!(
      info_markdown(&info, |value| value.to_string()),
      "The width of a tab.\n\nType: `integer`  \nDefault: `4`  \nAt least 1"
    );
  }

  #[test]
  fn empty_spans_widen_to_a_character() {
    assert_eq!(widen("ab\n", 1..1), 1..2);
    assert_eq!(widen("ab\n", 2..2), 2..2);
    assert_eq!(widen("é", 0..0), 0..2);
  }
}
