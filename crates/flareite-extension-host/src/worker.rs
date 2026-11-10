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
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wasmtime::Store;

use crate::api::ApiContext;
use crate::bindings::Extension;
use crate::bindings::flareite::extension::downloads::DownloadedFile;
use crate::bindings::flareite::extension::processes::{FinishedProcess, ProcessLines};
use crate::bindings::flareite::extension::prompts::PromptAnswer;
use crate::bindings::flareite::extension::types::{
  Activation, CommandContext, DocumentChange, DocumentInfo, Permission as WitPermission,
  TextChange, WebviewMessage,
};
use crate::bindings::flareite::extension::views::ExpandingItem;
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

pub type Reply = Sender<Result<(), String>>;

/// What a command was run for, which the extension reads during it.
#[derive(Default)]
pub enum Payload {
  #[default]
  None,
  /// A webview's `on_message`; see `webviews::message`.
  Message(WebviewMessage),
  /// A program `spawn` started ended; see `processes::finished`.
  Finished(FinishedProcess),
  /// A program `spawn-streaming` started wrote lines; see
  /// `processes::lines`. (1.17)
  Lines(ProcessLines),
  /// A file `downloads::download` asked for was had, or couldn't be; see
  /// `downloads::downloaded`. (1.17)
  Downloaded(DownloadedFile),
  /// Watched files changed; see `workspace::changes`.
  Changes(FileChanges),
  /// Run from a view's menu or button; see `views::selection`.
  Selection(Vec<String>),
  /// A view item is being opened; see `views::expanding`.
  Expand(ExpandingItem),
  /// The user answered a prompt; see `prompts::answer`.
  Answer(PromptAnswer),
  /// The editor needs the text at an address; see `documents::requested`.
  Provide(ProvideRequest),
  /// The editor needs a file's original; see `quick-diff::requested-file`.
  Original(crate::quick_diff::OriginalRequest),
  /// Changes to apply to an original; see `quick-diff::to-apply`.
  Apply(crate::bindings::flareite::extension::quick_diff::Applied),
  /// A timer ran out; see `timers::fired`.
  Timer(u64),
  /// A layout's command, for a tab; see `layouts::filling`, `invoked`, and
  /// `message`. (1.15)
  Layout(crate::layouts::LayoutCall),
  /// A link handler's command; see `links::opening`. (1.15)
  Link(crate::links::LinkRequest),
}

/// A request for the text at an address, answered with
/// `documents::provide`.
pub struct ProvideRequest {
  pub id: u64,
  pub uri: String,
}

/// Why a worker stopped running its extension.
pub struct Failure {
  pub reason: String,
  /// The command that caused it, answered once the host has recorded the
  /// failure.
  pub reply: Option<Reply>,
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
  Command {
    command: String,
    context: CommandContext,
    /// From a view item or menu; see `command-argument`.
    argument: Option<String>,
    payload: Payload,
    reply: Reply,
  },
}

impl Event {
  /// A command nobody waits for, run for `payload`. Its error, if any, is
  /// logged.
  pub fn background(command: String, argument: Option<String>, payload: Payload) -> Self {
    let (reply, _) = std::sync::mpsc::channel();
    Self::Command {
      command,
      context: CommandContext {
        document: None,
        selections: Vec::new(),
      },
      argument,
      payload,
      reply,
    }
  }
}

impl Event {
  fn document(&self) -> Option<u64> {
    match self {
      Self::Opened(info) => Some(info.id),
      Self::Changed { document, .. } | Self::Closed(document) => Some(*document),
      Self::Command { .. } => None,
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

  /// Queues a command nobody waits for: a spawned program's end, or file
  /// changes. These are few (programs and watches are limited), so they
  /// are queued even when the queue is full, as dropping them would lose
  /// the outcome. Changes for a watch whose command is still waiting are
  /// merged into it, as are a streaming program's lines (which are
  /// limited, see [`crate::processes::merge_lines`]).
  pub fn push_background(&self, event: Event) {
    let mut state = self.lock();
    if state.stopped {
      return;
    }
    if let Event::Command {
      payload: Payload::Changes(changes),
      ..
    } = &event
    {
      let waiting = state.events.iter_mut().find_map(|pending| match pending {
        Event::Command {
          payload: Payload::Changes(pending),
          ..
        } if pending.watch == changes.watch => Some(pending),
        _ => None,
      });
      if let Some(pending) = waiting {
        merge_changes(pending, changes);
        return;
      }
    }
    // Lines for a program whose output command is still waiting join it.
    let event = match event {
      Event::Command {
        payload: Payload::Lines(lines),
        command,
        context,
        argument,
        reply,
      } => {
        let waiting = state.events.iter_mut().find_map(|pending| match pending {
          Event::Command {
            payload: Payload::Lines(pending),
            ..
          } if pending.process == lines.process => Some(pending),
          _ => None,
        });
        if let Some(pending) = waiting {
          crate::processes::merge_lines(pending, lines);
          return;
        }
        Event::Command {
          payload: Payload::Lines(lines),
          command,
          context,
          argument,
          reply,
        }
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

  /// Stops the queue; waiting commands fail with `reason`.
  pub fn stop(&self, reason: &str) {
    let mut state = self.lock();
    state.stopped = true;
    for event in state.events.drain(..) {
      if let Event::Command { reply, .. } = event {
        let _ = reply.send(Err(reason.to_owned()));
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

/// The permission as the extension sees it in `activation.granted`; `None`
/// for permissions that concern the editor, not the extension's code, and
/// those added after API 1.0 (which the `permission` type can't name).
fn wit_permission(permission: Permission) -> Option<WitPermission> {
  match permission {
    Permission::DocumentsRead => Some(WitPermission::DocumentsRead),
    Permission::DocumentsWrite => Some(WitPermission::DocumentsWrite),
    Permission::FilesRead
    | Permission::FilesWrite
    | Permission::LanguageServers
    | Permission::Network
    | Permission::Processes
    | Permission::WebPages => None,
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
    granted: setup
      .granted
      .iter()
      .copied()
      .filter_map(wit_permission)
      .collect(),
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
      Event::Opened(info) => guarded(
        &mut store,
        budgets.event,
        "Handling an opened document",
        |store| extension.call_document_opened(&mut *store, &info),
      )?,
      Event::Changed {
        document,
        version,
        changes,
      } => {
        let change = DocumentChange {
          document,
          version,
          changes,
        };
        guarded(
          &mut store,
          budgets.event,
          "Handling a document change",
          |store| extension.call_document_changed(&mut *store, &change),
        )?;
      }
      Event::Closed(document) => guarded(
        &mut store,
        budgets.event,
        "Handling a closed document",
        |store| extension.call_document_closed(&mut *store, document),
      )?,
      Event::Command {
        command,
        context,
        argument,
        payload,
        reply,
      } => {
        store.data_mut().api.argument = argument;
        store.data_mut().api.payload = payload;
        let result = guarded(&mut store, budgets.command, "The command", |store| {
          extension.call_run_command(&mut *store, &command, &context)
        });
        store.data_mut().api.argument = None;
        store.data_mut().api.payload = Payload::None;
        match result {
          Ok(outcome) => {
            // Nobody waits for a command run in the background, such as
            // for a finished program: its error goes to the log.
            if let Err(mpsc::SendError(Err(message))) =
              reply.send(outcome.map_err(|message| truncate(&message)))
            {
              tracing::warn!(extension = %setup.manifest.id, %command, "{message}");
            }
          }
          Err(reason) => {
            return Err(Failure {
              reason,
              reply: Some(reply),
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
      context: CommandContext {
        document: None,
        selections: Vec::new(),
      },
      argument: None,
      payload: Payload::None,
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
        context: CommandContext {
          document: None,
          selections: Vec::new(),
        },
        argument: None,
        payload: Payload::None,
        reply,
      })
      .expect("queued");
    queue.stop("stopping");
    assert_eq!(receiver.recv().expect("reply"), Err("stopping".into()));
    assert!(queue.pop().is_none());
  }
}
