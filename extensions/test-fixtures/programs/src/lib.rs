//! A test fixture: it runs the Rust compiler, watches paths, marks files in
//! the explorer, reports the open folder, streams the compiler's output,
//! downloads files, keeps secrets, and asks for passwords. The host's
//! integration tests run its commands; outcomes, including the events they
//! lead to, are reported with `show_message`.

use std::collections::HashSet;

use flareite_extension_api::explorer::{self, FileMark};
use flareite_extension_api::processes::{self, ProcessOptions, ProcessOutput};
use flareite_extension_api::prompts::{self, AnswerValue, InputOptions};
use flareite_extension_api::events::Event;
use flareite_extension_api::types::{Activation, CommandContext, MessageLevel};
use flareite_extension_api::{Extension, host, register, workspace};
use flareite_extension_api::{downloads, secrets};

fn report(message: impl AsRef<str>) {
  host::show_message(MessageLevel::Info, message.as_ref());
}

fn options() -> ProcessOptions {
  ProcessOptions {
    cwd: None,
    env: Vec::new(),
    stdin: None,
    timeout_ms: None,
  }
}

/// The exit code and the first word of standard output.
fn summary(output: &ProcessOutput) -> String {
  let stdout = String::from_utf8_lossy(&output.stdout);
  let word = stdout.split_whitespace().next().unwrap_or_default();
  format!("{:?} {word}", output.exit_code)
}

fn mark(path: String, color: &str) -> FileMark {
  FileMark {
    path,
    badge: Some("M".into()),
    color: Some(color.into()),
    tooltip: Some("Modified".into()),
    faded: false,
    inherit: false,
    propagate: true,
    priority: 3,
  }
}

#[derive(Default)]
struct Programs {
  /// Programs started with `spawn-streaming`.
  streaming: HashSet<u64>,
  /// The lines a streamed program wrote so far.
  lines: Vec<String>,
}

impl Programs {
  fn finished(&mut self, process: u64, outcome: Result<ProcessOutput, processes::ProcessError>) {
    if !self.streaming.remove(&process) {
      report(match outcome {
        Ok(output) => format!("finished: {}", summary(&output)),
        Err(error) => format!("finished: {error:?}"),
      });
      return;
    }
    // The output was handed over before the end: nothing is left in it.
    report(match outcome {
      Ok(output) => format!(
        "streamed: {:?} {} {}",
        output.exit_code,
        output.stdout.is_empty(),
        self
          .lines
          .iter()
          .map(|line| line.split_whitespace().next().unwrap_or_default())
          .collect::<Vec<_>>()
          .join(","),
      ),
      Err(error) => format!("streamed: {error:?}"),
    });
  }
}

impl Extension for Programs {
  fn activate(_: Activation) -> Result<Self, String> {
    Ok(Self::default())
  }

  fn run_command(&mut self, command: &str, context: CommandContext) -> Result<(), String> {
    let argument = context.argument.unwrap_or_default();
    match command.trim_start_matches("test.programs.") {
      "run" => {
        let result = processes::run("rustc", &["--version".into()], &options());
        report(match result {
          Ok(output) => format!("run: {}", summary(&output)),
          Err(error) => format!("run: {error:?}"),
        });
      }
      "unknown" => {
        let result = processes::run("sh", &[], &options());
        report(format!("unknown: {:?}", result.err()));
      }
      "spawn" => {
        let result = processes::spawn("rustc", &["--version".into()], &options());
        report(format!("spawn: {}", result.is_ok()));
      }
      "watch" => {
        let result = workspace::watch(&argument);
        report(format!(
          "watch: {}",
          result.map(|_| "ok").unwrap_or("failed")
        ));
      }
      "mark" => {
        let result = explorer::set_file_marks(&[mark(argument, "modified")]);
        report(format!("mark: {result:?}"));
      }
      "badMark" => {
        let result = explorer::set_file_marks(&[mark(argument, "nope")]);
        report(format!("badMark: {}", result.is_err()));
      }
      "stream" => {
        self.lines.clear();
        let result = processes::spawn_streaming(
          "rustc",
          &["--version".into(), "--verbose".into()],
          &options(),
        );
        if let Ok(process) = result {
          self.streaming.insert(process);
        }
        report(format!("stream: {}", result.is_ok()));
      }
      "download" => {
        let result = downloads::download(&argument);
        report(format!("download: {:?}", result.map(|_| ())));
      }
      // `set:<key>=<value>`, `remove:<key>`, `get:<key>`, or `keys`.
      "secret" => {
        let message = if let Some(rest) = argument.strip_prefix("set:") {
          let (key, value) = rest.split_once('=').unwrap_or((rest, ""));
          format!("{:?}", secrets::set(key, Some(value)))
        } else if let Some(key) = argument.strip_prefix("remove:") {
          format!("{:?}", secrets::set(key, None))
        } else if let Some(key) = argument.strip_prefix("get:") {
          format!("{:?}", secrets::get(key))
        } else {
          secrets::keys().join(",")
        };
        report(format!("secret: {message}"));
      }
      "password" => {
        let result = prompts::input_password(&InputOptions {
          title: "API key".into(),
          value: String::new(),
          placeholder: None,
        });
        report(format!("password: {}", result.is_ok()));
      }
      other => return Err(format!("unknown command {other}")),
    }
    Ok(())
  }

  fn handle_event(&mut self, event: Event) -> Result<(), String> {
    match event {
      Event::ProcessFinished(finished) => self.finished(finished.process, finished.outcome),
      Event::ProcessOutput(lines) => self
        .lines
        .extend(lines.lines.into_iter().map(|line| line.text)),
      Event::FilesChanged(changes) => report(format!("changed: {}", changes.paths.join(";"))),
      Event::FolderChanged => {
        let folder = workspace::folder();
        report(format!("folder: {:?}", folder.map(|folder| folder.trusted)));
      }
      Event::DownloadFinished(file) => report(match file.outcome {
        Ok(bytes) => format!(
          "downloaded: {} {}",
          file.address,
          String::from_utf8_lossy(&bytes)
        ),
        Err(error) => format!("downloaded: {} failed: {error}", file.address),
      }),
      Event::PromptAnswered(answer) => {
        let kind = match answer.value {
          AnswerValue::Text(text) => format!("text of {} characters", text.chars().count()),
          other => format!("{other:?}"),
        };
        report(format!("answered: {kind}"));
      }
      _ => {}
    }
    Ok(())
  }
}

register!(Programs);
