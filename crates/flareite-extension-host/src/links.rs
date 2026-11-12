//! Opening links: the host functions of the `links` interface, and calling
//! link handlers.
//!
//! The workbench decides who opens a link the user Ctrl+clicked (in the
//! terminal, the editor's text, or a preview): Flareite itself, or an
//! extension's `[[link_handlers]]`, by the user's settings. For an
//! extension, it calls the world's `open-link` ([`ExtensionHost::open_link`]),
//! which can hand the link back for Flareite to open its own way.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::bindings::flareite::extension::links::{
  Host, Link, LinkKind, LinkRequest as WitRequest,
};
use crate::bindings::flareite::extension::types::ApiError;
use crate::contributions::LinkKind as DeclaredKind;
use crate::runtime::StoreState;
use crate::worker::{Answer, Request};
use crate::{CommandTicket, ExtensionHost, Notification};

/// Longest link target.
const MAX_TARGET_BYTES: usize = 8 * 1024;

/// A link to open: a web address, or a file's absolute path with the line
/// and column it names (from 0).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRequest {
  pub kind: DeclaredKind,
  pub target: String,
  pub line: Option<u32>,
  pub column: Option<u32>,
}

impl Host for StoreState {
  fn open_in_browser(&mut self, url: String) -> Result<(), ApiError> {
    let lower = url.get(..8).unwrap_or(&url).to_ascii_lowercase();
    let web =
      lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:");
    if !web
      || url.len() > MAX_TARGET_BYTES
      || url.contains(char::is_whitespace)
      || url.contains(char::is_control)
    {
      return Err(ApiError::InvalidArgument(format!(
        "only https:, http:, and mailto: addresses open in the browser, not {url:?}"
      )));
    }
    // At most one a second for each extension, so none can flood the
    // browser with tabs.
    {
      let mut last = LAST_OPENED.lock().unwrap_or_else(PoisonError::into_inner);
      let now = Instant::now();
      let id = self.api.extension_id().to_owned();
      if last
        .get(&id)
        .is_some_and(|at| now.duration_since(*at) < Duration::from_secs(1))
      {
        return Err(ApiError::TooLarge(
          "at most one address a second opens in the browser".into(),
        ));
      }
      last.insert(id, now);
    }
    self
      .api
      .shared()?
      .embedder
      .notify(Notification::OpenLinkDefault(LinkRequest {
        kind: DeclaredKind::Web,
        target: url,
        line: None,
        column: None,
      }));
    Ok(())
  }
}

/// When each extension last opened an address in the browser.
static LAST_OPENED: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Mutex::default);

impl ExtensionHost {
  /// Has a link handler (`handler` is its full id) open `link`, starting
  /// its extension if needed. A link it hands back opens Flareite's own
  /// way.
  pub fn open_link(&self, handler: &str, link: LinkRequest) -> Result<CommandTicket, String> {
    if link.target.is_empty() || link.target.len() > MAX_TARGET_BYTES {
      return Err("a link is 1 byte to 8 KiB".into());
    }
    let (extension, local) = self
      .statuses()
      .into_iter()
      .find_map(|status| {
        let declared = status
          .contributions
          .link_handlers
          .into_iter()
          .find(|declared| declared.full_id == handler && declared.kind == link.kind)?;
        Some((status.id, declared.id))
      })
      .ok_or_else(|| format!("No extension opens these links as {handler}."))?;
    let request = WitRequest {
      handler: local,
      link: Link {
        kind: match link.kind {
          DeclaredKind::Web => LinkKind::Web,
          DeclaredKind::File => LinkKind::File,
        },
        target: link.target.clone(),
        line: link.line,
        column: link.column,
      },
    };
    let shared = self.shared.clone();
    Ok(
      self
        .call(&extension, Request::OpenLink(request))?
        .then(move |answer| {
          if answer != Answer::OpenedLink(true) {
            shared.embedder.notify(Notification::OpenLinkDefault(link));
          }
          Ok(())
        }),
    )
  }
}
