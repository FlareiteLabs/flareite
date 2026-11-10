//! The `on-folder-contains:<pattern>` activation event (API 1.17): an
//! extension starts when the open folder has a file or folder its pattern
//! matches, as VS Code's `workspaceContains`. A Rojo extension, say, starts
//! for `*.project.json`.
//!
//! Patterns are globs relative to the folder, with `/` between names: `*`
//! and `?` don't cross a `/`, `**` matches any number of folders, and
//! `{a,b}` either. The folder is searched when it opens or an extension
//! loads, at most [`MAX_DEPTH`] levels deep (only as deep as the patterns
//! reach, unless one has `**`) and [`MAX_ENTRIES`] entries, leaving out
//! what the user's `files.exclude`, `.gitignore` files, and `.git` leave
//! out. Files created later start an extension too.

use std::path::{Component, Path};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Most entries looked at when searching a folder.
pub const MAX_ENTRIES: usize = 10_000;
/// Deepest a search goes, for `**` patterns.
pub const MAX_DEPTH: usize = 8;

/// Checks a pattern as the manifest gives it.
pub fn check(pattern: &str) -> Result<(), String> {
  if pattern.is_empty() || pattern.len() > 200 {
    return Err("a folder pattern is 1 to 200 characters".into());
  }
  if pattern.starts_with('/') || pattern.contains('\\') || pattern.contains(':') {
    return Err(format!(
      "{pattern:?} must be relative to the folder, with '/' between names"
    ));
  }
  if pattern
    .split('/')
    .any(|part| part == ".." || part.is_empty())
  {
    return Err(format!("{pattern:?} can't contain '..' or empty names"));
  }
  glob(pattern).map(|_| ())
}

fn glob(pattern: &str) -> Result<globset::Glob, String> {
  GlobBuilder::new(pattern)
    .literal_separator(true)
    .build()
    .map_err(|error| format!("{pattern:?} isn't a valid pattern: {error}"))
}

/// Patterns, ready to match paths relative to the folder.
pub struct FolderPatterns {
  set: GlobSet,
  /// How deep a search must go: `None` without limit (up to [`MAX_DEPTH`]).
  depth: Option<usize>,
}

impl FolderPatterns {
  /// `None` without patterns. Invalid ones (which the manifest refuses)
  /// are left out.
  pub fn new<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Option<Self> {
    let mut builder = GlobSetBuilder::new();
    let mut depth = Some(0);
    let mut any = false;
    for pattern in patterns {
      let Ok(glob) = glob(pattern) else {
        continue;
      };
      builder.add(glob);
      any = true;
      depth = if pattern.contains("**") {
        None
      } else {
        depth.map(|depth: usize| depth.max(pattern.split('/').count()))
      };
    }
    let set = builder.build().ok().filter(|_| any)?;
    Some(Self { set, depth })
  }

  /// Whether `relative` (a path inside the folder) matches.
  pub fn matches(&self, relative: &Path) -> bool {
    let relative = relative
      .components()
      .filter_map(|component| match component {
        Component::Normal(name) => name.to_str(),
        _ => None,
      })
      .collect::<Vec<_>>()
      .join("/");
    !relative.is_empty() && self.set.is_match(relative)
  }

  /// Whether the folder at `root` has a file or folder that matches.
  pub fn found_in(&self, root: &Path, exclude: &flareite_workspace::ExcludeFilter) -> bool {
    let depth = self.depth.unwrap_or(MAX_DEPTH).min(MAX_DEPTH);
    let walk = ignore::WalkBuilder::new(root)
      .max_depth(Some(depth))
      .hidden(false)
      .git_global(false)
      .require_git(false)
      .filter_entry({
        let root = root.to_path_buf();
        let exclude = exclude.clone();
        move |entry| entry.file_name() != ".git" && !exclude.is_excluded(&root, entry.path())
      })
      .build();
    for entry in walk.take(MAX_ENTRIES).flatten() {
      if let Ok(relative) = entry.path().strip_prefix(root)
        && self.matches(relative)
      {
        return true;
      }
    }
    false
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn patterns(list: &[&str]) -> FolderPatterns {
    FolderPatterns::new(list.iter().copied()).expect("patterns")
  }

  #[test]
  fn patterns_are_relative_globs() {
    assert!(check("*.project.json").is_ok());
    assert!(check("**/wally.toml").is_ok());
    assert!(check("src/{a,b}.luau").is_ok());
    for bad in ["", "/abs", "a\\b", "../up", "a//b", "C:/x", "[unclosed"] {
      assert!(check(bad).is_err(), "{bad:?}");
    }
  }

  #[test]
  fn stars_stay_within_a_folder_unless_doubled() {
    let root = patterns(&["*.project.json"]);
    assert!(root.matches(Path::new("default.project.json")));
    assert!(!root.matches(Path::new("game/default.project.json")));
    let deep = patterns(&["**/*.project.json"]);
    assert!(deep.matches(Path::new("default.project.json")));
    assert!(deep.matches(Path::new("game/default.project.json")));
    assert_eq!(root.depth, Some(1));
    assert_eq!(patterns(&["a/b.txt", "c"]).depth, Some(2));
    assert_eq!(deep.depth, None);
  }

  #[test]
  fn searches_the_folder_leaving_out_excluded_and_ignored_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("game/nested")).expect("folders");
    std::fs::write(root.join("game/nested/place.project.json"), "{}").expect("file");
    let none = flareite_workspace::ExcludeFilter::new::<&str>(&[]).expect("filter");
    assert!(!patterns(&["*.project.json"]).found_in(root, &none));
    assert!(patterns(&["**/*.project.json"]).found_in(root, &none));
    assert!(patterns(&["game/*/place.project.json"]).found_in(root, &none));

    let exclude = flareite_workspace::ExcludeFilter::new(&["game"]).expect("filter");
    assert!(!patterns(&["**/*.project.json"]).found_in(root, &exclude));

    std::fs::write(root.join(".gitignore"), "game/\n").expect("ignore file");
    assert!(!patterns(&["**/*.project.json"]).found_in(root, &none));

    std::fs::write(root.join(".luaurc"), "{}").expect("hidden file");
    assert!(patterns(&[".luaurc"]).found_in(root, &none));
  }
}
