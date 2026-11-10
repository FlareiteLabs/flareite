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
}
