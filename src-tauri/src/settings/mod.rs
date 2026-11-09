//! User settings, stored as TOML in the platform config directory and edited
//! in Flareite itself, as a file or in the settings editor. Saving the file
//! applies it; invalid files are reported and the previous settings stay in
//! effect.

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
  /// Command id to its keys, overriding the defaults.
  pub keybindings: BTreeMap<String, KeybindingSetting>,
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
  current: RwLock<Settings>,
}

impl SettingsState {
  /// Loads settings from the config directory. Problems are logged and the
  /// defaults are used.
  pub fn load(config_dir: Option<PathBuf>) -> Self {
    let path = config_dir.map(|dir| dir.join(FILE_NAME));
    let settings = match path.as_deref().map(std::fs::read_to_string) {
      Some(Ok(text)) => Settings::parse(&text).unwrap_or_else(|error| {
        tracing::warn!(%error, "invalid settings file; using defaults");
        Settings::default()
      }),
      Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
        tracing::warn!(%error, "could not read settings; using defaults");
        Settings::default()
      }
      _ => Settings::default(),
    };
    Self {
      path,
      current: RwLock::new(settings),
    }
  }

  pub fn get(&self) -> Settings {
    self
      .current
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  pub fn path(&self) -> Option<&Path> {
    self.path.as_deref()
  }

  fn replace(&self, settings: Settings) {
    *self.current.write().unwrap_or_else(PoisonError::into_inner) = settings;
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
  let previous = state.get();
  state.replace(settings.clone());
  apply(app, &previous, &settings, announce);
  tracing::info!("settings applied");
  Ok(())
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
    app.state::<crate::lsp::LspState>().restart_all(app);
  }
  crate::extensions::settings_changed(app, previous, settings);
  if previous.security != settings.security {
    crate::trust::changed(app);
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
}

/// Every built-in setting and every setting extensions declare, with its
/// description, default, and value in effect.
#[tauri::command]
pub async fn settings_schema(app: AppHandle) -> Result<SettingsSchemaDto, CommandError> {
  // Finding terminal profiles can take a moment.
  tauri::async_runtime::spawn_blocking(move || {
    let state = app.state::<SettingsState>();
    let settings = state.get();
    let text = state.path().map(std::fs::read_to_string);
    let (file, error) = match &text {
      None => (None, None),
      Some(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
      Some(Err(error)) => (None, Some(format!("Could not read settings: {error}"))),
      Some(Ok(text)) => (
        toml::from_str::<toml::Table>(text).ok(),
        Settings::parse(text).err(),
      ),
    };
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
    SettingsSchemaDto {
      file: state.path().map(|path| path.display().to_string()),
      error,
      settings: schema::describe(&settings, file.as_ref(), &installed),
    }
  })
  .await
  .map_err(CommandError::internal)
}

/// Sets one setting in the settings file from the settings editor, keeping
/// the file's comments and formatting, and applies it. `path` is the
/// setting's keys; `None` removes it, so its default applies.
#[tauri::command]
pub fn settings_set(
  app: AppHandle,
  path: Vec<String>,
  value: Option<serde_json::Value>,
) -> Result<(), CommandError> {
  let name = path.join(".");
  let invalid = |message: String| CommandError::new("invalidSetting", message);
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
  edit(&app, |document| set_value(document, &path, value))
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
}
