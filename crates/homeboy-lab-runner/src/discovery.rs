//! Canonical read-only Runner API service.

use homeboy_core::Result;
use homeboy_runner_contract::{
    RunnerCapabilities, RunnerDescriptor, RunnerInspection, RunnerKind, RunnerReadiness,
    RUNNER_CAPABILITIES_SCHEMA, RUNNER_DESCRIPTOR_SCHEMA, RUNNER_INSPECTION_SCHEMA,
    RUNNER_READINESS_SCHEMA,
};

use crate::{Runner, RunnerAdmissionSnapshot};

pub struct RunnerDiscoveryService;

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
}
