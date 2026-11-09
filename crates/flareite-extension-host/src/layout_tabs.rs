//! The host's side of layout tabs (API 1.15; see [`crate::layouts`]): what
//! each open tab shows, as its extension set it, its documents once the
//! workbench has opened them, and the pages of its webview panes, which
//! are kept with the previews' so they're served, messaged, and given
//! state as other webviews are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flareite_core::DocumentId;
use serde::Serialize;

use super::{CommandTicket, ExtensionHost, Outbox, PreviewRecord, Run, Shared, State};
use crate::Notification;
use crate::api::{self, to_position};
use crate::bindings::flareite::extension::types::Range as WitRange;
use crate::layouts::{
  Fill, LayoutCall, LayoutSpec, PaneContent, PaneDecorations, Point, TabDocument, TabId,
};
use crate::preview::PreviewInfo;
use crate::webview::{WebviewInfo, WebviewRecord};
use crate::worker::{Event, Payload};

/// An open layout tab.
pub(super) struct LayoutRecord {
  /// The extension's id.
  pub extension: String,
  /// What it shows; `None` until the extension sets it.
  pub spec: Option<Arc<LayoutSpec>>,
  /// What its editor panes show besides their text, by pane.
  pub decorations: BTreeMap<String, Arc<PaneDecorations>>,
  /// Its documents, once the workbench has opened them.
  pub documents: Vec<TabDocument>,
  /// The documents it had when the extension was last told they're ready.
  pub ready: Option<BTreeSet<String>>,
  /// Its webview and file page panes' webview ids, by pane.
  pub webviews: BTreeMap<String, String>,
  /// Its extension stopped since setting its layout: it's filled in again
  /// when the extension starts.
  pub stale: bool,
  pub width: u32,
}

impl LayoutRecord {
  fn new(extension: &str) -> Self {
    Self {
      extension: extension.to_owned(),
      spec: None,
      decorations: BTreeMap::new(),
      documents: Vec::new(),
      ready: None,
      webviews: BTreeMap::new(),
      stale: false,
      width: 0,
    }
  }
}

/// What the workbench draws for a tab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutView {
  pub spec: Option<Arc<LayoutSpec>>,
  /// By pane.
  pub decorations: BTreeMap<String, Arc<PaneDecorations>>,
}

/// The editor pane in use: its tab, pane, and document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActivePane {
  pub tab: TabId,
  pub pane: String,
}

impl Shared {
  /// The extension declaring a layout, by its full id.
  fn layout_extension(state: &State, layout: &str) -> Option<String> {
    state.extensions.iter().find_map(|entry| {
      entry
        .manifest
        .contributions
        .layouts
        .iter()
        .any(|declared| declared.full_id == layout)
        .then(|| entry.manifest.id.clone())
    })
  }

  /// Opens a tab (`layouts::open`), or shows it if it's open.
  pub(crate) fn open_layout_tab(&self, extension: &str, tab: TabId, beside: bool) {
    self
      .lock()
      .layouts
      .entry(tab.clone())
      .or_insert_with(|| LayoutRecord::new(extension));
    self
      .embedder
      .notify(Notification::OpenLayout { tab, beside });
  }

  /// The webview ids of an open tab's panes, by pane; `None` if it isn't
  /// open.
  pub(crate) fn layout_webviews(&self, tab: &TabId) -> Option<BTreeMap<String, String>> {
    Some(self.lock().layouts.get(tab)?.webviews.clone())
  }

  /// A new id for a pane's webview: `<extension id>.pane/<n>`, so the
  /// workbench knows its extension (for stored values that change), as it
  /// knows a preview's.
  pub(crate) fn new_pane_webview_id(&self, extension: &str) -> String {
    let mut state = self.lock();
    state.next_pane_webview += 1;
    format!("{extension}.pane/{}", state.next_pane_webview)
  }

  /// Sets what an open tab shows; false if it isn't open.
  pub(crate) fn set_layout(&self, tab: &TabId, spec: LayoutSpec) -> bool {
    let mut outbox = Outbox::default();
    {
      let mut guard = self.lock();
      let state = &mut *guard;
      let Some(record) = state.layouts.get_mut(tab) else {
        return false;
      };
      let extension = record.extension.clone();
      let name = state
        .extensions
        .iter()
        .find(|entry| entry.manifest.id == extension)
        .map(|entry| entry.manifest.name.clone())
        .unwrap_or_default();
      // The panes' webviews: kept while their pane shows the same page.
      let mut webviews = BTreeMap::new();
      for (pane, content) in spec.panes() {
        let (id, page, file) = match &content.content {
          PaneContent::Webview { page, id } => (id, page.clone(), None),
          PaneContent::FilePage { path, id } => (id, None, Some(PathBuf::from(path))),
          PaneContent::Editor { .. } => continue,
        };
        webviews.insert(pane.to_owned(), id.clone());
        match state.previews.get_mut(id) {
          Some(preview) => {
            if let Some(webview) = preview.webview.as_mut()
              && (webview.page != page || webview.file_page != file)
            {
              webview.page = page;
              webview.file_page = file;
              webview.html = None;
              webview.version += 1;
              outbox.push(Notification::WebviewChanged(id.clone()));
            }
          }
          None => {
            let mut webview = WebviewRecord::new(page);
            webview.file_page = file;
            state.previews.insert(
              id.clone(),
              PreviewRecord {
                extension: extension.clone(),
                info: PreviewInfo {
                  id: id.clone(),
                  extension: name.clone(),
                  title: spec.title.clone(),
                  icon: "book".into(),
                  document: None,
                  webview: true,
                },
                content: Arc::default(),
                images: BTreeSet::new(),
                file: None,
                webview: Some(webview),
                pane: Some((tab.clone(), pane.to_owned())),
              },
            );
          }
        }
      }
      let record = state.layouts.get_mut(tab).expect("checked above");
      for (pane, id) in &record.webviews {
        if webviews.get(pane) != Some(id) {
          state.previews.remove(id);
        }
      }
      // Panes that are gone, or aren't editors now, lose their decorations.
      record.decorations.retain(|pane, _| {
        matches!(
          spec.pane(pane).map(|pane| &pane.content),
          Some(PaneContent::Editor { .. })
        )
      });
      record.webviews = webviews;
      record.spec = Some(Arc::new(spec));
      record.stale = false;
      outbox.push(Notification::LayoutChanged(tab.clone()));
    }
    outbox.send(&*self.embedder);
    true
  }

  /// Asks the workbench to close a tab (`layouts::close`).
  pub(crate) fn close_layout_tab(&self, tab: &TabId) {
    if self.lock().layouts.contains_key(tab) {
      self.embedder.notify(Notification::CloseLayout(tab.clone()));
    }
  }

  pub(crate) fn layout_tabs_of(&self, extension: &str) -> Vec<TabId> {
    self
      .lock()
      .layouts
      .iter()
      .filter(|(_, record)| record.extension == extension)
      .map(|(tab, _)| tab.clone())
      .collect()
  }

  /// An open tab's layout and documents; `None` if it isn't open.
  pub(crate) fn layout_documents(
    &self,
    tab: &TabId,
  ) -> Option<(Option<Arc<LayoutSpec>>, Vec<TabDocument>)> {
    let state = self.lock();
    let record = state.layouts.get(tab)?;
    Some((record.spec.clone(), record.documents.clone()))
  }

  /// Replaces what an editor pane shows besides its text; false if the tab
  /// isn't open.
  pub(crate) fn set_pane_decorations(
    &self,
    tab: &TabId,
    pane: &str,
    decorations: PaneDecorations,
  ) -> bool {
    {
      let mut state = self.lock();
      let Some(record) = state.layouts.get_mut(tab) else {
        return false;
      };
      if record
        .decorations
        .get(pane)
        .is_some_and(|current| **current == decorations)
      {
        return true;
      }
      record
        .decorations
        .insert(pane.to_owned(), Arc::new(decorations));
    }
    self.embedder.notify(Notification::PaneDecorationsChanged {
      tab: tab.clone(),
      pane: pane.to_owned(),
    });
    true
  }

  pub(crate) fn reveal_in_pane(&self, tab: &TabId, pane: &str, start: Point, end: Point) -> bool {
    let open = self
      .lock()
      .layouts
      .get(tab)
      .and_then(|record| record.spec.as_ref())
      .is_some_and(|spec| spec.pane(pane).is_some());
    if open {
      self.embedder.notify(Notification::RevealInPane {
        tab: tab.clone(),
        pane: pane.to_owned(),
        start,
        end,
      });
    }
    open
  }

  /// The editor pane in use, its document, and the document's selections.
  pub(crate) fn active_pane(&self) -> Option<(TabId, String, u64, Vec<WitRange>)> {
    let (active, document) = {
      let state = self.lock();
      let active = state.active_pane.clone()?;
      let record = state.layouts.get(&active.tab)?;
      let spec = record.spec.as_ref()?;
      let PaneContent::Editor { uri, .. } = &spec.pane(&active.pane)?.content else {
        return None;
      };
      let document = record
        .documents
        .iter()
        .find(|document| document.uri == *uri)?
        .document;
      (active, document)
    };
    let selections = api::with_editor(self, |editor| {
      let document = editor.document(DocumentId::from_raw(document)).ok()?;
      let buffer = document.buffer();
      let selections = document.selections();
      let primary = selections.primary();
      Some(
        std::iter::once(primary)
          .chain(selections.iter().copied().filter(|s| *s != primary))
          .map(|selection| {
            let range = selection.range();
            WitRange {
              start: to_position(buffer, range.start),
              end: to_position(buffer, range.end),
            }
          })
          .collect::<Vec<_>>(),
      )
    })
    .flatten()?;
    Some((active.tab, active.pane, document, selections))
  }

  /// The webview id of one of an open tab's panes.
  pub(crate) fn pane_webview(&self, tab: &TabId, pane: &str) -> Option<String> {
    self.lock().layouts.get(tab)?.webviews.get(pane).cloned()
  }

  /// A stopped extension's tabs keep their layouts (they're the
  /// workbench's) but lose its decorations, and are filled in again when
  /// it starts.
  pub(super) fn forget_layouts(state: &mut State, extension: &str, outbox: &mut Outbox) {
    for (tab, record) in &mut state.layouts {
      if record.extension != extension {
        continue;
      }
      record.stale = record.spec.is_some();
      record.ready = None;
      for pane in std::mem::take(&mut record.decorations).into_keys() {
        outbox.push(Notification::PaneDecorationsChanged {
          tab: tab.clone(),
          pane,
        });
      }
    }
    if state.active_pane.as_ref().is_some_and(|active| {
      state
        .layouts
        .get(&active.tab)
        .is_some_and(|record| record.extension == extension)
    }) {
      // Kept: the user is still in the pane.
    }
  }

  /// Fills in the tabs a starting extension showed before it stopped.
  pub(super) fn refill_layouts(state: &mut State, index: usize) {
    let entry = &state.extensions[index];
    let Run::Running { worker, .. } = &entry.run else {
      return;
    };
    for (tab, record) in &mut state.layouts {
      if record.extension != entry.manifest.id || !record.stale {
        continue;
      }
      record.stale = false;
      let Some(declared) = entry
        .manifest
        .contributions
        .layouts
        .iter()
        .find(|declared| declared.full_id == tab.layout)
      else {
        continue;
      };
      let _ = worker.queue.push_command(Event::background(
        declared.command.clone(),
        None,
        Payload::Layout(LayoutCall::Fill {
          tab: tab.clone(),
          reason: Fill::Restored,
          width: record.width,
        }),
      ));
    }
  }

  /// The file page a webview shows, if it's one: the folder it's served
  /// from and the file's path in it, or why it can't be shown.
  pub(super) fn file_page_site(&self, file: &Path) -> Result<(PathBuf, String), String> {
    let folder = self
      .embedder
      .open_folder()
      .ok_or("Open the folder the file is in to preview it.")?;
    if !folder.trusted {
      return Err("Pages of files are shown only in a trusted folder.".into());
    }
    let relative = file
      .strip_prefix(&folder.root)
      .map_err(|_| "Only files in the open folder can be shown as pages.".to_owned())?;
    let entry = relative
      .components()
      .map(|component| component.as_os_str().to_string_lossy())
      .collect::<Vec<_>>()
      .join("/");
    Ok((folder.root, entry))
  }

  /// The text of an open document of `file`, as it is in the editor.
  pub(super) fn live_text(&self, file: &Path) -> Option<String> {
    api::with_editor(self, |editor| {
      editor.documents().find_map(|document| {
        let path = document.path()?;
        let same = path == file || path.canonicalize().is_ok_and(|canonical| canonical == file);
        same.then(|| document.buffer().contents())
      })
    })
    .flatten()
  }
}

impl ExtensionHost {
  /// What an open tab shows; `None` if no tab is open with that id.
  pub fn layout(&self, tab: &TabId) -> Option<LayoutView> {
    let state = self.shared.lock();
    let record = state.layouts.get(tab)?;
    Some(LayoutView {
      spec: record.spec.clone(),
      decorations: record.decorations.clone(),
    })
  }

  /// What an editor pane shows besides its text.
  pub fn pane_decorations(&self, tab: &TabId, pane: &str) -> Option<Arc<PaneDecorations>> {
    self
      .shared
      .lock()
      .layouts
      .get(tab)?
      .decorations
      .get(pane)
      .cloned()
  }

  /// Runs a tab's layout command to fill it in (or tell it the tab
  /// changed), opening the tab if it isn't open. A tab closing is
  /// [`Self::layout_closed`].
  pub fn fill_layout(
    &self,
    tab: &TabId,
    reason: Fill,
    width: u32,
  ) -> Result<CommandTicket, String> {
    let command = {
      let mut state = self.shared.lock();
      let extension = Shared::layout_extension(&state, &tab.layout)
        .ok_or_else(|| format!("No extension provides the layout {}.", tab.layout))?;
      let command = state
        .extensions
        .iter()
        .find(|entry| entry.manifest.id == extension)
        .and_then(|entry| {
          entry
            .manifest
            .contributions
            .layouts
            .iter()
            .find(|declared| declared.full_id == tab.layout)
        })
        .map(|declared| declared.command.clone())
        .ok_or_else(|| format!("No extension provides the layout {}.", tab.layout))?;
      let record = state
        .layouts
        .entry(tab.clone())
        .or_insert_with(|| LayoutRecord::new(&extension));
      record.width = width;
      if reason == Fill::Opened || reason == Fill::Restored {
        record.stale = false;
      }
      command
    };
    self.run(
      &command,
      None,
      None,
      Payload::Layout(LayoutCall::Fill {
        tab: tab.clone(),
        reason,
        width,
      }),
    )
  }

  /// The workbench opened a tab's documents. Runs the layout's command
  /// (`ready`) when they're not the ones it was last told about.
  pub fn layout_ready(
    &self,
    tab: &TabId,
    documents: Vec<TabDocument>,
  ) -> Result<Option<CommandTicket>, String> {
    let (command, width) = {
      let mut state = self.shared.lock();
      let Some(record) = state.layouts.get_mut(tab) else {
        return Err("the tab isn't open".into());
      };
      let uris: BTreeSet<String> = documents
        .iter()
        .map(|document| document.uri.clone())
        .collect();
      record.documents = documents;
      if record.ready.as_ref() == Some(&uris) {
        return Ok(None);
      }
      record.ready = Some(uris);
      let width = record.width;
      let command = Shared::layout_extension(&state, &tab.layout).and_then(|extension| {
        state
          .extensions
          .iter()
          .find(|entry| entry.manifest.id == extension)?
          .manifest
          .contributions
          .layouts
          .iter()
          .find(|declared| declared.full_id == tab.layout)
          .map(|declared| declared.command.clone())
      });
      (command, width)
    };
    let Some(command) = command else {
      return Ok(None);
    };
    self
      .run(
        &command,
        None,
        None,
        Payload::Layout(LayoutCall::Fill {
          tab: tab.clone(),
          reason: Fill::Ready,
          width,
        }),
      )
      .map(Some)
  }

  /// A tab closed: forgets it, and tells its extension if it's running.
  pub fn layout_closed(&self, tab: &TabId) {
    let mut state = self.shared.lock();
    let Some(record) = state.layouts.remove(tab) else {
      return;
    };
    for id in record.webviews.values() {
      state.previews.remove(id);
    }
    if state
      .active_pane
      .as_ref()
      .is_some_and(|active| active.tab == *tab)
    {
      state.active_pane = None;
    }
    let Some(entry) = state
      .extensions
      .iter()
      .find(|entry| entry.manifest.id == record.extension)
    else {
      return;
    };
    let (Run::Running { worker, .. }, Some(declared)) = (
      &entry.run,
      entry
        .manifest
        .contributions
        .layouts
        .iter()
        .find(|declared| declared.full_id == tab.layout),
    ) else {
      return;
    };
    let _ = worker.queue.push_command(Event::background(
      declared.command.clone(),
      None,
      Payload::Layout(LayoutCall::Fill {
        tab: tab.clone(),
        reason: Fill::Closed,
        width: record.width,
      }),
    ));
  }

  /// The editor pane in use, if it's a layout's (`layouts::active-pane`).
  pub fn set_active_pane(&self, active: Option<(TabId, String)>) {
    self.shared.lock().active_pane = active.map(|(tab, pane)| ActivePane { tab, pane });
  }

  /// Runs a command from a tab's button, header, or zone: `pane` is `None`
  /// for the tab's own buttons.
  pub fn run_layout_command(
    &self,
    tab: &TabId,
    pane: Option<String>,
    command: &str,
    document: Option<DocumentId>,
    argument: Option<String>,
  ) -> Result<CommandTicket, String> {
    let declares = {
      let state = self.shared.lock();
      Shared::layout_extension(&state, &tab.layout).is_some_and(|extension| {
        state
          .extensions
          .iter()
          .any(|entry| entry.manifest.id == extension && entry.manifest.declares_command(command))
      })
    };
    if !declares {
      return Err(format!(
        "{command} isn't a command of the layout's extension."
      ));
    }
    self.run(
      command,
      document,
      argument,
      Payload::Layout(LayoutCall::Invoked {
        tab: tab.clone(),
        pane,
      }),
    )
  }

  /// The workbench's theme, as pages are served it.
  fn webview_theme(&self) -> crate::webview::WebviewTheme {
    self
      .shared
      .webview_theme
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  /// Answers a file page's request for `path` (percent-encoded): files of
  /// the open folder, as they are in the editor. Only in a trusted folder.
  pub(super) fn file_page_resource(
    &self,
    record: &WebviewRecord,
    file: &Path,
    extension: &str,
    path: &str,
  ) -> Result<crate::webview::Resource, String> {
    let (root, _) = self.shared.file_page_site(file)?;
    let network = {
      let state = self.shared.lock();
      state
        .extensions
        .iter()
        .find(|entry| entry.manifest.id == extension)
        .is_some_and(|entry| {
          state
            .decisions
            .get(extension)
            .effective(&entry.manifest.permissions)
            .contains(&crate::Permission::Network)
        })
    };
    let live = |file: &Path| self.shared.live_text(file);
    let theme = self.webview_theme();
    crate::webview::serve_file_page(
      &crate::webview::FilePageSite {
        root: &root,
        token: &record.token,
        network,
        live: &live,
        theme: &theme,
      },
      path,
    )
  }

  /// A webview pane's page sent a message (JSON text): runs the layout's
  /// `on_message` command, if it has one.
  pub(super) fn pane_message(&self, tab: TabId, pane: String, data: String) -> Result<(), String> {
    let command = {
      let state = self.shared.lock();
      state.extensions.iter().find_map(|entry| {
        entry
          .manifest
          .contributions
          .layouts
          .iter()
          .find(|declared| declared.full_id == tab.layout)
          .map(|declared| declared.on_message.clone())
      })
    };
    let Some(Some(command)) = command else {
      return Ok(());
    };
    self.run(
      &command,
      None,
      None,
      Payload::Layout(LayoutCall::Message { tab, pane, data }),
    )?;
    Ok(())
  }

  /// How the workbench shows a webview pane, given its webview's record
  /// (see [`Self::webview`]): a file page's address is its file's in the
  /// open folder, or it says why it can't be shown.
  pub(super) fn pane_webview_info(&self, record: &WebviewRecord) -> WebviewInfo {
    let mut info = record.info();
    if let Some(file) = &record.file_page {
      match self.shared.file_page_site(file) {
        Ok((_, entry)) => info.url.push_str(&crate::webview::encode_path(&entry)),
        Err(problem) => info.blocked = Some(problem),
      }
    }
    info
  }
}
