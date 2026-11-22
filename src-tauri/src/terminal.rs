//! Integrated terminals: the commands over `flareite-terminal`.
//!
//! Each terminal sends its output through a `Channel` given when it's
//! created, or when the frontend attaches again after the window reloads
//! (the output kept so far is replayed first). Terminals outlive the webview;
//! they end when the frontend kills them or the app exits.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use flareite_terminal::profiles::{self, Profile};
use flareite_terminal::{
  Sink, Terminal, TerminalError, TerminalEvent, TerminalOptions, TerminalSize,
};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use crate::error::CommandError;
use crate::settings::SettingsState;
use crate::workspace::WorkspaceState;

/// The most terminals open at once.
const MAX_TERMINALS: usize = 32;
/// How long detected profiles are reused before looking again (shells get
/// installed while Flareite runs).
const PROFILES_FRESH_FOR: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
  pub id: u64,
  /// The profile's name, such as `PowerShell` or `Ubuntu (WSL)`.
  pub title: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
  tag = "type",
  rename_all = "camelCase",
  rename_all_fields = "camelCase"
)]
pub enum TerminalMessage {
  Output {
    data: String,
  },
  /// Output from before this channel was attached.
  Replay {
    data: String,
  },
  /// Always the last message.
  Exited {
    code: Option<u32>,
  },
}

struct Open {
  terminal: Arc<Terminal>,
  title: String,
}

#[derive(Default)]
struct Terminals {
  next_id: u64,
  open: BTreeMap<u64, Open>,
}

#[derive(Default)]
pub struct TerminalState {
  terminals: Mutex<Terminals>,
  /// The installed shells, when they were last looked for.
  detected: Mutex<Option<(Instant, Vec<Profile>)>>,
}

impl TerminalState {
  fn lock(&self) -> MutexGuard<'_, Terminals> {
    self
      .terminals
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
  }

  /// The installed shells, looked for again when the last look is stale.
  /// Blocks (it may run `wsl.exe`), so call it off the IPC thread.
  pub(crate) fn detected_profiles(&self) -> Vec<Profile> {
    let mut detected = self.detected.lock().unwrap_or_else(PoisonError::into_inner);
    match detected.as_ref() {
      Some((at, profiles)) if at.elapsed() < PROFILES_FRESH_FOR => profiles.clone(),
      _ => {
        let profiles = profiles::detect();
        *detected = Some((Instant::now(), profiles.clone()));
        profiles
      }
    }
  }

  fn get(&self, id: u64) -> Result<Arc<Terminal>, CommandError> {
    self
      .lock()
      .open
      .get(&id)
      .map(|open| open.terminal.clone())
      .ok_or_else(|| CommandError::new("unknownTerminal", "That terminal is no longer open."))
  }

  /// Kills every terminal's shell.
  pub fn shutdown(&self) {
    let open = std::mem::take(&mut self.lock().open);
    if !open.is_empty() {
      tracing::info!(count = open.len(), "ending terminals");
    }
  }
}

fn sink(channel: Channel<TerminalMessage>) -> Sink {
  Box::new(move |event| {
    let message = match event {
      TerminalEvent::Output(data) => TerminalMessage::Output { data },
      TerminalEvent::Replay(data) => TerminalMessage::Replay { data },
      TerminalEvent::Exited(code) => TerminalMessage::Exited { code },
    };
    if let Err(error) = channel.send(message) {
      tracing::debug!(%error, "terminal output not delivered");
    }
  })
}

impl From<TerminalError> for CommandError {
  fn from(error: TerminalError) -> Self {
    let code = match error {
      TerminalError::Spawn { .. } => "terminalSpawn",
      TerminalError::InputFull => "terminalBusy",
      TerminalError::InputTooLarge(_) => "tooLarge",
      TerminalError::Exited => "terminalExited",
      TerminalError::Pty(_) | TerminalError::Resize(_) => "terminal",
    };
    tracing::warn!(%error, code, "terminal command failed");
    Self::new(code, error.to_string())
  }
}

/// Variables that tell programs what the terminal supports (xterm.js, in
/// the frontend) and who runs it.
fn environment(app: &AppHandle) -> Vec<(String, String)> {
  let mut env = vec![
    ("COLORTERM".to_owned(), "truecolor".to_owned()),
    ("TERM_PROGRAM".to_owned(), "flareite".to_owned()),
    (
      "TERM_PROGRAM_VERSION".to_owned(),
      app.package_info().version.to_string(),
    ),
  ];
  // Windows programs don't read TERM, and ConPTY translates their console
  // calls; Unix programs need it.
  if cfg!(not(windows)) {
    env.push(("TERM".to_owned(), "xterm-256color".to_owned()));
  }
  env
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInfo {
  pub name: String,
  /// Whether new terminals use it unless another is chosen.
  pub default: bool,
}

/// The installed shells and configured profiles, the default marked.
#[tauri::command]
pub async fn terminal_profiles(app: AppHandle) -> Result<Vec<ProfileInfo>, CommandError> {
  let resolved = tauri::async_runtime::spawn_blocking(move || {
    let detected = app.state::<TerminalState>().detected_profiles();
    app
      .state::<SettingsState>()
      .get()
      .terminal_profiles(detected)
  })
  .await
  .map_err(CommandError::internal)?;
  Ok(
    resolved
      .profiles
      .into_iter()
      .map(|profile| ProfileInfo {
        default: resolved.default.as_deref() == Some(profile.name.as_str()),
        name: profile.name,
      })
      .collect(),
  )
}

/// Starts a terminal with a profile (the default if `None`) in the
/// workspace folder (or the home folder), and sends its output to `channel`.
#[tauri::command]
pub async fn terminal_create(
  app: AppHandle,
  cols: u32,
  rows: u32,
  profile: Option<String>,
  channel: Channel<TerminalMessage>,
) -> Result<TerminalInfo, CommandError> {
  let state = app.state::<TerminalState>();
  if state.lock().open.len() >= MAX_TERMINALS {
    return Err(too_many());
  }
  let cwd = app
    .state::<WorkspaceState>()
    .root()
    .filter(|root| root.is_dir())
    .or_else(|| app.path().home_dir().ok());
  let mut env = environment(&app);
  let size = TerminalSize::clamped(cols, rows);
  // Finding shells and starting a process can take a while; keep them off
  // the IPC thread.
  let task_app = app.clone();
  let (title, terminal) = tauri::async_runtime::spawn_blocking(move || {
    let detected = task_app.state::<TerminalState>().detected_profiles();
    let resolved = task_app
      .state::<SettingsState>()
      .get()
      .terminal_profiles(detected);
    let wanted = profile.or(resolved.default);
    let profile = resolved
      .profiles
      .into_iter()
      .find(|candidate| Some(&candidate.name) == wanted.as_ref())
      .ok_or_else(|| match &wanted {
        Some(name) => CommandError::new(
          "unknownProfile",
          format!("There's no terminal profile named \"{name}\"."),
        ),
        None => CommandError::new("noProfile", "No shell was found to start a terminal with."),
      })?;
    env.extend(profile.env);
    let options = TerminalOptions {
      shell: profile.shell,
      cwd,
      env,
      size,
    };
    Ok::<_, CommandError>((profile.name, Terminal::spawn(options)?))
  })
  .await
  .map_err(CommandError::internal)??;
  terminal.attach(sink(channel));
  let id = state.add(terminal, title.clone())?;
  Ok(TerminalInfo { id, title })
}

fn too_many() -> CommandError {
  CommandError::new(
    "tooManyTerminals",
    format!("At most {MAX_TERMINALS} terminals can be open. Close one first."),
  )
}

impl TerminalState {
  /// Keeps a started terminal, returning its id.
  fn add(&self, terminal: Terminal, title: String) -> Result<u64, CommandError> {
    let mut terminals = self.lock();
    if terminals.open.len() >= MAX_TERMINALS {
      return Err(too_many());
    }
    terminals.next_id += 1;
    let id = terminals.next_id;
    tracing::info!(id, shell = %title, pid = ?terminal.pid(), "terminal started");
    terminals.open.insert(
      id,
      Open {
        terminal: Arc::new(terminal),
        title,
      },
    );
    Ok(id)
  }
}

/// Starts a terminal running an extension's program (`terminals::run`),
/// which the extension host checked, returning its id and title. Its output
/// is kept until the workbench attaches (`AppEvent::TerminalOpened`).
pub fn run_program(
  app: &AppHandle,
  request: flareite_extension_host::TerminalRequest,
) -> Result<(u64, String), CommandError> {
  let state = app.state::<TerminalState>();
  if state.lock().open.len() >= MAX_TERMINALS {
    return Err(too_many());
  }
  let mut env = environment(app);
  env.extend(request.env);
  let terminal = Terminal::spawn(TerminalOptions {
    shell: flareite_terminal::shell::Shell {
      program: request.program.to_string_lossy().into_owned(),
      args: request.args,
    },
    cwd: Some(request.cwd),
    env,
    size: TerminalSize::clamped(80, 24),
  })?;
  let id = state.add(terminal, request.title.clone())?;
  Ok((id, request.title))
}

/// What the emulator needs to know about the platform's pseudoterminals.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPlatform {
  /// The Windows build: ConPTY's behavior on resize depends on it.
  pub windows_build: Option<u32>,
}

#[tauri::command]
pub fn terminal_platform() -> TerminalPlatform {
  TerminalPlatform {
    windows_build: flareite_terminal::windows_build(),
  }
}

/// The open terminals, for a reloaded window to attach to.
#[tauri::command]
pub fn terminal_list(state: State<'_, TerminalState>) -> Vec<TerminalInfo> {
  state
    .lock()
    .open
    .iter()
    .map(|(id, open)| TerminalInfo {
      id: *id,
      title: open.title.clone(),
    })
    .collect()
}

/// Sends a terminal's output to `channel` from now on, starting with the
/// output kept so far.
#[tauri::command]
pub fn terminal_attach(
  state: State<'_, TerminalState>,
  id: u64,
  channel: Channel<TerminalMessage>,
) -> Result<(), CommandError> {
  state.get(id)?.attach(sink(channel));
  Ok(())
}

/// Input for the shell. Input after it has exited is dropped.
#[tauri::command]
pub fn terminal_write(
  state: State<'_, TerminalState>,
  id: u64,
  data: String,
) -> Result<(), CommandError> {
  match state.get(id)?.write(data.as_bytes()) {
    Ok(()) | Err(TerminalError::Exited) => Ok(()),
    Err(error) => Err(error.into()),
  }
}

#[tauri::command]
pub fn terminal_resize(
  state: State<'_, TerminalState>,
  id: u64,
  cols: u32,
  rows: u32,
) -> Result<(), CommandError> {
  match state.get(id)?.resize(TerminalSize::clamped(cols, rows)) {
    Ok(()) | Err(TerminalError::Exited) => Ok(()),
    Err(error) => Err(error.into()),
  }
}

/// The frontend has drawn `units` of a terminal's output (UTF-16 code
/// units); see `flareite_terminal::flow`.
#[tauri::command]
pub fn terminal_acknowledge(state: State<'_, TerminalState>, id: u64, units: usize) {
  if let Ok(terminal) = state.get(id) {
    terminal.acknowledge(units);
  }
}

/// Ends a terminal's shell and forgets the terminal.
#[tauri::command]
pub fn terminal_kill(state: State<'_, TerminalState>, id: u64) {
  if state.lock().open.remove(&id).is_some() {
    tracing::info!(id, "terminal closed");
  }
}
