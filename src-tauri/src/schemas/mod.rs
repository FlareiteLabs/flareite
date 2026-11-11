//! Schemas for documents: which apply to a document, loading them, and
//! checking documents with them (the `flareite-schema` crate does the work
//! on a document), and answering what they say at a place in one.
//!
//! Flareite reads no configuration language itself: extensions parse their
//! languages' documents into trees of values (`schemas::set-document`, API
//! 1.18), which are checked here, and write what the schemas say in their
//! languages' syntax, for completion and hover (`schemas::properties`,
//! `values`, and `describe`).
//!
//! **Which schemas apply**, most specific first:
//!
//! 1. The one a document names (`"$schema"`, TOML's `#:schema` comment, a
//!    YAML modeline: as its extension reads it). It alone applies.
//! 2. Otherwise every match among the open folder's
//!    `[[schemas.associations]]` (once it's trusted), the user's, those
//!    extensions provide, and Flareite's own: `settings.toml` files (the
//!    user's and folders'), extensions' `extension.toml`, and `.tosd` files
//!    (completed and described by TOML Schema's own schema, but checked by
//!    loading them, which reports more and is much faster).
//! 3. Otherwise the first match in the SchemaStore catalog, if
//!    `schemas.catalog` is on.
//!
//! **Addresses.** Schemas are known by address: `file:` URLs, `https:`
//! (and `http:`) addresses, downloaded and kept in the cache folder
//! (`fetch`), `flareite://schemas/<name>` for Flareite's, and
//! `flareite://extensions/<id>/schemas/<id>` for those extensions' code
//! sets.
//!
//! **Checking** runs on a thread of its own, a moment after typing pauses,
//! on the latest tree an extension set for a document; the problems found
//! are kept with the version of the text they're for and move with later
//! edits until it runs again. Queries use the schemas already loaded, so
//! they never wait for the web.

mod fetch;
pub mod installed;
mod matcher;

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use flareite_core::{Document, DocumentId};
use flareite_extension_host::schemas::{Provided, SchemaAnswer, SchemaFormat, SchemaQuery};
use flareite_schema::document::{Document as Tree, Segment};
use flareite_schema::{Schema, Severity};
use flareite_text::{TextSnapshot, Version, map_offset_through};
use tauri::{AppHandle, Manager};

use crate::editor::EditorState;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::languages::LanguagesState;
use crate::lsp::{DiagnosticDto, DiagnosticMessageDto, FileDiagnostic};
use crate::settings::SettingsState;
use fetch::{Fetched, Fetcher};
use matcher::Matcher;

/// Flareite's own schemas.
const EXTENSION_SCHEMA: &str = "flareite://schemas/extension.tosd";
const SETTINGS_SCHEMA: &str = "flareite://schemas/settings.tosd";
const TOML_SCHEMA_SCHEMA: &str = "flareite://schemas/toml-schema.tosd";
const EXTENSION_PREFIX: &str = "flareite://extensions/";
/// The SchemaStore catalog: well-known files and their schemas.
const CATALOG: &str = "https://www.schemastore.org/api/json/catalog.json";
/// Downloaded schemas are downloaded again once this old.
const REFRESH: Duration = Duration::from_secs(24 * 60 * 60);
const CATALOG_REFRESH: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Largest document checked.
const MAX_CHECK_BYTES: usize = 4 * 1024 * 1024;
/// Largest schema file read.
const MAX_SCHEMA_BYTES: u64 = 8 * 1024 * 1024;
/// Most documents a JSON Schema's references bring in.
const MAX_REFERENCED: usize = 64;
/// How long typing must pause before a document is checked.
const PAUSE: Duration = Duration::from_millis(250);

/// A loaded schema.
pub struct Loaded {
  schema: Schema,
  /// What it's called in messages: its file's name, or its id.
  name: String,
}

#[derive(Clone)]
enum Slot {
  Ready(Arc<Loaded>),
  /// Being downloaded.
  Pending,
  Failed(String),
}

/// Where an association comes from, in order of precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
  Folder,
  User,
  Extension,
}

struct Association {
  files: Vec<String>,
  address: String,
  #[allow(dead_code)]
  origin: Origin,
}

struct Associations {
  list: Vec<Association>,
  matcher: Matcher,
}

impl Associations {
  fn new(list: Vec<Association>) -> Self {
    let matcher = Matcher::new(list.iter().map(|association| association.files.as_slice()));
    Self { list, matcher }
  }

  fn matching(&self, path: &Path) -> Vec<String> {
    let mut addresses: Vec<String> = Vec::new();
    for index in self.matcher.matches(path) {
      let address = &self.list[index].address;
      if !addresses.contains(address) {
        addresses.push(address.clone());
      }
    }
    addresses
  }
}

/// A problem found in a document, at byte offsets in its text.
#[derive(Debug, Clone)]
struct Stored {
  range: Range<usize>,
  /// 1 error, 2 warning, 3 information, 4 hint.
  severity: u8,
  message: String,
  source: String,
}

/// A document's tree, as an extension parsed it, with the text it's for.
struct Parsed {
  /// The extension's id.
  extension: String,
  /// What its own problems are labeled with: the language's name.
  source: String,
  tree: Tree,
  text: TextSnapshot,
  path: Option<PathBuf>,
}

struct Record {
  /// The latest tree, if an extension set one.
  parsed: Option<Arc<Parsed>>,
  /// The version last checked, and the schemas' generation then.
  checked: Option<(Version, u64)>,
  /// A check waiting to run, for this version and generation.
  queued: Option<(Version, u64)>,
  /// The version `diagnostics`' ranges are in.
  mapped: Version,
  diagnostics: Vec<Stored>,
}

#[derive(Default)]
struct Inner {
  /// Bumped when which schemas apply, or what they say, may have changed:
  /// every document is checked again.
  generation: u64,
  enabled: bool,
  schemas: HashMap<String, Slot>,
  associations: Option<Arc<Associations>>,
  catalog: Option<Arc<Associations>>,
  documents: HashMap<DocumentId, Record>,
}

/// A document to check.
struct Job {
  id: DocumentId,
  generation: u64,
  parsed: Arc<Parsed>,
}

#[derive(Default)]
pub struct SchemaState {
  inner: Mutex<Inner>,
  /// Every command, as the frontend registered them, for the settings'
  /// schema.
  commands: Mutex<Vec<flareite_schema::builtin::Named>>,
  jobs: OnceLock<Sender<Job>>,
  fetcher: OnceLock<Arc<Fetcher>>,
}

/// Starts checking documents, keeping downloaded schemas in the cache
/// folder.
pub fn init(app: &AppHandle) {
  let state = app.state::<SchemaState>();
  let dir = app
    .path()
    .app_cache_dir()
    .ok()
    .map(|dir| dir.join("schemas"));
  let user_agent = format!(
    "Flareite/{} (+https://github.com/FlareiteLabs/flareite)",
    app.package_info().version
  );
  let _ = state.fetcher.set(Arc::new(Fetcher::new(dir, user_agent)));
  state.lock().enabled = app.state::<SettingsState>().get().schemas.enabled;
  let (sender, receiver) = mpsc::channel();
  if state.jobs.set(sender).is_err() {
    return;
  }
  let app = app.clone();
  let spawned = std::thread::Builder::new()
    .name("schemas".into())
    .spawn(move || work(&app, &receiver));
  if let Err(error) = spawned {
    tracing::error!(%error, "could not start checking documents against schemas");
  }
}

fn severity(severity: Severity) -> u8 {
  match severity {
    Severity::Error => 1,
    Severity::Warning => 2,
    Severity::Information => 3,
    Severity::Hint => 4,
  }
}

impl SchemaState {
  fn lock(&self) -> MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Moves the problems kept for `document` through edits since they were
  /// found. Call with the editor locked.
  pub fn sync(&self, document: &Document) {
    let buffer = document.buffer();
    let version = buffer.version();
    let mut inner = self.lock();
    let Some(record) = inner.documents.get_mut(&document.id()) else {
      return;
    };
    if record.mapped != version {
      match buffer.edits_since(record.mapped) {
        Some(edits) => {
          for diagnostic in &mut record.diagnostics {
            let start = map_offset_through(&edits, diagnostic.range.start, true);
            let end = map_offset_through(&edits, diagnostic.range.end, false).max(start);
            diagnostic.range = start..end;
          }
        }
        None => record.diagnostics.clear(),
      }
      record.mapped = version;
    }
  }

  /// Keeps the tree an extension set for a document, and schedules
  /// checking it. A tree for a version the document has moved past is
  /// dropped: the extension sets one for the newer text too.
  fn set_tree(&self, app: &AppHandle, extension: &str, id: DocumentId, version: u64, tree: Tree) {
    let opened = {
      let state = app.state::<EditorState>();
      let editor = state.lock();
      editor.document(id).ok().and_then(|document| {
        let buffer = document.buffer();
        (buffer.version().get() == version).then(|| {
          let source = app
            .state::<LanguagesState>()
            .for_document(document)
            .map_or_else(
              || extension.to_owned(),
              |language| language.name().to_owned(),
            );
          (
            buffer.snapshot(),
            document.path().map(Path::to_path_buf),
            source,
          )
        })
      })
    };
    let Some((text, path, source)) = opened else {
      return;
    };
    let parsed = Arc::new(Parsed {
      extension: extension.to_owned(),
      source,
      tree,
      text,
      path,
    });
    let mut inner = self.lock();
    let mapped = parsed.text.version();
    let record = inner.documents.entry(id).or_insert_with(|| Record {
      parsed: None,
      checked: None,
      queued: None,
      mapped,
      diagnostics: Vec::new(),
    });
    record.parsed = Some(parsed);
    self.queue(&mut inner, id);
  }

  /// Schedules a check of document `id`'s tree, unless its problems are
  /// up to date or one is waiting.
  fn queue(&self, inner: &mut Inner, id: DocumentId) {
    let (enabled, generation) = (inner.enabled, inner.generation);
    let Some(record) = inner.documents.get_mut(&id) else {
      return;
    };
    let Some(parsed) = record.parsed.clone() else {
      return;
    };
    if !enabled {
      record.diagnostics.clear();
      record.checked = None;
      return;
    }
    let wanted = Some((parsed.text.version(), generation));
    if record.checked != wanted && record.queued != wanted {
      record.queued = wanted;
      if let Some(jobs) = self.jobs.get() {
        let _ = jobs.send(Job {
          id,
          generation,
          parsed,
        });
      }
    }
  }

  /// Forgets a document's tree, and its problems.
  fn clear(&self, app: &AppHandle, id: DocumentId) {
    let removed = self.lock().documents.remove(&id).is_some();
    if removed {
      events::emit(
        app,
        AppEvent::DiagnosticsChanged {
          document_id: id.get(),
        },
      );
    }
  }

  /// The problems on `lines`, for a view. Call with the editor locked,
  /// after [`Self::sync`].
  pub fn view(&self, document: &Document, lines: Range<usize>) -> Vec<DiagnosticDto> {
    let inner = self.lock();
    let Some(record) = inner.documents.get(&document.id()) else {
      return Vec::new();
    };
    let buffer = document.buffer();
    if record.mapped != buffer.version() || lines.is_empty() {
      return Vec::new();
    }
    let visible = buffer.line_start(lines.start)..buffer.line_end(lines.end.saturating_sub(1));
    record
      .diagnostics
      .iter()
      .filter(|d| d.range.start <= visible.end && d.range.end >= visible.start)
      .filter(|d| d.range.end <= buffer.len())
      .map(|d| DiagnosticDto {
        start: buffer.offset_to_utf16(d.range.start).into(),
        end: buffer.offset_to_utf16(d.range.end).into(),
        severity: d.severity,
      })
      .collect()
  }

  /// A document's problems, for the Problems view. Call with the editor
  /// locked, after [`Self::sync`].
  pub fn document_diagnostics(&self, document: &Document) -> Vec<FileDiagnostic> {
    let inner = self.lock();
    let Some(record) = inner.documents.get(&document.id()) else {
      return Vec::new();
    };
    let buffer = document.buffer();
    if record.mapped != buffer.version() {
      return Vec::new();
    }
    record
      .diagnostics
      .iter()
      .filter(|d| d.range.end <= buffer.len())
      .map(|d| FileDiagnostic {
        start: buffer.offset_to_utf16(d.range.start).into(),
        end: buffer.offset_to_utf16(d.range.end).into(),
        severity: d.severity,
        message: d.message.clone(),
        source: Some(d.source.clone()),
      })
      .collect()
  }

  /// The messages of the problems at `offset`, for hover.
  pub fn messages_at(&self, document: &Document, offset: usize) -> Vec<DiagnosticMessageDto> {
    let inner = self.lock();
    let Some(record) = inner.documents.get(&document.id()) else {
      return Vec::new();
    };
    if record.mapped != document.buffer().version() {
      return Vec::new();
    }
    record
      .diagnostics
      .iter()
      .filter(|d| d.range.start <= offset && offset <= d.range.end)
      .map(|d| DiagnosticMessageDto {
        severity: d.severity,
        message: d.message.clone(),
        source: Some(d.source.clone()),
      })
      .collect()
  }

  /// Forgets a closed document.
  pub fn closed(&self, id: DocumentId) {
    self.lock().documents.remove(&id);
  }

  /// Keeps what a check found, unless the document or the schemas changed
  /// meanwhile.
  fn store(&self, job: &Job, diagnostics: Vec<Stored>) -> bool {
    let mut inner = self.lock();
    let generation = inner.generation;
    let Some(record) = inner.documents.get_mut(&job.id) else {
      return false;
    };
    let version = job.parsed.text.version();
    let current = record
      .parsed
      .as_ref()
      .is_some_and(|parsed| Arc::ptr_eq(parsed, &job.parsed));
    if job.generation != generation || !current {
      return false;
    }
    record.checked = Some((version, job.generation));
    if record.queued == record.checked {
      record.queued = None;
    }
    // Kept in the checked text's terms; moved on at the next view.
    record.mapped = version;
    record.diagnostics = diagnostics;
    true
  }

  /// Every schema may say something else now: check every document again.
  /// Doesn't lock the editor.
  fn invalidate(&self, app: &AppHandle) {
    let ids: Vec<DocumentId> = {
      let mut inner = self.lock();
      inner.generation += 1;
      inner.schemas.clear();
      inner.associations = None;
      inner.enabled = app.state::<SettingsState>().get().schemas.enabled;
      let ids: Vec<DocumentId> = inner.documents.keys().copied().collect();
      for &id in &ids {
        self.queue(&mut inner, id);
      }
      ids
    };
    for id in ids {
      events::emit(
        app,
        AppEvent::DiagnosticsChanged {
          document_id: id.get(),
        },
      );
    }
  }

  fn fetcher(&self) -> Option<Arc<Fetcher>> {
    self.fetcher.get().cloned()
  }
}

/// An extension parsed an open document for schemas, or forgot it
/// (`schemas::set-document`, `clear-document`). Called from extension
/// threads, without the editor locked.
pub fn document_parsed(
  app: &AppHandle,
  extension: &str,
  id: DocumentId,
  version: u64,
  tree: Option<Tree>,
) {
  let state = app.state::<SchemaState>();
  match tree {
    Some(tree) => state.set_tree(app, extension, id, version, tree),
    None => state.clear(app, id),
  }
}

/// An extension stopped: the trees it set are gone, and their problems.
pub fn extension_forgotten(app: &AppHandle, extension: &str) {
  let state = app.state::<SchemaState>();
  let ids: Vec<DocumentId> = state
    .lock()
    .documents
    .iter()
    .filter(|(_, record)| {
      record
        .parsed
        .as_ref()
        .is_some_and(|parsed| parsed.extension == extension)
    })
    .map(|(id, _)| *id)
    .collect();
  for id in ids {
    state.clear(app, id);
  }
}

/// Settings that decide which schemas apply changed.
pub fn settings_changed(app: &AppHandle) {
  app.state::<SchemaState>().invalidate(app);
}

/// Extensions were loaded, changed, or stopped, or their code changed the
/// schemas it provides: their schemas, and the settings' schema, change.
pub fn extensions_changed(app: &AppHandle) {
  app.state::<SchemaState>().invalidate(app);
}

/// Files changed on disk (or were saved): schemas read from them are read
/// again.
pub fn files_changed(app: &AppHandle, paths: &[PathBuf]) {
  let state = app.state::<SchemaState>();
  let affected = {
    let inner = state.lock();
    paths.iter().any(|path| {
      url::Url::from_file_path(path).is_ok_and(|url| inner.schemas.contains_key(url.as_str()))
    })
  };
  if affected {
    state.invalidate(app);
  }
}

/// Checks documents as they come, a moment after typing pauses.
fn work(app: &AppHandle, jobs: &Receiver<Job>) {
  loop {
    let Ok(first) = jobs.recv() else {
      return;
    };
    let mut latest: HashMap<DocumentId, Job> = HashMap::from([(first.id, first)]);
    let until = Instant::now() + PAUSE;
    loop {
      match jobs.recv_timeout(until.saturating_duration_since(Instant::now())) {
        Ok(job) => {
          latest.insert(job.id, job);
        }
        Err(RecvTimeoutError::Timeout) => break,
        Err(RecvTimeoutError::Disconnected) => return,
      }
    }
    for job in latest.into_values() {
      let started = Instant::now();
      let diagnostics = check(app, &job.parsed);
      if app.state::<SchemaState>().store(&job, diagnostics) {
        events::emit(
          app,
          AppEvent::DiagnosticsChanged {
            document_id: job.id.get(),
          },
        );
      }
      let elapsed = started.elapsed();
      if elapsed > Duration::from_millis(500) {
        tracing::info!(?elapsed, document = %job.id, "checking a document against schemas was slow");
      }
    }
  }
}

/// The problems in a document.
fn check(app: &AppHandle, parsed: &Parsed) -> Vec<Stored> {
  if parsed.text.len() > MAX_CHECK_BYTES {
    return Vec::new();
  }
  let text = parsed.text.text();
  let mut found: Vec<Stored> = stored(
    flareite_schema::parse_problems(&parsed.tree),
    &parsed.source,
  )
  .collect();
  if parsed.tree.has_errors() {
    return found;
  }
  let resolution = resolve(app, parsed);
  if resolution.tosd {
    found.extend(stored(
      flareite_schema::check_tosd(&parsed.tree, &text),
      "TOML Schema",
    ));
    return found;
  }
  let first_line = {
    let start = text.len() - text.trim_start().len();
    start
      ..text[start..]
        .find('\n')
        .map_or(text.len(), |end| start + end)
  };
  for address in &resolution.addresses {
    match load(app, address) {
      Slot::Ready(loaded) => {
        if let Some(error) = loaded.schema.error() {
          found.push(Stored {
            range: first_line.clone(),
            severity: 2,
            message: format!("The schema {} can't check this file: {error}", loaded.name),
            source: loaded.name.clone(),
          });
          continue;
        }
        found.extend(stored(
          flareite_schema::check(&parsed.tree, &text, &[&loaded.schema]),
          &loaded.name,
        ));
      }
      Slot::Pending => {}
      Slot::Failed(error) => found.push(Stored {
        range: first_line.clone(),
        severity: 2,
        message: format!("The schema {address} can't be used: {error}"),
        source: parsed.source.clone(),
      }),
    }
  }
  if let Some(path) = &parsed.path
    && crate::settings::is_folder_settings_file(app, path)
  {
    folder_settings_problems(app, &parsed.tree, first_line, &mut found);
  }
  found.sort_by_key(|d| (d.range.start, d.severity));
  found
}

/// Problems a schema found, kept with where they came from.
fn stored(
  diagnostics: Vec<flareite_schema::Diagnostic>,
  source: &str,
) -> impl Iterator<Item = Stored> + '_ {
  diagnostics.into_iter().map(move |diagnostic| Stored {
    range: diagnostic.range,
    severity: severity(diagnostic.severity),
    message: diagnostic.message,
    source: source.to_owned(),
  })
}

/// What a folder's settings file sets that doesn't apply.
fn folder_settings_problems(
  app: &AppHandle,
  tree: &Tree,
  first_line: Range<usize>,
  found: &mut Vec<Stored>,
) {
  let Some(folder) = app.state::<SettingsState>().folder() else {
    return;
  };
  if !folder.trusted {
    found.push(Stored {
      range: first_line,
      severity: 3,
      message: "This folder isn't trusted, so its settings don't apply.".into(),
      source: "Settings".into(),
    });
  }
  let Some(part) = tree.parts.first() else {
    return;
  };
  for key in &folder.ignored {
    let path: Vec<Segment> = key.split('.').map(Segment::key).collect();
    let (node, taken) = part.tree.find(&path);
    if taken == path.len()
      && let Some(span) = &node.key_span
    {
      found.push(Stored {
        range: span.clone(),
        severity: 2,
        message: "Only your own settings can change this; it's ignored here.".into(),
        source: "Settings".into(),
      });
    }
  }
}

/// The schemas that apply to a document.
struct Resolution {
  addresses: Vec<String>,
  /// A TOML Schema: checked by loading it, completed by TOML Schema's
  /// schema.
  tosd: bool,
}

fn is_tosd(path: Option<&Path>) -> bool {
  path
    .and_then(Path::extension)
    .is_some_and(|extension| extension.eq_ignore_ascii_case("tosd"))
}

fn file_name_is(path: &Path, name: &str) -> bool {
  path
    .file_name()
    .is_some_and(|file| file.eq_ignore_ascii_case(name))
}

/// A path or an address as an address: relative paths are relative to
/// `base`.
fn address_of(reference: &str, base: Option<&Path>) -> Option<String> {
  let reference = reference.trim();
  if reference.contains("://") {
    return Some(reference.to_owned());
  }
  let path = Path::new(reference);
  let path = if path.is_absolute() {
    path.to_path_buf()
  } else {
    base?.join(path)
  };
  let path = crate::paths::normalize(&path);
  url::Url::from_file_path(&path).ok().map(String::from)
}

fn resolve(app: &AppHandle, parsed: &Parsed) -> Resolution {
  let none = Resolution {
    addresses: Vec::new(),
    tosd: false,
  };
  let state = app.state::<SchemaState>();
  if !state.lock().enabled {
    return none;
  }
  let path = parsed.path.as_deref();
  if is_tosd(path) {
    return Resolution {
      addresses: vec![TOML_SCHEMA_SCHEMA.to_owned()],
      tosd: true,
    };
  }
  if let Some(named) = &parsed.tree.named_schema {
    let base = path.and_then(Path::parent);
    if let Some(address) = address_of(named, base) {
      return Resolution {
        addresses: vec![address],
        tosd: false,
      };
    }
  }
  let Some(path) = path else {
    return none;
  };
  let mut addresses = associations(app).matching(path);
  let settings = crate::settings::is_settings_file(app, path)
    || (file_name_is(path, crate::settings::folder::FILE_NAME)
      && path
        .parent()
        .is_some_and(|dir| file_name_is(dir, crate::settings::folder::DIR)));
  if settings {
    addresses.push(SETTINGS_SCHEMA.to_owned());
  }
  // Zed's extensions have an extension.toml too, with a schema_version.
  if file_name_is(path, flareite_extension_host::MANIFEST_FILE)
    && !parsed
      .tree
      .parts
      .iter()
      .any(|part| part.tree.get("schema_version").is_some())
  {
    addresses.push(EXTENSION_SCHEMA.to_owned());
  }
  if addresses.is_empty()
    && app.state::<SettingsState>().get().schemas.catalog
    && let Some(catalog) = catalog(app)
  {
    addresses.extend(catalog.matching(path).into_iter().take(1));
  }
  Resolution {
    addresses,
    tosd: false,
  }
}

/// The associations of settings and extensions, built when first needed
/// after a change.
fn associations(app: &AppHandle) -> Arc<Associations> {
  let state = app.state::<SchemaState>();
  if let Some(associations) = &state.lock().associations {
    return associations.clone();
  }
  let settings = app.state::<SettingsState>();
  let mut list = Vec::new();
  if let Some(folder) = settings.folder()
    && folder.in_effect()
  {
    for association in &folder.associations {
      if let Some(address) = address_of(&association.schema, Some(&folder.root)) {
        list.push(Association {
          files: association.files.clone(),
          address,
          origin: Origin::Folder,
        });
      }
    }
  }
  let config_dir = settings
    .path()
    .and_then(Path::parent)
    .map(Path::to_path_buf);
  for association in &settings.user().schemas.associations {
    if let Some(address) = address_of(&association.schema, config_dir.as_deref()) {
      list.push(Association {
        files: association.files.clone(),
        address,
        origin: Origin::User,
      });
    }
  }
  if let Some(host) = app.state::<ExtensionsState>().host() {
    for provided in host.schemas() {
      let address = match &provided.source {
        Provided::File(path) => url::Url::from_file_path(path).ok().map(String::from),
        Provided::Url(url) => Some(url.clone()),
        Provided::Content { id, .. } => Some(format!(
          "{EXTENSION_PREFIX}{}/schemas/{id}",
          provided.extension
        )),
      };
      if let Some(address) = address {
        list.push(Association {
          files: provided.files.clone(),
          address,
          origin: Origin::Extension,
        });
      }
    }
  }
  let associations = Arc::new(Associations::new(list));
  state.lock().associations = Some(associations.clone());
  associations
}

/// The SchemaStore catalog's associations, if it's downloaded (else it's
/// downloaded now, and documents are checked again when it is).
fn catalog(app: &AppHandle) -> Option<Arc<Associations>> {
  let state = app.state::<SchemaState>();
  if let Some(catalog) = &state.lock().catalog {
    return Some(catalog.clone());
  }
  let fetcher = state.fetcher()?;
  let done = {
    let app = app.clone();
    move || {
      let state = app.state::<SchemaState>();
      state.lock().catalog = None;
      state.invalidate(&app);
    }
  };
  let Fetched::Ready(text) = fetcher.get(CATALOG, CATALOG_REFRESH, done) else {
    return None;
  };
  let json: serde_json::Value = serde_json::from_str(&text).ok()?;
  let list: Vec<Association> = json
    .get("schemas")?
    .as_array()?
    .iter()
    .filter_map(|entry| {
      let address = entry.get("url")?.as_str()?.to_owned();
      let files: Vec<String> = entry
        .get("fileMatch")?
        .as_array()?
        .iter()
        .filter_map(|pattern| pattern.as_str().map(str::to_owned))
        .collect();
      (!files.is_empty()).then_some(Association {
        files,
        address,
        origin: Origin::Extension,
      })
    })
    .collect();
  tracing::info!(schemas = list.len(), "read the SchemaStore catalog");
  let catalog = Arc::new(Associations::new(list));
  state.lock().catalog = Some(catalog.clone());
  Some(catalog)
}

/// The schema at `address`: loaded, being downloaded, or why it can't be.
fn load(app: &AppHandle, address: &str) -> Slot {
  let state = app.state::<SchemaState>();
  let generation = {
    let inner = state.lock();
    match inner.schemas.get(address) {
      Some(Slot::Pending) | None => {}
      Some(slot) => return slot.clone(),
    }
    inner.generation
  };
  let slot = match load_now(app, address) {
    Ok(Some(loaded)) => Slot::Ready(Arc::new(loaded)),
    Ok(None) => Slot::Pending,
    Err(error) => {
      tracing::info!(%address, %error, "a schema can't be loaded");
      Slot::Failed(error)
    }
  };
  let mut inner = state.lock();
  if inner.generation == generation {
    inner.schemas.insert(address.to_owned(), slot.clone());
  }
  slot
}

/// What an address names, by its last part.
fn name_of(address: &str) -> String {
  address
    .trim_end_matches('/')
    .rsplit('/')
    .next()
    .filter(|name| !name.is_empty())
    .unwrap_or(address)
    .to_owned()
}

/// The text at an address: a file, or a document on the web (`None` while
/// it's downloaded).
fn read(app: &AppHandle, address: &str) -> Result<Option<Arc<str>>, String> {
  if address.starts_with("https://") || address.starts_with("http://") {
    let fetcher = app
      .state::<SchemaState>()
      .fetcher()
      .ok_or("schemas aren't downloaded")?;
    let done = {
      let app = app.clone();
      move || app.state::<SchemaState>().invalidate(&app)
    };
    return match fetcher.get(address, REFRESH, done) {
      Fetched::Ready(text) => Ok(Some(text)),
      Fetched::Pending => Ok(None),
      Fetched::Failed(error) => Err(format!("it can't be downloaded: {error}")),
    };
  }
  let url = url::Url::parse(address).map_err(|_| format!("{address} isn't a path or address"))?;
  if url.scheme() != "file" {
    return Err(format!("{address} isn't a file or a web address"));
  }
  let path = url
    .to_file_path()
    .map_err(|()| format!("{address} isn't a file"))?;
  let metadata =
    std::fs::metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
  if metadata.len() > MAX_SCHEMA_BYTES {
    return Err(format!("{} is too large", path.display()));
  }
  std::fs::read_to_string(&path)
    .map(|text| Some(text.trim_start_matches('\u{feff}').into()))
    .map_err(|error| format!("{}: {error}", path.display()))
}

/// Parses a schema's text: TOML Schema if its address ends in `.tosd` (or
/// it isn't JSON but has a `[toml-schema]` table), else JSON Schema, with
/// the documents it references.
fn parse_schema(
  app: &AppHandle,
  address: &str,
  text: &str,
  toml: bool,
) -> Result<Option<Schema>, String> {
  let toml = toml
    || address.to_ascii_lowercase().ends_with(".tosd")
    || (!text.trim_start().starts_with('{') && text.contains("[toml-schema]"));
  if toml {
    return Schema::tosd(&name_of(address), text)
      .map(Some)
      .map_err(|error| error.to_string());
  }
  let value = json_value(text).ok_or("it isn't JSON")?;
  let mut fetched: HashMap<String, serde_json::Value> = HashMap::new();
  let mut queue = flareite_schema::json_schema::external_references(address, &value);
  while let Some(reference) = queue.pop() {
    if fetched.contains_key(&reference) || reference == address {
      continue;
    }
    if fetched.len() >= MAX_REFERENCED {
      break;
    }
    match read(app, &reference) {
      Ok(Some(text)) => {
        if let Some(document) = json_value(&text) {
          queue.extend(flareite_schema::json_schema::external_references(
            &reference, &document,
          ));
          fetched.insert(reference, document);
        }
      }
      // Loaded once what it references is.
      Ok(None) => return Ok(None),
      Err(error) => tracing::debug!(%reference, %error, "a schema's reference can't be read"),
    }
  }
  Ok(Some(Schema::json(address, value, fetched)))
}

/// A JSON Schema's text: JSON, JSON with comments, or YAML (which JSON is
/// too), as the YAML language server takes them.
fn json_value(text: &str) -> Option<serde_json::Value> {
  serde_json::from_str(text)
    .ok()
    .or_else(|| flareite_schema::json_schema::jsonc_value(text))
    .or_else(|| flareite_schema::yaml::value(text).filter(serde_json::Value::is_object))
}

fn load_now(app: &AppHandle, address: &str) -> Result<Option<Loaded>, String> {
  let builtin = |name: &str, text: &str| {
    Schema::tosd(name, text)
      .map(|schema| {
        Some(Loaded {
          schema,
          name: name.to_owned(),
        })
      })
      .map_err(|error| error.to_string())
  };
  match address {
    EXTENSION_SCHEMA => {
      return builtin(
        "extension.tosd",
        flareite_schema::builtin::EXTENSION_MANIFEST,
      );
    }
    TOML_SCHEMA_SCHEMA => {
      return builtin("toml-schema.tosd", flareite_schema::builtin::TOML_SCHEMA);
    }
    SETTINGS_SCHEMA => {
      let text = flareite_schema::builtin::settings_schema(&installed::settings_context(app));
      return builtin("settings.tosd", &text);
    }
    _ => {}
  }
  if let Some(rest) = address.strip_prefix(EXTENSION_PREFIX) {
    let (extension, id) = rest
      .split_once("/schemas/")
      .ok_or_else(|| format!("{address} names no extension's schema"))?;
    let extensions = app.state::<ExtensionsState>();
    let host = extensions.host().ok_or("extensions aren't running")?;
    let (format, text) = host
      .schemas()
      .into_iter()
      .filter(|provided| provided.extension == extension)
      .find_map(|provided| match provided.source {
        Provided::Content {
          id: known,
          format,
          text,
        } if known == id => Some((format, text)),
        _ => None,
      })
      .ok_or_else(|| format!("{extension} provides no schema {id}"))?;
    let toml = format == SchemaFormat::TomlSchema;
    return parse_schema(app, address, &text, toml).map(|schema| {
      schema.map(|schema| Loaded {
        schema,
        name: id.to_owned(),
      })
    });
  }
  let Some(text) = read(app, address)? else {
    return Ok(None);
  };
  parse_schema(app, address, &text, false).map(|schema| {
    schema.map(|schema| Loaded {
      schema,
      name: name_of(address),
    })
  })
}

/// The schemas loaded for a document, for queries: those that apply and
/// are loaded (none are waited for).
fn loaded_for(app: &AppHandle, parsed: &Parsed) -> Vec<Arc<Loaded>> {
  resolve(app, parsed)
    .addresses
    .iter()
    .filter_map(|address| match load(app, address) {
      Slot::Ready(loaded) => Some(loaded),
      _ => None,
    })
    .collect()
}

/// What the schemas that apply to a document say at `path` in part
/// `part` of its latest tree (`schemas::properties`, `values`, and
/// `describe`). Called from extension threads, without the editor locked.
pub fn query(
  app: &AppHandle,
  id: DocumentId,
  part: usize,
  path: &[Segment],
  query: SchemaQuery,
) -> Result<SchemaAnswer, String> {
  let parsed = app
    .state::<SchemaState>()
    .lock()
    .documents
    .get(&id)
    .and_then(|record| record.parsed.clone())
    .ok_or("the document has no tree: set one with set-document first")?;
  // A document without parts (nothing written yet) is an empty table.
  let empty = flareite_schema::document::Node::table(0..0, Vec::new());
  let tree = parsed
    .tree
    .parts
    .get(part)
    .map_or(&empty, |part| &part.tree);
  let loaded = loaded_for(app, &parsed);
  let schemas: Vec<&Schema> = loaded.iter().map(|loaded| &loaded.schema).collect();
  Ok(match query {
    SchemaQuery::Properties => {
      SchemaAnswer::Properties(flareite_schema::properties(&schemas, tree, path))
    }
    SchemaQuery::Values => {
      let mut choices = flareite_schema::choices(&schemas, tree, path);
      // What's installed, for the settings that name it.
      if loaded.iter().any(|loaded| loaded.name == "settings.tosd") {
        let keys: Vec<String> = path
          .iter()
          .filter_map(|segment| match segment {
            Segment::Key(key) => Some(key.clone()),
            Segment::Index(_) => None,
          })
          .collect();
        for (value, label) in crate::settings::value_suggestions(app, &keys) {
          let value = serde_json::Value::String(value);
          if choices.iter().any(|choice| choice.value == value) {
            continue;
          }
          let described = value.as_str().is_some_and(|value| value != label);
          choices.push(flareite_schema::nav::Choice {
            value,
            description: described.then_some(label),
            default: false,
          });
        }
      }
      SchemaAnswer::Values(choices)
    }
    SchemaQuery::Describe => {
      SchemaAnswer::Describe(flareite_schema::info(&schemas, tree, path).map(|info| {
        let names = loaded.iter().map(|loaded| loaded.name.clone()).collect();
        (info, names)
      }))
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn addresses_resolve_paths() {
    let base = if cfg!(windows) {
      PathBuf::from("C:\\work")
    } else {
      PathBuf::from("/work")
    };
    let address = address_of("schemas/a.tosd", Some(&base)).expect("address");
    assert!(address.starts_with("file:///"), "{address}");
    assert!(address.ends_with("/work/schemas/a.tosd"), "{address}");
    assert_eq!(
      address_of("https://x.dev/s.json", None).as_deref(),
      Some("https://x.dev/s.json")
    );
    assert_eq!(address_of("relative.json", None), None);
    assert_eq!(
      name_of("https://json.schemastore.org/package.json"),
      "package.json"
    );
    assert_eq!(name_of("flareite://schemas/settings.tosd"), "settings.tosd");
  }

  #[test]
  fn json_schemas_may_be_written_in_yaml_or_with_comments() {
    let schema = json_value(
      "type: object
properties:
  a: { type: integer }
",
    );
    assert_eq!(
      schema,
      Some(serde_json::json!({ "type": "object", "properties": { "a": { "type": "integer" } } }))
    );
    assert_eq!(
      json_value("// a schema\n{ \"type\": \"object\", }"),
      Some(serde_json::json!({ "type": "object" }))
    );
    assert!(json_value("not a schema").is_none());
    assert!(
      json_value(
        "a: 1
---
b: 2
"
      )
      .is_none()
    );
  }
}
