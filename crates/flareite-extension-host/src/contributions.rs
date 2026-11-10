//! Workbench contributions declared in a manifest: themes, file icon themes,
//! keybindings, menu items, status bar items, side bar views, decoration
//! types, previews, viewers, languages, and language servers.
//!
//! Contributions are data: the workbench renders them, and extensions only
//! fill in content (status text, view items, decorated ranges, preview
//! nodes) through the `workbench` interface. Every value is validated here, so nothing an
//! extension declares reaches the frontend as markup or arbitrary CSS:
//! colors must be hex colors and theme keys must be known tokens, and icons
//! are images (see [`crate::icons`]). The exception is webview pages, which
//! run sandboxed in frames of their own (see [`crate::webview`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::icons::{self, IconTheme};
use crate::keys;
use crate::manifest::{CommandContribution, ManifestError};

/// The semantic color tokens a theme may set (`--color-<token>`), shared
/// with the frontend, which checks them against `styles/tokens.css`.
pub static THEME_TOKENS: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
  serde_json::from_str(include_str!("../../../src/themes/tokens.json")).unwrap_or_default()
});
/// Icons a view can use in the activity bar, and a preview on its tab.
pub const VIEW_ICONS: &[&str] = &[
  "list",
  "check",
  "bolt",
  "book",
  "bug",
  "tag",
  "star",
  "flag",
  "bell",
  "source-control",
  "history",
];
/// Named icons for tab actions and webview pages, as SVG path data, shared
/// with the frontend (`scripts/action-icons.mjs` generates them).
pub static ACTION_ICONS: LazyLock<BTreeMap<String, String>> = LazyLock::new(|| {
  serde_json::from_str(include_str!("../../../src/components/actionIcons.json")).unwrap_or_default()
});
/// Most actions on one viewer's or preview's tabs.
const MAX_ACTIONS: usize = 10;
/// Largest icon file an action can use.
const MAX_ACTION_ICON_BYTES: u64 = 32 * 1024;
const MAX_THEME_FILE_BYTES: u64 = 256 * 1024;

fn invalid(message: impl Into<String>) -> ManifestError {
  ManifestError::Invalid(message.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
  Dark,
  Light,
}

/// A color theme: a base (dark or light) and colors that override its tokens.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeContribution {
  /// `<extension id>.<theme id>`, as written in `workbench.theme`.
  pub id: String,
  pub label: String,
  pub kind: ThemeKind,
  /// Token (without `--color-`) to hex color.
  pub colors: BTreeMap<String, String>,
  #[serde(skip)]
  pub path: PathBuf,
}

/// A file icon theme. Its icons are loaded with the manifest but only sent
/// to the frontend on request (see `ExtensionHost::icon_theme`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IconThemeContribution {
  /// `<extension id>.<icon theme id>`, as written in `workbench.icon_theme`.
  pub id: String,
  pub label: String,
  #[serde(skip)]
  pub path: PathBuf,
  #[serde(skip)]
  pub theme: Arc<IconTheme>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingContribution {
  pub command: String,
  pub key: String,
  /// A different binding on macOS.
  pub mac: Option<String>,
  /// Only while the editor has focus.
  pub editor_focus: bool,
  /// Only while a tab of one of these layouts (`<extension id>.<layout
  /// id>`) is active; empty: always. (1.15)
  pub layouts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MenuLocation {
  /// The editor's context menu.
  #[serde(rename = "editor/context")]
  EditorContext,
  /// The explorer's context menu for a file or folder; the command gets its
  /// path as the argument.
  #[serde(rename = "explorer/context")]
  ExplorerContext,
  /// A view's header: a button (`inline`) or an entry in its "…" menu.
  /// (1.7)
  #[serde(rename = "view/title")]
  ViewTitle,
  /// A view's items: a button on the row (`inline`) or an entry in its
  /// context menu, for items whose context is in `contexts`. The command
  /// gets the item's argument, and `views::selection`. (1.7)
  #[serde(rename = "view/item")]
  ViewItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuContribution {
  pub location: MenuLocation,
  pub command: String,
  /// Items are grouped by this, then sorted by title.
  pub group: String,
  /// In the explorer: shown for files.
  pub for_files: bool,
  /// In the explorer: shown for folders.
  pub for_folders: bool,
  /// File name patterns (`*.md`, `*.{ts,tsx}`, `Cargo.toml`), compared
  /// ignoring case: the item is shown only for files whose names match one
  /// (the active document's, in the editor). Empty: every file.
  pub patterns: Vec<String>,
  /// For `view/title` and `view/item`: the view, `<extension id>.<view id>`.
  pub view: Option<String>,
  /// For `view/item`: the item contexts it's for; empty means every item.
  pub contexts: Vec<String>,
  /// A button rather than a menu entry; it needs an icon.
  pub inline: bool,
  /// One of [`ACTION_ICONS`].
  pub icon: Option<String>,
  /// Shown only while the view has this flag set (`set-flags`), or with
  /// `!`, while it doesn't.
  pub when: Option<String>,
}

/// Most patterns in a menu item, and the longest pattern.
const MAX_MENU_PATTERNS: usize = 20;
const MAX_PATTERN_CHARS: usize = 100;

/// Checks a file name pattern: `*` (any characters), `?` (one character),
/// and `{a,b}` (either), matching a file's name, not its path.
fn check_pattern(pattern: &str) -> Result<(), String> {
  if pattern.is_empty() || pattern.chars().count() > MAX_PATTERN_CHARS {
    return Err(format!("must be 1 to {MAX_PATTERN_CHARS} characters"));
  }
  if pattern.contains(['/', '\\']) {
    return Err("matches a file's name, so it can't contain / or \\".into());
  }
  if pattern.contains(['[', ']']) || pattern.chars().any(char::is_control) {
    return Err("can only use *, ?, and {a,b}".into());
  }
  let mut open = false;
  for c in pattern.chars() {
    match c {
      '{' if open => return Err("can't nest { }".into()),
      '{' => open = true,
      '}' if !open => return Err("has a } without a {".into()),
      '}' => open = false,
      _ => {}
    }
  }
  if open {
    return Err("has a { without a }".into());
  }
  Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Alignment {
  Left,
  Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusItemContribution {
  pub id: String,
  pub alignment: Alignment,
  /// Higher priorities are placed further left within their side.
  pub priority: i32,
  /// Shown until the extension sets other content.
  pub text: Option<String>,
  pub tooltip: Option<String>,
  pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewContribution {
  /// The id the extension uses.
  pub id: String,
  /// `<extension id>.<view id>`, unique in the workbench.
  pub full_id: String,
  pub title: String,
  pub icon: String,
  /// The container it's a section of, `<extension id>.<container id>`;
  /// `None` for a view with an activity bar entry of its own. (1.7)
  pub container: Option<String>,
  /// A text box above its items. (1.7)
  pub input: Option<ViewInput>,
  /// Run to load the children of an item opened without any. (1.7)
  pub on_expand: Option<String>,
  /// Hidden until the extension shows it (`views::set-visible`). (1.13)
  pub hidden: bool,
}

/// A view's text box, such as a commit message. (1.7)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewInput {
  pub placeholder: Option<String>,
  /// Run by Ctrl+Enter (Cmd+Enter on macOS).
  pub command: Option<String>,
}

/// Read-only documents the extension provides for addresses with its
/// scheme, such as `git:HEAD:src/main.rs`. (1.8)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextProviderContribution {
  pub scheme: String,
  /// Run to provide the text at an address (`documents::requested`).
  pub command: String,
}

/// Comparing files with their originals as they're edited. (1.9)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickDiffContribution {
  /// Run to find a file's original (`quick-diff::requested-file`).
  pub original: String,
  /// Run to apply changes to an original (`quick-diff::to-apply`).
  pub apply: Option<String>,
  /// What applying is called, such as "Stage".
  pub apply_title: Option<String>,
}

/// Schemes no extension may provide: the editor's own, and the web's.
const RESERVED_SCHEMES: &[&str] = &[
  "file",
  "untitled",
  "http",
  "https",
  "data",
  "blob",
  "flareite",
  "flareite-webview",
];

/// Whether `scheme` can name a text provider: 2 to 32 lowercase letters,
/// digits, `+`, `-`, and `.`, starting with a letter, and not reserved.
pub fn valid_scheme(scheme: &str) -> bool {
  (2..=32).contains(&scheme.len())
    && scheme.starts_with(|c: char| c.is_ascii_lowercase())
    && scheme
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
    && !RESERVED_SCHEMES.contains(&scheme)
}

/// An activity bar entry whose views are shown as sections. (1.7)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewContainerContribution {
  pub id: String,
  /// `<extension id>.<container id>`.
  pub full_id: String,
  pub title: String,
  pub icon: String,
}

/// A kind of preview: a tab of formatted text the extension fills in, such
/// as a rendered Markdown file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewContribution {
  /// The id the extension uses.
  pub id: String,
  /// `<extension id>.<preview id>`.
  pub full_id: String,
  /// Shown on its tabs.
  pub icon: String,
  /// Set when it shows a page instead of preview content. (1.5)
  pub webview: Option<WebviewContribution>,
  /// Buttons beside its tabs. (1.5)
  pub actions: Vec<TabAction>,
}

/// A button beside a viewer's or preview's tabs. (1.5)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabAction {
  /// Unique among the tab's actions; what the page's `flareite.onAction`
  /// gets.
  pub id: String,
  /// Its tooltip and accessible name.
  pub title: String,
  pub icon: ActionIcon,
  /// Run with the tab as its argument (a viewer's file, or a preview's
  /// key); without one, the click goes to the webview's page.
  pub command: Option<String>,
}

/// A tab action's icon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ActionIcon {
  /// One of [`ACTION_ICONS`], drawn in the theme's colors.
  Named { name: String },
  /// An SVG or PNG file of the extension's, shown as an image.
  Image {
    /// Relative to the extension's folder.
    #[serde(skip)]
    path: String,
    /// The file as a `data:` URL, once loaded ([`load_action_icons`]).
    url: String,
  },
}

/// The page a viewer or preview shows instead of preview content. (1.5)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewContribution {
  /// The HTML file it shows, relative to the extension's folder; `None`
  /// when the extension sets the page with `set-webview-html`.
  pub page: Option<String>,
  /// Run when the page sends a message.
  pub on_message: Option<String>,
  /// Keeps the page loaded while its tab is hidden, instead of loading it
  /// again (with its saved state) when the tab is shown.
  pub retain_when_hidden: bool,
}

/// When a viewer opens the files it's for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewerPriority {
  /// Opening a matching file opens it in the viewer.
  Default,
  /// Only when the user chooses the viewer (Reopen Editor With).
  Option,
}

/// A viewer: a tab that shows files whose names match its patterns the way
/// the extension draws them (as preview content), instead of as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerContribution {
  /// The id the extension uses.
  pub id: String,
  /// `<extension id>.<viewer id>`.
  pub full_id: String,
  /// Shown when choosing how to open a file, e.g. "Image Viewer".
  pub title: String,
  /// File name patterns, as for menu items.
  pub patterns: Vec<String>,
  /// Run with the file's path as its argument to fill the viewer in.
  /// Optional for a webview, whose page can load the file itself.
  pub command: Option<String>,
  pub priority: ViewerPriority,
  /// Set when it shows a page instead of preview content. (1.5)
  pub webview: Option<WebviewContribution>,
  /// Buttons beside its tabs. (1.5)
  pub actions: Vec<TabAction>,
}

/// What a link handler opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
  /// Web addresses.
  Web,
  /// Files and folders.
  File,
}

/// Links the extension opens instead of Flareite (API 1.15): web addresses
/// with some schemes, or files whose names match some patterns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkHandlerContribution {
  /// The id the extension uses.
  pub id: String,
  /// `<extension id>.<handler id>`.
  pub full_id: String,
  /// Shown when choosing how to open a link, e.g. "Open in a Web Tab".
  pub title: String,
  pub kind: LinkKind,
  /// For web links: the schemes it takes (`https`, `http`, `mailto`).
  pub schemes: Vec<String>,
  /// For files: name patterns; empty takes every file and folder.
  pub patterns: Vec<String>,
  /// Run with the link (`links::opening`).
  pub command: String,
}

/// A layout: tabs the extension splits into panes (editors, pages, files
/// shown as pages), filled in by its command. (1.15)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutContribution {
  /// The id the extension uses.
  pub id: String,
  /// `<extension id>.<layout id>`.
  pub full_id: String,
  /// Shown when choosing how to open a file, e.g. "HTML Preview".
  pub title: String,
  /// Run to fill a tab in; see `layouts::filling`.
  pub command: String,
  /// Run when a webview pane's page sends a message.
  pub on_message: Option<String>,
  /// Run when a web page pane goes somewhere, finishes loading, or changes
  /// its title.
  pub on_navigate: Option<String>,
  /// File name patterns of the files it's for; empty for a layout only
  /// the extension opens.
  pub patterns: Vec<String>,
  /// With patterns: whether matching files open in it.
  pub priority: ViewerPriority,
  /// A named icon for the button that switches a file's tab to it.
  pub icon: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UnderlineStyle {
  Solid,
  Wavy,
  Dotted,
  Dashed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecorationColors {
  pub background: Option<String>,
  pub color: Option<String>,
  pub border: Option<String>,
  pub underline_color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecorationType {
  /// The name the extension uses.
  pub name: String,
  /// `<extension id>/<name>`, as decorations are tagged in views.
  pub key: String,
  pub dark: DecorationColors,
  pub light: DecorationColors,
  pub underline: Option<UnderlineStyle>,
  pub bold: bool,
  pub italic: bool,
  /// The background spans the whole width of each line the range covers,
  /// as VS Code's `isWholeLine`. (1.12)
  pub whole_line: bool,
}

/// A language: which files it is for, and a Tree-sitter grammar compiled to
/// WebAssembly with its queries. Paths are relative to the extension's
/// folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageContribution {
  /// The language id, e.g. `html`. Not prefixed with the extension's id:
  /// language servers, settings, and other languages' injections use it.
  pub id: String,
  pub name: String,
  /// Lowercase, without the leading dot.
  pub file_extensions: Vec<String>,
  pub file_names: Vec<String>,
  #[serde(skip)]
  pub grammar: PathBuf,
  /// The WebAssembly module exports `tree_sitter_<grammar_name>`.
  #[serde(skip)]
  pub grammar_name: String,
  #[serde(skip)]
  pub highlights: PathBuf,
  #[serde(skip)]
  pub injections: Option<PathBuf>,
  /// The fold query (`folds.scm`), which captures foldable nodes as `@fold`.
  #[serde(skip)]
  pub folds: Option<PathBuf>,
  /// Starts a comment to the end of the line, e.g. `//`.
  pub line_comment: Option<String>,
  /// Open and close a block comment, e.g. `/*` and `*/`.
  pub block_comment: Option<(String, String)>,
}

/// A language server the editor starts for documents of some languages,
/// once the user grants `language-servers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageServerContribution {
  pub name: String,
  /// A program on the `PATH`, or a path inside the extension's folder
  /// (made absolute by `ExtensionHost::language_servers`).
  pub command: String,
  pub args: Vec<String>,
  /// Language ids, e.g. `html`.
  pub languages: Vec<String>,
  pub root_markers: Vec<String>,
}

impl LanguageServerContribution {
  /// The command line, as shown when asking for permission.
  pub fn command_line(&self) -> String {
    std::iter::once(&self.command)
      .chain(&self.args)
      .map(|part| {
        if part.is_empty() || part.contains(char::is_whitespace) {
          format!("{part:?}")
        } else {
          part.clone()
        }
      })
      .collect::<Vec<_>>()
      .join(" ")
  }
}

/// A program the extension runs with the `processes` interface. (1.6)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramContribution {
  /// What the extension's code calls it.
  pub name: String,
  /// A program on the `PATH`, or a path inside the extension's folder.
  pub command: String,
  /// A path inside the open folder, such as `node_modules/.bin/prettier`,
  /// run instead when the folder has it and is trusted.
  pub workspace: Option<String>,
  /// It can ask the user to sign in (a password, a passphrase), which
  /// Flareite asks (see `askpass.rs`).
  pub askpass: bool,
}

/// A color the extension's explorer marks use, which themes and the user's
/// `workbench.colors` can change. (1.6)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorContribution {
  /// The name the extension uses.
  pub name: String,
  /// `<extension id>.<name>`: the token themes set.
  pub id: String,
  pub description: String,
  /// Hex colors for dark and light themes.
  pub dark: String,
  pub light: String,
}

/// A schema for TOML or JSON files. (1.16)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaContribution {
  /// The files it's for: name patterns, or ends of paths with '/'.
  pub files: Vec<String>,
  pub source: SchemaSource,
}

/// Where a schema an extension declares is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SchemaSource {
  /// A file in the extension's folder: relative to it in a manifest, and
  /// absolute once loaded.
  Path(PathBuf),
  /// An `https:` (or `http:`) address.
  Url(String),
}

/// Most schemas an extension provides, in its manifest and from its code.
pub const MAX_SCHEMAS: usize = 32;
/// Most file patterns a schema is for.
pub const MAX_SCHEMA_FILES: usize = 50;

/// Checks the patterns of the files a schema is for.
pub fn check_schema_files(files: &[String]) -> Result<(), String> {
  if files.is_empty() || files.len() > MAX_SCHEMA_FILES {
    return Err(format!(
      "a schema is for 1 to {MAX_SCHEMA_FILES} file patterns"
    ));
  }
  for pattern in files {
    if pattern.trim().is_empty()
      || pattern.chars().count() > MAX_PATTERN_CHARS
      || pattern.contains('\\')
    {
      return Err(format!(
        "file pattern {pattern:?} must be 1 to {MAX_PATTERN_CHARS} characters, with '/' between folders"
      ));
    }
  }
  Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contributions {
  pub themes: Vec<ThemeContribution>,
  pub icon_themes: Vec<IconThemeContribution>,
  pub keybindings: Vec<KeybindingContribution>,
  pub menus: Vec<MenuContribution>,
  pub status_items: Vec<StatusItemContribution>,
  pub views: Vec<ViewContribution>,
  pub decorations: Vec<DecorationType>,
  pub previews: Vec<PreviewContribution>,
  pub viewers: Vec<ViewerContribution>,
  /// (1.15)
  pub layouts: Vec<LayoutContribution>,
  /// (1.15)
  pub link_handlers: Vec<LinkHandlerContribution>,
  pub languages: Vec<LanguageContribution>,
  pub language_servers: Vec<LanguageServerContribution>,
  pub programs: Vec<ProgramContribution>,
  pub colors: Vec<ColorContribution>,
  pub view_containers: Vec<ViewContainerContribution>,
  pub text_providers: Vec<TextProviderContribution>,
  pub quick_diff: Option<QuickDiffContribution>,
  /// (1.16)
  pub schemas: Vec<SchemaContribution>,
}

impl Contributions {
  /// Whether anything needs the extension's code to run.
  pub fn needs_code(&self) -> bool {
    !(self.keybindings.is_empty()
      && self.menus.is_empty()
      && self.status_items.is_empty()
      && self.views.is_empty()
      && self.decorations.is_empty()
      && self.previews.is_empty()
      && self.text_providers.is_empty()
      && self.quick_diff.is_none()
      && self.layouts.is_empty()
      && self.link_handlers.is_empty()
      && self.viewers.iter().all(|viewer| {
        viewer.command.is_none()
          && viewer
            .webview
            .as_ref()
            .is_none_or(|webview| webview.on_message.is_none())
          && viewer.actions.iter().all(|action| action.command.is_none())
      }))
  }

  pub fn preview(&self, id: &str) -> Option<&PreviewContribution> {
    self.previews.iter().find(|preview| preview.id == id)
  }

  pub fn viewer(&self, id: &str) -> Option<&ViewerContribution> {
    self.viewers.iter().find(|viewer| viewer.id == id)
  }

  pub fn layout(&self, id: &str) -> Option<&LayoutContribution> {
    self.layouts.iter().find(|layout| layout.id == id)
  }

  pub fn container(&self, id: &str) -> Option<&ViewContainerContribution> {
    self
      .view_containers
      .iter()
      .find(|container| container.id == id)
  }

  pub fn view(&self, id: &str) -> Option<&ViewContribution> {
    self.views.iter().find(|view| view.id == id)
  }

  pub fn status_item(&self, id: &str) -> Option<&StatusItemContribution> {
    self.status_items.iter().find(|item| item.id == id)
  }

  pub fn decoration(&self, name: &str) -> Option<&DecorationType> {
    self
      .decorations
      .iter()
      .find(|decoration| decoration.name == name)
  }

  pub fn text_provider(&self, scheme: &str) -> Option<&TextProviderContribution> {
    self
      .text_providers
      .iter()
      .find(|provider| provider.scheme == scheme)
  }

  pub fn program(&self, name: &str) -> Option<&ProgramContribution> {
    self.programs.iter().find(|program| program.name == name)
  }

  pub fn color(&self, name: &str) -> Option<&ColorContribution> {
    self.colors.iter().find(|color| color.name == name)
  }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTheme {
  id: String,
  label: String,
  kind: String,
  path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawIconTheme {
  id: String,
  label: String,
  path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawKeybinding {
  command: String,
  key: String,
  mac: Option<String>,
  when: Option<String>,
  layouts: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawMenu {
  location: String,
  command: String,
  group: Option<String>,
  targets: Option<Vec<String>>,
  patterns: Option<Vec<String>>,
  view: Option<String>,
  contexts: Option<Vec<String>>,
  #[serde(default)]
  inline: bool,
  icon: Option<String>,
  when: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawStatusItem {
  id: String,
  alignment: Option<String>,
  priority: Option<i32>,
  text: Option<String>,
  tooltip: Option<String>,
  command: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawView {
  pub(crate) id: String,
  title: String,
  icon: Option<String>,
  container: Option<String>,
  input: Option<RawViewInput>,
  on_expand: Option<String>,
  #[serde(default)]
  hidden: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawViewInput {
  placeholder: Option<String>,
  command: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawQuickDiff {
  original: String,
  apply: Option<String>,
  apply_title: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTextProvider {
  scheme: String,
  command: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSchema {
  files: Vec<String>,
  path: Option<String>,
  url: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawViewContainer {
  id: String,
  title: String,
  icon: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPreview {
  id: String,
  icon: Option<String>,
  webview: Option<RawWebview>,
  on_message: Option<String>,
  retain_when_hidden: Option<bool>,
  #[serde(default)]
  actions: Vec<RawAction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawViewer {
  id: String,
  title: String,
  patterns: Vec<String>,
  command: Option<String>,
  priority: Option<String>,
  webview: Option<RawWebview>,
  on_message: Option<String>,
  retain_when_hidden: Option<bool>,
  #[serde(default)]
  actions: Vec<RawAction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLayout {
  id: String,
  title: String,
  command: String,
  on_navigate: Option<String>,
  on_message: Option<String>,
  #[serde(default)]
  patterns: Vec<String>,
  priority: Option<String>,
  icon: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLinkHandler {
  id: String,
  title: String,
  kind: String,
  schemes: Option<Vec<String>>,
  patterns: Option<Vec<String>>,
  command: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawAction {
  id: String,
  title: String,
  icon: String,
  command: Option<String>,
}

/// `webview = "page.html"`, or `webview = true` for a page the extension
/// sets.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum RawWebview {
  Page(String),
  Set(bool),
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RawDecorationColors {
  background: Option<String>,
  color: Option<String>,
  border: Option<String>,
  underline_color: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawDecoration {
  background: Option<String>,
  color: Option<String>,
  border: Option<String>,
  underline_color: Option<String>,
  underline: Option<String>,
  #[serde(default)]
  bold: bool,
  #[serde(default)]
  italic: bool,
  #[serde(default)]
  whole_line: bool,
  light: Option<RawDecorationColors>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLanguage {
  id: String,
  name: String,
  #[serde(default)]
  file_extensions: Vec<String>,
  #[serde(default)]
  file_names: Vec<String>,
  grammar: String,
  grammar_name: Option<String>,
  highlights: String,
  injections: Option<String>,
  folds: Option<String>,
  line_comment: Option<String>,
  block_comment: Option<(String, String)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLanguageServer {
  name: String,
  command: String,
  #[serde(default)]
  args: Vec<String>,
  languages: Vec<String>,
  #[serde(default)]
  root_markers: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawProgram {
  name: String,
  command: Option<String>,
  workspace: Option<String>,
  #[serde(default)]
  askpass: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawColor {
  #[serde(default)]
  description: String,
  dark: String,
  light: Option<String>,
}

/// The contribution sections of a manifest, as written.
#[derive(Default)]
pub struct RawContributions {
  pub themes: Vec<RawTheme>,
  pub icon_themes: Vec<RawIconTheme>,
  pub keybindings: Vec<RawKeybinding>,
  pub menus: Vec<RawMenu>,
  pub status_items: Vec<RawStatusItem>,
  pub views: Vec<RawView>,
  pub decorations: BTreeMap<String, RawDecoration>,
  pub previews: Vec<RawPreview>,
  pub viewers: Vec<RawViewer>,
  pub layouts: Vec<RawLayout>,
  pub link_handlers: Vec<RawLinkHandler>,
  pub languages: Vec<RawLanguage>,
  pub language_servers: Vec<RawLanguageServer>,
  pub programs: Vec<RawProgram>,
  pub colors: BTreeMap<String, RawColor>,
  pub view_containers: Vec<RawViewContainer>,
  pub text_providers: Vec<RawTextProvider>,
  pub quick_diff: Option<RawQuickDiff>,
  pub schemas: Vec<RawSchema>,
}

/// A view menu's `when`: a flag name, or `!` and one.
fn check_when(section: &str, when: &str) -> Result<(), ManifestError> {
  let flag = when.strip_prefix('!').unwrap_or(when);
  if (1..=64).contains(&flag.len())
    && flag
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
  {
    Ok(())
  } else {
    Err(invalid(format!(
      "{section}: when {when:?} must be a flag name, or ! and one"
    )))
  }
}

/// Validates contributions. Theme colors and icons are read later, by
/// [`load_theme_colors`] and [`load_icon_theme`].
pub fn parse(
  raw: RawContributions,
  extension: &str,
  commands: &[CommandContribution],
) -> Result<Contributions, ManifestError> {
  let declared = |section: &str, command: &str| {
    if commands.iter().any(|c| c.id == command) {
      Ok(command.to_owned())
    } else {
      Err(invalid(format!(
        "{section}: command {command:?} is not one of this extension's commands"
      )))
    }
  };
  let limit = |section: &str, count: usize, max: usize| {
    if count > max {
      Err(invalid(format!("at most {max} {section} can be declared")))
    } else {
      Ok(())
    }
  };
  limit("themes", raw.themes.len(), 20)?;
  limit("icon_themes", raw.icon_themes.len(), 5)?;
  limit("keybindings", raw.keybindings.len(), 100)?;
  limit("menus", raw.menus.len(), 100)?;
  limit("status_items", raw.status_items.len(), 20)?;
  limit("views", raw.views.len(), 10)?;
  limit("decorations", raw.decorations.len(), 20)?;
  limit("previews", raw.previews.len(), 10)?;
  limit("viewers", raw.viewers.len(), 10)?;
  limit("layouts", raw.layouts.len(), 10)?;
  limit("link_handlers", raw.link_handlers.len(), 5)?;
  limit("languages", raw.languages.len(), 20)?;
  limit("language_servers", raw.language_servers.len(), 10)?;
  limit("programs", raw.programs.len(), 10)?;
  limit("view_containers", raw.view_containers.len(), 5)?;
  limit("text_providers", raw.text_providers.len(), 5)?;
  limit("colors", raw.colors.len(), 50)?;
  limit("schemas", raw.schemas.len(), MAX_SCHEMAS)?;

  let mut contributions = Contributions::default();
  for theme in raw.themes {
    local_id("themes", &theme.id)?;
    let kind = match theme.kind.as_str() {
      "dark" => ThemeKind::Dark,
      "light" => ThemeKind::Light,
      other => {
        return Err(invalid(format!(
          "themes: kind {other:?} must be \"dark\" or \"light\""
        )));
      }
    };
    let label = text("themes", "label", &theme.label, 64)?;
    let path = inside("themes", &theme.path)?;
    let id = format!("{extension}.{}", theme.id);
    unique("themes", contributions.themes.iter().map(|t| &t.id), &id)?;
    contributions.themes.push(ThemeContribution {
      id,
      label,
      kind,
      colors: BTreeMap::new(),
      path,
    });
  }

  for theme in raw.icon_themes {
    local_id("icon_themes", &theme.id)?;
    let label = text("icon_themes", "label", &theme.label, 64)?;
    let path = inside("icon_themes", &theme.path)?;
    let id = format!("{extension}.{}", theme.id);
    unique(
      "icon_themes",
      contributions.icon_themes.iter().map(|t| &t.id),
      &id,
    )?;
    contributions.icon_themes.push(IconThemeContribution {
      id,
      label,
      path,
      theme: Arc::default(),
    });
  }

  for binding in raw.keybindings {
    let command = declared("keybindings", &binding.command)?;
    let key =
      keys::normalize(&binding.key).map_err(|error| invalid(format!("keybindings: {error}")))?;
    let mac = binding
      .mac
      .as_deref()
      .map(keys::normalize)
      .transpose()
      .map_err(|error| invalid(format!("keybindings: {error}")))?;
    let editor_focus = match binding.when.as_deref() {
      None => false,
      Some("editorFocus") => true,
      Some(other) => {
        return Err(invalid(format!(
          "keybindings: when {other:?} is not supported (only \"editorFocus\")"
        )));
      }
    };
    let layouts = binding.layouts.unwrap_or_default();
    if layouts.len() > 10 {
      return Err(invalid(format!(
        "keybindings: {command}: at most 10 layouts"
      )));
    }
    let layouts = layouts
      .into_iter()
      .map(|layout| {
        if raw.layouts.iter().any(|declared| declared.id == layout) {
          Ok(format!("{extension}.{layout}"))
        } else {
          Err(invalid(format!(
            "keybindings: {command}: layout {layout:?} is not declared"
          )))
        }
      })
      .collect::<Result<Vec<_>, _>>()?;
    contributions.keybindings.push(KeybindingContribution {
      command,
      key,
      mac,
      editor_focus,
      layouts,
    });
  }

  for menu in raw.menus {
    let location = match menu.location.as_str() {
      "editor/context" => MenuLocation::EditorContext,
      "explorer/context" => MenuLocation::ExplorerContext,
      "view/title" => MenuLocation::ViewTitle,
      "view/item" => MenuLocation::ViewItem,
      other => {
        return Err(invalid(format!(
          "menus: location {other:?} must be \"editor/context\", \"explorer/context\", \"view/title\", or \"view/item\""
        )));
      }
    };
    let command = declared("menus", &menu.command)?;
    let in_view = matches!(location, MenuLocation::ViewTitle | MenuLocation::ViewItem);
    let view = match (&menu.view, in_view) {
      (Some(view), true) => {
        if !raw.views.iter().any(|declared| declared.id == *view) {
          return Err(invalid(format!(
            "menus: {command}: view {view:?} is not declared"
          )));
        }
        Some(format!("{extension}.{view}"))
      }
      (None, true) => {
        return Err(invalid(format!(
          "menus: {command}: a view menu needs the view it's for"
        )));
      }
      (_, false) => {
        if menu.view.is_some() || menu.contexts.is_some() || menu.inline || menu.when.is_some() {
          return Err(invalid(format!(
            "menus: {command}: view, contexts, inline, and when only apply to view menus"
          )));
        }
        None
      }
    };
    if menu.contexts.is_some() && location != MenuLocation::ViewItem {
      return Err(invalid(format!(
        "menus: {command}: contexts only apply to \"view/item\""
      )));
    }
    let contexts = menu.contexts.clone().unwrap_or_default();
    if contexts.len() > MAX_MENU_PATTERNS {
      return Err(invalid(format!(
        "menus: {command}: at most {MAX_MENU_PATTERNS} contexts"
      )));
    }
    for context in &contexts {
      local_id(&format!("menus: {command}: contexts"), context)?;
    }
    if let Some(when) = &menu.when {
      check_when(&format!("menus: {command}"), when)?;
    }
    let icon = match &menu.icon {
      Some(icon) if ACTION_ICONS.contains_key(icon) => Some(icon.clone()),
      Some(icon) => {
        return Err(invalid(format!(
          "menus: {command}: icon {icon:?} must be one of the named icons"
        )));
      }
      None => None,
    };
    if menu.inline && icon.is_none() {
      return Err(invalid(format!(
        "menus: {command}: an inline button needs an icon"
      )));
    }
    let (for_files, for_folders) = match (location, menu.targets) {
      (_, None) => (true, false),
      (MenuLocation::EditorContext | MenuLocation::ViewTitle | MenuLocation::ViewItem, Some(_)) => {
        return Err(invalid(format!(
          "menus: {command}: targets only apply to \"explorer/context\""
        )));
      }
      (MenuLocation::ExplorerContext, Some(targets)) => {
        if let Some(other) = targets.iter().find(|t| *t != "files" && *t != "folders") {
          return Err(invalid(format!(
            "menus: {command}: target {other:?} must be \"files\" or \"folders\""
          )));
        }
        if targets.is_empty() {
          return Err(invalid(format!(
            "menus: {command}: targets must list \"files\", \"folders\", or both"
          )));
        }
        (
          targets.iter().any(|t| t == "files"),
          targets.iter().any(|t| t == "folders"),
        )
      }
    };
    let patterns = menu.patterns.unwrap_or_default();
    if patterns.len() > MAX_MENU_PATTERNS {
      return Err(invalid(format!(
        "menus: {command}: at most {MAX_MENU_PATTERNS} patterns"
      )));
    }
    if !patterns.is_empty() && !for_files {
      return Err(invalid(format!(
        "menus: {command}: patterns only apply to files"
      )));
    }
    for pattern in &patterns {
      check_pattern(pattern)
        .map_err(|problem| invalid(format!("menus: {command}: pattern {pattern:?} {problem}")))?;
    }
    if !patterns.is_empty() && in_view {
      return Err(invalid(format!(
        "menus: {command}: patterns don't apply to view menus"
      )));
    }
    contributions.menus.push(MenuContribution {
      location,
      command,
      group: menu.group.unwrap_or_default(),
      for_files,
      for_folders,
      patterns,
      view,
      contexts,
      inline: menu.inline,
      icon,
      when: menu.when,
    });
  }

  for item in raw.status_items {
    local_id("status_items", &item.id)?;
    unique(
      "status_items",
      contributions.status_items.iter().map(|i| &i.id),
      &item.id,
    )?;
    let alignment = match item.alignment.as_deref() {
      None | Some("left") => Alignment::Left,
      Some("right") => Alignment::Right,
      Some(other) => {
        return Err(invalid(format!(
          "status_items: alignment {other:?} must be \"left\" or \"right\""
        )));
      }
    };
    contributions.status_items.push(StatusItemContribution {
      id: item.id,
      alignment,
      priority: item.priority.unwrap_or(0),
      text: item
        .text
        .map(|value| text("status_items", "text", &value, 100))
        .transpose()?,
      tooltip: item
        .tooltip
        .map(|value| text("status_items", "tooltip", &value, 500))
        .transpose()?,
      command: item
        .command
        .map(|command| declared("status_items", &command))
        .transpose()?,
    });
  }

  for container in raw.view_containers {
    local_id("view_containers", &container.id)?;
    unique(
      "view_containers",
      contributions.view_containers.iter().map(|c| &c.id),
      &container.id,
    )?;
    let icon = container.icon.unwrap_or_else(|| "list".into());
    if !VIEW_ICONS.contains(&icon.as_str()) {
      return Err(invalid(format!(
        "view_containers: icon {icon:?} must be one of {}",
        VIEW_ICONS.join(", ")
      )));
    }
    contributions
      .view_containers
      .push(ViewContainerContribution {
        full_id: format!("{extension}.{}", container.id),
        title: text("view_containers", "title", &container.title, 40)?,
        id: container.id,
        icon,
      });
  }

  for view in raw.views {
    local_id("views", &view.id)?;
    unique("views", contributions.views.iter().map(|v| &v.id), &view.id)?;
    if contributions
      .view_containers
      .iter()
      .any(|c| c.id == view.id)
    {
      return Err(invalid(format!(
        "views: {:?} is also a container's id",
        view.id
      )));
    }
    let icon = view.icon.unwrap_or_else(|| "list".into());
    if !VIEW_ICONS.contains(&icon.as_str()) {
      return Err(invalid(format!(
        "views: icon {icon:?} must be one of {}",
        VIEW_ICONS.join(", ")
      )));
    }
    let section = format!("views.{}", view.id);
    let container = match view.container {
      Some(container) => Some(
        contributions
          .view_containers
          .iter()
          .find(|c| c.id == container)
          .ok_or_else(|| {
            invalid(format!(
              "{section}: container {container:?} is not declared"
            ))
          })?
          .full_id
          .clone(),
      ),
      None => None,
    };
    let input = match view.input {
      Some(input) => Some(ViewInput {
        placeholder: input
          .placeholder
          .map(|placeholder| text(&section, "input.placeholder", &placeholder, 100))
          .transpose()?,
        command: input
          .command
          .map(|command| declared(&section, &command))
          .transpose()?,
      }),
      None => None,
    };
    contributions.views.push(ViewContribution {
      full_id: format!("{extension}.{}", view.id),
      title: text("views", "title", &view.title, 40)?,
      id: view.id,
      icon,
      container,
      input,
      on_expand: view
        .on_expand
        .map(|command| declared(&section, &command))
        .transpose()?,
      hidden: view.hidden,
    });
  }
  for container in &contributions.view_containers {
    if !contributions
      .views
      .iter()
      .any(|view| view.container.as_ref() == Some(&container.full_id))
    {
      return Err(invalid(format!(
        "view_containers: {:?} has no views",
        container.id
      )));
    }
  }

  for (name, decoration) in raw.decorations {
    local_id("decorations", &name)?;
    let section = format!("decorations.{name}");
    let base = RawDecorationColors {
      background: decoration.background,
      color: decoration.color,
      border: decoration.border,
      underline_color: decoration.underline_color,
    };
    let dark = colors(&section, &base)?;
    let light = match &decoration.light {
      Some(light) => {
        let overrides = colors(&format!("{section}.light"), light)?;
        DecorationColors {
          background: overrides.background.or_else(|| dark.background.clone()),
          color: overrides.color.or_else(|| dark.color.clone()),
          border: overrides.border.or_else(|| dark.border.clone()),
          underline_color: overrides
            .underline_color
            .or_else(|| dark.underline_color.clone()),
        }
      }
      None => dark.clone(),
    };
    let underline = match decoration.underline.as_deref() {
      None => None,
      Some("solid") => Some(UnderlineStyle::Solid),
      Some("wavy") => Some(UnderlineStyle::Wavy),
      Some("dotted") => Some(UnderlineStyle::Dotted),
      Some("dashed") => Some(UnderlineStyle::Dashed),
      Some(other) => {
        return Err(invalid(format!(
          "{section}: underline {other:?} must be solid, wavy, dotted, or dashed"
        )));
      }
    };
    contributions.decorations.push(DecorationType {
      key: format!("{extension}/{name}"),
      name,
      dark,
      light,
      underline,
      bold: decoration.bold,
      italic: decoration.italic,
      whole_line: decoration.whole_line,
    });
  }

  for preview in raw.previews {
    local_id("previews", &preview.id)?;
    unique(
      "previews",
      contributions.previews.iter().map(|p| &p.id),
      &preview.id,
    )?;
    let icon = preview.icon.unwrap_or_else(|| "book".into());
    if !VIEW_ICONS.contains(&icon.as_str()) {
      return Err(invalid(format!(
        "previews: icon {icon:?} must be one of {}",
        VIEW_ICONS.join(", ")
      )));
    }
    let webview = webview(
      &format!("previews: {}", preview.id),
      preview.webview,
      preview.on_message,
      preview.retain_when_hidden,
      &declared,
    )?;
    let actions = actions(
      &format!("previews: {}", preview.id),
      preview.actions,
      webview.is_some(),
      &declared,
    )?;
    contributions.previews.push(PreviewContribution {
      full_id: format!("{extension}.{}", preview.id),
      id: preview.id,
      icon,
      webview,
      actions,
    });
  }

  for viewer in raw.viewers {
    local_id("viewers", &viewer.id)?;
    unique(
      "viewers",
      contributions.viewers.iter().map(|v| &v.id),
      &viewer.id,
    )?;
    let section = format!("viewers: {}", viewer.id);
    if viewer.patterns.is_empty() || viewer.patterns.len() > MAX_MENU_PATTERNS {
      return Err(invalid(format!(
        "{section}: patterns must list 1 to {MAX_MENU_PATTERNS} file name patterns"
      )));
    }
    for pattern in &viewer.patterns {
      check_pattern(pattern)
        .map_err(|problem| invalid(format!("{section}: pattern {pattern:?} {problem}")))?;
    }
    let priority = match viewer.priority.as_deref() {
      None | Some("default") => ViewerPriority::Default,
      Some("option") => ViewerPriority::Option,
      Some(other) => {
        return Err(invalid(format!(
          "{section}: priority {other:?} must be \"default\" or \"option\""
        )));
      }
    };
    let webview = webview(
      &section,
      viewer.webview,
      viewer.on_message,
      viewer.retain_when_hidden,
      &declared,
    )?;
    let actions = actions(&section, viewer.actions, webview.is_some(), &declared)?;
    if viewer.command.is_none() && webview.is_none() {
      return Err(invalid(format!(
        "{section}: a viewer needs a command, a webview, or both"
      )));
    }
    contributions.viewers.push(ViewerContribution {
      full_id: format!("{extension}.{}", viewer.id),
      title: text("viewers", "title", &viewer.title, 40)?,
      command: viewer
        .command
        .map(|command| declared("viewers", &command))
        .transpose()?,
      id: viewer.id,
      patterns: viewer.patterns,
      priority,
      webview,
      actions,
    });
  }

  for layout in raw.layouts {
    local_id("layouts", &layout.id)?;
    unique(
      "layouts",
      contributions.layouts.iter().map(|l| &l.id),
      &layout.id,
    )?;
    let section = format!("layouts: {}", layout.id);
    if layout.patterns.len() > MAX_MENU_PATTERNS {
      return Err(invalid(format!(
        "{section}: at most {MAX_MENU_PATTERNS} patterns"
      )));
    }
    for pattern in &layout.patterns {
      check_pattern(pattern)
        .map_err(|problem| invalid(format!("{section}: pattern {pattern:?} {problem}")))?;
    }
    let priority = match layout.priority.as_deref() {
      None | Some("option") => ViewerPriority::Option,
      Some("default") => ViewerPriority::Default,
      Some(other) => {
        return Err(invalid(format!(
          "{section}: priority {other:?} must be \"default\" or \"option\""
        )));
      }
    };
    if layout.priority.is_some() && layout.patterns.is_empty() {
      return Err(invalid(format!(
        "{section}: priority only applies to a layout with patterns"
      )));
    }
    let icon = match layout.icon {
      Some(icon) if ACTION_ICONS.contains_key(&icon) => Some(icon),
      Some(icon) => {
        return Err(invalid(format!(
          "{section}: icon {icon:?} must be one of the named icons"
        )));
      }
      None => None,
    };
    contributions.layouts.push(LayoutContribution {
      full_id: format!("{extension}.{}", layout.id),
      title: text(&section, "title", &layout.title, 40)?,
      command: declared(&section, &layout.command)?,
      on_message: layout
        .on_message
        .map(|command| declared(&section, &command))
        .transpose()?,
      on_navigate: layout
        .on_navigate
        .map(|command| declared(&section, &command))
        .transpose()?,
      id: layout.id,
      patterns: layout.patterns,
      priority,
      icon,
    });
  }

  for handler in raw.link_handlers {
    local_id("link_handlers", &handler.id)?;
    unique(
      "link_handlers",
      contributions.link_handlers.iter().map(|h| &h.id),
      &handler.id,
    )?;
    let section = format!("link_handlers: {}", handler.id);
    let kind = match handler.kind.as_str() {
      "web" => LinkKind::Web,
      "file" => LinkKind::File,
      other => {
        return Err(invalid(format!(
          "{section}: kind {other:?} must be \"web\" or \"file\""
        )));
      }
    };
    let schemes = match (kind, handler.schemes) {
      (LinkKind::Web, None) => vec!["https".to_owned(), "http".to_owned()],
      (LinkKind::Web, Some(schemes)) => {
        if schemes.is_empty() {
          return Err(invalid(format!(
            "{section}: schemes must list at least one"
          )));
        }
        for scheme in &schemes {
          if !matches!(scheme.as_str(), "https" | "http" | "mailto") {
            return Err(invalid(format!(
              "{section}: scheme {scheme:?} must be https, http, or mailto"
            )));
          }
        }
        schemes
      }
      (LinkKind::File, Some(_)) => {
        return Err(invalid(format!(
          "{section}: schemes only apply to web links"
        )));
      }
      (LinkKind::File, None) => Vec::new(),
    };
    let patterns = handler.patterns.unwrap_or_default();
    if !patterns.is_empty() && kind == LinkKind::Web {
      return Err(invalid(format!(
        "{section}: patterns only apply to file links"
      )));
    }
    if patterns.len() > MAX_MENU_PATTERNS {
      return Err(invalid(format!(
        "{section}: at most {MAX_MENU_PATTERNS} patterns"
      )));
    }
    for pattern in &patterns {
      check_pattern(pattern)
        .map_err(|problem| invalid(format!("{section}: pattern {pattern:?} {problem}")))?;
    }
    contributions.link_handlers.push(LinkHandlerContribution {
      full_id: format!("{extension}.{}", handler.id),
      title: text(&section, "title", &handler.title, 40)?,
      command: declared(&section, &handler.command)?,
      id: handler.id,
      kind,
      schemes,
      patterns,
    });
  }

  for language in raw.languages {
    local_id("languages", &language.id)?;
    unique(
      "languages",
      contributions.languages.iter().map(|l| &l.id),
      &language.id,
    )?;
    let section = format!("languages.{}", language.id);
    limit(
      &format!("{section}: file_extensions"),
      language.file_extensions.len(),
      50,
    )?;
    limit(
      &format!("{section}: file_names"),
      language.file_names.len(),
      50,
    )?;
    let file_extensions = language
      .file_extensions
      .iter()
      .map(|extension| file_extension(&section, extension))
      .collect::<Result<_, _>>()?;
    let file_names = language
      .file_names
      .iter()
      .map(|name| file_name(&section, name))
      .collect::<Result<_, _>>()?;
    let grammar = inside(&section, &language.grammar)?;
    if grammar
      .extension()
      .is_none_or(|extension| extension != "wasm")
    {
      return Err(invalid(format!(
        "{section}: grammar {:?} must be a .wasm file built with `tree-sitter build --wasm`",
        language.grammar
      )));
    }
    let grammar_name = language
      .grammar_name
      .unwrap_or_else(|| language.id.replace('-', "_"));
    let valid_name = (1..=64).contains(&grammar_name.len())
      && grammar_name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid_name {
      return Err(invalid(format!(
        "{section}: grammar_name {grammar_name:?} must be lowercase letters, digits, and '_'"
      )));
    }
    contributions.languages.push(LanguageContribution {
      name: text(&section, "name", &language.name, 64)?,
      file_extensions,
      file_names,
      grammar,
      grammar_name,
      highlights: inside(&section, &language.highlights)?,
      injections: language
        .injections
        .as_deref()
        .map(|path| inside(&section, path))
        .transpose()?,
      folds: language
        .folds
        .as_deref()
        .map(|path| inside(&section, path))
        .transpose()?,
      line_comment: language
        .line_comment
        .as_deref()
        .map(|token| comment_token(&section, "line_comment", token))
        .transpose()?,
      block_comment: language
        .block_comment
        .as_ref()
        .map(|(open, close)| {
          Ok::<_, ManifestError>((
            comment_token(&section, "block_comment", open)?,
            comment_token(&section, "block_comment", close)?,
          ))
        })
        .transpose()?,
      id: language.id,
    });
  }

  for server in raw.language_servers {
    let valid_name = (1..=64).contains(&server.name.len())
      && server
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid_name {
      return Err(invalid(format!(
        "language_servers: name {:?} must be 1 to 64 letters, digits, '-', '_', or '.'",
        server.name
      )));
    }
    unique(
      "language_servers",
      contributions.language_servers.iter().map(|s| &s.name),
      &server.name,
    )?;
    let section = format!("language_servers.{}", server.name);
    // A program to find on the PATH, or one the extension ships.
    if server.command.contains(['/', '\\']) {
      inside(&section, &server.command)?;
    } else {
      file_name(&section, &server.command)?;
    }
    limit(&format!("{section}: args"), server.args.len(), 32)?;
    if server
      .args
      .iter()
      .any(|arg| arg.chars().count() > 1000 || arg.contains('\0'))
    {
      return Err(invalid(format!(
        "{section}: arguments must be at most 1000 characters"
      )));
    }
    if server.languages.is_empty() {
      return Err(invalid(format!("{section}: languages must not be empty")));
    }
    limit(&format!("{section}: languages"), server.languages.len(), 20)?;
    for language in &server.languages {
      local_id(&section, language)?;
    }
    limit(
      &format!("{section}: root_markers"),
      server.root_markers.len(),
      20,
    )?;
    for marker in &server.root_markers {
      file_name(&section, marker)?;
    }
    contributions
      .language_servers
      .push(LanguageServerContribution {
        name: server.name,
        command: server.command,
        args: server.args,
        languages: server.languages,
        root_markers: server.root_markers,
      });
  }

  for provider in raw.text_providers {
    if !valid_scheme(&provider.scheme) {
      return Err(invalid(format!(
        "text_providers: scheme {:?} must be 2 to 32 lowercase letters, digits, '+', '-', or '.', start with a letter, and not be reserved",
        provider.scheme
      )));
    }
    unique(
      "text_providers",
      contributions.text_providers.iter().map(|p| &p.scheme),
      &provider.scheme,
    )?;
    let section = format!("text_providers.{}", provider.scheme);
    contributions.text_providers.push(TextProviderContribution {
      command: declared(&section, &provider.command)?,
      scheme: provider.scheme,
    });
  }

  for schema in raw.schemas {
    check_schema_files(&schema.files).map_err(|error| invalid(format!("schemas: {error}")))?;
    let source = match (schema.path, schema.url) {
      (Some(path), None) => {
        let relative = inside("schemas", &path)?;
        let extension = relative
          .extension()
          .and_then(|extension| extension.to_str())
          .map(str::to_ascii_lowercase);
        if !matches!(extension.as_deref(), Some("tosd" | "json")) {
          return Err(invalid(format!(
            "schemas: {path:?} must be a TOML Schema (.tosd) or a JSON Schema (.json)"
          )));
        }
        SchemaSource::Path(relative)
      }
      (None, Some(url)) => {
        if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 2048 {
          return Err(invalid(format!(
            "schemas: url {url:?} must be an https: or http: address"
          )));
        }
        SchemaSource::Url(url)
      }
      _ => {
        return Err(invalid(
          "schemas: each has a path or a url, not both".to_owned(),
        ));
      }
    };
    contributions.schemas.push(SchemaContribution {
      files: schema.files,
      source,
    });
  }

  if let Some(quick_diff) = raw.quick_diff {
    if quick_diff.apply.is_none() != quick_diff.apply_title.is_none() {
      return Err(invalid(
        "quick_diff: apply and apply_title go together".to_owned(),
      ));
    }
    if let Some(title) = &quick_diff.apply_title
      && !(1..=30).contains(&title.trim().chars().count())
    {
      return Err(invalid(
        "quick_diff: apply_title is 1 to 30 characters".to_owned(),
      ));
    }
    contributions.quick_diff = Some(QuickDiffContribution {
      original: declared("quick_diff", &quick_diff.original)?,
      apply: quick_diff
        .apply
        .map(|command| declared("quick_diff", &command))
        .transpose()?,
      apply_title: quick_diff.apply_title.map(|title| title.trim().to_owned()),
    });
  }

  for program in raw.programs {
    local_id("programs", &program.name)?;
    unique(
      "programs",
      contributions.programs.iter().map(|p| &p.name),
      &program.name,
    )?;
    let section = format!("programs.{}", program.name);
    let command = program.command.unwrap_or_else(|| program.name.clone());
    // A program to find on the PATH, or one the extension ships.
    if command.contains(['/', '\\']) {
      inside(&section, &command)?;
    } else {
      file_name(&section, &command)?;
    }
    if let Some(workspace) = &program.workspace {
      inside(&section, workspace).map_err(|_| {
        invalid(format!(
          "{section}: workspace {workspace:?} must be a relative path inside the open folder"
        ))
      })?;
    }
    contributions.programs.push(ProgramContribution {
      name: program.name,
      command,
      workspace: program.workspace,
      askpass: program.askpass,
    });
  }

  for (name, color) in raw.colors {
    local_id("colors", &name)?;
    let section = format!("colors.{name}");
    let check = |field: &str, value: &str| {
      if is_hex_color(value) {
        Ok(value.to_lowercase())
      } else {
        Err(invalid(format!(
          "{section}: {field} must be a hex color like \"#e2c08d\", not {value:?}"
        )))
      }
    };
    let dark = check("dark", &color.dark)?;
    let light = match &color.light {
      Some(light) => check("light", light)?,
      None => dark.clone(),
    };
    if color.description.chars().count() > 200 {
      return Err(invalid(format!(
        "{section}: description must be at most 200 characters"
      )));
    }
    contributions.colors.push(ColorContribution {
      id: format!("{extension}.{name}"),
      name,
      description: color.description.trim().to_owned(),
      dark,
      light,
    });
  }
  Ok(contributions)
}

/// Checks that a language's grammar and query files exist. They are read
/// when a file of the language is first opened.
pub fn check_language_files(
  dir: &Path,
  language: &LanguageContribution,
) -> Result<(), ManifestError> {
  let paths = [&language.grammar, &language.highlights]
    .into_iter()
    .chain(&language.injections)
    .chain(&language.folds);
  for path in paths {
    let full = dir.join(path);
    let metadata = std::fs::metadata(&full).map_err(|source| ManifestError::Read {
      path: full.clone(),
      source,
    })?;
    if !metadata.is_file() {
      return Err(invalid(format!(
        "languages.{}: {} is not a file",
        language.id,
        path.display()
      )));
    }
  }
  Ok(())
}

/// Largest schema file an extension provides.
pub const MAX_SCHEMA_BYTES: u64 = 4 * 1024 * 1024;

/// Checks that the schema files the manifest names exist, and makes their
/// paths absolute.
pub fn load_schemas(dir: &Path, contributions: &mut Contributions) -> Result<(), ManifestError> {
  for schema in &mut contributions.schemas {
    let SchemaSource::Path(path) = &mut schema.source else {
      continue;
    };
    let full = dir.join(&*path);
    let metadata = std::fs::metadata(&full).map_err(|source| ManifestError::Read {
      path: full.clone(),
      source,
    })?;
    if !metadata.is_file() {
      return Err(invalid(format!(
        "schemas: {} is not a file",
        path.display()
      )));
    }
    if metadata.len() > MAX_SCHEMA_BYTES {
      return Err(invalid(format!(
        "schemas: {} is larger than {} MiB",
        path.display(),
        MAX_SCHEMA_BYTES / 1024 / 1024
      )));
    }
    *path = full;
  }
  Ok(())
}

/// A file extension without the leading dot, lowercased.
fn file_extension(section: &str, extension: &str) -> Result<String, ManifestError> {
  let valid = (1..=32).contains(&extension.len())
    && !extension.starts_with('.')
    && !extension.ends_with('.')
    && extension
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'));
  if valid {
    Ok(extension.to_ascii_lowercase())
  } else {
    Err(invalid(format!(
      "{section}: file extension {extension:?} must be letters, digits, '.', '-', '_', or '+', without the leading dot"
    )))
  }
}

/// A file name without a folder: 1 to 128 characters.
fn file_name(section: &str, name: &str) -> Result<String, ManifestError> {
  let valid = (1..=128).contains(&name.len())
    && !matches!(name, "." | "..")
    && !name.contains(['/', '\\', '\0']);
  if valid {
    Ok(name.to_owned())
  } else {
    Err(invalid(format!(
      "{section}: {name:?} must be a file name without a folder"
    )))
  }
}

/// Reads and checks a theme's colors from its file:
/// `[colors]` mapping known tokens to hex colors.
pub fn load_theme_colors(dir: &Path, theme: &mut ThemeContribution) -> Result<(), ManifestError> {
  #[derive(Deserialize)]
  #[serde(deny_unknown_fields)]
  struct ThemeFile {
    colors: BTreeMap<String, String>,
  }
  let path = dir.join(&theme.path);
  let read = |source| ManifestError::Read {
    path: path.clone(),
    source,
  };
  if std::fs::metadata(&path).map_err(read)?.len() > MAX_THEME_FILE_BYTES {
    return Err(invalid(format!(
      "theme {} is too large",
      theme.path.display()
    )));
  }
  let text = std::fs::read_to_string(&path).map_err(read)?;
  let file: ThemeFile = toml::from_str(&text)
    .map_err(|error| invalid(format!("theme {}: {error}", theme.path.display())))?;
  theme.colors = check_theme_colors(&file.colors)
    .map_err(|error| invalid(format!("theme {}: {error}", theme.path.display())))?;
  Ok(())
}

/// Reads an icon theme's file and icons.
pub fn load_icon_theme(dir: &Path, theme: &mut IconThemeContribution) -> Result<(), ManifestError> {
  let loaded = icons::load(dir, &theme.path)
    .map_err(|error| invalid(format!("icon theme {}: {error}", theme.path.display())))?;
  theme.theme = Arc::new(loaded);
  Ok(())
}

/// Checks theme colors: keys must be known tokens or extension colors,
/// values hex colors. Also used for the user's `workbench.colors`.
pub fn check_theme_colors(
  colors: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
  let mut checked = BTreeMap::new();
  for (token, value) in colors {
    if !THEME_TOKENS.contains(token) && !is_extension_color(token) {
      return Err(format!("{token:?} is not a theme color"));
    }
    if !is_hex_color(value) {
      return Err(format!(
        "{token} must be a hex color like \"#1e1e2e\", not {value:?}"
      ));
    }
    checked.insert(token.clone(), value.to_lowercase());
  }
  Ok(checked)
}

/// Whether `token` names an extension's color, `<extension id>.<name>`
/// (such as `flareite.git.modified`). The extension needn't be loaded: a
/// theme may color extensions the user hasn't installed.
pub fn is_extension_color(token: &str) -> bool {
  let Some((extension, name)) = token.rsplit_once('.') else {
    return false;
  };
  let segment = |segment: &str| {
    !segment.is_empty()
      && segment
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
  };
  extension.contains('.')
    && extension.split('.').all(segment)
    && (1..=32).contains(&name.len())
    && name
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

/// `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa`.
pub fn is_hex_color(value: &str) -> bool {
  value.strip_prefix('#').is_some_and(|hex| {
    matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit())
  })
}

fn colors(section: &str, raw: &RawDecorationColors) -> Result<DecorationColors, ManifestError> {
  // A hex color, or (1.15) one of the workbench's color tokens, such as
  // `diff-inserted-line`, which follows the theme.
  let check = |field: &str, value: &Option<String>| match value {
    Some(value) if !is_hex_color(value) && !THEME_TOKENS.contains(value) => Err(invalid(format!(
      "{section}: {field} must be a hex color like \"#e5c07b\" or a theme color token, not {value:?}"
    ))),
    other => Ok(other.as_ref().map(|value| value.to_lowercase())),
  };
  Ok(DecorationColors {
    background: check("background", &raw.background)?,
    color: check("color", &raw.color)?,
    border: check("border", &raw.border)?,
    underline_color: check("underline_color", &raw.underline_color)?,
  })
}

/// Ids local to an extension: lowercase letters, digits, '-', and '_'.
fn local_id(section: &str, id: &str) -> Result<(), ManifestError> {
  let valid = (1..=32).contains(&id.len())
    && id
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
  if valid {
    Ok(())
  } else {
    Err(invalid(format!(
      "{section}: id {id:?} must be 1 to 32 lowercase letters, digits, '-', or '_'"
    )))
  }
}

fn unique<'a>(
  section: &str,
  mut existing: impl Iterator<Item = &'a String>,
  id: &str,
) -> Result<(), ManifestError> {
  if existing.any(|other| other == id) {
    Err(invalid(format!("{section}: {id:?} is declared twice")))
  } else {
    Ok(())
  }
}

fn text(section: &str, field: &str, value: &str, max: usize) -> Result<String, ManifestError> {
  let value = value.trim();
  if value.is_empty() || value.chars().count() > max {
    Err(invalid(format!(
      "{section}: {field} must be 1 to {max} characters"
    )))
  } else {
    Ok(value.to_owned())
  }
}

/// A comment delimiter: 1 to 8 characters, without whitespace.
fn comment_token(section: &str, field: &str, token: &str) -> Result<String, ManifestError> {
  if (1..=8).contains(&token.chars().count()) && !token.contains(char::is_whitespace) {
    Ok(token.to_owned())
  } else {
    Err(invalid(format!(
      "{section}: {field} delimiters must be 1 to 8 characters without spaces"
    )))
  }
}

/// A viewer's or preview's `webview`, `on_message`, and
/// `retain_when_hidden`.
fn webview(
  section: &str,
  raw: Option<RawWebview>,
  on_message: Option<String>,
  retain_when_hidden: Option<bool>,
  declared: &dyn Fn(&str, &str) -> Result<String, ManifestError>,
) -> Result<Option<WebviewContribution>, ManifestError> {
  let page = match raw {
    None | Some(RawWebview::Set(false)) => {
      return if on_message.is_some() || retain_when_hidden.is_some() {
        Err(invalid(format!(
          "{section}: on_message and retain_when_hidden need a webview"
        )))
      } else {
        Ok(None)
      };
    }
    Some(RawWebview::Set(true)) => None,
    Some(RawWebview::Page(page)) => {
      let path = inside(section, &page)?;
      let html = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "html" | "htm"));
      if !html {
        return Err(invalid(format!(
          "{section}: webview {page:?} must be an HTML file"
        )));
      }
      Some(page.replace('\\', "/"))
    }
  };
  Ok(Some(WebviewContribution {
    page,
    on_message: on_message
      .map(|command| declared(section, &command))
      .transpose()?,
    retain_when_hidden: retain_when_hidden.unwrap_or(false),
  }))
}

/// A viewer's or preview's `actions`.
fn actions(
  section: &str,
  raw: Vec<RawAction>,
  webview: bool,
  declared: &dyn Fn(&str, &str) -> Result<String, ManifestError>,
) -> Result<Vec<TabAction>, ManifestError> {
  if raw.len() > MAX_ACTIONS {
    return Err(invalid(format!(
      "{section}: at most {MAX_ACTIONS} actions can be declared"
    )));
  }
  let mut actions: Vec<TabAction> = Vec::new();
  for action in raw {
    let section = format!("{section}: actions");
    local_id(&section, &action.id)?;
    unique(&section, actions.iter().map(|a| &a.id), &action.id)?;
    let icon = if ACTION_ICONS.contains_key(&action.icon) {
      ActionIcon::Named { name: action.icon }
    } else {
      let path = inside(&section, &action.icon).map_err(|_| {
        invalid(format!(
          "{section}: icon {:?} must be one of the named icons or an SVG or PNG file in the extension's folder",
          action.icon
        ))
      })?;
      let image = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "svg" | "png"));
      if !image {
        return Err(invalid(format!(
          "{section}: icon {:?} must be one of the named icons or an SVG or PNG file",
          action.icon
        )));
      }
      ActionIcon::Image {
        path: action.icon.replace('\\', "/"),
        url: String::new(),
      }
    };
    let command = action
      .command
      .map(|command| declared(&section, &command))
      .transpose()?;
    if command.is_none() && !webview {
      return Err(invalid(format!(
        "{section}: action {:?} needs a command, since only a webview's page can handle a click",
        action.id
      )));
    }
    actions.push(TabAction {
      title: text(&section, "title", &action.title, 40)?,
      id: action.id,
      icon,
      command,
    });
  }
  Ok(actions)
}

/// Reads the icon files tab actions use, as `data:` URLs.
pub fn load_action_icons(
  dir: &Path,
  contributions: &mut Contributions,
) -> Result<(), ManifestError> {
  let actions = contributions
    .previews
    .iter_mut()
    .flat_map(|preview| preview.actions.iter_mut())
    .chain(
      contributions
        .viewers
        .iter_mut()
        .flat_map(|viewer| viewer.actions.iter_mut()),
    );
  for action in actions {
    let ActionIcon::Image { path, url } = &mut action.icon else {
      continue;
    };
    let full = dir.join(&*path);
    let read = |source| ManifestError::Read {
      path: full.clone(),
      source,
    };
    let size = std::fs::metadata(&full).map_err(read)?.len();
    if size > MAX_ACTION_ICON_BYTES {
      return Err(invalid(format!(
        "action icon {path} must be at most {} KiB",
        MAX_ACTION_ICON_BYTES / 1024
      )));
    }
    let bytes = std::fs::read(&full).map_err(read)?;
    let media = if path.to_ascii_lowercase().ends_with(".svg") {
      "image/svg+xml"
    } else {
      "image/png"
    };
    *url = format!(
      "data:{media};base64,{}",
      base64::engine::general_purpose::STANDARD.encode(bytes)
    );
  }
  Ok(())
}

/// Checks that the webview pages viewers and previews name are files.
pub fn check_webview_pages(dir: &Path, contributions: &Contributions) -> Result<(), ManifestError> {
  let pages = contributions
    .previews
    .iter()
    .filter_map(|preview| preview.webview.as_ref())
    .chain(
      contributions
        .viewers
        .iter()
        .filter_map(|viewer| viewer.webview.as_ref()),
    )
    .filter_map(|webview| webview.page.as_deref());
  for page in pages {
    let full = dir.join(page);
    let metadata = std::fs::metadata(&full).map_err(|source| ManifestError::Read {
      path: full.clone(),
      source,
    })?;
    if !metadata.is_file() {
      return Err(invalid(format!("webview {page} is not a file")));
    }
  }
  Ok(())
}

/// A relative path that stays inside the extension's folder.
pub(crate) fn inside(section: &str, path: &str) -> Result<PathBuf, ManifestError> {
  let relative = Path::new(path);
  let confined = !path.is_empty()
    && !relative.has_root()
    && relative
      .components()
      .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
  if confined {
    Ok(relative.to_path_buf())
  } else {
    Err(invalid(format!(
      "{section}: path {path:?} must be inside the extension's folder"
    )))
  }
}

/// Checks a page a webview pane shows: an HTML file in the extension's
/// folder, named relative to it.
pub(crate) fn check_page(page: &str) -> Result<(), String> {
  let html = Path::new(page)
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "html" | "htm"));
  if inside("page", page).is_err() || page.contains('\\') || !html || page.len() > 1_024 {
    return Err(format!(
      "{page:?} must be an HTML file in the extension's folder, named relative to it with /"
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn themes_may_color_extensions() {
    assert!(is_extension_color("flareite.git.modified"));
    assert!(is_extension_color("my-pub.tool.added_2"));
    assert!(
      !is_extension_color("git.modified"),
      "needs a whole extension id"
    );
    assert!(!is_extension_color("editor-background"));
    assert!(!is_extension_color("Flareite.git.modified"));
    assert!(!is_extension_color("flareite.git."));
    let colors = BTreeMap::from([
      ("flareite.git.modified".to_owned(), "#ABCDEF".to_owned()),
      ("editor-background".to_owned(), "#000".to_owned()),
    ]);
    assert_eq!(
      check_theme_colors(&colors).expect("valid")["flareite.git.modified"],
      "#abcdef"
    );
  }

  #[test]
  fn checks_menu_patterns() {
    for ok in ["*.md", "*.{ts,tsx}", "Cargo.toml", "?.txt", ".*rc"] {
      assert_eq!(check_pattern(ok), Ok(()), "{ok}");
    }
    for bad in [
      "", "src/*.rs", r"**\*.rs", "[ab].rs", "{a,{b}}", "{a", "a}", "a\tb",
    ] {
      assert!(check_pattern(bad).is_err(), "{bad:?}");
    }
  }

  #[test]
  fn the_token_list_is_loaded() {
    assert!(THEME_TOKENS.contains("editor-background"));
    assert!(THEME_TOKENS.len() > 50);
  }

  #[test]
  fn checks_theme_colors() {
    let ok = BTreeMap::from([("editor-background".to_owned(), "#ABCDEF".to_owned())]);
    assert_eq!(
      check_theme_colors(&ok).expect("valid")["editor-background"],
      "#abcdef"
    );
    let token = BTreeMap::from([("editor-backdrop".to_owned(), "#000".to_owned())]);
    assert!(check_theme_colors(&token).is_err());
    for value in ["red", "#12345", "url(x)", "#000;background:url(x)"] {
      let colors = BTreeMap::from([("text".to_owned(), value.to_owned())]);
      assert!(check_theme_colors(&colors).is_err(), "{value}");
    }
  }
}
