//! Flareite's editor core: open documents, selections, editing operations,
//! undo history, and dirty state.
//!
//! The core is independent of any UI and performs no I/O. Callers load files
//! with `flareite-workspace`, hand the text to [`Editor::open`], and save by
//! writing a [`SavePoint`] snapshot and then calling
//! [`Document::mark_saved`].
//!
//! Positions are UTF-8 byte offsets, as in `flareite-text`.

mod brackets;
mod document;
mod editor;
mod error;
mod find;
mod folds;
mod history;
mod lines;
mod ops;
mod pairs;
mod regions;
mod selection;
#[cfg(test)]
mod testing;
mod view;

pub use document::{
  Document, DocumentId, EXACT_COLUMN_BYTES, EditorConfig, LanguageChoice, SavePoint, ViewId,
};
pub use editor::Editor;
pub use error::CoreError;
pub use find::{MAX_MATCHES, MatchSummary};
pub use history::{EditKind, HistoryLimits, StateId};
pub use lines::CommentTokens;
pub use ops::{Direction, EditorOp, Motion, Unit, VerticalDirection};
pub use pairs::{
  AutoClose, AutoClosingPair, Closers, CursorPairs, DEFAULT_AUTO_CLOSE_BEFORE, LanguagePairs,
  MAX_PAIR_CHARS, PairContext, PairSettings, Scope, Surround,
};
pub use regions::{MarkedRegion, RegionMarkers, SectionHeader};
pub use selection::{Selection, Selections};
pub use view::{KnownLines, LineChunk, LineText, MAX_DISPLAY_LINE_BYTES, ViewportUpdate};
