use std::fmt;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};

use tree_sitter::wasmtime::Engine;
use tree_sitter::{Language as TsLanguage, Query, WasmStore};

use crate::highlight::TokenKind;
use crate::injections::Injections;
use crate::parse::SyntaxError;
use crate::symbols::SymbolQuery;

/// Larger grammar files are not loaded.
pub const MAX_GRAMMAR_BYTES: u64 = 32 * 1024 * 1024;
/// Larger query files are not loaded.
pub const MAX_QUERY_BYTES: u64 = 1024 * 1024;

/// A language Flareite can highlight, contributed by an extension. Cheap to
/// clone; two values are equal when they are the same registration, so a
/// language that is registered again (an extension reloaded with changed
/// files) compares unequal to the old one.
#[derive(Clone)]
pub struct Language(Arc<Definition>);

struct Definition {
  definition: LanguageDefinition,
  grammar: OnceLock<Result<Grammar, SyntaxError>>,
}

/// A compiled grammar and its queries.
pub(crate) struct Grammar {
  pub(crate) language: TsLanguage,
  pub(crate) query: Query,
  /// The token kind of each capture in `query`, by capture index.
  pub(crate) kinds: Vec<Option<TokenKind>>,
  pub(crate) injections: Option<Injections>,
  /// The fold query, which captures foldable nodes as `@fold`.
  pub(crate) folds: Option<Query>,
  /// The symbol query, which captures definitions and references.
  pub(crate) symbols: Option<SymbolQuery>,
}

/// A language as an extension declares it: a Tree-sitter grammar compiled
/// to WebAssembly (`tree-sitter build --wasm`) and its queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageDefinition {
  /// Identifier used by LSP (`languageId`), in settings, and for
  /// injections, e.g. `html`.
  pub id: String,
  pub name: String,
  /// Without the leading dot, e.g. `html`; may contain dots (`d.ts`).
  pub file_extensions: Vec<String>,
  /// Whole file names, e.g. `Makefile`.
  pub file_names: Vec<String>,
  pub grammar: PathBuf,
  /// The name the grammar was generated with: its WebAssembly module
  /// exports `tree_sitter_<grammar_name>`.
  pub grammar_name: String,
  /// The highlight query (`highlights.scm`).
  pub highlights: PathBuf,
  /// The injection query (`injections.scm`), if other languages are
  /// embedded in this one.
  pub injections: Option<PathBuf>,
  /// The fold query (`folds.scm`), if the language folds by its syntax.
  pub folds: Option<PathBuf>,
  /// The symbol query (`symbols.scm`), if the language's definitions are
  /// found by its syntax (see [`crate::document_symbols`]).
  pub symbols: Option<PathBuf>,
}

impl Language {
  /// Identifier used by LSP (`languageId`) and in settings, e.g. `rust`.
  pub fn id(&self) -> &str {
    &self.0.definition.id
  }

  /// The name shown to people, e.g. `Rust`.
  pub fn name(&self) -> &str {
    &self.0.definition.name
  }

  /// The file extensions it claims, lowercase and without the dot.
  pub fn file_extensions(&self) -> &[String] {
    &self.0.definition.file_extensions
  }

  /// The whole file names it claims, e.g. `Cargo.lock`.
  pub fn file_names(&self) -> &[String] {
    &self.0.definition.file_names
  }

  /// Whether the language has a fold query: it declares one, and its
  /// grammar hasn't failed to load. Doesn't load the grammar (or wait for
  /// it to load), as compiling one can take a while.
  pub fn has_folds(&self) -> bool {
    self.0.definition.folds.is_some() && self.0.grammar.get().is_none_or(|grammar| grammar.is_ok())
  }

  /// Whether the language has a symbol query: it declares one, and its
  /// grammar hasn't failed to load. Doesn't load the grammar.
  pub fn has_symbols(&self) -> bool {
    self.0.definition.symbols.is_some()
      && self.0.grammar.get().is_none_or(|grammar| grammar.is_ok())
  }

  /// The compiled grammar, loaded on first use.
  pub(crate) fn grammar(&self) -> Result<&Grammar, SyntaxError> {
    self
      .0
      .grammar
      .get_or_init(|| {
        let grammar = compile(&self.0.definition);
        if let Err(error) = &grammar {
          tracing::error!(language = self.id(), %error, "syntax highlighting is unavailable");
        }
        grammar
      })
      .as_ref()
      .map_err(Clone::clone)
  }

  /// How well this language claims `name` (a lowercase file name): a whole
  /// name beats an extension, and a longer extension beats a shorter one.
  fn claim(&self, name: &str) -> Option<usize> {
    let definition = &self.0.definition;
    if definition
      .file_names
      .iter()
      .any(|file_name| file_name.eq_ignore_ascii_case(name))
    {
      return Some(usize::MAX);
    }
    definition
      .file_extensions
      .iter()
      .filter(|extension| {
        name.len() > extension.len() + 1
          && name.ends_with(extension.as_str())
          && name.as_bytes()[name.len() - extension.len() - 1] == b'.'
      })
      .map(String::len)
      .max()
  }
}

impl PartialEq for Language {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.0, &other.0)
  }
}

impl Eq for Language {}

impl Hash for Language {
  fn hash<H: Hasher>(&self, state: &mut H) {
    Arc::as_ptr(&self.0).hash(state);
  }
}

impl fmt::Debug for Language {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("Language").field(&self.id()).finish()
  }
}

/// The languages available, all from extensions. Without any, nothing is
/// highlighted. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Registry {
  languages: Vec<Language>,
}

impl Registry {
  /// The languages `definitions` describe. A language whose id is already
  /// taken by an earlier one is left out, and described in the returned
  /// messages. Nothing is loaded until a language is first used.
  pub fn new(definitions: impl IntoIterator<Item = LanguageDefinition>) -> (Self, Vec<String>) {
    let mut languages: Vec<Language> = Vec::new();
    let mut problems = Vec::new();
    for mut definition in definitions {
      if languages.iter().any(|l| l.id() == definition.id) {
        problems.push(format!(
          "the language {:?} ({}) is already registered",
          definition.id, definition.name
        ));
        continue;
      }
      for extension in &mut definition.file_extensions {
        extension.make_ascii_lowercase();
      }
      languages.push(Language(Arc::new(Definition {
        definition,
        grammar: OnceLock::new(),
      })));
    }
    (Self { languages }, problems)
  }

  /// The language of a file, from its name or extension. When languages
  /// claim a file equally well, the one registered first wins.
  pub fn for_path(&self, path: &Path) -> Option<&Language> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let mut best: Option<(usize, &Language)> = None;
    for language in &self.languages {
      if let Some(claim) = language.claim(&name)
        && best.is_none_or(|(best, _)| claim > best)
      {
        best = Some((claim, language));
      }
    }
    best.map(|(_, language)| language)
  }

  /// The language with this id, e.g. for an injection.
  pub fn get(&self, id: &str) -> Option<&Language> {
    self.languages.iter().find(|language| language.id() == id)
  }

  /// The language with this id, or else for this file extension: how a
  /// Markdown code fence names one (`rust` or `rs`). Names are lowercase.
  pub fn for_name(&self, name: &str) -> Option<&Language> {
    let extension = !name.is_empty()
      && name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
    self
      .get(name)
      .or_else(|| extension.then(|| self.for_path(Path::new(&format!("x.{name}"))))?)
  }

  pub fn languages(&self) -> impl Iterator<Item = &Language> {
    self.languages.iter()
  }
}

/// The WebAssembly engine grammars run on. Separate from the extension
/// host's, which interrupts calls on a schedule Tree-sitter doesn't use.
pub(crate) fn engine() -> &'static Engine {
  static ENGINE: LazyLock<Engine> = LazyLock::new(Engine::default);
  &ENGINE
}

fn compile(definition: &LanguageDefinition) -> Result<Grammar, SyntaxError> {
  let id = &definition.id;
  let load = |message: String| SyntaxError::Load {
    language: id.clone(),
    message,
  };
  let bytes = read_limited(&definition.grammar, MAX_GRAMMAR_BYTES).map_err(load)?;
  // The store only compiles the module; each parser instantiates the
  // grammar in its own store (see `parse`).
  let mut store = WasmStore::new(engine()).map_err(|error| load(error.message))?;
  let language = store
    .load_language(&definition.grammar_name, &bytes)
    .map_err(|error| load(error.message))?;
  let query = |path: &Path| {
    read_limited(path, MAX_QUERY_BYTES).and_then(|bytes| {
      String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8", path.display()))
    })
  };
  let highlights = query(&definition.highlights).map_err(load)?;
  let injections = definition
    .injections
    .as_deref()
    .map(query)
    .transpose()
    .map_err(load)?;
  let folds = definition
    .folds
    .as_deref()
    .map(query)
    .transpose()
    .map_err(load)?;
  let symbols = definition
    .symbols
    .as_deref()
    .map(query)
    .transpose()
    .map_err(load)?;
  build(
    id,
    language,
    &highlights,
    injections.as_deref(),
    folds.as_deref(),
    symbols.as_deref(),
  )
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
  let describe = |error: std::io::Error| format!("could not read {}: {error}", path.display());
  let file = std::fs::File::open(path).map_err(describe)?;
  let mut bytes = Vec::new();
  file
    .take(limit + 1)
    .read_to_end(&mut bytes)
    .map_err(describe)?;
  if bytes.len() as u64 > limit {
    return Err(format!(
      "{} is larger than {} MiB",
      path.display(),
      limit / 1024 / 1024
    ));
  }
  Ok(bytes)
}

fn build(
  id: &str,
  language: TsLanguage,
  highlights: &str,
  injections: Option<&str>,
  folds: Option<&str>,
  symbols: Option<&str>,
) -> Result<Grammar, SyntaxError> {
  let invalid = |kind: &'static str, error: tree_sitter::QueryError| SyntaxError::Query {
    language: id.to_owned(),
    kind,
    message: error.to_string(),
  };
  let query = Query::new(&language, highlights).map_err(|error| invalid("highlight", error))?;
  let injections = injections
    .map(|text| Query::new(&language, text).map_err(|error| invalid("injection", error)))
    .transpose()?
    .and_then(Injections::new);
  let folds = folds
    .map(|text| Query::new(&language, text).map_err(|error| invalid("fold", error)))
    .transpose()?;
  let symbols = symbols
    .map(|text| Query::new(&language, text).map_err(|error| invalid("symbol", error)))
    .transpose()?
    .map(SymbolQuery::new);
  let kinds = query
    .capture_names()
    .iter()
    .map(|name| TokenKind::for_capture(name))
    .collect();
  Ok(Grammar {
    language,
    query,
    kinds,
    injections,
    folds,
    symbols,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::testing;

  fn definition(id: &str, extensions: &[&str], names: &[&str]) -> LanguageDefinition {
    LanguageDefinition {
      id: id.to_owned(),
      name: id.to_uppercase(),
      file_extensions: extensions.iter().map(|&e| e.to_owned()).collect(),
      file_names: names.iter().map(|&n| n.to_owned()).collect(),
      grammar: PathBuf::from("missing.wasm"),
      grammar_name: id.to_owned(),
      highlights: PathBuf::from("missing.scm"),
      injections: None,
      folds: None,
      symbols: None,
    }
  }

  #[test]
  fn detects_languages_from_file_names() {
    let (registry, problems) = Registry::new([
      definition("rust", &["rs"], &[]),
      definition("typescript", &["ts", "MTS"], &[]),
      definition("dts", &["d.ts"], &[]),
      definition("toml", &["toml"], &["Cargo.lock"]),
      definition("rust2", &["rs"], &[]),
      definition("rust", &["rs3"], &[]),
    ]);
    assert_eq!(problems.len(), 1, "{problems:?}");
    let detect = |name: &str| registry.for_path(Path::new(name)).map(Language::id);
    assert_eq!(detect("src/main.rs"), Some("rust"), "the first claim wins");
    assert_eq!(detect("vite.config.MTS"), Some("typescript"));
    assert_eq!(detect("app.ts"), Some("typescript"));
    assert_eq!(detect("lib.d.ts"), Some("dts"), "longer extensions win");
    assert_eq!(detect("cargo.lock"), Some("toml"));
    assert_eq!(detect("main.rs3"), None, "the duplicate id was left out");
    assert_eq!(detect("notes.txt"), None);
    assert_eq!(detect(".rs"), None, "a dot file has no extension");
    assert!(Registry::default().for_path(Path::new("main.rs")).is_none());

    let named = |name: &str| registry.for_name(name).map(Language::id);
    assert_eq!(named("typescript"), Some("typescript"));
    assert_eq!(
      named("mts"),
      Some("typescript"),
      "names are matched as extensions too"
    );
    assert_eq!(named("rs"), Some("rust"));
    assert_eq!(named("../x.rs"), None);
    assert_eq!(named(""), None);
  }

  #[test]
  fn every_example_grammar_and_query_compiles() {
    let registry = testing::registry();
    assert!(registry.languages().count() >= 9);
    for language in registry.languages() {
      let grammar = language
        .grammar()
        .unwrap_or_else(|error| panic!("{}: {error}", language.id()));
      assert!(
        grammar.kinds.iter().any(Option::is_some),
        "{} highlights something",
        language.id()
      );
    }
  }

  #[test]
  fn languages_are_equal_only_to_the_same_registration() {
    let (a, _) = Registry::new([definition("html", &["html"], &[])]);
    let (b, _) = Registry::new([definition("html", &["html"], &[])]);
    assert_eq!(a.get("html"), a.clone().get("html"));
    assert_ne!(a.get("html"), b.get("html"));
  }

  #[test]
  fn fold_support_is_known_without_loading_the_grammar() {
    let mut html = definition("html", &["html"], &[]);
    html.folds = Some(PathBuf::from("missing.scm"));
    let (registry, _) = Registry::new([html, definition("css", &["css"], &[])]);
    let html = registry.get("html").expect("registered");
    assert!(html.has_folds());
    assert!(html.0.grammar.get().is_none(), "not loaded");
    assert!(!registry.get("css").expect("registered").has_folds());
    // Once its grammar fails to load, it folds by indentation instead.
    assert!(html.grammar().is_err());
    assert!(!html.has_folds());
  }

  #[test]
  fn a_missing_grammar_is_an_error_not_a_panic() {
    let (registry, _) = Registry::new([definition("html", &["html"], &[])]);
    let error = registry
      .get("html")
      .expect("registered")
      .grammar()
      .err()
      .expect("missing file");
    assert!(error.to_string().contains("missing.wasm"), "{error}");
  }
}
