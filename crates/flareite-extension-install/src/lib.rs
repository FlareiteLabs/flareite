//! Installing extensions from GitHub releases.
//!
//! A release carries the extension as an asset named `extension.zip`. This
//! crate lists a repository's releases ([`Client::releases`]) and the
//! extensions an owner lists ([`Client::catalog`]), downloads a package and
//! checks it against GitHub's digest ([`Client::download`]), unpacks and
//! validates it ([`Installer::stage`]), and moves it into the extensions
//! folder with a record of where it came from ([`Installer::install`]).
//! It holds no state and knows nothing of the running extension host; the
//! app reloads extensions afterwards.

mod github;
mod package;

pub use github::{
  API, Asset, CATALOG_TOPIC, CatalogEntry, Client, MAX_PACKAGE_BYTES, PACKAGE_ASSET, RAW, Release,
  ReleaseFile, Repo, tag_version,
};
pub use package::{
  Installed, Installer, MAX_FILES, MAX_UNPACKED_BYTES, SOURCE_FILE, Source, Staged, Summary,
};

/// Why finding, downloading, or installing an extension failed, in words
/// for the user.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
  #[error("{0:?} isn't a GitHub repository (owner/name, or its address)")]
  InvalidRepository(String),
  #[error("{0} wasn't found on GitHub")]
  NotFound(String),
  #[error("{0} has no release with an {PACKAGE_ASSET}")]
  NoRelease(String),
  #[error("the release {0} has no {PACKAGE_ASSET}")]
  NoPackage(String),
  #[error("GitHub's limit of requests without signing in was reached; try again {}", reset_text(*.0))]
  RateLimited(Option<u64>),
  #[error("GitHub couldn't be reached: {0}")]
  Network(String),
  #[error("{0}")]
  TooLarge(String),
  #[error("the download doesn't match the digest GitHub published for it")]
  DigestMismatch,
  #[error("the package can't be installed: {0}")]
  InvalidPackage(String),
  #[error("the package's manifest is invalid: {0}")]
  InvalidManifest(String),
  #[error("{0}")]
  Io(String),
}

fn reset_text(reset: Option<u64>) -> String {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |elapsed| elapsed.as_secs());
  match reset {
    Some(reset) if reset > now => {
      let minutes = (reset - now).div_ceil(60);
      format!("in {minutes} minute{}", if minutes == 1 { "" } else { "s" })
    }
    _ => "later".into(),
  }
}

/// Whether `release` is newer than the installed extension: by version if
/// its tag names one, else by being another tag than the one installed.
///
/// Manifests carry `major.minor.patch` only, so a prerelease (tag
/// `v1.0.0-alpha.1`) installs with the version of the release it leads to
/// (`1.0.0`). The installed version is therefore the tag it came from, when
/// that names one, so `v1.0.0` is an update to it.
pub fn is_update(release: &Release, installed: &Installed) -> bool {
  let source = installed.source.as_ref();
  match release.version() {
    Some(version) => {
      let current = source
        .and_then(|source| tag_version(&source.tag))
        .unwrap_or_else(|| installed.version.clone());
      version > current
    }
    None => source.is_some_and(|source| source.tag != release.tag),
  }
}

/// Downloads a release's package and stages it.
pub fn fetch(
  client: &Client,
  installer: &Installer,
  release: &Release,
) -> Result<Staged, InstallError> {
  let asset = release
    .package
    .as_ref()
    .ok_or_else(|| InstallError::NoPackage(release.tag.clone()))?;
  let mut file = installer.download_file()?;
  client.download(asset, file.as_file_mut())?;
  let reopened = file
    .reopen()
    .map_err(|error| InstallError::Io(error.to_string()))?;
  installer.stage(reopened)
}
