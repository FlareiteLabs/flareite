//! Configuring the language servers an extension declares (API 1.16): their
//! arguments and settings, as its code sets them with the
//! `language-servers` interface, over its manifest's, and the workspace
//! folders they get besides their roots (2.3). They last until the
//! extension stops. Flareite runs the servers; see
//! [`crate::ExtensionHost::language_servers`].

use std::path::{Component, Path, PathBuf};

use crate::bindings::flareite::extension::language_servers::Host;
use crate::bindings::flareite::extension::types::ApiError;
use crate::contributions::{check_server_settings, downloads_in};
use crate::permissions::Permission;
use crate::runtime::StoreState;

/// Most arguments a server takes, and characters in each.
const MAX_ARGS: usize = 32;
const MAX_ARG_CHARS: usize = 1000;
/// Most workspace folders a server gets besides its root.
const MAX_FOLDERS: usize = 64;

/// What an extension's code changed about one of its servers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerChanges {
  pub args: Option<Vec<String>>,
  pub settings: Option<serde_json::Value>,
  /// Workspace folders besides an instance's root, resolved.
  pub folders: Option<Vec<PathBuf>>,
}

/// Checks a server's arguments as its code sets them.
fn check_args(args: &[String]) -> Result<(), ApiError> {
  if args.len() > MAX_ARGS {
    return Err(ApiError::TooLarge(format!(
      "a language server takes at most {MAX_ARGS} arguments"
    )));
  }
  for arg in args {
    if arg.chars().count() > MAX_ARG_CHARS || arg.contains('\0') {
      return Err(ApiError::InvalidArgument(format!(
        "arguments must be at most {MAX_ARG_CHARS} characters"
      )));
    }
    downloads_in(arg).map_err(ApiError::InvalidArgument)?;
  }
  Ok(())
}

fn parse_settings(settings: &str) -> Result<serde_json::Value, ApiError> {
  let value: serde_json::Value = serde_json::from_str(settings)
    .map_err(|error| ApiError::InvalidArgument(format!("the settings aren't JSON: {error}")))?;
  check_server_settings(&value).map_err(|error| {
    if error.contains("KiB") {
      ApiError::TooLarge(format!("the settings {error}"))
    } else {
      ApiError::InvalidArgument(format!("the settings {error}"))
    }
  })?;
  Ok(value)
}

/// Checks the workspace folders an extension gives a server: absolute
/// paths to folders inside the open folder, `root`. Returns them resolved
/// as the root is (links followed, without Windows' `\\?\` prefix), each
/// once.
fn check_folders(root: &Path, folders: &[String]) -> Result<Vec<PathBuf>, ApiError> {
  if folders.len() > MAX_FOLDERS {
    return Err(ApiError::TooLarge(format!(
      "a language server gets at most {MAX_FOLDERS} folders"
    )));
  }
  let mut resolved: Vec<PathBuf> = Vec::new();
  for folder in folders {
    let given = Path::new(folder);
    if !given.is_absolute() || given.components().any(|c| c == Component::ParentDir) {
      return Err(ApiError::InvalidArgument(format!(
        "{folder:?} isn't an absolute path"
      )));
    }
    if !given.is_dir() {
      return Err(ApiError::InvalidArgument(format!(
        "{folder:?} isn't a folder"
      )));
    }
    let path = flareite_workspace::normalize_path(given);
    if !path.starts_with(root) {
      return Err(ApiError::InvalidArgument(format!(
        "{folder:?} isn't inside the open folder"
      )));
    }
    if !resolved.contains(&path) {
      resolved.push(path);
    }
  }
  Ok(resolved)
}

impl StoreState {
  /// Checks that the extension may configure `server`: it declares it, and
  /// may declare language servers.
  fn own_server(&self, server: &str) -> Result<std::sync::Arc<crate::host::Shared>, ApiError> {
    if !self.api.has(Permission::LanguageServers) {
      return Err(ApiError::InvalidArgument(
        "the language_servers permission isn't granted".into(),
      ));
    }
    let declared = self
      .api
      .manifest()
      .contributions
      .language_servers
      .iter()
      .any(|declared| declared.name == server);
    if !declared {
      return Err(ApiError::InvalidArgument(format!(
        "the manifest declares no language server named {server:?}"
      )));
    }
    self.api.shared()
  }
}

impl Host for StoreState {
  fn set_arguments(&mut self, server: String, args: Vec<String>) -> Result<(), ApiError> {
    check_args(&args)?;
    let shared = self.own_server(&server)?;
    shared.change_server(self.api.extension_id(), &server, |changes| {
      changes.args = Some(args);
    });
    Ok(())
  }

  fn set_settings(&mut self, server: String, settings: String) -> Result<(), ApiError> {
    let settings = parse_settings(&settings)?;
    let shared = self.own_server(&server)?;
    shared.change_server(self.api.extension_id(), &server, |changes| {
      changes.settings = Some(settings);
    });
    Ok(())
  }

  fn restart(&mut self, server: String) -> Result<(), ApiError> {
    let shared = self.own_server(&server)?;
    shared.restart_server(&server);
    Ok(())
  }

  fn set_folders(&mut self, server: String, folders: Vec<String>) -> Result<(), ApiError> {
    let shared = self.own_server(&server)?;
    let folders = if folders.is_empty() {
      Vec::new()
    } else {
      let Some(open) = shared.embedder.open_folder() else {
        return Err(ApiError::InvalidArgument("no folder is open".into()));
      };
      check_folders(&open.root, &folders)?
    };
    shared.change_server(self.api.extension_id(), &server, |changes| {
      changes.folders = (!folders.is_empty()).then_some(folders);
    });
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn arguments_and_settings_are_checked() {
    assert!(
      check_args(&[
        "lsp".into(),
        "--docs=${download:https://a.dev/d.json}".into()
      ])
      .is_ok()
    );
    assert!(matches!(
      check_args(&["${download:file:///etc/passwd}".into()]),
      Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(
      check_args(&vec![String::new(); MAX_ARGS + 1]),
      Err(ApiError::TooLarge(_))
    ));
    assert!(matches!(
      check_args(&["x".repeat(MAX_ARG_CHARS + 1)]),
      Err(ApiError::InvalidArgument(_))
    ));

    assert_eq!(
      parse_settings(r#"{"a.b": 1}"#).ok(),
      Some(serde_json::json!({ "a.b": 1 }))
    );
    for bad in ["[1]", "{", "null"] {
      assert!(
        matches!(parse_settings(bad), Err(ApiError::InvalidArgument(_))),
        "{bad}"
      );
    }
    let huge = format!(r#"{{"a": "{}"}}"#, "x".repeat(300 * 1024));
    assert!(matches!(parse_settings(&huge), Err(ApiError::TooLarge(_))));
  }

  #[test]
  fn folders_are_folders_inside_the_open_one() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = flareite_workspace::normalize_path(temp.path());
    let kernel = root.join("kernel");
    std::fs::create_dir(&kernel).expect("creates");
    std::fs::write(root.join("Cargo.toml"), "").expect("writes");
    let text = |path: &Path| path.display().to_string();

    assert_eq!(
      check_folders(&root, &[text(&kernel), text(&kernel)]).ok(),
      Some(vec![kernel.clone()])
    );
    let outside = tempfile::tempdir().expect("temp dir");
    for bad in [
      "kernel".to_owned(),
      text(&root.join("kernel").join("..").join("kernel")),
      text(&root.join("missing")),
      text(&root.join("Cargo.toml")),
      text(outside.path()),
    ] {
      assert!(
        matches!(
          check_folders(&root, std::slice::from_ref(&bad)),
          Err(ApiError::InvalidArgument(_))
        ),
        "{bad}"
      );
    }
    assert!(matches!(
      check_folders(&root, &vec![text(&kernel); MAX_FOLDERS + 1]),
      Err(ApiError::TooLarge(_))
    ));
  }
}
