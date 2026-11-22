//! The host functions of the `processes` interface: programs the
//! manifest declares in `[[programs]]`, run on the user's computer.
//!
//! An extension names a declared program, never a path, and the user
//! granted `processes` seeing the list. Programs run only while the open
//! folder is trusted (or none is open), since programs such as Git and
//! build tools run code from the folder they work in. Each program runs in
//! a process group ([`ProcessGroup`]), so stopping it stops what it
//! started too. Output is collected up to a limit; `run` waits within the
//! running call's deadline, and `spawn` reports the outcome as a
//! `process-finished` event. `spawn-streaming` also hands the output over
//! line by line as it's read, as `process-output` events; lines waiting for
//! the extension are merged into one event, and dropped past a limit, so a
//! program that
//! writes faster than the extension reads can't flood its queue.
//! `spawn-interactive` (2.1) keeps the program's input open for
//! `write-input`, which a thread writes ([`Input`]) so the extension never
//! waits on the program, and hands its output over without dropping any:
//! its reader waits instead while the extension is behind, so the program
//! waits to write more. Programs still running when the extension stops are
//! stopped.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use flareite_process::{ProcessGroup, find_on_path};

use crate::api::ApiContext;
use crate::bindings::flareite::extension::events::Event as WitEvent;
use crate::bindings::flareite::extension::processes::{
  FinishedProcess, Host, OutputLine, ProcessError, ProcessLines, ProcessOptions, ProcessOutput,
};
use crate::permissions::Permission;
use crate::runtime::StoreState;
use crate::worker::Event;

/// Output kept from one program, standard output and error together.
pub const MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
/// Programs started with `spawn` running at once, per extension.
pub const MAX_RUNNING: usize = 16;
const MAX_ARGS: usize = 1_000;
const MAX_ARG_BYTES: usize = 1024 * 1024;
const MAX_ENV: usize = 100;
const MAX_STDIN_BYTES: usize = 16 * 1024 * 1024;
/// `run` returns this long before the running call's deadline, so the
/// extension can handle the timeout rather than be stopped.
const DEADLINE_MARGIN: Duration = Duration::from_millis(250);
/// How often a waiting program is checked on.
const POLL: Duration = Duration::from_millis(5);
/// How long output is read after the program ends, for programs it started
/// that hold on to its output.
const DRAIN_WAIT: Duration = Duration::from_secs(2);
/// Longest line `spawn-streaming` hands over; longer ones are cut.
pub const MAX_LINE_BYTES: usize = 16 * 1024;
/// Most text of lines waiting for an extension's output command; older
/// lines are dropped past it.
pub const MAX_PENDING_LINE_BYTES: usize = 1024 * 1024;
/// Longest line `spawn-interactive` hands over (2.1); longer ones are cut.
pub const MAX_INTERACTIVE_LINE_BYTES: usize = 32 * 1024 * 1024;
/// Most input waiting to be written to an interactive program (2.1).
const MAX_WAITING_INPUT_BYTES: usize = 16 * 1024 * 1024;

/// Receives the lines a streaming program writes, as they're read.
type Sink = Arc<dyn Fn(Vec<OutputLine>) + Send + Sync>;

type Group = Arc<Mutex<ProcessGroup>>;

fn lock(group: &Group) -> MutexGuard<'_, ProcessGroup> {
  group.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a program started with `spawn` hands over its output.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
  /// Collected, and given when it ends (`spawn`).
  Collect,
  /// Line by line, dropping the oldest when the extension is behind
  /// (`spawn-streaming`).
  Stream,
  /// Line by line, dropping none, with its input open
  /// (`spawn-interactive`, 2.1).
  Interactive,
}

/// The open standard input of a program `spawn-interactive` started. What
/// the extension writes goes to a thread that writes it, so that neither
/// waits for the other: the extension may be writing while the program,
/// waiting for its output to be read, isn't reading.
struct Input {
  /// `None` once closed: the thread then ends, closing the pipe.
  sender: Mutex<Option<Sender<Vec<u8>>>>,
  /// Bytes sent and not yet written.
  waiting: Arc<AtomicUsize>,
}

impl Input {
  fn start(mut pipe: ChildStdin, first: Option<Vec<u8>>) -> Arc<Self> {
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    let waiting = Arc::new(AtomicUsize::new(0));
    let written = waiting.clone();
    spawn_thread("extension-process-input", move || {
      for chunk in receiver {
        let failed = pipe.write_all(&chunk).and_then(|()| pipe.flush()).is_err();
        written.fetch_sub(chunk.len(), Ordering::Relaxed);
        // The program ended or closed its input: later writes fail, as the
        // receiver is gone.
        if failed {
          break;
        }
      }
    });
    let input = Arc::new(Self {
      sender: Mutex::new(Some(sender)),
      waiting,
    });
    if let Some(first) = first.filter(|first| !first.is_empty()) {
      // A new pipe takes it; it's within the input limit, checked before.
      let _ = input.write(first);
    }
    input
  }

  fn write(&self, data: Vec<u8>) -> Result<(), ProcessError> {
    let sender = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(sender) = sender.as_ref() else {
      return Err(ProcessError::InvalidArgument(
        "the program's input was closed".into(),
      ));
    };
    let length = data.len();
    if self.waiting.load(Ordering::Relaxed) + length > MAX_WAITING_INPUT_BYTES {
      return Err(ProcessError::TooLarge(format!(
        "at most {} MiB of input can wait to be written",
        MAX_WAITING_INPUT_BYTES / 1024 / 1024
      )));
    }
    self.waiting.fetch_add(length, Ordering::Relaxed);
    sender.send(data).map_err(|_| {
      self.waiting.fetch_sub(length, Ordering::Relaxed);
      ProcessError::InvalidArgument("the program no longer reads its input".into())
    })
  }

  /// Closes it once what was sent is written; false if it was closed.
  fn close(&self) -> bool {
    self
      .sender
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take()
      .is_some()
  }
}

/// A program started with `spawn`.
struct Spawned {
  group: Group,
  killed: Arc<AtomicBool>,
  /// Its input, when `spawn-interactive` started it.
  input: Option<Arc<Input>>,
}

/// The programs one instance of an extension started with `spawn`.
#[derive(Default)]
pub struct ProcessTable {
  next: AtomicU64,
  running: Mutex<HashMap<u64, Spawned>>,
  /// Set once the instance stopped: nothing more may start.
  closed: AtomicBool,
}

impl ProcessTable {
  fn running(&self) -> MutexGuard<'_, HashMap<u64, Spawned>> {
    self.running.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Stops a program; false if it had ended.
  fn kill(&self, id: u64) -> bool {
    let Some((group, killed)) = self
      .running()
      .get(&id)
      .map(|spawned| (spawned.group.clone(), spawned.killed.clone()))
    else {
      return false;
    };
    killed.store(true, Ordering::Relaxed);
    lock(&group).kill();
    true
  }

  /// The input of a running program `spawn-interactive` started.
  fn input(&self, id: u64) -> Result<Arc<Input>, ProcessError> {
    match self.running().get(&id) {
      Some(Spawned {
        input: Some(input), ..
      }) => Ok(input.clone()),
      Some(_) => Err(ProcessError::InvalidArgument(
        "the program wasn't started with spawn-interactive".into(),
      )),
      None => Err(ProcessError::InvalidArgument(
        "no program with this id is running".into(),
      )),
    }
  }

  /// Stops every program, and refuses new ones.
  pub fn kill_all(&self) {
    self.closed.store(true, Ordering::Relaxed);
    let groups: Vec<Group> = self
      .running()
      .drain()
      .map(|(_, spawned)| {
        spawned.killed.store(true, Ordering::Relaxed);
        spawned.group
      })
      .collect();
    for group in groups {
      lock(&group).kill();
    }
  }
}

/// A program to start.
pub(crate) struct Request {
  program: PathBuf,
  args: Vec<String>,
  cwd: PathBuf,
  env: Vec<(String, String)>,
  stdin: Option<Vec<u8>>,
  /// Its input stays open, after `stdin`, for `write-input` (2.1).
  interactive: bool,
}

/// A started program and the threads reading its output.
struct Running {
  group: Group,
  stdout: Receiver<Vec<u8>>,
  stderr: Receiver<Vec<u8>>,
  truncated: Arc<AtomicBool>,
  /// Its open input, for an interactive request.
  input: Option<Arc<Input>>,
}

/// Starts a program. With a `sink`, its output goes there line by line
/// rather than being collected.
fn start(request: Request, sink: Option<Sink>) -> Result<Running, ProcessError> {
  start_with(request, sink, MAX_LINE_BYTES)
}

/// Starts a program as [`start`] does, cutting lines handed to a `sink` at
/// `max_line` bytes.
fn start_with(
  request: Request,
  sink: Option<Sink>,
  max_line: usize,
) -> Result<Running, ProcessError> {
  let mut command = Command::new(&request.program);
  command
    .args(&request.args)
    .current_dir(&request.cwd)
    .envs(request.env.iter().map(|(key, value)| (key, value)))
    .stdin(if request.stdin.is_some() || request.interactive {
      Stdio::piped()
    } else {
      Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  let mut group = ProcessGroup::spawn(&mut command).map_err(|error| {
    if error.kind() == std::io::ErrorKind::NotFound {
      ProcessError::NotFound(format!("{} was not found", request.program.display()))
    } else {
      ProcessError::NotFound(format!(
        "{} could not be started: {error}",
        request.program.display()
      ))
    }
  })?;
  let child = group.child();
  let mut open_input = None;
  if request.interactive {
    if let Some(pipe) = child.stdin.take() {
      open_input = Some(Input::start(pipe, request.stdin));
    }
  } else if let (Some(input), Some(mut pipe)) = (request.stdin, child.stdin.take()) {
    spawn_thread("extension-process-input", move || {
      // A program that exits without reading it all closes the pipe.
      let _ = pipe.write_all(&input);
    });
  }
  let missing = || ProcessError::Failed("the program's output could not be read".into());
  let stdout = child.stdout.take().ok_or_else(missing)?;
  let stderr = child.stderr.take().ok_or_else(missing)?;
  let budget = Arc::new(AtomicUsize::new(MAX_OUTPUT_BYTES));
  let truncated = Arc::new(AtomicBool::new(false));
  let (stdout, stderr) = match sink {
    Some(sink) => (
      read_lines(stdout, false, sink.clone(), max_line),
      read_lines(stderr, true, sink, max_line),
    ),
    None => (
      read_all(stdout, budget.clone(), truncated.clone()),
      read_all(stderr, budget, truncated.clone()),
    ),
  };
  Ok(Running {
    group: Arc::new(Mutex::new(group)),
    stdout,
    stderr,
    truncated,
    input: open_input,
  })
}

fn spawn_thread(name: &str, work: impl FnOnce() + Send + 'static) {
  if let Err(error) = std::thread::Builder::new()
    .name(name.to_owned())
    .spawn(work)
  {
    tracing::error!(%error, "could not start a thread for an extension's program");
  }
}

/// Reads `source` to its end on a thread, keeping what fits in `budget`
/// (shared with the program's other output).
fn read_all(
  mut source: impl Read + Send + 'static,
  budget: Arc<AtomicUsize>,
  truncated: Arc<AtomicBool>,
) -> Receiver<Vec<u8>> {
  let (sender, receiver) = mpsc::channel();
  spawn_thread("extension-process-output", move || {
    let mut kept = Vec::new();
    let mut chunk = vec![0; 64 * 1024];
    loop {
      match source.read(&mut chunk) {
        Ok(0) | Err(_) => break,
        Ok(read) => {
          let allowed = budget
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
              Some(left - left.min(read))
            })
            .map_or(0, |left| left.min(read));
          kept.extend_from_slice(&chunk[..allowed]);
          if allowed < read {
            truncated.store(true, Ordering::Relaxed);
          }
        }
      }
    }
    let _ = sender.send(kept);
  });
  receiver
}

/// Reads `source` to its end on a thread, handing each batch of complete
/// lines read to `sink` (and the last one, without a line ending, at the
/// end), cut at `max_line` bytes. What the receiver gets is empty: it says
/// the reading ended.
fn read_lines(
  mut source: impl Read + Send + 'static,
  stderr: bool,
  sink: Sink,
  max_line: usize,
) -> Receiver<Vec<u8>> {
  let (sender, receiver) = mpsc::channel();
  spawn_thread("extension-process-lines", move || {
    let mut splitter = LineSplitter::new(max_line);
    let mut chunk = vec![0; 64 * 1024];
    loop {
      let read = match source.read(&mut chunk) {
        Ok(0) | Err(_) => break,
        Ok(read) => read,
      };
      let lines = splitter.push(&chunk[..read], stderr);
      if !lines.is_empty() {
        sink(lines);
      }
    }
    if let Some(last) = splitter.finish(stderr) {
      sink(vec![last]);
    }
    let _ = sender.send(Vec::new());
  });
  receiver
}

/// Splits output into lines as it's read.
struct LineSplitter {
  pending: Vec<u8>,
  /// In the rest of a line that was too long, until its end.
  skipping: bool,
  /// The longest line handed over, in bytes.
  max: usize,
}

impl Default for LineSplitter {
  fn default() -> Self {
    Self::new(MAX_LINE_BYTES)
  }
}

impl LineSplitter {
  fn new(max: usize) -> Self {
    Self {
      pending: Vec::new(),
      skipping: false,
      max,
    }
  }

  /// The lines `bytes` completes.
  fn push(&mut self, bytes: &[u8], stderr: bool) -> Vec<OutputLine> {
    let mut lines = Vec::new();
    let mut rest = bytes;
    while let Some(at) = rest.iter().position(|&byte| byte == b'\n') {
      if !self.skipping {
        self.pending.extend_from_slice(&rest[..at]);
        lines.push(output_line(&self.pending, stderr, self.max));
      }
      self.pending.clear();
      self.skipping = false;
      rest = &rest[at + 1..];
    }
    if !self.skipping {
      self.pending.extend_from_slice(rest);
      if self.pending.len() > self.max {
        lines.push(output_line(&self.pending, stderr, self.max));
        self.pending.clear();
        self.skipping = true;
      }
    }
    lines
  }

  /// The last line, if the output didn't end with a line ending.
  fn finish(self, stderr: bool) -> Option<OutputLine> {
    (!self.pending.is_empty() && !self.skipping)
      .then(|| output_line(&self.pending, stderr, self.max))
  }
}

/// A line as it's handed over: without a carriage return before its end,
/// as text, and at most `max` bytes.
fn output_line(bytes: &[u8], stderr: bool, max: usize) -> OutputLine {
  let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
  let mut text = String::from_utf8_lossy(bytes).into_owned();
  if text.len() > max {
    let mut end = max;
    while !text.is_char_boundary(end) {
      end -= 1;
    }
    text.truncate(end);
  }
  OutputLine { stderr, text }
}

/// Adds `later` lines to `pending` ones, dropping the oldest past
/// [`MAX_PENDING_LINE_BYTES`].
pub fn merge_lines(pending: &mut ProcessLines, later: ProcessLines) {
  pending.lines.extend(later.lines);
  pending.dropped = pending.dropped.saturating_add(later.dropped);
  trim_lines(pending);
}

/// Drops the oldest lines past [`MAX_PENDING_LINE_BYTES`].
pub fn trim_lines(lines: &mut ProcessLines) {
  let mut total: usize = lines.lines.iter().map(|line| line.text.len()).sum();
  let mut drop = 0;
  while total > MAX_PENDING_LINE_BYTES && drop < lines.lines.len() {
    total -= lines.lines[drop].text.len();
    drop += 1;
  }
  if drop > 0 {
    lines.lines.drain(..drop);
    lines.dropped = lines
      .dropped
      .saturating_add(u32::try_from(drop).unwrap_or(u32::MAX));
  }
}

/// Waits for a started program to end, stopping it at `deadline` or when
/// `stopped` says so (with that error).
fn wait(
  running: Running,
  deadline: Option<Instant>,
  stopped: impl Fn() -> Option<ProcessError>,
) -> Result<ProcessOutput, ProcessError> {
  let status = loop {
    match lock(&running.group).child().try_wait() {
      Ok(Some(status)) => break status,
      Ok(None) => {}
      Err(error) => return Err(ProcessError::Failed(error.to_string())),
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
      lock(&running.group).kill();
      return Err(ProcessError::TimedOut);
    }
    if let Some(error) = stopped() {
      lock(&running.group).kill();
      return Err(error);
    }
    std::thread::sleep(POLL);
  };
  // The output ends with the program, unless something it started still
  // holds on to it: then that is stopped.
  let collect = |receiver: &Receiver<Vec<u8>>| {
    receiver.recv_timeout(DRAIN_WAIT).unwrap_or_else(|_| {
      lock(&running.group).kill();
      receiver.recv().unwrap_or_default()
    })
  };
  let stdout = collect(&running.stdout);
  let stderr = collect(&running.stderr);
  Ok(ProcessOutput {
    exit_code: status.code(),
    stdout,
    stderr,
    truncated: running.truncated.load(Ordering::Relaxed),
  })
}

/// The user's home folder, where a terminal runs when no folder is open.
pub(crate) fn home_folder() -> Option<PathBuf> {
  let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
  std::env::var_os(variable)
    .map(PathBuf::from)
    .filter(|home| home.is_absolute())
}

impl ApiContext {
  /// Checks a request and finds the program to run. Without a `cwd`, it
  /// runs in the open folder, else in `fallback_cwd`.
  pub(crate) fn request(
    &self,
    program: &str,
    args: Vec<String>,
    options: ProcessOptions,
    interactive: bool,
    fallback_cwd: Option<PathBuf>,
  ) -> Result<Request, ProcessError> {
    if !self.has(Permission::Processes) {
      return Err(ProcessError::PermissionDenied);
    }
    let shared = self
      .shared()
      .map_err(|_| ProcessError::Failed("the editor is closing".into()))?;
    let declared = self
      .manifest()
      .contributions
      .program(program)
      .ok_or_else(|| ProcessError::UnknownProgram(program.to_owned()))?
      .clone();
    let folder = shared.embedder.open_folder();
    if folder.as_ref().is_some_and(|folder| !folder.trusted) {
      return Err(ProcessError::Untrusted);
    }
    if args.len() > MAX_ARGS || args.iter().map(String::len).sum::<usize>() > MAX_ARG_BYTES {
      return Err(ProcessError::TooLarge(format!(
        "at most {MAX_ARGS} arguments, of {} KiB together",
        MAX_ARG_BYTES / 1024
      )));
    }
    if args.iter().any(|arg| arg.contains('\0')) {
      return Err(ProcessError::InvalidArgument(
        "arguments can't contain NUL characters".into(),
      ));
    }
    if options.env.len() > MAX_ENV {
      return Err(ProcessError::TooLarge(format!(
        "at most {MAX_ENV} environment variables"
      )));
    }
    for (key, value) in &options.env {
      if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
        return Err(ProcessError::InvalidArgument(format!(
          "{key:?} is not a valid environment variable"
        )));
      }
    }
    if options
      .stdin
      .as_ref()
      .is_some_and(|stdin| stdin.len() > MAX_STDIN_BYTES)
    {
      return Err(ProcessError::TooLarge(format!(
        "at most {} MiB of input",
        MAX_STDIN_BYTES / 1024 / 1024
      )));
    }
    let cwd = match options.cwd {
      Some(cwd) => PathBuf::from(cwd),
      None => folder
        .as_ref()
        .map(|folder| folder.root.clone())
        .or(fallback_cwd)
        .ok_or_else(|| {
          ProcessError::InvalidArgument("no folder is open, so a cwd is needed".into())
        })?,
    };
    if !cwd.is_absolute() || !cwd.is_dir() {
      return Err(ProcessError::InvalidArgument(format!(
        "{} is not a folder",
        cwd.display()
      )));
    }
    let custom = shared.program_path(self.extension_id(), &declared.name);
    let in_folder = folder
      .as_ref()
      .zip(declared.workspace.as_deref())
      .and_then(|(folder, path)| executable(&folder.root.join(path)));
    let program = if let Some(custom) = custom {
      PathBuf::from(custom)
    } else if let Some(in_folder) = in_folder {
      in_folder
    } else if declared.command.contains(['/', '\\']) {
      self.manifest().dir.join(&declared.command)
    } else {
      find_on_path(&declared.command).ok_or_else(|| {
        ProcessError::NotFound(format!(
          "{} is not installed, or not on the PATH",
          declared.command
        ))
      })?
    };
    let mut env = options.env;
    if declared.askpass {
      env.extend(shared.askpass_env(self.extension_id()));
    }
    Ok(Request {
      program,
      args,
      cwd,
      env,
      stdin: options.stdin,
      interactive,
    })
  }
}

/// A request the extension host checked, for the embedder to run in a
/// terminal (`terminals::run`, 2.1).
pub(crate) struct CheckedRequest {
  pub program: PathBuf,
  pub args: Vec<String>,
  pub cwd: PathBuf,
  pub env: Vec<(String, String)>,
}

impl From<Request> for CheckedRequest {
  fn from(request: Request) -> Self {
    Self {
      program: request.program,
      args: request.args,
      cwd: request.cwd,
      env: request.env,
    }
  }
}

/// `path`, or on Windows the same with an executable extension, if it's a
/// file.
fn executable(path: &Path) -> Option<PathBuf> {
  let extensions: &[&str] = if cfg!(windows) {
    &["", ".exe", ".cmd", ".bat"]
  } else {
    &[""]
  };
  extensions
    .iter()
    .map(|extension| {
      let mut name = path.as_os_str().to_owned();
      name.push(extension);
      PathBuf::from(name)
    })
    .find(|candidate| candidate.is_file())
}

impl Host for StoreState {
  fn run(
    &mut self,
    program: String,
    args: Vec<String>,
    options: ProcessOptions,
  ) -> Result<ProcessOutput, ProcessError> {
    let timeout = options.timeout_ms;
    let request = self.api.request(&program, args, options, false, None)?;
    let mut deadline = self.call.as_ref().map(|call| {
      call
        .deadline
        .checked_sub(DEADLINE_MARGIN)
        .unwrap_or(call.deadline)
    });
    if let Some(timeout) = timeout {
      let mine = Instant::now() + Duration::from_millis(u64::from(timeout));
      deadline = Some(deadline.map_or(mine, |deadline| deadline.min(mine)));
    }
    tracing::debug!(extension = %self.api.extension_id(), %program, "running a program");
    let cancel = self.cancel.clone();
    wait(start(request, None)?, deadline, || {
      cancel
        .load(Ordering::Relaxed)
        .then_some(ProcessError::Killed)
    })
  }

  fn spawn(
    &mut self,
    program: String,
    args: Vec<String>,
    options: ProcessOptions,
  ) -> Result<u64, ProcessError> {
    self.spawn_program(program, args, options, Mode::Collect)
  }

  fn spawn_streaming(
    &mut self,
    program: String,
    args: Vec<String>,
    options: ProcessOptions,
  ) -> Result<u64, ProcessError> {
    self.spawn_program(program, args, options, Mode::Stream)
  }

  fn spawn_interactive(
    &mut self,
    program: String,
    args: Vec<String>,
    options: ProcessOptions,
  ) -> Result<u64, ProcessError> {
    self.spawn_program(program, args, options, Mode::Interactive)
  }

  fn write_input(&mut self, process: u64, data: Vec<u8>) -> Result<(), ProcessError> {
    if data.is_empty() {
      // Still says whether the program reads input.
      self.api.processes().input(process)?;
      return Ok(());
    }
    self.api.processes().input(process)?.write(data)
  }

  fn close_input(&mut self, process: u64) -> bool {
    self
      .api
      .processes()
      .input(process)
      .is_ok_and(|input| input.close())
  }

  fn kill(&mut self, process: u64) -> bool {
    self.api.processes().kill(process)
  }
}

impl StoreState {
  /// Starts a program for `spawn`, `spawn-streaming`, or
  /// `spawn-interactive`, as `mode` says.
  fn spawn_program(
    &mut self,
    program: String,
    args: Vec<String>,
    options: ProcessOptions,
    mode: Mode,
  ) -> Result<u64, ProcessError> {
    let deadline = options
      .timeout_ms
      .map(|timeout| Instant::now() + Duration::from_millis(u64::from(timeout)));
    let request = self
      .api
      .request(&program, args, options, mode == Mode::Interactive, None)?;
    let table = self.api.processes();
    if table.closed.load(Ordering::Relaxed) {
      return Err(ProcessError::Failed("the extension is stopping".into()));
    }
    if table.running().len() >= MAX_RUNNING {
      return Err(ProcessError::TooLarge(format!(
        "at most {MAX_RUNNING} programs can run at once"
      )));
    }
    let id = table.next.fetch_add(1, Ordering::Relaxed) + 1;
    let (shared, extension, generation) = (
      self.api.weak_shared(),
      self.api.extension_id().to_owned(),
      self.api.generation(),
    );
    let sink = match mode {
      Mode::Collect => None,
      Mode::Stream => {
        let (shared, extension, table) = (shared.clone(), extension.clone(), table.clone());
        Some(Arc::new(move |lines: Vec<OutputLine>| {
          if table.closed.load(Ordering::Relaxed) {
            return;
          }
          if let Some(shared) = shared.upgrade() {
            let mut lines = ProcessLines {
              process: id,
              lines,
              dropped: 0,
            };
            trim_lines(&mut lines);
            shared.deliver(
              &extension,
              generation,
              Event::notice(WitEvent::ProcessOutput(lines)),
            );
          }
        }) as Sink)
      }
      // Straight to this start's queue (stopped with it), then waiting
      // while the extension is behind, so the program waits to write more.
      Mode::Interactive => {
        let (queue, table) = (self.api.queue(), table.clone());
        Some(Arc::new(move |lines: Vec<OutputLine>| {
          if table.closed.load(Ordering::Relaxed) {
            return;
          }
          queue.push_output(ProcessLines {
            process: id,
            lines,
            dropped: 0,
          });
          queue.wait_for_output(id, MAX_PENDING_LINE_BYTES);
        }) as Sink)
      }
    };
    let max_line = if mode == Mode::Interactive {
      MAX_INTERACTIVE_LINE_BYTES
    } else {
      MAX_LINE_BYTES
    };
    let mut running = start_with(request, sink, max_line)?;
    let killed = Arc::new(AtomicBool::new(false));
    table.running().insert(
      id,
      Spawned {
        group: running.group.clone(),
        killed: killed.clone(),
        input: running.input.take(),
      },
    );
    tracing::debug!(extension = %extension, %program, id, "started a program");
    spawn_thread("extension-process", move || {
      let outcome = wait(running, deadline, || {
        killed
          .load(Ordering::Relaxed)
          .then_some(ProcessError::Killed)
      });
      table.running().remove(&id);
      if table.closed.load(Ordering::Relaxed) {
        return;
      }
      if let Some(shared) = shared.upgrade() {
        shared.deliver(
          &extension,
          generation,
          Event::notice(WitEvent::ProcessFinished(FinishedProcess {
            process: id,
            outcome,
          })),
        );
      }
    });
    Ok(id)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A program every test machine has: the Rust compiler that built this.
  fn rustc() -> PathBuf {
    find_on_path("rustc").expect("rustc on the PATH")
  }

  fn request(args: &[&str]) -> Request {
    Request {
      program: rustc(),
      args: args.iter().map(|arg| (*arg).to_owned()).collect(),
      cwd: std::env::temp_dir(),
      env: Vec::new(),
      stdin: None,
      interactive: false,
    }
  }

  #[test]
  fn runs_a_program_and_collects_its_output() {
    let output = wait(
      start(request(&["--version"]), None).expect("starts"),
      None,
      || None,
    )
    .expect("finishes");
    assert_eq!(output.exit_code, Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("rustc "));
    assert!(!output.truncated);
  }

  #[test]
  fn reports_failure_exit_codes_and_errors() {
    let output = wait(
      start(request(&["--no-such-flag"]), None).expect("starts"),
      None,
      || None,
    )
    .expect("finishes");
    assert_ne!(output.exit_code, Some(0));
    assert!(!output.stderr.is_empty());
  }

  /// Not a test: a program that runs a long time, for the tests below,
  /// which start this test binary to run only it.
  #[test]
  #[ignore = "run by stops_programs_at_the_deadline_or_when_asked"]
  fn sleeps_for_a_minute() {
    std::thread::sleep(Duration::from_secs(60));
  }

  fn sleeper() -> Request {
    Request {
      program: std::env::current_exe().expect("test binary"),
      args: [
        "--ignored",
        "--exact",
        "processes::tests::sleeps_for_a_minute",
      ]
      .map(str::to_owned)
      .to_vec(),
      cwd: std::env::temp_dir(),
      env: Vec::new(),
      stdin: None,
      interactive: false,
    }
  }

  #[test]
  fn stops_programs_at_the_deadline_or_when_asked() {
    let started = Instant::now();
    let deadline = Some(Instant::now() + Duration::from_millis(300));
    let result = wait(start(sleeper(), None).expect("starts"), deadline, || None);
    assert!(matches!(result, Err(ProcessError::TimedOut)), "{result:?}");

    let ask = Instant::now() + Duration::from_millis(300);
    let result = wait(start(sleeper(), None).expect("starts"), None, || {
      (Instant::now() >= ask).then_some(ProcessError::Killed)
    });
    assert!(matches!(result, Err(ProcessError::Killed)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(20));
  }

  #[test]
  fn passes_input_to_the_program() {
    let mut compile = request(&["--crate-type=lib", "--emit=metadata", "-o", "-", "-"]);
    compile.stdin = Some(b"pub fn f() {}".to_vec());
    let output = wait(start(compile, None).expect("starts"), None, || None).expect("finishes");
    assert_eq!(
      output.exit_code,
      Some(0),
      "{}",
      String::from_utf8_lossy(&output.stderr)
    );
  }

  #[test]
  fn streams_output_line_by_line() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink: Sink = {
      let lines = lines.clone();
      Arc::new(move |batch: Vec<OutputLine>| {
        lines.lock().expect("lines").extend(batch);
      })
    };
    let output = wait(
      start(request(&["--version", "--verbose"]), Some(sink)).expect("starts"),
      None,
      || None,
    )
    .expect("finishes");
    assert_eq!(output.exit_code, Some(0));
    assert!(output.stdout.is_empty(), "handed over, not collected");
    let lines = lines.lock().expect("lines");
    assert!(lines[0].text.starts_with("rustc "), "{lines:?}");
    assert!(lines.iter().any(|line| line.text.starts_with("host: ")));
    assert!(
      lines
        .iter()
        .all(|line| !line.stderr && !line.text.ends_with('\r'))
    );
  }

  #[test]
  fn splits_lines_across_reads_and_cuts_long_ones() {
    let mut splitter = LineSplitter::default();
    assert!(splitter.push(b"par", false).is_empty());
    let lines = splitter.push(b"tial\r\nwhole\nnext", false);
    let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(texts, ["partial", "whole"]);
    assert_eq!(
      splitter.finish(true).map(|line| (line.text, line.stderr)),
      Some(("next".into(), true))
    );

    let mut splitter = LineSplitter::default();
    let long = vec![b'x'; MAX_LINE_BYTES + 10];
    let lines = splitter.push(&long, false);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text.len(), MAX_LINE_BYTES);
    // The rest of the long line is left out, up to its end.
    let lines = splitter.push(b"xxx\nafter\n", false);
    let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(texts, ["after"]);
    assert_eq!(splitter.finish(false), None);
  }

  #[test]
  fn pending_lines_drop_the_oldest_past_the_limit() {
    let line = |text: &str| OutputLine {
      stderr: false,
      text: text.to_owned(),
    };
    // Together with "first" and "last", just past the limit.
    let big = "y".repeat(MAX_PENDING_LINE_BYTES / 2 - 2);
    let mut pending = ProcessLines {
      process: 1,
      lines: vec![line("first"), line(&big)],
      dropped: 0,
    };
    merge_lines(
      &mut pending,
      ProcessLines {
        process: 1,
        lines: vec![line(&big), line("last")],
        dropped: 2,
      },
    );
    assert_eq!(pending.dropped, 3, "two before, and \"first\"");
    assert_eq!(pending.lines.len(), 3);
    assert_eq!(pending.lines[2].text, "last");
  }

  /// Not a test: a program that writes back each line of its input, for
  /// the test below, which starts this test binary to run only it.
  #[test]
  #[ignore = "run by talks_to_an_interactive_program"]
  fn echoes_its_input() {
    for line in std::io::stdin().lines() {
      println!("echo {}", line.expect("input"));
    }
  }

  #[test]
  fn talks_to_an_interactive_program() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink: Sink = {
      let lines = lines.clone();
      Arc::new(move |batch: Vec<OutputLine>| {
        lines.lock().expect("lines").extend(batch);
      })
    };
    let echo = Request {
      program: std::env::current_exe().expect("test binary"),
      args: [
        "--ignored",
        "--exact",
        "--nocapture",
        "processes::tests::echoes_its_input",
      ]
      .map(str::to_owned)
      .to_vec(),
      cwd: std::env::temp_dir(),
      env: Vec::new(),
      stdin: Some(b"first\n".to_vec()),
      interactive: true,
    };
    let mut running = start_with(echo, Some(sink), MAX_INTERACTIVE_LINE_BYTES).expect("starts");
    let input = running.input.take().expect("open input");
    let long = "x".repeat(MAX_LINE_BYTES * 4);
    input
      .write(format!("{long}\n").into_bytes())
      .expect("writes");
    input.write(b"last\n".to_vec()).expect("writes");
    assert!(input.close());
    assert!(!input.close(), "closed once");
    assert!(matches!(
      input.write(b"late\n".to_vec()),
      Err(ProcessError::InvalidArgument(_))
    ));
    let output = wait(running, None, || None).expect("finishes");
    assert_eq!(output.exit_code, Some(0));
    let lines = lines.lock().expect("lines");
    let echoed: Vec<&str> = lines
      .iter()
      .filter_map(|line| line.text.strip_prefix("echo "))
      .collect();
    assert_eq!(echoed.len(), 3, "{echoed:?}");
    assert_eq!(echoed[0], "first");
    assert_eq!(echoed[1], long, "long lines aren't cut");
    assert_eq!(echoed[2], "last");
  }

  #[test]
  fn interactive_lines_are_cut_only_past_their_limit() {
    let mut splitter = LineSplitter::new(8);
    let lines = splitter.push(b"12345678\n123456789", false);
    let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
    assert_eq!(texts, ["12345678", "12345678"]);
    assert_eq!(
      splitter.finish(false),
      None,
      "the cut line's rest is left out"
    );
  }

  #[test]
  fn missing_programs_are_not_found() {
    let mut missing = request(&[]);
    missing.program = PathBuf::from("flareite-no-such-program-anywhere");
    assert!(matches!(
      start(missing, None),
      Err(ProcessError::NotFound(_))
    ));
  }
}
