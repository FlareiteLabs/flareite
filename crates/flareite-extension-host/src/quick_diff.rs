//! The host functions of the `quick-diff` interface (API 1.9): extensions
//! name the originals of files ([`crate::ExtensionHost::original_of`]) and
//! take changes applied to them ([`crate::ExtensionHost::apply_to_original`]).
//! The editor compares, marks, and reverts on its own.

use std::path::Path;

use crate::Notification;
use crate::bindings::flareite::extension::quick_diff::Host;
use crate::bindings::flareite::extension::types::ApiError;
use crate::documents::MAX_PROVIDED_BYTES;
use crate::runtime::StoreState;

fn invalid(message: impl Into<String>) -> ApiError {
  ApiError::InvalidArgument(message.into())
}

impl Host for StoreState {
  fn originals_changed(&mut self, paths: Vec<String>) -> Result<(), ApiError> {
    if paths.len() > 1_000 {
      return Err(ApiError::TooLarge("at most 1,000 files at once".into()));
    }
    if let Some(path) = paths.iter().find(|path| !Path::new(path).is_absolute()) {
      return Err(invalid(format!("{path:?} isn't an absolute path")));
    }
    if self.api.manifest().contributions.quick_diff.is_none() {
      return Err(invalid("the manifest declares no [quick_diff]"));
    }
    self
      .api
      .shared()?
      .embedder
      .notify(Notification::OriginalsChanged(paths));
    Ok(())
  }

  fn apply_selected_ranges(&mut self) -> Result<(), ApiError> {
    if self
      .api
      .manifest()
      .contributions
      .quick_diff
      .as_ref()
      .is_none_or(|quick_diff| quick_diff.apply_title.is_none())
    {
      return Err(invalid("the manifest's [quick_diff] has no apply_title"));
    }
    self
      .api
      .shared()?
      .embedder
      .notify(Notification::ApplySelectedRanges);
    Ok(())
  }
}

/// Checks a text to apply to an original.
pub fn check_applied(text: &str) -> Result<(), String> {
  if text.len() > MAX_PROVIDED_BYTES {
    Err(format!(
      "An original can be at most {} MiB.",
      MAX_PROVIDED_BYTES / 1024 / 1024
    ))
  } else {
    Ok(())
  }
}
