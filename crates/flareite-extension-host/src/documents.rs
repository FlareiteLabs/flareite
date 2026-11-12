//! The host functions of the `documents` interface (API 1.8): read-only
//! documents extensions provide for addresses with their schemes, and
//! saving documents (1.15). Diff and merge tabs (1.8, 1.12) are layouts
//! since 1.15; their functions fail.
//!
//! When the editor needs the text at an address, the host runs the
//! provider's command ([`crate::ExtensionHost::provide_text`]); the command
//! answers with `provide`, which the host keeps until the request reads it.
//! An extension can answer, open, and announce changes only for its own
//! schemes.

use flareite_core::DocumentId;

use crate::Notification;
use crate::bindings::flareite::extension::documents::Host;
use crate::bindings::flareite::extension::types::{ApiError, Permission as WitPermission};
use crate::runtime::StoreState;

/// Largest text an extension provides.
pub const MAX_PROVIDED_BYTES: usize = 32 * 1024 * 1024;
const MAX_URI_BYTES: usize = 4 * 1024;

/// An address's scheme: what's before its first `:`, if it's a valid
/// scheme.
pub fn scheme_of(uri: &str) -> Option<&str> {
  let (scheme, _) = uri.split_once(':')?;
  crate::contributions::valid_scheme(scheme).then_some(scheme)
}

fn invalid(message: String) -> ApiError {
  ApiError::InvalidArgument(message)
}

impl StoreState {
  /// Checks that `uri` is an address with one of the extension's schemes.
  pub(crate) fn own_uri(&self, uri: &str) -> Result<(), ApiError> {
    if uri.len() > MAX_URI_BYTES || uri.contains(char::is_control) {
      return Err(invalid(
        "addresses are at most 4 KiB, without control characters".into(),
      ));
    }
    let own = scheme_of(uri).is_some_and(|scheme| {
      self
        .api
        .manifest()
        .contributions
        .text_provider(scheme)
        .is_some()
    });
    if own {
      Ok(())
    } else {
      Err(invalid(format!(
        "{uri:?} isn't an address with one of this extension's schemes ([[text_providers]])"
      )))
    }
  }
}

impl Host for StoreState {
  fn changed(&mut self, uris: Vec<String>) -> Result<(), ApiError> {
    if uris.len() > 1_000 {
      return Err(ApiError::TooLarge("at most 1,000 addresses at once".into()));
    }
    for uri in &uris {
      self.own_uri(uri.trim_end_matches('*'))?;
    }
    if !uris.is_empty() {
      self
        .api
        .shared()?
        .embedder
        .notify(Notification::ProvidedChanged(uris));
    }
    Ok(())
  }

  fn open(&mut self, uri: String) -> Result<(), ApiError> {
    self.own_uri(&uri)?;
    self
      .api
      .shared()?
      .embedder
      .notify(Notification::OpenProvided(uri));
    Ok(())
  }

  fn save(&mut self, id: u64) -> Result<(), ApiError> {
    let shared = self.api.require(WitPermission::DocumentsWrite)?;
    let document = DocumentId::from_raw(id);
    let has_file = crate::api::with_editor(&shared, |editor| {
      editor
        .document(document)
        .map(|document| document.path().is_some())
    })
    .ok_or_else(|| invalid("the editor isn't available".into()))?
    .map_err(|_| ApiError::UnknownDocument)?;
    if !has_file {
      return Err(invalid("the document has no file to save to".into()));
    }
    shared.embedder.save(document).map_err(invalid)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn schemes_are_read_from_addresses() {
    assert_eq!(scheme_of("git:HEAD:src/main.rs"), Some("git"));
    assert_eq!(scheme_of("git::src/main.rs"), Some("git"));
    assert_eq!(scheme_of("C:/repo/main.rs"), None, "a drive isn't a scheme");
    assert_eq!(scheme_of("file:/a"), None, "reserved");
    assert_eq!(scheme_of("no scheme"), None);
  }
}
