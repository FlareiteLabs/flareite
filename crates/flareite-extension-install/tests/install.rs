//! Finding, downloading, and installing extensions, against a local server
//! that answers as GitHub's API and raw file host would.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use flareite_extension_install::{
  Client, InstallError, Installer, Release, Repo, SOURCE_FILE, Source, fetch, is_update,
};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

/// Canned answers by path (with query): status, headers, and body.
type Routes = Arc<Mutex<BTreeMap<String, (u16, Vec<(String, String)>, Vec<u8>)>>>;

struct Server {
  base: String,
  routes: Routes,
}

impl Server {
  fn start() -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let routes: Routes = Arc::default();
    let shared = routes.clone();
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut request = String::new();
        if reader.read_line(&mut request).is_err() {
          continue;
        }
        // Skip the headers.
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
          line.clear();
        }
        let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
        let (status, headers, body) = shared
          .lock()
          .expect("routes")
          .get(&path)
          .cloned()
          .unwrap_or((404, Vec::new(), b"{\"message\":\"Not Found\"}".to_vec()));
        let mut head = format!(
          "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
          body.len()
        );
        for (name, value) in headers {
          head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
      }
    });
    Self { base, routes }
  }

  fn route(&self, path: &str, status: u16, body: impl Into<Vec<u8>>) {
    self.route_with(path, status, Vec::new(), body);
  }

  fn route_with(
    &self,
    path: &str,
    status: u16,
    headers: Vec<(&str, &str)>,
    body: impl Into<Vec<u8>>,
  ) {
    let headers = headers
      .into_iter()
      .map(|(name, value)| (name.to_owned(), value.to_owned()))
      .collect();
    self
      .routes
      .lock()
      .expect("routes")
      .insert(path.to_owned(), (status, headers, body.into()));
  }

  fn client(&self) -> Client {
    Client::with_hosts("Flareite-tests", &self.base, &format!("{}/raw", self.base)).expect("client")
  }
}

fn manifest(id: &str, version: &str) -> String {
  format!(
    "id = \"{id}\"\nname = \"Hello\"\nversion = \"{version}\"\ndescription = \"Says hello.\"\n\
     author = \"Flareite\"\napi_version = \"2\"\nentrypoint = \"hello.wasm\"\n\
     [permissions]\ndocuments_read = true\n"
  )
}

/// A zip file of `files` (name, contents).
fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
  let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
  for (name, contents) in files {
    writer
      .start_file(*name, SimpleFileOptions::default())
      .expect("start");
    writer.write_all(contents).expect("write");
  }
  writer.finish().expect("finish").into_inner()
}

fn package(id: &str, version: &str) -> Vec<u8> {
  let manifest = manifest(id, version);
  zip_of(&[
    ("extension.toml", manifest.as_bytes()),
    ("hello.wasm", b"\0asm"),
    ("media/page.html", b"<p>hi</p>"),
  ])
}

fn digest(bytes: &[u8]) -> String {
  let hex: String = Sha256::digest(bytes)
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect();
  format!("sha256:{hex}")
}

/// A release's JSON, with an `extension.zip` served at `/download/<tag>`.
fn release_json(server: &Server, tag: &str, prerelease: bool, package: Option<&[u8]>) -> String {
  let assets = package.map_or_else(String::new, |bytes| {
    let url = format!("{}/download/{tag}", server.base);
    server.route(&format!("/download/{tag}"), 200, bytes.to_vec());
    format!(
      r#"{{"name":"extension.zip","size":{},"browser_download_url":"{url}","digest":"{}"}}"#,
      bytes.len(),
      digest(bytes)
    )
  });
  format!(
    r#"{{"tag_name":"{tag}","name":"{tag}","draft":false,"prerelease":{prerelease},
        "published_at":"2026-11-05T00:00:00Z","html_url":"https://github.com/o/r/releases/{tag}",
        "assets":[{assets}]}}"#
  )
}

fn installer(dir: &Path) -> Installer {
  Installer {
    extensions_dir: dir.join("extensions"),
    staging_dir: dir.join("extension-staging"),
    editor_version: semver::Version::new(0, 1, 0),
  }
}

fn repo() -> Repo {
  Repo::parse("FlareiteLabs/hello").expect("repo")
}

const RELEASES: &str = "/repos/FlareiteLabs/hello/releases?per_page=30";

#[test]
fn releases_leave_out_drafts_and_the_latest_is_stable_with_a_package() {
  let server = Server::start();
  let draft = r#"{"tag_name":"v3.0.0","draft":true,"html_url":"x","assets":[]}"#;
  let releases = format!(
    "[{draft},{},{},{},{}]",
    release_json(
      &server,
      "v2.1.0",
      true,
      Some(&package("test.hello", "2.1.0"))
    ),
    release_json(&server, "v2.0.0", false, None),
    release_json(
      &server,
      "v1.0.0",
      false,
      Some(&package("test.hello", "1.0.0"))
    ),
    release_json(
      &server,
      "v0.9.0",
      false,
      Some(&package("test.hello", "0.9.0"))
    ),
  );
  server.route(RELEASES, 200, releases);
  let client = server.client();

  let tags: Vec<String> = client
    .releases(&repo())
    .expect("releases")
    .into_iter()
    .map(|release| release.tag)
    .collect();
  assert_eq!(tags, ["v2.1.0", "v2.0.0", "v1.0.0", "v0.9.0"]);
  let latest = client.latest(&repo()).expect("latest");
  assert_eq!(
    latest.tag, "v1.0.0",
    "not the prerelease, nor the one without a package"
  );
  assert!(latest.package.is_some());

  let missing = client
    .releases(&Repo::parse("FlareiteLabs/missing").expect("repo"))
    .expect_err("missing");
  assert_eq!(
    missing,
    InstallError::NotFound("FlareiteLabs/missing".into())
  );
}

#[test]
fn the_rate_limit_is_explained() {
  let server = Server::start();
  server.route_with(
    RELEASES,
    403,
    vec![("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "1")],
    "{}",
  );
  let error = server.client().releases(&repo()).expect_err("limited");
  assert_eq!(error, InstallError::RateLimited(Some(1)));
  assert!(error.to_string().contains("try again later"), "{error}");
}

#[test]
fn the_catalog_lists_an_owners_extensions_at_their_latest_release() {
  let server = Server::start();
  let repos = r#"[
    {"name":"hello","owner":{"login":"FlareiteLabs"},"topics":["flareite-extension"]},
    {"name":"no-release","owner":{"login":"FlareiteLabs"},"topics":["flareite-extension"]},
    {"name":"old","owner":{"login":"FlareiteLabs"},"archived":true,"topics":["flareite-extension"]},
    {"name":"flareite","owner":{"login":"FlareiteLabs"},"topics":["editor"]}
  ]"#;
  // Not an organization: the user's repositories.
  server.route(
    "/users/FlareiteLabs/repos?type=owner&per_page=100&sort=full_name",
    200,
    repos,
  );
  let release = release_json(
    &server,
    "v1.0.0",
    false,
    Some(&package("test.hello", "1.0.0")),
  );
  server.route(RELEASES, 200, format!("[{release}]"));
  server.route(
    "/repos/FlareiteLabs/no-release/releases?per_page=30",
    200,
    "[]",
  );
  server.route(
    "/raw/FlareiteLabs/hello/v1.0.0/extension.toml",
    200,
    manifest("test.hello", "1.0.0"),
  );

  let catalog = server.client().catalog("FlareiteLabs").expect("catalog");
  assert_eq!(catalog.len(), 1, "{catalog:?}");
  let entry = &catalog[0];
  assert_eq!(entry.repo, repo());
  assert_eq!(
    (
      entry.id.as_str(),
      entry.name.as_str(),
      entry.version.as_str(),
      entry.tag.as_str()
    ),
    ("test.hello", "Hello", "1.0.0", "v1.0.0")
  );
  assert_eq!(entry.author.as_deref(), Some("Flareite"));
}

fn release(server: &Server, tag: &str, bytes: &[u8]) -> Release {
  server.route(
    RELEASES,
    200,
    format!("[{}]", release_json(server, tag, false, Some(bytes))),
  );
  server.client().latest(&repo()).expect("latest")
}

#[test]
fn installing_unpacks_records_its_source_and_replaces_older_versions() {
  let server = Server::start();
  let dir = tempfile::tempdir().expect("temp dir");
  let installer = installer(dir.path());
  let client = server.client();

  let first = release(&server, "v1.0.0", &package("test.hello", "1.0.0"));
  let staged = fetch(&client, &installer, &first).expect("staged");
  assert_eq!(staged.summary.id, "test.hello");
  assert_eq!(staged.summary.version, "1.0.0");
  assert_eq!(staged.summary.permissions.len(), 1);
  let source = Source::new(repo(), first.tag.clone(), None);
  let folder = installer.install(staged, &source).expect("installed");
  assert_eq!(folder, dir.path().join("extensions/test.hello"));
  assert!(folder.join("hello.wasm").is_file());
  assert!(folder.join("media/page.html").is_file());
  assert!(folder.join(SOURCE_FILE).is_file());
  let installed = installer.find("test.hello").expect("installed");
  assert_eq!(
    installed.source.as_ref().map(|s| s.tag.as_str()),
    Some("v1.0.0")
  );
  assert!(!is_update(&first, &installed));

  // A newer release replaces it in the same folder.
  let second = release(&server, "v1.1.0", &package("test.hello", "1.1.0"));
  assert!(is_update(&second, &installed));
  let staged = fetch(&client, &installer, &second).expect("staged");
  installer
    .install(staged, &Source::new(repo(), second.tag.clone(), None))
    .expect("updated");
  let installed = installer.installed();
  assert_eq!(installed.len(), 1);
  assert_eq!(installed[0].version, semver::Version::new(1, 1, 0));
  assert_eq!(installed[0].dir, folder);

  installer.uninstall("test.hello").expect("uninstalled");
  assert!(!folder.exists());
  assert!(installer.installed().is_empty());
  assert!(installer.uninstall("test.hello").is_err());
}

#[test]
fn an_extension_copied_in_by_hand_is_replaced_in_its_own_folder() {
  let server = Server::start();
  let dir = tempfile::tempdir().expect("temp dir");
  let installer = installer(dir.path());
  let by_hand = dir.path().join("extensions/hello");
  std::fs::create_dir_all(&by_hand).expect("mkdir");
  std::fs::write(
    by_hand.join("extension.toml"),
    manifest("test.hello", "0.1.0"),
  )
  .expect("write");
  std::fs::write(by_hand.join("old.txt"), "old").expect("write");
  assert_eq!(installer.find("test.hello").expect("found").source, None);

  let latest = release(&server, "v1.0.0", &package("test.hello", "1.0.0"));
  let staged = fetch(&server.client(), &installer, &latest).expect("staged");
  let folder = installer
    .install(staged, &Source::new(repo(), latest.tag, None))
    .expect("installed");
  assert_eq!(folder, by_hand);
  assert!(!by_hand.join("old.txt").exists(), "the old files are gone");
  assert_eq!(installer.installed().len(), 1);
}

#[test]
fn downloads_must_match_their_digest() {
  let server = Server::start();
  let dir = tempfile::tempdir().expect("temp dir");
  let installer = installer(dir.path());
  let mut latest = release(&server, "v1.0.0", &package("test.hello", "1.0.0"));
  if let Some(asset) = latest.package.as_mut() {
    asset.digest = Some(digest(b"something else"));
  }
  let error = fetch(&server.client(), &installer, &latest)
    .err()
    .expect("rejected");
  assert_eq!(error, InstallError::DigestMismatch);
  assert!(installer.installed().is_empty());
}

#[test]
fn unsafe_or_broken_packages_are_refused() {
  let server = Server::start();
  let dir = tempfile::tempdir().expect("temp dir");
  let installer = installer(dir.path());
  let manifest = manifest("test.hello", "1.0.0");
  let cases: Vec<(&str, Vec<u8>, &str)> = vec![
    ("not a zip", b"hello".to_vec(), "isn't a zip file"),
    (
      "escapes its folder",
      zip_of(&[
        ("extension.toml", manifest.as_bytes()),
        ("../evil.txt", b"x"),
      ]),
      "outside its folder",
    ),
    (
      "no manifest",
      zip_of(&[("hello.wasm", b"\0asm")]),
      "no extension.toml",
    ),
    (
      "no entrypoint",
      zip_of(&[("extension.toml", manifest.as_bytes())]),
      "entrypoint hello.wasm isn't in the package",
    ),
    (
      "invalid manifest",
      zip_of(&[("extension.toml", b"id = \"Not Valid\"")]),
      "manifest is invalid",
    ),
  ];
  for (what, bytes, expected) in cases {
    let latest = release(&server, "v1.0.0", &bytes);
    let error = fetch(&server.client(), &installer, &latest)
      .err()
      .expect(what);
    assert!(error.to_string().contains(expected), "{what}: {error}");
  }
  assert!(!dir.path().join("evil.txt").exists());
  assert!(!dir.path().join("extension-staging/evil.txt").exists());
  assert!(installer.installed().is_empty());

  // A package of one folder is fine.
  let nested = zip_of(&[
    ("hello/extension.toml", manifest.as_bytes()),
    ("hello/hello.wasm", b"\0asm"),
  ]);
  let latest = release(&server, "v1.0.0", &nested);
  let staged = fetch(&server.client(), &installer, &latest).expect("nested");
  assert_eq!(staged.summary.name, "Hello");
}

/// The app's commands are async: the client must be made, used, and
/// dropped on a blocking thread, since it runs a runtime of its own.
#[test]
fn the_client_works_from_an_async_runtime_on_a_blocking_thread() {
  let server = Server::start();
  server.route(RELEASES, 200, "[]");
  let (api, raw) = (server.base.clone(), format!("{}/raw", server.base));
  let runtime = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("runtime");
  let releases = runtime.block_on(async move {
    tokio::task::spawn_blocking(move || {
      let client = Client::with_hosts("Flareite-tests", &api, &raw)?;
      client.releases(&repo())
    })
    .await
    .expect("no panic")
  });
  assert_eq!(releases, Ok(Vec::new()));
}

/// Manifests carry only `major.minor.patch`, so an alpha of 1.0.0 installs as
/// 1.0.0; the release 1.0.0 is still an update to it.
#[test]
fn a_prerelease_is_updated_by_the_release_it_leads_to() {
  let server = Server::start();
  let dir = tempfile::tempdir().expect("temp dir");
  let installer = installer(dir.path());
  let alpha = release(&server, "v1.0.0-alpha.1", &package("test.hello", "1.0.0"));
  let staged = fetch(&server.client(), &installer, &alpha).expect("staged");
  installer
    .install(staged, &Source::new(repo(), alpha.tag.clone(), None))
    .expect("installed");
  let installed = installer.find("test.hello").expect("installed");
  assert_eq!(installed.version, semver::Version::new(1, 0, 0));

  let final_release = release(&server, "v1.0.0", &package("test.hello", "1.0.0"));
  assert!(is_update(&final_release, &installed));
  assert!(!is_update(&alpha, &installed));

  // Copied in by hand, there's no tag: the manifest's version is all there is.
  let by_hand = flareite_extension_install::Installed {
    source: None,
    ..installed
  };
  assert!(!is_update(&final_release, &by_hand));
}
