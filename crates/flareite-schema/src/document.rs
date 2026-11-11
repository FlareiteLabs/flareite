//! Documents as Flareite's schemas see them: the trees of values
//! extensions parse their languages' documents into
//! ([`flareite_schema_document`]), and converting them to the values each
//! kind of schema checks.

use serde_json::{Number, Value as Json};

pub use flareite_schema_document::{
  Document, Node, NodeKind, Part, Problem, Scalar, Segment, Severity, Span,
};

/// Converts TOML values to JSON for JSON Schemas: dates and times become
/// strings, as RFC 3339 writes them.
pub fn toml_to_json(value: &toml::Value) -> Json {
  match value {
    toml::Value::String(s) => Json::String(s.clone()),
    toml::Value::Integer(i) => Json::Number((*i).into()),
    toml::Value::Float(f) => Number::from_f64(*f).map_or(Json::Null, Json::Number),
    toml::Value::Boolean(b) => Json::Bool(*b),
    toml::Value::Datetime(d) => Json::String(d.to_string()),
    toml::Value::Array(items) => Json::Array(items.iter().map(toml_to_json).collect()),
    toml::Value::Table(table) => Json::Object(
      table
        .iter()
        .map(|(key, value)| (key.clone(), toml_to_json(value)))
        .collect(),
    ),
  }
}

/// A TOML value as a tree, written nowhere (every span is empty): for
/// checking values that aren't in a document, such as settings.
pub fn toml_tree(value: &toml::Value) -> Node {
  let scalar = |value| Node::scalar(0..0, Some(value));
  match value {
    toml::Value::String(s) => scalar(Scalar::String(s.clone())),
    toml::Value::Integer(i) => scalar(Scalar::Integer(*i)),
    toml::Value::Float(f) => scalar(Scalar::Float(*f)),
    toml::Value::Boolean(b) => scalar(Scalar::Bool(*b)),
    toml::Value::Datetime(d) => scalar(Scalar::Datetime(d.to_string())),
    toml::Value::Array(items) => Node::array(0..0, items.iter().map(toml_tree).collect()),
    toml::Value::Table(table) => Node::table(
      0..0,
      table
        .iter()
        .map(|(key, value)| (key.clone(), toml_tree(value)))
        .collect(),
    ),
  }
}

/// A tree as TOML values, for TOML Schemas. Fails with where a value TOML
/// can't hold is: null, or a scalar not yet known.
pub fn to_toml(node: &Node) -> Result<toml::Value, Span> {
  Ok(match &node.kind {
    NodeKind::Scalar(value) => match value.as_ref().ok_or_else(|| node.span.clone())? {
      Scalar::Null => return Err(node.span.clone()),
      Scalar::Bool(b) => toml::Value::Boolean(*b),
      Scalar::Integer(i) => toml::Value::Integer(*i),
      Scalar::Float(f) => toml::Value::Float(*f),
      Scalar::String(s) => toml::Value::String(s.clone()),
      Scalar::Datetime(d) => d
        .parse::<toml::value::Datetime>()
        .map_or_else(|_| toml::Value::String(d.clone()), toml::Value::Datetime),
    },
    NodeKind::Array(items) => {
      toml::Value::Array(items.iter().map(to_toml).collect::<Result<_, _>>()?)
    }
    NodeKind::Table(entries) => {
      let mut table = toml::Table::new();
      for (key, child) in entries {
        table.insert(key.clone(), to_toml(child)?);
      }
      toml::Value::Table(table)
    }
  })
}

/// Parses a TOML Schema instance path (`$.a."b.c"[0]`).
pub fn parse_instance_path(path: &str) -> Vec<Segment> {
  let mut segments = Vec::new();
  let rest = path.strip_prefix('$').unwrap_or(path);
  let mut chars = rest.char_indices().peekable();
  while let Some((_, c)) = chars.next() {
    match c {
      '.' => {
        if chars.peek().is_some_and(|&(_, c)| c == '"') {
          chars.next();
          let mut key = String::new();
          while let Some((_, c)) = chars.next() {
            match c {
              '"' => break,
              '\\' => match chars.next() {
                Some((_, 'u')) => {
                  let hex: String = (0..4)
                    .filter_map(|_| chars.next().map(|(_, c)| c))
                    .collect();
                  if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    key.push(c);
                  }
                }
                Some((_, 'n')) => key.push('\n'),
                Some((_, 't')) => key.push('\t'),
                Some((_, 'r')) => key.push('\r'),
                Some((_, 'b')) => key.push('\u{8}'),
                Some((_, 'f')) => key.push('\u{c}'),
                Some((_, other)) => key.push(other),
                None => {}
              },
              other => key.push(other),
            }
          }
          segments.push(Segment::Key(key));
        } else {
          let mut key = String::new();
          while let Some(&(_, c)) = chars.peek() {
            if c == '.' || c == '[' {
              break;
            }
            key.push(c);
            chars.next();
          }
          segments.push(Segment::Key(key));
        }
      }
      '[' => {
        let mut digits = String::new();
        for (_, c) in chars.by_ref() {
          if c == ']' {
            break;
          }
          digits.push(c);
        }
        if let Ok(index) = digits.parse() {
          segments.push(Segment::Index(index));
        }
      }
      _ => {}
    }
  }
  segments
}

/// Parses a JSON pointer (`/a/0`). Numbers are indexes; [`Node::find`]
/// reads them as keys in objects.
pub fn parse_json_pointer(pointer: &str) -> Vec<Segment> {
  if pointer.is_empty() {
    return Vec::new();
  }
  pointer
    .trim_start_matches('/')
    .split('/')
    .map(|part| {
      let part = part.replace("~1", "/").replace("~0", "~");
      match part.parse::<usize>() {
        Ok(index) if !part.starts_with('+') && (part == "0" || !part.starts_with('0')) => {
          Segment::Index(index)
        }
        _ => Segment::Key(part),
      }
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn trees_convert_to_toml() {
    let value: toml::Value =
      toml::from_str("a = 1\nb = 1979-05-27\n[t]\nc = [\"x\", 2.5]\n").expect("toml");
    let tree = toml_tree(&value);
    assert_eq!(to_toml(&tree), Ok(value));
    let null = Node::table(
      0..9,
      vec![("a".into(), Node::scalar(4..8, Some(Scalar::Null)))],
    );
    assert_eq!(to_toml(&null), Err(4..8));
  }

  #[test]
  fn paths_parse() {
    assert_eq!(
      parse_instance_path("$.site.\"google.com\"[2].x"),
      vec![
        Segment::key("site"),
        Segment::key("google.com"),
        Segment::Index(2),
        Segment::key("x")
      ]
    );
    assert_eq!(parse_instance_path("$"), Vec::<Segment>::new());
    assert_eq!(
      parse_json_pointer("/a~1b/01/3"),
      vec![Segment::key("a/b"), Segment::key("01"), Segment::Index(3)]
    );
  }
}
