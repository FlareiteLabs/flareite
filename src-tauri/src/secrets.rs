//! The system's credential store, which keeps extensions' secrets (API
//! 1.17): Windows Credential Manager, the macOS Keychain, or the Secret
//! Service on Linux (GNOME Keyring, KWallet). See the extension host's
//! `secrets` module, which names each secret's account after its extension
//! and key.
//!
//! Entries belong to the service named by the app's identifier, so a
//! development build's are apart from the installed app's.

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
mod store {
  fn entry(service: &str, account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, account).map_err(|error| error.to_string())
  }

  pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
    match entry(service, account)?.get_password() {
      Ok(value) => Ok(Some(value)),
      Err(keyring::Error::NoEntry) => Ok(None),
      Err(error) => Err(error.to_string()),
    }
  }

  pub fn set(service: &str, account: &str, value: Option<&str>) -> Result<(), String> {
    let entry = entry(service, account)?;
    let result = match value {
      Some(value) => entry.set_password(value),
      None => match entry.delete_credential() {
        Err(keyring::Error::NoEntry) => Ok(()),
        other => other,
      },
    };
    result.map_err(|error| error.to_string())
  }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod store {
  const UNSUPPORTED: &str = "there's no credential store on this system";

  pub fn get(_: &str, _: &str) -> Result<Option<String>, String> {
    Err(UNSUPPORTED.into())
  }

  pub fn set(_: &str, _: &str, _: Option<&str>) -> Result<(), String> {
    Err(UNSUPPORTED.into())
  }
}

/// A secret, or `None` if the store has none for `account`.
pub fn get(service: &str, account: &str) -> Result<Option<String>, String> {
  store::get(service, account)
}

/// Sets a secret, or removes it (`None`; removing one that isn't there
/// succeeds).
pub fn set(service: &str, account: &str, value: Option<&str>) -> Result<(), String> {
  store::set(service, account, value)
}

#[cfg(test)]
mod tests {
  /// Uses the real store, under a service of its own, and cleans up.
  #[test]
  #[cfg(windows)]
  fn keeps_and_removes_secrets_in_the_system_store() {
    let service = "dev.flareite.editor.tests";
    let account = format!("test.secrets/{}", std::process::id());
    assert_eq!(super::get(service, &account), Ok(None));
    super::set(service, &account, Some("hunter2")).expect("set");
    assert_eq!(super::get(service, &account), Ok(Some("hunter2".into())));
    super::set(service, &account, None).expect("removed");
    assert_eq!(super::get(service, &account), Ok(None));
    super::set(service, &account, None).expect("removing again succeeds");
  }
}
