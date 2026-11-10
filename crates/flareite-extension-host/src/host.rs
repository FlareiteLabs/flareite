//! The extension host: discovery, lifecycle, permission decisions, document
//! notifications, extension diagnostics and decorations, and the content of
//! extension status bar items, views, and previews.
//!
//! Lock order: the editor lock (held by the embedder around `sync` and the
//! diagnostics queries, and taken by host functions through
//! [`Embedder::with_editor`]), then the host's state lock. Nothing waits on
//! an extension while holding either; notifications to the embedder are
//! sent after the state lock is released.

#[path = "layout_tabs.rs"]
mod layout_tabs;

pub use layout_tabs::LayoutView;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock, Weak};
use std::time::{Duration, Instant};

use flareite_core::{Document, DocumentId, LanguageChoice};
use flareite_text::{TextBuffer, TextSnapshot, Version, map_offset_through};

use crate::api::{self, document_info, to_position};
use crate::bindings::flareite::extension::types::{
  ApiError, CommandContext, DocumentInfo, PreviewInstance, Range as WitRange, TextChange,
  ViewedFile, Webview as WitWebview, WebviewMessage,
};
use crate::bindings::flareite::extension::workspace::FileChanges;
use crate::config::{self, ConfigValue};
use crate::contributions::{LanguageContribution, LanguageServerContribution};
use crate::icons::IconTheme;
use crate::manifest::{self, MANIFEST_FILE, Manifest};
use crate::marks::{ExtensionMarks, FileMark};
use crate::permissions::{Decision, Decisions, Permission};
use crate::preview::{self, PreviewContent, PreviewInfo};
use crate::processes::ProcessTable;
use crate::prompts::{Prompt, PromptKind, PromptReply};
use crate::runtime::{MemoryPool, Runtime};
use crate::storage::{Scope, Storage};
use crate::usage::{ResourceUsage, Usage};
use crate::webview::{self, Resource, WebviewInfo, WebviewRecord, WebviewTheme};
use crate::worker::{
  self, Budgets, Event, Failure, MAX_PENDING_CHANGE_BYTES, MAX_PENDING_PATHS, Payload,
  ProvideRequest, Queue, Setup, Worker,
};
use crate::{
  CommandInfo, ConfiguredSetting, Embedder, ExtensionAnnotation, ExtensionConfiguration,
  ExtensionDecoration, ExtensionDiagnostic, ExtensionLens, ExtensionState, ExtensionStatus,
  HostOptions, Notification, PermissionStatus, Severity, StatusContent, StatusItem, ViewContent,
};

/// Synced versions kept per document, to interpret diagnostics published
/// for slightly older text.
const SYNC_HISTORY: usize = 8;
/// Added to a command's deadlines when waiting for its result, to cover
/// compiling the extension and events queued before it.
const COMMAND_SAFETY_MARGIN: Duration = Duration::from_secs(120);
/// How long shutdown waits for extensions to deactivate.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(1500);
/// Most watches per extension.
pub const MAX_WATCHES: usize = 32;

/// A diagnostic as byte ranges in the text at its set's version.
#[derive(Debug, Clone)]
pub struct StoredDiagnostic {
  pub range: Range<usize>,
  pub severity: Severity,
  pub message: String,
  pub code: Option<String>,
}

struct DiagnosticSet {
  version: Version,
  items: Vec<StoredDiagnostic>,
}

impl DiagnosticSet {
  fn catch_up(&mut self, buffer: &TextBuffer) {
    if !catch_up(
      &mut self.version,
      self.items.iter_mut().map(|item| &mut item.range),
      buffer,
    ) {
      self.items.clear();
    }
  }
}

/// Ranges decorated with one decoration type.
struct DecorationSet {
  version: Version,
  ranges: Vec<Range<usize>>,
}

impl DecorationSet {
  fn catch_up(&mut self, buffer: &TextBuffer) {
    if !catch_up(&mut self.version, self.ranges.iter_mut(), buffer) {
      self.ranges.clear();
    }
  }
}

/// An extension's code lenses in a document.
struct LensSet {
  version: Version,
  /// Each lens's line start, as an empty range so it maps like others.
  anchors: Vec<Range<usize>>,
  lenses: Vec<StoredLens>,
}

/// An extension's line annotations in a document.
struct AnnotationSet {
  version: Version,
  anchors: Vec<Range<usize>>,
  annotations: Vec<(String, Option<String>)>,
}

impl AnnotationSet {
  fn catch_up(&mut self, buffer: &TextBuffer) {
    if !catch_up(&mut self.version, self.anchors.iter_mut(), buffer) {
      self.anchors.clear();
      self.annotations.clear();
    }
  }
}

pub struct StoredLens {
  pub title: String,
  pub tooltip: Option<String>,
  pub command: String,
  pub argument: Option<String>,
}

impl LensSet {
  fn catch_up(&mut self, buffer: &TextBuffer) {
    if !catch_up(&mut self.version, self.anchors.iter_mut(), buffer) {
      self.anchors.clear();
      self.lenses.clear();
    }
  }
}

/// Maps ranges stored at `version` through the edits since, and updates
/// `version`. Returns `false` if the edits are too old to map.
fn catch_up<'a>(
  version: &mut Version,
  ranges: impl Iterator<Item = &'a mut Range<usize>>,
  buffer: &TextBuffer,
) -> bool {
  if *version == buffer.version() {
    return true;
  }
  let mapped = match buffer.edits_since(*version) {
    Some(edits) => {
      for range in ranges {
        let start = map_offset_through(&edits, range.start, true);
        let end = map_offset_through(&edits, range.end, false).max(start);
        *range = start..end;
      }
      true
    }
    None => false,
  };
  *version = buffer.version();
  mapped
}

/// What the host knows about an open document.
struct DocumentRecord {
  info: DocumentInfo,
  /// The path and chosen language `info` was built for.
  path: Option<PathBuf>,
  choice: Option<LanguageChoice>,
  /// Recently synced texts, oldest first; the last is the current one.
  history: VecDeque<TextSnapshot>,
  /// Diagnostics by extension id.
  diagnostics: BTreeMap<String, DiagnosticSet>,
  /// Decorations by extension id and decoration type.
  decorations: BTreeMap<(String, String), DecorationSet>,
  /// Code lenses by extension id.
  lenses: BTreeMap<String, LensSet>,
  /// Line annotations by extension id.
  annotations: BTreeMap<String, AnnotationSet>,
}

impl DocumentRecord {
  fn synced(&self) -> Option<&TextSnapshot> {
    self.history.back()
  }
}

enum Run {
  /// Not running: waiting for an activation event (or a permission
  /// decision, or enabling).
  Idle,
  Running {
    worker: Worker,
    active: bool,
  },
  Failed(String),
}

struct Entry {
  manifest: Arc<Manifest>,
  development: bool,
  config: Arc<RwLock<BTreeMap<String, ConfigValue>>>,
  config_errors: Vec<String>,
  run: Run,
  /// Kept across restarts and reloads of the extension.
  usage: Arc<Usage>,
}

/// A folder that could not be loaded as an extension.
struct Invalid {
  dir: PathBuf,
  development: bool,
  error: String,
}

/// An open preview.
struct PreviewRecord {
  /// The extension's id.
  extension: String,
  info: PreviewInfo,
  content: Arc<PreviewContent>,
  /// The picture files its content shows.
  images: BTreeSet<PathBuf>,
  /// For a viewer, the file open in it. Viewers are the workbench's tabs,
  /// so they stay open when their extension stops (only their content
  /// goes) and aren't listed with the previews.
  file: Option<Viewed>,
  /// For a webview, its page.
  webview: Option<WebviewRecord>,
  /// For a layout's webview or file page pane: its tab and pane. Such a
  /// page is the tab's, so it isn't listed with the previews and stays
  /// when its extension stops. (1.15)
  pane: Option<(crate::layouts::TabId, String)>,
}

/// A file open in a viewer.
struct Viewed {
  path: PathBuf,
  /// The viewer's command, which fills it in.
  command: Option<String>,
  /// Its content went when its extension stopped: the command runs again
  /// when the extension starts.
  stale: bool,
}

/// A prompt waiting for the user's answer.
struct PendingPrompt {
  id: u64,
  extension: String,
  generation: u64,
  answer: PromptAnswerTo,
}

/// Where a prompt's answer goes.
enum PromptAnswerTo {
  /// An extension command, run with it.
  Command(String),
  /// A thread waiting for it (a program asking for a password).
  Reply(mpsc::Sender<PromptReply>),
}

/// A path an extension watches (`workspace::watch`).
struct Watch {
  id: u64,
  extension: String,
  generation: u64,
  path: PathBuf,
  command: String,
  /// For a path outside the open folder, its own watcher. Paths inside it
  /// get the workspace's changes ([`ExtensionHost::files_changed`]).
  own_watcher: Option<flareite_workspace::WorkspaceWatcher>,
}

struct State {
  options: HostOptions,
  decisions: Decisions,
  extensions: Vec<Entry>,
  invalid: Vec<Invalid>,
  documents: HashMap<DocumentId, DocumentRecord>,
  /// Status bar content set by extensions, by extension and item id:
  /// `None` hides the item. Items not here show their manifest text.
  status: BTreeMap<(String, String), Option<StatusContent>>,
  /// View content set by extensions, by full view id.
  views: BTreeMap<String, ViewContent>,
  /// Open previews, by preview id.
  previews: BTreeMap<String, PreviewRecord>,
  /// Explorer marks, by extension id.
  marks: BTreeMap<String, Arc<Vec<FileMark>>>,
  /// The prompt showing, if any.
  prompt: Option<PendingPrompt>,
  /// Answers to requests for provided text, by request id, until the
  /// request reads them.
  provided: HashMap<u64, Option<String>>,
  next_request: u64,
  /// The file of the editor in use, as the workbench last said.
  active_file: Option<PathBuf>,
  /// Extensions' timers waiting to run.
  timers: Vec<crate::timers::Timer>,
  next_timer: u64,
  next_prompt: u64,
  watches: Vec<Watch>,
  next_watch: u64,
  next_generation: u64,
  shutting_down: bool,
  /// Open layout tabs. (1.15)
  layouts: BTreeMap<crate::layouts::TabId, layout_tabs::LayoutRecord>,
  /// The editor pane in use, when it's a layout's.
  active_pane: Option<layout_tabs::ActivePane>,
  next_pane_webview: u64,
  /// Schemas extensions' code set, by extension id. (1.16)
  schemas: BTreeMap<String, Vec<crate::schemas::RuntimeSchema>>,
  /// What extensions' code changed about their language servers, by
  /// extension id and server name. (1.16)
  servers: BTreeMap<String, BTreeMap<String, crate::language_servers::ServerChanges>>,
}

pub struct Shared {
  me: Weak<Shared>,
  pub embedder: Arc<dyn Embedder>,
  runtime: OnceLock<Result<Arc<Runtime>, String>>,
  /// Where programs' sign-in questions come, started on first use.
  askpass: OnceLock<Option<Arc<crate::askpass::Askpass>>>,
  /// Linear memory of all running extensions.
  memory: Arc<MemoryPool>,
  /// Extensions' stored values and webview tabs' states.
  pub storage: Storage,
  /// The workbench's theme, for webview pages.
  webview_theme: RwLock<WebviewTheme>,
  /// Development extensions' builds.
  builders: crate::development::Builders,
  state: Mutex<State>,
}

/// Notifications collected under the state lock, sent after it is released.
#[derive(Default)]
struct Outbox(Vec<Notification>);

impl Outbox {
  fn push(&mut self, notification: Notification) {
    self.0.push(notification);
  }

  fn send(self, embedder: &dyn Embedder) {
    for notification in self.0 {
      embedder.notify(notification);
    }
  }
}

impl Shared {
  fn lock(&self) -> MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The environment that lets `extension`'s programs ask the user to sign
  /// in, if the embedder can (see [`crate::askpass`]).
  pub fn askpass_env(&self, extension: &str) -> Vec<(String, String)> {
    let Some(program) = self.embedder.askpass_program() else {
      return Vec::new();
    };
    let server = self.askpass.get_or_init(|| {
      crate::askpass::Askpass::start(self.me.clone())
        .inspect_err(|error| tracing::error!(%error, "can't listen for sign-in questions"))
        .ok()
    });
    server
      .as_ref()
      .map(|server| server.env(extension, &program))
      .unwrap_or_default()
  }

  pub fn memory_pool(&self) -> Arc<MemoryPool> {
    self.memory.clone()
  }

  /// The runtime, created on first use.
  pub fn runtime(&self) -> Result<Arc<Runtime>, String> {
    self
      .runtime
      .get_or_init(|| Runtime::new().map(Arc::new))
      .clone()
  }

  pub fn budgets(&self) -> Budgets {
    let limits = &self.lock().options.limits;
    Budgets {
      activation: limits.activation_time,
      command: limits.command_time,
      event: limits.event_time,
      memory: limits.memory_bytes,
    }
  }

  /// Starts the extension at `index` unless it is running, disabled,
  /// waiting for a permission decision, or the host is shutting down.
  fn start(&self, state: &mut State, index: usize, outbox: &mut Outbox) {
    let entry = &state.extensions[index];
    let decision = state.decisions.get(&entry.manifest.id);
    if state.shutting_down
      || entry.manifest.entrypoint.is_none()
      || decision.disabled
      || !decision.pending(&entry.manifest.permissions).is_empty()
      || matches!(entry.run, Run::Running { .. })
    {
      return;
    }
    let granted = decision.effective(&entry.manifest.permissions);
    let queue = Arc::new(Queue::new(
      state.options.limits.queue_capacity,
      entry.usage.clone(),
    ));
    if granted.contains(&Permission::DocumentsRead) {
      let mut infos: Vec<&DocumentInfo> = state
        .documents
        .values()
        .map(|record| &record.info)
        .collect();
      infos.sort_by_key(|info| info.id);
      for info in infos {
        queue.push_document(Event::Opened(info.clone()));
      }
    }
    state.next_generation += 1;
    let setup = Setup {
      manifest: entry.manifest.clone(),
      generation: state.next_generation,
      granted,
      config: entry.config.clone(),
      queue,
      cancel: Arc::new(AtomicBool::new(false)),
      usage: entry.usage.clone(),
      processes: Arc::new(ProcessTable::default()),
    };
    tracing::info!(extension = %setup.manifest.id, "starting extension");
    let worker = worker::spawn(self.me.clone(), setup);
    state.extensions[index].run = Run::Running {
      worker,
      active: false,
    };
    // Fill in again the viewers it showed before it stopped.
    let entry = &state.extensions[index];
    if let Run::Running { worker, .. } = &entry.run {
      for record in state.previews.values_mut() {
        let Some(viewed) = record.file.as_mut().filter(|viewed| viewed.stale) else {
          continue;
        };
        if record.extension != entry.manifest.id {
          continue;
        }
        viewed.stale = false;
        let Some(command) = viewed.command.clone() else {
          continue;
        };
        // Nobody waits for the outcome: a failure leaves the tab without
        // content, and the log says why.
        let _ = worker.queue.push_command(Event::background(
          command,
          Some(viewed.path.display().to_string()),
          Payload::None,
        ));
      }
    }
    Self::refill_layouts(state, index);
    outbox.push(Notification::ExtensionsChanged);
  }

  /// Stops the extension at `index` and forgets its diagnostics.
  fn stop(state: &mut State, index: usize, outbox: &mut Outbox) {
    let entry = &mut state.extensions[index];
    if let Run::Running { worker, .. } = std::mem::replace(&mut entry.run, Run::Idle) {
      worker.stop(true);
    }
    let id = entry.manifest.id.clone();
    Self::forget_output(state, &id, outbox);
    outbox.push(Notification::ExtensionsChanged);
  }

  /// Removes what a stopped extension showed: diagnostics, decorations,
  /// status bar content, view content, explorer marks, and previews. Its
  /// watches stop.
  fn forget_output(state: &mut State, id: &str, outbox: &mut Outbox) {
    state.watches.retain(|watch| watch.extension != id);
    if state.schemas.remove(id).is_some() {
      outbox.push(Notification::SchemasChanged);
    }
    if state.servers.remove(id).is_some() {
      outbox.push(Notification::LanguageServersChanged);
    }
    state.timers.retain(|timer| {
      let mine = timer.extension == id;
      if mine {
        timer.cancel();
      }
      !mine
    });
    if state.marks.remove(id).is_some() {
      outbox.push(Notification::FileMarksChanged);
    }
    if state
      .prompt
      .as_ref()
      .is_some_and(|prompt| prompt.extension == id)
      && let Some(prompt) = state.prompt.take()
    {
      outbox.push(Notification::PromptClosed(prompt.id));
    }
    for (&document, record) in &mut state.documents {
      if record.diagnostics.remove(id).is_some() {
        outbox.push(Notification::DiagnosticsChanged(document));
      }
      let before = record.decorations.len();
      record
        .decorations
        .retain(|(extension, _), _| extension != id);
      let lenses = record.lenses.remove(id).is_some();
      let annotations = record.annotations.remove(id).is_some();
      if record.decorations.len() != before || lenses || annotations {
        outbox.push(Notification::DecorationsChanged(document));
      }
    }
    let before = state.status.len();
    state.status.retain(|(extension, _), _| extension != id);
    if state.status.len() != before {
      outbox.push(Notification::StatusItemsChanged);
    }
    let prefix = format!("{id}.");
    let views: Vec<String> = state
      .views
      .keys()
      .filter(|view| view.starts_with(&prefix))
      .cloned()
      .collect();
    for view in views {
      state.views.remove(&view);
      outbox.push(Notification::ViewChanged(view));
    }
    let before = state.previews.len();
    state.previews.retain(|_, preview| {
      preview.extension != id || preview.file.is_some() || preview.pane.is_some()
    });
    if state.previews.len() != before {
      outbox.push(Notification::PreviewsChanged);
    }
    Self::forget_layouts(state, id, outbox);
    for (viewer, record) in &mut state.previews {
      if record.extension != id {
        continue;
      }
      let Some(viewed) = record.file.as_mut() else {
        continue;
      };
      viewed.stale = true;
      if !record.content.nodes.is_empty() {
        record.content = Arc::default();
        record.images.clear();
        outbox.push(Notification::PreviewChanged(viewer.clone()));
      }
    }
  }

  /// Queues `event` for an extension, if this start of it (`generation`)
  /// is still running.
  pub fn deliver(&self, extension: &str, generation: u64, event: Event) {
    let state = self.lock();
    let worker = state
      .extensions
      .iter()
      .find(|entry| entry.manifest.id == extension)
      .and_then(|entry| match &entry.run {
        Run::Running { worker, .. } if worker.generation == generation => Some(worker),
        _ => None,
      });
    if let Some(worker) = worker {
      worker.queue.push_background(event);
    }
  }

  /// The path the user set for one of an extension's programs, if any.
  pub fn program_path(&self, extension: &str, program: &str) -> Option<String> {
    self
      .lock()
      .options
      .programs
      .get(&format!("{extension}.{program}"))
      .filter(|path| !path.trim().is_empty())
      .cloned()
  }

  /// Starts watching `path` (resolved) for an extension. A path outside
  /// the open folder (`outside`) gets a watcher of its own.
  pub fn add_watch(
    &self,
    extension: &str,
    generation: u64,
    path: PathBuf,
    outside: bool,
    command: String,
  ) -> Result<u64, String> {
    let id = {
      let mut state = self.lock();
      let count = state
        .watches
        .iter()
        .filter(|watch| watch.extension == extension)
        .count();
      if count >= MAX_WATCHES {
        return Err(format!("at most {MAX_WATCHES} watches"));
      }
      state.next_watch += 1;
      state.next_watch
    };
    let watcher = if outside {
      let shared = self.me.clone();
      let watcher = flareite_workspace::WorkspaceWatcher::new(&path, move |changed| {
        if let Some(shared) = shared.upgrade() {
          shared.dispatch_changes(&changed, |watch| watch.id == id);
        }
      })
      .map_err(|error| error.to_string())?;
      Some(watcher)
    } else {
      None
    };
    self.lock().watches.push(Watch {
      id,
      extension: extension.to_owned(),
      generation,
      path,
      command,
      own_watcher: watcher,
    });
    Ok(id)
  }

  pub fn remove_watch(&self, extension: &str, id: u64) {
    self
      .lock()
      .watches
      .retain(|watch| watch.extension != extension || watch.id != id);
  }

  /// Runs the commands of the watches `which` picks for the `changed`
  /// paths under them.
  fn dispatch_changes(&self, changed: &[PathBuf], which: impl Fn(&Watch) -> bool) {
    let state = self.lock();
    for watch in state.watches.iter().filter(|watch| which(watch)) {
      let mut paths: Vec<String> = changed
        .iter()
        .filter(|path| path.starts_with(&watch.path))
        .map(|path| path.display().to_string())
        .collect();
      if paths.is_empty() {
        continue;
      }
      paths.sort_unstable();
      paths.dedup();
      let overflow = paths.len() > MAX_PENDING_PATHS;
      if overflow {
        paths.clear();
      }
      let worker = state
        .extensions
        .iter()
        .find(|entry| entry.manifest.id == watch.extension)
        .and_then(|entry| match &entry.run {
          Run::Running { worker, .. } if worker.generation == watch.generation => Some(worker),
          _ => None,
        });
      if let Some(worker) = worker {
        worker.queue.push_background(Event::background(
          watch.command.clone(),
          None,
          Payload::Changes(FileChanges {
            watch: watch.id,
            paths,
            overflow,
          }),
        ));
      }
    }
  }

  /// Replaces an extension's explorer marks.
  pub fn set_file_marks(&self, extension: &str, marks: Vec<FileMark>) {
    let changed = {
      let mut state = self.lock();
      let before = state.marks.get(extension);
      if before.map_or(marks.is_empty(), |before| **before == marks) {
        false
      } else {
        if marks.is_empty() {
          state.marks.remove(extension);
        } else {
          state.marks.insert(extension.to_owned(), Arc::new(marks));
        }
        true
      }
    };
    if changed {
      self.embedder.notify(Notification::FileMarksChanged);
    }
  }

  /// Sets (or hides, with `None`) one of an extension's status bar items.
  pub fn set_status_item(&self, extension: &str, item: &str, content: Option<StatusContent>) {
    let changed = {
      let mut state = self.lock();
      let key = (extension.to_owned(), item.to_owned());
      if state.status.get(&key) == Some(&content) {
        false
      } else {
        state.status.insert(key, content);
        true
      }
    };
    if changed {
      self.embedder.notify(Notification::StatusItemsChanged);
    }
  }

  /// Changes the content of a view.
  pub fn update_view<R>(&self, full_id: &str, change: impl FnOnce(&mut ViewContent) -> R) -> R {
    let (result, changed) = {
      let mut state = self.lock();
      let content = state.views.entry(full_id.to_owned()).or_default();
      let before = content.clone();
      let result = change(content);
      (result, *content != before)
    };
    if changed {
      self
        .embedder
        .notify(Notification::ViewChanged(full_id.to_owned()));
    }
    result
  }

  /// A view's content, as last set.
  pub fn view_snapshot(&self, full_id: &str) -> ViewContent {
    self.lock().views.get(full_id).cloned().unwrap_or_default()
  }

  /// Shows a prompt for an extension, cancelling the one showing (whose
  /// command runs with `cancelled`). Returns its id.
  pub fn show_prompt(
    &self,
    extension: &str,
    generation: u64,
    command: String,
    kind: PromptKind,
  ) -> u64 {
    self.show(
      extension,
      generation,
      PromptAnswerTo::Command(command),
      kind,
    )
  }

  /// Asks the user something for an extension's program, cancelling the
  /// prompt showing. Returns its id and where the reply arrives; the sender
  /// is dropped if the prompt is withdrawn.
  pub fn ask_user(&self, extension: &str, kind: PromptKind) -> (u64, mpsc::Receiver<PromptReply>) {
    let (sender, receiver) = mpsc::channel();
    // The reply goes to the receiver, not to a run of the extension.
    let id = self.show(extension, 0, PromptAnswerTo::Reply(sender), kind);
    (id, receiver)
  }

  /// Withdraws prompt `id` if it's still showing.
  pub fn withdraw_prompt(&self, id: u64) {
    let withdrawn = {
      let mut state = self.lock();
      if state.prompt.as_ref().is_some_and(|prompt| prompt.id == id) {
        state.prompt.take()
      } else {
        None
      }
    };
    if withdrawn.is_some() {
      self.embedder.notify(Notification::PromptClosed(id));
    }
  }

  fn show(
    &self,
    extension: &str,
    generation: u64,
    answer: PromptAnswerTo,
    kind: PromptKind,
  ) -> u64 {
    let (id, name, replaced) = {
      let mut state = self.lock();
      state.next_prompt += 1;
      let id = state.next_prompt;
      let name = state
        .extensions
        .iter()
        .find(|entry| entry.manifest.id == extension)
        .map_or_else(|| extension.to_owned(), |entry| entry.manifest.name.clone());
      let replaced = state.prompt.replace(PendingPrompt {
        id,
        extension: extension.to_owned(),
        generation,
        answer,
      });
      (id, name, replaced)
    };
    if let Some(replaced) = replaced {
      self.answer_prompt(replaced, PromptReply::Cancelled);
    }
    self.embedder.notify(Notification::Prompt(Prompt {
      id,
      extension: name,
      kind,
    }));
    id
  }

  /// Runs `command` of an extension after `delay`, unless the timer is
  /// cleared or the extension stops first. `None` if the extension has as
  /// many timers as it may.
  pub fn set_timer(
    &self,
    extension: &str,
    generation: u64,
    delay: Duration,
    command: String,
  ) -> Option<u64> {
    let (id, wait) = {
      let mut state = self.lock();
      let waiting = state
        .timers
        .iter()
        .filter(|timer| timer.extension == extension)
        .count();
      if waiting >= crate::timers::MAX_TIMERS {
        return None;
      }
      state.next_timer += 1;
      let id = state.next_timer;
      let timer = crate::timers::Timer::new(id, extension.to_owned());
      let wait = timer.waiter(delay);
      state.timers.push(timer);
      (id, wait)
    };
    let shared = self.me.clone();
    let extension = extension.to_owned();
    let started = std::thread::Builder::new()
      .name("extension-timer".into())
      .spawn(move || {
        if !wait() {
          return;
        }
        let Some(shared) = shared.upgrade() else {
          return;
        };
        let due = {
          let mut state = shared.lock();
          let before = state.timers.len();
          state.timers.retain(|timer| timer.id != id);
          state.timers.len() != before
        };
        if due {
          shared.deliver(
            &extension,
            generation,
            Event::background(command, None, Payload::Timer(id)),
          );
        }
      });
    if let Err(error) = started {
      tracing::error!(%error, "could not start a timer's thread");
      self.lock().timers.retain(|timer| timer.id != id);
      return None;
    }
    Some(id)
  }

  /// Adds or replaces a schema an extension's code set. Returns false if it
  /// already has `room` and this one is new.
  pub fn set_schema(
    &self,
    extension: &str,
    schema: crate::schemas::RuntimeSchema,
    room: usize,
  ) -> bool {
    {
      let mut state = self.lock();
      let schemas = state.schemas.entry(extension.to_owned()).or_default();
      match schemas.iter().position(|known| known.id == schema.id) {
        Some(index) => schemas[index] = schema,
        None if schemas.len() >= room => return false,
        None => schemas.push(schema),
      }
    }
    self.embedder.notify(Notification::SchemasChanged);
    true
  }

  /// Changes what an extension's code set for one of its language
  /// servers, telling the editor if that changed anything.
  pub fn change_server(
    &self,
    extension: &str,
    server: &str,
    change: impl FnOnce(&mut crate::language_servers::ServerChanges),
  ) {
    let changed = {
      let mut state = self.lock();
      let changes = state
        .servers
        .entry(extension.to_owned())
        .or_default()
        .entry(server.to_owned())
        .or_default();
      let before = changes.clone();
      change(changes);
      *changes != before
    };
    if changed {
      self.embedder.notify(Notification::LanguageServersChanged);
    }
  }

  /// Asks the editor to restart a language server's running instances.
  pub fn restart_server(&self, server: &str) {
    self.embedder.notify(Notification::RestartLanguageServer {
      name: server.to_owned(),
    });
  }

  /// Removes a schema an extension's code set; false if it had none.
  pub fn remove_schema(&self, extension: &str, id: &str) -> bool {
    let removed = {
      let mut state = self.lock();
      let Some(schemas) = state.schemas.get_mut(extension) else {
        return false;
      };
      let before = schemas.len();
      schemas.retain(|schema| schema.id != id);
      let removed = schemas.len() != before;
      if schemas.is_empty() {
        state.schemas.remove(extension);
      }
      removed
    };
    if removed {
      self.embedder.notify(Notification::SchemasChanged);
    }
    removed
  }

  /// Cancels an extension's timer; false if it had run or was cancelled.
  pub fn clear_timer(&self, extension: &str, id: u64) -> bool {
    let mut state = self.lock();
    match state
      .timers
      .iter()
      .position(|timer| timer.id == id && timer.extension == extension)
    {
      Some(index) => {
        state.timers.remove(index).cancel();
        true
      }
      None => false,
    }
  }

  /// The file of the editor in use, as the workbench last said.
  pub fn active_file(&self) -> Option<PathBuf> {
    self.lock().active_file.clone()
  }

  /// Whether a loaded extension provides addresses with `scheme`.
  pub fn provides(&self, scheme: &str) -> bool {
    self
      .lock()
      .extensions
      .iter()
      .any(|entry| entry.manifest.contributions.text_provider(scheme).is_some())
  }

  /// Records the answer to request `id` for provided text.
  pub fn provided(&self, id: u64, text: Option<String>) {
    self.lock().provided.insert(id, text);
  }

  /// Runs a prompt's command with the user's reply, or hands the reply to
  /// the thread waiting for it.
  fn answer_prompt(&self, prompt: PendingPrompt, reply: PromptReply) {
    match prompt.answer {
      PromptAnswerTo::Command(command) => self.deliver(
        &prompt.extension,
        prompt.generation,
        Event::background(command, None, Payload::Answer(reply.into_answer(prompt.id))),
      ),
      PromptAnswerTo::Reply(sender) => {
        let _ = sender.send(reply);
      }
    }
  }

  /// Opens a preview, or updates its title, and asks to show it. A webview
  /// preview has `page`: the manifest's page, if any.
  pub fn show_preview(
    &self,
    extension: &str,
    info: PreviewInfo,
    page: Option<Option<String>>,
    beside: bool,
  ) {
    let id = info.id.clone();
    let changed = {
      let mut state = self.lock();
      match state.previews.get_mut(&id) {
        Some(record) if record.info == info => false,
        Some(record) => {
          record.info = info;
          true
        }
        None => {
          let webview = page.map(WebviewRecord::new);
          state.previews.insert(
            id.clone(),
            PreviewRecord {
              extension: extension.to_owned(),
              info,
              content: Arc::default(),
              images: BTreeSet::new(),
              file: None,
              webview,
              pane: None,
            },
          );
          true
        }
      }
    };
    if changed {
      self.embedder.notify(Notification::PreviewsChanged);
    }
    self
      .embedder
      .notify(Notification::ShowPreview { id, beside });
  }

  /// The document an open preview shows: `None` if it isn't open.
  pub fn preview_document(&self, id: &str) -> Option<Option<DocumentId>> {
    self
      .lock()
      .previews
      .get(id)
      .map(|record| record.info.document.map(DocumentId::from_raw))
  }

  /// The file open in a viewer: `None` if the viewer isn't open.
  pub fn viewed_file(&self, id: &str) -> Option<PathBuf> {
    Some(self.lock().previews.get(id)?.file.as_ref()?.path.clone())
  }

  /// Replaces an open preview's content; false if it isn't open.
  pub fn set_preview_content(
    &self,
    id: &str,
    content: PreviewContent,
    images: BTreeSet<PathBuf>,
  ) -> bool {
    let changed = {
      let mut state = self.lock();
      let Some(record) = state.previews.get_mut(id) else {
        return false;
      };
      if *record.content == content {
        false
      } else {
        record.content = Arc::new(content);
        record.images = images;
        true
      }
    };
    if changed {
      self
        .embedder
        .notify(Notification::PreviewChanged(id.to_owned()));
    }
    true
  }

  /// Tells pages that a stored value changed, and the workbench that the
  /// extension's stored size did.
  pub fn storage_changed(
    &self,
    extension: &str,
    scope: &Scope,
    key: &str,
    value: Option<String>,
    source: Option<&str>,
  ) {
    self.embedder.notify(Notification::StorageChanged {
      extension: extension.to_owned(),
      scope: Some(
        match scope {
          Scope::Global => "global",
          Scope::Workspace(_) => "workspace",
        }
        .to_owned(),
      ),
      key: Some(key.to_owned()),
      value,
      source: source.map(str::to_owned),
    });
    self.embedder.notify(Notification::ExtensionsChanged);
  }

  /// Replaces the page an open webview shows; false if it isn't open.
  pub fn set_webview_html(&self, id: &str, html: String) -> bool {
    {
      let mut state = self.lock();
      let Some(webview) = state
        .previews
        .get_mut(id)
        .and_then(|record| record.webview.as_mut())
      else {
        return false;
      };
      webview.html = Some(html.into());
      webview.version += 1;
    }
    self
      .embedder
      .notify(Notification::WebviewChanged(id.to_owned()));
    true
  }

  /// Sends a message (JSON text) to an open webview's page; false if it
  /// isn't open.
  pub fn post_webview_message(&self, id: &str, message: String) -> bool {
    let open = self
      .lock()
      .previews
      .get(id)
      .is_some_and(|record| record.webview.is_some());
    if open {
      self.embedder.notify(Notification::WebviewMessage {
        id: id.to_owned(),
        message,
      });
    }
    open
  }

  /// Closes a preview, if it's open.
  pub fn close_preview(&self, id: &str) {
    let removed = self.lock().previews.remove(id);
    if removed.is_some_and(|record| record.file.is_none() && record.pane.is_none()) {
      self.embedder.notify(Notification::PreviewsChanged);
    }
  }

  pub fn worker_active(&self, id: &str, generation: u64) {
    let mut state = self.lock();
    let Some(entry) = state
      .extensions
      .iter_mut()
      .find(|entry| entry.manifest.id == id)
    else {
      return;
    };
    if let Run::Running { worker, active } = &mut entry.run
      && worker.generation == generation
    {
      *active = true;
      tracing::info!(extension = %id, "extension active");
      drop(state);
      self.embedder.notify(Notification::ExtensionsChanged);
    }
  }

  pub fn worker_finished(&self, id: &str, generation: u64, outcome: Result<(), Failure>) {
    let mut outbox = Outbox::default();
    let mut reply = None;
    {
      let mut state = self.lock();
      let index = state.extensions.iter().position(|entry| {
        entry.manifest.id == id
          && matches!(&entry.run, Run::Running { worker, .. } if worker.generation == generation)
      });
      if let Some(index) = index {
        let entry = &mut state.extensions[index];
        let name = entry.manifest.name.clone();
        let (run, stopped) = match outcome {
          Ok(()) => (Run::Idle, "The extension stopped.".to_owned()),
          Err(failure) => {
            tracing::warn!(extension = %id, reason = %failure.reason, "extension failed");
            reply = failure.reply.map(|reply| (reply, failure.reason.clone()));
            outbox.push(Notification::ExtensionFailed {
              id: id.to_owned(),
              name,
              reason: failure.reason.clone(),
            });
            let stopped = format!("The extension stopped: {}", failure.reason);
            (Run::Failed(failure.reason), stopped)
          }
        };
        if let Run::Running { worker, .. } = std::mem::replace(&mut entry.run, run) {
          worker.queue.stop(&stopped);
        }
        Self::forget_output(&mut state, id, &mut outbox);
        outbox.push(Notification::ExtensionsChanged);
      } else if let Err(failure) = outcome {
        // Stopped or restarted meanwhile.
        reply = failure.reply.map(|reply| (reply, failure.reason));
      }
    }
    if let Some((reply, reason)) = reply {
      let _ = reply.send(Err(format!("The extension stopped: {reason}")));
    }
    outbox.send(self.embedder.as_ref());
  }

  /// Brings the host's view of `document` up to date and notifies
  /// extensions that read documents. Called with the editor locked.
  pub fn sync(&self, document: &Document) {
    let mut outbox = Outbox::default();
    {
      let mut guard = self.lock();
      let state = &mut *guard;
      if state.shutting_down {
        return;
      }
      let id = document.id();
      let buffer = document.buffer();
      // Most syncs (every view) find nothing new.
      if let Some(record) = state.documents.get(&id)
        && record.info.version == document.version().get()
        && record.info.dirty == document.is_dirty()
        && record.path.as_deref() == document.path()
        && record.choice.as_ref() == document.language_choice()
      {
        return;
      }
      // Renamed, moved, saved under another name, or set to another
      // language: to extensions, the document at the old path closed and one
      // at the new path opened (its language may differ), so their
      // diagnostics for it are dropped.
      if let Some(record) = state.documents.get(&id)
        && (record.path.as_deref() != document.path()
          || record.choice.as_ref() != document.language_choice())
      {
        state.documents.remove(&id);
        for entry in &state.extensions {
          if let Run::Running { worker, .. } = &entry.run
            && worker.reads_documents()
          {
            worker.queue.push_document(Event::Closed(id.get()));
          }
        }
      }
      let info = document_info(self, document);
      match state.documents.get_mut(&id) {
        None => {
          let language = info.language.clone();
          state.documents.insert(
            id,
            DocumentRecord {
              info: info.clone(),
              path: document.path().map(Path::to_path_buf),
              choice: document.language_choice().cloned(),
              history: VecDeque::from([buffer.snapshot()]),
              diagnostics: BTreeMap::new(),
              decorations: BTreeMap::new(),
              lenses: BTreeMap::new(),
              annotations: BTreeMap::new(),
            },
          );
          for entry in &state.extensions {
            if let Run::Running { worker, .. } = &entry.run
              && worker.reads_documents()
            {
              worker.queue.push_document(Event::Opened(info.clone()));
            }
          }
          if let Some(language) = language {
            let wanting: Vec<usize> = (0..state.extensions.len())
              .filter(|&index| {
                state.extensions[index]
                  .manifest
                  .activates_for_language(&language)
              })
              .collect();
            for index in wanting {
              self.start(state, index, &mut outbox);
            }
          }
        }
        Some(record) => {
          let synced_version = record.synced().map(TextSnapshot::version);
          record.info = info.clone();
          record.path = document.path().map(Path::to_path_buf);
          record.choice = document.language_choice().cloned();
          if synced_version != Some(buffer.version()) {
            let readers: Vec<&Worker> = state
              .extensions
              .iter()
              .filter_map(|entry| match &entry.run {
                Run::Running { worker, .. } if worker.reads_documents() => Some(worker),
                _ => None,
              })
              .collect();
            let changes = synced_version
              .filter(|_| !readers.is_empty())
              .and_then(|version| buffer.edits_since(version))
              .map(|edits| {
                edits
                  .into_iter()
                  .map(|edit| TextChange {
                    range: WitRange {
                      start: point(edit.start_point),
                      end: point(edit.old_end_point),
                    },
                    text: edit.text,
                  })
                  .collect::<Vec<_>>()
              })
              .filter(|changes| {
                changes
                  .iter()
                  .map(|change| change.text.len())
                  .sum::<usize>()
                  <= MAX_PENDING_CHANGE_BYTES
              });
            if record.history.len() == SYNC_HISTORY {
              record.history.pop_front();
            }
            record.history.push_back(buffer.snapshot());
            for worker in readers {
              worker.queue.push_document(Event::Changed {
                document: id.get(),
                version: info.version,
                changes: changes.clone(),
              });
            }
          }
        }
      }
    }
    outbox.send(self.embedder.as_ref());
  }

  /// Stores an extension's diagnostics, converting their ranges in the
  /// text at `version`: the current text, or a recently synced one.
  /// Called with the editor locked.
  pub fn store_diagnostics(
    &self,
    document: &Document,
    extension: &str,
    version: u64,
    convert: impl FnOnce(&TextBuffer) -> Result<Vec<StoredDiagnostic>, ApiError>,
  ) -> Result<(), ApiError> {
    let mut state = self.lock();
    let (items, at, record) = Self::convert_at(&mut state, document, extension, version, convert)?;
    record
      .diagnostics
      .insert(extension.to_owned(), DiagnosticSet { version: at, items });
    Ok(())
  }

  /// Stores an extension's ranges for one decoration type, like
  /// [`Self::store_diagnostics`].
  pub fn store_decorations(
    &self,
    document: &Document,
    extension: &str,
    decoration: &str,
    version: u64,
    convert: impl FnOnce(&TextBuffer) -> Result<Vec<Range<usize>>, ApiError>,
  ) -> Result<(), ApiError> {
    let mut state = self.lock();
    let (ranges, at, record) = Self::convert_at(&mut state, document, extension, version, convert)?;
    let key = (extension.to_owned(), decoration.to_owned());
    if ranges.is_empty() {
      record.decorations.remove(&key);
    } else {
      record.decorations.insert(
        key,
        DecorationSet {
          version: at,
          ranges,
        },
      );
    }
    Ok(())
  }

  /// Stores an extension's code lenses, each at the start of a line given
  /// as an offset into the text at `version`, like [`Self::store_diagnostics`].
  pub fn store_lenses(
    &self,
    document: &Document,
    extension: &str,
    version: u64,
    convert: impl FnOnce(&TextBuffer) -> Result<Vec<(usize, StoredLens)>, ApiError>,
  ) -> Result<(), ApiError> {
    let mut state = self.lock();
    let (lenses, at, record) = Self::convert_at(&mut state, document, extension, version, convert)?;
    if lenses.is_empty() {
      record.lenses.remove(extension);
    } else {
      let (anchors, lenses) = lenses
        .into_iter()
        .map(|(offset, lens)| (offset..offset, lens))
        .unzip();
      record.lenses.insert(
        extension.to_owned(),
        LensSet {
          version: at,
          anchors,
          lenses,
        },
      );
    }
    Ok(())
  }

  /// Stores an extension's line annotations, each at the start of a line
  /// given as an offset into the text at `version`.
  pub fn store_annotations(
    &self,
    document: &Document,
    extension: &str,
    version: u64,
    convert: impl FnOnce(&TextBuffer) -> Result<Vec<(usize, String, Option<String>)>, ApiError>,
  ) -> Result<(), ApiError> {
    let mut state = self.lock();
    let (found, at, record) = Self::convert_at(&mut state, document, extension, version, convert)?;
    if found.is_empty() {
      record.annotations.remove(extension);
    } else {
      let (anchors, annotations) = found
        .into_iter()
        .map(|(offset, text, tooltip)| (offset..offset, (text, tooltip)))
        .unzip();
      record.annotations.insert(
        extension.to_owned(),
        AnnotationSet {
          version: at,
          anchors,
          annotations,
        },
      );
    }
    Ok(())
  }

  /// Converts with the text at `version` (current or recently synced) for
  /// a running extension.
  fn convert_at<'s, T>(
    state: &'s mut State,
    document: &Document,
    extension: &str,
    version: u64,
    convert: impl FnOnce(&TextBuffer) -> Result<T, ApiError>,
  ) -> Result<(T, Version, &'s mut DocumentRecord), ApiError> {
    let current = document.version().get();
    let running = state
      .extensions
      .iter()
      .any(|entry| entry.manifest.id == extension && matches!(entry.run, Run::Running { .. }));
    if !running {
      return Err(ApiError::InvalidArgument(
        "the extension is not running".into(),
      ));
    }
    let record = state
      .documents
      .get_mut(&document.id())
      .ok_or(ApiError::UnknownDocument)?;
    if version == current {
      let converted = convert(document.buffer())?;
      return Ok((converted, document.version(), record));
    }
    let snapshot = record
      .history
      .iter()
      .find(|snapshot| snapshot.version().get() == version)
      .ok_or(ApiError::StaleVersion(current))?;
    let at = snapshot.version();
    let converted = convert(&TextBuffer::from_snapshot(snapshot))?;
    Ok((converted, at, record))
  }
}

fn point(point: flareite_text::TextPoint) -> crate::bindings::flareite::extension::types::Position {
  crate::bindings::flareite::extension::types::Position {
    line: u32::try_from(point.row).unwrap_or(u32::MAX),
    column: u32::try_from(point.column).unwrap_or(u32::MAX),
  }
}

/// A command run waiting for its result.
pub struct CommandTicket {
  receiver: Receiver<Result<(), String>>,
  timeout: Duration,
}

impl CommandTicket {
  /// A ticket for nothing to run.
  fn done() -> Self {
    let (reply, receiver) = mpsc::channel();
    let _ = reply.send(Ok(()));
    Self {
      receiver,
      timeout: Duration::ZERO,
    }
  }

  /// Waits for the command to finish. Fails with the extension's error, or
  /// if the extension stops.
  ///
  /// Every call into the extension has its own deadline, and the worker
  /// always replies, so this normally returns within those deadlines plus
  /// the time to compile the extension (which can't be interrupted). The
  /// timeout is only a safety net against a host bug.
  pub fn wait(self) -> Result<(), String> {
    match self.receiver.recv_timeout(self.timeout) {
      Ok(result) => result,
      Err(mpsc::RecvTimeoutError::Timeout) => Err("The extension did not respond.".into()),
      Err(mpsc::RecvTimeoutError::Disconnected) => Err("The extension stopped.".into()),
    }
  }
}

/// What a permission allows, for asking the user: for language servers,
/// the commands that would run, and for processes, the programs.
fn describe(permission: Permission, manifest: &Manifest) -> String {
  let description = permission.description();
  let contributions = &manifest.contributions;
  let programs: Vec<String> = match permission {
    Permission::LanguageServers => contributions
      .language_servers
      .iter()
      .map(|server| server.command_line())
      .collect(),
    Permission::Processes => contributions
      .programs
      .iter()
      .map(|program| match &program.workspace {
        Some(workspace) => format!("{} (or the folder's {workspace})", program.command),
        None => program.command.clone(),
      })
      .collect(),
    _ => return description.to_owned(),
  };
  format!("{description}: {}", programs.join("; "))
}

/// Loads and runs extensions for an editor. See the crate documentation.
pub struct ExtensionHost {
  shared: Arc<Shared>,
}

impl ExtensionHost {
  /// Creates a host. Nothing is loaded until [`Self::load`].
  pub fn new(embedder: Arc<dyn Embedder>, options: HostOptions) -> Self {
    let decisions = Decisions::load(options.permissions_file.clone());
    let memory = Arc::new(MemoryPool::new(options.limits.total_memory_bytes));
    let shared = Arc::new_cyclic(|me| Shared {
      me: me.clone(),
      embedder,
      runtime: OnceLock::new(),
      askpass: OnceLock::new(),
      memory,
      storage: Storage::new(options.storage_dir.clone()),
      webview_theme: RwLock::new(WebviewTheme::default()),
      builders: crate::development::Builders::default(),
      state: Mutex::new(State {
        options,
        decisions,
        extensions: Vec::new(),
        invalid: Vec::new(),
        documents: HashMap::new(),
        status: BTreeMap::new(),
        views: BTreeMap::new(),
        previews: BTreeMap::new(),
        marks: BTreeMap::new(),
        prompt: None,
        next_prompt: 0,
        provided: HashMap::new(),
        next_request: 0,
        active_file: None,
        timers: Vec::new(),
        next_timer: 0,
        watches: Vec::new(),
        next_watch: 0,
        next_generation: 0,
        shutting_down: false,
        layouts: BTreeMap::new(),
        active_pane: None,
        next_pane_webview: 0,
        schemas: BTreeMap::new(),
        servers: BTreeMap::new(),
      }),
    });
    Self { shared }
  }

  /// (Re)discovers extensions and starts those that activate on startup or
  /// for an open document's language. Running extensions are restarted.
  /// Reads the disk; call without holding the editor lock.
  pub fn load(&self) {
    let (dirs, editor_version) = {
      let state = self.shared.lock();
      let mut dirs: Vec<(PathBuf, bool)> = state
        .options
        .development
        .iter()
        .map(|dir| (dir.clone(), true))
        .collect();
      if let Some(root) = &state.options.extensions_dir {
        dirs.extend(installed_dirs(root).into_iter().map(|dir| (dir, false)));
      }
      (dirs, state.options.editor_version.clone())
    };
    let mut loaded: Vec<(Manifest, bool)> = Vec::new();
    let mut invalid = Vec::new();
    for (dir, development) in dirs {
      match manifest::load(&dir, &editor_version, development) {
        Ok(manifest) => {
          if loaded.iter().any(|(other, _)| other.id == manifest.id) {
            invalid.push(Invalid {
              dir,
              development,
              error: format!(
                "another extension with the id {:?} is already loaded",
                manifest.id
              ),
            });
          } else {
            tracing::info!(extension = %manifest.id, version = %manifest.version, development, "found extension");
            loaded.push((manifest, development));
          }
        }
        Err(error) => {
          tracing::warn!(dir = %dir.display(), %error, "invalid extension");
          invalid.push(Invalid {
            dir,
            development,
            error: error.to_string(),
          });
        }
      }
    }
    loaded.sort_by_key(|(manifest, _)| manifest.name.to_lowercase());

    let mut outbox = Outbox::default();
    {
      let mut state = self.shared.lock();
      if state.shutting_down {
        return;
      }
      for index in 0..state.extensions.len() {
        Shared::stop(&mut state, index, &mut outbox);
      }
      let mut usages: HashMap<String, Arc<Usage>> = state
        .extensions
        .drain(..)
        .map(|entry| (entry.manifest.id.clone(), entry.usage))
        .collect();
      state.extensions = loaded
        .into_iter()
        .map(|(manifest, development)| {
          let (values, config_errors) = config::resolve(
            &manifest.configuration,
            state.options.settings.get(&manifest.id),
          );
          let usage = usages.remove(&manifest.id).unwrap_or_default();
          Entry {
            manifest: Arc::new(manifest),
            development,
            config: Arc::new(RwLock::new(values)),
            config_errors,
            run: Run::Idle,
            usage,
          }
        })
        .collect();
      state.invalid = invalid;
      self.start_wanted(&mut state, &mut outbox);
      outbox.push(Notification::ExtensionsChanged);
    }
    outbox.send(self.shared.embedder.as_ref());
    self.sync_builders();
  }

  pub(crate) fn from_shared(shared: Arc<Shared>) -> Self {
    Self { shared }
  }

  /// Starts, replaces, and stops development extensions' builders to match
  /// their manifests' `[build]`s. Reads the disk.
  fn sync_builders(&self) {
    let dirs = {
      let state = self.shared.lock();
      if state.shutting_down {
        return;
      }
      if state.options.builds {
        state.options.development.clone()
      } else {
        Vec::new()
      }
    };
    let folders = dirs
      .into_iter()
      .map(|dir| {
        let planned = match manifest::read_build_plan(&dir) {
          Ok(Some(plan)) => crate::development::Planned::Plan(plan),
          Ok(None) => crate::development::Planned::None,
          Err(error) => {
            tracing::debug!(dir = %dir.display(), %error, "can't read the build");
            crate::development::Planned::Unreadable
          }
        };
        (dir, planned)
      })
      .collect();
    self.shared.builders.sync(&self.shared.me, folders);
  }

  /// Builds a development extension (its manifest's `[build]`) and loads
  /// it again after, as when a file in its folder changes. If it's
  /// building, it builds again after. Returns whether it has a build.
  pub fn build_development(&self, dir: &Path) -> bool {
    self.shared.builders.build(dir)
  }

  /// Loads a development extension's folder again after it was built:
  /// reads its manifest and restarts it if it was running. Unless
  /// `changed`, only if it isn't loaded (it may have lacked the entry point
  /// the build made). Other extensions are left alone, unless the folder
  /// becomes valid or invalid or changes its id: then all are rediscovered.
  pub(crate) fn reload_development(&self, dir: &Path, changed: bool) {
    let editor_version = {
      let state = self.shared.lock();
      if state.shutting_down || !state.options.development.iter().any(|d| d == dir) {
        return;
      }
      let loaded = state
        .extensions
        .iter()
        .any(|entry| entry.development && entry.manifest.dir == dir);
      if loaded && !changed {
        return;
      }
      state.options.editor_version.clone()
    };
    let loaded = manifest::load(dir, &editor_version, true);
    let mut outbox = Outbox::default();
    {
      let mut state = self.shared.lock();
      if state.shutting_down {
        return;
      }
      let index = state
        .extensions
        .iter()
        .position(|entry| entry.development && entry.manifest.dir == dir);
      match (index, loaded) {
        (Some(index), Ok(manifest)) if manifest.id == state.extensions[index].manifest.id => {
          tracing::info!(extension = %manifest.id, version = %manifest.version, "loading the new build");
          let restart = !matches!(state.extensions[index].run, Run::Idle);
          Shared::stop(&mut state, index, &mut outbox);
          let (values, config_errors) = config::resolve(
            &manifest.configuration,
            state.options.settings.get(&manifest.id),
          );
          let entry = &mut state.extensions[index];
          entry.manifest = Arc::new(manifest);
          entry.config = Arc::new(RwLock::new(values));
          entry.config_errors = config_errors;
          if restart {
            self.shared.start(&mut state, index, &mut outbox);
          }
          self.start_wanted(&mut state, &mut outbox);
          state
            .extensions
            .sort_by_key(|entry| entry.manifest.name.to_lowercase());
        }
        (None, Err(error)) if state.invalid.iter().any(|invalid| invalid.dir == dir) => {
          tracing::warn!(dir = %dir.display(), %error, "invalid extension");
          for invalid in &mut state.invalid {
            if invalid.dir == dir {
              invalid.error = error.to_string();
            }
          }
        }
        _ => {
          drop(state);
          self.load();
          return;
        }
      }
      outbox.push(Notification::ExtensionsChanged);
    }
    outbox.send(self.shared.embedder.as_ref());
    // Its `[build]` may have changed.
    self.sync_builders();
  }

  /// Starts idle extensions that activate on startup or for the language
  /// of an open document.
  fn start_wanted(&self, state: &mut State, outbox: &mut Outbox) {
    let languages: Vec<String> = state
      .documents
      .values()
      .filter_map(|record| record.info.language.clone())
      .collect();
    for index in 0..state.extensions.len() {
      let manifest = &state.extensions[index].manifest;
      let wanted = manifest.activates_on_startup()
        || languages
          .iter()
          .any(|language| manifest.activates_for_language(language));
      if wanted && matches!(state.extensions[index].run, Run::Idle) {
        self.shared.start(state, index, outbox);
      }
    }
  }

  /// Applies new options: rediscovers extensions if the folders changed,
  /// and updates configuration values in place otherwise.
  pub fn configure(&self, options: HostOptions) {
    let reload = {
      let mut guard = self.shared.lock();
      let state = &mut *guard;
      let reload = state.options.extensions_dir != options.extensions_dir
        || state.options.development != options.development
        || state.options.builds != options.builds;
      state.options = options;
      for entry in &mut state.extensions {
        let (values, errors) = config::resolve(
          &entry.manifest.configuration,
          state.options.settings.get(&entry.manifest.id),
        );
        let changed = {
          let mut config = entry.config.write().unwrap_or_else(PoisonError::into_inner);
          let changed = *config != values;
          *config = values;
          changed
        };
        entry.config_errors = errors;
        // Tell it, if it's running and wants to know. (1.16)
        if changed
          && let (Some(command), Run::Running { worker, .. }) =
            (&entry.manifest.on_configuration_changed, &entry.run)
        {
          worker
            .queue
            .push_background(Event::background(command.clone(), None, Payload::None));
        }
      }
      reload
    };
    if reload {
      self.load();
    } else {
      self.shared.embedder.notify(Notification::ExtensionsChanged);
    }
  }

  pub fn statuses(&self) -> Vec<ExtensionStatus> {
    let builds = self.shared.builders.statuses();
    let state = self.shared.lock();
    let mut statuses: Vec<ExtensionStatus> = state
      .extensions
      .iter()
      .map(|entry| {
        let manifest = &entry.manifest;
        let decision = state.decisions.get(&manifest.id);
        let (status, error) = match &entry.run {
          _ if decision.disabled => (ExtensionState::Disabled, None),
          Run::Idle if !decision.pending(&manifest.permissions).is_empty() => {
            (ExtensionState::AwaitingPermission, None)
          }
          _ if manifest.entrypoint.is_none() => (ExtensionState::Ready, None),
          Run::Idle => (ExtensionState::Inactive, None),
          Run::Running { active: false, .. } => (ExtensionState::Starting, None),
          Run::Running { active: true, .. } => (ExtensionState::Active, None),
          Run::Failed(reason) => (ExtensionState::Failed, Some(reason.clone())),
        };
        ExtensionStatus {
          id: manifest.id.clone(),
          name: manifest.name.clone(),
          version: manifest.version.to_string(),
          description: manifest.description.clone(),
          author: manifest.author.clone(),
          has_icon: manifest.icon.is_some(),
          has_readme: manifest.readme.is_some(),
          path: manifest.dir.to_string_lossy().into_owned(),
          development: entry.development,
          state: status,
          error,
          permissions: manifest
            .permissions
            .iter()
            .map(|&permission| PermissionStatus {
              permission,
              description: describe(permission, manifest),
              granted: decision.granted.contains(&permission),
              decided: decision.granted.contains(&permission)
                || decision.denied.contains(&permission),
            })
            .collect(),
          commands: manifest
            .commands
            .iter()
            .map(|command| CommandInfo {
              id: command.id.clone(),
              title: command.title.clone(),
              category: command.category.clone(),
              hidden: command.hidden,
            })
            .collect(),
          config_errors: entry.config_errors.clone(),
          has_settings: !manifest.configuration.is_empty(),
          contributions: manifest.contributions.clone(),
          usage: {
            let mut usage = entry.usage.snapshot();
            if !matches!(entry.run, Run::Running { .. }) {
              usage.memory_bytes = 0;
            }
            usage
          },
          stored_bytes: self.shared.storage.usage(&manifest.id),
          build: builds
            .get(&manifest.dir)
            .filter(|_| entry.development)
            .cloned(),
        }
      })
      .collect();
    statuses.extend(state.invalid.iter().map(|invalid| {
      let name = invalid.dir.file_name().map_or_else(
        || invalid.dir.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
      );
      ExtensionStatus {
        id: invalid.dir.to_string_lossy().into_owned(),
        name,
        version: String::new(),
        description: String::new(),
        author: None,
        has_icon: false,
        has_readme: false,
        path: invalid.dir.to_string_lossy().into_owned(),
        development: invalid.development,
        state: ExtensionState::Invalid,
        error: Some(invalid.error.clone()),
        permissions: Vec::new(),
        commands: Vec::new(),
        config_errors: Vec::new(),
        has_settings: false,
        contributions: crate::Contributions::default(),
        usage: ResourceUsage::default(),
        stored_bytes: 0,
        build: builds
          .get(&invalid.dir)
          .filter(|_| invalid.development)
          .cloned(),
      }
    }));
    statuses
  }

  /// The settings each extension declares, with the values in effect, for
  /// a settings editor. Extensions that declare none are left out.
  pub fn configurations(&self) -> Vec<ExtensionConfiguration> {
    let state = self.shared.lock();
    state
      .extensions
      .iter()
      .filter(|entry| !entry.manifest.configuration.is_empty())
      .map(|entry| {
        let manifest = &entry.manifest;
        let values = entry.config.read().unwrap_or_else(PoisonError::into_inner);
        let user = state.options.settings.get(&manifest.id);
        ExtensionConfiguration {
          id: manifest.id.clone(),
          name: manifest.name.clone(),
          settings: manifest
            .configuration
            .iter()
            .map(|(key, property)| ConfiguredSetting {
              key: key.clone(),
              property: property.clone(),
              value: values
                .get(key)
                .cloned()
                .unwrap_or_else(|| property.default.clone()),
              problem: user
                .and_then(|user| user.get(key))
                .and_then(|value| property.check(value).err()),
            })
            .collect(),
        }
      })
      .collect()
  }

  /// The schemas extensions that aren't turned off provide: those their
  /// manifests declare, then those their code set. (1.16)
  pub fn schemas(&self) -> Vec<crate::schemas::ProvidedSchema> {
    use crate::contributions::SchemaSource;
    use crate::schemas::{Provided, ProvidedSchema};
    let state = self.shared.lock();
    let mut schemas = Vec::new();
    for entry in &state.extensions {
      let id = &entry.manifest.id;
      if state.decisions.get(id).disabled {
        continue;
      }
      for schema in &entry.manifest.contributions.schemas {
        schemas.push(ProvidedSchema {
          extension: id.clone(),
          files: schema.files.clone(),
          source: match &schema.source {
            SchemaSource::Path(path) => Provided::File(path.clone()),
            SchemaSource::Url(url) => Provided::Url(url.clone()),
          },
        });
      }
      for schema in state.schemas.get(id).into_iter().flatten() {
        schemas.push(ProvidedSchema {
          extension: id.clone(),
          files: schema.files.clone(),
          source: Provided::Content {
            id: schema.id.clone(),
            format: schema.format,
            text: schema.content.clone(),
          },
        });
      }
    }
    schemas
  }

  /// The languages contributed by extensions that aren't turned off, with
  /// their files' paths made absolute.
  pub fn languages(&self) -> Vec<LanguageContribution> {
    let state = self.shared.lock();
    state
      .extensions
      .iter()
      .filter(|entry| !state.decisions.get(&entry.manifest.id).disabled)
      .flat_map(|entry| {
        let dir = &entry.manifest.dir;
        entry
          .manifest
          .contributions
          .languages
          .iter()
          .map(move |language| LanguageContribution {
            grammar: dir.join(&language.grammar),
            highlights: dir.join(&language.highlights),
            injections: language.injections.as_ref().map(|path| dir.join(path)),
            folds: language.folds.as_ref().map(|path| dir.join(path)),
            ..language.clone()
          })
      })
      .collect()
  }

  /// Files extensions that aren't turned off add to other extensions'
  /// languages (1.16), in the order extensions load.
  pub fn language_files(&self) -> Vec<crate::contributions::LanguageFiles> {
    let state = self.shared.lock();
    state
      .extensions
      .iter()
      .filter(|entry| !state.decisions.get(&entry.manifest.id).disabled)
      .flat_map(|entry| entry.manifest.contributions.language_files.iter().cloned())
      .collect()
  }

  /// The language servers of extensions that aren't turned off and have
  /// been granted `language-servers`. Commands inside an extension's folder
  /// are made absolute.
  pub fn language_servers(&self) -> Vec<LanguageServerContribution> {
    let state = self.shared.lock();
    state
      .extensions
      .iter()
      .filter(|entry| {
        let decision = state.decisions.get(&entry.manifest.id);
        !decision.disabled && decision.granted.contains(&Permission::LanguageServers)
      })
      .flat_map(|entry| {
        let dir = &entry.manifest.dir;
        let changed = state.servers.get(&entry.manifest.id);
        entry
          .manifest
          .contributions
          .language_servers
          .iter()
          .map(move |server| {
            let changes = changed.and_then(|servers| servers.get(&server.name));
            LanguageServerContribution {
              command: if server.command.contains(['/', '\\']) {
                dir.join(&server.command).to_string_lossy().into_owned()
              } else {
                server.command.clone()
              },
              args: changes
                .and_then(|changes| changes.args.clone())
                .unwrap_or_else(|| server.args.clone()),
              settings: changes
                .and_then(|changes| changes.settings.clone())
                .unwrap_or_else(|| server.settings.clone()),
              ..server.clone()
            }
          })
      })
      .collect()
  }

  /// Whether the language server `name` should wait before starting, for
  /// the extension declaring it to activate and configure it: the
  /// extension has code, activates on startup or for one of the server's
  /// languages, and is starting (or will start, being idle with nothing to
  /// decide). Not once it failed, or is active.
  pub fn server_waits(&self, name: &str) -> bool {
    let state = self.shared.lock();
    state.extensions.iter().any(|entry| {
      let manifest = &entry.manifest;
      let Some(server) = manifest
        .contributions
        .language_servers
        .iter()
        .find(|server| server.name == name)
      else {
        return false;
      };
      let activates = manifest.activation.iter().any(|event| match event {
        manifest::Activation::Startup => true,
        manifest::Activation::Language(language) => server.languages.contains(language),
        _ => false,
      });
      let decision = state.decisions.get(&manifest.id);
      manifest.entrypoint.is_some()
        && activates
        && !decision.disabled
        && match &entry.run {
          Run::Running { active, .. } => !active,
          Run::Idle => decision.pending(&manifest.permissions).is_empty(),
          Run::Failed(_) => false,
        }
    })
  }

  /// An icon theme (`<extension id>.<icon theme id>`) from an extension that
  /// isn't turned off.
  /// A loaded extension's manifest.
  fn manifest(&self, id: &str) -> Option<Arc<Manifest>> {
    let state = self.shared.lock();
    state
      .extensions
      .iter()
      .find(|entry| entry.manifest.id == id)
      .map(|entry| entry.manifest.clone())
  }

  /// An extension's icon, as a `data:` URL; `None` if it has none.
  pub fn icon(&self, id: &str) -> Option<Result<String, String>> {
    let icon = self.manifest(id)?.icon.clone()?;
    Some(preview::load_image(&icon))
  }

  /// An extension's README, as preview content (links and pictures
  /// resolved from its folder); `None` if it has none.
  pub fn readme(&self, id: &str) -> Option<Result<PreviewContent, String>> {
    let manifest = self.manifest(id)?;
    let path = manifest.readme.as_ref()?;
    Some(crate::readme::render(path, &manifest.dir))
  }

  /// A picture in an extension's README, as a `data:` URL. Only pictures in
  /// its folder are shown.
  pub fn readme_image(&self, id: &str, path: &Path) -> Result<String, String> {
    let manifest = self.manifest(id).ok_or("no such extension")?;
    crate::readme::image(&manifest.dir, path)
  }

  pub fn icon_theme(&self, id: &str) -> Option<Arc<IconTheme>> {
    let state = self.shared.lock();
    state
      .extensions
      .iter()
      .filter(|entry| !state.decisions.get(&entry.manifest.id).disabled)
      .flat_map(|entry| &entry.manifest.contributions.icon_themes)
      .find(|theme| theme.id == id)
      .map(|theme| theme.theme.clone())
  }

  /// Looks up the languages of open documents again, after the embedder's
  /// languages changed (an extension contributed one), and starts
  /// extensions that activate for them.
  pub fn languages_changed(&self) {
    let mut outbox = Outbox::default();
    {
      let mut guard = self.shared.lock();
      let state = &mut *guard;
      if state.shutting_down {
        return;
      }
      for record in state.documents.values_mut() {
        record.info.language = api::language_of(
          self.shared.embedder.as_ref(),
          record.path.as_deref(),
          record.choice.as_ref(),
        );
      }
      self.start_wanted(state, &mut outbox);
    }
    outbox.send(self.shared.embedder.as_ref());
  }

  /// Runs an extension command, starting the extension if needed.
  /// `document` is the active document, whose selections are passed along.
  /// Call without holding the editor lock.
  pub fn run_command(
    &self,
    command: &str,
    document: Option<DocumentId>,
    argument: Option<String>,
  ) -> Result<CommandTicket, String> {
    self.run(command, document, argument, Payload::None)
  }

  /// Runs an extension command with a payload, and no document or argument.
  pub(crate) fn run_with_payload(
    &self,
    command: &str,
    payload: Payload,
  ) -> Result<CommandTicket, String> {
    self.run(command, None, None, payload)
  }

  /// Runs an extension command, with what it's run for, such as a
  /// webview's message.
  fn run(
    &self,
    command: &str,
    document: Option<DocumentId>,
    argument: Option<String>,
    payload: Payload,
  ) -> Result<CommandTicket, String> {
    let selections = document
      .and_then(|id| {
        api::with_editor(&self.shared, |editor| {
          editor.document(id).ok().map(|document| {
            let buffer = document.buffer();
            let selections = document.selections();
            let primary = selections.primary();
            std::iter::once(primary)
              .chain(selections.iter().copied().filter(|s| *s != primary))
              .map(|selection| {
                let range = selection.range();
                WitRange {
                  start: to_position(buffer, range.start),
                  end: to_position(buffer, range.end),
                }
              })
              .collect::<Vec<_>>()
          })
        })
        .flatten()
      })
      .unwrap_or_default();
    let mut outbox = Outbox::default();
    let result = {
      let mut state = self.shared.lock();
      let index = state
        .extensions
        .iter()
        .position(|entry| entry.manifest.declares_command(command))
        .ok_or_else(|| format!("No extension provides the command {command}."))?;
      let entry = &state.extensions[index];
      let name = entry.manifest.name.clone();
      let decision = state.decisions.get(&entry.manifest.id);
      if decision.disabled {
        return Err(format!("{name} is disabled."));
      }
      if !decision.pending(&entry.manifest.permissions).is_empty() {
        return Err(format!(
          "{name} is waiting for you to allow or deny its permissions."
        ));
      }
      if let Run::Failed(reason) = &entry.run {
        return Err(format!(
          "{name} has stopped ({reason}). Restart it to try again."
        ));
      }
      self.shared.start(&mut state, index, &mut outbox);
      let command_time = state.options.limits.command_time;
      match &state.extensions[index].run {
        Run::Running { worker, .. } => {
          let (reply, receiver) = mpsc::channel();
          let context = CommandContext {
            document: document.map(DocumentId::get),
            selections,
          };
          worker
            .queue
            .push_command(Event::Command {
              command: command.to_owned(),
              context,
              argument,
              payload,
              reply,
            })
            .map(|()| CommandTicket {
              receiver,
              timeout: command_time + state.options.limits.activation_time + COMMAND_SAFETY_MARGIN,
            })
        }
        _ => Err(format!("{name} could not be started.")),
      }
    };
    outbox.send(self.shared.embedder.as_ref());
    result
  }

  /// Records the user's permission decision for an extension: `granted`
  /// are allowed, its other requested permissions denied. A running
  /// extension is restarted with the new permissions.
  pub fn set_permissions(&self, id: &str, granted: &[Permission]) {
    self.update_decision(id, |decision, requested| {
      decision.granted = requested
        .iter()
        .copied()
        .filter(|p| granted.contains(p))
        .collect();
      decision.denied = requested
        .iter()
        .copied()
        .filter(|p| !granted.contains(p))
        .collect();
    });
  }

  pub fn set_enabled(&self, id: &str, enabled: bool) {
    self.update_decision(id, |decision, _| decision.disabled = !enabled);
  }

  /// Forgets the user's decisions about an extension (its permissions, and
  /// whether it's turned off), as when it's uninstalled: installed again,
  /// it asks again. Takes effect for a loaded extension when it next loads.
  pub fn forget_decisions(&self, id: &str) {
    self
      .shared
      .lock()
      .decisions
      .set(id, crate::permissions::Decision::default());
  }

  fn update_decision(&self, id: &str, change: impl FnOnce(&mut Decision, &[Permission])) {
    let mut outbox = Outbox::default();
    {
      let mut state = self.shared.lock();
      let Some(index) = state
        .extensions
        .iter()
        .position(|entry| entry.manifest.id == id)
      else {
        return;
      };
      let mut decision = state.decisions.get(id);
      let before = decision.clone();
      change(&mut decision, &state.extensions[index].manifest.permissions);
      if decision == before {
        return;
      }
      state.decisions.set(id, decision);
      Shared::stop(&mut state, index, &mut outbox);
      self.start_wanted(&mut state, &mut outbox);
    }
    outbox.send(self.shared.embedder.as_ref());
  }

  /// The status bar items to show, from enabled extensions that haven't
  /// failed.
  pub fn status_items(&self) -> Vec<StatusItem> {
    let state = self.shared.lock();
    let mut items = Vec::new();
    for entry in &state.extensions {
      let manifest = &entry.manifest;
      if state.decisions.get(&manifest.id).disabled || matches!(entry.run, Run::Failed(_)) {
        continue;
      }
      for declared in &manifest.contributions.status_items {
        let content = match state
          .status
          .get(&(manifest.id.clone(), declared.id.clone()))
        {
          Some(content) => content.clone(),
          None => declared.text.clone().map(|text| StatusContent {
            text,
            tooltip: declared.tooltip.clone(),
            command: declared.command.clone(),
          }),
        };
        if let Some(content) = content {
          items.push(StatusItem {
            id: format!("{}.{}", manifest.id, declared.id),
            extension: manifest.name.clone(),
            alignment: declared.alignment,
            priority: declared.priority,
            content,
          });
        }
      }
    }
    items
  }

  /// The content of a view (by full id), as last set by its extension.
  pub fn view_content(&self, full_id: &str) -> ViewContent {
    self
      .shared
      .lock()
      .views
      .get(full_id)
      .cloned()
      .unwrap_or_default()
  }

  /// The open previews, in the order of their ids. Viewers aren't listed.
  pub fn previews(&self) -> Vec<PreviewInfo> {
    self
      .shared
      .lock()
      .previews
      .values()
      .filter(|record| record.file.is_none() && record.pane.is_none())
      .map(|record| record.info.clone())
      .collect()
  }

  /// Opens `path` in the viewer `viewer` (`<extension id>.<viewer id>`),
  /// or shows it again, and runs the viewer's command with the path as its
  /// argument so the extension fills it in: when the tab opens, when the
  /// file changes, and when the extension starts again. Its content and
  /// pictures are then read like a preview's, by the viewer's preview id,
  /// `<viewer>/<path>` ([`viewer_id`]).
  pub fn open_viewer(&self, viewer: &str, path: &Path) -> Result<CommandTicket, String> {
    let (extension, name, command, page) = {
      let state = self.shared.lock();
      let entry = state
        .extensions
        .iter()
        .find(|entry| {
          entry
            .manifest
            .contributions
            .viewers
            .iter()
            .any(|v| v.full_id == viewer)
        })
        .ok_or_else(|| format!("No extension provides the viewer {viewer}."))?;
      let declared = entry
        .manifest
        .contributions
        .viewers
        .iter()
        .find(|v| v.full_id == viewer);
      (
        entry.manifest.id.clone(),
        entry.manifest.name.clone(),
        declared.and_then(|v| v.command.clone()),
        declared.and_then(|v| v.webview.as_ref().map(|webview| webview.page.clone())),
      )
    };
    let id = viewer_id(viewer, path);
    let id_for_state = id.clone();
    {
      let mut state = self.shared.lock();
      let record = state
        .previews
        .entry(id.clone())
        .or_insert_with(|| PreviewRecord {
          extension,
          info: PreviewInfo {
            id,
            extension: name,
            title: path.file_name().map_or_else(
              || path.display().to_string(),
              |name| name.to_string_lossy().into_owned(),
            ),
            icon: "book".into(),
            document: None,
            webview: page.is_some(),
          },
          content: Arc::default(),
          images: BTreeSet::new(),
          file: Some(Viewed {
            path: path.to_path_buf(),
            command: command.clone(),
            stale: false,
          }),
          pane: None,
          webview: page.map(|page| {
            let mut webview = WebviewRecord::new(page);
            webview.state = self.shared.storage.tab_state(&id_for_state);
            webview
          }),
        });
      // It's about to be filled in.
      if let Some(viewed) = &mut record.file {
        viewed.stale = false;
      }
    }
    match command {
      Some(command) => self.run_command(&command, None, Some(path.display().to_string())),
      // A webview without a command: its page shows the file by itself.
      None => Ok(CommandTicket::done()),
    }
  }

  /// How the workbench shows an open webview; `None` if it isn't open or
  /// isn't a webview.
  pub fn webview(&self, id: &str) -> Option<WebviewInfo> {
    let record = {
      let state = self.shared.lock();
      state.previews.get(id)?.webview.clone()?
    };
    Some(self.pane_webview_info(&record))
  }

  /// Answers a webview page's request for `path` (percent-encoded) under
  /// its address `token`. Reads the disk.
  pub fn webview_resource(&self, token: &str, path: &str) -> Result<Resource, String> {
    let file_page = {
      let state = self.shared.lock();
      state.previews.values().find_map(|record| {
        let webview = record.webview.as_ref()?;
        (webview.token == token)
          .then(|| {
            webview
              .file_page
              .clone()
              .map(|file| (webview.clone(), file, record.extension.clone()))
          })
          .flatten()
      })
    };
    if let Some((record, file, extension)) = file_page {
      return self.file_page_resource(&record, &file, &extension, path);
    }
    let (dir, record, file, network) = {
      let state = self.shared.lock();
      let (record, webview) = state
        .previews
        .values()
        .find_map(|record| {
          let webview = record.webview.as_ref()?;
          (webview.token == token).then_some((record, webview))
        })
        .ok_or("no webview has this address")?;
      let manifest = &state
        .extensions
        .iter()
        .find(|entry| entry.manifest.id == record.extension)
        .ok_or("the webview's extension isn't loaded")?
        .manifest;
      let network = state
        .decisions
        .get(&manifest.id)
        .effective(&manifest.permissions)
        .contains(&Permission::Network);
      (
        manifest.dir.clone(),
        webview.clone(),
        record.file.as_ref().map(|viewed| viewed.path.clone()),
        network,
      )
    };
    let theme = self
      .shared
      .webview_theme
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .clone();
    webview::serve(
      &webview::Site {
        dir: &dir,
        record: &record,
        file: file.as_deref(),
        network,
        theme: &theme,
      },
      path,
    )
  }

  /// The workbench's theme changed: pages loaded from now on get it. (Pages
  /// already loaded get it from the workbench.)
  pub fn set_webview_theme(
    &self,
    kind: &str,
    variables: BTreeMap<String, String>,
  ) -> Result<(), String> {
    let theme = WebviewTheme::new(kind, variables)?;
    *self
      .shared
      .webview_theme
      .write()
      .unwrap_or_else(PoisonError::into_inner) = theme;
    Ok(())
  }

  /// A webview's page saved its state (JSON text), which it gets back when
  /// it loads again: after its tab was hidden, and for a viewer, in the
  /// next session.
  pub fn set_webview_state(&self, id: &str, state: String) -> Result<(), String> {
    if state.len() > crate::storage::MAX_TAB_STATE_BYTES {
      return Err(format!(
        "a page's state must be at most {} KiB",
        crate::storage::MAX_TAB_STATE_BYTES / 1024
      ));
    }
    crate::storage::check_json(&state)?;
    let viewer = {
      let mut shared = self.shared.lock();
      let record = shared
        .previews
        .get_mut(id)
        .filter(|record| record.webview.is_some())
        .ok_or("the webview isn't open")?;
      if let Some(webview) = record.webview.as_mut() {
        webview.state = Some(state.clone());
      }
      record.file.is_some()
    };
    if viewer {
      self.shared.storage.set_tab_state(id, Some(state));
    }
    Ok(())
  }

  /// The extension that owns an open webview, and the storage scope a page
  /// names (`global` or `workspace`).
  fn webview_scope(&self, id: &str, scope: &str) -> Result<(String, Option<Scope>), String> {
    let extension = self
      .shared
      .lock()
      .previews
      .get(id)
      .filter(|record| record.webview.is_some())
      .map(|record| record.extension.clone())
      .ok_or("the webview isn't open")?;
    let scope = match scope {
      "global" => Some(Scope::Global),
      "workspace" => self
        .shared
        .embedder
        .open_folder()
        .map(|folder| Scope::Workspace(folder.root)),
      other => return Err(format!("no storage scope {other:?}")),
    };
    Ok((extension, scope))
  }

  /// A stored value (JSON text) of a webview's extension.
  pub fn webview_storage_get(
    &self,
    id: &str,
    scope: &str,
    key: &str,
  ) -> Result<Option<String>, String> {
    let (extension, scope) = self.webview_scope(id, scope)?;
    Ok(scope.and_then(|scope| self.shared.storage.get(&extension, &scope, key)))
  }

  /// Sets (or removes, with `None`) a stored value of a webview's extension.
  pub fn webview_storage_set(
    &self,
    id: &str,
    scope: &str,
    key: &str,
    value: Option<String>,
  ) -> Result<(), String> {
    let (extension, scope) = self.webview_scope(id, scope)?;
    let scope = scope.ok_or("no folder is open")?;
    self
      .shared
      .storage
      .set(&extension, &scope, key, value.clone())
      .map_err(|error| error.to_string())?;
    self
      .shared
      .storage_changed(&extension, &scope, key, value, Some(id));
    Ok(())
  }

  /// The keys of a webview's extension's stored values.
  pub fn webview_storage_keys(&self, id: &str, scope: &str) -> Result<Vec<String>, String> {
    let (extension, scope) = self.webview_scope(id, scope)?;
    Ok(scope.map_or_else(Vec::new, |scope| {
      self.shared.storage.keys(&extension, &scope)
    }))
  }

  /// Removes everything an extension stores.
  pub fn clear_storage(&self, id: &str) -> Result<(), String> {
    self.shared.storage.clear(id)?;
    self.shared.embedder.notify(Notification::StorageChanged {
      extension: id.to_owned(),
      scope: None,
      key: None,
      value: None,
      source: None,
    });
    self.shared.embedder.notify(Notification::ExtensionsChanged);
    Ok(())
  }

  /// A webview's page sent a message (JSON text): runs the command its
  /// manifest entry names as `on_message`, starting the extension if
  /// needed. Messages to a webview without one are dropped.
  pub fn webview_message(&self, id: &str, data: String) -> Result<(), String> {
    if data.len() > webview::MAX_MESSAGE_BYTES {
      return Err(format!(
        "messages must be at most {} KiB",
        webview::MAX_MESSAGE_BYTES / 1024
      ));
    }
    serde_json::from_str::<serde::de::IgnoredAny>(&data)
      .map_err(|error| format!("the message isn't JSON: {error}"))?;
    let pane = self
      .shared
      .lock()
      .previews
      .get(id)
      .and_then(|record| record.pane.clone());
    if let Some((tab, pane)) = pane {
      return self.pane_message(tab, pane, data);
    }
    let (command, source) = {
      let state = self.shared.lock();
      let record = state
        .previews
        .get(id)
        .filter(|record| record.webview.is_some())
        .ok_or("the webview isn't open")?;
      let manifest = &state
        .extensions
        .iter()
        .find(|entry| entry.manifest.id == record.extension)
        .ok_or("the webview's extension isn't loaded")?
        .manifest;
      // `<extension id>.<viewer or preview id>/<path or key>`.
      let (full, rest) = id.split_once('/').ok_or("not a webview id")?;
      let local = full
        .strip_prefix(&format!("{}.", manifest.id))
        .ok_or("not a webview id")?;
      let contributions = &manifest.contributions;
      match &record.file {
        Some(viewed) => (
          contributions
            .viewer(local)
            .and_then(|viewer| viewer.webview.as_ref()?.on_message.clone()),
          WitWebview::Viewer(ViewedFile {
            viewer: local.to_owned(),
            path: viewed.path.display().to_string(),
          }),
        ),
        None => (
          contributions
            .preview(local)
            .and_then(|preview| preview.webview.as_ref()?.on_message.clone()),
          WitWebview::Preview(PreviewInstance {
            preview: local.to_owned(),
            key: rest.to_owned(),
          }),
        ),
      }
    };
    let Some(command) = command else {
      return Ok(());
    };
    self.run(
      &command,
      None,
      None,
      Payload::Message(WebviewMessage { source, data }),
    )?;
    Ok(())
  }

  /// A preview's content, as last set by its extension; `None` if it isn't
  /// open.
  pub fn preview_content(&self, id: &str) -> Option<Arc<PreviewContent>> {
    self
      .shared
      .lock()
      .previews
      .get(id)
      .map(|record| record.content.clone())
  }

  /// The user closed a preview's tab, or a viewer's, whose saved state goes.
  pub fn preview_closed(&self, id: &str) {
    let viewer = self
      .shared
      .lock()
      .previews
      .get(id)
      .is_some_and(|record| record.file.is_some());
    if viewer {
      self.shared.storage.set_tab_state(id, None);
    }
    self.shared.close_preview(id);
  }

  /// Whether a preview's content shows the picture file `path`, which the
  /// workbench may then load for it.
  pub fn preview_shows_image(&self, id: &str, path: &Path) -> bool {
    self
      .shared
      .lock()
      .previews
      .get(id)
      .is_some_and(|record| record.images.contains(path))
  }

  /// A picture a preview shows, as a `data:` URL. Only the files its
  /// content shows can be read. Reads the disk.
  pub fn preview_image(&self, id: &str, path: &Path) -> Result<String, String> {
    if !self.preview_shows_image(id, path) {
      return Err("the preview doesn't show this picture".into());
    }
    crate::preview::load_image(path)
  }

  /// Called when a view is shown: starts its extension if it activates for
  /// the view.
  pub fn view_opened(&self, full_id: &str) {
    let mut outbox = Outbox::default();
    {
      let mut state = self.shared.lock();
      let index = state.extensions.iter().position(|entry| {
        entry
          .manifest
          .contributions
          .views
          .iter()
          .any(|view| view.full_id == full_id && entry.manifest.activates_for_view(&view.id))
      });
      if let Some(index) = index
        && matches!(state.extensions[index].run, Run::Idle)
      {
        self.shared.start(&mut state, index, &mut outbox);
      }
    }
    outbox.send(self.shared.embedder.as_ref());
  }

  /// Extension decorations overlapping `bytes`, mapped to the current text.
  /// Call with the editor locked.
  pub fn decorations(&self, document: &Document, bytes: Range<usize>) -> Vec<ExtensionDecoration> {
    let mut state = self.shared.lock();
    let Some(record) = state.documents.get_mut(&document.id()) else {
      return Vec::new();
    };
    let mut found = Vec::new();
    for ((extension, name), set) in &mut record.decorations {
      set.catch_up(document.buffer());
      let key = format!("{extension}/{name}");
      found.extend(
        set
          .ranges
          .iter()
          .filter(|range| range.start <= bytes.end && range.end >= bytes.start)
          .map(|range| ExtensionDecoration {
            range: range.clone(),
            key: key.clone(),
          }),
      );
    }
    found
  }

  /// Extensions' code lenses in a document, mapped to the current text, in
  /// order. Call with the editor locked.
  pub fn code_lenses(&self, document: &Document) -> Vec<ExtensionLens> {
    let mut state = self.shared.lock();
    let Some(record) = state.documents.get_mut(&document.id()) else {
      return Vec::new();
    };
    let buffer = document.buffer();
    let mut found = Vec::new();
    for set in record.lenses.values_mut() {
      set.catch_up(buffer);
      for (anchor, lens) in set.anchors.iter().zip(&set.lenses) {
        // Edits may leave an anchor inside a line: it stays with that line.
        let line = buffer.line_of(anchor.start.min(buffer.len()));
        found.push(ExtensionLens {
          offset: buffer.line_start(line),
          title: lens.title.clone(),
          tooltip: lens.tooltip.clone(),
          command: lens.command.clone(),
          argument: lens.argument.clone(),
        });
      }
    }
    found.sort_by_key(|lens| lens.offset);
    found
  }

  /// Extensions' line annotations on lines starting in `bytes`, mapped to
  /// the current text. Call with the editor locked.
  pub fn line_annotations(
    &self,
    document: &Document,
    bytes: Range<usize>,
  ) -> Vec<ExtensionAnnotation> {
    let mut state = self.shared.lock();
    let Some(record) = state.documents.get_mut(&document.id()) else {
      return Vec::new();
    };
    let buffer = document.buffer();
    let mut found: Vec<ExtensionAnnotation> = Vec::new();
    for set in record.annotations.values_mut() {
      set.catch_up(buffer);
      for (anchor, (text, tooltip)) in set.anchors.iter().zip(&set.annotations) {
        let offset = buffer.line_start(buffer.line_of(anchor.start.min(buffer.len())));
        if bytes.contains(&offset) || offset == bytes.end {
          found.push(ExtensionAnnotation {
            offset,
            text: text.clone(),
            tooltip: tooltip.clone(),
          });
        }
      }
    }
    found.sort_by_key(|annotation| annotation.offset);
    found.dedup_by_key(|annotation| annotation.offset);
    found
  }

  /// The cursor moved to another line, or another document is in use:
  /// runs running extensions' `on_selection_changed` commands with it as
  /// their context. Call without holding the editor lock.
  pub fn selection_changed(&self, document: Option<DocumentId>) {
    let selections = document
      .and_then(|id| {
        api::with_editor(&self.shared, |editor| {
          editor.document(id).ok().map(|document| {
            let buffer = document.buffer();
            let primary = document.selections().primary().range();
            vec![WitRange {
              start: to_position(buffer, primary.start),
              end: to_position(buffer, primary.end),
            }]
          })
        })
        .flatten()
      })
      .unwrap_or_default();
    let state = self.shared.lock();
    for entry in &state.extensions {
      if let (Some(command), Run::Running { worker, .. }) =
        (&entry.manifest.on_selection_changed, &entry.run)
      {
        let (reply, _) = mpsc::channel();
        worker.queue.push_background(Event::Command {
          command: command.clone(),
          context: CommandContext {
            document: document.map(DocumentId::get),
            selections: selections.clone(),
          },
          argument: None,
          payload: Payload::None,
          reply,
        });
      }
    }
  }

  /// Runs a command from a view's menu or button, for the items selected
  /// (`selection`, their arguments; see `views::selection`). `argument`
  /// is the clicked item's.
  pub fn run_view_command(
    &self,
    command: &str,
    argument: Option<String>,
    selection: Vec<String>,
  ) -> Result<CommandTicket, String> {
    self.run(command, None, argument, Payload::Selection(selection))
  }

  /// The user opened a view item that has no children listed: runs the
  /// view's `on_expand` command for it, starting the extension if needed.
  pub fn expand_view_item(&self, view: &str, item: &str) -> Result<CommandTicket, String> {
    let (command, local) = {
      let state = self.shared.lock();
      let declared = state
        .extensions
        .iter()
        .flat_map(|entry| &entry.manifest.contributions.views)
        .find(|declared| declared.full_id == view)
        .ok_or_else(|| format!("No extension provides the view {view}."))?;
      (declared.on_expand.clone(), declared.id.clone())
    };
    let Some(command) = command else {
      return Ok(CommandTicket::done());
    };
    let argument = self
      .shared
      .view_snapshot(view)
      .items
      .into_iter()
      .find(|entry| entry.id == item)
      .ok_or_else(|| format!("The view has no item {item}."))?
      .argument;
    self.run(
      &command,
      None,
      None,
      Payload::Expand(crate::bindings::flareite::extension::views::ExpandingItem {
        view: local,
        item: item.to_owned(),
        argument,
      }),
    )
  }

  /// The text at a provided address (`None` when there's nothing there),
  /// from the extension declaring its scheme. Blocks until the provider's
  /// command finishes, so call it off the UI thread.
  pub fn provide_text(&self, uri: &str) -> Result<Option<String>, String> {
    let scheme = crate::documents::scheme_of(uri)
      .ok_or_else(|| format!("{uri} isn't an address an extension provides."))?;
    let (command, id) = {
      let mut state = self.shared.lock();
      let command = state
        .extensions
        .iter()
        .find_map(|entry| entry.manifest.contributions.text_provider(scheme))
        .map(|provider| provider.command.clone())
        .ok_or_else(|| format!("No extension provides {scheme}: addresses."))?;
      state.next_request += 1;
      (command, state.next_request)
    };
    let request = ProvideRequest {
      id,
      uri: uri.to_owned(),
    };
    let result = self
      .run(&command, None, None, Payload::Provide(request))
      .and_then(CommandTicket::wait);
    let answer = self.shared.lock().provided.remove(&id);
    result?;
    answer.ok_or_else(|| format!("The extension provided nothing for {uri}."))
  }

  /// The original of the file at `path` (an absolute path), from the first
  /// extension with `[quick_diff]` that names one: an address it provides.
  /// Blocks while the extensions answer, so call it off the UI thread.
  pub fn original_of(&self, path: &str) -> Option<String> {
    let commands: Vec<String> = self
      .shared
      .lock()
      .extensions
      .iter()
      .filter_map(|entry| entry.manifest.contributions.quick_diff.as_ref())
      .map(|quick_diff| quick_diff.original.clone())
      .collect();
    commands.into_iter().find_map(|command| {
      let id = {
        let mut state = self.shared.lock();
        state.next_request += 1;
        state.next_request
      };
      let request = crate::quick_diff::OriginalRequest {
        id,
        path: path.to_owned(),
      };
      let result = self
        .run(&command, None, None, Payload::Original(request))
        .and_then(CommandTicket::wait);
      let answer = self.shared.lock().provided.remove(&id);
      if let Err(message) = result {
        tracing::debug!(%command, %message, "asking for an original failed");
      }
      answer.flatten()
    })
  }

  /// What applying changes to the original at `uri` is called (such as
  /// "Stage"), if its extension can.
  pub fn apply_title(&self, uri: &str) -> Option<String> {
    let scheme = crate::documents::scheme_of(uri)?;
    self
      .shared
      .lock()
      .extensions
      .iter()
      .find(|entry| entry.manifest.contributions.text_provider(scheme).is_some())?
      .manifest
      .contributions
      .quick_diff
      .as_ref()?
      .apply_title
      .clone()
  }

  /// Makes the original at `uri` `text`, by the `apply` command of the
  /// extension providing it. Blocks until the command finishes.
  pub fn apply_to_original(&self, uri: &str, text: String) -> Result<(), String> {
    crate::quick_diff::check_applied(&text)?;
    let scheme = crate::documents::scheme_of(uri)
      .ok_or_else(|| format!("{uri} isn't an address an extension provides."))?;
    let command = self
      .shared
      .lock()
      .extensions
      .iter()
      .find(|entry| entry.manifest.contributions.text_provider(scheme).is_some())
      .and_then(|entry| {
        entry
          .manifest
          .contributions
          .quick_diff
          .as_ref()?
          .apply
          .clone()
      })
      .ok_or_else(|| format!("No extension can change {uri}."))?;
    let applied = crate::bindings::flareite::extension::quick_diff::Applied {
      uri: uri.to_owned(),
      text,
    };
    self
      .run(&command, None, None, Payload::Apply(applied))
      .and_then(CommandTicket::wait)
  }

  /// The user changed the text in a view's text box. Extensions read it
  /// when they need it, so they aren't told.
  pub fn set_view_input(&self, view: &str, text: String) {
    if text.len() <= 64 * 1024 {
      self
        .shared
        .lock()
        .views
        .entry(view.to_owned())
        .or_default()
        .input = text;
    }
  }

  /// The badges of views that have one, by full view id.
  pub fn view_badges(&self) -> BTreeMap<String, u32> {
    self
      .shared
      .lock()
      .views
      .iter()
      .filter(|(_, content)| content.badge > 0)
      .map(|(id, content)| (id.clone(), content.badge))
      .collect()
  }

  #[cfg(test)]
  pub(crate) fn shared(&self) -> &Arc<Shared> {
    &self.shared
  }

  /// The user answered a prompt (or closed it): runs its command. Answers
  /// to a prompt no longer showing are ignored.
  pub fn prompt_answered(&self, id: u64, reply: PromptReply) {
    let prompt = {
      let mut state = self.shared.lock();
      if state.prompt.as_ref().is_some_and(|prompt| prompt.id == id) {
        state.prompt.take()
      } else {
        None
      }
    };
    if let Some(prompt) = prompt {
      self.shared.answer_prompt(prompt, reply);
    }
  }

  /// Files in the open folder changed on disk: runs the commands of
  /// extensions watching them.
  pub fn files_changed(&self, changed: &[PathBuf]) {
    self
      .shared
      .dispatch_changes(changed, |watch| watch.own_watcher.is_none());
  }

  /// The open folder, or whether it's trusted, changed: runs running
  /// extensions' `on_folder_changed` commands.
  pub fn folder_changed(&self) {
    let state = self.shared.lock();
    for entry in &state.extensions {
      if let (Some(command), Run::Running { worker, .. }) =
        (&entry.manifest.on_folder_changed, &entry.run)
      {
        worker
          .queue
          .push_background(Event::background(command.clone(), None, Payload::None));
      }
    }
  }

  /// The file of the editor in use changed (`None`: none shows a file):
  /// runs running extensions' `on_active_file_changed` commands.
  pub fn active_file_changed(&self, path: Option<PathBuf>) {
    let mut state = self.shared.lock();
    if state.active_file == path {
      return;
    }
    state.active_file = path;
    for entry in &state.extensions {
      if let (Some(command), Run::Running { worker, .. }) =
        (&entry.manifest.on_active_file_changed, &entry.run)
      {
        worker
          .queue
          .push_background(Event::background(command.clone(), None, Payload::None));
      }
    }
  }

  /// Every extension's explorer marks, from extensions that are running.
  pub fn file_marks(&self) -> Vec<ExtensionMarks> {
    let state = self.shared.lock();
    state
      .marks
      .iter()
      .map(|(id, marks)| ExtensionMarks {
        extension: id.clone(),
        marks: marks.clone(),
      })
      .collect()
  }

  /// Restarts an extension (e.g. after it failed, or was rebuilt).
  pub fn restart(&self, id: &str) {
    let mut outbox = Outbox::default();
    {
      let mut state = self.shared.lock();
      let Some(index) = state
        .extensions
        .iter()
        .position(|entry| entry.manifest.id == id)
      else {
        return;
      };
      Shared::stop(&mut state, index, &mut outbox);
      self.shared.start(&mut state, index, &mut outbox);
    }
    outbox.send(self.shared.embedder.as_ref());
  }

  /// Brings the host's view of `document` up to date: announces new
  /// documents and sends changes to extensions. Call with the editor
  /// locked, whenever a document may have changed (e.g. on every view).
  pub fn sync(&self, document: &Document) {
    self.shared.sync(document);
  }

  /// Forgets a closed document, closes its previews, and tells extensions.
  pub fn closed(&self, id: DocumentId) {
    let previews_closed = {
      let mut state = self.shared.lock();
      let before = state.previews.len();
      state
        .previews
        .retain(|_, preview| preview.info.document != Some(id.get()));
      let previews_closed = state.previews.len() != before;
      if state.documents.remove(&id).is_some() {
        for entry in &state.extensions {
          if let Run::Running { worker, .. } = &entry.run
            && worker.reads_documents()
          {
            worker.queue.push_document(Event::Closed(id.get()));
          }
        }
      }
      previews_closed
    };
    if previews_closed {
      self.shared.embedder.notify(Notification::PreviewsChanged);
    }
  }

  /// Extension diagnostics overlapping `bytes`, mapped to the current text.
  /// Call with the editor locked.
  pub fn diagnostics(&self, document: &Document, bytes: Range<usize>) -> Vec<ExtensionDiagnostic> {
    self.collect(document, |range| {
      range.start <= bytes.end && range.end >= bytes.start
    })
  }

  /// Extension diagnostics covering `offset`.
  pub fn diagnostics_at(&self, document: &Document, offset: usize) -> Vec<ExtensionDiagnostic> {
    self.collect(document, |range| {
      range.start <= offset && offset <= range.end
    })
  }

  fn collect(
    &self,
    document: &Document,
    wanted: impl Fn(&Range<usize>) -> bool,
  ) -> Vec<ExtensionDiagnostic> {
    let mut state = self.shared.lock();
    let names: HashMap<String, String> = state
      .extensions
      .iter()
      .map(|entry| (entry.manifest.id.clone(), entry.manifest.name.clone()))
      .collect();
    let Some(record) = state.documents.get_mut(&document.id()) else {
      return Vec::new();
    };
    let mut found = Vec::new();
    for (id, set) in &mut record.diagnostics {
      set.catch_up(document.buffer());
      let source = names.get(id).cloned().unwrap_or_else(|| id.clone());
      found.extend(
        set
          .items
          .iter()
          .filter(|item| wanted(&item.range))
          .map(|item| ExtensionDiagnostic {
            range: item.range.clone(),
            severity: item.severity,
            message: item.message.clone(),
            code: item.code.clone(),
            source: source.clone(),
          }),
      );
    }
    found
  }

  /// Stops every extension, giving each a moment to deactivate.
  pub fn shutdown(&self) {
    let workers: Vec<Worker> = {
      let mut state = self.shared.lock();
      state.shutting_down = true;
      state
        .extensions
        .iter_mut()
        .filter_map(|entry| match std::mem::replace(&mut entry.run, Run::Idle) {
          Run::Running { worker, .. } => Some(worker),
          other => {
            entry.run = other;
            None
          }
        })
        .collect()
    };
    self.shared.builders.stop_all();
    for worker in &workers {
      worker.stop(false);
    }
    let deadline = Instant::now() + SHUTDOWN_WAIT;
    for mut worker in workers {
      let Some(thread) = worker.thread.take() else {
        continue;
      };
      while !thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
      }
      if thread.is_finished() {
        let _ = thread.join();
      } else {
        worker.stop(true);
        tracing::warn!("an extension did not stop in time");
      }
    }
  }
}

/// Subfolders of `root` that contain a manifest, sorted by name.
/// The preview id of a file open in a viewer: what the workbench fetches
/// its content and pictures by.
pub fn viewer_id(viewer: &str, path: &Path) -> String {
  format!("{viewer}/{}", path.display())
}

fn installed_dirs(root: &Path) -> Vec<PathBuf> {
  let Ok(entries) = std::fs::read_dir(root) else {
    return Vec::new();
  };
  let mut dirs: Vec<PathBuf> = entries
    .filter_map(Result::ok)
    .map(|entry| entry.path())
    .filter(|path| path.join(MANIFEST_FILE).is_file())
    .collect();
  dirs.sort();
  dirs
}
