//! Opening links (API 1.15): the host functions of the `links` interface,
//! and running a link handler's command.
//!
//! The workbench decides who opens a link the user Ctrl+clicked (in the
//! terminal, the editor's text, or a preview): Flareite itself, or an
//! extension's `[[link_handlers]]`, by the user's settings. For an
//! extension, it runs the handler's command with the link as its payload
//! ([`ExtensionHost::open_link`]); the command reads it with `opening`, and
//! can hand it back with `open-default`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::bindings::flareite::extension::links::{Host, Link, LinkKind};
use crate::bindings::flareite::extension::types::ApiError;
use crate::contributions::LinkKind as DeclaredKind;
use crate::runtime::StoreState;
use crate::worker::Payload;
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
  fn opening(&mut self) -> Option<Link> {
    match &self.api.payload {
      Payload::Link(link) => Some(Link {
        kind: match link.kind {
          DeclaredKind::Web => LinkKind::Web,
          DeclaredKind::File => LinkKind::File,
        },
        target: link.target.clone(),
        line: link.line,
        column: link.column,
      }),
      _ => None,
    }
  }

  fn open_default(&mut self) -> Result<(), ApiError> {
    let Payload::Link(link) = &self.api.payload else {
      return Err(ApiError::InvalidArgument(
        "open-default opens the link a handler's command was run for: call it from that command"
          .into(),
      ));
    };
    let link = link.clone();
    self
      .api
      .shared()?
      .embedder
      .notify(Notification::OpenLinkDefault(link));
    Ok(())
  }

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
  /// Runs a link handler's command (`handler` is its full id) to open
  /// `link`, starting its extension if needed.
  pub fn open_link(&self, handler: &str, link: LinkRequest) -> Result<CommandTicket, String> {
    if link.target.is_empty() || link.target.len() > MAX_TARGET_BYTES {
      return Err("a link is 1 byte to 8 KiB".into());
    }
    let command = self
      .statuses()
      .into_iter()
      .flat_map(|status| status.contributions.link_handlers)
      .find(|declared| declared.full_id == handler && declared.kind == link.kind)
      .map(|declared| declared.command)
      .ok_or_else(|| format!("No extension opens these links as {handler}."))?;
    self.run_with_payload(&command, Payload::Link(link))
  }
}
