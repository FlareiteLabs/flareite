//! What's installed that the settings' schema describes: extensions'
//! settings, theme color tokens (with their colors in the built-in themes),
//! language servers, terminal profiles, extensions' programs, and commands
//! (which the frontend registers, and tells the backend about).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use flareite_extension_host::contributions::THEME_TOKENS;
use flareite_extension_host::{ConfigType, ConfigValue, ExtensionState};
use flareite_schema::builtin::{ExtensionSettings, Named, Setting, SettingsContext};
use serde::Deserialize;
use tauri::{AppHandle, Manager};

use super::SchemaState;
use crate::extensions::ExtensionsState;
use crate::terminal::TerminalState;

/// The built-in themes' colors, which define every token.
const TOKENS_CSS: &str = include_str!("../../../src/styles/tokens.css");

/// What to say about each theme color token: the comment over it in
/// `tokens.css`, and its colors in the dark and light themes.
static TOKEN_DESCRIPTIONS: LazyLock<BTreeMap<String, String>> =
  LazyLock::new(|| token_descriptions(TOKENS_CSS));

/// A token's declarations in one of `tokens.css`'s theme blocks, with the
/// comment over each group of them.
fn declarations(css: &str, selector: &str) -> BTreeMap<String, (String, Option<String>)> {
  let mut found = BTreeMap::new();
  let Some(start) = css.find(selector) else {
    return found;
  };
  let block = &css[start..];
  let block = &block[..block.find('}').unwrap_or(block.len())];
  let mut comment: Option<String> = None;
  let mut in_comment: Option<String> = None;
  for line in block.lines() {
    let line = line.trim();
    if let Some(text) = &mut in_comment {
      text.push(' ');
      text.push_str(line.trim_end_matches("*/").trim());
      if line.ends_with("*/") {
        comment = in_comment.take();
      }
      continue;
    }
    if let Some(rest) = line.strip_prefix("/*") {
      if let Some(text) = rest.strip_suffix("*/") {
        comment = Some(text.trim().to_owned());
      } else {
        in_comment = Some(rest.trim().to_owned());
      }
      continue;
    }
    if line.is_empty() {
      // A comment covers the group of declarations under it.
      comment = None;
      continue;
    }
    if let Some(rest) = line.strip_prefix("--color-")
      && let Some((token, value)) = rest.split_once(':')
    {
      let value = value.trim().trim_end_matches(';').trim().to_owned();
      found.insert(token.trim().to_owned(), (value, comment.clone()));
    }
  }
  found
}

/// A token's value as written for people: another token's name, or a color.
fn shown(value: &str) -> String {
  match value
    .strip_prefix("var(--color-")
    .and_then(|rest| rest.strip_suffix(')'))
  {
    Some(token) => format!("as {token}"),
    None => format!("`{value}`"),
  }
}

fn token_descriptions(css: &str) -> BTreeMap<String, String> {
  let dark = declarations(css, "[data-theme='dark']");
  let light = declarations(css, "[data-theme='light']");
  dark
    .into_iter()
    .map(|(token, (dark_value, comment))| {
      // The light theme inherits what it doesn't redefine.
      let (light_value, light_comment) = light
        .get(&token)
        .cloned()
        .unwrap_or_else(|| (dark_value.clone(), None));
      let mut description = comment.or(light_comment).unwrap_or_default();
      if !description.is_empty() && !description.ends_with('.') {
        description.push('.');
      }
      if !description.is_empty() {
        description.push(' ');
      }
      description.push_str(&format!(
        "Dark theme: {}; light theme: {}.",
        shown(&dark_value),
        shown(&light_value)
      ));
      (token, description)
    })
    .collect()
}

/// A command the frontend registered.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
  pub id: String,
  /// Its category and title, as the command palette shows it.
  pub label: String,
  /// Its default keys on this platform, as settings write them.
  #[serde(default)]
  pub keys: Vec<String>,
}

/// Every command, from the frontend, which keeps the registry: the
/// settings' schema describes them under `[keybindings]`. Sent again when
/// commands are registered or removed.
#[tauri::command]
pub fn schemas_set_commands(app: AppHandle, commands: Vec<CommandInfo>) {
  let mut named: Vec<Named> = commands
    .into_iter()
    .map(|command| {
      let keys = if command.keys.is_empty() {
        "No keys by default.".to_owned()
      } else {
        format!(
          "By default: {}.",
          command
            .keys
            .iter()
            .map(|key| format!("`{key}`"))
            .collect::<Vec<_>>()
            .join(", ")
        )
      };
      Named::new(command.id, format!("{}\n\n{keys}", command.label))
    })
    .collect();
  named.sort_by(|a, b| a.name.cmp(&b.name));
  let state = app.state::<SchemaState>();
  let changed = {
    let mut known = state
      .commands
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let changed = *known != named;
    *known = named;
    changed
  };
  if changed {
    state.invalidate(&app);
  }
}

/// What's installed, for the settings' schema.
pub fn settings_context(app: &AppHandle) -> SettingsContext {
  let mut context = SettingsContext {
    commands: app
      .state::<SchemaState>()
      .commands
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone(),
    ..SettingsContext::default()
  };
  context.colors = THEME_TOKENS
    .iter()
    .map(|token| {
      Named::new(
        token.clone(),
        TOKEN_DESCRIPTIONS.get(token).cloned().unwrap_or_default(),
      )
    })
    .collect();
  context.profiles = app
    .state::<TerminalState>()
    .detected_profiles()
    .into_iter()
    .map(|profile| {
      let command = std::iter::once(profile.shell.program.as_str())
        .chain(profile.shell.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
      Named::new(
        profile.name,
        format!(
          "A profile Flareite found: `{command}`. Change it, or hide it with enabled = false."
        ),
      )
    })
    .collect();
  let extensions = app.state::<ExtensionsState>();
  let Some(host) = extensions.host() else {
    return context;
  };
  for status in host.statuses() {
    if matches!(
      status.state,
      ExtensionState::Disabled | ExtensionState::Invalid
    ) {
      continue;
    }
    let contributions = &status.contributions;
    for color in &contributions.colors {
      let what = if color.description.is_empty() {
        format!("A color of {}'s", status.name)
      } else {
        format!(
          "{} ({})",
          color.description.trim_end_matches('.'),
          status.name
        )
      };
      context.colors.push(Named::new(
        color.id.clone(),
        format!(
          "{what}. Dark theme: `{}`; light theme: `{}`.",
          color.dark, color.light
        ),
      ));
    }
    for server in &contributions.language_servers {
      context.servers.push(Named::new(
        server.name.clone(),
        format!(
          "The language server {} declares, for {}: `{}`. Change it, or turn it off with enabled = false.",
          status.name,
          server.languages.join(", "),
          server.command_line()
        ),
      ));
    }
    for program in &contributions.programs {
      context.programs.push(Named::new(
        format!("{}.{}", status.id, program.name),
        format!(
          "Where {} finds `{}`, which {} runs, if not on the PATH.",
          status.name, program.command, status.name
        ),
      ));
    }
  }
  context.extensions = extension_settings(host);
  context
}

/// The settings extensions declare.
fn extension_settings(host: &flareite_extension_host::ExtensionHost) -> Vec<ExtensionSettings> {
  host
    .configurations()
    .into_iter()
    .map(|extension| ExtensionSettings {
      id: extension.id,
      name: extension.name,
      settings: extension
        .settings
        .into_iter()
        .map(|setting| {
          let property = setting.property;
          Setting {
            key: setting.key,
            kind: match property.kind {
              ConfigType::Boolean => "boolean",
              ConfigType::Integer => "integer",
              ConfigType::Number => "number",
              ConfigType::String => "string",
            }
            .to_owned(),
            default: match property.default {
              ConfigValue::Boolean(b) => toml::Value::Boolean(b),
              ConfigValue::Integer(i) => toml::Value::Integer(i),
              ConfigValue::Number(n) => toml::Value::Float(n),
              ConfigValue::Text(s) => toml::Value::String(s),
            },
            description: property.description,
            minimum: property.minimum,
            maximum: property.maximum,
            choices: property.choices,
          }
        })
        .collect(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_token_is_described_with_its_colors() {
    let descriptions = token_descriptions(TOKENS_CSS);
    for token in THEME_TOKENS.iter() {
      let description = descriptions
        .get(token)
        .unwrap_or_else(|| panic!("{token} isn't described"));
      assert!(
        description.contains("Dark theme: "),
        "{token}: {description}"
      );
    }
    assert!(
      descriptions["editor-gutter-added"].starts_with("Beside lines changed"),
      "{}",
      descriptions["editor-gutter-added"]
    );
    assert!(
      descriptions["notification-foreground"].contains("Dark theme: as text"),
      "{}",
      descriptions["notification-foreground"]
    );
  }

  #[test]
  fn the_settings_schema_and_the_settings_agree_on_colors() {
    let context = SettingsContext {
      colors: THEME_TOKENS
        .iter()
        .map(|token| Named::new(token.clone(), String::new()))
        .collect(),
      ..SettingsContext::default()
    };
    let schema = flareite_schema::Schema::tosd(
      "settings.tosd",
      &flareite_schema::builtin::settings_schema(&context),
    )
    .expect("schema");
    let check = |text: &str| {
      let value: toml::Value = toml::from_str(text).expect("toml");
      let tree = flareite_schema::document::toml_tree(&value);
      let document = flareite_schema::document::Document::single(tree, "");
      flareite_schema::check(&document, "", &[&schema])
    };
    let every: String = THEME_TOKENS
      .iter()
      .map(|token| format!("{token} = \"#123456\"\n"))
      .collect();
    for text in [
      format!("[workbench.colors]\n{every}"),
      "[workbench.colors]\n\"flareite.git.modified\" = \"#f0c674\"\n".to_owned(),
    ] {
      crate::settings::Settings::parse(&text).expect("the settings accept it");
      assert!(check(&text).is_empty(), "{:?}", check(&text));
    }
    for text in [
      "[workbench.colors]\neditor-backgrund = \"#000000\"\n",
      "[workbench.colors]\neditor-background = \"black\"\n",
    ] {
      assert!(crate::settings::Settings::parse(text).is_err(), "{text}");
      assert!(!check(text).is_empty(), "{text}");
    }
  }

  #[test]
  fn comments_cover_their_group() {
    let css = "[data-theme='dark'] {\n  /* Lanes,\n     by turns. */\n  --color-a: #111;\n  --color-b: #222;\n\n  --color-c: var(--color-a);\n}\n[data-theme='light'] {\n  --color-a: #eee;\n}\n";
    let found = token_descriptions(css);
    assert_eq!(
      found["a"],
      "Lanes, by turns. Dark theme: `#111`; light theme: `#eee`."
    );
    assert_eq!(
      found["b"],
      "Lanes, by turns. Dark theme: `#222`; light theme: `#222`."
    );
    assert_eq!(found["c"], "Dark theme: as a; light theme: as a.");
  }
}
