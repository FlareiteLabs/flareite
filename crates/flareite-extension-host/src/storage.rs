//! Extension storage (API 1.5): small JSON values an extension keeps across
//! sessions, which its code (the `storage` interface) and its webview pages
//! (`flareite.storage`) share, and the state webview tabs save
//! (`flareite.setState`).
//!
//! Each extension has a global scope and one per folder, stored as JSON
//! files in `<dir>/<extension id>/`: `global.json`, and
//! `workspaces/<hash>.json`, which records its folder. Values are checked
//! to be JSON and kept within limits, and every change is written
//! atomically. The user can see how much an extension stores, and clear it.
//!
//! It also lists the keys of an extension's secrets (API 1.17) in
//! `secrets.json`, without their values, which the system's credential
//! store keeps: that can't list them by extension, and clearing an
//! extension's storage removes them too.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

/// Longest key.
pub const MAX_KEY_BYTES: usize = 200;
/// Most bytes in one scope, counting keys and values.
pub const MAX_SCOPE_BYTES: usize = 1024 * 1024;
/// Largest state one webview tab saves.
pub const MAX_TAB_STATE_BYTES: usize = 64 * 1024;
/// Most webview tabs whose state is kept for the next session.
const MAX_SAVED_TABS: usize = 500;
const TAB_STATE_FILE: &str = "webview-state.json";
const SECRETS_FILE: &str = "secrets.json";

/// Why a value wasn't stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
  /// A bad key, or a value that isn't JSON.
  #[error("{0}")]
  Invalid(String),
  /// The scope would hold more than it may.
  #[error("{0}")]
  TooLarge(String),
  /// The file couldn't be written.
  #[error("{0}")]
  Unsaved(String),
}

/// Where an extension's value is kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
  Global,
  /// The open folder's.
  Workspace(PathBuf),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ScopeFile {
  /// For a workspace scope, its folder.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  folder: Option<PathBuf>,
  values: BTreeMap<String, String>,
}

pub struct Storage {
  /// `None` keeps everything in memory (tests, benchmarks).
  dir: Option<PathBuf>,
  scopes: Mutex<HashMap<(String, Scope), BTreeMap<String, String>>>,
  /// Saved webview tab states, by viewer id; loaded on first use.
  tabs: Mutex<Option<BTreeMap<String, String>>>,
  /// The keys of extensions' secrets, by extension; loaded on first use.
  secrets: Mutex<HashMap<String, BTreeSet<String>>>,
}

impl Storage {
  pub fn new(dir: Option<PathBuf>) -> Self {
    Self {
      dir,
      scopes: Mutex::new(HashMap::new()),
      tabs: Mutex::new(None),
      secrets: Mutex::new(HashMap::new()),
    }
  }

  /// Runs `f` on the keys of an extension's secrets, loading them first.
  fn with_secrets<T>(&self, extension: &str, f: impl FnOnce(&mut BTreeSet<String>) -> T) -> T {
    let mut secrets = self.secrets.lock().unwrap_or_else(PoisonError::into_inner);
    let keys = secrets.entry(extension.to_owned()).or_insert_with(|| {
      self
        .dir
        .as_ref()
        .and_then(|dir| std::fs::read_to_string(dir.join(extension).join(SECRETS_FILE)).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
    });
    f(keys)
  }

  /// The keys of an extension's secrets, sorted.
  pub fn secret_keys(&self, extension: &str) -> Vec<String> {
    self.with_secrets(extension, |keys| keys.iter().cloned().collect())
  }

  /// Records that an extension has a secret of `key`, or no longer has
  /// one.
  pub fn note_secret(&self, extension: &str, key: &str, present: bool) -> Result<(), String> {
    let file = self
      .dir
      .as_ref()
      .map(|dir| dir.join(extension).join(SECRETS_FILE));
    self.with_secrets(extension, |keys| {
      let changed = if present {
        keys.insert(key.to_owned())
      } else {
        keys.remove(key)
      };
      match file {
        Some(file) if changed => write(&file, keys),
        _ => Ok(()),
      }
    })
  }

  fn file(&self, extension: &str, scope: &Scope) -> Option<PathBuf> {
    let dir = self.dir.as_ref()?.join(extension);
    Some(match scope {
      Scope::Global => dir.join("global.json"),
      Scope::Workspace(folder) => dir.join("workspaces").join(format!(
        "{:016x}.json",
        fnv1a(folder.to_string_lossy().as_bytes())
      )),
    })
  }

  /// Runs `f` on a scope's values, loading them first.
  fn with_scope<T>(
    &self,
    extension: &str,
    scope: &Scope,
    f: impl FnOnce(&mut BTreeMap<String, String>) -> T,
  ) -> T {
    let mut scopes = self.scopes.lock().unwrap_or_else(PoisonError::into_inner);
    let values = scopes
      .entry((extension.to_owned(), scope.clone()))
      .or_insert_with(|| self.load(extension, scope));
    f(values)
  }

  fn load(&self, extension: &str, scope: &Scope) -> BTreeMap<String, String> {
    let Some(file) = self.file(extension, scope) else {
      return BTreeMap::new();
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
      return BTreeMap::new();
    };
    match serde_json::from_str::<ScopeFile>(&text) {
      // A hash collision is someone else's folder.
      Ok(saved) if matches!(scope, Scope::Workspace(folder) if saved.folder.as_ref() != Some(folder)) => {
        BTreeMap::new()
      }
      Ok(saved) => saved.values,
      Err(error) => {
        tracing::warn!(extension, file = %file.display(), %error, "ignoring invalid extension storage");
        BTreeMap::new()
      }
    }
  }

  pub fn get(&self, extension: &str, scope: &Scope, key: &str) -> Option<String> {
    self.with_scope(extension, scope, |values| values.get(key).cloned())
  }

  pub fn keys(&self, extension: &str, scope: &Scope) -> Vec<String> {
    self.with_scope(extension, scope, |values| values.keys().cloned().collect())
  }

  /// Sets a value (JSON text), or removes it (`None`), and saves the scope.
  pub fn set(
    &self,
    extension: &str,
    scope: &Scope,
    key: &str,
    value: Option<String>,
  ) -> Result<(), StorageError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.contains(char::is_control) {
      return Err(StorageError::Invalid(format!(
        "keys must be 1 to {MAX_KEY_BYTES} bytes, without control characters"
      )));
    }
    if let Some(value) = &value {
      check_json(value).map_err(StorageError::Invalid)?;
    }
    let file = self.file(extension, scope);
    self.with_scope(extension, scope, |values| {
      let mut next = values.clone();
      match value {
        Some(value) => next.insert(key.to_owned(), value),
        None => next.remove(key),
      };
      let size: usize = next
        .iter()
        .map(|(key, value)| key.len() + value.len())
        .sum();
      if size > MAX_SCOPE_BYTES {
        return Err(StorageError::TooLarge(format!(
          "an extension can store at most {} KiB per scope",
          MAX_SCOPE_BYTES / 1024
        )));
      }
      if let Some(file) = file {
        let saved = ScopeFile {
          folder: match scope {
            Scope::Global => None,
            Scope::Workspace(folder) => Some(folder.clone()),
          },
          values: next.clone(),
        };
        write(&file, &saved).map_err(StorageError::Unsaved)?;
      }
      *values = next;
      Ok(())
    })
  }

  /// How many bytes an extension stores on disk.
  pub fn usage(&self, extension: &str) -> u64 {
    fn size(path: &Path) -> u64 {
      let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
      };
      if metadata.is_dir() {
        std::fs::read_dir(path)
          .into_iter()
          .flatten()
          .flatten()
          .map(|entry| size(&entry.path()))
          .sum()
      } else {
        metadata.len()
      }
    }
    self
      .dir
      .as_ref()
      .map_or(0, |dir| size(&dir.join(extension)))
  }

  /// Removes everything an extension stores, in every scope, and its
  /// viewer tabs' saved states.
  pub fn clear(&self, extension: &str) -> Result<(), String> {
    self
      .scopes
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .retain(|(owner, _), _| owner != extension);
    self
      .secrets
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .remove(extension);
    // Viewer ids are `<extension id>.<viewer id>/<path>`.
    let prefix = format!("{extension}.");
    let file = self.dir.as_ref().map(|dir| dir.join(TAB_STATE_FILE));
    self.with_tabs(|tabs| {
      let before = tabs.len();
      tabs.retain(|id, _| !id.starts_with(&prefix));
      if tabs.len() != before
        && let Some(file) = file
      {
        write(&file, tabs)?;
      }
      Ok::<(), String>(())
    })?;
    let Some(dir) = self.dir.as_ref().map(|dir| dir.join(extension)) else {
      return Ok(());
    };
    match std::fs::remove_dir_all(&dir) {
      Ok(()) => Ok(()),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(error) => Err(format!("{} couldn't be removed: {error}", dir.display())),
    }
  }

  fn with_tabs<T>(&self, f: impl FnOnce(&mut BTreeMap<String, String>) -> T) -> T {
    let mut tabs = self.tabs.lock().unwrap_or_else(PoisonError::into_inner);
    let tabs = tabs.get_or_insert_with(|| {
      self
        .dir
        .as_ref()
        .and_then(|dir| std::fs::read_to_string(dir.join(TAB_STATE_FILE)).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
    });
    f(tabs)
  }

  /// The state a viewer tab saved in an earlier session.
  pub fn tab_state(&self, id: &str) -> Option<String> {
    self.with_tabs(|tabs| tabs.get(id).cloned())
  }

  /// Saves (or forgets, with `None`) a viewer tab's state for the next
  /// session.
  pub fn set_tab_state(&self, id: &str, state: Option<String>) {
    let file = self.dir.as_ref().map(|dir| dir.join(TAB_STATE_FILE));
    self.with_tabs(|tabs| {
      let changed = match state {
        Some(state) if tabs.len() < MAX_SAVED_TABS || tabs.contains_key(id) => {
          tabs.insert(id.to_owned(), state.clone()).as_ref() != Some(&state)
        }
        Some(_) => false,
        None => tabs.remove(id).is_some(),
      };
      if changed
        && let Some(file) = file
        && let Err(error) = write(&file, tabs)
      {
        tracing::warn!(%error, "couldn't save webview state");
      }
    });
  }
}

/// Checks that `text` is JSON.
pub fn check_json(text: &str) -> Result<(), String> {
  serde_json::from_str::<serde::de::IgnoredAny>(text)
    .map(|_| ())
    .map_err(|error| format!("the value isn't JSON: {error}"))
}

fn write(file: &Path, value: &impl Serialize) -> Result<(), String> {
  let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
  if let Some(parent) = file.parent() {
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
  }
  flareite_workspace::write_file_atomically(file, &bytes)
    .map_err(|error| format!("{} couldn't be saved: {error}", file.display()))
}

/// A stable 64-bit hash (FNV-1a), for file names.
fn fnv1a(bytes: &[u8]) -> u64 {
  bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
    (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn keeps_values_per_extension_and_scope_across_sessions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let folder = Scope::Workspace(PathBuf::from("/projects/a"));
    let storage = Storage::new(Some(dir.path().to_path_buf()));
    storage
      .set(
        "flareite.images",
        &Scope::Global,
        "background",
        Some("\"white\"".into()),
      )
      .expect("set");
    storage
      .set("flareite.images", &folder, "zoom", Some("2".into()))
      .expect("set");
    assert_eq!(
      storage.get("example.other", &Scope::Global, "background"),
      None
    );
    assert_eq!(
      storage.get(
        "flareite.images",
        &Scope::Workspace("/projects/b".into()),
        "zoom"
      ),
      None
    );

    let later = Storage::new(Some(dir.path().to_path_buf()));
    assert_eq!(
      later
        .get("flareite.images", &Scope::Global, "background")
        .as_deref(),
      Some("\"white\"")
    );
    assert_eq!(later.keys("flareite.images", &folder), ["zoom"]);
    assert!(later.usage("flareite.images") > 0);

    later
      .set("flareite.images", &folder, "zoom", None)
      .expect("remove");
    assert_eq!(later.get("flareite.images", &folder, "zoom"), None);
    later.clear("flareite.images").expect("clear");
    assert_eq!(later.usage("flareite.images"), 0);
    assert_eq!(
      later.get("flareite.images", &Scope::Global, "background"),
      None
    );
  }

  #[test]
  fn lists_secret_keys_across_sessions_until_cleared() {
    let dir = tempfile::tempdir().expect("temp dir");
    let storage = Storage::new(Some(dir.path().to_path_buf()));
    storage.note_secret("ext.a", "token", true).expect("noted");
    storage
      .note_secret("ext.a", "api-key", true)
      .expect("noted");
    storage.note_secret("ext.b", "other", true).expect("noted");
    storage.note_secret("ext.a", "token", false).expect("noted");
    let again = Storage::new(Some(dir.path().to_path_buf()));
    assert_eq!(again.secret_keys("ext.a"), ["api-key"]);
    let text = std::fs::read_to_string(dir.path().join("ext.a").join(SECRETS_FILE)).expect("file");
    assert_eq!(text, r#"["api-key"]"#, "keys only");
    again.clear("ext.a").expect("cleared");
    assert!(again.secret_keys("ext.a").is_empty());
    assert_eq!(again.secret_keys("ext.b"), ["other"]);
  }

  #[test]
  fn refuses_bad_keys_values_and_too_much() {
    let storage = Storage::new(None);
    let set = |key: &str, value: &str| storage.set("x.y", &Scope::Global, key, Some(value.into()));
    assert!(set("", "1").is_err());
    assert!(set("a\nb", "1").is_err());
    assert!(set(&"k".repeat(MAX_KEY_BYTES + 1), "1").is_err());
    assert!(matches!(
      set("k", "not json"),
      Err(StorageError::Invalid(_))
    ));
    let big = format!("\"{}\"", "x".repeat(MAX_SCOPE_BYTES / 2));
    assert!(set("a", &big).is_ok());
    assert!(matches!(set("b", &big), Err(StorageError::TooLarge(_))));
    assert_eq!(storage.keys("x.y", &Scope::Global), ["a"], "unchanged");
  }

  #[test]
  fn saves_tab_states_for_the_next_session() {
    let dir = tempfile::tempdir().expect("temp dir");
    let storage = Storage::new(Some(dir.path().to_path_buf()));
    storage.set_tab_state("v/a.svg", Some("{\"zoom\":2}".into()));
    let later = Storage::new(Some(dir.path().to_path_buf()));
    assert_eq!(later.tab_state("v/a.svg").as_deref(), Some("{\"zoom\":2}"));
    later.set_tab_state("v/a.svg", None);
    assert_eq!(
      Storage::new(Some(dir.path().to_path_buf())).tab_state("v/a.svg"),
      None
    );

    // Clearing an extension's storage forgets its tabs too.
    let storage = Storage::new(Some(dir.path().to_path_buf()));
    storage.set_tab_state("flareite.images.svg/a.svg", Some("1".into()));
    storage.set_tab_state("flareite.imagesx.svg/b.svg", Some("2".into()));
    storage.clear("flareite.images").expect("clear");
    let later = Storage::new(Some(dir.path().to_path_buf()));
    assert_eq!(later.tab_state("flareite.images.svg/a.svg"), None);
    assert_eq!(
      later.tab_state("flareite.imagesx.svg/b.svg").as_deref(),
      Some("2")
    );
  }
}
