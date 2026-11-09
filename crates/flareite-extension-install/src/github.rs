//! GitHub's REST API: a repository's releases, and the repositories an
//! owner lists as extensions.
//!
//! Requests are anonymous, so GitHub allows 60 an hour from one address;
//! callers cache what they can. Manifests for the catalog come from
//! `raw.githubusercontent.com`, which doesn't count toward that.

use std::io::{Read, Write};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::InstallError;

pub const API: &str = "https://api.github.com";
pub const RAW: &str = "https://raw.githubusercontent.com";
/// The asset that holds a release's extension.
pub const PACKAGE_ASSET: &str = "extension.zip";
/// The topic that lists a repository in its owner's catalog.
pub const CATALOG_TOPIC: &str = "flareite-extension";
/// Largest package downloaded.
pub const MAX_PACKAGE_BYTES: u64 = 100 * 1024 * 1024;
/// Largest API response or manifest read.
const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
/// Releases listed per repository, newest first.
const RELEASES: u32 = 30;
/// Repositories looked at per catalog owner.
const CATALOG_REPOSITORIES: u32 = 100;

/// A GitHub repository, `owner/name`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Repo {
  pub owner: String,
  pub name: String,
}

impl Repo {
  /// `owner/name`, or a GitHub address of the repository or a page in
  /// it (`https://github.com/owner/name/releases`, `git@github.com:...`).
  pub fn parse(input: &str) -> Result<Self, InstallError> {
    let invalid = || InstallError::InvalidRepository(input.trim().to_owned());
    let mut rest = input.trim();
    for prefix in ["https://", "http://", "ssh://", "git@"] {
      if let Some(stripped) = rest.strip_prefix(prefix) {
        rest = stripped;
      }
    }
    for host in ["www.github.com", "github.com"] {
      if rest.len() > host.len() && rest[..host.len()].eq_ignore_ascii_case(host) {
        let after = &rest[host.len()..];
        if let Some(path) = after.strip_prefix('/').or_else(|| after.strip_prefix(':')) {
          rest = path;
          break;
        }
      }
    }
    let mut parts = rest.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let name = name.strip_suffix(".git").unwrap_or(name);
    let valid_owner = !owner.is_empty()
      && owner.len() <= 39
      && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let valid_name = !name.is_empty()
      && name.len() <= 100
      && name != "."
      && name != ".."
      && name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid_owner || !valid_name {
      return Err(invalid());
    }
    Ok(Self {
      owner: owner.to_owned(),
      name: name.to_owned(),
    })
  }

  /// The repository's page.
  pub fn url(&self) -> String {
    format!("https://github.com/{self}")
  }
}

impl std::fmt::Display for Repo {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}/{}", self.owner, self.name)
  }
}

impl From<Repo> for String {
  fn from(repo: Repo) -> Self {
    repo.to_string()
  }
}

impl TryFrom<String> for Repo {
  type Error = InstallError;
  fn try_from(text: String) -> Result<Self, Self::Error> {
    Self::parse(&text)
  }
}

/// A published release (drafts aren't listed).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
  pub tag: String,
  /// Its title, if it has one besides the tag.
  pub name: Option<String>,
  pub prerelease: bool,
  /// ISO 8601.
  pub published_at: Option<String>,
  /// The release's page.
  pub url: String,
  /// Its `extension.zip`, if it has one.
  pub package: Option<Asset>,
  /// Every file it has, `extension.zip` among them.
  #[serde(skip)]
  pub files: Vec<ReleaseFile>,
}

/// A file attached to a release.
#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseFile {
  pub name: String,
  pub asset: Asset,
}

impl Release {
  /// The version the tag names (`v1.2.0` or `1.2.0`), if it names one.
  pub fn version(&self) -> Option<semver::Version> {
    tag_version(&self.tag)
  }

  /// Its file named `name`, if it has one.
  pub fn file(&self, name: &str) -> Option<&ReleaseFile> {
    self.files.iter().find(|file| file.name == name)
  }
}

/// The version a tag names (`v1.2.0` or `1.2.0`).
pub fn tag_version(tag: &str) -> Option<semver::Version> {
  semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Asset {
  pub url: String,
  pub size: u64,
  /// `sha256:<hex>`, as GitHub reports it.
  pub digest: Option<String>,
}

/// An extension in an owner's catalog: a repository with the catalog topic
/// and a release with a package.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
  pub repo: Repo,
  /// From the release's `extension.toml`.
  pub id: String,
  pub name: String,
  pub description: String,
  pub author: Option<String>,
  pub version: String,
  /// The release it's from: the newest that isn't a prerelease.
  pub tag: String,
}

#[derive(Deserialize)]
struct RawRelease {
  tag_name: String,
  name: Option<String>,
  #[serde(default)]
  draft: bool,
  #[serde(default)]
  prerelease: bool,
  published_at: Option<String>,
  html_url: String,
  #[serde(default)]
  assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
  name: String,
  size: u64,
  browser_download_url: String,
  digest: Option<String>,
}

#[derive(Deserialize)]
struct RawRepo {
  name: String,
  owner: RawOwner,
  #[serde(default)]
  archived: bool,
  #[serde(default)]
  topics: Vec<String>,
}

#[derive(Deserialize)]
struct RawOwner {
  login: String,
}

/// The fields of a manifest the catalog shows.
#[derive(Deserialize)]
struct RawManifest {
  id: String,
  name: String,
  version: String,
  #[serde(default)]
  description: String,
  author: Option<String>,
}

impl From<RawRelease> for Release {
  fn from(raw: RawRelease) -> Self {
    let files: Vec<ReleaseFile> = raw
      .assets
      .into_iter()
      .map(|asset| ReleaseFile {
        name: asset.name,
        asset: Asset {
          url: asset.browser_download_url,
          size: asset.size,
          digest: asset.digest,
        },
      })
      .collect();
    let package = files
      .iter()
      .find(|file| file.name == PACKAGE_ASSET)
      .map(|file| file.asset.clone());
    Self {
      name: raw
        .name
        .filter(|name| !name.trim().is_empty() && *name != raw.tag_name),
      tag: raw.tag_name,
      prerelease: raw.prerelease,
      published_at: raw.published_at,
      url: raw.html_url,
      package,
      files,
    }
  }
}

/// Talks to GitHub. Cloning it is cheap, and clones share connections.
#[derive(Clone)]
pub struct Client {
  http: reqwest::blocking::Client,
  api: String,
  raw: String,
}

impl Client {
  /// A client for github.com. `user_agent` names the editor and its
  /// version, as GitHub asks.
  ///
  /// Its requests block, and it runs a Tokio runtime of its own, so make it
  /// and use it off any async runtime (in `spawn_blocking`, say): made or
  /// dropped on one, it panics.
  pub fn new(user_agent: &str) -> Result<Self, InstallError> {
    Self::with_hosts(user_agent, API, RAW)
  }

  /// A client for other addresses of the API and of raw files, for tests.
  /// Only HTTPS is used unless both are plain HTTP.
  pub fn with_hosts(user_agent: &str, api: &str, raw: &str) -> Result<Self, InstallError> {
    let https_only = !(api.starts_with("http://") && raw.starts_with("http://"));
    let http = reqwest::blocking::Client::builder()
      .user_agent(user_agent)
      .https_only(https_only)
      .connect_timeout(Duration::from_secs(15))
      .timeout(Duration::from_secs(300))
      .build()
      .map_err(|error| InstallError::Network(error.to_string()))?;
    Ok(Self {
      http,
      api: api.trim_end_matches('/').to_owned(),
      raw: raw.trim_end_matches('/').to_owned(),
    })
  }

  /// The repository's releases, newest first (at most 30), without drafts.
  pub fn releases(&self, repo: &Repo) -> Result<Vec<Release>, InstallError> {
    let path = format!("/repos/{repo}/releases?per_page={RELEASES}");
    let raw: Vec<RawRelease> = self.api_json(&path, &repo.to_string())?;
    Ok(
      raw
        .into_iter()
        .filter(|release| !release.draft)
        .map(Release::from)
        .collect(),
    )
  }

  /// The newest release with a package that isn't a prerelease.
  pub fn latest(&self, repo: &Repo) -> Result<Release, InstallError> {
    newest_stable(self.releases(repo)?).ok_or_else(|| InstallError::NoRelease(repo.to_string()))
  }

  /// The extensions an owner (an organization or a user) lists: their
  /// repositories with the catalog topic, at their newest release.
  /// Repositories without a usable release are left out.
  pub fn catalog(&self, owner: &str) -> Result<Vec<CatalogEntry>, InstallError> {
    let query = format!("per_page={CATALOG_REPOSITORIES}&sort=full_name");
    let repositories: Vec<RawRepo> =
      match self.api_json(&format!("/orgs/{owner}/repos?type=public&{query}"), owner) {
        Err(InstallError::NotFound(_)) => {
          self.api_json(&format!("/users/{owner}/repos?type=owner&{query}"), owner)?
        }
        other => other?,
      };
    let mut entries = Vec::new();
    for repository in repositories {
      if repository.archived || !repository.topics.iter().any(|t| t == CATALOG_TOPIC) {
        continue;
      }
      let repo = Repo {
        owner: repository.owner.login,
        name: repository.name,
      };
      match self.catalog_entry(&repo) {
        Ok(Some(entry)) => entries.push(entry),
        Ok(None) => {}
        // One repository's problem doesn't hide the others.
        Err(InstallError::RateLimited(reset)) => return Err(InstallError::RateLimited(reset)),
        Err(error) => tracing::warn!(%repo, %error, "skipping a catalog repository"),
      }
    }
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    Ok(entries)
  }

  fn catalog_entry(&self, repo: &Repo) -> Result<Option<CatalogEntry>, InstallError> {
    let Some(release) = newest_stable(self.releases(repo)?) else {
      return Ok(None);
    };
    let url = format!("{}/{repo}/{}/extension.toml", self.raw, release.tag);
    let text = self.text(&url, &repo.to_string())?;
    let manifest: RawManifest = toml::from_str(&text)
      .map_err(|error| InstallError::InvalidManifest(format!("{repo}: {error}")))?;
    Ok(Some(CatalogEntry {
      repo: repo.clone(),
      id: manifest.id,
      name: manifest.name,
      description: manifest.description,
      author: manifest.author,
      version: manifest.version,
      tag: release.tag,
    }))
  }

  /// Downloads a package into `to`, checking its size and, when GitHub
  /// reports one, its digest.
  pub fn download(&self, asset: &Asset, to: &mut impl Write) -> Result<(), InstallError> {
    if asset.size > MAX_PACKAGE_BYTES {
      return Err(too_large());
    }
    let response = self
      .http
      .get(&asset.url)
      .header("Accept", "application/octet-stream")
      .send()
      .map_err(network)?;
    let response = check(response, "the package")?;
    let mut body = response.take(MAX_PACKAGE_BYTES + 1);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut total: u64 = 0;
    loop {
      let read = body
        .read(&mut buffer)
        .map_err(|e| InstallError::Network(e.to_string()))?;
      if read == 0 {
        break;
      }
      total += read as u64;
      if total > MAX_PACKAGE_BYTES {
        return Err(too_large());
      }
      hasher.update(&buffer[..read]);
      to.write_all(&buffer[..read])
        .map_err(|error| InstallError::Io(error.to_string()))?;
    }
    if let Some(expected) = asset.digest.as_deref() {
      match expected.split_once(':') {
        Some(("sha256", hex)) => {
          let actual: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
          if !actual.eq_ignore_ascii_case(hex) {
            return Err(InstallError::DigestMismatch);
          }
        }
        _ => tracing::debug!(expected, "unknown digest algorithm; not checked"),
      }
    }
    Ok(())
  }

  fn api_json<T: DeserializeOwned>(&self, path: &str, what: &str) -> Result<T, InstallError> {
    let response = self
      .http
      .get(format!("{}{path}", self.api))
      .header("Accept", "application/vnd.github+json")
      .header("X-GitHub-Api-Version", "2022-11-28")
      .send()
      .map_err(network)?;
    let bytes = read_limited(check(response, what)?)?;
    serde_json::from_slice(&bytes)
      .map_err(|error| InstallError::Network(format!("GitHub's answer can't be read: {error}")))
  }

  fn text(&self, url: &str, what: &str) -> Result<String, InstallError> {
    let response = self.http.get(url).send().map_err(network)?;
    let bytes = read_limited(check(response, what)?)?;
    String::from_utf8(bytes)
      .map_err(|_| InstallError::InvalidManifest(format!("{what}: not UTF-8")))
  }
}

fn newest_stable(releases: Vec<Release>) -> Option<Release> {
  releases
    .into_iter()
    .find(|release| !release.prerelease && release.package.is_some())
}

fn network(error: reqwest::Error) -> InstallError {
  InstallError::Network(error.to_string())
}

fn too_large() -> InstallError {
  InstallError::TooLarge(format!(
    "the package is larger than {} MiB",
    MAX_PACKAGE_BYTES / 1024 / 1024
  ))
}

fn check(
  response: reqwest::blocking::Response,
  what: &str,
) -> Result<reqwest::blocking::Response, InstallError> {
  let status = response.status();
  if status.is_success() {
    return Ok(response);
  }
  let header = |name: &str| {
    response
      .headers()
      .get(name)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned)
  };
  if (status.as_u16() == 403 || status.as_u16() == 429)
    && header("x-ratelimit-remaining").as_deref() == Some("0")
  {
    let reset = header("x-ratelimit-reset").and_then(|reset| reset.parse().ok());
    return Err(InstallError::RateLimited(reset));
  }
  if status.as_u16() == 404 {
    return Err(InstallError::NotFound(what.to_owned()));
  }
  Err(InstallError::Network(format!(
    "GitHub answered {status} for {what}"
  )))
}

fn read_limited(response: reqwest::blocking::Response) -> Result<Vec<u8>, InstallError> {
  let mut bytes = Vec::new();
  response
    .take(MAX_RESPONSE_BYTES + 1)
    .read_to_end(&mut bytes)
    .map_err(|error| InstallError::Network(error.to_string()))?;
  if bytes.len() as u64 > MAX_RESPONSE_BYTES {
    return Err(InstallError::Network("GitHub's answer is too large".into()));
  }
  Ok(bytes)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn repo(input: &str) -> Option<String> {
    Repo::parse(input).ok().map(|repo| repo.to_string())
  }

  #[test]
  fn repositories_are_named_or_linked() {
    let expected = Some("FlareiteLabs/flareite-git-ext".to_owned());
    for input in [
      "FlareiteLabs/flareite-git-ext",
      " FlareiteLabs/flareite-git-ext ",
      "github.com/FlareiteLabs/flareite-git-ext",
      "https://github.com/FlareiteLabs/flareite-git-ext",
      "https://www.github.com/FlareiteLabs/flareite-git-ext/",
      "https://github.com/FlareiteLabs/flareite-git-ext.git",
      "https://github.com/FlareiteLabs/flareite-git-ext/releases/tag/v0.5.0",
      "git@github.com:FlareiteLabs/flareite-git-ext.git",
    ] {
      assert_eq!(repo(input), expected, "{input}");
    }
    for input in [
      "",
      "flareite-git-ext",
      "https://gitlab.com/FlareiteLabs/flareite-git-ext",
      "owner/..",
      "own er/name",
      "owner/na$me",
    ] {
      assert_eq!(repo(input), None, "{input}");
    }
  }

  #[test]
  fn tags_name_versions_with_or_without_a_v() {
    assert_eq!(tag_version("v1.2.0"), Some(semver::Version::new(1, 2, 0)));
    assert_eq!(tag_version("0.5.0"), Some(semver::Version::new(0, 5, 0)));
    assert_eq!(tag_version("latest"), None);
  }
}
