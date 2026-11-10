//! The host functions of the `secrets` interface (API 1.17): API keys,
//! tokens, and the like, kept in the system's credential store through
//! [`crate::Embedder::secret`] and [`crate::Embedder::set_secret`].
//!
//! An extension names a secret by its key and sees only its own: the
//! store's entry is named [`account`], from the extension's id and the key.
//! Their keys (never values) are listed in the extension's storage, so the
//! user clearing its storage removes them too.

use crate::bindings::flareite::extension::secrets::{Host, SecretError};
use crate::runtime::StoreState;

/// Longest key.
pub const MAX_SECRET_KEY_BYTES: usize = 200;
/// Longest value.
pub const MAX_SECRET_BYTES: usize = 2 * 1024;
/// Most secrets one extension keeps.
pub const MAX_SECRETS: usize = 32;

/// The credential store's name for an extension's secret.
pub fn account(extension: &str, key: &str) -> String {
  format!("{extension}/{key}")
}

fn check_key(key: &str) -> Result<(), SecretError> {
  if key.is_empty() || key.len() > MAX_SECRET_KEY_BYTES {
    return Err(SecretError::InvalidArgument(format!(
      "a key is 1 to {MAX_SECRET_KEY_BYTES} bytes"
    )));
  }
  if key.chars().any(char::is_control) {
    return Err(SecretError::InvalidArgument(
      "a key can't contain control characters".into(),
    ));
  }
  Ok(())
}

fn unavailable(_: crate::bindings::flareite::extension::types::ApiError) -> SecretError {
  SecretError::Unavailable("the extension was stopped".into())
}

impl Host for StoreState {
  fn get(&mut self, key: String) -> Result<Option<String>, SecretError> {
    check_key(&key)?;
    let shared = self.api.shared().map_err(unavailable)?;
    let extension = self.api.extension_id();
    // A secret not listed was never set, or was cleared.
    if !shared.storage.secret_keys(extension).contains(&key) {
      return Ok(None);
    }
    shared
      .embedder
      .secret(&account(extension, &key))
      .map_err(SecretError::Unavailable)
  }

  fn set(&mut self, key: String, value: Option<String>) -> Result<(), SecretError> {
    check_key(&key)?;
    if value
      .as_ref()
      .is_some_and(|value| value.len() > MAX_SECRET_BYTES)
    {
      return Err(SecretError::TooLarge(format!(
        "a secret is at most {} KiB",
        MAX_SECRET_BYTES / 1024
      )));
    }
    let shared = self.api.shared().map_err(unavailable)?;
    let extension = self.api.extension_id();
    let keys = shared.storage.secret_keys(extension);
    if value.is_some() && keys.len() >= MAX_SECRETS && !keys.contains(&key) {
      return Err(SecretError::TooLarge(format!(
        "an extension keeps at most {MAX_SECRETS} secrets"
      )));
    }
    shared
      .embedder
      .set_secret(&account(extension, &key), value.as_deref())
      .map_err(SecretError::Unavailable)?;
    shared
      .storage
      .note_secret(extension, &key, value.is_some())
      .map_err(SecretError::Unavailable)
  }

  fn keys(&mut self) -> Vec<String> {
    self
      .api
      .shared()
      .map(|shared| shared.storage.secret_keys(self.api.extension_id()))
      .unwrap_or_default()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn keys_are_checked_and_accounts_named_by_extension() {
    assert!(check_key("api-key").is_ok());
    assert!(check_key("").is_err());
    assert!(check_key(&"k".repeat(MAX_SECRET_KEY_BYTES + 1)).is_err());
    assert!(check_key("a\nb").is_err());
    assert_eq!(account("flareite.rojo", "api-key"), "flareite.rojo/api-key");
  }
}
