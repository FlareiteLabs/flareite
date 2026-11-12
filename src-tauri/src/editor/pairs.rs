//! The pairs typing uses at each cursor (see `flareite_core`'s `pairs`):
//! those of the language there, which inside an embedded language (CSS in
//! HTML, code in a Markdown fence) is the embedded one if it declares any,
//! and whether the cursor is in a string or a comment, from the syntax tree.

use std::sync::Arc;

use flareite_core::{CursorPairs, Document, LanguagePairs, PairContext, Scope};
use flareite_syntax::{TokenKind, language_at, scope_at};
use tauri::{AppHandle, Manager};

use super::EditorState;
use super::protocol::{DirectionDto, OpDto, UnitDto};
use crate::languages::LanguagesState;
use crate::settings::SettingsState;

/// What an operation needs to know about pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
  /// Each cursor's pairs (Enter, Backspace).
  Pairs,
  /// Each cursor's pairs and scope (typing).
  PairsAndScopes,
}

pub fn need(op: &OpDto) -> Option<Need> {
  match op {
    OpDto::Type { .. } => Some(Need::PairsAndScopes),
    OpDto::Newline
    | OpDto::Delete {
      direction: DirectionDto::Backward,
      unit: UnitDto::Grapheme,
    } => Some(Need::Pairs),
    _ => None,
  }
}

/// The pairs at each of `document`'s selections, with the user's settings.
/// Called with the editor locked.
pub fn context(app: &AppHandle, document: &Document, need: Need) -> PairContext {
  let settings = app.state::<SettingsState>().pair_settings();
  let languages = app.state::<LanguagesState>();
  let all = languages.pairs();
  if all.is_empty() {
    return PairContext {
      settings,
      cursors: Vec::new(),
    };
  }
  let none = Arc::new(LanguagePairs::default());
  let of = |id: &str| all.get(id).cloned().unwrap_or_else(|| none.clone());
  let syntax = &app.state::<EditorState>().syntax;
  let cursors = syntax.with_tree(app, document, |tree| {
    let Some(tree) = tree else {
      // Plain text, or not parsed yet: the document's language.
      let pairs = languages
        .for_document(document)
        .map_or_else(|| none.clone(), |language| of(language.id()));
      return document
        .selections()
        .iter()
        .map(|_| CursorPairs {
          pairs: pairs.clone(),
          scope: Scope::Code,
        })
        .collect();
    };
    document
      .selections()
      .iter()
      .map(|s| {
        let language = language_at(tree, s.head, |language| all.contains_key(language.id()));
        let pairs = of(language.id());
        let excludes = pairs
          .auto_closing
          .iter()
          .any(|pair| pair.not_in_string || pair.not_in_comment);
        let scope = if need == Need::PairsAndScopes && excludes {
          match scope_at(tree, document.buffer(), s.head) {
            Some(TokenKind::String) => Scope::String,
            Some(TokenKind::Comment) => Scope::Comment,
            _ => Scope::Code,
          }
        } else {
          Scope::Code
        };
        CursorPairs { pairs, scope }
      })
      .collect()
  });
  PairContext { settings, cursors }
}
