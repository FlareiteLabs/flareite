//! Matching files to the schemas for them by patterns, as VS Code's
//! `fileMatch` does: a pattern without '/' matches a file's name (in any
//! folder), one with '/' the end of its path (`.vscode/settings.json`,
//! `**/.github/workflows/*.json`), and one starting with '!' excludes
//! files the others match.

use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Patterns for several schemas, matched together.
pub struct Matcher {
  include: GlobSet,
  /// For each pattern in `include`, the schema it's for.
  include_owners: Vec<usize>,
  exclude: GlobSet,
  exclude_owners: Vec<usize>,
}

/// The glob a pattern stands for, matched against a whole path with '/'.
fn glob_of(pattern: &str) -> String {
  let pattern = pattern.trim().replace('\\', "/");
  let pattern = pattern.trim_start_matches("./");
  if pattern.contains('/') {
    format!(
      "**/{}",
      pattern.trim_start_matches("**/").trim_start_matches('/')
    )
  } else {
    format!("**/{pattern}")
  }
}

impl Matcher {
  /// A matcher for each schema's patterns, by its position in `patterns`.
  /// Patterns that aren't valid globs are left out (and logged).
  pub fn new<'a>(patterns: impl IntoIterator<Item = &'a [String]>) -> Self {
    let mut include = GlobSetBuilder::new();
    let mut include_owners = Vec::new();
    let mut exclude = GlobSetBuilder::new();
    let mut exclude_owners = Vec::new();
    for (owner, list) in patterns.into_iter().enumerate() {
      for pattern in list {
        let (negated, pattern) = match pattern.strip_prefix('!') {
          Some(rest) => (true, rest),
          None => (false, pattern.as_str()),
        };
        if pattern.trim().is_empty() {
          continue;
        }
        let glob = GlobBuilder::new(&glob_of(pattern))
          .literal_separator(true)
          .case_insensitive(cfg!(windows))
          .build();
        match glob {
          Ok(glob) if negated => {
            exclude.add(glob);
            exclude_owners.push(owner);
          }
          Ok(glob) => {
            include.add(glob);
            include_owners.push(owner);
          }
          Err(error) => tracing::debug!(pattern, %error, "a schema's file pattern isn't valid"),
        }
      }
    }
    Self {
      include: include.build().unwrap_or_else(|_| GlobSet::empty()),
      include_owners,
      exclude: exclude.build().unwrap_or_else(|_| GlobSet::empty()),
      exclude_owners,
    }
  }

  /// The schemas (by position) whose patterns match `path`, in order.
  pub fn matches(&self, path: &Path) -> Vec<usize> {
    let text = path.to_string_lossy().replace('\\', "/");
    let excluded: Vec<usize> = self
      .exclude
      .matches(&text)
      .into_iter()
      .map(|index| self.exclude_owners[index])
      .collect();
    let mut owners: Vec<usize> = self
      .include
      .matches(&text)
      .into_iter()
      .map(|index| self.include_owners[index])
      .filter(|owner| !excluded.contains(owner))
      .collect();
    owners.sort_unstable();
    owners.dedup();
    owners
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn list(patterns: &[&str]) -> Vec<String> {
    patterns.iter().map(|&p| p.to_owned()).collect()
  }

  #[test]
  fn names_and_ends_of_paths_match() {
    let schemas = [
      list(&["package.json"]),
      list(&[".vscode/settings.json"]),
      list(&["*.myconf.toml", "!secret.myconf.toml"]),
      list(&["**/.github/workflows/*.json", "/abs/only.json"]),
    ];
    let matcher = Matcher::new(schemas.iter().map(Vec::as_slice));
    let at = |path: &str| matcher.matches(Path::new(path));
    assert_eq!(at("/p/package.json"), [0]);
    assert_eq!(at("/p/sub/package.json"), [0]);
    assert!(at("/p/package.json.bak").is_empty());
    assert_eq!(at("/p/.vscode/settings.json"), [1]);
    assert!(at("/p/settings.json").is_empty());
    assert_eq!(at("/p/a.myconf.toml"), [2]);
    assert!(at("/p/secret.myconf.toml").is_empty());
    assert_eq!(at("/r/.github/workflows/ci.json"), [3]);
    assert_eq!(at("/x/abs/only.json"), [3]);
    if cfg!(windows) {
      assert_eq!(at(r"C:\p\Package.JSON"), [0]);
    }
  }
}
