//! Flareite's own schema engine, natively, for a language's tests: checking,
//! completing, and describing documents as Flareite will, without running
//! it.
//!
//! ```ignore
//! let schema = testing::json_schema(json!({ "properties": { "port": { "type": "integer" } } }));
//! let problems = testing::check(&Ini, "port = x\n", &[&schema]);
//! ```

use serde_json::Value as Json;

pub use flareite_schema::{Diagnostic, Schema, Severity};

use crate::document::{Document, Node, Segment};
use crate::{Choice, Completion, Info, Language, Property, Shape, Span};

/// A JSON Schema, as Flareite loads one.
pub fn json_schema(schema: Json) -> Schema {
  Schema::json("file:///test.schema.json", schema, Default::default())
}

/// A TOML Schema, as Flareite loads one.
pub fn toml_schema(text: &str) -> Schema {
  Schema::tosd("test.tosd", text).unwrap_or_else(|error| panic!("{error}"))
}

/// What `schemas` say about `document`, as Flareite answers.
pub struct Schemas<'a> {
  pub schemas: &'a [&'a Schema],
  pub document: &'a Document,
}

impl Schemas<'_> {
  fn tree(&self, part: usize) -> Node {
    self
      .document
      .parts
      .get(part)
      .map_or_else(|| Node::table(0..0, Vec::new()), |part| part.tree.clone())
  }
}

impl crate::Schemas for Schemas<'_> {
  fn properties(&self, part: usize, path: &[Segment]) -> Vec<Property> {
    flareite_schema::properties(self.schemas, &self.tree(part), path)
      .into_iter()
      .map(|property| Property {
        name: property.name,
        description: property.description,
        kind: property.kind,
        shape: match property.shape {
          flareite_schema::nav::Shape::Value => Shape::Value,
          flareite_schema::nav::Shape::Table => Shape::Table,
          flareite_schema::nav::Shape::ArrayOfTables => Shape::ArrayOfTables,
          flareite_schema::nav::Shape::Array => Shape::Array,
        },
        required: property.required,
        deprecated: property.deprecated,
      })
      .collect()
  }

  fn values(&self, part: usize, path: &[Segment]) -> Vec<Choice> {
    flareite_schema::choices(self.schemas, &self.tree(part), path)
      .into_iter()
      .map(|choice| Choice {
        value: choice.value,
        description: choice.description,
        default: choice.default,
      })
      .collect()
  }

  fn describe(&self, part: usize, path: &[Segment]) -> Option<Info> {
    flareite_schema::info(self.schemas, &self.tree(part), path).map(|info| Info {
      title: info.title,
      description: info.description,
      kind: info.kind,
      default: info.default,
      allowed: info.allowed,
      deprecated: info.deprecated,
      constraints: info.constraints,
      schemas: vec!["test".to_owned()],
    })
  }
}

/// The problems Flareite finds in `text`: its parser's, then, if none is
/// an error, the schemas'. Sorted by where they are.
pub fn check(language: &impl Language, text: &str, schemas: &[&Schema]) -> Vec<Diagnostic> {
  let document = language.parse(text);
  let mut problems = flareite_schema::parse_problems(&document);
  problems.extend(flareite_schema::check(&document, text, schemas));
  problems.sort_by_key(|problem| (problem.range.start, problem.severity));
  problems
}

/// The completions at `offset` in `text`.
pub fn complete(
  language: &impl Language,
  text: &str,
  offset: usize,
  schemas: &[&Schema],
) -> Vec<Completion> {
  let document = language.parse(text);
  let answers = Schemas {
    schemas,
    document: &document,
  };
  crate::complete(language, &answers, &document, text, offset)
}

/// The hover card at `offset` in `text`.
pub fn hover(
  language: &impl Language,
  text: &str,
  offset: usize,
  schemas: &[&Schema],
) -> Option<(String, Span)> {
  let document = language.parse(text);
  let answers = Schemas {
    schemas,
    document: &document,
  };
  crate::hover(language, &answers, &document, text, offset)
}
