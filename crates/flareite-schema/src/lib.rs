//! Schemas for configuration documents: checking a document against them,
//! and saying what they allow and describe at a path in it, for completion
//! and hover.
//!
//! Two kinds of schema are understood: TOML Schema 1.0 (`.tosd`,
//! <https://tomlschema.org>) and JSON Schema (drafts 4 to 2020-12). Neither
//! is tied to a language: documents come as trees of typed values
//! ([`document::Document`]), which extensions parse their languages'
//! documents into. A JSON Schema checks a tree as the JSON it stands for
//! (dates and times as strings); a TOML Schema, as the TOML (which has no
//! null). Where schemas come from, and fetching what they reference, is up
//! to the caller.

pub mod builtin;
pub mod document;
pub mod json_schema;
pub mod nav;
pub mod tosd;
pub mod yaml;

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value as Json;

pub use crate::document::Severity;
use crate::document::{Document, Node, NodeKind, Segment, Span};
use crate::json_schema::JsonSchema;
use crate::nav::{Choice, Info, Instance, Navigate, Property};

/// A loaded schema.
pub enum Schema {
  Tosd(Box<tosd::Schema>),
  Json(Box<JsonSchema>),
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
}

/// The problems the parser found, as diagnostics.
pub fn parse_problems(document: &Document) -> Vec<Diagnostic> {
  document
    .problems
    .iter()
    .map(|problem| Diagnostic {
      range: problem.span.clone(),
      severity: problem.severity,
      message: problem.message.clone(),
      code: None,
    })
    .collect()
}

/// Checks every part of a document, written in `text`, against each
/// schema; nothing if the parser found an error (see [`parse_problems`]).
pub fn check(document: &Document, text: &str, schemas: &[&Schema]) -> Vec<Diagnostic> {
  let mut diagnostics: Vec<Diagnostic> = Vec::new();
  if document.has_errors() {
    return diagnostics;
  }
  for schema in schemas {
    for part in &document.parts {
      let tree = &part.tree;
      let at = |path: &[Segment], key: bool| mark(tree, path, key, text, &part.span);
      match schema {
        Schema::Tosd(schema) => {
          let table = match document::to_toml(tree) {
            Ok(toml::Value::Table(table)) => table,
            Ok(_) => {
              diagnostics.push(Diagnostic {
                range: at(&[], false),
                severity: Severity::Error,
                message: "Expected a table: a TOML Schema describes tables.".to_owned(),
                code: Some("type-mismatch".to_owned()),
              });
              continue;
            }
            Err(span) => {
              diagnostics.push(Diagnostic {
                range: if span.is_empty() {
                  at(&[], false)
                } else {
                  span
                },
                severity: Severity::Error,
                message: "TOML has no null, so a TOML Schema can't check it.".to_owned(),
                code: Some("type-mismatch".to_owned()),
              });
              continue;
            }
          };
          let result = schema.validate(&table);
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
              range: at(&path, key),
              severity,
              message: explain(schema, &diagnostic.code, &path, &diagnostic.message),
              code: Some(diagnostic.code.clone()),
            });
          }
        }
        Schema::Json(schema) => {
          let Some(value) = tree.to_json() else {
            continue;
          };
          for problem in schema.validate(&value) {
            diagnostics.push(Diagnostic {
              range: at(&problem.path, problem.key),
              severity: Severity::Error,
              message: problem.message,
              code: None,
            });
          }
          deprecated_keys(schema.as_ref(), &Instance { tree }, &mut diagnostics);
        }
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
fn deprecated_keys(schema: &JsonSchema, document: &Instance<'_>, out: &mut Vec<Diagnostic>) {
  let mut checked = 0;
  document.tree.visit_keys(&mut |path, node| {
    checked += 1;
    if checked > MAX_DEPRECATION_CHECKS {
      return;
    }
    if let Some(reason) = schema
      .info_in(path, document)
      .and_then(|info| info.deprecated)
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
      let allowed: Vec<String> = info().allowed.iter().map(nav::render_toml).collect();
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

/// The line from `start` (past any blanks) to its end, without trailing
/// blanks.
fn first_line(text: &str, start: usize) -> Span {
  let Some(rest) = text.get(start.min(text.len())..) else {
    return start..start;
  };
  let start = start + rest.len() - rest.trim_start().len();
  let line = text[start..].lines().next().unwrap_or_default().trim_end();
  start..start + line.len()
}

/// Where to mark a problem with the value at `path` in the tree of a part
/// of the document written at `part`. A `key` problem (an unexpected key,
/// or a missing one, at its table's path) marks the key.
fn mark(tree: &Node, path: &[Segment], key: bool, text: &str, part: &Span) -> Span {
  let (node, taken) = tree.find(path);
  if let Some(span) = &node.key_span
    && ((taken == path.len() && key)
      || matches!(node.kind, NodeKind::Table(_) | NodeKind::Array(_))
      || node.span.is_empty())
  {
    // A table's whole span may be many lines.
    return span.clone();
  }
  match node.kind {
    // A scalar, as written.
    NodeKind::Scalar(_) if !node.span.is_empty() => node.span.clone(),
    // A table or array without a key (an item, or the document): its
    // first line.
    _ if !node.span.is_empty() => first_line(text, node.span.start),
    // Else the part's first line.
    _ => first_line(text, part.start),
  }
}

/// Why a TOML Schema, written in `text` and parsed as `document`, can't be
/// loaded (one problem at a time, as the loader stops at the first),
/// marked at the definition at fault; nothing if the parser found an
/// error.
pub fn check_tosd(document: &Document, text: &str) -> Vec<Diagnostic> {
  if document.has_errors() {
    return Vec::new();
  }
  let Err(error) = Schema::tosd("schema.tosd", text) else {
    return Vec::new();
  };
  let whole = 0..text.len();
  let range = match (document.parts.first(), &error.schema_path) {
    (Some(part), Some(path)) => {
      let path = document::parse_instance_path(path);
      let (node, _) = part.tree.find(&path);
      match &node.key_span {
        Some(key) => key.clone(),
        None => mark(&part.tree, &[], false, text, &part.span),
      }
    }
    (Some(part), None) => mark(&part.tree, &[], false, text, &part.span),
    (None, _) => first_line(text, whole.start),
  };
  vec![Diagnostic {
    range,
    severity: Severity::Error,
    message: tosd_message(&error.message),
    code: None,
  }]
}

/// The keys the table at `path` in `tree` may have, by every schema.
pub fn properties(schemas: &[&Schema], tree: &Node, path: &[Segment]) -> Vec<Property> {
  let document = Instance { tree };
  let mut properties = Vec::new();
  for schema in schemas {
    for property in schema.nav().properties_in(path, &document) {
      nav::add_property(&mut properties, property);
    }
  }
  properties
}

/// The values the schemas allow or suggest at `path` in `tree`.
pub fn choices(schemas: &[&Schema], tree: &Node, path: &[Segment]) -> Vec<Choice> {
  let document = Instance { tree };
  let mut choices = Vec::new();
  for schema in schemas {
    for choice in schema.nav().choices_in(path, &document) {
      nav::add_choice(&mut choices, choice);
    }
  }
  choices
}

/// What the schemas say about the value at `path` in `tree`.
pub fn info(schemas: &[&Schema], tree: &Node, path: &[Segment]) -> Option<Info> {
  let document = Instance { tree };
  let mut info: Option<Info> = None;
  for schema in schemas {
    if let Some(more) = schema.nav().info_in(path, &document) {
      match &mut info {
        Some(info) => info.merge(more),
        None => info = Some(more),
      }
    }
  }
  info.filter(|info| !info.is_empty())
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::document::{Part, Problem, Scalar};

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

  /// A document of TOML's values, written nowhere.
  fn toml(text: &str) -> Document {
    let value: toml::Value = toml::from_str(text).expect("toml");
    Document::single(document::toml_tree(&value), "")
  }

  fn json(value: Json) -> Document {
    Document::single(Node::from_json(&value), "")
  }

  fn keys(path: &[&str]) -> Vec<Segment> {
    path.iter().map(|key| Segment::key(*key)).collect()
  }

  #[test]
  fn documents_are_checked_against_toml_schemas() {
    let schema = tosd();
    let diagnostics = check(
      &toml("edition = \"2018\"\nextra = 1\n[server]\nport = 0\n"),
      "",
      &[&schema],
    );
    let codes: Vec<Option<&str>> = diagnostics.iter().map(|d| d.code.as_deref()).collect();
    for code in ["allowedvalues", "unknown-key", "min", "missing-required"] {
      assert!(codes.contains(&Some(code)), "{code}: {diagnostics:?}");
    }
    let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
    for expected in [
      "Expected one of \"2021\", \"2024\".",
      "Unknown key extra.",
      "Must be at least 1.",
      "Missing name, which is required.",
    ] {
      assert!(messages.contains(&expected), "{expected}: {messages:?}");
    }
    let typo = check(&toml("name = \"x\"\neditin = \"2021\"\n"), "", &[&schema]);
    assert_eq!(typo[0].message, "Unknown key editin. Did you mean edition?");
    // A JSON or YAML document is checked the same, but TOML has no null.
    let null = check(&json(json!({ "name": null })), "", &[&schema]);
    assert_eq!(null.len(), 1, "{null:?}");
    assert!(null[0].message.contains("no null"));
    assert!(check(&json(json!({ "name": "x" })), "", &[&schema]).is_empty());
  }

  #[test]
  fn documents_are_checked_against_json_schemas() {
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
    // Marked where they're written.
    let text = "port = \"x\"\nold = true\nzzz = 1\n";
    let tree = Node::table(
      0..text.len(),
      vec![
        (
          "port".into(),
          Node::scalar(7..10, Some(Scalar::String("x".into()))).keyed(0..4),
        ),
        (
          "old".into(),
          Node::scalar(17..21, Some(Scalar::Bool(true))).keyed(11..14),
        ),
        (
          "zzz".into(),
          Node::scalar(28..29, Some(Scalar::Integer(1))).keyed(22..25),
        ),
      ],
    );
    let diagnostics = check(&Document::single(tree, text), text, &[&schema]);
    let marked: Vec<&str> = diagnostics.iter().map(|d| &text[d.range.clone()]).collect();
    assert_eq!(marked, ["\"x\"", "old", "zzz"], "{diagnostics:?}");
    assert_eq!(diagnostics[1].severity, Severity::Warning);
  }

  #[test]
  fn parser_errors_stop_checking() {
    let schema = tosd();
    let mut document = toml("zzz = 1\n");
    document.problems.push(Problem {
      span: 0..1,
      severity: Severity::Warning,
      message: "Duplicate key.".into(),
    });
    assert!(
      !check(&document, "", &[&schema]).is_empty(),
      "warnings don't stop checking"
    );
    assert_eq!(parse_problems(&document).len(), 1);
    document.problems[0].severity = Severity::Error;
    assert!(check(&document, "", &[&schema]).is_empty());
    assert_eq!(parse_problems(&document)[0].code, None);
  }

  #[test]
  fn each_part_of_a_stream_is_checked() {
    let schema = Schema::json(
      "file:///s.json",
      json!({ "type": "object", "required": ["jobs"] }),
      HashMap::new(),
    );
    let text = "jobs: {}\n---\nname: 1\n";
    let part = |span: Span, tree: Node| Part { tree, span };
    let document = Document {
      parts: vec![
        part(
          0..9,
          Node::table(
            0..8,
            vec![("jobs".into(), Node::table(6..8, Vec::new()).keyed(0..4))],
          ),
        ),
        part(
          9..text.len(),
          Node::table(
            13..20,
            vec![(
              "name".into(),
              Node::scalar(19..20, Some(Scalar::Integer(1))).keyed(13..17),
            )],
          ),
        ),
      ],
      ..Document::default()
    };
    let diagnostics = check(&document, text, &[&schema]);
    let marked: Vec<&str> = diagnostics.iter().map(|d| &text[d.range.clone()]).collect();
    assert_eq!(marked, ["name: 1"], "{diagnostics:?}");
  }

  #[test]
  fn tosd_load_errors_are_reported() {
    let text =
      "[toml-schema]\nversion = \"1.0.0\"\n[elements.port]\ntype = \"integer\"\npatttern = \"x\"\n";
    let diagnostics = check_tosd(&toml(text), text);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(check_tosd(&toml(TOSD), TOSD).is_empty());
  }

  #[test]
  fn schemas_say_what_a_path_takes() {
    let schema = tosd();
    let tree = toml("name = \"x\"\n").parts.remove(0).tree;
    let names: Vec<String> = properties(&[&schema], &tree, &[])
      .into_iter()
      .map(|property| property.name)
      .collect();
    assert_eq!(names, ["edition", "name", "server"]);
    let port = &properties(&[&schema], &tree, &keys(&["server"]))[0];
    assert_eq!(port.kind.as_deref(), Some("integer"));
    assert!(port.required);
    let values: Vec<Json> = choices(&[&schema], &tree, &keys(&["edition"]))
      .into_iter()
      .map(|choice| choice.value)
      .collect();
    assert_eq!(values, [json!("2021"), json!("2024")]);
    let name = info(&[&schema], &tree, &keys(&["name"])).expect("info");
    assert_eq!(name.description.as_deref(), Some("The package's name."));
    let port = info(&[&schema], &tree, &keys(&["server", "port"])).expect("info");
    assert!(port.constraints.contains(&"At least 1".to_owned()));
    assert!(info(&[&schema], &tree, &keys(&["zzz"])).is_none());
  }

  /// Completion follows an `if` on a sibling's value to its branch, while
  /// the document is still being written.
  #[test]
  fn queries_follow_conditions_on_the_document() {
    let props = |names: &[&str]| {
      let properties: serde_json::Map<String, Json> = names
        .iter()
        .map(|name| ((*name).to_owned(), json!({ "type": "number" })))
        .collect();
      json!({ "properties": { "props": { "properties": properties } } })
    };
    let schema = Schema::json(
      "file:///s.json",
      json!({
        "type": "object",
        "properties": { "class": { "enum": ["Part", "Model"] } },
        "allOf": [
          { "if": { "properties": { "class": { "const": "Part" } }, "required": ["class"] },
            "then": props(&["Size", "Color"]) },
          { "if": { "properties": { "class": { "const": "Model" } }, "required": ["class"] },
            "then": props(&["Scale"]),
            "else": props(&["Name"]) }
        ]
      }),
      HashMap::new(),
    );
    let labels = |tree: &Node| -> Vec<String> {
      let mut labels: Vec<String> = properties(&[&schema], tree, &keys(&["props"]))
        .into_iter()
        .map(|property| property.name)
        .collect();
      labels.sort();
      labels
    };
    let with_class = |class: Option<Scalar>| {
      Node::table(
        0..0,
        vec![
          ("class".into(), Node::scalar(0..0, class)),
          ("props".into(), Node::table(0..0, Vec::new())),
        ],
      )
    };
    assert_eq!(
      labels(&with_class(Some(Scalar::String("Part".into())))),
      ["Color", "Name", "Size"]
    );
    assert_eq!(
      labels(&with_class(Some(Scalar::String("Model".into())))),
      ["Scale"]
    );
    // Without a class, neither `if` holds (both require it): the `else`.
    let classless = Node::table(0..0, vec![("props".into(), Node::table(0..0, Vec::new()))]);
    assert_eq!(labels(&classless), ["Name"]);
    // A class that isn't a value yet can't be told: both branches.
    assert_eq!(
      labels(&with_class(None)).len(),
      4,
      "Size, Color, Scale, Name"
    );
  }
}
