//! The host functions of the `files` interface (API 1.3): files in the
//! folder open in the editor, and their bytes (2.4).
//!
//! Every path is resolved (links and `..` included) and must be inside the
//! open folder and not hidden by `files.exclude`; Git's own folder is never
//! available, as writing there could make Git run code. A file open in the
//! editor is read from and written to its document, so an extension never
//! works on an older copy than the user sees.

use std::io;
use std::path::{Component, Path, PathBuf};

use flareite_core::DocumentId;
use flareite_text::{LineEnding, normalize_line_endings};
use flareite_workspace::{FileError as DiskError, load_text_file, normalize_path};

use crate::api::{ApiContext, with_editor};
use crate::bindings::flareite::extension::files::{ByteSlice, FileError, FileList, Host};
use crate::host::Shared;
use crate::permissions::Permission;
use crate::runtime::StoreState;
use crate::{Notification, OpenFolder};

/// Most files `list-files` returns.
pub const MAX_LISTED_FILES: usize = 10_000;
/// Largest file read or written.
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

impl ApiContext {
  fn require_files(&self, permission: Permission) -> Result<std::sync::Arc<Shared>, FileError> {
    if !self.has(permission) {
      let name = match permission {
        Permission::FilesWrite => "files-write",
        _ => "files-read",
      };
      return Err(FileError::PermissionDenied(name.into()));
    }
    self
      .shared()
      .map_err(|_| FileError::Failed("the editor is closing".into()))
  }
}

fn too_large() -> FileError {
  FileError::TooLarge(format!(
    "files of more than {} MiB can't be read or written",
    MAX_FILE_BYTES / 1024 / 1024
  ))
}

fn disk_error(error: DiskError) -> FileError {
  match error {
    DiskError::NotFound { .. } => FileError::NotFound,
    DiskError::IsDirectory { .. } => FileError::WrongKind,
    DiskError::Binary { .. } => FileError::NotText,
    other => FileError::Failed(other.to_string()),
  }
}

fn io_error(error: io::Error) -> FileError {
  match error.kind() {
    io::ErrorKind::NotFound => FileError::NotFound,
    _ => FileError::Failed(error.to_string()),
  }
}

/// The open folder.
fn folder(shared: &Shared) -> Result<OpenFolder, FileError> {
  shared
    .embedder
    .open_folder()
    .ok_or(FileError::OutsideFolder)
}

/// `path` resolved, if extensions may use it: inside the open folder, not
/// excluded, and not in Git's folder. With `new_allowed`, a file that
/// doesn't exist yet is resolved through its folder, which must exist.
fn resolve(folder: &OpenFolder, path: &str, new_allowed: bool) -> Result<PathBuf, FileError> {
  let given = Path::new(path);
  if !given.is_absolute() || given.components().any(|c| c == Component::ParentDir) {
    return Err(FileError::OutsideFolder);
  }
  let resolved = match std::fs::symlink_metadata(given) {
    Ok(_) => normalize_path(given),
    Err(error) if error.kind() == io::ErrorKind::NotFound && new_allowed => {
      let (Some(parent), Some(name)) = (given.parent(), given.file_name()) else {
        return Err(FileError::OutsideFolder);
      };
      if !parent.is_dir() {
        return Err(FileError::NotFound);
      }
      normalize_path(parent).join(name)
    }
    Err(error) => return Err(io_error(error)),
  };
  let inside = resolved
    .strip_prefix(&folder.root)
    .map_err(|_| FileError::OutsideFolder)?;
  let in_git = inside.components().any(|c| c.as_os_str() == ".git");
  // Exclusions match the excluded entry, not what's inside it.
  let excluded = resolved
    .ancestors()
    .take_while(|path| *path != folder.root)
    .any(|path| folder.exclude.is_excluded(&folder.root, path));
  if in_git || excluded {
    return Err(FileError::OutsideFolder);
  }
  Ok(resolved)
}

impl Host for StoreState {
  fn folder(&mut self) -> Result<Option<String>, FileError> {
    let shared = self.api.require_files(Permission::FilesRead)?;
    Ok(
      shared
        .embedder
        .open_folder()
        .map(|folder| folder.root.display().to_string()),
    )
  }

  fn list_files(&mut self, path: String) -> Result<FileList, FileError> {
    let shared = self.api.require_files(Permission::FilesRead)?;
    let folder = folder(&shared)?;
    let dir = resolve(&folder, &path, false)?;
    if !dir.is_dir() {
      return Err(FileError::WrongKind);
    }
    let (paths, truncated) =
      flareite_workspace::files_in(&folder.root, &dir, &folder.exclude, MAX_LISTED_FILES);
    Ok(FileList {
      paths: paths
        .into_iter()
        .map(|path| path.display().to_string())
        .collect(),
      truncated,
    })
  }

  fn read_file(&mut self, path: String) -> Result<String, FileError> {
    let shared = self.api.require_files(Permission::FilesRead)?;
    let path = resolve(&folder(&shared)?, &path, false)?;
    let open = with_editor(&shared, |editor| {
      let document = editor.document(editor.find_by_path(&path)?).ok()?;
      Some(if document.buffer().len() > MAX_FILE_BYTES {
        Err(too_large())
      } else {
        Ok(document.buffer().contents())
      })
    })
    .flatten();
    if let Some(text) = open {
      return text;
    }
    if std::fs::metadata(&path).map_err(io_error)?.len() > MAX_FILE_BYTES as u64 {
      return Err(too_large());
    }
    load_text_file(&path)
      .map(|loaded| loaded.text)
      .map_err(disk_error)
  }

  fn read_bytes(&mut self, path: String, offset: u64, length: u32) -> Result<ByteSlice, FileError> {
    use std::io::{Read, Seek, SeekFrom};
    let shared = self.api.require_files(Permission::FilesRead)?;
    if length as usize > MAX_FILE_BYTES {
      return Err(too_large());
    }
    let path = resolve(&folder(&shared)?, &path, false)?;
    // Before opening it: Windows refuses to open a folder.
    if std::fs::metadata(&path).map_err(io_error)?.is_dir() {
      return Err(FileError::WrongKind);
    }
    let mut file = std::fs::File::open(&path).map_err(io_error)?;
    let size = file.metadata().map_err(io_error)?.len();
    let mut bytes = Vec::new();
    if offset < size {
      file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
      file
        .take(u64::from(length))
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    }
    Ok(ByteSlice { size, bytes })
  }

  fn write_file(&mut self, path: String, text: String) -> Result<(), FileError> {
    let shared = self.api.require_files(Permission::FilesWrite)?;
    if text.len() > MAX_FILE_BYTES {
      return Err(too_large());
    }
    let path = resolve(&folder(&shared)?, &path, true)?;
    if path.is_dir() {
      return Err(FileError::WrongKind);
    }
    let id = self.api.extension_id().to_owned();

    // Open in the editor: change the document.
    let open = with_editor(&shared, |editor| {
      let document_id = editor.find_by_path(&path)?;
      let document = editor.document_mut(document_id).ok()?;
      let was_dirty = document.is_dirty();
      let text = normalize_line_endings(&text, document.line_ending());
      let changed = document
        .replace_text(&text)
        .map_err(|error| FileError::Failed(error.to_string()));
      if changed == Ok(true) {
        shared.sync(document);
      }
      Some(changed.map(|changed| (document_id, changed, was_dirty)))
    })
    .flatten();
    if let Some(outcome) = open {
      let (document, changed, was_dirty) = outcome?;
      if changed {
        tracing::info!(extension = %id, path = %path.display(), "extension changed a file's document");
        shared
          .embedder
          .notify(Notification::DocumentEdited(document));
        if !was_dirty {
          save(&shared, document)?;
        }
      }
      return Ok(());
    }

    // Not open: write the file.
    let bytes = match load_text_file(&path) {
      Ok(existing) => {
        let ending = LineEnding::detect(existing.text.bytes()).unwrap_or_default();
        let text = normalize_line_endings(&text, ending);
        if text == existing.text {
          return Ok(());
        }
        // In the file's own encoding.
        existing.encoding.encode(&text).map_err(|character| {
          FileError::Failed(format!(
            "the file's encoding, {}, has no {character:?}",
            existing.encoding.label()
          ))
        })?
      }
      Err(DiskError::NotFound { .. }) => text.into_bytes(),
      Err(error) => return Err(disk_error(error)),
    };
    flareite_workspace::write_file_atomically(&path, &bytes).map_err(io_error)?;
    tracing::info!(extension = %id, path = %path.display(), "extension wrote a file");
    Ok(())
  }

  fn delete(&mut self, path: String, to_trash: bool) -> Result<(), FileError> {
    let shared = self.api.require_files(Permission::FilesWrite)?;
    let folder = folder(&shared)?;
    let path = resolve(&folder, &path, false)?;
    if path == folder.root {
      return Err(FileError::OutsideFolder);
    }
    // Links are deleted, not what they point to.
    if std::fs::symlink_metadata(&path).is_err() {
      return Err(FileError::NotFound);
    }
    flareite_workspace::delete_entry(&path, to_trash)
      .map_err(|error| FileError::Failed(error.to_string()))?;
    tracing::info!(
      extension = %self.api.extension_id(),
      path = %path.display(),
      to_trash,
      "extension deleted a file"
    );
    Ok(())
  }
}

fn save(shared: &Shared, document: DocumentId) -> Result<(), FileError> {
  shared.embedder.save(document).map_err(|reason| {
    FileError::Failed(format!("the document was changed but not saved: {reason}"))
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_paths_inside_the_folder_and_not_excluded_resolve() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = normalize_path(dir.path());
    std::fs::create_dir_all(root.join("src")).expect("mkdir");
    std::fs::create_dir_all(root.join(".git").join("hooks")).expect("mkdir");
    std::fs::create_dir_all(root.join("node_modules")).expect("mkdir");
    std::fs::write(root.join("src").join("a.rs"), "").expect("write");
    let folder = OpenFolder {
      root: root.clone(),
      exclude: flareite_workspace::ExcludeFilter::new(&["node_modules"]).expect("valid"),
      trusted: true,
    };
    let at = |path: &Path| path.display().to_string();

    assert_eq!(
      resolve(&folder, &at(&root.join("src").join("a.rs")), false),
      Ok(root.join("src").join("a.rs"))
    );
    assert_eq!(
      resolve(&folder, &at(&root.join("src").join("new.rs")), false),
      Err(FileError::NotFound)
    );
    assert_eq!(
      resolve(&folder, &at(&root.join("src").join("new.rs")), true),
      Ok(root.join("src").join("new.rs"))
    );
    assert_eq!(
      resolve(&folder, &at(&root.join("missing").join("new.rs")), true),
      Err(FileError::NotFound)
    );
    for outside in [
      root.join("..").join("x.rs"),
      root.join(".git").join("hooks").join("pre-commit"),
      root.join("node_modules").join("x.js"),
      root.parent().expect("parent").to_path_buf(),
      PathBuf::from("src/a.rs"),
    ] {
      assert_eq!(
        resolve(&folder, &at(&outside), true),
        Err(FileError::OutsideFolder),
        "{}",
        outside.display()
      );
    }
  }
}
