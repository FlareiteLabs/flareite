//! Schemas for TOML and JSON documents: checking a document against them,
//! and completing and describing its keys and values as it's typed.
//!
//! Two kinds of schema are understood: TOML Schema 1.0 (`.tosd`,
//! <https://tomlschema.org>), for TOML documents, and JSON Schema (drafts 4
//! to 2020-12), for JSON documents and TOML documents alike (a TOML
//! document is checked as the JSON it converts to, with dates and times as
//! strings). Where schemas come from, and fetching what they reference, is
//! up to the caller.

pub mod builtin;
pub mod context;
pub mod document;
pub mod json_schema;
pub mod nav;
pub mod tosd;

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value as Json;

use crate::context::{Header, Place};
use crate::document::{Node, NodeKind, Segment, Span};
use crate::json_schema::JsonSchema;
use crate::nav::{Navigate, Shape};

/// The kind of document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
  Toml,
  Json,
}

/// A loaded schema.
pub enum Schema {
  Tosd(Box<tosd::Schema>),
  Json(Box<JsonSchema>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
  Error,
  Warning,
  Information,
  Hint,
}

/// A problem in a document.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
  pub range: Span,
  pub severity: Severity,
  pub message: String,
  /// A TOML Schema diagnostic code, such as `unknown-key`.
  pub code: Option<String>,
}

/// Why a TOML Schema can't be loaded, and where in it.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadError {
  pub message: String,
  /// The TOML Schema path of the definition at fault (`$.elements.port`).
  pub schema_path: Option<String>,
}

impl std::fmt::Display for LoadError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.message)
  }
}

impl Schema {
  /// Loads a TOML Schema from its text. `name` names it in messages.
  pub fn tosd(name: &str, text: &str) -> Result<Self, LoadError> {
    tosd::Schema::from_source(PathBuf::from(name), text)
      .map(|schema| Self::Tosd(Box::new(schema)))
      .map_err(|error| LoadError {
        message: error.message.clone(),
        schema_path: error.schema_path.clone(),
      })
  }

  /// Loads a JSON Schema at `uri`, with `fetched`, the documents it
  /// references (see [`json_schema::external_references`]). A schema
  /// that can't validate still helps complete; see [`Schema::error`].
  pub fn json(uri: &str, schema: Json, fetched: HashMap<String, Json>) -> Self {
    Self::Json(Box::new(JsonSchema::new(uri, schema, fetched)))
  }

  /// Why the schema can't check documents, if it can't.
  pub fn error(&self) -> Option<&str> {
    match self {
      Self::Tosd(_) => None,
      Self::Json(schema) => schema.error(),
    }
  }

  fn nav(&self) -> &dyn Navigate {
    match self {
      Self::Tosd(schema) => schema.as_ref(),
      Self::Json(schema) => schema.as_ref(),
    }
  }

  /// Whether it can check and complete documents of `format`: TOML
  /// Schemas are for TOML only.
  pub fn applies_to(&self, format: Format) -> bool {
    matches!(
      (self, format),
      (Self::Json(_), _) | (Self::Tosd(_), Format::Toml)
    )
  }
}

/// A parsed document, whichever format.
struct Parsed {
  tree: Node,
  syntax: Vec<Diagnostic>,
  /// For TOML Schemas.
  toml: Option<toml::Table>,
  /// For JSON Schemas.
  json: Option<Json>,
}

fn parse(format: Format, text: &str) -> Parsed {
  match format {
    Format::Toml => {
      let document = document::parse_toml(text);
      Parsed {
        syntax: document
          .errors
          .into_iter()
          .map(|error| Diagnostic {
            range: error.span,
            severity: Severity::Error,
            message: error.message,
            code: None,
          })
          .collect(),
        json: document
          .table
          .as_ref()
          .map(|table| document::toml_to_json(&toml::Value::Table(table.clone()))),
        toml: document.table,
        tree: document.tree,
      }
    }
    Format::Json => {
      let document = document::parse_json(text);
      let mut syntax: Vec<Diagnostic> = document
        .errors
        .into_iter()
        .map(|error| Diagnostic {
          range: error.span,
          severity: Severity::Error,
          message: error.message,
          code: None,
        })
        .collect();
      syntax.extend(document.duplicates.into_iter().map(|range| Diagnostic {
        range,
        severity: Severity::Warning,
        message: "Duplicate key: the last one is used.".to_owned(),
        code: None,
      }));
      Parsed {
        tree: document.tree,
        syntax,
        toml: None,
        json: document.value,
      }
    }
  }
}

/// Syntax errors in a document.
pub fn check_syntax(format: Format, text: &str) -> Vec<Diagnostic> {
  parse(format, text).syntax
}

/// Checks a document: its syntax, then, if it parses, against each schema
/// that applies to its format.
pub fn check(format: Format, text: &str, schemas: &[&Schema]) -> Vec<Diagnostic> {
  let parsed = parse(format, text);
  let mut diagnostics = parsed.syntax.clone();
  if !diagnostics.is_empty() {
    return diagnostics;
  }
  for schema in schemas {
    if !schema.applies_to(format) {
      continue;
    }
    match schema {
      Schema::Tosd(schema) => {
        let Some(table) = &parsed.toml else { continue };
        let result = schema.validate(table);
        for (diagnostic, severity) in result
          .errors
          .iter()
          .map(|d| (d, Severity::Error))
          .chain(result.warnings.iter().map(|d| (d, Severity::Warning)))
        {
          let path = document::parse_instance_path(&diagnostic.path);
          let key = matches!(
            diagnostic.code.as_str(),
            "unknown-key" | "missing-required" | "keypattern"
          );
          diagnostics.push(Diagnostic {
            range: mark(&parsed.tree, &path, key, format, text),
            severity,
            message: explain(schema, &diagnostic.code, &path, &diagnostic.message),
            code: Some(diagnostic.code.clone()),
          });
        }
      }
      Schema::Json(schema) => {
        let Some(value) = &parsed.json else { continue };
        for problem in schema.validate(value) {
          diagnostics.push(Diagnostic {
            range: mark(&parsed.tree, &problem.path, problem.key, format, text),
            severity: Severity::Error,
            message: problem.message,
            code: None,
          });
        }
        deprecated_keys(schema.as_ref(), &parsed.tree, &mut diagnostics);
      }
    }
  }
  diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start, diagnostic.severity));
  diagnostics.dedup_by(|a, b| a.range == b.range && a.message == b.message);
  diagnostics
}

/// Most keys checked for deprecation in a document.
const MAX_DEPRECATION_CHECKS: usize = 5000;

/// Warns about keys a JSON Schema deprecates (TOML Schemas do it
/// themselves).
fn deprecated_keys(schema: &JsonSchema, tree: &Node, out: &mut Vec<Diagnostic>) {
  let mut checked = 0;
  tree.visit_keys(&mut |path, node| {
    checked += 1;
    if checked > MAX_DEPRECATION_CHECKS {
      return;
    }
    if let Some(reason) = schema.info(path).and_then(|info| info.deprecated)
      && let Some(key) = &node.key_span
    {
      out.push(Diagnostic {
        range: key.clone(),
        severity: Severity::Warning,
        message: if reason.is_empty() {
          "Deprecated.".to_owned()
        } else {
          format!("Deprecated: {reason}")
        },
        code: Some("deprecated".to_owned()),
      });
    }
  });
}

/// A TOML Schema problem in words: what the schema expects at `path`,
/// rather than the reference implementation's terse message.
fn explain(schema: &tosd::Schema, code: &str, path: &[Segment], message: &str) -> String {
  let key = match path.last() {
    Some(Segment::Key(key)) => Some(key.as_str()),
    _ => None,
  };
  let info = || schema.info(path).unwrap_or_default();
  let constraint = |prefixes: &[&str]| {
    info()
      .constraints
      .into_iter()
      .find(|constraint| prefixes.iter().any(|prefix| constraint.starts_with(prefix)))
  };
  let explained = match code {
    // A key a collection doesn't know, and that isn't of the form it takes.
    "unknown-key" | "keypattern" => key.map(|key| {
      let parent = &path[..path.len() - 1];
      let known: Vec<String> = schema
        .properties(parent)
        .into_iter()
        .map(|property| property.name)
        .collect();
      match closest(key, &known) {
        Some(near) => format!("Unknown key {key}. Did you mean {near}?"),
        None => format!("Unknown key {key}."),
      }
    }),
    "missing-required" => key.map(|key| format!("Missing {key}, which is required.")),
    "type-mismatch" => info().kind.map(|kind| format!("Expected {kind}.")),
    "allowedvalues" => {
      let allowed: Vec<String> = info()
        .allowed
        .iter()
        .map(|value| nav::render(value, true))
        .collect();
      (!allowed.is_empty()).then(|| format!("Expected one of {}.", allowed.join(", ")))
    }
    "min" | "max" => constraint(&["From", "At least", "At most"])
      .map(|range| format!("Must be {}{}.", range[..1].to_lowercase(), &range[1..])),
    "minlength" | "maxlength" => constraint(&["Length"]).map(|length| format!("{length}.")),
    _ => None,
  };
  explained.unwrap_or_else(|| tosd_message(message))
}

/// The name in `known` closest to `name`, if it's close: a typo.
fn closest<'a>(name: &str, known: &'a [String]) -> Option<&'a str> {
  fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
      let mut previous = row[0];
      row[0] = i + 1;
      for (j, &cb) in b.iter().enumerate() {
        let current = row[j + 1];
        row[j + 1] = (previous + usize::from(ca != cb))
          .min(row[j] + 1)
          .min(current + 1);
        previous = current;
      }
    }
    row[b.len()]
  }
  known
    .iter()
    .map(|candidate| (distance(name, candidate), candidate))
    .filter(|(distance, candidate)| *distance <= 2.max(candidate.len() / 4) && *distance > 0)
    .min_by_key(|(distance, _)| *distance)
    .map(|(_, candidate)| candidate.as_str())
}

/// TOML Schema messages read as sentences.
fn tosd_message(message: &str) -> String {
  let mut message = message.trim().to_owned();
  // Not when it starts with a path (`elements.port ...`).
  let first_word = message.split_whitespace().next().unwrap_or_default();
  if !first_word.contains('.')
    && let Some(first) = message.get(..1)
  {
    let upper = first.to_uppercase();
    message.replace_range(..1, &upper);
  }
  if !message.ends_with('.') {
    message.push('.');
  }
  message
}

/// Where to mark a problem with the value at `path`. A `key` problem (an
/// unexpected key, or a missing one, at its table's path) marks the key.
fn mark(tree: &Node, path: &[Segment], key: bool, format: Format, text: &str) -> Span {
  let (node, taken) = tree.find(path);
  if taken == path.len()
    && key
    && let Some(span) = &node.key_span
  {
    return span.clone();
  }
  if node.key_span.is_some() {
    return node.mark();
  }
  // The document itself: its first line, or a JSON document's bracket.
  match format {
    Format::Json if !node.span.is_empty() => node.span.start..node.span.start + 1,
    _ => {
      let start = text.len() - text.trim_start().len();
      let end = text[start..]
        .find('\n')
        .map_or(text.len(), |end| start + end);
      start..end.max(start)
    }
  }
}

/// Problems with a TOML Schema's own text: its syntax, or why it can't be
/// loaded (one at a time, as the loader stops at the first).
pub fn check_tosd(text: &str) -> Vec<Diagnostic> {
  let syntax = check_syntax(Format::Toml, text);
  if !syntax.is_empty() {
    return syntax;
  }
  let Err(error) = Schema::tosd("schema.tosd", text) else {
    return Vec::new();
  };
  let tree = document::parse_toml(text).tree;
  let range = match &error.schema_path {
    Some(path) => {
      let path = document::parse_instance_path(path);
      let (node, _) = tree.find(&path);
      match &node.key_span {
        Some(key) => key.clone(),
        None => mark(&tree, &[], false, Format::Toml, text),
      }
    }
    None => mark(&tree, &[], false, Format::Toml, text),
  };
  vec![Diagnostic {
    range,
    severity: Severity::Error,
    message: tosd_message(&error.message),
    code: None,
  }]
}

/// What a completion inserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
  /// A key for a value.
  Property,
  /// A key for a table or array of tables.
  Table,
  /// A value the schema allows or suggests.
  Value,
}

/// A suggestion.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
  pub label: String,
  pub kind: CompletionKind,
  /// The kind of value, and whether it's required.
  pub detail: Option<String>,
  pub documentation: Option<String>,
  /// Replaces the text from `from` to the cursor.
  pub insert: String,
  pub from: usize,
  /// Matched against what's typed after `from`.
  pub filter: String,
  pub deprecated: bool,
  /// Required keys and the default value first.
  pub preferred: bool,
}

/// Where the cursor is in a document.
pub fn place(format: Format, text: &str, offset: usize) -> Place {
  match format {
    Format::Toml => context::toml_place(text, offset),
    Format::Json => context::json_place(text, offset),
  }
}

/// `offset` within `text`, at a character boundary.
fn clamp(text: &str, offset: usize) -> usize {
  let mut offset = offset.min(text.len());
  while !text.is_char_boundary(offset) {
    offset -= 1;
  }
  offset
}

/// Characters that, typed, should ask for completions.
pub fn trigger_characters(format: Format) -> &'static [&'static str] {
  match format {
    Format::Toml => &["[", ".", "\"", "=", " "],
    Format::Json => &["\"", ":", " "],
  }
}

/// Keys and values to complete at `offset`.
pub fn complete(format: Format, text: &str, offset: usize, schemas: &[&Schema]) -> Vec<Completion> {
  let offset = clamp(text, offset);
  let schemas: Vec<&Schema> = schemas
    .iter()
    .copied()
    .filter(|schema| schema.applies_to(format))
    .collect();
  if schemas.is_empty() {
    return Vec::new();
  }
  let toml = format == Format::Toml;
  let mut items = Vec::new();
  match place(format, text, offset) {
    Place::Nothing => {}
    Place::Key {
      table,
      start,
      header,
      ..
    } => {
      let mut properties = Vec::new();
      for schema in &schemas {
        for property in schema.nav().properties(&table) {
          nav::add_property(&mut properties, property);
        }
      }
      // Keys the table already has, except the one being typed.
      let typed = &text[start..offset.max(start)];
      let tree = parse(format, text).tree;
      let (node, taken) = tree.find(&table);
      let existing: Vec<String> = match &node.kind {
        NodeKind::Table(entries) if taken == table.len() => entries
          .iter()
          .filter(|(_, child)| child.key_span.as_ref().is_none_or(|key| key.start != start))
          .map(|(name, _)| name.clone())
          .collect(),
        _ => Vec::new(),
      };
      for property in properties {
        let fits = match header {
          Header::None => true,
          Header::Table => property.shape == Shape::Table,
          Header::ArrayOfTables => matches!(property.shape, Shape::Table | Shape::ArrayOfTables),
        };
        if !fits || (existing.contains(&property.name) && header == Header::None) {
          continue;
        }
        let key = if toml {
          nav::toml_key(&property.name)
        } else {
          serde_json::to_string(&property.name).unwrap_or_default()
        };
        let insert = match (format, header, property.shape) {
          (Format::Json, _, _) => format!("{key}: "),
          (Format::Toml, Header::None, Shape::Value | Shape::Array) => format!("{key} = "),
          _ => key.clone(),
        };
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
          from: start,
          // What's typed may be quoted.
          filter: if typed.starts_with(['"', '\'']) {
            key
          } else {
            property.name.clone()
          },
          deprecated: property.deprecated,
          preferred: property.required,
        });
      }
    }
    Place::Value { path, start, .. } => {
      let mut choices = Vec::new();
      for schema in &schemas {
        for choice in schema.nav().choices(&path) {
          nav::add_choice(&mut choices, choice);
        }
      }
      for choice in choices {
        let text = nav::render(&choice.value, toml);
        items.push(Completion {
          label: text.clone(),
          kind: CompletionKind::Value,
          detail: choice.default.then(|| "default".to_owned()),
          documentation: choice.description,
          insert: text.clone(),
          from: start,
          filter: text,
          deprecated: false,
          preferred: choice.default,
        });
      }
    }
  }
  items
}

/// What the schemas say about the key or value at `offset`, as Markdown,
/// and the span it's about.
pub fn hover(
  format: Format,
  text: &str,
  offset: usize,
  schemas: &[&Schema],
) -> Option<(String, Span)> {
  let offset = clamp(text, offset);
  let (path, span) = match place(format, text, offset) {
    Place::Key {
      mut table,
      start,
      end,
      ..
    } if end > start => {
      let raw = &text[start..end];
      let key = match format {
        Format::Toml => context::decode_key(raw),
        Format::Json => serde_json::from_str::<String>(raw).ok()?,
      };
      table.push(Segment::Key(key));
      (table, start..end)
    }
    Place::Value { path, start, end } if !path.is_empty() => (path, start..end.max(start)),
    _ => return None,
  };
  let mut info: Option<nav::Info> = None;
  for schema in schemas.iter().filter(|schema| schema.applies_to(format)) {
    if let Some(more) = schema.nav().info(&path) {
      match &mut info {
        Some(info) => info.merge(more),
        None => info = Some(more),
      }
    }
  }
  let markdown = nav::info_markdown(&info?, format == Format::Toml);
  (!markdown.is_empty()).then_some((markdown, span))
}

/// The schema a document names for itself: a TOML comment `#:schema
/// <address>` before its first key (as Taplo reads it), a TOML Schema's
/// `[toml-schema] location`, or a JSON document's `"$schema"`.
pub fn named_schema(format: Format, text: &str) -> Option<String> {
  match format {
    Format::Toml => {
      for line in text.lines() {
        let line = line.trim();
        if let Some(address) = line.strip_prefix("#:schema") {
          let address = address.trim();
          return (!address.is_empty()).then(|| address.to_owned());
        }
        if !(line.is_empty() || line.starts_with('#')) {
          break;
        }
      }
      let document = document::parse_toml(text);
      let location = document
        .table?
        .get("toml-schema")?
        .get("location")?
        .as_str()?
        .to_owned();
      Some(location)
    }
    Format::Json => {
      let document = document::parse_json(text);
      match &document.tree.kind {
        NodeKind::Table(_) => {}
        _ => return None,
      }
      document.value?.get("$schema")?.as_str().map(str::to_owned)
    }
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  const TOSD: &str = r#"
[toml-schema]
version = "1.0.0"

[elements.name]
type = "string"
description = "The package's name."

[elements.edition]
type = "string"
allowedvalues = ["2021", "2024"]
optional = true

[elements.server]
type = "table"
optional = true
[elements.server.port]
type = "integer"
min = 1
"#;

  fn tosd() -> Schema {
    Schema::tosd("test.tosd", TOSD).expect("loads")
  }

  #[test]
  fn toml_documents_are_checked_against_toml_schemas() {
    let schema = tosd();
    let text = "edition = \"2018\"\nextra = 1\n[server]\nport = 0\n";
    let diagnostics = check(Format::Toml, text, &[&schema]);
    let found: Vec<(&str, Option<&str>)> = diagnostics
      .iter()
      .map(|d| (&text[d.range.clone()], d.code.as_deref()))
      .collect();
    assert!(
      found.contains(&("\"2018\"", Some("allowedvalues"))),
      "{diagnostics:?}"
    );
    assert!(
      found.contains(&("extra", Some("unknown-key"))),
      "{diagnostics:?}"
    );
    assert!(found.contains(&("0", Some("min"))), "{diagnostics:?}");
    assert!(
      diagnostics
        .iter()
        .any(|d| d.code.as_deref() == Some("missing-required")),
      "{diagnostics:?}"
    );
    let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
    for expected in [
      "Expected one of \"2021\", \"2024\".",
      "Unknown key extra.",
      "Must be at least 1.",
      "Missing name, which is required.",
    ] {
      assert!(messages.contains(&expected), "{expected}: {messages:?}");
    }
    let typo = check(
      Format::Toml,
      "name = \"x\"\neditin = \"2021\"\n",
      &[&schema],
    );
    assert_eq!(typo[0].message, "Unknown key editin. Did you mean edition?");
    // Syntax errors come first, and alone.
    let broken = check(Format::Toml, "name = \n", &[&schema]);
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].code, None);
  }

  #[test]
  fn toml_and_json_documents_are_checked_against_json_schemas() {
    let schema = Schema::json(
      "file:///s.json",
      json!({
        "type": "object",
        "properties": {
          "port": { "type": "integer" },
          "old": { "type": "boolean", "deprecated": true }
        },
        "additionalProperties": false
      }),
      HashMap::new(),
    );
    let text = "port = \"x\"\nold = true\nzzz = 1\n";
    let diagnostics = check(Format::Toml, text, &[&schema]);
    let marked: Vec<&str> = diagnostics.iter().map(|d| &text[d.range.clone()]).collect();
    assert_eq!(marked, ["\"x\"", "old", "zzz"], "{diagnostics:?}");
    assert_eq!(diagnostics[1].severity, Severity::Warning);

    let text = "{ \"port\": 1, \"zzz\": 2 }";
    let diagnostics = check(Format::Json, text, &[&schema]);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(&text[diagnostics[0].range.clone()], "\"zzz\"");
    // TOML Schemas don't apply to JSON.
    assert!(check(Format::Json, "{\"a\": 1}", &[&tosd()]).is_empty());
  }

  #[test]
  fn tosd_files_report_load_errors_where_they_are() {
    let text =
      "[toml-schema]\nversion = \"1.0.0\"\n[elements.port]\ntype = \"integer\"\npatttern = \"x\"\n";
    let diagnostics = check_tosd(text);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
      ["patttern", "port"].contains(&&text[diagnostics[0].range.clone()]),
      "{diagnostics:?}"
    );
    assert!(check_tosd(TOSD).is_empty());
  }

  fn complete_at(format: Format, text: &str, schema: &Schema) -> Vec<Completion> {
    let offset = text.find('|').expect("cursor");
    let text = text.replacen('|', "", 1);
    complete(format, &text, offset, &[schema])
  }

  #[test]
  fn completes_keys_and_values() {
    let schema = tosd();
    let items = complete_at(Format::Toml, "name = \"x\"\n|", &schema);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["edition", "server"], "present keys are left out");
    assert_eq!(items[0].insert, "edition = ");
    assert_eq!(items[1].insert, "server");
    let headers = complete_at(Format::Toml, "[|", &schema);
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].label, "server");
    let values = complete_at(Format::Toml, "edition = |", &schema);
    let values: Vec<&str> = values.iter().map(|i| i.insert.as_str()).collect();
    assert_eq!(values, ["\"2021\"", "\"2024\""]);
    let ports = complete_at(Format::Toml, "[server]\n|", &schema);
    assert_eq!(ports[0].detail.as_deref(), Some("integer, required"));

    let json_schema = Schema::json(
      "file:///s.json",
      json!({ "properties": { "a b": { "enum": [1, 2] } } }),
      HashMap::new(),
    );
    let keys = complete_at(Format::Json, "{ \"a|", &json_schema);
    assert_eq!(keys[0].insert, "\"a b\": ");
    assert_eq!(keys[0].from, 2);
    assert_eq!(keys[0].filter, "\"a b\"");
    let values = complete_at(Format::Json, "{ \"a b\": |", &json_schema);
    assert_eq!(values.len(), 2);
  }

  #[test]
  fn hovers_describe_keys_and_values() {
    let schema = tosd();
    let text = "name = \"x\"\n[server]\nport = 8080\n";
    let (markdown, span) = hover(Format::Toml, text, 2, &[&schema]).expect("hover");
    assert_eq!(&text[span], "name");
    assert!(markdown.starts_with("The package's name."), "{markdown}");
    let offset = text.find("8080").expect("port") + 1;
    let (markdown, _) = hover(Format::Toml, text, offset, &[&schema]).expect("hover");
    assert!(markdown.contains("At least 1"), "{markdown}");
    assert!(hover(Format::Toml, "zzz = 1", 1, &[&schema]).is_none());
  }

  #[test]
  fn documents_name_their_schemas() {
    assert_eq!(
      named_schema(Format::Toml, "# x\n#:schema ./a.tosd\nname = 1"),
      Some("./a.tosd".into())
    );
    assert_eq!(
      named_schema(Format::Toml, "name = 1\n#:schema ./a.tosd"),
      None
    );
    assert_eq!(
      named_schema(Format::Toml, "[toml-schema]\nlocation = \"c.tosd\"\n"),
      Some("c.tosd".into())
    );
    assert_eq!(
      named_schema(Format::Json, "{ \"$schema\": \"https://x/s.json\" }"),
      Some("https://x/s.json".into())
    );
  }
}
