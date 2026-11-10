//! Configuring the language servers an extension declares (API 1.16): their
//! arguments and settings, as its code sets them with the
//! `language-servers` interface, over its manifest's. They last until the
//! extension stops. Flareite runs the servers; see
//! [`crate::ExtensionHost::language_servers`].

use crate::bindings::flareite::extension::language_servers::Host;
use crate::bindings::flareite::extension::types::ApiError;
use crate::contributions::{check_server_settings, downloads_in};
use crate::permissions::Permission;
use crate::runtime::StoreState;

/// Most arguments a server takes, and characters in each.
const MAX_ARGS: usize = 32;
const MAX_ARG_CHARS: usize = 1000;

/// What an extension's code changed about one of its servers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerChanges {
  pub args: Option<Vec<String>>,
  pub settings: Option<serde_json::Value>,
}

/// Checks a server's arguments as its code sets them.
fn check_args(args: &[String]) -> Result<(), ApiError> {
  if args.len() > MAX_ARGS {
    return Err(ApiError::TooLarge(format!(
      "a language server takes at most {MAX_ARGS} arguments"
    )));
  }
  for arg in args {
    if arg.chars().count() > MAX_ARG_CHARS || arg.contains('\0') {
      return Err(ApiError::InvalidArgument(format!(
        "arguments must be at most {MAX_ARG_CHARS} characters"
      )));
    }
    downloads_in(arg).map_err(ApiError::InvalidArgument)?;
  }
  Ok(())
}

fn parse_settings(settings: &str) -> Result<serde_json::Value, ApiError> {
  let value: serde_json::Value = serde_json::from_str(settings)
    .map_err(|error| ApiError::InvalidArgument(format!("the settings aren't JSON: {error}")))?;
  check_server_settings(&value).map_err(|error| {
    if error.contains("KiB") {
      ApiError::TooLarge(format!("the settings {error}"))
    } else {
      ApiError::InvalidArgument(format!("the settings {error}"))
    }
  })?;
  Ok(value)
}

impl StoreState {
  /// Checks that the extension may configure `server`: it declares it, and
  /// may declare language servers.
  fn own_server(&self, server: &str) -> Result<std::sync::Arc<crate::host::Shared>, ApiError> {
    if !self.api.has(Permission::LanguageServers) {
      return Err(ApiError::InvalidArgument(
        "the language_servers permission isn't granted".into(),
      ));
    }
    let declared = self
      .api
      .manifest()
      .contributions
      .language_servers
      .iter()
      .any(|declared| declared.name == server);
    if !declared {
      return Err(ApiError::InvalidArgument(format!(
        "the manifest declares no language server named {server:?}"
      )));
    }
    self.api.shared()
  }
}

impl Host for StoreState {
  fn set_arguments(&mut self, server: String, args: Vec<String>) -> Result<(), ApiError> {
    check_args(&args)?;
    let shared = self.own_server(&server)?;
    shared.change_server(self.api.extension_id(), &server, |changes| {
      changes.args = Some(args);
    });
    Ok(())
  }

  fn set_settings(&mut self, server: String, settings: String) -> Result<(), ApiError> {
    let settings = parse_settings(&settings)?;
    let shared = self.own_server(&server)?;
    shared.change_server(self.api.extension_id(), &server, |changes| {
      changes.settings = Some(settings);
    });
    Ok(())
  }

  fn restart(&mut self, server: String) -> Result<(), ApiError> {
    let shared = self.own_server(&server)?;
    shared.restart_server(&server);
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn arguments_and_settings_are_checked() {
    assert!(
      check_args(&[
        "lsp".into(),
        "--docs=${download:https://a.dev/d.json}".into()
      ])
      .is_ok()
    );
    assert!(matches!(
      check_args(&["${download:file:///etc/passwd}".into()]),
      Err(ApiError::InvalidArgument(_))
    ));
    assert!(matches!(
      check_args(&vec![String::new(); MAX_ARGS + 1]),
      Err(ApiError::TooLarge(_))
    ));
    assert!(matches!(
      check_args(&["x".repeat(MAX_ARG_CHARS + 1)]),
      Err(ApiError::InvalidArgument(_))
    ));

    assert_eq!(
      parse_settings(r#"{"a.b": 1}"#).ok(),
      Some(serde_json::json!({ "a.b": 1 }))
    );
    for bad in ["[1]", "{", "null"] {
      assert!(
        matches!(parse_settings(bad), Err(ApiError::InvalidArgument(_))),
        "{bad}"
      );
    }
    let huge = format!(r#"{{"a": "{}"}}"#, "x".repeat(300 * 1024));
    assert!(matches!(parse_settings(&huge), Err(ApiError::TooLarge(_))));
  }
}
