//! Layouts (API 1.15): tabs an extension splits into panes, as Git's diff
//! and merge editors and HTML & CSS's live preview are.
//!
//! A tab is named by its layout (`<extension id>.<layout id>`) and a key the
//! extension chooses. Its content is a [`LayoutSpec`] the extension sets
//! with `set-layout`, checked here: splits and panes (editors of documents,
//! pages of the extension's, and files of the open folder shown as pages),
//! headers, and buttons. Editor panes show what the extension sets with
//! `set-pane-decorations` ([`PaneDecorations`]) besides their text: ranges
//! decorated with its decoration types, zones (rows between lines), folds,
//! and marks beside the scroll bar.
//!
//! The workbench owns the tabs, as it owns viewers': they're saved with the
//! window's layout and stay open when their extension stops. It asks the
//! extension to fill a tab in by running the layout's command (see
//! `layouts::filling`), and tells it when the tab's documents are open and
//! when it closes. The host keeps each open tab's layout and decorations
//! (`host::layout_tabs`), so a tab shown again draws them at once.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::bindings::flareite::extension::layouts::{
  Button as WitButton, Decorated, Direction, FillReason, FillRequest, Host, InvokedFrom, Layout,
  LayoutTab as WitTab, Navigation, NodeKind, PaneContent as WitContent,
  PaneDecorations as WitDecorations, PaneMessage, PaneSelection, TabDocument as WitDocument,
  WebAction as WitWebAction, ZoneContent as WitZoneContent,
};
use crate::bindings::flareite::extension::types::{ApiError, Range as WitRange};
use crate::contributions::ACTION_ICONS;
use crate::runtime::StoreState;
use crate::worker::Payload;

/// Most nodes and panes in a layout.
pub const MAX_NODES: usize = 32;
pub const MAX_PANES: usize = 16;
/// Most buttons in a header or beside a tab.
pub const MAX_BUTTONS: usize = 20;
/// Most decorated ranges, zones, folds, and ruler marks in a pane.
pub const MAX_PANE_RANGES: usize = 50_000;
pub const MAX_ZONES: usize = 10_000;
pub const MAX_FOLDS: usize = 10_000;
pub const MAX_RULER_MARKS: usize = 10_000;
/// Most lines a zone shows of another document.
pub const MAX_ZONE_LINES: u32 = 10_000;
/// Most links in a zone.
const MAX_ZONE_LINKS: usize = 10;
/// Longest key, URI, argument, and node id.
pub const MAX_KEY_BYTES: usize = 4 * 1024;
const MAX_ARGUMENT_BYTES: usize = 4 * 1024;
const MAX_NODE_ID_CHARS: usize = 100;
/// Most breakpoints, and documents besides the panes'.
const MAX_BREAKPOINTS: usize = 8;
const MAX_DOCUMENTS: usize = 16;

/// A tab of a layout: the layout's full id and the tab's key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabId {
  /// `<extension id>.<layout id>`.
  pub layout: String,
  pub key: String,
}

/// What a tab shows, as its extension set it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutSpec {
  pub title: String,
  /// Parents first; the first fills the tab.
  pub nodes: Vec<LayoutNode>,
  /// Documents kept open besides the editor panes'.
  pub documents: Vec<String>,
  pub buttons: Vec<Button>,
  /// Sorted, without repeats.
  pub breakpoints: Vec<u32>,
  pub main: Option<String>,
}

impl LayoutSpec {
  /// The panes, by id.
  pub fn panes(&self) -> impl Iterator<Item = (&str, &PaneSpec)> {
    self.nodes.iter().filter_map(|node| match &node.kind {
      NodeSpec::Pane(pane) => Some((node.id.as_str(), pane)),
      NodeSpec::Split { .. } => None,
    })
  }

  pub fn pane(&self, id: &str) -> Option<&PaneSpec> {
    self
      .panes()
      .find_map(|(pane, spec)| (pane == id).then_some(spec))
  }

  /// Every document the tab keeps open: its editors', then the others.
  pub fn all_documents(&self) -> BTreeSet<&str> {
    self
      .panes()
      .filter_map(|(_, pane)| match &pane.content {
        PaneContent::Editor { uri, .. } => Some(uri.as_str()),
        _ => None,
      })
      .chain(self.documents.iter().map(String::as_str))
      .collect()
  }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutNode {
  pub id: String,
  pub parent: Option<String>,
  /// Its share of the parent split; positive.
  pub size: f32,
  #[serde(flatten)]
  pub kind: NodeSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NodeSpec {
  Split {
    /// `row` (side by side) or `column` (stacked).
    direction: &'static str,
  },
  Pane(PaneSpec),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneSpec {
  pub content: PaneContent,
  pub header: Option<Header>,
  pub scroll_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PaneContent {
  #[serde(rename_all = "camelCase")]
  Editor {
    uri: String,
    minimap: bool,
    folding: bool,
    quick_diff: bool,
  },
  /// A page of the extension's: `page` in its folder, or HTML it sets.
  /// `id` is its webview's, set by the host.
  #[serde(rename_all = "camelCase")]
  Webview { page: Option<String>, id: String },
  /// A file of the open folder, shown as a page; `id` is its webview's.
  #[serde(rename_all = "camelCase")]
  FilePage { path: String, id: String },
  /// A page from the web, in a browser view of its own; `id` names the
  /// view.
  #[serde(rename_all = "camelCase")]
  WebPage { url: String, id: String },
}

/// Where an extension moves a web page pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "url", rename_all = "camelCase")]
pub enum WebAction {
  Go(String),
  Back,
  Forward,
  Reload,
  Stop,
}

/// Whether `url` is a web address a web page pane may show.
pub fn is_web_address(url: &str) -> bool {
  let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
  (lower.starts_with("https://") || lower.starts_with("http://"))
    && url.len() <= MAX_KEY_BYTES
    && !url.contains(char::is_whitespace)
    && !url.contains(char::is_control)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
  pub title: String,
  pub description: Option<String>,
  pub buttons: Vec<Button>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Button {
  pub id: String,
  pub title: String,
  pub icon: Option<String>,
  pub command: String,
  pub argument: Option<String>,
  pub primary: bool,
  pub pressed: Option<bool>,
  pub disabled: bool,
}

/// A position in a document: in UTF-16 code units, as the workbench counts
/// them, once converted from the API's UTF-8 columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Point {
  pub line: u32,
  pub column: u32,
}

/// A range decorated with a decoration type, `<extension id>/<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecoratedRange {
  pub key: String,
  pub start: Point,
  pub end: Point,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LineRange {
  pub start: u32,
  pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneLink {
  pub title: String,
  pub tooltip: Option<String>,
  pub command: String,
  pub argument: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ZoneContent {
  Filler {
    rows: u32,
  },
  Text {
    text: String,
    tooltip: Option<String>,
    command: Option<String>,
    argument: Option<String>,
  },
  Links {
    label: Option<String>,
    links: Vec<ZoneLink>,
  },
  Lines {
    uri: String,
    lines: LineRange,
    decorations: Vec<DecoratedRange>,
    numbers: bool,
  },
}

impl ZoneContent {
  /// How many rows it takes.
  pub fn rows(&self) -> u32 {
    match self {
      Self::Filler { rows } => *rows,
      Self::Text { .. } | Self::Links { .. } => 1,
      Self::Lines { lines, .. } => lines.end - lines.start,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Zone {
  pub line: u32,
  #[serde(flatten)]
  pub content: ZoneContent,
  /// A decoration type's key, whose background it has.
  pub decoration: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RulerMark {
  pub lines: LineRange,
  pub decoration: String,
}

/// What an editor pane shows besides its text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneDecorations {
  pub decorations: Vec<DecoratedRange>,
  pub zones: Vec<Zone>,
  pub folds: Vec<LineRange>,
  pub ruler: Vec<RulerMark>,
}

/// Why a layout's command runs; see `layouts::filling`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
  Opened,
  Restored,
  Resized,
  Ready,
  Closed,
}

impl Fill {
  /// As the workbench names it.
  pub fn parse(reason: &str) -> Option<Self> {
    Some(match reason {
      "opened" => Self::Opened,
      "restored" => Self::Restored,
      "resized" => Self::Resized,
      "ready" => Self::Ready,
      "closed" => Self::Closed,
      _ => return None,
    })
  }

  fn wit(self) -> FillReason {
    match self {
      Self::Opened => FillReason::Opened,
      Self::Restored => FillReason::Restored,
      Self::Resized => FillReason::Resized,
      Self::Ready => FillReason::Ready,
      Self::Closed => FillReason::Closed,
    }
  }
}

/// What a layout's command was run for (a payload).
#[derive(Debug, Clone)]
pub enum LayoutCall {
  /// Filling a tab in.
  Fill {
    tab: TabId,
    reason: Fill,
    width: u32,
  },
  /// A button or zone was clicked; `pane` is `None` for the tab's own.
  Invoked { tab: TabId, pane: Option<String> },
  /// A webview pane's page sent a message (JSON text).
  Message {
    tab: TabId,
    pane: String,
    data: String,
  },
  /// A web page pane went somewhere, finished loading, or got its title.
  Navigated {
    tab: TabId,
    pane: String,
    url: String,
    title: Option<String>,
    loading: bool,
  },
}

/// One of a tab's open documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabDocument {
  pub uri: String,
  pub document: u64,
}

fn invalid(message: impl Into<String>) -> ApiError {
  ApiError::InvalidArgument(message.into())
}

/// `text`, trimmed, if it's 1 to `max` characters.
fn title(what: &str, text: &str, max: usize) -> Result<String, ApiError> {
  let text = text.trim();
  if text.is_empty() || text.chars().count() > max || text.contains(char::is_control) {
    return Err(invalid(format!(
      "{what} must be 1 to {max} characters, without control characters"
    )));
  }
  Ok(text.to_owned())
}

/// Optional text, cut to `max` characters.
fn optional(text: Option<String>, max: usize) -> Option<String> {
  text
    .map(|text| text.trim().chars().take(max).collect::<String>())
    .filter(|text| !text.is_empty())
}

fn argument(argument: Option<String>) -> Result<Option<String>, ApiError> {
  if argument
    .as_ref()
    .is_some_and(|argument| argument.len() > MAX_ARGUMENT_BYTES)
  {
    return Err(ApiError::TooLarge("an argument is at most 4 KiB".into()));
  }
  Ok(argument)
}

fn icon(icon: Option<String>) -> Result<Option<String>, ApiError> {
  match icon {
    Some(icon) if !ACTION_ICONS.contains_key(&icon) => Err(invalid(format!(
      "icon {icon:?} isn't one of the named icons"
    ))),
    icon => Ok(icon),
  }
}

fn point(position: crate::bindings::flareite::extension::types::Position) -> Point {
  Point {
    line: position.line,
    column: position.column,
  }
}

fn line_range(
  lines: crate::bindings::flareite::extension::layouts::LineRange,
) -> Result<LineRange, ApiError> {
  if lines.end < lines.start {
    return Err(ApiError::InvalidRange(format!(
      "lines {} to {} end before they start",
      lines.start, lines.end
    )));
  }
  Ok(LineRange {
    start: lines.start,
    end: lines.end,
  })
}

impl StoreState {
  /// A layout of the extension's, as the manifest declares it: its full id.
  fn layout_id(&self, layout: &str) -> Result<String, ApiError> {
    self
      .api
      .manifest()
      .contributions
      .layout(layout)
      .map(|declared| declared.full_id.clone())
      .ok_or_else(|| invalid(format!("no layout {layout:?} is declared")))
  }

  fn tab_id(&self, tab: &WitTab) -> Result<TabId, ApiError> {
    let layout = self.layout_id(&tab.layout)?;
    if tab.key.is_empty() || tab.key.len() > MAX_KEY_BYTES || tab.key.contains(char::is_control) {
      return Err(invalid(format!(
        "a tab's key must be 1 to {MAX_KEY_BYTES} bytes, without control characters"
      )));
    }
    Ok(TabId {
      layout,
      key: tab.key.clone(),
    })
  }

  /// A decoration type of the extension's: its key.
  fn decoration_key(&self, name: &str) -> Result<String, ApiError> {
    self
      .api
      .manifest()
      .contributions
      .decoration(name)
      .map(|declared| declared.key.clone())
      .ok_or_else(|| invalid(format!("no decoration type {name:?} is declared")))
  }

  fn button(&self, button: WitButton) -> Result<Button, ApiError> {
    self.api.own_command(Some(&button.command))?;
    if button.id.is_empty() || button.id.chars().count() > MAX_NODE_ID_CHARS {
      return Err(invalid("a button's id must be 1 to 100 characters"));
    }
    Ok(Button {
      id: button.id,
      title: title("a button's title", &button.title, 100)?,
      icon: icon(button.icon)?,
      command: button.command,
      argument: argument(button.argument)?,
      primary: button.primary,
      pressed: button.pressed,
      disabled: button.disabled,
    })
  }

  fn buttons(&self, buttons: Vec<WitButton>) -> Result<Vec<Button>, ApiError> {
    if buttons.len() > MAX_BUTTONS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_BUTTONS} buttons in one place"
      )));
    }
    let mut ids = BTreeSet::new();
    let buttons = buttons
      .into_iter()
      .map(|button| self.button(button))
      .collect::<Result<Vec<_>, _>>()?;
    for button in &buttons {
      if !ids.insert(button.id.as_str()) {
        return Err(invalid(format!("two buttons have the id {:?}", button.id)));
      }
    }
    Ok(buttons)
  }

  /// Checks a document a layout names: an absolute path, or an address an
  /// extension provides.
  fn layout_document(&self, uri: &str) -> Result<(), ApiError> {
    if uri.len() > MAX_KEY_BYTES {
      return Err(invalid("addresses are at most 4 KiB"));
    }
    if Path::new(uri).is_absolute() {
      return Ok(());
    }
    let provided = crate::documents::scheme_of(uri).is_some_and(|scheme| {
      self
        .api
        .shared()
        .is_ok_and(|shared| shared.provides(scheme))
    });
    if provided {
      Ok(())
    } else {
      Err(invalid(format!(
        "{uri:?} is neither an absolute path nor an address an extension provides"
      )))
    }
  }

  /// Checks a layout. Webview and file page panes get their ids from
  /// `webview_id`, given the pane's id.
  fn layout_spec(
    &self,
    layout: Layout,
    mut webview_id: impl FnMut(&str) -> String,
  ) -> Result<LayoutSpec, ApiError> {
    if layout.nodes.is_empty() || layout.nodes.len() > MAX_NODES {
      return Err(invalid(format!("a layout has 1 to {MAX_NODES} nodes")));
    }
    let mut splits = BTreeMap::<String, usize>::new();
    let mut ids = BTreeSet::new();
    let mut nodes = Vec::with_capacity(layout.nodes.len());
    let mut panes = 0;
    for (index, node) in layout.nodes.into_iter().enumerate() {
      if node.id.is_empty()
        || node.id.chars().count() > MAX_NODE_ID_CHARS
        || node.id.contains(char::is_control)
      {
        return Err(invalid("a node's id must be 1 to 100 characters"));
      }
      if !ids.insert(node.id.clone()) {
        return Err(invalid(format!("two nodes have the id {:?}", node.id)));
      }
      match (&node.parent, index) {
        (None, 0) => {}
        (None, _) => {
          return Err(invalid(format!(
            "node {:?} needs a parent: only the first node has none",
            node.id
          )));
        }
        (Some(_), 0) => return Err(invalid("the first node fills the tab: it has no parent")),
        (Some(parent), _) => match splits.get_mut(parent) {
          Some(children) => *children += 1,
          None => {
            return Err(invalid(format!(
              "node {:?}'s parent {parent:?} isn't a split listed before it",
              node.id
            )));
          }
        },
      }
      let size = if node.size.is_finite() && node.size > 0.0 {
        node.size
      } else if node.size == 0.0 {
        1.0
      } else {
        return Err(invalid(format!(
          "node {:?}'s size must be 0 or more",
          node.id
        )));
      };
      let kind = match node.kind {
        NodeKind::Split(direction) => {
          splits.insert(node.id.clone(), 0);
          NodeSpec::Split {
            direction: match direction {
              Direction::Row => "row",
              Direction::Column => "column",
            },
          }
        }
        NodeKind::Pane(pane) => {
          panes += 1;
          let content = match pane.content {
            WitContent::Editor(editor) => {
              self.layout_document(&editor.uri)?;
              PaneContent::Editor {
                uri: editor.uri,
                minimap: editor.minimap,
                folding: editor.folding,
                quick_diff: editor.quick_diff,
              }
            }
            WitContent::Webview(page) => {
              if let Some(page) = &page {
                crate::contributions::check_page(page).map_err(invalid)?;
              }
              PaneContent::Webview {
                page,
                id: webview_id(&node.id),
              }
            }
            WitContent::WebPage(url) => {
              // The `permission` type can't name it (it's newer than 1.0).
              if !self.api.has(crate::Permission::WebPages) {
                return Err(invalid(
                  "web page panes need the web_pages permission, which the user hasn't granted",
                ));
              }
              if !is_web_address(&url) {
                return Err(invalid(format!(
                  "a web page pane shows an https: or http: address, not {url:?}"
                )));
              }
              PaneContent::WebPage {
                url,
                id: webview_id(&node.id),
              }
            }
            WitContent::FilePage(path) => {
              if !Path::new(&path).is_absolute() || path.len() > MAX_KEY_BYTES {
                return Err(invalid(format!(
                  "a file page shows a file named by its absolute path, not {path:?}"
                )));
              }
              PaneContent::FilePage {
                path,
                id: webview_id(&node.id),
              }
            }
          };
          let header = pane
            .header
            .map(|header| -> Result<Header, ApiError> {
              Ok(Header {
                title: title("a pane's title", &header.title, 200)?,
                description: optional(header.description, 500),
                buttons: self.buttons(header.buttons)?,
              })
            })
            .transpose()?;
          NodeSpec::Pane(PaneSpec {
            content,
            header,
            scroll_group: optional(pane.scroll_group, 100),
          })
        }
      };
      nodes.push(LayoutNode {
        id: node.id,
        parent: node.parent,
        size,
        kind,
      });
    }
    if panes > MAX_PANES {
      return Err(invalid(format!("a layout has at most {MAX_PANES} panes")));
    }
    if let Some((split, _)) = splits.iter().find(|(_, children)| **children == 0) {
      return Err(invalid(format!("the split {split:?} has nothing in it")));
    }
    if layout.documents.len() > MAX_DOCUMENTS {
      return Err(invalid(format!(
        "a layout keeps at most {MAX_DOCUMENTS} other documents open"
      )));
    }
    for uri in &layout.documents {
      self.layout_document(uri)?;
    }
    if layout.breakpoints.len() > MAX_BREAKPOINTS {
      return Err(invalid(format!(
        "a layout has at most {MAX_BREAKPOINTS} breakpoints"
      )));
    }
    let mut breakpoints = layout.breakpoints;
    breakpoints.sort_unstable();
    breakpoints.dedup();
    if let Some(main) = &layout.main
      && !nodes
        .iter()
        .any(|node| node.id == *main && matches!(node.kind, NodeSpec::Pane(_)))
    {
      return Err(invalid(format!("main {main:?} isn't one of the panes")));
    }
    Ok(LayoutSpec {
      title: title("a layout's title", &layout.title, 200)?,
      nodes,
      documents: layout.documents,
      buttons: self.buttons(layout.buttons)?,
      breakpoints,
      main: layout.main,
    })
  }

  fn decorated(
    &self,
    decorated: Vec<Decorated>,
    count: &mut usize,
  ) -> Result<Vec<DecoratedRange>, ApiError> {
    let mut ranges = Vec::new();
    for group in decorated {
      let key = self.decoration_key(&group.decoration)?;
      *count += group.ranges.len();
      if *count > MAX_PANE_RANGES {
        return Err(ApiError::TooLarge(format!(
          "at most {MAX_PANE_RANGES} decorated ranges in a pane"
        )));
      }
      for WitRange { start, end } in group.ranges {
        if (end.line, end.column) < (start.line, start.column) {
          return Err(ApiError::InvalidRange(format!(
            "a range ends ({}:{}) before it starts ({}:{})",
            end.line, end.column, start.line, start.column
          )));
        }
        ranges.push(DecoratedRange {
          key: key.clone(),
          start: point(start),
          end: point(end),
        });
      }
    }
    Ok(ranges)
  }

  fn pane_decorations(
    &self,
    decorations: WitDecorations,
    spec: &LayoutSpec,
  ) -> Result<PaneDecorations, ApiError> {
    if decorations.zones.len() > MAX_ZONES {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_ZONES} zones in a pane"
      )));
    }
    if decorations.folds.len() > MAX_FOLDS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_FOLDS} folds in a pane"
      )));
    }
    if decorations.ruler.len() > MAX_RULER_MARKS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_RULER_MARKS} ruler marks in a pane"
      )));
    }
    let mut count = 0;
    let ranges = self.decorated(decorations.decorations, &mut count)?;
    let documents = spec.all_documents();
    let mut zones = Vec::with_capacity(decorations.zones.len());
    for zone in decorations.zones {
      let content = match zone.content {
        WitZoneContent::Filler(rows) => {
          if rows == 0 || rows > MAX_ZONE_LINES {
            return Err(invalid(format!(
              "a filler zone has 1 to {MAX_ZONE_LINES} rows"
            )));
          }
          ZoneContent::Filler { rows }
        }
        WitZoneContent::Text(text) => {
          self.api.own_command(text.command.as_ref())?;
          ZoneContent::Text {
            text: title("a zone's text", &text.text, 200)?,
            tooltip: optional(text.tooltip, 2_000),
            command: text.command,
            argument: argument(text.argument)?,
          }
        }
        WitZoneContent::Links(links) => {
          if links.links.len() > MAX_ZONE_LINKS {
            return Err(invalid(format!(
              "a zone has at most {MAX_ZONE_LINKS} links"
            )));
          }
          ZoneContent::Links {
            label: optional(links.label, 100),
            links: links
              .links
              .into_iter()
              .map(|link| -> Result<ZoneLink, ApiError> {
                self.api.own_command(Some(&link.command))?;
                Ok(ZoneLink {
                  title: title("a link's title", &link.title, 100)?,
                  tooltip: optional(link.tooltip, 2_000),
                  command: link.command,
                  argument: argument(link.argument)?,
                })
              })
              .collect::<Result<_, _>>()?,
          }
        }
        WitZoneContent::Lines(lines) => {
          if !documents.contains(lines.uri.as_str()) {
            return Err(invalid(format!(
              "{:?} isn't one of the tab's documents (add it to the layout's documents)",
              lines.uri
            )));
          }
          let range = line_range(lines.lines)?;
          if range.end == range.start || range.end - range.start > MAX_ZONE_LINES {
            return Err(invalid(format!("a zone shows 1 to {MAX_ZONE_LINES} lines")));
          }
          ZoneContent::Lines {
            uri: lines.uri,
            lines: range,
            decorations: self.decorated(lines.decorations, &mut count)?,
            numbers: lines.numbers,
          }
        }
      };
      zones.push(Zone {
        line: zone.line,
        content,
        decoration: zone
          .decoration
          .map(|name| self.decoration_key(&name))
          .transpose()?,
      });
    }
    let folds = decorations
      .folds
      .into_iter()
      .map(line_range)
      .collect::<Result<Vec<_>, _>>()?;
    let ruler = decorations
      .ruler
      .into_iter()
      .map(|mark| -> Result<RulerMark, ApiError> {
        Ok(RulerMark {
          lines: line_range(mark.lines)?,
          decoration: self.decoration_key(&mark.decoration)?,
        })
      })
      .collect::<Result<_, _>>()?;
    Ok(PaneDecorations {
      decorations: ranges,
      zones,
      folds,
      ruler,
    })
  }

  /// The tab a call is about, as the extension names it.
  fn wit_tab(&self, tab: &TabId) -> WitTab {
    WitTab {
      layout: tab
        .layout
        .strip_prefix(&format!("{}.", self.api.extension_id()))
        .unwrap_or(&tab.layout)
        .to_owned(),
      key: tab.key.clone(),
    }
  }
}

impl Host for StoreState {
  fn filling(&mut self) -> Option<FillRequest> {
    match &self.api.payload {
      Payload::Layout(LayoutCall::Fill { tab, reason, width }) => Some(FillRequest {
        tab: self.wit_tab(tab),
        reason: reason.wit(),
        width: *width,
      }),
      _ => None,
    }
  }

  fn open(&mut self, tab: WitTab, beside: bool) -> Result<(), ApiError> {
    let id = self.tab_id(&tab)?;
    let shared = self.api.shared()?;
    shared.open_layout_tab(self.api.extension_id(), id, beside);
    Ok(())
  }

  fn set_layout(&mut self, tab: WitTab, layout: Layout) -> Result<bool, ApiError> {
    let id = self.tab_id(&tab)?;
    let shared = self.api.shared()?;
    let Some(mut webviews) = shared.layout_webviews(&id) else {
      return Ok(false);
    };
    let spec = self.layout_spec(layout, |pane| {
      webviews
        .remove(pane)
        .unwrap_or_else(|| shared.new_pane_webview_id(self.api.extension_id()))
    })?;
    Ok(shared.set_layout(&id, spec))
  }

  fn close(&mut self, tab: WitTab) {
    if let (Ok(id), Ok(shared)) = (self.tab_id(&tab), self.api.shared()) {
      shared.close_layout_tab(&id);
    }
  }

  fn tabs(&mut self) -> Vec<WitTab> {
    let Ok(shared) = self.api.shared() else {
      return Vec::new();
    };
    shared
      .layout_tabs_of(self.api.extension_id())
      .iter()
      .map(|tab| self.wit_tab(tab))
      .collect()
  }

  fn documents(&mut self, tab: WitTab) -> Result<Vec<WitDocument>, ApiError> {
    let id = self.tab_id(&tab)?;
    let shared = self.api.shared()?;
    let Some((spec, documents)) = shared.layout_documents(&id) else {
      return Err(invalid("the tab isn't open"));
    };
    Ok(
      documents
        .into_iter()
        .map(|document| WitDocument {
          panes: spec
            .as_ref()
            .map(|spec| {
              spec
                .panes()
                .filter(|(_, pane)| {
                  matches!(&pane.content, PaneContent::Editor { uri, .. } if *uri == document.uri)
                })
                .map(|(pane, _)| pane.to_owned())
                .collect()
            })
            .unwrap_or_default(),
          uri: document.uri,
          document: document.document,
        })
        .collect(),
    )
  }

  fn set_pane_decorations(
    &mut self,
    tab: WitTab,
    pane: String,
    decorations: WitDecorations,
  ) -> Result<bool, ApiError> {
    let id = self.tab_id(&tab)?;
    let shared = self.api.shared()?;
    let Some((Some(spec), _)) = shared.layout_documents(&id) else {
      return Ok(false);
    };
    if !matches!(
      spec.pane(&pane).map(|pane| &pane.content),
      Some(PaneContent::Editor { .. })
    ) {
      return Ok(false);
    }
    let mut decorations = self.pane_decorations(decorations, &spec)?;
    in_utf16(
      &shared,
      &spec,
      &documents_of(&shared, &id),
      &pane,
      &mut decorations,
    );
    Ok(shared.set_pane_decorations(&id, &pane, decorations))
  }

  fn reveal(&mut self, tab: WitTab, pane: String, range: WitRange) -> Result<bool, ApiError> {
    let id = self.tab_id(&tab)?;
    let shared = self.api.shared()?;
    let (mut start, mut end) = (point(range.start), point(range.end));
    if let Some((Some(spec), documents)) = shared.layout_documents(&id)
      && let Some(PaneContent::Editor { uri, .. }) = spec.pane(&pane).map(|pane| &pane.content)
      && let Some(document) = documents.iter().find(|document| document.uri == *uri)
    {
      crate::api::with_editor(&shared, |editor| {
        if let Ok(document) =
          editor.document(flareite_core::DocumentId::from_raw(document.document))
        {
          start = utf16_point(document.buffer(), start);
          end = utf16_point(document.buffer(), end);
        }
      });
    }
    Ok(shared.reveal_in_pane(&id, &pane, start, end))
  }

  fn active_pane(&mut self) -> Option<PaneSelection> {
    let shared = self.api.shared().ok()?;
    let (tab, pane, document, selections) = shared.active_pane()?;
    let prefix = format!("{}.", self.api.extension_id());
    if !tab.layout.starts_with(&prefix) {
      return None;
    }
    Some(PaneSelection {
      tab: self.wit_tab(&tab),
      pane,
      document,
      selections,
    })
  }

  fn invoked(&mut self) -> Option<InvokedFrom> {
    match &self.api.payload {
      Payload::Layout(LayoutCall::Invoked { tab, pane }) => Some(InvokedFrom {
        tab: self.wit_tab(tab),
        pane: pane.clone(),
      }),
      _ => None,
    }
  }

  fn set_pane_html(&mut self, tab: WitTab, pane: String, html: String) -> Result<bool, ApiError> {
    if html.len() > crate::webview::MAX_HTML_BYTES {
      return Err(ApiError::TooLarge(format!(
        "a page is at most {} MiB",
        crate::webview::MAX_HTML_BYTES / 1024 / 1024
      )));
    }
    let id = self.tab_id(&tab)?;
    let shared = self.api.shared()?;
    Ok(
      shared
        .pane_webview(&id, &pane)
        .is_some_and(|webview| shared.set_webview_html(&webview, html)),
    )
  }

  fn post_message(&mut self, tab: WitTab, pane: String, message: String) -> Result<bool, ApiError> {
    if message.len() > crate::webview::MAX_MESSAGE_BYTES {
      return Err(ApiError::TooLarge("a message is at most 1 MiB".into()));
    }
    serde_json::from_str::<serde::de::IgnoredAny>(&message)
      .map_err(|error| invalid(format!("the message isn't JSON: {error}")))?;
    let id = self.tab_id(&tab)?;
    let shared = self.api.shared()?;
    Ok(
      shared
        .pane_webview(&id, &pane)
        .is_some_and(|webview| shared.post_webview_message(&webview, message)),
    )
  }

  fn message(&mut self) -> Option<PaneMessage> {
    match &self.api.payload {
      Payload::Layout(LayoutCall::Message { tab, pane, data }) => Some(PaneMessage {
        tab: self.wit_tab(tab),
        pane: pane.clone(),
        data: data.clone(),
      }),
      _ => None,
    }
  }

  fn navigated(&mut self) -> Option<Navigation> {
    match &self.api.payload {
      Payload::Layout(LayoutCall::Navigated {
        tab,
        pane,
        url,
        title,
        loading,
      }) => Some(Navigation {
        tab: self.wit_tab(tab),
        pane: pane.clone(),
        url: url.clone(),
        title: title.clone(),
        loading: *loading,
      }),
      _ => None,
    }
  }

  fn navigate_pane(
    &mut self,
    tab: WitTab,
    pane: String,
    action: WitWebAction,
  ) -> Result<bool, ApiError> {
    let id = self.tab_id(&tab)?;
    let action = match action {
      WitWebAction::Go(url) => {
        if !is_web_address(&url) {
          return Err(invalid(format!(
            "a web page pane goes to an https: or http: address, not {url:?}"
          )));
        }
        WebAction::Go(url)
      }
      WitWebAction::Back => WebAction::Back,
      WitWebAction::Forward => WebAction::Forward,
      WitWebAction::Reload => WebAction::Reload,
      WitWebAction::Stop => WebAction::Stop,
    };
    let shared = self.api.shared()?;
    Ok(shared.navigate_web_page(&id, &pane, action))
  }
}

/// The documents of an open tab, by address.
fn documents_of(shared: &crate::host::Shared, tab: &TabId) -> BTreeMap<String, u64> {
  shared
    .layout_documents(tab)
    .map(|(_, documents)| {
      documents
        .into_iter()
        .map(|document| (document.uri, document.document))
        .collect()
    })
    .unwrap_or_default()
}

/// `point`, a line and a column in UTF-8 bytes as the API counts them, as
/// the workbench counts columns (UTF-16 code units). A point past its line
/// or the text, or inside a character, moves back to where it can be.
fn utf16_point(buffer: &flareite_text::TextBuffer, point: Point) -> Point {
  let lines = buffer.line_count();
  if lines == 0 {
    return Point { line: 0, column: 0 };
  }
  let line = (point.line as usize).min(lines - 1);
  let start = buffer.line_start(line);
  let mut offset = (start + point.column as usize).min(buffer.line_end(line));
  while offset > start && !buffer.is_char_boundary(offset) {
    offset -= 1;
  }
  let converted = buffer.offset_to_utf16(offset);
  Point {
    line: u32::try_from(converted.line).unwrap_or(u32::MAX),
    column: u32::try_from(converted.column).unwrap_or(u32::MAX),
  }
}

/// Converts what an editor pane shows from the API's columns to the
/// workbench's: its own ranges in its document, a zone's in the document
/// whose lines it shows. Ranges in documents that aren't open yet stay.
fn in_utf16(
  shared: &crate::host::Shared,
  spec: &LayoutSpec,
  documents: &BTreeMap<String, u64>,
  pane: &str,
  decorations: &mut PaneDecorations,
) {
  let own = match spec.pane(pane).map(|pane| &pane.content) {
    Some(PaneContent::Editor { uri, .. }) => documents.get(uri).copied(),
    _ => None,
  };
  crate::api::with_editor(shared, |editor| {
    let convert = |document: Option<u64>, ranges: &mut [DecoratedRange]| {
      let Some(document) = document.and_then(|id| {
        editor
          .document(flareite_core::DocumentId::from_raw(id))
          .ok()
      }) else {
        return;
      };
      let buffer = document.buffer();
      for range in ranges {
        range.start = utf16_point(buffer, range.start);
        range.end = utf16_point(buffer, range.end);
      }
    };
    convert(own, &mut decorations.decorations);
    for zone in &mut decorations.zones {
      if let ZoneContent::Lines {
        uri, decorations, ..
      } = &mut zone.content
      {
        convert(documents.get(uri.as_str()).copied(), decorations);
      }
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn zones_take_rows() {
    assert_eq!(ZoneContent::Filler { rows: 3 }.rows(), 3);
    let lines = ZoneContent::Lines {
      uri: "git:HEAD:a".into(),
      lines: LineRange { start: 4, end: 9 },
      decorations: Vec::new(),
      numbers: true,
    };
    assert_eq!(lines.rows(), 5);
  }

  #[test]
  fn reasons_are_parsed() {
    assert_eq!(Fill::parse("ready"), Some(Fill::Ready));
    assert_eq!(Fill::parse("bogus"), None);
  }
}
