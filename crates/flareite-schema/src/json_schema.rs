//! JSON Schemas: validation with the `jsonschema` crate, and completion and
//! hover from the schema itself, following `$ref`s within it and into the
//! other schemas it references (which Flareite fetches first: see
//! [`external_references`]). VS Code's additions are understood:
//! `markdownDescription`, `enumDescriptions`, `markdownEnumDescriptions`,
//! and `deprecationMessage`.
//!
//! Knowing the document, completion and hover follow an `if` to its `then`
//! or its `else`, when the `if` is of the simple kind schemas use to pick a
//! branch (`const`, `enum`, `type`, `required`, and `properties` of those),
//! so that, say, an object's `"kind": "a"` offers only kind a's keys.
//! Otherwise both branches apply.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jsonschema::error::ValidationErrorKind;
use jsonschema::{Retrieve, Uri, Validator};
use regex::Regex;
use serde_json::Value as Json;
use url::Url;

use crate::document::{Node, NodeKind, Segment, parse_json_pointer};
use crate::nav::{Choice, Info, Instance, Navigate, Property, Shape, add_choice, add_property};

/// Deepest chain of references followed.
const MAX_DEPTH: usize = 32;
/// Most problems reported in one document.
const MAX_ERRORS: usize = 1000;

/// Keywords whose values are schemas.
const SCHEMA_KEYWORDS: &[&str] = &[
  "additionalProperties",
  "additionalItems",
  "items",
  "contains",
  "not",
  "if",
  "then",
  "else",
  "propertyNames",
  "unevaluatedProperties",
  "unevaluatedItems",
];
/// Keywords whose values are maps of schemas.
const SCHEMA_MAP_KEYWORDS: &[&str] = &[
  "properties",
  "patternProperties",
  "definitions",
  "$defs",
  "dependentSchemas",
];
/// Keywords whose values are arrays of schemas.
const SCHEMA_ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems", "items"];

/// A problem a JSON Schema finds.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonProblem {
  /// Where in the document, as a path.
  pub path: Vec<Segment>,
  /// Whether `path` names a key to mark (an unexpected or a missing
  /// property's object), rather than a value.
  pub key: bool,
  pub message: String,
}

/// A loaded JSON Schema, with the schemas it references.
pub struct JsonSchema {
  uri: String,
  /// Every document by its address (without a fragment): the schema and
  /// what it references.
  documents: HashMap<String, Arc<Json>>,
  /// Addresses that `$id`s and anchors give subschemas: the document and
  /// the JSON pointer to the subschema.
  ids: HashMap<String, (String, String)>,
  validator: Result<Validator, String>,
}

/// Retrieves only the documents Flareite fetched.
struct Fetched(HashMap<String, Arc<Json>>);

impl Retrieve for Fetched {
  fn retrieve(&self, uri: &Uri<String>) -> Result<Json, Box<dyn std::error::Error + Send + Sync>> {
    let address = uri.as_str();
    let address = address.split_once('#').map_or(address, |(base, _)| base);
    self
      .0
      .get(address)
      .map(|document| (**document).clone())
      .ok_or_else(|| format!("{address} was not fetched").into())
  }
}

/// Removes a fragment and an empty `#`.
fn without_fragment(uri: &str) -> &str {
  uri.split_once('#').map_or(uri, |(base, _)| base)
}

fn join(base: &str, reference: &str) -> Option<String> {
  match Url::parse(base) {
    Ok(base) => base.join(reference).ok().map(String::from),
    // Not a URL (an id like "settings"): only absolute references work.
    Err(_) => Url::parse(reference).ok().map(String::from),
  }
}

/// The addresses of other documents `schema` (at `uri`) references, so
/// they can be fetched before it's loaded: `$ref`s and `$schema` aren't
/// followed for the meta-schema, which is known.
pub fn external_references(uri: &str, schema: &Json) -> Vec<String> {
  let mut found = Vec::new();
  let mut ids = HashSet::new();
  collect_references(uri, schema, &mut found, &mut ids);
  found.retain(|address: &String| !ids.contains(address) && address != without_fragment(uri));
  found.sort();
  found.dedup();
  found
}

fn collect_references(
  base: &str,
  schema: &Json,
  found: &mut Vec<String>,
  ids: &mut HashSet<String>,
) {
  let Json::Object(map) = schema else {
    return;
  };
  let mut base = base.to_owned();
  if let Some(id) = map
    .get("$id")
    .or_else(|| map.get("id"))
    .and_then(Json::as_str)
    && let Some(joined) = join(&base, id)
  {
    base = joined;
    ids.insert(without_fragment(&base).to_owned());
  }
  for keyword in ["$ref", "$dynamicRef", "$recursiveRef"] {
    if let Some(reference) = map.get(keyword).and_then(Json::as_str)
      && !reference.starts_with('#')
      && let Some(address) = join(&base, reference)
    {
      found.push(without_fragment(&address).to_owned());
    }
  }
  for_each_subschema(map, |sub| collect_references(&base, sub, found, ids));
}

fn for_each_subschema(map: &serde_json::Map<String, Json>, mut f: impl FnMut(&Json)) {
  for keyword in SCHEMA_KEYWORDS {
    if let Some(sub) = map.get(*keyword)
      && sub.is_object()
    {
      f(sub);
    }
  }
  for keyword in SCHEMA_MAP_KEYWORDS {
    if let Some(Json::Object(subs)) = map.get(*keyword) {
      subs.values().for_each(&mut f);
    }
  }
  for keyword in SCHEMA_ARRAY_KEYWORDS {
    if let Some(Json::Array(subs)) = map.get(*keyword) {
      subs.iter().for_each(&mut f);
    }
  }
}

/// A subschema, and the address relative references in it resolve
/// against.
#[derive(Clone)]
struct Sub<'a> {
  schema: &'a Json,
  base: String,
}

impl JsonSchema {
  /// Loads `schema` (from `uri`), with `fetched`: the documents it
  /// references, by address.
  pub fn new(uri: &str, schema: Json, fetched: HashMap<String, Json>) -> Self {
    let uri = without_fragment(uri).to_owned();
    let mut documents: HashMap<String, Arc<Json>> = fetched
      .into_iter()
      .map(|(address, document)| (without_fragment(&address).to_owned(), Arc::new(document)))
      .collect();
    documents.insert(uri.clone(), Arc::new(schema));
    let mut ids = HashMap::new();
    for (address, document) in &documents {
      index_ids(address, address, document, String::new(), &mut ids);
    }
    let root = documents[&uri].clone();
    let mut options = jsonschema::options().with_retriever(Fetched(documents.clone()));
    if Url::parse(&uri).is_ok() {
      options = options.with_base_uri(uri.clone());
    }
    let validator = options.build(&root).map_err(|error| error.to_string());
    Self {
      uri,
      documents,
      ids,
      validator,
    }
  }

  /// Why the schema can't validate anything, if it can't.
  pub fn error(&self) -> Option<&str> {
    self.validator.as_ref().err().map(String::as_str)
  }

  /// The problems with `instance`.
  pub fn validate(&self, instance: &Json) -> Vec<JsonProblem> {
    let Ok(validator) = &self.validator else {
      return Vec::new();
    };
    let mut problems = Vec::new();
    for error in validator.iter_errors(instance).take(MAX_ERRORS) {
      let path = parse_json_pointer(error.instance_path().as_str());
      match error.kind() {
        ValidationErrorKind::AdditionalProperties { unexpected }
        | ValidationErrorKind::UnevaluatedProperties { unexpected } => {
          for name in unexpected {
            let mut path = path.clone();
            path.push(Segment::Key(name.clone()));
            problems.push(JsonProblem {
              path,
              key: true,
              message: format!("Property {name} is not allowed."),
            });
          }
        }
        ValidationErrorKind::Required { property } => problems.push(JsonProblem {
          path,
          key: true,
          message: format!(
            "Missing property {}.",
            property
              .as_str()
              .map_or_else(|| property.to_string(), |p| format!("\"{p}\""))
          ),
        }),
        _ => problems.push(JsonProblem {
          path,
          key: false,
          message: error.to_string(),
        }),
      }
    }
    problems
  }

  fn root(&self) -> Sub<'_> {
    Sub {
      schema: &self.documents[&self.uri],
      base: self.uri.clone(),
    }
  }

  /// The subschema an address names.
  fn resolve(&self, base: &str, reference: &str) -> Option<Sub<'_>> {
    let absolute = if reference.starts_with('#') {
      format!("{}{reference}", without_fragment(base))
    } else {
      join(base, reference)?
    };
    let (address, fragment) = absolute
      .split_once('#')
      .map_or((absolute.as_str(), ""), |(address, fragment)| {
        (address, fragment)
      });
    let (document, pointer) = if fragment.is_empty() || fragment.starts_with('/') {
      match self.ids.get(address) {
        Some((document, at)) => (document.clone(), format!("{at}{fragment}")),
        None => (address.to_owned(), fragment.to_owned()),
      }
    } else {
      // An anchor.
      self.ids.get(&absolute)?.clone()
    };
    let root = self.documents.get(&document)?;
    let pointer = percent_decode(&pointer);
    let schema = root.pointer(&pointer)?;
    Some(Sub {
      schema,
      base: address.to_owned(),
    })
  }

  /// `sub` and every subschema that applies with it: references,
  /// compositions, and every alternative and branch (the one an `if`
  /// picks, when `here` is the value it's for and it can tell).
  fn expand<'a>(&'a self, sub: Sub<'a>, out: &mut Vec<Sub<'a>>, depth: usize, here: Here<'_>) {
    if depth > MAX_DEPTH {
      return;
    }
    let Json::Object(map) = sub.schema else {
      return;
    };
    let mut base = sub.base.clone();
    if let Some(id) = map.get("$id").and_then(Json::as_str)
      && !id.starts_with('#')
      && let Some(joined) = join(&base, id)
    {
      base = without_fragment(&joined).to_owned();
    }
    for keyword in ["$ref", "$dynamicRef", "$recursiveRef"] {
      if let Some(reference) = map.get(keyword).and_then(Json::as_str)
        && let Some(target) = self.resolve(&base, reference)
      {
        self.expand(target, out, depth + 1, here);
      }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
      if let Some(Json::Array(subs)) = map.get(keyword) {
        for schema in subs {
          self.expand(
            Sub {
              schema,
              base: base.clone(),
            },
            out,
            depth + 1,
            here,
          );
        }
      }
    }
    let picked = map
      .get("if")
      .zip(here)
      .and_then(|(test, (node, document))| holds(test, node, document));
    for (keyword, applies) in [
      ("then", picked != Some(false)),
      ("else", picked != Some(true)),
    ] {
      if let Some(schema) = map.get(keyword).filter(|_| applies) {
        self.expand(
          Sub {
            schema,
            base: base.clone(),
          },
          out,
          depth + 1,
          here,
        );
      }
    }
    out.push(Sub {
      schema: sub.schema,
      base,
    });
  }

  fn expanded<'a>(&'a self, subs: Vec<Sub<'a>>, here: Here<'_>) -> Vec<Sub<'a>> {
    let mut out = Vec::new();
    for sub in subs {
      self.expand(sub, &mut out, 0, here);
    }
    out
  }

  /// The subschemas for the value at `path`, in `document` if known.
  fn at(&self, path: &[Segment], document: Option<&Instance<'_>>) -> Vec<Sub<'_>> {
    let here = |taken: &[Segment]| {
      document.and_then(|document| document.node(taken).map(|node| (node, document)))
    };
    let mut subs = self.expanded(vec![self.root()], here(&[]));
    for (index, segment) in path.iter().enumerate() {
      let mut next = Vec::new();
      for sub in &subs {
        step(sub, segment, &mut next, true);
      }
      if next.is_empty() {
        return next;
      }
      subs = self.expanded(next, here(&path[..=index]));
    }
    subs
  }

  fn shape(&self, sub: Sub<'_>) -> Shape {
    let subs = self.expanded(vec![sub], None);
    let is = |sub: &Sub<'_>, kind: &str| types(sub.schema).iter().any(|t| t == kind);
    let objectish = |sub: &Sub<'_>| {
      is(sub, "object") || sub.schema.get("properties").is_some_and(Json::is_object)
    };
    if subs.iter().any(objectish) {
      return Shape::Table;
    }
    if let Some(array) = subs.iter().find(|sub| is(sub, "array")) {
      let mut items = Vec::new();
      step(array, &Segment::Index(0), &mut items, false);
      let items = self.expanded(items, None);
      return if items.iter().any(objectish) {
        Shape::ArrayOfTables
      } else {
        Shape::Array
      };
    }
    Shape::Value
  }
}

/// Indexes `$id`s and anchors under `schema`, which is at `pointer` in the
/// document at `document`.
fn index_ids(
  document: &str,
  base: &str,
  schema: &Json,
  pointer: String,
  ids: &mut HashMap<String, (String, String)>,
) {
  let Json::Object(map) = schema else {
    return;
  };
  let mut base = base.to_owned();
  let id = map
    .get("$id")
    .or_else(|| map.get("id"))
    .and_then(Json::as_str);
  if let Some(id) = id
    && let Some(joined) = join(&base, id)
  {
    if let Some((_, anchor)) = joined.split_once('#')
      && !anchor.is_empty()
    {
      // Draft 7's `"$id": "#name"` is an anchor.
      ids.insert(joined.clone(), (document.to_owned(), pointer.clone()));
    } else {
      base = without_fragment(&joined).to_owned();
      ids
        .entry(base.clone())
        .or_insert_with(|| (document.to_owned(), pointer.clone()));
    }
  }
  for keyword in ["$anchor", "$dynamicAnchor"] {
    if let Some(anchor) = map.get(keyword).and_then(Json::as_str) {
      ids.insert(
        format!("{}#{anchor}", without_fragment(&base)),
        (document.to_owned(), pointer.clone()),
      );
    }
  }
  let escape = |key: &str| key.replace('~', "~0").replace('/', "~1");
  for keyword in SCHEMA_KEYWORDS {
    if let Some(sub) = map.get(*keyword)
      && sub.is_object()
    {
      index_ids(document, &base, sub, format!("{pointer}/{keyword}"), ids);
    }
  }
  for keyword in SCHEMA_MAP_KEYWORDS {
    if let Some(Json::Object(subs)) = map.get(*keyword) {
      for (name, sub) in subs {
        index_ids(
          document,
          &base,
          sub,
          format!("{pointer}/{keyword}/{}", escape(name)),
          ids,
        );
      }
    }
  }
  for keyword in SCHEMA_ARRAY_KEYWORDS {
    if let Some(Json::Array(subs)) = map.get(*keyword) {
      for (index, sub) in subs.iter().enumerate() {
        index_ids(
          document,
          &base,
          sub,
          format!("{pointer}/{keyword}/{index}"),
          ids,
        );
      }
    }
  }
}

fn percent_decode(text: &str) -> String {
  if !text.contains('%') {
    return text.to_owned();
  }
  let bytes = text.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%'
      && let Some(byte) = text
        .get(i + 1..i + 3)
        .and_then(|hex| u8::from_str_radix(hex, 16).ok())
    {
      out.push(byte);
      i += 3;
      continue;
    }
    out.push(bytes[i]);
    i += 1;
  }
  String::from_utf8_lossy(&out).into_owned()
}

/// The subschemas a step into an object or array leads to.
fn step<'a>(sub: &Sub<'a>, segment: &Segment, out: &mut Vec<Sub<'a>>, into_arrays: bool) {
  let Json::Object(map) = sub.schema else {
    return;
  };
  let with = |schema: &'a Json| Sub {
    schema,
    base: sub.base.clone(),
  };
  match segment {
    Segment::Key(key) => {
      let mut matched = false;
      if let Some(property) = map.get("properties").and_then(|p| p.get(key)) {
        out.push(with(property));
        matched = true;
      }
      if let Some(Json::Object(patterns)) = map.get("patternProperties") {
        for (pattern, schema) in patterns {
          if Regex::new(pattern).is_ok_and(|regex| regex.is_match(key)) {
            out.push(with(schema));
            matched = true;
          }
        }
      }
      if !matched {
        for keyword in ["additionalProperties", "unevaluatedProperties"] {
          if let Some(schema) = map.get(keyword)
            && schema.is_object()
          {
            out.push(with(schema));
            matched = true;
            break;
          }
        }
      }
      if !matched && into_arrays && types(sub.schema).iter().any(|t| t == "array") {
        // A key under a TOML array of tables names its last table's.
        let mut items = Vec::new();
        step(sub, &Segment::Index(0), &mut items, false);
        for item in items {
          step(&item, segment, out, false);
        }
      }
    }
    Segment::Index(index) => {
      let tuple = map
        .get("prefixItems")
        .or_else(|| map.get("items").filter(|items| items.is_array()))
        .and_then(Json::as_array);
      if let Some(item) = tuple.and_then(|tuple| tuple.get(*index)) {
        out.push(with(item));
        return;
      }
      let rest = if tuple.is_some() {
        map
          .get("items")
          .filter(|items| items.is_object())
          .or_else(|| map.get("additionalItems"))
      } else {
        map.get("items")
      };
      if let Some(schema) = rest.filter(|schema| schema.is_object()) {
        out.push(with(schema));
      }
    }
  }
}

/// The value a subschema is for, and its document, when known.
type Here<'d> = Option<(&'d Node, &'d Instance<'d>)>;

/// Whether `node` is valid against `test`, for the simple schemas an `if`
/// usually is: `const`, `enum`, `type`, `required`, and `properties` of
/// those. `None` when it can't tell, such as for other keywords, or a
/// value still being typed.
fn holds(test: &Json, node: &Node, document: &Instance<'_>) -> Option<bool> {
  let map = match test {
    Json::Bool(value) => return Some(*value),
    Json::Object(map) => map,
    _ => return None,
  };
  let mut result = true;
  for (keyword, value) in map {
    let ok = match keyword.as_str() {
      "const" => document.scalar(node)? == *value,
      "enum" => value.as_array()?.contains(&document.scalar(node)?),
      "type" => {
        let actual: &[&str] = match &node.kind {
          NodeKind::Table(_) => &["object"],
          NodeKind::Array(_) => &["array"],
          NodeKind::Scalar => match document.scalar(node)? {
            Json::Null => &["null"],
            Json::Bool(_) => &["boolean"],
            Json::String(_) => &["string"],
            Json::Number(number) if number.as_f64().is_some_and(|n| n.fract() == 0.0) => {
              &["number", "integer"]
            }
            Json::Number(_) => &["number"],
            _ => return None,
          },
        };
        types(test)
          .iter()
          .any(|kind| actual.contains(&kind.as_str()))
      }
      // On values that aren't objects, these hold.
      "required" => match &node.kind {
        NodeKind::Table(_) => value
          .as_array()?
          .iter()
          .all(|key| key.as_str().is_some_and(|key| node.get(key).is_some())),
        _ => true,
      },
      "properties" => match &node.kind {
        NodeKind::Table(_) => {
          let mut all = true;
          for (key, schema) in value.as_object()? {
            if let Some(child) = node.get(key) {
              all &= holds(schema, child, document)?;
            }
          }
          all
        }
        _ => true,
      },
      "$comment" | "title" | "description" | "$id" => true,
      _ => return None,
    };
    result &= ok;
  }
  Some(result)
}

/// A schema's `type`, as a list.
fn types(schema: &Json) -> Vec<String> {
  match schema.get("type") {
    Some(Json::String(kind)) => vec![kind.clone()],
    Some(Json::Array(kinds)) => kinds
      .iter()
      .filter_map(Json::as_str)
      .map(str::to_owned)
      .collect(),
    _ => Vec::new(),
  }
}

fn description(schema: &Json) -> Option<String> {
  schema
    .get("markdownDescription")
    .or_else(|| schema.get("description"))
    .and_then(Json::as_str)
    .map(str::to_owned)
}

fn deprecation(schema: &Json) -> Option<String> {
  if let Some(message) = schema.get("deprecationMessage").and_then(Json::as_str) {
    return Some(message.to_owned());
  }
  (schema.get("deprecated") == Some(&Json::Bool(true))).then(String::new)
}

fn kind_label(subs: &[Sub<'_>]) -> Option<String> {
  let mut kinds: Vec<String> = Vec::new();
  for sub in subs {
    for kind in types(sub.schema) {
      let label = if kind == "array" {
        match sub.schema.get("items").map(types).as_deref() {
          Some([item]) => format!("array<{item}>"),
          _ => kind,
        }
      } else {
        kind
      };
      if !kinds.contains(&label) {
        kinds.push(label);
      }
    }
  }
  (!kinds.is_empty()).then(|| kinds.join(" | "))
}

impl Navigate for JsonSchema {
  fn properties(&self, path: &[Segment]) -> Vec<Property> {
    self.properties_at(path, None)
  }

  fn choices(&self, path: &[Segment]) -> Vec<Choice> {
    self.choices_at(path, None)
  }

  fn info(&self, path: &[Segment]) -> Option<Info> {
    self.info_at(path, None)
  }

  fn properties_in(&self, path: &[Segment], document: &Instance<'_>) -> Vec<Property> {
    self.properties_at(path, Some(document))
  }

  fn choices_in(&self, path: &[Segment], document: &Instance<'_>) -> Vec<Choice> {
    self.choices_at(path, Some(document))
  }

  fn info_in(&self, path: &[Segment], document: &Instance<'_>) -> Option<Info> {
    self.info_at(path, Some(document))
  }
}

impl JsonSchema {
  fn properties_at(&self, path: &[Segment], document: Option<&Instance<'_>>) -> Vec<Property> {
    let subs = self.at(path, document);
    let mut required: HashSet<&str> = HashSet::new();
    for sub in &subs {
      if let Some(Json::Array(names)) = sub.schema.get("required") {
        required.extend(names.iter().filter_map(Json::as_str));
      }
    }
    let mut list = Vec::new();
    for sub in &subs {
      let Some(Json::Object(properties)) = sub.schema.get("properties") else {
        continue;
      };
      for (name, schema) in properties {
        let property = Sub {
          schema,
          base: sub.base.clone(),
        };
        let expanded = self.expanded(vec![property.clone()], None);
        add_property(
          &mut list,
          Property {
            name: name.clone(),
            description: expanded
              .iter()
              .rev()
              .find_map(|sub| description(sub.schema)),
            kind: kind_label(&expanded),
            shape: self.shape(property),
            required: required.contains(name.as_str()),
            deprecated: expanded.iter().any(|sub| deprecation(sub.schema).is_some()),
          },
        );
      }
    }
    list
  }

  fn choices_at(&self, path: &[Segment], document: Option<&Instance<'_>>) -> Vec<Choice> {
    let mut list = Vec::new();
    for sub in self.at(path, document) {
      let schema = sub.schema;
      if let Some(Json::Array(values)) = schema.get("enum") {
        let descriptions = schema
          .get("markdownEnumDescriptions")
          .or_else(|| schema.get("enumDescriptions"))
          .and_then(Json::as_array);
        for (index, value) in values.iter().enumerate() {
          add_choice(
            &mut list,
            Choice {
              value: value.clone(),
              description: descriptions
                .and_then(|d| d.get(index))
                .and_then(Json::as_str)
                .map(str::to_owned),
              default: false,
            },
          );
        }
      }
      if let Some(value) = schema.get("const") {
        add_choice(
          &mut list,
          Choice {
            value: value.clone(),
            description: description(schema),
            default: false,
          },
        );
      }
      if types(schema).iter().any(|kind| kind == "boolean") {
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
      if let Some(default) = schema.get("default") {
        add_choice(
          &mut list,
          Choice {
            value: default.clone(),
            description: None,
            default: true,
          },
        );
      }
      if let Some(Json::Array(examples)) = schema.get("examples") {
        for example in examples.iter().take(20) {
          add_choice(
            &mut list,
            Choice {
              value: example.clone(),
              description: None,
              default: false,
            },
          );
        }
      }
    }
    list
  }

  fn info_at(&self, path: &[Segment], document: Option<&Instance<'_>>) -> Option<Info> {
    let subs = self.at(path, document);
    if subs.is_empty() {
      return None;
    }
    let mut info = Info {
      kind: kind_label(&subs),
      ..Info::default()
    };
    // The referencing schema's words win over the referenced one's, which
    // `expand` puts first.
    for sub in subs.iter().rev() {
      let schema = sub.schema;
      let mut constraints = Vec::new();
      let number = |key: &str| {
        schema
          .get(key)
          .filter(|v| v.is_number())
          .map(Json::to_string)
      };
      match (number("minimum"), number("maximum")) {
        (Some(min), Some(max)) => constraints.push(format!("From {min} to {max}")),
        (Some(min), None) => constraints.push(format!("At least {min}")),
        (None, Some(max)) => constraints.push(format!("At most {max}")),
        (None, None) => {}
      }
      if let Some(min) = number("exclusiveMinimum") {
        constraints.push(format!("Greater than {min}"));
      }
      if let Some(max) = number("exclusiveMaximum") {
        constraints.push(format!("Less than {max}"));
      }
      for (min, max, what) in [
        ("minLength", "maxLength", "Length"),
        ("minItems", "maxItems", "Items"),
        ("minProperties", "maxProperties", "Properties"),
      ] {
        match (number(min), number(max)) {
          (Some(min), Some(max)) => constraints.push(format!("{what} {min} to {max}")),
          (Some(min), None) => constraints.push(format!("{what} at least {min}")),
          (None, Some(max)) => constraints.push(format!("{what} at most {max}")),
          (None, None) => {}
        }
      }
      if let Some(pattern) = schema.get("pattern").and_then(Json::as_str) {
        constraints.push(format!("Matches `{pattern}`"));
      }
      if let Some(format) = schema.get("format").and_then(Json::as_str) {
        constraints.push(format!("Format: {format}"));
      }
      if schema.get("uniqueItems") == Some(&Json::Bool(true)) {
        constraints.push("Items are unique".to_owned());
      }
      let mut allowed = Vec::new();
      if let Some(Json::Array(values)) = schema.get("enum") {
        allowed.clone_from(values);
      }
      if let Some(value) = schema.get("const") {
        allowed.push(value.clone());
      }
      info.merge(Info {
        title: schema
          .get("title")
          .and_then(Json::as_str)
          .map(str::to_owned),
        description: description(schema),
        kind: None,
        default: schema.get("default").cloned(),
        allowed,
        deprecated: deprecation(schema),
        constraints,
      });
    }
    (!info.is_empty()).then_some(info)
  }
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  fn schema() -> JsonSchema {
    let schema = json!({
      "$schema": "https://json-schema.org/draft/2020-12/schema",
      "type": "object",
      "properties": {
        "name": { "type": "string", "description": "The name.", "minLength": 1 },
        "kind": { "enum": ["app", "lib"], "enumDescriptions": ["An app.", "A library."] },
        "server": { "$ref": "#/$defs/server" },
        "remote": { "$ref": "https://example.com/remote.json#/$defs/r" },
        "old": { "type": "boolean", "deprecationMessage": "Use new." },
        "plugins": { "type": "array", "items": { "$ref": "#/$defs/plugin" } }
      },
      "required": ["name"],
      "additionalProperties": false,
      "$defs": {
        "server": {
          "type": "object",
          "properties": { "port": { "type": "integer", "minimum": 1, "default": 80 } }
        },
        "plugin": {
          "type": "object",
          "properties": { "id": { "type": "string" } }
        }
      }
    });
    let remote = json!({
      "$defs": { "r": { "type": "object", "properties": { "url": { "type": "string", "format": "uri" } } } }
    });
    JsonSchema::new(
      "file:///schemas/app.json",
      schema,
      HashMap::from([("https://example.com/remote.json".to_owned(), remote)]),
    )
  }

  #[test]
  fn references_to_other_documents_are_found() {
    let schema = json!({
      "$id": "https://example.com/root.json",
      "properties": {
        "a": { "$ref": "other.json#/x" },
        "b": { "$ref": "#/local" },
        "c": { "$ref": "https://example.com/root.json#/y" },
        "d": { "$id": "inner.json", "properties": { "e": { "$ref": "deeper.json" } } }
      }
    });
    assert_eq!(
      external_references("file:///root.json", &schema),
      [
        "https://example.com/deeper.json",
        "https://example.com/other.json"
      ]
    );
  }

  #[test]
  fn validation_reports_paths() {
    let schema = schema();
    assert_eq!(schema.error(), None);
    let problems = schema.validate(&json!({
      "kind": "nope",
      "server": { "port": 0 },
      "extra": 1,
      "remote": { "url": 5 }
    }));
    let paths: Vec<(Vec<Segment>, bool)> = problems
      .iter()
      .map(|problem| (problem.path.clone(), problem.key))
      .collect();
    assert!(
      paths.contains(&(vec![], true)),
      "missing name: {problems:?}"
    );
    assert!(
      paths.contains(&(vec![Segment::key("extra")], true)),
      "{problems:?}"
    );
    assert!(
      paths.contains(&(vec![Segment::key("kind")], false)),
      "{problems:?}"
    );
    assert!(
      paths.contains(&(vec![Segment::key("server"), Segment::key("port")], false)),
      "{problems:?}"
    );
    assert!(
      paths.contains(&(vec![Segment::key("remote"), Segment::key("url")], false)),
      "{problems:?}"
    );
  }

  #[test]
  fn schemas_that_cant_load_say_why() {
    let schema = JsonSchema::new(
      "file:///a.json",
      json!({ "properties": { "a": { "$ref": "missing.json" } } }),
      HashMap::new(),
    );
    assert!(schema.error().is_some());
  }

  #[test]
  fn navigation_follows_references() {
    let schema = schema();
    let names: Vec<String> = schema
      .properties(&[])
      .into_iter()
      .map(|property| property.name)
      .collect();
    // serde_json keeps keys sorted.
    assert_eq!(
      names,
      ["kind", "name", "old", "plugins", "remote", "server"]
    );
    let root = schema.properties(&[]);
    assert!(root[1].required);
    assert_eq!(root[5].shape, Shape::Table);
    assert_eq!(root[3].shape, Shape::ArrayOfTables);
    assert!(root[2].deprecated);
    let port = schema.properties(&[Segment::key("server")]);
    assert_eq!(port[0].name, "port");
    let url = schema.properties(&[Segment::key("remote")]);
    assert_eq!(url[0].name, "url");
    let ids = schema.properties(&[Segment::key("plugins"), Segment::Index(2)]);
    assert_eq!(ids[0].name, "id");

    let kinds = schema.choices(&[Segment::key("kind")]);
    assert_eq!(kinds.len(), 2);
    assert_eq!(kinds[1].description.as_deref(), Some("A library."));
    let port = schema
      .info(&[Segment::key("server"), Segment::key("port")])
      .expect("port");
    assert_eq!(port.kind.as_deref(), Some("integer"));
    assert_eq!(port.default, Some(json!(80)));
    assert_eq!(port.constraints, ["At least 1"]);
    let old = schema.info(&[Segment::key("old")]).expect("old");
    assert_eq!(old.deprecated.as_deref(), Some("Use new."));
  }
}
