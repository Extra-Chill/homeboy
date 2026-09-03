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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
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
    pub active_job_count: usize,
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
    pub descriptor: RunnerDescriptor,
    pub readiness: RunnerReadiness,
    pub capabilities: RunnerCapabilities,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_kind_keeps_the_established_wire_values() {
        assert_eq!(serde_json::to_value(RunnerKind::Local).unwrap(), "local");
        assert_eq!(serde_json::to_value(RunnerKind::Ssh).unwrap(), "ssh");
    }

    #[test]
    fn discovery_resources_are_versioned_and_omit_empty_optional_data() {
        let descriptor = RunnerDescriptor {
            schema: RUNNER_DESCRIPTOR_SCHEMA.to_string(),
            runner_id: "local".to_string(),
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: None,
            concurrency_limit: None,
        };
        let value = serde_json::to_value(descriptor).unwrap();

        assert_eq!(value["schema"], RUNNER_DESCRIPTOR_SCHEMA);
        assert_eq!(value["runner_id"], "local");
        assert_eq!(value["kind"], "local");
        assert!(value.get("server_id").is_none());
        assert!(value.get("concurrency_limit").is_none());
    }

    #[test]
    fn future_api_versions_remain_parseable() {
        let version: RunnerApiVersion =
            serde_json::from_value(serde_json::json!({ "major": 99 })).expect("api version");

        assert_eq!(version.major, 99);
    }

    #[test]
    fn incompatible_handshake_response_has_an_explicit_wire_shape() {
        let response = RunnerApiHandshakeResponse {
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
        };

        assert_eq!(
            serde_json::to_value(response).expect("handshake response JSON"),
            serde_json::json!({
                "schema": RUNNER_API_HANDSHAKE_RESPONSE_SCHEMA,
                "supported_versions": [{ "major": 1 }],
                "compatibility": {
                    "status": "incompatible",
                    "failures": [{
                        "code": "no_shared_api_version",
                        "message": "no shared version"
                    }]
                }
            })
        );
    }

    #[test]
    fn list_success_wire_shape_is_explicitly_versioned() {
        let response = RunnerApiListResponse {
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
        };

        assert_eq!(
            serde_json::to_value(response).expect("list response JSON"),
            serde_json::json!({
                "schema": RUNNER_API_LIST_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "descriptors": [{
                    "schema": RUNNER_DESCRIPTOR_SCHEMA,
                    "runner_id": "local",
                    "kind": "local"
                }]
            })
        );
    }

    #[test]
    fn inspect_failure_wire_shape_omits_inspection() {
        let response = RunnerApiInspectResponse {
            schema: RUNNER_API_INSPECT_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "missing".to_string(),
            inspection: None,
            failure: Some(RunnerApiOperationFailure {
                code: RunnerApiOperationFailureCode::RunnerNotFound,
                message: "Runner 'missing' was not found".to_string(),
            }),
        };

        assert_eq!(
            serde_json::to_value(response).expect("inspect response JSON"),
            serde_json::json!({
                "schema": RUNNER_API_INSPECT_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "missing",
                "failure": {
                    "code": "runner_not_found",
                    "message": "Runner 'missing' was not found"
                }
            })
        );
    }

    #[test]
    fn capabilities_operation_wire_shapes_are_explicitly_versioned() {
        let request = RunnerApiCapabilitiesRequest {
            schema: RUNNER_API_CAPABILITIES_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "local".to_string(),
        };
        assert_eq!(
            serde_json::to_value(request).expect("capabilities request JSON"),
            serde_json::json!({
                "schema": RUNNER_API_CAPABILITIES_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "local"
            })
        );

        let success = RunnerApiCapabilitiesResponse {
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
        };
        assert_eq!(
            serde_json::to_value(success).expect("capabilities success JSON"),
            serde_json::json!({
                "schema": RUNNER_API_CAPABILITIES_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "local",
                "capabilities": {
                    "schema": RUNNER_CAPABILITIES_SCHEMA,
                    "runner_id": "local",
                    "runtime_ids": ["homeboy"],
                    "capabilities": ["cargo"]
                }
            })
        );

        let failure = RunnerApiCapabilitiesResponse {
            schema: RUNNER_API_CAPABILITIES_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "missing".to_string(),
            capabilities: None,
            failure: Some(RunnerApiOperationFailure {
                code: RunnerApiOperationFailureCode::RunnerNotFound,
                message: "Runner 'missing' was not found".to_string(),
            }),
        };
        assert_eq!(
            serde_json::to_value(failure).expect("capabilities failure JSON"),
            serde_json::json!({
                "schema": RUNNER_API_CAPABILITIES_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "missing",
                "failure": {
                    "code": "runner_not_found",
                    "message": "Runner 'missing' was not found"
                }
            })
        );
    }

    #[test]
    fn readiness_operation_wire_shapes_are_explicitly_versioned() {
        let request = RunnerApiReadinessRequest {
            schema: RUNNER_API_READINESS_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "local".to_string(),
        };
        assert_eq!(
            serde_json::to_value(request).expect("readiness request JSON"),
            serde_json::json!({
                "schema": RUNNER_API_READINESS_REQUEST_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "local"
            })
        );

        let success = RunnerApiReadinessResponse {
            schema: RUNNER_API_READINESS_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "local".to_string(),
            readiness: Some(RunnerReadiness {
                schema: RUNNER_READINESS_SCHEMA.to_string(),
                runner_id: "local".to_string(),
                connected: true,
                accepting_jobs: true,
                active_job_count: 0,
                capacity: Some(2),
                reasons: Vec::new(),
            }),
            failure: None,
        };
        assert_eq!(
            serde_json::to_value(success).expect("readiness success JSON"),
            serde_json::json!({
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
            })
        );

        let failure = RunnerApiReadinessResponse {
            schema: RUNNER_API_READINESS_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "missing".to_string(),
            readiness: None,
            failure: Some(RunnerApiOperationFailure {
                code: RunnerApiOperationFailureCode::RunnerNotFound,
                message: "Runner 'missing' was not found".to_string(),
            }),
        };
        assert_eq!(
            serde_json::to_value(failure).expect("readiness failure JSON"),
            serde_json::json!({
                "schema": RUNNER_API_READINESS_RESPONSE_SCHEMA,
                "api_version": { "major": 1 },
                "runner_id": "missing",
                "failure": {
                    "code": "runner_not_found",
                    "message": "Runner 'missing' was not found"
                }
            })
        );
    }
}
