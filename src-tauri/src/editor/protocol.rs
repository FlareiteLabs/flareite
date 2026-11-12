//! The editor protocol: JSON shapes exchanged with the frontend, and their
//! conversion to and from core types.
//!
//! Positions on the wire are line plus UTF-16 column, so the frontend can use
//! JavaScript string indices directly. They are converted to byte offsets
//! against the document as it is when each operation is applied, and clamped
//! to valid positions.

use std::ops::Range;

use flareite_core::{
  CommentTokens, Direction, Document, EditorOp, KnownLines, LineText, Motion, PairContext, Unit,
  VerticalDirection, ViewportUpdate, WrapConfig, WrapIndent,
};
use flareite_text::{
  LineEnding, SearchQuery, TextBuffer, Utf16Position, Version, normalize_line_endings,
};
use serde::{Deserialize, Serialize};

use super::syntax::LineHighlightsDto;
use crate::lsp::{DiagnosticDto, LanguageStatusDto};

/// Most lines returned in one view, regardless of the requested viewport.
pub const MAX_VIEW_LINES: usize = 1_000;
/// Most selections returned in one view.
pub const MAX_VIEW_SELECTIONS: usize = 10_000;
/// Most operations accepted in one batch.
pub const MAX_OPS_PER_BATCH: usize = 1_000;
/// Documents up to this size count find matches synchronously; larger ones
/// count on a background worker.
pub const SYNC_COUNT_BYTES: usize = 1024 * 1024;
/// Most match highlights returned for the visible lines.
const MAX_VISIBLE_MATCHES: usize = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
  pub line: usize,
  pub column: usize,
}

impl From<Utf16Position> for Position {
  fn from(position: Utf16Position) -> Self {
    Self {
      line: position.line,
      column: position.column,
    }
  }
}

impl Position {
  pub(crate) fn to_offset(self, buffer: &TextBuffer) -> usize {
    buffer.utf16_to_offset(Utf16Position {
      line: self.line,
      column: self.column,
    })
  }
}

/// Display rows `start_row..end_row` (end exclusive) the frontend is
/// displaying. Rows skip the hidden lines of folded regions, and a wrapped
/// line has a row for each of its rows.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
  pub start_row: usize,
  pub end_row: usize,
  /// Where the editor wraps lines; absent if it doesn't. Every request
  /// says, so the view is never left wrapped by an editor that's gone.
  #[serde(default)]
  pub wrap: Option<WrapDto>,
}

impl Viewport {
  /// Where the view wraps lines, for the core.
  pub fn wrap_config(&self) -> Option<WrapConfig> {
    self.wrap.map(|wrap| WrapConfig {
      columns: wrap
        .columns
        .clamp(*WRAP_COLUMNS.start(), *WRAP_COLUMNS.end()),
      indent: match wrap.indent {
        WrapIndentDto::None => WrapIndent::None,
        WrapIndentDto::Same => WrapIndent::Same,
        WrapIndentDto::Indent => WrapIndent::Indent,
        WrapIndentDto::DeepIndent => WrapIndent::DeepIndent,
      },
    })
  }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DirectionDto {
  Backward,
  Forward,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UnitDto {
  Grapheme,
  Word,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VerticalDto {
  Up,
  Down,
}

impl From<VerticalDto> for VerticalDirection {
  fn from(direction: VerticalDto) -> Self {
    match direction {
      VerticalDto::Up => Self::Up,
      VerticalDto::Down => Self::Down,
    }
  }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MotionDto {
  Left,
  Right,
  WordLeft,
  WordRight,
  Up,
  Down,
  PageUp,
  PageDown,
  LineStart,
  LineEnd,
  DocumentStart,
  DocumentEnd,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQueryDto {
  pub pattern: String,
  #[serde(default)]
  pub regex: bool,
  #[serde(default)]
  pub case_sensitive: bool,
  #[serde(default)]
  pub whole_word: bool,
}

impl From<SearchQueryDto> for SearchQuery {
  fn from(query: SearchQueryDto) -> Self {
    Self {
      pattern: query.pattern,
      regex: query.regex,
      case_sensitive: query.case_sensitive,
      whole_word: query.whole_word,
    }
  }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(
  tag = "type",
  rename_all = "camelCase",
  rename_all_fields = "camelCase"
)]
pub enum OpDto {
  Insert {
    text: String,
  },
  /// Text typed at the keyboard, which may step over, add, or wrap with
  /// the language's pairs (see [`OpContext::pairs`]).
  Type {
    text: String,
  },
  /// Pasted text; see [`OpContext::copied_lines`].
  Paste {
    text: String,
  },
  /// Enter: with the language's brackets (see [`OpContext::pairs`]).
  Newline,
  /// Text an extension inserts after typing (see `editor_auto_insert`), if
  /// the document is still at `version` with its only cursor at
  /// `position`.
  AutoInsert {
    version: u64,
    position: Position,
    text: String,
    cursor: usize,
  },
  /// A snippet with tab stops, such as a completion's, at each selection.
  InsertSnippet {
    snippet: String,
    adjust_indentation: bool,
  },
  InsertLine {
    direction: VerticalDto,
  },
  Tab,
  Indent,
  Outdent,
  /// Uses the comment syntax of the document's language.
  ToggleComment,
  MoveLines {
    direction: VerticalDto,
  },
  CopyLines {
    direction: VerticalDto,
  },
  DeleteLines,
  Delete {
    direction: DirectionDto,
    unit: UnitDto,
  },
  DeleteSelection,
  Move {
    motion: MotionDto,
    extend: bool,
    /// Lines per page, for `pageUp` and `pageDown`.
    #[serde(default)]
    page_lines: Option<usize>,
  },
  SelectAll,
  SelectLine,
  CursorsAtLineEnds,
  CollapseSelections,
  CursorUndo,
  SetCursor {
    position: Position,
    extend: bool,
    add: bool,
  },
  SelectWord {
    position: Position,
  },
  AddCursor {
    direction: VerticalDto,
  },
  Undo,
  Redo,
  FindNext {
    query: SearchQueryDto,
    backward: bool,
  },
  SelectAllMatches {
    query: SearchQueryDto,
  },
  Replace {
    query: SearchQueryDto,
    replacement: String,
  },
  ReplaceAll {
    query: SearchQueryDto,
    replacement: String,
  },
  AddNextMatch,
  SelectAllOccurrences,
  JumpToBracket,
  /// Fold operations use the document's foldable regions (see
  /// [`OpContext::folds`]).
  Fold,
  Unfold,
  ToggleFold,
  /// A click on the fold marker of `line`.
  ToggleFoldAt {
    line: usize,
  },
  FoldAll,
  UnfoldAll,
  /// Fold or unfold every region marked by region comments.
  FoldAllRegions,
  UnfoldAllRegions,
  FoldRecursively,
  UnfoldRecursively,
  FoldLevel {
    level: usize,
  },
  /// Folds exactly these regions (`[start, end]` line ranges, at most
  /// [`MAX_SET_FOLDS`]), unfolding any others: a diff's unchanged lines.
  SetFolds {
    regions: Vec<[usize; 2]>,
  },
}

/// Where to wrap lines: a width in columns, and how continuation rows are
/// indented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WrapDto {
  pub columns: usize,
  pub indent: WrapIndentDto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WrapIndentDto {
  None,
  Same,
  Indent,
  DeepIndent,
}

/// Narrowest and widest wrapping widths, in columns.
pub const WRAP_COLUMNS: std::ops::RangeInclusive<usize> = 1..=10_000;

/// Most regions one `setFolds` folds.
pub const MAX_SET_FOLDS: usize = 10_000;

/// What some operations need besides the document.
#[derive(Debug, Default)]
pub struct OpContext {
  /// The comment syntax of the document's language.
  pub comments: CommentTokens,
  /// Foldable regions as line ranges: all of them, or those around the
  /// lines the operation is about.
  pub folds: Vec<Range<usize>>,
  /// The text last copied as whole lines. Pasting the same text pastes
  /// whole lines.
  pub copied_lines: Option<String>,
  /// The pairs at each cursor, for typing, Enter, and Backspace.
  pub pairs: PairContext,
}

impl OpDto {
  /// Whether this is a fold operation.
  pub fn changes_folds(&self) -> bool {
    matches!(
      self,
      Self::Fold
        | Self::Unfold
        | Self::ToggleFold
        | Self::ToggleFoldAt { .. }
        | Self::FoldAll
        | Self::UnfoldAll
        | Self::FoldAllRegions
        | Self::UnfoldAllRegions
        | Self::FoldRecursively
        | Self::UnfoldRecursively
        | Self::FoldLevel { .. }
    )
  }

  pub fn into_core(self, buffer: &TextBuffer, context: &OpContext) -> EditorOp {
    match self {
      Self::Insert { text } => EditorOp::Insert(text),
      Self::Type { text } => EditorOp::Type {
        text,
        pairs: context.pairs.clone(),
      },
      Self::Paste { text } => EditorOp::Paste {
        whole_lines: context
          .copied_lines
          .as_deref()
          .is_some_and(|copied| same_text(copied, &text)),
        text,
      },
      Self::Newline => EditorOp::Enter(context.pairs.clone()),
      Self::AutoInsert {
        version,
        position,
        text,
        cursor,
      } => EditorOp::AutoInsert {
        version: Version::from_raw(version),
        offset: position.to_offset(buffer),
        text,
        cursor,
      },
      Self::InsertSnippet {
        snippet,
        adjust_indentation,
      } => EditorOp::InsertSnippet {
        snippet,
        adjust_indentation,
      },
      Self::InsertLine { direction } => EditorOp::InsertLine(direction.into()),
      Self::Tab => EditorOp::Tab,
      Self::Indent => EditorOp::Indent,
      Self::Outdent => EditorOp::Outdent,
      Self::ToggleComment => EditorOp::ToggleComment(context.comments.clone()),
      Self::MoveLines { direction } => EditorOp::MoveLines(direction.into()),
      Self::CopyLines { direction } => EditorOp::CopyLines(direction.into()),
      Self::DeleteLines => EditorOp::DeleteLines,
      Self::Delete {
        direction: DirectionDto::Backward,
        unit: UnitDto::Grapheme,
      } => EditorOp::Backspace(context.pairs.clone()),
      Self::Delete { direction, unit } => EditorOp::Delete {
        direction: match direction {
          DirectionDto::Backward => Direction::Backward,
          DirectionDto::Forward => Direction::Forward,
        },
        unit: match unit {
          UnitDto::Grapheme => Unit::Grapheme,
          UnitDto::Word => Unit::Word,
        },
      },
      Self::DeleteSelection => EditorOp::DeleteSelection,
      Self::Move {
        motion,
        extend,
        page_lines,
      } => {
        let lines = page_lines.unwrap_or(1).clamp(1, MAX_VIEW_LINES);
        let motion = match motion {
          MotionDto::Left => Motion::Left,
          MotionDto::Right => Motion::Right,
          MotionDto::WordLeft => Motion::WordLeft,
          MotionDto::WordRight => Motion::WordRight,
          MotionDto::Up => Motion::Up,
          MotionDto::Down => Motion::Down,
          MotionDto::PageUp => Motion::PageUp { lines },
          MotionDto::PageDown => Motion::PageDown { lines },
          MotionDto::LineStart => Motion::LineStart,
          MotionDto::LineEnd => Motion::LineEnd,
          MotionDto::DocumentStart => Motion::DocumentStart,
          MotionDto::DocumentEnd => Motion::DocumentEnd,
        };
        EditorOp::Move { motion, extend }
      }
      Self::SelectAll => EditorOp::SelectAll,
      Self::SelectLine => EditorOp::SelectLine,
      Self::CursorsAtLineEnds => EditorOp::CursorsAtLineEnds,
      Self::CollapseSelections => EditorOp::CollapseSelections,
      Self::CursorUndo => EditorOp::CursorUndo,
      Self::SetCursor {
        position,
        extend,
        add,
      } => EditorOp::SetCursor {
        offset: position.to_offset(buffer),
        extend,
        add,
      },
      Self::SelectWord { position } => EditorOp::SelectWord {
        offset: position.to_offset(buffer),
      },
      Self::AddCursor { direction } => EditorOp::AddCursor(direction.into()),
      Self::Undo => EditorOp::Undo,
      Self::Redo => EditorOp::Redo,
      Self::FindNext { query, backward } => EditorOp::FindNext {
        query: query.into(),
        backward,
      },
      Self::SelectAllMatches { query } => EditorOp::SelectAllMatches {
        query: query.into(),
      },
      Self::Replace { query, replacement } => EditorOp::Replace {
        query: query.into(),
        replacement,
      },
      Self::ReplaceAll { query, replacement } => EditorOp::ReplaceAll {
        query: query.into(),
        replacement,
      },
      Self::AddNextMatch => EditorOp::AddNextMatch,
      Self::SelectAllOccurrences => EditorOp::SelectAllOccurrences,
      Self::JumpToBracket => EditorOp::JumpToBracket,
      Self::Fold => EditorOp::Fold(context.folds.clone()),
      Self::Unfold => EditorOp::Unfold,
      Self::ToggleFold => EditorOp::ToggleFold(context.folds.clone()),
      Self::ToggleFoldAt { line } => EditorOp::ToggleFoldAt {
        line,
        regions: context.folds.clone(),
      },
      Self::FoldAll => EditorOp::FoldAll(context.folds.clone()),
      Self::UnfoldAll => EditorOp::UnfoldAll,
      Self::FoldAllRegions => EditorOp::FoldAll(context.folds.clone()),
      Self::UnfoldAllRegions => EditorOp::UnfoldRegions(context.folds.clone()),
      Self::FoldRecursively => EditorOp::FoldRecursively(context.folds.clone()),
      Self::UnfoldRecursively => EditorOp::UnfoldRecursively(context.folds.clone()),
      Self::FoldLevel { level } => EditorOp::FoldLevel {
        level: level.max(1),
        regions: context.folds.clone(),
      },
      Self::SetFolds { regions } => EditorOp::SetFolds(
        regions
          .into_iter()
          .take(MAX_SET_FOLDS)
          .map(|[start, end]| start..end)
          .collect(),
      ),
    }
  }
}

/// Whether two texts are the same apart from line endings, which the system
/// clipboard may convert.
pub fn same_text(a: &str, b: &str) -> bool {
  normalize_line_endings(a, LineEnding::Lf) == normalize_line_endings(b, LineEnding::Lf)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSummary {
  pub id: u64,
  pub title: String,
  pub path: Option<String>,
  pub dirty: bool,
  pub line_ending: &'static str,
  /// The encoding's label, e.g. `UTF-8` or `Western (Windows 1252)`.
  pub encoding: String,
  /// Its id, for Reopen and Save with Encoding (`utf-8`, `windows-1252`).
  pub encoding_id: String,
}

impl DocumentSummary {
  pub fn of(document: &Document) -> Self {
    let encoding = document.encoding();
    Self {
      id: document.id().get(),
      title: document.title(),
      path: document.path().map(|path| path.display().to_string()),
      dirty: document.is_dirty(),
      line_ending: document.line_ending().label(),
      encoding: encoding.label(),
      encoding_id: encoding.id(),
    }
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionDto {
  pub anchor: Position,
  pub head: Position,
  pub primary: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorDto {
  pub position: Position,
  /// Zero-based visual column (tabs expanded), for display.
  pub visual_column: usize,
  /// Its display row: its line's, or the row it's on in a wrapped line.
  pub row: usize,
}

/// Where a line of the view wraps: the UTF-16 columns that start its rows
/// after the first, and the indent of those rows, in columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineWrapDto {
  pub line: usize,
  pub breaks: Vec<usize>,
  pub indent: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeDto {
  pub start: Position,
  pub end: Position,
}

/// Document-wide matches of one query at one version, counted in the
/// background for large documents.
#[derive(Debug, Clone)]
pub struct MatchCount {
  pub version: Version,
  pub query: SearchQuery,
  pub matches: Vec<Range<usize>>,
  pub truncated: bool,
}

/// Find results for the view's query: highlights for the visible lines and
/// document-wide counts.
#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchView {
  pub matches: Vec<RangeDto>,
  /// Total matches, or `None` while a background count is running.
  pub total: Option<usize>,
  pub truncated: bool,
  /// Index of the match the primary selection covers, if any.
  pub current: Option<usize>,
  /// Why the query could not be used, e.g. an invalid regular expression.
  pub error: Option<String>,
}

impl SearchView {
  /// Builds the search section for lines `start_line..end_line`. `counted`
  /// holds background results for this document, if any. Also returns
  /// whether a background count is needed.
  pub fn build(
    document: &Document,
    query: &SearchQuery,
    start_line: usize,
    end_line: usize,
    counted: Option<&MatchCount>,
  ) -> (Self, bool) {
    let searcher = match query.compile() {
      Ok(searcher) => searcher,
      Err(error) => {
        let view = Self {
          error: Some(error.to_string()),
          ..Self::default()
        };
        return (view, false);
      }
    };
    let buffer = document.buffer();
    let visible = buffer.line_start(start_line)..buffer.line_end(end_line.saturating_sub(1));
    let to_dto = |range: &Range<usize>| RangeDto {
      start: buffer.offset_to_utf16(range.start).into(),
      end: buffer.offset_to_utf16(range.end).into(),
    };

    if buffer.len() <= SYNC_COUNT_BYTES {
      let summary = document.match_summary(&searcher, visible);
      let view = Self {
        matches: summary.visible.iter().map(to_dto).collect(),
        total: Some(summary.total),
        truncated: summary.truncated,
        current: summary.current,
        error: None,
      };
      return (view, false);
    }

    let matches = searcher
      .find_in_buffer(buffer, visible, MAX_VISIBLE_MATCHES)
      .iter()
      .map(to_dto)
      .collect();
    let fresh =
      counted.filter(|count| count.version == document.version() && count.query == *query);
    let view = match fresh {
      Some(count) => {
        let primary = document.selections().primary().range();
        Self {
          matches,
          total: Some(count.matches.len()),
          truncated: count.truncated,
          current: count
            .matches
            .binary_search_by_key(&primary.start, |found| found.start)
            .ok()
            .filter(|&index| count.matches[index] == primary),
          error: None,
        }
      }
      None => Self {
        matches,
        ..Self::default()
      },
    };
    (view, fresh.is_none())
  }
}

/// Lines the client holds, from its last view: its `visible` ranges.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownDto {
  pub version: u64,
  pub ranges: Vec<[usize; 2]>,
}

impl KnownDto {
  pub fn ranges(&self) -> Vec<Range<usize>> {
    self.ranges.iter().map(|&[start, end]| start..end).collect()
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpliceDto {
  pub start: usize,
  pub removed: usize,
  pub inserted: usize,
}

/// A line's text. Lines cut for display carry their full length.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum LineDto {
  Text(String),
  #[serde(rename_all = "camelCase")]
  Cut {
    text: String,
    full_length: usize,
  },
}

impl From<LineText> for LineDto {
  fn from(line: LineText) -> Self {
    match line.full_utf16_len {
      None => Self::Text(line.text),
      Some(full_length) => Self::Cut {
        text: line.text,
        full_length,
      },
    }
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkDto {
  pub start: usize,
  pub lines: Vec<LineDto>,
}

/// How the client updates its lines: replay `splices` on the lines it held
/// at version `base` (or discard them if `base` is `None`), then fill in
/// `chunks`. Afterwards it holds exactly `start_line..end_line` of the view.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinesDto {
  pub base: Option<u64>,
  pub splices: Vec<SpliceDto>,
  pub chunks: Vec<ChunkDto>,
}

impl From<ViewportUpdate> for LinesDto {
  fn from(update: ViewportUpdate) -> Self {
    Self {
      base: update.base.map(Version::get),
      splices: update
        .splices
        .into_iter()
        .map(|splice| SpliceDto {
          start: splice.start,
          removed: splice.removed,
          inserted: splice.inserted,
        })
        .collect(),
      chunks: update
        .chunks
        .into_iter()
        .map(|chunk| ChunkDto {
          start: chunk.start,
          lines: chunk.lines.into_iter().map(LineDto::from).collect(),
        })
        .collect(),
    }
  }
}

/// What the frontend needs to render one viewport of a document.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorView {
  pub document_id: u64,
  pub version: u64,
  pub line_count: usize,
  /// Display rows: lines less those hidden in folded regions, plus the
  /// extra rows of wrapped lines.
  pub row_count: usize,
  /// The viewport this view describes: rows from `start_row`, showing the
  /// `visible` line ranges, which run from `start_line` to `end_line`.
  pub start_row: usize,
  /// The visible lines that wrap, in order.
  pub wraps: Vec<LineWrapDto>,
  pub start_line: usize,
  pub end_line: usize,
  pub visible: Vec<[usize; 2]>,
  /// Visible lines whose region is folded.
  pub folded: Vec<usize>,
  /// Visible lines that start a foldable region.
  pub foldable: Vec<usize>,
  /// Changes whenever the folded regions do.
  pub fold_generation: u64,
  pub lines: LinesDto,
  /// Selections touching the viewport, plus the primary selection.
  pub selections: Vec<SelectionDto>,
  pub selection_count: usize,
  pub primary: CursorDto,
  pub dirty: bool,
  /// Present when the request included a find query.
  pub search: Option<SearchView>,
  /// Syntax highlights for view lines whose spans changed. Lines sent in
  /// `lines.chunks` start with no spans; other lines keep theirs.
  pub highlights: Vec<LineHighlightsDto>,
  /// Diagnostics on the view's lines.
  pub diagnostics: Vec<DiagnosticDto>,
  /// The document's language and its server, if it has a known language.
  pub language: Option<LanguageStatusDto>,
  /// Extension decorations on the view's lines.
  pub decorations: Vec<DecorationDto>,
  /// Changes from the file's original touching the view's lines, for the
  /// gutter (see `quick_diff.rs`).
  pub changes: Vec<GutterChangeDto>,
  /// Extensions' code lenses in the whole document, in order.
  pub lenses: Vec<CodeLensDto>,
  /// Extensions' line annotations on the view's lines.
  pub annotations: Vec<LineAnnotationDto>,
}

/// Faint text an extension shows after a line.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineAnnotationDto {
  pub line: usize,
  pub text: String,
  pub tooltip: Option<String>,
}

/// A link an extension shows above a line.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeLensDto {
  pub line: usize,
  pub title: String,
  pub tooltip: Option<String>,
  pub command: String,
  pub argument: Option<String>,
}

/// Lines changed from a file's original: `start..end` (empty for lines
/// removed before `start`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GutterChangeDto {
  pub start: usize,
  pub end: usize,
  pub kind: GutterChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GutterChangeKind {
  Added,
  Modified,
  Removed,
}

/// A range decorated by an extension.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecorationDto {
  pub start: Position,
  pub end: Position,
  /// The decoration type, `<extension id>/<name>`.
  pub key: String,
}

impl EditorView {
  /// The view of `viewport` for a client holding `known`. `search` builds
  /// the search section for the lines the clamped viewport spans, if
  /// requested; `highlights` the highlights for its visible lines, given
  /// the line ranges sent as chunks.
  pub fn of(
    document: &Document,
    viewport: Viewport,
    known: Option<&KnownDto>,
    search: impl FnOnce(usize, usize) -> Option<SearchView>,
    highlights: impl FnOnce(&[Range<usize>], &[Range<usize>]) -> Vec<LineHighlightsDto>,
  ) -> Self {
    let buffer = document.buffer();
    let known = known.map(|known| KnownLines {
      version: Version::from_raw(known.version),
      ranges: known.ranges(),
    });
    let update = document.viewport_update(
      known.as_ref(),
      viewport.start_row..viewport.end_row,
      MAX_VIEW_LINES,
    );
    let ranges = update.lines.clone();
    let start = ranges.first().map_or(0, |range| range.start);
    let end = ranges.last().map_or(0, |range| range.end);
    let start_row = update.start_row;
    let chunks: Vec<Range<usize>> = update
      .chunks
      .iter()
      .map(|chunk| chunk.start..chunk.start + chunk.lines.len())
      .collect();
    let highlights = highlights(&ranges, &chunks);

    let selections = document.selections();
    let primary_index = selections.primary_index();
    let visible = selections
      .iter()
      .enumerate()
      .filter(|&(index, selection)| {
        index == primary_index
          || (buffer.line_of(selection.end()) >= start && buffer.line_of(selection.start()) < end)
      })
      .take(MAX_VIEW_SELECTIONS)
      .map(|(index, selection)| SelectionDto {
        anchor: buffer.offset_to_utf16(selection.anchor).into(),
        head: buffer.offset_to_utf16(selection.head).into(),
        primary: index == primary_index,
      })
      .collect();

    let head = selections.primary().head;
    let wraps = if document.wrap_config().is_some() {
      ranges
        .iter()
        .flat_map(Clone::clone)
        .filter_map(|line| {
          let wrap = document.line_wrap(line)?;
          let start = buffer.line_start(line);
          Some(LineWrapDto {
            line,
            breaks: wrap
              .breaks
              .iter()
              .map(|&at| buffer.offset_to_utf16(start + at).column)
              .collect(),
            indent: wrap.indent,
          })
        })
        .collect()
    } else {
      Vec::new()
    };
    Self {
      document_id: document.id().get(),
      version: document.version().get(),
      line_count: buffer.line_count(),
      row_count: document.row_count(),
      start_row,
      wraps,
      start_line: start,
      end_line: end,
      visible: ranges
        .iter()
        .map(|range| [range.start, range.end])
        .collect(),
      folded: document.folded_lines(start..end),
      foldable: Vec::new(),
      fold_generation: document.fold_generation(),
      lines: update.into(),
      selections: visible,
      selection_count: selections.len(),
      primary: CursorDto {
        position: buffer.offset_to_utf16(head).into(),
        visual_column: document.visual_column(head),
        row: document.row_of_offset(head),
      },
      dirty: document.is_dirty(),
      search: search(start, end),
      highlights,
      diagnostics: Vec::new(),
      language: None,
      decorations: Vec::new(),
      changes: Vec::new(),
      lenses: Vec::new(),
      annotations: Vec::new(),
    }
  }
}

#[cfg(test)]
// Line ranges are naturally single-element arrays here.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
  use flareite_core::{Editor, MAX_MATCHES};

  use super::*;

  fn ops(json: &str) -> Vec<OpDto> {
    serde_json::from_str(json).expect("valid ops")
  }

  fn viewport(start_row: usize, end_row: usize) -> Viewport {
    Viewport {
      start_row,
      end_row,
      wrap: None,
    }
  }

  fn view(document: &Document, viewport: Viewport, known: Option<&KnownDto>) -> EditorView {
    EditorView::of(document, viewport, known, |_, _| None, |_, _| Vec::new())
  }

  fn json(view: &EditorView) -> serde_json::Value {
    serde_json::to_value(view).expect("serializes")
  }

  #[test]
  fn deserializes_tagged_operations() {
    let parsed = ops(
      r#"[
        {"type": "insert", "text": "hi"},
        {"type": "delete", "direction": "backward", "unit": "word"},
        {"type": "move", "motion": "pageDown", "extend": true, "pageLines": 30},
        {"type": "setCursor", "position": {"line": 1, "column": 2}, "extend": false, "add": true},
        {"type": "addCursor", "direction": "up"},
        {"type": "undo"}
      ]"#,
    );
    let buffer = TextBuffer::from_text("ab\n🦀x");
    let context = OpContext::default();
    let core: Vec<_> = parsed
      .into_iter()
      .map(|op| op.into_core(&buffer, &context))
      .collect();
    assert_eq!(core[0], EditorOp::Insert("hi".into()));
    assert_eq!(
      core[2],
      EditorOp::Move {
        motion: Motion::PageDown { lines: 30 },
        extend: true
      }
    );
    // Line 1, UTF-16 column 2 is after the crab: byte 3 + 4.
    assert_eq!(
      core[3],
      EditorOp::SetCursor {
        offset: 7,
        extend: false,
        add: true
      }
    );
    assert_eq!(core[5], EditorOp::Undo);
  }

  #[test]
  fn auto_inserts_go_where_they_were_asked() {
    let parsed = ops(
      r#"[{"type": "autoInsert", "version": 4, "position": {"line": 1, "column": 2},
           "text": "</p>", "cursor": 0}]"#,
    );
    let buffer = TextBuffer::from_text("🦀\nab");
    let core: Vec<_> = parsed
      .into_iter()
      .map(|op| op.into_core(&buffer, &OpContext::default()))
      .collect();
    assert_eq!(
      core[0],
      EditorOp::AutoInsert {
        version: Version::from_raw(4),
        // After the crab (4 bytes) and the line break, then "ab".
        offset: 7,
        text: "</p>".into(),
        cursor: 0,
      }
    );
  }

  #[test]
  fn typing_enter_and_backspace_take_the_pairs() {
    let parsed = ops(
      r#"[
        {"type": "type", "text": "("},
        {"type": "newline"},
        {"type": "delete", "direction": "backward", "unit": "grapheme"},
        {"type": "delete", "direction": "backward", "unit": "word"},
        {"type": "delete", "direction": "forward", "unit": "grapheme"}
      ]"#,
    );
    let context = OpContext {
      pairs: PairContext {
        cursors: vec![flareite_core::CursorPairs::default()],
        ..PairContext::default()
      },
      ..OpContext::default()
    };
    let buffer = TextBuffer::from_text("");
    let core: Vec<_> = parsed
      .into_iter()
      .map(|op| op.into_core(&buffer, &context))
      .collect();
    let pairs = context.pairs.clone();
    assert_eq!(
      core[0],
      EditorOp::Type {
        text: "(".into(),
        pairs: pairs.clone()
      }
    );
    assert_eq!(core[1], EditorOp::Enter(pairs.clone()));
    assert_eq!(core[2], EditorOp::Backspace(pairs));
    assert!(matches!(
      core[3],
      EditorOp::Delete {
        unit: Unit::Word,
        ..
      }
    ));
    assert!(matches!(
      core[4],
      EditorOp::Delete {
        direction: Direction::Forward,
        ..
      }
    ));
  }

  #[test]
  fn line_operations_take_their_context() {
    let parsed = ops(
      r#"[
        {"type": "paste", "text": "a\r\n"},
        {"type": "paste", "text": "b\n"},
        {"type": "toggleComment"},
        {"type": "moveLines", "direction": "down"},
        {"type": "insertLine", "direction": "up"},
        {"type": "toggleFoldAt", "line": 3},
        {"type": "foldLevel", "level": 0}
      ]"#,
    );
    let context = OpContext {
      comments: CommentTokens {
        line: Some("#".into()),
        block: None,
      },
      folds: vec![3..6],
      copied_lines: Some("a\n".into()),
      pairs: PairContext::default(),
    };
    let buffer = TextBuffer::from_text("");
    let core: Vec<_> = parsed
      .into_iter()
      .map(|op| op.into_core(&buffer, &context))
      .collect();
    assert_eq!(
      core[0],
      EditorOp::Paste {
        text: "a\r\n".into(),
        whole_lines: true
      }
    );
    assert!(matches!(
      core[1],
      EditorOp::Paste {
        whole_lines: false,
        ..
      }
    ));
    assert_eq!(core[2], EditorOp::ToggleComment(context.comments.clone()));
    assert_eq!(core[3], EditorOp::MoveLines(VerticalDirection::Down));
    assert_eq!(core[4], EditorOp::InsertLine(VerticalDirection::Up));
    assert_eq!(
      core[5],
      EditorOp::ToggleFoldAt {
        line: 3,
        regions: vec![3..6]
      }
    );
    assert_eq!(
      core[6],
      EditorOp::FoldLevel {
        level: 1,
        regions: vec![3..6]
      },
      "levels start at 1"
    );
  }

  #[test]
  fn rejects_unknown_operations() {
    assert!(serde_json::from_str::<Vec<OpDto>>(r#"[{"type": "format"}]"#).is_err());
    assert!(
      serde_json::from_str::<Vec<OpDto>>(
        r#"[{"type": "move", "motion": "sideways", "extend": false}]"#
      )
      .is_err()
    );
  }

  #[test]
  fn views_clamp_the_viewport_and_report_utf16_positions() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("one\ntwo\n🦀3".into()))
      .expect("applies");

    let first = json(&view(document, viewport(1, 99), None));
    assert_eq!(
      first["lines"]["chunks"][0]["lines"],
      serde_json::json!(["two", "🦀3"])
    );
    assert_eq!(first["lines"]["base"], serde_json::Value::Null);
    assert_eq!(first["startLine"], 1);
    assert_eq!(first["endLine"], 3);
    assert_eq!(
      first["primary"]["position"],
      serde_json::json!({ "line": 2, "column": 3 })
    );
    assert_eq!(first["primary"]["visualColumn"], 2);
    assert_eq!(first["dirty"], true);

    let clamped = view(document, viewport(50, 10), None);
    assert_eq!((clamped.start_line, clamped.end_line), (2, 3));
  }

  #[test]
  fn views_send_only_what_the_client_is_missing() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("line\n".repeat(50)))
      .expect("applies");
    let first = view(document, viewport(0, 10), None);
    let known = KnownDto {
      version: first.version,
      ranges: first.visible.clone(),
    };
    // The cursor is on line 50, outside the viewport.
    document
      .apply(EditorOp::Insert("!".into()))
      .expect("applies");
    let next = json(&view(document, viewport(0, 10), Some(&known)));
    assert_eq!(next["lines"]["base"], first.version);
    assert_eq!(next["lines"]["chunks"], serde_json::json!([]));
    assert_eq!(next["lines"]["splices"].as_array().map(Vec::len), Some(1));
  }

  /// A performance budget that doesn't depend on timing, so it holds on
  /// any machine: what a keystroke and a page of scrolling cost on the wire
  /// in a large document. A change that resends lines or grows the view
  /// fails here before it shows up in the end-to-end benchmarks.
  #[test]
  fn views_say_where_lines_wrap_in_utf16_columns() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("ééé ééé ééé\nx".into()))
      .expect("applies");
    let wrapped: Viewport = serde_json::from_str(
      r#"{"startRow": 0, "endRow": 10, "wrap": {"columns": 8, "indent": "same"}}"#,
    )
    .expect("parses");
    document.set_wrap(wrapped.wrap_config());
    let shown = json(&view(document, wrapped, None));
    assert_eq!(shown["rowCount"], 3);
    // "ééé ééé " is 8 UTF-16 units (14 bytes).
    assert_eq!(
      shown["wraps"],
      serde_json::json!([{"line": 0, "breaks": [8], "indent": 0}])
    );
    assert_eq!(shown["primary"]["row"], 2, "the cursor is after `x`");
    let unwrapped: Viewport =
      serde_json::from_str(r#"{"startRow": 0, "endRow": 10}"#).expect("parses");
    document.set_wrap(unwrapped.wrap_config());
    let shown = json(&view(document, viewport(0, 10), None));
    assert_eq!(shown["rowCount"], 2);
    assert_eq!(shown["wraps"], serde_json::json!([]));
  }

  #[test]
  fn large_documents_cost_the_same_per_keystroke_as_small_ones() {
    const LINES: usize = 200_000;
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    let line = format!("{}\n", "x".repeat(79));
    document
      .apply(EditorOp::Insert(line.repeat(LINES)))
      .expect("applies");
    let at = document.buffer().line_start(LINES / 2);
    document
      .apply(EditorOp::SetCursor {
        offset: at,
        extend: false,
        add: false,
      })
      .expect("moves");
    let page = viewport(LINES / 2 - 30, LINES / 2 + 30);
    let first = view(document, page, None);
    let known = |view: &EditorView| KnownDto {
      version: view.version,
      ranges: view.visible.clone(),
    };

    document
      .apply(EditorOp::Insert("y".into()))
      .expect("applies");
    let keystroke = view(document, page, Some(&known(&first)));
    let sent: usize = keystroke.lines.chunks.iter().map(|c| c.lines.len()).sum();
    let bytes = serde_json::to_string(&keystroke).expect("serializes").len();
    assert_eq!(sent, 1, "a keystroke resends only its line");
    assert!(bytes < 1024, "a keystroke's view is {bytes} bytes");

    let next_page = viewport(LINES / 2 + 30, LINES / 2 + 90);
    let scrolled = view(document, next_page, Some(&known(&keystroke)));
    let sent: usize = scrolled.lines.chunks.iter().map(|c| c.lines.len()).sum();
    let bytes = serde_json::to_string(&scrolled).expect("serializes").len();
    assert!(sent <= 60, "a page of scrolling sent {sent} lines");
    assert!(
      bytes < 60 * 100 + 1024,
      "a page of scrolling is {bytes} bytes"
    );
  }

  #[test]
  fn cut_lines_serialize_with_their_full_length() {
    let line = LineDto::from(LineText {
      text: "abc".into(),
      full_utf16_len: Some(99),
    });
    assert_eq!(
      serde_json::to_value(line).expect("serializes"),
      serde_json::json!({ "text": "abc", "fullLength": 99 })
    );
  }

  #[test]
  fn views_include_only_nearby_selections_and_the_primary() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert("a\n".repeat(100)))
      .expect("applies");
    for op in [
      EditorOp::SetCursor {
        offset: 0,
        extend: false,
        add: false,
      },
      EditorOp::SetCursor {
        offset: 100,
        extend: false,
        add: true,
      },
    ] {
      document.apply(op).expect("applies");
    }
    let first = view(document, viewport(0, 10), None);
    assert_eq!(first.selection_count, 2);
    assert_eq!(
      first.selections.len(),
      2,
      "line 0 is visible and line 50 is primary"
    );

    document
      .apply(EditorOp::SetCursor {
        offset: 2,
        extend: false,
        add: true,
      })
      .expect("applies");
    let later = view(document, viewport(90, 99), None);
    assert_eq!(later.selections.len(), 1);
    assert!(later.selections[0].primary);
  }

  #[test]
  fn large_documents_count_matches_in_the_background() {
    let mut editor = Editor::default();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    // Large enough to count in the background, with a needle every ten lines
    // (so the total stays under the match limit).
    let block = format!("needle\n{}", "haystack haystack haystack\n".repeat(9));
    let blocks = SYNC_COUNT_BYTES / block.len() + 1;
    document
      .apply(EditorOp::Insert(block.repeat(blocks)))
      .expect("applies");
    let query = SearchQuery::literal("needle");

    let (pending, needs_count) = SearchView::build(document, &query, 0, 20, None);
    assert!(needs_count);
    assert_eq!(pending.total, None, "counting");
    assert_eq!(
      pending.matches.len(),
      2,
      "visible matches are found right away"
    );

    let searcher = query.compile().expect("compiles");
    let buffer = document.buffer();
    let count = MatchCount {
      version: document.version(),
      query: query.clone(),
      matches: searcher.find_in_buffer(buffer, 0..buffer.len(), MAX_MATCHES + 1),
      truncated: false,
    };
    let (counted, needs_count) = SearchView::build(document, &query, 0, 20, Some(&count));
    assert!(!needs_count);
    assert_eq!(counted.total, Some(blocks));

    let stale = MatchCount {
      query: SearchQuery::literal("other"),
      ..count
    };
    assert!(SearchView::build(document, &query, 0, 20, Some(&stale)).1);
  }
}
