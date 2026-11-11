//! JSON Schemas written in YAML, as the YAML language server takes them:
//! the value of a YAML stream of one document, by YAML 1.2's core schema
//! (`yes` is a string, `0o17` an integer), with aliases expanded. YAML
//! documents themselves are the YAML extension's to read.

use std::collections::HashMap;

use saphyr_parser::{Event, Parser, ScalarStyle, Tag};
use serde_json::{Map, Number, Value as Json};

/// Most values aliases may expand to, in all: aliases of aliases grow
/// exponentially ("billion laughs").
const MAX_EXPANDED: usize = 100_000;

/// Most nested mappings and sequences.
const MAX_DEPTH: usize = 512;

/// A mapping or sequence being read.
enum Frame {
  Mapping {
    anchor: usize,
    map: Map<String, Json>,
    key: Option<String>,
  },
  Sequence {
    anchor: usize,
    items: Vec<Json>,
  },
}

/// The value of a YAML stream of exactly one document, or `None` if it has
/// errors or more or fewer documents.
pub fn value(text: &str) -> Option<Json> {
  let mut frames: Vec<Frame> = Vec::new();
  let mut anchors: HashMap<usize, Json> = HashMap::new();
  let mut expanded = 0usize;
  let mut documents: Vec<Json> = Vec::new();
  for event in Parser::new_from_str(text) {
    let (event, _) = event.ok()?;
    let (value, anchor) = match event {
      Event::StreamStart | Event::StreamEnd | Event::Nothing | Event::DocumentStart(_) => continue,
      Event::DocumentEnd => continue,
      Event::Alias(anchor) => {
        let value = anchors.get(&anchor)?.clone();
        expanded += size(&value);
        if expanded > MAX_EXPANDED {
          return None;
        }
        (value, 0)
      }
      Event::Scalar(raw, style, anchor, tag) => (resolve(&raw, style, tag.as_deref()), anchor),
      Event::MappingStart(anchor, _) => {
        if frames.len() >= MAX_DEPTH {
          return None;
        }
        frames.push(Frame::Mapping {
          anchor,
          map: Map::new(),
          key: None,
        });
        continue;
      }
      Event::SequenceStart(anchor, _) => {
        if frames.len() >= MAX_DEPTH {
          return None;
        }
        frames.push(Frame::Sequence {
          anchor,
          items: Vec::new(),
        });
        continue;
      }
      Event::MappingEnd | Event::SequenceEnd => match frames.pop()? {
        Frame::Mapping { anchor, map, .. } => (Json::Object(map), anchor),
        Frame::Sequence { anchor, items } => (Json::Array(items), anchor),
      },
    };
    if anchor != 0 {
      anchors.insert(anchor, value.clone());
    }
    match frames.last_mut() {
      None => documents.push(value),
      Some(Frame::Sequence { items, .. }) => items.push(value),
      Some(Frame::Mapping { map, key, .. }) => match key.take() {
        None => {
          *key = Some(match value {
            Json::String(s) => s,
            other => other.to_string(),
          })
        }
        Some(name) => {
          map.insert(name, value);
        }
      },
    }
  }
  (documents.len() == 1).then(|| documents.pop()).flatten()
}

/// How many values a value holds, itself included.
fn size(value: &Json) -> usize {
  match value {
    Json::Array(items) => 1 + items.iter().map(size).sum::<usize>(),
    Json::Object(map) => 1 + map.values().map(size).sum::<usize>(),
    _ => 1,
  }
}

/// A scalar's value: by its tag if YAML's own, else by YAML 1.2's core
/// schema if it's plain (unquoted), else a string. Other tags (`!Ref`)
/// don't change what a value is.
fn resolve(raw: &str, style: ScalarStyle, tag: Option<&Tag>) -> Json {
  if let Some(tag) = tag {
    if tag.is_yaml_core_schema() {
      let resolved = match tag.suffix.as_str() {
        "null" => Some(Json::Null),
        "bool" => bool_value(raw).map(Json::Bool),
        "int" => int_value(raw),
        "float" => float_value(raw).or_else(|| int_value(raw)),
        _ => None,
      };
      return resolved.unwrap_or_else(|| Json::String(raw.to_owned()));
    }
    // The non-specific tag: a string.
    if tag.handle == "!" && tag.suffix.is_empty() {
      return Json::String(raw.to_owned());
    }
  }
  if style != ScalarStyle::Plain {
    return Json::String(raw.to_owned());
  }
  plain(raw)
}

/// A plain scalar's value, by YAML 1.2's core schema.
fn plain(raw: &str) -> Json {
  if matches!(raw, "" | "~" | "null" | "Null" | "NULL") {
    return Json::Null;
  }
  if let Some(value) = bool_value(raw) {
    return Json::Bool(value);
  }
  int_value(raw)
    .or_else(|| float_value(raw))
    .unwrap_or_else(|| Json::String(raw.to_owned()))
}

fn bool_value(raw: &str) -> Option<bool> {
  match raw {
    "true" | "True" | "TRUE" => Some(true),
    "false" | "False" | "FALSE" => Some(false),
    _ => None,
  }
}

/// `[-+]?[0-9]+`, `0o[0-7]+`, or `0x[0-9a-fA-F]+`.
fn int_value(raw: &str) -> Option<Json> {
  let radix = |digits: &str, radix: u32| {
    (!digits.is_empty())
      .then(|| i64::from_str_radix(digits, radix).ok())
      .flatten()
      .map(|value| Json::Number(value.into()))
  };
  if let Some(octal) = raw.strip_prefix("0o") {
    return radix(octal, 8);
  }
  if let Some(hex) = raw.strip_prefix("0x") {
    return radix(hex, 16);
  }
  let digits = raw.strip_prefix(['-', '+']).unwrap_or(raw);
  if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  let raw = raw.strip_prefix('+').unwrap_or(raw);
  if let Ok(value) = raw.parse::<i64>() {
    return Some(Json::Number(value.into()));
  }
  if let Ok(value) = raw.parse::<u64>() {
    return Some(Json::Number(value.into()));
  }
  raw
    .parse::<f64>()
    .ok()
    .and_then(Number::from_f64)
    .map(Json::Number)
}

/// `[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?`, or infinity or
/// not a number, which JSON can't hold: those stay as written.
fn float_value(raw: &str) -> Option<Json> {
  let unsigned = raw.strip_prefix(['-', '+']).unwrap_or(raw);
  if matches!(unsigned, ".inf" | ".Inf" | ".INF") || matches!(raw, ".nan" | ".NaN" | ".NAN") {
    return Some(Json::String(raw.to_owned()));
  }
  let bytes = unsigned.as_bytes();
  let mut i = 0;
  let digits = |i: &mut usize| {
    let start = *i;
    while bytes.get(*i).is_some_and(u8::is_ascii_digit) {
      *i += 1;
    }
    *i > start
  };
  let whole = digits(&mut i);
  if bytes.get(i) == Some(&b'.') {
    i += 1;
    let fraction = digits(&mut i);
    if !whole && !fraction {
      return None;
    }
  } else if !whole {
    return None;
  }
  if matches!(bytes.get(i), Some(b'e' | b'E')) {
    i += 1;
    if matches!(bytes.get(i), Some(b'-' | b'+')) {
      i += 1;
    }
    if !digits(&mut i) {
      return None;
    }
  }
  if i != bytes.len() {
    return None;
  }
  let raw = raw.strip_prefix('+').unwrap_or(raw);
  raw
    .parse::<f64>()
    .ok()
    .and_then(Number::from_f64)
    .map(Json::Number)
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  #[test]
  fn scalars_resolve_by_the_core_schema() {
    assert_eq!(
      value(
        "a: 1\nb: -0x1F\nc: 0o17\nd: 1.5e3\ne: true\nf: ~\ng: yes\nh: '1'\ni: !!str 2\nj: !!int \"3\"\nk: .inf\nl:\nm: !Ref x\nn: 0x1F\no: +12\np: .5\n"
      ),
      Some(json!({
        "a": 1, "b": "-0x1F", "c": 15, "d": 1500.0, "e": true, "f": null, "g": "yes",
        "h": "1", "i": "2", "j": 3, "k": ".inf", "l": null, "m": "x", "n": 31, "o": 12,
        "p": 0.5
      }))
    );
  }

  #[test]
  fn aliases_expand_within_limits() {
    assert_eq!(
      value("a: &x [1, 2]\nb: *x\n"),
      Some(json!({ "a": [1, 2], "b": [1, 2] }))
    );
    let mut text = String::from("a: &a [1, 1, 1, 1, 1, 1, 1, 1, 1, 1]\n");
    let mut previous = 'a';
    for name in 'b'..='k' {
      let alias = format!("*{previous}");
      let items = [alias.as_str(); 10].join(", ");
      text.push_str(&format!("{name}: &{name} [{items}]\n"));
      previous = name;
    }
    assert_eq!(value(&text), None);
  }

  #[test]
  fn only_streams_of_one_document_have_a_value() {
    assert_eq!(value("a: 1\n---\nb: 2\n"), None);
    assert_eq!(value("# nothing\n"), None);
    assert_eq!(value("a: [1\n"), None);
  }
}
