//! HTTP client with template-based authentication.
//!
//! Makes HTTP requests with auth headers resolved from project configuration.
//! Homeboy doesn't know about specific auth types - it just templates strings.

use crate::error::{Error, ErrorCode, Result};
use crate::extension::HttpMethod;
use crate::project::{ApiConfig, AuthConfig, AuthFlowConfig, VariableSource};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde_json::{json, Value};
use std::collections::HashMap;

fn config_error(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::ConfigInvalidValue, msg, Value::Null)
}

fn not_found_error(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::ExtensionNotFound, msg, Value::Null)
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

    /// Executes an HTTP request with optional body and authentication.
    fn execute_request(
        &self,
        method: HttpMethod,
        endpoint: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let url = format!("{}{}", self.base_url, endpoint);

        let request: RequestBuilder = match method {
            HttpMethod::Get => self.client.get(&url),
            HttpMethod::Post => self.client.post(&url),
            HttpMethod::Put => self.client.put(&url),
            HttpMethod::Patch => self.client.patch(&url),
            HttpMethod::Delete => self.client.delete(&url),
        };

        let request = if let Some(body) = body {
            request.json(body)
        } else {
            request
        };

        let request = if let Some(header) = self.resolve_auth_header()? {
            let (name, value) = parse_header(&header)?;
            request.header(name, value)
        } else {
            request
        };

        let response = request.send().map_err(http_error)?;
        parse_json_response(response)
    }

    /// Makes a GET request.
    pub fn get(&self, endpoint: &str) -> Result<Value> {
        self.execute_request(HttpMethod::Get, endpoint, None)
    }

    /// Makes a POST request with JSON body.
    pub fn post(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Post, endpoint, Some(body))
    }

    /// Makes a PUT request with JSON body.
    pub fn put(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Put, endpoint, Some(body))
    }

    /// Makes a PATCH request with JSON body.
    pub fn patch(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Patch, endpoint, Some(body))
    }

    /// Makes a DELETE request.
    pub fn delete(&self, endpoint: &str) -> Result<Value> {
        self.execute_request(HttpMethod::Delete, endpoint, None)
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

    /// Token refresh is not supported in the CLI.
    /// Use environment variables or config-based auth instead.
    pub fn refresh_if_needed(&self) -> Result<bool> {
        Ok(false)
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
        let _response = self.post_unauthenticated(&flow.endpoint, &Value::Object(body))?;

        // Note: credential storage (keychain) has been removed from the CLI.
        // Auth tokens from login flows are not persisted. Use env vars or
        // config-based auth for CLI/CI workflows.

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

    /// Clears stored auth data for this project.
    /// No-op in CLI mode (credentials are not persisted).
    pub fn logout(&self) -> Result<()> {
        Ok(())
    }

    /// Checks if authenticated (has required variables available).
    pub fn is_authenticated(&self) -> bool {
        let auth = match &self.auth {
            Some(a) => a,
            None => return true, // No auth required
        };

        // Check that all variables can be resolved from config or env
        for (var_name, source) in &auth.variables {
            if resolve_variable(&self.project_id, var_name, source).is_err() {
                return false;
            }
        }

        true
    }
}

/// Resolves a variable from its source.
fn resolve_variable(_project_id: &str, var_name: &str, source: &VariableSource) -> Result<String> {
    match source.source.as_str() {
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
        "keychain" => Err(config_error(format!(
            "Variable source 'keychain' is not supported in the CLI. Use 'env' or 'config' instead for '{}'",
            var_name
        ))),
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
    _project_id: &str,
) -> Result<String> {
    let mut result = template.to_string();

    for (key, value) in credentials {
        let placeholder = format!("{{{{{}}}}}", key);
        result = result.replace(&placeholder, value);
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

fn parse_json_response(response: Response) -> Result<Value> {
    let status = response.status();
    let body = response.text().map_err(http_error)?;

    if !status.is_success() {
        return Err(api_error(status.as_u16(), &body));
    }

    serde_json::from_str(&body).map_err(|e| parse_error(format!("Invalid JSON response: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_api_config_enabled() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let result = instance.new(&project_id, &api_config);
        let err = result.unwrap_err();
        // Branch returns Err(config_error("API is not enabled for this project") when: !api_config.enabled
        let err_msg = format!("{:?}", err);
        let _ = err_msg; // TODO: assert error contains "config_error("API is not enabled for this project""
    }

    #[test]
    fn test_new_api_config_base_url_is_empty() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let result = instance.new(&project_id, &api_config);
        let err = result.unwrap_err();
        // Branch returns Err(config_error("API base URL is not configured") when: api_config.base_url.is_empty()
        let err_msg = format!("{:?}", err);
        let _ = err_msg; // TODO: assert error contains "config_error("API base URL is not configured""
    }

    #[test]
    fn test_new_let_request_if_let_some_body_body() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_else() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_else() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_else() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_self_execute_request_httpmethod_post_endpoint_some_body() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_self_execute_request_httpmethod_put_endpoint_some_body() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_self_execute_request_httpmethod_patch_endpoint_some_body() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_default_path() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_default_path() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_default_path() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_ok_false() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let result = instance.new(&project_id, &api_config);
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_new_default_path() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_default_path() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_ok() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let result = instance.new(&project_id, &api_config);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_new_some_a_a() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_none_return_ok_none() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let result = instance.new(&project_id, &api_config);
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_new_default_path() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_header_contains_placeholder() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_ok_some_header() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let result = instance.new(&project_id, &api_config);
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_new_ok() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let result = instance.new(&project_id, &api_config);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_new_some_a_a() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _result = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_new_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let project_id = "";
        let api_config = Default::default();
        let _ = instance.new(&project_id, &api_config);
    }

    #[test]
    fn test_get_self_execute_request_httpmethod_post_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_self_execute_request_httpmethod_put_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_self_execute_request_httpmethod_patch_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_ok_false() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.get(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_get_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.get(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_get_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_none_return_ok_none() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.get(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_get_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_header_contains_placeholder() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_ok_some_header() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.get(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_get_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.get(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_get_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.get(&endpoint);
    }

    #[test]
    fn test_get_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let endpoint = "";
        let _ = instance.get(&endpoint);
    }

    #[test]
    fn test_post_self_execute_request_httpmethod_post_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_self_execute_request_httpmethod_put_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_self_execute_request_httpmethod_patch_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_ok_false() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_post_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_post_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_none_return_ok_none() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_post_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_header_contains_placeholder() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_ok_some_header() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_post_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_post_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_post_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _ = instance.post(&endpoint, &body);
    }

    #[test]
    fn test_put_self_execute_request_httpmethod_put_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_self_execute_request_httpmethod_patch_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_ok_false() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.put(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_put_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.put(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_put_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_none_return_ok_none() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.put(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_put_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_header_contains_placeholder() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_ok_some_header() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.put(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_put_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.put(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_put_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_put_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _ = instance.put(&endpoint, &body);
    }

    #[test]
    fn test_patch_self_execute_request_httpmethod_patch_endpoint_some_body() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_ok_false() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.patch(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_patch_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.patch(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_patch_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_none_return_ok_none() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.patch(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_patch_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_header_contains_placeholder() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_ok_some_header() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.patch(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_patch_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.patch(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_patch_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_patch_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _ = instance.patch(&endpoint, &body);
    }

    #[test]
    fn test_delete_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_ok_false() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.delete(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_delete_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.delete(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_delete_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_none_return_ok_none() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.delete(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_delete_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_header_contains_placeholder() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_ok_some_header() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.delete(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_delete_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let result = instance.delete(&endpoint);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_delete_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let endpoint = "";
        let _result = instance.delete(&endpoint);
    }

    #[test]
    fn test_delete_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let endpoint = "";
        let _ = instance.delete(&endpoint);
    }

    #[test]
    fn test_post_unauthenticated_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_ok_false() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post_unauthenticated(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_post_unauthenticated_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post_unauthenticated(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_post_unauthenticated_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_none_return_ok_none() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post_unauthenticated(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_post_unauthenticated_default_path() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_header_contains_placeholder() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_ok_some_header() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post_unauthenticated(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_post_unauthenticated_ok() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let result = instance.post_unauthenticated(&endpoint, &body);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_post_unauthenticated_some_a_a() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _result = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_post_unauthenticated_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let endpoint = "";
        let body = Default::default();
        let _ = instance.post_unauthenticated(&endpoint, &body);
    }

    #[test]
    fn test_login_default_path() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_default_path() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_ok_false() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let result = instance.login(&credentials);
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_login_default_path() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_default_path() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_ok() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let result = instance.login(&credentials);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_login_some_a_a() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_none_return_ok_none() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let result = instance.login(&credentials);
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_login_default_path() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_header_contains_placeholder() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_ok_some_header() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let result = instance.login(&credentials);
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_login_ok() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let result = instance.login(&credentials);
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_login_some_a_a() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _result = instance.login(&credentials);
    }

    #[test]
    fn test_login_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let credentials = Default::default();
        let _ = instance.login(&credentials);
    }

    #[test]
    fn test_refresh_if_needed_ok_false() {
        let instance = ApiClient::default();
        let result = instance.refresh_if_needed();
        let inner = result.unwrap();
        // Branch returns Ok(false) when: Ok(false)
        let _ = inner; // TODO: assert specific value for "false"
    }

    #[test]
    fn test_refresh_if_needed_default_path() {
        let instance = ApiClient::default();
        let _result = instance.refresh_if_needed();
    }

    #[test]
    fn test_refresh_if_needed_default_path() {
        let instance = ApiClient::default();
        let _result = instance.refresh_if_needed();
    }

    #[test]
    fn test_refresh_if_needed_ok() {
        let instance = ApiClient::default();
        let result = instance.refresh_if_needed();
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_refresh_if_needed_some_a_a() {
        let instance = ApiClient::default();
        let _result = instance.refresh_if_needed();
    }

    #[test]
    fn test_refresh_if_needed_none_return_ok_none() {
        let instance = ApiClient::default();
        let result = instance.refresh_if_needed();
        let inner = result.unwrap();
        // Branch returns Ok(None) when: None => return Ok(None),
        let _ = inner; // TODO: assert specific value for "None"
    }

    #[test]
    fn test_refresh_if_needed_default_path() {
        let instance = ApiClient::default();
        let _result = instance.refresh_if_needed();
    }

    #[test]
    fn test_refresh_if_needed_header_contains_placeholder() {
        let instance = ApiClient::default();
        let _result = instance.refresh_if_needed();
    }

    #[test]
    fn test_refresh_if_needed_ok_some_header() {
        let instance = ApiClient::default();
        let result = instance.refresh_if_needed();
        let inner = result.unwrap();
        // Branch returns Ok(Some(header) when: Ok(Some(header))
        let _ = inner; // TODO: assert specific value for "Some(header"
    }

    #[test]
    fn test_refresh_if_needed_ok() {
        let instance = ApiClient::default();
        let result = instance.refresh_if_needed();
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_refresh_if_needed_some_a_a() {
        let instance = ApiClient::default();
        let _result = instance.refresh_if_needed();
    }

    #[test]
    fn test_refresh_if_needed_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let _result = instance.refresh_if_needed();
    }

    #[test]
    fn test_refresh_if_needed_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let _result = instance.refresh_if_needed();
    }

    #[test]
    fn test_refresh_if_needed_has_expected_effects() {
        // Expected effects: mutation
        let instance = ApiClient::default();
        let _ = instance.refresh_if_needed();
    }

    #[test]
    fn test_logout_ok() {
        let instance = ApiClient::default();
        let result = instance.logout();
        let inner = result.unwrap();
        // Branch returns Ok(() when: Ok(())
        let _ = inner; // TODO: assert specific value for "("
    }

    #[test]
    fn test_logout_some_a_a() {
        let instance = ApiClient::default();
        let _result = instance.logout();
    }

    #[test]
    fn test_logout_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let _result = instance.logout();
    }

    #[test]
    fn test_logout_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let _result = instance.logout();
    }

    #[test]
    fn test_is_authenticated_some_a_a() {
        let instance = ApiClient::default();
        let _result = instance.is_authenticated();
    }

    #[test]
    fn test_is_authenticated_none_return_true_no_auth_required() {
        let instance = ApiClient::default();
        let result = instance.is_authenticated();
        assert!(
            result,
            "expected true when: None => return true, // No auth required"
        );
    }

    #[test]
    fn test_is_authenticated_resolve_variable_self_project_id_var_name_source_is_err() {
        let instance = ApiClient::default();
        let result = instance.is_authenticated();
        assert!(
            !result,
            "expected false when: resolve_variable(&self.project_id, var_name, source).is_err()"
        );
    }
}
