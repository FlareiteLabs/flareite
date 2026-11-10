//! Workspace Trust: whether the user trusts the files of the folder open in
//! the editor.
//!
//! Programs that work on a folder may run code from it: a language server
//! builds the project (rust-analyzer runs build scripts), and Git runs hooks
//! and programs its configuration names. Until the user trusts a folder,
//! language servers don't start and extensions can't run programs
//! (`processes`); everything else works. The user is asked the first time a
//! folder opens, and can trust a folder's parent, which trusts every folder
//! in it. Decisions are kept in `trusted-folders.json` next to
//! `settings.toml`; `security.workspace_trust = false` trusts every folder.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::error::CommandError;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::lsp::LspState;
use crate::settings::SettingsState;
use crate::workspace::WorkspaceState;

const FILE_NAME: &str = "trusted-folders.json";

/// Whether the user trusts a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Trust {
  Trusted,
  /// The user said they don't trust it.
  Untrusted,
  /// The user hasn't said: it's treated as untrusted until they do.
  Unknown,
}

/// The user's decisions, by folder.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Decisions {
  /// Trusted folders, and everything in them.
  trusted: Vec<PathBuf>,
  /// Folders the user said they don't trust, even inside a trusted one.
  untrusted: Vec<PathBuf>,
}

pub struct TrustState {
  path: Option<PathBuf>,
  decisions: Mutex<Decisions>,
  /// Trusts every folder without asking (benchmarks).
  trust_all: bool,
}

/// Whether `path` is `folder` or inside it; case doesn't matter on Windows.
fn within(path: &Path, folder: &Path) -> bool {
  if cfg!(windows) {
    let (path, folder) = (
      path.to_string_lossy().to_lowercase(),
      folder.to_string_lossy().to_lowercase(),
    );
    Path::new(&path).starts_with(Path::new(&folder))
  } else {
    path.starts_with(folder)
  }
}

fn same(a: &Path, b: &Path) -> bool {
  within(a, b) && within(b, a)
}

impl TrustState {
  /// Loads decisions from `dir`. A missing or unreadable file means no
  /// decisions (unreadable ones are logged), so every folder is asked
  /// about. Without `dir`, decisions last for the session.
  pub fn load(dir: Option<PathBuf>, trust_all: bool) -> Self {
    let path = dir.map(|dir| dir.join(FILE_NAME));
    let decisions = path
      .as_deref()
      .and_then(|path| match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
          .map_err(|error| tracing::warn!(%error, path = %path.display(), "ignoring invalid trusted folders file"))
          .ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
          tracing::warn!(%error, path = %path.display(), "could not read trusted folders");
          None
        }
      })
      .unwrap_or_default();
    Self {
      path,
      decisions: Mutex::new(decisions),
      trust_all,
    }
  }

  fn lock(&self) -> MutexGuard<'_, Decisions> {
    self
      .decisions
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
  }

  /// The user's decision about `folder`. Saying they don't trust a folder
  /// wins over trusting a folder it's in.
  pub fn decision(&self, folder: &Path) -> Trust {
    if self.trust_all {
      return Trust::Trusted;
    }
    let decisions = self.lock();
    if decisions.untrusted.iter().any(|path| same(path, folder)) {
      Trust::Untrusted
    } else if decisions.trusted.iter().any(|path| within(folder, path)) {
      Trust::Trusted
    } else {
      Trust::Unknown
    }
  }

  /// Records that the user trusts `folder` (or, with `parent`, the folder
  /// it's in), or doesn't trust `folder`, and saves the file.
  pub fn decide(&self, folder: &Path, trusted: bool, parent: bool) {
    {
      let mut decisions = self.lock();
      decisions.untrusted.retain(|path| !same(path, folder));
      if trusted {
        let target = if parent {
          folder.parent().unwrap_or(folder)
        } else {
          folder
        };
        // Folders inside it are trusted with it.
        decisions.trusted.retain(|path| !within(path, target));
        decisions.trusted.push(target.to_path_buf());
      } else {
        decisions.trusted.retain(|path| !same(path, folder));
        decisions.untrusted.push(folder.to_path_buf());
      }
      decisions.trusted.sort();
      decisions.untrusted.sort();
    }
    self.save();
  }

  fn save(&self) {
    let Some(path) = &self.path else {
      return;
    };
    let result = serde_json::to_string_pretty(&*self.lock())
      .map_err(std::io::Error::other)
      .and_then(|text| flareite_workspace::write_file_atomically(path, text.as_bytes()));
    if let Err(error) = result {
      tracing::error!(%error, path = %path.display(), "could not save trusted folders");
    }
  }
}

/// Whether the user trusts `folder`, with the setting applied.
pub fn trust_of(app: &AppHandle, folder: &Path) -> Trust {
  if !app.state::<SettingsState>().get().security.workspace_trust {
    return Trust::Trusted;
  }
  app.state::<TrustState>().decision(folder)
}

/// Whether programs may run on the open folder: it's trusted, or none is
/// open.
pub fn workspace_trusted(app: &AppHandle) -> bool {
  app
    .state::<WorkspaceState>()
    .root()
    .is_none_or(|root| trust_of(app, &root) == Trust::Trusted)
}

/// Tells everything that depends on trust that it changed: the frontend,
/// language servers (which start once trusted), and extensions.
pub fn changed(app: &AppHandle) {
  let Some(workspace) = crate::workspace::info(app) else {
    return;
  };
  tracing::info!(root = %workspace.root, trust = ?workspace.trust, "workspace trust changed");
  events::emit(app, AppEvent::WorkspaceTrustChanged { workspace });
  app.state::<LspState>().restart_all(app);
  if let Some(host) = app.state::<ExtensionsState>().host() {
    host.folder_changed();
  }
  // A folder's settings apply only while it's trusted.
  let _ = crate::settings::folder_changed(app, false);
}

/// Trusts the open folder (or, with `parent`, the folder it's in), or
/// records that the user doesn't.
#[tauri::command]
pub fn workspace_set_trust(
  app: AppHandle,
  trust: State<'_, TrustState>,
  trusted: bool,
  parent: bool,
) -> Result<(), CommandError> {
  let root = app
    .state::<WorkspaceState>()
    .root()
    .ok_or_else(|| CommandError::new("noWorkspace", "No folder is open."))?;
  trust.decide(&root, trusted, parent);
  changed(&app);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn decisions_cover_folders_inside_and_survive_a_reload() {
    let dir = tempfile::tempdir().expect("temp dir");
    let trust = TrustState::load(Some(dir.path().to_path_buf()), false);
    let projects = dir.path().join("projects");
    let app = projects.join("app");
    assert_eq!(trust.decision(&app), Trust::Unknown);

    trust.decide(&app, true, true);
    assert_eq!(trust.decision(&app), Trust::Trusted);
    assert_eq!(trust.decision(&projects.join("other")), Trust::Trusted);
    assert_eq!(trust.decision(dir.path()), Trust::Unknown);

    // Not trusting one folder wins over trusting the one it's in.
    trust.decide(&projects.join("other"), false, false);
    assert_eq!(trust.decision(&projects.join("other")), Trust::Untrusted);
    assert_eq!(trust.decision(&app), Trust::Trusted);

    let reloaded = TrustState::load(Some(dir.path().to_path_buf()), false);
    assert_eq!(reloaded.decision(&app), Trust::Trusted);
    assert_eq!(reloaded.decision(&projects.join("other")), Trust::Untrusted);

    // Trusting it again replaces the decision.
    reloaded.decide(&projects.join("other"), true, false);
    assert_eq!(reloaded.decision(&projects.join("other")), Trust::Trusted);
  }

  #[test]
  fn benchmarks_trust_everything() {
    let trust = TrustState::load(None, true);
    assert_eq!(trust.decision(Path::new("/any")), Trust::Trusted);
  }
}
