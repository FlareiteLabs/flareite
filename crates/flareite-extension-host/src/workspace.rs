//! The host functions of the `workspace` interface (API 1.6): the open
//! folder and whether it's trusted, and watching files for changes.
//!
//! Watching a path inside the open folder needs `files-read`, and uses the
//! changes the editor already watches the folder for. A path outside it
//! (such as the `.git` folder of a repository the open folder is inside)
//! needs `processes`, whose programs can read it anyway, and gets a watcher
//! of its own.
//!
//! Opening a file with the system's default app (`open-externally`, API
//! 1.17) needs `processes` too, and a trusted folder, as running a program
//! does; and since opening a program runs it, programs and scripts are
//! refused (see [`runs_when_opened`]).

use std::path::Path;

use flareite_workspace::normalize_path;

use crate::Notification;
use crate::bindings::flareite::extension::files::FileError;
use crate::bindings::flareite::extension::workspace::{FileChanges, FolderInfo, Host};
use crate::permissions::Permission;
use crate::runtime::StoreState;
use crate::worker::Payload;

impl Host for StoreState {
  fn folder(&mut self) -> Option<FolderInfo> {
    let folder = self.api.shared().ok()?.embedder.open_folder()?;
    Some(FolderInfo {
      path: folder.root.display().to_string(),
      trusted: folder.trusted,
    })
  }

  fn request_trust(&mut self) {
    if let Ok(shared) = self.api.shared()
      && shared
        .embedder
        .open_folder()
        .is_some_and(|folder| !folder.trusted)
    {
      shared.embedder.notify(Notification::TrustRequested {
        extension: self.api.extension_id().to_owned(),
      });
    }
  }

  fn open_file(&mut self, path: String) -> Result<(), FileError> {
    let given = Path::new(&path);
    if !given.is_absolute() {
      return Err(FileError::Failed(format!(
        "{path:?} is not an absolute path"
      )));
    }
    if given.is_dir() {
      return Err(FileError::WrongKind);
    }
    if !given.is_file() {
      return Err(FileError::NotFound);
    }
    let shared = self
      .api
      .shared()
      .map_err(|_| FileError::Failed("the editor is closing".into()))?;
    shared.embedder.notify(Notification::OpenFile {
      path: normalize_path(given),
    });
    Ok(())
  }

  fn open_externally(&mut self, path: String) -> Result<(), FileError> {
    if !self.api.has(Permission::Processes) {
      return Err(FileError::PermissionDenied("processes".into()));
    }
    let shared = self
      .api
      .shared()
      .map_err(|_| FileError::Failed("the editor is closing".into()))?;
    let folder = shared
      .embedder
      .open_folder()
      .ok_or(FileError::OutsideFolder)?;
    if !folder.trusted {
      return Err(FileError::Failed(
        "files open with other apps only in folders you trust".into(),
      ));
    }
    let given = Path::new(&path);
    if !given.is_absolute() {
      return Err(FileError::Failed(format!(
        "{path:?} is not an absolute path"
      )));
    }
    if given.is_dir() {
      return Err(FileError::WrongKind);
    }
    let (Ok(file), Ok(root)) = (given.canonicalize(), folder.root.canonicalize()) else {
      return Err(FileError::NotFound);
    };
    if !file.starts_with(&root) {
      return Err(FileError::OutsideFolder);
    }
    if runs_when_opened(&file) {
      return Err(FileError::Failed(format!(
        "{} is a program or script, which opening would run",
        given.display()
      )));
    }
    shared
      .embedder
      .open_externally(&normalize_path(given))
      .map_err(FileError::Failed)
  }

  fn open_folder(&mut self, path: String) -> Result<(), FileError> {
    let given = Path::new(&path);
    if !given.is_absolute() {
      return Err(FileError::Failed(format!(
        "{path:?} is not an absolute path"
      )));
    }
    if given.is_file() {
      return Err(FileError::WrongKind);
    }
    if !given.is_dir() {
      return Err(FileError::NotFound);
    }
    let shared = self
      .api
      .shared()
      .map_err(|_| FileError::Failed("the editor is closing".into()))?;
    shared.embedder.notify(Notification::OpenFolder {
      path: normalize_path(given),
    });
    Ok(())
  }

  fn watch(&mut self, path: String, command: String) -> Result<u64, FileError> {
    let api = &self.api;
    if !api.manifest().declares_command(&command) {
      return Err(FileError::Failed(format!(
        "{command:?} is not one of this extension's commands"
      )));
    }
    let shared = api
      .shared()
      .map_err(|_| FileError::Failed("the editor is closing".into()))?;
    let given = Path::new(&path);
    if !given.is_absolute() {
      return Err(FileError::Failed(format!(
        "{path:?} is not an absolute path"
      )));
    }
    if !given.exists() {
      return Err(FileError::NotFound);
    }
    let resolved = normalize_path(given);
    let inside = shared
      .embedder
      .open_folder()
      .is_some_and(|folder| resolved.starts_with(&folder.root));
    let (needed, name) = if inside {
      (Permission::FilesRead, "files-read")
    } else {
      (Permission::Processes, "processes")
    };
    if !api.has(needed) {
      return Err(FileError::PermissionDenied(name.into()));
    }
    shared
      .add_watch(
        api.extension_id(),
        api.generation(),
        resolved,
        !inside,
        command,
      )
      .map_err(|message| {
        if message.starts_with("at most") {
          FileError::TooLarge(message)
        } else {
          FileError::Failed(message)
        }
      })
  }

  fn unwatch(&mut self, watch: u64) {
    if let Ok(shared) = self.api.shared() {
      shared.remove_watch(self.api.extension_id(), watch);
    }
  }

  fn changes(&mut self) -> Option<FileChanges> {
    match &self.api.payload {
      Payload::Changes(changes) => Some(changes.clone()),
      _ => None,
    }
  }
}

/// File types that opening runs: programs, scripts, installers, and
/// shortcuts to them, on any system; and on Unix, executable files.
const RUN_WHEN_OPENED: &[&str] = &[
  "exe",
  "com",
  "bat",
  "cmd",
  "ps1",
  "psm1",
  "vbs",
  "vbe",
  "js",
  "jse",
  "wsf",
  "wsh",
  "msi",
  "msp",
  "msc",
  "lnk",
  "scr",
  "pif",
  "cpl",
  "hta",
  "reg",
  "url",
  "appref-ms",
  "jar",
  "sh",
  "bash",
  "zsh",
  "command",
  "app",
  "pkg",
  "dmg",
  "desktop",
  "appimage",
  "run",
  "py",
  "pyw",
  "rb",
  "pl",
];

/// Whether opening `path` with its default app would run it.
pub fn runs_when_opened(path: &Path) -> bool {
  let by_name = path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| RUN_WHEN_OPENED.contains(&extension.to_ascii_lowercase().as_str()));
  #[cfg(unix)]
  let executable = {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
  };
  #[cfg(not(unix))]
  let executable = false;
  by_name || executable
}

#[cfg(test)]
mod tests {
  use std::path::Path;

  #[test]
  fn programs_and_scripts_are_not_opened_with_other_apps() {
    for path in [
      "a/Game.EXE",
      "b/run.bat",
      "c/x.ps1",
      "d/Link.lnk",
      "e/s.sh",
      "f/T.app",
    ] {
      assert!(super::runs_when_opened(Path::new(path)), "{path}");
    }
    for path in [
      "a/place.rbxl",
      "b/model.rbxm",
      "c/readme.md",
      "d/no-extension",
    ] {
      assert!(!super::runs_when_opened(Path::new(path)), "{path}");
    }
  }
}
