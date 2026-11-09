//! Flareite updating itself from its GitHub releases (ADR 0040).
//!
//! A release build knows the tag it was built from (`FLAREITE_RELEASE_TAG`,
//! which the release workflow sets), since a prerelease's manifests carry
//! only its `major.minor.patch`. Builds without one don't update.
//!
//! Checking lists the repository's releases (the API the extension
//! installer uses) and picks the newest one past this build's tag, in the
//! channel the settings choose, that has an update manifest. Its manifest
//! and installer are then fetched with Tauri's updater, which installs only
//! what's signed with the key in `tauri.conf.json`. A download is kept in
//! memory until the user restarts: the window closes as it always does
//! (asking about unsaved changes), and the update is installed once
//! everything has shut down, then Flareite starts again.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flareite_extension_install::{Release, Repo, tag_version};
use semver::Version;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, Window};
use tauri_plugin_updater::UpdaterExt;

use crate::bench::BenchState;
use crate::editor;
use crate::error::CommandError;
use crate::extension_install::{blocking, client};
use crate::settings::{SettingsState, UpdateChannel};
use crate::ui_state::UiStateStore;

/// The repository whose releases are this build's updates. A fork's release
/// workflow names its own.
const REPOSITORY: &str = match option_env!("FLAREITE_RELEASE_REPOSITORY") {
  Some(repository) => repository,
  None => "FlareiteLabs/flareite",
};
/// The update manifest a release carries, which the release workflow makes
/// from its installers' signatures.
pub const MANIFEST: &str = "latest.json";
/// The automatic check runs at most this often.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Download progress is reported at most this often.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// The tag this build was released as, if it's a release build.
pub fn release_tag() -> Option<&'static str> {
  option_env!("FLAREITE_RELEASE_TAG").filter(|tag| !tag.is_empty())
}

/// The version this build was released as, if it's a release build.
fn running_version() -> Option<Version> {
  release_tag().and_then(tag_version)
}

/// Whether this build is a prerelease (`v0.2.0-alpha.1`).
pub fn running_prerelease() -> bool {
  running_version().is_some_and(|version| !version.pre.is_empty())
}

/// What's shown as Flareite's version: the release's, or the manifests'.
pub fn display_version(app: &AppHandle) -> String {
  running_version().map_or_else(|| app.package_info().version.to_string(), |v| v.to_string())
}

fn wants_prereleases(channel: UpdateChannel) -> bool {
  match channel {
    UpdateChannel::Auto => running_prerelease(),
    UpdateChannel::Stable => false,
    UpdateChannel::Prerelease => true,
  }
}

/// The newest release past `current` with an update manifest, leaving out
/// prereleases unless `prereleases`.
fn pick(releases: Vec<Release>, current: &Version, prereleases: bool) -> Option<Release> {
  releases
    .into_iter()
    .filter(|release| release.file(MANIFEST).is_some())
    .filter_map(|release| release.version().map(|version| (version, release)))
    .filter(|(version, release)| prereleases || (!release.prerelease && version.pre.is_empty()))
    .filter(|(version, _)| version > current)
    .max_by(|(a, _), (b, _)| a.cmp(b))
    .map(|(_, release)| release)
}

/// An update found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
  /// Its version, from its tag (`0.2.0-alpha.1`).
  pub version: String,
  pub tag: String,
  pub prerelease: bool,
  /// The release's page.
  pub url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
  /// The running version.
  pub current: String,
  /// Whether this build updates itself: only release builds do.
  pub updatable: bool,
  /// The page listing Flareite's releases.
  pub releases_url: String,
  /// The update the last check found.
  pub update: Option<UpdateInfo>,
  pub downloading: bool,
  /// Whether it's downloaded, to install when Flareite restarts.
  pub ready: bool,
  /// Why the update installed when Flareite last exited failed. Reported
  /// once.
  pub failed: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
  pub downloaded: u64,
  pub total: Option<u64>,
}

struct Found {
  info: UpdateInfo,
  update: tauri_plugin_updater::Update,
  /// The installer, downloaded and its signature checked.
  bytes: Option<Vec<u8>>,
}

#[derive(Default)]
struct Inner {
  found: Option<Found>,
  checking: bool,
  downloading: bool,
  /// Set when the user restarts to update: installed as Flareite exits.
  install_on_exit: bool,
}

#[derive(Default)]
pub struct UpdateState(Mutex<Inner>);

impl UpdateState {
  fn lock(&self) -> MutexGuard<'_, Inner> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Clears a flag when the work it marks ends, however it ends.
struct Clear<'a> {
  state: &'a UpdateState,
  flag: fn(&mut Inner) -> &mut bool,
}

impl Drop for Clear<'_> {
  fn drop(&mut self) {
    *(self.flag)(&mut self.state.lock()) = false;
  }
}

fn status(app: &AppHandle, failed: Option<String>) -> UpdateStatus {
  let inner = app.state::<UpdateState>();
  let inner = inner.lock();
  UpdateStatus {
    current: display_version(app),
    updatable: release_tag().is_some(),
    releases_url: format!("https://github.com/{REPOSITORY}/releases"),
    update: inner.found.as_ref().map(|found| found.info.clone()),
    downloading: inner.downloading,
    ready: inner
      .found
      .as_ref()
      .is_some_and(|found| found.bytes.is_some()),
    failed,
  }
}

fn now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |elapsed| elapsed.as_secs())
}

/// Where updates stand. The first call also reports an update that failed
/// to install when Flareite last exited.
#[tauri::command]
pub fn app_update_status(app: AppHandle) -> UpdateStatus {
  let mut failed = None;
  app
    .state::<UiStateStore>()
    .update(|state| failed = state.failed_update.take());
  status(&app, failed)
}

/// Looks for an update. `automatic` is the check at startup, which happens
/// only in release builds, when `update.check` is on, at most once a day.
#[tauri::command]
pub async fn app_update_check(
  app: AppHandle,
  automatic: bool,
) -> Result<UpdateStatus, CommandError> {
  let Some(current) = running_version() else {
    if automatic {
      return Ok(status(&app, None));
    }
    return Err(CommandError::new(
      "notRelease",
      "This build of Flareite isn't from a release, so it doesn't update itself.",
    ));
  };
  let settings = app.state::<SettingsState>().get().update;
  if automatic && app.state::<BenchState>().active() {
    return Ok(status(&app, None));
  }
  if automatic {
    let last = app.state::<UiStateStore>().get().last_update_check;
    let due = last.is_none_or(|last| now().saturating_sub(last) >= CHECK_INTERVAL.as_secs());
    if !settings.check || !due {
      return Ok(status(&app, None));
    }
  }
  let state = app.state::<UpdateState>();
  let busy = {
    let mut inner = state.lock();
    let busy = inner.checking || inner.downloading;
    if !busy {
      inner.checking = true;
    }
    busy
  };
  // Not while locked: `status` locks too.
  if busy {
    return Ok(status(&app, None));
  }
  let _clear = Clear {
    state: &state,
    flag: |inner| &mut inner.checking,
  };

  let prereleases = wants_prereleases(settings.channel);
  let releases = {
    let app = app.clone();
    blocking(move || {
      let repo = Repo::parse(REPOSITORY)?;
      client(&app)?.releases(&repo)
    })
    .await?
  };
  let picked = pick(releases, &current, prereleases);
  tracing::info!(
    current = %current,
    prereleases,
    found = picked.as_ref().map(|release| release.tag.as_str()),
    "checked for an update"
  );
  let Some(release) = picked else {
    state.lock().found = None;
    record_check(&app);
    return Ok(status(&app, None));
  };
  if state
    .lock()
    .found
    .as_ref()
    .is_some_and(|found| found.info.tag == release.tag)
  {
    record_check(&app);
    return Ok(status(&app, None));
  }

  let update = fetch_update(&app, &release).await?;
  let version = release
    .version()
    .map_or_else(|| release.tag.clone(), |version| version.to_string());
  let info = UpdateInfo {
    prerelease: release.prerelease || version.contains('-'),
    version,
    tag: release.tag,
    url: release.url,
  };
  state.lock().found = Some(Found {
    info,
    update,
    bytes: None,
  });
  record_check(&app);
  Ok(status(&app, None))
}

/// Notes when the automatic check last ran, or a manual one did.
fn record_check(app: &AppHandle) {
  app
    .state::<UiStateStore>()
    .update(|state| state.last_update_check = Some(now()));
}

/// Reads the release's update manifest, with the installer for this system.
async fn fetch_update(
  app: &AppHandle,
  release: &Release,
) -> Result<tauri_plugin_updater::Update, CommandError> {
  let Some(manifest) = release.file(MANIFEST) else {
    return Err(CommandError::internal("the picked release has no manifest"));
  };
  let url = manifest
    .asset
    .url
    .parse()
    .map_err(|error| CommandError::internal(format!("manifest address: {error}")))?;
  let expected = release.version();
  let updater = app
    .updater_builder()
    .endpoints(vec![url])
    .map_err(update_error)?
    .timeout(Duration::from_secs(30))
    // The release is newer by its tag. The manifest names the version its
    // installers were built and signed as: the tag's `major.minor.patch`.
    .version_comparator(move |_, remote| {
      expected.as_ref().is_some_and(|expected| {
        (
          remote.version.major,
          remote.version.minor,
          remote.version.patch,
        ) == (expected.major, expected.minor, expected.patch)
      })
    })
    .build()
    .map_err(update_error)?;
  match updater.check().await {
    Ok(Some(update)) => Ok(update),
    Ok(None) => Err(CommandError::new(
      "invalidUpdate",
      format!(
        "The update manifest of {} doesn't match its version.",
        release.tag
      ),
    )),
    Err(error) => Err(update_error(error)),
  }
}

fn update_error(error: tauri_plugin_updater::Error) -> CommandError {
  use tauri_plugin_updater::Error;
  tracing::warn!(%error, "updating failed");
  match error {
    Error::TargetNotFound(_) | Error::TargetsNotFound(_) => CommandError::new(
      "noInstaller",
      "The newer release has no update for this kind of installation. Download it from its release page.",
    ),
    Error::Minisign(_)
    | Error::Base64(_)
    | Error::SignatureUtf8(_)
    | Error::SignedVersionMismatch { .. }
    | Error::MissingSignedVersion => CommandError::new(
      "badSignature",
      "The update isn't signed with Flareite's key, so it wasn't installed.",
    ),
    Error::Reqwest(_) | Error::Network(_) => CommandError::new(
      "network",
      format!("The update couldn't be downloaded: {error}."),
    ),
    error => CommandError::new("update", format!("Updating failed: {error}.")),
  }
}

/// Downloads the update the last check found, checking its signature.
/// Reports progress on `progress`.
#[tauri::command]
pub async fn app_update_download(
  app: AppHandle,
  progress: Channel<DownloadProgress>,
) -> Result<UpdateStatus, CommandError> {
  let state = app.state::<UpdateState>();
  let update = {
    let mut inner = state.lock();
    if inner.downloading {
      return Err(CommandError::new(
        "busy",
        "The update is already downloading.",
      ));
    }
    let Some(found) = inner.found.as_ref() else {
      return Err(CommandError::new(
        "noUpdate",
        "There's no update to download.",
      ));
    };
    if found.bytes.is_some() {
      drop(inner);
      return Ok(status(&app, None));
    }
    let update = found.update.clone();
    inner.downloading = true;
    update
  };
  let _clear = Clear {
    state: &state,
    flag: |inner| &mut inner.downloading,
  };

  let mut downloaded = 0u64;
  let mut reported = Instant::now() - PROGRESS_INTERVAL;
  let bytes = update
    .download(
      |chunk, total| {
        downloaded += chunk as u64;
        if reported.elapsed() >= PROGRESS_INTERVAL {
          reported = Instant::now();
          let _ = progress.send(DownloadProgress { downloaded, total });
        }
      },
      || {},
    )
    .await
    .map_err(update_error)?;
  tracing::info!(version = %update.version, bytes = bytes.len(), "update downloaded");
  {
    let mut inner = state.lock();
    match inner.found.as_mut() {
      // A check while it downloaded may have found a newer one.
      Some(found) if found.update.download_url == update.download_url => {
        found.bytes = Some(bytes);
      }
      _ => {}
    }
  }
  Ok(status(&app, None))
}

/// Closes Flareite, asking about unsaved changes as closing the window
/// does, to install the downloaded update and start again. False if the
/// user cancelled.
#[tauri::command]
pub async fn app_update_restart(window: Window) -> Result<bool, CommandError> {
  let app = window.app_handle().clone();
  let ready = app
    .state::<UpdateState>()
    .lock()
    .found
    .as_ref()
    .is_some_and(|found| found.bytes.is_some());
  if !ready {
    return Err(CommandError::new(
      "noUpdate",
      "There's no downloaded update to install.",
    ));
  }
  if !editor::confirm_exit(&window).await {
    return Ok(false);
  }
  app.state::<UpdateState>().lock().install_on_exit = true;
  tracing::info!("restarting to update");
  window.destroy().map_err(CommandError::internal)?;
  Ok(true)
}

/// Installs the downloaded update if the user restarted to update, then
/// starts Flareite again. Called last as Flareite exits, after everything
/// has shut down. On Windows the installer takes over: this starts it and
/// ends the process, and it starts Flareite when it's done.
pub fn install_on_exit(app: &AppHandle) {
  let Some(state) = app.try_state::<UpdateState>() else {
    return;
  };
  let found = {
    let mut inner = state.lock();
    if !inner.install_on_exit {
      return;
    }
    inner.found.take()
  };
  let Some(Found {
    info,
    update,
    bytes: Some(bytes),
  }) = found
  else {
    return;
  };
  tracing::info!(version = %info.version, "installing the update");
  // On Windows, installing ends the process.
  if let Some(guard) = app.try_state::<crate::logging::LogGuard>() {
    guard.flush();
  }
  if let Err(error) = update.install(&bytes) {
    tracing::error!(%error, "the update couldn't be installed");
    app.state::<UiStateStore>().update(|state| {
      state.failed_update = Some(format!(
        "Flareite {} couldn't be installed: {error}.",
        info.version
      ));
    });
  }
  tauri::process::restart(&app.env());
}

#[cfg(test)]
mod tests {
  use flareite_extension_install::{Asset, ReleaseFile};

  use super::*;

  fn release(tag: &str, prerelease: bool, manifest: bool) -> Release {
    let files = manifest
      .then(|| ReleaseFile {
        name: MANIFEST.to_owned(),
        asset: Asset {
          url: format!("https://example.com/{tag}/{MANIFEST}"),
          size: 1,
          digest: None,
        },
      })
      .into_iter()
      .collect();
    Release {
      tag: tag.to_owned(),
      name: None,
      prerelease,
      published_at: None,
      url: format!("https://example.com/{tag}"),
      package: None,
      files,
    }
  }

  fn picked(releases: Vec<Release>, current: &str, prereleases: bool) -> Option<String> {
    let current = Version::parse(current).expect("a version");
    pick(releases, &current, prereleases).map(|release| release.tag)
  }

  #[test]
  fn the_newest_release_past_this_one_is_picked() {
    let releases = || {
      vec![
        release("v0.2.0-alpha.1", true, true),
        release("v0.1.1", false, true),
        release("v0.1.0", false, true),
        release("v0.1.0-alpha.10", true, true),
        release("v0.1.0-alpha.2", true, true),
      ]
    };
    assert_eq!(
      picked(releases(), "0.1.0-alpha.2", true).as_deref(),
      Some("v0.2.0-alpha.1")
    );
    assert_eq!(
      picked(releases(), "0.1.0-alpha.2", false).as_deref(),
      Some("v0.1.1")
    );
    assert_eq!(picked(releases(), "0.1.1", false), None);
    assert_eq!(picked(releases(), "0.2.0-alpha.1", true), None);
  }

  #[test]
  fn prereleases_are_ordered_by_semver_not_text() {
    let releases = vec![
      release("v0.1.0-alpha.9", true, true),
      release("v0.1.0-alpha.10", true, true),
    ];
    assert_eq!(
      picked(releases, "0.1.0-alpha.2", true).as_deref(),
      Some("v0.1.0-alpha.10")
    );
  }

  #[test]
  fn releases_without_a_manifest_or_a_version_are_skipped() {
    let releases = vec![
      release("v0.1.0-alpha.3", true, false),
      release("nightly", true, true),
      release("v0.1.0-alpha.2", true, true),
    ];
    assert_eq!(
      picked(releases, "0.1.0-alpha.1", true).as_deref(),
      Some("v0.1.0-alpha.2")
    );
  }

  #[test]
  fn a_prerelease_tag_counts_as_one_even_when_github_says_otherwise() {
    let releases = vec![release("v0.2.0-beta.1", false, true)];
    assert_eq!(picked(releases, "0.1.0", false), None);
  }
}
