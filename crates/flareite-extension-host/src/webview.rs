//! Webviews (API 1.5): viewers and previews that show a page of the
//! extension's own (HTML, styles, scripts) instead of preview content.
//!
//! The workbench shows each page in a sandboxed frame (`allow-scripts`
//! only, so the page has an origin of its own and can't reach the editor's
//! IPC or the workbench's document), loaded from the `flareite-webview:`
//! protocol, which the embedder serves with [`serve`]:
//!
//! - **Addresses** are `<origin>/<token>/<path>`. The token is random and
//!   made for one open tab, so a page can't name another tab's files.
//! - **Files** are the extension's own, confined to its folder (symlinks
//!   included), plus, in a viewer, the file open in it
//!   (`__flareite/file`). HTML set with `set-webview-html` is the root.
//! - **Every response carries a content security policy** that lets the
//!   page load only from its own address and `data:`/`blob:` URLs: no
//!   network, no forms, no other frames.
//! - **Pages get the bridge** (`webview.js`, the `flareite` object) as their
//!   first script; messages go through the workbench, which checks where
//!   they came from. Before it come the theme, as `--flareite-*` variables,
//!   and low-priority defaults (`webview-defaults.css`); pages can opt in to
//!   the kit (`__flareite/kit.css`), with the named icons.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};

use serde::Serialize;

/// The protocol pages are served from.
pub const SCHEME: &str = "flareite-webview";
/// Largest page `set-webview-html` takes.
pub const MAX_HTML_BYTES: usize = 4 * 1024 * 1024;
/// Largest message between a page and its extension.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Largest file served to a page.
pub const MAX_SERVED_BYTES: u64 = 256 * 1024 * 1024;
/// Where Flareite's own files are, under a page's address.
const RESERVED: &str = "__flareite/";

const BRIDGE: &str = include_str!("webview.js");
/// The script a file page gets instead (see [`serve_file_page`]).
const FILE_PAGE_BRIDGE: &str = include_str!("file-page.js");
const DEFAULTS: &str = include_str!("webview-defaults.css");
/// The kit, with a rule per named icon.
static KIT: LazyLock<String> = LazyLock::new(|| {
  let mut kit = include_str!("webview-kit.css").to_owned();
  for (name, path) in crate::contributions::ACTION_ICONS.iter() {
    let svg = format!(
      "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' \
       stroke-width='2' stroke-linecap='round' stroke-linejoin='round'><path d='{path}'/></svg>"
    );
    let _ = write!(
      kit,
      "\n.flareite-icon[data-icon='{name}'] {{\n  --flareite-icon: url(\"data:image/svg+xml,{}\");\n}}\n",
      escape_data(&svg)
    );
  }
  kit
});
/// Most theme variables, and the longest name and value.
const MAX_THEME_VARIABLES: usize = 400;
const MAX_THEME_VALUE_CHARS: usize = 300;

/// The workbench's theme, which pages get as `--flareite-<name>` variables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebviewTheme {
  /// `dark` or `light`.
  pub kind: String,
  /// By name, without `--flareite-`.
  pub variables: BTreeMap<String, String>,
}

impl WebviewTheme {
  /// A theme from the workbench, checked so it can only set variables: no
  /// name or value can end the rule or the `<style>` element.
  pub fn new(kind: &str, variables: BTreeMap<String, String>) -> Result<Self, String> {
    if !matches!(kind, "dark" | "light") {
      return Err(format!("no theme kind {kind:?}"));
    }
    if variables.len() > MAX_THEME_VARIABLES {
      return Err("too many theme variables".into());
    }
    for (name, value) in &variables {
      let name_ok = !name.is_empty()
        && name.len() <= 64
        && name
          .bytes()
          .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
      let value_ok = value.chars().count() <= MAX_THEME_VALUE_CHARS
        && !value.contains(['<', '>', '{', '}', ';', '\\']);
      if !name_ok || !value_ok {
        return Err(format!("theme variable {name:?} isn't valid"));
      }
    }
    Ok(Self {
      kind: kind.to_owned(),
      variables,
    })
  }

  /// The variables as a rule for the page's root.
  fn css(&self) -> String {
    let mut css = String::from(":root {");
    if !self.kind.is_empty() {
      let _ = write!(css, " --flareite-color-scheme: {};", self.kind);
    }
    for (name, value) in &self.variables {
      let _ = write!(css, " --flareite-{name}: {value};");
    }
    css.push_str(" }");
    css
  }
}

/// The origin pages are served from, as the webview addresses custom
/// protocols: `http://<scheme>.localhost` on Windows, `<scheme>://localhost`
/// elsewhere.
pub fn origin() -> String {
  if cfg!(windows) {
    format!("http://{SCHEME}.localhost")
  } else {
    format!("{SCHEME}://localhost")
  }
}

/// A new token for a tab's page: 128 random bits as hex.
pub fn new_token() -> String {
  let mut bytes = [0u8; 16];
  if let Err(error) = getrandom::fill(&mut bytes) {
    // Without randomness, tokens would be guessable; fail loudly instead.
    panic!("no randomness for a webview token: {error}");
  }
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The page shown in an open viewer or preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebviewRecord {
  pub token: String,
  /// The manifest's page, relative to the extension's folder.
  pub page: Option<String>,
  /// HTML the extension set, which replaces `page`.
  pub html: Option<Arc<str>>,
  /// Increases when the page is replaced, so the workbench loads it again.
  pub version: u64,
  /// What the page saved with `flareite.setState` (JSON text).
  pub state: Option<String>,
  /// For a layout's file page pane: the file it shows (1.15). Its page is
  /// served from the open folder by [`serve_file_page`].
  pub file_page: Option<PathBuf>,
}

impl WebviewRecord {
  pub fn new(page: Option<String>) -> Self {
    Self {
      token: new_token(),
      page,
      html: None,
      version: 0,
      state: None,
      file_page: None,
    }
  }

  /// Where the workbench loads the page from.
  pub fn info(&self) -> WebviewInfo {
    let page = match (&self.html, &self.page) {
      (None, Some(page)) => encode_path(page),
      _ => String::new(),
    };
    WebviewInfo {
      url: format!("{}/{}/{page}", origin(), self.token),
      version: self.version,
      blocked: None,
    }
  }
}

/// How the workbench shows a webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewInfo {
  /// The page's address.
  pub url: String,
  /// Changes when the page is replaced.
  pub version: u64,
  /// Why a file page can't be shown, if it can't (an untrusted folder, a
  /// file outside it).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub blocked: Option<String>,
}

/// What a page may load: everything [`serve`] needs to answer it.
pub struct Site<'a> {
  /// The extension's folder.
  pub dir: &'a Path,
  pub record: &'a WebviewRecord,
  /// In a viewer, the file open in it.
  pub file: Option<&'a Path>,
  /// Whether the extension has the `network` permission.
  pub network: bool,
  pub theme: &'a WebviewTheme,
}

/// A response to a page's request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
  pub body: Vec<u8>,
  pub mime: &'static str,
  /// The content security policy to send with it.
  pub csp: String,
}

/// Answers a request for `path` (percent-encoded, after the token) from a
/// page. `Err` means not found.
pub fn serve(site: &Site<'_>, path: &str) -> Result<Resource, String> {
  let path = decode_path(path).ok_or("the address isn't valid")?;
  let resource = |body: Vec<u8>, mime: &'static str| Resource {
    body,
    mime,
    csp: policy(&site.record.token, site.network),
  };
  if path.is_empty() {
    let html = site.record.html.as_deref().unwrap_or("<!doctype html>");
    return Ok(resource(inject(html, site).into_bytes(), "text/html"));
  }
  if let Some(own) = reserved(&path) {
    return match own {
      "webview.js" => Ok(resource(bridge(site).into_bytes(), "text/javascript")),
      "kit.css" => Ok(resource(KIT.clone().into_bytes(), "text/css")),
      "file" => {
        let file = site.file.ok_or("only a viewer has a file")?;
        Ok(resource(read(file)?, mime_type(file)))
      }
      _ => Err(format!("no file {path}")),
    };
  }
  let file = confined(site.dir, &path)?;
  let body = read(&file)?;
  let mime = mime_type(&file);
  if mime == "text/html" {
    let html = String::from_utf8_lossy(&body);
    return Ok(resource(inject(&html, site).into_bytes(), mime));
  }
  Ok(resource(body, mime))
}

/// What a file page may load: the open folder's files.
pub struct FilePageSite<'a> {
  /// The open folder.
  pub root: &'a Path,
  pub token: &'a str,
  /// Whether the layout's extension has the `network` permission.
  pub network: bool,
  /// The text of an open document of a file, which is served instead of
  /// the file, so unsaved changes show.
  pub live: &'a dyn Fn(&Path) -> Option<String>,
  /// For pages that use the kit.
  pub theme: &'a WebviewTheme,
}

/// Whether an HTML page links Flareite's kit (`__flareite/kit.css`): a
/// page being made for an extension's webview.
fn uses_kit(html: &str) -> bool {
  html.contains(&format!("{RESERVED}kit.css"))
}

/// Answers a request for `path` (percent-encoded, after the token) from a
/// file page: a layout pane showing a file of the open folder as a web
/// page (1.15). The folder's files are served as they are in the editor,
/// under the same policy as webview pages. HTML gets the file page script,
/// but not the real `flareite` object, since the page is the user's. A page
/// that links the kit (`__flareite/kit.css`, served as to webview pages) is
/// one being made for a webview: it also gets the theme's variables, the
/// webview defaults, and a stand-in `flareite` object (`kit-page.js`), so it
/// looks and runs as it will in a webview. Other pages get none of it, so
/// they look as they would in a browser.
pub fn serve_file_page(site: &FilePageSite<'_>, path: &str) -> Result<Resource, String> {
  let path = decode_path(path).ok_or("the address isn't valid")?;
  let resource = |body: Vec<u8>, mime: &'static str| Resource {
    body,
    mime,
    csp: policy(site.token, site.network),
  };
  if let Some(own) = reserved(&path) {
    return match own {
      "file-page.js" => Ok(resource(
        FILE_PAGE_BRIDGE.as_bytes().to_vec(),
        "text/javascript",
      )),
      "kit.css" => Ok(resource(KIT.clone().into_bytes(), "text/css")),
      // A page that uses the kit: the script with a stand-in `flareite`.
      "kit-page.js" => {
        let options = serde_json::json!({ "theme": site.theme.kind });
        Ok(resource(
          format!("const FLAREITE_STAND_IN = {options};\n{FILE_PAGE_BRIDGE}").into_bytes(),
          "text/javascript",
        ))
      }
      _ => Err(format!("no file {path}")),
    };
  }
  let file = confined(site.root, &path)?;
  let body = match (site.live)(&file) {
    Some(text) => text.into_bytes(),
    None => read(&file)?,
  };
  let mime = mime_type(&file);
  if mime == "text/html" {
    let html = String::from_utf8_lossy(&body);
    let mut tag = String::new();
    let kit = uses_kit(&html);
    if kit {
      let _ = write!(
        tag,
        "<style id=\"flareite-theme\">{}</style><style id=\"flareite-defaults\">{DEFAULTS}</style>",
        site.theme.css()
      );
    }
    let _ = write!(
      tag,
      "<script src=\"{}/{}/{RESERVED}{}\"></script>",
      origin(),
      site.token,
      if kit { "kit-page.js" } else { "file-page.js" }
    );
    return Ok(resource(insert_in_head(&html, &tag).into_bytes(), mime));
  }
  Ok(resource(body, mime))
}

/// The name of one of Flareite's files, which pages find under `__flareite/`
/// from any folder (`__flareite/kit.css`, `../__flareite/kit.css`).
fn reserved(path: &str) -> Option<&str> {
  let at = path.rfind(RESERVED)?;
  (at == 0 || path[..at].ends_with('/')).then(|| &path[at + RESERVED.len()..])
}

/// `text` for a `data:` URL in a quoted CSS `url()`.
fn escape_data(text: &str) -> String {
  let mut escaped = String::with_capacity(text.len());
  for c in text.chars() {
    match c {
      '%' => escaped.push_str("%25"),
      '#' => escaped.push_str("%23"),
      '<' => escaped.push_str("%3C"),
      '>' => escaped.push_str("%3E"),
      '"' => escaped.push_str("%22"),
      c => escaped.push(c),
    }
  }
  escaped
}

/// The policy every response carries: the page may load only from its own
/// address, and `data:` and `blob:` URLs, and with the `network`
/// permission, from `https:` and `wss:` addresses too.
fn policy(token: &str, network: bool) -> String {
  let own = format!("{}/{token}/", origin());
  let web = if network { " https:" } else { "" };
  let sockets = if network { " https: wss:" } else { "" };
  [
    "default-src 'none'".to_owned(),
    format!("script-src {own}{web} 'unsafe-inline'"),
    format!("style-src {own}{web} 'unsafe-inline'"),
    format!("img-src {own}{web} data: blob:"),
    format!("media-src {own}{web} data: blob:"),
    format!("font-src {own}{web} data:"),
    format!("connect-src {own}{sockets}"),
    "form-action 'none'".to_owned(),
    "base-uri 'none'".to_owned(),
  ]
  .join("; ")
}

/// The bridge script, after what it needs to know about the page.
fn bridge(site: &Site<'_>) -> String {
  let base = format!("{}/{}/", origin(), site.record.token);
  let state = site
    .record
    .state
    .as_deref()
    .and_then(|state| serde_json::from_str::<serde_json::Value>(state).ok());
  let page = serde_json::json!({
    "kind": if site.file.is_some() { "viewer" } else { "preview" },
    "file": site.file.map(|_| format!("{base}{RESERVED}file")),
    "fileName": site.file.and_then(Path::file_name).map(|name| name.to_string_lossy()),
    "state": state,
    "theme": site.theme.kind,
  });
  format!("const FLAREITE_PAGE = {page};\n{BRIDGE}")
}

/// `html` with the theme, the defaults, and the bridge first in its head:
/// after `<head>`, else after `<html>`, else after the doctype, else at the
/// start.
fn inject(html: &str, site: &Site<'_>) -> String {
  let tag = format!(
    "<style id=\"flareite-theme\">{}</style><style id=\"flareite-defaults\">{DEFAULTS}</style>\
     <script src=\"{}/{}/{RESERVED}webview.js\"></script>",
    site.theme.css(),
    origin(),
    site.record.token
  );
  insert_in_head(html, &tag)
}

/// `html` with `tag` first in its head: after `<head>`, else after `<html>`,
/// else after the doctype, else at the start.
fn insert_in_head(html: &str, tag: &str) -> String {
  let lower = html.to_ascii_lowercase();
  let after = |name: &str| {
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
      let start = from + found;
      // `<head>` or `<head ...>`, not `<header>`.
      let next = lower.as_bytes().get(start + name.len()).copied();
      if matches!(next, Some(b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/')) {
        return lower[start..].find('>').map(|end| start + end + 1);
      }
      from = start + name.len();
    }
    None
  };
  let at = after("<head")
    .or_else(|| after("<html"))
    .or_else(|| after("<!doctype"))
    .unwrap_or(0);
  format!("{}{tag}{}", &html[..at], &html[at..])
}

/// A file of the extension's, if `path` stays inside its folder once
/// symlinks are followed.
fn confined(dir: &Path, path: &str) -> Result<PathBuf, String> {
  let relative = Path::new(path);
  let plain = relative
    .components()
    .all(|component| matches!(component, Component::Normal(_)));
  if !plain || path.contains('\\') {
    return Err(format!("no file {path}"));
  }
  let root = dir
    .canonicalize()
    .map_err(|error| format!("the extension's folder can't be read: {error}"))?;
  let file = root
    .join(relative)
    .canonicalize()
    .map_err(|_| format!("no file {path}"))?;
  if !file.starts_with(&root) {
    return Err(format!("no file {path}"));
  }
  Ok(file)
}

fn read(file: &Path) -> Result<Vec<u8>, String> {
  let metadata = std::fs::metadata(file).map_err(|_| "the file can't be read".to_owned())?;
  if !metadata.is_file() {
    return Err("not a file".into());
  }
  if metadata.len() > MAX_SERVED_BYTES {
    return Err(format!(
      "the file is larger than {} MiB",
      MAX_SERVED_BYTES / 1024 / 1024
    ));
  }
  std::fs::read(file).map_err(|error| error.to_string())
}

/// The media type a file is served as, by its extension.
fn mime_type(file: &Path) -> &'static str {
  if let Some(image) = crate::preview::image_type(file) {
    return image;
  }
  let extension = file
    .extension()
    .and_then(|extension| extension.to_str())
    .map(str::to_ascii_lowercase);
  match extension.as_deref() {
    Some("html" | "htm") => "text/html",
    Some("js" | "mjs") => "text/javascript",
    Some("css") => "text/css",
    Some("json" | "map") => "application/json",
    Some("wasm") => "application/wasm",
    Some("txt" | "md") => "text/plain",
    Some("woff") => "font/woff",
    Some("woff2") => "font/woff2",
    Some("ttf") => "font/ttf",
    Some("otf") => "font/otf",
    Some("mp4") => "video/mp4",
    Some("webm") => "video/webm",
    Some("mp3") => "audio/mpeg",
    Some("wav") => "audio/wav",
    Some("ogg") => "audio/ogg",
    Some("pdf") => "application/pdf",
    _ => "application/octet-stream",
  }
}

/// `path` with each segment percent-encoded where an address needs it.
pub(crate) fn encode_path(path: &str) -> String {
  let mut encoded = String::new();
  for byte in path.bytes() {
    match byte {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
        encoded.push(char::from(byte));
      }
      _ => encoded.push_str(&format!("%{byte:02X}")),
    }
  }
  encoded
}

/// A percent-encoded path as text; `None` if it isn't UTF-8 or a `%`
/// isn't followed by two hex digits.
fn decode_path(path: &str) -> Option<String> {
  let bytes = path.as_bytes();
  let mut decoded = Vec::with_capacity(bytes.len());
  let mut index = 0;
  while index < bytes.len() {
    if bytes[index] == b'%' {
      let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
      decoded.push(u8::from_str_radix(hex, 16).ok()?);
      index += 3;
    } else {
      decoded.push(bytes[index]);
      index += 1;
    }
  }
  String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn record(page: Option<&str>) -> WebviewRecord {
    WebviewRecord {
      token: "t0k3n".into(),
      page: page.map(str::to_owned),
      html: None,
      version: 0,
      state: None,
      file_page: None,
    }
  }

  #[test]
  fn file_pages_are_the_folders_files_as_the_editor_has_them() {
    let root = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(root.path().join("css")).expect("folder");
    std::fs::write(
      root.path().join("index.html"),
      "<html><head></head><body>saved</body></html>",
    )
    .expect("written");
    std::fs::write(root.path().join("css/site.css"), "body {}").expect("written");
    let canonical = root.path().canonicalize().expect("canonical");
    let edited = canonical.join("index.html");
    let live = |file: &Path| {
      (file == edited).then(|| "<html><head></head><body>unsaved</body></html>".to_owned())
    };
    let theme = WebviewTheme::new(
      "dark",
      BTreeMap::from([("editor-background".to_owned(), "#1e1e2e".to_owned())]),
    )
    .expect("valid");
    let site = FilePageSite {
      root: root.path(),
      token: "t0k3n",
      network: false,
      live: &live,
      theme: &theme,
    };
    let page = serve_file_page(&site, "index.html").expect("served");
    let html = String::from_utf8(page.body).expect("text");
    assert!(html.contains("unsaved"), "the editor's text");
    assert!(
      html.contains("__flareite/file-page.js"),
      "the file page script"
    );
    assert!(
      !html.contains("flareite-theme"),
      "no theme: the page is the user's"
    );
    assert!(page.csp.contains("default-src 'none'"));
    let css = serve_file_page(&site, "css/site.css").expect("served");
    assert_eq!(css.body, b"body {}");
    assert_eq!(css.mime, "text/css");
    assert!(serve_file_page(&site, "../outside.txt").is_err());
    let kit = serve_file_page(&site, "__flareite/kit.css").expect("the kit");
    assert!(String::from_utf8_lossy(&kit.body).contains(".flareite-button"));
    assert_eq!(kit.mime, "text/css");
    std::fs::write(
      root.path().join("panel.html"),
      "<html><head><link rel=\"stylesheet\" href=\"__flareite/kit.css\"></head></html>",
    )
    .expect("written");
    let panel =
      String::from_utf8(serve_file_page(&site, "panel.html").expect("served").body).expect("text");
    assert!(
      panel.contains("--flareite-editor-background: #1e1e2e"),
      "the theme"
    );
    assert!(panel.contains("flareite-defaults"), "the defaults");
    assert!(
      !panel.contains("webview.js"),
      "not the real flareite object"
    );
    assert!(panel.contains("__flareite/kit-page.js"), "the stand-in");
    let stand_in = serve_file_page(&site, "__flareite/kit-page.js").expect("served");
    assert!(
      String::from_utf8_lossy(&stand_in.body)
        .starts_with(r#"const FLAREITE_STAND_IN = {"theme":"dark"};"#)
    );
    let script = serve_file_page(&site, "__flareite/file-page.js").expect("served");
    assert!(String::from_utf8_lossy(&script.body).contains("restoreScroll"));
  }

  #[test]
  fn tokens_are_random_hex() {
    let (a, b) = (new_token(), new_token());
    assert_eq!(a.len(), 32);
    assert!(a.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(a, b);
  }

  #[test]
  fn serves_the_extensions_files_with_the_bridge_and_a_policy() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join("media")).expect("mkdir");
    std::fs::write(
      dir.path().join("media/page.html"),
      "<!DOCTYPE html><html><HEAD><title>x</title></HEAD><body></body></html>",
    )
    .expect("write");
    std::fs::write(dir.path().join("media/a b.css"), "body{}").expect("write");
    let record = record(Some("media/page.html"));
    let site = Site {
      dir: dir.path(),
      record: &record,
      file: None,
      network: false,
      theme: &WebviewTheme::default(),
    };
    assert_eq!(
      record.info().url,
      format!("{}/t0k3n/media/page.html", origin())
    );

    let page = serve(&site, "media/page.html").expect("page");
    assert_eq!(page.mime, "text/html");
    let html = String::from_utf8(page.body).expect("utf-8");
    assert!(
      html.starts_with("<!DOCTYPE html><html><HEAD><style id=\"flareite-theme\">"),
      "{html}"
    );
    assert!(
      html.contains(&format!(
        "<script src=\"{}/t0k3n/__flareite/webview.js\"></script><title>",
        origin()
      )),
      "{html}"
    );
    assert!(page.csp.starts_with("default-src 'none'; script-src "));
    assert!(
      page
        .csp
        .contains(&format!("connect-src {}/t0k3n/;", origin()))
    );

    let style = serve(&site, "media/a%20b.css").expect("style");
    assert_eq!(
      (style.body.as_slice(), style.mime),
      (&b"body{}"[..], "text/css")
    );

    let bridge = serve(&site, "__flareite/webview.js").expect("bridge");
    let script = String::from_utf8(bridge.body).expect("utf-8");
    assert!(script.starts_with("const FLAREITE_PAGE = {"), "{script}");
    assert!(script.contains("\"kind\":\"preview\""));
    assert!(serve(&site, "__flareite/file").is_err(), "not a viewer");
  }

  #[test]
  fn nothing_outside_the_extensions_folder_is_served() {
    let root = tempfile::tempdir().expect("temp dir");
    let dir = root.path().join("extension");
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::write(root.path().join("secret.txt"), "secret").expect("write");
    std::fs::write(dir.join("ok.txt"), "ok").expect("write");
    let record = record(None);
    let site = Site {
      dir: &dir,
      record: &record,
      file: None,
      network: false,
      theme: &WebviewTheme::default(),
    };
    assert!(serve(&site, "ok.txt").is_ok());
    for path in [
      "../secret.txt",
      "%2E%2E/secret.txt",
      "./ok.txt",
      "a/../ok.txt",
      "..%5Csecret.txt",
      "/etc/passwd",
      "C:%5Cwindows",
      "%ZZ",
      "missing.txt",
      "__flareite/other",
    ] {
      assert!(serve(&site, path).is_err(), "{path}");
    }
  }

  #[test]
  fn a_viewer_serves_its_file_and_set_html_at_the_root() {
    let dir = tempfile::tempdir().expect("temp dir");
    let viewed = dir.path().join("cat.svg");
    std::fs::write(&viewed, "<svg/>").expect("write");
    let mut record = record(Some("page.html"));
    record.html = Some("<p>set</p>".into());
    assert_eq!(record.info().url, format!("{}/t0k3n/", origin()));
    let site = Site {
      dir: dir.path(),
      record: &record,
      file: Some(&viewed),
      network: false,
      theme: &WebviewTheme::default(),
    };
    let file = serve(&site, "__flareite/file").expect("file");
    assert_eq!(
      (file.body.as_slice(), file.mime),
      (&b"<svg/>"[..], "image/svg+xml")
    );
    let root = String::from_utf8(serve(&site, "").expect("root").body).expect("utf-8");
    assert!(root.starts_with("<style id=\"flareite-theme\">"), "{root}");
    assert!(root.ends_with("webview.js\"></script><p>set</p>"), "{root}");
    let bridge = String::from_utf8(serve(&site, "__flareite/webview.js").expect("bridge").body)
      .expect("utf-8");
    assert!(bridge.contains("\"kind\":\"viewer\""));
    assert!(bridge.contains("\"fileName\":\"cat.svg\""));
  }

  #[test]
  fn pages_get_the_theme_defaults_and_on_request_the_kit() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("media/deep")).expect("mkdir");
    std::fs::write(dir.path().join("media/deep/page.html"), "<head></head>").expect("write");
    let record = record(None);
    let theme = WebviewTheme::new(
      "light",
      BTreeMap::from([("editor-background".to_owned(), "#ffffff".to_owned())]),
    )
    .expect("valid");
    let site = Site {
      dir: dir.path(),
      record: &record,
      file: None,
      network: false,
      theme: &theme,
    };
    let page =
      String::from_utf8(serve(&site, "media/deep/page.html").expect("page").body).expect("utf-8");
    assert!(
      page.starts_with(
        "<head><style id=\"flareite-theme\">:root { --flareite-color-scheme: light; --flareite-editor-background: #ffffff; }</style><style id=\"flareite-defaults\">"
      ),
      "{page}"
    );
    assert!(page.find("flareite-defaults") < page.find("webview.js"));

    for path in ["__flareite/kit.css", "media/deep/__flareite/kit.css"] {
      let kit = serve(&site, path).expect("kit");
      assert_eq!(kit.mime, "text/css");
      let css = String::from_utf8(kit.body).expect("utf-8");
      assert!(
        css.contains(".flareite-icon[data-icon='zoom-in'] {"),
        "{path}"
      );
      assert!(css.contains("url(\"data:image/svg+xml,%3Csvg"));
    }
    assert!(serve(&site, "media/x__flareite/kit.css").is_err());
    let bridge = String::from_utf8(serve(&site, "__flareite/webview.js").expect("bridge").body)
      .expect("utf-8");
    assert!(bridge.contains("\"theme\":\"light\""));
  }

  #[test]
  fn theme_variables_cant_leave_their_rule() {
    let theme = |name: &str, value: &str| {
      WebviewTheme::new(
        "dark",
        BTreeMap::from([(name.to_owned(), value.to_owned())]),
      )
    };
    assert!(theme("font-ui", "system-ui, 'Segoe UI', sans-serif").is_ok());
    for value in ["red; } body { x", "</style><script>", "a\\3c"] {
      assert!(theme("x", value).is_err(), "{value}");
    }
    for name in ["Upper", "a b", "x:y", ""] {
      assert!(theme(name, "1px").is_err(), "{name}");
    }
    assert!(WebviewTheme::new("sepia", BTreeMap::new()).is_err());
  }

  #[test]
  fn the_network_permission_opens_the_policy_to_the_web() {
    let closed = policy("t", false);
    assert!(!closed.contains("https:"), "{closed}");
    let open = policy("t", true);
    for directive in [
      "script-src",
      "style-src",
      "img-src",
      "media-src",
      "font-src",
    ] {
      assert!(
        open.contains(&format!("{directive} {}/t/ https:", origin())),
        "{open}"
      );
    }
    assert!(open.contains(&format!("connect-src {}/t/ https: wss:", origin())));
    assert!(open.contains("form-action 'none'"));
  }

  #[test]
  fn pages_get_their_saved_state() {
    let mut record = record(None);
    record.state = Some("{\"zoom\":2}".into());
    let dir = tempfile::tempdir().expect("temp dir");
    let site = Site {
      dir: dir.path(),
      record: &record,
      file: None,
      network: false,
      theme: &WebviewTheme::default(),
    };
    let bridge = String::from_utf8(serve(&site, "__flareite/webview.js").expect("bridge").body)
      .expect("utf-8");
    assert!(bridge.contains("\"state\":{\"zoom\":2}"), "{bridge}");
  }

  #[test]
  fn the_bridge_goes_into_the_head_not_a_header() {
    let record = record(None);
    let dir = tempfile::tempdir().expect("temp dir");
    let site = Site {
      dir: dir.path(),
      record: &record,
      file: None,
      network: false,
      theme: &WebviewTheme::default(),
    };
    let script = inject("", &site);
    assert_eq!(
      inject("<header>h</header>", &site),
      format!("{script}<header>h</header>")
    );
    assert_eq!(
      inject("<!doctype html>\n<head lang=en><p>", &site),
      format!("<!doctype html>\n<head lang=en>{script}<p>")
    );
  }
}
