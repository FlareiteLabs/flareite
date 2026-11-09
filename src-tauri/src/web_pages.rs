//! Web page panes (API 1.15): pages from the web shown in layout tabs, each
//! in a browser view of its own (a child webview of the main window) that
//! the workbench lays over its pane.
//!
//! - **Isolation.** A view loads only `https:` and `http:` addresses: it
//!   can't navigate to Flareite's own pages or files. Flareite's commands
//!   are granted only to its own (local) pages (`capabilities/`), so a
//!   remote page can't call them. Windows a page opens show in the same
//!   view.
//! - **Placement.** The workbench measures the pane and sends its bounds
//!   (in CSS pixels, at its zoom); views are native, so they sit above the
//!   workbench's page, and the workbench hides them while a menu or dialog
//!   is open, and when their tab is hidden.
//! - **Navigation** is reported to the extension (`layouts::navigated`), and
//!   the extension moves views with `navigate-pane`.

use std::collections::HashMap;
use std::sync::Mutex;

use flareite_extension_host::layouts::{WebAction, is_web_address};
use tauri::webview::{NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Rect, Url, Webview, WebviewUrl};

use crate::error::CommandError;
use crate::extensions::ExtensionsState;

/// An open view, by the pane's view id.
struct View {
  webview: Webview,
  /// What its extension was last told.
  url: String,
  title: Option<String>,
}

#[derive(Default)]
pub struct WebPagesState {
  views: Mutex<HashMap<String, View>>,
  next_label: Mutex<u64>,
}

impl WebPagesState {
  fn views(&self) -> std::sync::MutexGuard<'_, HashMap<String, View>> {
    self
      .views
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

/// Where a view goes in the window, in logical pixels.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct Bounds {
  x: f64,
  y: f64,
  width: f64,
  height: f64,
}

impl Bounds {
  fn rect(self) -> Rect {
    Rect {
      position: LogicalPosition::new(self.x, self.y).into(),
      size: LogicalSize::new(self.width.max(1.0), self.height.max(1.0)).into(),
    }
  }
}

fn web_url(url: &str) -> Result<Url, CommandError> {
  if !is_web_address(url) {
    return Err(CommandError::new(
      "invalidArgument",
      "Web page panes show only https: and http: addresses.",
    ));
  }
  Url::parse(url).map_err(|error| CommandError::new("invalidArgument", error.to_string()))
}

/// Tells the view's extension where it is now.
fn report(app: &AppHandle, id: &str, url: Option<String>, title: Option<String>, loading: bool) {
  let state = app.state::<WebPagesState>();
  let (url, title) = {
    let mut views = state.views();
    let Some(view) = views.get_mut(id) else {
      return;
    };
    if let Some(url) = url {
      view.url = url;
    }
    if let Some(title) = title {
      view.title = Some(title);
    }
    (view.url.clone(), view.title.clone())
  };
  if let Some(host) = app.state::<ExtensionsState>().host()
    && let Err(error) = host.web_page_navigated(id, url, title, loading)
  {
    tracing::warn!(%error, "can't tell an extension where its web page went");
  }
}

/// Shows a pane's view at `bounds`, creating it on first use with `url`.
#[tauri::command]
pub async fn web_page_show(
  app: AppHandle,
  window: tauri::Window,
  id: String,
  url: String,
  bounds: Bounds,
) -> Result<(), CommandError> {
  let state = app.state::<WebPagesState>();
  if let Some(view) = state.views().get(&id) {
    view
      .webview
      .set_bounds(bounds.rect())
      .map_err(CommandError::internal)?;
    view.webview.show().map_err(CommandError::internal)?;
    return Ok(());
  }
  let start = web_url(&url)?;
  let label = {
    let mut next = state
      .next_label
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    *next += 1;
    format!("web-page-{next}")
  };
  let (navigation_app, navigation_id) = (app.clone(), id.clone());
  let (load_app, load_id) = (app.clone(), id.clone());
  let (title_app, title_id) = (app.clone(), id.clone());
  let (window_app, window_label) = (app.clone(), label.clone());
  let builder = WebviewBuilder::new(&label, WebviewUrl::External(start))
    .on_navigation(move |url| {
      let allowed = matches!(url.scheme(), "https" | "http");
      if allowed {
        report(
          &navigation_app,
          &navigation_id,
          Some(url.to_string()),
          None,
          true,
        );
      }
      allowed
    })
    .on_page_load(move |_, payload| {
      report(
        &load_app,
        &load_id,
        Some(payload.url().to_string()),
        None,
        payload.event() == PageLoadEvent::Started,
      );
    })
    .on_document_title_changed(move |_, title| {
      report(&title_app, &title_id, None, Some(title), false);
    })
    // A page that opens a window (a link with target=_blank) goes there in
    // the same view.
    .on_new_window(move |url, _| {
      if matches!(url.scheme(), "https" | "http")
        && let Some(webview) = window_app.get_webview(&window_label)
      {
        let _ = webview.navigate(url);
      }
      NewWindowResponse::Deny
    });
  let rect = bounds.rect();
  let webview = window
    .add_child(builder, rect.position, rect.size)
    .map_err(CommandError::internal)?;
  state.views().insert(
    id,
    View {
      webview,
      url,
      title: None,
    },
  );
  Ok(())
}

/// Moves a pane's view (it moved or was resized).
#[tauri::command]
pub fn web_page_bounds(
  state: tauri::State<'_, WebPagesState>,
  id: String,
  bounds: Bounds,
) -> Result<(), CommandError> {
  if let Some(view) = state.views().get(&id) {
    view
      .webview
      .set_bounds(bounds.rect())
      .map_err(CommandError::internal)?;
  }
  Ok(())
}

/// Hides a pane's view (its tab is hidden, or a menu is open over it).
#[tauri::command]
pub fn web_page_hide(
  state: tauri::State<'_, WebPagesState>,
  id: String,
) -> Result<(), CommandError> {
  if let Some(view) = state.views().get(&id) {
    view.webview.hide().map_err(CommandError::internal)?;
  }
  Ok(())
}

/// Closes a pane's view (its tab closed, or the pane went).
#[tauri::command]
pub fn web_page_close(
  state: tauri::State<'_, WebPagesState>,
  id: String,
) -> Result<(), CommandError> {
  let view = state.views().remove(&id);
  if let Some(view) = view {
    view.webview.close().map_err(CommandError::internal)?;
  }
  Ok(())
}

/// Moves a pane's view, as its extension asked.
pub fn act(app: &AppHandle, id: &str, action: WebAction) {
  let state = app.state::<WebPagesState>();
  let views = state.views();
  let Some(view) = views.get(id) else {
    return;
  };
  let outcome = match action {
    WebAction::Go(url) => match Url::parse(&url) {
      Ok(url) => view.webview.navigate(url),
      Err(error) => {
        tracing::warn!(%error, "an extension's address isn't valid");
        return;
      }
    },
    WebAction::Back => view.webview.eval("history.back()"),
    WebAction::Forward => view.webview.eval("history.forward()"),
    WebAction::Reload => view.webview.reload(),
    WebAction::Stop => view.webview.eval("window.stop()"),
  };
  if let Err(error) = outcome {
    tracing::warn!(%error, "can't move a web page");
  }
}
