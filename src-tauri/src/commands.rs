//! Tauri commands: the IPC boundary between the workbench frontend and the
//! backend.
//!
//! Payloads are plain serializable structs with camelCase fields, so the
//! frontend depends only on this documented shape and never on internal Rust
//! types. Each command validates its own input.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use flareite_extension_host::preview::{LinkTarget, link_target};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::error::CommandError;

/// Largest frontend log message accepted, in bytes. Longer messages are
/// truncated at a character boundary.
pub const MAX_FRONTEND_LOG_BYTES: usize = 8 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
  pub name: String,
  pub version: String,
  pub os: &'static str,
  pub arch: &'static str,
  pub tauri_version: &'static str,
}

/// Opens a web address with the system: `http:`, `https:`, and `mailto:`
/// addresses only. Links in previews use it.
#[tauri::command]
pub fn open_external(url: String) -> Result<(), CommandError> {
  if !matches!(link_target(&url, None), LinkTarget::Web { .. }) {
    return Err(CommandError::new(
      "unsupportedLink",
      "Only web and mail links can be opened.",
    ));
  }
  tracing::info!("opening a link with the system");
  open::that_detached(&url).map_err(|error| CommandError::new("openLink", error.to_string()))
}

/// Most paths `path_kinds` checks at once.
const MAX_PATH_CHECKS: usize = 200;

/// What each path is: `file`, `folder`, or `None` if nothing is there (or
/// it isn't an absolute path). For links in the terminal, which are
/// underlined only when they lead somewhere.
#[tauri::command]
pub async fn path_kinds(paths: Vec<String>) -> Result<Vec<Option<&'static str>>, CommandError> {
  if paths.len() > MAX_PATH_CHECKS {
    return Err(CommandError::new(
      "tooLarge",
      format!("at most {MAX_PATH_CHECKS} paths at once"),
    ));
  }
  tauri::async_runtime::spawn_blocking(move || {
    paths
      .iter()
      .map(|path| {
        let path = std::path::Path::new(path);
        if !path.is_absolute() {
          return None;
        }
        let metadata = std::fs::metadata(path).ok()?;
        Some(if metadata.is_dir() { "folder" } else { "file" })
      })
      .collect()
  })
  .await
  .map_err(CommandError::internal)
}

/// Opens a folder in the system's file manager: a folder link's default.
#[tauri::command]
pub fn open_folder_external(path: String) -> Result<(), CommandError> {
  let folder = std::path::Path::new(&path);
  if !folder.is_absolute() || !folder.is_dir() {
    return Err(CommandError::new(
      "notFound",
      format!("{path} isn't a folder."),
    ));
  }
  open::that_detached(folder).map_err(|error| CommandError::new("openLink", error.to_string()))
}

/// Largest text written to the clipboard, in bytes.
const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// Puts text on the system clipboard, e.g. a file's path from the explorer.
#[tauri::command]
pub fn clipboard_write_text(app: AppHandle, text: String) -> Result<(), CommandError> {
  if text.len() > MAX_CLIPBOARD_BYTES {
    return Err(CommandError::new(
      "tooLarge",
      "That's too much text to copy.",
    ));
  }
  app
    .clipboard()
    .write_text(text)
    .map_err(|error| CommandError::new("clipboard", error.to_string()))
}

#[tauri::command]
pub fn app_info(app: AppHandle) -> AppInfo {
  let package = app.package_info();
  AppInfo {
    name: package.name.clone(),
    // A prerelease's own version (`0.2.0-alpha.1`), not its manifests'.
    version: crate::app_update::display_version(&app),
    os: std::env::consts::OS,
    arch: std::env::consts::ARCH,
    tauri_version: tauri::VERSION,
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontendLogLevel {
  Error,
  Warn,
  Info,
  Debug,
  Trace,
}

/// Records a frontend log message under the `frontend` target.
#[tauri::command]
pub fn frontend_log(level: FrontendLogLevel, message: String) {
  let (message, truncated) = truncate_at_char_boundary(&message, MAX_FRONTEND_LOG_BYTES);
  match level {
    FrontendLogLevel::Error => tracing::error!(target: "frontend", truncated, "{message}"),
    FrontendLogLevel::Warn => tracing::warn!(target: "frontend", truncated, "{message}"),
    FrontendLogLevel::Info => tracing::info!(target: "frontend", truncated, "{message}"),
    FrontendLogLevel::Debug => tracing::debug!(target: "frontend", truncated, "{message}"),
    FrontendLogLevel::Trace => tracing::trace!(target: "frontend", truncated, "{message}"),
  }
}

/// Startup timing shared between `run` and the `frontend_ready` command.
pub struct StartupState {
  launched_at: Instant,
  frontend_ready: AtomicBool,
  /// Milliseconds from launch to the first `frontend_ready`, once known.
  ready_ms: AtomicU64,
}

impl StartupState {
  pub fn new(launched_at: Instant) -> Self {
    Self {
      launched_at,
      frontend_ready: AtomicBool::new(false),
      ready_ms: AtomicU64::new(0),
    }
  }

  /// Time from launch to the first frontend render, if it has happened.
  pub fn ready_ms(&self) -> Option<u64> {
    self
      .frontend_ready
      .load(Ordering::Relaxed)
      .then(|| self.ready_ms.load(Ordering::Relaxed))
  }
}

/// Called by the frontend after its first render. The first call logs the
/// time from backend launch to an interactive workbench; later calls (webview
/// reloads) are logged at debug level.
#[tauri::command]
pub fn frontend_ready(startup: State<'_, StartupState>) {
  let elapsed_ms = millis(startup.launched_at.elapsed());
  if startup.frontend_ready.load(Ordering::Relaxed) {
    tracing::debug!(elapsed_ms, "frontend reloaded");
  } else {
    startup.ready_ms.store(elapsed_ms, Ordering::Relaxed);
    startup.frontend_ready.store(true, Ordering::Relaxed);
    tracing::info!(elapsed_ms, "frontend ready");
  }
}

pub fn millis(duration: Duration) -> u64 {
  u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Returns the longest prefix of `text` that fits in `max_bytes` without
/// splitting a UTF-8 sequence, and whether anything was removed.
fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> (&str, bool) {
  if text.len() <= max_bytes {
    return (text, false);
  }
  let mut end = max_bytes;
  while !text.is_char_boundary(end) {
    end -= 1;
  }
  (&text[..end], true)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn short_text_is_not_truncated() {
    assert_eq!(truncate_at_char_boundary("hello", 5), ("hello", false));
    assert_eq!(truncate_at_char_boundary("", 0), ("", false));
  }

  #[test]
  fn long_ascii_is_truncated_to_limit() {
    assert_eq!(truncate_at_char_boundary("hello world", 5), ("hello", true));
  }

  #[test]
  fn truncation_never_splits_multibyte_characters() {
    // "é" is 2 bytes and "🦀" is 4 bytes in UTF-8.
    assert_eq!(truncate_at_char_boundary("aé", 2), ("a", true));
    assert_eq!(truncate_at_char_boundary("🦀🦀", 5), ("🦀", true));
    assert_eq!(truncate_at_char_boundary("🦀", 3), ("", true));
  }

  #[test]
  fn log_levels_deserialize_from_lowercase_names() {
    let level: FrontendLogLevel = serde_json::from_str("\"warn\"").expect("valid level");
    assert_eq!(level, FrontendLogLevel::Warn);
  }

  #[test]
  fn unknown_log_levels_are_rejected() {
    assert!(serde_json::from_str::<FrontendLogLevel>("\"fatal\"").is_err());
    assert!(serde_json::from_str::<FrontendLogLevel>("\"WARN\"").is_err());
  }

  #[test]
  fn app_info_serializes_with_camel_case_fields() {
    let info = AppInfo {
      name: "Flareite".into(),
      version: "0.1.0".into(),
      os: "windows",
      arch: "x86_64",
      tauri_version: "2.0.0",
    };
    let json = serde_json::to_value(&info).expect("serializable");
    assert_eq!(json["tauriVersion"], "2.0.0");
    assert_eq!(json["name"], "Flareite");
  }
}
