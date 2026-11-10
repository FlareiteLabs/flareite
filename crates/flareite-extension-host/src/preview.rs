//! Previews (API 1.2): tabs of formatted text that extensions fill in, such
//! as a rendered Markdown file.
//!
//! An extension sends a preview's content as a list of nodes, each naming
//! the node it's inside. This module checks that tree (what may contain
//! what, depth, sizes) and turns it into what the workbench draws, resolving
//! links and pictures against the previewed document's folder. None of it is
//! markup: the frontend builds its own elements from these values, so an
//! extension can't inject HTML, scripts, or styles.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;

use crate::bindings::flareite::extension::types::{
  ApiError, ColumnAlignment as WitAlignment, PreviewElement as WitElement, PreviewImage,
  PreviewLink, PreviewNode as WitNode,
};

/// Most nodes in a preview.
pub const MAX_PREVIEW_NODES: usize = 100_000;
/// Most text in a preview, in bytes, counting every string in its nodes.
pub const MAX_PREVIEW_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// Most levels of nesting.
const MAX_DEPTH: usize = 64;
const MAX_COLUMNS: usize = 64;
const MAX_LANGUAGE_CHARS: usize = 64;
pub const MAX_TITLE_CHARS: usize = 100;
pub const MAX_KEY_BYTES: usize = 200;
const MAX_ADDRESS_BYTES: usize = 4_000;
/// Largest picture file a preview or viewer shows. Pictures reach the
/// webview as `data:` URLs.
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Pictures a preview can show, by file extension.
const IMAGE_TYPES: &[(&str, &str)] = &[
  ("png", "image/png"),
  ("apng", "image/png"),
  ("jpg", "image/jpeg"),
  ("jpeg", "image/jpeg"),
  ("jpe", "image/jpeg"),
  ("jfif", "image/jpeg"),
  ("gif", "image/gif"),
  ("webp", "image/webp"),
  ("svg", "image/svg+xml"),
  ("bmp", "image/bmp"),
  ("ico", "image/x-icon"),
  ("avif", "image/avif"),
];

/// An open preview, for the workbench's tabs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewInfo {
  /// `<extension id>.<preview id>/<key>`.
  pub id: String,
  /// The extension's name.
  pub extension: String,
  pub title: String,
  /// One of the side bar view icons.
  pub icon: String,
  /// The document it shows.
  pub document: Option<u64>,
  /// Whether it shows a webview page instead of content (API 1.5).
  pub webview: bool,
}

/// What a preview shows: a tree of nodes, each after its parent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewContent {
  pub nodes: Vec<PreviewNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewNode {
  /// The index of the node this one is inside; `None` at the top level.
  pub parent: Option<u32>,
  #[serde(flatten)]
  pub element: PreviewElement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ColumnAlignment {
  None,
  Left,
  Center,
  Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PreviewElement {
  Paragraph,
  Heading {
    level: u8,
  },
  Quote,
  BulletedList,
  NumberedList {
    start: u32,
  },
  ListItem {
    checked: Option<bool>,
  },
  CodeBlock {
    /// As the extension wrote it, e.g. `rust` or `ts`.
    language: Option<String>,
    text: String,
  },
  Rule,
  Table {
    alignments: Vec<ColumnAlignment>,
  },
  TableHead,
  TableRow,
  TableCell,
  Text {
    text: String,
  },
  Emphasis,
  Strong,
  Strikethrough,
  Code {
    text: String,
  },
  Link {
    target: LinkTarget,
    title: Option<String>,
  },
  Image {
    source: ImageSource,
    description: String,
    title: Option<String>,
  },
  LineBreak,
}

/// Where a link goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LinkTarget {
  /// An `http:`, `https:`, or `mailto:` address, opened by the system.
  Web { url: String },
  /// A heading in the same preview, by its anchor (without the `#`).
  Heading { anchor: String },
  /// A file, and optionally a heading in it.
  File {
    path: String,
    anchor: Option<String>,
  },
  /// Not followed: other schemes, absolute paths, and relative paths in a
  /// preview without a saved document.
  Unsupported { destination: String },
}

/// Where a picture comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ImageSource {
  /// A picture file, loaded through `ExtensionHost::preview_image`.
  File {
    path: String,
  },
  /// A web address: not loaded, so opening a preview sends no requests.
  Web {
    url: String,
  },
  Unsupported {
    source: String,
  },
}

/// What a node may contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holds {
  /// Blocks and inline nodes: the top level, quotes, list items.
  Flow,
  /// Inline nodes only.
  Phrasing,
  /// List items only.
  Items,
  /// A table's header row and rows.
  Rows,
  /// Table cells.
  Cells,
  /// Nothing.
  Nothing,
}

fn holds(element: &PreviewElement) -> Holds {
  use PreviewElement as E;
  match element {
    E::Quote | E::ListItem { .. } => Holds::Flow,
    E::Paragraph
    | E::Heading { .. }
    | E::TableCell
    | E::Emphasis
    | E::Strong
    | E::Strikethrough
    | E::Link { .. } => Holds::Phrasing,
    E::BulletedList | E::NumberedList { .. } => Holds::Items,
    E::Table { .. } => Holds::Rows,
    E::TableHead | E::TableRow => Holds::Cells,
    E::CodeBlock { .. }
    | E::Rule
    | E::Text { .. }
    | E::Code { .. }
    | E::Image { .. }
    | E::LineBreak => Holds::Nothing,
  }
}

fn is_block(element: &PreviewElement) -> bool {
  use PreviewElement as E;
  matches!(
    element,
    E::Paragraph
      | E::Heading { .. }
      | E::Quote
      | E::BulletedList
      | E::NumberedList { .. }
      | E::CodeBlock { .. }
      | E::Rule
      | E::Table { .. }
  )
}

fn is_inline(element: &PreviewElement) -> bool {
  use PreviewElement as E;
  matches!(
    element,
    E::Text { .. }
      | E::Emphasis
      | E::Strong
      | E::Strikethrough
      | E::Code { .. }
      | E::Link { .. }
      | E::Image { .. }
      | E::LineBreak
  )
}

fn allowed(container: Holds, child: &PreviewElement) -> bool {
  use PreviewElement as E;
  match container {
    Holds::Flow => is_block(child) || is_inline(child),
    Holds::Phrasing => is_inline(child),
    Holds::Items => matches!(child, E::ListItem { .. }),
    Holds::Rows => matches!(child, E::TableHead | E::TableRow),
    Holds::Cells => matches!(child, E::TableCell),
    Holds::Nothing => false,
  }
}

fn name(element: &PreviewElement) -> &'static str {
  use PreviewElement as E;
  match element {
    E::Paragraph => "paragraph",
    E::Heading { .. } => "heading",
    E::Quote => "quote",
    E::BulletedList => "bulleted-list",
    E::NumberedList { .. } => "numbered-list",
    E::ListItem { .. } => "list-item",
    E::CodeBlock { .. } => "code-block",
    E::Rule => "rule",
    E::Table { .. } => "table",
    E::TableHead => "table-head",
    E::TableRow => "table-row",
    E::TableCell => "table-cell",
    E::Text { .. } => "text",
    E::Emphasis => "emphasis",
    E::Strong => "strong",
    E::Strikethrough => "strikethrough",
    E::Code { .. } => "code",
    E::Link { .. } => "link",
    E::Image { .. } => "image",
    E::LineBreak => "line-break",
  }
}

/// Checked content, and the picture files it shows.
pub struct Converted {
  pub content: PreviewContent,
  pub images: BTreeSet<PathBuf>,
}

/// Checks an extension's nodes and converts them. Relative links and
/// pictures are resolved from `base`, the previewed document's folder.
/// Markdown as preview content: rendered as `workbench::render-markdown`
/// renders it, then checked and resolved as an extension's preview is, with
/// links and pictures found from `base`. Extensions' READMEs and the
/// editor's hover cards are shown this way.
pub fn from_markdown(markdown: &str, base: Option<&Path>) -> Result<PreviewContent, String> {
  convert(crate::markdown::render(markdown), base)
    .map(|converted| converted.content)
    .map_err(|error| format!("{error:?}"))
}

pub fn convert(nodes: Vec<WitNode>, base: Option<&Path>) -> Result<Converted, ApiError> {
  if nodes.len() > MAX_PREVIEW_NODES {
    return Err(ApiError::TooLarge(format!(
      "a preview can have at most {MAX_PREVIEW_NODES} nodes"
    )));
  }
  let mut text_bytes = 0usize;
  let mut converted: Vec<PreviewNode> = Vec::with_capacity(nodes.len());
  let mut depths: Vec<u8> = Vec::with_capacity(nodes.len());
  let mut images = BTreeSet::new();
  for (index, node) in nodes.into_iter().enumerate() {
    let fail =
      |message: String| ApiError::InvalidArgument(format!("preview node {index}: {message}"));
    text_bytes += text_size(&node.element);
    if text_bytes > MAX_PREVIEW_TEXT_BYTES {
      return Err(ApiError::TooLarge(format!(
        "a preview can have at most {} MiB of text",
        MAX_PREVIEW_TEXT_BYTES / 1024 / 1024
      )));
    }
    let element = element(node.element, base, &mut images).map_err(fail)?;
    let (container, depth) = match node.parent {
      None => (Holds::Flow, 1),
      Some(parent) => {
        let Some(parent_node) = converted.get(parent as usize) else {
          return Err(fail(format!("its parent {parent} must come before it")));
        };
        (
          holds(&parent_node.element),
          depths[parent as usize] as usize + 1,
        )
      }
    };
    if !allowed(container, &element) {
      let place = node.parent.map_or("the top level", |parent| {
        name(&converted[parent as usize].element)
      });
      return Err(fail(format!("a {} can't be in {place}", name(&element))));
    }
    if depth > MAX_DEPTH {
      return Err(fail(format!(
        "nodes can be nested at most {MAX_DEPTH} deep"
      )));
    }
    depths.push(depth as u8);
    converted.push(PreviewNode {
      parent: node.parent,
      element,
    });
  }
  Ok(Converted {
    content: PreviewContent { nodes: converted },
    images,
  })
}

fn text_size(element: &WitElement) -> usize {
  let option = |text: &Option<String>| text.as_ref().map_or(0, String::len);
  match element {
    WitElement::CodeBlock(code) => code.text.len() + option(&code.language),
    WitElement::Text(text) | WitElement::Code(text) => text.len(),
    WitElement::Link(link) => link.destination.len() + option(&link.title),
    WitElement::Image(image) => image.source.len() + image.description.len() + option(&image.title),
    _ => 0,
  }
}

fn element(
  element: WitElement,
  base: Option<&Path>,
  images: &mut BTreeSet<PathBuf>,
) -> Result<PreviewElement, String> {
  use PreviewElement as E;
  Ok(match element {
    WitElement::Paragraph => E::Paragraph,
    WitElement::Heading(level) => {
      if !(1..=6).contains(&level) {
        return Err(format!("heading level {level} must be 1 to 6"));
      }
      E::Heading { level }
    }
    WitElement::Quote => E::Quote,
    WitElement::BulletedList => E::BulletedList,
    WitElement::NumberedList(start) => E::NumberedList { start },
    WitElement::ListItem(checked) => E::ListItem { checked },
    WitElement::CodeBlock(code) => E::CodeBlock {
      language: code
        .language
        .map(|language| language.trim().to_owned())
        .filter(|language| {
          !language.is_empty()
            && language.chars().count() <= MAX_LANGUAGE_CHARS
            && !language.contains(char::is_control)
        }),
      text: code.text,
    },
    WitElement::Rule => E::Rule,
    WitElement::Table(alignments) => {
      if alignments.len() > MAX_COLUMNS {
        return Err(format!("a table can have at most {MAX_COLUMNS} columns"));
      }
      E::Table {
        alignments: alignments
          .into_iter()
          .map(|alignment| match alignment {
            WitAlignment::None => ColumnAlignment::None,
            WitAlignment::Left => ColumnAlignment::Left,
            WitAlignment::Center => ColumnAlignment::Center,
            WitAlignment::Right => ColumnAlignment::Right,
          })
          .collect(),
      }
    }
    WitElement::TableHead => E::TableHead,
    WitElement::TableRow => E::TableRow,
    WitElement::TableCell => E::TableCell,
    WitElement::Text(text) => E::Text { text },
    WitElement::Emphasis => E::Emphasis,
    WitElement::Strong => E::Strong,
    WitElement::Strikethrough => E::Strikethrough,
    WitElement::Code(text) => E::Code { text },
    WitElement::Link(PreviewLink { destination, title }) => E::Link {
      target: link_target(&destination, base),
      title,
    },
    WitElement::Image(PreviewImage {
      source,
      description,
      title,
    }) => {
      let source = image_source(&source, base);
      if let ImageSource::File { path } = &source {
        images.insert(PathBuf::from(path));
      }
      E::Image {
        source,
        description,
        title,
      }
    }
    WitElement::LineBreak => E::LineBreak,
  })
}

/// The lowercase scheme of an address such as `https://…`. A single letter
/// before the colon is a Windows drive, not a scheme.
fn scheme(address: &str) -> Option<String> {
  let end = address.find(':')?;
  let scheme = &address[..end];
  let valid = scheme.len() > 1
    && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
    && scheme
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
  valid.then(|| scheme.to_ascii_lowercase())
}

/// Whether an address can be handed to the system as is.
fn clean_address(address: &str) -> bool {
  !address.is_empty()
    && address.len() <= MAX_ADDRESS_BYTES
    && !address.contains(|c: char| c.is_whitespace() || c.is_control())
}

/// Decodes `%XX` escapes, as in `my%20notes.md`. Malformed escapes and
/// non-UTF-8 results leave the text as it was.
fn percent_decode(text: &str) -> String {
  if !text.contains('%') {
    return text.to_owned();
  }
  let bytes = text.as_bytes();
  let mut decoded = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%'
      && let Some(hex) = text.get(i + 1..i + 3)
      && let Ok(byte) = u8::from_str_radix(hex, 16)
    {
      decoded.push(byte);
      i += 3;
    } else {
      decoded.push(bytes[i]);
      i += 1;
    }
  }
  String::from_utf8(decoded).unwrap_or_else(|_| text.to_owned())
}

/// A path relative to `base`, without `.` and `..`. `None` for absolute
/// paths, which previews don't follow.
fn resolve(base: &Path, relative: &str) -> Option<PathBuf> {
  let relative = Path::new(relative);
  if relative.has_root() || relative.is_absolute() {
    return None;
  }
  let mut resolved = base.to_path_buf();
  for component in relative.components() {
    match component {
      Component::Normal(part) => resolved.push(part),
      Component::ParentDir => {
        resolved.pop();
      }
      Component::CurDir => {}
      Component::Prefix(_) | Component::RootDir => return None,
    }
  }
  Some(resolved)
}

/// Splits `path#anchor`, dropping any `?query`.
fn split_anchor(destination: &str) -> (&str, Option<&str>) {
  let (path, anchor) = match destination.split_once('#') {
    Some((path, anchor)) => (path, Some(anchor)),
    None => (destination, None),
  };
  let path = path.split_once('?').map_or(path, |(path, _)| path);
  (path, anchor.filter(|anchor| !anchor.is_empty()))
}

pub fn link_target(destination: &str, base: Option<&Path>) -> LinkTarget {
  let destination = destination.trim();
  let unsupported = || LinkTarget::Unsupported {
    destination: destination.to_owned(),
  };
  if let Some(anchor) = destination.strip_prefix('#') {
    return LinkTarget::Heading {
      anchor: percent_decode(anchor),
    };
  }
  match scheme(destination).as_deref() {
    Some("http" | "https" | "mailto") if clean_address(destination) => LinkTarget::Web {
      url: destination.to_owned(),
    },
    Some(_) => unsupported(),
    None => {
      let (path, anchor) = split_anchor(destination);
      if path.is_empty() {
        return match anchor {
          Some(anchor) => LinkTarget::Heading {
            anchor: percent_decode(anchor),
          },
          None => unsupported(),
        };
      }
      match base.and_then(|base| resolve(base, &percent_decode(path))) {
        Some(path) => LinkTarget::File {
          path: path.to_string_lossy().into_owned(),
          anchor: anchor.map(percent_decode),
        },
        None => unsupported(),
      }
    }
  }
}

pub fn image_source(source: &str, base: Option<&Path>) -> ImageSource {
  let source = source.trim();
  let unsupported = || ImageSource::Unsupported {
    source: source.to_owned(),
  };
  match scheme(source).as_deref() {
    Some("http" | "https") if clean_address(source) => ImageSource::Web {
      url: source.to_owned(),
    },
    Some(_) => unsupported(),
    None => {
      let (path, _) = split_anchor(source);
      let Some(path) = base.and_then(|base| resolve(base, &percent_decode(path))) else {
        return unsupported();
      };
      if image_type(&path).is_none() {
        return unsupported();
      }
      ImageSource::File {
        path: path.to_string_lossy().into_owned(),
      }
    }
  }
}

/// Reads a picture file as a `data:` URL. SVG is shown as an image, so its
/// scripts don't run.
pub fn load_image(path: &Path) -> Result<String, String> {
  let media = image_type(path)
    .ok_or("previews show only PNG, JPEG, GIF, WebP, SVG, BMP, ICO, and AVIF pictures")?;
  let size = std::fs::metadata(path)
    .map_err(|error| error.to_string())?
    .len();
  if size > MAX_IMAGE_BYTES {
    return Err(format!(
      "the picture is larger than {} MiB",
      MAX_IMAGE_BYTES / 1024 / 1024
    ));
  }
  let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
  Ok(format!("data:{media};base64,{}", STANDARD.encode(bytes)))
}

/// The media type of a picture file a preview can show, by its extension.
pub fn image_type(path: &Path) -> Option<&'static str> {
  let extension = path.extension()?.to_str()?.to_ascii_lowercase();
  IMAGE_TYPES
    .iter()
    .find(|(known, _)| *known == extension)
    .map(|(_, media)| *media)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::bindings::flareite::extension::types::PreviewCode;

  fn node(parent: Option<u32>, element: WitElement) -> WitNode {
    WitNode { parent, element }
  }

  fn text(parent: u32, text: &str) -> WitNode {
    node(Some(parent), WitElement::Text(text.into()))
  }

  /// `/` and `parts`, joined as the platform does.
  fn joined(parts: &[&str]) -> String {
    let path = parts
      .iter()
      .fold(PathBuf::from("/"), |path, part| path.join(part));
    path.to_string_lossy().into_owned()
  }

  fn error(nodes: Vec<WitNode>) -> String {
    match convert(nodes, None) {
      Err(ApiError::InvalidArgument(message) | ApiError::TooLarge(message)) => message,
      Err(other) => format!("{other:?}"),
      Ok(_) => "accepted".into(),
    }
  }

  #[test]
  fn converts_a_document() {
    let nodes = vec![
      node(None, WitElement::Heading(1)),
      text(0, "Title"),
      node(None, WitElement::BulletedList),
      node(Some(2), WitElement::ListItem(Some(true))),
      text(3, "done"),
      node(
        None,
        WitElement::Table(vec![WitAlignment::Left, WitAlignment::Right]),
      ),
      node(Some(5), WitElement::TableHead),
      node(Some(6), WitElement::TableCell),
      node(Some(7), WitElement::Strong),
      text(8, "a"),
      node(
        None,
        WitElement::CodeBlock(PreviewCode {
          language: Some(" rust ".into()),
          text: "fn main() {}".into(),
        }),
      ),
    ];
    let converted = convert(nodes, None).expect("valid").content;
    assert_eq!(converted.nodes.len(), 11);
    assert_eq!(
      converted.nodes[3].element,
      PreviewElement::ListItem {
        checked: Some(true)
      }
    );
    assert_eq!(
      converted.nodes[10].element,
      PreviewElement::CodeBlock {
        language: Some("rust".into()),
        text: "fn main() {}".into()
      }
    );
    let json = serde_json::to_value(&converted.nodes[0]).expect("serializes");
    assert_eq!(
      json,
      serde_json::json!({"parent": null, "type": "heading", "level": 1})
    );
  }

  #[test]
  fn rejects_nodes_out_of_place() {
    assert!(error(vec![text(0, "x")]).contains("must come before it"));
    let in_text = vec![
      node(None, WitElement::Paragraph),
      text(0, "a"),
      text(1, "b"),
    ];
    assert!(error(in_text).contains("a text can't be in text"));
    let block_in_inline = vec![
      node(None, WitElement::Paragraph),
      node(Some(0), WitElement::Quote),
    ];
    assert!(error(block_in_inline).contains("a quote can't be in paragraph"));
    let loose_item = vec![node(None, WitElement::ListItem(None))];
    assert!(error(loose_item).contains("list-item can't be in the top level"));
    let cell_in_table = vec![
      node(None, WitElement::Table(vec![])),
      node(Some(0), WitElement::TableCell),
    ];
    assert!(error(cell_in_table).contains("table-cell can't be in table"));
    assert!(error(vec![node(None, WitElement::Heading(7))]).contains("level 7"));
  }

  #[test]
  fn limits_depth_and_size() {
    let mut deep = vec![node(None, WitElement::Quote)];
    for parent in 0..MAX_DEPTH as u32 {
      deep.push(node(Some(parent), WitElement::Quote));
    }
    assert!(error(deep).contains("nested at most"));
    let many = (0..=MAX_PREVIEW_NODES)
      .map(|_| node(None, WitElement::Rule))
      .collect();
    assert!(error(many).contains("at most 100000 nodes"));
    let big = "x".repeat(MAX_PREVIEW_TEXT_BYTES / 2 + 1);
    let large = vec![
      node(None, WitElement::Text(big.clone())),
      node(None, WitElement::Text(big)),
    ];
    assert!(error(large).contains("MiB of text"));
  }

  #[test]
  fn resolves_links() {
    let base = &PathBuf::from(joined(&["notes", "docs"]));
    let file = |parts: &[&str], anchor: Option<&str>| LinkTarget::File {
      path: joined(parts),
      anchor: anchor.map(str::to_owned),
    };
    assert_eq!(
      link_target("https://example.com/a b", Some(base)),
      LinkTarget::Unsupported {
        destination: "https://example.com/a b".into()
      },
      "whitespace isn't handed to the system"
    );
    assert_eq!(
      link_target("HTTPS://example.com", Some(base)),
      LinkTarget::Web {
        url: "HTTPS://example.com".into()
      }
    );
    assert_eq!(
      link_target("#Getting%20Started", Some(base)),
      LinkTarget::Heading {
        anchor: "Getting Started".into()
      }
    );
    assert_eq!(
      link_target("setup.md#install", Some(base)),
      file(&["notes", "docs", "setup.md"], Some("install"))
    );
    assert_eq!(
      link_target("../My%20Notes.md?plain=1", Some(base)),
      file(&["notes", "My Notes.md"], None)
    );
    for unsupported in [
      "javascript:alert(1)",
      "file:///etc/passwd",
      "/abs.md",
      "data:x",
    ] {
      assert!(
        matches!(
          link_target(unsupported, Some(base)),
          LinkTarget::Unsupported { .. }
        ),
        "{unsupported}"
      );
    }
    assert!(matches!(
      link_target("relative.md", None),
      LinkTarget::Unsupported { .. }
    ));
  }

  #[test]
  fn resolves_pictures_and_records_their_files() {
    let base = Path::new("/notes");
    assert_eq!(
      image_source("img/Logo.PNG", Some(base)),
      ImageSource::File {
        path: joined(&["notes", "img", "Logo.PNG"])
      }
    );
    assert!(matches!(
      image_source("https://example.com/a.png", Some(base)),
      ImageSource::Web { .. }
    ));
    for unsupported in ["notes.txt", "data:image/png;base64,xx", "/abs.png"] {
      assert!(
        matches!(
          image_source(unsupported, Some(base)),
          ImageSource::Unsupported { .. }
        ),
        "{unsupported}"
      );
    }
    let nodes = vec![
      node(None, WitElement::Paragraph),
      node(
        Some(0),
        WitElement::Image(PreviewImage {
          source: "a.svg".into(),
          description: "A".into(),
          title: None,
        }),
      ),
    ];
    let converted = convert(nodes, Some(base)).expect("valid");
    assert_eq!(
      converted.images.into_iter().collect::<Vec<_>>(),
      [PathBuf::from("/notes/a.svg")]
    );
  }
}
