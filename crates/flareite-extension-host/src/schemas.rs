//! Schemas extensions provide (API 1.16): those their manifests declare
//! (`[[schemas]]`), and those their code sets with the `schemas` interface,
//! which last until the extension stops; see
//! [`crate::ExtensionHost::schemas`]. And documents for them (1.18): the
//! trees extensions parse their languages' documents into, which the
//! editor checks against the schemas that apply ([`crate::Embedder::schema_document`]),
//! and what the schemas say at a path in one ([`crate::Embedder::schema_query`]).

use std::path::PathBuf;
use std::sync::Arc;

use flareite_core::DocumentId;
use flareite_schema::document::{Document, Node, Part, Problem, Scalar, Segment, Severity, Span};

use crate::bindings::flareite::extension::schemas::{
  DocumentTree, Host, NodeKind as WitKind, Property as WitProperty, Scalar as WitScalar,
  Schema as WitSchema, SchemaFormat as WitFormat, Segment as WitSegment, Shape as WitShape,
  Span as WitSpan, ValueChoice, ValueInfo,
};
use crate::bindings::flareite::extension::types::{
  ApiError, Permission as WitPermission, Severity as WitSeverity,
};
use crate::contributions::{MAX_SCHEMA_BYTES, MAX_SCHEMAS, check_schema_files};
use crate::runtime::StoreState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaFormat {
  /// TOML Schema (`.tosd`).
  TomlSchema,
  JsonSchema,
}

/// A schema an extension's code set.
#[derive(Debug, Clone)]
pub struct RuntimeSchema {
  pub id: String,
  pub files: Vec<String>,
  pub format: SchemaFormat,
  pub content: Arc<str>,
}

/// A schema an extension provides.
#[derive(Debug, Clone)]
pub struct ProvidedSchema {
  /// The extension's id.
  pub extension: String,
  /// The files it's for: name patterns, or ends of paths with '/'.
  pub files: Vec<String>,
  pub source: Provided,
}

#[derive(Debug, Clone)]
pub enum Provided {
  /// A file in the extension's folder (absolute).
  File(PathBuf),
  /// An address on the web.
  Url(String),
  /// What the extension's code set, with its id.
  Content {
    id: String,
    format: SchemaFormat,
    text: Arc<str>,
  },
}

/// Whether `id` can name a schema.
fn valid_id(id: &str) -> bool {
  (1..=64).contains(&id.len())
    && id
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Checks a schema an extension sets: its id, files, size, and that it
/// loads.
fn check(schema: WitSchema) -> Result<RuntimeSchema, ApiError> {
  if !valid_id(&schema.id) {
    return Err(ApiError::InvalidArgument(format!(
      "schema id {:?} must be 1 to 64 letters, digits, '-', '_', or '.'",
      schema.id
    )));
  }
  check_schema_files(&schema.files).map_err(ApiError::InvalidArgument)?;
  if schema.content.len() as u64 > MAX_SCHEMA_BYTES {
    return Err(ApiError::TooLarge(format!(
      "a schema is at most {} MiB",
      MAX_SCHEMA_BYTES / 1024 / 1024
    )));
  }
  let format = match schema.format {
    WitFormat::TomlSchema => {
      flareite_schema::Schema::tosd(&schema.id, &schema.content).map_err(|error| {
        ApiError::InvalidArgument(format!("the TOML Schema can't be loaded: {error}"))
      })?;
      SchemaFormat::TomlSchema
    }
    WitFormat::JsonSchema => {
      let value: serde_json::Value = serde_json::from_str(&schema.content).map_err(|error| {
        ApiError::InvalidArgument(format!("the JSON Schema isn't JSON: {error}"))
      })?;
      if !(value.is_object() || value.is_boolean()) {
        return Err(ApiError::InvalidArgument(
          "a JSON Schema is an object or a boolean".into(),
        ));
      }
      SchemaFormat::JsonSchema
    }
  };
  Ok(RuntimeSchema {
    id: schema.id,
    files: schema.files,
    format,
    content: schema.content.into(),
  })
}

/// Most nodes in a document's tree. (1.18)
pub const MAX_TREE_NODES: usize = 1_000_000;
/// Most problems a parser reports for a document. (1.18)
pub const MAX_PARSE_PROBLEMS: usize = 10_000;
/// Longest path a query takes. (1.18)
const MAX_PATH: usize = 1_000;

/// What an extension asks of the schemas applying to a document. (1.18)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaQuery {
  Properties,
  Values,
  Describe,
}

/// What the schemas say. (1.18)
#[derive(Debug, Clone, PartialEq)]
pub enum SchemaAnswer {
  Properties(Vec<flareite_schema::nav::Property>),
  Values(Vec<flareite_schema::nav::Choice>),
  /// What they say, and the names of the schemas that say it.
  Describe(Option<(flareite_schema::nav::Info, Vec<String>)>),
}

fn span(span: WitSpan) -> Result<Span, ApiError> {
  if span.start > span.end {
    return Err(ApiError::InvalidArgument(format!(
      "a span's start ({}) is after its end ({})",
      span.start, span.end
    )));
  }
  Ok(span.start as usize..span.end as usize)
}

fn scalar(value: WitScalar) -> Scalar {
  match value {
    WitScalar::Null => Scalar::Null,
    WitScalar::Boolean(b) => Scalar::Bool(b),
    WitScalar::Integer(i) => Scalar::Integer(i),
    WitScalar::Float(f) => Scalar::Float(f),
    WitScalar::String(s) => Scalar::String(s),
    WitScalar::Datetime(d) => Scalar::Datetime(d),
  }
}

/// A tree as an extension set it, checked and made into a document.
pub fn document(tree: DocumentTree) -> Result<Document, ApiError> {
  let invalid = |message: String| Err(ApiError::InvalidArgument(message));
  if tree.nodes.len() > MAX_TREE_NODES {
    return Err(ApiError::TooLarge(format!(
      "a document's tree has at most {MAX_TREE_NODES} nodes"
    )));
  }
  if tree.problems.len() > MAX_PARSE_PROBLEMS {
    return Err(ApiError::TooLarge(format!(
      "a document has at most {MAX_PARSE_PROBLEMS} problems"
    )));
  }
  // Each node's children, by index; and the roots.
  let mut children: Vec<Vec<usize>> = vec![Vec::new(); tree.nodes.len()];
  let mut roots = Vec::new();
  for (index, node) in tree.nodes.iter().enumerate() {
    match node.parent {
      None => roots.push(index),
      Some(parent) => {
        let parent = parent as usize;
        let container = tree
          .nodes
          .get(parent)
          .filter(|_| parent < index)
          .is_some_and(|parent| matches!(parent.kind, WitKind::Table | WitKind::Array));
        if !container {
          return invalid(format!(
            "node {index}'s parent ({parent}) must be a table or array before it"
          ));
        }
        if matches!(tree.nodes[parent].kind, WitKind::Table) && node.key.is_none() {
          return invalid(format!("node {index} is in a table, so it needs a key"));
        }
        children[parent].push(index);
      }
    }
  }
  if roots.len() != tree.parts.len() {
    return invalid(format!(
      "the tree has {} roots but {} parts: each root is a part's",
      roots.len(),
      tree.parts.len()
    ));
  }
  // Built from the last node back, so a node's children are built first.
  let mut built: Vec<Option<(Option<String>, Node)>> = vec![None; tree.nodes.len()];
  for (index, node) in tree.nodes.into_iter().enumerate().rev() {
    let value_span = span(node.span)?;
    let mut taken = children[index].iter().map(|&child| {
      built[child]
        .take()
        .unwrap_or_else(|| (None, Node::scalar(0..0, None)))
    });
    let mut built_node = match node.kind {
      WitKind::Table => Node::table(
        value_span,
        taken
          .by_ref()
          .map(|(key, node)| (key.unwrap_or_default(), node))
          .collect(),
      ),
      WitKind::Array => Node::array(value_span, taken.by_ref().map(|(_, node)| node).collect()),
      WitKind::Scalar(value) => Node::scalar(value_span, value.map(scalar)),
    };
    if let Some(key_span) = node.key_span {
      built_node.key_span = Some(span(key_span)?);
    }
    built[index] = Some((node.key, built_node));
  }
  let mut parts = Vec::with_capacity(roots.len());
  for (root, part_span) in roots.into_iter().zip(tree.parts) {
    let (_, tree) = built[root]
      .take()
      .unwrap_or_else(|| (None, Node::scalar(0..0, None)));
    parts.push(Part {
      tree,
      span: span(part_span)?,
    });
  }
  let problems = tree
    .problems
    .into_iter()
    .map(|problem| {
      Ok(Problem {
        span: span(problem.span)?,
        severity: match problem.severity {
          WitSeverity::Error => Severity::Error,
          WitSeverity::Warning => Severity::Warning,
          WitSeverity::Information => Severity::Information,
          WitSeverity::Hint => Severity::Hint,
        },
        message: problem.message,
      })
    })
    .collect::<Result<Vec<_>, ApiError>>()?;
  Ok(Document {
    parts,
    named_schema: tree.named_schema.filter(|named| !named.trim().is_empty()),
    problems,
  })
}

fn path(path: Vec<WitSegment>) -> Result<Vec<Segment>, ApiError> {
  if path.len() > MAX_PATH {
    return Err(ApiError::TooLarge(format!(
      "a path has at most {MAX_PATH} steps"
    )));
  }
  Ok(
    path
      .into_iter()
      .map(|segment| match segment {
        WitSegment::Key(key) => Segment::Key(key),
        WitSegment::Index(index) => Segment::Index(index as usize),
      })
      .collect(),
  )
}

fn json(value: &serde_json::Value) -> String {
  serde_json::to_string(value).unwrap_or_default()
}

impl StoreState {
  /// Asks the editor what the schemas say about a document.
  fn query(
    &mut self,
    id: u64,
    part: u32,
    at: Vec<WitSegment>,
    query: SchemaQuery,
  ) -> Result<SchemaAnswer, ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    let path = path(at)?;
    shared
      .embedder
      .schema_query(DocumentId::from_raw(id), part as usize, &path, query)
      .map_err(ApiError::InvalidArgument)
  }
}

impl Host for StoreState {
  fn set_schema(&mut self, schema: WitSchema) -> Result<(), ApiError> {
    let schema = check(schema)?;
    let declared = self.api.manifest().contributions.schemas.len();
    let shared = self.api.shared()?;
    if shared.set_schema(
      self.api.extension_id(),
      schema,
      MAX_SCHEMAS.saturating_sub(declared),
    ) {
      Ok(())
    } else {
      Err(ApiError::TooLarge(format!(
        "an extension provides at most {MAX_SCHEMAS} schemas"
      )))
    }
  }

  fn remove_schema(&mut self, id: String) -> bool {
    self
      .api
      .shared()
      .is_ok_and(|shared| shared.remove_schema(self.api.extension_id(), &id))
  }

  fn set_document(&mut self, id: u64, version: u64, tree: DocumentTree) -> Result<(), ApiError> {
    let shared = self.api.require(WitPermission::DocumentsRead)?;
    let document = document(tree)?;
    shared.embedder.schema_document(
      self.api.extension_id(),
      DocumentId::from_raw(id),
      version,
      Some(document),
    );
    Ok(())
  }

  fn clear_document(&mut self, id: u64) {
    if let Ok(shared) = self.api.shared() {
      shared
        .embedder
        .schema_document(self.api.extension_id(), DocumentId::from_raw(id), 0, None);
    }
  }

  fn properties(
    &mut self,
    id: u64,
    part: u32,
    path: Vec<WitSegment>,
  ) -> Result<Vec<WitProperty>, ApiError> {
    let SchemaAnswer::Properties(properties) =
      self.query(id, part, path, SchemaQuery::Properties)?
    else {
      return Ok(Vec::new());
    };
    Ok(
      properties
        .into_iter()
        .map(|property| WitProperty {
          name: property.name,
          description: property.description,
          kind: property.kind,
          shape: match property.shape {
            flareite_schema::nav::Shape::Value => WitShape::Value,
            flareite_schema::nav::Shape::Table => WitShape::Table,
            flareite_schema::nav::Shape::ArrayOfTables => WitShape::ArrayOfTables,
            flareite_schema::nav::Shape::Array => WitShape::Array,
          },
          required: property.required,
          deprecated: property.deprecated,
        })
        .collect(),
    )
  }

  fn values(
    &mut self,
    id: u64,
    part: u32,
    path: Vec<WitSegment>,
  ) -> Result<Vec<ValueChoice>, ApiError> {
    let SchemaAnswer::Values(choices) = self.query(id, part, path, SchemaQuery::Values)? else {
      return Ok(Vec::new());
    };
    Ok(
      choices
        .into_iter()
        .map(|choice| ValueChoice {
          value: json(&choice.value),
          description: choice.description,
          default: choice.default,
        })
        .collect(),
    )
  }

  fn describe(
    &mut self,
    id: u64,
    part: u32,
    path: Vec<WitSegment>,
  ) -> Result<Option<ValueInfo>, ApiError> {
    let SchemaAnswer::Describe(described) = self.query(id, part, path, SchemaQuery::Describe)?
    else {
      return Ok(None);
    };
    Ok(described.map(|(info, schemas)| ValueInfo {
      title: info.title,
      description: info.description,
      kind: info.kind,
      default: info.default.as_ref().map(json),
      allowed: info.allowed.iter().map(json).collect(),
      deprecated: info.deprecated,
      constraints: info.constraints,
      schemas,
    }))
  }
}

#[cfg(test)]
mod tests {
  use crate::bindings::flareite::extension::schemas::{Node as WitNode, ParseProblem};

  use super::*;

  fn node(parent: Option<u32>, key: Option<&str>, at: (u32, u32), kind: WitKind) -> WitNode {
    WitNode {
      parent,
      key: key.map(str::to_owned),
      key_span: None,
      span: WitSpan {
        start: at.0,
        end: at.1,
      },
      kind,
    }
  }

  #[test]
  fn trees_become_documents() {
    let whole = WitSpan { start: 0, end: 20 };
    let tree = DocumentTree {
      nodes: vec![
        node(None, None, (0, 20), WitKind::Table),
        node(Some(0), Some("a"), (4, 10), WitKind::Array),
        node(
          Some(1),
          None,
          (5, 6),
          WitKind::Scalar(Some(WitScalar::Integer(1))),
        ),
        node(Some(1), None, (8, 9), WitKind::Scalar(None)),
        node(
          Some(0),
          Some("b"),
          (15, 19),
          WitKind::Scalar(Some(WitScalar::Boolean(true))),
        ),
      ],
      parts: vec![whole],
      named_schema: Some("./s.json".into()),
      problems: vec![ParseProblem {
        span: WitSpan { start: 1, end: 2 },
        severity: WitSeverity::Warning,
        message: "Twice.".into(),
      }],
    };
    let document = document(tree).expect("document");
    assert_eq!(document.parts.len(), 1);
    let root = &document.parts[0].tree;
    let (a, _) = root.find(&[Segment::key("a"), Segment::Index(1)]);
    assert_eq!(a.span, 8..9);
    assert_eq!(a.value(), None);
    assert_eq!(
      root.get("b").and_then(Node::value),
      Some(&Scalar::Bool(true))
    );
    assert_eq!(document.named_schema.as_deref(), Some("./s.json"));
    assert_eq!(document.problems[0].severity, Severity::Warning);
  }

  #[test]
  fn malformed_trees_are_refused() {
    let whole = || WitSpan { start: 0, end: 9 };
    let refused = |nodes: Vec<WitNode>, parts: usize| {
      let tree = DocumentTree {
        nodes,
        parts: (0..parts).map(|_| whole()).collect(),
        named_schema: None,
        problems: Vec::new(),
      };
      matches!(document(tree), Err(ApiError::InvalidArgument(_)))
    };
    let scalar = || WitKind::Scalar(None);
    // A parent after its child, or a scalar as a parent.
    assert!(refused(
      vec![
        node(Some(1), Some("a"), (0, 1), scalar()),
        node(None, None, (0, 9), WitKind::Table)
      ],
      1
    ));
    assert!(refused(
      vec![
        node(None, None, (0, 9), scalar()),
        node(Some(0), None, (0, 1), scalar())
      ],
      1
    ));
    // A table's entry without a key; roots and parts that don't match; a
    // span backwards.
    assert!(refused(
      vec![
        node(None, None, (0, 9), WitKind::Table),
        node(Some(0), None, (0, 1), scalar())
      ],
      1
    ));
    assert!(refused(vec![node(None, None, (0, 9), WitKind::Table)], 2));
    assert!(refused(vec![node(None, None, (5, 1), scalar())], 1));
  }

  fn schema(id: &str, format: WitFormat, content: &str) -> WitSchema {
    WitSchema {
      id: id.into(),
      files: vec!["*.conf.toml".into()],
      format,
      content: content.into(),
    }
  }

  #[test]
  fn schemas_are_checked_when_set() {
    let tosd = "[toml-schema]\nversion = \"1.0.0\"\n[elements.a]\ntype = \"string\"\n";
    assert!(check(schema("conf", WitFormat::TomlSchema, tosd)).is_ok());
    assert!(
      check(schema(
        "conf",
        WitFormat::JsonSchema,
        "{\"type\": \"object\"}"
      ))
      .is_ok()
    );
    for (id, format, content) in [
      ("", WitFormat::JsonSchema, "{}"),
      ("a/b", WitFormat::JsonSchema, "{}"),
      ("conf", WitFormat::JsonSchema, "[1]"),
      ("conf", WitFormat::JsonSchema, "{"),
      (
        "conf",
        WitFormat::TomlSchema,
        "[elements.a]\ntype = \"string\"\n",
      ),
    ] {
      assert!(
        matches!(
          check(schema(id, format, content)),
          Err(ApiError::InvalidArgument(_))
        ),
        "{id} {content}"
      );
    }
    let mut unmatched = schema("conf", WitFormat::JsonSchema, "{}");
    unmatched.files.clear();
    assert!(check(unmatched).is_err());
  }
}
