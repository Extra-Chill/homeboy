use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::ConfigManager;
use crate::http::ApiClient;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiOutput {
    pub project_id: String,
    pub method: String,
    pub endpoint: String,
    pub response: Value,
}

/// Single entry point for API requests.
///
/// Input is always JSON:
/// ```json
/// {"projectId": "my-project", "method": "GET", "endpoint": "/wp/v2/posts", "body": null}
/// ```
pub fn run(input: &str) -> crate::Result<(ApiOutput, i32)> {
    let parsed: ApiInput = serde_json::from_str(input)
        .map_err(|e| crate::Error::validation_invalid_json(e, Some("parse api input".to_string())))?;

    let project = ConfigManager::load_project(&parsed.project_id)?;
    let client = ApiClient::new(&parsed.project_id, &project.api)?;

    let body = parsed.body.unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    let response = match parsed.method.to_uppercase().as_str() {
        "GET" => client.get(&parsed.endpoint)?,
        "POST" => client.post(&parsed.endpoint, &body)?,
        "PUT" => client.put(&parsed.endpoint, &body)?,
        "PATCH" => client.patch(&parsed.endpoint, &body)?,
        "DELETE" => client.delete(&parsed.endpoint)?,
        _ => {
            return Err(crate::Error::validation_invalid_argument(
                "method",
                &format!("Invalid HTTP method: {}", parsed.method),
                None,
                Some(vec!["GET".into(), "POST".into(), "PUT".into(), "PATCH".into(), "DELETE".into()]),
            ));
        }
    };

    Ok((
        ApiOutput {
            project_id: parsed.project_id,
            method: parsed.method.to_uppercase(),
            endpoint: parsed.endpoint,
            response,
        },
        0,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiInput {
    project_id: String,
    method: String,
    endpoint: String,
    body: Option<Value>,
}
