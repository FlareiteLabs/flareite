//! `.editorconfig` files (<https://editorconfig.org>): how a file is to be
//! indented and tidied, set for a project rather than by each editor's
//! settings (ADR 0054).
//!
//! A file's properties come from the `.editorconfig` files in its folder and
//! the folders above it, up to one that says `root = true`. Files nearer the
//! file win over those above, and later sections over earlier ones. A
//! section's glob without a `/` matches a file name at any depth below its
//! `.editorconfig`; one with a `/` matches the path from there. `unset`
//! takes a property back out.
//!
//! The properties Flareite uses are `indent_style`, `indent_size`,
//! `tab_width`, `trim_trailing_whitespace`, and `insert_final_newline`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use globset::GlobBuilder;

const FILE_NAME: &str = ".editorconfig";
/// Larger `.editorconfig` files are ignored.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// A numeric range in a glob (`{1..3}`) wider than this is left as it is.
const MAX_RANGE: i64 = 1000;

/// A file's properties: those it was given, by lowercase name, with their
/// values (lowercase for the properties whose values are words).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditorConfig {
  properties: HashMap<String, String>,
}

impl EditorConfig {
  pub fn is_empty(&self) -> bool {
    self.properties.is_empty()
  }

  /// A property's value, if it was given.
  pub fn get(&self, name: &str) -> Option<&str> {
    self.properties.get(name).map(String::as_str)
  }

  fn flag(&self, name: &str) -> Option<bool> {
    match self.get(name)? {
      "true" => Some(true),
      "false" => Some(false),
      _ => None,
    }
  }

  fn number(&self, name: &str) -> Option<usize> {
    self
      .get(name)?
      .parse()
      .ok()
      .filter(|&n: &usize| (1..=64).contains(&n))
  }

  /// Whether to indent with spaces (`indent_style = space`) or tabs.
  pub fn insert_spaces(&self) -> Option<bool> {
    match self.get("indent_style")? {
      "space" => Some(true),
      "tab" => Some(false),
      _ => None,
    }
  }

  /// The width of a tab, and of an indentation level with spaces:
  /// `indent_size`, or `tab_width` (which `indent_size = tab` names).
  pub fn tab_size(&self) -> Option<usize> {
    let indent_size = self.number("indent_size");
    let tab_width = self.number("tab_width");
    match self.insert_spaces() {
      Some(false) => tab_width.or(indent_size),
      _ => indent_size.or(tab_width),
    }
  }

  pub fn trim_trailing_whitespace(&self) -> Option<bool> {
    self.flag("trim_trailing_whitespace")
  }

  pub fn insert_final_newline(&self) -> Option<bool> {
    self.flag("insert_final_newline")
  }
}

/// One `.editorconfig` file, parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ConfigFile {
  root: bool,
  /// Each section's glob and properties, in order.
  sections: Vec<(String, Vec<(String, String)>)>,
}

/// Values compared as words; others (globs, numbers) are kept as written.
const WORD_VALUES: &[&str] = &[
  "indent_style",
  "indent_size",
  "end_of_line",
  "charset",
  "trim_trailing_whitespace",
  "insert_final_newline",
  "root",
];

fn parse(text: &str) -> ConfigFile {
  let mut file = ConfigFile::default();
  for line in text.lines() {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
      continue;
    }
    if let Some(glob) = line
      .strip_prefix('[')
      .and_then(|rest| rest.strip_suffix(']'))
    {
      file.sections.push((glob.to_owned(), Vec::new()));
      continue;
    }
    let Some((key, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
      continue;
    };
    let key = key.trim().to_ascii_lowercase();
    let mut value = value.trim().to_owned();
    if WORD_VALUES.contains(&key.as_str()) {
      value.make_ascii_lowercase();
    }
    match file.sections.last_mut() {
      Some((_, properties)) => properties.push((key, value)),
      // Before any section: only `root` means something.
      None if key == "root" => file.root = value == "true",
      None => {}
    }
  }
  file
}

/// `{1..3}` in `glob` as `{1,2,3}`, which `globset` understands.
fn expand_ranges(glob: &str) -> String {
  let mut out = String::new();
  let mut rest = glob;
  while let Some(open) = rest.find('{') {
    out.push_str(&rest[..open]);
    let after = &rest[open + 1..];
    let Some(close) = after.find('}') else {
      out.push_str(&rest[open..]);
      return out;
    };
    let inner = &after[..close];
    let range = inner
      .split_once("..")
      .and_then(|(from, to)| Some((from.parse::<i64>().ok()?, to.parse::<i64>().ok()?)));
    match range {
      Some((from, to)) if from <= to && to - from <= MAX_RANGE => {
        let numbers: Vec<String> = (from..=to).map(|n| n.to_string()).collect();
        out.push('{');
        out.push_str(&numbers.join(","));
        out.push('}');
      }
      _ => {
        out.push('{');
        out.push_str(inner);
        out.push('}');
      }
    }
    rest = &after[close + 1..];
  }
  out.push_str(rest);
  out
}

/// Whether section `glob` of an `.editorconfig` matches `relative`, the
/// file's path from the `.editorconfig`'s folder with `/` separators.
fn matches(glob: &str, relative: &str) -> bool {
  let glob = expand_ranges(glob);
  let glob = if glob.contains('/') {
    glob.trim_start_matches('/').to_owned()
  } else {
    format!("**/{glob}")
  };
  GlobBuilder::new(&glob)
    .literal_separator(true)
    .build()
    .is_ok_and(|glob| glob.compile_matcher().is_match(relative))
}

/// The properties `files` (each a folder and its `.editorconfig`, nearest
/// last) give `path`.
fn resolve(files: &[(PathBuf, ConfigFile)], path: &Path) -> EditorConfig {
  let mut properties = HashMap::new();
  for (folder, file) in files {
    let Ok(relative) = path.strip_prefix(folder) else {
      continue;
    };
    let relative = crate::filters::to_slash(relative);
    for (glob, section) in &file.sections {
      if matches(glob, &relative) {
        for (key, value) in section {
          if value == "unset" {
            properties.remove(key);
          } else {
            properties.insert(key.clone(), value.clone());
          }
        }
      }
    }
  }
  EditorConfig { properties }
}

/// The `.editorconfig` properties for the file at `path` (absolute), read
/// from its folder and those above it. Files that can't be read are
/// skipped.
pub fn editorconfig_for(path: &Path) -> EditorConfig {
  let mut files = Vec::new();
  let mut folder = path.parent();
  while let Some(dir) = folder {
    let candidate = dir.join(FILE_NAME);
    let readable =
      fs::metadata(&candidate).is_ok_and(|meta| meta.is_file() && meta.len() <= MAX_FILE_BYTES);
    if readable && let Ok(text) = fs::read_to_string(&candidate) {
      let file = parse(&text);
      let root = file.root;
      files.push((dir.to_path_buf(), file));
      if root {
        break;
      }
    }
    folder = dir.parent();
  }
  files.reverse();
  resolve(&files, path)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_sections_and_the_root_flag() {
    let file = parse(
      "# comment\nroot = TRUE\n\n[*]\nindent_style = Space\nindent_size=2\n\n[*.{md,txt}]\n; another\ntrim_trailing_whitespace = false\n",
    );
    assert!(file.root);
    assert_eq!(file.sections.len(), 2);
    assert_eq!(
      file.sections[0].1,
      [
        ("indent_style".to_owned(), "space".to_owned()),
        ("indent_size".to_owned(), "2".to_owned())
      ]
    );
  }

  #[test]
  fn globs_match_names_anywhere_or_paths_from_the_folder() {
    assert!(matches("*.rs", "src/main.rs"));
    assert!(matches("*", "a/b/c.txt"));
    assert!(matches("*.{js,ts}", "web/app.ts"));
    assert!(matches("src/*.rs", "src/main.rs"));
    assert!(!matches("src/*.rs", "src/net/http.rs"));
    assert!(matches("src/**/*.rs", "src/net/http.rs"));
    assert!(matches("/Makefile", "Makefile"));
    assert!(!matches("/Makefile", "sub/Makefile"));
    assert!(matches("file{1..3}.txt", "file2.txt"));
    assert!(!matches("file{1..3}.txt", "file4.txt"));
  }

  #[test]
  fn nearer_files_and_later_sections_win() {
    let top =
      parse("root = true\n[*]\nindent_style = tab\ntab_width = 8\ninsert_final_newline = true\n");
    let sub = parse(
      "[*.py]\nindent_style = space\nindent_size = 4\n[test_*.py]\ninsert_final_newline = unset\n",
    );
    let files = [(PathBuf::from("/p"), top), (PathBuf::from("/p/py"), sub)];
    let config = resolve(&files, Path::new("/p/py/test_a.py"));
    assert_eq!(config.insert_spaces(), Some(true));
    assert_eq!(config.tab_size(), Some(4));
    assert_eq!(config.insert_final_newline(), None, "unset");
    let other = resolve(&files, Path::new("/p/README"));
    assert_eq!(
      (other.insert_spaces(), other.tab_size()),
      (Some(false), Some(8))
    );
    assert_eq!(other.insert_final_newline(), Some(true));
  }

  #[test]
  fn indent_size_tab_takes_the_tab_width() {
    let file = parse("[*]\nindent_style = tab\nindent_size = tab\ntab_width = 3\n");
    let config = resolve(&[(PathBuf::from("/p"), file)], Path::new("/p/a"));
    assert_eq!(config.tab_size(), Some(3));
  }

  #[test]
  fn reads_files_up_to_the_root() {
    let dir = tempfile::tempdir().expect("temp dir");
    let outer = dir.path().join("outer");
    let project = outer.join("project");
    fs::create_dir_all(project.join("src")).expect("mkdir");
    // Above the root: never read.
    fs::write(
      outer.join(FILE_NAME),
      "[*]\ntrim_trailing_whitespace = true\n",
    )
    .expect("write");
    fs::write(
      project.join(FILE_NAME),
      "root = true\n[*]\nindent_size = 2\n",
    )
    .expect("write");
    fs::write(
      project.join("src").join(FILE_NAME),
      "[*.rs]\nindent_size = 4\n",
    )
    .expect("write");
    let config = editorconfig_for(&project.join("src").join("main.rs"));
    assert_eq!(config.tab_size(), Some(4));
    assert_eq!(config.trim_trailing_whitespace(), None);
    assert_eq!(editorconfig_for(&project.join("a.txt")).tab_size(), Some(2));
  }
}
