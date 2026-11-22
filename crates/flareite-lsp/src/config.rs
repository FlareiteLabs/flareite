//! Language servers (all configured by extensions or settings) and where
//! their workspaces are.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::client::ServerCommand;

/// A language server and the languages it serves.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
  /// Identifies the server in settings and the UI.
  pub name: String,
  pub command: ServerCommand,
  /// LSP language identifiers, e.g. `rust`.
  pub languages: Vec<String>,
  /// Files marking a project root, nearest first, used when a document is
  /// outside the open workspace folder.
  pub root_markers: Vec<String>,
  /// Its settings: a JSON object (dotted keys, like `"a.b"`, are nested
  /// tables), or null for none. Answered to `workspace/configuration`.
  pub settings: Value,
  /// Workspace folders to give it besides its root (API 2.3): each
  /// instance gets those inside its root (see [`folders_in`]), as an
  /// extension sets them for projects the server wouldn't find on its own.
  pub folders: Vec<PathBuf>,
}

/// The extra workspace folders an instance rooted at `root` gets: those
/// of `folders` inside it, but not the root itself, each once.
pub fn folders_in(root: &Path, folders: &[PathBuf]) -> Vec<PathBuf> {
  let mut inside: Vec<PathBuf> = Vec::new();
  for folder in folders {
    if folder.starts_with(root) && folder != root && !inside.contains(folder) {
      inside.push(folder.clone());
    }
  }
  inside
}

/// The workspace root for a server handling `file`: the open workspace
/// folder if it contains the file, else the nearest ancestor with a root
/// marker, else the file's directory.
pub fn find_root(file: &Path, markers: &[String], workspace: Option<&Path>) -> PathBuf {
  if let Some(workspace) = workspace
    && file.starts_with(workspace)
  {
    return workspace.to_path_buf();
  }
  let directory = file.parent().unwrap_or(file);
  directory
    .ancestors()
    .find(|dir| markers.iter().any(|marker| dir.join(marker).exists()))
    .unwrap_or(directory)
    .to_path_buf()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn roots_prefer_the_workspace_then_markers() {
    let temp = tempfile::tempdir().expect("temp dir");
    let project = temp.path().join("project");
    let source = project.join("src");
    std::fs::create_dir_all(&source).expect("creates");
    std::fs::write(project.join("Cargo.toml"), "").expect("writes");
    let file = source.join("main.rs");
    let markers = vec!["Cargo.toml".to_owned()];

    assert_eq!(find_root(&file, &markers, Some(temp.path())), temp.path());
    assert_eq!(find_root(&file, &markers, None), project);
    assert_eq!(find_root(&file, &[], None), source);
    let elsewhere = tempfile::tempdir().expect("temp dir");
    assert_eq!(find_root(&file, &markers, Some(elsewhere.path())), project);
  }

  #[test]
  fn instances_get_the_folders_inside_their_roots() {
    let root = Path::new("/work/os");
    let folders = [
      PathBuf::from("/work/os/kernel"),
      PathBuf::from("/work/os"),
      PathBuf::from("/work/other"),
      PathBuf::from("/work/os-tools"),
      PathBuf::from("/work/os/kernel"),
      PathBuf::from("/work/os/boot/loader"),
    ];
    assert_eq!(
      folders_in(root, &folders),
      [
        PathBuf::from("/work/os/kernel"),
        PathBuf::from("/work/os/boot/loader")
      ]
    );
    assert!(folders_in(Path::new("/elsewhere"), &folders).is_empty());
  }
}
