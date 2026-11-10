//! The host functions of the `prompts` interface (API 1.7): asking the user
//! to choose from a list, type a line of text, or confirm.
//!
//! The workbench shows one prompt at a time. A prompt returns its id at
//! once; the user's answer (or its cancellation, when the user closes it or
//! another prompt replaces it) runs the command the extension named, during
//! which `answer` returns it.

use serde::{Deserialize, Serialize};

use crate::bindings::flareite::extension::prompts::{
  AnswerValue, ConfirmOptions, Host, InputOptions, PickItem as WitPickItem, PromptAnswer,
};
use crate::bindings::flareite::extension::types::ApiError;
use crate::runtime::StoreState;
use crate::worker::Payload;

/// Most items a list to choose from has.
pub const MAX_PICK_ITEMS: usize = 1_000;
const MAX_BUTTONS: usize = 4;

/// A question for the user, as the workbench shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
  pub id: u64,
  /// The name of the extension asking.
  pub extension: String,
  #[serde(flatten)]
  pub kind: PromptKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
  tag = "type",
  rename_all = "camelCase",
  rename_all_fields = "camelCase"
)]
pub enum PromptKind {
  Pick {
    title: String,
    items: Vec<PickItem>,
  },
  Input {
    title: String,
    value: String,
    placeholder: Option<String>,
    /// What's typed is hidden, as for a password.
    password: bool,
  },
  Confirm {
    message: String,
    detail: Option<String>,
    buttons: Vec<String>,
    destructive: bool,
  },
  /// A folder, in the system's dialog; answered with its path as text.
  Folder {
    title: String,
  },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickItem {
  pub label: String,
  pub description: Option<String>,
  pub value: String,
}

/// The user's answer, as the workbench sends it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
  tag = "type",
  rename_all = "camelCase",
  rename_all_fields = "camelCase"
)]
pub enum PromptReply {
  Cancelled,
  Picked { value: String },
  Text { text: String },
  Button { index: u32 },
}

impl PromptReply {
  pub(crate) fn into_answer(self, prompt: u64) -> PromptAnswer {
    PromptAnswer {
      prompt,
      value: match self {
        Self::Cancelled => AnswerValue::Cancelled,
        Self::Picked { value } => AnswerValue::Picked(value),
        Self::Text { text } => AnswerValue::Text(text),
        Self::Button { index } => AnswerValue::Button(index),
      },
    }
  }
}

fn text(field: &str, value: &str, max: usize) -> Result<String, ApiError> {
  let value = value.trim();
  if value.is_empty() || value.chars().count() > max {
    Err(ApiError::InvalidArgument(format!(
      "{field} must be 1 to {max} characters"
    )))
  } else {
    Ok(value.to_owned())
  }
}

fn optional(field: &str, value: Option<String>, max: usize) -> Result<Option<String>, ApiError> {
  value
    .filter(|value| !value.trim().is_empty())
    .map(|value| text(field, &value, max))
    .transpose()
}

impl StoreState {
  fn show_prompt(&mut self, command: String, kind: PromptKind) -> Result<u64, ApiError> {
    let api = &self.api;
    if !api.manifest().declares_command(&command) {
      return Err(ApiError::InvalidArgument(format!(
        "{command:?} is not one of this extension's commands"
      )));
    }
    let shared = api.shared()?;
    Ok(shared.show_prompt(api.extension_id(), api.generation(), command, kind))
  }

  /// Asks for a line of text, hidden as it's typed with `password` (1.17).
  fn input_prompt(
    &mut self,
    options: InputOptions,
    command: String,
    password: bool,
  ) -> Result<u64, ApiError> {
    if options.value.len() > 64 * 1024 {
      return Err(ApiError::TooLarge("the text is at most 64 KiB".into()));
    }
    let kind = PromptKind::Input {
      title: text("title", &options.title, 200)?,
      value: options.value,
      placeholder: optional("placeholder", options.placeholder, 200)?,
      password,
    };
    self.show_prompt(command, kind)
  }
}

impl Host for StoreState {
  fn pick(
    &mut self,
    title: String,
    items: Vec<WitPickItem>,
    command: String,
  ) -> Result<u64, ApiError> {
    if items.len() > MAX_PICK_ITEMS {
      return Err(ApiError::TooLarge(format!(
        "at most {MAX_PICK_ITEMS} items can be offered"
      )));
    }
    let items = items
      .into_iter()
      .map(|item| {
        Ok(PickItem {
          label: text("an item's label", &item.label, 200)?,
          description: optional("an item's description", item.description, 200)?,
          value: item.value,
        })
      })
      .collect::<Result<Vec<_>, ApiError>>()?;
    let kind = PromptKind::Pick {
      title: text("title", &title, 200)?,
      items,
    };
    self.show_prompt(command, kind)
  }

  fn input(&mut self, options: InputOptions, command: String) -> Result<u64, ApiError> {
    self.input_prompt(options, command, false)
  }

  fn input_password(&mut self, options: InputOptions, command: String) -> Result<u64, ApiError> {
    self.input_prompt(options, command, true)
  }

  fn pick_folder(&mut self, title: String, command: String) -> Result<u64, ApiError> {
    let kind = PromptKind::Folder {
      title: text("title", &title, 200)?,
    };
    self.show_prompt(command, kind)
  }

  fn confirm(&mut self, options: ConfirmOptions, command: String) -> Result<u64, ApiError> {
    if options.buttons.is_empty() || options.buttons.len() > MAX_BUTTONS {
      return Err(ApiError::InvalidArgument(format!(
        "a confirmation needs 1 to {MAX_BUTTONS} buttons"
      )));
    }
    let kind = PromptKind::Confirm {
      message: text("message", &options.message, 500)?,
      detail: optional("detail", options.detail, 2_000)?,
      buttons: options
        .buttons
        .iter()
        .map(|button| text("a button", button, 40))
        .collect::<Result<_, _>>()?,
      destructive: options.destructive,
    };
    self.show_prompt(command, kind)
  }

  fn answer(&mut self) -> Option<PromptAnswer> {
    match &self.api.payload {
      Payload::Answer(answer) => Some(answer.clone()),
      _ => None,
    }
  }
}
