//! Flareite's extension host: loads WebAssembly extensions and runs them in
//! a sandbox.
//!
//! An extension is a folder with an `extension.toml` manifest and a
//! WebAssembly component (or a core module built with wit-bindgen, which
//! is turned into a component on load) implementing the `extension` world
//! of `flareite-extension-api`. Its only capabilities are the functions of
//! that interface: there is no WASI, so no network, environment, or clock
//! access, files only through the `files` interface (the open folder's,
//! with the user's permission), and programs only those its manifest
//! declares, with the user's permission, in a trusted folder.
//!
//! - **Permissions** requested in the manifest must be granted by the user
//!   before the extension starts, and are checked on every call.
//! - **Each extension runs on its own thread** with a bounded event queue;
//!   the editor only ever queues events, so a slow extension can't block
//!   typing.
//! - **Limits:** memory per instance, a deadline per call (interrupting
//!   infinite loops), stack depth, sizes of reads, edits, diagnostics, and
//!   output. A failing extension is stopped and reported; the editor
//!   carries on.
//!
//! The embedder (the app) implements [`Embedder`] to give the host access
//! to the editor core and to receive [`Notification`]s.

mod api;
pub mod askpass;
mod config;
pub mod contributions;
mod development;
pub mod documents;
mod files;
mod host;
pub mod icons;
pub mod keys;
pub mod language_servers;
pub mod layouts;
pub mod links;
pub mod manifest;
mod markdown;
pub mod marks;
mod permissions;
pub mod preview;
mod processes;
pub mod prompts;
mod quick_diff;
mod readme;
mod runtime;
pub mod schemas;
pub mod storage;
mod timers;
mod usage;
mod views;
pub mod webview;
mod worker;
mod workspace;

/// Host bindings generated from the WIT interface.
mod bindings {
  wasmtime::component::bindgen!({
    path: "../flareite-extension-api/wit",
    world: "extension",
    additional_derives: [PartialEq],
  });
}

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

pub use api::{MAX_DIAGNOSTICS, MAX_EDITS, MAX_READ_BYTES, MAX_READ_LINES};
pub use config::{ConfigProperty, ConfigType, ConfigValue};
pub use contributions::Contributions;
pub use files::{MAX_FILE_BYTES, MAX_LISTED_FILES};
pub use host::{CommandTicket, ExtensionHost, LayoutView, MAX_WATCHES, viewer_id};
pub use manifest::{Build, BuildPlan, MANIFEST_FILE, Manifest, ManifestError};
pub use marks::{ExtensionMarks, FileMark};
pub use permissions::{Decision, Permission};
pub use preview::{PreviewContent, PreviewInfo};
pub use prompts::{Prompt, PromptKind, PromptReply};
pub use usage::ResourceUsage;

/// What the host needs from the application.
pub trait Embedder: Send + Sync + 'static {
  /// Runs `f` with the editor locked. Called from extension threads.
  fn with_editor(&self, f: &mut dyn FnMut(&mut flareite_core::Editor));
  /// The language identifier for a file, e.g. `rust`.
  fn language_of(&self, path: &Path) -> Option<String>;
  /// Receives notifications. Must not block or call back into the host.
  fn notify(&self, notification: Notification);
  /// The folder open in the editor, for the `files` interface.
  fn open_folder(&self) -> Option<OpenFolder> {
    None
  }
  /// The executable programs run to ask the user for a password (the
  /// editor's own, which calls [`askpass::client_main`] first thing), for
  /// programs declared with `askpass`. `None` leaves them to ask on their own.
  fn askpass_program(&self) -> Option<PathBuf> {
    None
  }
  /// Saves an open document to its file, as the user's Save does but
  /// without asking anything. Called from extension threads, without the
  /// editor locked.
  fn save(&self, document: flareite_core::DocumentId) -> Result<(), String> {
    let _ = document;
    Err("documents can't be saved here".into())
  }
  /// Sets one of an extension's settings in the user's settings (`None`
  /// removes it), with a value already checked against its declaration.
  /// Called from extension threads.
  fn set_setting(
    &self,
    extension: &str,
    key: &str,
    value: Option<toml::Value>,
  ) -> Result<(), String> {
    let _ = (extension, key, value);
    Err("settings can't be changed here".into())
  }
}

/// The folder open in the editor.
#[derive(Debug, Clone)]
pub struct OpenFolder {
  /// As [`flareite_workspace::normalize_path`] gives it.
  pub root: PathBuf,
  /// The user's `files.exclude` setting.
  pub exclude: flareite_workspace::ExcludeFilter,
  /// Whether the user trusts it: programs run only in trusted folders.
  pub trusted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageLevel {
  Info,
  Warning,
  Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Notification {
  /// An extension asked to show a message.
  Message {
    /// The extension's id.
    id: String,
    /// The extension's name.
    extension: String,
    level: MessageLevel,
    message: String,
  },
  /// An extension changed a document's text.
  DocumentEdited(flareite_core::DocumentId),
  /// A document's extension diagnostics changed.
  DiagnosticsChanged(flareite_core::DocumentId),
  /// Extensions were loaded, started, stopped, or changed state.
  ExtensionsChanged,
  /// A development extension's build started or finished
  /// ([`ExtensionStatus::build`]).
  BuildChanged,
  /// An extension failed and was stopped.
  ExtensionFailed {
    id: String,
    name: String,
    reason: String,
  },
  /// Status bar items changed.
  StatusItemsChanged,
  /// The content of a view changed (by full view id).
  ViewChanged(String),
  /// An extension asked to show one of its views.
  ShowView(String),
  /// A document's extension decorations or code lenses changed.
  DecorationsChanged(flareite_core::DocumentId),
  /// An extension asked to select and show a range.
  Reveal {
    document: flareite_core::DocumentId,
    start: flareite_text::Utf16Position,
    end: flareite_text::Utf16Position,
  },
  /// Previews were opened or closed, or one's title changed.
  PreviewsChanged,
  /// The content of a preview changed (by preview id).
  PreviewChanged(String),
  /// An extension asked to show one of its previews, beside its document
  /// or in the same group.
  ShowPreview { id: String, beside: bool },
  /// An extension replaced a webview's page (by preview id).
  WebviewChanged(String),
  /// An extension sent a message (JSON text) to a webview's page.
  WebviewMessage { id: String, message: String },
  /// A value an extension stores changed (`None`: removed). `scope` and
  /// `key` are `None` when all were cleared. `source` is the webview whose
  /// page changed it, if one did.
  StorageChanged {
    extension: String,
    scope: Option<String>,
    key: Option<String>,
    value: Option<String>,
    source: Option<String>,
  },
  /// Extensions' explorer marks changed.
  FileMarksChanged,
  /// Schemas an extension's code provides changed (1.16); see
  /// [`ExtensionHost::schemas`]. (Those manifests declare change with
  /// [`Notification::ExtensionsChanged`].)
  SchemasChanged,
  /// What extensions' code set for their language servers changed
  /// (1.16); see [`ExtensionHost::language_servers`].
  LanguageServersChanged,
  /// An extension asked to restart a language server's running instances.
  RestartLanguageServer { name: String },
  /// An extension asked the user to trust the open folder.
  TrustRequested {
    /// The extension's id.
    extension: String,
  },
  /// An extension asked to open a file in the editor.
  OpenFile { path: PathBuf },
  /// An extension asked to open a folder in place of the open one.
  OpenFolder { path: PathBuf },
  /// An extension asks the user something; it replaces any prompt showing.
  Prompt(Prompt),
  /// A prompt was withdrawn (its extension stopped).
  PromptClosed(u64),
  /// The texts at these provided addresses may have changed; a trailing
  /// `*` stands for every address starting with what's before it.
  ProvidedChanged(Vec<String>),
  /// An extension asked to open the document at a provided address.
  OpenProvided(String),
  /// An extension asked to open one of its layouts' tabs, or show it.
  /// (1.15)
  OpenLayout { tab: layouts::TabId, beside: bool },
  /// An extension set what a layout tab shows.
  LayoutChanged(layouts::TabId),
  /// An extension asked to close a layout tab.
  CloseLayout(layouts::TabId),
  /// What an editor pane of a layout shows besides its text changed.
  PaneDecorationsChanged { tab: layouts::TabId, pane: String },
  /// A link handler handed its link back: Flareite opens it its own way.
  OpenLinkDefault(links::LinkRequest),
  /// An extension asked to move a web page pane (by its view's id).
  WebPageAction {
    id: String,
    action: layouts::WebAction,
  },
  /// An extension asked to select and show a range in an editor pane.
  RevealInPane {
    tab: layouts::TabId,
    pane: String,
    start: layouts::Point,
    end: layouts::Point,
  },
  /// Open files' originals may be different ones now (all, when empty).
  OriginalsChanged(Vec<String>),
  /// An extension asked to apply the changes the active editor's
  /// selections touch to its original.
  ApplySelectedRanges,
}

/// What a status bar item shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusContent {
  pub text: String,
  pub tooltip: Option<String>,
  /// The extension command run on click.
  pub command: Option<String>,
}

/// A status bar item to show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusItem {
  /// `<extension id>.<item id>`.
  pub id: String,
  /// The extension's name.
  pub extension: String,
  pub alignment: contributions::Alignment,
  pub priority: i32,
  #[serde(flatten)]
  pub content: StatusContent,
}

/// A row of an extension view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewEntry {
  pub id: String,
  pub parent: Option<String>,
  pub label: String,
  pub description: Option<String>,
  pub tooltip: Option<String>,
  pub expanded: bool,
  pub command: Option<String>,
  pub argument: Option<String>,
  /// (1.7)
  pub icon: Option<ItemIcon>,
  pub mark: Option<ItemMark>,
  /// Which `view/item` menus apply to it.
  pub context: Option<String>,
  /// It can be opened even with no children listed: opening it asks the
  /// extension for them.
  pub expandable: bool,
}

/// A view item's icon. (1.7)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ItemIcon {
  /// One of the named icons.
  Named { name: String },
  /// The file icon theme's icon for a file with this path or name.
  File { path: String },
  /// The file icon theme's icon for a folder.
  Folder { path: String },
}

/// How a view item looks besides its label. (1.7)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemMark {
  pub badge: Option<String>,
  /// A color's theme token, `<extension id>.<name>`.
  pub color: Option<String>,
  pub strikethrough: bool,
  pub faded: bool,
}

/// Shown in a view instead of its items while it has none. (1.7)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
  pub text: String,
  pub buttons: Vec<WelcomeButton>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WelcomeButton {
  pub label: String,
  pub command: String,
  pub argument: Option<String>,
}

/// The content of an extension view.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewContent {
  /// (1.7)
  pub welcome: Option<Welcome>,
  /// A count shown on its header and activity bar entry; 0 shows none.
  pub badge: u32,
  /// What its menus' `when` test.
  pub flags: Vec<String>,
  /// The text in its text box.
  pub input: String,
  pub message: Option<String>,
  pub items: Vec<ViewEntry>,
  /// A history graph beside its items, by item. (1.11)
  pub graph: Vec<GraphRow>,
  /// Shown or hidden by the extension; `None` follows the manifest. (1.13)
  pub visible: Option<bool>,
}

/// A row of a view's history graph. (1.11)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphRow {
  pub item: String,
  pub node: String,
  pub parents: Vec<String>,
  pub inputs: Vec<GraphLane>,
  pub outputs: Vec<GraphLane>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphLane {
  pub id: String,
  /// 0 to 4.
  pub color: u32,
}

/// A range decorated by an extension, in the current text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionDecoration {
  pub range: Range<usize>,
  /// The decoration type's key, `<extension id>/<name>`.
  pub key: String,
}

/// A link an extension shows above a line (a code lens), in the current text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionLens {
  /// The start of its line.
  pub offset: usize,
  pub title: String,
  pub tooltip: Option<String>,
  pub command: String,
  pub argument: Option<String>,
}

/// Faint text an extension shows after a line, in the current text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionAnnotation {
  /// The start of its line.
  pub offset: usize,
  pub text: String,
  pub tooltip: Option<String>,
}

/// Resource limits for each extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
  /// Linear memory per instance.
  pub memory_bytes: usize,
  /// Linear memory of all running extensions together.
  pub total_memory_bytes: usize,
  /// Instantiating and activating.
  pub activation_time: Duration,
  /// Running one command.
  pub command_time: Duration,
  /// Handling one document notification, and deactivating.
  pub event_time: Duration,
  /// Events waiting for an extension.
  pub queue_capacity: usize,
}

impl Default for Limits {
  fn default() -> Self {
    Self {
      memory_bytes: 128 * 1024 * 1024,
      total_memory_bytes: 512 * 1024 * 1024,
      activation_time: Duration::from_secs(5),
      command_time: Duration::from_secs(10),
      event_time: Duration::from_secs(1),
      queue_capacity: 1_024,
    }
  }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostOptions {
  /// Checked against manifests' `engines.flareite` requirement.
  pub editor_version: semver::Version,
  /// Installed extensions: each subfolder with a manifest.
  pub extensions_dir: Option<PathBuf>,
  /// Development extensions: folders with a manifest, loaded in place.
  pub development: Vec<PathBuf>,
  /// Whether development extensions' `[build]`s run: when they're loaded,
  /// and whenever a file in their folders changes.
  pub builds: bool,
  /// Where the user's permission decisions are kept.
  pub permissions_file: Option<PathBuf>,
  /// User configuration, by extension id.
  pub settings: BTreeMap<String, toml::Table>,
  /// Paths the user set for extensions' programs, by
  /// `<extension id>.<program name>`.
  pub programs: BTreeMap<String, String>,
  pub limits: Limits,
  /// Where extensions' storage and webview tabs' states are kept; `None`
  /// keeps them in memory. Read when the host is created.
  pub storage_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
  Error,
  Warning,
  Information,
  Hint,
}

impl Severity {
  /// The LSP number: 1 (error) to 4 (hint).
  pub fn number(self) -> u8 {
    match self {
      Self::Error => 1,
      Self::Warning => 2,
      Self::Information => 3,
      Self::Hint => 4,
    }
  }
}

/// A diagnostic published by an extension, in the current text.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionDiagnostic {
  pub range: Range<usize>,
  pub severity: Severity,
  pub message: String,
  pub code: Option<String>,
  /// The extension's name.
  pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionState {
  /// Loaded; starts on its activation event.
  Inactive,
  /// Waiting for the user to allow or deny requested permissions.
  AwaitingPermission,
  Starting,
  Active,
  /// Stopped after a failure (see `error`).
  Failed,
  /// Turned off by the user.
  Disabled,
  /// The manifest or folder is invalid (see `error`).
  Invalid,
  /// Contributes only data (themes, languages); there is no code to run.
  Ready,
}

/// Where a development extension's build is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BuildState {
  Building,
  Succeeded,
  Failed,
}

/// A development extension's build (its manifest's `[build]`), most
/// recently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildStatus {
  pub state: BuildState,
  /// Why it failed.
  pub error: Option<String>,
  /// How long the last finished build took.
  pub duration_ms: Option<u64>,
  /// The extension id whose log has the build's output: the manifest's id,
  /// or the folder's path when it has none.
  pub log: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionStatus {
  pub permission: Permission,
  pub description: String,
  pub granted: bool,
  /// Whether the user has decided (granted or denied).
  pub decided: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
  pub id: String,
  pub title: String,
  pub category: String,
  pub hidden: bool,
}

/// The settings an extension declares, for a settings editor.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionConfiguration {
  pub id: String,
  pub name: String,
  /// By key.
  pub settings: Vec<ConfiguredSetting>,
}

/// A declared setting and the value in effect.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredSetting {
  pub key: String,
  pub property: ConfigProperty,
  /// The user's value if it's valid, the default otherwise.
  pub value: ConfigValue,
  /// Why the user's value isn't used, if they set an invalid one.
  pub problem: Option<String>,
}

/// An extension and its state, for display.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatus {
  pub id: String,
  pub name: String,
  pub version: String,
  pub description: String,
  pub author: Option<String>,
  /// Whether it has an icon ([`ExtensionHost::icon`]).
  pub has_icon: bool,
  /// Whether it has a README ([`ExtensionHost::readme`]).
  pub has_readme: bool,
  /// The extension's folder.
  pub path: String,
  pub development: bool,
  pub state: ExtensionState,
  pub error: Option<String>,
  pub permissions: Vec<PermissionStatus>,
  pub commands: Vec<CommandInfo>,
  /// Problems with the user's configuration values.
  pub config_errors: Vec<String>,
  /// Whether it declares settings (see [`ExtensionHost::configurations`]).
  pub has_settings: bool,
  pub contributions: Contributions,
  pub usage: ResourceUsage,
  /// Bytes it stores ([`ExtensionHost::clear_storage`]).
  pub stored_bytes: u64,
  /// Its build, if it's a development extension with one
  /// ([`ExtensionHost::build_development`]).
  pub build: Option<BuildStatus>,
}
