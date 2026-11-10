//! Bindings for writing Flareite extensions in Rust.
//!
//! The interface is defined in `wit/extension.wit`; this crate generates
//! Rust bindings for it. An extension implements [`Guest`] and exports it
//! with [`export_extension!`], then builds for `wasm32-unknown-unknown`:
//!
//! ```ignore
//! use flareite_extension_api::{Guest, export_extension, host, types::*};
//!
//! struct Hello;
//!
//! impl Guest for Hello {
//!   fn activate(_: Activation) -> Result<(), String> {
//!     Ok(())
//!   }
//!   fn deactivate() {}
//!   fn run_command(_: String, _: CommandContext) -> Result<(), String> {
//!     host::show_message(MessageLevel::Info, "Hello from an extension!");
//!     Ok(())
//!   }
//!   fn document_opened(_: DocumentInfo) {}
//!   fn document_changed(_: DocumentChange) {}
//!   fn document_closed(_: DocumentId) {}
//! }
//!
//! export_extension!(Hello);
//! ```
//!
//! See `docs/extensions.md` for the manifest, permissions, and limits.

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
  documents, explorer, files, host, layouts, links, processes, prompts, quick_diff, schemas,
  storage, timers, types, viewers, views, webviews, workbench, workspace,
};
pub use bindings::{Guest, export_extension};

/// The API version this crate implements, as written in `api_version` in
/// an extension's manifest.
pub const API_VERSION: &str = "1";
