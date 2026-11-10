//! Documents as trees of values with where each is in the text, so problems
//! found in a value can be marked on it: TOML, with the `toml` crate's
//! spans, and JSON (with comments and trailing commas, as JSONC allows),
//! with a parser of its own. YAML's are made in [`crate::yaml`].

use std::ops::Range;

use serde_json::{Map, Number, Value as Json};
use toml::de::{DeTable, DeValue};

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

/// A value in a document.
#[derive(Debug, Clone)]
pub struct Node {
  /// The whole value, as written.
  pub span: Span,
  /// The key naming it in its table (in a TOML header, the header's last
  /// key), if it has one.
  pub key_span: Option<Span>,
  pub kind: NodeKind,
  /// A scalar's value, when its text alone doesn't say it (YAML's, which
  /// depend on their tags and indentation).
  pub value: Option<Json>,
}

#[derive(Debug, Clone)]
pub enum NodeKind {
  /// A table or object, with its entries in the order written.
  Table(Vec<(String, Node)>),
  Array(Vec<Node>),
  Scalar,
}

impl Node {
  pub(crate) fn scalar(span: Span) -> Self {
    Self {
      span,
      key_span: None,
      kind: NodeKind::Scalar,
      value: None,
    }
  }

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

  /// Where to mark a problem with this value: its key if it has one (a
  /// table's whole span may be many lines), else the value.
  pub fn mark(&self) -> Span {
    match (&self.kind, &self.key_span) {
      (NodeKind::Table(_) | NodeKind::Array(_), Some(key)) => key.clone(),
      _ => self.span.clone(),
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
        NodeKind::Scalar => {}
      }
    }
    walk(self, &mut Vec::new(), f);
  }
}

/// A syntax error.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxError {
  pub span: Span,
  pub message: String,
}

/// A parsed TOML document: its values for validation, and its tree.
pub struct TomlDocument {
  /// `None` when the text has syntax errors.
  pub table: Option<toml::Table>,
  pub tree: Node,
  pub errors: Vec<SyntaxError>,
}

pub fn parse_toml(text: &str) -> TomlDocument {
  let (parsed, errors) = DeTable::parse_recoverable(text);
  let tree = toml_node(
    &DeValue::Table(parsed.get_ref().clone()),
    0..text.len(),
    text,
  );
  let errors: Vec<SyntaxError> = errors
    .into_iter()
    .map(|error| {
      let span = error.span().unwrap_or(0..0);
      SyntaxError {
        span: widen(text, span),
        message: error.message().trim().to_owned(),
      }
    })
    .collect();
  let table = if errors.is_empty() {
    text.parse::<toml::Table>().ok()
  } else {
    None
  };
  TomlDocument {
    table,
    tree,
    errors,
  }
}

/// Makes an empty span cover the character there, so it can be seen.
pub(crate) fn widen(text: &str, span: Span) -> Span {
  if !span.is_empty() {
    return span;
  }
  let start = span.start.min(text.len());
  let end = text[start..].chars().next().map_or(start, |c| {
    if c == '\n' {
      start
    } else {
      start + c.len_utf8()
    }
  });
  start..end
}

fn toml_node(value: &DeValue<'_>, span: Span, text: &str) -> Node {
  match value {
    DeValue::Table(table) => Node {
      span,
      key_span: None,
      kind: NodeKind::Table(toml_entries(table, text)),
      value: None,
    },
    DeValue::Array(items) => Node {
      span,
      key_span: None,
      kind: NodeKind::Array(
        items
          .iter()
          .map(|item| toml_node(item.get_ref(), item.span(), text))
          .collect(),
      ),
      value: None,
    },
    _ => Node::scalar(span),
  }
}

fn toml_entries(table: &DeTable<'_>, text: &str) -> Vec<(String, Node)> {
  table
    .iter()
    .map(|(key, value)| {
      let mut node = toml_node(value.get_ref(), value.span(), text);
      node.key_span = Some(key.span());
      (key.get_ref().to_string(), node)
    })
    .collect()
}

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

/// A parsed JSON document.
pub struct JsonDocument {
  /// `None` when the text has syntax errors.
  pub value: Option<Json>,
  pub tree: Node,
  pub errors: Vec<SyntaxError>,
  /// Keys given twice in an object: the later one is used.
  pub duplicates: Vec<Span>,
}

/// Most nested arrays and objects.
const MAX_DEPTH: usize = 512;

/// Parses JSON, allowing comments and trailing commas (JSONC).
pub fn parse_json(text: &str) -> JsonDocument {
  let mut parser = JsonParser {
    text,
    bytes: text.as_bytes(),
    at: 0,
    errors: Vec::new(),
    duplicates: Vec::new(),
    depth: 0,
  };
  parser.skip_trivia();
  let (value, tree) = if parser.at >= parser.bytes.len() {
    parser.error(0..0, "Expected a value; the document is empty");
    (Json::Null, Node::scalar(0..0))
  } else {
    let parsed = parser.value();
    parser.skip_trivia();
    if parser.at < parser.bytes.len() {
      let at = parser.at;
      parser.error(at..at, "Expected the end of the document");
    }
    parsed
  };
  let failed = !parser.errors.is_empty();
  JsonDocument {
    value: (!failed).then_some(value),
    tree,
    errors: parser.errors,
    duplicates: parser.duplicates,
  }
}

struct JsonParser<'a> {
  text: &'a str,
  bytes: &'a [u8],
  at: usize,
  errors: Vec<SyntaxError>,
  duplicates: Vec<Span>,
  depth: usize,
}

impl JsonParser<'_> {
  fn error(&mut self, span: Span, message: &str) {
    // One error per place: recovery can stumble over the same spot.
    if self
      .errors
      .last()
      .is_some_and(|last| last.span.start == span.start)
    {
      return;
    }
    self.errors.push(SyntaxError {
      span: widen(self.text, span),
      message: message.to_owned(),
    });
  }

  fn peek(&self) -> Option<u8> {
    self.bytes.get(self.at).copied()
  }

  fn skip_trivia(&mut self) {
    loop {
      match self.peek() {
        Some(b' ' | b'\t' | b'\n' | b'\r') => self.at += 1,
        Some(b'/') if self.bytes.get(self.at + 1) == Some(&b'/') => {
          while self.peek().is_some_and(|b| b != b'\n') {
            self.at += 1;
          }
        }
        Some(b'/') if self.bytes.get(self.at + 1) == Some(&b'*') => {
          let start = self.at;
          match self.text[self.at + 2..].find("*/") {
            Some(end) => self.at += 2 + end + 2,
            None => {
              self.at = self.bytes.len();
              self.error(start..start + 2, "Unterminated comment");
            }
          }
        }
        // A byte order mark at the start.
        Some(0xEF) if self.at == 0 && self.text.starts_with('\u{feff}') => self.at += 3,
        _ => return,
      }
    }
  }

  fn value(&mut self) -> (Json, Node) {
    let start = self.at;
    match self.peek() {
      Some(b'{') => self.object(),
      Some(b'[') => self.array(),
      Some(b'"') => {
        let string = self.string();
        (Json::String(string), Node::scalar(start..self.at))
      }
      Some(b'-' | b'0'..=b'9') => self.number(),
      Some(b't') if self.text[self.at..].starts_with("true") => {
        self.at += 4;
        (Json::Bool(true), Node::scalar(start..self.at))
      }
      Some(b'f') if self.text[self.at..].starts_with("false") => {
        self.at += 5;
        (Json::Bool(false), Node::scalar(start..self.at))
      }
      Some(b'n') if self.text[self.at..].starts_with("null") => {
        self.at += 4;
        (Json::Null, Node::scalar(start..self.at))
      }
      _ => {
        // Skip the word that isn't a value.
        while self
          .peek()
          .is_some_and(|b| !matches!(b, b',' | b'}' | b']' | b':' | b' ' | b'\t' | b'\r' | b'\n'))
        {
          self.at += 1;
        }
        if self.at == start {
          self.error(start..start, "Expected a value");
        } else {
          self.error(start..self.at, "Expected a value");
        }
        // Keep `at` in a char boundary.
        while !self.text.is_char_boundary(self.at) {
          self.at += 1;
        }
        (Json::Null, Node::scalar(start..self.at))
      }
    }
  }

  fn nested(&mut self) -> bool {
    if self.depth >= MAX_DEPTH {
      let at = self.at;
      self.error(at..at + 1, "Too deeply nested");
      self.at = self.bytes.len();
      return false;
    }
    true
  }

  fn object(&mut self) -> (Json, Node) {
    let start = self.at;
    self.at += 1;
    let mut map = Map::new();
    let mut entries: Vec<(String, Node)> = Vec::new();
    if !self.nested() {
      return (Json::Object(map), Node::scalar(start..self.at));
    }
    self.depth += 1;
    loop {
      self.skip_trivia();
      match self.peek() {
        Some(b'}') => {
          self.at += 1;
          break;
        }
        None => {
          self.error(start..start + 1, "Unclosed object: expected '}'");
          break;
        }
        Some(b'"') => {
          let key_start = self.at;
          let key = self.string();
          let key_span = key_start..self.at;
          self.skip_trivia();
          if self.peek() == Some(b':') {
            self.at += 1;
          } else {
            let at = self.at;
            self.error(at..at, "Expected ':'");
          }
          self.skip_trivia();
          let (value, mut node) = if matches!(self.peek(), Some(b',' | b'}') | None) {
            let at = self.at;
            self.error(at..at, "Expected a value");
            (Json::Null, Node::scalar(at..at))
          } else {
            self.value()
          };
          node.key_span = Some(key_span.clone());
          if let Some(existing) = entries.iter().position(|(name, _)| *name == key) {
            self.duplicates.push(key_span);
            entries.remove(existing);
          }
          map.insert(key.clone(), value);
          entries.push((key, node));
          self.skip_trivia();
          match self.peek() {
            Some(b',') => self.at += 1,
            Some(b'}') | None => {}
            // A missing comma: read on.
            Some(b'"') => {
              let at = self.at;
              self.error(at..at, "Expected ',' or '}'");
            }
            _ => {
              let at = self.at;
              self.error(at..at, "Expected ',' or '}'");
              self.recover();
            }
          }
        }
        Some(_) => {
          let at = self.at;
          self.error(at..at, "Expected a property name in double quotes");
          self.recover();
          if self.peek() == Some(b',') {
            self.at += 1;
          }
        }
      }
    }
    self.depth -= 1;
    (
      Json::Object(map),
      Node {
        span: start..self.at,
        key_span: None,
        kind: NodeKind::Table(entries),
        value: None,
      },
    )
  }

  /// Skips to the next ',' or closing bracket on this level.
  fn recover(&mut self) {
    while let Some(b) = self.peek() {
      match b {
        b',' | b'}' | b']' => return,
        b'"' => {
          self.string();
        }
        b'{' | b'[' => {
          self.value();
        }
        _ => {
          self.at += 1;
          while !self.text.is_char_boundary(self.at) {
            self.at += 1;
          }
        }
      }
    }
  }

  fn array(&mut self) -> (Json, Node) {
    let start = self.at;
    self.at += 1;
    let mut values = Vec::new();
    let mut nodes = Vec::new();
    if !self.nested() {
      return (Json::Array(values), Node::scalar(start..self.at));
    }
    self.depth += 1;
    loop {
      self.skip_trivia();
      match self.peek() {
        Some(b']') => {
          self.at += 1;
          break;
        }
        None => {
          self.error(start..start + 1, "Unclosed array: expected ']'");
          break;
        }
        Some(_) => {
          let (value, node) = self.value();
          values.push(value);
          nodes.push(node);
          self.skip_trivia();
          match self.peek() {
            Some(b',') => self.at += 1,
            Some(b']') | None => {}
            Some(b'"' | b'{' | b'[' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n') => {
              let at = self.at;
              self.error(at..at, "Expected ',' or ']'");
            }
            _ => {
              let at = self.at;
              self.error(at..at, "Expected ',' or ']'");
              self.recover();
              if self.peek() == Some(b'}') {
                // A stray brace: leave the array.
                self.at += 1;
              }
            }
          }
        }
      }
    }
    self.depth -= 1;
    (
      Json::Array(values),
      Node {
        span: start..self.at,
        key_span: None,
        kind: NodeKind::Array(nodes),
        value: None,
      },
    )
  }

  /// Reads a string from its opening quote; returns its value.
  fn string(&mut self) -> String {
    let start = self.at;
    self.at += 1;
    let mut value = String::new();
    while let Some(rest) = self.text.get(self.at..) {
      let Some(c) = rest.chars().next() else {
        self.error(start..start + 1, "Unterminated string");
        return value;
      };
      match c {
        '"' => {
          self.at += 1;
          return value;
        }
        '\n' => {
          self.error(start..self.at, "Unterminated string");
          return value;
        }
        '\\' => {
          let escape_start = self.at;
          self.at += 1;
          match self.peek() {
            Some(b'"') => value.push('"'),
            Some(b'\\') => value.push('\\'),
            Some(b'/') => value.push('/'),
            Some(b'b') => value.push('\u{8}'),
            Some(b'f') => value.push('\u{c}'),
            Some(b'n') => value.push('\n'),
            Some(b'r') => value.push('\r'),
            Some(b't') => value.push('\t'),
            Some(b'u') => {
              let unit = |at: usize| {
                self
                  .text
                  .get(at..at + 4)
                  .and_then(|hex| u16::from_str_radix(hex, 16).ok())
              };
              match unit(self.at + 1) {
                Some(high) if (0xD800..0xDC00).contains(&high) => {
                  let low = (self.text.get(self.at + 5..self.at + 7) == Some("\\u"))
                    .then(|| unit(self.at + 7))
                    .flatten()
                    .filter(|low| (0xDC00..0xE000).contains(low));
                  match low {
                    Some(low) => {
                      let code =
                        0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                      value.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                      self.at += 6;
                    }
                    None => value.push('\u{fffd}'),
                  }
                  self.at += 4;
                }
                Some(unit) => {
                  value.push(char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'));
                  self.at += 4;
                }
                None => self.error(escape_start..self.at + 1, "Invalid unicode escape"),
              }
            }
            _ => self.error(escape_start..self.at + 1, "Invalid escape"),
          }
          if self.at < self.bytes.len() {
            self.at += 1;
          }
          while !self.text.is_char_boundary(self.at) {
            self.at += 1;
          }
        }
        c if u32::from(c) < 0x20 => {
          let at = self.at;
          self.error(at..at + 1, "Control characters must be escaped");
          self.at += 1;
        }
        c => {
          value.push(c);
          self.at += c.len_utf8();
        }
      }
    }
    value
  }

  fn number(&mut self) -> (Json, Node) {
    let start = self.at;
    while self
      .peek()
      .is_some_and(|b| matches!(b, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
    {
      self.at += 1;
    }
    let raw = &self.text[start..self.at];
    let valid = is_json_number(raw);
    let value = valid
      .then(|| serde_json::from_str::<Number>(raw).ok())
      .flatten();
    match value {
      Some(number) => (Json::Number(number), Node::scalar(start..self.at)),
      None => {
        self.error(start..self.at, "Invalid number");
        (Json::Null, Node::scalar(start..self.at))
      }
    }
  }
}

fn is_json_number(raw: &str) -> bool {
  let bytes = raw.as_bytes();
  let mut i = 0;
  if bytes.get(i) == Some(&b'-') {
    i += 1;
  }
  match bytes.get(i) {
    Some(b'0') => i += 1,
    Some(b'1'..=b'9') => {
      while bytes.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
      }
    }
    _ => return false,
  }
  if bytes.get(i) == Some(&b'.') {
    i += 1;
    if !bytes.get(i).is_some_and(u8::is_ascii_digit) {
      return false;
    }
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
      i += 1;
    }
  }
  if matches!(bytes.get(i), Some(b'e' | b'E')) {
    i += 1;
    if matches!(bytes.get(i), Some(b'+' | b'-')) {
      i += 1;
    }
    if !bytes.get(i).is_some_and(u8::is_ascii_digit) {
      return false;
    }
    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
      i += 1;
    }
  }
  i == bytes.len()
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
  fn toml_trees_know_where_keys_and_values_are() {
    let text = "a = 1\n[t]\nb = \"x\"\n[[arr]]\nc = true\n[[arr]]\nc = false\n";
    let document = parse_toml(text);
    assert!(document.errors.is_empty());
    assert!(document.table.is_some());
    let (b, taken) = document.tree.find(&[Segment::key("t"), Segment::key("b")]);
    assert_eq!(taken, 2);
    assert_eq!(&text[b.span.clone()], "\"x\"");
    assert_eq!(&text[b.key_span.clone().expect("key")], "b");
    let (c, _) = document
      .tree
      .find(&[Segment::key("arr"), Segment::Index(1), Segment::key("c")]);
    assert_eq!(&text[c.span.clone()], "false");
    let (t, _) = document.tree.find(&[Segment::key("t")]);
    assert_eq!(&text[t.mark()], "t");
    // A missing key finds its table.
    let (_, taken) = document.tree.find(&[Segment::key("t"), Segment::key("zz")]);
    assert_eq!(taken, 1);
  }

  #[test]
  fn toml_syntax_errors_are_placed() {
    let document = parse_toml("a = \nb = 2\n");
    assert!(document.table.is_none());
    assert_eq!(document.errors.len(), 1, "{:?}", document.errors);
    assert!(document.tree.get("b").is_some(), "recovers");
  }

  #[test]
  fn json_with_comments_and_trailing_commas_parses() {
    let text = "// settings\n{\n  \"a\": [1, 2,],\n  /* b */ \"b\": {\"c\": \"\\u00e9\\ud83d\\ude00\"},\n}\n";
    let document = parse_json(text);
    assert!(document.errors.is_empty(), "{:?}", document.errors);
    let value = document.value.expect("value");
    assert_eq!(value["a"][1], 2);
    assert_eq!(value["b"]["c"], "é😀");
    let (c, _) = document.tree.find(&parse_json_pointer("/b/c"));
    assert_eq!(&text[c.key_span.clone().expect("key")], "\"c\"");
    let (two, _) = document.tree.find(&parse_json_pointer("/a/1"));
    assert_eq!(&text[two.span.clone()], "2");
  }

  #[test]
  fn json_errors_are_reported_and_recovered_from() {
    let document = parse_json("{\"a\": , \"b\": tru, \"c\": 1 \"d\": 2}");
    assert!(document.value.is_none());
    assert!(document.errors.len() >= 2, "{:?}", document.errors);
    assert!(document.tree.get("d").is_some(), "{:?}", document.tree);
    let document = parse_json("{\"a\": 1, \"a\": 2}");
    assert_eq!(document.duplicates, vec![9..12]);
    assert_eq!(document.value.expect("value")["a"], 2);
    assert!(!parse_json("").errors.is_empty());
    assert!(!parse_json("[01]").errors.is_empty());
    assert!(!parse_json("[\"a\n\"]").errors.is_empty());
    assert!(parse_json("[1e5, -0.5, 0]").errors.is_empty());
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
