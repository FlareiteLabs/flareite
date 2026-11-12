//! The host functions of the `views` interface (API 1.7): side bar views as
//! trees with icons, marks, menus, and children loaded on demand, and their
//! text boxes, welcome messages, badges, and flags.
//!
//! Items are checked as `workbench::set-view-items` checks them: commands
//! must be the extension's own, parents must come first, and ids must be
//! unique in the view, children loaded later included. Icons are named
//! icons or file paths the workbench finds icon-theme icons for, and mark
//! colors are the manifest's, sent on as theme tokens.

use std::collections::HashSet;

use crate::api::ApiContext;
use crate::bindings::flareite::extension::types::ApiError;
use crate::bindings::flareite::extension::views::{
  Expansion as WitExpansion, GraphRow as WitGraphRow, Host, ItemIcon as WitIcon, TreeItem,
  Welcome as WitWelcome,
};
use crate::contributions::ACTION_ICONS;
use crate::runtime::StoreState;
use crate::{
  GraphLane, GraphRow, ItemIcon, ItemMark, ViewContent, ViewEntry, Welcome, WelcomeButton,
};

/// Most items in a view, loaded children included.
pub const MAX_ITEMS: usize = 10_000;
const MAX_FLAGS: usize = 32;
/// Most lanes at a graph row's top or bottom.
const MAX_LANES: usize = 64;
const MAX_INPUT_BYTES: usize = 64 * 1024;

fn invalid(message: String) -> ApiError {
  ApiError::InvalidArgument(message)
}

fn cut(text: &str, max: usize) -> String {
  match text.char_indices().nth(max) {
    Some((end, _)) => format!("{}…", &text[..end]),
    None => text.to_owned(),
  }
}

impl ApiContext {
  /// Checks tree items, whose parents must be `parent` (if given), an
  /// item in `known`, or one listed before them; their ids must not be in
  /// `known`.
  fn tree_items(
    &self,
    items: Vec<TreeItem>,
    parent: Option<&str>,
    known: &HashSet<String>,
  ) -> Result<Vec<ViewEntry>, ApiError> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut entries = Vec::with_capacity(items.len());
    for item in items {
      if item.id.is_empty() || item.id.len() > 200 {
        return Err(invalid("view item ids must be 1 to 200 bytes".into()));
      }
      if known.contains(&item.id) || !seen.insert(item.id.clone()) {
        return Err(invalid(format!("view item {:?} appears twice", item.id)));
      }
      match (&item.parent, parent) {
        (Some(of), _) if seen.contains(of) && of != &item.id => {}
        (Some(of), Some(parent)) if of == parent => {}
        (None, None) => {}
        (None, Some(parent)) => {
          return Err(invalid(format!(
            "view item {:?} must be inside {parent:?}",
            item.id
          )));
        }
        (Some(of), _) => {
          return Err(invalid(format!(
            "view item {:?}: its parent {of:?} must come before it",
            item.id
          )));
        }
      }
      self.own_command(item.command.as_ref())?;
      let icon = match item.icon {
        WitIcon::None => None,
        WitIcon::Named(name) if ACTION_ICONS.contains_key(&name) => Some(ItemIcon::Named { name }),
        WitIcon::Named(name) => {
          return Err(invalid(format!("{name:?} is not one of the named icons")));
        }
        WitIcon::File(path) => Some(ItemIcon::File { path }),
        WitIcon::Folder(path) => Some(ItemIcon::Folder { path }),
      };
      let mark = match item.mark {
        Some(mark) => {
          if let Some(badge) = &mark.badge
            && (!(1..=2).contains(&badge.chars().count())
              || badge.chars().any(|c| c.is_control() || c.is_whitespace()))
          {
            return Err(invalid(format!(
              "badge {badge:?} must be one or two characters"
            )));
          }
          let color = match &mark.color {
            Some(name) => Some(
              self
                .manifest()
                .contributions
                .color(name)
                .ok_or_else(|| invalid(format!("no color {name:?} is declared")))?
                .id
                .clone(),
            ),
            None => None,
          };
          Some(ItemMark {
            badge: mark.badge,
            color,
            strikethrough: mark.strikethrough,
            faded: mark.faded,
          })
        }
        None => None,
      };
      if let Some(context) = &item.context
        && (context.is_empty() || context.len() > 64)
      {
        return Err(invalid("item contexts must be 1 to 64 bytes".into()));
      }
      entries.push(ViewEntry {
        id: item.id,
        parent: item.parent,
        label: cut(&item.label, 200),
        description: item.description.map(|text| cut(&text, 200)),
        tooltip: item.tooltip.map(|text| cut(&text, 2_000)),
        expanded: item.expansion == WitExpansion::Expanded,
        command: item.command,
        argument: item.argument.map(|text| cut(&text, 4_000)),
        icon,
        mark,
        context: item.context,
        expandable: item.expansion != WitExpansion::None,
      });
    }
    Ok(entries)
  }

  /// Changes one of the extension's views (by its own id).
  fn change_view<R>(
    &self,
    view: &str,
    change: impl FnOnce(&mut ViewContent) -> R,
  ) -> Result<R, ApiError> {
    let full_id = self.view_id(view)?;
    Ok(self.shared()?.update_view(&full_id, change))
  }
}

/// The ids of `parent`'s descendants in `items`.
fn descendants(items: &[ViewEntry], parent: &str) -> HashSet<String> {
  let mut found: HashSet<String> = HashSet::new();
  // Parents come before their children, so one pass finds them all.
  for item in items {
    if let Some(of) = &item.parent
      && (of == parent || found.contains(of))
    {
      found.insert(item.id.clone());
    }
  }
  found
}

impl Host for StoreState {
  fn set_items(&mut self, view: String, items: Vec<TreeItem>) -> Result<(), ApiError> {
    if items.len() > MAX_ITEMS {
      return Err(ApiError::TooLarge(format!(
        "a view can show at most {MAX_ITEMS} items"
      )));
    }
    let entries = self.api.tree_items(items, None, &HashSet::new())?;
    self
      .api
      .change_view(&view, |content| content.items = entries)
  }

  fn set_children(
    &mut self,
    view: String,
    parent: String,
    items: Vec<TreeItem>,
  ) -> Result<bool, ApiError> {
    let full_id = self.api.view_id(&view)?;
    let shared = self.api.shared()?;
    let current = shared.view_snapshot(&full_id);
    if !current.items.iter().any(|item| item.id == parent) {
      return Ok(false);
    }
    let replaced = descendants(&current.items, &parent);
    let known: HashSet<String> = current
      .items
      .iter()
      .filter(|item| !replaced.contains(&item.id))
      .map(|item| item.id.clone())
      .collect();
    if known.len() + items.len() > MAX_ITEMS {
      return Err(ApiError::TooLarge(format!(
        "a view can show at most {MAX_ITEMS} items"
      )));
    }
    let entries = self.api.tree_items(items, Some(&parent), &known)?;
    Ok(shared.update_view(&full_id, |content| {
      // Changed meanwhile (on this thread, nothing else changes it).
      if !content.items.iter().any(|item| item.id == parent) {
        return false;
      }
      let replaced = descendants(&content.items, &parent);
      content.items.retain(|item| !replaced.contains(&item.id));
      let at = content
        .items
        .iter()
        .position(|item| item.id == parent)
        .map_or(content.items.len(), |index| index + 1);
      content.items.splice(at..at, entries);
      true
    }))
  }

  fn set_welcome(&mut self, view: String, welcome: Option<WitWelcome>) -> Result<(), ApiError> {
    let welcome = match welcome {
      Some(welcome) => {
        if welcome.buttons.len() > 4 {
          return Err(invalid("a welcome message has at most 4 buttons".into()));
        }
        let mut buttons = Vec::new();
        for button in welcome.buttons {
          self.api.own_command(Some(&button.command))?;
          buttons.push(WelcomeButton {
            label: cut(button.label.trim(), 40),
            command: button.command,
            argument: button.argument.map(|text| cut(&text, 4_000)),
          });
        }
        Some(Welcome {
          text: cut(&welcome.text, 1_000),
          buttons,
        })
      }
      None => None,
    };
    self
      .api
      .change_view(&view, |content| content.welcome = welcome)
  }

  fn set_graph(&mut self, view: String, rows: Vec<WitGraphRow>) -> Result<(), ApiError> {
    if rows.len() > MAX_ITEMS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_ITEMS} graph rows"
      )));
    }
    let id = |id: &str| -> Result<(), ApiError> {
      if id.is_empty() || id.len() > 200 {
        Err(invalid("graph ids are 1 to 200 bytes".into()))
      } else {
        Ok(())
      }
    };
    let mut graph = Vec::with_capacity(rows.len());
    for row in rows {
      if row.inputs.len() > MAX_LANES
        || row.outputs.len() > MAX_LANES
        || row.parents.len() > MAX_LANES
      {
        return Err(ApiError::TooLarge(format!(
          "at most {MAX_LANES} lanes in a graph row"
        )));
      }
      id(&row.item)?;
      id(&row.node)?;
      let lanes = |lanes: Vec<crate::bindings::flareite::extension::views::GraphLane>| {
        lanes
          .into_iter()
          .map(|lane| {
            id(&lane.id)?;
            Ok(GraphLane {
              id: lane.id,
              color: lane.color % 5,
            })
          })
          .collect::<Result<Vec<_>, ApiError>>()
      };
      for parent in &row.parents {
        id(parent)?;
      }
      graph.push(GraphRow {
        item: row.item,
        node: row.node,
        parents: row.parents,
        inputs: lanes(row.inputs)?,
        outputs: lanes(row.outputs)?,
      });
    }
    self.api.change_view(&view, |content| content.graph = graph)
  }

  fn set_visible(&mut self, view: String, visible: bool) -> Result<(), ApiError> {
    self
      .api
      .change_view(&view, |content| content.visible = Some(visible))
  }

  fn set_badge(&mut self, view: String, count: u32) -> Result<(), ApiError> {
    self.api.change_view(&view, |content| content.badge = count)
  }

  fn set_flags(&mut self, view: String, names: Vec<String>) -> Result<(), ApiError> {
    if names.len() > MAX_FLAGS
      || names
        .iter()
        .any(|name| name.is_empty() || name.len() > 64 || name.starts_with('!'))
    {
      return Err(invalid(format!(
        "at most {MAX_FLAGS} flags of 1 to 64 bytes, not starting with !"
      )));
    }
    self.api.change_view(&view, |content| content.flags = names)
  }

  fn input(&mut self, view: String) -> Result<String, ApiError> {
    let full_id = self.api.view_id(&view)?;
    Ok(self.api.shared()?.view_snapshot(&full_id).input)
  }

  fn set_input(&mut self, view: String, text: String) -> Result<(), ApiError> {
    if text.len() > MAX_INPUT_BYTES {
      return Err(ApiError::TooLarge("the text is at most 64 KiB".into()));
    }
    if self
      .api
      .manifest()
      .contributions
      .view(&view)
      .is_some_and(|declared| declared.input.is_none())
    {
      return Err(invalid(format!("the view {view:?} has no text box")));
    }
    self.api.change_view(&view, |content| content.input = text)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn entry(id: &str, parent: Option<&str>) -> ViewEntry {
    ViewEntry {
      id: id.into(),
      parent: parent.map(str::to_owned),
      label: id.into(),
      description: None,
      tooltip: None,
      expanded: false,
      command: None,
      argument: None,
      icon: None,
      mark: None,
      context: None,
      expandable: false,
    }
  }

  #[test]
  fn finds_every_descendant() {
    let items = [
      entry("a", None),
      entry("a1", Some("a")),
      entry("a1x", Some("a1")),
      entry("b", None),
      entry("b1", Some("b")),
    ];
    let found = descendants(&items, "a");
    assert_eq!(found, HashSet::from(["a1".to_owned(), "a1x".to_owned()]));
    assert!(descendants(&items, "a1x").is_empty());
  }
}
