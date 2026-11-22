//! The host functions of the `terminals` interface (2.1): a program the
//! manifest declares, run in the editor's integrated terminal for the user
//! to see and type to. The program is checked as `processes` checks it
//! (declared, the permission granted, the folder trusted), then the
//! embedder opens the terminal.

use crate::TerminalRequest;
use crate::bindings::flareite::extension::processes::{ProcessError, ProcessOptions};
use crate::bindings::flareite::extension::terminals::{Host, TerminalOptions};
use crate::processes::{CheckedRequest, home_folder};
use crate::runtime::StoreState;

/// Longest terminal title, in characters.
const MAX_TITLE_CHARS: usize = 100;

impl Host for StoreState {
  fn run(
    &mut self,
    program: String,
    args: Vec<String>,
    options: TerminalOptions,
  ) -> Result<(), ProcessError> {
    let title = match options.title {
      Some(title) if !title.trim().is_empty() => title.chars().take(MAX_TITLE_CHARS).collect(),
      _ => program.clone(),
    };
    let checked: CheckedRequest = self
      .api
      .request(
        &program,
        args,
        ProcessOptions {
          cwd: options.cwd,
          env: options.env,
          stdin: None,
          timeout_ms: None,
        },
        false,
        home_folder(),
      )?
      .into();
    let shared = self
      .api
      .shared()
      .map_err(|_| ProcessError::Failed("the extension was stopped".into()))?;
    tracing::info!(extension = %self.api.extension_id(), %program, "running a program in a terminal");
    shared
      .embedder
      .run_in_terminal(TerminalRequest {
        title,
        program: checked.program,
        args: checked.args,
        cwd: checked.cwd,
        env: checked.env,
      })
      .map_err(ProcessError::Failed)
  }
}
