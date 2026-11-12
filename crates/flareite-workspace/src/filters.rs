use std::path::{Path, PathBuf};

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

/// User-configured paths to hide from the explorer, the file index, and
/// search (for example `.git` or `**/dist/**`).
///
/// Patterns without a `/` match a file or directory name at any depth, like
/// gitignore. Patterns with a `/` match the path relative to the workspace
/// root, with `/` as the separator.
#[derive(Debug, Clone)]
pub struct ExcludeFilter {
  names: GlobSet,
  paths: GlobSet,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid exclude pattern {pattern:?}: {message}")]
pub struct FilterError {
  pub pattern: String,
  pub message: String,
}

impl Default for ExcludeFilter {
  fn default() -> Self {
    Self {
      names: GlobSet::empty(),
      paths: GlobSet::empty(),
    }
  }
}

impl ExcludeFilter {
  pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self, FilterError> {
    let mut names = GlobSetBuilder::new();
    let mut paths = GlobSetBuilder::new();
    for pattern in patterns {
      let pattern = pattern.as_ref().trim_end_matches('/');
      let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
      let error = |error: globset::Error| FilterError {
        pattern: pattern.to_owned(),
        message: error.kind().to_string(),
      };
      if pattern.contains('/') {
        let glob = GlobBuilder::new(pattern.trim_start_matches('/'))
          .literal_separator(true)
          .build()
          .map_err(error)?;
        paths.add(glob);
      } else {
        names.add(Glob::new(pattern).map_err(error)?);
      }
    }
    let build = |builder: GlobSetBuilder| {
      builder.build().map_err(|error| FilterError {
        pattern: String::new(),
        message: error.to_string(),
      })
    };
    Ok(Self {
      names: build(names)?,
      paths: build(paths)?,
    })
  }

  /// Whether `path`, a descendant of `root`, is excluded. Only the path's
  /// own name is checked against name patterns; callers that walk a tree
  /// skip excluded directories, so their contents are never reached.
  pub fn is_excluded(&self, root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
      return false;
    };
    if relative
      .file_name()
      .is_some_and(|name| self.names.is_match(name))
    {
      return true;
    }
    !self.paths.is_empty() && self.paths.is_match(to_slash(relative))
  }

  /// Whether `path`, a descendant of `root`, or a folder it's in (below
  /// `root`) matches: for patterns that include files, where a matching
  /// folder includes what's in it.
  pub fn matches_within(&self, root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
      return false;
    };
    let mut prefix = PathBuf::new();
    for component in relative.components() {
      prefix.push(component);
      if self.names.is_match(component.as_os_str()) {
        return true;
      }
      if !self.paths.is_empty() && self.paths.is_match(to_slash(&prefix)) {
        return true;
      }
    }
    false
  }

  /// Whether it has no patterns.
  pub fn is_empty(&self) -> bool {
    self.names.is_empty() && self.paths.is_empty()
  }
}

/// Patterns typed in a box, as VS Code takes them: separated by commas,
/// except inside braces (`*.{ts,tsx}`), trimmed, without empty ones.
pub fn split_patterns(text: &str) -> Vec<String> {
  let mut patterns = Vec::new();
  let mut current = String::new();
  let mut depth = 0usize;
  for ch in text.chars() {
    match ch {
      '{' => depth += 1,
      '}' => depth = depth.saturating_sub(1),
      ',' if depth == 0 => {
        patterns.push(std::mem::take(&mut current));
        continue;
      }
      _ => {}
    }
    current.push(ch);
  }
  patterns.push(current);
  patterns
    .into_iter()
    .map(|pattern| pattern.trim().to_owned())
    .filter(|pattern| !pattern.is_empty())
    .collect()
}

/// `relative` with `/` separators, for display and pattern matching.
pub fn to_slash(relative: &Path) -> String {
  let mut out = String::new();
  for (index, component) in relative.components().enumerate() {
    if index > 0 {
      out.push('/');
    }
    out.push_str(&component.as_os_str().to_string_lossy());
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn name_patterns_match_at_any_depth() {
    let filter = ExcludeFilter::new(&[".git", "*.log"]).expect("valid");
    let root = Path::new("/w");
    assert!(filter.is_excluded(root, Path::new("/w/.git")));
    assert!(filter.is_excluded(root, Path::new("/w/sub/.git")));
    assert!(filter.is_excluded(root, Path::new("/w/a/b/debug.log")));
    assert!(!filter.is_excluded(root, Path::new("/w/.gitignore")));
  }

  #[test]
  fn path_patterns_match_relative_paths() {
    let filter = ExcludeFilter::new(&["build/out", "**/gen/**"]).expect("valid");
    let root = Path::new("/w");
    assert!(filter.is_excluded(root, Path::new("/w/build/out")));
    assert!(!filter.is_excluded(root, Path::new("/w/other/build/out")));
    assert!(filter.is_excluded(root, Path::new("/w/a/gen/x.rs")));
  }

  #[test]
  fn folders_that_match_take_what_is_in_them() {
    let filter = ExcludeFilter::new(&["src", "./docs/api", "*.md"]).expect("valid");
    let root = Path::new("/w");
    assert!(filter.matches_within(root, Path::new("/w/src/a/b.rs")));
    assert!(filter.matches_within(root, Path::new("/w/x/src/b.rs")));
    assert!(filter.matches_within(root, Path::new("/w/docs/api/index.html")));
    assert!(!filter.matches_within(root, Path::new("/w/docs/guide.html")));
    assert!(filter.matches_within(root, Path::new("/w/docs/guide.md")));
    assert!(!filter.matches_within(root, Path::new("/w/lib/a.rs")));
  }

  #[test]
  fn typed_patterns_split_at_commas_outside_braces() {
    assert_eq!(
      split_patterns(" src/**, *.{ts,tsx} ,, tests "),
      ["src/**", "*.{ts,tsx}", "tests"]
    );
    assert!(split_patterns("  ").is_empty());
  }

  #[test]
  fn invalid_patterns_are_reported() {
    let error = ExcludeFilter::new(&["[unclosed"]).expect_err("invalid");
    assert_eq!(error.pattern, "[unclosed");
  }
}
