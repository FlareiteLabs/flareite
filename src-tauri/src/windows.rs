//! The workbench's windows: the main window, and the windows tabs are
//! dragged out into ([ADR 0046](../../docs/adr/0046-windows.md)).
//!
//! Another window's page is drawn by the main window's workbench: the
//! workbench opens it with `window.open` (`/window.html#<id>`, a blank page
//! of Flareite's own), which this module turns into a native window whose
//! page the main window scripts. So documents, editors, terminals, and
//! extensions stay in one place, the main window's, and a window is only
//! somewhere else to show them.
//!
//! - **Labels.** A window's label is `dock-<id>`, with the workbench's id for
//!   it (`w1`, `w2`, ...).
//! - **Closing.** Closing a window asks the workbench, which closes its tabs
//!   (asking about unsaved changes) and then the window (`window_close`).
//!   Closing the main window closes the others without asking: their
//!   unsaved changes were asked about with the main window's.
//! - **Reloading.** When the main window's page loads, it closes the windows
//!   it left behind (`windows_reset`) and opens the saved ones again.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{
  App, AppHandle, Manager, PhysicalPosition, PhysicalSize, Runtime, Url, WebviewUrl,
  WebviewWindowBuilder, Window, WindowEvent,
};

use crate::error::CommandError;
use crate::events::{self, AppEvent};

/// The main window's label (`tauri.conf.json`).
pub const MAIN: &str = "main";
/// The prefix of the labels of the windows tabs are dragged into.
const PREFIX: &str = "dock-";
/// The page another window shows until the workbench draws in it.
const PAGE: &str = "/window.html";
/// A new window's inner size when the workbench gives none, in logical pixels.
const DEFAULT_SIZE: (f64, f64) = (800.0, 600.0);
const MIN_SIZE: (f64, f64) = (320.0, 200.0);
/// How long a window must stay put before its new bounds are reported.
const BOUNDS_SETTLE: Duration = Duration::from_millis(300);

/// Where a window is, in physical pixels: its outer top left corner and its
/// inner size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
  pub x: i32,
  pub y: i32,
  pub width: u32,
  pub height: u32,
}

/// How the workbench wants a window it's about to open.
#[derive(Debug, Clone, Copy)]
struct Placement {
  bounds: Option<Bounds>,
  maximized: bool,
}

#[derive(Default)]
pub struct WindowsState {
  /// Windows the workbench is about to open, by id.
  pending: Mutex<HashMap<String, Placement>>,
  /// The page zoom, for new windows.
  zoom: Mutex<f64>,
  /// When each window that's moving or resizing last did, by label.
  moves: Mutex<HashMap<String, Instant>>,
}

impl WindowsState {
  fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, Placement>> {
    self.pending.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn zoom(&self) -> f64 {
    let zoom = *self.zoom.lock().unwrap_or_else(PoisonError::into_inner);
    if zoom > 0.0 { zoom } else { 1.0 }
  }
}

/// The workbench's id for the window labelled `label`, unless it's the main
/// window (or not one of the workbench's).
pub fn window_id(label: &str) -> Option<&str> {
  label.strip_prefix(PREFIX)
}

fn label_of(id: &str) -> String {
  format!("{PREFIX}{id}")
}

/// Whether `id` is a window id the workbench hands out.
fn valid_id(id: &str) -> bool {
  id.len() <= 8
    && id
      .strip_prefix('w')
      .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The window id `url` opens, if it's the workbench's window page on the
/// main page's own origin.
fn requested_id(url: &Url, main: &Url) -> Option<String> {
  let same_origin = url.scheme() == main.scheme()
    && url.host_str() == main.host_str()
    && url.port_or_known_default() == main.port_or_known_default();
  let id = url.fragment()?;
  (same_origin && url.path() == PAGE && url.query().is_none() && valid_id(id))
    .then(|| id.to_owned())
}

/// Creates the main window from its configuration.
pub fn create_main(app: &App) -> tauri::Result<()> {
  let config = app
    .config()
    .app
    .windows
    .iter()
    .find(|window| window.label == MAIN)
    .cloned()
    .ok_or(tauri::Error::WindowNotFound)?;
  let handle = app.handle().clone();
  WebviewWindowBuilder::from_config(app, &config)?
    .on_new_window(move |url, features| open_window(&handle, &url, features))
    .build()?;
  Ok(())
}

/// Opens a window the main window's page asked for with `window.open`, if
/// it's one of the workbench's; anything else is refused.
fn open_window(
  app: &AppHandle,
  url: &Url,
  features: NewWindowFeatures,
) -> NewWindowResponse<tauri::Wry> {
  let main = app.get_webview(MAIN).and_then(|webview| webview.url().ok());
  let Some(id) = main.as_ref().and_then(|main| requested_id(url, main)) else {
    tracing::warn!(%url, "refused to open a window");
    return NewWindowResponse::Deny;
  };
  let label = label_of(&id);
  if app.get_window(&label).is_some() {
    tracing::warn!(%label, "a window with this id is already open");
    return NewWindowResponse::Deny;
  }
  let state = app.state::<WindowsState>();
  let placement = state.pending().remove(&id).unwrap_or(Placement {
    bounds: None,
    maximized: false,
  });
  let background = app
    .config()
    .app
    .windows
    .iter()
    .find(|window| window.label == MAIN)
    .and_then(|window| window.background_color);
  let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url.clone()))
    .window_features(features)
    .title("Flareite")
    .inner_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1)
    .min_inner_size(MIN_SIZE.0, MIN_SIZE.1)
    .visible(false)
    // The workbench titles the page after the tab it shows.
    .on_document_title_changed(|window, title| {
      let _ = window.set_title(&title);
    });
  if let Some(color) = background {
    builder = builder.background_color(color);
  }
  let window = match builder.build() {
    Ok(window) => window,
    Err(error) => {
      tracing::error!(%error, "can't open a window");
      return NewWindowResponse::Deny;
    }
  };
  if let Some(bounds) = placement.bounds.and_then(|bounds| on_screen(app, bounds)) {
    // The outer position, then the inner size, as they were saved.
    let _ = window.set_position(PhysicalPosition::new(bounds.x, bounds.y));
    let _ = window.set_size(PhysicalSize::new(bounds.width, bounds.height));
  }
  let zoom = state.zoom();
  if (zoom - 1.0).abs() > f64::EPSILON
    && let Err(error) = window.set_zoom(zoom)
  {
    tracing::warn!(%error, "can't zoom a new window");
  }
  if placement.maximized {
    let _ = window.maximize();
  }
  let _ = window.show();
  let _ = window.set_focus();
  // Where the system put it is kept too, to open it there next time.
  report_bounds_later(&window.as_ref().window());
  tracing::info!(%label, "opened a window");
  NewWindowResponse::Create { window }
}

/// `bounds`, if at least part of the window's top would be on a monitor
/// (one may have been unplugged since they were saved).
fn on_screen(app: &AppHandle, bounds: Bounds) -> Option<Bounds> {
  let monitors = app.available_monitors().ok()?;
  let (x, y) = (bounds.x + 48, bounds.y + 8);
  monitors
    .iter()
    .any(|monitor| {
      let position = monitor.position();
      let size = monitor.size();
      x >= position.x
        && y >= position.y
        && i64::from(x) < i64::from(position.x) + i64::from(size.width)
        && i64::from(y) < i64::from(position.y) + i64::from(size.height)
    })
    .then_some(bounds)
}

/// Handles events of every window (`lib.rs` routes them here first).
/// Returns whether the event was one of another window's, handled.
pub fn on_window_event(window: &Window, event: &WindowEvent) -> bool {
  let label = window.label();
  if label == MAIN {
    if let WindowEvent::Destroyed = event {
      close_all(window.app_handle());
    }
    return false;
  }
  let Some(id) = window_id(label) else {
    return false;
  };
  let app = window.app_handle();
  match event {
    // The workbench closes its tabs first, which may ask about them, then
    // the window (`window_close`, which isn't asked: `destroy` skips this).
    WindowEvent::CloseRequested { api, .. } => {
      api.prevent_close();
      events::emit(
        app,
        AppEvent::WindowCloseRequested {
          window: id.to_owned(),
        },
      );
      true
    }
    WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
      report_bounds_later(window);
      true
    }
    _ => false,
  }
}

/// Reports a window's bounds once it has stayed put for a moment: one
/// thread per window waits while it keeps moving.
fn report_bounds_later(window: &Window) {
  let state = window.state::<WindowsState>();
  let label = window.label().to_owned();
  let waiting = state
    .moves
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .insert(label.clone(), Instant::now())
    .is_some();
  if waiting {
    return;
  }
  let window = window.clone();
  std::thread::spawn(move || {
    loop {
      let state = window.state::<WindowsState>();
      let last = state
        .moves
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&label)
        .copied();
      let Some(last) = last else { return };
      let settled = last + BOUNDS_SETTLE;
      let now = Instant::now();
      if now < settled {
        std::thread::sleep(settled - now);
        continue;
      }
      let mut moves = state.moves.lock().unwrap_or_else(PoisonError::into_inner);
      if moves.get(&label) != Some(&last) {
        continue;
      }
      moves.remove(&label);
      break;
    }
    report_bounds(&window);
  });
}

fn report_bounds(window: &Window) {
  let Some(id) = window_id(window.label()) else {
    return;
  };
  if window.is_minimized().unwrap_or(false) {
    return;
  }
  let maximized = window.is_maximized().unwrap_or(false);
  let bounds = if maximized { None } else { bounds_of(window) };
  events::emit(
    window.app_handle(),
    AppEvent::WindowBoundsChanged {
      window: id.to_owned(),
      bounds,
      maximized,
    },
  );
}

fn bounds_of<R: Runtime>(window: &Window<R>) -> Option<Bounds> {
  let position = window.outer_position().ok()?;
  let size = window.inner_size().ok()?;
  Some(Bounds {
    x: position.x,
    y: position.y,
    width: size.width,
    height: size.height,
  })
}

/// The workbench's windows, other than the main one.
fn dock_windows(app: &AppHandle) -> Vec<Window> {
  app
    .windows()
    .into_iter()
    .filter(|(label, _)| window_id(label).is_some())
    .map(|(_, window)| window)
    .collect()
}

/// Closes every other window without asking (the main window closed, or
/// its page loaded again).
fn close_all(app: &AppHandle) {
  for window in dock_windows(app) {
    crate::web_pages::rescue(app, &window);
    if let Err(error) = window.destroy() {
      tracing::warn!(%error, "can't close a window");
    }
  }
}

/// The window the user is in, for dialogs to belong to: the focused one of
/// Flareite's windows, else `fallback`.
pub fn dialog_parent(fallback: &Window) -> Window {
  fallback
    .app_handle()
    .windows()
    .into_values()
    .find(|window| window.is_focused().unwrap_or(false))
    .unwrap_or_else(|| fallback.clone())
}

/// The native window of the workbench's window `id` (`None`: the main
/// window).
pub fn native(app: &AppHandle, id: Option<&str>) -> Result<Window, CommandError> {
  let label = id.map_or_else(|| MAIN.to_owned(), label_of);
  app
    .get_window(&label)
    .ok_or_else(|| CommandError::new("unknownWindow", "That window isn't open."))
}

/// Where the workbench is about to open window `id` (`window.open` follows).
#[tauri::command]
pub fn window_prepare(
  app: AppHandle,
  id: String,
  bounds: Option<Bounds>,
  maximized: bool,
) -> Result<(), CommandError> {
  if !valid_id(&id) {
    return Err(CommandError::new("invalidArgument", "Not a window id."));
  }
  let state = app.state::<WindowsState>();
  let mut pending = state.pending();
  // Requests that never became windows don't pile up.
  if pending.len() > 32 {
    pending.clear();
  }
  pending.insert(id, Placement { bounds, maximized });
  Ok(())
}

/// Closes window `id` without asking (its tabs are gone).
#[tauri::command]
pub fn window_close(app: AppHandle, id: String) -> Result<(), CommandError> {
  let Ok(window) = native(&app, Some(&id)) else {
    return Ok(());
  };
  crate::web_pages::rescue(&app, &window);
  window.destroy().map_err(CommandError::internal)
}

/// Closes the windows the main window's page opened before it loaded again.
#[tauri::command]
pub fn windows_reset(app: AppHandle) {
  close_all(&app);
}

/// Brings window `id` (`None`: the main window) to the front and focuses it.
#[tauri::command]
pub fn window_focus(app: AppHandle, id: Option<String>) -> Result<(), CommandError> {
  let window = native(&app, id.as_deref())?;
  if window.is_minimized().unwrap_or(false) {
    let _ = window.unminimize();
  }
  window.set_focus().map_err(CommandError::internal)
}

/// Moves window `id`'s outer top left corner to `x`, `y` (physical pixels).
#[tauri::command]
pub fn window_move(app: AppHandle, id: String, x: i32, y: i32) -> Result<(), CommandError> {
  let window = native(&app, Some(&id))?;
  if window.is_maximized().unwrap_or(false) {
    let _ = window.unmaximize();
  }
  window
    .set_position(PhysicalPosition::new(x, y))
    .map_err(CommandError::internal)
}

/// Zooms every other window's page as the main window's is.
#[tauri::command]
pub fn windows_set_zoom(app: AppHandle, zoom: f64) -> Result<(), CommandError> {
  if !(0.25..=5.0).contains(&zoom) {
    return Err(CommandError::new("invalidArgument", "Zoom out of range."));
  }
  *app
    .state::<WindowsState>()
    .zoom
    .lock()
    .unwrap_or_else(PoisonError::into_inner) = zoom;
  for window in dock_windows(&app) {
    for webview in window.webviews() {
      // Web page panes keep their own zoom.
      if webview.label() == window.label()
        && let Err(error) = webview.set_zoom(zoom)
      {
        tracing::warn!(%error, "can't zoom a window");
      }
    }
  }
  Ok(())
}

/// Where a window's page is on screen, in physical pixels.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Geometry {
  /// The workbench's id for it; `None` for the main window.
  window: Option<String>,
  x: i32,
  y: i32,
  width: u32,
  height: u32,
  /// Its outer top left corner.
  outer_x: i32,
  outer_y: i32,
}

/// Where each window's page is on screen, leaving out minimized windows,
/// for following a drag from one window into another.
#[tauri::command]
pub fn windows_geometry(app: AppHandle) -> Vec<Geometry> {
  app
    .windows()
    .into_values()
    .filter(|window| window.label() == MAIN || window_id(window.label()).is_some())
    .filter(|window| {
      !window.is_minimized().unwrap_or(false) && window.is_visible().unwrap_or(false)
    })
    .filter_map(|window| {
      let inner = window.inner_position().ok()?;
      let outer = window.outer_position().ok()?;
      let size = window.inner_size().ok()?;
      Some(Geometry {
        window: window_id(window.label()).map(str::to_owned),
        x: inner.x,
        y: inner.y,
        width: size.width,
        height: size.height,
        outer_x: outer.x,
        outer_y: outer.y,
      })
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn opens_only_the_workbench_window_page_on_its_own_origin() {
    let main = Url::parse("http://localhost:1420/").expect("url");
    let id = |url: &str| requested_id(&Url::parse(url).expect("url"), &main);
    assert_eq!(
      id("http://localhost:1420/window.html#w3"),
      Some("w3".into())
    );
    assert_eq!(id("http://localhost:1420/window.html"), None);
    assert_eq!(id("http://localhost:1420/window.html#x3"), None);
    assert_eq!(id("http://localhost:1420/window.html#w"), None);
    assert_eq!(id("http://localhost:1420/window.html#w1234567890"), None);
    assert_eq!(id("http://localhost:1420/index.html#w3"), None);
    assert_eq!(id("http://localhost:1420/window.html?a=1#w3"), None);
    assert_eq!(id("http://localhost:1421/window.html#w3"), None);
    assert_eq!(id("https://example.com/window.html#w3"), None);
    assert_eq!(id("about:blank#w3"), None);

    let tauri = Url::parse("http://tauri.localhost/").expect("url");
    assert_eq!(
      requested_id(
        &Url::parse("http://tauri.localhost/window.html#w12").expect("url"),
        &tauri
      ),
      Some("w12".into())
    );
  }

  #[test]
  fn labels_name_their_window() {
    assert_eq!(window_id(&label_of("w4")), Some("w4"));
    assert_eq!(window_id(MAIN), None);
    assert_eq!(window_id("web-page-1"), None);
  }
}
