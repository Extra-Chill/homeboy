//! Transport-neutral runner discovery, readiness, and API handshake resources.
//!
//! Runner API negotiation covers the serialized service contract only. Homeboy
//! binary provenance, daemon freshness, source ancestry, and Lab capability
//! admission remain independently evaluated implementation concerns.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const RUNNER_DESCRIPTOR_SCHEMA: &str = "homeboy/runner-descriptor/v1";
pub const RUNNER_CAPABILITIES_SCHEMA: &str = "homeboy/runner-capabilities/v1";
pub const RUNNER_READINESS_SCHEMA: &str = "homeboy/runner-readiness/v1";
pub const RUNNER_INSPECTION_SCHEMA: &str = "homeboy/runner-inspection/v1";
pub const RUNNER_API_HANDSHAKE_REQUEST_SCHEMA: &str = "homeboy/runner-api-handshake-request/v1";
pub const RUNNER_API_HANDSHAKE_RESPONSE_SCHEMA: &str = "homeboy/runner-api-handshake-response/v1";
pub const RUNNER_API_LIST_REQUEST_SCHEMA: &str = "homeboy/runner-api-list-request/v1";
pub const RUNNER_API_LIST_RESPONSE_SCHEMA: &str = "homeboy/runner-api-list-response/v1";
pub const RUNNER_API_INSPECT_REQUEST_SCHEMA: &str = "homeboy/runner-api-inspect-request/v1";
pub const RUNNER_API_INSPECT_RESPONSE_SCHEMA: &str = "homeboy/runner-api-inspect-response/v1";
pub const RUNNER_API_CAPABILITIES_REQUEST_SCHEMA: &str =
    "homeboy/runner-api-capabilities-request/v1";
pub const RUNNER_API_CAPABILITIES_RESPONSE_SCHEMA: &str =
    "homeboy/runner-api-capabilities-response/v1";
pub const RUNNER_API_READINESS_REQUEST_SCHEMA: &str = "homeboy/runner-api-readiness-request/v1";
pub const RUNNER_API_READINESS_RESPONSE_SCHEMA: &str = "homeboy/runner-api-readiness-response/v1";
pub const RUNNER_API_V1: RunnerApiVersion = RunnerApiVersion { major: 1 };
/// Lease-bound runner-service observation served by local and direct transports.
pub const RUNNER_SERVICE_OBSERVATION_SCHEMA: &str = "homeboy/runner-service-observation/v1";
pub const RUNNER_API_DESCRIBE_PATH: &str = "/runner/describe";

/// A transport-neutral Runner API major version.
///
/// The numeric shape lets an older client parse and explicitly reject a future
/// version instead of failing to deserialize an unknown enum variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunnerApiVersion {
    pub major: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiHandshakeRequest {
    pub schema: String,
    pub supported_versions: Vec<RunnerApiVersion>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerApiCompatibilityStatus {
    Compatible,
    Incompatible,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerApiCompatibilityFailureCode {
    InvalidHandshakeSchema,
    NoSharedApiVersion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiCompatibilityFailure {
    pub code: RunnerApiCompatibilityFailureCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiCompatibility {
    pub status: RunnerApiCompatibilityStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<RunnerApiCompatibilityFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiHandshakeResponse {
    pub schema: String,
    pub supported_versions: Vec<RunnerApiVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_version: Option<RunnerApiVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection: Option<RunnerInspection>,
    pub compatibility: RunnerApiCompatibility,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerApiOperationFailureCode {
    InvalidRequestSchema,
    UnsupportedApiVersion,
    RunnerNotFound,
    /// The referenced runner job does not exist (or is not a job id). Added
    /// for the watch operation (#13881); no existing code named a missing job.
    JobNotFound,
    /// An authenticated runner addressed a job it does not own. Added for the
    /// watch operation (#13881); the existing codes name lookup, not standing.
    RunnerNotAuthorized,
    SubmissionRejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiOperationFailure {
    pub code: RunnerApiOperationFailureCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiListRequest {
    pub schema: String,
    pub api_version: RunnerApiVersion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiListResponse {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub descriptors: Vec<RunnerDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunnerApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiInspectRequest {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiInspectResponse {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection: Option<RunnerInspection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunnerApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiCapabilitiesRequest {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiCapabilitiesResponse {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<RunnerCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunnerApiOperationFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiReadinessRequest {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerApiReadinessResponse {
    pub schema: String,
    pub api_version: RunnerApiVersion,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<RunnerReadiness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunnerApiOperationFailure>,
}

/// The implementation kind backing a runner definition.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    #[default]
    Local,
    Ssh,
}

/// Stable configured identity used for runner discovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerDescriptor {
    pub schema: String,
    pub runner_id: String,
    pub kind: RunnerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency_limit: Option<usize>,
}

/// Capabilities observed through the runner's capability transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerCapabilities {
    pub schema: String,
    pub runner_id: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub runtime_ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub capabilities: BTreeSet<String>,
}

/// Authoritative admission and capacity projection for one runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerReadiness {
    pub schema: String,
    pub runner_id: String,
    pub connected: bool,
    pub accepting_jobs: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_job_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// One complete read-only inspection assembled by the Runner API service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunnerInspection {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<crate::RunnerEnvironment>,
    pub descriptor: RunnerDescriptor,
    pub readiness: RunnerReadiness,
    pub capabilities: RunnerCapabilities,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn not_found() -> RunnerApiOperationFailure {
        RunnerApiOperationFailure {
            code: RunnerApiOperationFailureCode::RunnerNotFound,
            message: "Runner 'missing' was not found".to_string(),
        }
    }

    /// The Runner API is versioned on the wire: every request and response
    /// names its schema and API version, and absent optional data is omitted
    /// rather than serialized as null.
    #[test]
    fn runner_api_operations_keep_their_versioned_wire_shapes() {
        let not_found_json = json!({
            "code": "runner_not_found",
            "message": "Runner 'missing' was not found"
        });
        let cases = [
            (
                "incompatible handshake",
                serde_json::to_value(RunnerApiHandshakeResponse {
                    schema: RUNNER_API_HANDSHAKE_RESPONSE_SCHEMA.to_string(),
                    supported_versions: vec![RUNNER_API_V1],
                    selected_version: None,
                    inspection: None,
                    compatibility: RunnerApiCompatibility {
                        status: RunnerApiCompatibilityStatus::Incompatible,
                        failures: vec![RunnerApiCompatibilityFailure {
                            code: RunnerApiCompatibilityFailureCode::NoSharedApiVersion,
                            message: "no shared version".to_string(),
                        }],
                    },
                }),
                json!({
                    "schema": RUNNER_API_HANDSHAKE_RESPONSE_SCHEMA,
                    "supported_versions": [{ "major": 1 }],
                    "compatibility": {
                        "status": "incompatible",
                        "failures": [{
                            "code": "no_shared_api_version",
                            "message": "no shared version"
                        }]
                    }
                }),
            ),
            (
                "list success",
                serde_json::to_value(RunnerApiListResponse {
                    schema: RUNNER_API_LIST_RESPONSE_SCHEMA.to_string(),
                    api_version: RUNNER_API_V1,
                    descriptors: vec![RunnerDescriptor {
                        schema: RUNNER_DESCRIPTOR_SCHEMA.to_string(),
                        runner_id: "local".to_string(),
                        kind: RunnerKind::Local,
                        server_id: None,
                        workspace_root: None,
                        concurrency_limit: None,
                    }],
                    failure: None,
                }),
                json!({
                    "schema": RUNNER_API_LIST_RESPONSE_SCHEMA,
                    "api_version": { "major": 1 },
                    "descriptors": [{
                        "schema": RUNNER_DESCRIPTOR_SCHEMA,
                        "runner_id": "local",
                        "kind": "local"
                    }]
                }),
            ),
            (
                "inspect failure",
                serde_json::to_value(RunnerApiInspectResponse {
                    schema: RUNNER_API_INSPECT_RESPONSE_SCHEMA.to_string(),
                    api_version: RUNNER_API_V1,
                    runner_id: "missing".to_string(),
                    inspection: None,
                    failure: Some(not_found()),
                }),
                json!({
                    "schema": RUNNER_API_INSPECT_RESPONSE_SCHEMA,
                    "api_version": { "major": 1 },
                    "runner_id": "missing",
                    "failure": not_found_json
                }),
            ),
            (
                "capabilities request",
                serde_json::to_value(RunnerApiCapabilitiesRequest {
                    schema: RUNNER_API_CAPABILITIES_REQUEST_SCHEMA.to_string(),
                    api_version: RUNNER_API_V1,
                    runner_id: "local".to_string(),
                }),
                json!({
                    "schema": RUNNER_API_CAPABILITIES_REQUEST_SCHEMA,
                    "api_version": { "major": 1 },
                    "runner_id": "local"
                }),
            ),
            (
                "capabilities success",
                serde_json::to_value(RunnerApiCapabilitiesResponse {
                    schema: RUNNER_API_CAPABILITIES_RESPONSE_SCHEMA.to_string(),
                    api_version: RUNNER_API_V1,
                    runner_id: "local".to_string(),
                    capabilities: Some(RunnerCapabilities {
                        schema: RUNNER_CAPABILITIES_SCHEMA.to_string(),
                        runner_id: "local".to_string(),
                        runtime_ids: BTreeSet::from(["homeboy".to_string()]),
                        capabilities: BTreeSet::from(["cargo".to_string()]),
                    }),
                    failure: None,
                }),
                json!({
                    "schema": RUNNER_API_CAPABILITIES_RESPONSE_SCHEMA,
                    "api_version": { "major": 1 },
                    "runner_id": "local",
                    "capabilities": {
                        "schema": RUNNER_CAPABILITIES_SCHEMA,
                        "runner_id": "local",
                        "runtime_ids": ["homeboy"],
                        "capabilities": ["cargo"]
                    }
                }),
            ),
            (
                "readiness request",
                serde_json::to_value(RunnerApiReadinessRequest {
                    schema: RUNNER_API_READINESS_REQUEST_SCHEMA.to_string(),
                    api_version: RUNNER_API_V1,
                    runner_id: "local".to_string(),
                }),
                json!({
                    "schema": RUNNER_API_READINESS_REQUEST_SCHEMA,
                    "api_version": { "major": 1 },
                    "runner_id": "local"
                }),
            ),
            (
                "readiness success",
                serde_json::to_value(RunnerApiReadinessResponse {
                    schema: RUNNER_API_READINESS_RESPONSE_SCHEMA.to_string(),
                    api_version: RUNNER_API_V1,
                    runner_id: "local".to_string(),
                    readiness: Some(RunnerReadiness {
                        schema: RUNNER_READINESS_SCHEMA.to_string(),
                        runner_id: "local".to_string(),
                        connected: true,
                        accepting_jobs: true,
                        active_job_count: Some(0),
                        capacity: Some(2),
                        reasons: Vec::new(),
                    }),
                    failure: None,
                }),
                json!({
                    "schema": RUNNER_API_READINESS_RESPONSE_SCHEMA,
                    "api_version": { "major": 1 },
                    "runner_id": "local",
                    "readiness": {
                        "schema": RUNNER_READINESS_SCHEMA,
                        "runner_id": "local",
                        "connected": true,
                        "accepting_jobs": true,
                        "active_job_count": 0,
                        "capacity": 2
                    }
                }),
            ),
        ];
        for (name, actual, expected) in cases {
            assert_eq!(actual.expect(name), expected, "{name}");
        }
    }
}
