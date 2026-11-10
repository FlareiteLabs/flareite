//! Flareite desktop application.
//!
//! This crate owns process lifecycle and dependency wiring: it initializes
//! logging, registers the Tauri commands that form the frontend boundary, and
//! runs the window event loop. Editor behavior belongs in dedicated crates
//! (see `docs/architecture.md`), not here.

mod app_update;
mod backups;
mod bench;
mod commands;
mod dialogs;
mod downloads;
mod editor;
mod entries;
mod error;
mod events;
mod extension_install;
mod extensions;
mod file_drop;
mod languages;
mod logging;
mod lsp;
mod paths;
mod perf;
mod problems;
mod schemas;
mod secrets;
mod settings;
mod terminal;
mod trust;
mod ui_state;
mod web_pages;
mod windows;
mod workspace;

use std::time::Instant;

use tauri::{Manager, RunEvent, WindowEvent};

/// Builds and runs the application. Returns only if startup fails; the process
/// exits when the event loop ends.
pub fn run() {
  let launched_at = Instant::now();

  let app = tauri::Builder::default()
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_clipboard_manager::init())
    .plugin(tauri_plugin_updater::Builder::new().build())
    .manage(app_update::UpdateState::default())
    .manage(editor::EditorState::default())
    .manage(events::EventHub::default())
    .manage(workspace::WorkspaceState::default())
    .manage(perf::Perf::default())
    .manage(lsp::LspState::default())
    .manage(schemas::SchemaState::default())
    .manage(downloads::DownloadsState::default())
    .manage(extensions::ExtensionsState::default())
    .manage(extension_install::InstallState::default())
    .manage(languages::LanguagesState::default())
    .manage(terminal::TerminalState::default())
    .manage(file_drop::DropState::default())
    .manage(web_pages::WebPagesState::default())
    .manage(windows::WindowsState::default())
    // Extensions' webview pages, which run in sandboxed frames.
    .register_asynchronous_uri_scheme_protocol(
      flareite_extension_host::webview::SCHEME,
      |context, request, responder| {
        let app = context.app_handle().clone();
        // Files are read off the main thread.
        tauri::async_runtime::spawn_blocking(move || {
          responder.respond(extensions::serve_webview(&app, &request));
        });
      },
    )
    .setup(move |app| {
      let log_dir = match app.path().app_log_dir() {
        Ok(dir) => Some(dir),
        Err(error) => {
          eprintln!("Flareite: log directory unavailable, logging to stderr only: {error}");
          None
        }
      };
      app.manage(logging::init(log_dir.as_deref()));
      logging::install_panic_hook();
      app.manage(commands::StartupState::new(launched_at));

      tracing::info!(
        version = %app.package_info().version,
        release = app_update::release_tag(),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        log_dir = ?log_dir,
        elapsed_ms = commands::millis(launched_at.elapsed()),
        "backend initialized"
      );
      let settings = settings::SettingsState::load(app.path().app_config_dir().ok());
      app
        .state::<editor::EditorState>()
        .lock()
        .set_config(settings.get().editor_config());
      app.manage(settings);
      schemas::init(app.handle());
      downloads::init(app.handle());
      let saved = ui_state::UiStateStore::load(app.path().app_local_data_dir().ok());
      let previous_files = saved.get().open_files;
      app.manage(saved);

      let args: Vec<String> = std::env::args().skip(1).collect();
      let bench = bench::BenchState::from_args(&args, launched_at);
      let benchmarking = bench.active();
      // Benchmarks trust their folders and keep no decisions.
      app.manage(trust::TrustState::load(
        app.path().app_config_dir().ok().filter(|_| !benchmarking),
        benchmarking,
      ));
      let load_extensions =
        !benchmarking || bench.with_extensions() || !bench.languages().is_empty();
      app.manage(bench);
      if benchmarking {
        app.state::<ui_state::UiStateStore>().make_ephemeral();
      }
      // Benchmarks measure the editor alone unless asked otherwise.
      extensions::init(app.handle(), load_extensions);
      // Benchmark runs keep no backups and restore none.
      let backup_root = app
        .path()
        .app_local_data_dir()
        .ok()
        .filter(|_| !benchmarking)
        .map(|dir| dir.join("backups"));
      let crashed = backups::init(app.handle(), backup_root);

      // Benchmarks start from a clean session.
      let (previous_files, named) = if benchmarking {
        (Vec::new(), Vec::new())
      } else {
        workspace::restore(app.handle());
        let skip = bench::BenchState::consumed(&args);
        let named: Vec<std::path::PathBuf> = args
          .iter()
          .skip(skip)
          .filter(|arg| !arg.starts_with('-'))
          .map(std::path::PathBuf::from)
          .collect();
        (previous_files, named)
      };
      // A file named on the command line becomes the active tab.
      if let Some(last) = named.last() {
        let active = paths::normalize(last);
        app
          .state::<ui_state::UiStateStore>()
          .update(|state| state.active_file = Some(active));
      }
      editor::open_startup_files(
        app.handle(),
        previous_files.into_iter().chain(named).collect(),
        crashed,
      );
      // Created here rather than from the configuration, to open the
      // windows tabs are dragged into (`windows.rs`).
      windows::create_main(app)?;
      Ok(())
    })
    .on_window_event(|window, event| {
      if windows::on_window_event(window, event) {
        return;
      }
      match event {
        // Not `get_webview_window`: while a web page pane's view is in the
        // window, Tauri doesn't count it as a webview window.
        WindowEvent::CloseRequested { api, .. } if window.label() == windows::MAIN => {
          editor::on_close_requested(window, api);
        }
        WindowEvent::DragDrop(event) => {
          file_drop::on_drag_drop(window.app_handle(), window.label(), event);
        }
        _ => {}
      }
    })
    .invoke_handler(tauri::generate_handler![
      commands::app_info,
      commands::clipboard_write_text,
      commands::frontend_log,
      commands::frontend_ready,
      commands::open_external,
      commands::path_kinds,
      commands::open_folder_external,
      web_pages::web_page_show,
      web_pages::web_page_bounds,
      web_pages::web_page_hide,
      web_pages::web_page_close,
      windows::window_prepare,
      windows::window_close,
      windows::window_focus,
      windows::window_move,
      windows::windows_reset,
      windows::windows_set_zoom,
      windows::windows_geometry,
      extensions::extension_open_link,
      editor::document_list,
      editor::document_new,
      editor::document_open,
      editor::document_open_provided,
      editor::document_hide,
      editor::quick_diff_hunk,
      editor::quick_diff_revert,
      editor::quick_diff_apply,
      editor::document_pick_file,
      editor::document_encodings,
      editor::document_reopen_with_encoding,
      editor::document_save_with_encoding,
      editor::document_set_language,
      languages::languages_list,
      editor::document_save,
      editor::document_save_as,
      editor::document_close,
      editor::editor_view,
      editor::editor_minimap,
      editor::editor_peek_lines,
      editor::editor_document_lines,
      editor::editor_apply,
      editor::editor_clipboard,
      editor::editor_copy_view,
      editor::editor_close_view,
      events::events_subscribe,
      settings::settings_get,
      settings::settings_file,
      settings::settings_folder_file,
      schemas::installed::schemas_set_commands,
      settings::settings_set_theme,
      settings::settings_set_icon_theme,
      settings::settings_set_default_profile,
      settings::settings_schema,
      settings::settings_set,
      settings::settings_problem,
      logging::logs_read,
      logging::logs_file,
      problems::problems_list,
      ui_state::ui_state_get,
      ui_state::ui_state_save,
      workspace::workspace_get,
      workspace::workspace_open,
      workspace::workspace_open_dialog,
      workspace::workspace_close,
      trust::workspace_set_trust,
      workspace::workspace_list_dir,
      workspace::workspace_create,
      entries::workspace_rename,
      entries::workspace_move,
      entries::workspace_copy,
      entries::workspace_copy_dropped,
      entries::workspace_delete,
      entries::workspace_reveal,
      workspace::workspace_find_files,
      workspace::workspace_search,
      workspace::workspace_search_cancel,
      lsp::lsp_hover,
      lsp::lsp_definition,
      lsp::lsp_references,
      lsp::lsp_document_symbols,
      lsp::lsp_workspace_symbols,
      lsp::lsp_resolve_symbol,
      lsp::lsp_completion,
      terminal::terminal_create,
      terminal::terminal_platform,
      terminal::terminal_profiles,
      terminal::terminal_list,
      terminal::terminal_attach,
      terminal::terminal_write,
      terminal::terminal_resize,
      terminal::terminal_acknowledge,
      terminal::terminal_kill,
      extensions::extensions_list,
      extensions::extensions_reload,
      extensions::extension_run_command,
      extensions::extension_status_items,
      extensions::extension_file_marks,
      extensions::extension_active_file,
      extensions::extension_selection_changed,
      dialogs::dialog_pick_folder,
      extensions::extension_run_view_command,
      extensions::extension_run_code_lens,
      extensions::extension_view_expand,
      extensions::extension_view_input,
      extensions::extension_view_badges,
      extensions::extension_prompt_answer,
      extensions::extension_view,
      extensions::extensions_icon_theme,
      extensions::extension_view_opened,
      extensions::extension_previews,
      extensions::extension_preview_content,
      extensions::extension_preview_closed,
      extensions::extension_viewer_open,
      extensions::extension_preview_image,
      extensions::extension_webview,
      extensions::extension_layout,
      extensions::extension_pane_decorations,
      extensions::extension_layout_fill,
      extensions::extension_layout_ready,
      extensions::extension_layout_closed,
      extensions::extension_layout_focus,
      extensions::extension_layout_command,
      extensions::extension_webview_message,
      extensions::extension_webview_set_state,
      extensions::extension_webview_theme,
      extensions::extension_webview_storage,
      extensions::extension_clear_storage,
      extensions::extension_icon,
      extensions::extension_readme,
      extensions::extension_readme_image,
      extensions::extension_set_permissions,
      extensions::extension_set_enabled,
      extensions::extension_restart,
      extensions::extension_build,
      extensions::extension_add_development,
      extension_install::extension_releases,
      extension_install::extension_install,
      extension_install::extension_uninstall,
      extension_install::extension_catalog,
      extension_install::extension_sources,
      extension_install::extension_check_updates,
      app_update::app_update_status,
      app_update::app_update_check,
      app_update::app_update_download,
      app_update::app_update_restart,
      perf::perf_stats,
      bench::bench_config,
      bench::bench_generate,
      bench::bench_report,
    ])
    .build(tauri::generate_context!());

  let app = match app {
    Ok(app) => app,
    Err(error) => {
      tracing::error!(%error, "failed to start");
      eprintln!("Flareite failed to start: {error}");
      std::process::exit(1);
    }
  };

  app.run(|handle, event| match event {
    RunEvent::ExitRequested { api, .. } => editor::on_exit_requested(handle, &api),
    RunEvent::Exit => {
      tracing::info!("exiting");
      // Removes the backups of unsaved changes, or keeps them if the exit
      // skipped asking about them.
      backups::shutdown(handle);
      // Stop terminals, extensions, language servers, and background work
      // (searches, indexing) before the process ends.
      if let Some(terminals) = handle.try_state::<terminal::TerminalState>() {
        terminals.shutdown();
      }
      extensions::shutdown(handle);
      if let Some(lsp) = handle.try_state::<lsp::LspState>() {
        lsp.shutdown_all();
      }
      if let Some(perf) = handle.try_state::<perf::Perf>() {
        perf.scheduler.shutdown();
      }
      // Installs the update the user restarted for, then starts again.
      app_update::install_on_exit(handle);
      if let Some(guard) = handle.try_state::<logging::LogGuard>() {
        guard.flush();
      }
    }
    _ => {}
  });
}
