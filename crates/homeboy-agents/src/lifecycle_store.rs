use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

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

    pub fn observation_db_path(&self) -> PathBuf {
        self.roots.data().join("homeboy.sqlite")
    }

    pub(crate) fn data_root(&self) -> PathBuf {
        self.roots.data().to_path_buf()
    }

    pub(crate) fn artifact_root(&self) -> PathBuf {
        self.roots.artifacts().to_path_buf()
    }

    pub(crate) fn matches_current_environment(&self) -> Result<bool> {
        Ok(self.roots == paths::PathRoots::from_environment()?)
    }

    pub(crate) fn workspace_claim_store(
        &self,
    ) -> homeboy_core::workspace_claim::WorkspaceClaimStore {
        super::workspace_claims::workspace_claim_store_at(self.data_root())
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
            &|run_id| homeboy_core::controller_runtime::admission_status(run_id).ok(),
            |run_id| {
                homeboy_core::controller_runtime::admit_current_for_with_cancellation_check(
                    run_id,
                    || Ok(self.read_record(run_id)?.state.is_terminal()),
                )
                .map(|admission| admission.runtime)
            },
        )
    }

    pub fn open_observation_initialized(&self) -> Result<ObservationStore> {
        ObservationStore::open_initialized_for_lifecycle_at_roots(
            self.observation_db_path(),
            self.artifact_root(),
        )
    }

    pub fn open_observation_readonly(&self) -> Result<ObservationStore> {
        ObservationStore::open_readonly_at(self.observation_db_path())
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

    pub fn read_record(&self, run_id: &str) -> Result<AgentTaskRunRecord> {
        read_record_in_store(self, run_id)
    }

    pub(crate) fn record_run_aggregate(
        &self,
        run_id: &str,
        plan: &AgentTaskPlan,
        aggregate: &AgentTaskAggregate,
    ) -> Result<AgentTaskRunRecord> {
        super::lifecycle_ops::record_run_aggregate_in_store(self, run_id, plan, aggregate)
    }

    /// Check this store for one exact durable run identity without Cook alias
    /// resolution.
    pub fn record_exists(&self, run_id: &str) -> Result<bool> {
        Ok(self
            .open_observation_initialized()?
            .get_run(run_id)?
            .is_some())
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

    pub fn read_record_bounded(&self, run_id: &str) -> Result<AgentTaskRunRecord> {
        read_record_bounded_in_store(self, run_id)
    }

    pub fn write_record(&self, record: &AgentTaskRunRecord) -> Result<()> {
        self.write_record_with_aggregate(
            record,
            read_mirrored_aggregate_in_store(self, &record.run_id)?,
        )
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
        super::workspace_authority::WorkspaceTerminalAuthorityStore::new(
            self.data_root(),
            self.roots.config().to_path_buf(),
        )
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
        self.write_record_with_aggregate(record, Some(aggregate.clone()))?;
        #[cfg(test)]
        if INTERRUPT_AFTER_TERMINAL_COMMIT.swap(false, Ordering::SeqCst) {
            return Err(Error::internal_io(
                "injected interruption after terminal lifecycle commit",
                Some(record.run_id.clone()),
            ));
        }
        self.write_aggregate(&record.run_id, aggregate)
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

#[cfg(test)]
static FAIL_NEXT_RECORD_WRITE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static INTERRUPT_AFTER_TERMINAL_COMMIT: AtomicBool = AtomicBool::new(false);

/// A crashed notifier cannot release its provisional claim. A bounded lease
/// keeps that crash window from permanently suppressing a detached resume.
const COOK_NOTIFICATION_CLAIM_LEASE: Duration = Duration::from_secs(5 * 60);

pub(super) fn write_plan(run_id: &str, plan: &AgentTaskPlan) -> Result<PathBuf> {
    write_plan_in_store(&default_store()?, run_id, plan)
}

fn write_plan_in_store(
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

pub(super) fn read_controller_plan(run_id: &str) -> Result<AgentTaskPlan> {
    read_controller_plan_in_store(&default_store()?, run_id)
}

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

pub(super) fn controller_plan_path(run_id: &str) -> Result<PathBuf> {
    Ok(default_store()?.controller_plan_path(run_id))
}

/// Controller lifecycle operations resolve the plan from their durable run
/// identity. `AgentTaskRunRecord::plan_path` can be runner-local transport
/// evidence after a Lab projection and is never controller execution authority.
pub(super) fn read_controller_plan_for_execution(run_id: &str) -> Result<AgentTaskPlan> {
    read_controller_plan_for_execution_in_store(&default_store()?, run_id)
}

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

pub(super) fn write_aggregate(run_id: &str, aggregate: &AgentTaskAggregate) -> Result<PathBuf> {
    write_aggregate_in_store(&default_store()?, run_id, aggregate)
}

fn write_aggregate_in_store(
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

fn read_aggregate_bounded_in_store(
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

/// Persist a record while the caller owns the config lock. Terminal workspace
/// authority is deliberately deferred because it acquires its own lock.
pub(super) fn write_record_locked_without_terminal_projection(
    record: &AgentTaskRunRecord,
) -> Result<AgentTaskRunRecord> {
    default_store()?.write_record_locked_without_terminal_projection(record)
}

/// Complete terminal projection from committed lifecycle truth after unlocking.
pub(super) fn project_terminal_record_after_unlock(run_id: &str) -> Result<()> {
    default_store()?.project_terminal_record_after_unlock(run_id)
}

/// Serialize a record read-modify-write so independent lifecycle projections do
/// not replace metadata written by another controller operation.
pub(super) fn mutate_record(
    run_id: &str,
    mutate: impl FnOnce(&mut AgentTaskRunRecord) -> bool,
) -> Result<Option<AgentTaskRunRecord>> {
    default_store()?.mutate_record(run_id, mutate)
}

/// Mutate a record while the caller owns the config lock. Terminal authority is
/// deliberately deferred to the caller's post-lock projection, matching
/// [`write_record_locked_without_terminal_projection`].
pub(super) fn mutate_record_locked_without_terminal_projection(
    run_id: &str,
    mutate: impl FnOnce(&mut AgentTaskRunRecord) -> bool,
) -> Result<Option<AgentTaskRunRecord>> {
    default_store()?.mutate_record_locked_without_terminal_projection(run_id, mutate)
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

#[cfg(test)]
pub(super) fn fail_next_record_write_for_test() {
    FAIL_NEXT_RECORD_WRITE.store(true, Ordering::SeqCst);
}

#[cfg(test)]
pub(super) fn interrupt_after_terminal_commit_for_test() {
    INTERRUPT_AFTER_TERMINAL_COMMIT.store(true, Ordering::SeqCst);
}

fn write_record_with_aggregate_without_workspace_authority(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: Option<AgentTaskAggregate>,
) -> Result<AgentTaskRunRecord> {
    #[cfg(test)]
    if FAIL_NEXT_RECORD_WRITE.swap(false, Ordering::SeqCst) {
        return Err(Error::internal_io(
            "injected lifecycle record persistence failure",
            Some(record.run_id.clone()),
        ));
    }
    let store = lifecycle_store.open_observation_initialized()?;
    let existing_metadata = store
        .get_run(&record.run_id)?
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
    let metadata_json =
        merge_observation_metadata(existing_metadata, observation_metadata(&record, aggregate)?);
    store.upsert_imported_run_preserving_terminal(&RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: plan_id_component(&record),
        started_at: record.submitted_at.clone(),
        finished_at: terminal_finished_at(&record),
        status: run_status(record.state).to_string(),
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version: Some(build_identity::current().version),
        git_sha: None,
        rig_id: None,
        metadata_json,
    })?;
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

fn read_record_in_store(
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

fn read_record_bounded_in_store(
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

pub(super) fn write_cook_index_attempt(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    recorded_at: String,
    candidate: Option<AgentTaskCookLatestSubstantiveCandidate>,
) -> Result<AgentTaskCookIndex> {
    write_cook_index_attempt_in_store(
        &default_store()?,
        cook_id,
        attempt,
        run_id,
        recorded_at,
        candidate,
    )
}

fn write_cook_index_attempt_in_store(
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

/// Write one Cook index attempt while the caller owns the config lock.
pub(super) fn write_cook_index_attempt_locked(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    recorded_at: String,
    candidate: Option<AgentTaskCookLatestSubstantiveCandidate>,
) -> Result<AgentTaskCookIndex> {
    write_cook_index_attempt_locked_in_store(
        &default_store()?,
        cook_id,
        attempt,
        run_id,
        recorded_at,
        candidate,
    )
}

fn write_cook_index_attempt_locked_in_store(
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

fn read_cook_index_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<AgentTaskCookIndex> {
    read_json(&store.cook_index_path(cook_id))
}

pub(super) fn cook_index_exists(cook_id: &str) -> Result<bool> {
    Ok(default_store()?.cook_index_path(cook_id).exists())
}

/// Claim the one terminal notification a Cook is allowed to deliver.
///
/// The claim is the creation of the marker file itself: `create_new` is
/// `O_EXCL`, so exactly one caller — in any process, on any thread — observes
/// `Ok(true)`. This is the cook-scoped counterpart of
/// `ObservationStore::mark_notification_delivered`, which cannot serve here
/// because it is keyed on a `runs` row and a Cook id is an alias with no row
/// of its own.
pub(super) fn claim_cook_notification(cook_id: &str, marker: &Value) -> Result<bool> {
    let delivered_path =
        cook_index_path(&sanitize_run_id(cook_id))?.with_file_name("notification.json");
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

/// Commit a successful notification claim. Only a confirmed transport delivery
/// becomes the durable exactly-once marker.
pub(super) fn confirm_cook_notification(cook_id: &str, marker: &Value) -> Result<()> {
    let delivered_path =
        cook_index_path(&sanitize_run_id(cook_id))?.with_file_name("notification.json");
    write_private_json(&delivered_path, marker)
}

/// Release a provisional claim after a non-delivery so a later terminal
/// observer can retry it.
pub(super) fn release_cook_notification_claim(cook_id: &str) -> Result<()> {
    let path =
        cook_index_path(&sanitize_run_id(cook_id))?.with_file_name("notification-claim.json");
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}

/// Persist the latest bounded, secret-safe terminal notification outcome.
pub(super) fn write_cook_notification_outcome(cook_id: &str, outcome: &Value) -> Result<()> {
    let path =
        cook_index_path(&sanitize_run_id(cook_id))?.with_file_name("notification-outcome.json");
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

pub(super) fn update_cook_index(
    cook_id: &str,
    mutate: impl FnOnce(&mut AgentTaskCookIndex) -> bool,
) -> Result<Option<AgentTaskCookIndex>> {
    default_store()?.update_cook_index(cook_id, mutate)
}

pub(super) fn record_exists(run_id: &str) -> Result<bool> {
    Ok(ObservationStore::open_initialized_for_lifecycle()?
        .get_run(run_id)?
        .is_some())
}

/// Check an existing observation store without creating its directory, running
/// migrations, or triggering startup repair work.
pub(super) fn record_exists_readonly(run_id: &str) -> Result<bool> {
    Ok(ObservationStore::open_readonly()?
        .get_run(run_id)?
        .is_some())
}

pub(super) fn validate_cook_index_attempt(cook_id: &str, attempt: u32, run_id: &str) -> Result<()> {
    validate_cook_index_attempt_in_store(&default_store()?, cook_id, attempt, run_id)
}

fn validate_cook_index_attempt_in_store(
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

pub(super) fn record_lacks_typed_metadata(run_id: &str) -> Result<bool> {
    Ok(ObservationStore::open_initialized_for_lifecycle()?
        .get_run(run_id)?
        .is_some_and(|run| run.metadata_json.get("agent_task_run").is_none()))
}

pub(super) fn read_records() -> Result<Vec<AgentTaskRunRecord>> {
    Ok(read_records_with_health()?.0)
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
pub(super) fn read_retry_successors(source_run_id: &str) -> Result<Vec<AgentTaskRunRecord>> {
    let page = ObservationStore::open_initialized_for_lifecycle()?.list_runs_by_retry_of_page(
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
    page.runs.iter().map(record_from_run).collect()
}

pub(super) fn read_records_with_health(
) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
    read_records_with_health_bounded(1000)
}

pub(super) fn read_records_with_health_bounded(
    limit: usize,
) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
    records_with_health(observation_runs_bounded(limit)?)
}

pub(super) fn read_all_records_with_health(
) -> Result<(Vec<AgentTaskRunRecord>, super::AgentTaskRecordHealthSummary)> {
    let store = ObservationStore::open_readonly()?;
    records_with_health(store.list_runs_all(RunListFilter {
        kind: Some("agent-task".to_string()),
        ..Default::default()
    })?)
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

pub(super) fn observation_runs() -> Result<Vec<RunRecord>> {
    observation_runs_bounded(1000)
}

fn observation_runs_bounded(limit: usize) -> Result<Vec<RunRecord>> {
    let store = ObservationStore::open_readonly()?;
    let filter = RunListFilter {
        kind: Some("agent-task".to_string()),
        limit: Some(i64::try_from(limit.clamp(1, 1000)).expect("bounded record limit")),
        ..Default::default()
    };
    store.list_runs(filter)
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
    record_from_run_with_schema_policy(run, true)
}

/// Read a durable record without enforcing the supported-schema guard.
///
/// Record health reconciliation exists to migrate legacy schemas, so it is the
/// one caller that must be able to read one. #11446 added the guard inside
/// `record_from_run`, which is exactly what the migration branch calls, making
/// legacy migration unreachable: the reconciler reported the unsupported-schema
/// diagnostic instead of migrating.
pub(super) fn record_from_run_allowing_legacy_schema(
    run: &RunRecord,
) -> Result<AgentTaskRunRecord> {
    record_from_run_with_schema_policy(run, false)
}

fn record_from_run_with_schema_policy(
    run: &RunRecord,
    enforce_schema: bool,
) -> Result<AgentTaskRunRecord> {
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
    let mut record: AgentTaskRunRecord =
        serde_json::from_value(value.clone()).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some(format!("parse agent-task run {}", run.id)),
            )
        })?;
    if enforce_schema && record.schema != super::records::schemas::RUN {
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

fn read_mirrored_aggregate(run_id: &str) -> Result<Option<AgentTaskAggregate>> {
    read_mirrored_aggregate_in_store(&default_store()?, run_id)
}

fn read_mirrored_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<AgentTaskAggregate>> {
    let store = lifecycle_store.open_observation_initialized()?;
    let Some(run) = store.get_run(run_id)? else {
        return Ok(None);
    };
    let Some(value) = run.metadata_json.get("agent_task_aggregate") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
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
