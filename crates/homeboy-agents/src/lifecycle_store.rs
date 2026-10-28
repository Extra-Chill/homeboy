use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(any(test, feature = "test-support"))]
use std::cell::Cell;

use serde_json::{json, Value};

use super::terminal_transition::{TerminalRecordCommit, TRANSITION_RETRY_ATTEMPTS};
use super::{
    sanitize_run_id, AgentTaskAdvance, AgentTaskCookIndex, AgentTaskCookIndexAttempt,
    AgentTaskCookLatestSubstantiveCandidate, AgentTaskRunRecord, AgentTaskRunState,
    AgentTaskTransition, AgentTaskTransitionConflict, AgentTaskTransitionError,
};
use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy_core::engine::local_files::{
    write_json_file as write_json, write_json_file_owner_only as write_private_json,
};
use homeboy_core::observation::{
    ControlPlaneResourceProjection, ObservationStore, RunCursor as ObservationRunCursor,
    RunListFilter, RunMetadataRevision, RunRecord, RunStatus,
};
use homeboy_core::{build_identity, paths, Error, ErrorCode, Result};

/// Durable agent-task lifecycle storage bound to immutable filesystem roots.
///
/// Record writes keep workspace owner renewal, terminal workspace authority,
/// and observation projection bound to the same roots.
#[derive(Clone, Debug)]
pub struct AgentTaskLifecycleStore {
    roots: paths::PathRoots,
}

impl AgentTaskLifecycleStore {
    pub fn new(roots: paths::PathRoots) -> Self {
        Self { roots }
    }

    /// The roots this store was built from.
    ///
    /// Exposed so operations that reach sibling stores below the same
    /// installation can derive their roots from this one rather than resolving
    /// the environment again (#7505).
    pub fn roots(&self) -> &paths::PathRoots {
        &self.roots
    }

    /// Construct an explicitly self-contained store from a data root.
    ///
    /// The companion roots live below the supplied root so this constructor
    /// never consults ambient configuration.
    pub fn from_data_root(data_root: PathBuf) -> Self {
        Self::new(paths::PathRoots::new(
            data_root.join("config"),
            data_root.clone(),
            data_root.join("artifacts"),
        ))
    }

    pub fn from_environment() -> Result<Self> {
        Ok(Self::new(paths::PathRoots::from_environment()?))
    }

    pub fn from_current_environment() -> Result<Self> {
        Self::from_environment()
    }

    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        self.roots
            .data()
            .join("agent-task-runs")
            .join(sanitize_run_id(run_id))
    }

    pub fn controller_plan_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("plan.json")
    }

    pub fn aggregate_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("aggregate.json")
    }

    /// Lock serializing a run's Lab handoff acceptance against expiry. Every
    /// runner dispatch path takes it, so its presence marks a run that may have
    /// crossed the runner boundary.
    pub fn lab_handoff_lock_path(&self, run_id: &str) -> PathBuf {
        self.run_dir(run_id).join("lab-handoff.lock")
    }

    /// The Cook's blob directory (recipe, input, notification markers).
    ///
    /// The Cook index itself is not a file: it lives only in the canonical
    /// `control_plane_resources` projection owned by the Cook alias.
    pub fn cook_dir(&self, cook_id: &str) -> PathBuf {
        self.roots
            .data()
            .join("agent-task-cooks")
            .join(sanitize_run_id(cook_id))
    }

    /// The observation database below these roots.
    ///
    /// Delegates to `paths` so this can never name a different file than the
    /// path `ObservationStore::*_in_roots` opens for the same roots.
    pub fn observation_db_path(&self) -> PathBuf {
        paths::observation_db_in_root(self.roots.data())
    }

    pub(crate) fn data_root(&self) -> PathBuf {
        self.roots.data().to_path_buf()
    }

    pub(crate) fn artifact_root(&self) -> PathBuf {
        self.roots.artifacts().to_path_buf()
    }

    pub(crate) fn controller_scratch_root(&self) -> PathBuf {
        self.data_root().join("controller-scratch")
    }

    pub(crate) fn matches_current_environment(&self) -> Result<bool> {
        Ok(self.roots == paths::PathRoots::from_environment()?)
    }

    pub(crate) fn workspace_claim_store(
        &self,
    ) -> homeboy_core::workspace_claim::WorkspaceClaimStore {
        super::workspace_claims::workspace_claim_store_at(self.data_root())
    }

    /// Terminal workspace authority bound to this store's own roots.
    ///
    /// Authority receipts and their release markers are a permission gate over
    /// a retained runner workspace, so they must be read and written in the
    /// same installation the record lives in (#7505).
    pub(crate) fn workspace_terminal_authority_store(
        &self,
    ) -> super::workspace_authority::WorkspaceTerminalAuthorityStore {
        super::workspace_authority::WorkspaceTerminalAuthorityStore::from_roots(&self.roots)
    }

    /// Submit an exact run identity using this store's durable lifecycle roots.
    /// The admission callback supplies runtime evidence without coupling tests to
    /// the ambient controller runtime.
    pub fn submit_plan_with_runtime_admission(
        &self,
        plan: &AgentTaskPlan,
        run_id: &str,
        admit_runtime: impl FnOnce(&str) -> Result<Value>,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::submit_plan_with_runtime_admission_in_store(
            self,
            plan,
            Some(run_id),
            None,
            None,
            None,
            admit_runtime,
        )
    }

    #[cfg(test)]
    pub(crate) fn submit_plan_with_runtime_admission_status(
        &self,
        plan: &AgentTaskPlan,
        run_id: &str,
        execution_runner_id: Option<String>,
        admission_status: &dyn Fn(&str) -> Option<Value>,
        admit_runtime: impl FnOnce(&str) -> Result<Value>,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::submit_plan_with_runtime_admission_in_store(
            self,
            plan,
            Some(run_id),
            execution_runner_id,
            None,
            Some(admission_status),
            admit_runtime,
        )
    }

    pub(crate) fn submit_plan_with_current_runtime(
        &self,
        plan: &AgentTaskPlan,
        run_id: &str,
    ) -> Result<AgentTaskRunRecord> {
        self.submit_plan_with_current_runtime_and_metadata(plan, run_id, None)
    }

    pub(crate) fn submit_plan_with_current_runtime_and_metadata(
        &self,
        plan: &AgentTaskPlan,
        run_id: &str,
        submission_metadata: Option<serde_json::Map<String, Value>>,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::submit_plan_with_runtime_admission_in_store(
            self,
            plan,
            Some(run_id),
            super::lifecycle_ops::execution_runner_id(),
            submission_metadata,
            Some(&crate::agent_task_service::cook_pre_execution::store_admission_status(self)),
            |run_id| {
                let runtime_root =
                    homeboy_core::controller_runtime::runtime_root_in(self.roots().data())?;
                homeboy_core::controller_runtime::admit_current_for_with_cancellation_check_in_root(
                    &runtime_root, run_id, || {
                        Ok(crate::agent_task_service::cook_pre_execution::runtime_admission_cancellation_requested(
                            &self.read_record(run_id)?,
                        ))
                    },
                ).map(|admission| admission.runtime)
            },
        )
    }

    pub fn open_observation_initialized(&self) -> Result<ObservationStore> {
        ObservationStore::open_initialized_for_lifecycle_in_roots(&self.roots)
    }

    /// Read the observation store through this store's own roots.
    ///
    /// This previously injected only the database path and left artifact
    /// resolution ambient, so a reader opened from injected roots reported
    /// artifacts against a different root than the database it read them from
    /// (#7505).
    pub fn open_observation_readonly(&self) -> Result<ObservationStore> {
        ObservationStore::open_readonly_in_roots(&self.roots)
    }

    /// Open this store's observation database with the same startup artifact
    /// maintenance the ambient `ObservationStore::open_initialized()` performs.
    ///
    /// This is deliberately not [`Self::open_observation_initialized`]. The two
    /// are not interchangeable: the lifecycle opener defers report-only artifact
    /// maintenance so a lifecycle transition can proceed while another process
    /// owns SQLite's writer lock, while this one first reconciles unfinished
    /// artifact publications. Rooting a caller
    /// that used the ambient `open_initialized()` therefore has to come here, or
    /// the reroot would silently change what that caller sees — the hazard
    /// #12618 recorded against `substantive_candidate_in_aggregate` (#7505).
    ///
    /// Like every opener on this store, BOTH roots come from `self`: the
    /// database below `data` and the artifact tree it indexes below `artifacts`,
    /// which `PathRoots` carries separately.
    ///
    /// Public because rooting is a cross-crate migration: `homeboy-lab-runner`
    /// promotes runner-exec artifacts and has the same constraint, so it needs
    /// the same opener. Reach for this only when replacing an ambient
    /// `ObservationStore::open_initialized()`; a caller that never ran startup
    /// artifact maintenance wants [`Self::open_observation_initialized`].
    pub fn open_observation_maintained(&self) -> Result<ObservationStore> {
        ObservationStore::open_initialized_in_roots(&self.roots)
    }

    pub fn with_config_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        homeboy_core::config::with_config_lock_at(self.roots.config(), operation)
    }

    pub fn write_controller_plan(&self, run_id: &str, plan: &AgentTaskPlan) -> Result<PathBuf> {
        write_plan_in_store(self, run_id, plan)
    }

    /// Bind a preflighted controller plan only while this consumer atomically
    /// claims the exact queued record. A losing consumer must not overwrite the
    /// winner's route or quarantine its now-running record.
    pub(crate) fn bind_controller_plan_and_claim_queued_run(
        &self,
        run_id: &str,
        plan: &AgentTaskPlan,
    ) -> Result<Option<AgentTaskRunRecord>> {
        self.bind_controller_plan_and_claim_queued_run_when(run_id, plan, |record| {
            !record.metadata["execution_scheduling"].is_object()
        })
    }

    pub(crate) fn bind_controller_plan_and_claim_queued_run_when(
        &self,
        run_id: &str,
        plan: &AgentTaskPlan,
        authorize: impl FnOnce(&AgentTaskRunRecord) -> bool,
    ) -> Result<Option<AgentTaskRunRecord>> {
        self.with_config_lock(|| {
            let mut record = self.read_record(run_id)?;
            if record.state() != super::AgentTaskRunState::Queued
                || record.metadata.get("queue_quarantine").is_some()
                || !authorize(&record)
            {
                return Ok(None);
            }
            validate_execution_budget(plan)?;
            validate_managed_services(plan)?;
            write_private_json(&self.controller_plan_path(run_id), plan)?;

            record.updated_at = Some(super::now_timestamp());
            super::start_run(&mut record);
            super::update_lifecycle_heartbeat(&mut record);
            record.record_runner_metadata(false);
            if record.metadata["execution_scheduling"].is_object() {
                let intent_id =
                    record.metadata["execution_scheduling"]["current_intent_id"].clone();
                record.metadata["execution_step_owner"] = execution_step_owner(intent_id)?;
            }
            self.advance_record_locked_without_terminal_projection(&record, AgentTaskAdvance::Start)
                .map(Some)
        })
    }

    pub(crate) fn claim_execution_admission(
        &self,
        intent: &crate::agent_task_service::ExecutionStepIntent,
    ) -> Result<bool> {
        self.with_config_lock(|| {
            let mut record = self.read_record(&intent.subject_id)?;
            if !intent.authorizes(&record) {
                return Ok(false);
            }
            record.record_runner_metadata(false);
            super::set_run_state(&mut record, super::AgentTaskRunState::Running);
            record.metadata["execution_step_owner"] =
                execution_step_owner(json!(intent.intent_id))?;
            self.advance_record_locked_without_terminal_projection(
                &record,
                AgentTaskAdvance::Start,
            )?;
            Ok(true)
        })
    }

    pub fn record_pre_execution_failure(
        &self,
        run_id: &str,
        plan: &AgentTaskPlan,
        phase: &str,
        error: &Error,
    ) -> Result<AgentTaskRunRecord> {
        super::record_pre_execution_failure_in_store(self, run_id, plan, phase, error)
    }

    pub fn record_workspace_snapshot_fence_invalidation(
        &self,
        run_id: &str,
        plan: &AgentTaskPlan,
        error: &Error,
    ) -> Result<AgentTaskRunRecord> {
        super::record_workspace_snapshot_fence_invalidation_in_store(self, run_id, plan, error)
    }

    pub fn record_lab_offload_planned(
        &self,
        input: super::LabOffloadProxyPlan<'_>,
    ) -> Result<AgentTaskRunRecord> {
        super::lab_offload::record_lab_offload_planned_in_store(self, input)
    }

    pub fn record_detached_lab_run(
        &self,
        input: super::DetachedLabRunRecord<'_>,
    ) -> Result<AgentTaskRunRecord> {
        super::lab_offload::record_detached_lab_run_in_store(self, input)
    }

    pub(crate) fn mark_running(&self, run_id: &str) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::mark_running_in_store(self, run_id)
    }

    pub(crate) fn reserve_provider_execution(
        &self,
        run_id: &str,
        task: &crate::agent_task::AgentTaskRequest,
        attempt: u32,
    ) -> Result<super::ProviderExecutionReservation> {
        super::lifecycle_ops::reserve_provider_execution_in_store(self, run_id, task, attempt)
    }

    pub(crate) fn record_provider_launch_context(
        &self,
        run_id: &str,
        task_id: &str,
        attempt: u32,
        context: &crate::agent_task_provider::AgentTaskProviderLaunchContext,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_provider_launch_context_in_store(
            self, run_id, task_id, attempt, context,
        )
    }

    pub(crate) fn record_provider_execution_terminal(
        &self,
        run_id: &str,
        task_id: &str,
        attempt: u32,
        state: &str,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_provider_execution_terminal_in_store(
            self, run_id, task_id, attempt, state,
        )
    }

    pub(crate) fn record_provider_execution_terminal_with_model(
        &self,
        run_id: &str,
        task_id: &str,
        attempt: u32,
        state: &str,
        model: Option<&str>,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_provider_execution_terminal_with_model_in_store(
            self, run_id, task_id, attempt, state, model,
        )
    }

    pub(crate) fn record_provider_execution_cleanup_elapsed(
        &self,
        run_id: &str,
        task_id: &str,
        attempt: u32,
        elapsed_ms: u64,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_provider_execution_cleanup_elapsed_in_store(
            self, run_id, task_id, attempt, elapsed_ms,
        )
    }

    pub fn claim_cook_operation(
        &self,
        run_id: &str,
        operation_key: &str,
        lease: Duration,
    ) -> Result<super::ClaimOutcome> {
        super::operation_claims::claim_cook_operation_in_store(self, run_id, operation_key, lease)
    }

    pub fn complete_cook_operation(
        &self,
        run_id: &str,
        operation_key: &str,
        result: Value,
    ) -> Result<()> {
        super::operation_claims::complete_cook_operation_in_store(
            self,
            run_id,
            operation_key,
            result,
        )
    }

    pub fn fail_cook_operation(
        &self,
        run_id: &str,
        operation_key: &str,
        result: Value,
    ) -> Result<()> {
        super::operation_claims::fail_cook_operation_in_store(self, run_id, operation_key, result)
    }

    pub(crate) fn aggregate_source_exact(&self, run_id: &str) -> Result<(String, PathBuf)> {
        let record = self.read_record(run_id)?;
        record.aggregate_path.as_ref().ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                format!(
                    "agent-task run '{}' has no aggregate artifact yet",
                    record.run_id
                ),
                Some(record.run_id.clone()),
                None,
            )
        })?;
        let path = self.aggregate_path(&record.run_id);
        // The canonical local aggregate.json is the controller-owned, exact-bytes
        // source (c138e12162) and stays preferred whenever it exists. But a run's
        // record can outlive that file — retention sweeps, or a controller crash
        // between writing the record and materializing the file — leaving the
        // durable record pointing at an aggregate that genuinely is not on this
        // installation's disk. Falling back to the SQLite-backed observation
        // mirror in that case only, rather than propagating a plain IO error,
        // keeps this accessor resolving whatever local evidence still exists
        // instead of spelunking `record.aggregate_path`'s stored string, which
        // may itself name a foreign installation's path (#7505).
        match fs::metadata(&path) {
            Ok(_) => {
                let raw = read_aggregate_bytes_bounded_in_store(self, &record.run_id)?;
                serde_json::from_slice::<AgentTaskAggregate>(&raw).map_err(|error| {
                    Error::internal_json(error.to_string(), Some(path.display().to_string()))
                })?;
                let raw = String::from_utf8(raw).map_err(|error| {
                    Error::internal_json(error.to_string(), Some(path.display().to_string()))
                })?;
                Ok((raw, path))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let aggregate = self.read_aggregate_readonly(&record.run_id)?;
                let raw = serde_json::to_string(&aggregate).map_err(|error| {
                    Error::internal_json(error.to_string(), Some(path.display().to_string()))
                })?;
                Ok((raw, path))
            }
            Err(error) => Err(Error::internal_io(
                error.to_string(),
                Some(path.display().to_string()),
            )),
        }
    }

    pub fn operation_claim(
        &self,
        run_id: &str,
        operation_key: &str,
    ) -> Result<Option<super::OperationClaim>> {
        super::operation_claims::operation_claim_in_store(self, run_id, operation_key)
    }

    pub fn checkpoint_candidate_adoption_remediation(
        &self,
        run_id: &str,
        remediation_run_id: &str,
    ) -> Result<()> {
        super::lifecycle_candidate_adoption::checkpoint_candidate_adoption_remediation_in_store(
            self,
            run_id,
            remediation_run_id,
        )
    }

    pub fn cancel_reserved_detached_cook_handoff_attempt_if_cancelled(
        &self,
        cook_id: &str,
    ) -> Result<bool> {
        super::lifecycle_ops::cancel_reserved_detached_cook_handoff_attempt_if_cancelled_in_store(
            self, cook_id,
        )
    }

    pub fn require_detached_cook_handoff_fence_open(&self, cook_id: &str) -> Result<()> {
        super::lifecycle_ops::require_detached_cook_handoff_fence_open_in_store(self, cook_id)
    }

    pub fn start_candidate_adoption_with_policy(
        &self,
        run_id: &str,
        candidate_sha: &str,
        ai_model: &str,
        active_gate: &str,
        rerun_completed_gates: bool,
        replace_interrupted: bool,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_candidate_adoption::start_candidate_adoption_with_policy_in_store(
            self,
            run_id,
            candidate_sha,
            ai_model,
            active_gate,
            rerun_completed_gates,
            replace_interrupted,
        )
    }

    pub fn start_candidate_adoption_gate(
        &self,
        run_id: &str,
        command: &str,
        process_group: u32,
        timeout_seconds: u64,
    ) -> Result<()> {
        super::lifecycle_candidate_adoption::start_candidate_adoption_gate_in_store(
            self,
            run_id,
            command,
            process_group,
            timeout_seconds,
        )
    }

    pub(crate) fn heartbeat_candidate_adoption_gate(
        &self,
        run_id: &str,
        visibility: crate::agent_task_gate::AgentTaskGateVisibility,
        reveal_policy: crate::agent_task_gate::AgentTaskGateRevealPolicy,
        status: &crate::agent_task_gate::AgentTaskGateLiveStatus,
    ) -> Result<()> {
        super::lifecycle_candidate_adoption::heartbeat_candidate_adoption_gate_in_store(
            self,
            run_id,
            visibility,
            reveal_policy,
            status,
        )
    }

    pub fn candidate_adoption_cancel_requested(&self, run_id: &str) -> Result<bool> {
        super::lifecycle_candidate_adoption::candidate_adoption_cancel_requested_in_store(
            self, run_id,
        )
    }

    pub fn checkpoint_candidate_adoption(
        &self,
        run_id: &str,
        phase: &str,
        active_gate: &str,
    ) -> Result<()> {
        super::lifecycle_candidate_adoption::checkpoint_candidate_adoption_in_store(
            self,
            run_id,
            phase,
            active_gate,
        )
    }

    pub fn finish_candidate_adoption(
        &self,
        run_id: &str,
        error: Option<String>,
        supersede_pre_execution_failure: bool,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_candidate_adoption::finish_candidate_adoption_in_store(
            self,
            run_id,
            error,
            supersede_pre_execution_failure,
        )
    }

    pub fn record_candidate_adoption_result(&self, run_id: &str, result: Value) -> Result<()> {
        super::lifecycle_candidate_adoption::record_candidate_adoption_result_in_store(
            self, run_id, result,
        )
    }

    pub fn record_promotion(&self, run_id: &str, promotion: Value) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_promotion_in_store(self, run_id, promotion)
    }

    pub(crate) fn record_cook_finalization(
        &self,
        run_id: &str,
        finalization: Value,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_cook_finalization_in_store(self, run_id, finalization)
    }

    pub(crate) fn record_cook_delivery_blocker(
        &self,
        run_id: &str,
        blocker: crate::agent_task_service::CookDeliveryBlocker,
        base_movements: u32,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_cook_delivery_blocker_in_store(
            self,
            run_id,
            blocker,
            base_movements,
        )
    }

    pub fn read_controller_plan(&self, run_id: &str) -> Result<AgentTaskPlan> {
        let path = self.controller_plan_path(run_id);
        let plan = read_json(&path).map_err(|error| {
            Error::internal_io(
                format!(
                    "authoritative controller-owned plan for agent-task run `{run_id}` is unavailable at {}; lifecycle record plan_path is runner execution transport and cannot be used for controller readback: {}",
                    path.display(),
                    error.message
                ),
                Some(path.display().to_string()),
            )
        })?;
        validate_execution_budget(&plan)?;
        validate_managed_services(&plan)?;
        Ok(plan)
    }

    pub fn write_aggregate(&self, run_id: &str, aggregate: &AgentTaskAggregate) -> Result<PathBuf> {
        let path = self.aggregate_path(run_id);
        write_json(&path, aggregate)?;
        Ok(path)
    }

    pub fn read_aggregate(&self, run_id: &str) -> Result<AgentTaskAggregate> {
        match read_mirrored_aggregate_in_store(self, run_id)? {
            Some(aggregate) => Ok(aggregate),
            None => read_json(&self.aggregate_path(run_id)),
        }
    }

    pub fn read_aggregate_bounded(&self, run_id: &str) -> Result<AgentTaskAggregate> {
        let path = self.aggregate_path(run_id);
        let raw = read_aggregate_bytes_bounded_in_store(self, run_id)?;
        serde_json::from_slice(&raw).map_err(|error| {
            Error::internal_json(error.to_string(), Some(path.display().to_string()))
        })
    }

    /// Observe only the aggregate mirrored in the authoritative SQLite row,
    /// without initializing or migrating the observation database.
    pub fn read_aggregate_readonly(&self, run_id: &str) -> Result<AgentTaskAggregate> {
        let observations = self.open_observation_readonly()?;
        let run = observations.get_run(run_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                format!("agent-task run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            )
        })?;
        let value = run
            .metadata_json
            .get("agent_task_aggregate")
            .filter(|value| !value.is_null())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "agent_task_aggregate",
                    "authoritative observation has no mirrored agent-task aggregate",
                    Some(run_id.to_string()),
                    None,
                )
            })?;
        serde_json::from_value(value.clone()).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse agent-task aggregate {}", run.id)),
            )
        })
    }

    pub fn write_cook_index_attempt(
        &self,
        cook_id: &str,
        attempt: u32,
        run_id: &str,
        recorded_at: String,
        candidate: Option<AgentTaskCookLatestSubstantiveCandidate>,
    ) -> Result<AgentTaskCookIndex> {
        self.with_config_lock(|| {
            self.write_cook_index_attempt_locked(cook_id, attempt, run_id, recorded_at, candidate)
        })
    }

    pub(crate) fn write_cook_index_attempt_locked(
        &self,
        cook_id: &str,
        attempt: u32,
        run_id: &str,
        recorded_at: String,
        candidate: Option<AgentTaskCookLatestSubstantiveCandidate>,
    ) -> Result<AgentTaskCookIndex> {
        let cook_id = sanitize_run_id(cook_id);
        let run_id = sanitize_run_id(run_id);
        self.validate_cook_index_attempt(&cook_id, attempt, &run_id)?;
        let mut index = if let Some(index) = projected_cook_index_in_store(self, &cook_id)? {
            index
        } else {
            AgentTaskCookIndex {
                schema: super::records::schemas::COOK_INDEX.to_string(),
                cook_id: cook_id.clone(),
                latest_run_id: run_id.clone(),
                latest_substantive_candidate: None,
                cancellation_fence: None,
                attempts: Vec::new(),
            }
        };
        index.cook_id = cook_id.clone();
        index.attempts.retain(|entry| entry.run_id != run_id);
        index.attempts.push(AgentTaskCookIndexAttempt {
            attempt,
            run_id,
            recorded_at,
        });
        index.latest_run_id = index
            .attempts
            .iter()
            .max_by_key(|entry| entry.attempt)
            .expect("Cook index has the recorded attempt")
            .run_id
            .clone();
        if let Some(candidate) = candidate {
            let replace = index
                .latest_substantive_candidate
                .as_ref()
                .is_none_or(|current| {
                    candidate.attempt > current.attempt
                        || (candidate.attempt == current.attempt
                            && candidate.run_id >= current.run_id)
                });
            if replace {
                index.latest_substantive_candidate = Some(candidate);
            }
        }
        persist_cook_index_in_store(self, &index)?;
        Ok(index)
    }

    pub(crate) fn validate_cook_index_attempt(
        &self,
        cook_id: &str,
        attempt: u32,
        run_id: &str,
    ) -> Result<()> {
        let cook_id = sanitize_run_id(cook_id);
        let run_id = sanitize_run_id(run_id);
        // SQLite owns the Cook index; there is no filesystem copy.
        let Some(index) = projected_cook_index_in_store(self, &cook_id)? else {
            return Ok(());
        };
        if index.cook_id != cook_id {
            return Err(Error::validation_invalid_argument(
                "cook_id",
                "durable Cook index belongs to a different Cook",
                Some(cook_id),
                None,
            ));
        }
        if index
            .attempts
            .iter()
            .any(|entry| entry.run_id == run_id && entry.attempt != attempt)
        {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "durable Cook index maps this run to a different attempt",
                Some(run_id),
                None,
            ));
        }
        Ok(())
    }

    pub fn read_cook_index(&self, cook_id: &str) -> Result<AgentTaskCookIndex> {
        let cook_id = sanitize_run_id(cook_id);
        if let Some(index) = projected_cook_index_in_store(self, &cook_id)? {
            return Ok(index);
        }
        Err(Error::validation_invalid_argument(
            "cook_id",
            "canonical Cook alias projection not found; initialize or repair the lifecycle store before reading it",
            Some(cook_id),
            None,
        ))
    }

    pub(crate) fn select_cook_candidate(
        &self,
        cook_id: &str,
    ) -> Result<super::AgentTaskCookCandidateSelection> {
        super::lifecycle_ops::select_cook_candidate_in_store(self, cook_id)
    }

    pub(crate) fn update_cook_index(
        &self,
        cook_id: &str,
        mutate: impl FnOnce(&mut AgentTaskCookIndex) -> bool,
    ) -> Result<Option<AgentTaskCookIndex>> {
        let Some(mut index) = projected_cook_index_in_store(self, &sanitize_run_id(cook_id))?
        else {
            return Ok(None);
        };
        if mutate(&mut index) {
            // The mutated index is persisted whole. Re-deriving it from its
            // attempt fields would discard every other mutation the caller
            // made, including the cancellation fence that closes a mission.
            persist_cook_index_in_store(self, &index)?;
        }
        Ok(Some(index))
    }

    pub fn cook_index_exists(&self, cook_id: &str) -> bool {
        projected_cook_index_in_store(self, &sanitize_run_id(cook_id))
            .ok()
            .flatten()
            .is_some()
    }

    /// Persist the latest bounded, secret-safe terminal notification outcome
    /// in this store's own Cook directory rather than the ambient one.
    pub fn write_cook_notification_outcome(&self, cook_id: &str, outcome: &Value) -> Result<()> {
        let path = self
            .cook_dir(&sanitize_run_id(cook_id))
            .join("notification-outcome.json");
        write_private_json(&path, outcome)
    }

    /// Claim the one terminal notification this store's Cook may deliver.
    ///
    /// The `O_EXCL` marker is created in this store's own Cook directory, so
    /// two stores hold two independent claims rather than contending for the
    /// ambient one.
    pub fn claim_cook_notification(&self, cook_id: &str, marker: &Value) -> Result<bool> {
        claim_cook_notification_event_in_store(self, cook_id, "notification", marker)
    }

    pub fn claim_cook_started_notification(&self, cook_id: &str) -> Result<bool> {
        if cook_id.trim().is_empty() {
            return Ok(false);
        }
        claim_cook_notification_event_in_store(
            self,
            cook_id,
            "notification-started",
            &serde_json::json!({"at": chrono::Utc::now().to_rfc3339(), "by": "cook-controller"}),
        )
    }

    pub fn confirm_cook_started_notification(&self, cook_id: &str) -> Result<()> {
        confirm_cook_notification_event_in_store(
            self,
            cook_id,
            "notification-started",
            &serde_json::json!({"at": chrono::Utc::now().to_rfc3339(), "by": "cook-controller", "state": "delivered"}),
        )
    }

    pub fn release_cook_started_notification_claim(&self, cook_id: &str) -> Result<()> {
        release_cook_notification_event_claim_in_store(self, cook_id, "notification-started")
    }

    /// Exactly-once marker for announcing that one attempt's automatic terminal
    /// continuation failed. Kept separate from the Cook terminal marker so a
    /// later operator `agent-task recover` can still announce the final outcome.
    pub fn claim_cook_continuation_failed_notification(
        &self,
        cook_id: &str,
        run_id: &str,
    ) -> Result<bool> {
        if cook_id.trim().is_empty() || run_id.trim().is_empty() {
            return Ok(false);
        }
        claim_cook_notification_event_in_store(
            self,
            cook_id,
            &continuation_failed_notification_event(run_id),
            &serde_json::json!({"at": chrono::Utc::now().to_rfc3339(), "by": "terminal-continuation"}),
        )
    }

    pub fn confirm_cook_continuation_failed_notification(
        &self,
        cook_id: &str,
        run_id: &str,
    ) -> Result<()> {
        confirm_cook_notification_event_in_store(
            self,
            cook_id,
            &continuation_failed_notification_event(run_id),
            &serde_json::json!({"at": chrono::Utc::now().to_rfc3339(), "by": "terminal-continuation", "state": "delivered"}),
        )
    }

    pub fn release_cook_continuation_failed_notification_claim(
        &self,
        cook_id: &str,
        run_id: &str,
    ) -> Result<()> {
        release_cook_notification_event_claim_in_store(
            self,
            cook_id,
            &continuation_failed_notification_event(run_id),
        )
    }

    /// Commit a confirmed terminal delivery in this store's own Cook directory.
    pub fn confirm_cook_notification(&self, cook_id: &str, marker: &Value) -> Result<()> {
        confirm_cook_notification_event_in_store(self, cook_id, "notification", marker)
    }

    /// Release this store's own provisional terminal-notification claim.
    ///
    /// The claim, the confirmation, and this release are one exactly-once
    /// protocol, so all three have to name the same installation.
    pub fn release_cook_notification_claim(&self, cook_id: &str) -> Result<()> {
        release_cook_notification_event_claim_in_store(self, cook_id, "notification")
    }

    pub fn read_record(&self, run_id: &str) -> Result<AgentTaskRunRecord> {
        let store = self.open_observation_initialized()?;
        let run = store.get_run(run_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                format!("agent-task run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            )
        })?;
        let record = snapshot_from_run(&run)?;
        // Existing lifecycle rows predate the resource table. Their exact run
        // projection is backfilled once, on first read.
        if store
            .control_plane_resource_projection("agent_task_run", &record.run_id)?
            .is_none()
        {
            let projection = agent_task_resource_projection(self, &record, None)?;
            if let Some(mission) = crate::agent_task_lifecycle::canonical_mission(&record)? {
                store.upsert_imported_run_with_mission_and_resource_projection(
                    &run,
                    mission.as_str(),
                    &projection,
                    true,
                )?;
            } else {
                store.upsert_imported_run_with_resource_projection(&run, &projection, true)?;
            }
        }
        Ok(record)
    }

    /// List every durable agent-task record from this store's own observation
    /// database.
    ///
    /// Queue scanning is a read of the observation DB, not of the record files,
    /// so a scan that resolved it ambiently would inspect one queue while the
    /// claim it produced mutated another (#7505).
    pub fn read_records(&self) -> Result<Vec<AgentTaskRunRecord>> {
        Ok(records_with_health(observation_runs_bounded_in_store(self, 1000)?)?.0)
    }

    /// Read every retry successor of `source_run_id` from this store's own
    /// observation database.
    pub(crate) fn read_retry_successors(
        &self,
        source_run_id: &str,
    ) -> Result<Vec<AgentTaskRunRecord>> {
        let observation = self.open_observation_initialized()?;
        let page = observation.list_runs_by_retry_of_page(
            "agent-task",
            source_run_id,
            RETRY_SUCCESSOR_SCAN_LIMIT,
        )?;
        if page.truncated {
            return Err(Error::internal_unexpected(format!(
                "retry lineage for {source_run_id} exceeded {RETRY_SUCCESSOR_SCAN_LIMIT} successors; \
                 refusing to answer from a truncated lineage"
            )));
        }
        page.runs.iter().map(snapshot_from_run).collect()
    }

    pub(crate) fn record_run_aggregate(
        &self,
        run_id: &str,
        plan: &AgentTaskPlan,
        aggregate: &AgentTaskAggregate,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_run_aggregate_in_store(self, run_id, plan, aggregate)
    }

    /// Whether this store's observation record predates typed agent-task
    /// metadata, so a legacy row can be resubmitted rather than rejected.
    ///
    /// The answer is a property of one observation database, so it has to be
    /// read from the same roots the caller is about to write the replacement
    /// record into (#7505).
    pub(crate) fn record_lacks_typed_metadata(&self, run_id: &str) -> Result<bool> {
        Ok(self
            .open_observation_initialized()?
            .get_run(run_id)?
            .is_some_and(|run| run.metadata_json.get("agent_task_run").is_none()))
    }

    /// Check this store for one exact durable run identity without Cook alias
    /// resolution.
    pub fn record_exists(&self, run_id: &str) -> Result<bool> {
        Ok(self
            .open_observation_initialized()?
            .get_run(run_id)?
            .is_some())
    }

    /// Check this store for one exact durable run identity without creating its
    /// observation database, running migrations, or triggering startup repair.
    pub fn record_exists_readonly(&self, run_id: &str) -> Result<bool> {
        Ok(self.open_observation_readonly()?.get_run(run_id)?.is_some())
    }

    /// This store's bounded page of raw durable agent-task observation rows.
    ///
    /// Record-health reconciliation classifies rows it cannot parse into typed
    /// records, so it cannot go through [`AgentTaskLifecycleStore::read_records`],
    /// which silently drops them. It needs the raw rows — and it needs them from
    /// the same roots it is about to commit the repaired record, or the
    /// quarantine stamp, back into (#7505).
    pub(crate) fn observation_runs(&self) -> Result<Vec<RunRecord>> {
        observation_runs_bounded_in_store(self, 1000)
    }

    pub fn read_records_with_health_bounded(
        &self,
        limit: usize,
    ) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
        let (mut records, health) =
            records_with_health(observation_runs_bounded_in_store(self, limit)?)?;
        super::lifecycle_ops::sort_records_newest_first(&mut records);
        Ok((records, health))
    }

    /// Read every durable registry record in this store without a display bound.
    pub fn read_all_records_with_health(
        &self,
    ) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
        let store = self.open_observation_readonly()?;
        records_with_health(store.list_runs_all(RunListFilter {
            kind: Some("agent-task".to_string()),
            ..Default::default()
        })?)
    }

    /// Non-terminal agent-task records. Queued and running states persist as
    /// a `running` run, so the indexed status query replaces decoding the
    /// whole historical registry to find the few live records.
    pub fn read_active_records(
        &self,
    ) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
        let store = self.open_observation_readonly()?;
        let active: Vec<RunRecord> = store
            .list_active_runs()?
            .into_iter()
            .filter(|run| run.kind == "agent-task")
            .collect();
        let (mut records, health) = records_with_health(active)?;
        records.retain(|record| !record.state().is_terminal());
        Ok((records, health))
    }

    /// Read one immutable-keyset page of typed agent-task records without
    /// loading the full historical registry.
    pub(crate) fn read_record_page(
        &self,
        after: Option<ObservationRunCursor>,
        limit: usize,
    ) -> Result<(Vec<AgentTaskRunRecord>, bool, Option<ObservationRunCursor>)> {
        let (records, _, truncated, next) = self.read_record_keyset_page(after, limit)?;
        Ok((
            records.into_iter().map(|record| record.record).collect(),
            truncated,
            next,
        ))
    }

    /// Read one immutable-keyset page, omitting unreadable rows.
    /// The continuation is based on the
    /// physical observation page, so unreadable rows never stall a walk.
    pub(crate) fn read_record_keyset_page(
        &self,
        after: Option<ObservationRunCursor>,
        limit: usize,
    ) -> Result<(
        Vec<AgentTaskRecordPageRecord>,
        usize,
        bool,
        Option<ObservationRunCursor>,
    )> {
        let page = self
            .open_observation_readonly()?
            .list_runs_page(RunListFilter {
                kind: Some("agent-task".to_string()),
                limit: Some(i64::try_from(limit.clamp(1, 101)).expect("bounded page limit")),
                after,
                ..Default::default()
            })?;
        let physical_count = page.runs.len();
        let records = page_records(page.runs)?;
        Ok((records, physical_count, page.truncated, page.next_cursor))
    }

    pub(crate) fn read_mission_record_page(
        &self,
        mission_id: &str,
        after: Option<ObservationRunCursor>,
        limit: usize,
    ) -> Result<(Vec<AgentTaskRunRecord>, bool, Option<ObservationRunCursor>)> {
        let observation = self.open_observation_readonly()?;
        let page = observation.list_mission_runs_page(mission_id, after.as_ref(), limit)?;
        let records = page
            .runs
            .iter()
            .map(snapshot_from_run)
            .collect::<Result<Vec<_>>>()?;
        Ok((records, page.truncated, page.next_cursor))
    }

    pub(crate) fn read_mission(
        &self,
        mission_id: &str,
    ) -> Result<Option<homeboy_core::observation::MissionRecord>> {
        self.open_observation_readonly()?.get_mission(mission_id)
    }

    pub(crate) fn read_mission_page(
        &self,
        after: Option<&homeboy_core::observation::MissionCursor>,
        limit: usize,
    ) -> Result<homeboy_core::observation::MissionPage> {
        self.open_observation_readonly()?
            .list_missions_page(after, limit)
    }

    /// Register a Cook attempt using this store's record, lock, index, and
    /// terminal-projection roots.
    pub fn record_cook_attempt(
        &self,
        cook_id: &str,
        attempt: u32,
        run_id: &str,
    ) -> Result<AgentTaskCookIndex> {
        super::lifecycle_ops::record_cook_attempt_in_store(self, cook_id, attempt, run_id)
    }

    /// Persist Cook phase and an optional provider activity sample using this
    /// store's durable lifecycle roots.
    pub fn record_cook_progress_with_activity(
        &self,
        run_id: &str,
        phase: &str,
        attempt: u32,
        detail: Option<&str>,
        activity: Option<Value>,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_cook_progress_with_activity_in_store(
            self, run_id, phase, attempt, detail, activity,
        )
    }

    pub(crate) fn record_metadata_value(
        &self,
        run_id: &str,
        key: &str,
        value: Value,
    ) -> Result<()> {
        let run_id = sanitize_run_id(run_id);
        self.mutate_record(&run_id, |record| {
            record
                .ensure_metadata_object()
                .insert(key.to_string(), value.clone());
            record.updated_at = Some(super::now_timestamp());
            true
        })
        .map(|_| ())
    }

    /// Atomically persist a metadata value only when it is absent or already
    /// byte-for-byte identical. This prevents concurrent recovery callers from
    /// publishing against a receipt another caller authored.
    pub(crate) fn compare_and_set_metadata_value(
        &self,
        run_id: &str,
        key: &str,
        value: Value,
    ) -> Result<()> {
        let run_id = sanitize_run_id(run_id);
        let mut conflict = false;
        self.mutate_record(&run_id, |record| {
            let metadata = record.ensure_metadata_object();
            match metadata.get(key) {
                Some(existing) if existing == &value => false,
                Some(_) => {
                    conflict = true;
                    false
                }
                None => {
                    metadata.insert(key.to_string(), value.clone());
                    record.updated_at = Some(super::now_timestamp());
                    true
                }
            }
        })?;
        if conflict {
            return Err(Error::validation_invalid_argument(
                key,
                "durable metadata already contains a different value",
                Some(run_id),
                None,
            ));
        }
        Ok(())
    }

    pub fn read_record_bounded(&self, run_id: &str) -> Result<AgentTaskRunRecord> {
        let store = self.open_observation_readonly()?;
        let run = store.get_run(run_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                format!("agent-task run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            )
        })?;
        snapshot_from_run(&run)
    }

    /// Read one record and its mirrored aggregate from the same read-only
    /// SQLite observation. A missing mirror is returned as `None`; callers must
    /// not pair that record with the independently materialized aggregate cache.
    pub fn read_record_with_aggregate_bounded(
        &self,
        run_id: &str,
    ) -> Result<(AgentTaskRunRecord, Option<AgentTaskAggregate>)> {
        let store = self.open_observation_readonly()?;
        let run = store.get_run(run_id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                format!("agent-task run record not found: {run_id}"),
                Some(run_id.to_string()),
                None,
            )
        })?;
        record_and_aggregate_from_run(&run)
    }

    /// Replace a record unconditionally, ignoring its revision. Test fixtures
    /// only: production commits a snapshot through [`Self::commit_record`] or a
    /// typed [`Self::transition`], both fenced on the revision it was read at.
    #[cfg(any(test, feature = "test-support"))]
    pub fn write_record(&self, record: &AgentTaskRunRecord) -> Result<()> {
        self.write_record_with_aggregate(
            record,
            read_mirrored_aggregate_in_store(self, &record.run_id)?,
        )
        .map(|_| ())
    }

    /// [`Self::write_record`] that syncs the copy's revision. Test fixtures only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn write_record_in_place(&self, record: &mut AgentTaskRunRecord) -> Result<()> {
        let committed = self.write_record_with_aggregate(
            record,
            read_mirrored_aggregate_in_store(self, &record.run_id)?,
        )?;
        record.revision = committed.revision;
        Ok(())
    }

    /// Commit a snapshot the caller read and annotated, at the revision it was
    /// read at (#15718). A writer that committed in between makes this fail
    /// with an `agent_task_transition_conflict` and write nothing, instead of
    /// being silently overwritten.
    ///
    /// The annotation may not change the run state, the Lab handoff or
    /// workspace ownership; those move only through
    /// [`Self::advance_record`] or a terminal transition. The caller's
    /// revision is synced to the committed one, so it can keep using `record`.
    pub fn commit_record(&self, record: &mut AgentTaskRunRecord) -> Result<()> {
        self.commit_prepared(record, None, true)
    }

    /// Commit a typed non-terminal state or ownership change prepared from
    /// the record at `record.revision`. Fenced exactly like
    /// [`Self::commit_record`] and validated against the record it replaces.
    pub(crate) fn advance_record(
        &self,
        record: &mut AgentTaskRunRecord,
        kind: AgentTaskAdvance,
    ) -> Result<()> {
        self.commit_prepared(record, Some(kind), true)
    }

    /// Import a record and aggregate whose authority is another store (a Lab
    /// runner's result), replacing whatever this store holds for the run.
    ///
    /// The imported revision belongs to the remote store, so it is replaced by
    /// the local one read under the config lock; the write is fenced at that
    /// revision and recorded as an `import` transition.
    pub(crate) fn import_record_with_aggregate(
        &self,
        record: &mut AgentTaskRunRecord,
        aggregate: &AgentTaskAggregate,
    ) -> Result<()> {
        let committed = self.with_config_lock(|| {
            let stored = self.read_record(&record.run_id).ok();
            record.revision = stored.as_ref().map_or(0, |stored| stored.revision);
            let committed = write_record_fenced(
                self,
                record,
                Some(aggregate.clone()),
                true,
                true,
                RecordWriteGuard {
                    expected_revision: record.revision,
                    from: stored.map_or(record.state(), |stored| stored.state()),
                    transition: Some("import"),
                },
            )?;
            self.write_aggregate(&record.run_id, aggregate)?;
            Ok(committed)
        })?;
        self.project_terminal_record_after_unlock(&committed.run_id)?;
        record.revision = committed.revision;
        Ok(())
    }

    /// [`Self::commit_record`] without generated progress events.
    pub(crate) fn commit_record_without_events(
        &self,
        record: &mut AgentTaskRunRecord,
    ) -> Result<()> {
        self.commit_prepared(record, None, false)
    }

    fn commit_prepared(
        &self,
        record: &mut AgentTaskRunRecord,
        kind: Option<AgentTaskAdvance>,
        emit_events: bool,
    ) -> Result<()> {
        let mut prepared = record.clone();
        super::workspace_claims::renew_record_workspace_owner_in_store(
            &super::workspace_claims::workspace_claim_store_at(self.data_root()),
            &mut prepared,
        )?;
        let committed =
            self.with_config_lock(|| self.commit_prepared_locked(&prepared, kind, emit_events))?;
        self.project_terminal_record_after_unlock(&committed.run_id)?;
        record.revision = committed.revision;
        Ok(())
    }

    /// [`Self::commit_record`] for a caller that owns the config lock and
    /// decided from a record read under it. Terminal authority is projected
    /// by the caller after the lock is released.
    pub(crate) fn commit_record_locked_without_terminal_projection(
        &self,
        record: &AgentTaskRunRecord,
    ) -> Result<AgentTaskRunRecord> {
        self.commit_prepared_locked(record, None, true)
    }

    /// [`Self::advance_record`] for a caller that owns the config lock.
    pub(crate) fn advance_record_locked_without_terminal_projection(
        &self,
        record: &AgentTaskRunRecord,
        kind: AgentTaskAdvance,
    ) -> Result<AgentTaskRunRecord> {
        self.commit_prepared_locked(record, Some(kind), true)
    }

    fn commit_prepared_locked(
        &self,
        prepared: &AgentTaskRunRecord,
        kind: Option<AgentTaskAdvance>,
        emit_events: bool,
    ) -> Result<AgentTaskRunRecord> {
        if kind == Some(AgentTaskAdvance::Submit) {
            // A submission may create the row, so there is no stored record
            // to validate against; the fence alone proves it replaces exactly
            // the record the submission merged from (revision 0: none).
            return write_record_fenced(
                self,
                prepared,
                read_mirrored_aggregate_in_store(self, &prepared.run_id)?,
                true,
                emit_events,
                RecordWriteGuard::annotation(prepared),
            );
        }
        let transition = match kind {
            Some(kind) => AgentTaskTransition::Advance {
                kind,
                prepared: Box::new(prepared.clone()),
            },
            None => AgentTaskTransition::Annotate {
                prepared: Box::new(prepared.clone()),
            },
        };
        self.transition_locked_with_events(
            &prepared.run_id,
            prepared.revision,
            transition,
            emit_events,
        )?
        .map_err(|conflict| AgentTaskTransitionError::Conflict(conflict).into())
    }

    /// Rearm a retryable zero-provider record as queued. The caller's copy
    /// carries whichever controller runtime its submission admitted.
    pub(crate) fn rearm_pre_execution_record(
        &self,
        record: &AgentTaskRunRecord,
    ) -> Result<AgentTaskRunRecord> {
        self.with_config_lock(|| {
            let existing = self.read_record(&record.run_id)?;
            if !existing.state().is_terminal()
                || !crate::agent_task_service::has_unambiguous_zero_execution(&existing)
                || !crate::agent_task_service::cook_pre_execution::retryable_pre_execution_failure(
                    &existing,
                )
                || existing.metadata["provider_executions_consumed"]
                    .as_u64()
                    .unwrap_or(0)
                    != 0
            {
                return Err(Error::validation_invalid_argument(
                    "pre_execution_recovery",
                    "rearming requires a retryable terminal pre-execution record with zero provider executions",
                    Some(record.run_id.clone()),
                    None,
                ));
            }
            if record.state() != AgentTaskRunState::Queued
                || record.run_id != existing.run_id
                || record.plan_id != existing.plan_id
            {
                return Err(Error::validation_invalid_argument(
                    "pre_execution_recovery",
                    "rearming must preserve the durable run and plan identity while rearming it as queued",
                    Some(record.run_id.clone()),
                    None,
                ));
            }

            let mut rebound = record.clone();
            rebound.metadata["pre_execution_recovery"] = serde_json::json!({
                "schema": "homeboy/pre-execution-recovery/v1",
                "reason": "retryable_pre_execution_failure",
                "provider_executions_consumed": 0,
                "recovered_at": super::now_timestamp(),
            });
            rebound.metadata["controller_identity"] =
                serde_json::json!(homeboy_core::build_identity::current().display);
            // Rearming replaces the terminal record with the caller's queued
            // copy, so it commits only at the revision that copy was read at.
            let committed = write_record_fenced(
                self,
                &rebound,
                read_mirrored_aggregate_in_store(self, &rebound.run_id)?,
                false,
                true,
                RecordWriteGuard {
                    expected_revision: rebound.revision,
                    from: existing.state(),
                    transition: Some("rearm"),
                },
            )?;
            // A pre-execution failure writes a synthetic failed aggregate for
            // durable diagnostics. Once the zero-provider attempt is rearmed,
            // that file is no longer an execution result. Leaving it behind
            // lets the next detached-handoff write mirror the stale failure
            // back into the now-queued record (#13552).
            let aggregate_path = self.aggregate_path(&rebound.run_id);
            match fs::remove_file(&aggregate_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(Error::internal_io(
                        format!("remove rearmed pre-execution aggregate: {error}"),
                        Some(aggregate_path.display().to_string()),
                    ));
                }
            }
            Ok(committed)
        })
    }

    /// Unconditional record/aggregate replacement. Test fixtures only.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn write_aggregate_and_record_locked_without_terminal_projection(
        &self,
        record: &AgentTaskRunRecord,
        aggregate: &AgentTaskAggregate,
    ) -> Result<AgentTaskRunRecord> {
        let committed = write_record_with_aggregate_without_workspace_authority(
            self,
            record,
            Some(aggregate.clone()),
        )?;
        #[cfg(test)]
        if INTERRUPT_AFTER_TERMINAL_COMMIT.replace(false) {
            return Err(Error::internal_io(
                "injected interruption after terminal lifecycle commit",
                Some(record.run_id.clone()),
            ));
        }
        self.write_aggregate(&record.run_id, aggregate)?;
        Ok(committed)
    }

    pub fn mutate_record(
        &self,
        run_id: &str,
        mutate: impl FnOnce(&mut AgentTaskRunRecord) -> bool,
    ) -> Result<Option<AgentTaskRunRecord>> {
        let record = self.with_config_lock(|| {
            self.mutate_record_locked_without_terminal_projection(run_id, mutate)
        })?;
        if let Some(record) = record.as_ref() {
            self.project_terminal_record_after_unlock(&record.run_id)?;
        }
        Ok(record)
    }

    pub(crate) fn mutate_record_locked_without_terminal_projection(
        &self,
        run_id: &str,
        mutate: impl FnOnce(&mut AgentTaskRunRecord) -> bool,
    ) -> Result<Option<AgentTaskRunRecord>> {
        // The read self-heals this row's own resource projection if missing,
        // so a legacy row does not defer that repair to an unrelated read.
        //
        // The mutation is applied to a record read under the config lock and
        // committed at the revision it was read at, so it can never replace a
        // write it did not see (#15718). It is not validated as an annotation:
        // the decision is made from the fresh record, which is what a typed
        // transition would validate against.
        let mut record = self.read_record(run_id)?;
        if !mutate(&mut record) {
            return Ok(None);
        }
        write_record_fenced(
            self,
            &record,
            read_mirrored_aggregate_in_store(self, &record.run_id)?,
            true,
            true,
            RecordWriteGuard::annotation(&record),
        )
        .map(Some)
    }

    /// Commit a typed terminal transition only while the stored run revision
    /// still equals `expected_revision` (#15718).
    ///
    /// The check runs in the same SQL statement as the row replacement, the
    /// committed revision is the stored one plus one, and a `run.transitioned`
    /// event is appended in that transaction. A stale `expected_revision`
    /// returns [`AgentTaskTransitionError::Conflict`] and writes nothing.
    ///
    /// The edge from the stored state to the committed one must be legal in
    /// [`AgentTaskRunState::can_transition_to`]; an illegal edge is refused
    /// with `agent_task_illegal_transition` and writes nothing.
    pub fn transition(
        &self,
        run_id: &str,
        expected_revision: u64,
        transition: AgentTaskTransition,
    ) -> std::result::Result<AgentTaskRunRecord, AgentTaskTransitionError> {
        let run_id = sanitize_run_id(run_id);
        let committed = self.with_config_lock(|| {
            self.transition_locked_without_terminal_projection(
                &run_id,
                expected_revision,
                transition,
            )
        })?;
        let committed = committed.map_err(AgentTaskTransitionError::Conflict)?;
        self.project_terminal_record_after_unlock(&committed.run_id)?;
        Ok(committed)
    }

    /// Decide a terminal transition from the current record and commit it at
    /// that record's revision, all under the config lock.
    ///
    /// `decide` returning `None` is a successful no-op. A conflict re-reads and
    /// re-decides, which is safe because the decision is re-evaluated against
    /// the fresh record; after [`TRANSITION_RETRY_ATTEMPTS`] the conflict is
    /// surfaced.
    pub(crate) fn transition_when(
        &self,
        run_id: &str,
        decide: impl FnMut(&AgentTaskRunRecord) -> Option<AgentTaskTransition>,
    ) -> Result<Option<AgentTaskRunRecord>> {
        let run_id = sanitize_run_id(run_id);
        let committed = self.with_config_lock(|| {
            self.transition_when_locked_without_terminal_projection(&run_id, decide)
        })?;
        if let Some(record) = committed.as_ref() {
            self.project_terminal_record_after_unlock(&record.run_id)?;
        }
        Ok(committed)
    }

    /// The locked body of [`Self::transition_when`], for callers that already
    /// own this store's config lock. Terminal authority is projected by the
    /// caller only after the lock is released.
    pub(crate) fn transition_when_locked_without_terminal_projection(
        &self,
        run_id: &str,
        mut decide: impl FnMut(&AgentTaskRunRecord) -> Option<AgentTaskTransition>,
    ) -> Result<Option<AgentTaskRunRecord>> {
        let mut attempts = 0;
        loop {
            let current = self.read_record(run_id)?;
            let Some(transition) = decide(&current) else {
                return Ok(None);
            };
            match self.transition_locked_without_terminal_projection(
                run_id,
                current.revision,
                transition,
            )? {
                Ok(committed) => return Ok(Some(committed)),
                Err(conflict) => {
                    attempts += 1;
                    if attempts >= TRANSITION_RETRY_ATTEMPTS {
                        return Err(AgentTaskTransitionError::Conflict(conflict).into());
                    }
                }
            }
        }
    }

    /// Commit a terminal record prepared from a caller-held snapshot, at that
    /// snapshot's revision (#15718).
    ///
    /// A terminal decision another writer committed after the snapshot was
    /// read is never replaced: nothing is written and the stored record is
    /// returned as [`TerminalRecordCommit::Superseded`]. An intervening live
    /// write is rebased over, keeping the last-writer semantics the plain
    /// `write_record` this replaces had for non-terminal fields.
    pub(crate) fn commit_terminal_record(
        &self,
        prepared: &AgentTaskRunRecord,
    ) -> Result<TerminalRecordCommit> {
        let committed = self.with_config_lock(|| {
            let mut expected = prepared.revision;
            let mut attempts = 0;
            loop {
                let transition = AgentTaskTransition::CommitRecord {
                    prepared: Box::new(prepared.clone()),
                };
                match self.transition_locked_without_terminal_projection(
                    &prepared.run_id,
                    expected,
                    transition,
                )? {
                    Ok(committed) => return Ok(TerminalRecordCommit::Committed(committed)),
                    Err(conflict) if conflict.actual_state.is_some_and(|s| s.is_terminal()) => {
                        return Ok(TerminalRecordCommit::Superseded(
                            self.read_record(&prepared.run_id)?,
                        ));
                    }
                    Err(conflict) => {
                        attempts += 1;
                        if attempts >= TRANSITION_RETRY_ATTEMPTS {
                            return Err(AgentTaskTransitionError::Conflict(conflict).into());
                        }
                        expected = conflict.actual_revision;
                    }
                }
            }
        })?;
        if let TerminalRecordCommit::Committed(record) = &committed {
            self.project_terminal_record_after_unlock(&record.run_id)?;
        }
        Ok(committed)
    }

    /// Commit a terminal record/aggregate pair decided, under this store's
    /// config lock, from the record at `record.revision`. The compare-and-swap
    /// refuses the commit if a writer outside the lock moved the run since;
    /// the conflict is surfaced and nothing is written (#15718).
    ///
    /// Artifact and workspace authority projection must happen only after the
    /// caller releases the lock.
    pub(crate) fn commit_terminal_aggregate_locked_without_terminal_projection(
        &self,
        record: &AgentTaskRunRecord,
        aggregate: &AgentTaskAggregate,
    ) -> Result<AgentTaskRunRecord> {
        let transition = AgentTaskTransition::ProjectAggregate {
            prepared: Box::new(record.clone()),
            aggregate: Box::new(aggregate.clone()),
        };
        self.transition_locked_without_terminal_projection(
            &record.run_id,
            record.revision,
            transition,
        )?
        .map_err(|conflict| AgentTaskTransitionError::Conflict(conflict).into())
    }

    /// Commit a terminal aggregate projection prepared from a caller-held
    /// snapshot, at that snapshot's revision.
    ///
    /// On conflict the projection is rebased onto the stored revision only
    /// while that cannot replace a terminal decision the caller did not see:
    /// the stored run is still live, it is in the terminal state the caller
    /// decided from (a re-projection of later evidence), or it is already in
    /// the state this projection commits (an idempotent retry). A rebase keeps
    /// today's last-writer semantics for the non-terminal fields written in
    /// between. Any other conflict is surfaced and nothing is written.
    pub(crate) fn project_terminal_aggregate(
        &self,
        prepared: &AgentTaskRunRecord,
        decided_from: AgentTaskRunState,
        aggregate: &AgentTaskAggregate,
    ) -> Result<AgentTaskRunRecord> {
        let committed = self.with_config_lock(|| {
            let mut expected = prepared.revision;
            let mut attempts = 0;
            loop {
                let transition = AgentTaskTransition::ProjectAggregate {
                    prepared: Box::new(prepared.clone()),
                    aggregate: Box::new(aggregate.clone()),
                };
                match self.transition_locked_without_terminal_projection(
                    &prepared.run_id,
                    expected,
                    transition,
                )? {
                    Ok(committed) => return Ok(committed),
                    Err(conflict) => {
                        attempts += 1;
                        let rebase = match conflict.actual_state {
                            Some(state) if state.is_terminal() => {
                                state == decided_from || state == prepared.state()
                            }
                            _ => true,
                        };
                        if !rebase || attempts >= TRANSITION_RETRY_ATTEMPTS {
                            return Err(AgentTaskTransitionError::Conflict(conflict).into());
                        }
                        expected = conflict.actual_revision;
                    }
                }
            }
        })?;
        self.project_terminal_record_after_unlock(&committed.run_id)?;
        Ok(committed)
    }

    /// The locked body of [`Self::transition`]. Terminal authority is
    /// projected by the caller only after the lock is released.
    pub(crate) fn transition_locked_without_terminal_projection(
        &self,
        run_id: &str,
        expected_revision: u64,
        transition: AgentTaskTransition,
    ) -> Result<std::result::Result<AgentTaskRunRecord, AgentTaskTransitionConflict>> {
        self.transition_locked_with_events(run_id, expected_revision, transition, true)
    }

    fn transition_locked_with_events(
        &self,
        run_id: &str,
        expected_revision: u64,
        transition: AgentTaskTransition,
        emit_events: bool,
    ) -> Result<std::result::Result<AgentTaskRunRecord, AgentTaskTransitionConflict>> {
        let name = transition.name();
        let event = transition.records_event().then_some(name);
        let stored = self.read_record(run_id)?;
        let conflict = |actual_revision, actual_state| AgentTaskTransitionConflict {
            run_id: run_id.to_string(),
            expected_revision,
            actual_revision,
            actual_state,
            transition: name,
        };
        if stored.revision != expected_revision {
            return Ok(Err(conflict(stored.revision, Some(stored.state()))));
        }
        let from = stored.state();
        let stored_aggregate = read_mirrored_aggregate_in_store(self, run_id)?;
        let (next, aggregate, write_cache) = transition.apply(stored, stored_aggregate)?;
        let guard = RecordWriteGuard {
            expected_revision,
            from,
            transition: event,
        };
        match write_record_guarded(
            self,
            &next,
            aggregate.clone(),
            true,
            emit_events,
            RecordWriteFence::Revision(&guard),
        )? {
            GuardedRecordWrite::Conflict {
                actual_revision,
                actual_state,
            } => Ok(Err(conflict(actual_revision, actual_state))),
            GuardedRecordWrite::Committed(committed) => {
                if write_cache {
                    #[cfg(test)]
                    if INTERRUPT_AFTER_TERMINAL_COMMIT.replace(false) {
                        return Err(Error::internal_io(
                            "injected interruption after terminal lifecycle commit",
                            Some(run_id.to_string()),
                        ));
                    }
                    if let Some(aggregate) = aggregate.as_ref() {
                        self.write_aggregate(run_id, aggregate)?;
                    }
                }
                Ok(Ok(committed))
            }
        }
    }

    /// Complete terminal projection after the record lock has been released:
    /// receipt first, then the workspace owner lease.
    pub(crate) fn project_terminal_record_after_unlock(&self, run_id: &str) -> Result<()> {
        // Terminal projection reads back the row this same commit just wrote,
        // self-healing its own resource projection if missing.
        let record = self.read_record(run_id)?;
        self.workspace_terminal_authority_store()
            .persist_terminal_from_record(&record)
            .and_then(|_| {
                super::workspace_claims::release_terminal_record_workspace_owner_in_store(
                    &super::workspace_claims::workspace_claim_store_at(self.data_root()),
                    &record,
                )
            })
    }

    /// Unconditional record/aggregate replacement. Test fixtures only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn write_aggregate_and_record(
        &self,
        record: &AgentTaskRunRecord,
        aggregate: &AgentTaskAggregate,
    ) -> Result<PathBuf> {
        let committed = self.with_config_lock(|| {
            self.write_aggregate_and_record_locked_without_terminal_projection(record, aggregate)
        })?;
        self.project_terminal_record_after_unlock(&committed.run_id)?;
        Ok(self.aggregate_path(&committed.run_id))
    }

    #[cfg(any(test, feature = "test-support"))]
    fn write_record_with_aggregate(
        &self,
        record: &AgentTaskRunRecord,
        aggregate: Option<AgentTaskAggregate>,
    ) -> Result<AgentTaskRunRecord> {
        let mut record = record.clone();
        super::workspace_claims::renew_record_workspace_owner_in_store(
            &super::workspace_claims::workspace_claim_store_at(self.data_root()),
            &mut record,
        )?;
        let committed = self.with_config_lock(|| {
            write_record_with_aggregate_without_workspace_authority(self, &record, aggregate)
        })?;
        self.project_terminal_record_after_unlock(&committed.run_id)?;
        Ok(committed)
    }
}

/// A decoded lifecycle record with the exact observation keyset that produced
/// it. Filtered discovery must retain this boundary rather than reconstructing
/// it from record fields, which are not the observation ordering contract.
pub(crate) struct AgentTaskRecordPageRecord {
    pub record: AgentTaskRunRecord,
    pub cursor: ObservationRunCursor,
}

fn default_store() -> Result<AgentTaskLifecycleStore> {
    AgentTaskLifecycleStore::from_current_environment()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// One-shot write failure owned by the test thread that armed it. A global
    /// atomic let unrelated parallel tests consume each other's fault (#11897).
    static FAIL_NEXT_RECORD_WRITE: Cell<bool> = const { Cell::new(false) };
    /// One-shot post-commit failure owned by the test thread that armed it.
    static INTERRUPT_AFTER_TERMINAL_COMMIT: Cell<bool> = const { Cell::new(false) };
}

/// A crashed notifier cannot release its provisional claim. A bounded lease
/// keeps that crash window from permanently suppressing a detached resume.
const COOK_NOTIFICATION_CLAIM_LEASE: Duration = Duration::from_secs(5 * 60);

pub(super) fn write_plan_in_store(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<PathBuf> {
    store.with_config_lock(|| {
        validate_execution_budget(plan)?;
        validate_managed_services(plan)?;
        let path = store.controller_plan_path(run_id);
        write_private_json(&path, plan)?;
        Ok(path)
    })
}

pub(super) fn read_plan_path(path: &str) -> Result<AgentTaskPlan> {
    let plan = read_json(&PathBuf::from(path))?;
    validate_execution_budget(&plan)?;
    validate_managed_services(&plan)?;
    Ok(plan)
}

fn validate_execution_budget(plan: &AgentTaskPlan) -> Result<()> {
    match plan.options.execution_budget.version {
        crate::agent_task_scheduler::AgentTaskExecutionBudget::VERSION => Ok(()),
        version => Err(Error::validation_invalid_argument(
            "execution_budget.version",
            format!(
                "unsupported agent-task execution budget version {version}; this Homeboy build supports version {}",
                crate::agent_task_scheduler::AgentTaskExecutionBudget::VERSION
            ),
            Some(version.to_string()),
            None,
        )),
    }
}

fn validate_managed_services(plan: &AgentTaskPlan) -> Result<()> {
    plan.validate_managed_services().map_err(|message| {
        Error::validation_invalid_argument("services.cleanup_deadline_ms", message, None, None)
    })
}

#[cfg(test)]
pub(super) fn write_aggregate(run_id: &str, aggregate: &AgentTaskAggregate) -> Result<PathBuf> {
    default_store()?.write_aggregate(run_id, aggregate)
}

pub(super) fn read_aggregate(run_id: &str) -> Result<AgentTaskAggregate> {
    default_store()?.read_aggregate(run_id)
}

/// Maximum controller-local aggregate size accepted by a read-only inspection.
///
/// Aggregates are compact lifecycle projections, not artifact payloads. Keeping
/// this cap below the general output limits makes a malformed or accidentally
/// redirected aggregate fail as partial evidence rather than consuming an
/// unbounded amount of memory in `status`, `evidence`, or `review`.
pub(super) const DURABLE_AGGREGATE_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Read the controller-owned aggregate without consulting the mirrored
/// observation row. The reader checks metadata before allocating and takes one
/// extra byte while reading to defend against a file changing after `metadata`.
pub(super) fn read_aggregate_bounded(run_id: &str) -> Result<AgentTaskAggregate> {
    default_store()?.read_aggregate_bounded(run_id)
}

fn read_aggregate_bytes_bounded_in_store(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Vec<u8>> {
    let path = store.aggregate_path(run_id);
    let metadata = fs::metadata(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if metadata.len() > DURABLE_AGGREGATE_MAX_BYTES {
        let mut error = Error::internal_io(
            format!(
                "aggregate exceeds durable read size budget: {} bytes exceeds {} bytes",
                metadata.len(),
                DURABLE_AGGREGATE_MAX_BYTES
            ),
            Some(path.display().to_string()),
        );
        error.details = json!({ "reason_code": "durable_read.oversized" });
        return Err(error);
    }
    let mut file = fs::File::open(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let mut raw = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(DURABLE_AGGREGATE_MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if raw.len() as u64 > DURABLE_AGGREGATE_MAX_BYTES {
        let mut error = Error::internal_io(
            format!(
                "aggregate exceeds durable read size budget while reading: more than {} bytes",
                DURABLE_AGGREGATE_MAX_BYTES
            ),
            Some(path.display().to_string()),
        );
        error.details = json!({ "reason_code": "durable_read.oversized" });
        return Err(error);
    }
    Ok(raw)
}

pub(super) fn aggregate_path(run_id: &str) -> Result<PathBuf> {
    Ok(default_store()?.aggregate_path(run_id))
}

#[cfg(test)]
pub(super) fn write_record(record: &AgentTaskRunRecord) -> Result<()> {
    default_store()?.write_record(record)
}

/// Serialize a record read-modify-write so independent lifecycle projections do
/// not replace metadata written by another controller operation.
pub(super) fn mutate_record(
    run_id: &str,
    mutate: impl FnOnce(&mut AgentTaskRunRecord) -> bool,
) -> Result<Option<AgentTaskRunRecord>> {
    default_store()?.mutate_record(run_id, mutate)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn fail_next_record_write_for_test() {
    FAIL_NEXT_RECORD_WRITE.set(true);
}

#[cfg(test)]
pub(super) fn interrupt_after_terminal_commit_for_test() {
    INTERRUPT_AFTER_TERMINAL_COMMIT.set(true);
}

#[cfg(any(test, feature = "test-support"))]
fn write_record_with_aggregate_without_workspace_authority(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
) -> Result<AgentTaskRunRecord> {
    match write_record_guarded(
        lifecycle_store,
        record,
        aggregate,
        true,
        true,
        RecordWriteFence::Unfenced,
    )? {
        GuardedRecordWrite::Committed(committed) => Ok(committed),
        GuardedRecordWrite::Conflict { .. } => Err(Error::internal_unexpected(format!(
            "unfenced agent-task record write reported a revision conflict: {}",
            record.run_id
        ))),
    }
}

/// SQLite JSON path of [`AgentTaskRunRecord::revision`] inside the run row.
const RUN_REVISION_JSON_PATH: &str = "$.agent_task_run.revision";

/// The compare-and-swap half of every production record write (#15718).
pub(super) struct RecordWriteGuard {
    /// Commit only while the stored revision still equals this value.
    pub expected_revision: u64,
    /// Run state the transition was decided from, recorded on its event.
    pub from: AgentTaskRunState,
    /// Stable name of a typed transition, recorded as a `run.transitioned`
    /// event. `None` for an annotation, which records no transition.
    pub transition: Option<&'static str>,
}

impl RecordWriteGuard {
    /// Fence a write at the revision `record` was read at, without recording
    /// a transition event.
    pub(super) fn annotation(record: &AgentTaskRunRecord) -> Self {
        Self {
            expected_revision: record.revision,
            from: record.state(),
            transition: None,
        }
    }
}

/// How a record write is fenced against the stored revision.
pub(super) enum RecordWriteFence<'a> {
    /// Compare-and-swap: every production write.
    Revision(&'a RecordWriteGuard),
    /// Unconditional replacement. Exists only for test fixtures, so no
    /// production writer can commit a snapshot without a revision check.
    #[cfg(any(test, feature = "test-support"))]
    Unfenced,
}

impl RecordWriteFence<'_> {
    fn guard(&self) -> Option<&RecordWriteGuard> {
        match self {
            Self::Revision(guard) => Some(guard),
            #[cfg(any(test, feature = "test-support"))]
            Self::Unfenced => None,
        }
    }
}

/// Outcome of a guarded record write.
pub(super) enum GuardedRecordWrite {
    Committed(AgentTaskRunRecord),
    /// The stored revision moved; nothing was written.
    Conflict {
        actual_revision: u64,
        actual_state: Option<AgentTaskRunState>,
    },
}

fn stored_run_revision(metadata_json: &Value) -> u64 {
    metadata_json
        .pointer("/agent_task_run/revision")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// A fenced write whose conflict is surfaced as an `Error` carrying
/// `agent_task_transition_conflict`. Nothing is written on conflict.
fn write_record_fenced(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
    preserve_terminal: bool,
    emit_events: bool,
    guard: RecordWriteGuard,
) -> Result<AgentTaskRunRecord> {
    match write_record_guarded(
        lifecycle_store,
        record,
        aggregate,
        preserve_terminal,
        emit_events,
        RecordWriteFence::Revision(&guard),
    )? {
        GuardedRecordWrite::Committed(committed) => Ok(committed),
        GuardedRecordWrite::Conflict {
            actual_revision,
            actual_state,
        } => Err(
            AgentTaskTransitionError::Conflict(AgentTaskTransitionConflict {
                run_id: record.run_id.clone(),
                expected_revision: guard.expected_revision,
                actual_revision,
                actual_state,
                transition: guard.transition.unwrap_or("annotate"),
            })
            .into(),
        ),
    }
}

/// The single lifecycle write path. Every write bumps the run-level revision
/// in the same statement as the row replacement. A [`RecordWriteFence::Revision`]
/// makes it a compare-and-swap, and a typed transition additionally appends a
/// `run.transitioned` event in the same transaction (#15718).
/// This process's claim on the exact step that authorized it.
fn execution_step_owner(intent_id: Value) -> Result<Value> {
    Ok(json!({
        "intent_id": intent_id,
        "job_id": std::env::var(homeboy_core::daemon::step_job::STEP_JOB_ID_ENV).ok(),
        "pid": std::process::id(),
        "process_start_identity": homeboy_core::process::process_start_identity(std::process::id())
            .map_err(Error::internal_unexpected)?,
    }))
}

pub(super) fn write_record_guarded(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
    preserve_terminal: bool,
    emit_events: bool,
    fence: RecordWriteFence<'_>,
) -> Result<GuardedRecordWrite> {
    let guard = fence.guard();
    super::cancellation::validate_cancellation_invariant(record)?;
    #[cfg(any(test, feature = "test-support"))]
    if FAIL_NEXT_RECORD_WRITE.replace(false) {
        return Err(Error::internal_io(
            "injected lifecycle record persistence failure",
            Some(record.run_id.clone()),
        ));
    }
    let store = lifecycle_store.open_observation_initialized()?;
    let existing = store.get_run(&record.run_id)?;
    let homeboy_version = existing
        .as_ref()
        .map(|run| run.homeboy_version.clone())
        .unwrap_or_else(|| Some(build_identity::current().version));
    let existing_metadata = existing
        .map(|run| run.metadata_json)
        .unwrap_or_else(|| json!({}));
    let stored_revision = stored_run_revision(&existing_metadata);
    let stored_state = existing_metadata
        .pointer("/agent_task_run/state")
        .cloned()
        .and_then(|state| serde_json::from_value::<AgentTaskRunState>(state).ok());
    if let Some(guard) = guard {
        if guard.expected_revision != stored_revision {
            return Ok(GuardedRecordWrite::Conflict {
                actual_revision: stored_revision,
                actual_state: stored_state,
            });
        }
    }
    // The run state graph is enforced here, against the record the fenced
    // write was decided from, so no prepared record or mutation closure can
    // commit an edge it does not name (#15718). Only a test fixture writes
    // unfenced, and it seeds a state rather than taking an edge.
    if let (Some(guard), Some(from)) = (guard, stored_state) {
        if !from.can_transition_to(record.state()) {
            return Err(super::terminal_transition::illegal_state_transition(
                &record.run_id,
                from,
                record.state(),
                guard.transition.unwrap_or("annotate"),
                &format!(
                    "agent-task run `{}` cannot move from {from:?} to {:?}; nothing was written",
                    record.run_id,
                    record.state()
                ),
            ));
        }
    }
    let mut record = record.clone();
    let execution_intent = record
        .metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(crate::agent_task_service::PENDING_EXECUTION_INTENT))
        .map(serde_json::from_value::<homeboy_core::observation::WorkIntent>)
        .transpose()
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    if let Some(intent) = &execution_intent {
        intent.validate(&record.run_id)?;
        let execution = crate::agent_task_service::ExecutionStepIntent::parse(&intent.payload)?;
        if intent.kind != crate::agent_task_service::EXECUTION_STEP_KIND
            || intent.version != 1
            || intent.id != execution.intent_id
            || !execution.authorizes(&record)
        {
            return Err(Error::validation_invalid_argument(
                "execution_intent",
                "execution draft does not authorize its lifecycle record",
                None,
                None,
            ));
        }
    }
    // The store owns the revision: whatever the caller carried, the committed
    // row is one past the stored one. SQL repeats the bump atomically.
    record.revision = stored_revision + 1;
    if let Some(owner) =
        existing_metadata.pointer("/agent_task_run/metadata/client_context/caller_context")
    {
        if let Some(incoming) = record.metadata.pointer("/client_context/caller_context") {
            if incoming != owner {
                return Err(Error::validation_invalid_argument(
                    "caller_context",
                    "A durable task's original caller ownership cannot be reassigned",
                    Some(record.run_id.clone()),
                    None,
                ));
            }
        }
        if record.metadata["client_context"].is_null() {
            record.metadata["client_context"] = json!({});
        }
        record.metadata["client_context"]["caller_context"] = owner.clone();
        if record.metadata["caller_workspace"].is_null() {
            if let Some(workspace) =
                existing_metadata.pointer("/agent_task_run/metadata/caller_workspace")
            {
                record.metadata["caller_workspace"] = workspace.clone();
            }
        }
    }
    if let Some(requester) =
        existing_metadata.pointer("/agent_task_run/metadata/client_context/requester")
    {
        if record.metadata["client_context"].is_null() {
            record.metadata["client_context"] = json!({});
        }
        record.metadata["client_context"]["requester"] = requester.clone();
    }
    if record.metadata.get("cook_operation_claims").is_none() {
        if let Some(claims) = existing_metadata
            .pointer("/agent_task_run/metadata/cook_operation_claims")
            .cloned()
        {
            record.metadata["cook_operation_claims"] = claims;
        }
    }
    let run = homeboy_control_plane_contract::RunId::new(&record.run_id).map_err(|error| {
        Error::validation_invalid_argument(
            "run_id",
            error.to_string(),
            Some(record.run_id.clone()),
            None,
        )
    })?;
    let receipts = store
        .control_plane_event_receipt_digests(&run)?
        .into_iter()
        .collect();
    let ledger = store.control_plane_event_stream(&run)?.unwrap_or_default();
    let mut events = if emit_events {
        super::durable_progress::prepared_progress_events(&record, aggregate.as_ref())?
    } else {
        Vec::new()
    };
    if emit_events {
        events.extend(super::durable_progress::prepared_unreceipted_action_events(
            &record, &store, &receipts, &ledger,
        )?);
    }
    let transition = guard.and_then(|guard| guard.transition.map(|name| (guard.from, name)));
    if let Some((from, name)) = transition {
        // The transition is the cause; the progress events it produces follow
        // it, so a run's log still ends on its state event rather than on the
        // audit record.
        events.insert(
            0,
            super::durable_progress::prepared_transition_event(&record, from, name)?,
        );
    }
    let stored_snapshot = transition.is_none().then(|| existing_metadata.clone());
    let mut metadata_json =
        merge_observation_metadata(existing_metadata, observation_metadata(&record, aggregate)?);
    if !preserve_terminal {
        metadata_json["agent_task_aggregate"] = Value::Null;
    }
    let (status, finished_at) = if cook_owns_observation(lifecycle_store, &record) {
        if preserve_terminal && record.state().is_terminal() {
            store
                .get_run(&record.run_id)?
                .map(|run| (run.status, run.finished_at))
                .unwrap_or_else(|| (RunStatus::Running.as_str().to_string(), None))
        } else {
            (RunStatus::Running.as_str().to_string(), None)
        }
    } else if is_redirected_cook(&record) {
        redirected_cook_row_status(lifecycle_store, &store, &record.run_id)?
    } else {
        (
            run_status(record.state()).to_string(),
            terminal_finished_at(&record),
        )
    };
    if status == RunStatus::Running.as_str() {
        // A live owner write supersedes any stale verdict another process
        // stamped on the row; a `running` row cannot carry one (#15978).
        if let Some(object) = metadata_json.as_object_mut() {
            object.remove("homeboy_reconciled");
        }
    }
    if let Some(cook_id) = record
        .metadata
        .get("cook_id")
        .and_then(Value::as_str)
        .filter(|cook_id| !cook_id.is_empty() && *cook_id != record.run_id)
    {
        metadata_json["parent_run_id"] = json!(cook_id);
    }
    // The revision versions the run snapshot. A rewrite that leaves it
    // identical (an idempotent replay, or a write that only refreshes the
    // mirrored aggregate or row projection) cannot invalidate a decision made
    // from that snapshot, so it keeps the stored revision.
    let mut increment = true;
    if let Some(stored_snapshot) = stored_snapshot {
        let mut candidate = metadata_json["agent_task_run"].clone();
        match stored_snapshot.pointer("/agent_task_run/revision") {
            Some(revision) => candidate["revision"] = revision.clone(),
            None => {
                if let Some(run) = candidate.as_object_mut() {
                    run.remove("revision");
                }
            }
        }
        if stored_snapshot.get("agent_task_run") == Some(&candidate) {
            metadata_json["agent_task_run"] = candidate;
            record.revision = stored_revision;
            increment = false;
        }
    }
    let projected = RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: plan_id_component(&record),
        started_at: record.submitted_at.clone(),
        finished_at,
        status,
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version,
        git_sha: None,
        rig_id: None,
        metadata_json,
    };
    let record = normalize_decoded_record(&projected, record)?;
    let resource_projection = agent_task_record_write_projection(lifecycle_store, &store, &record)?;
    let mission = crate::agent_task_lifecycle::canonical_mission(&record)?;
    let mut intents = work_intents_for_record(&record)?;
    intents.extend(execution_intent);
    let applied = store.upsert_imported_run_with_events_intents_and_revision(
        &projected,
        preserve_terminal,
        mission.as_ref().map(|mission| mission.as_str()),
        Some(&resource_projection),
        &events,
        &intents,
        Some(RunMetadataRevision {
            path: RUN_REVISION_JSON_PATH,
            // An unchanged rewrite must not regress a revision a writer
            // outside the lock committed in between; it is skipped instead.
            expected: match guard {
                Some(guard) => Some(guard.expected_revision),
                None if !increment => Some(stored_revision),
                None => None,
            },
            increment,
        }),
    )?;
    let committed = store.get_run(&record.run_id)?.ok_or_else(|| {
        Error::internal_unexpected(format!(
            "committed agent-task run record is unavailable: {}",
            record.run_id
        ))
    })?;
    if !applied {
        let committed_revision = stored_run_revision(&committed.metadata_json);
        if committed_revision != stored_revision {
            if guard.is_some() {
                // Another writer committed between our read and the statement.
                return Ok(GuardedRecordWrite::Conflict {
                    actual_revision: committed_revision,
                    actual_state: committed
                        .metadata_json
                        .pointer("/agent_task_run/state")
                        .cloned()
                        .and_then(|state| serde_json::from_value(state).ok()),
                });
            }
            // An unchanged unfenced rewrite yields to the newer revision.
        } else {
            // The revision did not move, so the terminal-row guard refused
            // this write. Over a row the lifecycle itself finished, that is a
            // late live write losing to a final record, and the stored record
            // is the answer. Over any other row the refusal would drop a live
            // owner's write while reporting it committed (#15978), so it is an
            // error rather than a silent success.
            let stored_state = committed
                .metadata_json
                .pointer("/agent_task_run/state")
                .cloned()
                .and_then(|state| serde_json::from_value::<AgentTaskRunState>(state).ok());
            if !stored_state.is_some_and(AgentTaskRunState::is_terminal) {
                return Err(Error::internal_unexpected(format!(
                    "agent-task record write refused by the terminal-row guard: run {} is stored as `{}` over a non-terminal typed record and the refused write projected `{}`",
                    record.run_id, committed.status, projected.status,
                )));
            }
        }
    }
    if let Some(cook_id) = record
        .metadata
        .get("cook_id")
        .and_then(Value::as_str)
        .filter(|cook_id| !cook_id.is_empty() && *cook_id != record.run_id)
    {
        mirror_cook_row_in_store(lifecycle_store, &store, cook_id)?;
    }
    snapshot_from_run(&committed).map(GuardedRecordWrite::Committed)
}

/// A Cook handed off to its attempts: its own record holds the handoff, and
/// its progress is its latest attempt's.
fn is_redirected_cook(record: &AgentTaskRunRecord) -> bool {
    let handoff = &record.metadata["detached_cook_handoff"];
    handoff["cook_id"] == record.run_id && handoff["state"] == "redirected"
}

/// The row status a redirected Cook reports: its latest attempt's (#16179).
/// Until an attempt row exists, the Cook is running.
fn redirected_cook_row_status(
    lifecycle_store: &AgentTaskLifecycleStore,
    store: &homeboy_core::observation::ObservationStore,
    cook_id: &str,
) -> Result<(String, Option<String>)> {
    let latest = projected_cook_index_in_store(lifecycle_store, cook_id)?
        .map(|index| index.latest_run_id)
        .filter(|latest| latest != cook_id);
    Ok(
        match latest
            .map(|latest| store.get_run(&latest))
            .transpose()?
            .flatten()
        {
            Some(attempt) => (attempt.status, attempt.finished_at),
            None => (RunStatus::Running.as_str().to_string(), None),
        },
    )
}

/// Keep a redirected Cook's row on its latest attempt's status, so a Cook
/// whose attempt ended stops reading `running`, and one whose recovery starts
/// a new attempt reads `running` again (#16179). Called whenever an attempt
/// row is written and when the Cook registers a new latest attempt.
pub(crate) fn mirror_cook_row_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    store: &homeboy_core::observation::ObservationStore,
    cook_id: &str,
) -> Result<()> {
    let cook_id = sanitize_run_id(cook_id);
    if lifecycle_store
        .cook_dir(&cook_id)
        .join("recipe.json")
        .exists()
    {
        return Ok(());
    }
    let Some(cook) = store.get_run(&cook_id)? else {
        return Ok(());
    };
    let handoff = &cook.metadata_json["agent_task_run"]["metadata"]["detached_cook_handoff"];
    if handoff["cook_id"] != cook_id.as_str() || handoff["state"] != "redirected" {
        return Ok(());
    }
    let Some(index) = projected_cook_index_in_store(lifecycle_store, &cook_id)? else {
        return Ok(());
    };
    if index.latest_run_id != cook_id {
        store.mirror_run_status(&cook_id, &index.latest_run_id)?;
    }
    Ok(())
}

fn work_intents_for_record(
    record: &AgentTaskRunRecord,
) -> Result<Vec<homeboy_core::observation::WorkIntent>> {
    let continuation = &record.metadata["cook_continuation"];
    if continuation["state"] != "pending" {
        return Ok(Vec::new());
    }
    let Some(cook_id) = continuation["cook_id"].as_str() else {
        return Ok(Vec::new());
    };
    if continuation["run_id"].as_str() != Some(record.run_id.as_str()) {
        return Ok(Vec::new());
    }
    let generation = continuation["generation"].as_u64().unwrap_or(0);
    Ok(vec![homeboy_core::observation::WorkIntent {
        id: format!("terminal-cook:{cook_id}:{}:{generation}", record.run_id),
        run_id: record.run_id.clone(),
        kind: "terminal-cook-continuation".to_string(),
        version: 1,
        payload: json!({
            "schema": "homeboy/terminal-cook-continuation-request/v1",
            "cook_id": cook_id,
            "run_id": record.run_id,
            "generation": generation,
        }),
    }])
}
/// Rebuild one record's projection without disturbing Cook alias ownership.
///
/// An ordinary record write knows its own lifecycle state but nothing about
/// which attempt currently owns the Cook alias. Recomputing the projection from
/// the record alone would therefore publish an empty alias set and silently
/// retract an alias that [`AgentTaskLifecycleStore::write_cook_index_attempt_locked`] owns, so
/// the current alias and Cook index carry forward unchanged.
fn agent_task_record_write_projection(
    lifecycle_store: &AgentTaskLifecycleStore,
    observations: &ObservationStore,
    record: &AgentTaskRunRecord,
) -> Result<ControlPlaneResourceProjection> {
    let mut projection = agent_task_resource_projection(lifecycle_store, record, None)?;
    if let Some(existing) =
        observations.control_plane_resource_projection_exact("agent_task_run", &record.run_id)?
    {
        projection.aliases = existing.aliases;
        if let Some(cook_index) = existing.provenance.get("cook_index") {
            projection.provenance["cook_index"] = cook_index.clone();
        }
    }
    Ok(projection)
}

/// Agent-task owns the interpretation of this generic authority payload. The
/// observation store atomically persists it beside the run projection so action
/// admission can never observe a lifecycle version from another write.
fn agent_task_resource_projection(
    _lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    cook_index: Option<&AgentTaskCookIndex>,
) -> Result<ControlPlaneResourceProjection> {
    // Canonical resource IDs resolve directly and must not also occupy the
    // movable alias namespace used by Cook's latest-attempt pointer.
    let mut aliases = Vec::new();
    let cook_id = record
        .metadata
        .get("cook_id")
        .and_then(Value::as_str)
        .or_else(|| cook_index.map(|index| index.cook_id.as_str()));
    if let Some(cook_id) = cook_id {
        // Alias ownership is the SQLite projection committed with the lifecycle
        // record; the filesystem index is derived from it.
        if let Some(index) = cook_index {
            if index.latest_run_id == record.run_id && cook_id != record.run_id {
                aliases.push(cook_id.to_string());
            }
        }
    }
    Ok(ControlPlaneResourceProjection {
        resource_type: "agent_task_run".to_string(),
        resource_id: record.run_id.clone(),
        version: record
            .updated_at
            .clone()
            .unwrap_or_else(|| record.submitted_at.clone()),
        state: serde_json::to_value(record.state())
            .map_err(|error| Error::internal_json(error.to_string(), None))?
            .as_str()
            .unwrap_or("unknown")
            .to_string(),
        aliases,
        eligibility: serde_json::to_value(
            crate::agent_task_lifecycle::lifecycle_action_eligibility(record, None),
        )
        .map_err(|error| Error::internal_json(error.to_string(), None))?,
        provenance: json!({
            "source": "agent_task_lifecycle",
            "record_schema": record.schema,
            "persisted_data_window": "sqlite-canonical-cook-index/v1",
            // The complete index rematerializes the compatibility file after a
            // crash or write failure; it is never read as lifecycle authority.
            "cook_index": cook_index,
        }),
    })
}

pub(super) fn read_record(run_id: &str) -> Result<AgentTaskRunRecord> {
    default_store()?.read_record(run_id)
}

/// Read one durable record under the observation store's read-only busy budget.
/// Inspection must not initialize, migrate, or contend like a writer.
pub(super) fn read_record_bounded(run_id: &str) -> Result<AgentTaskRunRecord> {
    default_store()?.read_record_bounded(run_id)
}

/// Bypass typed record validation solely to seed corruption-recovery fixtures.
/// Production writes use `commit_record` or a typed transition; normal test
/// rewrites use `write_record`.
#[cfg(any(test, feature = "test-support"))]
pub(super) fn inject_raw_record_metadata_for_corruption_test(
    run_id: &str,
    inject: impl FnOnce(&mut Value),
) -> Result<()> {
    let store = ObservationStore::open_initialized_for_lifecycle()?;
    let mut run = store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("agent-task run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })?;
    inject(&mut run.metadata_json);
    store.upsert_imported_run(&run)
}

/// Commit one complete Cook index: its canonical SQLite projection and the
/// alias it owns. There is no filesystem copy.
///
/// Callers hand over the whole index rather than a set of attempt fields, so a
/// mutation to any other field — a cancellation fence above all — is persisted
/// instead of being silently dropped by a rebuild from storage.
pub(super) fn persist_cook_index_in_store(
    store: &AgentTaskLifecycleStore,
    index: &AgentTaskCookIndex,
) -> Result<()> {
    let cook_id = sanitize_run_id(&index.cook_id);
    // Publishing an alias is an index operation, not a record read: a root may
    // legitimately index an attempt whose lifecycle record it does not hold.
    let latest_projection = match store.read_record(&index.latest_run_id) {
        Ok(latest) => agent_task_resource_projection(store, &latest, Some(index))?,
        Err(error) if error.code == ErrorCode::ValidationInvalidArgument => {
            ControlPlaneResourceProjection {
                resource_type: "agent_task_run".to_string(),
                resource_id: index.latest_run_id.clone(),
                version: index
                    .attempts
                    .last()
                    .map(|entry| entry.recorded_at.clone())
                    .unwrap_or_default(),
                state: "unknown".to_string(),
                aliases: vec![cook_id.clone()],
                eligibility: json!({}),
                provenance: json!({
                    "source": "agent_task_lifecycle_cook_index",
                    "cook_index": index,
                }),
            }
        }
        Err(error) => return Err(error),
    };
    let observations = store.open_observation_initialized()?;
    let previous = observations.control_plane_resource_projection("agent_task_run", &cook_id)?;
    let mut projections = Vec::new();
    if let Some(previous) = previous.filter(|previous| previous.resource_id != index.latest_run_id)
    {
        let previous_record = store.read_record(&previous.resource_id)?;
        // The superseded attempt surrenders both the alias and the index. Only
        // the current alias owner may carry the Cook index, because a reader
        // that resolves this record must not find an index naming a different
        // owner.
        projections.push(agent_task_resource_projection(
            store,
            &previous_record,
            None,
        )?);
    }
    projections.push(latest_projection);
    observations.replace_control_plane_resource_projections(&projections)
}

#[cfg(test)]
pub(super) fn write_cook_index_for_test(index: &AgentTaskCookIndex) -> Result<()> {
    let cook_id = sanitize_run_id(&index.cook_id);
    // SQLite owns Cook alias resolution, so seeding a fixture index has to
    // publish the canonical projection rather than only its derived file.
    // Fixture attempts have no lifecycle records, so the alias owner is
    // declared directly instead of being rebuilt from one.
    default_store()?
        .open_observation_initialized()?
        .upsert_control_plane_resource_projection(&ControlPlaneResourceProjection {
            resource_type: "agent_task_run".to_string(),
            resource_id: index.latest_run_id.clone(),
            version: index
                .attempts
                .last()
                .map(|attempt| attempt.recorded_at.clone())
                .unwrap_or_else(|| "fixture".to_string()),
            state: "unknown".to_string(),
            aliases: vec![cook_id.clone()],
            eligibility: json!({}),
            provenance: json!({
                "source": "agent_task_lifecycle_test_fixture",
                "cook_index": index,
            }),
        })
}

pub(super) fn read_cook_index(cook_id: &str) -> Result<AgentTaskCookIndex> {
    default_store()?.read_cook_index(cook_id)
}

fn projected_cook_index_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<Option<AgentTaskCookIndex>> {
    let Some(projection) = store
        .open_observation_readonly()?
        .control_plane_resource_projection("agent_task_run", cook_id)?
    else {
        return Ok(None);
    };
    let Some(value) = projection.provenance.get("cook_index") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let index = serde_json::from_value::<AgentTaskCookIndex>(value.clone())
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    // Resolving an attempt id reaches that attempt's own projection, which
    // carries the index it owns for a different alias. That is an ordinary
    // "this id is not a Cook alias" answer, not a corrupt projection.
    if index.cook_id != cook_id {
        return Ok(None);
    }
    if index.latest_run_id != projection.resource_id {
        return Err(Error::internal_unexpected(
            "control-plane Cook-index projection does not match its alias owner",
        ));
    }
    Ok(Some(index))
}

/// Claim within one explicitly rooted store. `cook_dir` as a free
/// function is `default_store()?`, so the store's own method is used here: the
/// claim marker is a bare filesystem create with no record read in front of it,
/// and an ambient reach would land the marker in another home without failing.
fn continuation_failed_notification_event(run_id: &str) -> String {
    format!(
        "notification-continuation-failed-{}",
        sanitize_run_id(run_id)
    )
}

fn claim_cook_notification_event_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    event: &str,
    marker: &Value,
) -> Result<bool> {
    let delivered_path = store
        .cook_dir(&sanitize_run_id(cook_id))
        .join(format!("{event}.json"));
    if delivered_path.exists() {
        return Ok(false);
    }
    let path = delivered_path.with_file_name(format!("{event}-claim.json"));
    if path.exists() {
        let stale = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
            .map(|age| age >= COOK_NOTIFICATION_CLAIM_LEASE)
            // An unreadable claim is not proof of delivery; leave it eligible
            // for the normal create-new race rather than making it permanent.
            .unwrap_or(true);
        if !stale {
            return Ok(false);
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(path.display().to_string()),
                ));
            }
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            let bytes = serde_json::to_vec(marker).map_err(|error| {
                Error::internal_json(error.to_string(), Some(path.display().to_string()))
            })?;
            file.write_all(&bytes).map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}

fn confirm_cook_notification_event_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    event: &str,
    marker: &Value,
) -> Result<()> {
    let delivered_path = store
        .cook_dir(&sanitize_run_id(cook_id))
        .join(format!("{event}.json"));
    write_private_json(&delivered_path, marker)
}

fn release_cook_notification_event_claim_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    event: &str,
) -> Result<()> {
    let path = store
        .cook_dir(&sanitize_run_id(cook_id))
        .join(format!("{event}-claim.json"));
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}

pub(super) fn read_cook_notification_outcome(cook_id: &str) -> Result<Option<Value>> {
    let path = cook_dir(&sanitize_run_id(cook_id))?.join("notification-outcome.json");
    if !path.exists() {
        return Ok(None);
    }
    read_json(&path).map(Some)
}

pub(super) fn record_exists(run_id: &str) -> Result<bool> {
    Ok(ObservationStore::open_initialized_for_lifecycle()?
        .get_run(run_id)?
        .is_some())
}

/// Check an existing observation store without creating its directory, running
/// migrations, or triggering startup repair work.
pub(super) fn record_exists_readonly(run_id: &str) -> Result<bool> {
    default_store()?.record_exists_readonly(run_id)
}

/// Bound on a single source run's retry lineage. Deliberately far above any
/// plausible retry count: an exceeded lineage is reported as an error, and this
/// is the threshold at which "this is a runaway, not a lineage" becomes true.
const RETRY_SUCCESSOR_SCAN_LIMIT: usize = 256;

/// Read every retry successor of `source_run_id`.
///
/// Callers treat an empty or non-matching result as "no reservation exists"
/// and then create one, so a truncated lineage would silently double-book a
/// retry. The page is therefore read with an explicit truncation signal and a
/// truncated lineage fails loudly rather than answering wrongly (#11177).
#[cfg(test)]
pub(super) fn read_retry_successors(source_run_id: &str) -> Result<Vec<AgentTaskRunRecord>> {
    default_store()?.read_retry_successors(source_run_id)
}

fn records_with_health(
    observation_runs: Vec<RunRecord>,
) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
    let mut health = super::AgentTaskRecordHealthSummary::healthy();
    let mut records = Vec::new();
    for run in observation_runs {
        match super::health::diagnose_run(&run) {
            Ok(record) => {
                health.healthy += 1;
                records.push(record);
            }
            Err(item) => super::health::record_health_item(&mut health, item),
        }
    }
    Ok((records, health))
}

/// Preserve readable rows and their exact observation ordering keys.
fn page_records(observation_runs: Vec<RunRecord>) -> Result<Vec<AgentTaskRecordPageRecord>> {
    let mut records = Vec::new();
    for run in observation_runs {
        if let Ok(record) = snapshot_from_run(&run) {
            records.push(AgentTaskRecordPageRecord {
                record,
                cursor: ObservationRunCursor::from_run(&run),
            });
        }
    }
    Ok(records)
}

/// Raw durable rows are read only through a resolved store now. The last
/// ambient caller was record-health reconciliation, whose scan decides which
/// rows to migrate or quarantine and must therefore read the same observation
/// database those writes land in (#7505).
fn observation_runs_bounded_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    limit: usize,
) -> Result<Vec<RunRecord>> {
    lifecycle_store
        .open_observation_readonly()?
        .list_runs(bounded_agent_task_filter(limit))
}

fn bounded_agent_task_filter(limit: usize) -> RunListFilter {
    RunListFilter {
        kind: Some("agent-task".to_string()),
        limit: Some(i64::try_from(limit.clamp(1, 1000)).expect("bounded record limit")),
        ..Default::default()
    }
}

fn observation_metadata(
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
) -> Result<Value> {
    let record_json = serde_json::to_value(record).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("serialize agent-task run {}", record.run_id)),
        )
    })?;
    Ok(json!({
        "schema": "homeboy/agent-task-observation-record/v1",
        "agent_task_run": record_json,
        "agent_task_aggregate": aggregate,
    }))
}

fn merge_observation_metadata(mut existing: Value, typed: Value) -> Value {
    if !existing.is_object() {
        existing = json!({ "homeboy_original_metadata": existing });
    }
    if let (Some(existing), Some(typed)) = (existing.as_object_mut(), typed.as_object()) {
        for (key, value) in typed {
            existing.insert(key.clone(), value.clone());
        }
    }
    existing
}

/// The single lifecycle read: the stored snapshot exactly as persisted.
///
/// There is no read projection. Cancellation is a write invariant enforced by
/// [`write_record_guarded`], and every production write is fenced on the
/// revision it read, so the stored snapshot is the run's state (#15718).
pub(crate) fn snapshot_from_run(run: &RunRecord) -> Result<AgentTaskRunRecord> {
    let record = parse_record_from_run(run)?;
    if record.schema != super::records::schemas::RUN {
        return Err(Error::validation_invalid_argument(
            "agent_task_run.schema",
            format!(
                "unsupported durable agent-task run schema `{}`; this Homeboy build supports `{}`",
                record.schema,
                super::records::schemas::RUN
            ),
            Some(run.id.clone()),
            None,
        ));
    }
    normalize_decoded_record(run, record)
}

fn record_and_aggregate_from_run(
    run: &RunRecord,
) -> Result<(AgentTaskRunRecord, Option<AgentTaskAggregate>)> {
    Ok((snapshot_from_run(run)?, aggregate_from_run(run)?))
}

/// Decode a durable record without requiring the current schema.
///
/// Health reconciliation uses this boundary to classify and migrate legacy
/// schemas. All normal reads go through [`snapshot_from_run`], which accepts only
/// the current schema.
pub(super) fn decode_record_from_run(run: &RunRecord) -> Result<AgentTaskRunRecord> {
    let record = parse_record_from_run(run)?;
    normalize_decoded_record(run, record)
}

fn parse_record_from_run(run: &RunRecord) -> Result<AgentTaskRunRecord> {
    let value = run.metadata_json.get("agent_task_run").ok_or_else(|| {
        Error::new(
            ErrorCode::InternalJsonError,
            format!(
                "observation run {} is missing agent_task_run metadata",
                run.id
            ),
            json!({ "context": run.id }),
        )
    })?;
    serde_json::from_value(value.clone()).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("parse agent-task run {}", run.id)),
        )
    })
}

fn normalize_decoded_record(
    run: &RunRecord,
    record: AgentTaskRunRecord,
) -> Result<AgentTaskRunRecord> {
    if let Some(problem) = record.lab_handoff_validation_error() {
        return Err(Error::internal_json(
            problem,
            Some(format!("validate agent-task Lab handoff {}", run.id)),
        ));
    }
    if let Some(identity) = record.workspace_identity.as_ref() {
        identity.verify()?;
        if record.workspace_owner_lease.as_ref().is_some_and(|lease| {
            lease.workspace != *identity
                || lease.lifecycle_revision != record.workspace_lifecycle_revision
                || lease.owner_id != record.run_id
        }) {
            return Err(Error::validation_invalid_argument(
                "workspace_owner_lease",
                "agent-task workspace owner lease contradicts durable identity or lifecycle revision",
                Some(run.id.clone()),
                None,
            ));
        }
    }
    Ok(record)
}

fn read_mirrored_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<AgentTaskAggregate>> {
    let store = lifecycle_store.open_observation_initialized()?;
    let Some(run) = store.get_run(run_id)? else {
        return Ok(None);
    };
    aggregate_from_run(&run)
}

fn aggregate_from_run(run: &RunRecord) -> Result<Option<AgentTaskAggregate>> {
    let Some(value) = run
        .metadata_json
        .get("agent_task_aggregate")
        .filter(|value| !value.is_null())
    else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .map(Some)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse agent-task aggregate {}", run.id)),
            )
        })
}

pub(crate) fn cook_owns_observation(
    store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
) -> bool {
    let cook_id = record.metadata["cook_id"]
        .as_str()
        .unwrap_or(&record.run_id);
    // A preparation refusal has no Cook driver left to settle the observation.
    // Its durable pre-execution failure owns the terminal result instead.
    store.cook_dir(cook_id).join("recipe.json").exists()
        && !(record.state().is_terminal()
            && record.metadata["pre_execution_failure"]["phase"] == "retry_preparation"
            && !record.has_recorded_provider_progress())
}

fn run_status(state: AgentTaskRunState) -> &'static str {
    match state {
        AgentTaskRunState::Queued | AgentTaskRunState::Running => RunStatus::Running.as_str(),
        AgentTaskRunState::Succeeded => RunStatus::Pass.as_str(),
        AgentTaskRunState::CandidateRecoverable => RunStatus::Fail.as_str(),
        AgentTaskRunState::PartialFailure | AgentTaskRunState::Failed => RunStatus::Fail.as_str(),
        AgentTaskRunState::Cancelled => RunStatus::Skipped.as_str(),
    }
}

fn terminal_finished_at(record: &AgentTaskRunRecord) -> Option<String> {
    if record.state().is_terminal() {
        record
            .updated_at
            .clone()
            .or_else(|| Some(record.submitted_at.clone()))
    } else {
        None
    }
}

fn plan_id_component(record: &AgentTaskRunRecord) -> Option<String> {
    crate::agent_task_service::admitted_component_id(
        record
            .metadata
            .pointer("/unmaterialized_cook_admission/binding/source/repository_identity"),
    )
    .or_else(|| {
        crate::agent_task_service::admitted_component_id(
            record.metadata.get("cook_repository_identity"),
        )
    })
    .or_else(|| {
        record
            .metadata
            .pointer("/unmaterialized_cook_admission/binding/source/repository")
            .and_then(Value::as_str)
            .filter(|component| !component.is_empty())
            .map(ToString::to_string)
    })
    .or_else(|| {
        record
            .metadata
            .get("repo")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
    .or_else(|| {
        record
            .metadata
            .get("kind")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let raw = fs::read_to_string(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))
}

pub(super) fn run_dir(run_id: &str) -> Result<PathBuf> {
    Ok(default_store()?.run_dir(run_id))
}

fn cook_dir(cook_id: &str) -> Result<PathBuf> {
    Ok(default_store()?.cook_dir(cook_id))
}
