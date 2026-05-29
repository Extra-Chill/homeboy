use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::Value;

use crate::core::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct BrokerEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

pub(crate) fn post_json(
    client: &Client,
    base_url: &str,
    path: &str,
    body: Value,
    action: &str,
) -> Result<Value> {
    let response = client
        .post(format!("{}{}", base_url.trim_end_matches('/'), path))
        .json(&body)
        .send()
        .map_err(|err| Error::internal_unexpected(format!("{action}: {err}")))?;
    let status_code = response.status().as_u16();
    let envelope: BrokerEnvelope = response.json().map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse broker response".to_string()))
    })?;
    if status_code >= 400 || !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "broker request failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("broker response missing data"))
}
