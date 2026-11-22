//! Go to Symbol, Go to Definition, and Find References from a language's
//! symbol query (`symbols.scm`), for documents whose language server can't
//! answer (or that have none). They work within the document.

use std::ops::Range;
use std::time::Duration;

use flareite_core::Document;
use flareite_syntax::{SyntaxTree, deadline, parse};
use flareite_text::TextBuffer;
use tauri::{AppHandle, Manager};

use super::commands::{LocationDto, ReferenceDto};
use super::symbols::DocumentSymbolDto;
use crate::editor::EditorState;
use crate::editor::protocol::Position;
use crate::editor::syntax::MAX_SYNTAX_BYTES;
use crate::languages::LanguagesState;
use flareite_core::MAX_DISPLAY_LINE_BYTES;

/// Longest a parse may take for a request when the document's tree isn't
/// ready yet.
const PARSE_BUDGET: Duration = Duration::from_millis(500);

/// The kinds a symbol query may name, as completion items' and language
/// servers' symbols are named; others are shown as `property`.
const KINDS: &[&str] = &[
  "file",
  "module",
  "namespace",
  "package",
  "class",
  "method",
  "property",
  "field",
  "constructor",
  "enum",
  "interface",
  "function",
  "variable",
  "constant",
  "string",
  "number",
  "boolean",
  "array",
  "object",
  "key",
  "null",
  "enumMember",
  "struct",
  "event",
  "operator",
  "typeParameter",
];

fn kind_name(kind: &str) -> &'static str {
  KINDS
    .iter()
    .find(|known| **known == kind)
    .copied()
    .unwrap_or("property")
}

/// `document`'s syntax tree, if its language has a symbol query: the one
/// kept for highlighting, or else a parse now. Called with the editor locked.
fn tree(app: &AppHandle, document: &Document) -> Option<SyntaxTree> {
  let language = app
    .state::<LanguagesState>()
    .for_document(document)
    .filter(|language| language.has_symbols())?;
  // The syntax states live in the editor's state (its lock is held).
  if let Some(tree) = app.state::<EditorState>().syntax.tree(app, document) {
    return Some(tree);
  }
  let buffer = document.buffer();
  if buffer.len() > MAX_SYNTAX_BYTES {
    return None;
  }
  let registry = app.state::<LanguagesState>().registry();
  parse(&language, &registry, buffer, None, deadline(PARSE_BUDGET))
    .ok()
    .flatten()
}

fn position(buffer: &TextBuffer, offset: usize) -> Position {
  buffer.offset_to_utf16(offset).into()
}

/// The document's symbols, or `None` if its language has no symbol query.
/// Called with the editor locked.
pub(super) fn document_symbols(
  app: &AppHandle,
  document: &Document,
) -> Option<Vec<DocumentSymbolDto>> {
  let tree = tree(app, document)?;
  let buffer = document.buffer();
  let symbols = flareite_syntax::document_symbols(&tree, buffer)?;
  Some(
    symbols
      .into_iter()
      .map(|symbol| DocumentSymbolDto {
        name: symbol.name,
        kind: kind_name(&symbol.kind),
        container: symbol.container,
        deprecated: false,
        start: position(buffer, symbol.range.start),
        end: position(buffer, symbol.range.end),
        selection: position(buffer, symbol.name_range.start),
      })
      .collect(),
  )
}

/// Where the name at `offset` is defined in the document: `Some(None)` if
/// nowhere, `None` if its language has no symbol query. Called with the
/// editor locked.
pub(super) fn definition(
  app: &AppHandle,
  document: &Document,
  path: &str,
  offset: usize,
) -> Option<Option<LocationDto>> {
  let tree = tree(app, document)?;
  let buffer = document.buffer();
  let found = flareite_syntax::definition_at(&tree, buffer, offset)?;
  Some(found.map(|symbol| LocationDto {
    path: path.to_owned(),
    start: position(buffer, symbol.name_range.start),
    end: position(buffer, symbol.name_range.end),
  }))
}

/// Where the name at `offset` appears in the document, or `None` if its
/// language has no symbol query. Called with the editor locked.
pub(super) fn references(
  app: &AppHandle,
  document: &Document,
  path: &str,
  offset: usize,
) -> Option<Vec<ReferenceDto>> {
  let tree = tree(app, document)?;
  let buffer = document.buffer();
  let ranges = flareite_syntax::references_at(&tree, buffer, offset)?;
  Some(
    ranges
      .into_iter()
      .map(|range: Range<usize>| {
        let start = position(buffer, range.start);
        ReferenceDto {
          path: path.to_owned(),
          start,
          end: position(buffer, range.end),
          line: buffer.line_prefix(start.line, MAX_DISPLAY_LINE_BYTES).0,
        }
      })
      .collect(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn unknown_kinds_are_properties() {
    assert_eq!(kind_name("struct"), "struct");
    assert_eq!(kind_name("enumMember"), "enumMember");
    assert_eq!(kind_name("region"), "property");
  }
}
