//! Files from the web that language servers use, such as type definitions:
//! downloaded into the cache folder, kept there, and named in a server's
//! arguments by their paths. An argument names one as
//! `${download:https://...}` (see `docs/extensions.md`).
//!
//! A kept copy is used at once. Once it's older than a day it's downloaded
//! again in the background, and the next start of the server gets the new
//! one. Without a copy, the server's start waits for the download. Failed
//! downloads aren't tried again for a few minutes.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use flareite_extension_host::contributions::DOWNLOAD_START;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

/// How old a kept copy may be before it's downloaded again.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Largest file downloaded.
const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// How long a failed download is remembered before it's tried again.
const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
const TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct State {
  /// Downloads under way in the background.
  refreshing: HashSet<String>,
  failures: HashMap<String, (Instant, String)>,
  /// One lock per address, so two servers starting together download it
  /// once.
  locks: HashMap<String, Arc<Mutex<()>>>,
}

pub struct Downloads {
  dir: Option<PathBuf>,
  user_agent: String,
  client: OnceLock<Result<reqwest::blocking::Client, String>>,
  state: Mutex<State>,
}

/// Where a file's copy is kept: named by its address, ending with the
/// address's own file name (some programs go by the extension).
pub fn file_name(address: &str) -> String {
  let digest = Sha256::digest(address.as_bytes());
  let hex: String = digest[..8]
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
  let last = address
    .split(['?', '#'])
    .next()
    .unwrap_or_default()
    .rsplit('/')
    .next()
    .unwrap_or_default();
  let last: String = last
    .chars()
    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    .take(80)
    .collect();
  let last = last.trim_start_matches('.');
  if last.is_empty() {
    hex
  } else {
    format!("{hex}-{last}")
  }
}

/// The addresses in `arg` and where each is replaced:
/// `(start, end, address)`, by byte. Arguments are checked when they're
/// declared or set, so a malformed one names none.
fn placeholders(arg: &str) -> Vec<(usize, usize, &str)> {
  let mut found = Vec::new();
  let mut from = 0;
  while let Some(start) = arg[from..].find(DOWNLOAD_START).map(|i| from + i) {
    let inner = start + DOWNLOAD_START.len();
    let Some(close) = arg[inner..].find('}').map(|i| inner + i) else {
      break;
    };
    found.push((start, close + 1, &arg[inner..close]));
    from = close + 1;
  }
  found
}

/// `args` with each `${download:<address>}` replaced by the path `path`
/// gives for it; an argument with an address it fails for is left out
/// (and the failure logged).
pub fn resolve_args(
  server: &str,
  args: &[String],
  mut path: impl FnMut(&str) -> Result<PathBuf, String>,
) -> Vec<String> {
  args
    .iter()
    .filter_map(|arg| {
      let mut out = String::new();
      let mut last = 0;
      for (start, end, address) in placeholders(arg) {
        match path(address) {
          Ok(file) => {
            out.push_str(&arg[last..start]);
            out.push_str(&file.to_string_lossy());
            last = end;
          }
          Err(error) => {
            tracing::warn!(%server, %address, %error, "a language server's file couldn't be downloaded; leaving out its argument");
            return None;
          }
        }
      }
      out.push_str(&arg[last..]);
      Some(out)
    })
    .collect()
}

impl Downloads {
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

  /// The kept copy of the file at `address` (`https:`), downloading it
  /// first if there's none (which blocks). A copy older than `max_age` is
  /// returned and downloaded again in the background.
  pub fn file(self: &Arc<Self>, address: &str, max_age: Duration) -> Result<PathBuf, String> {
    let dir = self
      .dir
      .as_ref()
      .ok_or("there's no cache folder to keep downloads in")?;
    let path = dir.join(file_name(address));
    if let Some(age) = age_of(&path) {
      if age > max_age {
        self.refresh(address, &path);
      }
      return Ok(path);
    }
    if let Some((when, error)) = self.lock().failures.get(address)
      && when.elapsed() < RETRY_AFTER
    {
      return Err(error.clone());
    }
    let one_at_a_time = self
      .lock()
      .locks
      .entry(address.to_owned())
      .or_default()
      .clone();
    let _downloading = one_at_a_time.lock().unwrap_or_else(PoisonError::into_inner);
    // Another start may have just downloaded it.
    if path.exists() {
      return Ok(path);
    }
    match self.download(address, &path) {
      Ok(()) => Ok(path),
      Err(error) => {
        self
          .lock()
          .failures
          .insert(address.to_owned(), (Instant::now(), error.clone()));
        Err(error)
      }
    }
  }

  /// Downloads a stale copy again in the background.
  fn refresh(self: &Arc<Self>, address: &str, path: &Path) {
    if !self.lock().refreshing.insert(address.to_owned()) {
      return;
    }
    let (downloads, owned, path) = (self.clone(), address.to_owned(), path.to_path_buf());
    let spawned = std::thread::Builder::new()
      .name("download-refresh".into())
      .spawn(move || {
        if let Err(error) = downloads.download(&owned, &path) {
          // The kept copy goes on being used; try again in a while.
          tracing::warn!(address = %owned, %error, "could not download a newer copy of a language server's file");
          touch(&path);
        }
        downloads.lock().refreshing.remove(&owned);
      });
    if let Err(error) = spawned {
      tracing::error!(%error, "could not start a download");
      self.lock().refreshing.remove(address);
    }
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

  fn download(&self, address: &str, path: &Path) -> Result<(), String> {
    if !address.starts_with("https://") {
      return Err(format!("{address} is not an https: address"));
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
    let too_large = || format!("it's larger than {} MiB", MAX_BYTES / 1024 / 1024);
    if response
      .content_length()
      .is_some_and(|length| length > MAX_BYTES)
    {
      return Err(too_large());
    }
    let mut bytes = Vec::new();
    response
      .take(MAX_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
      return Err(too_large());
    }
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    flareite_workspace::write_file_atomically(path, &bytes).map_err(|error| error.to_string())?;
    self.lock().failures.remove(address);
    tracing::info!(%address, bytes = bytes.len(), "downloaded a language server's file");
    Ok(())
  }
}

fn age_of(path: &Path) -> Option<Duration> {
  let modified = std::fs::metadata(path).ok()?.modified().ok()?;
  Some(
    SystemTime::now()
      .duration_since(modified)
      .unwrap_or_default(),
  )
}

/// Marks a kept copy as fresh, so a failing address isn't tried at every
/// start.
fn touch(path: &Path) {
  if let Ok(file) = std::fs::File::options().append(true).open(path) {
    let _ = file.set_modified(SystemTime::now());
  }
}

#[derive(Default)]
pub struct DownloadsState(OnceLock<Arc<Downloads>>);

impl DownloadsState {
  pub fn get(&self) -> Option<Arc<Downloads>> {
    self.0.get().cloned()
  }
}

/// Sets up the cache: `downloads` in the app's cache folder.
pub fn init(app: &AppHandle) {
  let dir = app
    .path()
    .app_cache_dir()
    .ok()
    .map(|dir| dir.join("downloads"));
  let user_agent = format!(
    "Flareite/{} (+https://github.com/FlareiteLabs/flareite)",
    app.package_info().version
  );
  let _ = app
    .state::<DownloadsState>()
    .0
    .set(Arc::new(Downloads::new(dir, user_agent)));
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn copies_are_named_by_address_and_keep_its_file_name() {
    let a = "https://luau-lsp.pages.dev/type-definitions/globalTypes.None.d.luau";
    let name = file_name(a);
    assert!(name.ends_with("-globalTypes.None.d.luau"), "{name}");
    assert_eq!(name, file_name(a));
    assert_ne!(name, file_name(&a.replace("None", "PluginSecurity")));
    assert_eq!(file_name("https://x.dev/").len(), 16);
    assert!(file_name("https://x.dev/a?b=../c").ends_with("-a"));
    assert!(file_name("https://x.dev/..").len() == 16);
  }

  #[test]
  fn arguments_get_paths_and_failing_ones_are_left_out() {
    let args = vec![
      "lsp".to_owned(),
      "--definitions:@roblox=${download:https://a.dev/types.d.luau}".to_owned(),
      "--docs=${download:https://a.dev/missing.json}".to_owned(),
      "x${download:https://a.dev/1}y${download:https://a.dev/2}z".to_owned(),
    ];
    let resolved = resolve_args("s", &args, |address| match address {
      "https://a.dev/missing.json" => Err("404".into()),
      other => Ok(PathBuf::from(format!(
        "/cache/{}",
        other.rsplit('/').next().unwrap_or_default()
      ))),
    });
    assert_eq!(
      resolved,
      [
        "lsp",
        "--definitions:@roblox=/cache/types.d.luau",
        "x/cache/1y/cache/2z",
      ]
    );
  }

  #[test]
  fn kept_copies_are_used_without_downloading() {
    let dir = tempfile::tempdir().expect("temp dir");
    let downloads = Arc::new(Downloads::new(
      Some(dir.path().to_path_buf()),
      "test".into(),
    ));
    let address = "https://example.invalid/kept.json";
    let kept = dir.path().join(file_name(address));
    std::fs::write(&kept, "{}").expect("writes");
    assert_eq!(downloads.file(address, MAX_AGE), Ok(kept));
    // Nothing kept, and nowhere to get it: fails, and is remembered.
    let missing = "https://example.invalid/missing.json";
    assert!(downloads.file(missing, MAX_AGE).is_err());
    assert!(downloads.lock().failures.contains_key(missing));
  }
}
