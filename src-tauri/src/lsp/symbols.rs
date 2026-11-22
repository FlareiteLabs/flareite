//! Symbol requests, for Go to Symbol: a document's symbols, and symbols
//! anywhere in the workspace by name. A document's are mapped to its text as
//! it is when they arrive. Workspace symbols are left as their server put
//! them until one is chosen (`lsp_resolve_symbol`), so a search doesn't read
//! every file it names.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use flareite_core::DocumentId;
use flareite_lsp::LspError;
use flareite_lsp::convert::{PositionEncoding, path_to_uri, to_offset, uri_to_path};
use flareite_lsp::lsp_types::request::{DocumentSymbolRequest, WorkspaceSymbolRequest};
use flareite_lsp::lsp_types::{
  DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, OneOf, Range, SymbolKind,
  SymbolTag, TextDocumentIdentifier, Uri, WorkspaceSymbolParams, WorkspaceSymbolResponse,
};
use flareite_text::{TextBuffer, map_offset_through};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use super::LspState;
use super::commands::{LocationDto, REQUEST_TIMEOUT, blocking, log_failure, resolve};
use crate::editor::EditorState;
use crate::editor::protocol::Position;
use crate::error::CommandError;
use crate::paths;

/// Most symbols listed for a document.
const MAX_DOCUMENT_SYMBOLS: usize = 10_000;
/// Most symbols listed from each server for a workspace search.
const MAX_WORKSPACE_SYMBOLS: usize = 1_000;

/// Bumped by each workspace search, to cancel the one before.
static WORKSPACE_SEARCH: AtomicU64 = AtomicU64::new(0);

/// A symbol in a document.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSymbolDto {
  pub name: String,
  pub kind: &'static str,
  /// The name of the symbol it's in.
  pub container: Option<String>,
  pub deprecated: bool,
  /// The whole symbol: a function with its body, say.
  pub start: Position,
  pub end: Position,
  /// Where its name starts, which choosing it goes to.
  pub selection: Position,
}

/// A symbol found in the workspace.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSymbolDto {
  pub name: String,
  pub kind: &'static str,
  pub container: Option<String>,
  pub deprecated: bool,
  pub location: SymbolLocationDto,
}

/// Where a workspace symbol is, as its server put it; `lsp_resolve_symbol`
/// finds that in the file's text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolLocationDto {
  pub path: String,
  /// `None` when the server named only the file.
  pub range: Option<Range>,
  /// The unit of the range's columns: `utf-8` or `utf-16`.
  pub encoding: String,
}

/// A symbol as a server described it.
#[derive(Debug, PartialEq)]
struct Symbol {
  name: String,
  kind: SymbolKind,
  container: Option<String>,
  deprecated: bool,
  range: Range,
  selection: Range,
}

/// The symbols in a document, in order of position, each after the symbol
/// it's in; `None` when neither its language server nor its language's
/// symbol query can list them.
#[tauri::command]
pub async fn lsp_document_symbols(
  app: AppHandle,
  id: u64,
) -> Result<Option<Vec<DocumentSymbolDto>>, CommandError> {
  let id = DocumentId::from_raw(id);
  // The text the server has, which its answer refers to.
  let (target, text) = {
    let state = app.state::<EditorState>();
    let editor = state.lock();
    let document = editor.document(id)?;
    let lsp = app.state::<LspState>();
    lsp.sync(&app, document);
    (
      lsp.target(id),
      TextBuffer::from_snapshot(&document.buffer().snapshot()),
    )
  };
  let from_query = || -> Result<_, CommandError> {
    let state = app.state::<EditorState>();
    let editor = state.lock();
    Ok(super::query_symbols::document_symbols(
      &app,
      editor.document(id)?,
    ))
  };
  let Some(target) = target else {
    return from_query();
  };
  if matches!(
    target.server.capabilities().document_symbol_provider,
    None | Some(OneOf::Left(false))
  ) {
    return from_query();
  }
  let params = DocumentSymbolParams {
    text_document: TextDocumentIdentifier::new(target.uri.clone()),
    work_done_progress_params: Default::default(),
    partial_result_params: Default::default(),
  };
  let server = target.server.clone();
  let symbols = match blocking(move || {
    server.request::<DocumentSymbolRequest>(params, REQUEST_TIMEOUT, || false)
  })
  .await?
  {
    Ok(response) => response.map(flatten).unwrap_or_default(),
    Err(error) => {
      log_failure("document symbols", &error);
      Vec::new()
    }
  };

  let encoding = target.server.encoding();
  let state = app.state::<EditorState>();
  let editor = state.lock();
  let buffer = editor.document(id)?.buffer();
  let edits = buffer.edits_since(text.version()).unwrap_or_default();
  let current = |position| -> Position {
    let offset = to_offset(&text, position, encoding);
    buffer
      .offset_to_utf16(map_offset_through(&edits, offset, false))
      .into()
  };
  let mut symbols: Vec<DocumentSymbolDto> = symbols
    .into_iter()
    .take(MAX_DOCUMENT_SYMBOLS)
    .map(|symbol| DocumentSymbolDto {
      name: symbol.name,
      kind: kind_name(symbol.kind),
      container: symbol.container,
      deprecated: symbol.deprecated,
      start: current(symbol.range.start),
      end: current(symbol.range.end),
      selection: current(symbol.selection.start),
    })
    .collect();
  symbols.sort_by_key(|symbol| (symbol.start.line, symbol.start.column));
  Ok(Some(symbols))
}

/// A document's symbols as a list, each tree of symbols in pre-order with the
/// name of the symbol each is in.
fn flatten(response: DocumentSymbolResponse) -> Vec<Symbol> {
  fn walk(symbols: Vec<DocumentSymbol>, container: Option<&str>, out: &mut Vec<Symbol>) {
    for symbol in symbols {
      #[allow(deprecated)]
      let deprecated = is_deprecated(symbol.tags.as_deref(), symbol.deprecated);
      let name = symbol.name.trim().to_owned();
      out.push(Symbol {
        name: name.clone(),
        kind: symbol.kind,
        container: container.map(str::to_owned),
        deprecated,
        range: symbol.range,
        selection: symbol.selection_range,
      });
      walk(symbol.children.unwrap_or_default(), Some(&name), out);
    }
  }
  match response {
    DocumentSymbolResponse::Flat(symbols) => symbols
      .into_iter()
      .map(|symbol| {
        #[allow(deprecated)]
        let deprecated = is_deprecated(symbol.tags.as_deref(), symbol.deprecated);
        Symbol {
          name: symbol.name.trim().to_owned(),
          kind: symbol.kind,
          container: symbol.container_name.filter(|name| !name.is_empty()),
          deprecated,
          range: symbol.location.range,
          selection: symbol.location.range,
        }
      })
      .collect(),
    DocumentSymbolResponse::Nested(symbols) => {
      let mut out = Vec::new();
      walk(symbols, None, &mut out);
      out
    }
  }
}

/// Symbols matching `query` in every running server's workspace that can be
/// searched. A newer search cancels this one.
#[tauri::command]
pub async fn lsp_workspace_symbols(
  app: AppHandle,
  query: String,
) -> Result<Vec<WorkspaceSymbolDto>, CommandError> {
  let search = WORKSPACE_SEARCH.fetch_add(1, Ordering::SeqCst) + 1;
  let requests: Vec<_> = app
    .state::<LspState>()
    .running()
    .into_iter()
    .filter(|server| {
      !matches!(
        server.capabilities().workspace_symbol_provider,
        None | Some(OneOf::Left(false))
      )
    })
    .map(|server| {
      let params = WorkspaceSymbolParams {
        query: query.clone(),
        ..Default::default()
      };
      tauri::async_runtime::spawn_blocking(move || {
        let response = server.request::<WorkspaceSymbolRequest>(params, REQUEST_TIMEOUT, || {
          WORKSPACE_SEARCH.load(Ordering::SeqCst) != search
        });
        (server.encoding(), response)
      })
    })
    .collect();
  let mut symbols = Vec::new();
  for request in requests {
    match request.await.map_err(CommandError::internal)? {
      (encoding, Ok(Some(response))) => {
        let mut found = workspace_symbols(response, encoding);
        found.truncate(MAX_WORKSPACE_SYMBOLS);
        symbols.extend(found);
      }
      (_, Ok(None) | Err(LspError::Cancelled)) => {}
      (_, Err(error)) => log_failure("workspace symbols", &error),
    }
  }
  Ok(symbols)
}

fn workspace_symbols(
  response: WorkspaceSymbolResponse,
  encoding: PositionEncoding,
) -> Vec<WorkspaceSymbolDto> {
  let symbol = |name: String,
                kind,
                container: Option<String>,
                deprecated,
                uri: &Uri,
                range|
   -> Option<WorkspaceSymbolDto> {
    Some(WorkspaceSymbolDto {
      name: name.trim().to_owned(),
      kind: kind_name(kind),
      container: container.filter(|name| !name.is_empty()),
      deprecated,
      location: SymbolLocationDto {
        path: paths::normalize(&uri_to_path(uri)?).display().to_string(),
        range,
        encoding: encoding.label().to_owned(),
      },
    })
  };
  match response {
    WorkspaceSymbolResponse::Flat(symbols) => symbols
      .into_iter()
      .filter_map(|found| {
        #[allow(deprecated)]
        let deprecated = is_deprecated(found.tags.as_deref(), found.deprecated);
        symbol(
          found.name,
          found.kind,
          found.container_name,
          deprecated,
          &found.location.uri,
          Some(found.location.range),
        )
      })
      .collect(),
    WorkspaceSymbolResponse::Nested(symbols) => symbols
      .into_iter()
      .filter_map(|found| {
        let (uri, range) = match &found.location {
          OneOf::Left(location) => (&location.uri, Some(location.range)),
          OneOf::Right(location) => (&location.uri, None),
        };
        symbol(
          found.name,
          found.kind,
          found.container_name,
          is_deprecated(found.tags.as_deref(), None),
          uri,
          range,
        )
      })
      .collect(),
  }
}

/// Where a workspace symbol is in its file's text as it is now: an open
/// document's, or the file's on disk. `None` if the file can't be read.
#[tauri::command]
pub async fn lsp_resolve_symbol(
  app: AppHandle,
  location: SymbolLocationDto,
) -> Result<Option<LocationDto>, CommandError> {
  let encoding = if location.encoding == PositionEncoding::Utf8.label() {
    PositionEncoding::Utf8
  } else {
    PositionEncoding::Utf16
  };
  let Some(uri) = path_to_uri(Path::new(&location.path)) else {
    return Ok(None);
  };
  let resolved = resolve(
    &app,
    encoding,
    vec![(uri, location.range.unwrap_or_default())],
  )
  .await?;
  Ok(resolved.into_iter().next().map(|found| LocationDto {
    path: found.path,
    start: found.start,
    end: found.end,
  }))
}

fn is_deprecated(tags: Option<&[SymbolTag]>, deprecated: Option<bool>) -> bool {
  deprecated == Some(true) || tags.is_some_and(|tags| tags.contains(&SymbolTag::DEPRECATED))
}

/// A symbol kind's name, as completion items' kinds are named.
fn kind_name(kind: SymbolKind) -> &'static str {
  match kind {
    SymbolKind::FILE => "file",
    SymbolKind::MODULE => "module",
    SymbolKind::NAMESPACE => "namespace",
    SymbolKind::PACKAGE => "package",
    SymbolKind::CLASS => "class",
    SymbolKind::METHOD => "method",
    SymbolKind::PROPERTY => "property",
    SymbolKind::FIELD => "field",
    SymbolKind::CONSTRUCTOR => "constructor",
    SymbolKind::ENUM => "enum",
    SymbolKind::INTERFACE => "interface",
    SymbolKind::FUNCTION => "function",
    SymbolKind::VARIABLE => "variable",
    SymbolKind::CONSTANT => "constant",
    SymbolKind::STRING => "string",
    SymbolKind::NUMBER => "number",
    SymbolKind::BOOLEAN => "boolean",
    SymbolKind::ARRAY => "array",
    SymbolKind::OBJECT => "object",
    SymbolKind::KEY => "key",
    SymbolKind::NULL => "null",
    SymbolKind::ENUM_MEMBER => "enumMember",
    SymbolKind::STRUCT => "struct",
    SymbolKind::EVENT => "event",
    SymbolKind::OPERATOR => "operator",
    SymbolKind::TYPE_PARAMETER => "typeParameter",
    _ => "property",
  }
}

#[cfg(test)]
mod tests {
  use flareite_lsp::lsp_types::{
    Location, Position as LspPosition, SymbolInformation, WorkspaceLocation, WorkspaceSymbol,
  };

  use super::*;

  fn range(line: u32, start: u32, end: u32) -> Range {
    Range::new(LspPosition::new(line, start), LspPosition::new(line, end))
  }

  #[allow(deprecated)]
  fn document_symbol(name: &str, kind: SymbolKind, line: u32) -> DocumentSymbol {
    DocumentSymbol {
      name: name.into(),
      detail: None,
      kind,
      tags: None,
      deprecated: None,
      range: range(line, 0, 20),
      selection_range: range(line, 4, 4 + name.len() as u32),
      children: None,
    }
  }

  #[test]
  fn nested_symbols_are_listed_after_the_symbol_they_are_in() {
    let mut parser = document_symbol("Parser", SymbolKind::STRUCT, 0);
    let mut parse = document_symbol(" parse ", SymbolKind::METHOD, 1);
    parse.tags = Some(vec![SymbolTag::DEPRECATED]);
    parser.children = Some(vec![parse]);
    let symbols = flatten(DocumentSymbolResponse::Nested(vec![
      parser,
      document_symbol("main", SymbolKind::FUNCTION, 3),
    ]));
    let names: Vec<_> = symbols
      .iter()
      .map(|symbol| {
        (
          symbol.name.as_str(),
          symbol.container.as_deref(),
          symbol.deprecated,
        )
      })
      .collect();
    assert_eq!(
      names,
      [
        ("Parser", None, false),
        ("parse", Some("Parser"), true),
        ("main", None, false)
      ]
    );
    assert_eq!(symbols[1].selection, range(1, 4, 11));
  }

  #[test]
  #[allow(deprecated)]
  fn flat_symbols_keep_their_container() {
    let uri: Uri = "file:///w/main.rs".parse().expect("a URI");
    let symbols = flatten(DocumentSymbolResponse::Flat(vec![SymbolInformation {
      name: "parse".into(),
      kind: SymbolKind::FUNCTION,
      tags: None,
      deprecated: Some(true),
      location: Location::new(uri, range(2, 3, 8)),
      container_name: Some("Parser".into()),
    }]));
    assert_eq!(symbols[0].container.as_deref(), Some("Parser"));
    assert!(symbols[0].deprecated);
    assert_eq!(symbols[0].selection, range(2, 3, 8));
  }

  #[test]
  fn workspace_symbols_keep_their_servers_position() {
    let uri: Uri = "file:///w/src/lib.rs".parse().expect("a URI");
    let found = workspace_symbols(
      WorkspaceSymbolResponse::Nested(vec![
        WorkspaceSymbol {
          name: "total".into(),
          kind: SymbolKind::FUNCTION,
          tags: None,
          container_name: Some(String::new()),
          location: OneOf::Left(Location::new(uri.clone(), range(4, 7, 12))),
          data: None,
        },
        WorkspaceSymbol {
          name: "Lazy".into(),
          kind: SymbolKind::STRUCT,
          tags: None,
          container_name: Some("util".into()),
          location: OneOf::Right(WorkspaceLocation { uri }),
          data: None,
        },
      ]),
      PositionEncoding::Utf8,
    );
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].kind, "function");
    assert_eq!(found[0].container, None);
    assert_eq!(found[0].location.range, Some(range(4, 7, 12)));
    assert_eq!(found[0].location.encoding, "utf-8");
    assert!(found[0].location.path.ends_with("lib.rs"));
    assert_eq!(found[1].container.as_deref(), Some("util"));
    assert_eq!(found[1].location.range, None);
  }
}
