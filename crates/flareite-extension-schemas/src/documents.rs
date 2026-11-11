//! Keeping a language's open documents, parsed for Flareite's schemas, and
//! answering completion and hover requests for them.

use std::collections::HashMap;

use flareite_extension_api::host;
use flareite_extension_api::language_features::{self, CompletionItem, CompletionKind as WitKind};
use flareite_extension_api::schemas::{
  self, DocumentTree, Node as WitNode, NodeKind as WitNodeKind, ParseProblem, Scalar as WitScalar,
  Segment as WitSegment, Shape as WitShape, Span as WitSpan,
};
use flareite_extension_api::types::{DocumentChange, DocumentInfo, Position, Range, Severity};
use serde_json::Value as Json;

use crate::document::{self, Document, Node, NodeKind, Scalar, Segment, Span};
use crate::{Choice, CompletionKind, Info, Language, Property, Schemas, Shape};

/// Larger documents aren't parsed: Flareite doesn't check them.
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

/// An open document.
struct Open {
  text: String,
  version: u64,
  /// The tree of `text`, once parsed.
  parsed: Option<Document>,
}

/// A language's open documents, parsed as they change for Flareite to
/// check. Hand it the extension's document events, and the commands of
/// its completion and hover providers.
pub struct Documents<L: Language> {
  language: L,
  open: HashMap<u64, Open>,
}

impl<L: Language> Documents<L> {
  pub fn new(language: L) -> Self {
    Self {
      language,
      open: HashMap::new(),
    }
  }

  pub fn language(&self) -> &L {
    &self.language
  }

  fn ours(&self, info: &DocumentInfo) -> bool {
    info
      .language
      .as_deref()
      .is_some_and(|language| self.language.languages().contains(&language))
      && info.length <= MAX_DOCUMENT_BYTES as u64
  }

  /// A document was opened (or its language changed): keeps it, if it's
  /// of the language, and has Flareite check it.
  pub fn opened(&mut self, info: DocumentInfo) {
    if !self.ours(&info) {
      if self.open.remove(&info.id).is_some() {
        schemas::clear_document(info.id);
      }
      return;
    }
    if let Some((text, version)) = read(&info) {
      self.open.insert(
        info.id,
        Open {
          text,
          version,
          parsed: None,
        },
      );
      self.parse(info.id);
    }
  }

  /// A document changed: follows the changes, and has Flareite check it.
  pub fn changed(&mut self, change: DocumentChange) {
    let Some(open) = self.open.get_mut(&change.document) else {
      return;
    };
    let applied = change.changes.is_some_and(|changes| {
      changes.into_iter().all(|change| {
        let start = offset_of(&open.text, change.range.start);
        let end = offset_of(&open.text, change.range.end);
        match (start, end) {
          (Some(start), Some(end)) if start <= end => {
            open.text.replace_range(start..end, &change.text);
            true
          }
          _ => false,
        }
      })
    });
    open.version = change.version;
    open.parsed = None;
    if !applied {
      // Too large to send, or out of step: read it again.
      match host::document(change.document) {
        Ok(info) => {
          self.open.remove(&change.document);
          self.opened(info);
        }
        Err(_) => {
          self.open.remove(&change.document);
        }
      }
      return;
    }
    if open.text.len() > MAX_DOCUMENT_BYTES {
      self.open.remove(&change.document);
      schemas::clear_document(change.document);
      return;
    }
    self.parse(change.document);
  }

  pub fn closed(&mut self, id: u64) {
    self.open.remove(&id);
  }

  /// Parses a document, unless it's parsed, and gives Flareite its tree.
  fn parse(&mut self, id: u64) {
    let Some(open) = self.open.get_mut(&id) else {
      return;
    };
    if open.parsed.is_none() {
      let document = self.language.parse(&open.text);
      if let Err(error) = schemas::set_document(id, open.version, &to_tree(&document)) {
        host::log(
          flareite_extension_api::types::LogLevel::Warn,
          &format!("Flareite refused a document's tree: {error:?}"),
        );
      }
      open.parsed = Some(document);
    }
  }

  /// Answers the completion or hover request the running command is for,
  /// if it's one. Returns whether it was.
  pub fn run_command(&mut self) -> Result<bool, String> {
    if let Some(request) = language_features::completing() {
      let items = self.completions(request.document, request.position);
      language_features::set_completions(&items).map_err(|error| format!("{error:?}"))?;
      return Ok(true);
    }
    if let Some(request) = language_features::hovering() {
      let markdown = self
        .hover(request.document, request.position)
        .unwrap_or_default();
      language_features::set_hover(&markdown).map_err(|error| format!("{error:?}"))?;
      return Ok(true);
    }
    Ok(false)
  }

  /// The completions at `position` in document `id`.
  pub fn completions(&mut self, id: u64, position: Position) -> Vec<CompletionItem> {
    self.parse(id);
    let Some(open) = self.open.get(&id) else {
      return Vec::new();
    };
    let (Some(document), Some(offset)) = (&open.parsed, offset_of(&open.text, position)) else {
      return Vec::new();
    };
    let line_start = offset - position.column as usize;
    let schemas = HostSchemas { document: id };
    crate::complete(&self.language, &schemas, document, &open.text, offset)
      .into_iter()
      // On the cursor's line, as Flareite takes them.
      .filter(|item| item.start >= line_start && item.start <= offset)
      .map(|item| CompletionItem {
        label: item.label,
        kind: match item.kind {
          CompletionKind::Property => WitKind::Property,
          CompletionKind::Table => WitKind::Module,
          CompletionKind::Value => WitKind::EnumMember,
        },
        detail: item.detail,
        start: Position {
          line: position.line,
          column: u32::try_from(item.start - line_start).unwrap_or(u32::MAX),
        },
        insert: item.insert,
        filter: Some(item.filter),
        sort: Some(item.sort),
        deprecated: item.deprecated,
      })
      .collect()
  }

  /// What the schemas say at `position` in document `id`, as Markdown.
  pub fn hover(&mut self, id: u64, position: Position) -> Option<String> {
    self.parse(id);
    let open = self.open.get(&id)?;
    let document = open.parsed.as_ref()?;
    let offset = offset_of(&open.text, position)?;
    let schemas = HostSchemas { document: id };
    crate::hover(&self.language, &schemas, document, &open.text, offset)
      .map(|(markdown, _)| markdown)
  }
}

/// A document's whole text, and its version, read from Flareite.
fn read(info: &DocumentInfo) -> Option<(String, u64)> {
  for _ in 0..3 {
    let last = info.line_count.saturating_sub(1);
    let slice = host::read_lines(info.id, last, last + 1).ok()?;
    let length = slice.lines.first().map_or(0, String::len);
    let text = host::read_range(
      info.id,
      Range {
        start: Position { line: 0, column: 0 },
        end: Position {
          line: last,
          column: u32::try_from(length).ok()?,
        },
      },
    )
    .ok()?;
    if text.version == slice.version {
      return Some((text.text, text.version));
    }
  }
  None
}

/// The byte offset of `position` in `text`, whose lines end with `\n`,
/// `\r\n`, or `\r`, as Flareite's do.
pub(crate) fn offset_of(text: &str, position: Position) -> Option<usize> {
  let bytes = text.as_bytes();
  let mut start = 0;
  for _ in 0..position.line {
    let rest = &bytes[start..];
    let at = rest.iter().position(|&b| b == b'\n' || b == b'\r')?;
    start += at
      + if rest[at..].starts_with(b"\r\n") {
        2
      } else {
        1
      };
  }
  let line_end = bytes[start..]
    .iter()
    .position(|&b| b == b'\n' || b == b'\r')
    .map_or(bytes.len(), |end| start + end);
  let offset = start + position.column as usize;
  (offset <= line_end && text.is_char_boundary(offset)).then_some(offset)
}

fn wit_span(span: &Span) -> WitSpan {
  let at = |offset: usize| u32::try_from(offset).unwrap_or(u32::MAX);
  WitSpan {
    start: at(span.start),
    end: at(span.end),
  }
}

/// A document as the `schemas` interface takes it: its nodes in a list,
/// each after the one it's in.
pub fn to_tree(document: &Document) -> DocumentTree {
  let mut nodes = Vec::new();
  // Pre-order, without recursion: documents nest deeply.
  let mut stack: Vec<(&Node, Option<u32>, Option<&str>)> = document
    .parts
    .iter()
    .rev()
    .map(|part| (&part.tree, None, None))
    .collect();
  while let Some((node, parent, key)) = stack.pop() {
    let index = u32::try_from(nodes.len()).unwrap_or(u32::MAX);
    let kind = match &node.kind {
      NodeKind::Table(entries) => {
        for (key, child) in entries.iter().rev() {
          stack.push((child, Some(index), Some(key)));
        }
        WitNodeKind::Table
      }
      NodeKind::Array(items) => {
        for item in items.iter().rev() {
          stack.push((item, Some(index), None));
        }
        WitNodeKind::Array
      }
      NodeKind::Scalar(value) => WitNodeKind::Scalar(value.as_ref().map(|value| match value {
        Scalar::Null => WitScalar::Null,
        Scalar::Bool(b) => WitScalar::Boolean(*b),
        Scalar::Integer(i) => WitScalar::Integer(*i),
        Scalar::Float(f) => WitScalar::Float(*f),
        Scalar::String(s) => WitScalar::String(s.clone()),
        Scalar::Datetime(d) => WitScalar::Datetime(d.clone()),
      })),
    };
    nodes.push(WitNode {
      parent,
      key: key.map(str::to_owned),
      key_span: node.key_span.as_ref().map(wit_span),
      span: wit_span(&node.span),
      kind,
    });
  }
  DocumentTree {
    nodes,
    parts: document
      .parts
      .iter()
      .map(|part| wit_span(&part.span))
      .collect(),
    named_schema: document.named_schema.clone(),
    problems: document
      .problems
      .iter()
      .map(|problem| ParseProblem {
        span: wit_span(&problem.span),
        severity: match problem.severity {
          document::Severity::Error => Severity::Error,
          document::Severity::Warning => Severity::Warning,
          document::Severity::Information => Severity::Information,
          document::Severity::Hint => Severity::Hint,
        },
        message: problem.message.clone(),
      })
      .collect(),
  }
}

/// What the schemas say, from Flareite, about a document whose tree it has.
pub struct HostSchemas {
  pub document: u64,
}

fn wit_path(path: &[Segment]) -> Vec<WitSegment> {
  path
    .iter()
    .map(|segment| match segment {
      Segment::Key(key) => WitSegment::Key(key.clone()),
      Segment::Index(index) => WitSegment::Index(u32::try_from(*index).unwrap_or(u32::MAX)),
    })
    .collect()
}

fn part(part: usize) -> u32 {
  u32::try_from(part).unwrap_or(u32::MAX)
}

fn json(text: &str) -> Json {
  serde_json::from_str(text).unwrap_or(Json::Null)
}

impl Schemas for HostSchemas {
  fn properties(&self, at: usize, path: &[Segment]) -> Vec<Property> {
    schemas::properties(self.document, part(at), &wit_path(path))
      .unwrap_or_default()
      .into_iter()
      .map(|property| Property {
        name: property.name,
        description: property.description,
        kind: property.kind,
        shape: match property.shape {
          WitShape::Value => Shape::Value,
          WitShape::Table => Shape::Table,
          WitShape::ArrayOfTables => Shape::ArrayOfTables,
          WitShape::Array => Shape::Array,
        },
        required: property.required,
        deprecated: property.deprecated,
      })
      .collect()
  }

  fn values(&self, at: usize, path: &[Segment]) -> Vec<Choice> {
    schemas::values(self.document, part(at), &wit_path(path))
      .unwrap_or_default()
      .into_iter()
      .map(|choice| Choice {
        value: json(&choice.value),
        description: choice.description,
        default: choice.default,
      })
      .collect()
  }

  fn describe(&self, at: usize, path: &[Segment]) -> Option<Info> {
    let info = schemas::describe(self.document, part(at), &wit_path(path)).ok()??;
    Some(Info {
      title: info.title,
      description: info.description,
      kind: info.kind,
      default: info.default.as_deref().map(json),
      allowed: info.allowed.iter().map(|value| json(value)).collect(),
      deprecated: info.deprecated,
      constraints: info.constraints,
      schemas: info.schemas,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::document::Part;

  #[test]
  fn positions_are_offsets_in_lines_ending_any_way() {
    let text = "ab\r\ncd\ref\ngh";
    let at = |line, column| offset_of(text, Position { line, column });
    assert_eq!(at(0, 2), Some(2));
    assert_eq!(at(1, 0), Some(4));
    assert_eq!(at(2, 1), Some(8));
    assert_eq!(at(3, 2), Some(12));
    assert_eq!(at(0, 3), None, "past the line");
    assert_eq!(at(4, 0), None, "past the text");
    assert_eq!(offset_of("é", Position { line: 0, column: 1 }), None);
  }

  #[test]
  fn trees_flatten_parents_first() {
    let tree = Node::table(
      0..20,
      vec![
        (
          "a".into(),
          Node::array(
            4..10,
            vec![
              Node::scalar(5..6, Some(Scalar::Integer(1))),
              Node::scalar(8..9, None),
            ],
          )
          .keyed(0..1),
        ),
        (
          "b".into(),
          Node::scalar(15..19, Some(Scalar::Bool(true))).keyed(11..12),
        ),
      ],
    );
    let document = Document {
      parts: vec![Part { tree, span: 0..20 }],
      ..Document::default()
    };
    let flat = to_tree(&document);
    let described: Vec<(Option<u32>, Option<&str>)> = flat
      .nodes
      .iter()
      .map(|node| (node.parent, node.key.as_deref()))
      .collect();
    assert_eq!(
      described,
      [
        (None, None),
        (Some(0), Some("a")),
        (Some(1), None),
        (Some(1), None),
        (Some(0), Some("b"))
      ]
    );
    assert_eq!(flat.parts.len(), 1);
  }
}
