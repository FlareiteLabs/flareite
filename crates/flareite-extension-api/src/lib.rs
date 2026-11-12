//! Writing Flareite extensions in Rust, for the extension API version 2.
//!
//! The interface is defined in `wit/` (one file per interface, and the
//! world in `world.wit`); this crate generates Rust bindings for it and adds
//! a small framework on top. An extension implements [`Extension`] for a
//! type that holds its state, which `activate` creates, and registers it
//! with [`register!`]; then it builds for `wasm32-unknown-unknown`:
//!
//! ```ignore
//! use flareite_extension_api::{Extension, host, register, types::*};
//!
//! struct Hello {
//!   greeted: u32,
//! }
//!
//! impl Extension for Hello {
//!   fn activate(_: Activation) -> Result<Self, String> {
//!     Ok(Self { greeted: 0 })
//!   }
//!
//!   fn run_command(&mut self, command: &str, _: CommandContext) -> Result<(), String> {
//!     match command {
//!       "example.hello.greet" => {
//!         self.greeted += 1;
//!         host::show_message(MessageLevel::Info, "Hello from an extension!");
//!         Ok(())
//!       }
//!       _ => Err(format!("unknown command {command}")),
//!     }
//!   }
//! }
//!
//! register!(Hello);
//! ```
//!
//! Everything but `activate` has a default: events are ignored, and
//! providers answer with nothing. See `docs/extensions.md` for the manifest,
//! permissions, and limits.

use std::cell::RefCell;
use std::thread::LocalKey;

/// The generated bindings. They live in a module because wit-bindgen's
/// exported macros would otherwise clash with their own re-exports at the
/// crate root.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub mod bindings {
  wit_bindgen::generate!({
    world: "extension",
    path: "wit",
    pub_export_macro: true,
    export_macro_name: "export_extension",
    default_bindings_module: "flareite_extension_api::bindings",
    additional_derives: [PartialEq],
  });
}

pub use bindings::flareite::extension::{
  documents, downloads, events, explorer, files, host, language_features, language_servers,
  layouts, links, processes, prompts, quick_diff, schemas, secrets, storage, timers, types,
  viewers, views, webviews, workbench, workspace,
};
pub use bindings::{Guest, export_extension};

use events::Event;
use language_features::{
  AutoInsertRequest, CodeAction, CodeActionRequest, CompletionItem, CompletionRequest,
  FormattingRequest, HoverRequest, Insertion,
};
use links::LinkRequest;
use quick_diff::Applied;
use types::{Activation, CommandContext, TextEdit};

/// The API version this crate implements, as written in `api_version` in
/// an extension's manifest.
pub const API_VERSION: &str = "2";

/// An extension: its state, and what it does when Flareite calls it. Calls
/// never overlap, so each gets the state mutably.
pub trait Extension: Sized + 'static {
  /// Starts the extension, before any other call: its state. An error fails
  /// the extension, and is shown to the user.
  fn activate(context: Activation) -> Result<Self, String>;

  /// Called before the extension is unloaded.
  fn deactivate(&mut self) {}

  /// Runs one of the commands its manifest declares, which the user ran. An
  /// error is shown to the user.
  fn run_command(&mut self, command: &str, context: CommandContext) -> Result<(), String> {
    let _ = context;
    Err(format!("{command} isn't one of this extension's commands"))
  }

  /// Something happened that the extension hears of: a document changed,
  /// a program it started ended, a prompt it showed was answered, ... (see
  /// [`events::Event`]). An error is logged.
  fn handle_event(&mut self, event: Event) -> Result<(), String> {
    let _ = event;
    Ok(())
  }

  /// The completions for a `[[completion_providers]]` language.
  fn provide_completions(
    &mut self,
    request: CompletionRequest,
  ) -> Result<Vec<CompletionItem>, String> {
    let _ = request;
    Ok(Vec::new())
  }

  /// Markdown describing a place in a `[[hover_providers]]` language.
  fn provide_hover(&mut self, request: HoverRequest) -> Result<Option<String>, String> {
    let _ = request;
    Ok(None)
  }

  /// Text to insert as an `[[auto_insert_providers]]` trigger is typed.
  fn provide_auto_insert(
    &mut self,
    request: AutoInsertRequest,
  ) -> Result<Option<Insertion>, String> {
    let _ = request;
    Ok(None)
  }

  /// Edits formatting a document of a `[[formatting_providers]]` language.
  fn provide_formatting(&mut self, request: FormattingRequest) -> Result<Vec<TextEdit>, String> {
    let _ = request;
    Ok(Vec::new())
  }

  /// Code actions for a `[[code_action_providers]]` language.
  fn provide_code_actions(
    &mut self,
    request: CodeActionRequest,
  ) -> Result<Vec<CodeAction>, String> {
    let _ = request;
    Ok(Vec::new())
  }

  /// The text at an address with one of its `[[text_providers]]`' schemes.
  fn provide_text(&mut self, uri: &str) -> Result<Option<String>, String> {
    let _ = uri;
    Ok(None)
  }

  /// The address of a file's original, for its `[quick_diff]`.
  fn provide_original(&mut self, path: &str) -> Result<Option<String>, String> {
    let _ = path;
    Ok(None)
  }

  /// Makes an original what `applied` says (`[quick_diff]`'s `apply_title`).
  fn change_original(&mut self, applied: Applied) -> Result<(), String> {
    Err(format!("{} can't be changed", applied.uri))
  }

  /// Opens a link for one of its `[[link_handlers]]`; `false` hands it back.
  fn open_link(&mut self, request: LinkRequest) -> Result<bool, String> {
    let _ = request;
    Ok(false)
  }
}

impl types::Permission {
  /// What it allows, for messages: "read documents", "run programs".
  pub fn describe(self) -> &'static str {
    match self {
      Self::DocumentsRead => "read documents",
      Self::DocumentsWrite => "edit documents",
      Self::FilesRead => "read the folder's files",
      Self::FilesWrite => "change the folder's files",
      Self::LanguageServers => "run language servers",
      Self::Network => "download files",
      Self::Processes => "run programs",
      Self::WebPages => "show web pages",
    }
  }
}

/// Where [`register!`] keeps the extension's state.
#[doc(hidden)]
pub type Slot<E> = RefCell<Option<E>>;

/// Runs `f` on the extension's state; fails if it isn't active.
#[doc(hidden)]
pub fn with_extension<E: Extension, R>(
  slot: &'static LocalKey<Slot<E>>,
  f: impl FnOnce(&mut E) -> Result<R, String>,
) -> Result<R, String> {
  slot.with(|slot| match slot.borrow_mut().as_mut() {
    Some(extension) => f(extension),
    None => Err("the extension isn't active".into()),
  })
}

/// Registers a type implementing [`Extension`] as the extension's code:
/// exports the world's functions, which create its state in `activate` and
/// call its methods with it.
#[macro_export]
macro_rules! register {
  ($extension:ty) => {
    ::std::thread_local! {
      static __FLAREITE_EXTENSION: $crate::Slot<$extension> =
        const { ::std::cell::RefCell::new(::core::option::Option::None) };
    }

    #[doc(hidden)]
    pub struct __FlareiteExtension;

    impl $crate::Guest for __FlareiteExtension {
      fn activate(
        context: $crate::types::Activation,
      ) -> ::core::result::Result<(), ::std::string::String> {
        let state = <$extension as $crate::Extension>::activate(context)?;
        __FLAREITE_EXTENSION.with(|slot| *slot.borrow_mut() = ::core::option::Option::Some(state));
        ::core::result::Result::Ok(())
      }

      fn deactivate() {
        let state = __FLAREITE_EXTENSION.with(|slot| slot.borrow_mut().take());
        if let ::core::option::Option::Some(mut state) = state {
          $crate::Extension::deactivate(&mut state);
        }
      }

      fn run_command(
        command: ::std::string::String,
        context: $crate::types::CommandContext,
      ) -> ::core::result::Result<(), ::std::string::String> {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::run_command(extension, &command, context)
        })
      }

      fn handle_event(
        event: $crate::events::Event,
      ) -> ::core::result::Result<(), ::std::string::String> {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::handle_event(extension, event)
        })
      }

      fn provide_completions(
        request: $crate::language_features::CompletionRequest,
      ) -> ::core::result::Result<
        ::std::vec::Vec<$crate::language_features::CompletionItem>,
        ::std::string::String,
      > {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::provide_completions(extension, request)
        })
      }

      fn provide_hover(
        request: $crate::language_features::HoverRequest,
      ) -> ::core::result::Result<
        ::core::option::Option<::std::string::String>,
        ::std::string::String,
      > {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::provide_hover(extension, request)
        })
      }

      fn provide_auto_insert(
        request: $crate::language_features::AutoInsertRequest,
      ) -> ::core::result::Result<
        ::core::option::Option<$crate::language_features::Insertion>,
        ::std::string::String,
      > {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::provide_auto_insert(extension, request)
        })
      }

      fn provide_formatting(
        request: $crate::language_features::FormattingRequest,
      ) -> ::core::result::Result<::std::vec::Vec<$crate::types::TextEdit>, ::std::string::String>
      {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::provide_formatting(extension, request)
        })
      }

      fn provide_code_actions(
        request: $crate::language_features::CodeActionRequest,
      ) -> ::core::result::Result<
        ::std::vec::Vec<$crate::language_features::CodeAction>,
        ::std::string::String,
      > {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::provide_code_actions(extension, request)
        })
      }

      fn provide_text(
        uri: ::std::string::String,
      ) -> ::core::result::Result<
        ::core::option::Option<::std::string::String>,
        ::std::string::String,
      > {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::provide_text(extension, &uri)
        })
      }

      fn provide_original(
        path: ::std::string::String,
      ) -> ::core::result::Result<
        ::core::option::Option<::std::string::String>,
        ::std::string::String,
      > {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::provide_original(extension, &path)
        })
      }

      fn change_original(
        applied: $crate::quick_diff::Applied,
      ) -> ::core::result::Result<(), ::std::string::String> {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::change_original(extension, applied)
        })
      }

      fn open_link(
        request: $crate::links::LinkRequest,
      ) -> ::core::result::Result<bool, ::std::string::String> {
        $crate::with_extension(&__FLAREITE_EXTENSION, |extension| {
          $crate::Extension::open_link(extension, request)
        })
      }
    }

    $crate::export_extension!(__FlareiteExtension);
  };
}
