//! Authentication operations for project APIs.
//!
//! Provides login, logout, and status checking without exposing
//! the underlying HTTP client or keychain implementation.

use super::http::ApiClient;
use crate::core::error::{Error, Result};
use crate::core::keychain;
use crate::core::project;
use serde::Serialize;
use std::collections::HashMap;
use std::io::IsTerminal;

#[derive(Debug, Serialize)]
pub struct LoginResult {
    pub project_id: String,
    pub success: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthStatus {
    pub project_id: String,
    pub authenticated: bool,
    pub variables: Vec<AuthVariableStatus>,
}

#[derive(Debug, Serialize)]
pub struct LogoutResult {
    pub project_id: String,
    pub removed: usize,
}

#[derive(Debug, Serialize)]
pub struct SetResult {
    pub project_id: String,
    pub variable: String,
    pub stored: bool,
}

#[derive(Debug, Serialize)]
pub struct GetResult {
    pub project_id: String,
    pub variable: String,
    pub value: Option<String>,
    pub redacted: bool,
    pub state: String,
    pub source: String,
    pub diagnostic: KeychainReadDiagnostic,
}

#[derive(Debug, Serialize)]
pub struct RemoveResult {
    pub project_id: String,
    pub variable: String,
    pub removed: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthVariableStatus {
    pub name: String,
    pub source: String,
    pub available: bool,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<KeychainReadDiagnostic>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct KeychainReadDiagnostic {
    pub source: String,
    pub backend: String,
    pub service: String,
    pub account: String,
    pub value_status: String,
    pub storage_context: String,
    pub interactive: bool,
    pub ssh_context: bool,
    pub message: String,
    pub hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

/// Authenticates with a project's API using provided credentials.
///
/// The caller is responsible for obtaining credentials (prompting, flags, etc.).
/// This function handles the authentication flow and token storage.
pub fn login(project_id: &str, credentials: HashMap<String, String>) -> Result<LoginResult> {
    let project = project::load(project_id)?;
    let client = ApiClient::new(project_id, &project.api)?;
    client.login(&credentials)?;

    Ok(LoginResult {
        project_id: project_id.to_string(),
        success: true,
    })
}

/// Clears stored authentication for a project.
pub fn logout(project_id: &str) -> Result<LogoutResult> {
    let project = project::load(project_id)?;
    let variable_names = keychain_variable_names(&project);
    let removed = keychain::remove_many(project_id, &variable_names)?;

    Ok(LogoutResult {
        project_id: project_id.to_string(),
        removed,
    })
}

/// Stores a project API variable in the keychain.
pub fn set(project_id: &str, variable: &str, value: &str) -> Result<SetResult> {
    keychain::set(project_id, variable, value)?;
    Ok(SetResult {
        project_id: project_id.to_string(),
        variable: variable.to_string(),
        stored: true,
    })
}

/// Retrieves a project API variable from the keychain.
pub fn get(project_id: &str, variable: &str, redacted: bool) -> Result<GetResult> {
    let read = keychain::get(project_id, variable)?;
    let state = if read.is_some() {
        "available"
    } else {
        "missing"
    }
    .to_string();
    let diagnostic = keychain_read_diagnostic(project_id, variable, &state, None);
    let value = read.map(|value| if redacted { redact(&value) } else { value });

    Ok(GetResult {
        project_id: project_id.to_string(),
        variable: variable.to_string(),
        value,
        redacted,
        state,
        source: "project-keychain".to_string(),
        diagnostic,
    })
}

/// Removes a project API variable from the keychain.
pub fn remove(project_id: &str, variable: &str) -> Result<RemoveResult> {
    let removed = keychain::get(project_id, variable)?.is_some();
    keychain::remove(project_id, variable)?;

    Ok(RemoveResult {
        project_id: project_id.to_string(),
        variable: variable.to_string(),
        removed,
    })
}

/// Checks authentication status for a project.
pub fn status(project_id: &str) -> Result<AuthStatus> {
    let project = project::load(project_id)?;
    let client = ApiClient::new(project_id, &project.api)?;

    Ok(AuthStatus {
        project_id: project_id.to_string(),
        authenticated: client.is_authenticated(),
        variables: variable_statuses(project_id, &project),
    })
}

fn keychain_variable_names(project: &project::Project) -> Vec<String> {
    project
        .api
        .auth
        .as_ref()
        .map(|auth| {
            auth.variables
                .iter()
                .filter(|&(_name, source)| source.source == "keychain")
                .map(|(name, _source)| name.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn variable_statuses(project_id: &str, project: &project::Project) -> Vec<AuthVariableStatus> {
    let Some(auth) = project.api.auth.as_ref() else {
        return Vec::new();
    };

    auth.variables
        .iter()
        .map(|(name, source)| variable_status(project_id, name, source))
        .collect()
}

fn variable_status(
    project_id: &str,
    name: &str,
    source: &project::VariableSource,
) -> AuthVariableStatus {
    let (available, state, diagnostic) = match source.source.as_str() {
        "keychain" => match keychain::get(project_id, name) {
            Ok(Some(_)) => (
                true,
                "available".to_string(),
                Some(keychain_read_diagnostic(
                    project_id,
                    name,
                    "available",
                    None,
                )),
            ),
            Ok(None) => (
                false,
                "missing".to_string(),
                Some(keychain_read_diagnostic(project_id, name, "missing", None)),
            ),
            Err(error) => (
                false,
                "backend_error".to_string(),
                Some(keychain_read_diagnostic(
                    project_id,
                    name,
                    "backend_error",
                    Some(&error),
                )),
            ),
        },
        _ => {
            let available = variable_available(project_id, name, source);
            let state = if available { "available" } else { "missing" };
            (available, state.to_string(), None)
        }
    };

    AuthVariableStatus {
        name: name.to_string(),
        source: source.source.clone(),
        available,
        state,
        diagnostic,
    }
}

fn variable_available(_project_id: &str, name: &str, source: &project::VariableSource) -> bool {
    match source.source.as_str() {
        "config" => source.value.is_some(),
        "env" => {
            let default_env = name.to_string();
            let env_var = source.env_var.as_ref().unwrap_or(&default_env);
            std::env::var(env_var).is_ok()
        }
        _ => false,
    }
}

fn keychain_read_diagnostic(
    project_id: &str,
    variable: &str,
    value_status: &str,
    error: Option<&Error>,
) -> KeychainReadDiagnostic {
    let storage_context = if in_ssh_context() { "ssh" } else { "local" };
    let interactive = std::io::stdin().is_terminal();
    let message = match value_status {
        "available" => {
            "Project keychain value is available; value remains redacted by diagnostics."
        }
        "missing" => "Project keychain returned no value in this Homeboy storage context.",
        "backend_error" => "Project keychain backend could not be read in this runtime context.",
        _ => "Project keychain read completed with an unrecognized status.",
    };

    let mut hints = vec![
        "Controller and Lab hosts use separate OS keychain storage contexts.".to_string(),
        "For Lab jobs, prefer controller-side secret hydration and forwarding with --secret-env."
            .to_string(),
        "Use source: \"env\" for CI/headless environments when OS keychain access is unavailable."
            .to_string(),
    ];
    if !interactive || in_ssh_context() {
        hints.push(
            "This command is running in a non-interactive or SSH context; OS keychain prompts may be unavailable."
                .to_string(),
        );
    }
    if let Some(error) = error {
        hints.extend(error.hints.iter().map(|hint| hint.message.clone()));
    }

    KeychainReadDiagnostic {
        source: "project-keychain".to_string(),
        backend: "os-keychain".to_string(),
        service: "homeboy".to_string(),
        account: format!("{}:{}", project_id, variable),
        value_status: value_status.to_string(),
        storage_context: storage_context.to_string(),
        interactive,
        ssh_context: in_ssh_context(),
        message: message.to_string(),
        hints,
        error_code: error.map(|error| error.code.as_str().to_string()),
    }
}

fn in_ssh_context() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_CLIENT").is_some()
}

fn redact(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    "********".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::project::VariableSource;

    #[test]
    fn test_redact() {
        assert_eq!(redact("secret"), "********");
        assert_eq!(redact(""), "");
    }

    #[test]
    fn test_variable_available_config() {
        let source = VariableSource {
            source: "config".to_string(),
            value: Some("value".to_string()),
            env_var: None,
        };

        assert!(variable_available("project", "token", &source));
    }

    #[test]
    fn variable_status_config_keeps_compatibility_fields_without_diagnostic() {
        let source = VariableSource {
            source: "config".to_string(),
            value: Some("value".to_string()),
            env_var: None,
        };

        let status = variable_status("project", "token", &source);

        assert_eq!(status.name, "token");
        assert_eq!(status.source, "config");
        assert!(status.available);
        assert_eq!(status.state, "available");
        assert_eq!(status.diagnostic, None);
    }

    #[test]
    fn keychain_missing_diagnostic_is_redacted_and_actionable() {
        let diagnostic = keychain_read_diagnostic("project", "TOKEN", "missing", None);

        assert_eq!(diagnostic.source, "project-keychain");
        assert_eq!(diagnostic.backend, "os-keychain");
        assert_eq!(diagnostic.service, "homeboy");
        assert_eq!(diagnostic.account, "project:TOKEN");
        assert_eq!(diagnostic.value_status, "missing");
        assert!(diagnostic.message.contains("returned no value"));
        assert!(diagnostic
            .hints
            .iter()
            .any(|hint| hint.contains("separate OS keychain storage contexts")));
        assert!(diagnostic
            .hints
            .iter()
            .any(|hint| hint.contains("--secret-env")));
        assert!(!serde_json::to_string(&diagnostic)
            .expect("serialize diagnostic")
            .contains("secret-value"));
    }

    #[test]
    fn keychain_error_diagnostic_reports_error_code_without_value() {
        let error = Error::new(
            crate::core::error::ErrorCode::InternalUnexpected,
            "Keychain error: failed to find keychain item (-25308)",
            serde_json::json!({ "status": -25308, "action": "find keychain item" }),
        )
        .with_hint("Use source: \"env\" for CI/headless environments");

        let diagnostic =
            keychain_read_diagnostic("project", "TOKEN", "backend_error", Some(&error));

        assert_eq!(diagnostic.value_status, "backend_error");
        assert_eq!(
            diagnostic.error_code.as_deref(),
            Some("internal.unexpected")
        );
        assert!(diagnostic
            .hints
            .iter()
            .any(|hint| hint.contains("CI/headless")));
        assert!(!serde_json::to_string(&diagnostic)
            .expect("serialize diagnostic")
            .contains("secret-value"));
    }

    #[test]
    fn test_variable_available_missing_config() {
        let source = VariableSource {
            source: "config".to_string(),
            value: None,
            env_var: None,
        };

        assert!(!variable_available("project", "token", &source));
    }

    #[test]
    fn test_variable_available_unknown_source() {
        let source = VariableSource {
            source: "unknown".to_string(),
            value: None,
            env_var: None,
        };

        assert!(!variable_available("project", "token", &source));
    }

    #[test]
    #[ignore]
    fn test_login() {
        let credentials = HashMap::new();
        let _ = login("homeboy-auth-test", credentials);
    }

    #[test]
    #[ignore]
    fn test_logout() {
        let _ = logout("homeboy-auth-test");
    }

    #[test]
    #[ignore]
    fn test_set() {
        let result = set("homeboy-auth-test", "token", "secret-value").expect("store value");

        assert!(result.stored);
        assert_eq!(result.project_id, "homeboy-auth-test");
        assert_eq!(result.variable, "token");
        remove("homeboy-auth-test", "token").expect("cleanup value");
    }

    #[test]
    #[ignore]
    fn test_get() {
        set("homeboy-auth-test", "token", "secret-value").expect("store value");
        let result = get("homeboy-auth-test", "token", true).expect("read value");

        assert_eq!(result.value.as_deref(), Some("********"));
        assert!(result.redacted);
        remove("homeboy-auth-test", "token").expect("cleanup value");
    }

    #[test]
    #[ignore]
    fn test_remove() {
        set("homeboy-auth-test", "token", "secret-value").expect("store value");
        let result = remove("homeboy-auth-test", "token").expect("remove value");

        assert!(result.removed);
    }

    #[test]
    #[ignore]
    fn test_status() {
        let _ = status("homeboy-auth-test");
    }
}
