//! Completion and hover for TOML Schemas: the definitions that may apply at
//! a path, following references, compositions, and alternatives (all of
//! them: a document being typed hasn't chosen yet).

use serde_json::Value as Json;

use super::schema::{ConditionPredicate, Definition, Schema, SchemaType};
use crate::document::{Segment, toml_to_json};
use crate::nav::{Choice, Info, Navigate, Property, Shape, add_choice, add_property};

/// Deepest chain of references followed.
const MAX_DEPTH: usize = 32;

#[derive(Clone, Copy)]
enum Target<'a> {
  /// The document: `[elements]`.
  Root,
  Def(&'a Definition),
  Builtin(SchemaType),
}

impl Schema {
  /// What a `type`-like reference names.
  fn target(&self, reference: &str) -> Option<Target<'_>> {
    let name = reference.strip_prefix("types.").unwrap_or(reference);
    if let Some(builtin) = SchemaType::parse(name) {
      return Some(Target::Builtin(builtin));
    }
    self.types.get(name).map(Target::Def)
  }

  /// `target` and every definition it brings in: its named type, `allof`
  /// components, and every alternative and branch.
  fn expand<'a>(&'a self, target: Target<'a>, out: &mut Vec<Target<'a>>, depth: usize) {
    if depth > MAX_DEPTH {
      return;
    }
    out.push(target);
    let Target::Def(definition) = target else {
      return;
    };
    let mut references: Vec<&str> = Vec::new();
    references.extend(definition.reference.as_deref());
    references.extend(definition.all_of.iter().map(String::as_str));
    references.extend(definition.one_of.iter().map(String::as_str));
    references.extend(definition.any_of.iter().map(String::as_str));
    if let Some(conditional) = &definition.conditional {
      references.push(&conditional.then_reference);
      references.push(&conditional.else_reference);
    }
    for reference in references {
      if let Some(next) = self.target(reference) {
        self.expand(next, out, depth + 1);
      }
    }
  }

  fn expanded<'a>(&'a self, targets: &[Target<'a>]) -> Vec<Target<'a>> {
    let mut out = Vec::new();
    for &target in targets {
      self.expand(target, &mut out, 0);
    }
    out
  }

  /// The definitions at `path`.
  fn at(&self, path: &[Segment]) -> Vec<Target<'_>> {
    let mut targets = vec![Target::Root];
    for segment in path {
      let mut next = Vec::new();
      for target in self.expanded(&targets) {
        self.step(target, segment, &mut next, true);
      }
      if next.is_empty() {
        return next;
      }
      targets = next;
    }
    self.expanded(&targets)
  }

  fn step<'a>(
    &'a self,
    target: Target<'a>,
    segment: &Segment,
    out: &mut Vec<Target<'a>>,
    into_arrays: bool,
  ) {
    match (target, segment) {
      (Target::Root, Segment::Key(key)) => out.extend(self.elements.get(key).map(Target::Def)),
      (Target::Def(definition), Segment::Key(key)) => {
        if let Some(child) = definition.children.get(key) {
          out.push(Target::Def(child));
        } else if is_kind(definition, SchemaType::Collection) {
          out.extend(item_target(self, definition));
        } else if into_arrays && is_kind(definition, SchemaType::Array) {
          // A key under an array of tables names the last table's.
          for item in item_targets(self, definition) {
            for item in self.expanded(&[item]) {
              self.step(item, segment, out, false);
            }
          }
        }
      }
      (Target::Def(definition), Segment::Index(index)) => {
        if definition.items.is_empty() {
          out.extend(item_target(self, definition));
        } else if let Some(reference) = definition.items.get(*index) {
          out.extend(self.target(reference));
        }
      }
      _ => {}
    }
  }

  fn info_of(&self, target: Target<'_>) -> Info {
    let Target::Def(definition) = target else {
      return Info {
        kind: match target {
          Target::Builtin(kind) => Some(kind.schema_name().to_owned()),
          _ => None,
        },
        ..Info::default()
      };
    };
    let mut constraints = Vec::new();
    let quoted = |value: &toml::Value| crate::nav::render_toml(&toml_to_json(value));
    match (&definition.min, &definition.max) {
      (Some(min), Some(max)) => {
        constraints.push(format!("From {} to {}", quoted(min), quoted(max)))
      }
      (Some(min), None) => constraints.push(format!("At least {}", quoted(min))),
      (None, Some(max)) => constraints.push(format!("At most {}", quoted(max))),
      (None, None) => {}
    }
    let length = match (definition.min_length, definition.max_length) {
      (Some(min), Some(max)) if min == max => Some(format!("Length {min}")),
      (Some(min), Some(max)) => Some(format!("Length {min} to {max}")),
      (Some(min), None) => Some(format!("Length at least {min}")),
      (None, Some(max)) => Some(format!("Length at most {max}")),
      (None, None) => None,
    };
    constraints.extend(length);
    if let Some(pattern) = &definition.pattern {
      constraints.push(format!("Matches `{}`", pattern.as_str()));
    }
    if let Some(pattern) = &definition.key_pattern {
      constraints.push(format!("Keys match `{}`", pattern.as_str()));
    }
    if let Some(format) = &definition.string_format {
      constraints.push(format!("Format: {}", format.name()));
    }
    if definition.unique_items == Some(true) {
      constraints.push("Items are unique".to_owned());
    }
    Info {
      title: None,
      description: definition.description.clone(),
      kind: kind_label(self, definition),
      default: definition.default_value.as_ref().map(toml_to_json),
      allowed: definition.allowed_values.iter().map(toml_to_json).collect(),
      deprecated: definition.deprecated.then(String::new),
      constraints,
    }
  }
}

fn is_kind(definition: &Definition, kind: SchemaType) -> bool {
  definition.type_name == Some(kind)
}

fn item_target<'a>(schema: &'a Schema, definition: &'a Definition) -> Option<Target<'a>> {
  definition
    .item_reference
    .as_deref()
    .and_then(|reference| schema.target(reference))
}

fn item_targets<'a>(schema: &'a Schema, definition: &'a Definition) -> Vec<Target<'a>> {
  let mut targets: Vec<Target<'a>> = item_target(schema, definition).into_iter().collect();
  targets.extend(
    definition
      .items
      .iter()
      .filter_map(|reference| schema.target(reference)),
  );
  targets
}

/// The kind of value a definition takes, in TOML Schema's words.
fn kind_label(schema: &Schema, definition: &Definition) -> Option<String> {
  if let Some(kind) = definition.type_name {
    return Some(match kind {
      SchemaType::Array | SchemaType::Collection => match &definition.item_reference {
        Some(item) => format!(
          "{}<{}>",
          kind.schema_name(),
          item.strip_prefix("types.").unwrap_or(item)
        ),
        None => kind.schema_name().to_owned(),
      },
      kind => kind.schema_name().to_owned(),
    });
  }
  if let Some(reference) = &definition.reference {
    return match schema.target(reference) {
      Some(Target::Def(named)) => kind_label(schema, named).or_else(|| {
        Some(
          reference
            .strip_prefix("types.")
            .unwrap_or(reference)
            .to_owned(),
        )
      }),
      _ => Some(reference.clone()),
    };
  }
  let alternatives = if definition.one_of.is_empty() {
    &definition.any_of
  } else {
    &definition.one_of
  };
  if !alternatives.is_empty() {
    let labels: Vec<String> = alternatives
      .iter()
      .map(|reference| match schema.target(reference) {
        Some(Target::Def(named)) => kind_label(schema, named).unwrap_or_else(|| reference.clone()),
        Some(Target::Builtin(kind)) => kind.schema_name().to_owned(),
        _ => reference.clone(),
      })
      .collect();
    let mut unique: Vec<String> = Vec::new();
    for label in labels {
      if !unique.contains(&label) {
        unique.push(label);
      }
    }
    return Some(unique.join(" | "));
  }
  if !definition.children.is_empty() {
    return Some("table".to_owned());
  }
  None
}

/// How a definition's value is written.
fn shape(schema: &Schema, definition: &Definition) -> Shape {
  let mut targets = Vec::new();
  schema.expand(Target::Def(definition), &mut targets, 0);
  for target in targets {
    let Target::Def(definition) = target else {
      continue;
    };
    match definition.type_name {
      Some(SchemaType::Table | SchemaType::Collection) => return Shape::Table,
      Some(SchemaType::Array) => {
        let tables = item_targets(schema, definition).into_iter().any(|item| {
          let mut items = Vec::new();
          schema.expand(item, &mut items, 0);
          items.iter().any(|item| match item {
            Target::Builtin(SchemaType::Table) => true,
            Target::Def(item) => {
              matches!(
                item.type_name,
                Some(SchemaType::Table | SchemaType::Collection)
              ) || (item.type_name.is_none() && !item.children.is_empty())
            }
            _ => false,
          })
        });
        return if tables {
          Shape::ArrayOfTables
        } else {
          Shape::Array
        };
      }
      None if !definition.children.is_empty() && definition.reference.is_none() => {
        return Shape::Table;
      }
      _ => {}
    }
  }
  Shape::Value
}

impl Navigate for Schema {
  fn properties(&self, path: &[Segment]) -> Vec<Property> {
    let mut list = Vec::new();
    let mut add = |name: &str, child: &Definition| {
      add_property(
        &mut list,
        Property {
          name: name.to_owned(),
          description: child.description.clone().or_else(|| {
            // A named type's description.
            child
              .reference
              .as_deref()
              .and_then(|reference| match self.target(reference) {
                Some(Target::Def(named)) => named.description.clone(),
                _ => None,
              })
          }),
          kind: kind_label(self, child),
          shape: shape(self, child),
          required: !child.optional,
          deprecated: child.deprecated,
        },
      );
    };
    for target in self.at(path) {
      match target {
        Target::Root => {
          for (name, child) in &self.elements {
            add(name, child);
          }
        }
        Target::Def(definition) => {
          for (name, child) in &definition.children {
            add(name, child);
          }
        }
        Target::Builtin(_) => {}
      }
    }
    list
  }

  fn choices(&self, path: &[Segment]) -> Vec<Choice> {
    let mut list = Vec::new();
    let targets = self.at(path);
    for &target in &targets {
      match target {
        Target::Builtin(SchemaType::Boolean) => {
          for value in [true, false] {
            add_choice(
              &mut list,
              Choice {
                value: Json::Bool(value),
                description: None,
                default: false,
              },
            );
          }
        }
        Target::Def(definition) => {
          for value in &definition.allowed_values {
            add_choice(
              &mut list,
              Choice {
                value: toml_to_json(value),
                description: None,
                default: false,
              },
            );
          }
          if is_kind(definition, SchemaType::Boolean) {
            for value in [true, false] {
              add_choice(
                &mut list,
                Choice {
                  value: Json::Bool(value),
                  description: None,
                  default: false,
                },
              );
            }
          }
          if let Some(default) = &definition.default_value {
            add_choice(
              &mut list,
              Choice {
                value: toml_to_json(default),
                description: None,
                default: true,
              },
            );
          }
        }
        _ => {}
      }
    }
    // A parent's conditional decides between branches by a key's value:
    // offer the values it tests.
    if let Some((Segment::Key(key), parent)) = path.split_last() {
      for target in self.at(parent) {
        if let Target::Def(definition) = target
          && let Some(conditional) = &definition.conditional
          && conditional.key == *key
        {
          let values: Vec<&toml::Value> = match &conditional.predicate {
            ConditionPredicate::Equals(value) => vec![value],
            ConditionPredicate::In(values) => values.iter().collect(),
          };
          for value in values {
            add_choice(
              &mut list,
              Choice {
                value: toml_to_json(value),
                description: None,
                default: false,
              },
            );
          }
        }
      }
    }
    list
  }

  fn info(&self, path: &[Segment]) -> Option<Info> {
    let targets = self.at(path);
    let mut info: Option<Info> = None;
    for target in targets {
      if matches!(target, Target::Root) {
        continue;
      }
      let more = self.info_of(target);
      match &mut info {
        Some(info) => info.merge(more),
        None => info = Some(more),
      }
    }
    info.filter(|info| !info.is_empty())
  }
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use super::*;

  const SCHEMA: &str = r#"
[toml-schema]
version = "1.0.0"

[types.port]
type = "integer"
min = 1
max = 65535
description = "A TCP port."

[types.product]
type = "table"
[types.product.name]
type = "string"
[types.product.sku]
type = "string"
optional = true
deprecated = true

[elements.title]
type = "string"
description = "The title."
default = "Untitled"

[elements.mode]
type = "string"
allowedvalues = ["fast", "safe"]
optional = true

[elements.server]
type = "table"
optional = true
[elements.server.port]
type = "types.port"
[elements.server.enabled]
type = "boolean"
optional = true

[elements.products]
type = "array"
itemtype = "types.product"
optional = true

[elements.env]
type = "collection"
itemtype = "string"
optional = true
"#;

  fn schema() -> Schema {
    Schema::from_source(PathBuf::from("test.tosd"), SCHEMA).expect("schema loads")
  }

  #[test]
  fn properties_follow_tables_arrays_and_types() {
    let schema = schema();
    let names = |path: &[Segment]| -> Vec<String> {
      schema
        .properties(path)
        .into_iter()
        .map(|property| property.name)
        .collect()
    };
    assert_eq!(names(&[]), ["env", "mode", "products", "server", "title"]);
    assert_eq!(names(&[Segment::key("server")]), ["enabled", "port"]);
    assert_eq!(
      names(&[Segment::key("products"), Segment::Index(0)]),
      ["name", "sku"]
    );
    // Without an index, as in `[products.x]` under `[[products]]`.
    assert_eq!(names(&[Segment::key("products")]), Vec::<String>::new());
    let root = schema.properties(&[]);
    let products = root
      .iter()
      .find(|p| p.name == "products")
      .expect("products");
    assert_eq!(products.shape, Shape::ArrayOfTables);
    assert!(!products.required);
    let server = root.iter().find(|p| p.name == "server").expect("server");
    assert_eq!(server.shape, Shape::Table);
    let title = root.iter().find(|p| p.name == "title").expect("title");
    assert!(title.required);
    assert_eq!(title.kind.as_deref(), Some("string"));
    let sku = schema.properties(&[Segment::key("products"), Segment::Index(3)]);
    assert!(sku.iter().any(|p| p.name == "sku" && p.deprecated));
  }

  #[test]
  fn choices_and_info() {
    let schema = schema();
    let values: Vec<Json> = schema
      .choices(&[Segment::key("mode")])
      .into_iter()
      .map(|choice| choice.value)
      .collect();
    assert_eq!(values, [Json::from("fast"), Json::from("safe")]);
    let enabled: Vec<Json> = schema
      .choices(&[Segment::key("server"), Segment::key("enabled")])
      .into_iter()
      .map(|choice| choice.value)
      .collect();
    assert_eq!(enabled, [Json::Bool(true), Json::Bool(false)]);
    let title = schema.choices(&[Segment::key("title")]);
    assert!(title[0].default);

    let port = schema
      .info(&[Segment::key("server"), Segment::key("port")])
      .expect("info");
    assert_eq!(port.description.as_deref(), Some("A TCP port."));
    assert_eq!(port.kind.as_deref(), Some("integer"));
    assert_eq!(port.constraints, ["From 1 to 65535"]);
    let env = schema
      .info(&[Segment::key("env"), Segment::key("ANY")])
      .expect("collection entries");
    assert_eq!(env.kind.as_deref(), Some("string"));
    assert!(schema.info(&[Segment::key("nope")]).is_none());
  }
}
