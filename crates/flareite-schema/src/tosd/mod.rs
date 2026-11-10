//! TOML Schema 1.0 (`.tosd`, <https://tomlschema.org>): the reference
//! implementation's loader and validator (`schema`), and what completion and
//! hover need from a loaded schema (`nav`).

pub mod nav;
#[allow(clippy::all, clippy::unwrap_used, unused)]
pub mod schema;

pub use schema::{Diagnostic, DiagnosticPhase, DiagnosticSeverity, Schema, SchemaError};
