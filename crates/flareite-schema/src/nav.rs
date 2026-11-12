//! What completion and hover learn from a schema about a place in a
//! document, whatever kind of schema it is. Extensions write it in their
//! languages' syntax.

use serde_json::Value as Json;

use crate::document::{Node, Segment};

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

/// A key a table may have.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
  pub name: String,
  pub description: Option<String>,
  /// The kind of value, as a schema names it (`string`, `integer | boolean`).
  pub kind: Option<String>,
  pub shape: Shape,
  pub required: bool,
  pub deprecated: bool,
}

/// A value a schema suggests or allows.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
  pub value: Json,
  pub description: Option<String>,
  /// The schema's default.
  pub default: bool,
}

/// What a schema says about a value, for hover.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
  pub title: Option<String>,
  pub description: Option<String>,
  pub kind: Option<String>,
  pub default: Option<Json>,
  /// The only values allowed, if the schema lists them.
  pub allowed: Vec<Json>,
  /// `Some` when it's deprecated, with why, if the schema says.
  pub deprecated: Option<String>,
  /// Other constraints, in words ("at least 1", "matches `^[a-z]+$`").
  pub constraints: Vec<String>,
}

impl Info {
  pub fn is_empty(&self) -> bool {
    *self == Self::default()
  }

  /// Joins what several schemas say about a value: the first that says
  /// something wins, constraints and allowed values add up.
  pub fn merge(&mut self, other: Info) {
    self.title = self.title.take().or(other.title);
    self.description = self.description.take().or(other.description);
    self.kind = self.kind.take().or(other.kind);
    self.default = self.default.take().or(other.default);
    self.deprecated = self.deprecated.take().or(other.deprecated);
    for value in other.allowed {
      if !self.allowed.contains(&value) {
        self.allowed.push(value);
      }
    }
    for constraint in other.constraints {
      if !self.constraints.contains(&constraint) {
        self.constraints.push(constraint);
      }
    }
  }
}

/// The document a schema completes or describes, as far as it's written
/// (it may have syntax errors): for schemas whose parts apply only to some
/// values, such as a JSON Schema's `if`.
pub struct Instance<'a> {
  pub tree: &'a Node,
}

impl Instance<'_> {
  /// The value at `path`, if the document has one there.
  pub fn node(&self, path: &[Segment]) -> Option<&Node> {
    let (node, taken) = self.tree.find(path);
    (taken == path.len()).then_some(node)
  }

  /// A scalar's value as JSON, if it's known.
  pub fn scalar(&self, node: &Node) -> Option<Json> {
    node.value().map(crate::document::Scalar::to_json)
  }
}

/// A schema's view of a document's paths.
pub trait Navigate {
  /// The keys the table at `path` may have, by name.
  fn properties(&self, path: &[Segment]) -> Vec<Property>;
  /// Values to suggest at `path`: allowed values, booleans, the default.
  fn choices(&self, path: &[Segment]) -> Vec<Choice>;
  /// What the schema says about the value at `path`.
  fn info(&self, path: &[Segment]) -> Option<Info>;

  /// [`Navigate::properties`] in `document`, for schemas that say more
  /// knowing it.
  fn properties_in(&self, path: &[Segment], document: &Instance<'_>) -> Vec<Property> {
    let _ = document;
    self.properties(path)
  }
  /// [`Navigate::choices`] in `document`.
  fn choices_in(&self, path: &[Segment], document: &Instance<'_>) -> Vec<Choice> {
    let _ = document;
    self.choices(path)
  }
  /// [`Navigate::info`] in `document`.
  fn info_in(&self, path: &[Segment], document: &Instance<'_>) -> Option<Info> {
    let _ = document;
    self.info(path)
  }
}

/// Adds `property` to `list` unless a property of its name is there, in
/// which case what's missing is filled in.
pub(crate) fn add_property(list: &mut Vec<Property>, property: Property) {
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

pub(crate) fn add_choice(list: &mut Vec<Choice>, choice: Choice) {
  match list.iter_mut().find(|known| known.value == choice.value) {
    Some(known) => {
      known.default |= choice.default;
      known.description = known.description.take().or(choice.description);
    }
    None => list.push(choice),
  }
}

/// Writes a value as TOML does, on one line: for TOML Schemas' own words
/// about their values, and the TOML Schemas Flareite writes.
pub fn render_toml(value: &Json) -> String {
  match value {
    Json::Null => "null".to_owned(),
    Json::Bool(b) => b.to_string(),
    Json::Number(n) => n.to_string(),
    Json::String(s) => toml_string(s),
    Json::Array(items) => format!(
      "[{}]",
      items.iter().map(render_toml).collect::<Vec<_>>().join(", ")
    ),
    Json::Object(map) => {
      if map.is_empty() {
        return "{}".to_owned();
      }
      format!(
        "{{ {} }}",
        map
          .iter()
          .map(|(key, value)| format!("{} = {}", toml_key(key), render_toml(value)))
          .collect::<Vec<_>>()
          .join(", ")
      )
    }
  }
}

/// A TOML basic string.
pub fn toml_string(s: &str) -> String {
  let mut out = String::with_capacity(s.len() + 2);
  out.push('"');
  for c in s.chars() {
    match c {
      '"' => out.push_str("\\\""),
      '\\' => out.push_str("\\\\"),
      '\n' => out.push_str("\\n"),
      '\t' => out.push_str("\\t"),
      '\r' => out.push_str("\\r"),
      c if u32::from(c) < 0x20 || c == '\u{7f}' => {
        out.push_str(&format!("\\u{:04X}", u32::from(c)))
      }
      c => out.push(c),
    }
  }
  out.push('"');
  out
}

/// A TOML key: bare if it can be, else quoted.
pub fn toml_key(key: &str) -> String {
  if !key.is_empty()
    && key
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
  {
    key.to_owned()
  } else {
    toml_string(key)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn values_render_as_toml() {
    assert_eq!(render_toml(&json!("a\"b")), "\"a\\\"b\"");
    assert_eq!(
      render_toml(&json!({"x y": [1, true]})),
      "{ \"x y\" = [1, true] }"
    );
    assert_eq!(toml_key("tab_size"), "tab_size");
    assert_eq!(toml_key("example.tidy"), "\"example.tidy\"");
  }
}
