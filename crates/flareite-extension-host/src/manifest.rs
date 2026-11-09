//! Extension manifests (`extension.toml`): parsing and validation.
//!
//! A manifest is validated completely before anything is loaded: unknown
//! fields, unsupported API versions, editor version requirements that
//! aren't met, malformed identifiers, undeclared commands in activation
//! events, and configuration defaults of the wrong type are all rejected
//! with a message naming the problem.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::Deserialize;

use crate::config::{ConfigProperty, ConfigType, ConfigValue};
use crate::contributions::{self, Contributions, RawContributions};
use crate::permissions::Permission;

/// The manifest file in an extension's folder.
pub const MANIFEST_FILE: &str = "extension.toml";
/// The extension API versions this host implements.
pub const SUPPORTED_API_VERSIONS: &[u32] = &[1];
/// Larger entry points are rejected.
pub const MAX_ENTRYPOINT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
  #[error("could not read {path}: {source}")]
  Read {
    path: PathBuf,
    source: std::io::Error,
  },
  #[error("{0}")]
  Invalid(String),
  #[error("requires extension API version {found}; this version of Flareite supports {supported}")]
  UnsupportedApi { found: String, supported: String },
  #[error("requires Flareite {required}; this is Flareite {editor}")]
  IncompatibleEditor { required: String, editor: String },
}

fn invalid(message: impl Into<String>) -> ManifestError {
  ManifestError::Invalid(message.into())
}

/// When an extension is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
  /// When the editor starts (or the extension is loaded).
  Startup,
  /// When one of its commands runs. Every extension activates for its own
  /// commands, so this is implied.
  Command(String),
  /// When a document of this language (e.g. `rust`) is opened.
  Language(String),
  /// When one of its side bar views is shown.
  View(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandContribution {
  pub id: String,
  pub title: String,
  /// Shown before the title in the command palette; the extension's name
  /// by default.
  pub category: String,
  /// Left out of the command palette (e.g. run only from view items).
  pub hidden: bool,
}

/// How a development extension is built (`[build]`): a program run in its
/// folder, again whenever a file there changes. Installed extensions may
/// declare one, but it is never run for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
  /// A program on the `PATH`, or a path relative to the extension's folder
  /// (with a `/` or `\`).
  pub command: String,
  pub args: Vec<String>,
  /// Which changes start a build: exclude-style patterns relative to the
  /// folder (a name at any depth without a `/`, a path with one). Empty:
  /// any file the folder's `.gitignore` doesn't ignore.
  pub watch: Vec<String>,
}

const MAX_BUILD_ARGS: usize = 64;
const MAX_BUILD_WATCH: usize = 64;
const MAX_BUILD_STRING: usize = 1000;

#[derive(Debug, Clone)]
pub struct Manifest {
  pub id: String,
  pub name: String,
  pub version: Version,
  pub description: String,
  /// Who made it, as they want to be credited.
  pub author: Option<String>,
  pub api_version: u32,
  /// The folder the manifest is in.
  pub dir: PathBuf,
  /// A picture shown beside the extension's name: a PNG or SVG file in
  /// `dir`.
  pub icon: Option<PathBuf>,
  /// `README.md` in `dir` (any case), shown on the extension's page.
  pub readme: Option<PathBuf>,
  /// The component or module to load, resolved against `dir`. Extensions
  /// that only contribute themes and icon themes have none.
  pub entrypoint: Option<PathBuf>,
  pub activation: Vec<Activation>,
  pub permissions: Vec<Permission>,
  pub commands: Vec<CommandContribution>,
  pub configuration: BTreeMap<String, ConfigProperty>,
  pub contributions: Contributions,
  /// Run when the open folder, or whether it's trusted, changes. (1.6)
  pub on_folder_changed: Option<String>,
  /// Run when the file of the editor in use changes (1.11).
  pub on_active_file_changed: Option<String>,
  /// Run when the cursor moves to another line, or another document is
  /// in use (1.13).
  pub on_selection_changed: Option<String>,
  /// How it's built while in development.
  pub build: Option<Build>,
}

impl Manifest {
  pub fn activates_on_startup(&self) -> bool {
    self.activation.contains(&Activation::Startup)
  }

  pub fn activates_for_language(&self, language: &str) -> bool {
    self
      .activation
      .iter()
      .any(|activation| matches!(activation, Activation::Language(l) if l == language))
  }

  pub fn activates_for_view(&self, view: &str) -> bool {
    self
      .activation
      .iter()
      .any(|activation| matches!(activation, Activation::View(v) if v == view))
  }

  pub fn declares_command(&self, id: &str) -> bool {
    self.commands.iter().any(|command| command.id == id)
  }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
  id: String,
  name: String,
  version: String,
  #[serde(default)]
  description: String,
  author: Option<String>,
  icon: Option<String>,
  api_version: String,
  entrypoint: Option<String>,
  #[serde(default)]
  activation: Vec<String>,
  #[serde(default)]
  engines: RawEngines,
  #[serde(default)]
  permissions: RawPermissions,
  #[serde(default)]
  commands: Vec<RawCommand>,
  #[serde(default)]
  configuration: BTreeMap<String, RawProperty>,
  #[serde(default)]
  themes: Vec<contributions::RawTheme>,
  #[serde(default)]
  icon_themes: Vec<contributions::RawIconTheme>,
  #[serde(default)]
  keybindings: Vec<contributions::RawKeybinding>,
  #[serde(default)]
  menus: Vec<contributions::RawMenu>,
  #[serde(default)]
  status_items: Vec<contributions::RawStatusItem>,
  #[serde(default)]
  views: Vec<contributions::RawView>,
  #[serde(default)]
  decorations: BTreeMap<String, contributions::RawDecoration>,
  #[serde(default)]
  previews: Vec<contributions::RawPreview>,
  #[serde(default)]
  viewers: Vec<contributions::RawViewer>,
  #[serde(default)]
  layouts: Vec<contributions::RawLayout>,
  #[serde(default)]
  link_handlers: Vec<contributions::RawLinkHandler>,
  #[serde(default)]
  languages: Vec<contributions::RawLanguage>,
  #[serde(default)]
  language_servers: Vec<contributions::RawLanguageServer>,
  #[serde(default)]
  programs: Vec<contributions::RawProgram>,
  #[serde(default)]
  colors: BTreeMap<String, contributions::RawColor>,
  #[serde(default)]
  view_containers: Vec<contributions::RawViewContainer>,
  #[serde(default)]
  text_providers: Vec<contributions::RawTextProvider>,
  quick_diff: Option<contributions::RawQuickDiff>,
  on_folder_changed: Option<String>,
  on_active_file_changed: Option<String>,
  on_selection_changed: Option<String>,
  build: Option<RawBuild>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
  command: String,
  #[serde(default)]
  args: Vec<String>,
  #[serde(default)]
  watch: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawEngines {
  flareite: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawPermissions {
  #[serde(default)]
  documents_read: bool,
  #[serde(default)]
  documents_write: bool,
  #[serde(default)]
  files_read: bool,
  #[serde(default)]
  files_write: bool,
  #[serde(default)]
  language_servers: bool,
  #[serde(default)]
  network: bool,
  #[serde(default)]
  processes: bool,
  #[serde(default)]
  web_pages: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
  id: String,
  title: String,
  category: Option<String>,
  #[serde(default)]
  hidden: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProperty {
  #[serde(rename = "type")]
  kind: String,
  default: toml::Value,
  #[serde(default)]
  description: String,
  minimum: Option<f64>,
  maximum: Option<f64>,
  choices: Option<Vec<String>>,
}

/// Reads and validates `dir/extension.toml`. `development` extensions may
/// load an entry point from outside their folder (e.g. a build output).
pub fn load(
  dir: &Path,
  editor_version: &Version,
  development: bool,
) -> Result<Manifest, ManifestError> {
  let path = dir.join(MANIFEST_FILE);
  let read = |source| ManifestError::Read {
    path: path.clone(),
    source,
  };
  let size = std::fs::metadata(&path).map_err(read)?.len();
  if size > MAX_MANIFEST_BYTES {
    return Err(invalid(format!("{MANIFEST_FILE} is too large")));
  }
  let text = std::fs::read_to_string(&path).map_err(read)?;
  let mut manifest = parse(&text, dir, editor_version, development)?;
  if let Some(entrypoint) = &manifest.entrypoint {
    let entry = std::fs::metadata(entrypoint).map_err(|source| ManifestError::Read {
      path: entrypoint.clone(),
      source,
    })?;
    if !entry.is_file() {
      return Err(invalid(format!(
        "entrypoint {} is not a file",
        entrypoint.display()
      )));
    }
    if entry.len() > MAX_ENTRYPOINT_BYTES {
      return Err(invalid(format!(
        "entrypoint is larger than {} MiB",
        MAX_ENTRYPOINT_BYTES / 1024 / 1024
      )));
    }
  }
  for theme in &mut manifest.contributions.themes {
    contributions::load_theme_colors(dir, theme)?;
  }
  for theme in &mut manifest.contributions.icon_themes {
    contributions::load_icon_theme(dir, theme)?;
  }
  for language in &manifest.contributions.languages {
    contributions::check_language_files(dir, language)?;
  }
  contributions::check_webview_pages(dir, &manifest.contributions)?;
  contributions::load_action_icons(dir, &mut manifest.contributions)?;
  Ok(manifest)
}

/// Parses and validates manifest text. Does not touch the filesystem.
pub fn parse(
  text: &str,
  dir: &Path,
  editor_version: &Version,
  development: bool,
) -> Result<Manifest, ManifestError> {
  let raw: RawManifest = toml::from_str(text).map_err(|error| invalid(error.to_string()))?;

  validate_id(&raw.id)?;
  let name = raw.name.trim();
  if name.is_empty() || name.chars().count() > 64 {
    return Err(invalid("name must be 1 to 64 characters"));
  }
  let version = Version::parse(&raw.version).map_err(|error| {
    invalid(format!(
      "version {:?} is not a semantic version: {error}",
      raw.version
    ))
  })?;
  if raw.description.chars().count() > 500 {
    return Err(invalid("description must be at most 500 characters"));
  }
  let author = raw
    .author
    .as_deref()
    .map(str::trim)
    .filter(|a| !a.is_empty());
  if author.is_some_and(|author| author.chars().count() > 100) {
    return Err(invalid("author must be at most 100 characters"));
  }
  let icon = raw
    .icon
    .as_deref()
    .map(|icon| check_icon(dir, icon))
    .transpose()?;

  let api_version = raw
    .api_version
    .parse::<u32>()
    .ok()
    .filter(|version| SUPPORTED_API_VERSIONS.contains(version))
    .ok_or_else(|| ManifestError::UnsupportedApi {
      found: raw.api_version.clone(),
      supported: SUPPORTED_API_VERSIONS
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", "),
    })?;
  if let Some(required) = &raw.engines.flareite {
    let requirement = VersionReq::parse(required)
      .map_err(|error| invalid(format!("engines.flareite {required:?} is invalid: {error}")))?;
    if !requirement.matches(editor_version) {
      return Err(ManifestError::IncompatibleEditor {
        required: required.clone(),
        editor: editor_version.to_string(),
      });
    }
  }

  let entrypoint = raw
    .entrypoint
    .as_deref()
    .map(|entrypoint| resolve_entrypoint(dir, entrypoint, development))
    .transpose()?;
  let build = raw.build.map(parse_build).transpose()?;

  let mut commands: Vec<CommandContribution> = Vec::new();
  for command in raw.commands {
    let prefix = format!("{}.", raw.id);
    let rest = command.id.strip_prefix(&prefix).unwrap_or_default();
    if rest.is_empty()
      || command.id.len() > 128
      || !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
      return Err(invalid(format!(
        "command id {:?} must start with \"{prefix}\" followed by letters, digits, '.', '-', or '_'",
        command.id
      )));
    }
    if commands.iter().any(|existing| existing.id == command.id) {
      return Err(invalid(format!(
        "command {:?} is declared twice",
        command.id
      )));
    }
    let title = command.title.trim();
    if title.is_empty() || title.chars().count() > 100 {
      return Err(invalid(format!(
        "command {:?} needs a title of 1 to 100 characters",
        command.id
      )));
    }
    commands.push(CommandContribution {
      id: command.id,
      title: title.to_owned(),
      category: command
        .category
        .map_or_else(|| name.to_owned(), |category| category.trim().to_owned()),
      hidden: command.hidden,
    });
  }

  let mut activation = Vec::new();
  for event in &raw.activation {
    let parsed = if event == "on-startup" {
      Activation::Startup
    } else if let Some(command) = event.strip_prefix("on-command:") {
      if !commands.iter().any(|c| c.id == command) {
        return Err(invalid(format!(
          "activation {event:?} names a command that is not declared"
        )));
      }
      Activation::Command(command.to_owned())
    } else if let Some(language) = event.strip_prefix("on-language:")
      && !language.is_empty()
    {
      Activation::Language(language.to_owned())
    } else if let Some(view) = event.strip_prefix("on-view:") {
      if !raw.views.iter().any(|v| v.id == view) {
        return Err(invalid(format!(
          "activation {event:?} names a view that is not declared"
        )));
      }
      Activation::View(view.to_owned())
    } else {
      return Err(invalid(format!(
        "unknown activation event {event:?} (expected \"on-startup\", \"on-command:<id>\", \"on-language:<id>\", or \"on-view:<id>\")"
      )));
    };
    activation.push(parsed);
  }

  let mut permissions = Vec::new();
  if raw.permissions.documents_read {
    permissions.push(Permission::DocumentsRead);
  }
  if raw.permissions.documents_write {
    permissions.push(Permission::DocumentsWrite);
  }
  if raw.permissions.files_read {
    permissions.push(Permission::FilesRead);
  }
  if raw.permissions.files_write {
    permissions.push(Permission::FilesWrite);
  }
  if raw.permissions.language_servers {
    permissions.push(Permission::LanguageServers);
  }
  if raw.permissions.network {
    permissions.push(Permission::Network);
  }
  if raw.permissions.processes {
    permissions.push(Permission::Processes);
  }
  if raw.permissions.web_pages {
    permissions.push(Permission::WebPages);
  }
  if raw.permissions.language_servers == raw.language_servers.is_empty() {
    return Err(invalid(
      "permissions.language_servers must be requested exactly when language_servers are declared",
    ));
  }
  if raw.permissions.processes == raw.programs.is_empty() {
    return Err(invalid(
      "permissions.processes must be requested exactly when programs are declared",
    ));
  }
  for (field, command) in [
    ("on_folder_changed", &raw.on_folder_changed),
    ("on_active_file_changed", &raw.on_active_file_changed),
    ("on_selection_changed", &raw.on_selection_changed),
  ] {
    if let Some(command) = command
      && !commands.iter().any(|c| &c.id == command)
    {
      return Err(invalid(format!(
        "{field}: command {command:?} is not one of this extension's commands"
      )));
    }
  }

  let mut configuration = BTreeMap::new();
  for (key, property) in raw.configuration {
    if key.is_empty()
      || key.len() > 64
      || !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
      return Err(invalid(format!(
        "configuration key {key:?} must be letters, digits, '_', '-', or '.'"
      )));
    }
    let property = parse_property(&key, property)?;
    configuration.insert(key, property);
  }

  let contributions = contributions::parse(
    RawContributions {
      themes: raw.themes,
      icon_themes: raw.icon_themes,
      keybindings: raw.keybindings,
      menus: raw.menus,
      status_items: raw.status_items,
      views: raw.views,
      decorations: raw.decorations,
      previews: raw.previews,
      viewers: raw.viewers,
      layouts: raw.layouts,
      link_handlers: raw.link_handlers,
      languages: raw.languages,
      language_servers: raw.language_servers,
      programs: raw.programs,
      colors: raw.colors,
      view_containers: raw.view_containers,
      text_providers: raw.text_providers,
      quick_diff: raw.quick_diff,
    },
    &raw.id,
    &commands,
  )?;
  // The editor starts language servers itself, and pages' policies carry
  // the network permission.
  let code_permissions = permissions.iter().any(|&permission| {
    !matches!(
      permission,
      Permission::LanguageServers | Permission::Network
    )
  });
  if entrypoint.is_none()
    && (!commands.is_empty()
      || !activation.is_empty()
      || code_permissions
      || !configuration.is_empty()
      || contributions.needs_code())
  {
    return Err(invalid(
      "entrypoint is required unless the extension only contributes themes, icon themes, languages, and language servers",
    ));
  }

  Ok(Manifest {
    id: raw.id,
    name: name.to_owned(),
    version,
    description: raw.description.trim().to_owned(),
    author: author.map(str::to_owned),
    api_version,
    dir: dir.to_path_buf(),
    icon,
    readme: find_readme(dir),
    entrypoint,
    activation,
    permissions,
    commands,
    configuration,
    contributions,
    on_folder_changed: raw.on_folder_changed,
    on_active_file_changed: raw.on_active_file_changed,
    on_selection_changed: raw.on_selection_changed,
    build,
  })
}

fn parse_build(raw: RawBuild) -> Result<Build, ManifestError> {
  let bad = |s: &str| s.is_empty() || s.chars().count() > MAX_BUILD_STRING || s.contains('\0');
  if bad(raw.command.trim()) {
    return Err(invalid(format!(
      "build: command must be 1 to {MAX_BUILD_STRING} characters"
    )));
  }
  if raw.args.len() > MAX_BUILD_ARGS
    || raw
      .args
      .iter()
      .any(|arg| arg.chars().count() > MAX_BUILD_STRING || arg.contains('\0'))
  {
    return Err(invalid(format!(
      "build: at most {MAX_BUILD_ARGS} args, each at most {MAX_BUILD_STRING} characters"
    )));
  }
  if raw.watch.len() > MAX_BUILD_WATCH || raw.watch.iter().any(|pattern| bad(pattern)) {
    return Err(invalid(format!(
      "build: at most {MAX_BUILD_WATCH} watch patterns, each 1 to {MAX_BUILD_STRING} characters"
    )));
  }
  flareite_workspace::ExcludeFilter::new(&raw.watch)
    .map_err(|error| invalid(format!("build: watch: {error}")))?;
  Ok(Build {
    command: raw.command.trim().to_owned(),
    args: raw.args,
    watch: raw.watch,
  })
}

/// What a development extension's folder says about building it, read
/// even when the rest of its manifest is invalid (it may only lack the
/// entry point the build makes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPlan {
  /// The folder: the build runs there.
  pub dir: PathBuf,
  /// The manifest's `id`, if it has a string one: the build's log goes to
  /// its channel.
  pub id: Option<String>,
  /// The manifest's `name`, if it has a string one.
  pub name: Option<String>,
  /// The entry point, if the manifest names one: changes to it don't
  /// start a build.
  pub entrypoint: Option<PathBuf>,
  pub build: Build,
}

/// Reads the `[build]` table of `dir/extension.toml`. `Ok(None)` if it has
/// none; an error if the file can't be read or parsed as TOML, or the table
/// is invalid.
pub fn read_build_plan(dir: &Path) -> Result<Option<BuildPlan>, ManifestError> {
  let path = dir.join(MANIFEST_FILE);
  let read = |source| ManifestError::Read {
    path: path.clone(),
    source,
  };
  if std::fs::metadata(&path).map_err(read)?.len() > MAX_MANIFEST_BYTES {
    return Err(invalid(format!("{MANIFEST_FILE} is too large")));
  }
  let text = std::fs::read_to_string(&path).map_err(read)?;
  let mut table: toml::Table = text.parse().map_err(|error| invalid(format!("{error}")))?;
  let Some(raw) = table.remove("build") else {
    return Ok(None);
  };
  let raw: RawBuild = raw
    .try_into()
    .map_err(|error| invalid(format!("build: {error}")))?;
  let string = |key: &str| table.get(key).and_then(toml::Value::as_str);
  Ok(Some(BuildPlan {
    dir: dir.to_path_buf(),
    id: string("id").map(str::to_owned),
    name: string("name").map(|name| name.trim().to_owned()),
    entrypoint: string("entrypoint")
      .and_then(|entrypoint| resolve_entrypoint(dir, entrypoint, true).ok()),
    build: parse_build(raw)?,
  }))
}

/// Largest icon file.
pub const MAX_ICON_BYTES: u64 = 256 * 1024;

/// The icon file `icon` names: a PNG or SVG file in `dir`, at most 256 KiB.
fn check_icon(dir: &Path, icon: &str) -> Result<PathBuf, ManifestError> {
  let path = dir.join(contributions::inside("icon", icon)?);
  let extension = path
    .extension()
    .map(|extension| extension.to_string_lossy().to_lowercase());
  if !matches!(extension.as_deref(), Some("png" | "svg")) {
    return Err(invalid(format!("icon {icon:?} must be a PNG or SVG file")));
  }
  let size = std::fs::metadata(&path)
    .map_err(|error| invalid(format!("icon {icon:?} could not be read: {error}")))?
    .len();
  if size > MAX_ICON_BYTES {
    return Err(invalid(format!(
      "icon {icon:?} must be at most {} KiB",
      MAX_ICON_BYTES / 1024
    )));
  }
  Ok(path)
}

/// `README.md` in `dir`, whatever its case.
fn find_readme(dir: &Path) -> Option<PathBuf> {
  std::fs::read_dir(dir)
    .ok()?
    .filter_map(Result::ok)
    .find(|entry| {
      entry.file_name().eq_ignore_ascii_case("README.md")
        && entry.file_type().is_ok_and(|kind| kind.is_file())
    })
    .map(|entry| entry.path())
}

/// Identifiers are dot-separated segments of lowercase letters, digits, and
/// hyphens, like `publisher.name`.
fn validate_id(id: &str) -> Result<(), ManifestError> {
  let segments: Vec<&str> = id.split('.').collect();
  let valid = (3..=64).contains(&id.len())
    && segments.len() >= 2
    && segments.iter().all(|segment| {
      !segment.is_empty()
        && !segment.starts_with('-')
        && segment
          .chars()
          .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    });
  if valid {
    Ok(())
  } else {
    Err(invalid(format!(
      "id {id:?} must look like \"publisher.name\": lowercase letters, digits, and '-', separated by '.'"
    )))
  }
}

fn resolve_entrypoint(
  dir: &Path,
  entrypoint: &str,
  development: bool,
) -> Result<PathBuf, ManifestError> {
  let relative = Path::new(entrypoint);
  if entrypoint.is_empty() || relative.is_absolute() || relative.has_root() {
    return Err(invalid(
      "entrypoint must be a path relative to the manifest",
    ));
  }
  let escapes = relative
    .components()
    .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir));
  if escapes && !development {
    return Err(invalid(
      "entrypoint must be inside the extension's folder (only development extensions may use '..')",
    ));
  }
  Ok(dir.join(relative))
}

fn parse_property(key: &str, raw: RawProperty) -> Result<ConfigProperty, ManifestError> {
  let kind = match raw.kind.as_str() {
    "boolean" => ConfigType::Boolean,
    "integer" => ConfigType::Integer,
    "number" => ConfigType::Number,
    "string" => ConfigType::String,
    other => {
      return Err(invalid(format!(
        "configuration.{key}: unknown type {other:?} (expected boolean, integer, number, or string)"
      )));
    }
  };
  if raw.choices.is_some() && kind != ConfigType::String {
    return Err(invalid(format!(
      "configuration.{key}: choices are only allowed for strings"
    )));
  }
  if (raw.minimum.is_some() || raw.maximum.is_some())
    && !matches!(kind, ConfigType::Integer | ConfigType::Number)
  {
    return Err(invalid(format!(
      "configuration.{key}: minimum and maximum are only allowed for numbers"
    )));
  }
  let mut property = ConfigProperty {
    kind,
    default: ConfigValue::Boolean(false),
    description: raw.description,
    minimum: raw.minimum,
    maximum: raw.maximum,
    choices: raw.choices,
  };
  property.default = property
    .check(&raw.default)
    .map_err(|error| invalid(format!("configuration.{key}: default {error}")))?;
  Ok(property)
}

#[cfg(test)]
mod tests {
  use super::*;

  const VALID: &str = r#"
id = "example.tidy"
name = "Tidy"
version = "0.1.0"
api_version = "1"
entrypoint = "tidy.wasm"
activation = ["on-startup", "on-language:rust", "on-command:example.tidy.trim"]

[engines]
flareite = ">=0.1.0"

[permissions]
documents_read = true

[[commands]]
id = "example.tidy.trim"
title = "Trim"

[configuration.width]
type = "integer"
default = 100
minimum = 0
"#;

  fn editor() -> Version {
    Version::new(0, 1, 0)
  }

  fn parse_text(text: &str) -> Result<Manifest, ManifestError> {
    parse(text, Path::new("/ext"), &editor(), false)
  }

  fn error_of(text: &str) -> String {
    parse_text(text).expect_err("rejected").to_string()
  }

  #[test]
  fn reads_programs_colors_and_the_folder_command() {
    let text = VALID.replace(
      "documents_read = true",
      "documents_read = true\nprocesses = true",
    ) + r##"
[[programs]]
name = "git"

[[programs]]
name = "prettier"
command = "bin/prettier"
workspace = "node_modules/.bin/prettier"

[colors.modified]
description = "Changed files"
dark = "#E2C08D"
light = "#895503"

[colors.added]
dark = "#81b88b"
"##;
    let text = text.replacen(
      "[engines]",
      "on_folder_changed = \"example.tidy.trim\"\n\n[engines]",
      1,
    );
    let manifest = parse_text(&text).expect("valid");
    assert_eq!(
      manifest.on_folder_changed.as_deref(),
      Some("example.tidy.trim")
    );
    assert!(manifest.permissions.contains(&Permission::Processes));
    let programs = &manifest.contributions.programs;
    assert_eq!(programs[0].command, "git");
    assert_eq!(programs[1].command, "bin/prettier");
    assert_eq!(
      programs[1].workspace.as_deref(),
      Some("node_modules/.bin/prettier")
    );
    let modified = manifest.contributions.color("modified").expect("declared");
    assert_eq!(modified.id, "example.tidy.modified");
    assert_eq!(modified.dark, "#e2c08d");
    let added = manifest.contributions.color("added").expect("declared");
    assert_eq!(added.light, added.dark, "light defaults to dark");
  }

  const VIEWS: &str = r#"
[[view_containers]]
id = "scm"
title = "Source Control"
icon = "source-control"

[[views]]
id = "changes"
title = "Changes"
container = "scm"
input = { placeholder = "Message", command = "example.tidy.trim" }

[[views]]
id = "commits"
title = "Commits"
container = "scm"
on_expand = "example.tidy.trim"

[[menus]]
location = "view/item"
view = "changes"
contexts = ["staged"]
command = "example.tidy.trim"
inline = true
icon = "remove"

[[menus]]
location = "view/title"
view = "commits"
command = "example.tidy.trim"
when = "!tree"
"#;

  #[test]
  fn reads_view_containers_and_view_menus() {
    let manifest = parse_text(&format!("{VALID}{VIEWS}")).expect("valid");
    let contributions = &manifest.contributions;
    assert_eq!(contributions.view_containers[0].full_id, "example.tidy.scm");
    let changes = contributions.view("changes").expect("declared");
    assert_eq!(changes.container.as_deref(), Some("example.tidy.scm"));
    assert_eq!(
      changes
        .input
        .as_ref()
        .and_then(|input| input.command.as_deref()),
      Some("example.tidy.trim")
    );
    let commits = contributions.view("commits").expect("declared");
    assert_eq!(commits.on_expand.as_deref(), Some("example.tidy.trim"));
    let item = &contributions.menus[0];
    assert_eq!(item.view.as_deref(), Some("example.tidy.changes"));
    assert_eq!(item.contexts, vec!["staged"]);
    assert!(item.inline);
    assert_eq!(contributions.menus[1].when.as_deref(), Some("!tree"));
  }

  #[test]
  fn view_menus_and_containers_are_checked() {
    let views = format!("{VALID}{VIEWS}");
    let bad = |from: &str, to: &str| error_of(&views.replacen(from, to, 1));
    assert!(bad("container = \"scm\"", "container = \"nope\"").contains("not declared"));
    assert!(bad("view = \"changes\"", "view = \"nope\"").contains("not declared"));
    assert!(bad("icon = \"remove\"", "icon = \"nope\"").contains("named icons"));
    assert!(bad("icon = \"remove\"\n", "").contains("needs an icon"));
    assert!(bad("when = \"!tree\"", "when = \"a b\"").contains("flag name"));
    let explorer = VALID.to_owned()
      + "\n[[menus]]\nlocation = \"explorer/context\"\ncommand = \"example.tidy.trim\"\ninline = true\n";
    assert!(error_of(&explorer).contains("only apply to view menus"));
    let empty = VALID.to_owned() + "\n[[view_containers]]\nid = \"empty\"\ntitle = \"Empty\"\n";
    assert!(error_of(&empty).contains("has no views"));
  }

  #[test]
  fn reads_and_checks_text_providers() {
    let provider = "\n[[text_providers]]\nscheme = \"git\"\ncommand = \"example.tidy.trim\"\n";
    let manifest = parse_text(&format!("{VALID}{provider}")).expect("valid");
    let git = manifest
      .contributions
      .text_provider("git")
      .expect("declared");
    assert_eq!(git.command, "example.tidy.trim");
    assert!(manifest.contributions.needs_code());

    let bad =
      |from: &str, to: &str| error_of(&format!("{VALID}{}", provider.replacen(from, to, 1)));
    assert!(bad("\"git\"", "\"file\"").contains("reserved"));
    assert!(bad("\"git\"", "\"Git\"").contains("lowercase"));
    assert!(bad("\"git\"", "\"g\"").contains("2 to 32"));
    assert!(bad("example.tidy.trim", "example.tidy.nope").contains("not one of"));
    let twice = format!("{VALID}{provider}{provider}");
    assert!(error_of(&twice).contains("git"));
  }

  #[test]
  fn reads_and_checks_quick_diff() {
    let table = "\n[quick_diff]\noriginal = \"example.tidy.trim\"\napply = \"example.tidy.trim\"\napply_title = \"Stage\"\n";
    let manifest = parse_text(&format!("{VALID}{table}")).expect("valid");
    let quick_diff = manifest.contributions.quick_diff.expect("declared");
    assert_eq!(quick_diff.apply_title.as_deref(), Some("Stage"));
    let without_title = table.replace("apply_title = \"Stage\"\n", "");
    assert!(error_of(&format!("{VALID}{without_title}")).contains("go together"));
    let unknown = table.replace(
      "original = \"example.tidy.trim\"",
      "original = \"example.tidy.nope\"",
    );
    assert!(error_of(&format!("{VALID}{unknown}")).contains("not one of"));
  }

  #[test]
  fn programs_need_the_processes_permission_and_the_other_way_round() {
    let programs = "\n[[programs]]\nname = \"git\"\n";
    assert!(error_of(&format!("{VALID}{programs}")).contains("permissions.processes"));
    let asks = VALID.replace(
      "documents_read = true",
      "documents_read = true\nprocesses = true",
    );
    assert!(error_of(&asks).contains("permissions.processes"));
    let escapes = asks.clone() + "\n[[programs]]\nname = \"x\"\ncommand = \"../x\"\n";
    assert!(error_of(&escapes).contains("inside the extension's folder"));
    let absolute = asks.clone() + "\n[[programs]]\nname = \"x\"\nworkspace = \"/bin/x\"\n";
    assert!(error_of(&absolute).contains("inside the open folder"));
    let bad_color = VALID.to_owned() + "\n[colors.x]\ndark = \"red\"\n";
    assert!(error_of(&bad_color).contains("hex color"));
    let unknown = VALID.replacen(
      "[engines]",
      "on_folder_changed = \"example.tidy.nope\"\n\n[engines]",
      1,
    );
    assert!(error_of(&unknown).contains("on_folder_changed"));
  }

  #[test]
  fn reads_the_author_icon_and_readme() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("icon.svg"), "<svg/>").expect("write");
    std::fs::write(dir.path().join("readme.MD"), "# Tidy").expect("write");
    let with = |extra: &str| {
      let text = VALID.replace(
        "api_version = \"1\"",
        &format!("api_version = \"1\"\n{extra}"),
      );
      parse(&text, dir.path(), &editor(), false)
    };
    let manifest = with("author = \"  Flareite  \"\nicon = \"icon.svg\"").expect("valid");
    assert_eq!(manifest.author.as_deref(), Some("Flareite"));
    assert_eq!(manifest.icon, Some(dir.path().join("icon.svg")));
    assert_eq!(manifest.readme, Some(dir.path().join("readme.MD")));

    for (icon, problem) in [
      ("missing.png", "could not be read"),
      ("icon.gif", "PNG or SVG"),
      ("../icon.svg", "inside the extension's folder"),
    ] {
      let error = with(&format!("icon = \"{icon}\"")).expect_err("rejected");
      assert!(error.to_string().contains(problem), "{icon}: {error}");
    }
    let long = format!("author = \"{}\"", "a".repeat(101));
    assert!(with(&long).is_err());
    assert_eq!(parse_text(VALID).expect("valid").readme, None);
  }

  #[test]
  fn parses_a_complete_manifest() {
    let manifest = parse_text(VALID).expect("valid");
    assert_eq!(manifest.id, "example.tidy");
    assert_eq!(manifest.version, Version::new(0, 1, 0));
    assert_eq!(
      manifest.entrypoint,
      Some(Path::new("/ext").join("tidy.wasm"))
    );
    assert!(manifest.activates_on_startup());
    assert!(manifest.activates_for_language("rust"));
    assert_eq!(manifest.permissions, vec![Permission::DocumentsRead]);
    assert_eq!(manifest.commands[0].category, "Tidy");
    assert_eq!(
      manifest.configuration["width"].default,
      ConfigValue::Integer(100)
    );
  }

  #[test]
  fn rejects_unsupported_api_and_editor_versions() {
    let api = VALID.replace("api_version = \"1\"", "api_version = \"2\"");
    assert!(matches!(
      parse_text(&api),
      Err(ManifestError::UnsupportedApi { .. })
    ));
    assert!(error_of(&api).contains("supports 1"));
    let engine = VALID.replace(">=0.1.0", ">=9.0");
    assert!(matches!(
      parse_text(&engine),
      Err(ManifestError::IncompatibleEditor { .. })
    ));
  }

  #[test]
  fn rejects_unknown_fields_and_permissions() {
    let field = format!("{VALID}\nunknown = 1\n");
    assert!(error_of(&field).contains("unknown"));
    let permission = VALID.replace("documents_read = true", "clipboard = true");
    assert!(error_of(&permission).contains("clipboard"));
  }

  #[test]
  fn parses_tab_actions_and_loads_their_icons() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join("media")).expect("mkdir");
    std::fs::write(dir.path().join("media/view.html"), "").expect("write");
    std::fs::write(dir.path().join("media/grid.svg"), "<svg/>").expect("write");
    let text = format!(
      r#"{VALID}
[[viewers]]
id = "image"
title = "Image Viewer"
patterns = ["*.png"]
webview = "media/view.html"

[[viewers.actions]]
id = "fit"
title = "Fit"
icon = "fit"

[[viewers.actions]]
id = "grid"
title = "Show Grid"
icon = "media/grid.svg"
command = "example.tidy.trim"
"#
    );
    std::fs::write(dir.path().join(MANIFEST_FILE), &text).expect("write");
    std::fs::write(dir.path().join("tidy.wasm"), "").expect("write");
    let manifest = load(dir.path(), &editor(), false).expect("valid");
    let actions = &manifest.contributions.viewers[0].actions;
    assert_eq!(actions[0].id, "fit");
    assert_eq!(
      actions[0].icon,
      crate::contributions::ActionIcon::Named { name: "fit".into() }
    );
    assert_eq!(actions[0].command, None);
    let crate::contributions::ActionIcon::Image { url, .. } = &actions[1].icon else {
      panic!("an image icon");
    };
    assert!(url.starts_with("data:image/svg+xml;base64,"), "{url}");
    assert_eq!(actions[1].command.as_deref(), Some("example.tidy.trim"));

    let with = |old: &str, new: &str| error_of(&text.replacen(old, new, 1));
    assert!(with("icon = \"fit\"", "icon = \"skull\"").contains("named icons"));
    assert!(with("icon = \"fit\"", "icon = \"../fit.svg\"").contains("named icons"));
    assert!(with("icon = \"fit\"", "icon = \"fit.gif\"").contains("SVG or PNG"));
    assert!(with("id = \"grid\"", "id = \"fit\"").contains("declared twice"));
    assert!(
      with(
        "webview = \"media/view.html\"",
        "command = \"example.tidy.trim\""
      )
      .contains("needs a command")
    );
  }

  #[test]
  fn the_network_permission_is_for_pages_and_needs_no_code() {
    let pages = r#"
id = "example.pages"
name = "Pages"
version = "1.0.0"
api_version = "1"

[permissions]
network = true

[[viewers]]
id = "page"
title = "Page"
patterns = ["*.txt"]
webview = "page.html"
"#;
    let manifest = parse_text(pages).expect("valid");
    assert_eq!(manifest.permissions, vec![Permission::Network]);
  }

  #[test]
  fn validates_identifiers_and_commands() {
    for id in ["Tidy", "tidy", "example..tidy", "example.Tidy", "a.b c"] {
      let text = VALID.replace("id = \"example.tidy\"", &format!("id = {id:?}"));
      assert!(error_of(&text).contains("publisher.name"), "{id}");
    }
    let foreign = VALID.replace("id = \"example.tidy.trim\"", "id = \"other.trim\"");
    assert!(error_of(&foreign).contains("must start with"));
    let undeclared = VALID.replace("on-command:example.tidy.trim", "on-command:example.tidy.x");
    assert!(error_of(&undeclared).contains("not declared"));
    let event = VALID.replace("on-startup", "on-boot");
    assert!(error_of(&event).contains("unknown activation"));
  }

  const BUILD: &str = r#"
[build]
command = "cargo"
args = ["build", "--release"]
watch = ["src", "Cargo.toml"]
"#;

  #[test]
  fn reads_and_checks_the_build() {
    assert_eq!(parse_text(VALID).expect("valid").build, None);
    let manifest = parse_text(&format!("{VALID}{BUILD}")).expect("valid");
    assert_eq!(
      manifest.build,
      Some(Build {
        command: "cargo".into(),
        args: vec!["build".into(), "--release".into()],
        watch: vec!["src".into(), "Cargo.toml".into()],
      })
    );
    let bad = |from: &str, to: &str| error_of(&format!("{VALID}{}", BUILD.replacen(from, to, 1)));
    assert!(bad("\"cargo\"", "\" \"").contains("build: command"));
    assert!(bad("\"Cargo.toml\"", "\"src/[\"").contains("build: watch"));
    assert!(bad("\"Cargo.toml\"", "\"\"").contains("watch patterns"));
    assert!(bad("watch", "shell").contains("unknown field"));
  }

  #[test]
  fn the_build_is_read_when_the_rest_is_invalid() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join(MANIFEST_FILE), format!("{VALID}{BUILD}")).expect("write");
    // The entry point is what the build makes.
    assert!(load(dir.path(), &editor(), true).is_err());
    let plan = read_build_plan(dir.path())
      .expect("readable")
      .expect("a build");
    assert_eq!(plan.id.as_deref(), Some("example.tidy"));
    assert_eq!(plan.name.as_deref(), Some("Tidy"));
    assert_eq!(plan.entrypoint, Some(dir.path().join("tidy.wasm")));
    assert_eq!(plan.build.command, "cargo");

    std::fs::write(dir.path().join(MANIFEST_FILE), VALID).expect("write");
    assert_eq!(read_build_plan(dir.path()).expect("readable"), None);
    std::fs::write(dir.path().join(MANIFEST_FILE), "[build]\nargs = []\n").expect("write");
    let error = read_build_plan(dir.path()).expect_err("no command");
    assert!(error.to_string().contains("build:"), "{error}");
    std::fs::write(dir.path().join(MANIFEST_FILE), "id = ").expect("write");
    assert!(read_build_plan(dir.path()).is_err(), "not TOML");
  }

  #[test]
  fn confines_entrypoints_unless_in_development() {
    let outside = VALID.replace("tidy.wasm", "../tidy.wasm");
    assert!(error_of(&outside).contains("inside the extension's folder"));
    assert!(parse(&outside, Path::new("/ext"), &editor(), true).is_ok());
    let absolute = VALID.replace("\"tidy.wasm\"", "\"/tmp/tidy.wasm\"");
    assert!(error_of(&absolute).contains("relative"));
  }

  #[test]
  fn checks_configuration_defaults() {
    let wrong_type = VALID.replace("default = 100", "default = \"wide\"");
    assert!(error_of(&wrong_type).contains("configuration.width"));
    let out_of_range = VALID.replace("default = 100", "default = -1");
    assert!(error_of(&out_of_range).contains("at least 0"));
    let unknown = VALID.replace("type = \"integer\"", "type = \"color\"");
    assert!(error_of(&unknown).contains("unknown type"));
  }

  const WORKBENCH: &str = r##"
[[keybindings]]
command = "example.tidy.trim"
key = "Shift+Ctrl+T"
mac = "mod+shift+t"
when = "editorFocus"

[[menus]]
location = "editor/context"
command = "example.tidy.trim"

[[status_items]]
id = "count"
text = "3 problems"
command = "example.tidy.trim"

[[views]]
id = "problems"
title = "Problems"
icon = "check"

[decorations.todo]
background = "#E5C07B33"
underline = "dotted"
[decorations.todo.light]
background = "#98680122"

[[previews]]
id = "report"
icon = "bug"

[[previews]]
id = "summary"

[[viewers]]
id = "image"
title = "Image Viewer"
patterns = ["*.png", "*.{jpg,jpeg}"]
command = "example.tidy.trim"

[[viewers]]
id = "vector"
title = "Vector Viewer"
patterns = ["*.svg"]
command = "example.tidy.trim"
priority = "option"
"##;

  #[test]
  fn parses_workbench_contributions() {
    let manifest = parse_text(&format!("{VALID}{WORKBENCH}")).expect("valid");
    let contributions = &manifest.contributions;
    let binding = &contributions.keybindings[0];
    assert_eq!(binding.key, "ctrl+shift+t");
    assert_eq!(binding.mac.as_deref(), Some("mod+shift+t"));
    assert!(binding.editor_focus);
    assert_eq!(
      contributions.menus[0].location,
      crate::contributions::MenuLocation::EditorContext
    );
    let item = &contributions.status_items[0];
    assert_eq!(item.alignment, crate::contributions::Alignment::Left);
    assert_eq!(item.text.as_deref(), Some("3 problems"));
    assert_eq!(contributions.views[0].full_id, "example.tidy.problems");
    let todo = &contributions.decorations[0];
    assert_eq!(todo.key, "example.tidy/todo");
    assert_eq!(todo.dark.background.as_deref(), Some("#e5c07b33"));
    assert_eq!(todo.light.background.as_deref(), Some("#98680122"));
    assert_eq!(contributions.previews[0].full_id, "example.tidy.report");
    assert_eq!(contributions.previews[0].icon, "bug");
    assert_eq!(contributions.previews[1].icon, "book", "the default");
    let viewer = &contributions.viewers[0];
    assert_eq!(viewer.full_id, "example.tidy.image");
    assert_eq!(viewer.patterns, ["*.png", "*.{jpg,jpeg}"]);
    assert_eq!(
      viewer.priority,
      crate::contributions::ViewerPriority::Default
    );
    assert_eq!(
      contributions.viewers[1].priority,
      crate::contributions::ViewerPriority::Option
    );
  }

  const LAYOUTS: &str = r##"
[[layouts]]
id = "diff"
title = "Diff"
command = "example.tidy.trim"

[[layouts]]
id = "preview"
title = "HTML Preview"
command = "example.tidy.trim"
on_message = "example.tidy.trim"
patterns = ["*.html"]
priority = "default"
icon = "preview"

[[keybindings]]
command = "example.tidy.trim"
key = "f7"
layouts = ["diff"]

[decorations.inserted]
background = "diff-inserted-line"
whole_line = true
"##;

  #[test]
  fn parses_layouts() {
    let manifest = parse_text(&format!("{VALID}{LAYOUTS}")).expect("valid");
    let contributions = &manifest.contributions;
    let diff = &contributions.layouts[0];
    assert_eq!(diff.full_id, "example.tidy.diff");
    assert!(diff.patterns.is_empty());
    assert_eq!(
      diff.priority,
      crate::contributions::ViewerPriority::Option,
      "the default"
    );
    let preview = &contributions.layouts[1];
    assert_eq!(preview.patterns, ["*.html"]);
    assert_eq!(
      preview.priority,
      crate::contributions::ViewerPriority::Default
    );
    assert_eq!(preview.icon.as_deref(), Some("preview"));
    assert_eq!(preview.on_message.as_deref(), Some("example.tidy.trim"));
    assert_eq!(contributions.keybindings[0].layouts, ["example.tidy.diff"]);
    assert_eq!(
      contributions.decorations[0].dark.background.as_deref(),
      Some("diff-inserted-line"),
      "a theme token"
    );
  }

  const LINKS: &str = r##"
[[link_handlers]]
id = "tab"
title = "Open in a Web Tab"
kind = "web"
command = "example.tidy.trim"

[[link_handlers]]
id = "pictures"
title = "Show the Picture"
kind = "file"
patterns = ["*.png"]
command = "example.tidy.trim"
"##;

  #[test]
  fn parses_link_handlers_and_web_pages() {
    let text = format!(
      "{}{LINKS}{}",
      VALID.replace(
        "documents_read = true",
        "documents_read = true\nweb_pages = true"
      ),
      LAYOUTS.replace(
        "on_message = \"example.tidy.trim\"",
        "on_message = \"example.tidy.trim\"\non_navigate = \"example.tidy.trim\""
      )
    );
    let manifest = parse_text(&text).expect("valid");
    assert!(manifest.permissions.contains(&Permission::WebPages));
    let handlers = &manifest.contributions.link_handlers;
    assert_eq!(handlers[0].full_id, "example.tidy.tab");
    assert_eq!(handlers[0].kind, crate::contributions::LinkKind::Web);
    assert_eq!(handlers[0].schemes, ["https", "http"], "the default");
    assert_eq!(handlers[1].patterns, ["*.png"]);
    assert_eq!(
      manifest.contributions.layouts[1].on_navigate.as_deref(),
      Some("example.tidy.trim")
    );
  }

  #[test]
  fn rejects_invalid_link_handlers() {
    let with = |old: &str, new: &str| error_of(&format!("{VALID}{}", LINKS.replacen(old, new, 1)));
    assert!(with("kind = \"web\"", "kind = \"ftp\"").contains("must be \"web\" or \"file\""));
    assert!(
      with("kind = \"web\"", "kind = \"web\"\nschemes = [\"ftp\"]")
        .contains("must be https, http, or mailto")
    );
    assert!(
      with("kind = \"web\"", "kind = \"web\"\npatterns = [\"*.png\"]")
        .contains("patterns only apply to file links")
    );
    assert!(
      with("kind = \"file\"", "kind = \"file\"\nschemes = [\"https\"]")
        .contains("schemes only apply to web links")
    );
  }

  #[test]
  fn rejects_invalid_layouts() {
    let with =
      |old: &str, new: &str| error_of(&format!("{VALID}{}", LAYOUTS.replacen(old, new, 1)));
    assert!(
      with("command = \"example.tidy.trim\"", "command = \"other.x\"").contains("layouts: diff")
    );
    assert!(with("layouts = [\"diff\"]", "layouts = [\"nope\"]").contains("not declared"));
    assert!(with("icon = \"preview\"", "icon = \"nope\"").contains("named icons"));
    assert!(with("\"diff-inserted-line\"", "\"not-a-token\"").contains("theme color token"));
    assert!(
      with(
        "command = \"example.tidy.trim\"\n\n[[layouts]]",
        "command = \"example.tidy.trim\"\npriority = \"default\"\n\n[[layouts]]"
      )
      .contains("priority only applies")
    );
  }

  #[test]
  fn rejects_invalid_workbench_contributions() {
    let with =
      |old: &str, new: &str| error_of(&format!("{VALID}{}", WORKBENCH.replacen(old, new, 1)));
    assert!(
      with(
        "command = \"example.tidy.trim\"\nkey",
        "command = \"other.x\"\nkey"
      )
      .contains("not one of")
    );
    assert!(with("Shift+Ctrl+T", "hyper+t").contains("unknown modifier"));
    assert!(with("editor/context", "title/bar").contains("location"));
    assert!(with("icon = \"check\"", "icon = \"skull\"").contains("icon"));
    assert!(with("#E5C07B33", "red").contains("hex color"));
    assert!(with("underline = \"dotted\"", "underline = \"zigzag\"").contains("underline"));
    assert!(with("icon = \"bug\"", "icon = \"globe\"").contains("previews: icon"));
    assert!(with("id = \"summary\"", "id = \"report\"").contains("declared twice"));
    assert!(with("id = \"summary\"", "id = \"Sum mary\"").contains("previews: id"));
    assert!(with("[\"*.svg\"]", "[]").contains("patterns must list"));
    assert!(with("[\"*.svg\"]", "[\"img/*.svg\"]").contains("pattern \"img/*.svg\""));
    assert!(with("\"option\"", "\"always\"").contains("priority"));
    assert!(
      with(
        "command = \"example.tidy.trim\"
priority",
        "command = \"other.open\"
priority"
      )
      .contains("not one of")
    );
    let view = VALID.replace("\"on-startup\"", "\"on-view:missing\"");
    assert!(error_of(&view).contains("view that is not declared"));
  }

  #[test]
  fn parses_webviews() {
    let webviews = format!(
      r#"{VALID}
[[viewers]]
id = "image"
title = "Image Viewer"
patterns = ["*.png"]
webview = "media\\viewer.html"
on_message = "example.tidy.trim"

[[previews]]
id = "graph"
webview = true
"#
    );
    let manifest = parse_text(&webviews).expect("valid");
    let viewer = &manifest.contributions.viewers[0];
    assert_eq!(viewer.command, None);
    let webview = viewer.webview.as_ref().expect("a webview");
    assert_eq!(webview.page.as_deref(), Some("media/viewer.html"));
    assert_eq!(webview.on_message.as_deref(), Some("example.tidy.trim"));
    let preview = manifest.contributions.previews[0].webview.as_ref();
    assert_eq!(preview.and_then(|webview| webview.page.clone()), None);

    let with = |old: &str, new: &str| error_of(&webviews.replacen(old, new, 1));
    assert!(with("media\\\\viewer.html", "../viewer.html").contains("inside the extension"));
    assert!(with("media\\\\viewer.html", "viewer.js").contains("must be an HTML file"));
    assert!(with("\"example.tidy.trim\"\n\n", "\"other.x\"\n\n").contains("not one of"));
    assert!(
      with(
        "webview = true",
        "webview = false\non_message = \"example.tidy.trim\""
      )
      .contains("on_message and retain_when_hidden need a webview")
    );
    let neither = webviews.replacen(
      "webview = \"media\\\\viewer.html\"\non_message = \"example.tidy.trim\"\n",
      "",
      1,
    );
    assert!(error_of(&neither).contains("needs a command, a webview, or both"));

    // A webview that needs no code needs no entrypoint.
    let page_only = r#"
id = "example.pages"
name = "Pages"
version = "1.0.0"
api_version = "1"

[[viewers]]
id = "page"
title = "Page"
patterns = ["*.txt"]
webview = "page.html"
"#;
    assert!(parse_text(page_only).is_ok());
    let messaging = page_only.replace("page.html\"", "page.html\"\non_message = \"x.y\"");
    assert!(parse_text(&messaging).is_err());
  }

  #[test]
  fn icon_theme_extensions_may_omit_the_entrypoint() {
    let icons = r#"
id = "example.icons"
name = "Icons"
version = "1.0.0"
api_version = "1"

[[icon_themes]]
id = "colorful"
label = "Colorful"
path = "icons/theme.toml"
"#;
    let manifest = parse_text(icons).expect("valid");
    assert!(manifest.entrypoint.is_none());
    let theme = &manifest.contributions.icon_themes[0];
    assert_eq!(theme.id, "example.icons.colorful");
    assert_eq!(theme.label, "Colorful");
    let escaping = icons.replace("icons/theme.toml", "../theme.toml");
    assert!(error_of(&escaping).contains("inside the extension's folder"));
    let twice = format!(
      "{icons}
[[icon_themes]]
id = \"colorful\"
label = \"Again\"
path = \"a.toml\"
"
    );
    assert!(error_of(&twice).contains("declared twice"));
  }

  #[test]
  fn only_theme_extensions_may_omit_the_entrypoint() {
    let themes = r#"
id = "example.themes"
name = "Themes"
version = "1.0.0"
api_version = "1"

[[themes]]
id = "night"
label = "Night"
kind = "dark"
path = "themes/night.toml"
"#;
    let manifest = parse_text(themes).expect("valid");
    assert!(manifest.entrypoint.is_none());
    assert_eq!(manifest.contributions.themes[0].id, "example.themes.night");
    let escaping = themes.replace("themes/night.toml", "../night.toml");
    assert!(error_of(&escaping).contains("inside the extension's folder"));
    let code = VALID.replace("entrypoint = \"tidy.wasm\"\n", "");
    assert!(error_of(&code).contains("entrypoint is required"));
  }

  const LANGUAGES: &str = r#"
id = "example.web"
name = "Web"
version = "1.0.0"
api_version = "1"

[permissions]
language_servers = true

[[languages]]
id = "html"
name = "HTML"
file_extensions = ["html", "HTM"]
grammar = "grammars/tree-sitter-html.wasm"
highlights = "queries/html/highlights.scm"
injections = "queries/html/injections.scm"
folds = "queries/html/folds.scm"
block_comment = ["<!--", "-->"]

[[languages]]
id = "objective-c"
name = "Objective-C"
file_names = ["Podfile"]
grammar = "grammars/objc.wasm"
highlights = "queries/objc.scm"
line_comment = "//"

[[language_servers]]
name = "vscode-html-language-server"
command = "vscode-html-language-server"
args = ["--stdio"]
languages = ["html"]
root_markers = ["package.json"]
"#;

  #[test]
  fn language_extensions_need_no_entrypoint() {
    let manifest = parse_text(LANGUAGES).expect("valid");
    assert!(manifest.entrypoint.is_none());
    assert_eq!(manifest.permissions, vec![Permission::LanguageServers]);
    let html = &manifest.contributions.languages[0];
    assert_eq!(html.id, "html");
    assert_eq!(html.file_extensions, ["html", "htm"]);
    assert_eq!(html.grammar_name, "html");
    assert_eq!(
      html.injections.as_deref(),
      Some(Path::new("queries/html/injections.scm"))
    );
    assert_eq!(
      html.folds.as_deref(),
      Some(Path::new("queries/html/folds.scm"))
    );
    assert_eq!(html.line_comment, None);
    assert_eq!(
      html.block_comment,
      Some(("<!--".to_owned(), "-->".to_owned()))
    );
    let objc = &manifest.contributions.languages[1];
    assert_eq!(objc.grammar_name, "objective_c", "derived from the id");
    assert_eq!(objc.line_comment.as_deref(), Some("//"));
    assert_eq!(objc.file_names, ["Podfile"]);
    let server = &manifest.contributions.language_servers[0];
    assert_eq!(server.command_line(), "vscode-html-language-server --stdio");
    assert_eq!(server.root_markers, ["package.json"]);
  }

  #[test]
  fn rejects_invalid_languages() {
    let with = |old: &str, new: &str| error_of(&LANGUAGES.replacen(old, new, 1));
    assert!(with("id = \"html\"", "id = \"HTML\"").contains("lowercase"));
    assert!(with("\"HTM\"", "\".htm\"").contains("without the leading dot"));
    assert!(with("\"HTM\"", "\"a/b\"").contains("file extension"));
    assert!(with("\"Podfile\"", "\"dir/Podfile\"").contains("without a folder"));
    assert!(with("grammars/tree-sitter-html.wasm", "../html.wasm").contains("inside"));
    assert!(with("grammars/tree-sitter-html.wasm", "html.so").contains(".wasm"));
    assert!(with("queries/html/injections.scm", "/etc/x.scm").contains("inside"));
    assert!(with("queries/html/folds.scm", "../folds.scm").contains("inside"));
    assert!(with("id = \"objective-c\"", "id = \"html\"").contains("declared twice"));
    assert!(with("line_comment = \"//\"", "line_comment = \"# \"").contains("line_comment"));
    assert!(with("\"<!--\", \"-->\"", "\"<!--\", \"\"").contains("block_comment"));
    let name = LANGUAGES.replacen(
      "highlights = \"queries/objc.scm\"",
      "highlights = \"queries/objc.scm\"\ngrammar_name = \"Objective C\"",
      1,
    );
    assert!(error_of(&name).contains("grammar_name"));
  }

  #[test]
  fn language_servers_need_their_permission_and_valid_commands() {
    let with = |old: &str, new: &str| error_of(&LANGUAGES.replacen(old, new, 1));
    assert!(
      with("language_servers = true", "language_servers = false")
        .contains("permissions.language_servers")
    );
    let only_permission = LANGUAGES
      .split("[[language_servers]]")
      .next()
      .expect("head");
    assert!(error_of(only_permission).contains("permissions.language_servers"));
    assert!(
      with(
        "command = \"vscode-html-language-server\"",
        "command = \"/usr/bin/server\""
      )
      .contains("inside")
    );
    assert!(
      with(
        "command = \"vscode-html-language-server\"",
        "command = \"../server\""
      )
      .contains("inside")
    );
    let shipped = LANGUAGES.replacen(
      "command = \"vscode-html-language-server\"",
      "command = \"bin/server\"",
      1,
    );
    assert!(parse_text(&shipped).is_ok(), "a server inside the folder");
    assert!(with("languages = [\"html\"]", "languages = []").contains("must not be empty"));
    assert!(with("[\"package.json\"]", "[\"a/package.json\"]").contains("without a folder"));
    assert!(
      with(
        "name = \"vscode-html-language-server\"",
        "name = \"two words\""
      )
      .contains("name")
    );
    // Code permissions still need code.
    let read = LANGUAGES.replacen(
      "language_servers = true",
      "language_servers = true\ndocuments_read = true",
      1,
    );
    assert!(error_of(&read).contains("entrypoint is required"));
  }

  #[test]
  fn language_files_must_exist() {
    let dir = tempfile::tempdir().expect("temp dir");
    let manifest = LANGUAGES
      .split("[[languages]]\nid = \"objective-c\"")
      .next()
      .expect("html only")
      .to_owned()
      + &LANGUAGES[LANGUAGES.find("[[language_servers]]").expect("servers")..];
    std::fs::write(dir.path().join(MANIFEST_FILE), &manifest).expect("writes");
    let error = load(dir.path(), &editor(), false).expect_err("files are missing");
    assert!(
      error.to_string().contains("tree-sitter-html.wasm"),
      "{error}"
    );
    for file in [
      "grammars/tree-sitter-html.wasm",
      "queries/html/highlights.scm",
      "queries/html/injections.scm",
      "queries/html/folds.scm",
    ] {
      let path = dir.path().join(file);
      std::fs::create_dir_all(path.parent().expect("parent")).expect("creates");
      std::fs::write(path, "").expect("writes");
    }
    assert!(load(dir.path(), &editor(), false).is_ok());
  }
}
