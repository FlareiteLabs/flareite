//! Find in files: searches every non-ignored, non-excluded text file in the
//! workspace, reporting results file by file as they are found.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use flareite_text::{Searcher, TextSnapshot};
use ignore::{WalkBuilder, WalkState};

use crate::filters::{ExcludeFilter, to_slash};

/// Longest preview of a matching line, in characters, around the match.
const PREVIEW_CHARS: usize = 200;
/// Bytes inspected for NUL to detect binary files.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct SearchLimits {
  /// Files larger than this are skipped.
  pub max_file_bytes: u64,
  /// The search stops after this many matches.
  pub max_matches: usize,
  /// Matches per file beyond this are not reported.
  pub max_matches_per_file: usize,
  /// Files searched in parallel. Opening and reading many small files
  /// dominates, so a few threads help a lot (see docs/performance.md).
  pub threads: usize,
}

impl Default for SearchLimits {
  fn default() -> Self {
    Self {
      max_file_bytes: 4 * 1024 * 1024,
      max_matches: 20_000,
      max_matches_per_file: 1_000,
      threads: std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, 4),
    }
  }
}

/// Which files a search looks in.
#[derive(Debug, Clone, Default)]
pub struct SearchScope {
  /// `files.exclude`, which also turns on `.gitignore` files; `None` for
  /// neither.
  pub ignored: Option<ExcludeFilter>,
  /// Files to look in: those that match, or are in a folder that does.
  /// `None` for every file.
  pub include: Option<ExcludeFilter>,
  /// Files and folders to leave out.
  pub exclude: ExcludeFilter,
}

impl SearchScope {
  /// Every file but those `files.exclude` and `.gitignore` files leave out.
  pub fn excluding(ignored: ExcludeFilter) -> Self {
    Self {
      ignored: Some(ignored),
      ..Self::default()
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMatch {
  /// Zero-based line number.
  pub line: usize,
  /// Match start and end on that line, in UTF-16 columns. Matches spanning
  /// lines end at the end of the first line.
  pub start_column: usize,
  pub end_column: usize,
  /// The line's text, shortened around the match if long.
  pub preview: String,
  /// The match within `preview`, in UTF-16 columns.
  pub preview_start: usize,
  pub preview_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMatches {
  pub path: PathBuf,
  pub relative: String,
  pub matches: Vec<LineMatch>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchSummary {
  pub files_searched: usize,
  pub files_with_matches: usize,
  pub matches: usize,
  /// The match limit was reached.
  pub truncated: bool,
  pub cancelled: bool,
}

/// Searches the files of the workspace that `scope` takes, on
/// `limits.threads` threads. `.git` is always left out. Files in `open` are
/// searched as they are in the editor (including unsaved changes) instead of
/// on disk. Binary, non-UTF-8, and oversized files are skipped. Results
/// arrive file by file, in no particular order. Stops early when `cancel` is
/// set or the match limit is reached.
pub fn search_workspace(
  root: &Path,
  scope: &SearchScope,
  searcher: &Searcher,
  open: &HashMap<PathBuf, TextSnapshot>,
  limits: SearchLimits,
  cancel: &AtomicBool,
  on_file: impl FnMut(FileMatches) + Send,
) -> SearchSummary {
  let filter_root = root.to_path_buf();
  let filter = scope.clone();
  let ignore_files = scope.ignored.is_some();
  let walker = WalkBuilder::new(root)
    .hidden(false)
    .git_ignore(ignore_files)
    .git_global(ignore_files)
    .git_exclude(ignore_files)
    .ignore(ignore_files)
    .parents(ignore_files)
    .threads(limits.threads.max(1))
    .filter_entry(move |entry| {
      let path = entry.path();
      entry.file_name() != ".git"
        && !filter.exclude.is_excluded(&filter_root, path)
        && !filter
          .ignored
          .as_ref()
          .is_some_and(|ignored| ignored.is_excluded(&filter_root, path))
    })
    .build_parallel();

  let on_file = Mutex::new(on_file);
  let files_searched = AtomicUsize::new(0);
  let files_with_matches = AtomicUsize::new(0);
  // Matches reserved so far; may overshoot the limit by what a thread could not use.
  let reserved = AtomicUsize::new(0);
  let limit_reached = AtomicBool::new(false);

  walker.run(|| {
    Box::new(|entry| {
      if cancel.load(Ordering::Relaxed) || limit_reached.load(Ordering::Relaxed) {
        return WalkState::Quit;
      }
      let Ok(entry) = entry else {
        return WalkState::Continue;
      };
      if !entry.file_type().is_some_and(|kind| kind.is_file()) {
        return WalkState::Continue;
      }
      let path = entry.path();
      if scope
        .include
        .as_ref()
        .is_some_and(|include| !include.matches_within(root, path))
      {
        return WalkState::Continue;
      }
      let Some(text) = read_searchable(path, open, limits.max_file_bytes) else {
        return WalkState::Continue;
      };
      files_searched.fetch_add(1, Ordering::Relaxed);

      let ranges: Vec<_> = searcher
        .find_in_str(&text)
        .take(limits.max_matches_per_file)
        .collect();
      if ranges.is_empty() {
        return WalkState::Continue;
      }
      let before = reserved.fetch_add(ranges.len(), Ordering::Relaxed);
      let allowed = ranges.len().min(limits.max_matches.saturating_sub(before));
      if before + ranges.len() >= limits.max_matches {
        limit_reached.store(true, Ordering::Relaxed);
      }
      if allowed == 0 {
        return WalkState::Quit;
      }
      let matches = locate(&text, &ranges[..allowed]);
      files_with_matches.fetch_add(1, Ordering::Relaxed);
      let mut on_file = on_file.lock().unwrap_or_else(PoisonError::into_inner);
      on_file(FileMatches {
        path: path.to_path_buf(),
        relative: path.strip_prefix(root).map(to_slash).unwrap_or_default(),
        matches,
      });
      WalkState::Continue
    })
  });

  SearchSummary {
    files_searched: files_searched.into_inner(),
    files_with_matches: files_with_matches.into_inner(),
    matches: reserved.into_inner().min(limits.max_matches),
    truncated: limit_reached.into_inner(),
    cancelled: cancel.load(Ordering::Relaxed),
  }
}

fn read_searchable(
  path: &Path,
  open: &HashMap<PathBuf, TextSnapshot>,
  max_bytes: u64,
) -> Option<String> {
  if let Some(snapshot) = open.get(path) {
    return Some(snapshot.chunks().collect());
  }
  if fs::metadata(path).ok()?.len() > max_bytes {
    return None;
  }
  let bytes = fs::read(path).ok()?;
  if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
    return None;
  }
  let text = String::from_utf8(bytes).ok()?;
  // As the editor reads it: without a byte order mark, so columns agree.
  Some(match text.strip_prefix('\u{feff}') {
    Some(rest) => rest.to_owned(),
    None => text,
  })
}

/// Converts byte ranges (sorted) into line/column matches with previews.
/// Lines break at LF, CRLF, and CR, like the editor.
fn locate(text: &str, ranges: &[std::ops::Range<usize>]) -> Vec<LineMatch> {
  let bytes = text.as_bytes();
  let (mut line, mut line_start, mut scanned) = (0, 0, 0);
  ranges
    .iter()
    .map(|range| {
      while scanned < range.start {
        match bytes[scanned] {
          b'\n' => {
            line += 1;
            line_start = scanned + 1;
          }
          b'\r' if bytes.get(scanned + 1) != Some(&b'\n') => {
            line += 1;
            line_start = scanned + 1;
          }
          _ => {}
        }
        scanned += 1;
      }
      let line_end = text[line_start..]
        .find(['\n', '\r'])
        .map_or(text.len(), |end| line_start + end);
      let end = range.end.min(line_end);
      let line_text = &text[line_start..line_end];
      let start_column = utf16_len(&text[line_start..range.start]);
      let end_column = start_column + utf16_len(&text[range.start..end]);
      let (preview, offset) = preview_window(line_text, range.start - line_start);
      let preview_start = start_column - offset;
      LineMatch {
        line,
        start_column,
        end_column,
        preview_end: (preview_start + end_column - start_column).min(utf16_len(&preview)),
        preview,
        preview_start,
      }
    })
    .collect()
}

/// The line, or a window of it around byte `at` if the line is long, plus
/// the UTF-16 length of the text cut from the start.
fn preview_window(line: &str, at: usize) -> (String, usize) {
  if line.chars().count() <= PREVIEW_CHARS {
    return (line.to_owned(), 0);
  }
  let before = line[..at]
    .chars()
    .rev()
    .take(PREVIEW_CHARS / 4)
    .map(char::len_utf8)
    .sum::<usize>();
  let start = at - before;
  let window: String = line[start..].chars().take(PREVIEW_CHARS).collect();
  (window, utf16_len(&line[..start]))
}

fn utf16_len(text: &str) -> usize {
  text.chars().map(char::len_utf16).sum()
}

#[cfg(test)]
mod tests {
  use flareite_text::{SearchQuery, TextBuffer};

  use super::*;

  fn searcher(pattern: &str) -> Searcher {
    SearchQuery {
      pattern: pattern.into(),
      ..SearchQuery::default()
    }
    .compile()
    .expect("compiles")
  }

  fn run(
    root: &Path,
    pattern: &str,
    open: &HashMap<PathBuf, TextSnapshot>,
    limits: SearchLimits,
  ) -> (Vec<FileMatches>, SearchSummary) {
    run_in(
      root,
      pattern,
      open,
      limits,
      &SearchScope::excluding(ExcludeFilter::default()),
    )
  }

  fn run_in(
    root: &Path,
    pattern: &str,
    open: &HashMap<PathBuf, TextSnapshot>,
    limits: SearchLimits,
    scope: &SearchScope,
  ) -> (Vec<FileMatches>, SearchSummary) {
    let mut files = Vec::new();
    let summary = search_workspace(
      root,
      scope,
      &searcher(pattern),
      open,
      limits,
      &AtomicBool::new(false),
      |file| files.push(file),
    );
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    (files, summary)
  }

  #[test]
  fn finds_matches_with_lines_columns_and_previews() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(
      dir.path().join("a.rs"),
      "fn main() {\r\n  let 🦀 = needle;\r\n}\n",
    )
    .expect("write");
    fs::write(dir.path().join("b.txt"), "no match here").expect("write");

    let (files, summary) = run(
      dir.path(),
      "needle",
      &HashMap::new(),
      SearchLimits::default(),
    );
    assert_eq!(summary.files_searched, 2);
    assert_eq!(summary.matches, 1);
    assert_eq!(files.len(), 1);
    let found = &files[0].matches[0];
    assert_eq!(files[0].relative, "a.rs");
    assert_eq!(
      (found.line, found.start_column, found.end_column),
      (1, 11, 17)
    );
    assert_eq!(found.preview, "  let 🦀 = needle;");
    assert_eq!((found.preview_start, found.preview_end), (11, 17));
  }

  #[test]
  fn skips_ignored_binary_and_large_files() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    fs::create_dir(root.join(".git")).expect("mkdir");
    fs::write(root.join(".gitignore"), "ignored.txt\n").expect("write");
    fs::write(root.join("ignored.txt"), "needle").expect("write");
    fs::write(root.join("binary.bin"), b"needle\0").expect("write");
    fs::write(root.join("big.txt"), "needle ".repeat(100)).expect("write");
    fs::write(root.join("kept.txt"), "needle").expect("write");

    let limits = SearchLimits {
      max_file_bytes: 100,
      ..SearchLimits::default()
    };
    let (files, _) = run(root, "needle", &HashMap::new(), limits);
    let names: Vec<_> = files.iter().map(|f| f.relative.as_str()).collect();
    assert_eq!(names, vec!["kept.txt"]);
  }

  #[test]
  fn looks_only_where_the_patterns_say() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    for path in ["src/a.rs", "src/tests/b.rs", "docs/c.md", "d.rs"] {
      fs::create_dir_all(root.join(path).parent().expect("parent")).expect("mkdir");
      fs::write(root.join(path), "needle").expect("write");
    }
    let scope = |include: &[&str], exclude: &[&str]| SearchScope {
      ignored: Some(ExcludeFilter::default()),
      include: (!include.is_empty()).then(|| ExcludeFilter::new(include).expect("valid")),
      exclude: ExcludeFilter::new(exclude).expect("valid"),
    };
    let found = |scope: &SearchScope| {
      let (files, _) = run_in(
        root,
        "needle",
        &HashMap::new(),
        SearchLimits::default(),
        scope,
      );
      files.into_iter().map(|f| f.relative).collect::<Vec<_>>()
    };
    assert_eq!(found(&scope(&["src"], &[])), ["src/a.rs", "src/tests/b.rs"]);
    assert_eq!(found(&scope(&["src"], &["tests"])), ["src/a.rs"]);
    assert_eq!(found(&scope(&["*.rs"], &["src/**"])), ["d.rs"]);
    assert_eq!(found(&scope(&[], &["*.rs"])), ["docs/c.md"]);
  }

  #[test]
  fn ignore_files_apply_only_when_asked() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    fs::create_dir(root.join(".git")).expect("mkdir");
    fs::write(root.join(".git").join("config"), "needle").expect("write");
    fs::write(root.join(".gitignore"), "ignored.txt\n").expect("write");
    fs::write(root.join("ignored.txt"), "needle").expect("write");
    fs::write(root.join("kept.txt"), "needle").expect("write");
    let everything = SearchScope::default();
    let (files, _) = run_in(
      root,
      "needle",
      &HashMap::new(),
      SearchLimits::default(),
      &everything,
    );
    let names: Vec<_> = files.iter().map(|f| f.relative.as_str()).collect();
    assert_eq!(names, ["ignored.txt", "kept.txt"], "never in .git");
  }

  #[test]
  fn a_byte_order_mark_is_not_a_column() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(dir.path().join("bom.txt"), "\u{feff}ab needle").expect("write");
    let (files, _) = run(
      dir.path(),
      "needle",
      &HashMap::new(),
      SearchLimits::default(),
    );
    assert_eq!(files[0].matches[0].start_column, 3);
  }

  #[test]
  fn searches_open_documents_as_edited() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("open.txt");
    fs::write(&path, "old text").expect("write");
    let open = HashMap::from([(path, TextBuffer::from_text("new needle").snapshot())]);
    let (files, _) = run(dir.path(), "needle", &open, SearchLimits::default());
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].matches[0].preview, "new needle");
  }

  #[test]
  fn stops_at_the_match_limit() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(dir.path().join("many.txt"), "x\n".repeat(50)).expect("write");
    let limits = SearchLimits {
      max_matches: 10,
      ..SearchLimits::default()
    };
    let (files, summary) = run(dir.path(), "x", &HashMap::new(), limits);
    assert_eq!(files[0].matches.len(), 10);
    assert!(summary.truncated);
  }

  #[test]
  fn cancellation_stops_the_search() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(dir.path().join("a.txt"), "needle").expect("write");
    let summary = search_workspace(
      dir.path(),
      &SearchScope::default(),
      &searcher("needle"),
      &HashMap::new(),
      SearchLimits::default(),
      &AtomicBool::new(true),
      |_| panic!("no results after cancellation"),
    );
    assert!(summary.cancelled);
  }

  #[test]
  fn long_lines_get_a_window_preview() {
    let line = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
    let matches = locate(&line, std::slice::from_ref(&(500..506)));
    let found = &matches[0];
    assert!(found.preview.chars().count() <= PREVIEW_CHARS);
    let units: Vec<u16> = found.preview.encode_utf16().collect();
    assert_eq!(
      String::from_utf16_lossy(&units[found.preview_start..found.preview_end]),
      "needle"
    );
    assert_eq!(found.start_column, 500);
  }
}
