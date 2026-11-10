//! Schemas extensions provide for TOML and JSON files (API 1.16): those
//! their manifests declare (`[[schemas]]`), and those their code sets with
//! the `schemas` interface, which last until the extension stops. The
//! editor checks files against them, and completes and describes their
//! keys; see [`crate::ExtensionHost::schemas`].

use std::path::PathBuf;
use std::sync::Arc;

use crate::bindings::flareite::extension::schemas::{
  Host, Schema as WitSchema, SchemaFormat as WitFormat,
};
use crate::bindings::flareite::extension::types::ApiError;
use crate::contributions::{MAX_SCHEMA_BYTES, MAX_SCHEMAS, check_schema_files};
use crate::runtime::StoreState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaFormat {
  /// TOML Schema (`.tosd`).
  TomlSchema,
  JsonSchema,
}

/// A schema an extension's code set.
#[derive(Debug, Clone)]
pub struct RuntimeSchema {
  pub id: String,
  pub files: Vec<String>,
  pub format: SchemaFormat,
  pub content: Arc<str>,
}

/// A schema an extension provides.
#[derive(Debug, Clone)]
pub struct ProvidedSchema {
  /// The extension's id.
  pub extension: String,
  /// The files it's for: name patterns, or ends of paths with '/'.
  pub files: Vec<String>,
  pub source: Provided,
}

#[derive(Debug, Clone)]
pub enum Provided {
  /// A file in the extension's folder (absolute).
  File(PathBuf),
  /// An address on the web.
  Url(String),
  /// What the extension's code set, with its id.
  Content {
    id: String,
    format: SchemaFormat,
    text: Arc<str>,
  },
}

/// Whether `id` can name a schema.
fn valid_id(id: &str) -> bool {
  (1..=64).contains(&id.len())
    && id
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Checks a schema an extension sets: its id, files, size, and that it
/// loads.
fn check(schema: WitSchema) -> Result<RuntimeSchema, ApiError> {
  if !valid_id(&schema.id) {
    return Err(ApiError::InvalidArgument(format!(
      "schema id {:?} must be 1 to 64 letters, digits, '-', '_', or '.'",
      schema.id
    )));
  }
  check_schema_files(&schema.files).map_err(ApiError::InvalidArgument)?;
  if schema.content.len() as u64 > MAX_SCHEMA_BYTES {
    return Err(ApiError::TooLarge(format!(
      "a schema is at most {} MiB",
      MAX_SCHEMA_BYTES / 1024 / 1024
    )));
  }
  let format = match schema.format {
    WitFormat::TomlSchema => {
      flareite_schema::Schema::tosd(&schema.id, &schema.content).map_err(|error| {
        ApiError::InvalidArgument(format!("the TOML Schema can't be loaded: {error}"))
      })?;
      SchemaFormat::TomlSchema
    }
    WitFormat::JsonSchema => {
      let value: serde_json::Value = serde_json::from_str(&schema.content).map_err(|error| {
        ApiError::InvalidArgument(format!("the JSON Schema isn't JSON: {error}"))
      })?;
      if !(value.is_object() || value.is_boolean()) {
        return Err(ApiError::InvalidArgument(
          "a JSON Schema is an object or a boolean".into(),
        ));
      }
      SchemaFormat::JsonSchema
    }
  };
  Ok(RuntimeSchema {
    id: schema.id,
    files: schema.files,
    format,
    content: schema.content.into(),
  })
}

impl Host for StoreState {
  fn set_schema(&mut self, schema: WitSchema) -> Result<(), ApiError> {
    let schema = check(schema)?;
    let declared = self.api.manifest().contributions.schemas.len();
    let shared = self.api.shared()?;
    if shared.set_schema(
      self.api.extension_id(),
      schema,
      MAX_SCHEMAS.saturating_sub(declared),
    ) {
      Ok(())
    } else {
      Err(ApiError::TooLarge(format!(
        "an extension provides at most {MAX_SCHEMAS} schemas"
      )))
    }
  }

  fn remove_schema(&mut self, id: String) -> bool {
    self
      .api
      .shared()
      .is_ok_and(|shared| shared.remove_schema(self.api.extension_id(), &id))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn schema(id: &str, format: WitFormat, content: &str) -> WitSchema {
    WitSchema {
      id: id.into(),
      files: vec!["*.conf.toml".into()],
      format,
      content: content.into(),
    }
  }

  #[test]
  fn schemas_are_checked_when_set() {
    let tosd = "[toml-schema]\nversion = \"1.0.0\"\n[elements.a]\ntype = \"string\"\n";
    assert!(check(schema("conf", WitFormat::TomlSchema, tosd)).is_ok());
    assert!(
      check(schema(
        "conf",
        WitFormat::JsonSchema,
        "{\"type\": \"object\"}"
      ))
      .is_ok()
    );
    for (id, format, content) in [
      ("", WitFormat::JsonSchema, "{}"),
      ("a/b", WitFormat::JsonSchema, "{}"),
      ("conf", WitFormat::JsonSchema, "[1]"),
      ("conf", WitFormat::JsonSchema, "{"),
      (
        "conf",
        WitFormat::TomlSchema,
        "[elements.a]\ntype = \"string\"\n",
      ),
    ] {
      assert!(
        matches!(
          check(schema(id, format, content)),
          Err(ApiError::InvalidArgument(_))
        ),
        "{id} {content}"
      );
    }
    let mut unmatched = schema("conf", WitFormat::JsonSchema, "{}");
    unmatched.files.clear();
    assert!(check(unmatched).is_err());
  }
}
