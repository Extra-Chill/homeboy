//! Canonical read-only Runner API service.

use homeboy_core::Result;
use homeboy_runner_contract::{
    RunnerApiCompatibility, RunnerApiCompatibilityFailure, RunnerApiCompatibilityFailureCode,
    RunnerApiCompatibilityStatus, RunnerApiHandshakeRequest, RunnerApiHandshakeResponse,
    RunnerApiVersion, RunnerCapabilities, RunnerDescriptor, RunnerInspection, RunnerKind,
    RunnerReadiness, RUNNER_API_HANDSHAKE_REQUEST_SCHEMA, RUNNER_API_HANDSHAKE_RESPONSE_SCHEMA,
    RUNNER_API_V1, RUNNER_CAPABILITIES_SCHEMA, RUNNER_DESCRIPTOR_SCHEMA, RUNNER_INSPECTION_SCHEMA,
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
}
