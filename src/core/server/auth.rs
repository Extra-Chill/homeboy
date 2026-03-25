//! Authentication operations for project APIs.
//!
//! Provides login, logout, and status checking without exposing
//! the underlying HTTP client or keychain implementation.

use super::http::ApiClient;
use crate::error::Result;
use crate::project;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Serialize)]
pub struct LoginResult {
    pub project_id: String,
    pub success: bool,
}

#[derive(Debug, Serialize)]
pub struct AuthStatus {
    pub project_id: String,
    pub authenticated: bool,
}

#[derive(Debug, Serialize)]
pub struct LogoutResult {
    pub project_id: String,
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
    let client = ApiClient::new(project_id, &project.api)?;
    client.logout()?;

    Ok(LogoutResult {
        project_id: project_id.to_string(),
    })
}

/// Checks authentication status for a project.
pub fn status(project_id: &str) -> Result<AuthStatus> {
    let project = project::load(project_id)?;
    let client = ApiClient::new(project_id, &project.api)?;

    Ok(AuthStatus {
        project_id: project_id.to_string(),
        authenticated: client.is_authenticated(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_login_default_path() {
        let project_id = "";
        let credentials = HashMap::new();
        let _result = login(&project_id, credentials);
    }

    #[test]
    fn test_login_default_path_2() {
        let project_id = "";
        let credentials = HashMap::new();
        let _result = login(&project_id, credentials);
    }

    #[test]
    fn test_login_default_path_3() {
        let project_id = "";
        let credentials = HashMap::new();
        let _result = login(&project_id, credentials);
    }

    #[test]
    fn test_logout_default_path() {
        let project_id = "";
        let _result = logout(&project_id);
    }

    #[test]
    fn test_logout_default_path_2() {
        let project_id = "";
        let _result = logout(&project_id);
    }

    #[test]
    fn test_logout_default_path_3() {
        let project_id = "";
        let _result = logout(&project_id);
    }

    #[test]
    fn test_status_default_path() {
        let project_id = "";
        let _result = status(&project_id);
    }

    #[test]
    fn test_status_default_path_2() {
        let project_id = "";
        let _result = status(&project_id);
    }

}
