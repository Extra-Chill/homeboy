//! Public Homeboy orchestration service.
//!
//! Owns capabilities and run retrieval. The local CLI and the daemon HTTP
//! adapter call this service; they do not assemble a second control-plane
//! projection. Construct with an explicit lookup — the service does not
//! resolve ambient stores or providers itself.

use homeboy_control_plane_contract::{
    ControlPlaneCapabilities, ControlPlaneError, ControlPlaneEvidenceRef, ControlPlaneLocation,
    ControlPlaneRef, ControlPlaneRun, ControlPlaneRunState, ExecutionId, MissionId, RunId,
};
use homeboy_core::control_plane::{register_control_plane_provider, ControlPlaneProvider};

use crate::agent_task_lifecycle::{
    canonical_control_plane_identities, resolve_run_id_in_store, AgentTaskLifecycleStore,
    AgentTaskRunRecord, AgentTaskRunState, CanonicalControlPlaneIdentities,
};

/// Lookup used by [`OrchestrationService`]. Callers inject stores or test
/// doubles; the service never opens an environment-rooted store itself.
pub trait RunLookup {
    fn get(&self, id: &RunId) -> Result<Option<AgentTaskRunRecord>, ControlPlaneError>;
    fn resolve_alias(&self, id: &RunId) -> Result<RunId, ControlPlaneError>;
}

/// Durable lifecycle-store lookup. Bounded, non-reconciling, non-writing.
pub struct LifecycleStoreLookup {
    store: AgentTaskLifecycleStore,
}

impl LifecycleStoreLookup {
    pub fn new(store: AgentTaskLifecycleStore) -> Self {
        Self { store }
    }
}

impl RunLookup for LifecycleStoreLookup {
    fn get(&self, id: &RunId) -> Result<Option<AgentTaskRunRecord>, ControlPlaneError> {
        match self.store.read_record_bounded(id.as_str()) {
            Ok(record) => Ok(Some(record)),
            Err(error) if is_run_not_found(&error) => Ok(None),
            Err(error) => Err(ControlPlaneError::unavailable(
                error.message,
                "homeboy agent-task status",
            )),
        }
    }

    fn resolve_alias(&self, id: &RunId) -> Result<RunId, ControlPlaneError> {
        let resolved = resolve_run_id_in_store(&self.store, id.as_str()).map_err(|error| {
            ControlPlaneError::unavailable(error.message, "homeboy agent-task status")
        })?;
        RunId::new(resolved).map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
    }
}

/// Public typed orchestration facade.
pub struct OrchestrationService<L> {
    lookup: L,
}

impl<L: RunLookup> OrchestrationService<L> {
    pub fn new(lookup: L) -> Self {
        Self { lookup }
    }

    /// Operations actually wired in this build. Mutations are not advertised.
    pub fn capabilities(&self) -> ControlPlaneCapabilities {
        ControlPlaneCapabilities::this_build()
    }

    /// Pure, bounded, non-reconciling run read.
    pub fn run(&self, requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        let record = match self.lookup.get(requested_id)? {
            Some(record) => record,
            None => {
                let resolved = self.lookup.resolve_alias(requested_id)?;
                if resolved == *requested_id {
                    return Err(ControlPlaneError::not_found(
                        format!("agent-task run not found: {requested_id}"),
                        "homeboy agent-task active",
                    ));
                }
                self.lookup.get(&resolved)?.ok_or_else(|| {
                    ControlPlaneError::not_found(
                        format!("agent-task run not found: {requested_id}"),
                        "homeboy agent-task active",
                    )
                })?
            }
        };
        project_record(&record, requested_id)
    }
}

/// Project a durable record the status CLI already loaded. No store lookup.
pub fn project_record(
    record: &AgentTaskRunRecord,
    requested_id: &RunId,
) -> Result<ControlPlaneRun, ControlPlaneError> {
    let run = RunId::new(&record.run_id)
        .map_err(|error| ControlPlaneError::invalid_argument(format!("durable run id: {error}")))?;
    let requested = requested_ref(requested_id, &record.run_id)?;
    let resolved = ControlPlaneRef::Run(run.clone());
    let identities = identities_for_record(record)?;
    let mut resource = ControlPlaneRun::new(run, requested, resolved);
    if let Some(identities) = identities {
        resource.mission = Some(identities.mission);
        resource.attempt = Some(identities.attempt);
        resource.attempt_number = Some(identities.attempt_number);
    }
    resource.state = run_state(record);
    resource.location = location(record);
    resource.execution = execution(record)?;
    resource.created_at = record.submitted_at.clone();
    resource.updated_at = record.updated_at.clone();
    if resource.state.is_terminal() {
        resource.finished_at = record.updated_at.clone();
    }
    resource.evidence = evidence_refs(record);
    resource.artifacts = artifact_refs(record);
    resource.reconciles = false;
    Ok(resource)
}

/// Status-compatible identity projection. Preserves the existing CLI
/// `control_plane` object while routing through the orchestration service.
pub fn status_identities_for_run(
    run_id: &str,
    recorded_attempt: Option<u32>,
) -> homeboy_core::Result<Option<CanonicalControlPlaneIdentities>> {
    crate::agent_task_lifecycle::canonical_control_plane_identities_for_run(
        run_id,
        recorded_attempt,
    )
}

pub fn status_identities_for_record(
    record: &AgentTaskRunRecord,
) -> homeboy_core::Result<Option<CanonicalControlPlaneIdentities>> {
    let requested = RunId::new(&record.run_id).map_err(|error| {
        homeboy_core::Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let resource = project_record(record, &requested).map_err(|error| {
        homeboy_core::Error::validation_invalid_argument(
            "run_id",
            error.message,
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let (Some(mission), Some(attempt), Some(attempt_number)) =
        (resource.mission, resource.attempt, resource.attempt_number)
    else {
        return Ok(None);
    };
    Ok(Some(CanonicalControlPlaneIdentities {
        mission,
        run: resource.run,
        attempt,
        attempt_number,
    }))
}

fn identities_for_record(
    record: &AgentTaskRunRecord,
) -> Result<Option<CanonicalControlPlaneIdentities>, ControlPlaneError> {
    canonical_control_plane_identities(record)
        .map_err(|error| ControlPlaneError::invalid_argument(error.message))
}

fn requested_ref(
    requested_id: &RunId,
    resolved_run_id: &str,
) -> Result<ControlPlaneRef, ControlPlaneError> {
    if requested_id.as_str() == resolved_run_id {
        return Ok(ControlPlaneRef::Run(requested_id.clone()));
    }
    Ok(ControlPlaneRef::Mission(
        MissionId::new(requested_id.as_str())
            .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))?,
    ))
}

fn run_state(record: &AgentTaskRunRecord) -> ControlPlaneRunState {
    if record.is_stale_running() {
        return ControlPlaneRunState::Stale;
    }
    match record.state {
        AgentTaskRunState::Queued => ControlPlaneRunState::Queued,
        AgentTaskRunState::Running => ControlPlaneRunState::Running,
        AgentTaskRunState::Succeeded => ControlPlaneRunState::Succeeded,
        AgentTaskRunState::CandidateRecoverable => ControlPlaneRunState::CandidateRecoverable,
        AgentTaskRunState::PartialRecoverable => ControlPlaneRunState::PartialRecoverable,
        AgentTaskRunState::PartialFailure => ControlPlaneRunState::PartialFailure,
        AgentTaskRunState::Failed => ControlPlaneRunState::Failed,
        AgentTaskRunState::Cancelled => ControlPlaneRunState::Cancelled,
    }
}

fn location(record: &AgentTaskRunRecord) -> Option<ControlPlaneLocation> {
    let runner_id = record.runner_id().map(str::to_string);
    let transport = record
        .metadata
        .get("remote_run_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    if runner_id.is_none() && transport.is_none() {
        return None;
    }
    Some(ControlPlaneLocation {
        runner_id,
        remote_run_id: transport,
    })
}

fn execution(record: &AgentTaskRunRecord) -> Result<Option<ExecutionId>, ControlPlaneError> {
    let Some(job_id) = record.runner_job_id() else {
        return Ok(None);
    };
    ExecutionId::new(job_id)
        .map(Some)
        .map_err(|error| ControlPlaneError::invalid_argument(error.to_string()))
}

fn evidence_refs(record: &AgentTaskRunRecord) -> Vec<ControlPlaneEvidenceRef> {
    record
        .latest_executor_evidence
        .iter()
        .flat_map(|evidence| evidence.refs())
        .enumerate()
        .map(|(index, evidence)| ControlPlaneEvidenceRef {
            id: evidence
                .label
                .unwrap_or_else(|| format!("evidence-{}", index + 1)),
            kind: evidence.kind,
            uri: evidence.uri,
        })
        .collect()
}

fn artifact_refs(record: &AgentTaskRunRecord) -> Vec<ControlPlaneEvidenceRef> {
    record
        .artifact_refs
        .iter()
        .map(|artifact| ControlPlaneEvidenceRef {
            id: artifact
                .label
                .clone()
                .unwrap_or_else(|| artifact.task_id.clone()),
            kind: artifact.kind.clone(),
            uri: artifact.uri.clone(),
        })
        .collect()
}

fn is_run_not_found(error: &homeboy_core::Error) -> bool {
    error.code == homeboy_core::ErrorCode::ValidationInvalidArgument
        && error.message.contains("not found")
}

struct RegisteredProvider;

impl ControlPlaneProvider for RegisteredProvider {
    fn capabilities(&self) -> ControlPlaneCapabilities {
        ControlPlaneCapabilities::this_build()
    }

    fn run(&self, requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        let store = AgentTaskLifecycleStore::from_environment().map_err(|error| {
            ControlPlaneError::unavailable(error.message, "homeboy agent-task status")
        })?;
        OrchestrationService::new(LifecycleStoreLookup::new(store)).run(requested_id)
    }
}

/// Register the orchestration service as the HTTP control-plane provider.
pub fn register() {
    register_control_plane_provider(Box::new(RegisteredProvider));
}

#[cfg(test)]
mod tests {
    use super::{project_record, OrchestrationService, RunLookup};
    use crate::agent_task_lifecycle::AgentTaskRunRecord;
    use homeboy_control_plane_contract::{
        ControlPlaneErrorClass, ControlPlaneOperation, ControlPlaneRef, ControlPlaneRunState,
        RunId, CONTROL_PLANE_RUN_SCHEMA,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    const AGENT_TASK_COOK: &str = "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e";
    const AGENT_TASK_RUN: &str =
        "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751";

    struct MapLookup {
        records: BTreeMap<String, AgentTaskRunRecord>,
        aliases: BTreeMap<String, String>,
    }

    impl RunLookup for MapLookup {
        fn get(
            &self,
            id: &RunId,
        ) -> Result<Option<AgentTaskRunRecord>, homeboy_control_plane_contract::ControlPlaneError>
        {
            Ok(self.records.get(id.as_str()).cloned())
        }

        fn resolve_alias(
            &self,
            id: &RunId,
        ) -> Result<RunId, homeboy_control_plane_contract::ControlPlaneError> {
            let resolved = self
                .aliases
                .get(id.as_str())
                .cloned()
                .unwrap_or_else(|| id.as_str().to_string());
            RunId::new(resolved).map_err(|error| {
                homeboy_control_plane_contract::ControlPlaneError::invalid_argument(
                    error.to_string(),
                )
            })
        }
    }

    fn record(run_id: &str) -> AgentTaskRunRecord {
        let mut record: AgentTaskRunRecord = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": run_id,
            "plan_id": "plan",
            "state": "succeeded",
            "submitted_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:01:00Z",
            "plan_path": "/plan",
            "artifact_refs": [{
                "task_id": "review",
                "kind": "review_form",
                "uri": "homeboy://artifact/review"
            }],
            "metadata": {
                "cook_attempt": 1,
                "runner_id": "homeboy-lab",
                "runner_job_id": "job-1",
                "remote_run_id": "remote-1"
            }
        }))
        .expect("record");
        record.plan_path = "/secret/workspace".to_string();
        record
    }

    fn service() -> OrchestrationService<MapLookup> {
        let mut records = BTreeMap::new();
        records.insert(AGENT_TASK_RUN.to_string(), record(AGENT_TASK_RUN));
        let mut aliases = BTreeMap::new();
        aliases.insert(AGENT_TASK_COOK.to_string(), AGENT_TASK_RUN.to_string());
        OrchestrationService::new(MapLookup { records, aliases })
    }

    #[test]
    fn capabilities_advertise_only_wired_reads() {
        let capabilities = service().capabilities();
        assert_eq!(
            capabilities.operations,
            vec![
                ControlPlaneOperation::GetCapabilities,
                ControlPlaneOperation::GetRun
            ]
        );
        assert!(!capabilities.operations.is_empty());
    }

    #[test]
    fn run_projects_canonical_identities_and_redacts_durable_payload() {
        let resource = service()
            .run(&RunId::new(AGENT_TASK_RUN).expect("run id"))
            .expect("run");
        assert_eq!(resource.schema, CONTROL_PLANE_RUN_SCHEMA);
        assert_eq!(
            resource.requested,
            ControlPlaneRef::Run(
                homeboy_control_plane_contract::RunId::new(AGENT_TASK_RUN).expect("run")
            )
        );
        assert_eq!(resource.run.as_str(), AGENT_TASK_RUN);
        assert_eq!(
            resource.mission.as_ref().map(|id| id.as_str()),
            Some(AGENT_TASK_COOK)
        );
        assert_eq!(resource.attempt_number, Some(1));
        assert_eq!(resource.state, ControlPlaneRunState::Succeeded);
        assert_eq!(resource.reconciles, false);
        assert_eq!(
            resource.execution.as_ref().map(|id| id.as_str()),
            Some("job-1")
        );
        assert_eq!(
            resource
                .location
                .as_ref()
                .and_then(|location| location.runner_id.as_deref()),
            Some("homeboy-lab")
        );
        assert_eq!(resource.artifacts.len(), 1);
        let value = serde_json::to_value(&resource).expect("serialize");
        assert!(value.get("metadata").is_none());
        assert!(value.get("cwd").is_none());
        assert!(value.get("plan_path").is_none());
        assert!(value.get("prompt").is_none());
        let decoded: homeboy_control_plane_contract::ControlPlaneRun =
            serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, resource);
    }

    #[test]
    fn cook_alias_reports_requested_mission_and_resolved_run() {
        let resource = service()
            .run(&RunId::new(AGENT_TASK_COOK).expect("Cook alias"))
            .expect("alias");
        assert_eq!(
            resource.requested,
            ControlPlaneRef::Mission(
                homeboy_control_plane_contract::MissionId::new(AGENT_TASK_COOK).expect("mission")
            )
        );
        assert_eq!(
            resource.resolved,
            ControlPlaneRef::Run(
                homeboy_control_plane_contract::RunId::new(AGENT_TASK_RUN).expect("run")
            )
        );
        assert_eq!(resource.run.as_str(), AGENT_TASK_RUN);
    }

    #[test]
    fn unknown_run_is_typed_not_found() {
        let error = service()
            .run(&RunId::new("no-such-run").expect("run id"))
            .expect_err("missing");
        assert_eq!(error.class, ControlPlaneErrorClass::NotFound);
        assert!(!error.retryable);
        assert_eq!(
            error.next_action.as_deref(),
            Some("homeboy agent-task active")
        );
    }

    #[test]
    fn project_record_matches_service_run() {
        let seeded = record(AGENT_TASK_RUN);
        let requested = RunId::new(AGENT_TASK_RUN).expect("run id");
        let from_record = project_record(&seeded, &requested).expect("project");
        let from_service = service().run(&requested).expect("run");
        assert_eq!(from_record, from_service);
    }
}
