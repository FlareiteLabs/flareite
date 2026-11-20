//! Integrated terminals for Flareite.
//!
//! A [`Terminal`] runs a shell in a pseudoterminal (ConPTY on Windows, a
//! Unix pty elsewhere, through `portable-pty`) and streams its output, as
//! text, to whoever is attached. Terminal emulation (escape sequences,
//! drawing, selection) happens in the frontend; this crate moves bytes.
//!
//! Three threads serve each terminal: one writes queued input (so callers
//! never block on a program that isn't reading), one reads output, and one
//! waits for the shell to exit. Output is decoded as UTF-8
//! ([`utf8::Utf8Stream`]), kept in a bounded history so a new frontend can
//! replay it ([`history::History`]), and paced by acknowledgements from the
//! frontend ([`flow`]).

pub mod flow;
pub mod history;
pub mod profiles;
pub mod shell;
pub mod utf8;

use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::flow::Flow;
use crate::history::History;
use crate::utf8::Utf8Stream;

pub use crate::shell::{Shell, find_on_path};

/// The Windows build, which decides how ConPTY redraws when a terminal is
/// resized. The emulator needs it to avoid duplicating lines (xterm.js's
/// `windowsPty` option). `None` on other platforms.
pub fn windows_build() -> Option<u32> {
  #[cfg(windows)]
  {
    Some(windows_version::OsVersion::current().build)
  }
  #[cfg(not(windows))]
  {
    None
  }
}

/// Output kept for replay, in bytes.
pub const HISTORY_LIMIT: usize = 256 * 1024;
/// The most input accepted in one write (a large paste).
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
/// Writes queued while the program isn't reading its input.
const INPUT_QUEUE: usize = 64;
const READ_BUFFER: usize = 16 * 1024;
/// How long to wait, after the shell exits, for output still on its way.
/// A program left running in the background can hold the terminal open.
const EXIT_GRACE: Duration = Duration::from_secs(1);
/// How long a killed shell has to exit after its hangup before it's killed
/// outright (Unix). A shell can catch the hangup, and bash caught it in a
/// signal handler can miss it.
#[cfg(unix)]
const KILL_GRACE: Duration = Duration::from_secs(2);

/// A terminal's size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
  pub cols: u16,
  pub rows: u16,
}

impl TerminalSize {
  pub const MAX_COLS: u16 = 1000;
  pub const MAX_ROWS: u16 = 500;

  /// A size within what pseudoterminals accept.
  pub fn clamped(cols: u32, rows: u32) -> Self {
    let clamp = |value: u32, min: u16, max: u16| {
      u16::try_from(value).map_or(max, |value| value.clamp(min, max))
    };
    Self {
      cols: clamp(cols, 2, Self::MAX_COLS),
      rows: clamp(rows, 1, Self::MAX_ROWS),
    }
  }
}

impl From<TerminalSize> for PtySize {
  fn from(size: TerminalSize) -> Self {
    Self {
      rows: size.rows,
      cols: size.cols,
      pixel_width: 0,
      pixel_height: 0,
    }
  }
}

pub struct TerminalOptions {
  pub shell: Shell,
  /// The working directory; the shell's default if `None`.
  pub cwd: Option<PathBuf>,
  /// Added to (or replacing) the inherited environment.
  pub env: Vec<(String, String)>,
  pub size: TerminalSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
  Output(String),
  /// Output from before the sink was attached, sent first. The emulator's
  /// replies to it (such as cursor position reports) are stale.
  Replay(String),
  /// The shell exited, with its exit code if it could be read. Always the
  /// last event.
  Exited(Option<u32>),
}

/// Receives a terminal's events, on its reader and waiter threads.
pub type Sink = Box<dyn FnMut(TerminalEvent) + Send>;

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
  #[error("Couldn't open a terminal: {0}")]
  Pty(String),
  #[error("Couldn't start {program}: {reason}")]
  Spawn { program: String, reason: String },
  #[error("The terminal isn't accepting more input right now.")]
  InputFull,
  #[error("The input is too large to send at once ({0} bytes).")]
  InputTooLarge(usize),
  #[error("The terminal's process has exited.")]
  Exited,
  #[error("Couldn't resize the terminal: {0}")]
  Resize(String),
}

struct Output {
  sink: Option<Sink>,
  history: History,
  /// Set once the shell has exited.
  exit: Option<Option<u32>>,
}

struct Shared {
  output: Mutex<Output>,
  flow: Flow,
  /// Dropped when the shell exits, which closes the pseudoterminal.
  master: Mutex<Option<Box<dyn MasterPty + Send>>>,
  /// Set as soon as the shell has been waited for, when its process id
  /// stops being its (before the exit is reported).
  reaped: AtomicBool,
  /// Set once the shell has been told to end.
  killed: AtomicBool,
}

impl Shared {
  fn output(&self) -> MutexGuard<'_, Output> {
    self.output.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Records and forwards output. Returns false once the terminal is done.
  fn deliver(&self, text: String) -> bool {
    if !self.flow.wait_for_room() {
      return false;
    }
    let mut output = self.output();
    if output.exit.is_some() {
      return false;
    }
    output.history.push(&text);
    if let Some(sink) = output.sink.as_mut() {
      self.flow.sent(flow::units(&text));
      sink(TerminalEvent::Output(text));
    }
    true
  }

  fn exited(&self, code: Option<u32>) {
    let mut output = self.output();
    output.exit = Some(code);
    self.flow.close();
    if let Some(sink) = output.sink.as_mut() {
      sink(TerminalEvent::Exited(code));
    }
  }
}

/// A shell running in a pseudoterminal. Dropping it kills the shell.
pub struct Terminal {
  shared: Arc<Shared>,
  input: mpsc::SyncSender<Vec<u8>>,
  killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
  pid: Option<u32>,
}

impl Terminal {
  /// Starts the shell. Its output is kept until a sink is
  /// [attached](Self::attach).
  pub fn spawn(options: TerminalOptions) -> Result<Self, TerminalError> {
    let pty_error = |error: &dyn std::fmt::Display| TerminalError::Pty(error.to_string());
    let pair = native_pty_system()
      .openpty(options.size.into())
      .map_err(|error| pty_error(&error))?;
    let reader = pair
      .master
      .try_clone_reader()
      .map_err(|error| pty_error(&error))?;
    let writer = pair
      .master
      .take_writer()
      .map_err(|error| pty_error(&error))?;

    let mut command = CommandBuilder::new(&options.shell.program);
    command.args(&options.shell.args);
    if let Some(cwd) = &options.cwd {
      command.cwd(cwd);
    }
    for (key, value) in &options.env {
      command.env(key, value);
    }
    let child = pair
      .slave
      .spawn_command(command)
      .map_err(|error| TerminalError::Spawn {
        program: options.shell.program.clone(),
        reason: error.to_string(),
      })?;
    // The shell has its own handle to the terminal now. Keeping ours would
    // keep the terminal open after the shell exits.
    drop(pair.slave);

    let mut killer = child.clone_killer();
    let pid = child.process_id();
    let shared = Arc::new(Shared {
      output: Mutex::new(Output {
        sink: None,
        history: History::new(HISTORY_LIMIT),
        exit: None,
      }),
      flow: Flow::default(),
      master: Mutex::new(Some(pair.master)),
      reaped: AtomicBool::new(false),
      killed: AtomicBool::new(false),
    });
    let (input, queue) = mpsc::sync_channel(INPUT_QUEUE);
    let (reader_done, reader_finished) = mpsc::channel();

    let started = spawn_thread("terminal-writer", move || write_loop(writer, &queue))
      .and_then(|()| {
        let shared = shared.clone();
        spawn_thread("terminal-reader", move || {
          read_loop(reader, &shared);
          let _ = reader_done.send(());
        })
      })
      .and_then(|()| {
        let shared = shared.clone();
        spawn_thread("terminal-waiter", move || {
          wait_loop(child, &shared, &reader_finished);
        })
      });
    if let Err(error) = started {
      let _ = killer.kill();
      shared.flow.close();
      return Err(pty_error(&error));
    }

    Ok(Self {
      shared,
      input,
      killer: Mutex::new(killer),
      pid,
    })
  }

  /// Sends events to `sink` from now on, replacing any previous sink. The
  /// output kept so far, and the exit if the shell has exited, are sent to
  /// it first.
  pub fn attach(&self, mut sink: Sink) {
    let mut output = self.shared.output();
    // Whoever was attached before won't acknowledge what they were sent.
    self.shared.flow.reset();
    let replay = output.history.text();
    if !replay.is_empty() {
      self.shared.flow.sent(flow::units(replay));
      sink(TerminalEvent::Replay(replay.to_owned()));
    }
    if let Some(code) = output.exit {
      sink(TerminalEvent::Exited(code));
    }
    output.sink = Some(sink);
  }

  /// Queues input for the shell (typed keys, pasted text, terminal replies).
  pub fn write(&self, data: &[u8]) -> Result<(), TerminalError> {
    if data.len() > MAX_INPUT_BYTES {
      return Err(TerminalError::InputTooLarge(data.len()));
    }
    if self.has_exited() {
      return Err(TerminalError::Exited);
    }
    self
      .input
      .try_send(data.to_vec())
      .map_err(|error| match error {
        mpsc::TrySendError::Full(_) => TerminalError::InputFull,
        mpsc::TrySendError::Disconnected(_) => TerminalError::Exited,
      })
  }

  pub fn resize(&self, size: TerminalSize) -> Result<(), TerminalError> {
    let master = self
      .shared
      .master
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    match master.as_ref() {
      Some(master) => master
        .resize(size.into())
        .map_err(|error| TerminalError::Resize(error.to_string())),
      None => Err(TerminalError::Exited),
    }
  }

  /// The frontend has drawn `units` of output (see [`flow`]).
  pub fn acknowledge(&self, units: usize) {
    self.shared.flow.acknowledge(units);
  }

  /// Ends the shell. Its exit is reported to the sink as usual.
  ///
  /// On Unix the shell is sent a hangup, as a closed terminal would, and is
  /// killed outright if it hasn't exited [a moment](KILL_GRACE) later.
  pub fn kill(&self) {
    if self.has_exited() || self.shared.killed.swap(true, Ordering::AcqRel) {
      return;
    }
    let mut killer = self.killer.lock().unwrap_or_else(PoisonError::into_inner);
    if let Err(error) = killer.kill() {
      tracing::debug!(%error, "could not kill a terminal's shell");
    }
    #[cfg(unix)]
    if let Some(pid) = self.pid {
      let shared = self.shared.clone();
      let escalated = spawn_thread("terminal-killer", move || {
        kill_if_still_running(&shared, pid)
      });
      if let Err(error) = escalated {
        tracing::debug!(%error, "could not watch a killed terminal's shell");
      }
    }
  }

  pub fn has_exited(&self) -> bool {
    self.shared.output().exit.is_some()
  }

  /// The shell's process id, where the platform reports it.
  pub fn pid(&self) -> Option<u32> {
    self.pid
  }
}

impl Drop for Terminal {
  fn drop(&mut self) {
    self.shared.output().sink = None;
    self.kill();
    self.shared.flow.close();
  }
}

fn spawn_thread(name: &str, work: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
  thread::Builder::new()
    .name(name.to_owned())
    .spawn(work)
    .map(drop)
}

fn write_loop(mut writer: Box<dyn Write + Send>, queue: &mpsc::Receiver<Vec<u8>>) {
  for data in queue {
    if let Err(error) = writer.write_all(&data).and_then(|()| writer.flush()) {
      tracing::debug!(%error, "terminal input not delivered");
      break;
    }
  }
}

fn read_loop(mut reader: Box<dyn Read + Send>, shared: &Shared) {
  let mut buffer = vec![0; READ_BUFFER];
  let mut decoder = Utf8Stream::default();
  loop {
    let read = match reader.read(&mut buffer) {
      Ok(0) => break,
      Ok(read) => read,
      Err(error) if error.kind() == ErrorKind::Interrupted => continue,
      // How the end shows up: EIO on Linux, a broken pipe on Windows.
      Err(_) => break,
    };
    let text = decoder.decode(&buffer[..read]);
    if !text.is_empty() && !shared.deliver(text) {
      return;
    }
  }
  let rest = decoder.finish();
  if !rest.is_empty() {
    shared.deliver(rest);
  }
}

fn wait_loop(
  mut child: Box<dyn Child + Send + Sync>,
  shared: &Shared,
  reader_finished: &mpsc::Receiver<()>,
) {
  let code = match child.wait() {
    Ok(status) => Some(status.exit_code()),
    Err(error) => {
      tracing::warn!(%error, "could not wait for a terminal's shell");
      None
    }
  };
  shared.reaped.store(true, Ordering::Release);
  // Closing the pseudoterminal ends the output. On Windows, the reader sees
  // the end only once the pseudoconsole is closed.
  let master = shared
    .master
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .take();
  drop(master);
  // Let the reader pass on the last of the output before reporting the exit.
  let _ = reader_finished.recv_timeout(EXIT_GRACE);
  shared.exited(code);
}

/// Kills the shell `pid` outright unless it exits within [`KILL_GRACE`] of
/// its hangup. Once it's been waited for, the id may be another process's,
/// so it's checked for that right before.
#[cfg(unix)]
fn kill_if_still_running(shared: &Shared, pid: u32) {
  use rustix::process::{Pid, Signal, kill_process};
  use std::time::Instant;

  let deadline = Instant::now() + KILL_GRACE;
  while Instant::now() < deadline {
    if shared.reaped.load(Ordering::Acquire) {
      return;
    }
    thread::sleep(Duration::from_millis(50));
  }
  let Some(process) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
    return;
  };
  if !shared.reaped.load(Ordering::Acquire) {
    tracing::debug!(pid, "a terminal's shell ignored its hangup");
    if let Err(error) = kill_process(process, Signal::KILL) {
      tracing::debug!(%error, "could not kill a terminal's shell");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reports_the_windows_build_only_on_windows() {
    assert_eq!(windows_build().is_some(), cfg!(windows));
    if let Some(build) = windows_build() {
      // Every Windows with ConPTY is build 17763 (1809) or later.
      assert!(build >= 17763, "{build}");
    }
  }

  #[test]
  fn clamps_sizes() {
    assert_eq!(
      TerminalSize::clamped(80, 24),
      TerminalSize { cols: 80, rows: 24 }
    );
    assert_eq!(
      TerminalSize::clamped(0, 0),
      TerminalSize { cols: 2, rows: 1 }
    );
    assert_eq!(
      TerminalSize::clamped(u32::MAX, 100_000),
      TerminalSize {
        cols: TerminalSize::MAX_COLS,
        rows: TerminalSize::MAX_ROWS
      }
    );
  }
}
