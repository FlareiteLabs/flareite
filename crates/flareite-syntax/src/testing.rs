//! The languages in `extensions/test-fixtures/languages/` (copies of the
//! official extensions' grammars and queries), for tests.

use std::path::Path;

use crate::languages::{LanguageDefinition, Registry};

/// (extension folder, id, file extensions, grammar file, grammar name,
/// highlight query, injection query). The fold query is `folds.scm` beside
/// the highlight query, if there is one.
type Example = (
  &'static str,
  &'static str,
  &'static [&'static str],
  &'static str,
  &'static str,
  &'static str,
  Option<&'static str>,
);

const EXAMPLES: &[Example] = &[
  (
    "rust",
    "rust",
    &["rs"],
    "tree-sitter-rust",
    "rust",
    "highlights.scm",
    None,
  ),
  (
    "typescript",
    "typescript",
    &["ts"],
    "tree-sitter-typescript",
    "typescript",
    "typescript/highlights.scm",
    None,
  ),
  (
    "typescript",
    "typescriptreact",
    &["tsx"],
    "tree-sitter-tsx",
    "tsx",
    "tsx/highlights.scm",
    None,
  ),
  (
    "javascript",
    "javascript",
    &["js"],
    "tree-sitter-javascript",
    "javascript",
    "highlights.scm",
    None,
  ),
  (
    "json",
    "json",
    &["json"],
    "tree-sitter-json",
    "json",
    "highlights.scm",
    None,
  ),
  (
    "toml",
    "toml",
    &["toml"],
    "tree-sitter-toml",
    "toml",
    "highlights.scm",
    None,
  ),
  (
    "python",
    "python",
    &["py"],
    "tree-sitter-python",
    "python",
    "highlights.scm",
    None,
  ),
  (
    "luau",
    "luau",
    &["luau"],
    "tree-sitter-luau",
    "luau",
    "highlights.scm",
    None,
  ),
  (
    "html-css",
    "html",
    &["html"],
    "tree-sitter-html",
    "html",
    "html/highlights.scm",
    Some("html/injections.scm"),
  ),
  (
    "html-css",
    "css",
    &["css"],
    "tree-sitter-css",
    "css",
    "css/highlights.scm",
    None,
  ),
  (
    "html-css",
    "svg",
    &["svg"],
    "tree-sitter-xml",
    "xml",
    "svg/highlights.scm",
    Some("svg/injections.scm"),
  ),
  (
    "markdown",
    "markdown",
    &["md"],
    "tree-sitter-markdown",
    "markdown",
    "markdown/highlights.scm",
    Some("markdown/injections.scm"),
  ),
  (
    "markdown",
    "markdown_inline",
    &[],
    "tree-sitter-markdown_inline",
    "markdown_inline",
    "markdown_inline/highlights.scm",
    Some("markdown_inline/injections.scm"),
  ),
];

/// All the example languages.
pub(crate) fn registry() -> Registry {
  build(true)
}

/// All the example languages, without their fold queries.
pub(crate) fn registry_without_folds() -> Registry {
  build(false)
}

fn build(folds: bool) -> Registry {
  let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/test-fixtures/languages");
  let definitions = EXAMPLES.iter().map(
    |&(folder, id, extensions, grammar, grammar_name, highlights, injections)| {
      let dir = root.join(folder);
      LanguageDefinition {
        id: id.to_owned(),
        name: id.to_owned(),
        file_extensions: extensions.iter().map(|&e| e.to_owned()).collect(),
        file_names: Vec::new(),
        grammar: dir.join("grammars").join(format!("{grammar}.wasm")),
        grammar_name: grammar_name.to_owned(),
        highlights: dir.join("queries").join(highlights),
        injections: injections.map(|path| dir.join("queries").join(path)),
        folds: folds
          .then(|| {
            dir
              .join("queries")
              .join(highlights.replace("highlights.scm", "folds.scm"))
          })
          .filter(|path| path.is_file()),
      }
    },
  );
  let (registry, problems) = Registry::new(definitions);
  assert!(problems.is_empty(), "{problems:?}");
  registry
}
