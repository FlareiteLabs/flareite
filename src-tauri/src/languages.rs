//! The languages the editor knows and their language servers, all from
//! extensions: Flareite has none built in, so without language extensions
//! every file is plain text.
//!
//! Rebuilt in the background whenever extensions change. A language whose
//! definition and files are unchanged keeps its registration, so its grammar
//! isn't compiled again; a changed one (say, an edited query after
//! restarting a development extension) gets a new registration, and open
//! documents of it are parsed again.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::SystemTime;

use flareite_core::{
  AutoClosingPair, CommentTokens, DEFAULT_AUTO_CLOSE_BEFORE, Document, LanguageChoice,
  LanguagePairs,
};
use flareite_extension_host::contributions::{
  LanguageContribution, LanguageFiles, LanguageServerContribution,
};
use flareite_lsp::{ServerCommand, ServerConfig};
use flareite_syntax::{Language, LanguageDefinition, Registry};
use flareite_tasks::Priority;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::lsp::LspState;
use crate::perf::Perf;

#[derive(Default)]
pub struct LanguagesState {
  current: RwLock<Current>,
  /// Held while rebuilding, so rebuilds don't overtake each other.
  rebuilding: Mutex<()>,
}

#[derive(Default)]
struct Current {
  registry: Arc<Registry>,
  /// What `registry` was built from, with when each file was modified.
  built_from: Vec<(LanguageDefinition, Vec<Option<SystemTime>>)>,
  servers: Vec<ServerConfig>,
  /// Each language's comment syntax, by language id.
  comments: HashMap<String, CommentTokens>,
  /// The pairs of the languages that declare any, by language id.
  pairs: Arc<PairsById>,
}

/// Languages' pairs, by language id.
pub type PairsById = HashMap<String, Arc<LanguagePairs>>;

impl LanguagesState {
  pub fn registry(&self) -> Arc<Registry> {
    self.read(|current| current.registry.clone())
  }

  /// The language of a file, if it has one.
  pub fn for_path(&self, path: &Path) -> Option<Language> {
    self.read(|current| current.registry.for_path(path).cloned())
  }

  /// The language of a document (see [`language_of`]).
  pub fn for_document(&self, document: &Document) -> Option<Language> {
    self.read(|current| language_of(&current.registry, document).cloned())
  }

  /// The comment syntax of a document's language; none for plain text.
  pub fn comments_for(&self, document: &Document) -> CommentTokens {
    self.read(|current| {
      language_of(&current.registry, document)
        .and_then(|language| current.comments.get(language.id()))
        .cloned()
        .unwrap_or_default()
    })
  }

  /// The pairs of the languages that declare brackets or auto-closing
  /// pairs, by language id.
  pub fn pairs(&self) -> Arc<PairsById> {
    self.read(|current| current.pairs.clone())
  }

  /// Language servers declared by extensions the user allowed to run them.
  pub fn servers(&self) -> Vec<ServerConfig> {
    self.read(|current| current.servers.clone())
  }

  fn read<T>(&self, f: impl FnOnce(&Current) -> T) -> T {
    f(&self.current.read().unwrap_or_else(PoisonError::into_inner))
  }
}

/// A document's language in `registry`: the one chosen for it, else the one
/// its file name suggests. `None` for plain text, including when the chosen
/// language is no longer registered.
pub fn language_of<'r>(registry: &'r Registry, document: &Document) -> Option<&'r Language> {
  match document.language_choice() {
    Some(LanguageChoice::PlainText) => None,
    Some(LanguageChoice::Language(id)) => registry.get(id),
    // A provided document goes by the name at the end of its address.
    None => document
      .path()
      .or_else(|| document.provided_name().map(Path::new))
      .and_then(|path| registry.for_path(path)),
  }
}

/// A language's pairs, with VS Code's defaults: the brackets auto-close
/// unless the auto-closing pairs are given, and those surround unless the
/// surrounding pairs are. `None` if it declares neither brackets nor
/// auto-closing pairs.
fn pairs_of(language: &LanguageContribution) -> Option<LanguagePairs> {
  let auto_closing: Vec<AutoClosingPair> = match &language.auto_closing_pairs {
    Some(pairs) => pairs
      .iter()
      .map(|pair| AutoClosingPair {
        open: pair.open.clone(),
        close: pair.close.clone(),
        not_in_string: pair.not_in_string,
        not_in_comment: pair.not_in_comment,
      })
      .collect(),
    None => language
      .brackets
      .iter()
      .map(|(open, close)| AutoClosingPair::new(open, close))
      .collect(),
  };
  if language.brackets.is_empty() && auto_closing.is_empty() {
    return None;
  }
  let surrounding = match &language.surrounding_pairs {
    Some(pairs) => pairs.clone(),
    None => auto_closing
      .iter()
      .map(|pair| (pair.open.clone(), pair.close.clone()))
      .collect(),
  }
  .into_iter()
  // Typing one character wraps: longer openers can't.
  .filter(|(open, _)| open.chars().count() == 1)
  .collect();
  Some(LanguagePairs {
    brackets: language.brackets.clone(),
    auto_closing,
    surrounding,
    auto_close_before: language
      .auto_close_before
      .clone()
      .unwrap_or_else(|| DEFAULT_AUTO_CLOSE_BEFORE.to_owned()),
  })
}

/// A language a document can be set to.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageDto {
  pub id: String,
  pub name: String,
  /// The file extensions it's for, without the dot.
  pub file_extensions: Vec<String>,
}

/// The languages a document can be set to, by name: those that files can
/// have, leaving out ones only embedded in others (Markdown's inline text).
#[tauri::command]
pub fn languages_list(state: State<'_, LanguagesState>) -> Vec<LanguageDto> {
  choices(&state.registry())
}

fn choices(registry: &Registry) -> Vec<LanguageDto> {
  let mut languages: Vec<LanguageDto> = registry
    .languages()
    .filter(|language| !language.file_extensions().is_empty() || !language.file_names().is_empty())
    .map(|language| LanguageDto {
      id: language.id().to_owned(),
      name: language.name().to_owned(),
      file_extensions: language.file_extensions().to_vec(),
    })
    .collect();
  languages.sort_by_cached_key(|language| language.name.to_lowercase());
  languages
}

/// Rebuilds the languages from the extensions in the background.
pub fn refresh(app: &AppHandle) {
  let task_app = app.clone();
  app.state::<Perf>().scheduler.spawn(
    "languages",
    Priority::High,
    Some("languages".into()),
    move |_| rebuild(&task_app),
  );
}

fn rebuild(app: &AppHandle) {
  let extensions = app.state::<ExtensionsState>();
  let Some(host) = extensions.host() else {
    return;
  };
  let state = app.state::<LanguagesState>();
  let _rebuilding = state
    .rebuilding
    .lock()
    .unwrap_or_else(PoisonError::into_inner);
  let languages = with_files(host.languages(), host.language_files());
  let mut comments = HashMap::new();
  let mut pairs = HashMap::new();
  for language in &languages {
    // As in the registry, the first language with an id wins.
    if comments.contains_key(&language.id) {
      continue;
    }
    comments.insert(
      language.id.clone(),
      CommentTokens {
        line: language.line_comment.clone(),
        block: language.block_comment.clone(),
      },
    );
    if let Some(declared) = pairs_of(language) {
      pairs.insert(language.id.clone(), Arc::new(declared));
    }
  }
  {
    let mut current = state
      .current
      .write()
      .unwrap_or_else(PoisonError::into_inner);
    current.comments = comments;
    current.pairs = Arc::new(pairs);
  }
  let built_from: Vec<_> = languages
    .into_iter()
    .map(|language| {
      let definition = definition(language);
      let stamps = stamps(&definition);
      (definition, stamps)
    })
    .collect();
  let servers: Vec<ServerConfig> = host
    .language_servers()
    .into_iter()
    .map(server_config)
    .collect();
  let (languages_changed, servers_changed) =
    state.read(|current| (current.built_from != built_from, current.servers != servers));

  if languages_changed {
    let (registry, problems) = Registry::new(built_from.iter().map(|(d, _)| d.clone()));
    for problem in problems {
      tracing::warn!(%problem, "an extension's language was left out");
    }
    tracing::info!(
      languages = ?built_from.iter().map(|(d, _)| &d.id).collect::<Vec<_>>(),
      "extension languages changed"
    );
    let mut current = state
      .current
      .write()
      .unwrap_or_else(PoisonError::into_inner);
    current.registry = Arc::new(registry);
    current.built_from = built_from;
  }
  if servers_changed {
    state
      .current
      .write()
      .unwrap_or_else(PoisonError::into_inner)
      .servers = servers;
  }
  // Restarts the servers that changed, stopping any no longer allowed, and
  // starts those that waited for their extension to activate.
  app.state::<LspState>().configure(app);
  if languages_changed {
    host.languages_changed();
    // Documents of a new language may now have a server: `configure`
    // syncs them.
    events::emit(app, AppEvent::LanguagesChanged);
  }
}

/// `languages` with the files other extensions add to them (as Luau adds
/// `.luaurc` to JSON). They go to the first language of the id, the one the
/// registry keeps; files for a language no extension declares stay plain
/// text.
fn with_files(
  mut languages: Vec<LanguageContribution>,
  files: Vec<LanguageFiles>,
) -> Vec<LanguageContribution> {
  for added in files {
    let Some(language) = languages
      .iter_mut()
      .find(|language| language.id == added.language)
    else {
      tracing::debug!(language = %added.language, "files were added to a language no extension declares");
      continue;
    };
    for extension in added.file_extensions {
      if !language.file_extensions.contains(&extension) {
        language.file_extensions.push(extension);
      }
    }
    for name in added.file_names {
      if !language.file_names.contains(&name) {
        language.file_names.push(name);
      }
    }
  }
  languages
}

fn definition(language: LanguageContribution) -> LanguageDefinition {
  LanguageDefinition {
    id: language.id,
    name: language.name,
    file_extensions: language.file_extensions,
    file_names: language.file_names,
    grammar: language.grammar,
    grammar_name: language.grammar_name,
    highlights: language.highlights,
    injections: language.injections,
    folds: language.folds,
  }
}

/// When each of a language's files was last modified.
fn stamps(definition: &LanguageDefinition) -> Vec<Option<SystemTime>> {
  [&definition.grammar, &definition.highlights]
    .into_iter()
    .chain(&definition.injections)
    .chain(&definition.folds)
    .map(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())
    .collect()
}

fn server_config(server: LanguageServerContribution) -> ServerConfig {
  ServerConfig {
    name: server.name,
    command: ServerCommand {
      program: server.command,
      args: server.args,
    },
    languages: server.languages,
    root_markers: server.root_markers,
    settings: server.settings,
  }
}

/// The languages in `extensions/test-fixtures/languages/` (e.g. `rust`),
/// loaded from their manifests, for tests.
#[cfg(test)]
pub fn examples(folders: &[&str]) -> Registry {
  let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../extensions/test-fixtures/languages");
  let mut languages = Vec::new();
  let mut files = Vec::new();
  for folder in folders {
    let dir = root.join(folder);
    let text = std::fs::read_to_string(dir.join(flareite_extension_host::manifest::MANIFEST_FILE))
      .unwrap_or_else(|error| panic!("{folder}: {error}"));
    let manifest =
      flareite_extension_host::manifest::parse(&text, &dir, &semver::Version::new(0, 1, 0), true)
        .unwrap_or_else(|error| panic!("{folder}: {error}"));
    let contributions = manifest.contributions;
    files.extend(contributions.language_files);
    languages.extend(
      contributions
        .languages
        .into_iter()
        .map(|language| LanguageContribution {
          grammar: dir.join(&language.grammar),
          highlights: dir.join(&language.highlights),
          injections: language.injections.as_ref().map(|path| dir.join(path)),
          folds: language.folds.as_ref().map(|path| dir.join(path)),
          ..language
        }),
    );
  }
  let definitions = with_files(languages, files).into_iter().map(definition);
  let (registry, problems) = Registry::new(definitions);
  assert!(problems.is_empty(), "{problems:?}");
  registry
}

#[cfg(test)]
mod tests {
  use flareite_core::{Editor, EditorConfig};

  use super::*;

  #[test]
  fn extensions_add_files_to_other_extensions_languages() {
    // Luau's manifest says .luaurc is JSON; JSON loads after it.
    let registry = examples(&["luau", "json"]);
    let id = |path: &str| {
      registry
        .for_path(Path::new(path))
        .map(|l| l.id().to_owned())
    };
    assert_eq!(id("project/.luaurc").as_deref(), Some("json"));
    assert_eq!(id("project/a.json").as_deref(), Some("json"));
    assert_eq!(id("project/a.luau").as_deref(), Some("luau"));
    // Without JSON, it's plain text.
    let alone = examples(&["luau"]);
    assert!(alone.for_path(Path::new("project/.luaurc")).is_none());
  }

  /// The pairs a fixture's language declares.
  fn pairs_in(folder: &str, id: &str) -> Option<LanguagePairs> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
      .join("../extensions/test-fixtures/languages")
      .join(folder);
    let text = std::fs::read_to_string(dir.join(flareite_extension_host::manifest::MANIFEST_FILE))
      .expect("manifest");
    let manifest =
      flareite_extension_host::manifest::parse(&text, &dir, &semver::Version::new(0, 1, 0), true)
        .expect("valid");
    let language = manifest
      .contributions
      .languages
      .iter()
      .find(|language| language.id == id)
      .expect("declared");
    pairs_of(language)
  }

  #[test]
  fn languages_declare_their_pairs() {
    let rust = pairs_in("rust", "rust").expect("pairs");
    assert_eq!(rust.brackets.len(), 3);
    let quote = rust
      .auto_closing
      .iter()
      .find(|pair| pair.open == "\"")
      .expect("quotes");
    assert!(quote.not_in_string && !quote.not_in_comment);
    // `/*` auto-closes, but only single characters surround.
    assert!(rust.auto_closing.iter().any(|pair| pair.open == "/*"));
    assert!(rust.surrounding.iter().all(|(open, _)| open.len() == 1));
    assert_eq!(rust.auto_close_before, DEFAULT_AUTO_CLOSE_BEFORE);
    let typescript = pairs_in("typescript", "typescript").expect("pairs");
    assert!(typescript.auto_close_before.contains('`'));
    // Markdown's inline text declares none: it's Markdown's.
    assert!(pairs_in("markdown", "markdown_inline").is_none());
  }

  #[test]
  fn pairs_default_as_in_vs_code() {
    let mut language = flareite_extension_host::contributions::LanguageContribution {
      id: "x".into(),
      name: "X".into(),
      file_extensions: Vec::new(),
      file_names: Vec::new(),
      grammar: "x.wasm".into(),
      grammar_name: "x".into(),
      highlights: "x.scm".into(),
      injections: None,
      folds: None,
      line_comment: None,
      block_comment: None,
      brackets: Vec::new(),
      auto_closing_pairs: None,
      surrounding_pairs: None,
      auto_close_before: None,
    };
    assert!(pairs_of(&language).is_none());
    language.brackets = vec![("{".into(), "}".into()), ("begin".into(), "end".into())];
    let pairs = pairs_of(&language).expect("pairs");
    // The brackets auto-close, and those surround.
    assert_eq!(pairs.auto_closing.len(), 2);
    assert_eq!(pairs.surrounding, vec![("{".to_owned(), "}".to_owned())]);
  }

  #[test]
  fn documents_have_the_language_chosen_for_them_else_their_files() {
    let registry = examples(&["html-css"]);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("icon.svg");
    std::fs::write(&path, "<svg/>").expect("writes");
    let mut editor = Editor::new(EditorConfig::default());
    let loaded = flareite_workspace::load_text_file(&path).expect("loads");
    let file = editor.open(path, loaded);
    let untitled = editor.create_untitled();
    let mut language = |id, choice| {
      let document = editor.document_mut(id).expect("open");
      document.set_language_choice(choice);
      language_of(&registry, document).map(|language| language.id().to_owned())
    };
    let chosen = |id: &str| Some(LanguageChoice::Language(id.to_owned()));

    assert_eq!(language(file, None).as_deref(), Some("svg"));
    assert_eq!(language(file, chosen("html")).as_deref(), Some("html"));
    assert_eq!(language(file, Some(LanguageChoice::PlainText)), None);
    assert_eq!(language(untitled, None), None);
    assert_eq!(language(untitled, chosen("css")).as_deref(), Some("css"));
    // A language that's gone (its extension was removed) leaves plain text.
    assert_eq!(language(untitled, chosen("cobol")), None);
  }

  #[test]
  fn languages_to_choose_from_are_those_files_can_have_by_name() {
    let registry = examples(&["markdown", "html-css"]);
    let names: Vec<String> = choices(&registry)
      .into_iter()
      .map(|language| language.name)
      .collect();
    // Not Markdown's inline text, which is only embedded in Markdown.
    assert_eq!(names, ["CSS", "HTML", "Markdown", "SVG"]);
  }
}
