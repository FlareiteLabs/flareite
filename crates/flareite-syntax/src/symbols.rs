//! Symbols from a language's symbol query (`symbols.scm`), for languages
//! without a language server that lists them: Go to Symbol lists the
//! definitions, and Go to Definition and Find References match names in
//! the document.
//!
//! The query follows Tree-sitter's tags convention:
//!
//! - `@definition.<kind>` captures a whole definition (a function with its
//!   body), and `@name`, in the same match, its name. The kind is a symbol
//!   kind such as `function`, `struct`, or `variable`.
//! - `@reference` captures a name that may refer to a definition, and
//!   `@reference.<kind>` one that refers to a definition of that kind.
//!
//! Other captures (`@_function`, for a predicate) are ignored. Only the
//! document's own language is searched, not languages injected into it.

use std::ops::Range;

use flareite_text::TextBuffer;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor};

use crate::parse::SyntaxTree;

/// Most symbols listed for a document.
pub const MAX_SYMBOLS: usize = 10_000;
/// Longer names are left out.
const MAX_NAME_BYTES: usize = 1024;

/// A definition the symbol query found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
  pub name: String,
  /// From its capture's name: `function` for `@definition.function`.
  pub kind: String,
  /// The name of the innermost definition it's in.
  pub container: Option<String>,
  /// The whole definition, in bytes.
  pub range: Range<usize>,
  /// Its name, in bytes.
  pub name_range: Range<usize>,
}

/// What a capture of the symbol query means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Role {
  Definition(String),
  Name,
  Reference(Option<String>),
  Ignored,
}

/// A compiled symbol query and what each of its captures means.
pub(crate) struct SymbolQuery {
  query: Query,
  roles: Vec<Role>,
}

impl SymbolQuery {
  pub(crate) fn new(query: Query) -> Self {
    let roles = query
      .capture_names()
      .iter()
      .map(|name| role(name))
      .collect();
    Self { query, roles }
  }
}

fn role(name: &str) -> Role {
  match name.split_once('.') {
    _ if name == "name" => Role::Name,
    _ if name == "reference" => Role::Reference(None),
    Some(("definition", kind)) if !kind.is_empty() => Role::Definition(kind.to_owned()),
    Some(("reference", kind)) if !kind.is_empty() => Role::Reference(Some(kind.to_owned())),
    _ => Role::Ignored,
  }
}

/// A name the query found where it may refer to a definition.
struct Reference {
  range: Range<usize>,
  kind: Option<String>,
}

/// Everything the query found.
struct Found {
  symbols: Vec<Symbol>,
  references: Vec<Reference>,
}

/// The definitions in `syntax`, in order of position, each with the
/// definition it's in; at most [`MAX_SYMBOLS`]. `None` if its language has
/// no symbol query.
pub fn document_symbols(syntax: &SyntaxTree, buffer: &TextBuffer) -> Option<Vec<Symbol>> {
  Some(find(syntax, buffer, false)?.symbols)
}

/// The definition of the name at `offset` (in bytes): the first one with
/// that name, preferring one of the kind the name refers to. `Some(None)`
/// if there's no name there or nothing defines it; `None` if the language
/// has no symbol query.
pub fn definition_at(
  syntax: &SyntaxTree,
  buffer: &TextBuffer,
  offset: usize,
) -> Option<Option<Symbol>> {
  let found = find(syntax, buffer, true)?;
  let Some((name, kind)) = name_at(&found, buffer, offset) else {
    return Some(None);
  };
  let named = |symbol: &&Symbol| symbol.name == name;
  let definition = kind
    .and_then(|kind| {
      found
        .symbols
        .iter()
        .filter(named)
        .find(|symbol| symbol.kind == kind)
    })
    .or_else(|| found.symbols.iter().find(named))
    .cloned();
  Some(definition)
}

/// Where the name at `offset` appears, its definitions included, in order
/// of position. `Some` of an empty list if there's no name there; `None` if
/// the language has no symbol query.
pub fn references_at(
  syntax: &SyntaxTree,
  buffer: &TextBuffer,
  offset: usize,
) -> Option<Vec<Range<usize>>> {
  let found = find(syntax, buffer, true)?;
  let Some((name, _)) = name_at(&found, buffer, offset) else {
    return Some(Vec::new());
  };
  let mut ranges: Vec<Range<usize>> = found
    .symbols
    .iter()
    .map(|symbol| symbol.name_range.clone())
    .chain(found.references.iter().map(|found| found.range.clone()))
    .filter(|range| text(buffer, range.clone()).is_some_and(|text| text == name))
    .collect();
  ranges.sort_by_key(|range| (range.start, range.end));
  ranges.dedup();
  Some(ranges)
}

/// The name at `offset` (a definition's or a reference's, the narrowest
/// touching it) and the kind a reference names.
fn name_at(found: &Found, buffer: &TextBuffer, offset: usize) -> Option<(String, Option<String>)> {
  let touches = |range: &Range<usize>| range.start <= offset && offset <= range.end;
  let definition = found
    .symbols
    .iter()
    .filter(|symbol| touches(&symbol.name_range))
    .map(|symbol| (symbol.name_range.clone(), None));
  let reference = found
    .references
    .iter()
    .filter(|found| touches(&found.range))
    .map(|found| (found.range.clone(), found.kind.clone()));
  // The narrowest; for the same range, one that names a kind.
  let (range, kind) = definition
    .chain(reference)
    .min_by_key(|(range, kind)| (range.len(), kind.is_none()))?;
  Some((text(buffer, range)?, kind))
}

fn find(syntax: &SyntaxTree, buffer: &TextBuffer, references: bool) -> Option<Found> {
  let grammar = syntax.language().grammar().ok()?;
  let SymbolQuery { query, roles } = grammar.symbols.as_ref()?;
  let mut found = Found {
    symbols: Vec::new(),
    references: Vec::new(),
  };
  let mut cursor = QueryCursor::new();
  let source = |node: Node| buffer.chunks_in(node.byte_range()).map(str::as_bytes);
  let mut matches = cursor.matches(query, syntax.tree().root_node(), source);
  while let Some(found_match) = matches.next() {
    let mut definition = None;
    let mut name = None;
    for capture in found_match.captures() {
      match &roles[capture.index as usize] {
        Role::Definition(kind) => definition = Some((kind, capture.node.byte_range())),
        Role::Name => name = Some(capture.node.byte_range()),
        Role::Reference(kind) if references => found.references.push(Reference {
          range: capture.node.byte_range(),
          kind: kind.clone(),
        }),
        Role::Reference(_) | Role::Ignored => {}
      }
    }
    if let (Some((kind, range)), Some(name_range)) = (definition, name)
      && found.symbols.len() < MAX_SYMBOLS
      && let Some(name) = text(buffer, name_range.clone())
      && !name.trim().is_empty()
    {
      found.symbols.push(Symbol {
        name: name.trim().to_owned(),
        kind: kind.clone(),
        container: None,
        range,
        name_range,
      });
    }
  }
  found
    .symbols
    .sort_by(|a, b| (a.range.start, b.range.end).cmp(&(b.range.start, a.range.end)));
  found
    .symbols
    .dedup_by(|a, b| a.range == b.range && a.name_range == b.name_range);
  set_containers(&mut found.symbols);
  Some(found)
}

/// Gives each symbol the name of the innermost one whose range holds it.
/// Symbols are sorted by start, outer before inner.
fn set_containers(symbols: &mut [Symbol]) {
  let mut open: Vec<(Range<usize>, String)> = Vec::new();
  for symbol in symbols {
    while open
      .last()
      .is_some_and(|(range, _)| range.end < symbol.range.end || range.start > symbol.range.start)
    {
      open.pop();
    }
    symbol.container = open.last().map(|(_, name)| name.clone());
    open.push((symbol.range.clone(), symbol.name.clone()));
  }
}

/// The text of a name, unless it's too long to be one.
fn text(buffer: &TextBuffer, range: Range<usize>) -> Option<String> {
  (range.len() <= MAX_NAME_BYTES && range.end <= buffer.len()).then(|| buffer.text_in(range))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parse::parse;
  use crate::testing;

  const SCRIPT: &str = "MEMORY\n{\n  FLASH (rx) : ORIGIN = 0x0, LENGTH = 64K\n  RAM (rwx) : ORIGIN = 0x20000000, LENGTH = 8K\n}\n\nSECTIONS\n{\n  .text : { *(.text) _etext = .; } > FLASH\n  .data : { *(.data) } > RAM AT> FLASH\n  _end = SIZEOF(.text) + ORIGIN(RAM);\n}\n";

  fn tree(id: &str, text: &str) -> (SyntaxTree, TextBuffer) {
    let registry = testing::registry();
    let buffer = TextBuffer::from_text(text);
    let tree = parse(
      registry.get(id).expect("an example"),
      &registry,
      &buffer,
      None,
      || false,
    )
    .expect("grammar")
    .expect("parses");
    (tree, buffer)
  }

  fn offset_of(text: &str, needle: &str, nth: usize) -> usize {
    text
      .match_indices(needle)
      .nth(nth)
      .map(|(offset, _)| offset)
      .expect("in the text")
  }

  #[test]
  fn definitions_are_listed_with_their_container() {
    let (tree, buffer) = tree("linkerscript", SCRIPT);
    let symbols = document_symbols(&tree, &buffer).expect("a symbol query");
    let listed: Vec<_> = symbols
      .iter()
      .map(|symbol| {
        (
          symbol.name.as_str(),
          symbol.kind.as_str(),
          symbol.container.as_deref(),
        )
      })
      .collect();
    assert_eq!(
      listed,
      [
        ("FLASH", "struct", None),
        ("RAM", "struct", None),
        (".text", "module", None),
        ("_etext", "variable", Some(".text")),
        (".data", "module", None),
        ("_end", "variable", None),
      ]
    );
    let text = &symbols[2];
    assert_eq!(&SCRIPT[text.name_range.clone()], ".text");
    assert!(SCRIPT[text.range.clone()].ends_with("> FLASH"));
  }

  #[test]
  fn names_go_to_their_definition_of_the_kind_they_name() {
    let (tree, buffer) = tree("linkerscript", SCRIPT);
    let at = |needle: &str, nth: usize| {
      definition_at(&tree, &buffer, offset_of(SCRIPT, needle, nth))
        .expect("a symbol query")
        .map(|symbol| (symbol.name, symbol.kind))
    };
    assert_eq!(at("> FLASH", 0).map(|found| found.0), None, "on `>`");
    assert_eq!(
      at("FLASH\n", 0),
      Some(("FLASH".into(), "struct".into())),
      "a region after `>`"
    );
    assert_eq!(at("RAM)", 0), Some(("RAM".into(), "struct".into())));
    assert_eq!(at(".text)", 1), Some((".text".into(), "module".into())));
    assert_eq!(at("SIZEOF", 0), None, "defined nowhere");
    // The end of a name counts, as the cursor is often after it.
    let end = offset_of(SCRIPT, "_etext", 0) + "_etext".len();
    assert_eq!(
      definition_at(&tree, &buffer, end)
        .flatten()
        .map(|symbol| symbol.name),
      Some("_etext".into())
    );
  }

  #[test]
  fn references_include_the_definition() {
    let (tree, buffer) = tree("linkerscript", SCRIPT);
    let found = references_at(&tree, &buffer, offset_of(SCRIPT, "FLASH", 0)).expect("a query");
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().all(|range| &SCRIPT[range.clone()] == "FLASH"));
    assert_eq!(
      references_at(&tree, &buffer, offset_of(SCRIPT, "{", 0)),
      Some(Vec::new())
    );
  }

  #[test]
  fn languages_without_a_symbol_query_have_none() {
    let (tree, buffer) = tree("json", "{\"a\": 1}");
    assert_eq!(document_symbols(&tree, &buffer), None);
    assert_eq!(definition_at(&tree, &buffer, 2), None);
  }

  #[test]
  fn capture_names_give_roles() {
    assert_eq!(
      role("definition.function"),
      Role::Definition("function".into())
    );
    assert_eq!(role("name"), Role::Name);
    assert_eq!(role("reference"), Role::Reference(None));
    assert_eq!(
      role("reference.struct"),
      Role::Reference(Some("struct".into()))
    );
    assert_eq!(role("definition"), Role::Ignored);
    assert_eq!(role("_function"), Role::Ignored);
  }
}
