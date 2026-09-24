//! Transport-neutral Runner API v1 `watch` operation.
//!
//! A controller resumes a job's durable event log after a dropped connection
//! by repeating `watch` with the last `next_sequence` it received. The broker
//! returns every event with `sequence > after_sequence` exactly once, so the
//! resume rule needs no other client state (see #13881 step 1).

use homeboy_error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RunnerApiOperationFailure, RunnerApiVersion};

pub const RUNNER_API_WATCH_REQUEST_SCHEMA: &str = "homeboy/runner-api-watch-request/v1";
pub const RUNNER_API_WATCH_RESPONSE_SCHEMA: &str = "homeboy/runner-api-watch-response/v1";

/// Capability id a broker advertises when it serves the watch operation.
pub const RUNNER_API_WATCH_CAPABILITY: &str = "runner-api-watch";

/// The version of the watch operation contract currently served.
pub const RUNNER_API_WATCH_CAPABILITY_VERSION: u32 = 1;

/// The transport-neutral request to resume one job's event log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiWatchRequest {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
    pub job_id: String,
    /// Exclusive lower bound: only events with `sequence > after_sequence`
    /// are returned. `0` reads the whole retained log.
    pub after_sequence: u64,
    /// Optional page cap. When set, at most `limit` events are returned and
    /// `next_sequence` names the highest returned sequence so the next watch
    /// call continues without gaps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One durable job event projected onto the Runner API watch surface.
///
/// The core `JobEvent` stays in the jobs contract; this projection carries
/// its wire fields without a contract dependency edge in this direction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiWatchedEvent {
    pub sequence: u64,
    /// The `JobEventKind` wire string: `status`, `stdout`, `stderr`,
    /// `progress`, `result`, or `error`.
    pub kind: String,
    pub timestamp_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// The typed terminal outcome of a watched job.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerApiWatchTerminalOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiWatchResponse {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub job_id: String,
    /// Events with `sequence > after_sequence`, ascending. Empty when the
    /// client is already caught up (or on failure).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<RunnerApiWatchedEvent>,
    /// The highest returned sequence, or the request's `after_sequence` when
    /// no events were returned. A client that loses its connection repeats
    /// `watch` with this value and receives every later event exactly once.
    pub next_sequence: u64,
    /// Whether the job has reached a terminal status.
    pub terminal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_outcome: Option<RunnerApiWatchTerminalOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunnerApiOperationFailure>,
}

/// The watch operation advertisement carried in a capabilities response, in
/// the same `{capability, version}` shape the neighbouring protocols use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerApiWatchCapability {
    pub capability: String,
    pub version: u32,
}

impl RunnerApiWatchCapability {
    pub fn current() -> Self {
        Self {
            capability: RUNNER_API_WATCH_CAPABILITY.to_string(),
            version: RUNNER_API_WATCH_CAPABILITY_VERSION,
        }
    }

    pub fn verify(&self) -> Result<()> {
        (self.capability == RUNNER_API_WATCH_CAPABILITY
            && self.version == RUNNER_API_WATCH_CAPABILITY_VERSION)
            .then_some(())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "runner_api_watch_capability",
                    "broker does not advertise the required Runner API watch capability",
                    None,
                    None,
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RunnerApiOperationFailureCode, RUNNER_API_V1};

    #[test]
    fn watch_request_is_strict_and_versioned() {
        let request = RunnerApiWatchRequest {
            schema: RUNNER_API_WATCH_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "lab-1".to_string(),
            job_id: "job-1".to_string(),
            after_sequence: 7,
            limit: Some(2),
        };
        let encoded = serde_json::to_value(&request).expect("watch request JSON");
        assert_eq!(
            encoded,
            serde_json::json!({
                "schema": RUNNER_API_WATCH_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "lab-1",
                "job_id": "job-1",
                "after_sequence": 7,
                "limit": 2,
            })
        );
        assert!(serde_json::from_value::<RunnerApiWatchRequest>(encoded).is_ok());
        assert!(
            serde_json::from_value::<RunnerApiWatchRequest>(serde_json::json!({
                "schema": RUNNER_API_WATCH_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "lab-1", "job_id": "job-1", "after_sequence": 0,
                "unexpected": true,
            }))
            .is_err()
        );
        let unlimited = RunnerApiWatchRequest {
            limit: None,
            ..serde_json::from_value(serde_json::json!({
                "schema": RUNNER_API_WATCH_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "lab-1", "job_id": "job-1", "after_sequence": 0,
            }))
            .expect("limit defaults when omitted")
        };
        assert_eq!(unlimited.limit, None);
        assert!(serde_json::to_value(unlimited)
            .expect("unlimited request JSON")
            .get("limit")
            .is_none());
    }

    #[test]
    fn watch_response_pins_events_resume_and_terminal_wires() {
        let caught_up = RunnerApiWatchResponse {
            schema: RUNNER_API_WATCH_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            job_id: "job-1".to_string(),
            events: Vec::new(),
            next_sequence: 9,
            terminal: false,
            terminal_outcome: None,
            failure: None,
        };
        assert_eq!(
            serde_json::to_value(&caught_up).expect("caught-up JSON"),
            serde_json::json!({
                "schema": RUNNER_API_WATCH_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "job_id": "job-1",
                "next_sequence": 9,
                "terminal": false,
            })
        );

        let page = RunnerApiWatchResponse {
            schema: RUNNER_API_WATCH_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            job_id: "job-1".to_string(),
            events: vec![RunnerApiWatchedEvent {
                sequence: 4,
                kind: "progress".to_string(),
                timestamp_ms: 12,
                message: Some("compiling".to_string()),
                data: Some(serde_json::json!({ "percent": 50 })),
            }],
            next_sequence: 4,
            terminal: true,
            terminal_outcome: Some(RunnerApiWatchTerminalOutcome::Succeeded),
            failure: None,
        };
        let encoded = serde_json::to_value(&page).expect("page JSON");
        assert_eq!(encoded["events"][0]["sequence"], 4);
        assert_eq!(encoded["events"][0]["kind"], "progress");
        assert_eq!(encoded["events"][0]["timestamp_ms"], 12);
        assert_eq!(encoded["events"][0]["message"], "compiling");
        assert_eq!(encoded["events"][0]["data"]["percent"], 50);
        assert_eq!(encoded["terminal"], true);
        assert_eq!(encoded["terminal_outcome"], "succeeded");
        assert_eq!(
            serde_json::to_value(RunnerApiWatchTerminalOutcome::Failed)
                .expect("failed outcome JSON"),
            "failed"
        );
        assert_eq!(
            serde_json::to_value(RunnerApiWatchTerminalOutcome::Cancelled)
                .expect("cancelled outcome JSON"),
            "cancelled"
        );
        let decoded: RunnerApiWatchResponse =
            serde_json::from_value(encoded).expect("page round-trips");
        assert_eq!(decoded, page);
    }

    #[test]
    fn watch_failure_wire_uses_the_operation_failure_vocabulary() {
        let failure = RunnerApiWatchResponse {
            schema: RUNNER_API_WATCH_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            job_id: "missing".to_string(),
            events: Vec::new(),
            next_sequence: 0,
            terminal: false,
            terminal_outcome: None,
            failure: Some(RunnerApiOperationFailure {
                code: RunnerApiOperationFailureCode::JobNotFound,
                message: "job not found".to_string(),
            }),
        };
        assert_eq!(
            serde_json::to_value(&failure).expect("failure JSON"),
            serde_json::json!({
                "schema": RUNNER_API_WATCH_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "job_id": "missing",
                "next_sequence": 0,
                "terminal": false,
                "failure": { "code": "job_not_found", "message": "job not found" },
            })
        );
    }

    #[test]
    fn watch_capability_advertisement_keeps_the_protocol_shape() {
        let capability = RunnerApiWatchCapability::current();
        assert_eq!(capability.capability, RUNNER_API_WATCH_CAPABILITY);
        assert_eq!(
            serde_json::to_value(&capability).expect("capability JSON"),
            serde_json::json!({
                "capability": RUNNER_API_WATCH_CAPABILITY,
                "version": RUNNER_API_WATCH_CAPABILITY_VERSION,
            })
        );
        assert!(capability.verify().is_ok());
        assert!(RunnerApiWatchCapability {
            version: RUNNER_API_WATCH_CAPABILITY_VERSION + 1,
            ..capability.clone()
        }
        .verify()
        .is_err());
        let decoded: RunnerApiWatchCapability =
            serde_json::from_value(serde_json::to_value(capability).expect("capability JSON"))
                .expect("capability round-trips");
        assert_eq!(decoded.version, RUNNER_API_WATCH_CAPABILITY_VERSION);
    }
}
