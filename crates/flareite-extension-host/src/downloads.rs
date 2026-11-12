//! The host functions of the `downloads` interface: files from
//! the web that an extension's code reads, such as data it builds a schema
//! from. They need the `network` permission.
//!
//! The editor downloads and keeps the files ([`crate::Embedder::download`]),
//! sharing the copies with language servers' `${download:...}` arguments.
//! `download` returns at once; the file is had on a thread of its own, and
//! its content handed to the extension as a `download-finished` event.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::bindings::flareite::extension::downloads::{DownloadedFile, Host};
use crate::bindings::flareite::extension::events::Event as WitEvent;
use crate::bindings::flareite::extension::types::{ApiError, Permission as WitPermission};
use crate::permissions::Permission;
use crate::runtime::StoreState;
use crate::worker::Event;

/// Largest file handed to an extension.
pub const MAX_DOWNLOAD_BYTES: u64 = 32 * 1024 * 1024;
/// Downloads one start of an extension may have under way at once.
pub const MAX_DOWNLOADING: usize = 8;
const MAX_ADDRESS_BYTES: usize = 2_000;

/// The downloads one start of an extension asked for.
#[derive(Default)]
pub struct DownloadTable {
  next: AtomicU64,
  under_way: Arc<AtomicUsize>,
}

/// Checks an address an extension asks for: `https://`, with a host, and
/// nothing that can't be in one.
pub fn check_address(address: &str) -> Result<(), String> {
  if address.len() > MAX_ADDRESS_BYTES {
    return Err(format!(
      "an address is at most {MAX_ADDRESS_BYTES} characters"
    ));
  }
  let Some(rest) = address.strip_prefix("https://") else {
    return Err(format!("{address:?} isn't an https:// address"));
  };
  let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
  if host.is_empty() || host.contains('@') {
    return Err(format!("{address:?} has no host, or has a user name"));
  }
  if address.chars().any(|c| c.is_whitespace() || c.is_control()) {
    return Err(format!("{address:?} contains spaces or control characters"));
  }
  Ok(())
}

impl Host for StoreState {
  fn download(&mut self, address: String) -> Result<u64, ApiError> {
    if !self.api.has(Permission::Network) {
      return Err(ApiError::PermissionDenied(WitPermission::Network));
    }
    check_address(&address).map_err(ApiError::InvalidArgument)?;
    let shared = self.api.shared()?;
    let table = self.api.downloads();
    let under_way = table.under_way.clone();
    if under_way
      .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
        (count < MAX_DOWNLOADING).then_some(count + 1)
      })
      .is_err()
    {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_DOWNLOADING} downloads can be under way at once"
      )));
    }
    let request = table.next.fetch_add(1, Ordering::Relaxed) + 1;
    let (weak, extension, generation) = (
      self.api.weak_shared(),
      self.api.extension_id().to_owned(),
      self.api.generation(),
    );
    let embedder = shared.embedder.clone();
    tracing::debug!(%extension, %address, request, "downloading a file for an extension");
    let work = move || {
      let outcome = embedder.download(&address).and_then(|path| {
        let size = std::fs::metadata(&path)
          .map_err(|error| error.to_string())?
          .len();
        if size > MAX_DOWNLOAD_BYTES {
          return Err(format!(
            "the file is larger than {} MiB",
            MAX_DOWNLOAD_BYTES / 1024 / 1024
          ));
        }
        std::fs::read(&path).map_err(|error| error.to_string())
      });
      under_way.fetch_sub(1, Ordering::Relaxed);
      if let Some(shared) = weak.upgrade() {
        shared.deliver(
          &extension,
          generation,
          Event::notice(WitEvent::DownloadFinished(DownloadedFile {
            request,
            address,
            outcome,
          })),
        );
      }
    };
    if let Err(error) = std::thread::Builder::new()
      .name("extension-download".into())
      .spawn(work)
    {
      table.under_way.fetch_sub(1, Ordering::Relaxed);
      return Err(ApiError::InvalidArgument(format!(
        "the download couldn't start: {error}"
      )));
    }
    Ok(request)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_https_addresses_with_a_host_are_downloaded() {
    assert!(check_address("https://example.com/api.json").is_ok());
    assert!(check_address("https://example.com").is_ok());
    assert!(check_address("http://example.com/api.json").is_err());
    assert!(check_address("file:///etc/passwd").is_err());
    assert!(check_address("https:///no-host").is_err());
    assert!(check_address("https://user@example.com/").is_err());
    assert!(check_address("https://example.com/a b").is_err());
    assert!(check_address(&format!("https://example.com/{}", "a".repeat(2_000))).is_err());
  }
}
