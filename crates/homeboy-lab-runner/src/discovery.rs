//! Canonical read-only Runner API service.

use homeboy_core::{error::ErrorCode, Result};
use homeboy_runner_contract::{
    RunnerApiCompatibility, RunnerApiCompatibilityFailure, RunnerApiCompatibilityFailureCode,
    RunnerApiCompatibilityStatus, RunnerApiHandshakeRequest, RunnerApiHandshakeResponse,
    RunnerApiInspectRequest, RunnerApiInspectResponse, RunnerApiListRequest, RunnerApiListResponse,
    RunnerApiOperationFailure, RunnerApiOperationFailureCode, RunnerApiVersion, RunnerCapabilities,
    RunnerDescriptor, RunnerInspection, RunnerKind, RunnerReadiness,
    RUNNER_API_HANDSHAKE_REQUEST_SCHEMA, RUNNER_API_HANDSHAKE_RESPONSE_SCHEMA,
    RUNNER_API_INSPECT_REQUEST_SCHEMA, RUNNER_API_INSPECT_RESPONSE_SCHEMA,
    RUNNER_API_LIST_REQUEST_SCHEMA, RUNNER_API_LIST_RESPONSE_SCHEMA, RUNNER_API_V1,
    RUNNER_CAPABILITIES_SCHEMA, RUNNER_DESCRIPTOR_SCHEMA, RUNNER_INSPECTION_SCHEMA,
    RUNNER_READINESS_SCHEMA,
};

use crate::{Runner, RunnerAdmissionSnapshot};

pub struct RunnerDiscoveryService;

const SUPPORTED_RUNNER_API_VERSIONS: &[RunnerApiVersion] = &[RUNNER_API_V1];

impl RunnerDiscoveryService {
    pub fn list() -> Result<Vec<RunnerDescriptor>> {
        crate::list().map(|runners| runners.iter().map(descriptor).collect())
    }

    pub fn inspect(runner_id: &str) -> Result<RunnerInspection> {
        Ok(RunnerInspection {
            schema: RUNNER_INSPECTION_SCHEMA.to_string(),
            descriptor: Self::descriptor(runner_id)?,
            readiness: Self::readiness(runner_id)?,
            capabilities: Self::capabilities(runner_id)?,
        })
    }

    pub fn descriptor(runner_id: &str) -> Result<RunnerDescriptor> {
        crate::load(runner_id).map(|runner| descriptor(&runner))
    }

    pub fn readiness(runner_id: &str) -> Result<RunnerReadiness> {
        let runner = crate::load(runner_id)?;
        if runner.kind == RunnerKind::Local {
            return Ok(RunnerReadiness {
                schema: RUNNER_READINESS_SCHEMA.to_string(),
                runner_id: runner.id,
                connected: true,
                accepting_jobs: true,
                active_job_count: 0,
                capacity: runner.settings.concurrency_limit,
                reasons: Vec::new(),
            });
        }
        Ok(readiness(
            &runner,
            crate::runner_admission_snapshot(runner_id)?,
        ))
    }

    pub fn capabilities(runner_id: &str) -> Result<RunnerCapabilities> {
        let inventory = crate::runner_capability_inventory(runner_id)?;
        Ok(RunnerCapabilities {
            schema: RUNNER_CAPABILITIES_SCHEMA.to_string(),
            runner_id: runner_id.to_string(),
            runtime_ids: inventory.runtime_ids,
            capabilities: inventory.capabilities,
        })
    }

    /// Negotiate the transport-neutral Runner API before serving inspection data.
    pub fn negotiate(
        runner_id: &str,
        request: &RunnerApiHandshakeRequest,
    ) -> Result<RunnerApiHandshakeResponse> {
        let supported_versions = SUPPORTED_RUNNER_API_VERSIONS.to_vec();
        let valid_schema = request.schema == RUNNER_API_HANDSHAKE_REQUEST_SCHEMA;
        let selected_version = valid_schema
            .then(|| {
                request
                    .supported_versions
                    .iter()
                    .filter(|version| SUPPORTED_RUNNER_API_VERSIONS.contains(version))
                    .max()
                    .copied()
            })
            .flatten();
        let failures = if !valid_schema {
            vec![RunnerApiCompatibilityFailure {
                code: RunnerApiCompatibilityFailureCode::InvalidHandshakeSchema,
                message: format!(
                    "Unsupported Runner API handshake schema '{}'; expected '{}'",
                    request.schema, RUNNER_API_HANDSHAKE_REQUEST_SCHEMA
                ),
            }]
        } else if selected_version.is_none() {
            vec![RunnerApiCompatibilityFailure {
                code: RunnerApiCompatibilityFailureCode::NoSharedApiVersion,
                message: "Client and runner do not share a Runner API major version".to_string(),
            }]
        } else {
            Vec::new()
        };
        let compatibility = RunnerApiCompatibility {
            status: if failures.is_empty() {
                RunnerApiCompatibilityStatus::Compatible
            } else {
                RunnerApiCompatibilityStatus::Incompatible
            },
            failures,
        };
        let inspection = if selected_version.is_some() {
            Some(Self::inspect(runner_id)?)
        } else {
            None
        };

        Ok(RunnerApiHandshakeResponse {
            schema: RUNNER_API_HANDSHAKE_RESPONSE_SCHEMA.to_string(),
            supported_versions,
            selected_version,
            inspection,
            compatibility,
        })
    }

    pub fn list_api(request: &RunnerApiListRequest) -> Result<RunnerApiListResponse> {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            RUNNER_API_LIST_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return Ok(list_failure(failure));
        }

        Ok(RunnerApiListResponse {
            schema: RUNNER_API_LIST_RESPONSE_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            descriptors: Self::list()?,
            failure: None,
        })
    }

    pub fn inspect_api(request: &RunnerApiInspectRequest) -> Result<RunnerApiInspectResponse> {
        if let Some(failure) = validate_operation_request(
            &request.schema,
            RUNNER_API_INSPECT_REQUEST_SCHEMA,
            request.api_version,
        ) {
            return Ok(inspect_failure(request, failure));
        }

        match Self::inspect(&request.runner_id) {
            Ok(inspection) => Ok(RunnerApiInspectResponse {
                schema: RUNNER_API_INSPECT_RESPONSE_SCHEMA.to_string(),
                api_version: RUNNER_API_V1,
                runner_id: request.runner_id.clone(),
                inspection: Some(inspection),
                failure: None,
            }),
            Err(error) if error.code == ErrorCode::RunnerNotFound => Ok(inspect_failure(
                request,
                operation_failure(RunnerApiOperationFailureCode::RunnerNotFound, error.message),
            )),
            Err(error) => Err(error),
        }
    }
}

fn validate_operation_request(
    actual_schema: &str,
    expected_schema: &str,
    api_version: RunnerApiVersion,
) -> Option<RunnerApiOperationFailure> {
    if actual_schema != expected_schema {
        return Some(operation_failure(
            RunnerApiOperationFailureCode::InvalidRequestSchema,
            format!("Unsupported request schema '{actual_schema}'; expected '{expected_schema}'"),
        ));
    }
    (api_version != RUNNER_API_V1).then(|| {
        operation_failure(
            RunnerApiOperationFailureCode::UnsupportedApiVersion,
            format!(
                "Runner API major {} is not supported; Homeboy supports major {}",
                api_version.major, RUNNER_API_V1.major
            ),
        )
    })
}

fn list_failure(failure: RunnerApiOperationFailure) -> RunnerApiListResponse {
    RunnerApiListResponse {
        schema: RUNNER_API_LIST_RESPONSE_SCHEMA.to_string(),
        api_version: RUNNER_API_V1,
        descriptors: Vec::new(),
        failure: Some(failure),
    }
}

fn inspect_failure(
    request: &RunnerApiInspectRequest,
    failure: RunnerApiOperationFailure,
) -> RunnerApiInspectResponse {
    RunnerApiInspectResponse {
        schema: RUNNER_API_INSPECT_RESPONSE_SCHEMA.to_string(),
        api_version: RUNNER_API_V1,
        runner_id: request.runner_id.clone(),
        inspection: None,
        failure: Some(failure),
    }
}

fn operation_failure(
    code: RunnerApiOperationFailureCode,
    message: String,
) -> RunnerApiOperationFailure {
    RunnerApiOperationFailure { code, message }
}

fn descriptor(runner: &Runner) -> RunnerDescriptor {
    RunnerDescriptor {
        schema: RUNNER_DESCRIPTOR_SCHEMA.to_string(),
        runner_id: runner.id.clone(),
        kind: runner.kind.clone(),
        server_id: runner.server_id.clone(),
        workspace_root: runner.workspace_root.clone(),
        concurrency_limit: runner.settings.concurrency_limit,
    }
}

fn readiness(runner: &Runner, snapshot: RunnerAdmissionSnapshot) -> RunnerReadiness {
    let availability = snapshot
        .status
        .admission_availability(runner.settings.concurrency_limit);
    let mut reasons = availability.reasons;
    if !snapshot.summary.daemon_compatible {
        reasons.push("daemon_incompatible".to_string());
    }
    if !snapshot.summary.daemon_fresh {
        reasons.push("daemon_not_fresh".to_string());
    }
    if !snapshot.summary.admission_blocking_job_ids.is_empty() {
        reasons.push("retained_job_owners".to_string());
    }
    if snapshot.summary.stale_job_count > 0 {
        reasons.push("stale_jobs".to_string());
    }
    if !snapshot.summary.accepting_jobs && reasons.is_empty() {
        reasons.push("admission_blocked".to_string());
    }
    reasons.sort();
    reasons.dedup();

    RunnerReadiness {
        schema: RUNNER_READINESS_SCHEMA.to_string(),
        runner_id: snapshot.summary.runner_id,
        connected: snapshot.summary.connected,
        accepting_jobs: snapshot.summary.accepting_jobs,
        active_job_count: snapshot.summary.active_job_count,
        capacity: availability.capacity,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_runner_descriptor_uses_the_canonical_resource() {
        let runner = crate::builtin_local_runner();
        let descriptor = descriptor(&runner);

        assert_eq!(descriptor.schema, RUNNER_DESCRIPTOR_SCHEMA);
        assert_eq!(descriptor.runner_id, "local");
        assert_eq!(descriptor.kind, RunnerKind::Local);
    }

    #[test]
    fn handshake_selects_the_highest_shared_version_and_returns_inspection() {
        let response = RunnerDiscoveryService::negotiate(
            "local",
            &RunnerApiHandshakeRequest {
                schema: RUNNER_API_HANDSHAKE_REQUEST_SCHEMA.to_string(),
                supported_versions: vec![RunnerApiVersion { major: 0 }, RUNNER_API_V1],
            },
        )
        .expect("negotiate Runner API");

        assert_eq!(response.selected_version, Some(RUNNER_API_V1));
        assert_eq!(
            response.compatibility.status,
            RunnerApiCompatibilityStatus::Compatible
        );
        assert_eq!(
            response
                .inspection
                .expect("compatible inspection")
                .descriptor
                .runner_id,
            "local"
        );
    }

    #[test]
    fn invalid_or_disjoint_handshakes_fail_before_runner_lookup() {
        let fixtures = [
            (
                "homeboy/runner-api-handshake-request/v99",
                vec![RUNNER_API_V1],
                RunnerApiCompatibilityFailureCode::InvalidHandshakeSchema,
            ),
            (
                RUNNER_API_HANDSHAKE_REQUEST_SCHEMA,
                vec![RunnerApiVersion { major: 99 }],
                RunnerApiCompatibilityFailureCode::NoSharedApiVersion,
            ),
        ];

        for (schema, supported_versions, expected_failure) in fixtures {
            let response = RunnerDiscoveryService::negotiate(
                "runner-that-does-not-exist",
                &RunnerApiHandshakeRequest {
                    schema: schema.to_string(),
                    supported_versions,
                },
            )
            .expect("incompatibility response");

            assert_eq!(response.selected_version, None);
            assert_eq!(response.inspection, None);
            assert_eq!(
                response.compatibility.status,
                RunnerApiCompatibilityStatus::Incompatible
            );
            assert_eq!(response.compatibility.failures[0].code, expected_failure);
        }
    }

    #[test]
    fn list_api_returns_canonical_descriptors() {
        let response = RunnerDiscoveryService::list_api(&RunnerApiListRequest {
            schema: RUNNER_API_LIST_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
        })
        .expect("list Runner API");

        assert_eq!(response.api_version, RUNNER_API_V1);
        assert!(response.failure.is_none());
        assert!(response
            .descriptors
            .iter()
            .any(|descriptor| descriptor.runner_id == "local"));
    }

    #[test]
    fn inspect_api_returns_the_canonical_inspection() {
        let response = RunnerDiscoveryService::inspect_api(&RunnerApiInspectRequest {
            schema: RUNNER_API_INSPECT_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "local".to_string(),
        })
        .expect("inspect Runner API");

        assert!(response.failure.is_none());
        assert_eq!(
            response
                .inspection
                .expect("inspection")
                .descriptor
                .runner_id,
            "local"
        );
    }

    #[test]
    fn discovery_operations_validate_before_runner_lookup() {
        let fixtures = [
            (
                "homeboy/runner-api-inspect-request/v99",
                RUNNER_API_V1,
                RunnerApiOperationFailureCode::InvalidRequestSchema,
            ),
            (
                RUNNER_API_INSPECT_REQUEST_SCHEMA,
                RunnerApiVersion { major: 99 },
                RunnerApiOperationFailureCode::UnsupportedApiVersion,
            ),
        ];

        for (schema, api_version, expected_failure) in fixtures {
            let response = RunnerDiscoveryService::inspect_api(&RunnerApiInspectRequest {
                schema: schema.to_string(),
                api_version,
                runner_id: "runner-that-does-not-exist".to_string(),
            })
            .expect("validation response");

            assert_eq!(response.inspection, None);
            assert_eq!(response.failure.expect("failure").code, expected_failure);
        }
    }

    #[test]
    fn inspect_api_types_unknown_runner_failures() {
        let response = RunnerDiscoveryService::inspect_api(&RunnerApiInspectRequest {
            schema: RUNNER_API_INSPECT_REQUEST_SCHEMA.to_string(),
            api_version: RUNNER_API_V1,
            runner_id: "runner-that-does-not-exist".to_string(),
        })
        .expect("unknown runner response");

        assert_eq!(response.inspection, None);
        assert_eq!(
            response.failure.expect("failure").code,
            RunnerApiOperationFailureCode::RunnerNotFound
        );
    }
}
