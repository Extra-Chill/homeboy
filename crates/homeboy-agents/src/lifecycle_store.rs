use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(any(test, feature = "test-support"))]
use std::cell::Cell;

use serde_json::{json, Value};

use super::{
    sanitize_run_id, AgentTaskCookIndex, AgentTaskCookIndexAttempt,
    AgentTaskCookLatestSubstantiveCandidate, AgentTaskRunRecord, AgentTaskRunState,
};
use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy_core::engine::local_files::{
    write_json_file as write_json, write_json_file_owner_only as write_private_json,
};
use homeboy_core::observation::{ObservationStore, RunListFilter, RunRecord, RunStatus};
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

    pub fn cook_index_path(&self, cook_id: &str) -> PathBuf {
        self.roots
            .data()
            .join("agent-task-cooks")
            .join(sanitize_run_id(cook_id))
            .join("index.json")
    }

    pub(crate) fn unmaterialized_admission_cursor(&self) -> Option<String> {
        let path = self
            .data_root()
            .join("agent-task-cook-admissions")
            .join("reconcile-cursor.json");
        let value = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())?;
        value["last_run_id"].as_str().map(str::to_string)
    }

    pub(crate) fn write_unmaterialized_admission_cursor(&self, run_id: &str) -> Result<()> {
        let path = self
            .data_root()
            .join("agent-task-cook-admissions")
            .join("reconcile-cursor.json");
        write_private_json(
            &path,
            &json!({
                "schema": "homeboy/unmaterialized-cook-reconcile-cursor/v1",
                "last_run_id": run_id,
            }),
        )
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
        self.submit_plan_with_runtime_admission_status(
            plan,
            run_id,
            super::lifecycle_ops::execution_runner_id(),
            &crate::agent_task_service::cook_pre_execution::store_admission_status(self),
            |run_id| {
                let runtime_root =
                    homeboy_core::controller_runtime::runtime_root_in(self.roots().data())?;
                homeboy_core::controller_runtime::admit_current_for_with_cancellation_check_in_root(
                    &runtime_root,
                    run_id,
                    || {
                        Ok(crate::agent_task_service::cook_pre_execution::runtime_admission_cancellation_requested(
                            &self.read_record(run_id)?,
                        ))
                    },
                )
                .map(|admission| admission.runtime)
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
    /// artifact publications and backfills artifact handles. Rooting a caller
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
        let aggregate = self.read_aggregate(&record.run_id)?;
        let raw = serde_json::to_string_pretty(&aggregate).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("serialize agent-task aggregate {}", record.run_id)),
            )
        })?;
        Ok((raw, self.aggregate_path(&record.run_id)))
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

    pub(crate) fn record_cook_moving_base_recovery(
        &self,
        run_id: &str,
        recovery: Value,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_cook_moving_base_recovery_in_store(self, run_id, recovery)
    }

    pub(crate) fn clear_cook_moving_base_recovery(
        &self,
        run_id: &str,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::clear_cook_moving_base_recovery_in_store(self, run_id)
    }

    pub fn read_controller_plan(&self, run_id: &str) -> Result<AgentTaskPlan> {
        read_controller_plan_in_store(self, run_id)
    }

    pub fn read_controller_plan_for_execution(&self, run_id: &str) -> Result<AgentTaskPlan> {
        read_controller_plan_for_execution_in_store(self, run_id)
    }

    pub fn write_aggregate(&self, run_id: &str, aggregate: &AgentTaskAggregate) -> Result<PathBuf> {
        write_aggregate_in_store(self, run_id, aggregate)
    }

    pub fn read_aggregate(&self, run_id: &str) -> Result<AgentTaskAggregate> {
        read_aggregate_in_store(self, run_id)
    }

    pub fn read_aggregate_bounded(&self, run_id: &str) -> Result<AgentTaskAggregate> {
        read_aggregate_bounded_in_store(self, run_id)
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
        write_cook_index_attempt_in_store(self, cook_id, attempt, run_id, recorded_at, candidate)
    }

    pub(crate) fn write_cook_index_attempt_locked(
        &self,
        cook_id: &str,
        attempt: u32,
        run_id: &str,
        recorded_at: String,
        candidate: Option<AgentTaskCookLatestSubstantiveCandidate>,
    ) -> Result<AgentTaskCookIndex> {
        write_cook_index_attempt_locked_in_store(
            self,
            cook_id,
            attempt,
            run_id,
            recorded_at,
            candidate,
        )
    }

    pub(crate) fn validate_cook_index_attempt(
        &self,
        cook_id: &str,
        attempt: u32,
        run_id: &str,
    ) -> Result<()> {
        validate_cook_index_attempt_in_store(self, cook_id, attempt, run_id)
    }

    pub fn read_cook_index(&self, cook_id: &str) -> Result<AgentTaskCookIndex> {
        read_cook_index_in_store(self, cook_id)
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
        let path = self.cook_index_path(cook_id);
        if !path.exists() {
            return Ok(None);
        }
        let mut index = read_json(&path)?;
        if mutate(&mut index) {
            write_json(&path, &index)?;
        }
        Ok(Some(index))
    }

    pub fn cook_index_exists(&self, cook_id: &str) -> bool {
        self.cook_index_path(cook_id).exists()
    }

    /// Persist the latest bounded, secret-safe terminal notification outcome
    /// beside this store's own Cook index rather than the ambient one.
    pub fn write_cook_notification_outcome(&self, cook_id: &str, outcome: &Value) -> Result<()> {
        write_cook_notification_outcome_in_store(self, cook_id, outcome)
    }

    /// Claim the one terminal notification this store's Cook may deliver.
    ///
    /// The `O_EXCL` marker is created beside this store's own Cook index, so
    /// two stores hold two independent claims rather than contending for the
    /// ambient one.
    pub fn claim_cook_notification(&self, cook_id: &str, marker: &Value) -> Result<bool> {
        claim_cook_notification_in_store(self, cook_id, marker)
    }

    /// Commit a confirmed terminal delivery beside this store's own Cook index.
    pub fn confirm_cook_notification(&self, cook_id: &str, marker: &Value) -> Result<()> {
        confirm_cook_notification_in_store(self, cook_id, marker)
    }

    /// Release this store's own provisional terminal-notification claim.
    ///
    /// The claim, the confirmation, and this release are one exactly-once
    /// protocol, so all three have to name the same installation.
    pub fn release_cook_notification_claim(&self, cook_id: &str) -> Result<()> {
        release_cook_notification_claim_in_store(self, cook_id)
    }

    pub fn read_record(&self, run_id: &str) -> Result<AgentTaskRunRecord> {
        read_record_in_store(self, run_id)
    }

    /// List every durable agent-task record from this store's own observation
    /// database.
    ///
    /// Queue scanning is a read of the observation DB, not of the record files,
    /// so a scan that resolved it ambiently would inspect one queue while the
    /// claim it produced mutated another (#7505).
    pub fn read_records(&self) -> Result<Vec<AgentTaskRunRecord>> {
        read_records_in_store(self)
    }

    /// Read every retry successor of `source_run_id` from this store's own
    /// observation database.
    pub(crate) fn read_retry_successors(
        &self,
        source_run_id: &str,
    ) -> Result<Vec<AgentTaskRunRecord>> {
        read_retry_successors_in_store(self, source_run_id)
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

    /// Read this store's bounded durable registry snapshot with the health
    /// summary of the records that could not be parsed.
    pub fn read_records_with_health(
        &self,
    ) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
        self.read_records_with_health_bounded(1000)
    }

    pub fn read_records_with_health_bounded(
        &self,
        limit: usize,
    ) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
        records_with_health(observation_runs_bounded_in_store(self, limit)?)
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

    pub fn read_record_bounded(&self, run_id: &str) -> Result<AgentTaskRunRecord> {
        read_record_bounded_in_store(self, run_id)
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

    pub fn write_record(&self, record: &AgentTaskRunRecord) -> Result<()> {
        self.write_record_with_aggregate(
            record,
            read_mirrored_aggregate_in_store(self, &record.run_id)?,
        )
    }

    pub(crate) fn rearm_pre_execution_record_with_runtime(
        &self,
        record: &AgentTaskRunRecord,
        runtime: Value,
    ) -> Result<AgentTaskRunRecord> {
        homeboy_core::controller_runtime::validate(&runtime)?;
        self.rearm_pre_execution_record_inner(record, Some(runtime))
    }

    /// Rearm a retryable zero-provider record without changing controller
    /// runtime ownership. Runner-bound retries execute under a separate runner
    /// runtime while the original controller pin remains authoritative.
    pub(crate) fn rearm_pre_execution_record(
        &self,
        record: &AgentTaskRunRecord,
    ) -> Result<AgentTaskRunRecord> {
        self.rearm_pre_execution_record_inner(record, None)
    }

    fn rearm_pre_execution_record_inner(
        &self,
        record: &AgentTaskRunRecord,
        runtime: Option<Value>,
    ) -> Result<AgentTaskRunRecord> {
        self.with_config_lock(|| {
            let existing = self.read_record(&record.run_id)?;
            if !existing.state.is_terminal()
                || !crate::agent_task_service::cook_pre_execution::retryable_pre_execution_failure(
                    &existing,
                )
                || existing.metadata["provider_executions_consumed"]
                    .as_u64()
                    .unwrap_or(0)
                    != 0
            {
                return Err(Error::validation_invalid_argument(
                    "controller_runtime_recovery",
                    "controller runtime rebinding requires a retryable terminal pre-execution record with zero provider executions",
                    Some(record.run_id.clone()),
                    None,
                ));
            }
            if record.state != AgentTaskRunState::Queued
                || record.run_id != existing.run_id
                || record.plan_id != existing.plan_id
            {
                return Err(Error::validation_invalid_argument(
                    "controller_runtime_recovery",
                    "controller runtime rebinding must preserve the durable run and plan identity while rearming it as queued",
                    Some(record.run_id.clone()),
                    None,
                ));
            }

            let mut rebound = record.clone();
            if let Some(runtime) = runtime {
                let previous = existing
                    .metadata
                    .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
                    .cloned()
                    .unwrap_or(Value::Null);
                rebound.metadata["controller_runtime_recovery"] = serde_json::json!({
                    "schema": "homeboy/controller-runtime-pre-execution-recovery/v1",
                    "reason": "retryable_pre_execution_failure",
                    "previous": previous,
                    "current": runtime.clone(),
                    "provider_executions_consumed": 0,
                    "recovered_at": super::now_timestamp(),
                });
                rebound.metadata
                    [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = runtime;
            } else {
                rebound.metadata["pre_execution_recovery"] = serde_json::json!({
                    "schema": "homeboy/pre-execution-recovery/v1",
                    "reason": "retryable_pre_execution_failure",
                    "provider_executions_consumed": 0,
                    "recovered_at": super::now_timestamp(),
                });
            }
            rebound.metadata["controller_identity"] =
                serde_json::json!(homeboy_core::build_identity::current().display);
            let committed = write_record_with_aggregate_without_workspace_authority_mode(
                self,
                &rebound,
                read_mirrored_aggregate_in_store(self, &rebound.run_id)?,
                false,
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

    /// Persist a record while the caller owns this store's config lock. Terminal
    /// authority is intentionally projected only after that lock is released.
    pub(crate) fn write_record_locked_without_terminal_projection(
        &self,
        record: &AgentTaskRunRecord,
    ) -> Result<AgentTaskRunRecord> {
        write_record_with_aggregate_without_workspace_authority(
            self,
            record,
            read_mirrored_aggregate_in_store(self, &record.run_id)?,
        )
    }

    /// Commit the authoritative record/aggregate pair while the caller owns
    /// this store's config lock. Artifact and workspace authority projection
    /// must happen only after releasing that lock.
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
        let mut record = self.read_record(run_id)?;
        if !mutate(&mut record) {
            return Ok(None);
        }
        self.write_record_locked_without_terminal_projection(&record)
            .map(Some)
    }

    /// Complete terminal projection after the record lock has been released:
    /// receipt first, then the workspace owner lease.
    pub(crate) fn project_terminal_record_after_unlock(&self, run_id: &str) -> Result<()> {
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

    fn write_record_with_aggregate(
        &self,
        record: &AgentTaskRunRecord,
        aggregate: Option<AgentTaskAggregate>,
    ) -> Result<()> {
        let mut record = record.clone();
        super::workspace_claims::renew_record_workspace_owner_in_store(
            &super::workspace_claims::workspace_claim_store_at(self.data_root()),
            &mut record,
        )?;
        let committed = self.with_config_lock(|| {
            write_record_with_aggregate_without_workspace_authority(self, &record, aggregate)
        })?;
        self.project_terminal_record_after_unlock(&committed.run_id)
    }
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
        let mut plan = plan.clone();
        migrate_execution_budget(&mut plan)?;
        validate_managed_services(&plan)?;
        let path = store.controller_plan_path(run_id);
        write_private_json(&path, &plan)?;
        Ok(path)
    })
}

pub(super) fn read_plan_path(path: &str) -> Result<AgentTaskPlan> {
    let plan = read_json(&PathBuf::from(path))?;
    validate_execution_budget(&plan)?;
    validate_managed_services(&plan)?;
    Ok(plan)
}

/// The controller-owned plan is read only through a resolved store now: the
/// last ambient caller was record-health reconciliation, which had to read the
/// plan and commit the record it reconstructs from that plan into the same home
/// (#7505). There is deliberately no `store::read_controller_plan` shim left to
/// reach for.
fn read_controller_plan_in_store(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskPlan> {
    let path = store.controller_plan_path(run_id);
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

/// Controller lifecycle operations resolve the plan from their durable run
/// identity. `AgentTaskRunRecord::plan_path` can be runner-local transport
/// evidence after a Lab projection and is never controller execution authority.
///
/// There is no ambient `store::` shim for this any more: the migration branch
/// below rewrites `plan.json`, so the last caller —
/// `load_plan_for_execution` — now resolves one store and hands it here rather
/// than letting the Cook-alias resolution and the plan rewrite land in
/// separately resolved homes (#7505).
fn read_controller_plan_for_execution_in_store(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskPlan> {
    store.with_config_lock(|| {
        let path = store.controller_plan_path(run_id);
        let mut plan = read_controller_plan_in_store(store, run_id)?;
        if migrate_execution_budget(&mut plan)? {
            write_private_json(&path, &plan)?;
        }
        Ok(plan)
    })
}

fn validate_execution_budget(plan: &AgentTaskPlan) -> Result<()> {
    match plan.options.execution_budget.version {
        0 | crate::agent_task_scheduler::AgentTaskExecutionBudget::VERSION => Ok(()),
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

fn migrate_execution_budget(plan: &mut AgentTaskPlan) -> Result<bool> {
    plan.options
        .execution_budget
        .migrate_legacy()
        .map_err(|message| {
            Error::validation_invalid_argument(
                "execution_budget.version",
                message,
                Some(plan.options.execution_budget.version.to_string()),
                None,
            )
        })
}

#[cfg(test)]
pub(super) fn write_aggregate(run_id: &str, aggregate: &AgentTaskAggregate) -> Result<PathBuf> {
    write_aggregate_in_store(&default_store()?, run_id, aggregate)
}

pub(super) fn write_aggregate_in_store(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
    aggregate: &AgentTaskAggregate,
) -> Result<PathBuf> {
    let path = store.aggregate_path(run_id);
    write_json(&path, aggregate)?;
    Ok(path)
}

pub(super) fn read_aggregate(run_id: &str) -> Result<AgentTaskAggregate> {
    read_aggregate_in_store(&default_store()?, run_id)
}

fn read_aggregate_in_store(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskAggregate> {
    match read_mirrored_aggregate_in_store(store, run_id)? {
        Some(aggregate) => Ok(aggregate),
        None => read_json(&store.aggregate_path(run_id)),
    }
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
    read_aggregate_bounded_in_store(&default_store()?, run_id)
}

pub(super) fn read_aggregate_bounded_in_store(
    store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskAggregate> {
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
    serde_json::from_slice(&raw)
        .map_err(|error| Error::internal_json(error.to_string(), Some(path.display().to_string())))
}

pub(super) fn aggregate_path(run_id: &str) -> Result<PathBuf> {
    Ok(default_store()?.aggregate_path(run_id))
}

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

/// Commit the controller projection and child aggregate in one observation row.
/// The JSON aggregate is a post-commit cache: readers use the committed row, so
/// interruption before or after cache persistence exposes a complete state.
pub(super) fn write_aggregate_and_record(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<PathBuf> {
    default_store()?.write_aggregate_and_record(record, aggregate)
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn fail_next_record_write_for_test() {
    FAIL_NEXT_RECORD_WRITE.set(true);
}

#[cfg(test)]
pub(super) fn interrupt_after_terminal_commit_for_test() {
    INTERRUPT_AFTER_TERMINAL_COMMIT.set(true);
}

fn write_record_with_aggregate_without_workspace_authority(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
) -> Result<AgentTaskRunRecord> {
    write_record_with_aggregate_without_workspace_authority_mode(
        lifecycle_store,
        record,
        aggregate,
        true,
    )
}

fn write_record_with_aggregate_without_workspace_authority_mode(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
    preserve_terminal: bool,
) -> Result<AgentTaskRunRecord> {
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
    let mut record = record.clone();
    if record.metadata.get("cook_operation_claims").is_none() {
        if let Some(claims) = existing_metadata
            .pointer("/agent_task_run/metadata/cook_operation_claims")
            .cloned()
        {
            record.metadata["cook_operation_claims"] = claims;
        }
    }
    let mut metadata_json =
        merge_observation_metadata(existing_metadata, observation_metadata(&record, aggregate)?);
    if !preserve_terminal {
        metadata_json["agent_task_aggregate"] = Value::Null;
    }
    let projected = RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: plan_id_component(&record),
        started_at: record.submitted_at.clone(),
        finished_at: terminal_finished_at(&record),
        status: run_status(record.state).to_string(),
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version,
        git_sha: None,
        rig_id: None,
        metadata_json,
    };
    if preserve_terminal {
        store.upsert_imported_run_preserving_terminal(&projected)?;
    } else {
        store.upsert_imported_run(&projected)?;
    }
    let committed = store.get_run(&record.run_id)?.ok_or_else(|| {
        Error::internal_unexpected(format!(
            "committed agent-task run record is unavailable: {}",
            record.run_id
        ))
    })?;
    record_from_run(&committed)
}

pub(super) fn read_record(run_id: &str) -> Result<AgentTaskRunRecord> {
    default_store()?.read_record(run_id)
}

pub(super) fn read_record_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let store = lifecycle_store.open_observation_initialized()?;
    let run = store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("agent-task run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })?;
    record_from_run(&run)
}

/// Read one durable record under the observation store's read-only busy budget.
/// Inspection must not initialize, migrate, or contend like a writer.
pub(super) fn read_record_bounded(run_id: &str) -> Result<AgentTaskRunRecord> {
    default_store()?.read_record_bounded(run_id)
}

pub(super) fn read_record_bounded_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let store = lifecycle_store.open_observation_readonly()?;
    let run = store.get_run(run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            format!("agent-task run record not found: {run_id}"),
            Some(run_id.to_string()),
            None,
        )
    })?;
    record_from_run(&run)
}

/// Bypass typed record validation solely to seed corruption-recovery fixtures.
/// Production writes and normal test rewrites must use `write_record`.
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

pub(super) fn write_cook_index_attempt_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    recorded_at: String,
    candidate: Option<AgentTaskCookLatestSubstantiveCandidate>,
) -> Result<AgentTaskCookIndex> {
    store.with_config_lock(|| {
        write_cook_index_attempt_locked_in_store(
            store,
            cook_id,
            attempt,
            run_id,
            recorded_at,
            candidate,
        )
    })
}

pub(super) fn write_cook_index_attempt_locked_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    recorded_at: String,
    candidate: Option<AgentTaskCookLatestSubstantiveCandidate>,
) -> Result<AgentTaskCookIndex> {
    let cook_id = sanitize_run_id(cook_id);
    let run_id = sanitize_run_id(run_id);
    validate_cook_index_attempt_in_store(store, &cook_id, attempt, &run_id)?;
    let path = store.cook_index_path(&cook_id);
    let mut index = if path.exists() {
        read_json(&path)?
    } else {
        AgentTaskCookIndex {
            schema: super::records::schemas::COOK_INDEX.to_string(),
            cook_id: cook_id.clone(),
            latest_run_id: run_id.clone(),
            latest_substantive_candidate: None,
            attempts: Vec::new(),
        }
    };
    index.cook_id = cook_id;
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
                    || (candidate.attempt == current.attempt && candidate.run_id >= current.run_id)
            });
        if replace {
            index.latest_substantive_candidate = Some(candidate);
        }
    }
    write_json(&path, &index)?;
    Ok(index)
}

#[cfg(test)]
pub(super) fn write_cook_index_for_test(index: &AgentTaskCookIndex) -> Result<()> {
    write_json(&cook_index_path(&sanitize_run_id(&index.cook_id))?, index)
}

pub(super) fn read_cook_index(cook_id: &str) -> Result<AgentTaskCookIndex> {
    read_cook_index_in_store(&default_store()?, cook_id)
}

pub(super) fn read_cook_index_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<AgentTaskCookIndex> {
    read_json(&store.cook_index_path(cook_id))
}

pub(super) fn cook_index_exists(cook_id: &str) -> Result<bool> {
    Ok(default_store()?.cook_index_path(cook_id).exists())
}

/// Claim within one explicitly rooted store. `cook_index_path` as a free
/// function is `default_store()?`, so the store's own method is used here: the
/// claim marker is a bare filesystem create with no record read in front of it,
/// and an ambient reach would land the marker in another home without failing.
pub(super) fn claim_cook_notification_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    marker: &Value,
) -> Result<bool> {
    let delivered_path = store
        .cook_index_path(&sanitize_run_id(cook_id))
        .with_file_name("notification.json");
    if delivered_path.exists() {
        return Ok(false);
    }
    let path = delivered_path.with_file_name("notification-claim.json");
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

pub(super) fn confirm_cook_notification_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    marker: &Value,
) -> Result<()> {
    let delivered_path = store
        .cook_index_path(&sanitize_run_id(cook_id))
        .with_file_name("notification.json");
    write_private_json(&delivered_path, marker)
}

/// Release a provisional claim beside the injected store's own Cook index.
///
/// This removes the marker `claim_cook_notification_in_store` created, so it
/// has to follow the same root. Releasing the ambient home's marker instead
/// would leave the injected store's claim standing — permanently blocking the
/// later observer this release exists to unblock — while deleting a claim
/// nobody took. Neither half fails: `remove_file` treats `NotFound` as success,
/// so the wrong-root release returns `Ok(())` having done nothing (#7505).
pub(super) fn release_cook_notification_claim_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<()> {
    let path = store
        .cook_index_path(&sanitize_run_id(cook_id))
        .with_file_name("notification-claim.json");
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}

pub(super) fn write_cook_notification_outcome_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    outcome: &Value,
) -> Result<()> {
    let path = store
        .cook_index_path(&sanitize_run_id(cook_id))
        .with_file_name("notification-outcome.json");
    write_private_json(&path, outcome)
}

pub(super) fn read_cook_notification_outcome(cook_id: &str) -> Result<Option<Value>> {
    let path =
        cook_index_path(&sanitize_run_id(cook_id))?.with_file_name("notification-outcome.json");
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

pub(super) fn validate_cook_index_attempt_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
    attempt: u32,
    run_id: &str,
) -> Result<()> {
    let cook_id = sanitize_run_id(cook_id);
    let run_id = sanitize_run_id(run_id);
    let path = store.cook_index_path(&cook_id);
    if !path.exists() {
        return Ok(());
    }
    let index: AgentTaskCookIndex = read_json(&path)?;
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

/// The store-rooted body of [`AgentTaskLifecycleStore::read_records`], following
/// the same bound and the same health projection but reading this store's own
/// observation database instead of `paths::observation_db()`.
///
/// The ambient `read_records()` free shim that used to sit above this is gone.
/// Its last caller was `reconcile_active_lab_runner_handoffs_in_store`, a queue scan that
/// mutates every row it selects — expiring, terminalizing, and reconciling them
/// — so it now scans the store it was handed rather than deciding from one
/// installation's queue and committing into another (#7505).
fn read_records_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<Vec<AgentTaskRunRecord>> {
    Ok(records_with_health(observation_runs_bounded_in_store(lifecycle_store, 1000)?)?.0)
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
    read_retry_successors_in_store(&default_store()?, source_run_id)
}

/// The store-rooted counterpart of [`read_retry_successors`], reading this
/// store's own observation database instead of the ambient one.
///
/// The truncation contract is the reason this matters more than a plain read:
/// callers treat "no successor" as authority to create one, so a lineage scanned
/// in the wrong home answers "none" for a source whose successor is durable here
/// and double-books the reservation.
pub(super) fn read_retry_successors_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    source_run_id: &str,
) -> Result<Vec<AgentTaskRunRecord>> {
    let page = lifecycle_store
        .open_observation_initialized()?
        .list_runs_by_retry_of_page("agent-task", source_run_id, RETRY_SUCCESSOR_SCAN_LIMIT)?;
    if page.truncated {
        return Err(Error::internal_unexpected(format!(
            "retry lineage for {source_run_id} exceeded {RETRY_SUCCESSOR_SCAN_LIMIT} successors; \
             refusing to answer from a truncated lineage"
        )));
    }
    page.runs.iter().map(record_from_run).collect()
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

pub(super) fn record_from_run(run: &RunRecord) -> Result<AgentTaskRunRecord> {
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
    Ok((record_from_run(run)?, aggregate_from_run(run)?))
}

/// Decode a durable record without requiring the current schema.
///
/// Health reconciliation uses this boundary to classify and migrate legacy
/// schemas. All normal reads go through [`record_from_run`], which accepts only
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
    mut record: AgentTaskRunRecord,
) -> Result<AgentTaskRunRecord> {
    record.hydrate_legacy_lab_handoff();
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

fn run_status(state: AgentTaskRunState) -> &'static str {
    match state {
        AgentTaskRunState::Queued | AgentTaskRunState::Running => RunStatus::Running.as_str(),
        AgentTaskRunState::Succeeded => RunStatus::Pass.as_str(),
        AgentTaskRunState::CandidateRecoverable => RunStatus::Fail.as_str(),
        AgentTaskRunState::PartialRecoverable => RunStatus::Fail.as_str(),
        AgentTaskRunState::PartialFailure | AgentTaskRunState::Failed => RunStatus::Fail.as_str(),
        AgentTaskRunState::Cancelled => RunStatus::Skipped.as_str(),
    }
}

fn terminal_finished_at(record: &AgentTaskRunRecord) -> Option<String> {
    if record.state.is_terminal() {
        record
            .updated_at
            .clone()
            .or_else(|| Some(record.submitted_at.clone()))
    } else {
        None
    }
}

fn plan_id_component(record: &AgentTaskRunRecord) -> Option<String> {
    record
        .metadata
        .get("repo")
        .and_then(Value::as_str)
        .map(ToString::to_string)
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

fn cook_index_path(cook_id: &str) -> Result<PathBuf> {
    Ok(default_store()?.cook_index_path(cook_id))
}
