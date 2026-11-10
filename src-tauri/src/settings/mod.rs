//! User settings, stored as TOML in the platform config directory and edited
//! in Flareite itself, as a file or in the settings editor. Saving the file
//! applies it; invalid files are reported and the previous settings stay in
//! effect. The open folder's own settings (`.flareite/settings.toml`) apply
//! over them while it's trusted; see `folder`.

pub mod folder;
mod schema;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use flareite_core::EditorConfig;
use flareite_extension_host::contributions::check_theme_colors;
use flareite_extension_host::keys::normalize as normalize_key;
use flareite_lsp::{ServerCommand, ServerConfig};
use flareite_terminal::Shell;
use flareite_terminal::profiles::{self, Profile};
use flareite_workspace::ExcludeFilter;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::editor::EditorState;
use crate::error::CommandError;
use crate::events::{self, AppEvent};
use crate::extensions::ExtensionsState;
use crate::terminal::TerminalState;

const FILE_NAME: &str = "settings.toml";

/// Written when the settings file does not exist yet. Every setting is shown
/// with its default value, commented out.
const TEMPLATE: &str = r##"# Flareite settings. Uncomment a line to change it; save to apply.

[editor]
# tab_size = 4
# insert_spaces = true
# font_size = 14
# font_family = ""          # empty: the default monospace font
# minimap_region_headers = true  # label named #region comments in the minimap
# minimap_mark_headers = true    # label MARK: comments in the minimap

[workbench]
# theme = "system"          # "system", "dark", "light", or an extension's theme id
# icon_theme = "default"    # file icons: "default" or an extension's icon theme id
# open_web_links = "auto"   # who opens Ctrl+clicked web links: "auto" (an extension's
#                           # link handler if installed, else your browser),
#                           # "default" (your browser), or a handler's id
# open_file_links = "auto"  # the same for file paths ("default": the editor)
#
# Override theme colors (hex), e.g. the editor background:
# [workbench.colors]
# editor-background = "#1b1b1f"

[files]
# Hidden from the explorer, Go to File, and search. Patterns without "/"
# match names at any depth; others match paths relative to the folder.
# exclude = [".git", ".DS_Store", "Thumbs.db"]

[search]
# max_file_bytes = 4194304  # larger files are skipped by find in files

[lsp]
# Language servers provide diagnostics, hover, go to definition, references,
# symbols, and completion. Language extensions declare them (the Rust extension declares
# rust-analyzer, for example), and they start when a file needs one, if
# installed and allowed.
# enabled = true
#
# Change an extension's server by its name, or add one for any language an
# extension adds (its id, such as rust or python):
# [lsp.servers.rust-analyzer]
# command = "rust-analyzer"
# args = []
# enabled = true
#
# A server's settings, over those its extension gives (a folder's
# .flareite/settings.toml can set these too). Dotted keys are the same as
# nested tables, as in VS Code's settings:
# [lsp.servers.luau-lsp.settings]
# "luau-lsp.require.mode" = "relativeToFile"
#
# [lsp.servers.my-server]
# command = "my-language-server"
# args = ["--stdio"]
# languages = ["python"]
# root_markers = ["pyproject.toml"]

[terminal]
# The profile new terminals use, by name. Empty: PowerShell on Windows
# (PowerShell 7 if installed), otherwise $SHELL (as a login shell on macOS).
# Installed shells are found: Windows PowerShell, Command Prompt, Git Bash,
# Cygwin, MSYS2, WSL distributions ("Ubuntu (WSL)"), and /etc/shells.
# default_profile = ""
# scrollback = 1000         # lines kept above the screen
# screen_reader = false     # announce output to screen readers (slower)
#
# Add a profile, or change or hide one that was found:
# [terminal.profiles."Nushell"]
# command = "C:/tools/nu.exe"
# args = []
# env = { EDITOR = "code --wait" }
#
# [terminal.profiles."Command Prompt"]
# enabled = false

[extensions]
# WebAssembly extensions. Installed extensions are folders in "extensions"
# next to this file. Each asks before using the permissions it requests.
# enabled = true
#
# Folders of extensions under development, loaded in place:
# development = ["C:/code/my-extension"]
#
# GitHub owners whose extensions (repositories with the "flareite-extension"
# topic) the Extensions view lists to install; [] lists none and asks GitHub
# nothing until you install one by its repository:
# catalog = ["FlareiteLabs"]
#
# Settings that extensions declare, by extension id:
# [extensions.settings."example.tidy"]
# max_line_length = 120
#
# Where to find programs extensions run, if not on the PATH:
# [extensions.programs]
# "flareite.git.git" = "C:/Program Files/Git/cmd/git.exe"

[security]
# Ask whether you trust a folder's files before running programs on it
# (language servers, and programs extensions run, such as Git).
# workspace_trust = true

[update]
# Look for a newer Flareite in its GitHub releases at startup, at most once a
# day. "Check for Updates" in the command palette works either way.
# check = true
# Which releases to offer: "stable", "prerelease" (alphas and betas too), or
# "auto" (prereleases if this build is one).
# channel = "auto"

[schemas]
# Schemas check TOML and JSON files as you edit them, and complete and
# describe their keys and values. Extensions provide schemas too.
# enabled = true
# Match well-known files (package.json, tsconfig.json, pyproject.toml, ...)
# with schemas from the SchemaStore catalog (https://www.schemastore.org):
# catalog = true
#
# Schemas for files: name patterns, or ends of paths with "/", and a TOML
# Schema (.tosd) or JSON Schema (.json) as a path (relative to this file's
# folder) or an https address. A file can also name its schema: "$schema"
# in JSON, or a "#:schema <path or address>" comment at the top of TOML.
# [[schemas.associations]]
# files = ["*.myconf.toml"]
# schema = "schemas/myconf.tosd"

[keybindings]
# Change a command's keys (find command ids in the Keyboard Shortcuts list).
# "mod" is Ctrl on Windows and Linux and Cmd on macOS; "" removes them.
# "workbench.showSearch" = "mod+shift+h"
# "file.save" = ["mod+s", "f2"]
"##;

/// Built-in theme names; other values name a theme contributed by an
/// extension (`<extension id>.<theme id>`).
pub const BUILTIN_THEMES: &[&str] = &["system", "dark", "light"];
/// The built-in file icon theme; other values name one contributed by an
/// extension (`<extension id>.<icon theme id>`).
pub const DEFAULT_ICON_THEME: &str = "default";

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
  pub editor: EditorSettings,
  pub workbench: WorkbenchSettings,
  pub files: FilesSettings,
  pub search: SearchSettings,
  pub lsp: LspSettings,
  pub terminal: TerminalSettings,
  pub extensions: ExtensionsSettings,
  pub security: SecuritySettings,
  pub update: UpdateSettings,
  pub schemas: SchemasSettings,
  /// Command id to its keys, overriding the defaults.
  pub keybindings: BTreeMap<String, KeybindingSetting>,
}

/// Schemas for TOML and JSON files (`crate::schemas`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchemasSettings {
  /// Check files against schemas, and complete their keys.
  pub enabled: bool,
  /// Match well-known files with the SchemaStore catalog's schemas.
  pub catalog: bool,
  pub associations: Vec<SchemaAssociation>,
}

impl Default for SchemasSettings {
  fn default() -> Self {
    Self {
      enabled: true,
      catalog: true,
      associations: Vec::new(),
    }
  }
}

/// A schema for files.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaAssociation {
  /// Name patterns, or ends of paths with '/'.
  pub files: Vec<String>,
  /// A path (relative to the settings file's folder; a folder's own
  /// settings: the folder) or an address.
  pub schema: String,
}

impl SchemaAssociation {
  pub fn check(&self) -> Result<(), String> {
    if self.files.is_empty() || self.files.iter().any(|pattern| pattern.trim().is_empty()) {
      return Err("schemas.associations: files must list file patterns".into());
    }
    if self.schema.trim().is_empty() {
      return Err("schemas.associations: schema must be a path or an address".into());
    }
    Ok(())
  }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecuritySettings {
  /// Ask whether to trust each folder before running programs on it.
  /// Turned off, every folder is trusted.
  pub workspace_trust: bool,
}

impl Default for SecuritySettings {
  fn default() -> Self {
    Self {
      workspace_trust: true,
    }
  }
}

/// Flareite updating itself (`app_update`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateSettings {
  /// Look for an update at startup, at most once a day.
  pub check: bool,
  pub channel: UpdateChannel,
}

impl Default for UpdateSettings {
  fn default() -> Self {
    Self {
      check: true,
      channel: UpdateChannel::Auto,
    }
  }
}

/// Which releases are offered as updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
  /// Prereleases if the running build is one, else only stable releases.
  #[default]
  Auto,
  Stable,
  /// Stable releases and prereleases.
  Prerelease,
}

/// One key or several; `""` or `[]` removes a command's keys.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum KeybindingSetting {
  One(String),
  Many(Vec<String>),
}

impl KeybindingSetting {
  fn keys(&self) -> Vec<&str> {
    match self {
      Self::One(key) if key.trim().is_empty() => Vec::new(),
      Self::One(key) => vec![key.as_str()],
      Self::Many(keys) => keys.iter().map(String::as_str).collect(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExtensionsSettings {
  pub enabled: bool,
  /// Folders of development extensions.
  pub development: Vec<String>,
  /// Values for the settings extensions declare, by extension id.
  pub settings: BTreeMap<String, toml::Table>,
  /// Paths of programs extensions run, by `<extension id>.<program>`.
  pub programs: BTreeMap<String, String>,
  /// GitHub owners whose extensions the Extensions view lists to install.
  pub catalog: Vec<String>,
}

impl Default for ExtensionsSettings {
  fn default() -> Self {
    Self {
      enabled: true,
      development: Vec::new(),
      settings: BTreeMap::new(),
      programs: BTreeMap::new(),
      catalog: vec!["FlareiteLabs".into()],
    }
  }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorSettings {
  pub tab_size: usize,
  pub insert_spaces: bool,
  pub font_size: u32,
  pub font_family: String,
  pub minimap_region_headers: bool,
  pub minimap_mark_headers: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkbenchSettings {
  pub theme: String,
  pub icon_theme: String,
  /// Theme color overrides: token to hex color.
  pub colors: BTreeMap<String, String>,
  /// Who opens web links: `auto` (an extension's link handler if one is
  /// installed, else the system's browser), `default` (the browser), or a
  /// handler's id.
  pub open_web_links: String,
  /// Who opens file links: as `open_web_links` (`default` is the editor).
  pub open_file_links: String,
}

impl Default for WorkbenchSettings {
  fn default() -> Self {
    Self {
      theme: "system".into(),
      icon_theme: DEFAULT_ICON_THEME.into(),
      colors: BTreeMap::new(),
      open_web_links: "auto".into(),
      open_file_links: "auto".into(),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilesSettings {
  pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchSettings {
  pub max_file_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
  /// A profile's name; empty for the platform's default.
  pub default_profile: String,
  pub scrollback: u32,
  pub screen_reader: bool,
  /// Added profiles, and changes to detected ones, by name.
  pub profiles: BTreeMap<String, ProfileSettings>,
}

impl Default for TerminalSettings {
  fn default() -> Self {
    Self {
      default_profile: String::new(),
      scrollback: 1000,
      screen_reader: false,
      profiles: BTreeMap::new(),
    }
  }
}

/// Changes to a detected profile, or a new one (which needs `command`).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileSettings {
  pub command: Option<String>,
  pub args: Option<Vec<String>>,
  pub env: Option<BTreeMap<String, String>>,
  pub enabled: Option<bool>,
}

/// The profiles to offer, and the one new terminals use.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalProfiles {
  pub profiles: Vec<Profile>,
  /// The default profile's name, if there is any profile.
  pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LspSettings {
  pub enabled: bool,
  pub servers: BTreeMap<String, ServerSettings>,
}

impl Default for LspSettings {
  fn default() -> Self {
    Self {
      enabled: true,
      servers: BTreeMap::new(),
    }
  }
}

/// Changes to a known server, or a new one (which needs `command` and
/// `languages`).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSettings {
  pub enabled: Option<bool>,
  pub command: Option<String>,
  pub args: Option<Vec<String>>,
  pub languages: Option<Vec<String>>,
  pub root_markers: Option<Vec<String>>,
  /// Settings over the extension's for the server: answered to its
  /// `workspace/configuration` requests.
  pub settings: Option<toml::Table>,
}

impl Default for EditorSettings {
  fn default() -> Self {
    Self {
      tab_size: 4,
      insert_spaces: true,
      font_size: 14,
      font_family: String::new(),
      minimap_region_headers: true,
      minimap_mark_headers: true,
    }
  }
}

impl Default for FilesSettings {
  fn default() -> Self {
    Self {
      exclude: vec![".git".into(), ".DS_Store".into(), "Thumbs.db".into()],
    }
  }
}

impl Default for SearchSettings {
  fn default() -> Self {
    Self {
      max_file_bytes: 4 * 1024 * 1024,
    }
  }
}

/// Settings the frontend applies itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontendSettings {
  pub theme: String,
  pub icon_theme: String,
  pub colors: BTreeMap<String, String>,
  /// Command id to normalized keys; empty removes the defaults.
  pub keybindings: BTreeMap<String, Vec<String>>,
  pub font_size: u32,
  pub font_family: String,
  pub tab_size: usize,
  pub terminal_scrollback: u32,
  pub terminal_screen_reader: bool,
  pub minimap_region_headers: bool,
  pub minimap_mark_headers: bool,
  pub open_web_links: String,
  pub open_file_links: String,
}

impl Settings {
  /// Parses and validates a settings file.
  pub fn parse(text: &str) -> Result<Self, String> {
    let settings: Self = toml::from_str(text).map_err(|error| error.to_string())?;
    settings.validate()?;
    Ok(settings)
  }

  /// Validates settings already read as TOML (such as a folder's laid over
  /// the user's).
  pub fn from_table(table: toml::Table) -> Result<Self, String> {
    let settings: Self = toml::Value::Table(table)
      .try_into()
      .map_err(|error: toml::de::Error| error.to_string())?;
    settings.validate()?;
    Ok(settings)
  }

  fn validate(&self) -> Result<(), String> {
    if !(1..=16).contains(&self.editor.tab_size) {
      return Err("editor.tab_size must be between 1 and 16".into());
    }
    if !(6..=72).contains(&self.editor.font_size) {
      return Err("editor.font_size must be between 6 and 72".into());
    }
    if self.search.max_file_bytes == 0 {
      return Err("search.max_file_bytes must be greater than 0".into());
    }
    if self.terminal.scrollback > 100_000 {
      return Err("terminal.scrollback must be at most 100000".into());
    }
    for (name, profile) in &self.terminal.profiles {
      if name.trim().is_empty() || name.chars().count() > 64 {
        return Err("terminal.profiles names must have 1 to 64 characters".into());
      }
      if profile
        .command
        .as_deref()
        .is_some_and(|command| command.trim().is_empty())
      {
        return Err(format!(
          "terminal.profiles.\"{name}\".command must not be empty"
        ));
      }
    }
    ExcludeFilter::new(&self.files.exclude).map_err(|error| format!("files.exclude: {error}"))?;
    // Entries for servers that aren't known may change an extension's
    // server, so they need not be complete (see `language_servers_with`).
    if self
      .extensions
      .development
      .iter()
      .any(|folder| folder.trim().is_empty())
    {
      return Err("extensions.development entries must be folder paths".into());
    }
    let theme = self.workbench.theme.trim();
    if theme.is_empty() || theme.len() > 128 {
      return Err(format!(
        "workbench.theme must be one of {}, or an extension's theme id",
        BUILTIN_THEMES.join(", ")
      ));
    }
    let icon_theme = self.workbench.icon_theme.trim();
    if icon_theme.is_empty() || icon_theme.len() > 128 {
      return Err(format!(
        "workbench.icon_theme must be \"{DEFAULT_ICON_THEME}\" or an extension's icon theme id"
      ));
    }
    check_theme_colors(&self.workbench.colors)
      .map_err(|error| format!("workbench.colors: {error}"))?;
    for (key, value) in [
      ("open_web_links", &self.workbench.open_web_links),
      ("open_file_links", &self.workbench.open_file_links),
    ] {
      let value = value.trim();
      if value.is_empty() || value.len() > 128 {
        return Err(format!(
          "workbench.{key} must be \"auto\", \"default\", or an extension's link handler id"
        ));
      }
    }
    for (command, setting) in &self.keybindings {
      for key in setting.keys() {
        normalize_key(key).map_err(|error| format!("keybindings.\"{command}\": {error}"))?;
      }
    }
    for association in &self.schemas.associations {
      association.check()?;
    }
    Ok(())
  }

  /// The language servers to use: those extensions declare (`contributed`)
  /// with any changes from settings, plus ones added in settings, minus
  /// disabled ones. Extensions that declare a server with the same name
  /// and command share it, serving all their languages; with another
  /// command, the first declaration wins. A settings entry for a server
  /// that doesn't exist is ignored unless it has a command and languages.
  pub fn language_servers_with(&self, contributed: Vec<ServerConfig>) -> Vec<ServerConfig> {
    if !self.lsp.enabled {
      return Vec::new();
    }
    let mut servers: Vec<ServerConfig> = Vec::new();
    for server in contributed {
      match servers.iter_mut().find(|known| known.name == server.name) {
        None => servers.push(server),
        Some(known) if known.command == server.command => {
          for language in server.languages {
            if !known.languages.contains(&language) {
              known.languages.push(language);
            }
          }
        }
        Some(_) => tracing::warn!(
          server = %server.name,
          "two extensions declare this language server with different commands; using the first"
        ),
      }
    }
    for (name, settings) in &self.lsp.servers {
      let index = match servers.iter().position(|server| server.name == *name) {
        Some(index) => index,
        None if settings.command.is_none() || settings.languages.is_none() => {
          if settings.enabled != Some(false) {
            tracing::warn!(
              server = %name,
              "lsp.servers.{name} names no extension's server, and has no command and languages"
            );
          }
          continue;
        }
        None => {
          servers.push(ServerConfig {
            name: name.clone(),
            command: ServerCommand {
              program: String::new(),
              args: Vec::new(),
            },
            languages: Vec::new(),
            root_markers: Vec::new(),
            settings: serde_json::Value::Null,
          });
          servers.len() - 1
        }
      };
      let server = &mut servers[index];
      if let Some(command) = &settings.command {
        server.command.program = command.clone();
      }
      if let Some(args) = &settings.args {
        server.command.args = args.clone();
      }
      if let Some(languages) = &settings.languages {
        server.languages = languages.clone();
      }
      if let Some(markers) = &settings.root_markers {
        server.root_markers = markers.clone();
      }
      if let Some(user) = &settings.settings {
        let user = flareite_schema::document::toml_to_json(&toml::Value::Table(user.clone()));
        let mut merged = flareite_lsp::session::expand(&server.settings);
        if merged.is_null() {
          merged = serde_json::Value::Object(serde_json::Map::new());
        }
        flareite_lsp::session::merge(&mut merged, flareite_lsp::session::expand(&user));
        server.settings = merged;
      }
    }
    servers
      .retain(|server| self.lsp.servers.get(&server.name).and_then(|s| s.enabled) != Some(false));
    servers
  }

  pub fn exclude_filter(&self) -> ExcludeFilter {
    // Validated on load, so building cannot fail; fall back to no exclusions.
    ExcludeFilter::new(&self.files.exclude).unwrap_or_default()
  }

  /// The detected profiles with the user's changes: changed or hidden by
  /// name, and added ones after them. Added profiles need a `command`;
  /// changes to profiles that weren't found are ignored.
  pub fn terminal_profiles(&self, detected: Vec<Profile>) -> TerminalProfiles {
    let mut profiles: Vec<Profile> = detected
      .into_iter()
      .filter_map(|mut profile| {
        let Some(change) = self.terminal.profiles.get(&profile.name) else {
          return Some(profile);
        };
        if change.enabled == Some(false) {
          return None;
        }
        apply_profile_change(&mut profile, change);
        Some(profile)
      })
      .collect();
    for (name, change) in &self.terminal.profiles {
      let new = !profiles.iter().any(|profile| profile.name == *name);
      if new && change.enabled != Some(false) && change.command.is_some() {
        let mut profile = Profile {
          name: name.clone(),
          shell: Shell {
            program: String::new(),
            args: Vec::new(),
          },
          env: Vec::new(),
        };
        apply_profile_change(&mut profile, change);
        profiles.push(profile);
      }
    }
    let wanted = self.terminal.default_profile.trim();
    let chosen = profiles
      .iter()
      .find(|profile| profile.name == wanted)
      .or_else(|| {
        profiles
          .iter()
          .find(|profile| profile.name.eq_ignore_ascii_case(wanted))
      })
      .or_else(|| profiles::default_profile(&profiles, &profiles::System));
    TerminalProfiles {
      default: chosen.map(|profile| profile.name.clone()),
      profiles,
    }
  }

  pub fn editor_config(&self) -> EditorConfig {
    EditorConfig {
      tab_size: self.editor.tab_size,
      insert_spaces: self.editor.insert_spaces,
      ..EditorConfig::default()
    }
  }

  pub fn frontend(&self) -> FrontendSettings {
    FrontendSettings {
      theme: self.workbench.theme.trim().to_owned(),
      icon_theme: self.workbench.icon_theme.trim().to_owned(),
      // Validated on load.
      colors: check_theme_colors(&self.workbench.colors).unwrap_or_default(),
      keybindings: self
        .keybindings
        .iter()
        .map(|(command, setting)| {
          let keys = setting
            .keys()
            .into_iter()
            .filter_map(|key| normalize_key(key).ok())
            .collect();
          (command.clone(), keys)
        })
        .collect(),
      font_size: self.editor.font_size,
      font_family: self.editor.font_family.clone(),
      tab_size: self.editor.tab_size,
      terminal_scrollback: self.terminal.scrollback,
      terminal_screen_reader: self.terminal.screen_reader,
      minimap_region_headers: self.editor.minimap_region_headers,
      minimap_mark_headers: self.editor.minimap_mark_headers,
      open_web_links: self.workbench.open_web_links.trim().to_owned(),
      open_file_links: self.workbench.open_file_links.trim().to_owned(),
    }
  }
}

pub struct SettingsState {
  path: Option<PathBuf>,
  /// The settings in effect: the user's, with the open folder's over them.
  current: RwLock<Settings>,
  /// The user's settings, and their file's values.
  user: RwLock<(Settings, toml::Table)>,
  /// The open folder's settings, if a folder is open.
  folder: RwLock<Option<folder::FolderSettings>>,
}

impl SettingsState {
  /// Loads settings from the config directory. Problems are logged and the
  /// defaults are used.
  pub fn load(config_dir: Option<PathBuf>) -> Self {
    let path = config_dir.map(|dir| dir.join(FILE_NAME));
    let user = match path.as_deref().map(std::fs::read_to_string) {
      Some(Ok(text)) => match Settings::parse(&text) {
        Ok(settings) => (settings, text.parse().unwrap_or_default()),
        Err(error) => {
          tracing::warn!(%error, "invalid settings file; using defaults");
          (Settings::default(), toml::Table::new())
        }
      },
      Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
        tracing::warn!(%error, "could not read settings; using defaults");
        (Settings::default(), toml::Table::new())
      }
      _ => (Settings::default(), toml::Table::new()),
    };
    Self {
      path,
      current: RwLock::new(user.0.clone()),
      user: RwLock::new(user),
      folder: RwLock::new(None),
    }
  }

  /// The settings in effect.
  pub fn get(&self) -> Settings {
    self
      .current
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  /// The user's settings, without the folder's.
  pub fn user(&self) -> Settings {
    self
      .user
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .0
      .clone()
  }

  /// The open folder's settings, if a folder is open.
  pub fn folder(&self) -> Option<folder::FolderSettings> {
    self
      .folder
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  pub fn path(&self) -> Option<&Path> {
    self.path.as_deref()
  }

  /// Makes `user` and `folder` the settings (each if given), and returns
  /// the ones now in effect. A folder whose settings can't be laid over the
  /// user's gets the reason as its error.
  fn replace(
    &self,
    user: Option<(Settings, toml::Table)>,
    folder: Option<Option<folder::FolderSettings>>,
  ) -> Settings {
    let mut user_guard = self.user.write().unwrap_or_else(PoisonError::into_inner);
    if let Some(user) = user {
      *user_guard = user;
    }
    let mut folder_guard = self.folder.write().unwrap_or_else(PoisonError::into_inner);
    if let Some(folder) = folder {
      *folder_guard = folder;
    }
    let effective = match folder_guard.as_mut() {
      Some(folder) if folder.in_effect() && !folder.applied.is_empty() => {
        match Settings::from_table(folder::merged(&user_guard.1, &folder.applied)) {
          Ok(settings) => settings,
          Err(error) => {
            folder.error = Some(error);
            user_guard.0.clone()
          }
        }
      }
      _ => user_guard.0.clone(),
    };
    *self.current.write().unwrap_or_else(PoisonError::into_inner) = effective.clone();
    effective
  }
}

fn apply_profile_change(profile: &mut Profile, change: &ProfileSettings) {
  if let Some(command) = &change.command {
    profile.shell.program = command.trim().to_owned();
  }
  if let Some(args) = &change.args {
    profile.shell.args.clone_from(args);
  }
  if let Some(env) = &change.env {
    for (key, value) in env {
      profile.env.retain(|(existing, _)| existing != key);
      profile.env.push((key.clone(), value.clone()));
    }
  }
}

/// Re-reads the settings file after it was saved and applies it everywhere.
/// Returns an error message for the user if the file is invalid.
pub fn reload(app: &AppHandle) -> Result<(), CommandError> {
  reload_and_announce(app, true)
}

/// Re-reads and applies the settings file; `announce` tells the user it was
/// applied (not needed when they changed one setting and can see it).
fn reload_and_announce(app: &AppHandle, announce: bool) -> Result<(), CommandError> {
  let state = app.state::<SettingsState>();
  let Some(path) = state.path() else {
    return Ok(());
  };
  let text = std::fs::read_to_string(path)
    .map_err(|error| CommandError::new("settings", format!("Could not read settings: {error}")))?;
  let settings = Settings::parse(&text).map_err(|error| {
    CommandError::new("invalidSettings", format!("Settings not applied: {error}"))
  })?;
  let table = text.parse().unwrap_or_default();
  let previous = state.get();
  let settings = state.replace(Some((settings, table)), None);
  apply(app, &previous, &settings, announce);
  tracing::info!("settings applied");
  Ok(())
}

/// Reads the open folder's settings again (after the folder, its trust, or
/// its settings file changed) and applies the result. With `announce`, the
/// user is told, and an invalid file is an error.
pub fn folder_changed(app: &AppHandle, announce: bool) -> Result<(), CommandError> {
  let state = app.state::<SettingsState>();
  let root = app.state::<crate::workspace::WorkspaceState>().root();
  let trusted = crate::trust::workspace_trusted(app);
  let folder = root.map(|root| folder::FolderSettings::read(&root, trusted));
  if state.folder() == folder && !announce {
    return Ok(());
  }
  let previous = state.get();
  let settings = state.replace(None, Some(folder));
  apply(app, &previous, &settings, announce);
  crate::schemas::settings_changed(app);
  match state.folder() {
    Some(folder) if announce && folder.exists => match folder.error {
      Some(error) => Err(CommandError::new(
        "invalidSettings",
        format!("Folder settings not applied: {error}"),
      )),
      None if !folder.trusted => Err(CommandError::new(
        "untrustedFolder",
        "Folder settings apply once you trust the folder.",
      )),
      None => Ok(()),
    },
    _ => Ok(()),
  }
}

/// Applies a settings file that was saved: the user's, or the open
/// folder's. Returns an error message for the user if it's invalid.
pub fn saved(app: &AppHandle, path: &Path) -> Result<(), CommandError> {
  if is_settings_file(app, path) {
    return reload(app);
  }
  if is_folder_settings_file(app, path) {
    return folder_changed(app, true);
  }
  Ok(())
}

/// Whether `path` is the open folder's settings file.
pub fn is_folder_settings_file(app: &AppHandle, path: &Path) -> bool {
  app
    .state::<crate::workspace::WorkspaceState>()
    .root()
    .is_some_and(|root| crate::paths::normalize(&folder::file_of(&root)) == path)
}

fn apply(app: &AppHandle, previous: &Settings, settings: &Settings, announce: bool) {
  app
    .state::<EditorState>()
    .lock()
    .set_config(settings.editor_config());
  if previous.files.exclude != settings.files.exclude {
    crate::workspace::exclusions_changed(app);
  }
  if previous.lsp != settings.lsp {
    // Restarts the servers whose commands changed; others get their new
    // settings.
    app.state::<crate::lsp::LspState>().configure(app);
  }
  crate::extensions::settings_changed(app, previous, settings);
  if previous.security != settings.security {
    crate::trust::changed(app);
  }
  if previous.schemas != settings.schemas {
    crate::schemas::settings_changed(app);
  }
  events::emit(
    app,
    AppEvent::SettingsChanged {
      settings: settings.frontend(),
      announce,
    },
  );
}

/// Whether `path` is the settings file.
pub fn is_settings_file(app: &AppHandle, path: &Path) -> bool {
  app
    .state::<SettingsState>()
    .path()
    .is_some_and(|settings| crate::paths::normalize(settings) == path)
}

#[tauri::command]
pub fn settings_get(state: State<'_, SettingsState>) -> FrontendSettings {
  state.get().frontend()
}

/// Why the settings file isn't in effect, if it isn't: it can't be read, or
/// it's invalid. `None` when it's fine or doesn't exist yet.
#[tauri::command]
pub fn settings_problem(state: State<'_, SettingsState>) -> Option<String> {
  match std::fs::read_to_string(state.path()?) {
    Ok(text) => Settings::parse(&text).err(),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
    Err(error) => Some(format!("Could not read settings: {error}")),
  }
}

/// The settings file's path, creating it from the template if needed.
#[tauri::command]
pub fn settings_file(state: State<'_, SettingsState>) -> Result<String, CommandError> {
  let path = state
    .path()
    .ok_or_else(|| CommandError::new("settings", "No configuration directory is available."))?;
  if !path.exists() {
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir).map_err(|error| {
        CommandError::new(
          "settings",
          format!("Could not create {}: {error}", dir.display()),
        )
      })?;
    }
    std::fs::write(path, TEMPLATE).map_err(|error| {
      CommandError::new("settings", format!("Could not create settings: {error}"))
    })?;
  }
  Ok(path.display().to_string())
}

/// Sets `workbench.theme` in the settings file, keeping its comments and
/// formatting, and applies it.
#[tauri::command]
pub fn settings_set_theme(app: AppHandle, theme: String) -> Result<(), CommandError> {
  let name = theme.as_str();
  edit(&app, |document| {
    if !document.contains_table("workbench") {
      document["workbench"] = toml_edit::table();
    }
    document["workbench"]["theme"] = toml_edit::value(name);
    Ok(())
  })
}

/// Sets `terminal.default_profile` in the settings file, keeping its
/// comments and formatting, and applies it.
#[tauri::command]
pub fn settings_set_default_profile(app: AppHandle, profile: String) -> Result<(), CommandError> {
  let name = profile.as_str();
  edit(&app, |document| {
    if !document.contains_table("terminal") {
      document["terminal"] = toml_edit::table();
    }
    document["terminal"]["default_profile"] = toml_edit::value(name);
    Ok(())
  })
}

/// Sets `workbench.icon_theme` in the settings file, keeping its comments
/// and formatting, and applies it.
#[tauri::command]
pub fn settings_set_icon_theme(app: AppHandle, icon_theme: String) -> Result<(), CommandError> {
  let name = icon_theme.as_str();
  edit(&app, |document| {
    if !document.contains_table("workbench") {
      document["workbench"] = toml_edit::table();
    }
    document["workbench"]["icon_theme"] = toml_edit::value(name);
    Ok(())
  })
}

/// The open folder's settings file, created (with a comment saying what
/// it's for) if needed.
#[tauri::command]
pub fn settings_folder_file(app: AppHandle) -> Result<String, CommandError> {
  let root = app
    .state::<crate::workspace::WorkspaceState>()
    .root()
    .ok_or_else(|| CommandError::new("noWorkspace", "No folder is open."))?;
  let path = folder::file_of(&root);
  if !path.exists() {
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir).map_err(|error| {
        CommandError::new(
          "settings",
          format!("Could not create {}: {error}", dir.display()),
        )
      })?;
    }
    flareite_workspace::write_file_atomically(&path, folder::TEMPLATE.as_bytes()).map_err(
      |error| {
        CommandError::new(
          "settings",
          format!("Could not create the folder's settings: {error}"),
        )
      },
    )?;
  }
  Ok(path.display().to_string())
}

/// Changes the open folder's settings file with `change`, as [`edit`]
/// changes the user's, and applies the result.
fn edit_folder(
  app: &AppHandle,
  change: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>,
) -> Result<(), CommandError> {
  let path = PathBuf::from(settings_folder_file(app.clone())?);
  let text = std::fs::read_to_string(&path).map_err(|error| {
    CommandError::new(
      "settings",
      format!("Could not read the folder's settings: {error}"),
    )
  })?;
  let mut document: toml_edit::DocumentMut = text.parse().map_err(|error| {
    CommandError::new(
      "invalidSettings",
      format!("The folder's settings file is invalid: {error}"),
    )
  })?;
  change(&mut document).map_err(|message| CommandError::new("invalidSettings", message))?;
  let text = document.to_string();
  let root = path
    .parent()
    .and_then(Path::parent)
    .map(Path::to_path_buf)
    .unwrap_or_default();
  let mut checked = folder::FolderSettings::read(&root, true);
  checked.exists = true;
  checked.load(&text);
  if let Some(error) = checked.error {
    return Err(CommandError::new(
      "invalidSettings",
      format!("Folder settings not changed: {error}"),
    ));
  }
  flareite_workspace::write_file_atomically(&path, text.as_bytes()).map_err(|error| {
    CommandError::new(
      "settings",
      format!("Could not write the folder's settings: {error}"),
    )
  })?;
  folder_changed(app, false)
}

/// Changes the settings file with `change`, keeping its comments and
/// formatting, and applies the result. Nothing is written if the result
/// would be invalid.
pub fn edit(
  app: &AppHandle,
  change: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>,
) -> Result<(), CommandError> {
  let path = PathBuf::from(settings_file(app.state())?);
  let text = std::fs::read_to_string(&path)
    .map_err(|error| CommandError::new("settings", format!("Could not read settings: {error}")))?;
  let mut document: toml_edit::DocumentMut = text.parse().map_err(|error| {
    CommandError::new(
      "invalidSettings",
      format!("Settings file is invalid: {error}"),
    )
  })?;
  change(&mut document).map_err(|message| CommandError::new("invalidSettings", message))?;
  let text = document.to_string();
  Settings::parse(&text).map_err(|error| {
    CommandError::new("invalidSettings", format!("Settings not changed: {error}"))
  })?;
  flareite_workspace::write_file_atomically(&path, text.as_bytes())
    .map_err(|error| CommandError::new("settings", format!("Could not write settings: {error}")))?;
  reload_and_announce(app, false)
}

/// Sets one of an extension's settings (`None` removes it), as the
/// extension asked (`host::set-configuration`); the value was checked
/// against its declaration.
pub fn set_extension_setting(
  app: &AppHandle,
  extension: &str,
  key: &str,
  value: Option<toml::Value>,
) -> Result<(), CommandError> {
  let value = match value {
    None => None,
    Some(toml::Value::Boolean(b)) => Some(toml_edit::Value::from(b)),
    Some(toml::Value::Integer(i)) => Some(toml_edit::Value::from(i)),
    Some(toml::Value::Float(f)) => Some(toml_edit::Value::from(f)),
    Some(toml::Value::String(s)) => Some(toml_edit::Value::from(s)),
    Some(other) => {
      return Err(CommandError::new(
        "invalidSettings",
        format!("{other} isn't a setting's value"),
      ));
    }
  };
  let path = ["extensions", "settings", extension, key].map(str::to_owned);
  edit(app, |document| set_value(document, &path, value))
}

/// Sets the value at `path` (`None` removes it), creating the tables on the
/// way. A replaced value keeps its comments.
fn set_value(
  document: &mut toml_edit::DocumentMut,
  path: &[String],
  value: Option<toml_edit::Value>,
) -> Result<(), String> {
  let (key, tables) = path.split_last().ok_or("no setting was named")?;
  let mut table: &mut dyn toml_edit::TableLike = document.as_table_mut();
  for (depth, name) in tables.iter().enumerate() {
    if value.is_none() && !table.contains_key(name) {
      return Ok(());
    }
    let item = table.entry(name).or_insert_with(|| {
      let mut new = toml_edit::Table::new();
      // Only the innermost table gets a header.
      new.set_implicit(depth + 1 < tables.len());
      toml_edit::Item::Table(new)
    });
    table = item
      .as_table_like_mut()
      .ok_or_else(|| format!("{} must be a table", tables[..=depth].join(".")))?;
  }
  match value {
    Some(mut value) => {
      if let Some(old) = table.get(key).and_then(toml_edit::Item::as_value) {
        *value.decor_mut() = old.decor().clone();
      }
      table.insert(key, toml_edit::Item::Value(value));
    }
    None => {
      table.remove(key);
      prune_empty_tables(document, tables);
    }
  }
  Ok(())
}

/// Removes the tables on `path` that removing a key left empty, innermost
/// first, such as `[extensions.settings."example.tidy"]`. Top-level tables
/// (the template's sections) and tables with comments stay.
fn prune_empty_tables(document: &mut toml_edit::DocumentMut, path: &[String]) {
  for depth in (1..path.len()).rev() {
    let mut parent: &mut dyn toml_edit::TableLike = document.as_table_mut();
    for name in &path[..depth] {
      let Some(table) = parent
        .get_mut(name)
        .and_then(toml_edit::Item::as_table_like_mut)
      else {
        return;
      };
      parent = table;
    }
    let name = &path[depth];
    let Some(item) = parent.get(name) else {
      return;
    };
    let empty = item
      .as_table_like()
      .is_some_and(toml_edit::TableLike::is_empty);
    let commented = item.as_table().is_some_and(|table| {
      table
        .decor()
        .prefix()
        .and_then(toml_edit::RawString::as_str)
        .is_some_and(|prefix| prefix.contains('#'))
    });
    if !empty || commented {
      return;
    }
    parent.remove(name);
  }
}

/// Every setting for the settings editor, with what `settings.toml` sets.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSchemaDto {
  /// The settings file, if there's a configuration directory.
  file: Option<String>,
  /// Why the settings file isn't in effect, if it's invalid.
  error: Option<String>,
  settings: Vec<schema::SettingDto>,
  /// The open folder's settings, if a folder is open.
  folder: Option<FolderDto>,
}

/// The open folder's settings, for the settings editor's Folder tab.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderDto {
  /// The folder's name.
  name: String,
  /// Its settings file.
  file: String,
  exists: bool,
  trusted: bool,
  /// Why its settings don't apply, if they can't.
  error: Option<String>,
  /// Settings it sets that only the user's settings can.
  ignored: Vec<String>,
}

/// Which settings file the settings editor shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
  /// The user's `settings.toml`.
  #[default]
  User,
  /// The open folder's `.flareite/settings.toml`.
  Workspace,
}

/// Every built-in setting and every setting extensions declare, with its
/// description, default, and value: as the user's settings set it, or (for
/// `Scope::Workspace`) as it is in effect in the open folder.
#[tauri::command]
pub async fn settings_schema(
  app: AppHandle,
  scope: Option<Scope>,
) -> Result<SettingsSchemaDto, CommandError> {
  let scope = scope.unwrap_or_default();
  // Finding terminal profiles can take a moment.
  tauri::async_runtime::spawn_blocking(move || {
    let state = app.state::<SettingsState>();
    let folder = state.folder();
    let in_folder = scope == Scope::Workspace && folder.is_some();
    let settings = if in_folder { state.get() } else { state.user() };
    let text = state.path().map(std::fs::read_to_string);
    let (mut file, error) = match &text {
      None => (None, None),
      Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
      Some(Err(error)) => (None, Some(format!("Could not read settings: {error}"))),
      Some(Ok(text)) => (
        toml::from_str::<toml::Table>(text).ok(),
        Settings::parse(text).err(),
      ),
    };
    if in_folder && let Some(folder) = &folder {
      file = Some(folder.table.clone());
    }
    let installed = installed(&app, &settings);
    let mut described = schema::describe(&settings, file.as_ref(), &installed);
    for setting in &mut described {
      let keys: Vec<&str> = setting.path.iter().map(String::as_str).collect();
      setting.folder = folder::applies(&keys);
      if in_folder && setting.modified && !setting.folder {
        setting.problem =
          Some("Only your own settings can change this; the folder's value is ignored.".into());
      }
    }
    let folder = folder.map(|folder| FolderDto {
      name: folder.root.file_name().map_or_else(
        || folder.root.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
      ),
      file: folder.path.display().to_string(),
      exists: folder.exists,
      trusted: folder.trusted,
      error: folder.error,
      ignored: folder.ignored,
    });
    SettingsSchemaDto {
      file: state.path().map(|path| path.display().to_string()),
      error,
      settings: described,
      folder,
    }
  })
  .await
  .map_err(CommandError::internal)
}

/// What's installed that settings name: themes, icon themes, link
/// handlers, terminal profiles (with `settings`' changes), and extensions'
/// settings. Finding terminal profiles can take a moment.
fn installed(app: &AppHandle, settings: &Settings) -> schema::Installed {
  let mut installed = schema::Installed::default();
  if let Some(host) = app.state::<ExtensionsState>().host() {
    for status in host.statuses() {
      if matches!(
        status.state,
        flareite_extension_host::ExtensionState::Disabled
          | flareite_extension_host::ExtensionState::Invalid
      ) {
        continue;
      }
      let contributions = status.contributions;
      installed.themes.extend(
        contributions
          .themes
          .into_iter()
          .map(|theme| schema::Choice::new(theme.id, theme.label)),
      );
      installed.icon_themes.extend(
        contributions
          .icon_themes
          .into_iter()
          .map(|theme| schema::Choice::new(theme.id, theme.label)),
      );
      for handler in contributions.link_handlers {
        let choice = schema::Choice::new(handler.full_id, handler.title);
        match handler.kind {
          flareite_extension_host::contributions::LinkKind::Web => {
            installed.web_link_handlers.push(choice);
          }
          flareite_extension_host::contributions::LinkKind::File => {
            installed.file_link_handlers.push(choice);
          }
        }
      }
    }
    installed.extensions = host.configurations();
  }
  let detected = app.state::<TerminalState>().detected_profiles();
  // What "automatic" picks: the default without the user's choice.
  let mut automatic = settings.clone();
  automatic.terminal.default_profile.clear();
  installed.default_profile = automatic.terminal_profiles(detected.clone()).default;
  installed.profiles = settings
    .terminal_profiles(detected)
    .profiles
    .into_iter()
    .map(|profile| profile.name)
    .collect();
  installed
}

/// Values to suggest for the setting at `path` (its keys) when it's typed
/// in a settings file, with their labels: what's installed for settings
/// that name it.
pub fn value_suggestions(app: &AppHandle, path: &[String]) -> Vec<(String, String)> {
  let keys: Vec<&str> = path.iter().map(String::as_str).collect();
  // The languages a language server serves.
  if let ["lsp", "servers", _, "languages"] = keys.as_slice() {
    return crate::languages::languages_list(app.state())
      .into_iter()
      .map(|language| (language.id, language.name))
      .collect();
  }
  if !schema::has_choices(&keys) {
    return Vec::new();
  }
  let settings = app.state::<SettingsState>().get();
  schema::suggestions(&keys, &installed(app, &settings))
}

/// Sets one setting in the settings file from the settings editor, keeping
/// the file's comments and formatting, and applies it. `path` is the
/// setting's keys; `None` removes it, so its default applies.
#[tauri::command]
pub fn settings_set(
  app: AppHandle,
  path: Vec<String>,
  value: Option<serde_json::Value>,
  scope: Option<Scope>,
) -> Result<(), CommandError> {
  let name = path.join(".");
  let invalid = |message: String| CommandError::new("invalidSetting", message);
  let scope = scope.unwrap_or_default();
  if scope == Scope::Workspace {
    let keys: Vec<&str> = path.iter().map(String::as_str).collect();
    if !folder::applies(&keys) {
      return Err(invalid(format!(
        "{name} can only be changed in your own settings"
      )));
    }
  }
  let value = match path.as_slice() {
    [extensions, settings, id, key] if extensions == "extensions" && settings == "settings" => {
      let property = app
        .state::<ExtensionsState>()
        .host()
        .and_then(|host| {
          host
            .configurations()
            .into_iter()
            .find(|extension| extension.id == *id)?
            .settings
            .into_iter()
            .find(|setting| setting.key == *key)
        })
        .map(|setting| setting.property)
        .ok_or_else(|| invalid(format!("{name} is not a setting of an extension")))?;
      value
        .map(|value| {
          let converted = schema::to_toml(schema::config_kind(property.kind), &value)?;
          property.check(&converted)?;
          Ok(converted)
        })
        .transpose()
        .map_err(|error: String| invalid(format!("{name} {error}")))?
    }
    _ => {
      let kind = schema::builtin_kind(&path).ok_or_else(|| {
        invalid(format!(
          "{name} can't be set here; change it in settings.toml"
        ))
      })?;
      value
        .map(|value| schema::to_toml(kind, &value))
        .transpose()
        .map_err(|error| invalid(format!("{name} {error}")))?
    }
  };
  let value = value.as_ref().map(schema::to_edit);
  match scope {
    Scope::User => edit(&app, |document| set_value(document, &path, value)),
    Scope::Workspace => edit_folder(&app, |document| set_value(document, &path, value)),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn set(text: &str, path: &[&str], value: Option<toml_edit::Value>) -> String {
    let mut document: toml_edit::DocumentMut = text.parse().expect("toml");
    let path: Vec<String> = path.iter().map(|&key| key.to_owned()).collect();
    set_value(&mut document, &path, value).expect("set");
    document.to_string()
  }

  #[test]
  fn sets_and_removes_values_keeping_the_file_as_written() {
    let changed = set(TEMPLATE, &["editor", "tab_size"], Some(2.into()));
    assert!(
      changed.contains("[editor]\ntab_size = 2\n# tab_size = 4"),
      "{changed}"
    );
    assert_eq!(Settings::parse(&changed).expect("valid").editor.tab_size, 2);

    // A replaced value keeps its comment; removing it brings the default back.
    let commented = "[editor]\ntab_size = 2 # narrow\n";
    assert_eq!(
      set(commented, &["editor", "tab_size"], Some(8.into())),
      "[editor]\ntab_size = 8 # narrow\n"
    );
    assert_eq!(set(commented, &["editor", "tab_size"], None), "[editor]\n");
    assert_eq!(set("", &["editor", "tab_size"], None), "");

    // Extension settings go in their own table, after [extensions].
    let tidy = set(
      TEMPLATE,
      &["extensions", "settings", "example.tidy", "max_line_length"],
      Some(80.into()),
    );
    let at = |needle: &str| {
      tidy
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} in {tidy}"))
    };
    assert!(
      at("[extensions]") < at("[extensions.settings.\"example.tidy\"]\nmax_line_length = 80")
    );
    assert!(at("max_line_length = 80") < at("[keybindings]"));
    assert!(!tidy.contains("[extensions.settings]\n"));
    let parsed = Settings::parse(&tidy).expect("valid");
    assert_eq!(
      parsed.extensions.settings["example.tidy"]["max_line_length"].as_integer(),
      Some(80)
    );
    // Resetting it leaves the file as it was, without an empty table.
    let path = ["extensions", "settings", "example.tidy", "max_line_length"];
    assert_eq!(set(&tidy, &path, None), TEMPLATE);
    // Tables that hold other settings, or comments, stay.
    let two = "[extensions.settings.\"x\"]\na = 1\nb = 2\n";
    assert_eq!(
      set(two, &["extensions", "settings", "x", "a"], None),
      "[extensions.settings.\"x\"]\nb = 2\n"
    );
    let commented = "# Tidy\n[extensions.settings.\"x\"]\na = 1\n";
    assert_eq!(
      set(commented, &["extensions", "settings", "x", "a"], None),
      "# Tidy\n[extensions.settings.\"x\"]\n"
    );

    // A table may also be written inline.
    let inline = set(
      "extensions = { settings = { a = { b = 1 } } }\n",
      &["extensions", "settings", "a", "b"],
      Some(2.into()),
    );
    assert_eq!(inline, "extensions = { settings = { a = { b = 2 } } }\n");

    let mut document: toml_edit::DocumentMut = "editor = 3\n".parse().expect("toml");
    let error = set_value(
      &mut document,
      &["editor".to_owned(), "tab_size".to_owned()],
      Some(2.into()),
    )
    .expect_err("not a table");
    assert!(error.contains("editor must be a table"), "{error}");
  }

  #[test]
  fn the_template_parses_to_the_defaults() {
    assert_eq!(Settings::parse(TEMPLATE), Ok(Settings::default()));
  }

  #[test]
  fn parses_partial_settings_over_defaults() {
    let settings =
      Settings::parse("[editor]\ntab_size = 2\n[workbench]\ntheme = \"light\"\n").expect("valid");
    assert_eq!(settings.editor.tab_size, 2);
    assert!(settings.editor.insert_spaces);
    assert_eq!(settings.workbench.theme, "light");
    assert_eq!(settings.files.exclude, FilesSettings::default().exclude);
  }

  #[test]
  fn rejects_invalid_settings_with_a_message() {
    let unknown = Settings::parse("[editor]\ntab_width = 2\n").expect_err("unknown key");
    assert!(unknown.contains("tab_width"), "{unknown}");
    let range = Settings::parse("[editor]\ntab_size = 0\n").expect_err("out of range");
    assert!(range.contains("tab_size"));
    let theme = Settings::parse("[workbench]\ntheme = \"\"\n").expect_err("empty theme");
    assert!(theme.contains("workbench.theme"), "{theme}");
    let icons = Settings::parse("[workbench]\nicon_theme = \" \"\n").expect_err("empty");
    assert!(icons.contains("workbench.icon_theme"), "{icons}");
    let color = Settings::parse("[workbench.colors]\ntext = \"red\"\n").expect_err("bad color");
    assert!(color.contains("workbench.colors"), "{color}");
    let key = Settings::parse("[keybindings]\n\"file.save\" = \"hyper+s\"\n").expect_err("bad key");
    assert!(key.contains("file.save"), "{key}");
    let glob = Settings::parse("[files]\nexclude = [\"[x\"]\n").expect_err("bad glob");
    assert!(glob.contains("files.exclude"));
    // May change an extension's server; ignored if there isn't one.
    let partial = Settings::parse("[lsp.servers.mine]\ncommand = \"x\"\n").expect("valid");
    assert!(
      partial
        .language_servers_with(Vec::new())
        .iter()
        .all(|server| server.name != "mine")
    );
    let scrollback =
      Settings::parse("[terminal]\nscrollback = 1000000\n").expect_err("too much scrollback");
    assert!(scrollback.contains("terminal.scrollback"), "{scrollback}");
    let command =
      Settings::parse("[terminal.profiles.x]\ncommand = \" \"\n").expect_err("empty command");
    assert!(command.contains("terminal.profiles"), "{command}");
  }

  fn profile(name: &str, program: &str) -> Profile {
    Profile {
      name: name.into(),
      shell: Shell {
        program: program.into(),
        args: Vec::new(),
      },
      env: Vec::new(),
    }
  }

  #[test]
  fn combines_detected_and_configured_terminal_profiles() {
    let detected = || {
      vec![
        profile("Windows PowerShell", "powershell.exe"),
        profile("Command Prompt", "cmd.exe"),
        profile("Git Bash", "bash.exe"),
      ]
    };
    let settings = Settings::parse(
      r#"
[terminal]
default_profile = "git bash"
[terminal.profiles."Command Prompt"]
enabled = false
[terminal.profiles."Git Bash"]
args = ["--login"]
env = { CHERE_INVOKING = "1" }
[terminal.profiles."Nushell"]
command = " C:/tools/nu.exe "
[terminal.profiles."Not installed"]
args = ["-x"]
"#,
    )
    .expect("valid");
    let result = settings.terminal_profiles(detected());
    let names: Vec<&str> = result.profiles.iter().map(|p| p.name.as_str()).collect();
    // Hidden, changed, added; a change without a command adds nothing.
    assert_eq!(names, ["Windows PowerShell", "Git Bash", "Nushell"]);
    assert_eq!(result.profiles[1].shell.args, ["--login"]);
    assert_eq!(
      result.profiles[1].env,
      [("CHERE_INVOKING".into(), "1".into())]
    );
    assert_eq!(result.profiles[2].shell.program, "C:/tools/nu.exe");
    // The default is matched without regard to case.
    assert_eq!(result.default.as_deref(), Some("Git Bash"));

    // An unknown default falls back to the platform's.
    let unknown = Settings::parse("[terminal]\ndefault_profile = \"Fish\"\n").expect("valid");
    assert!(unknown.terminal_profiles(detected()).default.is_some());
    assert_eq!(
      Settings::default().terminal_profiles(Vec::new()),
      TerminalProfiles {
        profiles: Vec::new(),
        default: None
      }
    );
  }

  #[test]
  fn keybindings_and_colors_reach_the_frontend_normalized() {
    let settings = Settings::parse(
      r##"
[workbench]
theme = "example.themes.night"
icon_theme = " example.icons.colorful "
[workbench.colors]
editor-background = "#ABCDEF"
[keybindings]
"file.save" = ["Ctrl+S", "f2"]
"workbench.showSearch" = ""
"##,
    )
    .expect("valid");
    let frontend = settings.frontend();
    assert_eq!(frontend.theme, "example.themes.night");
    assert_eq!(frontend.icon_theme, "example.icons.colorful");
    assert_eq!(frontend.colors["editor-background"], "#abcdef");
    assert_eq!(frontend.keybindings["file.save"], ["ctrl+s", "f2"]);
    assert!(frontend.keybindings["workbench.showSearch"].is_empty());
  }

  #[test]
  fn parses_extension_settings() {
    let settings = Settings::parse(
      r#"
[extensions]
development = ["C:/code/tidy"]
[extensions.settings."example.tidy"]
max_line_length = 80
"#,
    )
    .expect("valid");
    assert!(settings.extensions.enabled);
    assert_eq!(settings.extensions.development, ["C:/code/tidy"]);
    assert_eq!(
      settings.extensions.settings["example.tidy"]["max_line_length"].as_integer(),
      Some(80)
    );
    let empty = Settings::parse("[extensions]\ndevelopment = [\" \"]\n").expect_err("empty path");
    assert!(empty.contains("extensions.development"), "{empty}");
  }

  #[test]
  fn language_servers_come_from_extensions_with_settings_over_them() {
    let server = |name: &str, program: &str, languages: &[&str]| ServerConfig {
      name: name.into(),
      command: ServerCommand {
        program: program.into(),
        args: vec!["--stdio".into()],
      },
      languages: languages.iter().map(|&l| l.into()).collect(),
      root_markers: Vec::new(),
      settings: serde_json::Value::Null,
    };
    let contributed = vec![
      server("rust-analyzer", "rust-analyzer", &["rust"]),
      server("pyright", "pyright-langserver", &["python"]),
      // Two extensions that declare the same server share it.
      server("tsls", "typescript-language-server", &["typescript"]),
      server(
        "tsls",
        "typescript-language-server",
        &["javascript", "typescript"],
      ),
      server("tsls", "something-else", &["css"]),
    ];

    let plain = Settings::parse("").expect("valid");
    assert!(
      plain.language_servers_with(Vec::new()).is_empty(),
      "none built in"
    );
    let servers = plain.language_servers_with(contributed.clone());
    assert_eq!(servers.len(), 3);
    let tsls = servers.iter().find(|s| s.name == "tsls").expect("shared");
    assert_eq!(tsls.languages, ["typescript", "javascript"]);
    assert_eq!(tsls.command.program, "typescript-language-server");

    let settings = Settings::parse(
      r#"
[lsp.servers.rust-analyzer]
command = "/opt/ra"
[lsp.servers.pyright]
enabled = false
[lsp.servers.mine]
command = "mine"
args = ["--stdio"]
languages = ["toml"]
[lsp.servers.typo]
command = "x"
"#,
    )
    .expect("valid");
    let servers = settings.language_servers_with(contributed);
    let rust = servers
      .iter()
      .find(|s| s.name == "rust-analyzer")
      .expect("rust");
    assert_eq!(rust.command.program, "/opt/ra", "settings change a server");
    assert_eq!(rust.languages, ["rust"]);
    assert!(
      servers.iter().all(|s| s.name != "pyright"),
      "or turn it off"
    );
    let mine = servers.iter().find(|s| s.name == "mine").expect("added");
    assert_eq!(mine.command.args, ["--stdio"]);
    assert!(
      servers.iter().all(|s| s.name != "typo"),
      "incomplete entries for no server are ignored"
    );

    let disabled = Settings::parse("[lsp]\nenabled = false\n").expect("valid");
    assert!(
      disabled
        .language_servers_with(vec![server("a", "a", &["rust"])])
        .is_empty()
    );
  }

  /// The keys serde accepts for `T`, from its unknown-field error.
  fn fields<T: serde::de::DeserializeOwned>() -> Vec<String> {
    let error = toml::from_str::<T>("zzz_unknown = 1")
      .err()
      .map(|error| error.to_string())
      .unwrap_or_default();
    let expected = error.split("expected").nth(1).unwrap_or_default();
    let mut names: Vec<String> = expected
      .split('`')
      .skip(1)
      .step_by(2)
      .map(str::to_owned)
      .collect();
    names.sort();
    names
  }

  #[test]
  fn the_settings_schema_agrees_with_the_settings() {
    use flareite_schema::document::Segment;
    use flareite_schema::nav::Navigate;

    let schema = flareite_schema::tosd::Schema::from_source(
      "settings.tosd".into(),
      flareite_schema::builtin::SETTINGS,
    )
    .expect("schema");
    let keys = |path: &[&str]| -> Vec<String> {
      let path: Vec<Segment> = path.iter().map(|&key| Segment::key(key)).collect();
      let mut names: Vec<String> = schema
        .properties(&path)
        .into_iter()
        .map(|property| property.name)
        .collect();
      names.sort();
      names
    };
    let checks: Vec<(Vec<String>, &[&str])> = vec![
      (fields::<Settings>(), &[]),
      (fields::<EditorSettings>(), &["editor"]),
      (fields::<WorkbenchSettings>(), &["workbench"]),
      (fields::<FilesSettings>(), &["files"]),
      (fields::<SearchSettings>(), &["search"]),
      (fields::<LspSettings>(), &["lsp"]),
      (fields::<ServerSettings>(), &["lsp", "servers", "x"]),
      (fields::<TerminalSettings>(), &["terminal"]),
      (fields::<ProfileSettings>(), &["terminal", "profiles", "x"]),
      (fields::<ExtensionsSettings>(), &["extensions"]),
      (fields::<SecuritySettings>(), &["security"]),
      (fields::<UpdateSettings>(), &["update"]),
      (fields::<SchemasSettings>(), &["schemas"]),
    ];
    for (settings, path) in checks {
      assert!(!settings.is_empty(), "{path:?}");
      assert_eq!(settings, keys(path), "settings.tosd at {path:?}");
    }

    // The schema's defaults are the settings' defaults, and valid.
    let defaults = Settings::default();
    for spec_path in [
      ["editor", "tab_size"],
      ["editor", "font_size"],
      ["search", "max_file_bytes"],
      ["terminal", "scrollback"],
    ] {
      let path: Vec<Segment> = spec_path.iter().map(|&key| Segment::key(key)).collect();
      let default = schema
        .info(&path)
        .and_then(|info| info.default)
        .expect("default");
      let text = format!("[{}]\n{} = {default}\n", spec_path[0], spec_path[1]);
      assert_eq!(Settings::parse(&text).expect("valid"), defaults, "{text}");
    }

    // Documents the settings accept, the schema accepts, and the other way.
    let schema = flareite_schema::Schema::Tosd(Box::new(schema));
    let check =
      |text: &str| flareite_schema::check(flareite_schema::Format::Toml, text, &[&schema]);
    let uncommented: String = TEMPLATE
      .lines()
      .filter(|line| line.starts_with("# ") && line.contains(" = ") && !line.starts_with("# #"))
      .map(|line| {
        line
          .trim_start_matches("# ")
          .split("  #")
          .next()
          .unwrap_or(line)
      })
      .collect::<Vec<_>>()
      .join("\n");
    assert!(!uncommented.is_empty());
    for text in [
      "[editor]\ntab_size = 2\n[keybindings]\n\"file.save\" = [\"mod+s\", \"f2\"]\n\"a\" = \"\"\n[[schemas.associations]]\nfiles = [\"*.x.toml\"]\nschema = \"x.tosd\"\n",
      "[lsp.servers.rust-analyzer]\nenabled = false\n[terminal.profiles.\"Nu\"]\ncommand = \"nu\"\nenv = { A = \"b\" }\n",
    ] {
      Settings::parse(text).expect("the settings accept it");
      assert!(check(text).is_empty(), "{text}: {:?}", check(text));
    }
    for text in [
      "[editor]\ntab_size = 0\n",
      "[update]\nchannel = \"nightly\"\n",
      "[workbench.colors]\neditor-background = \"red\"\n",
      "nope = 1\n",
    ] {
      assert!(Settings::parse(text).is_err(), "{text}");
      assert!(!check(text).is_empty(), "{text}");
    }
  }
}
