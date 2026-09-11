//! Blocking HTTP client for the versioned Homeboy control-plane API.

use std::fmt;
use std::time::Duration;

use homeboy_control_plane_contract::{
    ControlPlaneError, ControlPlaneEventPage, ControlPlaneResult, ControlPlaneRun, EventCursor,
    RunId,
};
use reqwest::blocking::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use serde::Deserialize;

#[derive(Debug)]
pub enum ClientError {
    Transport(String),
    Decode(String),
    Protocol(String),
    ControlPlane(ControlPlaneError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(message) | Self::Decode(message) | Self::Protocol(message) => {
                formatter.write_str(message)
            }
            Self::ControlPlane(error) => formatter.write_str(&error.message),
        }
    }
}

impl std::error::Error for ClientError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonEnvelope {
    status: u16,
    endpoint: String,
    body: serde_json::Value,
}

pub struct ControlPlaneClient {
    base_url: String,
    client: Client,
    token: Option<String>,
}

impl ControlPlaneClient {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ClientError> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client,
            token: None,
        })
    }

    pub fn new_local(base_url: impl Into<String>, timeout: Duration) -> Result<Self, ClientError> {
        let client = Client::builder()
            .no_proxy()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| ClientError::Transport(error.to_string()))?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client,
            token: None,
        })
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn run(&self, run: &RunId) -> Result<ControlPlaneRun, ClientError> {
        self.get(&format!("/v1/control-plane/runs/{run}"), None)
    }

    pub fn events(
        &self,
        run: &RunId,
        cursor: Option<&EventCursor>,
    ) -> Result<ControlPlaneEventPage, ClientError> {
        self.get(
            &format!("/v1/control-plane/runs/{run}/events"),
            cursor.map(|cursor| ("cursor", cursor.as_str())),
        )
    }

    /// Consume all currently available pages. The cursor advances only after
    /// the caller accepts every event in a page, so reconnects can safely replay.
    pub fn consume_available_events(
        &self,
        run: &RunId,
        mut cursor: Option<EventCursor>,
        mut consume: impl FnMut(
            &homeboy_control_plane_contract::ControlPlaneEvent,
        ) -> Result<(), ClientError>,
    ) -> Result<Option<EventCursor>, ClientError> {
        loop {
            let page = self.events(run, cursor.as_ref())?;
            for event in &page.events {
                consume(event)?;
            }
            if let Some(next) = page.next_cursor {
                cursor = Some(next);
            }
            if !page.has_more {
                return Ok(cursor);
            }
        }
    }

    fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Option<(&str, &str)>,
    ) -> Result<T, ClientError> {
        let mut request = self.client.get(format!("{}{}", self.base_url, path));
        if let Some(query) = query {
            request = request.query(&[query]);
        }
        let response = self.authenticate(request).send().map_err(|error| {
            ClientError::Transport(format!("control-plane GET {path} failed: {error}"))
        })?;
        let http_status = response.status().as_u16();
        let envelope: DaemonEnvelope = response
            .json()
            .map_err(|error| ClientError::Decode(format!("decode {path}: {error}")))?;
        if envelope.status != http_status {
            return Err(ClientError::Protocol(format!(
                "control-plane response status mismatch for {}: HTTP {} != envelope {}",
                envelope.endpoint, http_status, envelope.status
            )));
        }
        let result: ControlPlaneResult<T> =
            serde_json::from_value(envelope.body).map_err(|error| {
                ClientError::Decode(format!("decode {} body: {error}", envelope.endpoint))
            })?;
        match (result.ok, result.resource, result.error) {
            (true, Some(resource), None) => Ok(resource),
            (false, None, Some(error)) => Err(ClientError::ControlPlane(error)),
            _ => Err(ClientError::Protocol(format!(
                "control-plane response for {} has an incoherent result envelope",
                envelope.endpoint
            ))),
        }
    }

    fn authenticate(&self, request: RequestBuilder) -> RequestBuilder {
        match self.token.as_deref() {
            Some(token) => request
                .header("x-homeboy-broker-token", token)
                .bearer_auth(token),
            None => request,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_control_plane_contract::{
        ControlPlaneErrorClass, ControlPlaneRunState, CONTROL_PLANE_EVENT_PAGE_SCHEMA,
        CONTROL_PLANE_RESULT_SCHEMA,
    };
    use tiny_http::{Header, Response, Server};

    #[test]
    fn reads_the_real_daemon_and_control_plane_envelopes() {
        let server = Server::http("127.0.0.1:0").expect("server");
        let address = server.server_addr();
        let thread =
            std::thread::spawn(move || {
                let request = server.recv().expect("request");
                assert_eq!(request.url(), "/v1/control-plane/runs/run-1");
                assert_eq!(
                    request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("x-homeboy-broker-token"))
                        .map(|header| header.value.as_str()),
                    Some("secret")
                );
                let mut run = ControlPlaneRun::new(RunId::new("run-1").unwrap());
                run.state = ControlPlaneRunState::Succeeded;
                run.created_at = "2026-09-08T00:00:00Z".to_string();
                let body = serde_json::json!({
                    "status": 200,
                    "endpoint": "control_plane_run",
                    "body": {
                        "schema": CONTROL_PLANE_RESULT_SCHEMA,
                        "ok": true,
                        "resource": run
                    }
                })
                .to_string();
                request
                    .respond(Response::from_string(body).with_header(
                        Header::from_bytes("content-type", "application/json").unwrap(),
                    ))
                    .unwrap();
            });

        let run = ControlPlaneClient::new(format!("http://{address}"))
            .unwrap()
            .with_token("secret")
            .run(&RunId::new("run-1").unwrap())
            .unwrap();
        thread.join().unwrap();
        assert_eq!(run.state, ControlPlaneRunState::Succeeded);
    }

    #[test]
    fn event_pages_resume_with_the_last_accepted_opaque_cursor() {
        let server = Server::http("127.0.0.1:0").expect("server");
        let address = server.server_addr();
        let thread = std::thread::spawn(move || {
            for (expected_url, next_cursor, has_more) in [
                (
                    "/v1/control-plane/runs/run-1/events",
                    Some("opaque-1"),
                    true,
                ),
                (
                    "/v1/control-plane/runs/run-1/events?cursor=opaque-1",
                    Some("opaque-2"),
                    false,
                ),
            ] {
                let request = server.recv().expect("request");
                assert_eq!(request.url(), expected_url);
                let body = serde_json::json!({
                    "status": 200,
                    "endpoint": "control_plane_events",
                    "body": {
                        "schema": CONTROL_PLANE_RESULT_SCHEMA,
                        "ok": true,
                        "resource": {
                            "schema": CONTROL_PLANE_EVENT_PAGE_SCHEMA,
                            "run": "run-1",
                            "events": [],
                            "next_cursor": next_cursor,
                            "has_more": has_more
                        }
                    }
                })
                .to_string();
                request.respond(Response::from_string(body)).unwrap();
            }
        });

        let cursor = ControlPlaneClient::new(format!("http://{address}"))
            .unwrap()
            .consume_available_events(&RunId::new("run-1").unwrap(), None, |_| Ok(()))
            .unwrap();
        thread.join().unwrap();

        assert_eq!(cursor.as_ref().map(EventCursor::as_str), Some("opaque-2"));
    }

    #[test]
    fn cursor_expiry_remains_a_typed_non_retryable_error() {
        let server = Server::http("127.0.0.1:0").expect("server");
        let address = server.server_addr();
        let thread = std::thread::spawn(move || {
            let request = server.recv().expect("request");
            let body = serde_json::json!({
                "status": 410,
                "endpoint": "control_plane_events",
                "body": {
                    "schema": CONTROL_PLANE_RESULT_SCHEMA,
                    "ok": false,
                    "error": {
                        "class": "cursor_expired",
                        "retryable": false,
                        "message": "cursor expired"
                    }
                }
            })
            .to_string();
            request
                .respond(Response::from_string(body).with_status_code(410))
                .unwrap();
        });

        let error = ControlPlaneClient::new(format!("http://{address}"))
            .unwrap()
            .events(
                &RunId::new("run-1").unwrap(),
                Some(&EventCursor::new("expired").unwrap()),
            )
            .expect_err("expired cursor");
        thread.join().unwrap();

        match error {
            ClientError::ControlPlane(error) => {
                assert_eq!(error.class, ControlPlaneErrorClass::CursorExpired);
                assert!(!error.retryable);
            }
            other => panic!("expected typed control-plane error, got {other}"),
        }
    }

    #[test]
    fn authenticated_requests_do_not_follow_redirects() {
        let destination = Server::http("127.0.0.1:0").expect("destination server");
        let destination_address = destination.server_addr();
        let source = Server::http("127.0.0.1:0").expect("source server");
        let source_address = source.server_addr();
        let thread = std::thread::spawn(move || {
            let request = source.recv().expect("request");
            request
                .respond(
                    Response::empty(302).with_header(
                        Header::from_bytes(
                            "location",
                            format!("http://{destination_address}/stolen"),
                        )
                        .unwrap(),
                    ),
                )
                .unwrap();
        });

        let error = ControlPlaneClient::new(format!("http://{source_address}"))
            .unwrap()
            .with_token("secret")
            .run(&RunId::new("run-1").unwrap())
            .unwrap_err();
        thread.join().unwrap();
        assert!(matches!(error, ClientError::Decode(_)));
        assert!(destination
            .recv_timeout(Duration::from_millis(50))
            .unwrap()
            .is_none());
    }
}
