//! Syntax highlighting for open documents.
//!
//! Each document of a known language keeps a syntax tree (with layers for
//! languages injected into it). Edits update
//! the tree's positions right away; then a small incremental reparse runs
//! inline within [`INLINE_PARSE_BUDGET`], and anything longer (first parses,
//! large documents) runs on the worker pool over a snapshot, after which the
//! frontend is told to refresh. Until then the edited old tree is used, so
//! colors stay in place while typing.
//!
//! Views carry highlights only for lines whose spans the client does not
//! already have: the last spans sent are remembered (as hashes) for the lines
//! the client holds and replayed through line splices like the lines
//! themselves. Each of a document's views (one per editor showing it) has
//! its own.
//!
//! The tree also gives the document's foldable regions, when its language
//! has a fold query.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use flareite_core::{Document, DocumentId, MAX_DISPLAY_LINE_BYTES, ViewId};
use flareite_syntax::{
  Language, Registry, Span, SyntaxTree, apply_edits, deadline, fold_ranges, highlight_lines, parse,
};
use flareite_tasks::Priority;
use flareite_text::{TextBuffer, TextSnapshot, Version};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use super::protocol::KnownDto;
use crate::events::{self, AppEvent};
use crate::languages::{LanguagesState, language_of};
use crate::perf::Perf;

/// Larger documents are not highlighted.
pub const MAX_SYNTAX_BYTES: usize = 16 * 1024 * 1024;
/// Longest reparse done while handling an edit; longer ones go to a worker.
const INLINE_PARSE_BUDGET: Duration = Duration::from_millis(3);

/// The highlight spans of one line, flattened as `[start, end, kind, ...]`
/// with UTF-16 columns. Kinds index `TokenKind::NAMES`, starting at 1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineHighlightsDto {
  pub line: usize,
  pub spans: Vec<u32>,
}

/// The spans sent for the lines a client holds.
struct Mirror {
  version: Version,
  /// The lines, as sorted ranges.
  ranges: Vec<Range<usize>>,
  /// A hash of each line's spans, in order.
  hashes: Vec<u64>,
}

struct ParseJob {
  language: Language,
  registry: Arc<Registry>,
  snapshot: TextSnapshot,
  old: Option<SyntaxTree>,
  version: Version,
}

struct DocumentSyntax {
  language: Language,
  /// Where injected languages are looked up.
  registry: Arc<Registry>,
  tree: Option<SyntaxTree>,
  /// The text version `tree` is a parse of.
  parsed: Version,
  /// The version `tree`'s positions have been edited to.
  edited: Version,
  /// The version a background parse is running for.
  pending: Option<Version>,
  /// What each view's client holds.
  mirrors: HashMap<ViewId, Mirror>,
}

impl DocumentSyntax {
  fn new(language: Language, registry: Arc<Registry>) -> Self {
    Self {
      language,
      registry,
      tree: None,
      parsed: Version::INITIAL,
      edited: Version::INITIAL,
      pending: None,
      mirrors: HashMap::new(),
    }
  }

  /// Brings the tree up to date with `buffer` as far as possible without
  /// blocking. Returns a parse to run in the background, if one is needed.
  fn update(&mut self, buffer: &TextBuffer) -> Option<ParseJob> {
    let version = buffer.version();
    self.follow_edits(buffer);
    if (self.tree.is_some() && self.parsed == version) || self.pending.is_some() {
      return None;
    }
    if let Some(tree) = &self.tree
      && let Ok(Some(tree)) = parse(
        &self.language,
        &self.registry,
        buffer,
        Some(tree),
        deadline(INLINE_PARSE_BUDGET),
      )
    {
      self.tree = Some(tree);
      self.parsed = version;
      return None;
    }
    self.pending = Some(version);
    Some(ParseJob {
      language: self.language.clone(),
      registry: self.registry.clone(),
      snapshot: buffer.snapshot(),
      old: self.tree.clone(),
      version,
    })
  }

  /// Moves the tree's positions to `buffer`'s current text, without
  /// parsing it again.
  fn follow_edits(&mut self, buffer: &TextBuffer) {
    let version = buffer.version();
    if let Some(tree) = &mut self.tree
      && self.edited != version
    {
      match buffer.edits_since(self.edited) {
        Some(edits) => {
          apply_edits(tree, &edits);
          self.edited = version;
        }
        // Too far behind to catch up: start over.
        None => self.tree = None,
      }
    }
  }

  /// Takes the result of a background parse of `version`. Returns whether
  /// the highlighting may have changed.
  fn finish(&mut self, version: Version, tree: Option<SyntaxTree>, buffer: &TextBuffer) -> bool {
    if self.pending == Some(version) {
      self.pending = None;
    }
    let Some(mut tree) = tree else {
      return false;
    };
    if self.tree.is_some() && self.parsed.get() >= version.get() {
      return false;
    }
    match buffer.edits_since(version) {
      Some(edits) => apply_edits(&mut tree, &edits),
      None => return false,
    }
    self.tree = Some(tree);
    self.parsed = version;
    self.edited = buffer.version();
    true
  }

  /// Highlights for the lines in `ranges` that the client of `view`
  /// holding `known` lacks, given that the view replaces `chunks` (whose
  /// spans the client resets).
  fn highlights(
    &mut self,
    buffer: &TextBuffer,
    view: ViewId,
    ranges: &[Range<usize>],
    known: Option<&KnownDto>,
    chunks: &[Range<usize>],
  ) -> Vec<LineHighlightsDto> {
    let carried = self.carried(buffer, view, known, chunks);
    let mut changed = Vec::new();
    let mut hashes = Vec::new();
    for lines in ranges {
      let spans = match &self.tree {
        Some(tree) => highlight_lines(tree, buffer, lines.clone(), MAX_DISPLAY_LINE_BYTES),
        None => vec![Vec::new(); lines.len()],
      };
      for (line, spans) in lines.clone().zip(spans) {
        let spans = line_spans(buffer, line, &spans);
        let hash = hash_spans(&spans);
        if carried.get(&line) != Some(&hash) {
          changed.push(LineHighlightsDto { line, spans });
        }
        hashes.push(hash);
      }
    }
    self.mirrors.insert(
      view,
      Mirror {
        version: buffer.version(),
        ranges: ranges.to_vec(),
        hashes,
      },
    );
    changed
  }

  /// The hash of the spans the client has for each line it holds, where
  /// known.
  fn carried(
    &self,
    buffer: &TextBuffer,
    view: ViewId,
    known: Option<&KnownDto>,
    chunks: &[Range<usize>],
  ) -> HashMap<usize, u64> {
    let mut carried = HashMap::new();
    if let (Some(mirror), Some(known)) = (self.mirrors.get(&view), known)
      && mirror.version.get() == known.version
      && mirror.ranges == known.ranges()
      && let Some(splices) = buffer.line_changes_since(mirror.version)
    {
      // As the client does with its lines (see `lineCache.ts`).
      let mut held: Vec<(usize, u64)> = mirror
        .ranges
        .iter()
        .flat_map(Clone::clone)
        .zip(mirror.hashes.iter().copied())
        .collect();
      for splice in splices {
        let removed_end = splice.start + splice.removed;
        held.retain(|&(line, _)| line < splice.start || line >= removed_end);
        for (line, _) in &mut held {
          if *line >= removed_end {
            *line = *line + splice.inserted - splice.removed;
          }
        }
      }
      carried.extend(held);
    }
    let empty = hash_spans(&[]);
    for line in chunks.iter().flat_map(Clone::clone) {
      carried.insert(line, empty);
    }
    carried
  }
}

fn hash_spans(spans: &[u32]) -> u64 {
  let mut hasher = DefaultHasher::new();
  spans.hash(&mut hasher);
  hasher.finish()
}

/// Highlights a snippet of code, such as a code block in a preview, in the
/// language `name` (an id or file extension): each line's spans, flattened
/// as in views. `None` for unknown languages, or if parsing takes longer
/// than `budget`.
pub fn highlight_snippet(
  registry: &Registry,
  name: &str,
  text: &str,
  budget: Duration,
) -> Option<Vec<Vec<u32>>> {
  let language = registry.for_name(&name.to_ascii_lowercase())?;
  let buffer = TextBuffer::from_text(text);
  let tree = parse(language, registry, &buffer, None, deadline(budget)).ok()??;
  Some(highlight_buffer_lines(
    Some(&tree),
    &buffer,
    0..buffer.line_count(),
  ))
}

/// Highlights `lines` of `buffer` with `tree` (a parse of it), flattened as
/// in views; no spans without a tree.
pub fn highlight_buffer_lines(
  tree: Option<&SyntaxTree>,
  buffer: &TextBuffer,
  lines: Range<usize>,
) -> Vec<Vec<u32>> {
  let Some(tree) = tree else {
    return vec![Vec::new(); lines.len()];
  };
  let spans = highlight_lines(tree, buffer, lines.clone(), MAX_DISPLAY_LINE_BYTES);
  lines
    .zip(spans)
    .map(|(line, spans)| line_spans(buffer, line, &spans))
    .collect()
}

/// The spans of `line` (byte columns), flattened as UTF-16 triples.
fn line_spans(buffer: &TextBuffer, line: usize, spans: &[Span]) -> Vec<u32> {
  if spans.is_empty() {
    return Vec::new();
  }
  let (text, _) = buffer.line_prefix(line, MAX_DISPLAY_LINE_BYTES);
  flatten_utf16(&text, spans)
}

/// `spans` (byte columns into `text`, in order) as flat UTF-16 triples.
fn flatten_utf16(text: &str, spans: &[Span]) -> Vec<u32> {
  let mut flat = Vec::with_capacity(spans.len() * 3);
  let ascii = text.is_ascii();
  let column = |byte: usize, walked: &mut (usize, usize)| -> u32 {
    let byte = byte.min(text.len());
    if ascii {
      return byte as u32;
    }
    // Spans are in order, so each conversion resumes where the last ended.
    let (mut offset, mut column) = *walked;
    for ch in text[offset..].chars() {
      if offset >= byte {
        break;
      }
      offset += ch.len_utf8();
      column += ch.len_utf16();
    }
    *walked = (offset, column);
    column as u32
  };
  let mut walked = (0, 0);
  for span in spans {
    let start = column(span.start, &mut walked);
    let end = column(span.end, &mut walked);
    if start < end {
      flat.extend([start, end, span.kind as u32]);
    }
  }
  flat
}

#[derive(Default)]
pub struct SyntaxStates(Mutex<States>);

#[derive(Default)]
struct States {
  documents: HashMap<DocumentId, DocumentSyntax>,
  /// Views of documents that lost their language (set to plain text, say)
  /// since they were last sent colors, which they still show.
  uncolored: HashSet<(DocumentId, ViewId)>,
}

impl SyntaxStates {
  fn lock(&self) -> MutexGuard<'_, States> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Highlights for a view of the lines in `ranges` (see
  /// [`DocumentSyntax::highlights`]), scheduling a background parse if
  /// needed. Called with the editor locked.
  pub fn highlights(
    &self,
    app: &AppHandle,
    document: &Document,
    ranges: &[Range<usize>],
    known: Option<&KnownDto>,
    chunks: &[Range<usize>],
  ) -> Vec<LineHighlightsDto> {
    let mut states = self.lock();
    let Some(syntax) = Self::entry(app, &mut states, document) else {
      // Clears the colors of a document that lost its language.
      if !states.uncolored.remove(&(document.id(), document.view())) {
        return Vec::new();
      }
      return ranges
        .iter()
        .flat_map(Clone::clone)
        .map(|line| LineHighlightsDto {
          line,
          spans: Vec::new(),
        })
        .collect();
    };
    let started = Instant::now();
    let job = syntax.update(document.buffer());
    let highlights = syntax.highlights(document.buffer(), document.view(), ranges, known, chunks);
    drop(states);
    let perf = app.state::<Perf>();
    perf.record("syntax.highlight", started.elapsed());
    if let Some(job) = job {
      spawn_parse(app, document.id(), job);
    }
    highlights
  }

  /// Calls `f` with `document`'s syntax tree as it is (edited to the
  /// current text, but not parsed again), or `None` if it has none yet.
  /// Called with the editor locked.
  pub fn with_tree<T>(
    &self,
    app: &AppHandle,
    document: &Document,
    f: impl FnOnce(Option<&SyntaxTree>) -> T,
  ) -> T {
    let mut states = self.lock();
    match Self::entry(app, &mut states, document) {
      Some(syntax) => {
        syntax.follow_edits(document.buffer());
        f(syntax.tree.as_ref())
      }
      None => f(None),
    }
  }

  /// A copy of `document`'s syntax tree, edited to its current text, for
  /// highlighting without the editor lock (the minimap). Unlike
  /// [`Self::highlights`], this leaves alone what the editor's view has been
  /// sent. Called with the editor locked.
  pub fn tree(&self, app: &AppHandle, document: &Document) -> Option<SyntaxTree> {
    let mut states = self.lock();
    let syntax = Self::entry(app, &mut states, document)?;
    let job = syntax.update(document.buffer());
    let tree = syntax.tree.clone();
    drop(states);
    if let Some(job) = job {
      spawn_parse(app, document.id(), job);
    }
    tree
  }

  /// The regions the fold query of `document`'s language finds touching
  /// the lines in `ranges`, from the tree as it is (none before the first
  /// parse). `None` if the document has no language or its language has no
  /// fold query. Called with the editor locked.
  pub fn fold_regions(
    &self,
    app: &AppHandle,
    document: &Document,
    ranges: &[Range<usize>],
  ) -> Option<Vec<Range<usize>>> {
    let mut states = self.lock();
    let syntax = Self::entry(app, &mut states, document)?;
    if !syntax.language.has_folds() {
      return None;
    }
    let job = syntax.update(document.buffer());
    let mut regions = Vec::new();
    if let Some(tree) = &syntax.tree {
      for lines in ranges {
        regions.extend(fold_ranges(tree, document.buffer(), lines.clone())?);
      }
    }
    drop(states);
    if let Some(job) = job {
      spawn_parse(app, document.id(), job);
    }
    regions.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    regions.dedup();
    Some(regions)
  }

  /// Whether `document`'s foldable regions are known: its language has no
  /// fold query (so they come from indentation), or its tree is parsed.
  /// Called with the editor locked.
  pub fn folds_ready(&self, app: &AppHandle, document: &Document) -> bool {
    let mut states = self.lock();
    let Some(syntax) = Self::entry(app, &mut states, document) else {
      return true;
    };
    if !syntax.language.has_folds() {
      return true;
    }
    let job = syntax.update(document.buffer());
    let ready = syntax.tree.is_some();
    drop(states);
    if let Some(job) = job {
      spawn_parse(app, document.id(), job);
    }
    ready
  }

  /// The syntax state of `document`, created or reset for its language
  /// (or when the languages change); or `None`, dropping any state, if it
  /// isn't highlighted.
  fn entry<'a>(
    app: &AppHandle,
    states: &'a mut States,
    document: &Document,
  ) -> Option<&'a mut DocumentSyntax> {
    let id = document.id();
    let registry = app.state::<LanguagesState>().registry();
    let language = language_of(&registry, document)
      .filter(|_| document.buffer().len() <= MAX_SYNTAX_BYTES)
      .cloned();
    let Some(language) = language else {
      if let Some(syntax) = states.documents.remove(&id) {
        states
          .uncolored
          .extend(syntax.mirrors.into_keys().map(|view| (id, view)));
      }
      return None;
    };
    // A new state sends every line's colors again.
    states.uncolored.retain(|&(document, _)| document != id);
    Some(
      states
        .documents
        .entry(id)
        .and_modify(|syntax| {
          if syntax.language != language || !Arc::ptr_eq(&syntax.registry, &registry) {
            *syntax = DocumentSyntax::new(language.clone(), registry.clone());
          }
        })
        .or_insert_with(|| DocumentSyntax::new(language.clone(), registry.clone())),
    )
  }

  /// Drops the syntax state of a closed document.
  pub fn forget(&self, id: DocumentId) {
    let mut states = self.lock();
    states.documents.remove(&id);
    states.uncolored.retain(|&(document, _)| document != id);
  }

  /// Drops what a closed view of a document was sent.
  pub fn forget_view(&self, id: DocumentId, view: ViewId) {
    let mut states = self.lock();
    if let Some(syntax) = states.documents.get_mut(&id) {
      syntax.mirrors.remove(&view);
    }
    states.uncolored.remove(&(id, view));
  }
}

fn spawn_parse(app: &AppHandle, id: DocumentId, job: ParseJob) {
  let task_app = app.clone();
  app.state::<Perf>().scheduler.spawn(
    "parse",
    Priority::High,
    Some(format!("parse:{id}")),
    move |token| {
      let started = Instant::now();
      let (tree, failed) = match parse(
        &job.language,
        &job.registry,
        &job.snapshot,
        job.old.as_ref(),
        || token.is_cancelled(),
      ) {
        Ok(tree) => (tree, false),
        Err(error) => {
          tracing::error!(%error, "syntax highlighting is unavailable");
          (None, true)
        }
      };
      task_app
        .state::<Perf>()
        .record("syntax.parse", started.elapsed());
      let state = task_app.state::<super::EditorState>();
      let updated = {
        let editor = state.lock();
        let Ok(document) = editor.document(id) else {
          return;
        };
        let mut states = state.syntax.lock();
        states
          .documents
          .get_mut(&id)
          .is_some_and(|syntax| syntax.finish(job.version, tree, document.buffer()))
      };
      // A grammar that failed to load leaves the document to fold by
      // indentation, which the view finds out when it refreshes.
      if updated || failed {
        events::emit(
          &task_app,
          AppEvent::SyntaxUpdated {
            document_id: id.get(),
          },
        );
      }
    },
  );
}

#[cfg(test)]
// Chunk ranges are naturally single-element arrays here.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
  use flareite_syntax::TokenKind;
  use flareite_text::{ChangeSet, Edit};

  use super::*;

  fn rust() -> DocumentSyntax {
    let registry = Arc::new(crate::languages::examples(&["rust"]));
    let rust = registry.get("rust").expect("the example").clone();
    DocumentSyntax::new(rust, registry)
  }

  fn run(job: &ParseJob) -> Option<SyntaxTree> {
    parse(&job.language, &job.registry, &job.snapshot, None, || false).expect("grammar")
  }

  fn parsed(text: &str) -> (TextBuffer, DocumentSyntax) {
    let buffer = TextBuffer::from_text(text);
    let mut syntax = rust();
    let job = syntax
      .update(&buffer)
      .expect("first parse runs in the background");
    let tree = run(&job);
    assert!(syntax.finish(job.version, tree, &buffer));
    (buffer, syntax)
  }

  fn known(version: Version, lines: Range<usize>) -> Option<KnownDto> {
    Some(KnownDto {
      version: version.get(),
      ranges: vec![[lines.start, lines.end]],
    })
  }

  #[test]
  fn sends_only_highlights_the_client_lacks() {
    let (mut buffer, mut syntax) = parsed("fn a() {}\nfn b() {}\nfn c() {}\n");
    let first = syntax.highlights(&buffer, ViewId::default(), &[0..4], None, &[0..4]);
    assert_eq!(first.len(), 3, "the empty last line needs nothing");
    let v1 = buffer.version();

    // Unchanged: nothing to send.
    assert!(
      syntax
        .highlights(
          &buffer,
          ViewId::default(),
          &[0..4],
          known(v1, 0..4).as_ref(),
          &[]
        )
        .is_empty()
    );

    // Editing line 1 resends it (as a chunk) and nothing else.
    buffer
      .apply(&ChangeSet::single(Edit::insert(13, "x")).expect("valid"))
      .expect("applies");
    assert!(
      syntax.update(&buffer).is_none(),
      "small reparse runs inline"
    );
    let after = syntax.highlights(
      &buffer,
      ViewId::default(),
      &[0..4],
      known(v1, 0..4).as_ref(),
      &[1..2],
    );
    assert_eq!(after.iter().map(|h| h.line).collect::<Vec<_>>(), [1]);

    // A view from a client that does not match resends everything.
    let mismatch = syntax.highlights(
      &buffer,
      ViewId::default(),
      &[0..4],
      known(v1, 0..4).as_ref(),
      &[],
    );
    assert_eq!(mismatch.len(), 4);
  }

  #[test]
  fn edits_that_change_later_lines_resend_them() {
    let (mut buffer, mut syntax) = parsed("a();\nb();\n");
    syntax.highlights(&buffer, ViewId::default(), &[0..3], None, &[0..3]);
    let v1 = buffer.version();
    buffer
      .apply(&ChangeSet::single(Edit::insert(0, "/*")).expect("valid"))
      .expect("applies");
    syntax.update(&buffer);
    let after = syntax.highlights(
      &buffer,
      ViewId::default(),
      &[0..3],
      known(v1, 0..3).as_ref(),
      &[0..1],
    );
    let lines: Vec<usize> = after.iter().map(|h| h.line).collect();
    assert_eq!(lines, [0, 1], "line 1 is inside the new comment");
    assert_eq!(after[1].spans, [0, 4, TokenKind::Comment as u32]);
  }

  #[test]
  fn each_view_has_its_own_mirror() {
    let (buffer, mut syntax) = parsed(
      "fn a() {}
",
    );
    let (first, second) = (ViewId::default(), ViewId::from_raw(1));
    let v1 = buffer.version();
    assert_eq!(
      syntax
        .highlights(&buffer, first, &[0..2], None, &[0..2])
        .len(),
      1
    );
    // The second view holds the same lines, but hasn't been sent their colors.
    let sent = syntax.highlights(&buffer, second, &[0..2], known(v1, 0..2).as_ref(), &[]);
    assert_eq!(sent.iter().map(|h| h.line).collect::<Vec<_>>(), [0, 1]);
    assert!(
      syntax
        .highlights(&buffer, first, &[0..2], known(v1, 0..2).as_ref(), &[])
        .is_empty(),
      "the first view's mirror is still its own"
    );
  }

  #[test]
  fn stale_background_results_are_caught_up_or_dropped() {
    let mut buffer = TextBuffer::from_text("fn a() {}\n");
    let mut syntax = rust();
    let job = syntax.update(&buffer).expect("background parse");
    assert!(syntax.update(&buffer).is_none(), "one parse at a time");
    buffer
      .apply(&ChangeSet::single(Edit::insert(0, "\n")).expect("valid"))
      .expect("applies");
    let tree = run(&job);
    assert!(syntax.finish(job.version, tree, &buffer));
    let tree = syntax.tree.as_ref().expect("tree");
    assert_eq!(
      tree
        .tree()
        .root_node()
        .child(0)
        .expect("fn")
        .start_position()
        .row,
      1
    );
    assert_eq!(syntax.edited, buffer.version());
  }

  #[test]
  fn highlights_snippets_by_language_id_or_extension() {
    let registry = crate::languages::examples(&["rust", "typescript"]);
    let budget = Duration::from_secs(5);
    let lines = highlight_snippet(&registry, "RS", "// é\nfn main() {}", budget).expect("rust");
    assert_eq!(
      lines[0],
      [0, 4, TokenKind::Comment as u32],
      "UTF-16 columns"
    );
    assert_eq!(lines[1][..3], [0, 2, TokenKind::Keyword as u32]);
    assert!(highlight_snippet(&registry, "typescript", "const x = 1", budget).is_some());
    assert!(highlight_snippet(&registry, "cobol", "MOVE A TO B", budget).is_none());
  }

  #[test]
  fn converts_byte_columns_to_utf16() {
    let span = |start, end| Span {
      start,
      end,
      kind: TokenKind::String,
    };
    let kind = TokenKind::String as u32;
    assert_eq!(flatten_utf16("ab", &[span(0, 2)]), [0, 2, kind]);
    // "é" is 2 bytes and 1 unit; "🦀" is 4 bytes and 2 units.
    assert_eq!(
      flatten_utf16("é🦀x", &[span(0, 2), span(2, 6), span(6, 7)]),
      [0, 1, kind, 1, 3, kind, 3, 4, kind]
    );
  }
}
