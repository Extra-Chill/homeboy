use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::core::error::{Error, Result};

use super::super::broker_http;
use super::super::daemon_http_get::daemon_get;
use super::super::{load, status, RunnerTunnelMode};

#[allow(unused_imports)]
use super::*;

fn unsupported_daemon_api_method(method: &str) -> Error {
    Error::internal_unexpected(format!("unsupported daemon API method {method}"))
}

pub(crate) fn canonical_daemon_body<'a>(data: &'a Value, context: &str) -> Result<&'a Value> {
    data.get("body")
        .ok_or_else(|| Error::internal_unexpected(format!("{context} missing canonical data.body")))
}

pub(crate) fn daemon_api_get(runner_id: &str, path: &str) -> Result<Value> {
    daemon_api_request(runner_id, path, "GET")
}

pub fn daemon_api_post(runner_id: &str, path: &str) -> Result<Value> {
    daemon_api_request(runner_id, path, "POST")
}

pub(super) fn daemon_api_request(runner_id: &str, path: &str, method: &str) -> Result<Value> {
    let runner = load(runner_id)?;
    let connected = status(runner_id)?;
    let Some(session) = connected.session.filter(|_| connected.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` first",
            Some(runner.id),
            Some(vec![
                "Read/query integrations use the connected daemon so results come from the runner machine.".to_string(),
            ]),
        ));
    };
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    if let Some(local_url) = session.local_url.as_deref() {
        return match method {
            "GET" => daemon_get(&client, local_url, path),
            "POST" => daemon_post(&client, local_url, path),
            _ => Err(unsupported_daemon_api_method(method)),
        };
    }
    if session.mode == RunnerTunnelMode::Reverse {
        let Some(broker_url) = session.broker_url.as_deref() else {
            return Err(Error::validation_invalid_argument(
                "runner",
                "reverse runner session does not expose a broker URL",
                Some(runner.id),
                Some(vec![
                    "Reconnect the reverse runner with `homeboy runner connect <controller-id> --reverse --reverse-runner <runner-id> --broker-url <url>`.".to_string(),
                ]),
            ));
        };
        let broker_token = super::super::broker_auth::broker_token_from_env();
        return match method {
            "GET" => broker_http::get_json(
                &client,
                broker_url,
                path,
                "query reverse runner broker",
                broker_token.as_deref(),
            ),
            "POST" => broker_http::post_json(
                &client,
                broker_url,
                path,
                json!({}),
                "query reverse runner broker",
                broker_token.as_deref(),
            ),
            _ => Err(unsupported_daemon_api_method(method)),
        };
    }
    Err(Error::validation_invalid_argument(
        "runner",
        "runner session does not expose a local daemon URL or reverse broker URL",
        Some(runner.id),
        Some(vec![
            "Use a direct daemon connection or a reverse runner session registered with a broker before querying runner jobs.".to_string(),
        ]),
    ))
}

pub(super) fn daemon_post(client: &Client, local_url: &str, path: &str) -> Result<Value> {
    let response = client
        .post(format!("{}{}", local_url.trim_end_matches('/'), path))
        .send()
        .map_err(|err| Error::internal_unexpected(format!("query runner daemon: {err}")))?;
    let envelope: DaemonEnvelope = response.json().map_err(|err| {
        Error::internal_json(err.to_string(), Some("parse daemon response".to_string()))
    })?;
    if !envelope.success {
        return Err(Error::internal_unexpected(format!(
            "daemon request failed: {}",
            envelope.error.unwrap_or(Value::Null)
        )));
    }
    envelope
        .data
        .ok_or_else(|| Error::internal_unexpected("daemon response missing data"))
}
