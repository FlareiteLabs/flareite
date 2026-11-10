//! Schemas on the web: downloaded in the background, kept in the cache
//! folder (and in memory), and downloaded again once they're older than
//! their caller wants, while the kept copy goes on being used.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};

/// Largest schema downloaded.
const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// How long a failed download is remembered before it's tried again.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
const TIMEOUT: Duration = Duration::from_secs(30);

/// What's known of a document on the web.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetched {
  Ready(Arc<str>),
  /// Being downloaded.
  Pending,
  Failed(String),
}

#[derive(Default)]
struct State {
  memory: HashMap<String, Arc<str>>,
  in_flight: HashSet<String>,
  failures: HashMap<String, (Instant, String)>,
}

pub struct Fetcher {
  dir: Option<PathBuf>,
  user_agent: String,
  client: OnceLock<Result<reqwest::blocking::Client, String>>,
  state: Mutex<State>,
}

/// Where a document's copy is kept.
fn file_name(address: &str) -> String {
  let digest = Sha256::digest(address.as_bytes());
  let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
  format!("{hex}.json")
}

impl Fetcher {
  /// Keeps copies in `dir`, if given.
  pub fn new(dir: Option<PathBuf>, user_agent: String) -> Self {
    Self {
      dir,
      user_agent,
      client: OnceLock::new(),
      state: Mutex::default(),
    }
  }

  fn lock(&self) -> MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// The document at `address` (`https:` or `http:`), if a copy is kept;
  /// else it's downloaded in the background and `done` is called when
  /// that finishes. A copy older than `max_age` is used, and downloaded
  /// again; `done` is called if it changed.
  pub fn get(
    self: &Arc<Self>,
    address: &str,
    max_age: Duration,
    done: impl FnOnce() + Send + 'static,
  ) -> Fetched {
    let kept = self.kept(address);
    let mut state = self.lock();
    if let Some((text, age)) = &kept {
      state.memory.insert(address.to_owned(), text.clone());
      if *age > max_age && !state.in_flight.contains(address) {
        state.in_flight.insert(address.to_owned());
        drop(state);
        self.download(address.to_owned(), Some(text.clone()), done);
      }
      return Fetched::Ready(text.clone());
    }
    if let Some((when, error)) = state.failures.get(address)
      && when.elapsed() < RETRY_AFTER
    {
      return Fetched::Failed(error.clone());
    }
    if state.in_flight.insert(address.to_owned()) {
      drop(state);
      self.download(address.to_owned(), None, done);
    }
    Fetched::Pending
  }

  /// The kept copy, and how old it is.
  fn kept(&self, address: &str) -> Option<(Arc<str>, Duration)> {
    if let Some(text) = self.lock().memory.get(address) {
      // Its file's age decides; without one, it's as new as this session.
      let age = self.file_age(address).unwrap_or_default();
      return Some((text.clone(), age));
    }
    let path = self.dir.as_ref()?.join(file_name(address));
    let text = std::fs::read_to_string(&path).ok()?;
    Some((text.into(), self.file_age(address).unwrap_or_default()))
  }

  fn file_age(&self, address: &str) -> Option<Duration> {
    let path = self.dir.as_ref()?.join(file_name(address));
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok()
  }

  fn client(&self) -> Result<reqwest::blocking::Client, String> {
    self
      .client
      .get_or_init(|| {
        reqwest::blocking::Client::builder()
          .user_agent(&self.user_agent)
          .connect_timeout(Duration::from_secs(15))
          .timeout(TIMEOUT)
          .build()
          .map_err(|error| error.to_string())
      })
      .clone()
  }

  fn download(
    self: &Arc<Self>,
    address: String,
    previous: Option<Arc<str>>,
    done: impl FnOnce() + Send + 'static,
  ) {
    let fetcher = self.clone();
    let spawned = std::thread::Builder::new()
      .name("schema-download".into())
      .spawn(move || {
        let result = fetcher.fetch(&address);
        let changed = {
          let mut state = fetcher.lock();
          state.in_flight.remove(&address);
          match result {
            Ok(text) => {
              state.failures.remove(&address);
              let changed = previous.as_deref() != Some(text.as_str());
              if let Some(dir) = &fetcher.dir {
                let path = dir.join(file_name(&address));
                let written = std::fs::create_dir_all(dir)
                  .and_then(|()| flareite_workspace::write_file_atomically(&path, text.as_bytes()));
                if let Err(error) = written {
                  tracing::warn!(%error, "could not keep a downloaded schema");
                }
              }
              state.memory.insert(address.clone(), text.into());
              tracing::info!(%address, "downloaded a schema");
              changed
            }
            Err(error) => {
              tracing::warn!(%address, %error, "could not download a schema");
              // A kept copy goes on being used.
              let first = previous.is_none();
              if first {
                state
                  .failures
                  .insert(address.clone(), (Instant::now(), error));
              }
              first
            }
          }
        };
        if changed {
          done();
        }
      });
    if let Err(error) = spawned {
      tracing::error!(%error, "could not start a schema download");
      self.lock().in_flight.clear();
    }
  }

  fn fetch(&self, address: &str) -> Result<String, String> {
    if !(address.starts_with("https://") || address.starts_with("http://")) {
      return Err(format!("{address} is not a web address"));
    }
    let response = self
      .client()?
      .get(address)
      .send()
      .map_err(|error| error.to_string())?;
    let status = response.status();
    if !status.is_success() {
      return Err(format!("the server answered {status}"));
    }
    if response
      .content_length()
      .is_some_and(|length| length > MAX_BYTES)
    {
      return Err(format!("it's larger than {} MiB", MAX_BYTES / 1024 / 1024));
    }
    let mut bytes = Vec::new();
    response
      .take(MAX_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
      return Err(format!("it's larger than {} MiB", MAX_BYTES / 1024 / 1024));
    }
    let text = String::from_utf8(bytes).map_err(|_| "it isn't UTF-8 text".to_owned())?;
    Ok(text.trim_start_matches('\u{feff}').to_owned())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn copies_are_named_by_address() {
    assert_eq!(file_name("https://a/b.json"), file_name("https://a/b.json"));
    assert_ne!(file_name("https://a/b.json"), file_name("https://a/c.json"));
    assert!(file_name("x").ends_with(".json"));
  }

  #[test]
  fn kept_copies_are_used_and_other_addresses_fail() {
    let dir = tempfile::tempdir().expect("temp dir");
    let fetcher = Arc::new(Fetcher::new(Some(dir.path().to_path_buf()), "test".into()));
    std::fs::write(
      dir.path().join(file_name("https://example.invalid/s.json")),
      "{}",
    )
    .expect("writes");
    assert_eq!(
      fetcher.get(
        "https://example.invalid/s.json",
        Duration::from_secs(3600),
        || {}
      ),
      Fetched::Ready("{}".into())
    );
    assert!(fetcher.fetch("file:///etc/passwd").is_err());
  }
}
