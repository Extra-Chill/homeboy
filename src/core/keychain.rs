//! OS keychain storage for project API variables.
//!
//! Values are stored under service `homeboy` with account
//! `<project-id>:<variable-name>`.

use crate::core::error::{Error, ErrorCode, Result};
use keyring::Entry;
use serde_json::{json, Value};

const SERVICE_NAME: &str = "homeboy";

fn keyring_error(e: keyring::Error) -> Error {
    Error::new(
        ErrorCode::InternalUnexpected,
        format!("Keychain error: {}", e),
        json!({ "error": e.to_string() }),
    )
    .with_hint("Use source: \"env\" for CI/headless environments, or unlock/configure the OS keychain for local use")
}

fn account_key(project_id: &str, variable_name: &str) -> String {
    format!("{}:{}", project_id, variable_name)
}

fn entry(project_id: &str, variable_name: &str) -> Result<Entry> {
    Entry::new(SERVICE_NAME, &account_key(project_id, variable_name)).map_err(keyring_error)
}

/// Stores a project API variable in the OS keychain.
pub fn set(project_id: &str, variable_name: &str, value: &str) -> Result<()> {
    entry(project_id, variable_name)?
        .set_password(value)
        .map_err(keyring_error)
}

/// Retrieves a project API variable from the OS keychain.
pub fn get(project_id: &str, variable_name: &str) -> Result<Option<String>> {
    match entry(project_id, variable_name)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(keyring_error(e)),
    }
}

/// Removes a project API variable from the OS keychain.
pub fn remove(project_id: &str, variable_name: &str) -> Result<()> {
    match entry(project_id, variable_name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(keyring_error(e)),
    }
}

/// Checks whether a project API variable is present in the OS keychain.
pub fn exists(project_id: &str, variable_name: &str) -> bool {
    get(project_id, variable_name)
        .map(|value| value.is_some())
        .unwrap_or(false)
}

/// Removes the named project API variables from the OS keychain.
pub fn remove_many(project_id: &str, variable_names: &[String]) -> Result<usize> {
    let mut removed = 0;
    for variable_name in variable_names {
        if get(project_id, variable_name)?.is_some() {
            remove(project_id, variable_name)?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn missing_error(project_id: &str, variable_name: &str) -> Error {
    Error::new(
        ErrorCode::ExtensionNotFound,
        format!(
            "Keychain variable '{}' is not set for project '{}'",
            variable_name, project_id
        ),
        Value::Null,
    )
    .with_hint(format!(
        "Run 'homeboy auth set --project {} {}' to store it locally",
        project_id, variable_name
    ))
    .with_hint("Use source: \"env\" instead for CI/headless environments")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_key_uses_project_and_variable() {
        assert_eq!(account_key("wpcloud-api", "token"), "wpcloud-api:token");
    }

    #[test]
    fn test_missing_error() {
        let err = missing_error("wpcloud-api", "token");

        assert!(err.message.contains("token"));
        assert!(err.message.contains("wpcloud-api"));
        assert_eq!(err.hints.len(), 2);
    }

    #[test]
    #[ignore]
    fn test_set() {
        let project_id = "homeboy-keychain-test-set";
        let variable_name = "token";

        remove(project_id, variable_name).expect("clean old value");
        set(project_id, variable_name, "secret-value").expect("store value");
        assert_eq!(
            get(project_id, variable_name)
                .expect("read value")
                .as_deref(),
            Some("secret-value")
        );
        remove(project_id, variable_name).expect("cleanup value");
    }

    #[test]
    #[ignore]
    fn test_get() {
        let project_id = "homeboy-keychain-test-get";
        let variable_name = "token";

        remove(project_id, variable_name).expect("clean old value");
        assert_eq!(
            get(project_id, variable_name).expect("read missing value"),
            None
        );
        set(project_id, variable_name, "secret-value").expect("store value");
        assert_eq!(
            get(project_id, variable_name)
                .expect("read value")
                .as_deref(),
            Some("secret-value")
        );
        remove(project_id, variable_name).expect("cleanup value");
    }

    #[test]
    #[ignore]
    fn test_remove() {
        let project_id = "homeboy-keychain-test-remove";
        let variable_name = "token";

        set(project_id, variable_name, "secret-value").expect("store value");
        remove(project_id, variable_name).expect("remove value");
        assert_eq!(
            get(project_id, variable_name).expect("read missing value"),
            None
        );
    }

    #[test]
    #[ignore]
    fn test_exists() {
        let project_id = "homeboy-keychain-test-exists";
        let variable_name = "token";

        remove(project_id, variable_name).expect("clean old value");
        assert!(!exists(project_id, variable_name));
        set(project_id, variable_name, "secret-value").expect("store value");
        assert!(exists(project_id, variable_name));
        remove(project_id, variable_name).expect("cleanup value");
    }

    #[test]
    #[ignore]
    fn test_remove_many() {
        let project_id = "homeboy-keychain-test-remove-many";
        let variables = vec!["token".to_string(), "refresh".to_string()];

        set(project_id, "token", "secret-value").expect("store token");
        set(project_id, "refresh", "refresh-value").expect("store refresh");
        assert_eq!(
            remove_many(project_id, &variables).expect("remove values"),
            2
        );
        assert!(!exists(project_id, "token"));
        assert!(!exists(project_id, "refresh"));
    }

    #[test]
    #[ignore]
    fn stores_reads_and_removes_keychain_value() {
        let project_id = "homeboy-keychain-test";
        let variable_name = "token";
        let value = "secret-value";

        set(project_id, variable_name, value).expect("store value");
        assert_eq!(
            get(project_id, variable_name).expect("read value"),
            Some(value.to_string())
        );
        remove(project_id, variable_name).expect("remove value");
        assert_eq!(
            get(project_id, variable_name).expect("read missing value"),
            None
        );
    }
}
