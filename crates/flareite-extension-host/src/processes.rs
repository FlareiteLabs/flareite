//! The host functions of the `processes` interface (API 1.6): programs the
//! manifest declares in `[[programs]]`, run on the user's computer.
//!
//! An extension names a declared program, never a path, and the user
//! granted `processes` seeing the list. Programs run only while the open
//! folder is trusted (or none is open), since programs such as Git and
//! build tools run code from the folder they work in. Each program runs in
//! a process group ([`ProcessGroup`]), so stopping it stops what it
//! started too. Output is collected up to a limit; `run` waits within the
//! running call's deadline, and `spawn` reports the outcome by running a
//! command. `spawn-streaming` (API 1.17) instead hands the output over line
//! by line as it's read, by running another command; lines waiting for it
//! are merged into one event, and dropped past a limit, so a program that
//! writes faster than the extension reads can't flood its queue. Programs
//! still running when the extension stops are stopped.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use flareite_process::{ProcessGroup, find_on_path};

use crate::api::ApiContext;
use crate::bindings::flareite::extension::processes::{
  FinishedProcess, Host, OutputLine, ProcessError, ProcessLines, ProcessOptions, ProcessOutput,
};
use crate::permissions::Permission;
use crate::runtime::StoreState;
use crate::worker::{Event, Payload};

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

/// Receives the lines a streaming program writes, as they're read.
type Sink = Arc<dyn Fn(Vec<OutputLine>) + Send + Sync>;

type Group = Arc<Mutex<ProcessGroup>>;

fn lock(group: &Group) -> MutexGuard<'_, ProcessGroup> {
  group.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A program started with `spawn`.
struct Spawned {
  group: Group,
  killed: Arc<AtomicBool>,
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
struct Request {
  program: PathBuf,
  args: Vec<String>,
  cwd: PathBuf,
  env: Vec<(String, String)>,
  stdin: Option<Vec<u8>>,
}

/// A started program and the threads reading its output.
struct Running {
  group: Group,
  stdout: Receiver<Vec<u8>>,
  stderr: Receiver<Vec<u8>>,
  truncated: Arc<AtomicBool>,
}

/// Starts a program. With a `sink`, its output goes there line by line
/// rather than being collected.
fn start(request: Request, sink: Option<Sink>) -> Result<Running, ProcessError> {
  let mut command = Command::new(&request.program);
  command
    .args(&request.args)
    .current_dir(&request.cwd)
    .envs(request.env.iter().map(|(key, value)| (key, value)))
    .stdin(if request.stdin.is_some() {
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
  if let (Some(input), Some(mut pipe)) = (request.stdin, child.stdin.take()) {
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
      read_lines(stdout, false, sink.clone()),
      read_lines(stderr, true, sink),
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
/// end). What the receiver gets is empty: it says the reading ended.
fn read_lines(
  mut source: impl Read + Send + 'static,
  stderr: bool,
  sink: Sink,
) -> Receiver<Vec<u8>> {
  let (sender, receiver) = mpsc::channel();
  spawn_thread("extension-process-lines", move || {
    let mut splitter = LineSplitter::default();
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
#[derive(Default)]
struct LineSplitter {
  pending: Vec<u8>,
  /// In the rest of a line that was too long, until its end.
  skipping: bool,
}

impl LineSplitter {
  /// The lines `bytes` completes.
  fn push(&mut self, bytes: &[u8], stderr: bool) -> Vec<OutputLine> {
    let mut lines = Vec::new();
    let mut rest = bytes;
    while let Some(at) = rest.iter().position(|&byte| byte == b'\n') {
      if !self.skipping {
        self.pending.extend_from_slice(&rest[..at]);
        lines.push(output_line(&self.pending, stderr));
      }
      self.pending.clear();
      self.skipping = false;
      rest = &rest[at + 1..];
    }
    if !self.skipping {
      self.pending.extend_from_slice(rest);
      if self.pending.len() > MAX_LINE_BYTES {
        lines.push(output_line(&self.pending, stderr));
        self.pending.clear();
        self.skipping = true;
      }
    }
    lines
  }

  /// The last line, if the output didn't end with a line ending.
  fn finish(self, stderr: bool) -> Option<OutputLine> {
    (!self.pending.is_empty() && !self.skipping).then(|| output_line(&self.pending, stderr))
  }
}

/// A line as it's handed over: without a carriage return before its end,
/// as text, and at most [`MAX_LINE_BYTES`].
fn output_line(bytes: &[u8], stderr: bool) -> OutputLine {
  let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
  let mut text = String::from_utf8_lossy(bytes).into_owned();
  if text.len() > MAX_LINE_BYTES {
    let mut end = MAX_LINE_BYTES;
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

impl ApiContext {
  /// Checks a request and finds the program to run.
  fn request(
    &self,
    program: &str,
    args: Vec<String>,
    options: ProcessOptions,
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
    })
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
    let request = self.api.request(&program, args, options)?;
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
    command: String,
  ) -> Result<u64, ProcessError> {
    self.spawn_program(program, args, options, command, None)
  }

  fn spawn_streaming(
    &mut self,
    program: String,
    args: Vec<String>,
    options: ProcessOptions,
    command: String,
    output: String,
  ) -> Result<u64, ProcessError> {
    self.spawn_program(program, args, options, command, Some(output))
  }

  fn kill(&mut self, process: u64) -> bool {
    self.api.processes().kill(process)
  }

  fn finished(&mut self) -> Option<FinishedProcess> {
    match &self.api.payload {
      Payload::Finished(finished) => Some(finished.clone()),
      _ => None,
    }
  }

  fn lines(&mut self) -> Option<ProcessLines> {
    match &self.api.payload {
      Payload::Lines(lines) => Some(lines.clone()),
      _ => None,
    }
  }
}

impl StoreState {
  /// Starts a program for `spawn`, or for `spawn-streaming` with its
  /// `output` command.
  fn spawn_program(
    &mut self,
    program: String,
    args: Vec<String>,
    options: ProcessOptions,
    command: String,
    output: Option<String>,
  ) -> Result<u64, ProcessError> {
    for command in std::iter::once(&command).chain(output.as_ref()) {
      if !self.api.manifest().declares_command(command) {
        return Err(ProcessError::InvalidArgument(format!(
          "{command:?} is not one of this extension's commands"
        )));
      }
    }
    let deadline = options
      .timeout_ms
      .map(|timeout| Instant::now() + Duration::from_millis(u64::from(timeout)));
    let request = self.api.request(&program, args, options)?;
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
    let sink = output.map(|output| {
      let (shared, extension, table) = (shared.clone(), extension.clone(), table.clone());
      Arc::new(move |lines: Vec<OutputLine>| {
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
            Event::background(output.clone(), None, Payload::Lines(lines)),
          );
        }
      }) as Sink
    });
    let running = start(request, sink)?;
    let killed = Arc::new(AtomicBool::new(false));
    table.running().insert(
      id,
      Spawned {
        group: running.group.clone(),
        killed: killed.clone(),
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
          Event::background(
            command,
            None,
            Payload::Finished(FinishedProcess {
              process: id,
              outcome,
            }),
          ),
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
