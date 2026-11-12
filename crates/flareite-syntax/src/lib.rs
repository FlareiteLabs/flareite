//! Syntax support for Flareite, built on Tree-sitter.
//!
//! - [`Registry`] holds the [`Language`]s, which all come from extensions
//!   as Tree-sitter grammars compiled to WebAssembly
//!   ([`LanguageDefinition`]), and maps file names to them. No language is
//!   built in. A language's grammar and queries are loaded and compiled the
//!   first time a file of that language is parsed.
//! - [`parse`] (re)parses a buffer or snapshot into a [`SyntaxTree`],
//!   incrementally from an edited old one, and can be stopped part way (by
//!   a time budget or cancellation) so callers can fall back to a
//!   background parse instead of blocking. Languages embedded in the text
//!   (injections, such as CSS in HTML) are parsed as layers of the tree.
//! - [`highlight_lines`] turns a tree into per-line [`Span`]s of
//!   [`TokenKind`]s for display.
//! - [`fold_ranges`] finds the foldable regions a language's fold query
//!   captures.
//! - [`language_at`] and [`scope_at`] tell what's at a position: the
//!   (injected) language there, and whether it's in a string or comment.
//!
//! Positions are UTF-8 byte offsets, as everywhere in Flareite.

mod folds;
mod highlight;
mod injections;
mod languages;
mod parse;
mod scope;
#[cfg(test)]
mod testing;

pub use folds::fold_ranges;
pub use highlight::{Span, TokenKind, highlight_lines};
pub use languages::{Language, LanguageDefinition, MAX_GRAMMAR_BYTES, MAX_QUERY_BYTES, Registry};
pub use parse::{SyntaxError, SyntaxTree, TextSource, apply_edits, deadline, parse};
pub use scope::{language_at, scope_at};
