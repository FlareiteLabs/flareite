//! Real shells in real pseudoterminals: `cmd.exe` on Windows, `/bin/sh`
//! elsewhere.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use flareite_terminal::flow::units;
use flareite_terminal::{
  Shell, Terminal, TerminalError, TerminalEvent, TerminalOptions, TerminalSize,
};

const TIMEOUT: Duration = Duration::from_secs(20);
const MARKER: &str = "flareite-terminal-marker";
/// ConPTY asks where the cursor is when it starts, and waits for the answer.
const CURSOR_QUERY: &str = "\x1b[6n";

/// Runs `script` and exits.
fn script(script: &str) -> Shell {
  if cfg!(windows) {
    Shell {
      program: "cmd.exe".into(),
      args: vec!["/d".into(), "/c".into(), script.into()],
    }
  } else {
    Shell {
      program: "/bin/sh".into(),
      args: vec!["-c".into(), script.into()],
    }
  }
}

fn interactive() -> Shell {
  if cfg!(windows) {
    Shell {
      program: "cmd.exe".into(),
      args: vec!["/d".into()],
    }
  } else {
    Shell {
      program: "/bin/sh".into(),
      args: Vec::new(),
    }
  }
}

/// Prints the marker from the environment, so it doesn't appear in the
/// echoed command.
const PRINT_MARKER: &str = if cfg!(windows) {
  "echo %FLAREITE_MARKER%"
} else {
  "echo $FLAREITE_MARKER"
};

struct Harness {
  terminal: Terminal,
  events: mpsc::Receiver<TerminalEvent>,
  output: String,
  answered: usize,
}

fn spawn(shell: Shell) -> Result<Terminal, TerminalError> {
  Terminal::spawn(TerminalOptions {
    shell,
    cwd: Some(std::env::temp_dir()),
    env: vec![("FLAREITE_MARKER".into(), MARKER.into())],
    size: TerminalSize { cols: 80, rows: 24 },
  })
}

fn start(shell: Shell) -> Harness {
  let terminal = spawn(shell).expect("starts");
  let events = attach(&terminal);
  Harness {
    terminal,
    events,
    output: String::new(),
    answered: 0,
  }
}

fn attach(terminal: &Terminal) -> mpsc::Receiver<TerminalEvent> {
  let (sender, events) = mpsc::channel();
  terminal.attach(Box::new(move |event| {
    let _ = sender.send(event);
  }));
  events
}

impl Harness {
  /// The next event, after answering cursor queries and acknowledging output
  /// as a frontend would.
  fn next(&mut self, deadline: Instant) -> TerminalEvent {
    let wait = deadline.saturating_duration_since(Instant::now());
    let event = self.events.recv_timeout(wait).unwrap_or_else(|_| {
      panic!("timed out; output so far: {:?}", self.output);
    });
    if let TerminalEvent::Output(text) = &event {
      self.output.push_str(text);
      self.terminal.acknowledge(units(text));
      let queries = self.output.matches(CURSOR_QUERY).count();
      for _ in self.answered..queries {
        self.terminal.write(b"\x1b[1;1R").expect("answers");
      }
      self.answered = queries;
    }
    event
  }

  fn wait_for_output(&mut self, needle: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while !self.output.contains(needle) {
      if let TerminalEvent::Exited(code) = self.next(deadline) {
        panic!(
          "exited ({code:?}) before printing {needle:?}: {:?}",
          self.output
        );
      }
    }
  }

  fn wait_for_exit(&mut self) -> Option<u32> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
      if let TerminalEvent::Exited(code) = self.next(deadline) {
        return code;
      }
    }
  }
}

#[test]
fn runs_a_command_and_reports_its_exit_code() {
  let mut harness = start(script(&format!("{PRINT_MARKER}&& exit 3")));
  let code = harness.wait_for_exit();
  assert!(harness.output.contains(MARKER), "{:?}", harness.output);
  assert_eq!(code, Some(3));
  assert!(harness.terminal.has_exited());
  assert!(matches!(
    harness.terminal.write(b"more"),
    Err(TerminalError::Exited)
  ));
  assert!(harness.events.try_recv().is_err(), "nothing after the exit");
}

#[test]
fn sends_input_resizes_and_can_be_killed() {
  let mut harness = start(interactive());
  harness
    .terminal
    .write(format!("{PRINT_MARKER}\r").as_bytes())
    .expect("writes");
  harness.wait_for_output(MARKER);
  harness
    .terminal
    .resize(TerminalSize::clamped(120, 40))
    .expect("resizes");

  harness.terminal.kill();
  harness.wait_for_exit();
  assert!(matches!(
    harness.terminal.resize(TerminalSize::clamped(80, 24)),
    Err(TerminalError::Exited)
  ));
}

/// A hangup ends most shells; one that ignores it is killed outright.
#[cfg(unix)]
#[test]
fn a_shell_ignoring_the_hangup_is_killed_anyway() {
  let mut harness = start(interactive());
  harness
    .terminal
    .write(format!("trap '' HUP; {PRINT_MARKER}\r").as_bytes())
    .expect("writes");
  harness.wait_for_output(MARKER);

  harness.terminal.kill();
  harness.wait_for_exit();
  assert!(harness.terminal.has_exited());
}

#[test]
fn replays_output_and_the_exit_to_a_new_sink() {
  let mut harness = start(script(PRINT_MARKER));
  let code = harness.wait_for_exit();

  let events = attach(&harness.terminal);
  let replayed: Vec<TerminalEvent> = events.try_iter().collect();
  match replayed.as_slice() {
    [
      TerminalEvent::Replay(text),
      TerminalEvent::Exited(replayed_code),
    ] => {
      assert!(text.contains(MARKER), "{text:?}");
      assert_eq!(*replayed_code, code);
    }
    other => panic!("unexpected replay: {other:?}"),
  }
}

#[test]
fn reports_a_shell_that_cannot_start() {
  let shell = Shell {
    program: "flareite-no-such-shell".into(),
    args: Vec::new(),
  };
  match spawn(shell) {
    Err(TerminalError::Spawn { program, .. }) => assert_eq!(program, "flareite-no-such-shell"),
    Err(other) => panic!("unexpected error: {other}"),
    Ok(_) => panic!("started a shell that doesn't exist"),
  }
}

#[test]
fn rejects_oversized_input() {
  let harness = start(interactive());
  let input = vec![b'x'; flareite_terminal::MAX_INPUT_BYTES + 1];
  assert!(matches!(
    harness.terminal.write(&input),
    Err(TerminalError::InputTooLarge(_))
  ));
}

#[test]
fn detects_this_computers_shells() {
  let profiles = flareite_terminal::profiles::detect();
  for profile in &profiles {
    println!(
      "{}: {} {:?}",
      profile.name, profile.shell.program, profile.shell.args
    );
  }
  assert!(!profiles.is_empty(), "no shells found");
  let default =
    flareite_terminal::profiles::default_profile(&profiles, &flareite_terminal::profiles::System);
  assert!(default.is_some());
  // Every profile's program exists (WSL's is wsl.exe).
  for profile in &profiles {
    assert!(
      std::path::Path::new(&profile.shell.program).is_file(),
      "{profile:?}"
    );
  }
}
