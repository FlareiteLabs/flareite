//! A configuration document as Flareite's schemas see it: tables, arrays,
//! and scalars, each with where it's written, so a problem found in a value
//! can be marked on it.
//!
//! Flareite knows no configuration language itself. An extension parses its
//! language's documents into this tree (the `schemas` interface's
//! `set-document`), and Flareite checks the tree against the schemas that
//! apply, and answers what they say about a path in it, which the extension
//! writes in its own syntax for completion and hover.
//!
//! Values are typed as TOML's are, which covers JSON's and YAML's too: null
//! (which TOML lacks), booleans, integers, floats, strings, and TOML's dates
//! and times. JSON Schemas see dates and times as strings.

use std::ops::Range;

use serde_json::{Number, Value as Json};

/// Byte offsets in a document's text.
pub type Span = Range<usize>;

/// A step in a path to a value: a table's key or an array's index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Segment {
  Key(String),
  Index(usize),
}

impl Segment {
  pub fn key(key: impl Into<String>) -> Self {
    Self::Key(key.into())
  }
}

/// A scalar value.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
  Null,
  Bool(bool),
  Integer(i64),
  Float(f64),
  String(String),
  /// A TOML date, time, or both, as RFC 3339 (or TOML) writes it:
  /// `1979-05-27T07:32:00Z`, `1979-05-27`, `07:32:00`.
  Datetime(String),
}

impl Scalar {
  /// The value as JSON: dates and times are strings, and floats JSON can't
  /// hold (infinities, not a number) are strings as TOML writes them.
  pub fn to_json(&self) -> Json {
    match self {
      Self::Null => Json::Null,
      Self::Bool(b) => Json::Bool(*b),
      Self::Integer(i) => Json::Number((*i).into()),
      Self::Float(f) => match Number::from_f64(*f) {
        Some(number) => Json::Number(number),
        None if f.is_nan() => Json::String("nan".into()),
        None if *f > 0.0 => Json::String("inf".into()),
        None => Json::String("-inf".into()),
      },
      Self::String(s) | Self::Datetime(s) => Json::String(s.clone()),
    }
  }

  /// A JSON scalar as a scalar; `None` for arrays and objects.
  pub fn from_json(value: &Json) -> Option<Self> {
    Some(match value {
      Json::Null => Self::Null,
      Json::Bool(b) => Self::Bool(*b),
      Json::Number(n) => match n.as_i64() {
        Some(i) => Self::Integer(i),
        None => Self::Float(n.as_f64().unwrap_or(f64::NAN)),
      },
      Json::String(s) => Self::String(s.clone()),
      Json::Array(_) | Json::Object(_) => return None,
    })
  }
}

/// A value in a document.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
  /// The whole value, as written. Empty when the value isn't written out
  /// anywhere of its own (a TOML table made by a dotted key).
  pub span: Span,
  /// The key naming it in its table (in a TOML header, the header's last
  /// key), if it has one.
  pub key_span: Option<Span>,
  pub kind: NodeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
  /// A table, object, or mapping, with its entries in the order written.
  Table(Vec<(String, Node)>),
  Array(Vec<Node>),
  /// A scalar, with its value when the document says it: `None` for one
  /// still being typed, which can't be read yet.
  Scalar(Option<Scalar>),
}

impl Node {
  pub fn scalar(span: Span, value: Option<Scalar>) -> Self {
    Self {
      span,
      key_span: None,
      kind: NodeKind::Scalar(value),
    }
  }

  pub fn table(span: Span, entries: Vec<(String, Node)>) -> Self {
    Self {
      span,
      key_span: None,
      kind: NodeKind::Table(entries),
    }
  }

  pub fn array(span: Span, items: Vec<Node>) -> Self {
    Self {
      span,
      key_span: None,
      kind: NodeKind::Array(items),
    }
  }

  /// The node with its key at `key_span`.
  pub fn keyed(mut self, key_span: Span) -> Self {
    self.key_span = Some(key_span);
    self
  }

  /// The entry `key` of a table.
  pub fn get(&self, key: &str) -> Option<&Node> {
    match &self.kind {
      NodeKind::Table(entries) => entries
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, node)| node),
      _ => None,
    }
  }

  /// The value at `path`, or the deepest one on the way there, with how
  /// many steps of `path` were taken.
  pub fn find(&self, path: &[Segment]) -> (&Node, usize) {
    let mut node = self;
    for (taken, segment) in path.iter().enumerate() {
      let next = match (segment, &node.kind) {
        (Segment::Key(key), NodeKind::Table(_)) => node.get(key),
        (Segment::Index(index), NodeKind::Array(items)) => items.get(*index),
        // A JSON pointer's number may name an object's key.
        (Segment::Index(index), NodeKind::Table(_)) => node.get(&index.to_string()),
        _ => None,
      };
      match next {
        Some(next) => node = next,
        None => return (node, taken),
      }
    }
    (node, path.len())
  }

  /// A scalar's value, if it's known.
  pub fn value(&self) -> Option<&Scalar> {
    match &self.kind {
      NodeKind::Scalar(value) => value.as_ref(),
      _ => None,
    }
  }

  /// Calls `f` with each key in the tree, its path, and its value.
  pub fn visit_keys(&self, f: &mut dyn FnMut(&[Segment], &Node)) {
    fn walk(node: &Node, path: &mut Vec<Segment>, f: &mut dyn FnMut(&[Segment], &Node)) {
      match &node.kind {
        NodeKind::Table(entries) => {
          for (key, child) in entries {
            path.push(Segment::Key(key.clone()));
            if child.key_span.is_some() {
              f(path, child);
            }
            walk(child, path, f);
            path.pop();
          }
        }
        NodeKind::Array(items) => {
          for (index, item) in items.iter().enumerate() {
            path.push(Segment::Index(index));
            walk(item, path, f);
            path.pop();
          }
        }
        NodeKind::Scalar(_) => {}
      }
    }
    walk(self, &mut Vec::new(), f);
  }

  /// The value as JSON, or `None` if a scalar in it isn't known. Of a key
  /// written twice, the last is kept.
  pub fn to_json(&self) -> Option<Json> {
    Some(match &self.kind {
      NodeKind::Scalar(value) => value.as_ref()?.to_json(),
      NodeKind::Array(items) => Json::Array(
        items
          .iter()
          .map(Node::to_json)
          .collect::<Option<Vec<_>>>()?,
      ),
      NodeKind::Table(entries) => {
        let mut map = serde_json::Map::new();
        for (key, child) in entries {
          map.insert(key.clone(), child.to_json()?);
        }
        Json::Object(map)
      }
    })
  }

  /// A JSON value as a tree, written nowhere (every span is empty).
  pub fn from_json(value: &Json) -> Self {
    match value {
      Json::Array(items) => Self::array(0..0, items.iter().map(Self::from_json).collect()),
      Json::Object(map) => Self::table(
        0..0,
        map
          .iter()
          .map(|(key, value)| (key.clone(), Self::from_json(value)))
          .collect(),
      ),
      scalar => Self::scalar(0..0, Scalar::from_json(scalar)),
    }
  }
}

/// A document in a file: one, except in a stream of them, such as YAML's
/// documents separated by `---`.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
  pub tree: Node,
  /// The text it's written in. A stream's parts cover the text between
  /// them, in order, so the cursor is always in one.
  pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
  Error,
  Warning,
  Information,
  Hint,
}

/// A problem the parser found: a syntax error, or a key written twice.
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
  pub span: Span,
  pub severity: Severity,
  pub message: String,
}

/// A parsed document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
  /// The documents in the file: none if it's empty (or only comments).
  pub parts: Vec<Part>,
  /// The schema the document names for itself (a `$schema` key, or a
  /// comment such as `#:schema`), as written: an address, or a path
  /// relative to the document's file.
  pub named_schema: Option<String>,
  /// What the parser found wrong. If one is an error, the document isn't
  /// checked against schemas: the problems are reported alone.
  pub problems: Vec<Problem>,
}

impl Document {
  /// A document of one part, `tree`, written in `text`.
  pub fn single(tree: Node, text: &str) -> Self {
    Self {
      parts: vec![Part {
        tree,
        span: 0..text.len(),
      }],
      named_schema: None,
      problems: Vec::new(),
    }
  }

  /// Whether a problem is an error.
  pub fn has_errors(&self) -> bool {
    self
      .problems
      .iter()
      .any(|problem| problem.severity == Severity::Error)
  }

  /// The index of the part at `offset`: the first that ends at or after it,
  /// else the last. `None` if there are none.
  pub fn part_at(&self, offset: usize) -> Option<usize> {
    if self.parts.is_empty() {
      return None;
    }
    Some(
      self
        .parts
        .iter()
        .position(|part| offset <= part.span.end)
        .unwrap_or(self.parts.len() - 1),
    )
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  #[test]
  fn trees_convert_to_and_from_json() {
    let value = json!({ "a": [1, 2.5, "x", null, true], "b": { "c": {} } });
    let tree = Node::from_json(&value);
    assert_eq!(tree.to_json(), Some(value));
    let (node, taken) = tree.find(&[Segment::key("a"), Segment::Index(2)]);
    assert_eq!(taken, 2);
    assert_eq!(node.value(), Some(&Scalar::String("x".into())));
    let (_, taken) = tree.find(&[Segment::key("b"), Segment::key("zzz")]);
    assert_eq!(taken, 1);
    // A scalar not yet known makes the whole unknown.
    let unknown = Node::table(0..0, vec![("a".into(), Node::scalar(0..0, None))]);
    assert_eq!(unknown.to_json(), None);
    assert_eq!(Scalar::Float(f64::INFINITY).to_json(), json!("inf"));
    assert_eq!(
      Scalar::Datetime("1979-05-27".into()).to_json(),
      json!("1979-05-27")
    );
  }

  #[test]
  fn keys_are_visited_with_their_paths() {
    let tree = Node::table(
      0..10,
      vec![(
        "a".into(),
        Node::array(
          4..9,
          vec![Node::table(
            5..8,
            vec![(
              "b".into(),
              Node::scalar(7..8, Some(Scalar::Integer(1))).keyed(5..6),
            )],
          )],
        )
        .keyed(0..1),
      )],
    );
    let mut seen = Vec::new();
    tree.visit_keys(&mut |path, _| seen.push(path.to_vec()));
    assert_eq!(
      seen,
      [
        vec![Segment::key("a")],
        vec![Segment::key("a"), Segment::Index(0), Segment::key("b")]
      ]
    );
  }

  #[test]
  fn the_part_at_an_offset() {
    let part = |span: Span| Part {
      tree: Node::table(span.clone(), Vec::new()),
      span,
    };
    let document = Document {
      parts: vec![part(0..10), part(10..20)],
      ..Document::default()
    };
    assert_eq!(document.part_at(3), Some(0));
    assert_eq!(document.part_at(10), Some(0));
    assert_eq!(document.part_at(11), Some(1));
    assert_eq!(document.part_at(99), Some(1));
    assert_eq!(Document::default().part_at(0), None);
  }
}
