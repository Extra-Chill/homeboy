use reqwest::blocking::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::Value;

use super::broker_auth::BROKER_TOKEN_HEADER;
use crate::core::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct BrokerEnvelope {
    success: bool,
    data: Option<Value>,
    error: Option<Value>,
}

/// Attach the paired broker bearer token, when present, to an outgoing broker
/// request. Sent via both the canonical header and `Authorization: Bearer` so
/// the request works through proxies that strip one or the other.
fn with_broker_token(builder: RequestBuilder, token: Option<&str>) -> RequestBuilder {
    match token {
        Some(token) if !token.trim().is_empty() => builder
            .header(BROKER_TOKEN_HEADER, token)
            .bearer_auth(token),
        _ => builder,
    }
}

pub(crate) fn post_json(
    client: &Client,
    base_url: &str,
    path: &str,
    body: Value,
    action: &str,
    token: Option<&str>,
) -> Result<Value> {
    let response = with_broker_token(
        client
            .post(format!("{}{}", base_url.trim_end_matches('/'), path))
            .json(&body),
        token,
    )
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
    let data = envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("broker response missing data"))?;
    canonical_broker_body(&data)
}

pub(crate) fn get_json(
    client: &Client,
    base_url: &str,
    path: &str,
    action: &str,
    token: Option<&str>,
) -> Result<Value> {
    let response = with_broker_token(
        client.get(format!("{}{}", base_url.trim_end_matches('/'), path)),
        token,
    )
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
    let data = envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("broker response missing data"))?;
    canonical_broker_body(&data)
}

fn canonical_broker_body(data: &Value) -> Result<Value> {
    data.get("body")
        .cloned()
        .ok_or_else(|| Error::internal_unexpected("broker response missing canonical data.body"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_broker_body_requires_data_body() {
        let err = canonical_broker_body(&json!({ "job": {} })).expect_err("reject legacy data");
        assert!(err.message.contains("data.body"));
    }

    #[test]
    fn canonical_broker_body_returns_nested_body() {
        let body =
            canonical_broker_body(&json!({ "body": { "job": { "id": "job-1" } } })).expect("body");
        assert_eq!(body["job"]["id"], "job-1");
    }
}
