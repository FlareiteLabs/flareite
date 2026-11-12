//! Each running extension has a worker thread that owns its WebAssembly
//! instance and processes its event queue one event at a time.
//!
//! The editor never calls into an extension directly: it pushes events
//! (document changes, commands) onto the extension's queue and moves on.
//! The queue is bounded, and consecutive changes to a document are merged
//! while they wait, so a slow extension sees fewer, larger changes rather
//! than an ever-growing backlog. Every call runs under a deadline; a trap,
//! an exceeded limit, or a missed deadline ends the instance, and the host
//! marks the extension failed.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wasmtime::Store;

use crate::api::ApiContext;
use crate::bindings::Extension;
use crate::bindings::flareite::extension::events::Event as WitEvent;
use crate::bindings::flareite::extension::language_features::{
  AutoInsertRequest, CodeAction, CodeActionRequest, CompletionItem, CompletionRequest,
  FormattingRequest, HoverRequest, Insertion,
};
use crate::bindings::flareite::extension::links::LinkRequest;
use crate::bindings::flareite::extension::quick_diff::Applied;
use crate::bindings::flareite::extension::types::{
  Activation, CommandContext, DocumentChange, DocumentInfo, Permission as WitPermission,
  TextChange, TextEdit,
};
use crate::bindings::flareite::extension::workspace::FileChanges;
use crate::host::Shared;
use crate::manifest::Manifest;
use crate::permissions::Permission;
use crate::processes::ProcessTable;
use crate::runtime::{Call, Interrupt, Limiter, MemoryExceeded, Runtime, StoreState};
use crate::usage::Usage;

/// Changed text kept in one pending notification; beyond this the extension
/// is told to re-read the document instead.
pub const MAX_PENDING_CHANGE_BYTES: usize = 1024 * 1024;
/// Changed paths kept in one pending watch notification; beyond this the
/// extension is told to look at everything again.
pub const MAX_PENDING_PATHS: usize = 10_000;
/// Error messages from extensions are cut to this length.
const MAX_ERROR_CHARS: usize = 1_000;

/// Where the outcome of a command (or an awaited event) goes.
pub type Reply = Sender<Result<(), String>>;
/// Where the answer to a request goes.
pub type AnswerReply = Sender<Result<Answer, String>>;

/// A call of one of the world's exports whose answer the editor waits for.
pub enum Request {
  Completions(CompletionRequest),
  Hover(HoverRequest),
  AutoInsert(AutoInsertRequest),
  Formatting(FormattingRequest),
  CodeActions(CodeActionRequest),
  /// The text at a provided address.
  Text(String),
  /// A file's original.
  Original(String),
  ChangeOriginal(Applied),
  OpenLink(LinkRequest),
}

impl Request {
  /// What it is, for messages about it.
  fn what(&self) -> &'static str {
    match self {
      Self::Completions(_) => "Completion",
      Self::Hover(_) => "Hover",
      Self::AutoInsert(_) => "Inserting text as you type",
      Self::Formatting(_) => "Formatting",
      Self::CodeActions(_) => "Finding code actions",
      Self::Text(_) => "Providing a document",
      Self::Original(_) => "Finding an original",
      Self::ChangeOriginal(_) => "Changing an original",
      Self::OpenLink(_) => "Opening a link",
    }
  }
}

/// The answer to a [`Request`] of the same kind.
#[derive(Debug, PartialEq)]
pub enum Answer {
  Completions(Vec<CompletionItem>),
  Hover(Option<String>),
  AutoInsert(Option<Insertion>),
  Formatting(Vec<TextEdit>),
  CodeActions(Vec<CodeAction>),
  Text(Option<String>),
  Original(Option<String>),
  OriginalChanged,
  /// Whether it opened the link.
  OpenedLink(bool),
}

/// Why a worker stopped running its extension.
pub struct Failure {
  pub reason: String,
  /// The call that caused it, answered once the host has recorded the
  /// failure.
  pub reply: Option<Waiting>,
}

/// Someone waiting for a call's outcome.
pub enum Waiting {
  Done(Reply),
  Answer(AnswerReply),
}

impl Waiting {
  pub fn fail(self, reason: &str) {
    match self {
      Self::Done(reply) => {
        let _ = reply.send(Err(reason.to_owned()));
      }
      Self::Answer(reply) => {
        let _ = reply.send(Err(reason.to_owned()));
      }
    }
  }
}

impl From<String> for Failure {
  fn from(reason: String) -> Self {
    Self {
      reason,
      reply: None,
    }
  }
}

impl From<&str> for Failure {
  fn from(reason: &str) -> Self {
    reason.to_owned().into()
  }
}

pub enum Event {
  Opened(DocumentInfo),
  Changed {
    document: u64,
    version: u64,
    changes: Option<Vec<TextChange>>,
  },
  Closed(u64),
  /// One of the extension's commands, which the user ran.
  Command {
    command: String,
    context: CommandContext,
    reply: Reply,
  },
  /// Something for `handle-event`; `reply` when someone waits for it to be
  /// handled.
  Notice {
    event: WitEvent,
    reply: Option<Reply>,
  },
  /// A request someone waits for the answer to.
  Call {
    request: Request,
    reply: AnswerReply,
  },
}

/// The context of a command run with nothing in particular: from the
/// palette, a keybinding, or the editor's menu.
#[cfg(test)]
pub fn context(
  document: Option<u64>,
  selections: Vec<crate::bindings::flareite::extension::types::Range>,
) -> CommandContext {
  CommandContext {
    document,
    selections,
    argument: None,
    selection: Vec::new(),
    invoked_from: None,
  }
}

impl Event {
  /// An event nobody waits for. Its error, if any, is logged.
  pub fn notice(event: WitEvent) -> Self {
    Self::Notice { event, reply: None }
  }

  fn document(&self) -> Option<u64> {
    match self {
      Self::Opened(info) => Some(info.id),
      Self::Changed { document, .. } | Self::Closed(document) => Some(*document),
      _ => None,
    }
  }

  /// Who waits for it, if anyone.
  fn waiting(self) -> Option<Waiting> {
    match self {
      Self::Command { reply, .. }
      | Self::Notice {
        reply: Some(reply), ..
      } => Some(Waiting::Done(reply)),
      Self::Call { reply, .. } => Some(Waiting::Answer(reply)),
      _ => None,
    }
  }
}

fn change_bytes(changes: &[TextChange]) -> usize {
  changes.iter().map(|change| change.text.len()).sum()
}

/// Adds `later` changed paths to `pending`, sorted and without repeats, up
/// to [`MAX_PENDING_PATHS`].
pub fn merge_changes(pending: &mut FileChanges, later: &FileChanges) {
  pending.overflow |= later.overflow;
  if !pending.overflow {
    pending.paths.extend(later.paths.iter().cloned());
    pending.paths.sort_unstable();
    pending.paths.dedup();
  }
  if pending.overflow || pending.paths.len() > MAX_PENDING_PATHS {
    pending.overflow = true;
    pending.paths.clear();
  }
}

struct QueueState {
  events: VecDeque<Event>,
  /// Documents whose events were dropped because the queue was full: their
  /// next change asks the extension to re-read them.
  dropped: HashSet<u64>,
  stopped: bool,
}

/// A bounded queue of events for one extension.
pub struct Queue {
  state: Mutex<QueueState>,
  ready: Condvar,
  capacity: usize,
  usage: Arc<Usage>,
}

impl Queue {
  pub fn new(capacity: usize, usage: Arc<Usage>) -> Self {
    Self {
      state: Mutex::new(QueueState {
        events: VecDeque::new(),
        dropped: HashSet::new(),
        stopped: false,
      }),
      ready: Condvar::new(),
      capacity: capacity.max(1),
      usage,
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, QueueState> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Queues a document event, merging a change into a change of the same
  /// document that is still waiting.
  pub fn push_document(&self, event: Event) {
    let mut state = self.lock();
    if state.stopped {
      return;
    }
    let Some(document) = event.document() else {
      return;
    };
    if let Event::Changed {
      version, changes, ..
    } = event
    {
      let resync = state.dropped.remove(&document);
      let waiting = state
        .events
        .iter_mut()
        .rev()
        .find(|pending| pending.document() == Some(document));
      if let Some(Event::Changed {
        version: pending_version,
        changes: pending_changes,
        ..
      }) = waiting
      {
        *pending_version = version;
        *pending_changes = match (pending_changes.take(), changes) {
          (Some(mut earlier), Some(later))
            if !resync
              && change_bytes(&earlier) + change_bytes(&later) <= MAX_PENDING_CHANGE_BYTES =>
          {
            earlier.extend(later);
            Some(earlier)
          }
          _ => None,
        };
        return;
      }
      let event = Event::Changed {
        document,
        version,
        changes: if resync { None } else { changes },
      };
      self.push_locked(state, event, document);
      return;
    }
    self.push_locked(state, event, document);
  }

  fn push_locked(
    &self,
    mut state: std::sync::MutexGuard<'_, QueueState>,
    event: Event,
    document: u64,
  ) {
    if state.events.len() >= self.capacity {
      tracing::warn!(document, "extension event queue is full; dropping an event");
      self.usage.record_dropped_event();
      state.dropped.insert(document);
      return;
    }
    state.events.push_back(event);
    self.ready.notify_one();
  }

  /// Queues a command. Fails if the queue is full or stopped.
  pub fn push_command(&self, event: Event) -> Result<(), String> {
    let mut state = self.lock();
    if state.stopped {
      return Err("The extension is stopping.".into());
    }
    if state.events.len() >= self.capacity {
      return Err("The extension is busy; try again.".into());
    }
    state.events.push_back(event);
    self.ready.notify_one();
    Ok(())
  }

  /// Queues an event nobody waits for: a spawned program's end, or file
  /// changes. These are few (programs and watches are limited), so they
  /// are queued even when the queue is full, as dropping them would lose
  /// the outcome. Changes for a watch whose event is still waiting are
  /// merged into it, as are a streaming program's lines (which are
  /// limited, see [`crate::processes::merge_lines`]).
  pub fn push_background(&self, event: Event) {
    let mut state = self.lock();
    if state.stopped {
      return;
    }
    let event = match event {
      Event::Notice {
        event: WitEvent::FilesChanged(changes),
        reply: None,
      } => {
        let waiting = state.events.iter_mut().find_map(|pending| match pending {
          Event::Notice {
            event: WitEvent::FilesChanged(pending),
            ..
          } if pending.watch == changes.watch => Some(pending),
          _ => None,
        });
        if let Some(pending) = waiting {
          merge_changes(pending, &changes);
          return;
        }
        Event::notice(WitEvent::FilesChanged(changes))
      }
      // Lines for a program whose output is still waiting join it.
      Event::Notice {
        event: WitEvent::ProcessOutput(lines),
        reply: None,
      } => {
        let waiting = state.events.iter_mut().find_map(|pending| match pending {
          Event::Notice {
            event: WitEvent::ProcessOutput(pending),
            ..
          } if pending.process == lines.process => Some(pending),
          _ => None,
        });
        if let Some(pending) = waiting {
          crate::processes::merge_lines(pending, lines);
          return;
        }
        Event::notice(WitEvent::ProcessOutput(lines))
      }
      event => event,
    };
    state.events.push_back(event);
    self.ready.notify_one();
  }

  /// Waits for the next event. `None` once the queue is stopped.
  fn pop(&self) -> Option<Event> {
    let mut state = self.lock();
    loop {
      if state.stopped {
        return None;
      }
      if let Some(event) = state.events.pop_front() {
        return Some(event);
      }
      state = self
        .ready
        .wait(state)
        .unwrap_or_else(PoisonError::into_inner);
    }
  }

  /// Stops the queue; waiting calls fail with `reason`.
  pub fn stop(&self, reason: &str) {
    let mut state = self.lock();
    state.stopped = true;
    for event in state.events.drain(..) {
      if let Some(waiting) = event.waiting() {
        waiting.fail(reason);
      }
    }
    self.ready.notify_all();
  }

  #[cfg(test)]
  fn len(&self) -> usize {
    self.lock().events.len()
  }
}

/// What a worker needs to start.
pub struct Setup {
  pub manifest: Arc<Manifest>,
  pub generation: u64,
  pub granted: BTreeSet<Permission>,
  pub config:
    Arc<std::sync::RwLock<std::collections::BTreeMap<String, crate::config::ConfigValue>>>,
  pub queue: Arc<Queue>,
  pub cancel: Arc<AtomicBool>,
  pub usage: Arc<Usage>,
  /// Programs it started with `spawn`.
  pub processes: Arc<ProcessTable>,
}

/// The host's handle on a running worker.
pub struct Worker {
  pub generation: u64,
  pub granted: BTreeSet<Permission>,
  pub queue: Arc<Queue>,
  pub cancel: Arc<AtomicBool>,
  pub processes: Arc<ProcessTable>,
  pub thread: Option<JoinHandle<()>>,
}

impl Worker {
  pub fn reads_documents(&self) -> bool {
    self.granted.contains(&Permission::DocumentsRead)
  }

  /// Asks the worker to finish: it deactivates the extension after the
  /// event it is processing, which is cancelled if `cancel` is set. The
  /// programs it started are stopped.
  pub fn stop(&self, cancel: bool) {
    self.queue.stop("The extension was stopped.");
    self.processes.kill_all();
    if cancel {
      self.cancel.store(true, Ordering::Relaxed);
    }
  }
}

pub fn spawn(shared: Weak<Shared>, setup: Setup) -> Worker {
  let worker = Worker {
    generation: setup.generation,
    granted: setup.granted.clone(),
    queue: setup.queue.clone(),
    cancel: setup.cancel.clone(),
    processes: setup.processes.clone(),
    thread: None,
  };
  let id = setup.manifest.id.clone();
  let generation = setup.generation;
  let thread = std::thread::Builder::new()
    .name(format!("extension-{id}"))
    .spawn(move || {
      let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| run(&shared, &setup)))
        .unwrap_or_else(|_| Err("stopped because of an internal error".into()));
      // However it ended, nothing is left to hear from its programs.
      setup.processes.kill_all();
      if let Some(shared) = shared.upgrade() {
        shared.worker_finished(&setup.manifest.id, setup.generation, outcome);
      }
    });
  match thread {
    Ok(thread) => Worker {
      thread: Some(thread),
      ..worker
    },
    Err(error) => {
      tracing::error!(%error, extension = %id, generation, "could not start an extension thread");
      worker.queue.stop("The extension could not be started.");
      worker
    }
  }
}

/// Deadlines for a worker's calls.
#[derive(Debug, Clone, Copy)]
pub struct Budgets {
  pub activation: Duration,
  pub command: Duration,
  pub event: Duration,
  pub memory: usize,
}

/// The permission as the extension sees it, in `activation.granted` and
/// `permission-denied`.
pub fn wit_permission(permission: Permission) -> WitPermission {
  match permission {
    Permission::DocumentsRead => WitPermission::DocumentsRead,
    Permission::DocumentsWrite => WitPermission::DocumentsWrite,
    Permission::FilesRead => WitPermission::FilesRead,
    Permission::FilesWrite => WitPermission::FilesWrite,
    Permission::LanguageServers => WitPermission::LanguageServers,
    Permission::Network => WitPermission::Network,
    Permission::Processes => WitPermission::Processes,
    Permission::WebPages => WitPermission::WebPages,
  }
}

/// Runs an extension until its queue is stopped (`Ok`) or it fails.
fn run(shared: &Weak<Shared>, setup: &Setup) -> Result<(), Failure> {
  // Holding the runtime keeps the deadline ticker alive while this runs.
  let (runtime, budgets, pool) = {
    let shared = shared.upgrade().ok_or("the editor is closing")?;
    (shared.runtime()?, shared.budgets(), shared.memory_pool())
  };
  let entrypoint = setup
    .manifest
    .entrypoint
    .as_ref()
    .ok_or("has no code to run")?;
  let starting = Instant::now();
  let compiling = Instant::now();
  let component = runtime.compile(entrypoint)?;
  setup.usage.record_compile(compiling.elapsed());
  // Stopped while compiling (say, reloaded): another start may be running.
  if setup.cancel.load(Ordering::Relaxed) {
    return Ok(());
  }
  let mut store = Store::new(
    runtime.engine(),
    StoreState {
      api: ApiContext::new(shared.clone(), setup),
      limiter: Limiter::new(budgets.memory, setup.usage.clone(), pool),
      call: None,
      cancel: setup.cancel.clone(),
      interrupt: None,
    },
  );
  store.limiter(|state| &mut state.limiter);
  Runtime::watch(&mut store);

  let extension = guarded(&mut store, budgets.activation, "Starting", |store| {
    Extension::instantiate(&mut *store, &component, runtime.linker())
  })
  .map_err(|error| {
    if error.contains("import") || error.contains("export") {
      format!(
        "does not implement the Flareite extension API {}: {error}",
        setup.manifest.api_version
      )
    } else {
      error
    }
  })?;

  let activation = Activation {
    granted: setup.granted.iter().copied().map(wit_permission).collect(),
  };
  guarded(&mut store, budgets.activation, "Activation", |store| {
    extension.call_activate(&mut *store, &activation)
  })?
  .map_err(|message| format!("could not start: {}", truncate(&message)))?;
  setup.usage.record_activation(starting.elapsed());
  if let Some(shared) = shared.upgrade() {
    shared.worker_active(&setup.manifest.id, setup.generation);
  }

  while let Some(event) = setup.queue.pop() {
    match event {
      Event::Opened(info) => handle(
        &mut store,
        &extension,
        budgets.event,
        setup,
        WitEvent::DocumentOpened(info),
        None,
      )?,
      Event::Changed {
        document,
        version,
        changes,
      } => handle(
        &mut store,
        &extension,
        budgets.event,
        setup,
        WitEvent::DocumentChanged(DocumentChange {
          document,
          version,
          changes,
        }),
        None,
      )?,
      Event::Closed(document) => handle(
        &mut store,
        &extension,
        budgets.event,
        setup,
        WitEvent::DocumentClosed(document),
        None,
      )?,
      Event::Notice { event, reply } => {
        handle(&mut store, &extension, budgets.command, setup, event, reply)?;
      }
      Event::Command {
        command,
        context,
        reply,
      } => {
        let result = guarded(&mut store, budgets.command, "The command", |store| {
          extension.call_run_command(&mut *store, &command, &context)
        });
        match result {
          Ok(outcome) => {
            let _ = reply.send(outcome.map_err(|message| truncate(&message)));
          }
          Err(reason) => {
            return Err(Failure {
              reason,
              reply: Some(Waiting::Done(reply)),
            });
          }
        }
      }
      Event::Call { request, reply } => {
        match answer(&mut store, &extension, budgets.command, request) {
          Ok(outcome) => {
            let _ = reply.send(outcome.map_err(|message| truncate(&message)));
          }
          Err(reason) => {
            return Err(Failure {
              reason,
              reply: Some(Waiting::Answer(reply)),
            });
          }
        }
      }
    }
  }

  // Stopped: let the extension clean up, briefly, unless it was cancelled.
  if !setup.cancel.load(Ordering::Relaxed) {
    let _ = guarded(&mut store, budgets.event, "Deactivation", |store| {
      extension.call_deactivate(&mut *store)
    });
  }
  Ok(())
}

/// Hands `event` to the extension's `handle-event`. Its error goes to
/// `reply`, or to the log when nobody waits; a trap or a missed deadline
/// fails the extension.
fn handle(
  store: &mut Store<StoreState>,
  extension: &Extension,
  budget: Duration,
  setup: &Setup,
  event: WitEvent,
  reply: Option<Reply>,
) -> Result<(), Failure> {
  let what = event_name(&event);
  match guarded(store, budget, what, |store| {
    extension.call_handle_event(&mut *store, &event)
  }) {
    Ok(outcome) => {
      let outcome = outcome.map_err(|message| truncate(&message));
      match reply {
        Some(reply) => {
          let _ = reply.send(outcome);
        }
        None => {
          if let Err(message) = outcome {
            tracing::warn!(extension = %setup.manifest.id, event = what, "{message}");
          }
        }
      }
      Ok(())
    }
    Err(reason) => Err(Failure {
      reason,
      reply: reply.map(Waiting::Done),
    }),
  }
}

/// What an event is, for messages about handling it.
fn event_name(event: &WitEvent) -> &'static str {
  match event {
    WitEvent::DocumentOpened(_) => "Handling an opened document",
    WitEvent::DocumentChanged(_) => "Handling a document change",
    WitEvent::DocumentClosed(_) => "Handling a closed document",
    WitEvent::FolderChanged => "Handling the folder's change",
    WitEvent::ActiveFileChanged(_) => "Handling the active file's change",
    WitEvent::SelectionChanged(_) => "Handling a cursor move",
    WitEvent::ConfigurationChanged => "Handling a settings change",
    WitEvent::ProcessFinished(_) => "Handling a program's end",
    WitEvent::ProcessOutput(_) => "Handling a program's output",
    WitEvent::DownloadFinished(_) => "Handling a download",
    WitEvent::FilesChanged(_) => "Handling changed files",
    WitEvent::TimerFired(_) => "Handling a timer",
    WitEvent::PromptAnswered(_) => "Handling an answer",
    WitEvent::ViewerOpened(_) => "Showing a file",
    WitEvent::WebviewMessage(_) | WitEvent::PaneMessage(_) => "Handling a page's message",
    WitEvent::PaneNavigated(_) => "Handling a page's navigation",
    WitEvent::LayoutFill(_) => "Filling in a tab",
    WitEvent::ViewItemExpanding(_) => "Listing an item's children",
  }
}

/// Calls the export `request` is for: its answer, or the extension's error
/// (inside). The outer error fails the extension.
fn answer(
  store: &mut Store<StoreState>,
  extension: &Extension,
  budget: Duration,
  request: Request,
) -> Result<Result<Answer, String>, String> {
  let what = request.what();
  guarded(store, budget, what, |store| {
    Ok(match &request {
      Request::Completions(request) => extension
        .call_provide_completions(&mut *store, request)?
        .map(Answer::Completions),
      Request::Hover(request) => extension
        .call_provide_hover(&mut *store, *request)?
        .map(Answer::Hover),
      Request::AutoInsert(request) => extension
        .call_provide_auto_insert(&mut *store, request)?
        .map(Answer::AutoInsert),
      Request::Formatting(request) => extension
        .call_provide_formatting(&mut *store, *request)?
        .map(Answer::Formatting),
      Request::CodeActions(request) => extension
        .call_provide_code_actions(&mut *store, request)?
        .map(Answer::CodeActions),
      Request::Text(uri) => extension
        .call_provide_text(&mut *store, uri)?
        .map(Answer::Text),
      Request::Original(path) => extension
        .call_provide_original(&mut *store, path)?
        .map(Answer::Original),
      Request::ChangeOriginal(applied) => extension
        .call_change_original(&mut *store, applied)?
        .map(|()| Answer::OriginalChanged),
      Request::OpenLink(link) => extension
        .call_open_link(&mut *store, link)?
        .map(Answer::OpenedLink),
    })
  })
}

/// Runs `f` with a deadline of `budget`, and accounts for its time. Calls
/// that take a large part of their budget are logged. Failures are
/// described for the user, starting with `what`.
fn guarded<R>(
  store: &mut Store<StoreState>,
  budget: Duration,
  what: &str,
  f: impl FnOnce(&mut Store<StoreState>) -> wasmtime::Result<R>,
) -> Result<R, String> {
  let started = Instant::now();
  store.data_mut().call = Some(Call {
    deadline: started + budget,
  });
  store.data_mut().interrupt = None;
  store.set_epoch_deadline(1);
  let result = f(store);
  store.data_mut().call = None;
  let elapsed = started.elapsed();
  let data = store.data();
  data.limiter.usage.record_call(elapsed);
  if elapsed > budget / 4 && result.is_ok() {
    tracing::warn!(
      extension = %data.api.extension_id(),
      call = what,
      elapsed_ms = elapsed.as_secs_f64() * 1000.0,
      budget_ms = budget.as_secs_f64() * 1000.0,
      "slow extension call"
    );
  }
  result.map_err(|error| describe_failure(store.data(), &error, budget, what))
}

fn describe_failure(
  state: &StoreState,
  error: &wasmtime::Error,
  budget: Duration,
  what: &str,
) -> String {
  match state.interrupt {
    Some(Interrupt::Deadline) => {
      return format!(
        "{what} took longer than {} and was stopped",
        seconds(budget)
      );
    }
    Some(Interrupt::Cancelled) => return format!("{what} was cancelled"),
    None => {}
  }
  match state.limiter.memory_exceeded {
    Some(MemoryExceeded::Instance) => {
      return format!(
        "exceeded its memory limit of {} MiB",
        state.limiter.max_memory / 1024 / 1024
      );
    }
    Some(MemoryExceeded::Total) => {
      return format!(
        "needed more memory than is left for extensions (all extensions together may use {} MiB)",
        state.limiter.pool.limit() / 1024 / 1024
      );
    }
    None => {}
  }
  match error.downcast_ref::<wasmtime::Trap>() {
    Some(wasmtime::Trap::StackOverflow) => "ran out of stack space (too deep recursion)".into(),
    Some(wasmtime::Trap::UnreachableCodeReached) => "crashed (it panicked or aborted)".into(),
    Some(trap) => format!("crashed: {trap}"),
    None => truncate(&format!("failed: {error:#}")),
  }
}

fn seconds(duration: Duration) -> String {
  let secs = duration.as_secs_f64();
  if secs >= 1.0 && duration.subsec_millis() == 0 {
    format!("{secs:.0} s")
  } else {
    format!("{} ms", duration.as_millis())
  }
}

pub fn truncate(message: &str) -> String {
  if message.chars().count() <= MAX_ERROR_CHARS {
    message.to_owned()
  } else {
    let mut cut: String = message.chars().take(MAX_ERROR_CHARS).collect();
    cut.push('…');
    cut
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::bindings::flareite::extension::types::{Position, Range};

  fn change(text: &str) -> TextChange {
    let at = Position { line: 0, column: 0 };
    TextChange {
      range: Range { start: at, end: at },
      text: text.into(),
    }
  }

  fn changed(document: u64, version: u64, text: &str) -> Event {
    Event::Changed {
      document,
      version,
      changes: Some(vec![change(text)]),
    }
  }

  fn pending_changes(queue: &Queue) -> Vec<(u64, u64, Option<usize>)> {
    queue
      .lock()
      .events
      .iter()
      .filter_map(|event| match event {
        Event::Changed {
          document,
          version,
          changes,
        } => Some((*document, *version, changes.as_ref().map(Vec::len))),
        _ => None,
      })
      .collect()
  }

  #[test]
  fn merges_waiting_changes_to_the_same_document() {
    let queue = Queue::new(10, Arc::default());
    queue.push_document(changed(1, 2, "a"));
    queue.push_document(changed(2, 5, "x"));
    queue.push_document(changed(1, 3, "b"));
    assert_eq!(
      pending_changes(&queue),
      vec![(1, 3, Some(2)), (2, 5, Some(1))]
    );
  }

  #[test]
  fn large_merged_changes_become_a_reread() {
    let queue = Queue::new(10, Arc::default());
    let big = "x".repeat(MAX_PENDING_CHANGE_BYTES);
    queue.push_document(changed(1, 2, &big));
    queue.push_document(changed(1, 3, "more"));
    assert_eq!(pending_changes(&queue), vec![(1, 3, None)]);
  }

  #[test]
  fn a_full_queue_drops_events_and_asks_for_a_reread_later() {
    let usage = Arc::new(Usage::default());
    let queue = Queue::new(1, usage.clone());
    queue.push_document(Event::Closed(9));
    queue.push_document(changed(1, 2, "a"));
    assert_eq!(queue.len(), 1);
    assert_eq!(usage.snapshot().dropped_events, 1);
    let (reply, _receiver) = std::sync::mpsc::channel();
    let command = Event::Command {
      command: "x".into(),
      context: context(None, Vec::new()),
      reply,
    };
    assert!(
      queue.push_command(command).is_err(),
      "commands are refused when full"
    );
    assert!(matches!(queue.pop(), Some(Event::Closed(9))));
    queue.push_document(changed(1, 3, "b"));
    assert_eq!(pending_changes(&queue), vec![(1, 3, None)]);
  }

  #[test]
  fn stopping_fails_waiting_commands() {
    let queue = Queue::new(4, Arc::default());
    let (reply, receiver) = std::sync::mpsc::channel();
    queue
      .push_command(Event::Command {
        command: "x".into(),
        context: context(None, Vec::new()),
        reply,
      })
      .expect("queued");
    queue.stop("stopping");
    assert_eq!(receiver.recv().expect("reply"), Err("stopping".into()));
    assert!(queue.pop().is_none());
  }
}
