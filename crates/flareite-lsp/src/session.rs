//! What a running server asks of the client besides documents: its
//! settings (`workspace/configuration`), and the files it watches
//! (`workspace/didChangeWatchedFiles`, which it registers as it runs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use globset::{GlobBuilder, GlobMatcher};
use serde_json::{Map, Value, json};

use crate::convert::{path_to_uri, uri_to_path};

/// Settings as VS Code keeps them: dotted keys (`"luau-lsp.platform.type"`)
/// are the same as nested tables, so either way of writing them works.
/// Objects are merged, and later keys win.
pub fn expand(settings: &Value) -> Value {
  let Value::Object(entries) = settings else {
    return settings.clone();
  };
  let mut out = Value::Object(Map::new());
  for (key, value) in entries {
    let mut target = &mut out;
    let mut parts = key.split('.').filter(|part| !part.is_empty()).peekable();
    while let Some(part) = parts.next() {
      let Value::Object(map) = target else {
        break;
      };
      if parts.peek().is_none() {
        let value = expand(value);
        match (map.get_mut(part), value) {
          (Some(existing @ Value::Object(_)), Value::Object(new)) => {
            merge(existing, Value::Object(new));
          }
          (_, value) => {
            map.insert(part.to_owned(), value);
          }
        }
        break;
      }
      let entry = map
        .entry(part.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
      if !entry.is_object() {
        *entry = Value::Object(Map::new());
      }
      target = entry;
    }
  }
  out
}

/// Merges `over` into `base`: objects key by key, anything else replaced.
pub fn merge(base: &mut Value, over: Value) {
  match (base, over) {
    (Value::Object(base), Value::Object(over)) => {
      for (key, value) in over {
        match base.get_mut(&key) {
          Some(existing) => merge(existing, value),
          None => {
            base.insert(key, value);
          }
        }
      }
    }
    (base, over) => *base = over,
  }
}

/// The value at a dotted `section` of expanded settings (all of them
/// without one); null if there's none.
pub fn section(settings: &Value, section: Option<&str>) -> Value {
  let Some(section) = section.filter(|section| !section.is_empty()) else {
    return settings.clone();
  };
  section
    .split('.')
    .try_fold(settings, |value, part| value.get(part))
    .cloned()
    .unwrap_or(Value::Null)
}

/// A file change, for watchers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChange {
  Created,
  Changed,
  Deleted,
}

impl FileChange {
  /// Its `FileChangeType`, and its bit in a watcher's `kind`.
  fn code(self) -> (u8, u8) {
    match self {
      Self::Created => (1, 1),
      Self::Changed => (2, 2),
      Self::Deleted => (3, 4),
    }
  }
}

/// One `FileSystemWatcher` a server registered.
struct Watcher {
  glob: GlobMatcher,
  /// Matched against the path relative to this, for a relative pattern.
  base: Option<PathBuf>,
  /// Which changes: 1 created, 2 changed, 4 deleted.
  kind: u8,
}

impl Watcher {
  fn parse(value: &Value) -> Option<Self> {
    let (pattern, base) = match &value["globPattern"] {
      Value::String(pattern) => (pattern.as_str(), None),
      Value::Object(relative) => {
        let base = match &relative.get("baseUri")? {
          Value::String(uri) => uri.as_str(),
          // A WorkspaceFolder.
          other => other.get("uri")?.as_str()?,
        };
        let base = uri_to_path(&base.parse().ok()?)?;
        (relative.get("pattern")?.as_str()?, Some(base))
      }
      _ => return None,
    };
    let glob = GlobBuilder::new(pattern)
      .literal_separator(true)
      .case_insensitive(cfg!(windows))
      .build()
      .ok()?
      .compile_matcher();
    let kind = value["kind"].as_u64().map_or(7, |kind| (kind & 7) as u8);
    Some(Self { glob, base, kind })
  }

  fn matches(&self, path: &Path, change: FileChange) -> bool {
    if self.kind & change.code().1 == 0 {
      return false;
    }
    let candidate = match &self.base {
      Some(base) => match path.strip_prefix(base) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => return false,
      },
      None => path.to_path_buf(),
    };
    // Globs use '/', as servers write them.
    let text = candidate.to_string_lossy().replace('\\', "/");
    self.glob.is_match(&text)
  }
}

/// A server's settings and watched files, shared by its threads.
#[derive(Default)]
pub struct Session {
  /// Expanded (see [`expand`]).
  settings: Mutex<Value>,
  /// Watchers by registration id.
  watchers: Mutex<HashMap<String, Vec<Watcher>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Session {
  pub fn new(settings: &Value) -> Self {
    Self {
      settings: Mutex::new(expand(settings)),
      watchers: Mutex::default(),
    }
  }

  /// Replaces the settings; returns the expanded ones if they changed.
  pub fn set_settings(&self, settings: &Value) -> Option<Value> {
    let expanded = expand(settings);
    let mut current = lock(&self.settings);
    (*current != expanded).then(|| {
      *current = expanded.clone();
      expanded
    })
  }

  /// The answer to `workspace/configuration`: one value per item.
  pub fn configuration(&self, params: Option<&Value>) -> Value {
    let settings = lock(&self.settings);
    let items = params
      .and_then(|params| params.get("items"))
      .and_then(Value::as_array)
      .map(Vec::as_slice)
      .unwrap_or_default();
    Value::Array(
      items
        .iter()
        .map(|item| section(&settings, item.get("section").and_then(Value::as_str)))
        .collect(),
    )
  }

  /// Handles `client/registerCapability`: keeps file watchers, and accepts
  /// (and ignores) the rest.
  pub fn register(&self, params: Option<&Value>) {
    let registrations = params
      .and_then(|params| params.get("registrations"))
      .and_then(Value::as_array);
    for registration in registrations.into_iter().flatten() {
      if registration["method"] != "workspace/didChangeWatchedFiles" {
        continue;
      }
      let Some(id) = registration["id"].as_str() else {
        continue;
      };
      let watchers: Vec<Watcher> = registration["registerOptions"]["watchers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Watcher::parse)
        .collect();
      lock(&self.watchers).insert(id.to_owned(), watchers);
    }
  }

  /// Handles `client/unregisterCapability` (whose list is misspelled
  /// `unregisterations` in the protocol).
  pub fn unregister(&self, params: Option<&Value>) {
    let removals = params.and_then(|params| {
      params
        .get("unregisterations")
        .or_else(|| params.get("unregistrations"))
        .and_then(Value::as_array)
    });
    let mut watchers = lock(&self.watchers);
    for removal in removals.into_iter().flatten() {
      if let Some(id) = removal["id"].as_str() {
        watchers.remove(id);
      }
    }
  }

  /// The `workspace/didChangeWatchedFiles` params for the changes the
  /// server watches; `None` if it watches none of them.
  pub fn watched_changes(&self, changes: &[(PathBuf, FileChange)]) -> Option<Value> {
    let watchers = lock(&self.watchers);
    let events: Vec<Value> = changes
      .iter()
      .filter(|(path, change)| {
        watchers
          .values()
          .flatten()
          .any(|watcher| watcher.matches(path, *change))
      })
      .filter_map(|(path, change)| {
        Some(json!({ "uri": path_to_uri(path)?.as_str(), "type": change.code().0 }))
      })
      .collect();
    (!events.is_empty()).then(|| json!({ "changes": events }))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn dotted_keys_nest_and_merge() {
    let settings = json!({
      "luau-lsp.platform.type": "roblox",
      "luau-lsp": { "sourcemap": { "enabled": true } },
      "luau-lsp.sourcemap.rojoProjectFile": "x.project.json",
      "plain": 1,
    });
    assert_eq!(
      expand(&settings),
      json!({
        "luau-lsp": {
          "platform": { "type": "roblox" },
          "sourcemap": { "enabled": true, "rojoProjectFile": "x.project.json" },
        },
        "plain": 1,
      })
    );
  }

  #[test]
  fn configuration_answers_each_section() {
    let session = Session::new(&json!({ "a.b": 1, "c": { "d": [2] } }));
    let params = json!({ "items": [
      { "section": "a" }, { "section": "a.b" }, { "section": "c.d" },
      { "section": "missing" }, {},
    ]});
    assert_eq!(
      session.configuration(Some(&params)),
      json!([{ "b": 1 }, 1, [2], null, { "a": { "b": 1 }, "c": { "d": [2] } }])
    );
    assert_eq!(session.configuration(None), json!([]));
    assert!(
      session
        .set_settings(&json!({ "a": { "b": 1 }, "c.d": [2] }))
        .is_none()
    );
    assert_eq!(
      session.set_settings(&json!({ "a.b": 2 })),
      Some(json!({ "a": { "b": 2 } }))
    );
  }

  #[test]
  fn registered_watchers_pick_changes() {
    let root = if cfg!(windows) { "C:/work" } else { "/work" };
    let session = Session::default();
    session.register(Some(&json!({ "registrations": [
      { "id": "files", "method": "workspace/didChangeWatchedFiles", "registerOptions": {
        "watchers": [{ "globPattern": "**/*.{lua,luau}", "kind": 7 }, { "globPattern": "**/.luaurc" }],
      }},
      { "id": "map", "method": "workspace/didChangeWatchedFiles", "registerOptions": {
        "watchers": [{ "globPattern": "**/sourcemap.json", "kind": 2 }],
      }},
      { "id": "other", "method": "textDocument/formatting" },
    ]})));
    let path = |name: &str| PathBuf::from(format!("{root}/{name}"));
    let changes = vec![
      (path("src/a.luau"), FileChange::Created),
      (path("src/.luaurc"), FileChange::Changed),
      (path("sourcemap.json"), FileChange::Changed),
      (path("sourcemap.json"), FileChange::Deleted),
      (path("README.md"), FileChange::Changed),
    ];
    let sent = session.watched_changes(&changes).expect("some are watched");
    let types: Vec<(String, u64)> = sent["changes"]
      .as_array()
      .expect("changes")
      .iter()
      .map(|change| {
        let uri = change["uri"].as_str().expect("uri");
        let name = uri.rsplit('/').next().expect("name").to_owned();
        (name, change["type"].as_u64().expect("type"))
      })
      .collect();
    assert_eq!(
      types,
      [
        ("a.luau".to_owned(), 1),
        (".luaurc".to_owned(), 2),
        ("sourcemap.json".to_owned(), 2),
      ]
    );

    session.unregister(Some(&json!({ "unregisterations": [{ "id": "files" }] })));
    assert!(
      session
        .watched_changes(&[(path("src/a.luau"), FileChange::Changed)])
        .is_none()
    );
  }

  #[test]
  fn relative_patterns_match_under_their_base() {
    let root = if cfg!(windows) { "C:/work" } else { "/work" };
    let base = path_to_uri(Path::new(root)).expect("uri");
    let session = Session::default();
    session.register(Some(&json!({ "registrations": [
      { "id": "r", "method": "workspace/didChangeWatchedFiles", "registerOptions": {
        "watchers": [{ "globPattern": { "baseUri": base.as_str(), "pattern": "src/*.luau" } }],
      }},
    ]})));
    let file = |name: &str| (PathBuf::from(format!("{root}/{name}")), FileChange::Changed);
    assert!(session.watched_changes(&[file("src/a.luau")]).is_some());
    assert!(
      session
        .watched_changes(&[file("src/deep/a.luau")])
        .is_none()
    );
    assert!(
      session
        .watched_changes(&[file("other/src/a.luau")])
        .is_none()
    );
  }
}
