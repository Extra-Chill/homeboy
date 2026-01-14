//! Keychain storage for project variables.
//!
//! Provides secure storage for authentication tokens and other sensitive values.
//! Uses the system keychain (macOS Keychain, Linux Secret Service, Windows Credential Manager).

use crate::error::{Error, ErrorCode, Result};
use keyring::Entry;
use serde_json::Value;

const SERVICE_NAME: &str = "homeboy";

fn keyring_error(e: keyring::Error) -> Error {
    Error::new(
        ErrorCode::InternalUnexpected,
        format!("Keychain error: {}", e),
        Value::Null,
    )
}

/// Stores a value in the keychain for a project variable.
///
/// Key format: `<project-id>:<variable-name>`
pub fn store(project_id: &str, variable_name: &str, value: &str) -> Result<()> {
    let key = format!("{}:{}", project_id, variable_name);
    let entry = Entry::new(SERVICE_NAME, &key).map_err(keyring_error)?;
    entry.set_password(value).map_err(keyring_error)?;
    Ok(())
}

/// Retrieves a value from the keychain for a project variable.
///
/// Returns `None` if the key doesn't exist.
pub fn get(project_id: &str, variable_name: &str) -> Result<Option<String>> {
    let key = format!("{}:{}", project_id, variable_name);
    let entry = Entry::new(SERVICE_NAME, &key).map_err(keyring_error)?;

    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(keyring_error(e)),
    }
}

/// Deletes a value from the keychain.
pub fn delete(project_id: &str, variable_name: &str) -> Result<()> {
    let key = format!("{}:{}", project_id, variable_name);
    let entry = Entry::new(SERVICE_NAME, &key).map_err(keyring_error)?;

    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()), // Already deleted
        Err(e) => Err(keyring_error(e)),
    }
}

/// Checks if a keychain entry exists.
pub fn exists(project_id: &str, variable_name: &str) -> bool {
    get(project_id, variable_name)
        .map(|v| v.is_some())
        .unwrap_or(false)
}

/// Deletes all keychain entries for a project.
///
/// This deletes entries matching common variable names.
/// For complete cleanup, caller should specify known variable names.
pub fn clear_project(project_id: &str, variable_names: &[&str]) -> Result<()> {
    for name in variable_names {
        let _ = delete(project_id, name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require keychain access and may prompt for permissions
    // Run manually with: cargo test -p homeboy keychain -- --ignored

    #[test]
    #[ignore]
    fn test_store_and_get() {
        let project_id = "test-project";
        let var_name = "test_token";
        let value = "secret_value_123";

        store(project_id, var_name, value).unwrap();
        let retrieved = get(project_id, var_name).unwrap();
        assert_eq!(retrieved, Some(value.to_string()));

        delete(project_id, var_name).unwrap();
        let after_delete = get(project_id, var_name).unwrap();
        assert_eq!(after_delete, None);
    }
}
