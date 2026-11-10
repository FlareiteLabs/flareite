//! Integration tests: real extensions, built from `extensions/` for
//! `wasm32-unknown-unknown`, running in the host against an editor core.
//!
//! `example.tidy` and `example.todos` are example extensions; the fixtures
//! in `extensions/test-fixtures/` exercise the rest of the API, and
//! `test.misbehaving` breaks rules and exceeds limits on request, and every
//! test checks that the host (and the document) survive it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use flareite_core::{DocumentId, Editor, EditorOp, LanguageChoice};
use flareite_extension_host::preview::{ImageSource, LinkTarget, PreviewElement};
use flareite_extension_host::{
  BuildState, ConfigValue, Embedder, ExtensionHost, ExtensionState, ExtensionStatus, HostOptions,
  Limits, Notification, OpenFolder, Permission, PreviewInfo, PromptKind, PromptReply, Severity,
};

/// Builds the extensions workspace once and returns its output folder.
fn wasm_dir() -> &'static Path {
  static BUILT: OnceLock<PathBuf> = OnceLock::new();
  BUILT.get_or_init(|| {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
      .args(["build", "--release", "--quiet"])
      .current_dir(&root)
      .env_remove("CARGO_TARGET_DIR")
      .env_remove("CARGO_BUILD_TARGET")
      .env_remove("RUSTFLAGS")
      .env_remove("CARGO_ENCODED_RUSTFLAGS")
      .status()
      .expect("cargo runs");
    assert!(
      status.success(),
      "building the test extensions failed; is the target installed? \
       (rustup target add wasm32-unknown-unknown)"
    );
    root.join("target/wasm32-unknown-unknown/release")
  })
}

/// The application side: an editor core, and the notifications received.
#[derive(Default)]
struct Fake {
  editor: Mutex<Editor>,
  notifications: Mutex<Vec<Notification>>,
  arrived: Condvar,
  /// The open folder, if any.
  folder: Mutex<Option<PathBuf>>,
  /// Documents saved through `save`.
  saved: Mutex<Vec<DocumentId>>,
  /// Whether the open folder is untrusted.
  untrusted: Mutex<bool>,
  /// Files on the "web", by address, for `download`.
  web: Mutex<BTreeMap<String, String>>,
  /// Where downloaded files are put.
  downloads: OnceLock<tempfile::TempDir>,
  /// The "credential store", by account.
  secrets: Mutex<BTreeMap<String, String>>,
}

impl Embedder for Fake {
  fn with_editor(&self, f: &mut dyn FnMut(&mut Editor)) {
    f(&mut self.editor());
  }

  fn language_of(&self, path: &Path) -> Option<String> {
    path
      .extension()
      .and_then(|extension| extension.to_str())
      .map(|extension| match extension {
        "rs" => "rust",
        "md" => "markdown",
        other => other,
      })
      .map(str::to_owned)
  }

  fn notify(&self, notification: Notification) {
    self
      .notifications
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push(notification);
    self.arrived.notify_all();
  }

  fn open_folder(&self) -> Option<OpenFolder> {
    let root = self
      .folder
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()?;
    Some(OpenFolder {
      root,
      exclude: flareite_workspace::ExcludeFilter::new(&[".git", "excluded"]).expect("valid"),
      trusted: !*self
        .untrusted
        .lock()
        .unwrap_or_else(PoisonError::into_inner),
    })
  }

  fn save(&self, document: DocumentId) -> Result<(), String> {
    let mut editor = self.editor();
    let document = editor.document_mut(document).map_err(|e| e.to_string())?;
    let point = document.save_point();
    let path = document.path().expect("a file").to_path_buf();
    let stamp = flareite_workspace::save_text_file(&path, &point.snapshot, point.encoding, None)
      .map_err(|e| e.to_string())?;
    document.mark_saved(&point, path, stamp);
    self
      .saved
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push(document.id());
    Ok(())
  }

  fn download(&self, address: &str) -> Result<PathBuf, String> {
    let content = self
      .web
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get(address)
      .cloned()
      .ok_or("not found")?;
    let dir = self
      .downloads
      .get_or_init(|| tempfile::tempdir().expect("temp dir"));
    let path = dir.path().join(address.len().to_string());
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(path)
  }

  fn secret(&self, account: &str) -> Result<Option<String>, String> {
    Ok(
      self
        .secrets
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(account)
        .cloned(),
    )
  }

  fn set_secret(&self, account: &str, value: Option<&str>) -> Result<(), String> {
    let mut secrets = self.secrets.lock().unwrap_or_else(PoisonError::into_inner);
    match value {
      Some(value) => secrets.insert(account.to_owned(), value.to_owned()),
      None => secrets.remove(account),
    };
    Ok(())
  }
}

impl Fake {
  fn editor(&self) -> MutexGuard<'_, Editor> {
    self.editor.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Waits for a notification matching `wanted` (which may have arrived
  /// already) and removes it and everything before it.
  fn wait_for(&self, what: &str, wanted: impl Fn(&Notification) -> bool) -> Notification {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut notifications = self
      .notifications
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    loop {
      if let Some(index) = notifications.iter().position(&wanted) {
        let found = notifications[index].clone();
        notifications.drain(..=index);
        return found;
      }
      let now = Instant::now();
      assert!(
        now < deadline,
        "timed out waiting for {what}; got {notifications:#?}"
      );
      notifications = self
        .arrived
        .wait_timeout(notifications, deadline - now)
        .unwrap_or_else(PoisonError::into_inner)
        .0;
    }
  }

  fn wait_for_message(&self, starting: &str) -> String {
    match self.wait_for(
      starting,
      |n| matches!(n, Notification::Message { message, .. } if message.starts_with(starting)),
    ) {
      Notification::Message { message, .. } => message,
      _ => unreachable!(),
    }
  }

  /// Opens an untitled document with `text` and tells the host.
  fn open(&self, host: &ExtensionHost, text: &str) -> DocumentId {
    let mut editor = self.editor();
    let id = editor.create_untitled();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert(text.into()))
      .expect("insert");
    document
      .apply(EditorOp::SetCursor {
        offset: 0,
        extend: false,
        add: false,
      })
      .expect("cursor");
    host.sync(document);
    id
  }

  /// Opens the file at `path` and tells the host.
  fn open_file(&self, host: &ExtensionHost, path: &Path) -> DocumentId {
    let file = flareite_workspace::load_text_file(path).expect("readable");
    let mut editor = self.editor();
    let id = editor.open(path.to_path_buf(), file);
    host.sync(editor.document(id).expect("open"));
    id
  }

  fn type_text(&self, host: &ExtensionHost, id: DocumentId, text: &str) {
    let mut editor = self.editor();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::Insert(text.into()))
      .expect("insert");
    host.sync(document);
  }

  fn text(&self, id: DocumentId) -> String {
    self
      .editor()
      .document(id)
      .expect("open")
      .buffer()
      .contents()
  }
}

/// Copies a built extension into `dir` with its manifest, edited by `edit`.
fn install(
  dir: &Path,
  crate_name: &str,
  manifest_dir: &str,
  edit: impl FnOnce(String) -> String,
) -> PathBuf {
  let source = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../../extensions")
    .join(manifest_dir);
  let target = dir.join(crate_name);
  std::fs::create_dir_all(&target).expect("mkdir");
  let wasm = format!("{crate_name}.wasm");
  std::fs::copy(wasm_dir().join(&wasm), target.join(&wasm)).expect("copy wasm");
  for file in ["icon.svg", "README.md"] {
    if source.join(file).exists() {
      std::fs::copy(source.join(file), target.join(file)).expect("copy");
    }
  }
  // Webview pages.
  if let Ok(entries) = std::fs::read_dir(source.join("media")) {
    std::fs::create_dir_all(target.join("media")).expect("mkdir");
    for entry in entries.flatten() {
      std::fs::copy(entry.path(), target.join("media").join(entry.file_name())).expect("copy");
    }
  }
  let manifest = std::fs::read_to_string(source.join("extension.toml")).expect("manifest");
  let manifest = manifest
    .lines()
    .map(|line| {
      if line.starts_with("entrypoint") {
        format!("entrypoint = \"{wasm}\"")
      } else {
        line.to_owned()
      }
    })
    .collect::<Vec<_>>()
    .join("\n");
  std::fs::write(target.join("extension.toml"), edit(manifest)).expect("write manifest");
  target
}

struct Setup {
  _dir: tempfile::TempDir,
  fake: Arc<Fake>,
  host: ExtensionHost,
}

fn limits() -> Limits {
  Limits {
    memory_bytes: 32 * 1024 * 1024,
    total_memory_bytes: 48 * 1024 * 1024,
    activation_time: Duration::from_secs(10),
    command_time: Duration::from_millis(1500),
    event_time: Duration::from_millis(1500),
    queue_capacity: 64,
  }
}

/// [`limits`], but with the editor's command deadline: the example
/// extensions run programs (such as several `git`s) within a command, which
/// takes long on a loaded CI machine. Only the misbehaving fixture, whose
/// tests time out commands on purpose, has the short one.
fn example_limits() -> Limits {
  Limits {
    command_time: Limits::default().command_time,
    ..limits()
  }
}

/// A host with one extension installed as a development extension.
/// `grant` lists the permissions granted up front (`None`: undecided).
fn setup(
  crate_name: &str,
  manifest_dir: &str,
  grant: Option<&[Permission]>,
  settings: &str,
  edit: impl FnOnce(String) -> String,
) -> Setup {
  let dir = tempfile::tempdir().expect("temp dir");
  let extension = install(dir.path(), crate_name, manifest_dir, edit);
  let permissions_file = dir.path().join("permissions.json");
  let fake = Arc::new(Fake::default());
  let settings: BTreeMap<String, toml::Table> = toml::from_str(settings).expect("settings");
  let host = ExtensionHost::new(
    fake.clone(),
    HostOptions {
      editor_version: semver::Version::new(0, 1, 0),
      extensions_dir: None,
      development: vec![extension],
      builds: false,
      permissions_file: Some(permissions_file),
      settings,
      programs: BTreeMap::new(),
      limits: if crate_name == "misbehaving" {
        limits()
      } else {
        example_limits()
      },
      storage_dir: None,
    },
  );
  if let Some(granted) = grant {
    // Decisions are made per loaded extension; load once to record them.
    host.load();
    let id = host.statuses()[0].id.clone();
    host.set_permissions(&id, granted);
  }
  Setup {
    _dir: dir,
    fake,
    host,
  }
}

const ALL: &[Permission] = &[Permission::DocumentsRead, Permission::DocumentsWrite];

fn misbehaving(settings: &str) -> Setup {
  setup(
    "misbehaving",
    "test-fixtures/misbehaving",
    Some(ALL),
    settings,
    |m| m,
  )
}

fn state_of(host: &ExtensionHost) -> (ExtensionState, Option<String>) {
  let status = &host.statuses()[0];
  (status.state, status.error.clone())
}

fn run(setup: &Setup, command: &str, document: Option<DocumentId>) -> Result<(), String> {
  setup
    .host
    .run_command(&format!("test.misbehaving.{command}"), document, None)?
    .wait()
}

#[test]
fn tidy_flags_and_trims_trailing_whitespace() {
  let s = setup("tidy", "tidy", Some(ALL), "", |m| m);
  let id = s.fake.open(&s.host, "fn main() {   \n  ok();\n}\n");
  s.host.load();
  s.fake.wait_for("diagnostics", |n| {
    *n == Notification::DiagnosticsChanged(id)
  });
  let diagnostics = {
    let editor = s.fake.editor();
    let document = editor.document(id).expect("open");
    s.host.diagnostics(document, 0..document.buffer().len())
  };
  assert_eq!(diagnostics.len(), 1);
  assert_eq!(diagnostics[0].range, 11..14);
  assert_eq!(diagnostics[0].severity, Severity::Warning);
  assert_eq!(diagnostics[0].source, "Tidy");
  assert_eq!(diagnostics[0].code.as_deref(), Some("trailing-whitespace"));

  s.host
    .run_command("example.tidy.trimTrailingWhitespace", Some(id), None)
    .expect("queued")
    .wait()
    .expect("trimmed");
  assert_eq!(s.fake.text(id), "fn main() {\n  ok();\n}\n");
  s.fake
    .wait_for("edit", |n| *n == Notification::DocumentEdited(id));
  s.fake
    .wait_for_message("Trimmed trailing whitespace from 1 line.");

  // The extension hears about its own edit and clears the diagnostic.
  s.fake.wait_for("cleared diagnostics", |n| {
    *n == Notification::DiagnosticsChanged(id)
  });
  let editor = s.fake.editor();
  let document = editor.document(id).expect("open");
  assert!(
    s.host
      .diagnostics(document, 0..document.buffer().len())
      .is_empty()
  );
}

#[test]
fn tidy_rechecks_only_changed_lines_and_counts_words() {
  let s = setup(
    "tidy",
    "tidy",
    Some(ALL),
    r#"[ "example.tidy" ]
max_line_length = 10
"#,
    |m| m,
  );
  let id = s.fake.open(&s.host, "short\n");
  s.host.load();
  s.fake.wait_for("diagnostics", |n| {
    *n == Notification::DiagnosticsChanged(id)
  });
  // Typing at the start of the first line makes it too long.
  s.fake.type_text(&s.host, id, "much longer ");
  s.fake.wait_for("new diagnostics", |n| {
    *n == Notification::DiagnosticsChanged(id)
  });
  let diagnostics = {
    let editor = s.fake.editor();
    let document = editor.document(id).expect("open");
    s.host.diagnostics(document, 0..document.buffer().len())
  };
  assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
  assert!(
    diagnostics[0]
      .message
      .contains("17 characters long (limit 10)")
  );

  s.host
    .run_command("example.tidy.countWords", Some(id), None)
    .expect("queued")
    .wait()
    .expect("counted");
  s.fake.wait_for_message("3 words in 2 lines.");
}

#[test]
fn extensions_wait_for_permission_and_denied_permissions_are_enforced() {
  let s = setup("tidy", "tidy", None, "", |m| m);
  let id = s.fake.open(&s.host, "x  \n");
  s.host.load();
  assert_eq!(state_of(&s.host).0, ExtensionState::AwaitingPermission);
  let refused = s
    .host
    .run_command("example.tidy.trimTrailingWhitespace", Some(id), None)
    .err()
    .expect("refused while undecided");
  assert!(refused.contains("allow or deny"), "{refused}");

  s.host
    .set_permissions("example.tidy", &[Permission::DocumentsRead]);
  let error = s
    .host
    .run_command("example.tidy.trimTrailingWhitespace", Some(id), None)
    .expect("queued")
    .wait()
    .expect_err("editing is not allowed");
  assert!(error.contains("not allowed to edit documents"), "{error}");
  assert_eq!(s.fake.text(id), "x  \n", "the document is unchanged");
  assert_eq!(state_of(&s.host).0, ExtensionState::Active);
}

#[test]
fn forgotten_decisions_are_asked_for_again() {
  let s = setup("tidy", "tidy", Some(ALL), "", |m| m);
  s.host.set_enabled("example.tidy", false);
  assert_eq!(state_of(&s.host).0, ExtensionState::Disabled);
  // As when it's uninstalled and installed again.
  s.host.forget_decisions("example.tidy");
  s.host.load();
  assert_eq!(state_of(&s.host).0, ExtensionState::AwaitingPermission);
}

#[test]
fn document_changes_are_sent_as_incremental_utf8_changes() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "héllo\nworld");
  s.host.load();
  // It activates for its commands; open documents are announced first.
  run(&s, "read", Some(id)).expect("ran");
  s.fake.wait_for_message(&format!("opened {}", id.get()));
  {
    let mut editor = s.fake.editor();
    let document = editor.document_mut(id).expect("open");
    document
      .apply(EditorOp::SetCursor {
        offset: 7,
        extend: false,
        add: false,
      })
      .expect("cursor");
    document
      .apply(EditorOp::Insert("big ".into()))
      .expect("insert");
    s.host.sync(document);
  }
  let message = s.fake.wait_for_message(&format!("changed {}", id.get()));
  assert!(message.ends_with("[1:0-1:0=\"big \"]"), "{message}");

  s.host.closed(id);
  s.fake.wait_for_message(&format!("closed {}", id.get()));
}

#[test]
fn edits_are_validated_and_applied_atomically() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "abcdef");
  s.host.load();

  run(&s, "editStale", Some(id)).expect("ran");
  assert!(s.fake.wait_for_message("edit:").contains("StaleVersion"));
  run(&s, "editBadRange", Some(id)).expect("ran");
  assert!(s.fake.wait_for_message("edit:").contains("InvalidRange"));
  run(&s, "editOverlapping", Some(id)).expect("ran");
  assert!(s.fake.wait_for_message("edit:").contains("edits overlap"));
  assert_eq!(s.fake.text(id), "abcdef", "rejected edits change nothing");

  run(&s, "edit", Some(id)).expect("ran");
  assert!(s.fake.wait_for_message("edit:").contains("Ok("));
  assert_eq!(s.fake.text(id), "Xabcdef");
  // One undo step.
  s.fake
    .editor()
    .document_mut(id)
    .expect("open")
    .apply(EditorOp::Undo)
    .expect("undo");
  assert_eq!(s.fake.text(id), "abcdef");
}

#[test]
fn extensions_have_no_filesystem_environment_or_arguments() {
  let s = misbehaving("");
  s.host.load();
  run(&s, "escape", None).expect("ran");
  let message = s.fake.wait_for_message("file:");
  assert!(message.starts_with("file: Err("), "{message}");
  assert!(message.contains("env: Err(NotPresent)"), "{message}");
  assert!(message.ends_with("args: 0"), "{message}");
}

#[test]
fn an_infinite_loop_is_stopped_and_the_extension_can_be_restarted() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "text");
  s.host.load();
  // Start (and compile) the extension first, so only the loop is timed.
  run(&s, "read", Some(id)).expect("started");
  let started = Instant::now();
  let error = run(&s, "loop", Some(id)).expect_err("stopped");
  assert!(error.contains("took longer than"), "{error}");
  // The 1.5 s command deadline, with room for a loaded test machine.
  assert!(started.elapsed() < Duration::from_secs(10));
  let failed = s.fake.wait_for("failure", |n| {
    matches!(n, Notification::ExtensionFailed { .. })
  });
  assert!(
    matches!(failed, Notification::ExtensionFailed { reason, .. } if reason.contains("took longer"))
  );
  assert_eq!(state_of(&s.host).0, ExtensionState::Failed);
  let refused = run(&s, "read", Some(id)).expect_err("failed extensions don't run");
  assert!(refused.contains("Restart"), "{refused}");

  s.host.restart("test.misbehaving");
  run(&s, "read", Some(id)).expect("works again");
  assert!(s.fake.wait_for_message("read:").contains("[\"text\"]"));
  assert_eq!(s.fake.text(id), "text");
}

#[test]
fn resource_use_is_accounted_per_extension() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "text");
  s.host.load();
  run(&s, "read", Some(id)).expect("ran");
  let usage = s.host.statuses()[0].usage.clone();
  // Activation, the opened document, and the command.
  assert!(usage.calls >= 3, "{usage:?}");
  assert!(usage.busy_ms > 0.0 && usage.slowest_call_ms <= usage.busy_ms);
  assert!(usage.compile_ms > 0.0);
  assert!(usage.memory_bytes > 0 && usage.peak_memory_bytes >= usage.memory_bytes);

  run(&s, "alloc", None).expect_err("exceeds its memory");
  let after = s.host.statuses()[0].usage.clone();
  assert_eq!(after.memory_bytes, 0, "a stopped instance holds no memory");
  assert!(after.peak_memory_bytes >= usage.peak_memory_bytes);
  assert!(after.calls > usage.calls);

  // Totals survive a restart.
  s.host.restart("test.misbehaving");
  run(&s, "read", Some(id)).expect("works again");
  assert!(s.host.statuses()[0].usage.calls > after.calls);
}

#[test]
fn memory_stack_and_panics_end_the_instance_with_a_clear_reason() {
  for (command, expected) in [
    ("alloc", "memory limit of 32 MiB"),
    ("recurse", "stack space"),
    ("panic", "panicked"),
  ] {
    let s = misbehaving("");
    s.host.load();
    let error = run(&s, command, None).expect_err("fails");
    assert!(error.contains(expected), "{command}: {error}");
    let (state, reason) = state_of(&s.host);
    assert_eq!(state, ExtensionState::Failed);
    assert!(reason.is_some_and(|reason| reason.contains(expected)));
  }
}

#[test]
fn output_floods_are_limited() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "x");
  s.host.load();
  run(&s, "flood", Some(id)).expect("ran");
  assert!(s.fake.wait_for_message("diagnostics:").contains("TooLarge"));
  assert_eq!(state_of(&s.host).0, ExtensionState::Active);
}

#[test]
fn diagnostics_are_removed_when_an_extension_fails() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "abc");
  s.host.load();
  run(&s, "diagnose", Some(id)).expect("ran");
  assert!(s.fake.wait_for_message("diagnostics:").contains("Ok(())"));
  let count = || {
    let editor = s.fake.editor();
    let document = editor.document(id).expect("open");
    s.host.diagnostics_at(document, 0).len()
  };
  assert_eq!(count(), 1);
  run(&s, "panic", Some(id)).expect_err("fails");
  assert_eq!(count(), 0);
}

#[test]
fn activation_failures_and_slow_handlers_are_reported() {
  let refused = misbehaving("[\"test.misbehaving\"]\non_activate = \"error\"\n");
  refused.host.load();
  run(&refused, "read", None).expect_err("activation fails");
  let (state, reason) = state_of(&refused.host);
  assert_eq!(state, ExtensionState::Failed);
  assert!(reason.is_some_and(|reason| reason.contains("activation refused")));

  let slow = misbehaving("[\"test.misbehaving\"]\non_change = \"loop\"\n");
  let id = slow.fake.open(&slow.host, "a");
  slow.host.load();
  run(&slow, "read", Some(id)).expect("starts");
  slow.fake.type_text(&slow.host, id, "b");
  let failed = slow.fake.wait_for("failure", |n| {
    matches!(n, Notification::ExtensionFailed { .. })
  });
  assert!(
    matches!(&failed, Notification::ExtensionFailed { reason, .. } if reason.contains("Handling a document change took longer")),
    "{failed:?}"
  );
}

#[test]
fn configuration_comes_from_settings_with_declared_defaults() {
  let s = misbehaving("[\"test.misbehaving\"]\nlimit = 7\non_change = \"sideways\"\n");
  s.host.load();
  run(&s, "config", None).expect("ran");
  let message = s.fake.wait_for_message("on_change:");
  assert_eq!(
    message,
    "on_change: report; limit: Some(ConfigValue::Integer(7)); unknown: None"
  );
  let errors = &s.host.statuses()[0].config_errors;
  assert_eq!(errors.len(), 1);
  assert!(errors[0].contains("on_change"), "{errors:?}");
  assert!(s.host.statuses()[0].has_settings);

  // For a settings editor: every declared setting with the value in use.
  let configurations = s.host.configurations();
  assert_eq!(configurations.len(), 1);
  assert_eq!(configurations[0].id, "test.misbehaving");
  let setting = |key: &str| {
    configurations[0]
      .settings
      .iter()
      .find(|setting| setting.key == key)
      .expect(key)
  };
  assert_eq!(setting("limit").value, ConfigValue::Integer(7));
  assert_eq!(setting("limit").problem, None);
  let on_change = setting("on_change");
  assert_eq!(on_change.value, on_change.property.default);
  assert!(on_change.problem.is_some(), "an invalid value is explained");
}

#[test]
fn invalid_manifests_and_foreign_components_are_reported() {
  let bad = setup("misbehaving", "test-fixtures/misbehaving", None, "", |m| {
    m.replace("api_version = \"1\"", "api_version = \"7\"")
  });
  bad.host.load();
  let (state, reason) = state_of(&bad.host);
  assert_eq!(state, ExtensionState::Invalid);
  assert!(reason.is_some_and(|reason| reason.contains("API version 7")));

  // A component that imports an API version this host doesn't provide.
  let foreign = setup(
    "misbehaving",
    "test-fixtures/misbehaving",
    Some(ALL),
    "",
    |m| m,
  );
  let component =
    wat::parse_str("(component (import \"flareite:extension/host@9.0.0\" (instance)))")
      .expect("wat");
  let dir = PathBuf::from(&foreign.host.statuses()[0].path);
  std::fs::write(dir.join("misbehaving.wasm"), component).expect("write");
  foreign.host.load();
  let error = run(&foreign, "read", None).expect_err("cannot start");
  assert!(
    error.contains("does not implement the Flareite extension API"),
    "{error}"
  );
}

#[test]
fn shutdown_stops_busy_extensions_promptly() {
  let s = misbehaving("");
  s.host.load();
  let ticket = s
    .host
    .run_command("test.misbehaving.loop", None, None)
    .expect("queued");
  std::thread::sleep(Duration::from_millis(200));
  let started = Instant::now();
  s.host.shutdown();
  assert!(started.elapsed() < Duration::from_secs(3));
  assert!(ticket.wait().is_err());
}

#[test]
fn extensions_fill_status_items_views_and_decorations() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "abcdef");
  s.host.load();
  // The manifest's text shows before the extension runs.
  let items = s.host.status_items();
  assert_eq!(items.len(), 1);
  assert_eq!(items[0].id, "test.misbehaving.state");
  assert_eq!(items[0].content.text, "idle");

  run(&s, "ui", Some(id)).expect("ran");
  // In the order the extension made the calls.
  s.fake
    .wait_for("status", |n| *n == Notification::StatusItemsChanged);
  s.fake.wait_for("decorations", |n| {
    *n == Notification::DecorationsChanged(id)
  });
  let reveal = s
    .fake
    .wait_for("reveal", |n| matches!(n, Notification::Reveal { .. }));
  assert!(
    matches!(reveal, Notification::Reveal { document, end, .. } if document == id && end.column == 2)
  );
  let view = Notification::ShowView("test.misbehaving.items".into());
  s.fake.wait_for("show view", |n| *n == view);
  let report = s.fake.wait_for_message("ui:");
  assert_eq!(report, "ui: Ok(()) Ok(()) Ok(()) Ok(()) Ok(()) Ok(())");
  let items = s.host.status_items();
  assert_eq!(items[0].content.text, "busy");
  assert_eq!(
    items[0].content.command.as_deref(),
    Some("test.misbehaving.argument")
  );
  let view = s.host.view_content("test.misbehaving.items");
  assert_eq!(view.message.as_deref(), Some("Two items"));
  assert_eq!(view.items.len(), 2);
  assert_eq!(view.items[1].parent.as_deref(), Some("a"));
  let decorations = {
    let editor = s.fake.editor();
    s.host.decorations(editor.document(id).expect("open"), 0..6)
  };
  assert_eq!(decorations.len(), 1);
  assert_eq!(decorations[0].range, 0..2);
  assert_eq!(decorations[0].key, "test.misbehaving/mark");

  // Decorations move with the text.
  s.fake.type_text(&s.host, id, "12");
  let editor = s.fake.editor();
  let moved = s.host.decorations(editor.document(id).expect("open"), 0..8);
  assert_eq!(moved[0].range, 2..4);
}

#[test]
fn commands_receive_their_argument() {
  let s = misbehaving("");
  s.host.load();
  s.host
    .run_command("test.misbehaving.argument", None, Some("item b".into()))
    .expect("queued")
    .wait()
    .expect("ran");
  assert_eq!(
    s.fake.wait_for_message("argument:"),
    "argument: Some(\"item b\")"
  );
  run(&s, "argument", None).expect("ran");
  assert_eq!(s.fake.wait_for_message("argument:"), "argument: None");
}

#[test]
fn workbench_misuse_is_rejected_and_output_is_removed_on_failure() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "abc");
  s.host.load();
  run(&s, "uiErrors", Some(id)).expect("ran");
  let report = s.fake.wait_for_message("uiErrors:");
  assert_eq!(report.matches("InvalidArgument").count(), 5, "{report}");

  run(&s, "ui", Some(id)).expect("ran");
  s.fake.wait_for_message("ui:");
  run(&s, "panic", Some(id)).expect_err("fails");
  assert!(
    s.host.status_items().is_empty(),
    "a failed extension's items are hidden"
  );
  assert!(
    s.host
      .view_content("test.misbehaving.items")
      .items
      .is_empty()
  );
  let editor = s.fake.editor();
  assert!(
    s.host
      .decorations(editor.document(id).expect("open"), 0..3)
      .is_empty()
  );
}

#[test]
fn decorations_need_the_read_permission() {
  let s = misbehaving("");
  let id = s.fake.open(&s.host, "abc");
  s.host.load();
  s.host.set_permissions("test.misbehaving", &[]);
  run(&s, "ui", Some(id)).expect("ran");
  let report = s.fake.wait_for_message("ui:");
  assert!(report.contains("PermissionDenied"), "{report}");
  // Status items and views need no permission.
  assert!(report.starts_with("ui: Ok(()) Ok(()) Ok(())"), "{report}");
}

#[test]
fn showing_a_view_activates_its_extension() {
  let s = misbehaving("");
  s.host.load();
  assert_eq!(state_of(&s.host).0, ExtensionState::Inactive);
  s.host.view_opened("test.misbehaving.items");
  s.fake.wait_for_message("activated with");
}

#[test]
fn statuses_have_the_author_icon_readme_and_activation_time() {
  let s = setup("tidy", "tidy", Some(ALL), "", |m| m);
  s.host.load();
  s.fake
    .wait_for("started", |n| *n == Notification::ExtensionsChanged);
  let deadline = Instant::now() + Duration::from_secs(30);
  while state_of(&s.host).0 != ExtensionState::Active {
    assert!(Instant::now() < deadline, "Tidy never started");
    std::thread::sleep(Duration::from_millis(20));
  }

  let status = &s.host.statuses()[0];
  assert_eq!(status.author.as_deref(), Some("Flareite"));
  assert!(status.has_icon && status.has_readme);
  assert!(status.usage.activation_ms.is_some_and(|ms| ms > 0.0));
  let icon = s
    .host
    .icon("example.tidy")
    .expect("has one")
    .expect("loads");
  assert!(icon.starts_with("data:image/svg+xml;base64,"));
  let readme = s
    .host
    .readme("example.tidy")
    .expect("has one")
    .expect("renders");
  assert!(matches!(
    readme.nodes[0].element,
    PreviewElement::Heading { level: 1 }
  ));
  assert!(s.host.icon("example.unknown").is_none());
}

const WITH_FILES: &[Permission] = &[
  Permission::DocumentsRead,
  Permission::DocumentsWrite,
  Permission::FilesRead,
  Permission::FilesWrite,
];

/// A folder of files for Tidy to trim, open in the fake editor.
fn tidy_folder(s: &Setup) -> PathBuf {
  let root = flareite_workspace::normalize_path(s._dir.path()).join("project");
  let write = |relative: &str, bytes: &[u8]| {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, bytes).expect("write");
  };
  write("src/a.rs", b"fn a() {  \r\n}\r\n");
  write("src/clean.rs", b"fn clean() {}\n");
  write("src/open.rs", b"open  \n");
  write("src/edited.rs", b"edited  \n");
  write("src/image.bin", b"\x89PNG\0\0  \n");
  write("src/latin1.rs", b"// caf\xe9  \n");
  write("notes.md", b"hard break  \n");
  write("excluded/x.rs", b"x  \n");
  *s.fake.folder.lock().unwrap_or_else(PoisonError::into_inner) = Some(root.clone());
  root
}

#[test]
fn tidy_trims_files_in_a_folder_from_the_explorer() {
  let s = setup("tidy", "tidy", Some(WITH_FILES), "", |m| m);
  let root = tidy_folder(&s);
  let src = root.join("src");
  let open = s.fake.open_file(&s.host, &src.join("open.rs"));
  let edited = s.fake.open_file(&s.host, &src.join("edited.rs"));
  s.fake.type_text(&s.host, edited, "// mine\n");
  s.host.load();

  s.host
    .run_command(
      "example.tidy.trimFiles",
      None,
      Some(root.display().to_string()),
    )
    .expect("queued")
    .wait()
    .expect("trimmed");
  assert_eq!(
    s.fake.wait_for_message("Trimmed"),
    "Trimmed trailing whitespace in 4 files."
  );
  let read = |path: &Path| std::fs::read(path).expect("reads");
  // Line breaks are kept.
  assert_eq!(read(&src.join("a.rs")), b"fn a() {\r\n}\r\n");
  assert_eq!(read(&src.join("clean.rs")), b"fn clean() {}\n");
  // Written back in its own encoding.
  assert_eq!(read(&src.join("latin1.rs")), b"// caf\xe9\n");
  // Skipped: not text, Markdown, and excluded.
  assert_eq!(read(&src.join("image.bin")), b"\x89PNG\0\0  \n");
  assert_eq!(read(&root.join("notes.md")), b"hard break  \n");
  assert_eq!(read(&root.join("excluded/x.rs")), b"x  \n");

  // An open document is changed in the editor (undoably), and saved if it
  // had no unsaved changes.
  assert_eq!(s.fake.text(open), "open\n");
  assert_eq!(read(&src.join("open.rs")), b"open\n");
  assert_eq!(s.fake.text(edited), "// mine\nedited\n");
  assert_eq!(read(&src.join("edited.rs")), b"edited  \n");
  assert_eq!(
    *s.fake.saved.lock().unwrap_or_else(PoisonError::into_inner),
    vec![open]
  );
  {
    let editor = s.fake.editor();
    assert!(!editor.document(open).expect("open").is_dirty());
    assert!(editor.document(edited).expect("open").is_dirty());
  }

  // A file outside the folder, or an excluded one, can't be used.
  let outside = s._dir.path().join("outside.rs");
  std::fs::write(&outside, "x  \n").expect("write");
  for path in [outside, root.join("excluded/x.rs")] {
    let error = s
      .host
      .run_command(
        "example.tidy.trimFiles",
        None,
        Some(path.display().to_string()),
      )
      .expect("queued")
      .wait()
      .expect_err("refused");
    assert!(error.contains("only files in the open folder"), "{error}");
  }
}

#[test]
fn files_need_their_permissions() {
  let s = setup("tidy", "tidy", Some(ALL), "", |m| m);
  let root = tidy_folder(&s);
  s.host.load();
  let error = s
    .host
    .run_command(
      "example.tidy.trimFiles",
      None,
      Some(root.display().to_string()),
    )
    .expect("queued")
    .wait()
    .expect_err("refused");
  assert!(error.contains("not allowed files-read"), "{error}");
  assert_eq!(
    std::fs::read(root.join("src/a.rs")).expect("reads"),
    b"fn a() {  \r\n}\r\n"
  );
}

#[test]
fn a_moved_document_is_reopened_for_extensions() {
  let s = setup("tidy", "tidy", Some(ALL), "", |m| m);
  let dir = flareite_workspace::normalize_path(s._dir.path());
  std::fs::write(dir.join("a.txt"), "x  \n").expect("write");
  let id = s.fake.open_file(&s.host, &dir.join("a.txt"));
  s.host.load();
  s.fake.wait_for("diagnostics", |n| {
    *n == Notification::DiagnosticsChanged(id)
  });

  std::fs::rename(dir.join("a.txt"), dir.join("b.txt")).expect("rename");
  {
    let mut editor = s.fake.editor();
    editor.follow_move(&dir.join("a.txt"), &dir.join("b.txt"));
    s.host.sync(editor.document(id).expect("open"));
  }
  // Closed and opened again: Tidy checks it afresh.
  s.fake.wait_for("diagnostics again", |n| {
    *n == Notification::DiagnosticsChanged(id)
  });
  let editor = s.fake.editor();
  let document = editor.document(id).expect("open");
  assert_eq!(
    s.host
      .diagnostics(document, 0..document.buffer().len())
      .len(),
    1
  );
}

#[test]
fn choosing_a_documents_language_starts_extensions_for_it() {
  let s = setup("todos", "todos", Some(ALL), "", |m| {
    m.replace(
      "activation = [\"on-startup\"]",
      "activation = [\"on-language:rust\"]",
    )
  });
  let id = s.fake.open(&s.host, "// TODO: x\n");
  s.host.load();
  assert_ne!(state_of(&s.host).0, ExtensionState::Active, "plain text");

  {
    let mut editor = s.fake.editor();
    let document = editor.document_mut(id).expect("open");
    document.set_language_choice(Some(LanguageChoice::Language("rust".into())));
    s.host.sync(document);
  }
  s.fake.wait_for("decorations", |n| {
    *n == Notification::DecorationsChanged(id)
  });
  assert_eq!(state_of(&s.host).0, ExtensionState::Active);
}

#[test]
fn theme_only_extensions_need_no_code() {
  let dir = tempfile::tempdir().expect("temp dir");
  let extension = dir.path().join("themes");
  std::fs::create_dir_all(extension.join("themes")).expect("mkdir");
  std::fs::write(
    extension.join("extension.toml"),
    "id = \"example.themes\"\nname = \"Themes\"\nversion = \"1.0.0\"\napi_version = \"1\"\n\
     [[themes]]\nid = \"night\"\nlabel = \"Night\"\nkind = \"dark\"\npath = \"themes/night.toml\"\n",
  )
  .expect("manifest");
  let theme = extension.join("themes/night.toml");
  std::fs::write(&theme, "[colors]\neditor-background = \"#101020\"\n").expect("theme");
  let fake = Arc::new(Fake::default());
  let options = HostOptions {
    editor_version: semver::Version::new(0, 1, 0),
    extensions_dir: Some(dir.path().to_path_buf()),
    development: Vec::new(),
    builds: false,
    permissions_file: None,
    settings: BTreeMap::new(),
    programs: BTreeMap::new(),
    limits: limits(),
    storage_dir: None,
  };
  let host = ExtensionHost::new(fake, options);
  host.load();
  let status = &host.statuses()[0];
  assert_eq!(status.state, ExtensionState::Ready);
  let themes = &status.contributions.themes;
  assert_eq!(themes[0].id, "example.themes.night");
  assert_eq!(themes[0].colors["editor-background"], "#101020");

  std::fs::write(&theme, "[colors]\neditor-background = \"url(evil)\"\n").expect("theme");
  host.load();
  let status = &host.statuses()[0];
  assert_eq!(status.state, ExtensionState::Invalid);
  assert!(
    status
      .error
      .as_deref()
      .is_some_and(|e| e.contains("hex color"))
  );
}

/// Polls the host's statuses until one matches `wanted`.
fn wait_for_status(
  host: &ExtensionHost,
  what: &str,
  wanted: impl Fn(&ExtensionStatus) -> bool,
) -> ExtensionStatus {
  let deadline = Instant::now() + Duration::from_secs(30);
  loop {
    let statuses = host.statuses();
    if let Some(status) = statuses.iter().find(|status| wanted(status)) {
      return status.clone();
    }
    assert!(
      Instant::now() < deadline,
      "timed out waiting for {what}; got {statuses:#?}"
    );
    std::thread::sleep(Duration::from_millis(50));
  }
}

#[test]
fn development_builds_run_when_files_change_and_load_the_result() {
  let temp = tempfile::tempdir().expect("temp dir");
  let ext = temp.path().join("ext");
  std::fs::create_dir(&ext).expect("mkdir");
  // `rustc` stands in for a build: it's on the PATH wherever this runs.
  let manifest = |name: &str, args: &str| {
    format!(
      "id = \"test.built\"\nname = \"{name}\"\nversion = \"1.0.0\"\napi_version = \"1\"\n\
       icon = \"icon.svg\"\n\n[build]\ncommand = \"rustc\"\nargs = [{args}]\n"
    )
  };
  std::fs::write(
    ext.join("extension.toml"),
    manifest("Built", "\"--version\""),
  )
  .expect("write");
  let built = |status: &ExtensionStatus| {
    status
      .build
      .as_ref()
      .is_some_and(|build| build.state == BuildState::Succeeded)
  };

  let fake = Arc::new(Fake::default());
  let host = ExtensionHost::new(
    fake.clone(),
    HostOptions {
      editor_version: semver::Version::new(0, 1, 0),
      extensions_dir: None,
      development: vec![ext.clone()],
      builds: true,
      permissions_file: None,
      settings: BTreeMap::new(),
      programs: BTreeMap::new(),
      limits: limits(),
      storage_dir: None,
    },
  );
  host.load();
  // Invalid (its icon is missing), but built straight away all the same.
  let status = wait_for_status(&host, "the first build", built);
  assert_eq!(status.state, ExtensionState::Invalid);
  assert_eq!(
    status.build.as_ref().map(|b| b.log.as_str()),
    Some("test.built")
  );
  assert!(
    status
      .build
      .as_ref()
      .is_some_and(|b| b.duration_ms.is_some())
  );

  // A change builds it again, and loads it: it's valid now.
  std::fs::write(ext.join("icon.svg"), "<svg/>").expect("write");
  wait_for_status(&host, "the valid extension", |status| {
    status.state == ExtensionState::Ready && built(status)
  });

  // Loaded again in place after the manifest changes.
  std::fs::write(
    ext.join("extension.toml"),
    manifest("Renamed", "\"--version\""),
  )
  .expect("write");
  wait_for_status(&host, "the new name", |status| {
    status.name == "Renamed" && built(status)
  });

  // A new [build] is used for the next build; its failure is told once.
  std::fs::write(
    ext.join("extension.toml"),
    manifest("Renamed", "\"--no-such-flag\""),
  )
  .expect("write");
  let message = fake.wait_for_message("The build failed");
  assert!(message.contains("rustc exited with code"), "{message}");
  let status = wait_for_status(&host, "the failed build", |status| {
    status
      .build
      .as_ref()
      .is_some_and(|build| build.state == BuildState::Failed)
  });
  assert_eq!(
    status.state,
    ExtensionState::Ready,
    "the last good load stays"
  );

  assert!(host.build_development(&ext));
  assert!(!host.build_development(temp.path()));
  host.shutdown();
}

#[test]
fn development_builds_only_run_when_asked_for() {
  let temp = tempfile::tempdir().expect("temp dir");
  std::fs::write(
    temp.path().join("extension.toml"),
    "id = \"test.built\"\nname = \"Built\"\nversion = \"1.0.0\"\napi_version = \"1\"\n\n\
     [build]\ncommand = \"rustc\"\nargs = [\"--version\"]\n",
  )
  .expect("write");
  let host = ExtensionHost::new(
    Arc::new(Fake::default()),
    HostOptions {
      editor_version: semver::Version::new(0, 1, 0),
      extensions_dir: None,
      development: vec![temp.path().to_path_buf()],
      builds: false,
      permissions_file: None,
      settings: BTreeMap::new(),
      programs: BTreeMap::new(),
      limits: limits(),
      storage_dir: None,
    },
  );
  host.load();
  assert_eq!(host.statuses()[0].state, ExtensionState::Ready);
  assert_eq!(host.statuses()[0].build, None);
  assert!(!host.build_development(temp.path()));
}

#[test]
fn the_icons_example_contributes_an_icon_theme() {
  let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/icons");
  let fake = Arc::new(Fake::default());
  let options = HostOptions {
    editor_version: semver::Version::new(0, 1, 0),
    extensions_dir: None,
    development: vec![example],
    builds: false,
    permissions_file: None,
    settings: BTreeMap::new(),
    programs: BTreeMap::new(),
    limits: limits(),
    storage_dir: None,
  };
  let host = ExtensionHost::new(fake, options);
  host.load();
  let status = &host.statuses()[0];
  assert_eq!(status.state, ExtensionState::Ready, "{:?}", status.error);
  assert_eq!(
    status.contributions.icon_themes[0].id,
    "example.icons.colorful"
  );

  let theme = host
    .icon_theme("example.icons.colorful")
    .expect("an icon theme");
  assert_eq!(theme.associations.file_extensions["rs"], "rust");
  assert_eq!(theme.associations.folder_names["src"], "folder-src");
  assert_eq!(theme.light.folder.as_deref(), Some("folder-light"));
  assert!(theme.icons["rust"].starts_with("data:image/svg+xml;base64,"));
  assert!(host.icon_theme("example.icons.missing").is_none());

  host.set_enabled("example.icons", false);
  assert!(host.icon_theme("example.icons.colorful").is_none());
}

#[test]
fn language_extensions_load_and_declare_their_languages_and_servers() {
  let examples =
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/test-fixtures/languages");
  let folders = [
    "rust",
    "typescript",
    "javascript",
    "json",
    "toml",
    "python",
    "markdown",
  ];
  let options = HostOptions {
    editor_version: semver::Version::new(0, 1, 0),
    extensions_dir: None,
    development: folders.iter().map(|folder| examples.join(folder)).collect(),
    builds: false,
    permissions_file: None,
    settings: BTreeMap::new(),
    programs: BTreeMap::new(),
    limits: limits(),
    storage_dir: None,
  };
  let host = ExtensionHost::new(Arc::new(Fake::default()), options);
  host.load();
  for status in host.statuses() {
    assert_ne!(
      status.state,
      ExtensionState::Invalid,
      "{}: {:?}",
      status.name,
      status.error
    );
  }
  let mut languages: Vec<String> = host.languages().into_iter().map(|l| l.id).collect();
  languages.sort();
  assert_eq!(
    languages,
    [
      "javascript",
      "json",
      "markdown",
      "markdown_inline",
      "python",
      "rust",
      "toml",
      "typescript",
      "typescriptreact"
    ]
  );
  for language in host.languages() {
    assert!(language.grammar.is_file(), "{}", language.grammar.display());
  }
  // Servers wait for the user's permission.
  assert!(host.language_servers().is_empty());
  for id in [
    "test.rust",
    "test.typescript",
    "test.javascript",
    "test.python",
  ] {
    host.set_permissions(id, &[Permission::LanguageServers]);
  }
  let mut servers: Vec<String> = host
    .language_servers()
    .into_iter()
    .map(|s| s.name)
    .collect();
  servers.sort();
  assert_eq!(
    servers,
    [
      "pyright",
      "rust-analyzer",
      "typescript-language-server",
      "typescript-language-server"
    ],
    "JavaScript and TypeScript declare the same server, which settings merge"
  );
}

#[test]
fn a_language_extension_contributes_languages_and_servers_once_allowed() {
  let example =
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/test-fixtures/languages/html-css");
  let dir = tempfile::tempdir().expect("temp dir");
  let fake = Arc::new(Fake::default());
  let options = HostOptions {
    editor_version: semver::Version::new(0, 1, 0),
    extensions_dir: None,
    development: vec![example.clone()],
    builds: false,
    permissions_file: Some(dir.path().join("permissions.json")),
    settings: BTreeMap::new(),
    programs: BTreeMap::new(),
    limits: limits(),
    storage_dir: None,
  };
  let host = ExtensionHost::new(fake.clone(), options);
  host.load();
  let status = &host.statuses()[0];
  assert_eq!(status.id, "test.html-css");
  assert_eq!(
    status.state,
    ExtensionState::AwaitingPermission,
    "{:?}",
    status.error
  );
  let permission = &status.permissions[0];
  assert_eq!(permission.permission, Permission::LanguageServers);
  assert!(
    permission
      .description
      .contains("vscode-html-language-server --stdio; vscode-css-language-server --stdio"),
    "{}",
    permission.description
  );

  // Languages don't wait for the permission; servers do.
  let languages = host.languages();
  let ids: Vec<&str> = languages.iter().map(|l| l.id.as_str()).collect();
  assert_eq!(ids, ["html", "css", "svg"]);
  assert_eq!(
    languages[0].grammar,
    example.join("grammars/tree-sitter-html.wasm")
  );
  assert!(languages[0].grammar.is_file());
  assert_eq!(languages[2].grammar_name, "xml");
  assert!(languages[2].grammar.is_file());
  assert!(host.language_servers().is_empty());

  host.set_permissions("test.html-css", &[Permission::LanguageServers]);
  fake.wait_for("a state change", |n| {
    matches!(n, Notification::ExtensionsChanged)
  });
  assert_eq!(host.statuses()[0].state, ExtensionState::Ready);
  let servers = host.language_servers();
  assert_eq!(servers.len(), 2);
  assert_eq!(servers[0].command, "vscode-html-language-server");
  assert_eq!(servers[1].languages, ["css"]);

  host.set_permissions("test.html-css", &[]);
  assert!(host.language_servers().is_empty());
  assert_eq!(host.statuses()[0].state, ExtensionState::Ready, "decided");
  host.set_enabled("test.html-css", false);
  assert!(host.languages().is_empty());
}

#[test]
fn todos_example_highlights_lists_and_counts_markers() {
  let s = setup("todos", "todos", Some(ALL), "", |m| m);
  let id = s.fake.open(
    &s.host,
    "fn main() {\n  // TODO: tidy up\n}\n// FIXME later\n",
  );
  s.host.load();
  s.fake.wait_for("decorations", |n| {
    *n == Notification::DecorationsChanged(id)
  });
  s.fake.wait_for("view", |n| {
    *n == Notification::ViewChanged("example.todos.list".into())
  });
  let decorations = {
    let editor = s.fake.editor();
    let document = editor.document(id).expect("open");
    s.host.decorations(document, 0..document.buffer().len())
  };
  let keys: Vec<_> = decorations
    .iter()
    .map(|d| (d.range.clone(), d.key.as_str()))
    .collect();
  assert_eq!(
    keys,
    [
      (17..21, "example.todos/keyword"),
      (36..41, "example.todos/keyword")
    ]
  );
  let view = s.host.view_content("example.todos.list");
  assert_eq!(view.items.len(), 3, "{view:?}");
  assert_eq!(view.items[1].label, "TODO: tidy up");
  let reveal = view.items[1].argument.clone();
  s.fake
    .wait_for("count", |n| *n == Notification::StatusItemsChanged);
  assert_eq!(s.host.status_items()[0].content.text, "☐ 2 TODOs");

  // Selecting the item reveals the marker.
  s.host
    .run_command("example.todos.reveal", None, reveal)
    .expect("queued")
    .wait()
    .expect("revealed");
  let revealed = s
    .fake
    .wait_for("reveal", |n| matches!(n, Notification::Reveal { .. }));
  assert!(
    matches!(revealed, Notification::Reveal { start, .. } if start.line == 1 && start.column == 5)
  );
}

#[test]
fn previews_show_rendered_markdown_and_follow_their_documents() {
  let s = setup(
    "pages",
    "test-fixtures/pages",
    Some(&[Permission::DocumentsRead]),
    "",
    |m| m,
  );
  let docs = s._dir.path().join("docs");
  std::fs::create_dir_all(&docs).expect("mkdir");
  let path = docs.join("README.md");
  std::fs::write(
    &path,
    "# Hello\n\nSee [setup](setup.md#install) and ![logo](img/logo.png).\n",
  )
  .expect("write");
  let id = s.fake.open_file(&s.host, &path);
  s.host.load();
  let run = |command: &str, document| {
    s.host
      .run_command(&format!("test.pages.{command}"), document, None)
      .expect("queued")
      .wait()
  };
  run("previewToSide", Some(id)).expect("previewed");

  let preview = format!("test.pages.preview/{}", id.get());
  s.fake.wait_for("show", |n| {
    *n == Notification::ShowPreview {
      id: preview.clone(),
      beside: true,
    }
  });
  s.fake.wait_for("content", |n| {
    *n == Notification::PreviewChanged(preview.clone())
  });
  assert_eq!(
    s.host.previews(),
    [PreviewInfo {
      id: preview.clone(),
      extension: "Pages".into(),
      title: "Preview README.md".into(),
      icon: "book".into(),
      document: Some(id.get()),
      webview: false,
    }]
  );
  let content = s.host.preview_content(&preview).expect("open");
  let elements: Vec<_> = content.nodes.iter().map(|node| &node.element).collect();
  assert_eq!(*elements[0], PreviewElement::Heading { level: 1 });
  assert!(elements.contains(&&PreviewElement::Link {
    target: LinkTarget::File {
      path: docs.join("setup.md").to_string_lossy().into_owned(),
      anchor: Some("install".into()),
    },
    title: None,
  }));
  let logo = docs.join("img").join("logo.png");
  assert!(elements.contains(&&PreviewElement::Image {
    source: ImageSource::File {
      path: logo.to_string_lossy().into_owned(),
    },
    description: "logo".into(),
    title: None,
  }));
  assert!(s.host.preview_shows_image(&preview, &logo));
  assert!(
    !s.host
      .preview_shows_image(&preview, &docs.join("other.png"))
  );
  std::fs::create_dir_all(docs.join("img")).expect("mkdir");
  std::fs::write(&logo, b"\x89PNG\r\n").expect("write");
  assert_eq!(
    s.host.preview_image(&preview, &logo).as_deref(),
    Ok("data:image/png;base64,iVBORw0K")
  );
  std::fs::write(docs.join("other.png"), b"x").expect("write");
  assert!(
    s.host
      .preview_image(&preview, &docs.join("other.png"))
      .is_err()
  );

  // The preview follows the document.
  s.fake.type_text(&s.host, id, "Intro\n\n");
  s.fake.wait_for("updated content", |n| {
    *n == Notification::PreviewChanged(preview.clone())
  });
  let content = s.host.preview_content(&preview).expect("open");
  assert_eq!(content.nodes[0].element, PreviewElement::Paragraph);

  // Closing the tab closes the preview; the command opens it again, and
  // closing the document closes it for good.
  s.host.preview_closed(&preview);
  assert!(s.host.previews().is_empty());
  s.fake.type_text(&s.host, id, "More\n\n");
  run("preview", Some(id)).expect("previewed");
  s.fake.wait_for("show again", |n| {
    *n == Notification::ShowPreview {
      id: preview.clone(),
      beside: false,
    }
  });
  assert_eq!(s.host.previews().len(), 1);
  s.fake.editor().close(id).expect("closed");
  s.host.closed(id);
  s.fake
    .wait_for("closed", |n| *n == Notification::PreviewsChanged);
  assert!(s.host.previews().is_empty());
  assert!(s.host.preview_content(&preview).is_none());
}

/// A 3 × 2 PNG with alpha: the signature, the IHDR chunk, and the end.
const PNG_HEADER: &[u8] =
  b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x03\0\0\0\x02\x08\x06\0\0\0\0\0\0\0\0\0\0\0IEND";

/// The page's address, split into the token and the path after it.
fn webview_address(url: &str) -> (String, String) {
  let after = url
    .strip_prefix(&flareite_extension_host::webview::origin())
    .expect("Flareite's origin")
    .trim_start_matches('/');
  let (token, path) = after.split_once('/').expect("token/path");
  (token.to_owned(), path.to_owned())
}

#[test]
fn viewers_show_files_in_a_webview_that_extensions_answer() {
  let s = setup(
    "pages",
    "test-fixtures/pages",
    Some(&[Permission::DocumentsRead]),
    "",
    |m| m,
  );
  s.host.load();
  let status = &s.host.statuses()[0];
  assert_eq!(status.error, None);
  let viewers = &status.contributions.viewers;
  assert_eq!(viewers.len(), 1);
  assert_eq!(viewers[0].full_id, "test.pages.file");
  assert_eq!(viewers[0].patterns, ["*.png"]);
  assert_eq!(viewers[0].command, None, "the page loads the file");

  let pictures = s._dir.path().join("my pictures");
  std::fs::create_dir_all(&pictures).expect("mkdir");
  let path = pictures.join("dot #1.png");
  std::fs::write(&path, PNG_HEADER).expect("write");

  s.host
    .open_viewer("test.pages.file", &path)
    .expect("opens")
    .wait()
    .expect("nothing to run");
  let id = flareite_extension_host::viewer_id("test.pages.file", &path);
  assert!(s.host.previews().is_empty(), "viewers aren't previews");

  // The page, with the bridge, and the file it shows.
  let info = s.host.webview(&id).expect("a webview");
  let (token, page) = webview_address(&info.url);
  assert_eq!(page, "media/viewer.html");
  let html = s.host.webview_resource(&token, &page).expect("page");
  let html = String::from_utf8(html.body).expect("utf-8");
  assert!(html.contains("__flareite/webview.js"), "{html}");
  assert!(html.contains("viewer.js"));
  let script = s
    .host
    .webview_resource(&token, "media/viewer.js")
    .expect("the script");
  assert_eq!(script.mime, "text/javascript");
  let picture = s
    .host
    .webview_resource(&token, "__flareite/file")
    .expect("the file");
  assert_eq!(
    (picture.body.as_slice(), picture.mime),
    (PNG_HEADER, "image/png")
  );
  assert!(
    s.host
      .webview_resource(&token, "../extension.toml")
      .is_err()
  );

  // The page asks for the size; the extension answers it.
  s.host
    .webview_message(&id, r#"{"type":"size"}"#.into())
    .expect("sent");
  let answer = format!(r#"{{"size":{}}}"#, PNG_HEADER.len());
  s.fake.wait_for("the answer", |n| {
    *n == Notification::WebviewMessage {
      id: id.clone(),
      message: answer.clone(),
    }
  });

  // Closed: its page is gone, and messages can't reach the extension.
  s.host.preview_closed(&id);
  assert!(s.host.webview(&id).is_none());
  assert!(s.host.webview_resource(&token, &page).is_err());
  assert!(
    s.host
      .webview_message(&id, "{}".into())
      .expect_err("closed")
      .contains("isn't open")
  );
  let error = s
    .host
    .open_viewer("test.pages.missing", &path)
    .err()
    .expect("no such viewer");
  assert!(error.contains("No extension provides"), "{error}");
}

#[test]
fn a_viewer_keeps_its_tab_but_not_its_content_when_its_extension_stops() {
  let s = misbehaving("");
  s.host.load();
  let path = s._dir.path().join("a.bytes");
  std::fs::write(&path, PNG_HEADER).expect("write");
  s.host
    .open_viewer("test.misbehaving.bytes", &path)
    .expect("runs")
    .wait()
    .expect("fills it in");
  let id = flareite_extension_host::viewer_id("test.misbehaving.bytes", &path);
  let content = s.host.preview_content(&id).expect("open");
  assert!(content.nodes.iter().any(|node| matches!(
    &node.element,
    PreviewElement::Text { text } if *text == format!("{} bytes", PNG_HEADER.len())
  )));

  s.host.set_enabled("test.misbehaving", false);
  assert!(
    s.host
      .preview_content(&id)
      .expect("still open")
      .nodes
      .is_empty()
  );
  let error = s
    .host
    .open_viewer("test.misbehaving.bytes", &path)
    .err()
    .expect("disabled");
  assert!(error.contains("disabled"), "{error}");

  s.host.set_enabled("test.misbehaving", true);
  s.host
    .open_viewer("test.misbehaving.bytes", &path)
    .expect("runs")
    .wait()
    .expect("fills it in again");
  assert!(!s.host.preview_content(&id).expect("open").nodes.is_empty());

  // Restarted, it fills in the viewers it showed without being asked.
  s.host.restart("test.misbehaving");
  let deadline = Instant::now() + Duration::from_secs(30);
  while s.host.preview_content(&id).expect("open").nodes.is_empty() {
    assert!(
      Instant::now() < deadline,
      "the viewer wasn't filled in again"
    );
    std::thread::sleep(Duration::from_millis(20));
  }
}

fn programs(grant: &[Permission]) -> Setup {
  setup("programs", "test-fixtures/programs", Some(grant), "", |m| m)
}

/// Runs one of the programs fixture's commands with `argument`.
fn run_programs(s: &Setup, command: &str, argument: Option<String>) {
  s.host
    .run_command(&format!("test.programs.{command}"), None, argument)
    .expect("runs")
    .wait()
    .expect("succeeds");
}

/// A trusted open folder for the programs fixture.
fn open_folder(s: &Setup) -> PathBuf {
  let folder = flareite_workspace::normalize_path(s._dir.path());
  *s.fake.folder.lock().unwrap_or_else(PoisonError::into_inner) = Some(folder.clone());
  folder
}

#[test]
fn programs_run_with_permission_in_trusted_folders() {
  let s = programs(&[Permission::FilesRead, Permission::Processes]);
  open_folder(&s);
  s.host.load();

  run_programs(&s, "run", None);
  assert_eq!(s.fake.wait_for_message("run:"), "run: Some(0) rustc");
  run_programs(&s, "unknown", None);
  assert_eq!(
    s.fake.wait_for_message("unknown:"),
    "unknown: Some(ProcessError::UnknownProgram(\"sh\"))"
  );

  // Started in the background, its outcome runs a command.
  run_programs(&s, "spawn", None);
  assert_eq!(s.fake.wait_for_message("spawn:"), "spawn: true");
  assert_eq!(
    s.fake.wait_for_message("finished:"),
    "finished: Some(0) rustc"
  );

  // In a folder the user doesn't trust, nothing runs, and extensions hear
  // when trust changes.
  *s.fake
    .untrusted
    .lock()
    .unwrap_or_else(PoisonError::into_inner) = true;
  s.host.folder_changed();
  assert_eq!(s.fake.wait_for_message("folder:"), "folder: Some(false)");
  run_programs(&s, "run", None);
  assert_eq!(
    s.fake.wait_for_message("run:"),
    "run: ProcessError::Untrusted"
  );
}

#[test]
fn programs_need_the_processes_permission() {
  let s = programs(&[Permission::FilesRead]);
  open_folder(&s);
  s.host.load();
  run_programs(&s, "run", None);
  assert_eq!(
    s.fake.wait_for_message("run:"),
    "run: ProcessError::PermissionDenied"
  );

  // Nor can it watch outside the open folder.
  let outside = tempfile::tempdir().expect("temp dir");
  run_programs(&s, "watch", Some(outside.path().display().to_string()));
  assert_eq!(s.fake.wait_for_message("watch:"), "watch: failed");
}

#[test]
fn watched_files_report_their_changes() {
  let s = programs(&[Permission::FilesRead, Permission::Processes]);
  let folder = open_folder(&s);
  s.host.load();

  // Inside the open folder, the editor's own changes are used.
  run_programs(&s, "watch", Some(folder.display().to_string()));
  assert_eq!(s.fake.wait_for_message("watch:"), "watch: ok");
  let changed = folder.join("a.txt");
  s.host
    .files_changed(&[changed.clone(), PathBuf::from("/elsewhere/b.txt")]);
  assert_eq!(
    s.fake.wait_for_message("changed:"),
    format!("changed: {}", changed.display())
  );

  // Outside it, the path gets a watcher of its own.
  let outside = tempfile::tempdir().expect("temp dir");
  let outside = flareite_workspace::normalize_path(outside.path());
  run_programs(&s, "watch", Some(outside.display().to_string()));
  assert_eq!(s.fake.wait_for_message("watch:"), "watch: ok");
  std::fs::write(outside.join("c.txt"), "c").expect("write");
  let message = s.fake.wait_for_message("changed:");
  assert!(message.contains("c.txt"), "{message}");
}

#[test]
fn explorer_marks_are_checked_and_removed_with_the_extension() {
  let s = programs(&[Permission::FilesRead, Permission::Processes]);
  let folder = open_folder(&s);
  s.host.load();

  // Forward slashes are turned into the platform's form.
  let file = folder.join("src").join("main.rs");
  let given = file.display().to_string().replace('\\', "/");
  run_programs(&s, "mark", Some(given));
  s.fake
    .wait_for("marks", |n| *n == Notification::FileMarksChanged);
  assert_eq!(s.fake.wait_for_message("mark:"), "mark: Ok(())");
  let marks = s.host.file_marks();
  assert_eq!(marks.len(), 1);
  assert_eq!(marks[0].extension, "test.programs");
  let mark = &marks[0].marks[0];
  assert_eq!(mark.path, file.display().to_string());
  assert_eq!(mark.color.as_deref(), Some("test.programs.modified"));
  assert_eq!(mark.badge.as_deref(), Some("M"));

  run_programs(&s, "badMark", Some(file.display().to_string()));
  assert_eq!(s.fake.wait_for_message("badMark:"), "badMark: true");
  assert_eq!(s.host.file_marks()[0].marks.len(), 1, "unchanged");

  s.host.restart("test.programs");
  s.fake
    .wait_for("marks", |n| *n == Notification::FileMarksChanged);
  assert!(s.host.file_marks().is_empty());
}

#[test]
fn the_processes_permission_names_the_programs() {
  let s = programs(&[]);
  let status = &s.host.statuses()[0];
  let processes = status
    .permissions
    .iter()
    .find(|p| p.permission == Permission::Processes)
    .expect("requested");
  assert!(
    processes.description.ends_with(": rustc"),
    "{}",
    processes.description
  );
}

/// Waits for the fixture to be in `state`.
fn wait_for_state(s: &Setup, state: ExtensionState) {
  let deadline = Instant::now() + Duration::from_secs(30);
  while state_of(&s.host).0 != state {
    assert!(Instant::now() < deadline, "never {state:?}");
    std::thread::sleep(Duration::from_millis(20));
  }
}

#[test]
fn streamed_programs_hand_over_their_output_before_they_end() {
  let s = programs(&[Permission::FilesRead, Permission::Processes]);
  open_folder(&s);
  s.host.load();
  run_programs(&s, "stream", None);
  assert_eq!(s.fake.wait_for_message("stream:"), "stream: true");
  // `rustc --version --verbose`: every line, in order, then the end with
  // no output left in it.
  let message = s.fake.wait_for_message("streamed:");
  assert!(
    message.starts_with("streamed: Some(0) true rustc,binary:,commit-hash:"),
    "{message}"
  );
  assert!(message.contains(",host:,"), "{message}");
}

/// The programs fixture, starting for `*.marker` files in the open folder.
fn marked_programs() -> Setup {
  setup(
    "programs",
    "test-fixtures/programs",
    Some(&[Permission::FilesRead, Permission::Processes]),
    "",
    |manifest| {
      manifest.replace(
        r#"activation = ["on-startup"]"#,
        r#"activation = ["on-folder-contains:*.marker"]"#,
      )
    },
  )
}

#[test]
fn extensions_start_when_the_open_folder_has_their_files() {
  // Created later: a file deeper than the pattern reaches doesn't count.
  let s = marked_programs();
  let folder = open_folder(&s);
  s.host.load();
  assert_eq!(state_of(&s.host).0, ExtensionState::Inactive);
  std::fs::create_dir(folder.join("nested")).expect("folder");
  let nested = folder.join("nested").join("a.marker");
  std::fs::write(&nested, "").expect("file");
  s.host.files_changed(&[nested]);
  assert_eq!(state_of(&s.host).0, ExtensionState::Inactive);
  let marker = folder.join("b.marker");
  std::fs::write(&marker, "").expect("file");
  s.host.files_changed(&[marker]);
  wait_for_state(&s, ExtensionState::Active);

  // Already there when the extension loads.
  let s = marked_programs();
  let folder = open_folder(&s);
  std::fs::write(folder.join("c.marker"), "").expect("file");
  s.host.load();
  wait_for_state(&s, ExtensionState::Active);

  // In a folder opened later.
  let s = marked_programs();
  s.host.load();
  assert_eq!(state_of(&s.host).0, ExtensionState::Inactive);
  let folder = open_folder(&s);
  std::fs::write(folder.join("d.marker"), "").expect("file");
  s.host.folder_changed();
  wait_for_state(&s, ExtensionState::Active);
}

#[test]
fn folder_patterns_are_checked() {
  let s = setup("programs", "test-fixtures/programs", None, "", |manifest| {
    manifest.replace(
      r#"activation = ["on-startup"]"#,
      r#"activation = ["on-folder-contains:../*.marker"]"#,
    )
  });
  s.host.load();
  let (state, error) = state_of(&s.host);
  assert_eq!(state, ExtensionState::Invalid);
  assert!(error.unwrap_or_default().contains("on-folder-contains"));
}

#[test]
fn downloads_need_the_network_permission_and_hand_over_the_file() {
  let s = programs(&[Permission::FilesRead, Permission::Network]);
  let address = "https://example.com/data.json";
  s.fake
    .web
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .insert(address.into(), r#"{"a":1}"#.into());
  s.host.load();
  run_programs(&s, "download", Some(address.into()));
  assert_eq!(s.fake.wait_for_message("download:"), "download: Ok(())");
  assert_eq!(
    s.fake.wait_for_message("downloaded:"),
    r#"downloaded: https://example.com/data.json {"a":1}"#
  );
  run_programs(&s, "download", Some("https://example.com/missing".into()));
  assert_eq!(
    s.fake.wait_for_message("downloaded:"),
    "downloaded: https://example.com/missing failed: not found"
  );
  run_programs(&s, "download", Some("http://example.com/data.json".into()));
  let message = s.fake.wait_for_message("download:");
  assert!(message.contains("isn't an https:// address"), "{message}");

  let s = programs(&[Permission::FilesRead]);
  s.host.load();
  run_programs(&s, "download", Some(address.into()));
  let message = s.fake.wait_for_message("download:");
  assert!(message.contains("network permission"), "{message}");
}

#[test]
fn secrets_are_the_extensions_own_and_cleared_with_its_storage() {
  let s = programs(&[]);
  s.host.load();
  let secret = |argument: &str| {
    run_programs(&s, "secret", Some(argument.into()));
    s.fake.wait_for_message("secret:")
  };
  assert_eq!(secret("set:api-key=hunter2"), "secret: Ok(())");
  assert_eq!(secret("get:api-key"), r#"secret: Ok(Some("hunter2"))"#);
  assert_eq!(secret("get:other"), "secret: Ok(None)");
  assert_eq!(secret("keys"), "secret: api-key");
  let empty_key = secret("set:=x");
  assert!(
    empty_key.starts_with("secret: Err(SecretError::InvalidArgument"),
    "{empty_key}"
  );
  let stored = |account: &str| {
    s.fake
      .secrets
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get(account)
      .cloned()
  };
  assert_eq!(
    stored("test.programs/api-key").as_deref(),
    Some("hunter2"),
    "kept in the store under the extension's id"
  );
  // A value in the store that the extension never set isn't its.
  s.fake
    .secrets
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .insert("test.programs/planted".into(), "x".into());
  assert_eq!(secret("get:planted"), "secret: Ok(None)");

  s.host.clear_storage("test.programs").expect("cleared");
  assert_eq!(stored("test.programs/api-key"), None);
  assert_eq!(secret("keys"), "secret: ");
}

#[test]
fn password_prompts_hide_what_is_typed() {
  let s = programs(&[]);
  s.host.load();
  run_programs(&s, "password", None);
  // The prompt shows before the command's message.
  let Notification::Prompt(prompt) = s
    .fake
    .wait_for("prompt", |n| matches!(n, Notification::Prompt(_)))
  else {
    unreachable!()
  };
  assert_eq!(s.fake.wait_for_message("password:"), "password: true");
  assert!(
    matches!(prompt.kind, PromptKind::Input { password: true, .. }),
    "{prompt:?}"
  );
  s.host.prompt_answered(
    prompt.id,
    PromptReply::Text {
      text: "hunter2".into(),
    },
  );
  assert_eq!(
    s.fake.wait_for_message("answered:"),
    "answered: text of 7 characters"
  );
}
