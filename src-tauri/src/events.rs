//! Backend-to-frontend notifications.
//!
//! The frontend subscribes once with a Tauri `Channel`; events are pushed
//! through it. A channel is part of IPC itself, so no event permissions are
//! granted to the webview. A new subscription (e.g. after a reload) replaces
//! the old one. Events sent before the first subscription (during startup)
//! are kept and delivered when it arrives.

use std::sync::{Mutex, PoisonError};

use flareite_extension_host::MessageLevel;
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use crate::editor::protocol::{DocumentSummary, Position};
use crate::file_drop::FileDragPhase;
use crate::settings::FrontendSettings;

/// One of the versions a merge editor combines.
#[derive(Debug, Clone, Serialize)]
#[serde(
  tag = "type",
  rename_all = "camelCase",
  rename_all_fields = "camelCase"
)]
pub enum AppEvent {
  /// A workspace folder was opened or closed.
  WorkspaceChanged { workspace: Option<WorkspaceInfo> },
  /// Whether the user trusts the open folder changed.
  WorkspaceTrustChanged { workspace: WorkspaceInfo },
  /// An extension asked the user to trust the open folder.
  WorkspaceTrustRequested {
    /// The extension's id.
    extension: String,
  },
  /// Listings of these directories may have changed.
  FilesChanged { directories: Vec<String> },
  /// A clean document was reloaded because its file changed on disk.
  DocumentReloaded { document: DocumentSummary },
  /// A document with unsaved changes was modified on disk by another program.
  DocumentChangedOnDisk { document: DocumentSummary },
  /// A document's file was renamed or moved (from the explorer), so its
  /// path and title changed.
  DocumentMoved { document: DocumentSummary },
  /// A document was saved without the frontend asking (an extension
  /// changed its file).
  DocumentSaved { document: DocumentSummary },
  /// Settings were applied; `announce` tells the user (not when they
  /// changed one setting in the settings editor).
  SettingsChanged {
    settings: FrontendSettings,
    announce: bool,
  },
  /// A background match count for a large document finished.
  SearchCounted { document_id: u64 },
  /// A document's diagnostics changed.
  DiagnosticsChanged { document_id: u64 },
  /// Diagnostics of files that aren't open changed, so the Problems view
  /// should be fetched again (as it should after `DiagnosticsChanged`).
  ProblemsChanged,
  /// A language server started, stopped, or began or finished work.
  LanguageServerChanged,
  /// A background parse finished, so a document's highlighting may have
  /// changed.
  SyntaxUpdated { document_id: u64 },
  /// A file was opened in the background (e.g. at startup).
  DocumentOpened {
    document: DocumentSummary,
    /// Whether it should become the active tab.
    active: bool,
  },
  /// A file named at startup that isn't a text document (it looks binary),
  /// for the workbench to open in an extension's viewer, or to ask about.
  OpenFile {
    path: String,
    /// Whether it should become the active tab.
    active: bool,
  },
  /// An extension asked to open a folder in place of the open one.
  OpenFolder { path: String },
  /// An extension asked to open the document at a provided address.
  OpenProvided { uri: String },
  /// An extension asked to open a layout tab, or show it.
  OpenLayout {
    layout: String,
    key: String,
    beside: bool,
  },
  /// What a layout tab shows changed.
  LayoutChanged { layout: String, key: String },
  /// An extension asked to close a layout tab.
  CloseLayout { layout: String, key: String },
  /// What an editor pane of a layout tab shows besides its text changed.
  PaneDecorationsChanged {
    layout: String,
    key: String,
    pane: String,
  },
  /// An extension asked to select and show a range in a layout's editor
  /// pane.
  RevealInPane {
    layout: String,
    key: String,
    pane: String,
    start: flareite_extension_host::layouts::Point,
    end: flareite_extension_host::layouts::Point,
  },
  /// Progress of the Go to File index.
  IndexStatus {
    indexing: bool,
    files: usize,
    truncated: bool,
  },
  /// Extensions were loaded or changed state.
  ExtensionsChanged,
  /// The languages extensions contribute changed, so documents may be
  /// highlighted differently.
  LanguagesChanged,
  /// An extension asked to show a message.
  ExtensionMessage {
    /// The extension's id.
    id: String,
    /// Its name.
    extension: String,
    level: MessageLevel,
    message: String,
  },
  /// An extension failed and was stopped.
  ExtensionFailed {
    id: String,
    name: String,
    reason: String,
  },
  /// An extension edited a document.
  DocumentEdited { document_id: u64 },
  /// A document's changes from its original (the gutter's) are ready.
  QuickDiffChanged { document_id: u64 },
  /// An extension asked to apply the changes the active editor's selections
  /// touch to its original ("Stage Selected Ranges").
  ApplySelectedRanges,
  /// Extension status bar items changed.
  ExtensionStatusItemsChanged,
  /// The content of an extension view changed.
  ExtensionViewChanged { view: String },
  /// An extension asked to show one of its views.
  ExtensionShowView { view: String },
  /// A document's extension decorations changed.
  DecorationsChanged { document_id: u64 },
  /// Extensions' explorer marks changed.
  ExtensionFileMarksChanged,
  /// An extension asks the user something, replacing any prompt showing.
  ExtensionPrompt {
    prompt: flareite_extension_host::Prompt,
  },
  /// An extension's prompt was withdrawn (the extension stopped).
  ExtensionPromptClosed { id: u64 },
  /// An extension asked to select and show a range in a document.
  ExtensionReveal {
    document_id: u64,
    start: Position,
    end: Position,
  },
  /// Extension previews were opened or closed, or one's title changed.
  ExtensionPreviewsChanged,
  /// The content of an extension preview changed.
  ExtensionPreviewChanged { preview: String },
  /// An extension asked to show one of its previews.
  ExtensionShowPreview {
    preview: String,
    /// Beside its document, rather than in the same group.
    beside: bool,
  },
  /// An extension replaced the page of a webview (viewer or preview).
  ExtensionWebviewChanged { preview: String },
  /// An extension sent a message (JSON text) to a webview's page.
  ExtensionWebviewMessage { preview: String, message: String },
  /// A value an extension stores changed (`value` `None`: removed), or all
  /// were cleared (`scope` and `key` `None`). `source` is the webview whose
  /// page changed it.
  ExtensionStorageChanged {
    extension: String,
    scope: Option<String>,
    key: Option<String>,
    value: Option<String>,
    source: Option<String>,
  },
  /// Something the user should know about, such as files that could not be
  /// reopened or restored unsaved changes.
  Notice { level: NoticeLevel, message: String },
  /// Files and folders dragged in from another program. The position is in
  /// physical pixels from the webview's top left (zero for `Leave`). Paths
  /// come with `Enter` and `Drop`; `drop_id` names a drop, for copying it.
  FileDrag {
    phase: FileDragPhase,
    x: f64,
    y: f64,
    files: Vec<String>,
    folders: Vec<String>,
    drop_id: Option<u64>,
  },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NoticeLevel {
  Warning,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
  pub root: String,
  pub name: String,
  pub trust: crate::trust::Trust,
}

/// Events kept before the frontend first subscribes.
const MAX_EARLY_EVENTS: usize = 256;

#[derive(Default)]
struct Hub {
  channel: Option<Channel<AppEvent>>,
  early: Vec<AppEvent>,
}

#[derive(Default)]
pub struct EventHub(Mutex<Hub>);

impl EventHub {
  pub fn emit(&self, event: AppEvent) {
    let mut hub = self.0.lock().unwrap_or_else(PoisonError::into_inner);
    match hub.channel.as_ref() {
      Some(channel) => {
        if let Err(error) = channel.send(event) {
          tracing::debug!(%error, "event not delivered");
        }
      }
      None if hub.early.len() < MAX_EARLY_EVENTS => hub.early.push(event),
      None => tracing::debug!("event dropped before the frontend subscribed"),
    }
  }

  fn subscribe(&self, channel: Channel<AppEvent>) {
    let mut hub = self.0.lock().unwrap_or_else(PoisonError::into_inner);
    for event in std::mem::take(&mut hub.early) {
      if let Err(error) = channel.send(event) {
        tracing::debug!(%error, "event not delivered");
      }
    }
    hub.channel = Some(channel);
  }
}

/// Sends `event` to the frontend, if it has subscribed.
pub fn emit(app: &AppHandle, event: AppEvent) {
  app.state::<EventHub>().emit(event);
}

#[tauri::command]
pub fn events_subscribe(hub: State<'_, EventHub>, channel: Channel<AppEvent>) {
  hub.subscribe(channel);
}
