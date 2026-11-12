//! Workspace services: discovery (indexing), find in files, and Go to File.

use std::collections::HashMap;
use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use flareite_benches::{create_workspace, synthetic_paths};
use flareite_text::SearchQuery;
use flareite_workspace::{
  ExcludeFilter, FileIndex, SearchLimits, SearchScope, list_directory, scan_files, search_workspace,
};

const FILES: usize = 10_000;

fn workspace(c: &mut Criterion) {
  let dir = tempfile::tempdir().expect("temp dir");
  let root = dir.path();
  create_workspace(root, FILES, 2048);
  let exclude = ExcludeFilter::new(&[".git"]).expect("valid");
  let scope = SearchScope::excluding(exclude.clone());
  let never = AtomicBool::new(false);

  let mut group = c.benchmark_group("workspace/10k_files");
  group
    .sample_size(10)
    .measurement_time(Duration::from_secs(10));
  group.bench_function("discover_and_index", |b| {
    b.iter(|| {
      let index = scan_files(black_box(root), &exclude, &never);
      assert_eq!(index.len(), FILES + 1, "sources and .gitignore");
      index
    })
  });
  for (name, query) in [
    ("search_literal_rare", SearchQuery::literal("needle")),
    ("search_literal_common", SearchQuery::literal("buffer")),
    (
      "search_regex",
      SearchQuery {
        pattern: r"fn\s+\w+\(".into(),
        regex: true,
        case_sensitive: true,
        whole_word: false,
      },
    ),
  ] {
    let searcher = query.compile().expect("compiles");
    let parallel = SearchLimits::default();
    let sequential = SearchLimits {
      threads: 1,
      ..parallel
    };
    for (label, limits) in [("1_thread", sequential), ("default_threads", parallel)] {
      group.bench_function(format!("{name}/{label}"), |b| {
        b.iter(|| {
          search_workspace(
            root,
            &scope,
            &searcher,
            &HashMap::new(),
            limits,
            &never,
            |file| {
              black_box(file);
            },
          )
        })
      });
    }
  }
  group.finish();

  c.bench_function("workspace/list_directory_100_entries", |b| {
    let dir = root.join("src/module_0");
    b.iter(|| list_directory(root, black_box(&dir), &exclude).expect("lists"))
  });
}

fn fuzzy(c: &mut Criterion) {
  let index = FileIndex::from_paths(Path::new("/w"), synthetic_paths(100_000));
  let mut group = c.benchmark_group("workspace/go_to_file_100k_paths");
  for query in ["ed", "editordoc", "selection_9999"] {
    group.bench_function(query, |b| b.iter(|| index.query(black_box(query), 100)));
  }
  group.finish();
}

criterion_group! {
  name = benches;
  config = Criterion::default().warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(3));
  targets = workspace, fuzzy
}
criterion_main!(benches);
