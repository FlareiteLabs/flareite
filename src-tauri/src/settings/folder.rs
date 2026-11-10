//! A folder's own settings: `.flareite/settings.toml` in the folder open in
//! the editor, applied over the user's settings while the folder is open
//! and trusted.
//!
//! A folder can change only settings that are safe to take from its files
//! (see [`applies`]): how the editor edits and searches, its schemas,
//! whether language servers run and their settings (not their commands),
//! and extensions' settings. The rest (what
//! programs run, themes, keybindings, updates, trust itself) stay the
//! user's; a folder's file that sets them is reported, and they're
//! ignored. Nothing applies until the user trusts the folder, as its files
//! could have been written by anyone.

use std::path::{Path, PathBuf};

use super::{SchemaAssociation, Settings};

/// The folder in an open folder that holds Flareite's files for it.
pub const DIR: &str = ".flareite";
pub const FILE_NAME: &str = "settings.toml";

/// Written when a folder's settings file is first created.
pub const TEMPLATE: &str = r##"# This folder's settings, applied over your settings while it's open and
# trusted. Only some settings can be changed here: [editor], [files],
# [search], [schemas], lsp.enabled, language servers' settings
# ([lsp.servers.<name>.settings]), and [extensions.settings]. Save to apply.
"##;

/// Whether a folder's settings may set the setting at `path` (its keys).
pub fn applies(path: &[&str]) -> bool {
  matches!(
    path,
    ["editor" | "files" | "search" | "schemas", ..]
      | ["lsp", "enabled"]
      | ["lsp", "servers", _, "settings", ..]
      | ["extensions", "settings", ..]
  )
}

/// The settings file of the folder at `root`.
pub fn file_of(root: &Path) -> PathBuf {
  root.join(DIR).join(FILE_NAME)
}

/// A folder's settings, as read.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderSettings {
  /// The folder.
  pub root: PathBuf,
  /// Its settings file.
  pub path: PathBuf,
  /// Whether the file exists.
  pub exists: bool,
  /// Whether the user trusts the folder; its settings apply only then.
  pub trusted: bool,
  /// What the file sets that a folder can, without
  /// `schemas.associations` (see `associations`).
  pub applied: toml::Table,
  /// Everything the file sets, for the settings editor.
  pub table: toml::Table,
  /// Settings the file sets that only the user's settings can, as dotted
  /// keys.
  pub ignored: Vec<String>,
  /// Its schemas for files, with paths relative to the folder.
  pub associations: Vec<SchemaAssociation>,
  /// Why its settings don't apply, if they can't: the file can't be read,
  /// isn't TOML, or has invalid values.
  pub error: Option<String>,
}

impl FolderSettings {
  /// Reads the settings of the folder at `root`.
  pub fn read(root: &Path, trusted: bool) -> Self {
    let path = file_of(root);
    let mut folder = Self {
      root: root.to_path_buf(),
      path: path.clone(),
      exists: false,
      trusted,
      applied: toml::Table::new(),
      table: toml::Table::new(),
      ignored: Vec::new(),
      associations: Vec::new(),
      error: None,
    };
    let text = match std::fs::read_to_string(&path) {
      Ok(text) => text,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return folder,
      Err(error) => {
        folder.exists = path.exists();
        folder.error = Some(format!("Could not read {}: {error}", path.display()));
        return folder;
      }
    };
    folder.exists = true;
    folder.load(&text);
    folder
  }

  /// Takes the file's text: what applies, what doesn't, and whether it's
  /// valid on its own.
  pub fn load(&mut self, text: &str) {
    self.applied.clear();
    self.table.clear();
    self.ignored.clear();
    self.associations.clear();
    self.error = None;
    let table: toml::Table = match text.parse() {
      Ok(table) => table,
      Err(error) => {
        self.error = Some(error.to_string());
        return;
      }
    };
    let (mut applied, ignored) = split(&table);
    self.associations = take_associations(&mut applied);
    self.table = table;
    self.ignored = ignored;
    // The values must be valid on their own, whatever the user sets.
    if let Err(error) = Settings::from_table(merged(&toml::Table::new(), &applied)) {
      self.error = Some(error);
      return;
    }
    if let Err(error) = check_associations(&self.associations) {
      self.error = Some(error);
      return;
    }
    self.applied = applied;
  }

  /// Whether its settings are in effect.
  pub fn in_effect(&self) -> bool {
    self.exists && self.trusted && self.error.is_none()
  }
}

/// Splits what a folder's file sets into what applies and the dotted keys
/// of what doesn't.
pub fn split(table: &toml::Table) -> (toml::Table, Vec<String>) {
  let mut applied = toml::Table::new();
  let mut ignored = Vec::new();
  for (section, value) in table {
    match (section.as_str(), value) {
      ("editor" | "files" | "search" | "schemas", _) => {
        applied.insert(section.clone(), value.clone());
      }
      ("lsp" | "extensions", toml::Value::Table(entries)) => {
        let keep = if section == "lsp" {
          "enabled"
        } else {
          "settings"
        };
        let mut kept = toml::Table::new();
        for (key, value) in entries {
          if key == keep {
            kept.insert(key.clone(), value.clone());
          } else if section == "lsp"
            && key == "servers"
            && let toml::Value::Table(servers) = value
          {
            // Only servers' settings: their commands stay the user's.
            let mut kept_servers = toml::Table::new();
            for (name, server) in servers {
              let toml::Value::Table(server) = server else {
                ignored.push(format!("lsp.servers.{name}"));
                continue;
              };
              for (field, value) in server {
                if field == "settings" {
                  let mut only = toml::Table::new();
                  only.insert(field.clone(), value.clone());
                  kept_servers.insert(name.clone(), toml::Value::Table(only));
                } else {
                  ignored.push(format!("lsp.servers.{name}.{field}"));
                }
              }
            }
            if !kept_servers.is_empty() {
              kept.insert(key.clone(), toml::Value::Table(kept_servers));
            }
          } else {
            ignored.push(format!("{section}.{key}"));
          }
        }
        if !kept.is_empty() {
          applied.insert(section.clone(), toml::Value::Table(kept));
        }
      }
      _ => ignored.push(section.clone()),
    }
  }
  (applied, ignored)
}

/// Removes `schemas.associations` from a folder's settings: they're added
/// to the user's, with paths relative to the folder, not replacing them.
fn take_associations(applied: &mut toml::Table) -> Vec<SchemaAssociation> {
  let Some(toml::Value::Table(schemas)) = applied.get_mut("schemas") else {
    return Vec::new();
  };
  let Some(associations) = schemas.remove("associations") else {
    return Vec::new();
  };
  if schemas.is_empty() {
    applied.remove("schemas");
  }
  associations.try_into().unwrap_or_else(|_| {
    // Checked by `check_associations` through the error below.
    vec![SchemaAssociation {
      files: Vec::new(),
      schema: String::new(),
    }]
  })
}

fn check_associations(associations: &[SchemaAssociation]) -> Result<(), String> {
  for association in associations {
    association.check()?;
  }
  Ok(())
}

/// `base` with `over` laid on it: tables merge, other values replace.
pub fn merged(base: &toml::Table, over: &toml::Table) -> toml::Table {
  let mut result = base.clone();
  for (key, value) in over {
    match (result.get_mut(key), value) {
      (Some(toml::Value::Table(inner)), toml::Value::Table(more)) => {
        *inner = merged(inner, more);
      }
      _ => {
        result.insert(key.clone(), value.clone());
      }
    }
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  fn table(text: &str) -> toml::Table {
    text.parse().expect("toml")
  }

  #[test]
  fn folders_change_only_safe_settings() {
    let (applied, ignored) = split(&table(
      "[editor]\ntab_size = 2\n[lsp]\nenabled = false\n[lsp.servers.x]\ncommand = \"evil\"\nsettings = { \"x.mode\" = \"a\" }\n[extensions]\ndevelopment = [\"x\"]\n[extensions.settings.\"a.b\"]\nc = 1\n[terminal]\nscrollback = 5\n[keybindings]\n\"a\" = \"b\"\n",
    ));
    assert_eq!(
      applied,
      table(
        "[editor]\ntab_size = 2\n[lsp]\nenabled = false\n[lsp.servers.x.settings]\n\"x.mode\" = \"a\"\n[extensions.settings.\"a.b\"]\nc = 1\n"
      )
    );
    assert_eq!(
      ignored,
      [
        "extensions.development",
        "keybindings",
        "lsp.servers.x.command",
        "terminal"
      ]
    );
    assert!(applies(&["editor", "tab_size"]));
    assert!(applies(&["extensions", "settings", "a.b", "c"]));
    assert!(!applies(&["lsp", "servers"]));
    assert!(!applies(&["lsp", "servers", "x", "command"]));
    assert!(applies(&["lsp", "servers", "x", "settings", "x.mode"]));
    assert!(!applies(&["workbench", "theme"]));
  }

  #[test]
  fn tables_merge_and_values_replace() {
    let user = table("[editor]\ntab_size = 4\nfont_size = 12\n[files]\nexclude = [\"a\"]\n");
    let folder = table("[editor]\ntab_size = 2\n[files]\nexclude = [\"b\"]\n");
    assert_eq!(
      merged(&user, &folder),
      table("[editor]\ntab_size = 2\nfont_size = 12\n[files]\nexclude = [\"b\"]\n")
    );
  }

  #[test]
  fn folder_files_are_read_and_checked() {
    let mut folder = FolderSettings::read(Path::new("/nonexistent-folder"), true);
    assert!(!folder.exists);
    assert!(!folder.in_effect());
    folder.exists = true;
    folder.load("[editor]\ntab_size = 2\n[[schemas.associations]]\nfiles = [\"*.x.toml\"]\nschema = \"x.tosd\"\n");
    assert!(folder.in_effect());
    assert_eq!(folder.associations.len(), 1);
    assert!(!folder.applied.contains_key("schemas"));
    let mut invalid = folder.clone();
    invalid.load("[editor]\ntab_size = 99\n");
    assert!(
      invalid
        .error
        .as_deref()
        .is_some_and(|e| e.contains("tab_size"))
    );
    assert!(!invalid.in_effect());
    let mut untrusted = folder.clone();
    untrusted.trusted = false;
    assert!(!untrusted.in_effect());
  }
}
