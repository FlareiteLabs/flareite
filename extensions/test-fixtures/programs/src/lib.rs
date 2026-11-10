//! A test fixture for the API 1.6 interfaces: it runs the Rust compiler,
//! watches paths, marks files in the explorer, and reports the open folder.
//! And for API 1.17's: it streams the compiler's output, downloads files,
//! keeps secrets, and asks for passwords.
//! The host's integration tests run its commands; outcomes are reported
//! with `show_message`.

use std::sync::Mutex;

use flareite_extension_api::explorer::{self, FileMark};
use flareite_extension_api::processes::{self, ProcessOptions, ProcessOutput};
use flareite_extension_api::prompts::{self, AnswerValue, InputOptions};
use flareite_extension_api::types::{
  Activation, CommandContext, DocumentChange, DocumentId, DocumentInfo, MessageLevel,
};
use flareite_extension_api::{Guest, export_extension, host, workbench, workspace};
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

/// The lines a streamed program wrote so far.
static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Programs;

impl Guest for Programs {
  fn activate(_: Activation) -> Result<(), String> {
    Ok(())
  }

  fn deactivate() {}

  fn run_command(command: String, _: CommandContext) -> Result<(), String> {
    let argument = workbench::command_argument().unwrap_or_default();
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
        let result = processes::spawn(
          "rustc",
          &["--version".into()],
          &options(),
          "test.programs.finished",
        );
        report(format!("spawn: {}", result.is_ok()));
      }
      "finished" => {
        let finished = processes::finished().ok_or("not run for a program")?;
        report(match finished.outcome {
          Ok(output) => format!("finished: {}", summary(&output)),
          Err(error) => format!("finished: {error:?}"),
        });
      }
      "watch" => {
        let result = workspace::watch(&argument, "test.programs.changed");
        report(format!(
          "watch: {}",
          result.map(|_| "ok").unwrap_or("failed")
        ));
      }
      "changed" => {
        let changes = workspace::changes().ok_or("not run for changes")?;
        report(format!("changed: {}", changes.paths.join(";")));
      }
      "mark" => {
        let result = explorer::set_file_marks(&[mark(argument, "modified")]);
        report(format!("mark: {result:?}"));
      }
      "badMark" => {
        let result = explorer::set_file_marks(&[mark(argument, "nope")]);
        report(format!("badMark: {}", result.is_err()));
      }
      "folder" => {
        let folder = workspace::folder();
        report(format!("folder: {:?}", folder.map(|folder| folder.trusted)));
      }
      "stream" => {
        LINES.lock().map_err(|e| e.to_string())?.clear();
        let result = processes::spawn_streaming(
          "rustc",
          &["--version".into(), "--verbose".into()],
          &options(),
          "test.programs.streamed",
          "test.programs.lines",
        );
        report(format!("stream: {}", result.is_ok()));
      }
      "lines" => {
        let lines = processes::lines().ok_or("not run for lines")?;
        LINES
          .lock()
          .map_err(|e| e.to_string())?
          .extend(lines.lines.into_iter().map(|line| line.text));
      }
      "streamed" => {
        let finished = processes::finished().ok_or("not run for a program")?;
        let lines = LINES.lock().map_err(|e| e.to_string())?;
        // The output was handed over before the end: nothing is left in it.
        report(match finished.outcome {
          Ok(output) => format!(
            "streamed: {:?} {} {}",
            output.exit_code,
            output.stdout.is_empty(),
            lines
              .iter()
              .map(|line| line.split_whitespace().next().unwrap_or_default())
              .collect::<Vec<_>>()
              .join(","),
          ),
          Err(error) => format!("streamed: {error:?}"),
        });
      }
      "download" => {
        let result = downloads::download(&argument, "test.programs.downloaded");
        report(format!("download: {:?}", result.map(|_| ())));
      }
      "downloaded" => {
        let file = downloads::downloaded().ok_or("not run for a download")?;
        report(match file.outcome {
          Ok(bytes) => format!(
            "downloaded: {} {}",
            file.address,
            String::from_utf8_lossy(&bytes)
          ),
          Err(error) => format!("downloaded: {} failed: {error}", file.address),
        });
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
        let result = prompts::input_password(
          &InputOptions {
            title: "API key".into(),
            value: String::new(),
            placeholder: None,
          },
          "test.programs.answered",
        );
        report(format!("password: {}", result.is_ok()));
      }
      "answered" => {
        let answer = prompts::answer().ok_or("not run for an answer")?;
        let kind = match answer.value {
          AnswerValue::Text(text) => format!("text of {} characters", text.chars().count()),
          other => format!("{other:?}"),
        };
        report(format!("answered: {kind}"));
      }
      other => return Err(format!("unknown command {other}")),
    }
    Ok(())
  }

  fn document_opened(_: DocumentInfo) {}
  fn document_changed(_: DocumentChange) {}
  fn document_closed(_: DocumentId) {}
}

export_extension!(Programs);
