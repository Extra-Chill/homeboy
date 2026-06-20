//! HTTP client with template-based authentication.
//!
//! Makes HTTP requests with auth headers resolved from project configuration.
//! Homeboy doesn't know about specific auth types - it just templates strings.

use crate::core::error::{Error, ErrorCode, Result};
use crate::core::extension::HttpMethod;
use crate::core::keychain;
use crate::core::project::{ApiConfig, AuthConfig, AuthFlowConfig, VariableSource};
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder, Response};
use reqwest::Proxy;
use serde_json::{json, Value};
use std::collections::HashMap;

fn config_error(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::ConfigInvalidValue, msg, Value::Null)
}

fn not_found_error(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::ExtensionNotFound, msg, Value::Null)
}

pub(crate) fn http_error(e: reqwest::Error) -> Error {
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

        let client = build_client(api_config)?;

        Ok(Self {
            client,
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
        body_format: BodyFormat,
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
            match body_format {
                BodyFormat::Json => request.json(body),
                BodyFormat::Form => request.form(&form_fields(body)?),
            }
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
        self.execute_request(HttpMethod::Get, endpoint, None, BodyFormat::Json)
    }

    /// Makes a POST request with JSON body.
    pub fn post(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Post, endpoint, Some(body), BodyFormat::Json)
    }

    /// Makes a POST request with form fields.
    pub fn post_form(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Post, endpoint, Some(body), BodyFormat::Form)
    }

    /// Makes a PUT request with JSON body.
    pub fn put(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Put, endpoint, Some(body), BodyFormat::Json)
    }

    /// Makes a PUT request with form fields.
    pub fn put_form(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Put, endpoint, Some(body), BodyFormat::Form)
    }

    /// Makes a PATCH request with JSON body.
    pub fn patch(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Patch, endpoint, Some(body), BodyFormat::Json)
    }

    /// Makes a PATCH request with form fields.
    pub fn patch_form(&self, endpoint: &str, body: &Value) -> Result<Value> {
        self.execute_request(HttpMethod::Patch, endpoint, Some(body), BodyFormat::Form)
    }

    /// Makes a DELETE request.
    pub fn delete(&self, endpoint: &str) -> Result<Value> {
        self.execute_request(HttpMethod::Delete, endpoint, None, BodyFormat::Json)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyFormat {
    #[default]
    Json,
    Form,
}

fn form_fields(body: &Value) -> Result<Vec<(String, String)>> {
    if let Some(pairs) = body.as_array() {
        return pairs
            .iter()
            .map(|pair| {
                let pair = pair.as_array().ok_or_else(|| {
                    config_error("Form body array entries must be [key, value] pairs")
                })?;
                if pair.len() != 2 {
                    return Err(config_error(
                        "Form body array entries must be [key, value] pairs",
                    ));
                }
                let key = pair[0]
                    .as_str()
                    .ok_or_else(|| config_error("Form field key must be a string"))?;
                let value = pair[1]
                    .as_str()
                    .ok_or_else(|| config_error("Form field value must be a string"))?;
                Ok((key.to_string(), value.to_string()))
            })
            .collect();
    }

    let object = body.as_object().ok_or_else(|| {
        config_error("Form body must be a JSON object or an array of [key, value] pairs")
    })?;

    object
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_string()))
                .ok_or_else(|| config_error(format!("Form field '{}' must be a string", key)))
        })
        .collect()
}

pub(crate) fn build_client_with_proxy(proxy_url: Option<&str>) -> Result<Client> {
    let mut builder = ClientBuilder::new();

    if let Some(proxy_url) = proxy_url {
        builder =
            builder.proxy(Proxy::all(proxy_url).map_err(|e| {
                config_error(format!("Invalid API proxy URL '{}': {}", proxy_url, e))
            })?);
    }

    builder.build().map_err(http_error)
}

fn build_client(api_config: &ApiConfig) -> Result<Client> {
    build_client_with_proxy(api_config.proxy_url.as_deref())
}

/// Resolves a variable from its source.
fn resolve_variable(project_id: &str, var_name: &str, source: &VariableSource) -> Result<String> {
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
        "keychain" => keychain::get(project_id, var_name)?
            .ok_or_else(|| keychain::missing_error(project_id, var_name)),
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
    use crate::core::project::{AuthConfig, AuthFlowConfig};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn test_client(base_url: String) -> ApiClient {
        ApiClient::new(
            "test-project",
            &ApiConfig {
                enabled: true,
                base_url,
                proxy_url: None,
                auth: None,
            },
        )
        .expect("api client")
    }

    fn test_client_with_auth(base_url: String, auth: AuthConfig) -> ApiClient {
        ApiClient::new(
            "test-project",
            &ApiConfig {
                enabled: true,
                base_url,
                proxy_url: None,
                auth: Some(auth),
            },
        )
        .expect("api client")
    }

    fn with_test_server<F>(assert_request: F) -> String
    where
        F: FnOnce(&str) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buffer = Vec::new();
            let mut temp = [0_u8; 1024];
            loop {
                let read = stream.read(&mut temp).expect("read request");
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&temp[..read]);

                let request = String::from_utf8_lossy(&buffer);
                if let Some(header_end) = request.find("\r\n\r\n") {
                    let headers = &request[..header_end];
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("Content-Length: ")
                                .and_then(|value| value.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    let body_len = buffer.len().saturating_sub(header_end + 4);
                    if body_len >= content_length {
                        break;
                    }
                }
            }

            let request = String::from_utf8_lossy(&buffer).to_string();
            assert_request(&request);
            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: application/json\r\n",
                "Content-Length: 11\r\n",
                "Connection: close\r\n\r\n",
                "{\"ok\":true}"
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        format!("http://{}", addr)
    }

    fn assert_method_path(request: &str, method: &str, path: &str) {
        assert!(
            request.starts_with(&format!("{} {} HTTP/1.1", method, path)),
            "unexpected request: {}",
            request
        );
    }

    #[test]
    fn test_http_error() {
        let reqwest_error = Client::new()
            .get("http://")
            .send()
            .expect_err("invalid URL should fail before network I/O");

        let err = http_error(reqwest_error);

        assert_eq!(err.code, ErrorCode::RemoteCommandFailed);
        assert!(err.message.contains("HTTP request failed"));
    }

    #[test]
    fn test_build_client_with_proxy() {
        build_client_with_proxy(Some("socks5://127.0.0.1:8080"))
            .expect("socks proxy should be accepted");
    }

    #[test]
    fn builds_client_with_socks_proxy() {
        let config = ApiConfig {
            enabled: true,
            base_url: "https://api.example.test/v1".to_string(),
            proxy_url: Some("socks5://127.0.0.1:8080".to_string()),
            auth: None,
        };

        build_client(&config).expect("socks proxy should be accepted");
    }

    #[test]
    fn rejects_invalid_proxy_url() {
        let config = ApiConfig {
            enabled: true,
            base_url: "https://example.com".to_string(),
            proxy_url: Some("not a proxy".to_string()),
            auth: None,
        };

        let err = build_client(&config).expect_err("invalid proxy should fail");
        assert!(err.to_string().contains("Invalid API proxy URL"));
    }

    #[test]
    fn form_fields_preserve_duplicate_keys() {
        let fields = form_fields(&serde_json::json!([
            ["provision[]", "base"],
            ["provision[]", "install-wp"],
            ["provision[]", "dereference"]
        ]))
        .expect("form fields");

        assert_eq!(
            fields,
            vec![
                ("provision[]".to_string(), "base".to_string()),
                ("provision[]".to_string(), "install-wp".to_string()),
                ("provision[]".to_string(), "dereference".to_string()),
            ]
        );
    }

    #[test]
    fn test_get() {
        let base_url = with_test_server(|request| assert_method_path(request, "GET", "/items"));
        let response = test_client(base_url).get("/items").expect("get response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_post() {
        let base_url = with_test_server(|request| {
            assert_method_path(request, "POST", "/items");
            assert!(request.contains("content-type: application/json"));
            assert!(request.contains("\"name\":\"soup\""));
        });
        let response = test_client(base_url)
            .post("/items", &serde_json::json!({ "name": "soup" }))
            .expect("post response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_post_form() {
        let base_url = with_test_server(|request| {
            assert_method_path(request, "POST", "/items");
            assert!(request.contains("content-type: application/x-www-form-urlencoded"));
            assert!(request.contains("name=soup"));
        });
        let response = test_client(base_url)
            .post_form("/items", &serde_json::json!([["name", "soup"]]))
            .expect("post form response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_post_unauthenticated() {
        let base_url = with_test_server(|request| {
            assert_method_path(request, "POST", "/login");
            assert!(request.contains("\"username\":\"chris\""));
        });
        let response = test_client(base_url)
            .post_unauthenticated("/login", &serde_json::json!({ "username": "chris" }))
            .expect("unauthenticated post response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_put() {
        let base_url = with_test_server(|request| {
            assert_method_path(request, "PUT", "/items/1");
            assert!(request.contains("\"name\":\"stew\""));
        });
        let response = test_client(base_url)
            .put("/items/1", &serde_json::json!({ "name": "stew" }))
            .expect("put response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_put_form() {
        let base_url = with_test_server(|request| {
            assert_method_path(request, "PUT", "/items/1");
            assert!(request.contains("name=stew"));
        });
        let response = test_client(base_url)
            .put_form("/items/1", &serde_json::json!([["name", "stew"]]))
            .expect("put form response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_patch() {
        let base_url = with_test_server(|request| {
            assert_method_path(request, "PATCH", "/items/1");
            assert!(request.contains("\"name\":\"bisque\""));
        });
        let response = test_client(base_url)
            .patch("/items/1", &serde_json::json!({ "name": "bisque" }))
            .expect("patch response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_patch_form() {
        let base_url = with_test_server(|request| {
            assert_method_path(request, "PATCH", "/items/1");
            assert!(request.contains("name=bisque"));
        });
        let response = test_client(base_url)
            .patch_form("/items/1", &serde_json::json!([["name", "bisque"]]))
            .expect("patch form response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_delete() {
        let base_url =
            with_test_server(|request| assert_method_path(request, "DELETE", "/items/1"));
        let response = test_client(base_url)
            .delete("/items/1")
            .expect("delete response");
        assert_eq!(response["ok"], true);
    }

    #[test]
    fn test_login() {
        let base_url = with_test_server(|request| {
            assert_method_path(request, "POST", "/login");
            assert!(request.contains("\"username\":\"chris\""));
        });
        let client = test_client_with_auth(
            base_url,
            AuthConfig {
                header: "Auth: {{token}}".to_string(),
                variables: HashMap::new(),
                login: Some(AuthFlowConfig {
                    endpoint: "/login".to_string(),
                    method: "POST".to_string(),
                    body: HashMap::from([("username".to_string(), "{{username}}".to_string())]),
                    store: HashMap::new(),
                }),
                refresh: None,
            },
        );

        client
            .login(&HashMap::from([(
                "username".to_string(),
                "chris".to_string(),
            )]))
            .expect("login response");
    }

    #[test]
    fn test_refresh_if_needed() {
        let client = test_client("http://127.0.0.1:1".to_string());
        assert!(!client.refresh_if_needed().expect("refresh is unsupported"));
    }
}
