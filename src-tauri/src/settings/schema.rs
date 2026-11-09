//! The settings editor's view of the settings: every built-in setting and
//! every setting extensions declare, with its description, type, default,
//! and value in effect, and turning a value from the editor into TOML.
//!
//! The settings editor writes one key of `settings.toml` at a time, so
//! settings that are tables of entries (theme colors, language servers,
//! terminal profiles, keybindings) are listed but edited in the file.

use flareite_extension_host::{ConfigType, ConfigValue, ExtensionConfiguration};
use serde::Serialize;
use serde_json::{Value as Json, json};

use super::{BUILTIN_THEMES, DEFAULT_ICON_THEME, Settings};

/// A built-in setting.
struct Spec {
  path: &'static [&'static str],
  section: &'static str,
  title: &'static str,
  description: &'static str,
  kind: Kind,
  value: fn(&Settings) -> Json,
}

#[derive(Debug, Clone, Copy)]
enum Kind {
  Boolean,
  Integer {
    minimum: Option<i64>,
    maximum: Option<i64>,
  },
  Text,
  /// A string, offered from what's installed. Other values are allowed
  /// (e.g. a theme whose extension isn't loaded yet).
  Choice(Choices),
  /// A list of strings.
  List,
  /// A table of entries, edited in the file; its value is the entries'
  /// names.
  Table,
}

#[derive(Debug, Clone, Copy)]
enum Choices {
  Themes,
  IconThemes,
  TerminalProfiles,
  WebLinkHandlers,
  FileLinkHandlers,
}

const SPECS: &[Spec] = &[
  Spec {
    path: &["editor", "tab_size"],
    section: "Editor",
    title: "Tab Size",
    description: "The width of a tab, in spaces, and how many spaces Tab inserts when inserting spaces.",
    kind: Kind::Integer {
      minimum: Some(1),
      maximum: Some(16),
    },
    value: |s| json!(s.editor.tab_size),
  },
  Spec {
    path: &["editor", "insert_spaces"],
    section: "Editor",
    title: "Insert Spaces",
    description: "Insert spaces when Tab is pressed, instead of a tab character.",
    kind: Kind::Boolean,
    value: |s| json!(s.editor.insert_spaces),
  },
  Spec {
    path: &["editor", "font_size"],
    section: "Editor",
    title: "Font Size",
    description: "The editor's font size, in pixels.",
    kind: Kind::Integer {
      minimum: Some(6),
      maximum: Some(72),
    },
    value: |s| json!(s.editor.font_size),
  },
  Spec {
    path: &["editor", "font_family"],
    section: "Editor",
    title: "Font Family",
    description: "The editor's font. Empty uses the default monospace font.",
    kind: Kind::Text,
    value: |s| json!(s.editor.font_family),
  },
  Spec {
    path: &["editor", "minimap_region_headers"],
    section: "Editor",
    title: "Minimap Region Headers",
    description: "Label named #region comments in the minimap.",
    kind: Kind::Boolean,
    value: |s| json!(s.editor.minimap_region_headers),
  },
  Spec {
    path: &["editor", "minimap_mark_headers"],
    section: "Editor",
    title: "Minimap Mark Headers",
    description: "Label MARK: comments in the minimap.",
    kind: Kind::Boolean,
    value: |s| json!(s.editor.minimap_mark_headers),
  },
  Spec {
    path: &["workbench", "theme"],
    section: "Workbench",
    title: "Color Theme",
    description: "The workbench's colors: following the system, dark, light, or an extension's theme.",
    kind: Kind::Choice(Choices::Themes),
    value: |s| json!(s.workbench.theme.trim()),
  },
  Spec {
    path: &["workbench", "icon_theme"],
    section: "Workbench",
    title: "File Icon Theme",
    description: "The icons of files and folders.",
    kind: Kind::Choice(Choices::IconThemes),
    value: |s| json!(s.workbench.icon_theme.trim()),
  },
  Spec {
    path: &["workbench", "open_web_links"],
    section: "Workbench",
    title: "Open Web Links With",
    description: "Who opens web links you Ctrl+click: an extension's link handler if one is installed (automatic), your browser, or a handler you choose. Ctrl+Shift+click asks.",
    kind: Kind::Choice(Choices::WebLinkHandlers),
    value: |s| json!(s.workbench.open_web_links.trim()),
  },
  Spec {
    path: &["workbench", "open_file_links"],
    section: "Workbench",
    title: "Open File Links With",
    description: "Who opens file paths you Ctrl+click: an extension's link handler if one is installed (automatic), the editor (folders in your file manager), or a handler you choose. Ctrl+Shift+click asks.",
    kind: Kind::Choice(Choices::FileLinkHandlers),
    value: |s| json!(s.workbench.open_file_links.trim()),
  },
  Spec {
    path: &["workbench", "colors"],
    section: "Workbench",
    title: "Color Overrides",
    description: "Theme colors to replace, by token, as hex colors, e.g. editor-background = \"#1b1b1f\".",
    kind: Kind::Table,
    value: |s| json!(s.workbench.colors.keys().collect::<Vec<_>>()),
  },
  Spec {
    path: &["files", "exclude"],
    section: "Files",
    title: "Exclude",
    description: "Hidden from the explorer, Go to File, and search. Patterns without \"/\" match names at any depth; others match paths relative to the folder.",
    kind: Kind::List,
    value: |s| json!(s.files.exclude),
  },
  Spec {
    path: &["search", "max_file_bytes"],
    section: "Search",
    title: "Max File Size",
    description: "Find in files skips files larger than this, in bytes.",
    kind: Kind::Integer {
      minimum: Some(1),
      maximum: None,
    },
    value: |s| json!(s.search.max_file_bytes),
  },
  Spec {
    path: &["lsp", "enabled"],
    section: "Language Servers",
    title: "Enabled",
    description: "Run language servers, which provide diagnostics, hover, go to definition, references, symbols, and completion. Language extensions declare them, and they start when a file needs one, if installed and allowed.",
    kind: Kind::Boolean,
    value: |s| json!(s.lsp.enabled),
  },
  Spec {
    path: &["lsp", "servers"],
    section: "Language Servers",
    title: "Servers",
    description: "Change or turn off a server an extension declares, by its name, or add one for any language an extension adds.",
    kind: Kind::Table,
    value: |s| json!(s.lsp.servers.keys().collect::<Vec<_>>()),
  },
  Spec {
    path: &["terminal", "default_profile"],
    section: "Terminal",
    title: "Default Profile",
    description: "The profile new terminals use.",
    kind: Kind::Choice(Choices::TerminalProfiles),
    value: |s| json!(s.terminal.default_profile),
  },
  Spec {
    path: &["terminal", "scrollback"],
    section: "Terminal",
    title: "Scrollback",
    description: "Lines kept above the screen.",
    kind: Kind::Integer {
      minimum: Some(0),
      maximum: Some(100_000),
    },
    value: |s| json!(s.terminal.scrollback),
  },
  Spec {
    path: &["terminal", "screen_reader"],
    section: "Terminal",
    title: "Screen Reader",
    description: "Announce terminal output to screen readers. This makes terminals slower.",
    kind: Kind::Boolean,
    value: |s| json!(s.terminal.screen_reader),
  },
  Spec {
    path: &["terminal", "profiles"],
    section: "Terminal",
    title: "Profiles",
    description: "Add a profile, or change or hide one that was found, by name.",
    kind: Kind::Table,
    value: |s| json!(s.terminal.profiles.keys().collect::<Vec<_>>()),
  },
  Spec {
    path: &["extensions", "enabled"],
    section: "Extensions",
    title: "Enabled",
    description: "Load WebAssembly extensions.",
    kind: Kind::Boolean,
    value: |s| json!(s.extensions.enabled),
  },
  Spec {
    path: &["extensions", "development"],
    section: "Extensions",
    title: "Development Folders",
    description: "Folders of extensions under development, loaded in place.",
    kind: Kind::List,
    value: |s| json!(s.extensions.development),
  },
  Spec {
    path: &["extensions", "catalog"],
    section: "Extensions",
    title: "Catalog",
    description: "GitHub owners whose extensions (repositories with the \"flareite-extension\" topic) the Extensions view lists to install. Empty lists none.",
    kind: Kind::List,
    value: |s| json!(s.extensions.catalog),
  },
  Spec {
    path: &["extensions", "programs"],
    section: "Extensions",
    title: "Program Paths",
    description: "Where to find a program an extension runs, by \"<extension id>.<program>\" (such as \"flareite.git.git\"), when it isn't on the PATH.",
    kind: Kind::Table,
    value: |s| json!(s.extensions.programs.keys().collect::<Vec<_>>()),
  },
  Spec {
    path: &["security", "workspace_trust"],
    section: "Security",
    title: "Workspace Trust",
    description: "Ask whether you trust a folder's files before running programs on it: language servers, and programs extensions run (such as Git). Turned off, every folder is trusted.",
    kind: Kind::Boolean,
    value: |s| json!(s.security.workspace_trust),
  },
  Spec {
    path: &["keybindings"],
    section: "Keyboard Shortcuts",
    title: "Keybindings",
    description: "Change a command's keys, by command id. \"mod\" is Ctrl on Windows and Linux and Cmd on macOS; \"\" removes them.",
    kind: Kind::Table,
    value: |s| json!(s.keybindings.keys().collect::<Vec<_>>()),
  },
];

/// One setting, as the settings editor shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingDto {
  /// Its keys in `settings.toml`, e.g. `["editor", "tab_size"]`.
  pub path: Vec<String>,
  pub section: String,
  /// The id of the extension that declares it.
  pub extension: Option<String>,
  pub title: String,
  pub description: String,
  #[serde(flatten)]
  pub kind: KindDto,
  pub default: Json,
  pub value: Json,
  /// Whether `settings.toml` sets it.
  pub modified: bool,
  /// Why the value in `settings.toml` isn't used.
  pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum KindDto {
  Boolean,
  Integer {
    minimum: Option<f64>,
    maximum: Option<f64>,
  },
  Number {
    minimum: Option<f64>,
    maximum: Option<f64>,
  },
  /// `choices` are the values offered; for built-in settings, others may be
  /// set in the file.
  String {
    choices: Option<Vec<Choice>>,
  },
  List,
  Table,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
  pub value: String,
  pub label: String,
}

impl Choice {
  pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
    Self {
      value: value.into(),
      label: label.into(),
    }
  }
}

/// What the settings editor needs besides the settings: what's installed.
#[derive(Debug, Default)]
pub struct Installed {
  /// Contributed color themes.
  pub themes: Vec<Choice>,
  /// Contributed file icon themes.
  pub icon_themes: Vec<Choice>,
  /// Contributed link handlers, for web links and for file links.
  pub web_link_handlers: Vec<Choice>,
  pub file_link_handlers: Vec<Choice>,
  /// Terminal profiles by name, and the one used when none is set.
  pub profiles: Vec<String>,
  pub default_profile: Option<String>,
  pub extensions: Vec<ExtensionConfiguration>,
}

/// Every setting: the built-in ones, then each extension's. `settings` are
/// the settings in effect, and `file` what `settings.toml` sets (`None` if
/// it can't be read as TOML).
pub fn describe(
  settings: &Settings,
  file: Option<&toml::Table>,
  installed: &Installed,
) -> Vec<SettingDto> {
  let defaults = Settings::default();
  let builtin = SPECS.iter().map(|spec| SettingDto {
    path: spec.path.iter().map(|&key| key.to_owned()).collect(),
    section: spec.section.to_owned(),
    extension: None,
    title: spec.title.to_owned(),
    description: spec.description.to_owned(),
    kind: match spec.kind {
      Kind::Boolean => KindDto::Boolean,
      #[allow(clippy::cast_precision_loss)]
      Kind::Integer { minimum, maximum } => KindDto::Integer {
        minimum: minimum.map(|n| n as f64),
        maximum: maximum.map(|n| n as f64),
      },
      Kind::Text => KindDto::String { choices: None },
      Kind::Choice(choices) => KindDto::String {
        choices: Some(choices_for(choices, installed)),
      },
      Kind::List => KindDto::List,
      Kind::Table => KindDto::Table,
    },
    default: (spec.value)(&defaults),
    value: (spec.value)(settings),
    modified: match spec.kind {
      // An empty table changes nothing.
      Kind::Table => lookup(file, spec.path.iter().copied()).is_some_and(|value| match value {
        toml::Value::Table(table) => !table.is_empty(),
        _ => true,
      }),
      _ => lookup(file, spec.path.iter().copied()).is_some(),
    },
    problem: None,
  });
  let extensions = installed.extensions.iter().flat_map(|extension| {
    extension.settings.iter().map(|setting| {
      let property = &setting.property;
      SettingDto {
        path: extension_path(&extension.id, &setting.key),
        section: extension.name.clone(),
        extension: Some(extension.id.clone()),
        title: title_of(&setting.key),
        description: property.description.clone(),
        kind: match property.kind {
          ConfigType::Boolean => KindDto::Boolean,
          ConfigType::Integer => KindDto::Integer {
            minimum: property.minimum,
            maximum: property.maximum,
          },
          ConfigType::Number => KindDto::Number {
            minimum: property.minimum,
            maximum: property.maximum,
          },
          ConfigType::String => KindDto::String {
            choices: property.choices.as_ref().map(|choices| {
              choices
                .iter()
                .map(|choice| Choice::new(choice, choice))
                .collect()
            }),
          },
        },
        default: config_json(&property.default),
        value: config_json(&setting.value),
        modified: lookup(
          file,
          ["extensions", "settings", &extension.id, &setting.key].into_iter(),
        )
        .is_some(),
        problem: setting.problem.clone(),
      }
    })
  });
  builtin.chain(extensions).collect()
}

fn choices_for(choices: Choices, installed: &Installed) -> Vec<Choice> {
  match choices {
    Choices::Themes => {
      let labels = ["Follow System", "Dark", "Light"];
      BUILTIN_THEMES
        .iter()
        .zip(labels)
        .map(|(&value, label)| Choice::new(value, label))
        .chain(installed.themes.iter().cloned())
        .collect()
    }
    Choices::IconThemes => std::iter::once(Choice::new(DEFAULT_ICON_THEME, "Default"))
      .chain(installed.icon_themes.iter().cloned())
      .collect(),
    Choices::WebLinkHandlers => [
      Choice::new("auto", "Automatic"),
      Choice::new("default", "Your Browser"),
    ]
    .into_iter()
    .chain(installed.web_link_handlers.iter().cloned())
    .collect(),
    Choices::FileLinkHandlers => [
      Choice::new("auto", "Automatic"),
      Choice::new("default", "The Editor"),
    ]
    .into_iter()
    .chain(installed.file_link_handlers.iter().cloned())
    .collect(),
    Choices::TerminalProfiles => {
      let automatic = match &installed.default_profile {
        Some(name) => format!("Automatic ({name})"),
        None => "Automatic".to_owned(),
      };
      std::iter::once(Choice::new("", automatic))
        .chain(
          installed
            .profiles
            .iter()
            .map(|name| Choice::new(name, name)),
        )
        .collect()
    }
  }
}

/// Where an extension's setting is in `settings.toml`.
pub fn extension_path(id: &str, key: &str) -> Vec<String> {
  ["extensions", "settings", id, key]
    .map(str::to_owned)
    .to_vec()
}

fn lookup<'a, 'k>(
  file: Option<&'a toml::Table>,
  mut path: impl Iterator<Item = &'k str>,
) -> Option<&'a toml::Value> {
  let mut value = file?.get(path.next()?)?;
  for key in path {
    value = value.as_table()?.get(key)?;
  }
  Some(value)
}

/// `max_line_length` or `format.onSave` as a title: "Max Line Length",
/// "Format › On Save".
fn title_of(key: &str) -> String {
  key
    .split('.')
    .map(|part| {
      let mut words: Vec<String> = Vec::new();
      let mut word = String::new();
      for c in part.chars() {
        if c == '_' || c == '-' || c == ' ' {
          words.extend((!word.is_empty()).then(|| std::mem::take(&mut word)));
        } else {
          if c.is_uppercase() && word.chars().last().is_some_and(char::is_lowercase) {
            words.push(std::mem::take(&mut word));
          }
          word.push(c);
        }
      }
      words.extend((!word.is_empty()).then_some(word));
      words
        .iter()
        .map(|word| {
          let mut chars = word.chars();
          chars.next().map_or_else(String::new, |first| {
            first.to_uppercase().chain(chars).collect()
          })
        })
        .collect::<Vec<_>>()
        .join(" ")
    })
    .collect::<Vec<_>>()
    .join(" › ")
}

fn config_json(value: &ConfigValue) -> Json {
  match value {
    ConfigValue::Boolean(b) => json!(b),
    ConfigValue::Integer(i) => json!(i),
    ConfigValue::Number(f) => json!(f),
    ConfigValue::Text(s) => json!(s),
  }
}

/// What can be written to a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
  Boolean,
  Integer,
  Number,
  String,
  List,
}

/// The kind of value the built-in setting at `path` takes. `None` if there's
/// no such setting, or it's edited in the file.
pub fn builtin_kind(path: &[String]) -> Option<ValueKind> {
  let spec = SPECS.iter().find(|spec| spec.path.iter().eq(path.iter()))?;
  match spec.kind {
    Kind::Boolean => Some(ValueKind::Boolean),
    Kind::Integer { .. } => Some(ValueKind::Integer),
    Kind::Text | Kind::Choice(_) => Some(ValueKind::String),
    Kind::List => Some(ValueKind::List),
    Kind::Table => None,
  }
}

pub fn config_kind(kind: ConfigType) -> ValueKind {
  match kind {
    ConfigType::Boolean => ValueKind::Boolean,
    ConfigType::Integer => ValueKind::Integer,
    ConfigType::Number => ValueKind::Number,
    ConfigType::String => ValueKind::String,
  }
}

/// `value` from the settings editor as TOML, or why it isn't a `kind`.
pub fn to_toml(kind: ValueKind, value: &Json) -> Result<toml::Value, String> {
  match (kind, value) {
    (ValueKind::Boolean, Json::Bool(b)) => Ok(toml::Value::Boolean(*b)),
    (ValueKind::Integer, Json::Number(n)) => n
      .as_i64()
      .map(toml::Value::Integer)
      .ok_or_else(|| "must be a whole number".to_owned()),
    (ValueKind::Number, Json::Number(n)) => n
      .as_f64()
      .filter(|f| f.is_finite())
      .map(toml::Value::Float)
      .ok_or_else(|| "must be a number".to_owned()),
    (ValueKind::String, Json::String(s)) => Ok(toml::Value::String(s.clone())),
    (ValueKind::List, Json::Array(items)) => items
      .iter()
      .map(|item| match item {
        Json::String(s) => Ok(toml::Value::String(s.clone())),
        _ => Err("must be a list of strings".to_owned()),
      })
      .collect::<Result<_, _>>()
      .map(toml::Value::Array),
    (kind, _) => Err(
      match kind {
        ValueKind::Boolean => "must be true or false",
        ValueKind::Integer => "must be a whole number",
        ValueKind::Number => "must be a number",
        ValueKind::String => "must be a string",
        ValueKind::List => "must be a list of strings",
      }
      .to_owned(),
    ),
  }
}

/// A value from [`to_toml`] for `toml_edit`.
pub fn to_edit(value: &toml::Value) -> toml_edit::Value {
  match value {
    toml::Value::Boolean(b) => (*b).into(),
    toml::Value::Integer(i) => (*i).into(),
    toml::Value::Float(f) => (*f).into(),
    toml::Value::Array(items) => toml_edit::Value::Array(items.iter().map(to_edit).collect()),
    // Strings; dates and tables aren't produced.
    other => other.as_str().unwrap_or_default().into(),
  }
}

#[cfg(test)]
mod tests {
  use flareite_extension_host::{ConfigProperty, ConfiguredSetting};

  use super::*;

  #[test]
  fn every_builtin_setting_is_in_the_template_and_has_a_kind() {
    for spec in SPECS {
      let (key, tables) = spec.path.split_last().expect("a key");
      let written = if tables.is_empty() {
        format!("[{key}]")
      } else {
        format!("[{}]", tables.join("."))
      };
      assert!(
        super::super::TEMPLATE.contains(&written),
        "{written} is missing"
      );
      let mentioned = format!("# {key} =");
      let table = format!("[{}", spec.path.join("."));
      assert!(
        tables.is_empty()
          || super::super::TEMPLATE.contains(&mentioned)
          || super::super::TEMPLATE.contains(&format!("{table}]"))
          || super::super::TEMPLATE.contains(&format!("{table}.")),
        "{} is missing from the template",
        spec.path.join(".")
      );
      let path: Vec<String> = spec.path.iter().map(|&k| k.to_owned()).collect();
      assert_eq!(
        builtin_kind(&path).is_none(),
        matches!(spec.kind, Kind::Table)
      );
    }
  }

  #[test]
  fn describes_values_defaults_and_what_the_file_sets() {
    let text =
      "[editor]\ntab_size = 2\n[workbench.colors]\n[extensions.settings.\"x.tidy\"]\nlimit = 0\n";
    let settings = Settings::parse(text).expect("valid");
    let file: toml::Table = toml::from_str(text).expect("toml");
    let property = ConfigProperty {
      kind: ConfigType::Integer,
      default: ConfigValue::Integer(3),
      description: "How many.".into(),
      minimum: Some(1.0),
      maximum: None,
      choices: None,
    };
    let installed = Installed {
      themes: vec![Choice::new("x.themes.night", "Night")],
      profiles: vec!["Git Bash".into()],
      default_profile: Some("Git Bash".into()),
      extensions: vec![ExtensionConfiguration {
        id: "x.tidy".into(),
        name: "Tidy".into(),
        settings: vec![ConfiguredSetting {
          key: "limit".into(),
          property: property.clone(),
          value: ConfigValue::Integer(3),
          problem: Some("must be at least 1".into()),
        }],
      }],
      ..Installed::default()
    };
    let described = describe(&settings, Some(&file), &installed);
    let find = |path: &[&str]| {
      described
        .iter()
        .find(|setting| setting.path.iter().eq(path.iter()))
        .expect("described")
    };
    let tab_size = find(&["editor", "tab_size"]);
    assert_eq!(
      (tab_size.value.clone(), tab_size.default.clone()),
      (json!(2), json!(4))
    );
    assert!(tab_size.modified);
    assert!(!find(&["editor", "font_size"]).modified);
    assert!(!find(&["workbench", "colors"]).modified, "an empty table");

    let theme = find(&["workbench", "theme"]);
    let KindDto::String {
      choices: Some(choices),
    } = &theme.kind
    else {
      panic!("theme choices")
    };
    assert_eq!(choices.len(), 4);
    assert_eq!(choices[3], Choice::new("x.themes.night", "Night"));
    let KindDto::String {
      choices: Some(profiles),
    } = &find(&["terminal", "default_profile"]).kind
    else {
      panic!("profile choices")
    };
    assert_eq!(profiles[0], Choice::new("", "Automatic (Git Bash)"));

    let limit = find(&["extensions", "settings", "x.tidy", "limit"]);
    assert_eq!(limit.section, "Tidy");
    assert_eq!(limit.extension.as_deref(), Some("x.tidy"));
    assert_eq!(limit.title, "Limit");
    assert!(limit.modified);
    assert_eq!(limit.problem.as_deref(), Some("must be at least 1"));
    assert_eq!(
      serde_json::to_value(&limit.kind).expect("json"),
      json!({ "type": "integer", "minimum": 1.0, "maximum": null })
    );

    // Unreadable files set nothing.
    assert!(
      describe(&settings, None, &Installed::default())
        .iter()
        .all(|setting| !setting.modified)
    );
  }

  #[test]
  fn titles_come_from_keys() {
    assert_eq!(title_of("max_line_length"), "Max Line Length");
    assert_eq!(title_of("format.onSave"), "Format › On Save");
    assert_eq!(title_of("severity"), "Severity");
  }

  #[test]
  fn converts_values_from_the_editor() {
    assert_eq!(
      to_toml(ValueKind::Integer, &json!(4)),
      Ok(toml::Value::Integer(4))
    );
    assert!(to_toml(ValueKind::Integer, &json!(4.5)).is_err());
    assert!(to_toml(ValueKind::Integer, &json!("4")).is_err());
    assert_eq!(
      to_toml(ValueKind::Number, &json!(2)),
      Ok(toml::Value::Float(2.0))
    );
    assert_eq!(
      to_toml(ValueKind::List, &json!(["a", "b"])).map(|v| to_edit(&v).to_string()),
      Ok("[\"a\", \"b\"]".to_owned())
    );
    assert!(to_toml(ValueKind::List, &json!(["a", 1])).is_err());
    assert!(to_toml(ValueKind::Boolean, &json!(null)).is_err());
  }
}
