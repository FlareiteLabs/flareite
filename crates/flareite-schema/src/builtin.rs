//! Flareite's own schemas: an extension's manifest (`extension.toml`), the
//! settings (`settings.toml`, made with the settings extensions declare),
//! and TOML Schema itself (for `.tosd` files).

use std::fmt::Write as _;

use crate::nav::{toml_key, toml_string};

/// The schema of `extension.toml`.
pub const EXTENSION_MANIFEST: &str = include_str!("../schemas/extension.tosd");
/// The built-in part of the settings' schema; see [`settings_schema`].
pub const SETTINGS: &str = include_str!("../schemas/settings.tosd");
/// The schema of TOML Schemas, from the TOML Schema project.
pub const TOML_SCHEMA: &str = include_str!("../schemas/toml-schema.tosd");

/// The settings an extension declares.
#[derive(Debug, Clone)]
pub struct ExtensionSettings {
  pub id: String,
  pub name: String,
  pub settings: Vec<Setting>,
}

/// A setting an extension declares.
#[derive(Debug, Clone)]
pub struct Setting {
  pub key: String,
  /// `boolean`, `integer`, `number`, or `string`.
  pub kind: String,
  pub default: toml::Value,
  pub description: String,
  pub minimum: Option<f64>,
  pub maximum: Option<f64>,
  pub choices: Option<Vec<String>>,
}

/// TOML Schema's own properties: a child key spelled like one must be
/// written under `children`.
const PROPERTY_NAMES: &[&str] = &[
  "type",
  "description",
  "format",
  "itemtype",
  "items",
  "oneof",
  "anyof",
  "if",
  "then",
  "else",
  "allof",
  "allowedvalues",
  "pattern",
  "keypattern",
  "optional",
  "min",
  "max",
  "minlength",
  "maxlength",
  "uniqueitems",
  "dependentrequired",
  "mutuallyexclusive",
  "exactlyone",
  "default",
  "deprecated",
  "children",
];

/// A child definition's key in a header.
fn child(name: &str) -> String {
  if PROPERTY_NAMES.contains(&name) {
    format!("children.{}", toml_key(name))
  } else {
    toml_key(name)
  }
}

/// A TOML value on one line.
fn inline(value: &toml::Value) -> String {
  match value {
    toml::Value::String(s) => toml_string(s),
    toml::Value::Float(f) if f.is_finite() && f.fract() == 0.0 => format!("{f:.1}"),
    other => other.to_string(),
  }
}

/// Something installed that settings name by key, and what to say about
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
  pub name: String,
  pub description: String,
}

impl Named {
  pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      description: description.into(),
    }
  }
}

/// What's installed that the settings' schema describes, besides the
/// built-in settings.
#[derive(Debug, Clone, Default)]
pub struct SettingsContext {
  /// The settings each extension declares: `[extensions.settings."<id>"]`.
  pub extensions: Vec<ExtensionSettings>,
  /// Theme color tokens, the workbench's and extensions'
  /// (`<extension id>.<name>`): `[workbench.colors]`.
  pub colors: Vec<Named>,
  /// Language servers extensions declare: `[lsp.servers]`.
  pub servers: Vec<Named>,
  /// Terminal profiles found on this machine: `[terminal.profiles]`.
  pub profiles: Vec<Named>,
  /// Programs extensions run, as `<extension id>.<program>`:
  /// `[extensions.programs]`.
  pub programs: Vec<Named>,
  /// Commands, by id: `[keybindings]`.
  pub commands: Vec<Named>,
}

/// Adds a known key under the table at `table` (a collection, which also
/// takes others), whose values are `kind`.
fn known_key(text: &mut String, table: &str, named: &Named, kind: &str) {
  let _ = writeln!(text, "\n[{table}.{}]", child(&named.name));
  let _ = writeln!(text, "type = \"{kind}\"");
  text.push_str("optional = true\n");
  if !named.description.is_empty() {
    let _ = writeln!(text, "description = {}", toml_string(&named.description));
  }
}

/// The settings' schema: the built-in part, with what's installed: each
/// extension's settings under `[extensions.settings."<id>"]`, and the
/// theme colors, language servers, terminal profiles, programs, and
/// commands other tables are keyed by.
pub fn settings_schema(context: &SettingsContext) -> String {
  let mut text = String::from(SETTINGS);
  let mut seen = std::collections::HashSet::new();
  for (table, list, kind) in [
    ("elements.workbench.colors", &context.colors, "types.color"),
    ("elements.lsp.servers", &context.servers, "types.server"),
    (
      "elements.terminal.profiles",
      &context.profiles,
      "types.profile",
    ),
    ("elements.extensions.programs", &context.programs, "string"),
    ("elements.keybindings", &context.commands, "types.keys"),
  ] {
    for named in list {
      // A key can only be defined once.
      if !named.name.is_empty() && seen.insert((table, named.name.as_str())) {
        known_key(&mut text, table, named, kind);
      }
    }
  }
  let base = "elements.extensions.settings";
  let mut bounded = 0;
  let mut pending_bounds: Vec<(usize, Option<f64>, Option<f64>)> = Vec::new();
  for extension in &context.extensions {
    if extension.settings.is_empty() {
      continue;
    }
    let table = format!("{base}.{}", child(&extension.id));
    let _ = write!(
      text,
      "\n[{table}]\ntype = \"table\"\noptional = true\ndescription = {}\n",
      toml_string(&format!("{}'s settings.", extension.name))
    );
    for setting in &extension.settings {
      let _ = writeln!(text, "[{table}.{}]", child(&setting.key));
      let integer = |n: f64| -> Option<String> {
        (n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15).then(|| format!("{}", n as i64))
      };
      match setting.kind.as_str() {
        "number" if setting.minimum.is_none() && setting.maximum.is_none() => {
          text.push_str("anyof = [\"integer\", \"float\"]\n");
        }
        "number" => {
          // Bounds can't go on a union: each alternative is a named type
          // with them, defined after this table.
          bounded += 1;
          let _ = writeln!(
            text,
            "anyof = [\"types.bounded-{bounded}-integer\", \"types.bounded-{bounded}-float\"]"
          );
          pending_bounds.push((bounded, setting.minimum, setting.maximum));
        }
        kind @ ("boolean" | "integer" | "string") => {
          let _ = writeln!(text, "type = \"{kind}\"");
          if kind == "integer" {
            if let Some(min) = setting.minimum.and_then(integer) {
              let _ = writeln!(text, "min = {min}");
            }
            if let Some(max) = setting.maximum.and_then(integer) {
              let _ = writeln!(text, "max = {max}");
            }
          }
          if kind == "string"
            && let Some(choices) = &setting.choices
            && !choices.is_empty()
          {
            let list: Vec<String> = choices.iter().map(|c| toml_string(c)).collect();
            let _ = writeln!(text, "allowedvalues = [{}]", list.join(", "));
          }
        }
        _ => text.push_str("type = \"any\"\n"),
      }
      text.push_str("optional = true\n");
      let _ = writeln!(text, "default = {}", inline(&setting.default));
      if !setting.description.is_empty() {
        let _ = writeln!(text, "description = {}", toml_string(&setting.description));
      }
    }
  }
  for (index, minimum, maximum) in pending_bounds {
    // Integers compare exactly with float bounds; float bounds need a
    // decimal point to be floats.
    let float = |n: f64| format!("{n:?}");
    for kind in ["integer", "float"] {
      let _ = writeln!(text, "\n[types.bounded-{index}-{kind}]\ntype = \"{kind}\"");
      let bound = |n: f64| match kind {
        "integer" if n.fract() == 0.0 && n.abs() < 9.0e15 => Some(format!("{}", n as i64)),
        "integer" => None,
        _ => n.is_finite().then(|| float(n)),
      };
      if let Some(min) = minimum.and_then(bound) {
        let _ = writeln!(text, "min = {min}");
      } else if kind == "integer"
        && let Some(min) = minimum
      {
        // A fractional bound: the next whole number.
        let _ = writeln!(text, "min = {}", min.ceil() as i64);
      }
      if let Some(max) = maximum.and_then(bound) {
        let _ = writeln!(text, "max = {max}");
      } else if kind == "integer"
        && let Some(max) = maximum
      {
        let _ = writeln!(text, "max = {}", max.floor() as i64);
      }
    }
  }
  text
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::document::{Document, Segment, toml_tree};
  use crate::{Diagnostic, Schema};

  /// Checks TOML `text`'s values against `schema`.
  fn check(text: &str, schema: &Schema) -> Vec<Diagnostic> {
    let value: toml::Value = toml::from_str(text).expect("toml");
    crate::check(&Document::single(toml_tree(&value), ""), "", &[schema])
  }

  /// The keys the schema offers in the table at `path`.
  fn keys(schema: &Schema, path: &[&str]) -> Vec<crate::nav::Property> {
    let path: Vec<Segment> = path.iter().map(|key| Segment::key(*key)).collect();
    let tree = crate::document::Node::table(0..0, Vec::new());
    let mut keys = crate::properties(&[schema], &tree, &path);
    keys.sort_by(|a, b| a.name.cmp(&b.name));
    keys
  }

  #[test]
  fn the_built_in_schemas_load() {
    for (name, text) in [
      ("extension.tosd", EXTENSION_MANIFEST),
      ("settings.tosd", SETTINGS),
      ("toml-schema.tosd", TOML_SCHEMA),
    ] {
      if let Err(error) = Schema::tosd(name, text) {
        panic!("{name}: {error} at {:?}", error.schema_path);
      }
    }
  }

  #[test]
  fn tosd_files_follow_the_meta_schema() {
    // The meta-schema checking itself takes seconds (its unions are
    // evaluated speculatively), which is why the editor checks .tosd files
    // by loading them, and uses it only to complete and describe.
    let meta = Schema::tosd("toml-schema.tosd", TOML_SCHEMA).expect("meta-schema");
    for (name, text) in [
      ("extension.tosd", EXTENSION_MANIFEST),
      ("settings.tosd", SETTINGS),
    ] {
      let problems = check(text, &meta);
      assert!(problems.is_empty(), "{name}: {problems:?}");
    }
  }

  #[test]
  fn manifests_are_checked() {
    let schema = Schema::tosd("extension.tosd", EXTENSION_MANIFEST).expect("schema");
    let manifest = r#"
id = "example.tidy"
name = "Tidy"
version = "0.1.0"
api_version = "1"
entrypoint = "tidy.wasm"
activation = ["on-startup", "on-language:rust"]

[permissions]
documents_read = true

[[commands]]
id = "example.tidy.trim"
title = "Trim"

[configuration.max_line_length]
type = "integer"
default = 100
minimum = 0
description = "Long lines."

[configuration.severity]
type = "string"
default = "warning"
choices = ["error", "warning"]

[[languages]]
id = "toml"
name = "TOML"
grammar = "grammars/tree-sitter-toml.wasm"
highlights = "queries/highlights.scm"
block_comment = ["/*", "*/"]

[[previews]]
id = "preview"
webview = true

[[schemas]]
files = ["*.tidy.toml"]
path = "schemas/tidy.tosd"
"#;
    let problems = check(manifest, &schema);
    assert!(problems.is_empty(), "{problems:?}");

    let wrong = "id = \"Example\"\nname = \"x\"\nversion = \"1\"\napi_version = \"2\"\ncolour = 1\n\n[[schemas]]\nfiles = [\"a\"]\n";
    let problems = check(wrong, &schema);
    let codes: Vec<&str> = problems.iter().filter_map(|p| p.code.as_deref()).collect();
    for code in ["pattern", "allowedvalues", "unknown-key", "exactlyone"] {
      assert!(codes.contains(&code), "{code}: {problems:?}");
    }
  }

  #[test]
  fn the_settings_schema_has_what_is_installed() {
    let context = SettingsContext {
      colors: vec![
        Named::new("editor-background", "The editor's background."),
        Named::new("border", ""),
        Named::new("flareite.git.modified", "Files with changes."),
      ],
      servers: vec![Named::new("rust-analyzer", "Rust's language server.")],
      profiles: vec![Named::new("Git Bash", "bash.exe --login")],
      programs: vec![Named::new("flareite.git.git", "Git")],
      commands: vec![
        Named::new("file.save", "File: Save"),
        // The same command twice (an extension's, say) is described once.
        Named::new("file.save", "File: Save again"),
        Named::new("type", "A command named like a schema property"),
      ],
      ..SettingsContext::default()
    };
    let text = settings_schema(&context);
    let schema =
      Schema::tosd("settings.tosd", &text).unwrap_or_else(|error| panic!("{error}\n{text}"));
    let good = "[workbench.colors]\neditor-background = \"#101010\"\n\"other.extension.mark\" = \"#fff\"\n[lsp.servers.rust-analyzer]\nenabled = false\n[terminal.profiles.\"Git Bash\"]\nenabled = false\n[extensions.programs]\n\"flareite.git.git\" = \"C:/git.exe\"\n[keybindings]\n\"file.save\" = \"mod+s\"\n\"some.other\" = []\n";
    let problems = check(good, &schema);
    assert!(problems.is_empty(), "{problems:?}");
    let typo = check("[workbench.colors]\neditor-backgrund = \"#000\"\n", &schema);
    assert_eq!(typo.len(), 1, "{typo:?}");
    assert_eq!(
      typo[0].message,
      "Unknown key editor-backgrund. Did you mean editor-background?"
    );
    let colors = keys(&schema, &["workbench", "colors"]);
    let labels: Vec<&str> = colors.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(
      labels,
      ["border", "editor-background", "flareite.git.modified"]
    );
    assert_eq!(
      colors[1].description.as_deref(),
      Some("The editor's background.")
    );
    let tree = crate::document::Node::table(0..0, Vec::new());
    let path = [Segment::key("keybindings"), Segment::key("file.save")];
    let info = crate::info(&[&schema], &tree, &path).expect("info");
    assert_eq!(info.description.as_deref(), Some("File: Save"), "{info:?}");
    assert_eq!(keys(&schema, &["keybindings"]).len(), 2);
  }

  #[test]
  fn the_settings_schema_has_extension_settings() {
    let context = SettingsContext {
      extensions: vec![ExtensionSettings {
        id: "example.tidy".into(),
        name: "Tidy".into(),
        settings: vec![
          Setting {
            key: "max_line_length".into(),
            kind: "integer".into(),
            default: toml::Value::Integer(100),
            description: "Long lines.".into(),
            minimum: Some(0.0),
            maximum: None,
            choices: None,
          },
          Setting {
            key: "type".into(),
            kind: "string".into(),
            default: toml::Value::String("a".into()),
            description: String::new(),
            minimum: None,
            maximum: None,
            choices: Some(vec!["a".into(), "b".into()]),
          },
          Setting {
            key: "ratio".into(),
            kind: "number".into(),
            default: toml::Value::Float(1.0),
            description: String::new(),
            minimum: Some(0.5),
            maximum: Some(2.5),
            choices: None,
          },
        ],
      }],
      ..SettingsContext::default()
    };
    let text = settings_schema(&context);
    let schema =
      Schema::tosd("settings.tosd", &text).unwrap_or_else(|error| panic!("{error}\n{text}"));
    let good = "[editor]\ntab_size = 2\n[extensions.settings.\"example.tidy\"]\nmax_line_length = 80\ntype = \"b\"\nratio = 2\n[extensions.settings.\"other.thing\"]\nanything = 1\n";
    assert!(check(good, &schema).is_empty());
    let bad = "[editor]\ntab_size = 20\n[extensions.settings.\"example.tidy\"]\nmax_line_length = -1\ntype = \"c\"\nnope = 1\nratio = 3\n";
    let problems = check(bad, &schema);
    assert_eq!(problems.len(), 5, "{problems:?}");
    // Numbers have their bounds, as integers and as floats.
    for (ratio, ok) in [
      ("0.5", true),
      ("2", true),
      ("2.6", false),
      ("0", false),
      ("1", true),
    ] {
      let text = format!("[extensions.settings.\"example.tidy\"]\nratio = {ratio}\n");
      assert_eq!(check(&text, &schema).is_empty(), ok, "{ratio}");
    }
    let settings = keys(&schema, &["extensions", "settings", "example.tidy"]);
    let labels: Vec<&str> = settings.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(labels, ["max_line_length", "ratio", "type"]);
  }
}
