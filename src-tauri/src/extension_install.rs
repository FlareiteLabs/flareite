//! Installing, updating, and uninstalling extensions from GitHub releases
//! (`flareite-extension-install`), and the catalog of official extensions.
//!
//! Nothing here runs on its own: the network is used only when the user
//! installs, checks for updates, or opens the catalog (once a session,
//! unless refreshed). Every install and uninstall is confirmed in a dialog
//! that shows what the package's manifest says, after it's downloaded and
//! checked; then extensions are reloaded.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use flareite_extension_host::ExtensionHost;
use flareite_extension_install::{
  CatalogEntry, Client, InstallError, Installer, Release, Repo, Source, Summary, fetch, is_update,
};
use serde::Serialize;
use tauri::{AppHandle, Manager, Window};

use crate::dialogs;
use crate::error::CommandError;
use crate::extensions::{self, ExtensionsState};
use crate::settings::SettingsState;

/// Unpacked packages, beside the extensions folder (on the same volume, so
/// they can be moved into it).
const STAGING_DIR: &str = "extension-staging";

#[derive(Default)]
pub struct InstallState {
  client: OnceLock<Result<Client, String>>,
  /// The catalog fetched this session, with the owners it's for.
  catalog: Mutex<Option<(Vec<String>, Vec<CatalogEntry>)>>,
  /// Newer releases the last update check found, by extension id.
  updates: Mutex<BTreeMap<String, UpdateDto>>,
  /// Set while an install or uninstall runs, so they don't overlap.
  busy: AtomicBool,
}

impl InstallState {
  fn updates(&self) -> MutexGuard<'_, BTreeMap<String, UpdateDto>> {
    self.updates.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn catalog(&self) -> MutexGuard<'_, Option<(Vec<String>, Vec<CatalogEntry>)>> {
    self.catalog.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Clears the busy flag when an install or uninstall ends, however it ends.
struct Busy<'a>(&'a AtomicBool);

impl<'a> Busy<'a> {
  fn take(flag: &'a AtomicBool) -> Result<Self, CommandError> {
    if flag.swap(true, Ordering::AcqRel) {
      return Err(CommandError::new(
        "busy",
        "Another extension is being installed or uninstalled. Try again when it's done.",
      ));
    }
    Ok(Self(flag))
  }
}

impl Drop for Busy<'_> {
  fn drop(&mut self) {
    self.0.store(false, Ordering::Release);
  }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseDto {
  tag: String,
  name: Option<String>,
  prerelease: bool,
  published_at: Option<String>,
  url: String,
  /// Whether it has an `extension.zip` to install.
  installable: bool,
}

impl From<&Release> for ReleaseDto {
  fn from(release: &Release) -> Self {
    Self {
      tag: release.tag.clone(),
      name: release.name.clone(),
      prerelease: release.prerelease,
      published_at: release.published_at.clone(),
      url: release.url.clone(),
      installable: release.package.is_some(),
    }
  }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleasesDto {
  /// `owner/name`.
  repository: String,
  releases: Vec<ReleaseDto>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledDto {
  id: String,
  name: String,
  version: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDto {
  id: String,
  repository: String,
  /// The installed version.
  version: String,
  /// The newer release's tag.
  tag: String,
}

/// Where an installed extension came from, and its update, if the last
/// check found one.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDto {
  id: String,
  repository: String,
  tag: String,
  update: Option<String>,
}

fn install_error(error: InstallError) -> CommandError {
  let code = match &error {
    InstallError::InvalidRepository(_) => "invalidRepository",
    InstallError::NotFound(_) => "notFound",
    InstallError::NoRelease(_) | InstallError::NoPackage(_) => "noPackage",
    InstallError::RateLimited(_) => "rateLimited",
    InstallError::Network(_) => "network",
    InstallError::DigestMismatch => "digestMismatch",
    InstallError::TooLarge(_)
    | InstallError::InvalidPackage(_)
    | InstallError::InvalidManifest(_) => "invalidPackage",
    InstallError::Io(_) => "io",
  };
  CommandError::new(code, sentence(&error.to_string()))
}

/// `text` with a capital letter and a full stop.
fn sentence(text: &str) -> String {
  let mut chars = text.chars();
  let mut sentence: String = chars
    .next()
    .map(|first| first.to_uppercase().chain(chars).collect())
    .unwrap_or_default();
  if !sentence.ends_with('.') {
    sentence.push('.');
  }
  sentence
}

async fn blocking<T: Send + 'static>(
  f: impl FnOnce() -> Result<T, InstallError> + Send + 'static,
) -> Result<T, CommandError> {
  tauri::async_runtime::spawn_blocking(f)
    .await
    .map_err(CommandError::internal)?
    .map_err(install_error)
}

/// The GitHub client, made the first time it's needed. Call it only on a
/// blocking thread (in [`blocking`]): reqwest's blocking client starts and
/// stops a runtime of its own, which panics on Tauri's async runtime.
fn client(app: &AppHandle) -> Result<Client, InstallError> {
  app
    .state::<InstallState>()
    .client
    .get_or_init(|| {
      let user_agent = format!(
        "Flareite/{} (+https://github.com/FlareiteLabs/flareite)",
        app.package_info().version
      );
      Client::new(&user_agent).map_err(|error| error.to_string())
    })
    .clone()
    .map_err(InstallError::Network)
}

fn installer(app: &AppHandle) -> Result<Installer, CommandError> {
  let dir = extensions::config_dir(app)
    .ok_or_else(|| CommandError::new("noConfigDir", "There's no folder for settings."))?;
  let editor_version = semver::Version::parse(&app.package_info().version.to_string())
    .unwrap_or_else(|_| semver::Version::new(0, 0, 0));
  Ok(Installer {
    extensions_dir: dir.join(extensions::EXTENSIONS_DIR),
    staging_dir: dir.join(STAGING_DIR),
    editor_version,
  })
}

fn host(app: &AppHandle) -> Option<&ExtensionHost> {
  app.state::<ExtensionsState>().inner().host()
}

/// Whether a development extension with this id is loaded (it wins over
/// an installed one).
fn development_loaded(app: &AppHandle, id: &str) -> bool {
  host(app).is_some_and(|host| {
    host
      .statuses()
      .iter()
      .any(|status| status.id == id && status.development)
  })
}

/// The newest release that isn't a prerelease and has a package.
fn newest(releases: Vec<Release>, repo: &Repo) -> Result<Release, InstallError> {
  releases
    .into_iter()
    .find(|release| !release.prerelease && release.package.is_some())
    .ok_or_else(|| InstallError::NoRelease(repo.to_string()))
}

/// The repository's releases, newest first.
#[tauri::command]
pub async fn extension_releases(
  app: AppHandle,
  repository: String,
) -> Result<ReleasesDto, CommandError> {
  let repo = Repo::parse(&repository).map_err(install_error)?;
  let asked = repo.clone();
  let releases = blocking(move || client(&app)?.releases(&asked)).await?;
  Ok(ReleasesDto {
    repository: repo.to_string(),
    releases: releases.iter().map(ReleaseDto::from).collect(),
  })
}

/// What the confirmation says, and its button.
fn install_question(
  summary: &Summary,
  repo: &Repo,
  tag: &str,
  installed: Option<&semver::Version>,
  development: bool,
) -> (String, &'static str) {
  let name = &summary.name;
  let version = &summary.version;
  let (question, action) = match installed {
    None => (format!("Install {name} {version}?"), "Install"),
    Some(old) if semver::Version::parse(version).is_ok_and(|new| new > *old) => {
      (format!("Update {name} from {old} to {version}?"), "Update")
    }
    Some(old) => (format!("Replace {name} {old} with {version}?"), "Replace"),
  };
  let mut message = question;
  if !summary.description.is_empty() {
    message.push_str(&format!("\n\n{}", summary.description));
  }
  let by = summary
    .author
    .as_deref()
    .map(|author| format!("By {author}, from "))
    .unwrap_or_else(|| "From ".into());
  message.push_str(&format!("\n\n{by}{repo} (release {tag})."));
  if summary.permissions.is_empty() {
    message.push_str("\n\nIt requests no permissions.");
  } else {
    message.push_str("\n\nIt asks before it can:");
    for permission in &summary.permissions {
      message.push_str(&format!("\n• {permission}"));
    }
  }
  if development {
    message.push_str(
      "\n\nA development extension with the same id is loaded; it's used instead while it is.",
    );
  }
  (message, action)
}

/// Downloads a release of a repository (`tag`, or the newest), checks it,
/// asks, and installs it, replacing the installed extension with its id.
/// `None` if the user declined.
#[tauri::command]
pub async fn extension_install(
  app: AppHandle,
  window: Window,
  repository: String,
  tag: Option<String>,
) -> Result<Option<InstalledDto>, CommandError> {
  let state = app.state::<InstallState>();
  let _busy = Busy::take(&state.busy)?;
  let repo = Repo::parse(&repository).map_err(install_error)?;
  let installer = installer(&app)?;
  let (release, staged) = {
    let (repo, installer, handle) = (repo.clone(), installer.clone(), app.clone());
    blocking(move || {
      let client = client(&handle)?;
      let releases = client.releases(&repo)?;
      let release = match tag {
        Some(tag) => releases
          .into_iter()
          .find(|release| release.tag == tag)
          .ok_or_else(|| InstallError::NotFound(format!("The release {tag} of {repo}")))?,
        None => newest(releases, &repo)?,
      };
      let staged = fetch(&client, &installer, &release)?;
      Ok((release, staged))
    })
    .await?
  };
  let summary = staged.summary.clone();
  let installed = installer
    .find(&summary.id)
    .map(|installed| installed.version);
  let (message, action) = install_question(
    &summary,
    &repo,
    &release.tag,
    installed.as_ref(),
    development_loaded(&app, &summary.id),
  );
  if !dialogs::confirm(&window, message, action).await {
    // Dropping the staged package removes it.
    return Ok(None);
  }
  let digest = release
    .package
    .as_ref()
    .and_then(|asset| asset.digest.clone());
  let source = Source::new(repo, release.tag.clone(), digest);
  let reload = app.clone();
  blocking(move || {
    let dir = installer.install(staged, &source)?;
    tracing::info!(repository = %source.repository, tag = %source.tag, dir = %dir.display(), "installed an extension");
    if let Some(host) = host(&reload) {
      host.load();
    }
    Ok(())
  })
  .await?;
  state.updates().remove(&summary.id);
  Ok(Some(InstalledDto {
    id: summary.id,
    name: summary.name,
    version: summary.version,
  }))
}

/// Asks, then removes an installed extension, what it stored, and the
/// permissions it was given. Its settings stay. False if the user declined.
#[tauri::command]
pub async fn extension_uninstall(
  app: AppHandle,
  window: Window,
  id: String,
) -> Result<bool, CommandError> {
  let state = app.state::<InstallState>();
  let _busy = Busy::take(&state.busy)?;
  let installer = installer(&app)?;
  if installer.find(&id).is_none() {
    return Err(CommandError::new(
      "notInstalled",
      format!(
        "{id} isn't installed. A development extension is removed from extensions.development in the settings."
      ),
    ));
  }
  let name = host(&app)
    .and_then(|host| {
      host
        .statuses()
        .into_iter()
        .find(|status| status.id == id && !status.development)
        .map(|status| status.name)
    })
    .unwrap_or_else(|| id.clone());
  let message = format!(
    "Uninstall {name}?\n\nIts folder is deleted, with the values it stored and the permissions you gave it. Its settings stay in settings.toml."
  );
  if !dialogs::confirm(&window, message, "Uninstall").await {
    return Ok(false);
  }
  let reload = app.clone();
  let removed = id.clone();
  blocking(move || {
    installer.uninstall(&removed)?;
    tracing::info!(id = %removed, "uninstalled an extension");
    if let Some(host) = host(&reload) {
      host.forget_decisions(&removed);
      if let Err(error) = host.clear_storage(&removed) {
        tracing::warn!(%error, id = %removed, "could not clear an uninstalled extension's storage");
      }
      host.load();
    }
    Ok(())
  })
  .await?;
  state.updates().remove(&id);
  Ok(true)
}

/// The extensions the owners in `extensions.catalog` list, fetched once a
/// session unless `refresh`.
#[tauri::command]
pub async fn extension_catalog(
  app: AppHandle,
  refresh: bool,
) -> Result<Vec<CatalogEntry>, CommandError> {
  let owners = app
    .state::<SettingsState>()
    .get()
    .extensions
    .catalog
    .clone();
  if owners.is_empty() {
    return Ok(Vec::new());
  }
  let state = app.state::<InstallState>();
  if !refresh
    && let Some((cached_owners, entries)) = state.catalog().as_ref()
    && *cached_owners == owners
  {
    return Ok(entries.clone());
  }
  let asked = owners.clone();
  let handle = app.clone();
  let entries = blocking(move || {
    let client = client(&handle)?;
    let mut entries = Vec::new();
    for owner in &asked {
      entries.extend(client.catalog(owner)?);
    }
    Ok(entries)
  })
  .await?;
  *state.catalog() = Some((owners, entries.clone()));
  Ok(entries)
}

/// Where each extension installed from GitHub came from.
#[tauri::command]
pub async fn extension_sources(app: AppHandle) -> Result<Vec<SourceDto>, CommandError> {
  let installer = installer(&app)?;
  let installed = blocking(move || Ok(installer.installed())).await?;
  let updates = app.state::<InstallState>().updates().clone();
  Ok(
    installed
      .into_iter()
      .filter_map(|installed| {
        let source = installed.source?;
        Some(SourceDto {
          update: updates.get(&installed.id).map(|update| update.tag.clone()),
          id: installed.id,
          repository: source.repository.to_string(),
          tag: source.tag,
        })
      })
      .collect(),
  )
}

/// Looks for newer releases of the extensions installed from GitHub.
#[tauri::command]
pub async fn extension_check_updates(app: AppHandle) -> Result<Vec<UpdateDto>, CommandError> {
  let installer = installer(&app)?;
  let handle = app.clone();
  let updates = blocking(move || {
    let client = client(&handle)?;
    let mut updates = Vec::new();
    for installed in installer.installed() {
      let Some(source) = &installed.source else {
        continue;
      };
      match client.latest(&source.repository) {
        Ok(release) if is_update(&release, &installed) => updates.push(UpdateDto {
          id: installed.id.clone(),
          repository: source.repository.to_string(),
          version: installed.version.to_string(),
          tag: release.tag,
        }),
        Ok(_) => {}
        Err(InstallError::RateLimited(reset)) => return Err(InstallError::RateLimited(reset)),
        Err(error) => {
          tracing::warn!(%error, id = %installed.id, "could not check an extension for updates");
        }
      }
    }
    Ok(updates)
  })
  .await?;
  *app.state::<InstallState>().updates() = updates
    .iter()
    .map(|update| (update.id.clone(), update.clone()))
    .collect();
  Ok(updates)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn summary(permissions: &[&str]) -> Summary {
    Summary {
      id: "flareite.git".into(),
      name: "Git".into(),
      version: "0.6.0".into(),
      description: "Source control.".into(),
      author: Some("Flareite".into()),
      permissions: permissions.iter().map(|&p| p.to_owned()).collect(),
    }
  }

  #[test]
  fn the_question_says_what_changes_and_what_it_may_do() {
    let repo = Repo::parse("FlareiteLabs/flareite-git-ext").expect("repo");
    let (message, action) =
      install_question(&summary(&["Run programs"]), &repo, "v0.6.0", None, false);
    assert_eq!(action, "Install");
    assert_eq!(
      message,
      "Install Git 0.6.0?\n\nSource control.\n\nBy Flareite, from FlareiteLabs/flareite-git-ext (release v0.6.0).\n\nIt asks before it can:\n• Run programs"
    );
    let older = semver::Version::new(0, 5, 0);
    let (message, action) = install_question(&summary(&[]), &repo, "v0.6.0", Some(&older), true);
    assert_eq!(action, "Update");
    assert!(
      message.starts_with("Update Git from 0.5.0 to 0.6.0?"),
      "{message}"
    );
    assert!(message.contains("requests no permissions"));
    assert!(message.contains("development extension"));
    let newer = semver::Version::new(0, 7, 0);
    let (message, action) = install_question(&summary(&[]), &repo, "v0.6.0", Some(&newer), false);
    assert_eq!(action, "Replace");
    assert!(
      message.starts_with("Replace Git 0.7.0 with 0.6.0?"),
      "{message}"
    );
  }

  #[test]
  fn errors_read_as_sentences() {
    assert_eq!(sentence("the package is bad"), "The package is bad.");
    assert_eq!(sentence("Done."), "Done.");
  }
}
