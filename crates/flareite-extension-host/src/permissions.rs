//! Permissions and the user's decisions about them.
//!
//! A manifest only *requests* permissions. An extension gets a permission
//! when the user grants it; the decisions are stored in a JSON file and
//! enforced on every host call. Until the user has decided about every
//! permission an extension requests, it is not started.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
  DocumentsRead,
  DocumentsWrite,
  /// Read files in the open folder (`files` interface, API 1.3).
  FilesRead,
  /// Change and create files in the open folder.
  FilesWrite,
  /// Start the language servers the manifest declares. Not checked by the
  /// extension API: the editor starts the servers, not the extension.
  LanguageServers,
  /// Let its webview pages load content and code from the web (API 1.5),
  /// enforced by the pages' content security policy; and download files
  /// from the web (`downloads` interface, API 1.17).
  Network,
  /// Run the programs the manifest declares (`processes` interface, API
  /// 1.6), in trusted folders.
  Processes,
  /// Show pages from the web in its layouts' tabs (`web-page` panes, API
  /// 1.15).
  WebPages,
}

impl Permission {
  pub fn description(self) -> &'static str {
    match self {
      Self::DocumentsRead => "Read open documents and their changes, and show problems in them",
      Self::DocumentsWrite => "Edit open documents",
      Self::FilesRead => "Read the files in the open folder",
      Self::FilesWrite => "Change and create files in the open folder",
      Self::LanguageServers => "Run language servers, which are programs on this computer",
      Self::Network => "Get content from the web, for itself and its pages",
      Self::Processes => "Run programs on this computer, in folders you trust",
      Self::WebPages => {
        "Show pages from the web in its tabs, where you can browse and sign in to sites"
      }
    }
  }
}

/// The user's decisions for one extension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Decision {
  pub granted: BTreeSet<Permission>,
  pub denied: BTreeSet<Permission>,
  /// Turned off by the user.
  pub disabled: bool,
}

impl Decision {
  /// The requested permissions the user has granted.
  pub fn effective(&self, requested: &[Permission]) -> BTreeSet<Permission> {
    requested
      .iter()
      .copied()
      .filter(|permission| self.granted.contains(permission))
      .collect()
  }

  /// Requested permissions the user hasn't decided about yet.
  pub fn pending(&self, requested: &[Permission]) -> Vec<Permission> {
    requested
      .iter()
      .copied()
      .filter(|permission| !self.granted.contains(permission) && !self.denied.contains(permission))
      .collect()
  }
}

/// All decisions, by extension id, persisted to `path`.
#[derive(Debug, Default)]
pub struct Decisions {
  path: Option<PathBuf>,
  entries: BTreeMap<String, Decision>,
}

impl Decisions {
  /// Loads decisions from `path`. A missing or unreadable file means no
  /// decisions (unreadable ones are logged), so nothing is granted.
  pub fn load(path: Option<PathBuf>) -> Self {
    let entries = path
      .as_deref()
      .and_then(|path| match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
          .map_err(|error| tracing::warn!(%error, path = %path.display(), "ignoring invalid extension permissions file"))
          .ok(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
          tracing::warn!(%error, path = %path.display(), "could not read extension permissions");
          None
        }
      })
      .unwrap_or_default();
    Self { path, entries }
  }

  pub fn get(&self, id: &str) -> Decision {
    self.entries.get(id).cloned().unwrap_or_default()
  }

  /// Records a decision and saves the file.
  pub fn set(&mut self, id: &str, decision: Decision) {
    if decision == Decision::default() {
      self.entries.remove(id);
    } else {
      self.entries.insert(id.to_owned(), decision);
    }
    self.save();
  }

  fn save(&self) {
    let Some(path) = &self.path else {
      return;
    };
    let result = serde_json::to_string_pretty(&self.entries)
      .map_err(std::io::Error::other)
      .and_then(|text| flareite_workspace::write_file_atomically(path, text.as_bytes()));
    if let Err(error) = result {
      tracing::error!(%error, path = %path.display(), "could not save extension permissions");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_granted_requested_permissions_are_effective() {
    let decision = Decision {
      granted: BTreeSet::from([Permission::DocumentsRead]),
      denied: BTreeSet::from([Permission::DocumentsWrite]),
      disabled: false,
    };
    let requested = [Permission::DocumentsRead, Permission::DocumentsWrite];
    assert_eq!(
      decision.effective(&requested),
      BTreeSet::from([Permission::DocumentsRead])
    );
    assert!(decision.pending(&requested).is_empty());
    assert!(decision.effective(&[Permission::DocumentsWrite]).is_empty());
    assert_eq!(Decision::default().pending(&requested), requested.to_vec());
  }

  #[test]
  fn decisions_survive_a_reload_and_bad_files_grant_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("permissions.json");
    let mut decisions = Decisions::load(Some(path.clone()));
    let decision = Decision {
      granted: BTreeSet::from([Permission::DocumentsRead]),
      ..Decision::default()
    };
    decisions.set("example.tidy", decision.clone());
    assert_eq!(
      Decisions::load(Some(path.clone())).get("example.tidy"),
      decision
    );

    std::fs::write(&path, "{ not json").expect("write");
    assert_eq!(
      Decisions::load(Some(path)).get("example.tidy"),
      Decision::default()
    );
  }
}
