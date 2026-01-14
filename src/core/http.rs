//! HTTP client with template-based authentication.
//!
//! Makes HTTP requests with auth headers resolved from project configuration.
//! Homeboy doesn't know about specific auth types - it just templates strings.

use crate::error::{Error, ErrorCode, Result};
use crate::keychain;
use crate::project::{ApiConfig, AuthConfig, AuthFlowConfig, VariableSource};
use reqwest::blocking::{Client, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

fn config_error(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::ConfigInvalidValue, msg, Value::Null)
}

fn not_found_error(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::ModuleNotFound, msg, Value::Null)
}

fn http_error(e: reqwest::Error) -> Error {
    Error::new(
        ErrorCode::RemoteCommandFailed,
        format!("HTTP request failed: {}", e),
        json!({ "error": e.to_string() }),
    )
}

fn api_error(status: u16, body: &str) -> Error {
    Error::new(
        ErrorCode::RemoteCommandFailed,
        format!("API error: HTTP {}", status),
        json!({ "status": status, "body": body }),
    )
}

fn parse_error(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::InternalJsonError, msg, Value::Null)
}

/// HTTP client for a project's API.
pub struct ApiClient {
    client: Client,
    base_url: String,
    project_id: String,
    auth: Option<AuthConfig>,
}

impl ApiClient {
    /// Creates a new API client from project configuration.
    pub fn new(project_id: &str, api_config: &ApiConfig) -> Result<Self> {
        if !api_config.enabled {
            return Err(config_error("API is not enabled for this project"));
        }

        if api_config.base_url.is_empty() {
            return Err(config_error("API base URL is not configured"));
        }

        Ok(Self {
            client: Client::new(),
            base_url: api_config.base_url.clone(),
            project_id: project_id.to_string(),
            auth: api_config.auth.clone(),
        })
    }

    /// Makes a GET request.
    pub fn get(&self, endpoint: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut request = self.client.get(&url);

        if let Some(header) = self.resolve_auth_header()? {
            let (name, value) = parse_header(&header)?;
            request = request.header(name, value);
        }

        let response = request.send().map_err(http_error)?;
        parse_json_response(response)
    }

    /// Makes a POST request with JSON body.
    pub fn post(&self, endpoint: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut request = self.client.post(&url).json(body);

        if let Some(header) = self.resolve_auth_header()? {
            let (name, value) = parse_header(&header)?;
            request = request.header(name, value);
        }

        let response = request.send().map_err(http_error)?;
        parse_json_response(response)
    }

    /// Makes a PUT request with JSON body.
    pub fn put(&self, endpoint: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut request = self.client.put(&url).json(body);

        if let Some(header) = self.resolve_auth_header()? {
            let (name, value) = parse_header(&header)?;
            request = request.header(name, value);
        }

        let response = request.send().map_err(http_error)?;
        parse_json_response(response)
    }

    /// Makes a PATCH request with JSON body.
    pub fn patch(&self, endpoint: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut request = self.client.patch(&url).json(body);

        if let Some(header) = self.resolve_auth_header()? {
            let (name, value) = parse_header(&header)?;
            request = request.header(name, value);
        }

        let response = request.send().map_err(http_error)?;
        parse_json_response(response)
    }

    /// Makes a DELETE request.
    pub fn delete(&self, endpoint: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, endpoint);
        let mut request = self.client.delete(&url);

        if let Some(header) = self.resolve_auth_header()? {
            let (name, value) = parse_header(&header)?;
            request = request.header(name, value);
        }

        let response = request.send().map_err(http_error)?;
        parse_json_response(response)
    }

    /// Makes a POST request without auth (for login flows).
    pub fn post_unauthenticated(&self, endpoint: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, endpoint);
        let response = self
            .client
            .post(&url)
            .json(body)
            .send()
            .map_err(http_error)?;
        parse_json_response(response)
    }

    /// Executes the login flow if configured.
    pub fn login(&self, credentials: &HashMap<String, String>) -> Result<()> {
        let auth = self
            .auth
            .as_ref()
            .ok_or_else(|| config_error("No auth configuration for this project"))?;

        let login = auth
            .login
            .as_ref()
            .ok_or_else(|| config_error("No login flow configured for this project"))?;

        self.execute_auth_flow(login, credentials)
    }

    /// Executes the refresh flow if configured and tokens are expired.
    pub fn refresh_if_needed(&self) -> Result<bool> {
        let auth = match &self.auth {
            Some(a) => a,
            None => return Ok(false),
        };

        let refresh = match &auth.refresh {
            Some(r) => r,
            None => return Ok(false),
        };

        // Check if we have an expires_at value
        let expires_at = match keychain::get(&self.project_id, "expires_at")? {
            Some(v) => v.parse::<i64>().unwrap_or(0),
            None => return Ok(false),
        };

        // Check if expired (with 60 second buffer)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        if now < expires_at - 60 {
            return Ok(false); // Not expired yet
        }

        // Get refresh token
        let refresh_token = keychain::get(&self.project_id, "refresh_token")?
            .ok_or_else(|| not_found_error("No refresh token stored"))?;

        let mut credentials = HashMap::new();
        credentials.insert("refresh_token".to_string(), refresh_token);

        self.execute_auth_flow(refresh, &credentials)?;
        Ok(true)
    }

    /// Executes an auth flow (login or refresh).
    fn execute_auth_flow(
        &self,
        flow: &AuthFlowConfig,
        credentials: &HashMap<String, String>,
    ) -> Result<()> {
        // Build request body by templating
        let mut body = serde_json::Map::new();
        for (key, template) in &flow.body {
            let value = resolve_template(template, credentials, &self.project_id)?;
            body.insert(key.clone(), Value::String(value));
        }

        // Make the request
        let response = self.post_unauthenticated(&flow.endpoint, &Value::Object(body))?;

        // Store response fields in keychain
        for (var_name, json_path) in &flow.store {
            if let Some(value) = get_json_path(&response, json_path) {
                let value_str = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => value.to_string(),
                };
                keychain::store(&self.project_id, var_name, &value_str)?;
            }
        }

        Ok(())
    }

    /// Resolves the auth header template with variable values.
    fn resolve_auth_header(&self) -> Result<Option<String>> {
        let auth = match &self.auth {
            Some(a) => a,
            None => return Ok(None),
        };

        // Auto-refresh if needed
        self.refresh_if_needed()?;

        // Resolve variables in the header template
        let mut header = auth.header.clone();
        for (var_name, source) in &auth.variables {
            let placeholder = format!("{{{{{}}}}}", var_name);
            if header.contains(&placeholder) {
                let value = resolve_variable(&self.project_id, var_name, source)?;
                header = header.replace(&placeholder, &value);
            }
        }

        Ok(Some(header))
    }

    /// Clears all stored auth data for this project.
    pub fn logout(&self) -> Result<()> {
        let common_vars = ["access_token", "refresh_token", "expires_at", "password"];
        keychain::clear_project(&self.project_id, &common_vars)?;

        // Also clear any custom variables from auth config
        if let Some(auth) = &self.auth {
            for var_name in auth.variables.keys() {
                let _ = keychain::delete(&self.project_id, var_name);
            }
        }

        Ok(())
    }

    /// Checks if authenticated (has access token or required variables).
    pub fn is_authenticated(&self) -> bool {
        let auth = match &self.auth {
            Some(a) => a,
            None => return true, // No auth required
        };

        // Check if all keychain variables have values
        for (var_name, source) in &auth.variables {
            if source.source == "keychain"
                && !keychain::exists(&self.project_id, var_name) {
                    return false;
                }
        }

        true
    }
}

/// Resolves a variable from its source.
fn resolve_variable(project_id: &str, var_name: &str, source: &VariableSource) -> Result<String> {
    match source.source.as_str() {
        "keychain" => keychain::get(project_id, var_name)?.ok_or_else(|| {
            not_found_error(format!("Variable '{}' not found in keychain", var_name))
        }),
        "config" => source
            .value
            .clone()
            .ok_or_else(|| config_error(format!("Variable '{}' has no config value", var_name))),
        "env" => {
            let default_env = var_name.to_string();
            let env_var = source.env_var.as_ref().unwrap_or(&default_env);
            std::env::var(env_var)
                .map_err(|_| not_found_error(format!("Environment variable '{}' not set", env_var)))
        }
        _ => Err(config_error(format!(
            "Unknown variable source: {}",
            source.source
        ))),
    }
}

/// Resolves a template string with credential values.
fn resolve_template(
    template: &str,
    credentials: &HashMap<String, String>,
    project_id: &str,
) -> Result<String> {
    let mut result = template.to_string();

    // First, try credentials
    for (key, value) in credentials {
        let placeholder = format!("{{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
    }

    // Then, try keychain for any remaining placeholders
    let re = regex::Regex::new(r"\{\{(\w+)\}\}").unwrap();
    for cap in re.captures_iter(template) {
        let var_name = &cap[1];
        let placeholder = format!("{{{{{}}}}}", var_name);
        if result.contains(&placeholder) {
            if let Some(value) = keychain::get(project_id, var_name)? {
                result = result.replace(&placeholder, &value);
            }
        }
    }

    Ok(result)
}

/// Parses a header string like "Authorization: Bearer token" into (name, value).
fn parse_header(header: &str) -> Result<(&str, &str)> {
    let parts: Vec<&str> = header.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(config_error(format!("Invalid header format: {}", header)));
    }
    Ok((parts[0].trim(), parts[1].trim()))
}

/// Gets a value from JSON using a simple dot-notation path.
fn get_json_path<'a>(json: &'a Value, path: &str) -> Option<&'a Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = json;

    for part in parts {
        current = current.get(part)?;
    }

    Some(current)
}

fn parse_json_response(response: Response) -> Result<Value> {
    let status = response.status();
    let body = response.text().map_err(http_error)?;

    if !status.is_success() {
        return Err(api_error(status.as_u16(), &body));
    }

    serde_json::from_str(&body).map_err(|e| parse_error(format!("Invalid JSON response: {}", e)))
}
