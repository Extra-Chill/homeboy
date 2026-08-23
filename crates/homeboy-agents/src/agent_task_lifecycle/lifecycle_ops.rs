use super::acceptance_verifier::{
    validate_acceptance_requirement, validate_attestation, with_acceptance_verifier,
};
use super::*;

const DETACHED_COOK_ADMISSION_LEASE_SECONDS: i64 = 30;
use chrono::DateTime;
use homeboy_engine_primitives::content_hash;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};

const LAB_HANDOFF_ACCEPTANCE_TIMEOUT_SECONDS: i64 = 120;
pub(crate) const EXPIRED_LAB_HANDOFF_REASON: &str =
    "runner handoff acceptance deadline expired before a runner job was recorded";

pub(super) fn lab_handoff_acceptance_timeout_seconds() -> i64 {
    std::env::var("HOMEBOY_TEST_LAB_HANDOFF_ACCEPTANCE_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|seconds| *seconds >= 0)
        .unwrap_or(LAB_HANDOFF_ACCEPTANCE_TIMEOUT_SECONDS)
}

// The ambient `reconcile_deferred_candidate()` shim that used to sit above this
// resolved a root and delegated straight here. It had no callers, so it was a
// resolution point that existed for nobody (#7505).

/// Merge a completed deferred-cleanup candidate into its timeout outcome,
/// inside an explicitly rooted store.
///
/// The worker owns the mutable workspace until it exits; this lifecycle-side
/// operation is the only place where its immutable recovery result is adopted.
/// A per-run advisory lock makes concurrent status/artifact/Cook readers
/// reread and persist one coherent aggregate and terminal projection.
///
/// The advisory lock is the whole point of this operation, so it must be taken
/// in the same installation the record, aggregate, plan, and terminal
/// projection are read and written in. The ambient form resolved the data root
/// for the lock separately from the root every `store::` shim below it resolved
/// for itself, which is a lock that excludes nobody as soon as the two differ
/// (#7505). Deriving it from `run_dir` also makes the lock path agree with the
/// aggregate path, which already sanitized the resolved run id when the lock
/// did not.
pub(crate) fn reconcile_deferred_candidate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<bool> {
    let run_id = resolve_run_id_in_store(lifecycle_store, run_id)?;
    let lock_path = lifecycle_store
        .run_dir(&run_id)
        .join("deferred-candidate.lock");
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|error| Error::internal_io(error.to_string(), None))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open deferred candidate lock".to_string()),
            )
        })?;
    let _lock = DeferredCandidateLock::lock(file)?;

    let mut record = lifecycle_store.read_record(&run_id)?;
    let mut aggregate = match lifecycle_store.read_aggregate(&run_id) {
        Ok(aggregate) => aggregate,
        // The worker may finish before the aggregate is committed. A later
        // read retries from durable state rather than inventing a projection.
        Err(_) => return Ok(false),
    };
    let mut changed = false;

    for outcome in &mut aggregate.outcomes {
        if outcome.status != AgentTaskOutcomeStatus::Timeout {
            continue;
        }
        let Some(action) = outcome.artifacts.iter().find(|artifact| {
            artifact.schema == crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA
                && artifact.kind == "cleanup_action"
                && artifact.role.as_deref() == Some("cleanup_action")
                && artifact.metadata.get("run_id").and_then(Value::as_str) == Some(run_id.as_str())
                && artifact.metadata.get("task_id").and_then(Value::as_str)
                    == Some(outcome.task_id.as_str())
        }) else {
            continue;
        };
        let Some(path) = action.path.as_deref() else {
            continue;
        };
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !outcome
                    .diagnostics
                    .iter()
                    .any(|entry| entry.class == "agent_task.deferred_cleanup_descriptor_missing")
                {
                    outcome.diagnostics.push(AgentTaskDiagnostic {
                        class: "agent_task.deferred_cleanup_descriptor_missing".to_string(),
                        message: "deferred cleanup descriptor is missing; its workspace receipt cannot be reconciled".to_string(),
                        data: json!({
                            "path": path,
                            "safe_next_action": format!(
                                "homeboy agent-task diagnose {run_id} --full"
                            ),
                        }),
                    });
                    changed = true;
                }
                continue;
            }
            Err(_) => continue,
        };
        let action_value: Value = match serde_json::from_str(&raw) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if action_value.get("schema").and_then(Value::as_str)
            != Some("homeboy/agent-task-deferred-cleanup/v1")
            || action_value.get("run_id").and_then(Value::as_str) != Some(run_id.as_str())
            || action_value.get("task_id").and_then(Value::as_str) != Some(outcome.task_id.as_str())
            || action_value.get("attempt") != action.metadata.get("attempt")
        {
            continue;
        }
        match action_value.get("status").and_then(Value::as_str) {
            Some("pending") | Some("completed") | Some("completed_no_candidate") | None => continue,
            Some("failed") => {
                let diagnostic = action_value
                    .get("diagnostic")
                    .and_then(Value::as_str)
                    .unwrap_or("deferred cleanup failed");
                if !outcome
                    .diagnostics
                    .iter()
                    .any(|entry| entry.class == "agent_task.deferred_cleanup_failed")
                {
                    outcome.diagnostics.push(AgentTaskDiagnostic {
                        class: "agent_task.deferred_cleanup_failed".to_string(),
                        message: diagnostic.chars().take(512).collect(),
                        data: json!({ "safe_next_action": "Inspect the deferred cleanup diagnostic before retrying the provider." }),
                    });
                    changed = true;
                }
            }
            Some("candidate_recovered") => {
                let Some(candidates) = action_value
                    .get("candidate_artifacts")
                    .and_then(Value::as_array)
                else {
                    continue;
                };
                let mut recovered = Vec::new();
                for value in candidates {
                    let Ok(artifact) = serde_json::from_value::<AgentTaskArtifact>(value.clone())
                    else {
                        continue;
                    };
                    let portable = artifact.url.as_deref()
                        == Some(&candidate_artifact_url(
                            &run_id,
                            &outcome.task_id,
                            &artifact.id,
                        ));
                    let valid_sha = artifact
                        .sha256
                        .as_deref()
                        .is_some_and(content_hash::is_sha256_hex);
                    let content_matches = artifact
                        .path
                        .as_deref()
                        .and_then(|candidate_path| fs::read(candidate_path).ok())
                        .is_some_and(|bytes| {
                            let actual = content_hash::sha256_hex(&bytes);
                            artifact.sha256.as_deref() == Some(actual.as_str())
                        });
                    if artifact.schema == crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA
                        && portable
                        && valid_sha
                        && content_matches
                        && crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(
                            &artifact,
                        )
                    {
                        recovered.push(artifact);
                    }
                }
                if recovered.is_empty() {
                    continue;
                }
                crate::agent_task_timeout_artifacts::append_unique_artifacts(
                    &mut outcome.artifacts,
                    recovered,
                );
                outcome.status = AgentTaskOutcomeStatus::CandidateRecoverable;
                outcome.failure_classification = None;
                outcome.summary =
                    Some("deferred cleanup recovered a canonical patch candidate".to_string());
                outcome.metadata["deferred_candidate_reconciled_at"] = json!(now_timestamp());
                outcome.metadata["safe_next_action"] =
                    json!("Promote the recovered candidate through controller-owned gates.");
                changed = true;
            }
            Some(_) => continue,
        }
    }
    if !changed {
        return Ok(false);
    }

    let plan = lifecycle_store.read_controller_plan(&run_id)?;
    aggregate.status = aggregate_status(&aggregate.outcomes);
    aggregate.totals = aggregate_totals(plan.tasks.len(), &aggregate.outcomes);
    let aggregate_path = lifecycle_store
        .aggregate_path(&run_id)
        .display()
        .to_string();
    apply_aggregate_to_record(&mut record, &plan, &aggregate, aggregate_path);
    lifecycle_store.write_aggregate_and_record(&record, &aggregate)?;
    record_terminal_artifact_projection_in_store(lifecycle_store, &mut record, &aggregate)?;
    Ok(true)
}

fn candidate_artifact_url(run_id: &str, task_id: &str, artifact_id: &str) -> String {
    use homeboy_core::execution_contract::encode_uri_component;

    format!(
        "homeboy://agent-task/run/{}/artifacts#task={}&artifact={}",
        encode_uri_component(run_id),
        encode_uri_component(task_id),
        encode_uri_component(artifact_id),
    )
}

struct DeferredCandidateLock {
    #[allow(dead_code)] // Retains the advisory lock until this guard drops.
    file: File,
}

pub(crate) struct LabHandoffLock {
    // Retains the advisory lock until acceptance or expiry has been persisted.
    #[allow(dead_code)]
    file: File,
}

impl LabHandoffLock {
    /// Take the Lab handoff acceptance lock inside an explicitly rooted store.
    ///
    /// There is deliberately no ambient constructor. This lock serializes
    /// acceptance against expiry for one run, and every state it guards — the
    /// record, its submission intent, its aggregate — is read and written
    /// through a lifecycle store. A lock file resolved from
    /// `paths::homeboy_data()` while those reads and writes followed an
    /// injected store would sit in a different installation than the state it
    /// protects, and a lock taken in the wrong home excludes nobody: expiry and
    /// acceptance would both believe they held it (#7505). Requiring the store
    /// in the signature is what makes that divergence unrepresentable.
    ///
    /// Deriving the path from `run_dir` also sanitizes the run id, which the
    /// ambient form did not, so the lock now names a path that agrees with the
    /// run's own record and aggregate — the same correction
    /// `reconcile_deferred_candidate_in_store` made for its per-run lock.
    pub(crate) fn lock_in_store(
        lifecycle_store: &AgentTaskLifecycleStore,
        run_id: &str,
    ) -> Result<Self> {
        let lock_path = lifecycle_store.run_dir(run_id).join("lab-handoff.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| Error::internal_io(error.to_string(), None))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some("open Lab handoff lock".to_string()))
            })?;
        #[cfg(unix)]
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some("lock Lab handoff".to_string()),
            ));
        }
        Ok(Self { file })
    }
}
impl DeferredCandidateLock {
    #[cfg(unix)]
    fn lock(file: File) -> Result<Self> {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some("lock deferred candidate".to_string()),
            ));
        }
        Ok(Self { file })
    }
    #[cfg(not(unix))]
    fn lock(file: File) -> Result<Self> {
        Ok(Self { file })
    }
}

fn aggregate_status(outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateStatus {
    if outcomes
        .iter()
        .any(|outcome| outcome.status == AgentTaskOutcomeStatus::Cancelled)
    {
        return AgentTaskAggregateStatus::Cancelled;
    }
    if outcomes
        .iter()
        .any(|outcome| outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable)
    {
        return AgentTaskAggregateStatus::PartialRecoverable;
    }
    let succeeded = outcomes.iter().any(|outcome| {
        matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
        )
    });
    let failed = outcomes.iter().any(|outcome| {
        !matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
        )
    });
    match (succeeded, failed) {
        (true, false) => AgentTaskAggregateStatus::Succeeded,
        (true, true) => AgentTaskAggregateStatus::PartialFailure,
        _ => AgentTaskAggregateStatus::Failed,
    }
}

fn aggregate_totals(total_tasks: usize, outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateTotals {
    let mut totals = AgentTaskAggregateTotals {
        queued: total_tasks.saturating_sub(outcomes.len()),
        ..Default::default()
    };
    for outcome in outcomes {
        match outcome.status {
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
                totals.succeeded += 1
            }
            AgentTaskOutcomeStatus::Timeout => totals.timed_out += 1,
            AgentTaskOutcomeStatus::Cancelled => totals.cancelled += 1,
            AgentTaskOutcomeStatus::CandidateRecoverable => totals.recoverable_candidates += 1,
            _ => totals.failed += 1,
        }
    }
    totals
}

pub fn submit_plan(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    submit_plan_in_store(&lifecycle_store, plan, requested_run_id)
}

/// Submit a plan into an explicitly rooted store.
///
/// The admission cancellation check is the reach that has to move with the
/// submission: it decides whether to abandon a queued admission by reading the
/// run's own terminal state. Reading that ambiently would let another home's
/// record of the same identity abandon this store's admission, or leave a
/// cancellation recorded here unseen while the controller waits on the
/// admission lock. The controller-runtime store follows the same root now
/// (#12859), so the admission queue and its lock belong to this installation
/// rather than the machine.
pub fn submit_plan_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let runtime_root =
        homeboy_core::controller_runtime::runtime_root_in(lifecycle_store.roots().data())?;
    submit_plan_with_runtime_admission_in_store(
        lifecycle_store,
        plan,
        requested_run_id,
        execution_runner_id(),
        None,
        Some(&|run_id| {
            homeboy_core::controller_runtime::admission_status_at(&runtime_root, run_id).ok()
        }),
        |run_id| {
            homeboy_core::controller_runtime::admit_current_for_with_cancellation_check_in_root(
                &runtime_root,
                run_id,
                || Ok(lifecycle_store.read_record(run_id)?.state.is_terminal()),
            )
        },
    )
}

/// Persist the parent for a locally detached Cook before the detached process
/// has prepared its first attempt. The parent uses the Cook ID itself, so the
/// normal Cook-index alias takes over automatically once that attempt exists.
///
/// An empty plan is intentional: this record owns only the handoff lifecycle,
/// while the detached Cook persists the immutable execution plan and attempt.
pub fn record_detached_cook_handoff_parent(cook_id: &str) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_detached_cook_handoff_parent_in_store(&lifecycle_store, cook_id)
}

/// Persist a detached Cook's handoff parent inside an explicitly rooted store.
///
/// Both admission guards read the store the parent is written into. The alias
/// guard consults this store's own Cook index, so an attempt published in
/// another home can neither reject a fresh parent here nor let one be minted
/// over an attempt that already exists here. The collision guard is the same
/// decision for the record: read ambiently, an unrelated run in another home
/// could veto this parent, or the idempotent re-record of a live handoff could
/// be misread as a collision.
pub fn record_detached_cook_handoff_parent_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<AgentTaskRunRecord> {
    let cook_id = sanitize_run_id(cook_id);
    let resolved_run_id = resolve_run_id_in_store(lifecycle_store, &cook_id)?;
    if resolved_run_id != cook_id {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "detached Cook id already resolves to an existing Cook attempt",
            Some(cook_id),
            None,
        ));
    }
    if let Ok(record) = lifecycle_store.read_record(&cook_id) {
        if record.metadata["detached_cook_handoff"]["cook_id"] == cook_id {
            return Ok(record);
        }
        return Err(Error::validation_invalid_argument(
            "run_id",
            "detached Cook id collides with an existing non-handoff run",
            Some(cook_id),
            None,
        ));
    }

    let plan = AgentTaskPlan::new(format!("detached-cook-handoff-{cook_id}"), Vec::new());
    let mut record = submit_plan_in_store(lifecycle_store, &plan, Some(&cook_id))?;
    record.metadata["detached_cook_handoff"] = json!({
        "state": "pending",
        "admission_state": "pre_supervisor",
        "admission_deadline_at": (chrono::Utc::now()
            + chrono::Duration::seconds(DETACHED_COOK_ADMISSION_LEASE_SECONDS))
            .to_rfc3339(),
        "cook_id": cook_id,
        "cancellation_fence": { "state": "open" },
    });
    lifecycle_store.write_record(&record)?;
    record_cook_progress_in_store(
        lifecycle_store,
        &record.run_id,
        "detached_handoff_pending",
        0,
        Some("waiting for the detached Cook to materialize its first attempt"),
    )
}

/// Persist a detached Cook while its Lab destination is not yet admissible.
///
/// The binding is deliberately assembled by the CLI from identities, immutable
/// input references, and secret-free replay arguments. It never contains secret
/// values or a redacted receipt that cannot replay. Reusing the Cook id for a
/// different request is rejected before any destination can be provisioned.
pub fn record_unmaterialized_cook_admission(
    cook_id: &str,
    binding: Value,
    state: &str,
    reason: &str,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_unmaterialized_cook_admission_in_store(&lifecycle_store, cook_id, binding, state, reason)
}

/// Create a complete detached parent and immutable admission binding in the
/// first durable run-record write. Input bytes remain staged and this state is
/// deliberately invisible to runner selection until publication is recovered.
pub fn prepare_unmaterialized_cook_admission(
    cook_id: &str,
    binding: Value,
    requested_state: &str,
    reason: &str,
) -> Result<AgentTaskRunRecord> {
    validate_unmaterialized_admission_input(&binding, requested_state)?;
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    let cook_id = sanitize_run_id(cook_id);
    store.with_config_lock(|| {
        let resolved = resolve_run_id_in_store(&store, &cook_id)?;
        if resolved != cook_id {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "detached Cook id already resolves to an existing Cook attempt",
                Some(cook_id.clone()),
                None,
            ));
        }
        if let Ok(existing) = store.read_record(&cook_id) {
            if existing.metadata["detached_cook_handoff"]["cook_id"] != cook_id {
                return Err(Error::validation_invalid_argument(
                    "run_id",
                    "detached Cook id collides with an existing non-handoff run",
                    Some(cook_id.clone()),
                    None,
                ));
            }
            if existing.metadata["unmaterialized_cook_admission"]["binding"] != binding {
                return Err(Error::validation_invalid_argument(
                    "run_id",
                    "Cook id is already bound to a different unmaterialized admission request",
                    Some(cook_id.clone()),
                    None,
                ));
            }
            return Ok(existing);
        }

        let plan = AgentTaskPlan::new(format!("detached-cook-handoff-{cook_id}"), Vec::new());
        let mut submission_metadata = serde_json::Map::new();
        submission_metadata.insert(
            "detached_cook_handoff".to_string(),
            json!({
                "state": "pending",
                "admission_state": "unmaterialized",
                "cook_id": cook_id,
                "cancellation_fence": { "state": "open" },
            }),
        );
        submission_metadata.insert(
            "unmaterialized_cook_admission".to_string(),
            unmaterialized_admission_value(
                &cook_id,
                binding.clone(),
                "preparing_inputs",
                requested_state,
                reason,
            ),
        );
        let runtime_root = homeboy_core::controller_runtime::runtime_root_in(store.roots().data())?;
        submit_plan_with_runtime_admission_in_store(
            &store,
            &plan,
            Some(&cook_id),
            execution_runner_id(),
            Some(submission_metadata),
            Some(&|run_id| {
                homeboy_core::controller_runtime::admission_status_at(&runtime_root, run_id).ok()
            }),
            |run_id| {
                homeboy_core::controller_runtime::admit_current_for_with_cancellation_check_in_root(
                    &runtime_root,
                    run_id,
                    || Ok(store.read_record(run_id)?.state.is_terminal()),
                )
            },
        )
    })
}

fn validate_unmaterialized_admission_input(binding: &Value, state: &str) -> Result<()> {
    if !matches!(
        state,
        "queued" | "blocked_runner_unavailable" | "blocked_runner_stale"
    ) {
        return Err(Error::validation_invalid_argument(
            "cook_admission.state",
            "unmaterialized Cook admission requires a typed queued or runner-blocked state",
            Some(state.to_string()),
            None,
        ));
    }
    if !binding.is_object() {
        return Err(Error::validation_invalid_argument(
            "cook_admission.binding",
            "unmaterialized Cook admission requires an immutable object binding",
            None,
            None,
        ));
    }
    Ok(())
}

fn unmaterialized_admission_value(
    cook_id: &str,
    binding: Value,
    state: &str,
    requested_state: &str,
    reason: &str,
) -> Value {
    json!({
        "schema": "homeboy/unmaterialized-cook-admission/v1",
        "state": state,
        "requested_state": requested_state,
        "reason": homeboy_core::redaction::redact_string(reason),
        "binding": binding,
        "admission_attempts": 0,
        "fence": 0,
        "retry": {
            "policy": "bounded_exponential",
            "next_attempt_at": (chrono::Utc::now() + chrono::Duration::seconds(15)).to_rfc3339(),
            "max_attempts": 20,
        },
        "commands": {
            "status": format!("homeboy agent-task status {cook_id}"),
            "watch": format!("homeboy agent-task status {cook_id} --watch"),
            "cancel": format!("homeboy agent-task cancel {cook_id}"),
            "resume": format!("homeboy agent-task resume {cook_id}"),
            "resume_trigger": "daemon-tick-or-runner-admission-event",
        },
    })
}

pub fn record_unmaterialized_cook_admission_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    binding: Value,
    state: &str,
    reason: &str,
) -> Result<AgentTaskRunRecord> {
    lifecycle_store.with_config_lock(|| {
        record_unmaterialized_cook_admission_locked(
            lifecycle_store,
            cook_id,
            binding,
            state,
            reason,
        )
    })
}

fn record_unmaterialized_cook_admission_locked(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    binding: Value,
    state: &str,
    reason: &str,
) -> Result<AgentTaskRunRecord> {
    const SCHEMA: &str = "homeboy/unmaterialized-cook-admission/v1";
    if !matches!(
        state,
        "queued" | "blocked_runner_unavailable" | "blocked_runner_stale"
    ) {
        return Err(Error::validation_invalid_argument(
            "cook_admission.state",
            "unmaterialized Cook admission requires a typed queued or runner-blocked state",
            Some(state.to_string()),
            None,
        ));
    }
    if !binding.is_object() {
        return Err(Error::validation_invalid_argument(
            "cook_admission.binding",
            "unmaterialized Cook admission requires an immutable object binding",
            None,
            None,
        ));
    }

    let cook_id = sanitize_run_id(cook_id);
    if lifecycle_store.read_record(&cook_id).is_err() {
        let plan = AgentTaskPlan::new(format!("detached-cook-handoff-{cook_id}"), Vec::new());
        let mut submission_metadata = serde_json::Map::new();
        submission_metadata.insert(
            "detached_cook_handoff".to_string(),
            json!({
                "state": "pending",
                "admission_state": "unmaterialized",
                "cook_id": cook_id,
                "cancellation_fence": { "state": "open" },
            }),
        );
        submission_metadata.insert(
            "unmaterialized_cook_admission".to_string(),
            unmaterialized_admission_value(&cook_id, binding, state, state, reason),
        );
        let runtime_root =
            homeboy_core::controller_runtime::runtime_root_in(lifecycle_store.roots().data())?;
        let record = submit_plan_with_runtime_admission_in_store(
            lifecycle_store,
            &plan,
            Some(&cook_id),
            execution_runner_id(),
            Some(submission_metadata),
            Some(&|run_id| {
                homeboy_core::controller_runtime::admission_status_at(&runtime_root, run_id).ok()
            }),
            |run_id| {
                homeboy_core::controller_runtime::admit_current_for_with_cancellation_check_in_root(
                    &runtime_root,
                    run_id,
                    || Ok(lifecycle_store.read_record(run_id)?.state.is_terminal()),
                )
            },
        )?;
        return record_cook_progress_in_store(
            lifecycle_store,
            &record.run_id,
            state,
            0,
            Some(reason),
        );
    }
    let _ = record_detached_cook_handoff_parent_in_store(lifecycle_store, &cook_id)?;
    let state = state.to_string();
    let reason = homeboy_core::redaction::redact_string(reason);
    let binding_for_write = binding.clone();
    let record = lifecycle_store.mutate_record(&cook_id, |record| {
        if record.state.is_terminal()
            || record.metadata["detached_cook_handoff"]["cancellation_fence"]["state"]
                == "cancelled"
        {
            return false;
        }
        let existing = &record.metadata["unmaterialized_cook_admission"];
        if existing.is_object() {
            // The first immutable admission owns retry/lease state. An
            // idempotent duplicate must not erase an active replay claim.
            return false;
        }
        record.metadata["unmaterialized_cook_admission"] = json!({
            "schema": SCHEMA,
            "state": state,
            "reason": reason,
            "binding": binding_for_write,
            "admission_attempts": 0,
            "fence": 0,
            "retry": {
                "policy": "bounded_exponential",
                "next_attempt_at": (chrono::Utc::now() + chrono::Duration::seconds(15)).to_rfc3339(),
                "max_attempts": 20,
            },
            "commands": {
                "status": format!("homeboy agent-task status {cook_id}"),
                "watch": format!("homeboy agent-task status {cook_id} --watch"),
                "cancel": format!("homeboy agent-task cancel {cook_id}"),
                "resume": format!("homeboy agent-task resume {cook_id}"),
                "resume_trigger": "daemon-tick-or-runner-admission-event",
            },
        });
        // Unlike the short pre-spawn lease, this admission is daemon-owned and
        // remains live until cancellation, exhaustion, or materialization.
        record.metadata["detached_cook_handoff"]["admission_state"] =
            json!("unmaterialized");
        record.metadata["detached_cook_handoff"]
            .as_object_mut()
            .expect("handoff metadata object")
            .remove("admission_deadline_at");
        record.updated_at = Some(now_timestamp());
        true
    })?;
    let Some(record) = record else {
        let existing = lifecycle_store.read_record(&cook_id)?;
        if existing.metadata["unmaterialized_cook_admission"]["binding"] != binding {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "Cook id is already bound to a different unmaterialized admission request",
                Some(cook_id),
                None,
            ));
        }
        return Ok(existing);
    };
    record_cook_progress_in_store(lifecycle_store, &record.run_id, &state, 0, Some(&reason))
}

/// Validate immutable Cook admission ownership before the CLI snapshots any
/// replay bytes. The final parent+binding write repeats this decision under the
/// same config lock, so this is an early rejection optimization, not authority.
pub fn precheck_unmaterialized_cook_admission(
    cook_id: &str,
    request_ref: &str,
) -> Result<Option<AgentTaskRunRecord>> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    let cook_id = sanitize_run_id(cook_id);
    store.with_config_lock(|| {
        let resolved = resolve_run_id_in_store(&store, &cook_id)?;
        if resolved != cook_id {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "Cook id already resolves to an existing Cook attempt",
                Some(cook_id.clone()),
                None,
            ));
        }
        let Ok(record) = store.read_record(&cook_id) else {
            return Ok(None);
        };
        if record.metadata["detached_cook_handoff"]["cook_id"] != cook_id {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "Cook id collides with an existing non-handoff run",
                Some(cook_id.clone()),
                None,
            ));
        }
        let admission = &record.metadata["unmaterialized_cook_admission"];
        if !admission.is_object() {
            return Ok(None);
        }
        if admission["binding"]["request_ref"].as_str() != Some(request_ref) {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "Cook id is already bound to a different unmaterialized admission request",
                Some(cook_id.clone()),
                None,
            ));
        }
        Ok(Some(record))
    })
}

/// Recover or complete the atomic input-directory publication for one prepared
/// admission, then expose its requested queue state in the same locked update.
pub fn recover_unmaterialized_cook_input_publication(cook_id: &str) -> Result<AgentTaskRunRecord> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    recover_unmaterialized_cook_input_publication_in_store(&store, cook_id)
}

pub fn recover_unmaterialized_cook_input_publication_in_store(
    store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<AgentTaskRunRecord> {
    let cook_id = sanitize_run_id(cook_id);
    store.with_config_lock(|| {
        let current = store.read_record(&cook_id)?;
        let admission = &current.metadata["unmaterialized_cook_admission"];
        if admission["state"] != "preparing_inputs" {
            return Ok(current);
        }
        let publication = &admission["binding"]["input_publication"];
        let staging = publication["staging_root"].as_str().ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_admission.input_publication",
                "prepared admission has no durable staging root",
                Some(cook_id.clone()),
                None,
            )
        })?;
        let published = publication["published_root"].as_str().ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_admission.input_publication",
                "prepared admission has no durable publication root",
                Some(cook_id.clone()),
                None,
            )
        })?;
        let staging = std::path::PathBuf::from(staging);
        let published = std::path::PathBuf::from(published);
        let admission_root = store.data_root().join("agent-task-cook-admissions");
        if !staging.starts_with(&admission_root) || !published.starts_with(&admission_root) {
            return Err(Error::validation_invalid_argument(
                "cook_admission.input_publication",
                "prepared admission input roots escape durable admission storage",
                Some(cook_id.clone()),
                None,
            ));
        }
        if !published.exists() {
            if !staging.is_dir() {
                return Err(Error::internal_io(
                    "prepared Cook input staging directory is unavailable".to_string(),
                    Some(staging.display().to_string()),
                ));
            }
            if let Some(parent) = published.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(parent.display().to_string()))
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(
                        |error| {
                            Error::internal_io(
                                error.to_string(),
                                Some(parent.display().to_string()),
                            )
                        },
                    )?;
                }
            }
            fs::rename(&staging, &published).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "publish {} -> {}",
                        staging.display(),
                        published.display()
                    )),
                )
            })?;
        } else if staging.exists() {
            fs::remove_dir_all(&staging).map_err(|error| {
                Error::internal_io(error.to_string(), Some(staging.display().to_string()))
            })?;
        }
        let updated =
            store.mutate_record_locked_without_terminal_projection(&cook_id, |record| {
                let admission = &mut record.metadata["unmaterialized_cook_admission"];
                if admission["state"] != "preparing_inputs" {
                    return false;
                }
                let requested = admission["requested_state"]
                    .as_str()
                    .filter(|state| {
                        matches!(
                            *state,
                            "queued" | "blocked_runner_unavailable" | "blocked_runner_stale"
                        )
                    })
                    .unwrap_or("blocked_runner_unavailable")
                    .to_string();
                admission["state"] = json!(requested);
                admission["binding"]["input_publication"]["state"] = json!("published");
                admission["binding"]["input_publication"]["published_at"] =
                    json!(chrono::Utc::now().to_rfc3339());
                // The first bounded selection belongs to this admission call.
                // Backoff applies only after an observed blocked attempt.
                admission["retry"]["next_attempt_at"] = json!(chrono::Utc::now().to_rfc3339());
                record.updated_at = Some(now_timestamp());
                true
            })?;
        Ok(updated.unwrap_or(current))
    })
}

/// Consume the exact replay generation selected by the daemon. This is the
/// worker-side mutation fence and must run before routing can provision a
/// destination or dispatch a provider.
pub fn consume_unmaterialized_cook_replay_claim(
    cook_id: &str,
    fence: u64,
    token: &str,
) -> Result<bool> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    let cook_id = sanitize_run_id(cook_id);
    let token = token.to_string();
    let owner = current_replay_worker_owner()?;
    let consumed = store.mutate_record(&cook_id, |record| {
        if record.state.is_terminal() {
            return false;
        }
        let admission = &mut record.metadata["unmaterialized_cook_admission"];
        if admission["lease"]["state"] != "claimed"
            || admission["lease"]["fence"].as_u64() != Some(fence)
            || admission["lease"]["token"].as_str() != Some(token.as_str())
        {
            return false;
        }
        admission["state"] = json!("replaying");
        admission["lease"]["state"] = json!("consumed");
        admission["lease"]["consumed_at"] = json!(chrono::Utc::now().to_rfc3339());
        admission["lease"]["owner"] = owner.clone();
        admission["lease"]["expires_at"] =
            json!((chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339());
        record.updated_at = Some(now_timestamp());
        true
    })?;
    Ok(consumed.is_some())
}

/// Revalidate and renew a consumed replay lease immediately before the existing
/// Cook route crosses into destination materialization. A superseded worker can
/// never pass this boundary, even if it wakes after its original lease expired.
pub fn renew_unmaterialized_cook_replay_claim(
    cook_id: &str,
    fence: u64,
    token: &str,
) -> Result<bool> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    let cook_id = sanitize_run_id(cook_id);
    let token = token.to_string();
    let owner = current_replay_worker_owner()?;
    let renewed = store.mutate_record(&cook_id, |record| {
        if record.state.is_terminal() {
            return false;
        }
        let admission = &mut record.metadata["unmaterialized_cook_admission"];
        if admission["state"] != "replaying"
            || admission["lease"]["state"] != "consumed"
            || admission["lease"]["fence"].as_u64() != Some(fence)
            || admission["lease"]["token"].as_str() != Some(token.as_str())
            || admission["lease"]["owner"] != owner
        {
            return false;
        }
        admission["state"] = json!("materializing");
        admission["lease"]["state"] = json!("materializing");
        admission["lease"]
            .as_object_mut()
            .expect("replay lease object")
            .remove("expires_at");
        admission["lease"]["materialization_fence_at"] = json!(chrono::Utc::now().to_rfc3339());
        record.updated_at = Some(now_timestamp());
        true
    })?;
    Ok(renewed.is_some())
}

fn current_replay_worker_owner() -> Result<serde_json::Value> {
    let pid = std::process::id();
    let process_start_identity = homeboy_core::process::process_start_identity(pid)
        .map_err(|error| Error::internal_io(error, Some(format!("inspect replay worker {pid}"))))?
        .ok_or_else(|| {
            Error::internal_io(
                "replay worker process start identity is unavailable".to_string(),
                Some(format!("pid:{pid}")),
            )
        })?;
    Ok(json!({
        "pid": pid,
        "process_start_identity": process_start_identity,
    }))
}

/// Release an exact replay claim after its supervised worker exits before an
/// attempt lifecycle record is published. A published index or reserved child
/// record is the normal handoff and remains authoritative.
pub fn release_unmaterialized_cook_replay_claim_after_worker_exit(
    cook_id: &str,
    fence: u64,
    token: &str,
) -> Result<bool> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    let cook_id = sanitize_run_id(cook_id);
    let token = token.to_string();
    let released = store.mutate_record(&cook_id, |record| {
        if record.state.is_terminal() || store.read_cook_index(&cook_id).is_ok() {
            return false;
        }
        let reserved_child_published = record.metadata["detached_cook_handoff"]
            ["materializing_attempt_run_id"]
            .as_str()
            .is_some_and(|run_id| store.read_record(run_id).is_ok());
        let admission = &mut record.metadata["unmaterialized_cook_admission"];
        if reserved_child_published
            || !matches!(
                admission["lease"]["state"].as_str(),
                Some("claimed" | "consumed" | "materializing")
            )
            || admission["lease"]["fence"].as_u64() != Some(fence)
            || admission["lease"]["token"].as_str() != Some(token.as_str())
        {
            return false;
        }
        admission["state"] = json!("queued");
        admission["reason"] = json!("replay worker exited before attempt publication");
        admission["lease"]["state"] = json!("released");
        admission["retry"]["next_attempt_at"] = json!(chrono::Utc::now().to_rfc3339());
        record.updated_at = Some(now_timestamp());
        true
    })?;
    Ok(released.is_some())
}

/// Rearm one blocked admission for an explicit scoped resume. Active replay or
/// materialization ownership is preserved; terminal records are never reopened.
pub fn rearm_unmaterialized_cook_admission(cook_id: &str) -> Result<AgentTaskRunRecord> {
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    let cook_id = sanitize_run_id(cook_id);
    let current = store.read_record(&cook_id)?;
    if current.state.is_terminal() {
        return Ok(current);
    }
    let updated = store.mutate_record(&cook_id, |record| {
        let admission = &mut record.metadata["unmaterialized_cook_admission"];
        if !admission.is_object()
            || matches!(
                admission["lease"]["state"].as_str(),
                Some("claimed" | "consumed" | "materializing")
            )
        {
            return false;
        }
        admission["retry"]["next_attempt_at"] = json!(chrono::Utc::now().to_rfc3339());
        admission["reason"] = json!("explicit scoped resume requested");
        record.updated_at = Some(now_timestamp());
        true
    })?;
    Ok(updated.unwrap_or(current))
}

/// Reject detached Cook work after the pre-spawn parent has been cancelled.
/// The child reads this durable fence independently of launcher liveness.
pub fn require_detached_cook_handoff_fence_open(cook_id: &str) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    require_detached_cook_handoff_fence_open_in_store(&lifecycle_store, cook_id)
}

/// Read the durable cancellation fence from an explicitly rooted store.
///
/// The fence is a field on the handoff parent record, not a separate marker
/// file, so rooting the record read roots the whole decision. It has to be the
/// store the child will materialize its attempt into: a fence read from another
/// home would let a cancelled handoff proceed here, or let an open one be
/// refused because an unrelated parent elsewhere was cancelled. An absent or
/// unreadable record is still an open fence — a parent that was never persisted
/// cannot have been cancelled.
pub fn require_detached_cook_handoff_fence_open_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<()> {
    let cook_id = sanitize_run_id(cook_id);
    let Ok(record) = lifecycle_store.read_record(&cook_id) else {
        return Ok(());
    };
    if record.metadata["detached_cook_handoff"]["cook_id"] != cook_id {
        return Ok(());
    }
    if record.state == AgentTaskRunState::Cancelled
        || record.metadata["detached_cook_handoff"]["cancellation_fence"]["state"] == "cancelled"
    {
        return Err(Error::validation_invalid_argument(
            "cook_id",
            "detached Cook handoff was cancelled before its attempt could materialize",
            Some(cook_id),
            None,
        ));
    }
    Ok(())
}

/// Attach the child identity only after it has been spawned. Cancellation uses
/// this durable identity to stop preparation before an attempt exists.
pub fn record_detached_cook_handoff_child(
    cook_id: &str,
    pid: u32,
    start_identity: homeboy_core::process::ProcessStartIdentity,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_detached_cook_handoff_child_in_store(&lifecycle_store, cook_id, pid, start_identity)
}

/// Attach the spawned child's identity inside an explicitly rooted store.
///
/// The cancellation state this write reads — the record's own run state and the
/// fence it carries forward — is read inside the mutation, from the same store
/// the mutation lands in. Ambient, another home's parent could decide whether
/// this child is recorded as pending or already cancelled, and the durable
/// identity cancellation later signals on could be attached to a record no
/// cancellation in this store would ever reach.
pub fn record_detached_cook_handoff_child_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    pid: u32,
    start_identity: homeboy_core::process::ProcessStartIdentity,
) -> Result<AgentTaskRunRecord> {
    let cook_id = sanitize_run_id(cook_id);
    let record = lifecycle_store.mutate_record(&cook_id, |record| {
        // A concurrent observer may have already terminalized the handoff
        // before this attachment write acquired the record lock. Keep that
        // classification intact for the launcher to report.
        if record.state.is_terminal() {
            return false;
        }
        let cancellation_fence =
            record.metadata["detached_cook_handoff"]["cancellation_fence"].clone();
        let metadata = record.ensure_metadata_object();
        metadata["detached_cook_handoff"] = json!({
            "state": "pending",
        "admission_state": "child_attached",
        "child_supervisor_deadline_at": (chrono::Utc::now()
            + chrono::Duration::seconds(DETACHED_COOK_ADMISSION_LEASE_SECONDS))
            .to_rfc3339(),
            "cook_id": cook_id,
            "child_pid": pid,
            "child_start_identity": start_identity,
            "cancellation_fence": cancellation_fence,
        });
        record.updated_at = Some(now_timestamp());
        true
    })?;
    Ok(record.unwrap_or(lifecycle_store.read_record(&cook_id)?))
}

/// Persist the daemon job that supervises this locally launched Cook.
pub fn record_detached_cook_supervisor(cook_id: &str, job_id: &str) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_detached_cook_supervisor_in_store(&lifecycle_store, cook_id, job_id)
}

/// Persist the supervising daemon job inside an explicitly rooted store.
///
/// The ownership guard — that this record really is the named Cook's handoff
/// parent — sits inside the mutation closure, so it is read from the store the
/// write lands in. This is also the transition to `supervising`, the admission
/// state that makes a lease live indefinitely, so a supervisor recorded against
/// another home's parent would leave this store's admission to expire while a
/// daemon was in fact supervising it.
pub fn record_detached_cook_supervisor_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    job_id: &str,
) -> Result<()> {
    let cook_id = sanitize_run_id(cook_id);
    let job_id = job_id.to_string();
    let _ = lifecycle_store.mutate_record(&cook_id, |record| {
        if record.metadata["detached_cook_handoff"]["cook_id"] != cook_id {
            return false;
        }
        record.metadata["detached_cook_handoff"]["supervisor_job_id"] = json!(job_id);
        record.metadata["detached_cook_handoff"]["admission_state"] = json!("supervising");
        record.metadata["detached_cook_handoff"]["reattach_command"] =
            json!(format!("homeboy agent-task status {cook_id} --full"));
        true
    })?;
    Ok(())
}

/// Reserve the first attempt identity before its lifecycle record is submitted.
///
/// The record and Cook index are separate durable writes. This reservation keeps
/// a supervisor that observes the child exit after the record write from
/// terminalizing the handoff between those writes.
pub fn reserve_detached_cook_handoff_materialization(
    cook_id: &str,
    attempt_run_id: &str,
) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    reserve_detached_cook_handoff_materialization_in_store(
        &lifecycle_store,
        cook_id,
        attempt_run_id,
    )
}

/// Reserve a detached Cook's first attempt within its explicitly selected
/// lifecycle roots.
pub fn reserve_detached_cook_handoff_materialization_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    attempt_run_id: &str,
) -> Result<()> {
    let cook_id = sanitize_run_id(cook_id);
    let attempt_run_id = sanitize_run_id(attempt_run_id);
    if lifecycle_store.read_record(&cook_id).is_err() {
        return Ok(());
    }
    let record = lifecycle_store.mutate_record(&cook_id, |record| {
        if record.metadata["detached_cook_handoff"]["cook_id"] != cook_id {
            return false;
        }
        if let Some(existing) =
            record.metadata["detached_cook_handoff"]["materializing_attempt_run_id"].as_str()
        {
            return existing == attempt_run_id;
        }
        // A redirected parent has completed its one-time handoff. Later Cook
        // retries are ordinary attempt materializations and need no reservation.
        if record.metadata["detached_cook_handoff"]["state"] == "redirected" {
            return true;
        }
        if record.state.is_terminal()
            || record.metadata["detached_cook_handoff"]["cancellation_fence"]["state"]
                == "cancelled"
            || record.metadata["detached_cook_handoff"]["state"] != "pending"
        {
            return false;
        }
        let metadata = record.ensure_metadata_object();
        metadata["detached_cook_handoff"]["materializing_attempt_run_id"] = json!(attempt_run_id);
        record.updated_at = Some(now_timestamp());
        true
    })?;
    let record = record.ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_id",
            "detached Cook handoff was cancelled or terminal before its attempt could materialize",
            Some(cook_id.clone()),
            None,
        )
    })?;
    if record.metadata["detached_cook_handoff"]["cook_id"] != cook_id {
        return Err(Error::validation_invalid_argument(
            "cook_id",
            "detached Cook handoff was cancelled or terminal before its attempt could materialize",
            Some(cook_id),
            None,
        ));
    }
    Ok(())
}

/// Cancel a submitted first child when its reserved handoff parent was
/// cancelled before the Cook index could be published. The reservation is the
/// durable link that keeps this otherwise-unindexed record reachable.
pub fn cancel_reserved_detached_cook_handoff_attempt_if_cancelled(cook_id: &str) -> Result<bool> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    cancel_reserved_detached_cook_handoff_attempt_if_cancelled_in_store(&lifecycle_store, cook_id)
}

pub fn cancel_reserved_detached_cook_handoff_attempt_if_cancelled_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<bool> {
    let cook_id = sanitize_run_id(cook_id);
    let Ok(parent) = lifecycle_store.read_record(&cook_id) else {
        return Ok(false);
    };
    let handoff = &parent.metadata["detached_cook_handoff"];
    let Some(attempt_run_id) = handoff["materializing_attempt_run_id"].as_str() else {
        return Ok(false);
    };
    if handoff["cook_id"] != cook_id || parent.state != AgentTaskRunState::Cancelled {
        return Ok(false);
    }
    let Ok(child) = lifecycle_store.read_record(attempt_run_id) else {
        return Ok(false);
    };
    // The reservation makes an unindexed child reachable, but it does not give
    // parent cancellation authority to overwrite a child that already finished.
    if child.state.is_terminal() {
        return Ok(false);
    }
    super::cancellation::cancel_exact_run_in_store(
        lifecycle_store,
        attempt_run_id,
        Some("detached Cook handoff cancelled"),
    )?;
    Ok(true)
}

/// Terminalize a still-pending handoff parent once no child can materialize its
/// first attempt. A redirected or terminal parent is authoritative evidence
/// from a concurrent materialization or cancellation and is never rewritten.
pub fn fail_detached_cook_handoff_parent(
    cook_id: &str,
    reason: &str,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    fail_detached_cook_handoff_parent_in_store(&lifecycle_store, cook_id, reason)
}

/// Terminalize a still-pending handoff parent inside an explicitly rooted
/// store.
///
/// The two protections that make a parent authoritative — a published Cook
/// index, and a materializing attempt whose record already exists — are read
/// from the same store the mutation lands in. Reading them ambiently would let
/// another home's index or attempt record veto, or fail to veto, a terminal
/// transition in this one.
pub fn fail_detached_cook_handoff_parent_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    reason: &str,
) -> Result<AgentTaskRunRecord> {
    let cook_id = sanitize_run_id(cook_id);
    let record = lifecycle_store.mutate_record(&cook_id, |record| {
        if record.metadata["detached_cook_handoff"]["cook_id"] != cook_id
            || record.state.is_terminal()
            || record.metadata["detached_cook_handoff"]["state"] != "pending"
            || lifecycle_store.read_cook_index(&cook_id).is_ok()
            || record.metadata["detached_cook_handoff"]["materializing_attempt_run_id"]
                .as_str()
                .is_some_and(|run_id| lifecycle_store.read_record(run_id).is_ok())
        {
            return false;
        }
        let cancelled = record.state == AgentTaskRunState::Cancelled;
        let metadata = record.ensure_metadata_object();
        metadata["detached_cook_handoff"]["state"] = json!(if cancelled {
            "cancelled"
        } else {
            "exited_before_handoff"
        });
        metadata["detached_cook_handoff"]["admission_state"] =
            json!(if cancelled { "cancelled" } else { "failed" });
        metadata["detached_cook_handoff"]["reason"] = json!(reason);
        if !record.state.is_terminal() {
            set_run_state(record, AgentTaskRunState::Failed);
        }
        record.updated_at = Some(now_timestamp());
        true
    })?;
    // A protected parent is a successful no-op: it is the authoritative result
    // of materialization or a prior terminal transition, not a missing parent.
    Ok(record.unwrap_or(lifecycle_store.read_record(&cook_id)?))
}

fn complete_detached_cook_handoff_parent_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    attempt_run_id: &str,
) -> Result<()> {
    let cook_id = sanitize_run_id(cook_id);
    let attempt_run_id = sanitize_run_id(attempt_run_id);
    if lifecycle_store.read_record(&cook_id).is_err() {
        return Ok(());
    }
    let _ =
        lifecycle_store.mutate_record_locked_without_terminal_projection(&cook_id, |record| {
            if record.metadata["detached_cook_handoff"]["cook_id"] != cook_id
                || record.state.is_terminal()
                || record.metadata["detached_cook_handoff"]["state"] != "pending"
            {
                return false;
            }
            let metadata = record.ensure_metadata_object();
            metadata["detached_cook_handoff"]["state"] = json!("redirected");
            metadata["detached_cook_handoff"]["admission_state"] = json!("materialized");
            metadata["detached_cook_handoff"]["attempt_run_id"] = json!(attempt_run_id);
            metadata["detached_cook_handoff"]
                .as_object_mut()
                .expect("handoff metadata object")
                .remove("materializing_attempt_run_id");
            set_run_state(record, AgentTaskRunState::Succeeded);
            record.updated_at = Some(now_timestamp());
            true
        })?;
    Ok(())
}

/// A detached Cook parent is admission state, never an executable queued plan.
///
/// The original persisted shape only carries `state: pending`; treating that as
/// the default preserves mixed-version records while newer writers add a more
/// specific `admission_state` for operator visibility.
pub fn has_pending_detached_cook_handoff(record: &AgentTaskRunRecord) -> bool {
    record.metadata["detached_cook_handoff"]["cook_id"] == record.run_id
        && record.metadata["detached_cook_handoff"]["state"] == "pending"
}

pub fn detached_cook_admission_is_live(
    record: &AgentTaskRunRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if !has_pending_detached_cook_handoff(record) {
        return false;
    }
    match record.metadata["detached_cook_handoff"]["admission_state"].as_str() {
        Some("supervising") => true,
        Some("unmaterialized") => {
            record.state == AgentTaskRunState::Queued
                && record.metadata["unmaterialized_cook_admission"]["state"]
                    .as_str()
                    .is_some_and(|state| {
                        matches!(
                            state,
                            "preparing_inputs"
                                | "queued"
                                | "blocked_runner_unavailable"
                                | "blocked_runner_stale"
                                | "replaying"
                                | "materializing"
                        )
                    })
        }
        Some("child_attached") => detached_cook_child_is_live(record).unwrap_or_else(|| {
            detached_cook_deadline(record, "child_supervisor_deadline_at")
                .is_some_and(|deadline| deadline > now)
        }),
        Some("pre_supervisor") | None => detached_cook_deadline(record, "admission_deadline_at")
            .is_some_and(|deadline| deadline > now),
        _ => false,
    }
}

pub fn has_expired_detached_cook_admission(
    record: &AgentTaskRunRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    has_pending_detached_cook_handoff(record) && !detached_cook_admission_is_live(record, now)
}

// The ambient `expire_detached_cook_admission()` shim that used to sit here is
// gone; the reconciler was its only caller and now expires inside the store it classified the run from (#7505).

/// Expire a detached Cook's admission lease inside an explicitly rooted store.
///
/// The liveness read and the terminal handoff write share one store, so an
/// expiry decided from one queue's evidence can never terminalize a parent
/// admission in another. Liveness itself is computed from the record and the
/// process table, both of which are already root-free.
pub fn expire_detached_cook_admission_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<bool> {
    let record = lifecycle_store.read_record(cook_id)?;
    if !has_expired_detached_cook_admission(&record, chrono::Utc::now()) {
        return Ok(false);
    }
    let terminal = fail_detached_cook_handoff_parent_in_store(
        lifecycle_store,
        cook_id,
        "detached Cook admission lease expired before child or supervisor ownership attached",
    )?;
    Ok(terminal.state == AgentTaskRunState::Failed
        && terminal.metadata["detached_cook_handoff"]["admission_state"] == "failed")
}

fn detached_cook_child_is_live(record: &AgentTaskRunRecord) -> Option<bool> {
    let handoff = &record.metadata["detached_cook_handoff"];
    let pid = handoff["child_pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())?;
    let identity = serde_json::from_value(handoff["child_start_identity"].clone()).ok()?;
    match homeboy_core::process::process_identity_state_with_start_identity(
        pid,
        None,
        Some(&identity),
    ) {
        homeboy_core::process::ProcessIdentityState::Live => Some(true),
        homeboy_core::process::ProcessIdentityState::Dead
        | homeboy_core::process::ProcessIdentityState::IdentityMismatch => Some(false),
        homeboy_core::process::ProcessIdentityState::Unverifiable => None,
    }
}

fn detached_cook_deadline(
    record: &AgentTaskRunRecord,
    field: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    record.metadata["detached_cook_handoff"][field]
        .as_str()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(&record.submitted_at)
                .ok()
                .map(|value| {
                    value.with_timezone(&chrono::Utc)
                        + chrono::Duration::seconds(DETACHED_COOK_ADMISSION_LEASE_SECONDS)
                })
        })
}

/// Build the canonical decision for a run this controller is about to execute
/// itself. Identity is read off the plan's own workspace so the durable record
/// names the same checkout the run operates on.
fn controller_local_placement_decision(
    plan: &AgentTaskPlan,
) -> homeboy_lab_runner_contract::ExecutionPlacementDecision {
    let workspace_root = plan
        .tasks
        .first()
        .and_then(|task| task.workspace.root.as_deref());
    // `local_override` is the preflight record of an operator's explicit
    // `--placement local`. Carrying it here is what gives a locally-created run
    // an authorized owner rather than a merely-default one.
    let requested = if homeboy_core::resource_policy_context::captured_context()
        .is_some_and(|context| context.local_override)
    {
        homeboy_lab_runner_contract::Placement::Local
    } else {
        homeboy_lab_runner_contract::Placement::Auto
    };
    homeboy_lab_runner_contract::ExecutionPlacementDecision::controller_local(
        homeboy_lab_runner_contract::CONTROLLER_LOCAL_SUBMISSION_POLICY_ID,
        "v1",
        homeboy_lab_runner_contract::ExecutionPlacementIdentity {
            repository: workspace_root
                .and_then(|root| std::path::Path::new(root).file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "controller-local".to_string()),
            workspace: workspace_root
                .map(str::to_string)
                .unwrap_or_else(|| "controller-local".to_string()),
            task: plan
                .tasks
                .first()
                .map(|task| task.task_id.clone())
                .unwrap_or_else(|| plan.plan_id.clone()),
            candidate: None,
            base: None,
        },
        requested,
    )
}

/// Adopt a canonical placement decision for a durable run that has none.
///
/// Records written before placement decisions were canonical — and records
/// written by submission paths that never authored one — carry a null
/// `execution_placement_decision`. That is not a contradiction to fail closed
/// on; it is missing evidence, and the routing decision the caller is holding
/// *is* the authoritative one for the execution about to be verified. Adopting
/// it (and saying so, under `execution_placement_normalized`) is what lets an
/// older local run be diagnosed and retried instead of dead-ending (#11600).
///
/// Never overwrites a routed decision. A submission stamp is superseded,
/// because it was authored in the absence of routing precisely so this moment
/// would have something to supersede.
pub fn normalize_missing_execution_placement_decision(
    run_id: &str,
    decision: &homeboy_lab_runner_contract::ExecutionPlacementDecision,
) -> Result<bool> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    normalize_missing_execution_placement_decision_in_store(&lifecycle_store, run_id, decision)
}

/// Adopt a canonical placement decision inside an explicitly rooted store.
///
/// The persisted-decision read and the adoption write are one decision, so they
/// share one store. Read ambiently, a decision already recorded in another home
/// could veto an adoption here, or a supersedable submission stamp found there
/// could authorize a write into this store that its own record never justified.
pub fn normalize_missing_execution_placement_decision_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    decision: &homeboy_lab_runner_contract::ExecutionPlacementDecision,
) -> Result<bool> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    let persisted =
        serde_json::from_value::<homeboy_lab_runner_contract::ExecutionPlacementDecision>(
            record.metadata["execution_placement_decision"].clone(),
        )
        .ok();
    let reason = match persisted {
        Some(persisted) if persisted.decision_id == decision.decision_id => return Ok(false),
        Some(persisted) if persisted.is_submission_stamp() => {
            "durable run carried a submission-derived placement decision"
        }
        Some(_) => return Ok(false),
        None => "durable run had no canonical placement decision",
    };
    record.metadata["execution_placement_decision"] =
        serde_json::to_value(decision).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize normalized placement decision".to_string()),
            )
        })?;
    record.metadata["execution_placement_normalized"] = json!({
        "at": now_timestamp(),
        "reason": reason,
        "adopted_decision_id": decision.decision_id,
    });
    lifecycle_store.write_record(&record)?;
    Ok(true)
}

/// Append a provider-verified placement outcome to the durable run without
/// re-evaluating policy. A mismatched decision id is a stale/replayed attempt
/// and fails closed.
pub fn record_execution_placement_outcome(
    run_id: &str,
    outcome: homeboy_lab_runner_contract::ExecutionPlacementOutcome,
) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_execution_placement_outcome_in_store(&lifecycle_store, run_id, outcome)
}

/// Append a provider-verified placement outcome inside an explicitly rooted
/// store.
///
/// The decision this outcome is checked against and the record it is appended
/// to are read and written through the same store. Reading the decision
/// ambiently would let another home's routing decide whether a verified
/// execution here is a stale replay, and would fail closed or open on evidence
/// that never described this run.
pub fn record_execution_placement_outcome_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    outcome: homeboy_lab_runner_contract::ExecutionPlacementOutcome,
) -> Result<()> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    let decision: homeboy_lab_runner_contract::ExecutionPlacementDecision = serde_json::from_value(
        record.metadata["execution_placement_decision"].clone(),
    )
    .map_err(|error| {
        Error::validation_invalid_argument(
            "execution_placement_decision",
            format!("durable run has no valid canonical placement decision: {error}"),
            Some(run_id.to_string()),
            None,
        )
    })?;
    if decision.decision_id != outcome.decision_id
        || !decision.verifies_outcome(outcome.effective)
        || (outcome.effective == homeboy_lab_runner_contract::EffectiveExecutionPlacement::Lab
            && decision
                .runner
                .as_ref()
                .map(|runner| runner.runner_id.as_str())
                != outcome.runner_id.as_deref())
    {
        return Err(Error::validation_invalid_argument(
            "execution_placement_outcome",
            "verified placement outcome contradicts the durable routing decision",
            Some(run_id.to_string()),
            None,
        ));
    }
    record.metadata["execution_placement_outcome"] =
        serde_json::to_value(outcome).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize placement outcome".to_string()),
            )
        })?;
    lifecycle_store.write_record(&record)
}

/// Replace a failed pre-provider attempt's placement with an explicitly
/// authorized local continuation. The immutable Cook recipe remains the source
/// of work; this records only the next execution's routing authority.
pub fn transition_execution_placement_for_continuation(
    run_id: &str,
    replacement: homeboy_lab_runner_contract::ExecutionPlacementDecision,
) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    transition_execution_placement_for_continuation_in_store(&lifecycle_store, run_id, replacement)
}

/// [`transition_execution_placement_for_continuation`] against an explicitly
/// rooted lifecycle store.
pub fn transition_execution_placement_for_continuation_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    replacement: homeboy_lab_runner_contract::ExecutionPlacementDecision,
) -> Result<()> {
    use homeboy_lab_runner_contract::ExecutionPlacementRequirement;

    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    let prior: homeboy_lab_runner_contract::ExecutionPlacementDecision = serde_json::from_value(
        record.metadata["execution_placement_decision"].clone(),
    )
    .map_err(|error| {
        Error::validation_invalid_argument(
            "execution_placement_decision",
            format!("durable run has no valid canonical placement decision: {error}"),
            Some(run_id.to_string()),
            None,
        )
    })?;
    let pre_provider_failure = record.metadata["pre_execution_failure"].is_object()
        && record.metadata["provider_executions"]
            .as_array()
            .is_none_or(Vec::is_empty);
    if prior.required == ExecutionPlacementRequirement::Lab
        || !replacement.permits_local_execution()
        || !pre_provider_failure
        || prior.identity != replacement.identity
        || prior.policy_id != replacement.policy_id
        || prior.policy_revision != replacement.policy_revision
        || record.metadata["execution_placement_transition"].is_object()
        || record.metadata["transport_admission_reset"].is_object()
    {
        return Err(Error::validation_invalid_argument(
            "placement",
            "explicit local continuation is permitted once after a pre-provider failure from a non-Lab-required placement with the same execution identity",
            Some(run_id.to_string()),
            None,
        ));
    }

    let mut plan = load_controller_plan_in_store(lifecycle_store, &record.run_id)?;
    plan.metadata["execution_placement_decision"] =
        serde_json::to_value(&replacement).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize continuation placement".to_string()),
            )
        })?;
    persist_controller_plan_in_store(lifecycle_store, &record.run_id, &plan)?;
    record.metadata["execution_placement_decision"] =
        plan.metadata["execution_placement_decision"].clone();
    record.metadata["execution_placement_outcome"] = Value::Null;
    record.metadata["execution_placement_transition"] = json!({
        "kind": "explicit_local_continuation",
        "prior_decision_id": prior.decision_id,
        "replacement_decision_id": replacement.decision_id,
        "reason": "pre_provider_failure",
    });
    // This is a one-shot admission reset, not a retry-budget reset. The
    // replacement run consumes it by receiving a new transport identity.
    record.metadata["transport_admission_reset"] = json!({
        "kind": "placement_transition",
        "prior_decision_id": prior.decision_id,
        "replacement_decision_id": replacement.decision_id,
        "reason": "explicit_local_continuation_after_pre_provider_failure",
    });
    lifecycle_store.write_record(&record)
}

// The ambient `normalize_local_execution_placement()` shim that used to sit here is gone;
// its two normalization tests now normalize inside a store they resolve (#7505).

/// Restore an explicit plan placement decision onto a legacy record inside an
/// explicitly rooted store.
///
/// The plan read moves with the record. `load_plan` resolves a Cook alias
/// against the ambient index and then reads the ambient run directory, so an
/// ambient reach here could copy another home's plan decision onto this store's
/// record — the one restoration this operation exists to make trustworthy.
pub fn normalize_local_execution_placement_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    if record
        .metadata
        .get("execution_placement_decision")
        .is_some_and(|value| !value.is_null())
    {
        return Ok(record);
    }
    let plan = load_controller_plan_in_store(lifecycle_store, &record.run_id)?;
    let Some(decision) = plan
        .metadata
        .get("execution_placement_decision")
        .filter(|value| !value.is_null())
    else {
        return Ok(record);
    };
    record
        .ensure_metadata_object()
        .insert("execution_placement_decision".to_string(), decision.clone());
    record.ensure_metadata_object().insert(
        "execution_placement_normalization".to_string(),
        json!({ "source": "controller_plan", "reason": "legacy_or_null_record_decision" }),
    );
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

#[cfg(test)]
mod execution_placement_tests {
    use super::*;
    use homeboy_lab_runner_contract::{
        EffectiveExecutionPlacement, ExecutionPlacementFallback, ExecutionPlacementIdentity,
        ExecutionPlacementOverrideAuthorization, ExecutionPlacementRequirement,
        ExecutionPlacementRunnerSelection, Placement, RunnerSelectionSource,
    };

    fn decision() -> homeboy_lab_runner_contract::ExecutionPlacementDecision {
        homeboy_lab_runner_contract::ExecutionPlacementDecision::new(
            "test-policy",
            "1",
            ExecutionPlacementIdentity {
                repository: "repo".to_string(),
                workspace: "workspace".to_string(),
                task: "task".to_string(),
                candidate: Some("candidate-a".to_string()),
                base: Some("base-a".to_string()),
            },
            Placement::Lab,
            ExecutionPlacementRequirement::Lab,
            EffectiveExecutionPlacement::Lab,
            Some(ExecutionPlacementRunnerSelection {
                runner_id: "lab-a".to_string(),
                source: RunnerSelectionSource::Explicit,
            }),
            ExecutionPlacementFallback {
                local_allowed: false,
                reason: None,
            },
            ExecutionPlacementOverrideAuthorization {
                authorized: false,
                authority: None,
            },
        )
    }

    fn local_continuation(
        prior: &homeboy_lab_runner_contract::ExecutionPlacementDecision,
    ) -> homeboy_lab_runner_contract::ExecutionPlacementDecision {
        homeboy_lab_runner_contract::ExecutionPlacementDecision::new(
            prior.policy_id.clone(),
            prior.policy_revision.clone(),
            prior.identity.clone(),
            Placement::Local,
            ExecutionPlacementRequirement::Either,
            EffectiveExecutionPlacement::Local,
            None,
            ExecutionPlacementFallback {
                local_allowed: false,
                reason: None,
            },
            ExecutionPlacementOverrideAuthorization {
                authorized: true,
                authority: Some("operator --placement local".to_string()),
            },
        )
    }

    #[test]
    fn explicit_local_continuation_replaces_only_pre_provider_auto_lab_placement() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut plan = super::super::tests::test_plan();
            let lab = decision();
            let prior = homeboy_lab_runner_contract::ExecutionPlacementDecision::new(
                lab.policy_id,
                lab.policy_revision,
                lab.identity,
                Placement::Auto,
                ExecutionPlacementRequirement::Either,
                EffectiveExecutionPlacement::Lab,
                lab.runner,
                ExecutionPlacementFallback {
                    local_allowed: true,
                    reason: None,
                },
                lab.override_authorization,
            );
            plan.metadata = json!({ "execution_placement_decision": prior });
            submit_plan_with_runtime_admission(&plan, Some("local-continuation"), |_| {
                Ok(json!({}))
            })
            .expect("submit routed Lab attempt");
            record_pre_execution_failure(
                "local-continuation",
                &plan,
                "lab_handoff_preacceptance",
                &Error::internal_unexpected("Lab disconnected before provider start"),
            )
            .expect("record pre-provider failure");

            let replacement = local_continuation(&prior);
            let mut stale_identity = prior.identity.clone();
            stale_identity.candidate = Some("candidate-b".to_string());
            let stale = homeboy_lab_runner_contract::ExecutionPlacementDecision::new(
                prior.policy_id.clone(),
                prior.policy_revision.clone(),
                stale_identity,
                Placement::Local,
                ExecutionPlacementRequirement::Either,
                EffectiveExecutionPlacement::Local,
                None,
                ExecutionPlacementFallback {
                    local_allowed: false,
                    reason: None,
                },
                ExecutionPlacementOverrideAuthorization {
                    authorized: true,
                    authority: Some("operator --placement local".to_string()),
                },
            );
            transition_execution_placement_for_continuation("local-continuation", stale)
                .expect_err("a stale placement decision is rejected");
            transition_execution_placement_for_continuation(
                "local-continuation",
                replacement.clone(),
            )
            .expect("explicit local continuation is authorized");

            let record = status("local-continuation").expect("read transitioned attempt");
            assert_eq!(
                record.metadata["execution_placement_decision"]["decision_id"],
                replacement.decision_id
            );
            assert_eq!(
                record.metadata["execution_placement_transition"]["prior_decision_id"],
                prior.decision_id
            );
            assert_eq!(
                load_controller_plan("local-continuation")
                    .expect("read transitioned controller plan")
                    .metadata["execution_placement_decision"]["decision_id"],
                replacement.decision_id
            );
            assert_eq!(
                record.metadata["transport_admission_reset"]["replacement_decision_id"],
                replacement.decision_id
            );
            transition_execution_placement_for_continuation("local-continuation", replacement)
                .expect_err("the placement transition cannot mint another admission reset");
        });
    }

    #[test]
    fn explicit_local_continuation_rejects_lab_required_policy() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut plan = super::super::tests::test_plan();
            let prior = decision();
            plan.metadata = json!({ "execution_placement_decision": prior });
            submit_plan_with_runtime_admission(&plan, Some("lab-only-continuation"), |_| {
                Ok(json!({}))
            })
            .expect("submit Lab-required attempt");
            record_pre_execution_failure(
                "lab-only-continuation",
                &plan,
                "lab_handoff_preacceptance",
                &Error::internal_unexpected("Lab disconnected before provider start"),
            )
            .expect("record pre-provider failure");

            let error = transition_execution_placement_for_continuation(
                "lab-only-continuation",
                local_continuation(&prior),
            )
            .expect_err("Lab-required policy fails closed");
            assert!(error.message.contains("non-Lab-required"));
            assert_eq!(
                status("lab-only-continuation")
                    .expect("read unchanged attempt")
                    .metadata["execution_placement_decision"]["decision_id"],
                prior.decision_id
            );
        });
    }

    #[test]
    fn durable_plan_status_and_verified_outcome_keep_one_decision_identity() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut plan = super::super::tests::test_plan();
            let placement = decision();
            plan.metadata = json!({ "execution_placement_decision": placement });
            let record = submit_plan_with_runtime_admission(&plan, Some("placement-run"), |_| {
                Ok(json!({ "runtime": "test" }))
            })
            .expect("submit plan");
            assert_eq!(
                record.metadata["execution_placement_decision"]["decision_id"],
                decision().decision_id
            );
            record_execution_placement_outcome(
                "placement-run",
                decision()
                    .outcome(EffectiveExecutionPlacement::Lab, Some("lab-a".to_string()))
                    .expect("Lab is authorized"),
            )
            .expect("record verified outcome");
            let status = status("placement-run").expect("status");
            assert_eq!(
                status.metadata["execution_placement_outcome"]["decision_id"],
                decision().decision_id
            );
        });
    }

    #[test]
    fn a_plan_without_a_routing_decision_persists_a_canonical_local_one() {
        // The originating defect in #11600: an explicitly local Cook submitted
        // a plan that had never been routed anywhere, so nothing wrote a
        // canonical decision and `execution_placement_decision` stayed null.
        // Retry then could not decode the owner of the run it was recovering.
        homeboy_core::test_support::with_isolated_home(|_| {
            let plan = super::super::tests::test_plan();
            let record = submit_plan_with_runtime_admission(&plan, Some("local-run"), |_| {
                Ok(json!({ "runtime": "test" }))
            })
            .expect("submit plan");

            let decision: homeboy_lab_runner_contract::ExecutionPlacementDecision =
                serde_json::from_value(record.metadata["execution_placement_decision"].clone())
                    .expect("a controller-local run records a decodable canonical decision");
            assert_eq!(decision.selected, EffectiveExecutionPlacement::Local);
            assert!(decision.runner.is_none());
            assert!(decision.permits_local_execution());

            // The whole point of recording it: the local outcome now verifies.
            record_execution_placement_outcome(
                "local-run",
                decision
                    .outcome(EffectiveExecutionPlacement::Local, None)
                    .expect("a controller-local decision authorizes a local outcome"),
            )
            .expect("record verified local outcome");
        });
    }

    #[test]
    fn a_null_decision_is_normalized_to_the_routing_decision_that_verified_it() {
        // Records written before the canonical decision existed carry a null.
        // That is missing evidence, not a contradiction — adopting the routing
        // decision in hand (and saying so) is what makes an older local run
        // recoverable instead of a dead end (#11600).
        homeboy_core::test_support::with_isolated_home(|_| {
            let plan = super::super::tests::test_plan();
            submit_plan_with_runtime_admission(&plan, Some("legacy-run"), |_| Ok(json!({})))
                .expect("submit plan");
            let mut record = store::read_record("legacy-run").expect("read record");
            record.metadata["execution_placement_decision"] = Value::Null;
            store::write_record(&record).expect("write legacy record");

            let routed = homeboy_lab_runner_contract::ExecutionPlacementDecision::controller_local(
                "lab-route-contract",
                "v1",
                ExecutionPlacementIdentity {
                    repository: "homeboy".to_string(),
                    workspace: "/workspace/homeboy".to_string(),
                    task: "legacy-task".to_string(),
                    candidate: None,
                    base: None,
                },
                Placement::Local,
            );

            assert!(
                normalize_missing_execution_placement_decision("legacy-run", &routed)
                    .expect("normalize legacy record"),
                "a null decision is adopted"
            );
            assert!(
                !normalize_missing_execution_placement_decision("legacy-run", &routed)
                    .expect("normalize is idempotent"),
                "an already-canonical decision is never overwritten"
            );

            let record = store::read_record("legacy-run").expect("read normalized record");
            assert_eq!(
                record.metadata["execution_placement_decision"]["decision_id"],
                json!(routed.decision_id)
            );
            assert_eq!(
                record.metadata["execution_placement_normalized"]["adopted_decision_id"],
                json!(routed.decision_id)
            );
            record_execution_placement_outcome(
                "legacy-run",
                routed
                    .outcome(EffectiveExecutionPlacement::Local, None)
                    .expect("local outcome"),
            )
            .expect("a normalized record accepts its verified local outcome");
        });
    }

    #[test]
    fn a_submission_stamp_is_superseded_by_the_routing_decision_that_verified_it() {
        // The stamp exists so a purely local run has an owner. It is derived,
        // not routed, so when routing later produces its own decision for the
        // same run the routed one wins — otherwise the stamp would collide with
        // the outcome it was introduced to make recordable.
        homeboy_core::test_support::with_isolated_home(|_| {
            let plan = super::super::tests::test_plan();
            submit_plan_with_runtime_admission(&plan, Some("stamped-run"), |_| Ok(json!({})))
                .expect("submit plan");
            let stamped = store::read_record("stamped-run").expect("read record");
            assert_eq!(
                stamped.metadata["execution_placement_decision"]["policy_id"],
                json!(homeboy_lab_runner_contract::CONTROLLER_LOCAL_SUBMISSION_POLICY_ID)
            );

            let routed = homeboy_lab_runner_contract::ExecutionPlacementDecision::controller_local(
                "lab-route-contract",
                "v1",
                ExecutionPlacementIdentity {
                    repository: "homeboy".to_string(),
                    workspace: "/workspace/homeboy".to_string(),
                    task: "routed-task".to_string(),
                    candidate: None,
                    base: None,
                },
                Placement::Local,
            );
            assert!(
                normalize_missing_execution_placement_decision("stamped-run", &routed)
                    .expect("supersede the submission stamp")
            );
            record_execution_placement_outcome(
                "stamped-run",
                routed
                    .outcome(EffectiveExecutionPlacement::Local, None)
                    .expect("local outcome"),
            )
            .expect("the routed decision now owns the record");
        });
    }

    #[test]
    fn stale_or_contradictory_outcomes_fail_closed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut plan = super::super::tests::test_plan();
            plan.metadata = json!({ "execution_placement_decision": decision() });
            submit_plan_with_runtime_admission(&plan, Some("placement-run"), |_| Ok(json!({})))
                .expect("submit plan");
            let error = record_execution_placement_outcome(
                "placement-run",
                homeboy_lab_runner_contract::ExecutionPlacementOutcome {
                    decision_id: "stale".to_string(),
                    effective: EffectiveExecutionPlacement::Local,
                    runner_id: None,
                },
            )
            .expect_err("reject contradictory outcome");
            assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        });
    }

    #[test]
    fn normalization_preserves_submission_stamp_and_projects_explicit_plan_decision() {
        homeboy_core::test_support::with_isolated_home(|_| {
            // One store for the whole test: both normalizations and the record
            // they read must name one installation.
            let store =
                AgentTaskLifecycleStore::from_current_environment().expect("lifecycle store");
            let plan = super::super::tests::test_plan();
            let record =
                submit_plan_with_runtime_admission(&plan, Some("placement-run"), |_| Ok(json!({})))
                    .expect("submit plan");

            let normalized = normalize_local_execution_placement_in_store(&store, &record.run_id)
                .expect("submission stamp remains authoritative");
            let submission_decision: homeboy_lab_runner_contract::ExecutionPlacementDecision =
                serde_json::from_value(normalized.metadata["execution_placement_decision"].clone())
                    .expect("controller-local submission records a canonical decision");
            assert!(submission_decision.is_submission_stamp());
            assert_eq!(
                submission_decision.selected,
                EffectiveExecutionPlacement::Local
            );
            assert!(submission_decision.permits_local_execution());
            assert!(normalized
                .metadata
                .get("execution_placement_normalization")
                .is_none());
            assert_eq!(load_plan(&record.run_id).expect("durable plan"), plan);

            let mut plan = plan;
            plan.metadata = json!({ "execution_placement_decision": decision() });
            let record =
                submit_plan_with_runtime_admission(&plan, Some("placement-projection-run"), |_| {
                    Ok(json!({}))
                })
                .expect("submit plan with placement decision");
            let mut legacy_record = store::read_record(&record.run_id).expect("durable record");
            legacy_record
                .ensure_metadata_object()
                .remove("execution_placement_decision");
            store::write_record(&legacy_record).expect("remove legacy record projection");

            let normalized = normalize_local_execution_placement_in_store(&store, &record.run_id)
                .expect("explicit plan decision is restored");
            assert_eq!(
                normalized.metadata["execution_placement_decision"]["decision_id"],
                decision().decision_id
            );
        });
    }
}

pub(crate) trait RuntimeAdmissionEvidence {
    fn runtime(&self) -> Value;
}

impl RuntimeAdmissionEvidence for homeboy_core::controller_runtime::RuntimeAdmission {
    fn runtime(&self) -> Value {
        self.runtime.clone()
    }
}

impl RuntimeAdmissionEvidence for Value {
    fn runtime(&self) -> Value {
        self.clone()
    }
}

/// Persist the run identity before controller admission so an admission failure
/// remains inspectable and retryable through the normal lifecycle commands.
pub(crate) fn submit_plan_with_runtime_admission<F, A>(
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
    admit_runtime: F,
) -> Result<AgentTaskRunRecord>
where
    F: FnOnce(&str) -> Result<A>,
    A: RuntimeAdmissionEvidence,
{
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    submit_plan_with_runtime_admission_in_store(
        &lifecycle_store,
        plan,
        requested_run_id,
        execution_runner_id(),
        None,
        Some(&|run_id| homeboy_core::controller_runtime::admission_status(run_id).ok()),
        admit_runtime,
    )
}

// `submit_plan_with_runtime_admission_on_runner_with_metadata` lived here. Its
// only caller was the retry admission, which now calls
// `submit_plan_with_runtime_admission_in_store` with the store it was handed
// rather than resolving a second one from the environment (#7505).
//
// `submit_plan_with_runtime_admission_on_runner` lived here too, and went the
// same way for a weaker reason: it never had a caller at all. It resolved a
// root from the environment and passed it to
// `submit_plan_with_runtime_admission_in_store` with an explicit
// `execution_runner_id`, which is what every remaining caller already does with
// the store it was handed (#7505).

pub(crate) fn submit_plan_with_runtime_admission_in_store<F, A>(
    lifecycle_store: &AgentTaskLifecycleStore,
    plan: &AgentTaskPlan,
    requested_run_id: Option<&str>,
    execution_runner_id: Option<String>,
    submission_metadata: Option<serde_json::Map<String, Value>>,
    admission_status: Option<&dyn Fn(&str) -> Option<Value>>,
    admit_runtime: F,
) -> Result<AgentTaskRunRecord>
where
    F: FnOnce(&str) -> Result<A>,
    A: RuntimeAdmissionEvidence,
{
    let workspace_claim_store = lifecycle_store.workspace_claim_store();
    let mut normalized_plan = plan.clone();
    if normalized_plan.workspace_identity.is_none() {
        normalized_plan.workspace_identity = identity_for_plan(&normalized_plan)?;
    }
    let run_id = requested_run_id
        .map(sanitize_run_id)
        .unwrap_or_else(default_run_id);
    if let Some(identity) = normalized_plan.workspace_identity.clone() {
        normalized_plan.workspace_owner_lease = Some(register_local_workspace_owner_in_store(
            &workspace_claim_store,
            identity,
            &run_id,
        )?);
        normalized_plan.workspace_lifecycle_revision = normalized_plan
            .workspace_owner_lease
            .as_ref()
            .expect("registered owner lease")
            .lifecycle_revision;
    }
    let plan = &normalized_plan;
    let plan_path = match lifecycle_store.write_controller_plan(&run_id, plan) {
        Ok(path) => path,
        Err(error) => {
            if let Some(lease) = plan.workspace_owner_lease.as_ref() {
                let _ = release_local_workspace_owner_in_store(&workspace_claim_store, lease);
            }
            return Err(error);
        }
    };

    let mut metadata = json!({
        "task_count": plan.tasks.len(),
        "max_concurrency": plan.options.max_concurrency,
        "provider_run_ids": [],
        "provider_executions_consumed": 0,
        "controller_identity": homeboy_core::build_identity::current().display,
        "lifecycle_schema": RUN_LIFECYCLE_RECORD_SCHEMA,
        "note": "submitted tasks are durable; provider run ids are recorded after an executor returns them as generic artifacts or evidence refs"
    });
    let activity_contexts = plan
        .tasks
        .iter()
        .map(|task| {
            json!({
                "task_url": task.workspace.task_url.clone().or_else(|| task.source_refs.iter().find(|source| source.kind == "task").or_else(|| task.source_refs.first()).map(|source| source.uri.clone())),
                "repository": task.workspace.task_url.as_deref().or_else(|| task.source_refs.iter().find(|source| source.kind == "task").or_else(|| task.source_refs.first()).map(|source| source.uri.as_str())).and_then(|url| url.split("github.com/").nth(1)).and_then(|path| { let mut segments = path.split('/'); Some(format!("{}/{}", segments.next()?, segments.next()?)) }),
                "worktree": task.workspace.root,
            })
        })
        .collect::<Vec<_>>();
    if let Some(context) = activity_contexts.first() {
        // Keep the original single-task context readable for mixed-version
        // consumers while publishing every identity in the additive array.
        metadata["activity_context"] = context.clone();
        metadata["activity_contexts"] = json!(activity_contexts);
    }
    let acceptance_requirement = plan
        .metadata
        .get("acceptance")
        .cloned()
        .and_then(|value| serde_json::from_value::<AgentTaskAcceptanceRequirement>(value).ok());
    if let Some(requirement) = acceptance_requirement {
        validate_acceptance_requirement(&requirement)?;
        metadata["acceptance_requirement"] =
            serde_json::to_value(requirement).expect("acceptance requirement is serializable");
    }
    if let Some(runner_id) = execution_runner_id.as_deref() {
        metadata["runner_id"] = json!(runner_id);
        if let Some(execution_context) =
            homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::from_direct_daemon_child_environment(
                runner_id,
            )?
        {
            project_runner_execution_context(&mut metadata, &execution_context)?;
        }
    }
    // The plan is the immutable cross-process carrier. Project the identical
    // decision onto the run record so status, finalization, and PR evidence do
    // not infer placement from ambient runner metadata.
    match plan.metadata.get("execution_placement_decision") {
        Some(decision) if !decision.is_null() => {
            metadata["execution_placement_decision"] = decision.clone();
        }
        // A plan that carries no routing decision was not routed anywhere: this
        // controller is submitting a run it is about to execute itself. That is
        // a placement, and it has to be *recorded* rather than inferred later.
        //
        // Leaving it null is what made an explicitly local Cook unrecoverable —
        // retry decodes the originating record's canonical decision, and there
        // was nothing there to decode (#11600).
        //
        // A submission carrying `execution_runner_id` is a runner-side
        // projection of a decision the controller already owns; it is not this
        // process's to author.
        _ if execution_runner_id.is_none() => {
            metadata["execution_placement_decision"] =
                serde_json::to_value(controller_local_placement_decision(plan))
                    .expect("controller-local placement decision is serializable");
        }
        _ => {}
    }
    // A replacement decision is only reviewable after it reaches the durable
    // run record. Keep its invalidation evidence alongside the decision rather
    // than leaving it in the transient plan file.
    if let Some(invalidation) = plan.metadata.get("execution_placement_invalidated") {
        metadata["execution_placement_invalidated"] = invalidation.clone();
    }
    // Surface controller-owned worktree convergence in the run record as well
    // as the immutable plan, so status and resumed execution retain the same
    // reviewer-facing evidence.
    if let Some(provision) = plan
        .tasks
        .first()
        .and_then(|task| task.metadata.get("worktree_provision"))
    {
        metadata["worktree_provision"] = provision.clone();
    }
    if let Some(route) = homeboy_core::notification_route::current() {
        route.insert_into_metadata(&mut metadata);
    }
    if let Some(resolution) = homeboy_core::notification_route::current_resolution() {
        resolution.insert_into_metadata(&mut metadata);
    }
    if let Some(submission_metadata) = submission_metadata {
        metadata
            .as_object_mut()
            .expect("submission metadata is an object")
            .extend(submission_metadata);
    }

    let mut record = AgentTaskRunRecord {
        schema: schemas::RUN.to_string(),
        run_id: run_id.clone(),
        plan_id: plan.plan_id.clone(),
        state: AgentTaskRunState::Queued,
        submitted_at: now_timestamp(),
        updated_at: None,
        plan_path: plan_path.display().to_string(),
        aggregate_path: None,
        totals: None,
        tasks: plan.tasks.iter().map(queued_task).collect(),
        artifact_refs: Vec::new(),
        provider_handles: Vec::new(),
        latest_executor_evidence: None,
        lifecycle: lifecycle_for_submitted_plan(plan),
        lab_handoff: None,
        candidate_adoption: None,
        adoption_run_id: None,
        acceptance: None,
        workspace_identity: plan.workspace_identity.clone(),
        workspace_lifecycle_revision: plan.workspace_lifecycle_revision,
        workspace_owner_lease: plan.workspace_owner_lease.clone(),
        workspace_claim: None,
        metadata,
    };
    let mut preserved_controller_runtime = None;
    if let Ok(existing) = lifecycle_store.read_record(&run_id) {
        // A runner may re-submit the plan after the controller reserved a
        // side-effect claim. Claims are durable exactly-once ownership, not
        // plan-derived state, so replacing the record must retain them.
        if let Some(claims) = existing.metadata.get("cook_operation_claims") {
            record.metadata["cook_operation_claims"] = claims.clone();
        }
        // Cook reports `provider_start` before a local executor or Lab runner
        // re-submits its materialized plan. That submission refreshes plan-owned
        // fields but must not erase the durable lifecycle boundary that says
        // provider work has started.
        for key in ["cook_id", "cook_attempt", "cook_progress"] {
            if let Some(value) = existing.metadata.get(key) {
                record.metadata[key] = value.clone();
            }
        }
        // A runner re-submitting a retry must not erase the predecessor identity
        // that makes the reservation discoverable through the indexed lookup.
        for key in [
            "retry_of",
            "retried_from",
            "retry_root",
            "retries",
            "retry_requested_at",
            "retry_origin",
        ] {
            if let Some(value) = existing.metadata.get(key) {
                record.metadata[key] = value.clone();
            }
        }
        if execution_runner_id.as_deref().is_some_and(|runner_id| {
            existing
                .runner_id()
                .is_none_or(|existing_runner_id| existing_runner_id == runner_id)
        }) {
            // A foreground daemon binds its job before launching runner-local
            // `run-plan`. Keep that transport identity when run-plan replaces
            // the staged record, or terminal projection cannot join its daemon
            // snapshot back to the completed agent-task run.
            if let Some(runner_job_id) = existing.runner_job_id() {
                record.metadata["runner_job_id"] = json!(runner_job_id);
            }
            // The runner can re-submit after its workspace has been reaped,
            // including before the handoff acceptance projection is durable.
            // Its local pin is execution evidence only; continuations remain
            // owned by the controller seat that created this record. Fail closed
            // if that controller pin is unavailable rather than replacing it
            // with the runner's host-local executable.
            preserved_controller_runtime = Some(controller_runtime_for_runner_execution(
                &existing,
                execution_runner_id.as_deref(),
            )?);
            if existing.lab_handoff.as_ref().is_some_and(|handoff| {
                handoff.state == AgentTaskLabHandoffState::Accepted
                    && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
            }) {
                record.lab_handoff = existing.lab_handoff;
            }
        }
        // A replay of the same run retains its fenced ownership. A different
        // claim is never silently substituted after ownership was established.
        if existing.workspace_identity == record.workspace_identity
            && existing.workspace_lifecycle_revision == record.workspace_lifecycle_revision
        {
            record.workspace_owner_lease = existing.workspace_owner_lease;
        }
    }
    require_record_workspace_owner_in_store(&workspace_claim_store, &record)?;
    lifecycle_store.write_record(&record)?;

    // The queue is durable independently of this foreground controller. Status
    // and cancellation can therefore resolve a waiter after a restart.
    if let Some(admission) = admission_status.and_then(|project| project(&run_id)) {
        record.metadata["controller_admission"] = admission;
        lifecycle_store.write_record(&record)?;
    }

    match admit_runtime(&run_id) {
        Ok(admission) => {
            // The admission claim checks this state under the queue lock. Read
            // it once more before recording runtime provenance or dispatching
            // any provider work in case cancellation won immediately after.
            if let Ok(cancelled) = lifecycle_store.read_record(&run_id) {
                if cancelled.state.is_terminal() {
                    return Ok(cancelled);
                }
            }
            if let Some(controller_runtime) = preserved_controller_runtime {
                // The runner executes the portable plan, but the durable run
                // remains owned by the controller that admitted the handoff.
                // Keep their immutable pins distinct across host boundaries.
                record.metadata
                    [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                    controller_runtime;
                record.metadata["runner_execution_runtime"] = admission.runtime();
            } else {
                record.metadata
                    [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                    admission.runtime();
            }
            lifecycle_store.write_record(&record)?;
        }
        Err(error) => {
            // Cancellation is persisted before removing a queue entry. Do not
            // overwrite that terminal lifecycle state with a synthetic
            // pre-execution admission failure when the waiter wakes up.
            if let Ok(cancelled) = lifecycle_store.read_record(&run_id) {
                if cancelled.state == AgentTaskRunState::Cancelled
                    || cancelled.metadata["controller_admission_cancellation_requested"] == true
                {
                    return Ok(cancelled);
                }
            }
            record_pre_execution_failure_in_store(
                lifecycle_store,
                &run_id,
                plan,
                "controller_admission",
                &error,
            )?;
            return Err(error);
        }
    }
    Ok(record)
}

pub(crate) fn project_runner_execution_context(
    metadata: &mut Value,
    execution_context: &homeboy_core::runner_job_execution_context::RunnerJobExecutionContext,
) -> Result<()> {
    execution_context.verify_integrity()?;
    metadata["runner_job_id"] = json!(execution_context.runner_job_id());
    metadata["runner_execution_context"] = execution_context.evidence_record()?;
    Ok(())
}

// The ambient `persist_controller_plan()` shim that used to sit above this is
// gone. Its last caller was the Cook retry boundary, which now persists the
// controller plan into the same store it reserved the successor in (#7505).

pub(crate) fn persist_controller_plan_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<()> {
    lifecycle_store
        .write_controller_plan(run_id, plan)
        .map(|_| ())
}

pub(crate) fn controller_runtime_for_runner_execution(
    existing: &AgentTaskRunRecord,
    execution_runner_id: Option<&str>,
) -> Result<Value> {
    if existing
        .runner_id()
        .is_some_and(|runner_id| Some(runner_id) != execution_runner_id)
    {
        return Err(Error::internal_unexpected(
            "runner execution identity does not match the controller-owned Lab handoff",
        ));
    }
    existing
        .metadata
        .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "accepted Lab handoff has no controller runtime pin",
                Some(existing.run_id.clone()),
                None,
            )
        })
}

pub(crate) fn execution_runner_id() -> Option<String> {
    std::env::var(homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV)
        .ok()
        .filter(|runner_id| !runner_id.trim().is_empty())
}

/// Bind an inherited route when a detached workload recreates an agent-task run.
pub fn persist_notification_route(
    run_id: &str,
    route: &homeboy_core::notification_route::NotificationRoute,
) -> Result<()> {
    let mut record = store::read_record(run_id)?;
    route.insert_into_metadata(&mut record.metadata);
    store::write_record(&record)
}

/// Read back the route a run was launched with.
///
/// The route a caller supplied is bound to the launching *thread*
/// (`notification_route::current`), which a process that did not launch the
/// cook — `cook-continue`, controller adoption, claimed continuation — never
/// inherits. It is also persisted on the durable record by
/// [`persist_notification_route`], and that copy survives the process. This is
/// the same durable read the daemon completion backstop and `runs watch`
/// already perform (#11115).
///
/// A missing record, a record without a route, or a malformed route is `None`:
/// notification routing is observability and must never fail a cook.
pub fn durable_notification_route(
    run_id: &str,
) -> Option<homeboy_core::notification_route::NotificationRoute> {
    if run_id.trim().is_empty() {
        return None;
    }
    let record = store::read_record(&resolve_run_id(run_id).ok()?).ok()?;
    homeboy_core::notification_route::NotificationRoute::from_metadata(&record.metadata)
}

/// Claim the single terminal notification a Cook is allowed to deliver.
///
/// Returns `Ok(true)` for the caller that won the claim and `Ok(false)` for
/// every later one, mirroring `ObservationStore::mark_notification_delivered`
/// — same `{at, by}` marker, same only-if-absent semantics, same
/// "the winner dispatches" contract. It differs only in its key: that marker
/// is a column on one `runs` row, and a Cook spans many runs
/// (`{cook_id}-attempt-{n}-{suffix}`), so a per-run marker cannot dedupe a
/// per-cook event. Durable route rehydration (#11115) lets a second process
/// reach the same cook's terminal boundary, so the cook needs its own claim.
pub fn claim_cook_terminal_notification(cook_id: &str, delivered_by: &str) -> Result<bool> {
    if cook_id.trim().is_empty() {
        return Ok(false);
    }
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    claim_cook_terminal_notification_in_store(&lifecycle_store, cook_id, delivered_by)
}

/// Claim a Cook's single terminal notification within an explicitly rooted
/// store. The `O_EXCL` marker is created beside that store's own Cook index, so
/// two stores are two independent claims and neither consumes the other's
/// exactly-once eligibility.
///
/// The empty-id guard is repeated here rather than delegated to the ambient
/// entry point. A blank Cook id is not an identity: `sanitize_run_id` turns
/// `""` into a freshly minted `agent-task-<uuid>` and `"   "` into `___`,
/// either of which would win a durable claim keyed to a Cook nobody can name.
/// The ambient shim keeps its own copy so a broken environment still answers
/// `Ok(false)` without constructing a store, exactly as it does today.
pub fn claim_cook_terminal_notification_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    delivered_by: &str,
) -> Result<bool> {
    if cook_id.trim().is_empty() {
        return Ok(false);
    }
    lifecycle_store.claim_cook_notification(
        cook_id,
        &json!({
            "at": now_timestamp(),
            "by": delivered_by,
        }),
    )
}

// The ambient `confirm_cook_terminal_notification()` shim that used to sit
// above this is gone. Its only callers were the three terminal-notification
// entry points in `agent_task_notify`, which now resolve one lifecycle store
// for the whole claim/confirm/release protocol and pass it to each rooted
// sibling, rather than resolving a fresh root per step (#7505).

/// Persist a confirmed terminal delivery beside the injected store's own Cook
/// index. Like the claim it commits, this is a bare filesystem write with no
/// record read in front of it, so an ambient reach here would consume the wrong
/// root's eligibility without ever failing.
///
/// No empty-id guard is added: the ambient function has never had one, and a
/// sibling that refused an id its pair accepts would not be the same operation.
pub fn confirm_cook_terminal_notification_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    delivered_by: &str,
) -> Result<()> {
    lifecycle_store.confirm_cook_notification(
        cook_id,
        &json!({
            "at": now_timestamp(),
            "by": delivered_by,
            "state": "delivered",
        }),
    )
}

/// Allow a later terminal observer to retry a notification that did not reach
/// its transport. Notification delivery remains non-fatal to Cook execution.
pub fn release_cook_terminal_notification_claim(cook_id: &str) -> Result<()> {
    store::release_cook_notification_claim(cook_id)
}

/// Release a provisional terminal-notification claim inside an explicitly
/// rooted store.
///
/// This is the third step of the same exactly-once protocol as
/// [`claim_cook_terminal_notification_in_store`] and
/// [`confirm_cook_terminal_notification_in_store`], and it is the step that
/// previously had no rooted form at all: the ambient shim above reaches
/// `store::`, which resolves a root of its own. A claim taken in an injected
/// store and released against the ambient one leaves the real claim standing
/// until its lease expires, which is the duplicate-notification outcome the
/// claim exists to prevent.
pub fn release_cook_terminal_notification_claim_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<()> {
    lifecycle_store.release_cook_notification_claim(cook_id)
}

// The ambient `record_cook_terminal_notification_outcome()` shim that used to
// sit above this is gone, for the same reason as the confirm shim: its only
// callers were the `agent_task_notify` entry points, which now record the
// outcome in the store they claimed against (#7505).

/// Store the latest compact notification delivery outcome beside the injected
/// store's own Cook index. The marker is a file next to `index.json`, not an
/// observation row, so it follows this store's data root rather than the
/// ambient one.
pub fn record_cook_terminal_notification_outcome_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    outcome: Value,
) -> Result<()> {
    lifecycle_store.write_cook_notification_outcome(cook_id, &outcome)
}

/// Read the latest terminal notification outcome without loading Cook attempts.
pub fn cook_terminal_notification_outcome(cook_id: &str) -> Result<Option<Value>> {
    store::read_cook_notification_outcome(cook_id)
}

pub fn record_completed_run(
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_completed_run_in_store(&lifecycle_store, plan, aggregate, requested_run_id)
}

/// Submit and immediately terminalize one run inside an explicitly rooted
/// store.
///
/// Both halves follow the injected root. Splitting them is the failure this
/// sibling exists to make impossible: a submission in one home followed by an
/// aggregate write in another leaves a queued record that never completes and a
/// terminal projection with no run behind it, and neither write ever fails.
///
/// The controller-runtime admission that `submit_plan_in_store` performs
/// follows this store's root as well (#12859, #12862). The pin itself is
/// content-addressed and shared, but the admission queue and its cross-process
/// lock are per-installation: resolving them from the ambient home made every
/// rooted caller serialize on one machine-global lock.
pub fn record_completed_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let mut record = submit_plan_in_store(lifecycle_store, plan, requested_run_id)?;
    record_aggregate_in_store(lifecycle_store, &mut record, plan, aggregate)
}

pub fn load_plan(run_id: &str) -> Result<AgentTaskPlan> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    load_plan_in_store(&lifecycle_store, run_id)
}

/// [`load_plan`] against explicitly injected durable lifecycle roots.
pub fn load_plan_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskPlan> {
    let run_id = resolve_run_id_in_store(lifecycle_store, run_id)?;
    lifecycle_store.read_controller_plan(&run_id)
}

/// Load the plan owned by this controller's durable run identity. Runner paths
/// projected into lifecycle metadata are transport evidence, not retry input.
pub fn load_controller_plan(run_id: &str) -> Result<AgentTaskPlan> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    load_controller_plan_in_store(&lifecycle_store, run_id)
}

/// Load the controller-owned plan from an explicitly rooted store. Both halves
/// follow the injected root: the Cook alias is resolved against that store's
/// own index, and the plan is read from that store's own run directory.
pub fn load_controller_plan_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskPlan> {
    let run_id = resolve_run_id_in_store(lifecycle_store, run_id)?;
    lifecycle_store.read_controller_plan(&run_id)
}

/// Load a durable plan for a scheduler or provider execution. This is the only
/// read path allowed to upgrade a legacy execution-budget envelope.
pub fn load_plan_for_execution(run_id: &str) -> Result<AgentTaskPlan> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    load_plan_for_execution_in_store(&lifecycle_store, run_id)
}

/// [`load_plan_for_execution`] against explicitly injected durable lifecycle
/// roots.
///
/// Both halves follow the injected root, and the second one is a write: the
/// legacy execution-budget upgrade rewrites `plan.json` under this store's
/// config lock. Resolving the Cook alias against one home's index and
/// migrating another home's plan file would rewrite a plan this caller never
/// read (#7505).
pub fn load_plan_for_execution_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskPlan> {
    let run_id = resolve_run_id_in_store(lifecycle_store, run_id)?;
    lifecycle_store.read_controller_plan_for_execution(&run_id)
}

/// Validate a queued lifecycle's pinned controller without scheduling provider work.
pub fn validate_controller_runtime(run_id: &str) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    validate_controller_runtime_in_store(&lifecycle_store, run_id)
}

/// Validate a queued lifecycle's pinned controller against an explicitly rooted
/// store.
///
/// The record read and the legacy-pin migration write both follow
/// `lifecycle_store`. The immutable controller-runtime pin store that
/// `controller_runtime::validate` consults is deliberately left process-global:
/// it is a content-addressed executable cache shared across homes, not durable
/// lifecycle state, so it is not one of this store's roots.
pub fn validate_controller_runtime_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    migrate_record_controller_runtime_in_store(lifecycle_store, &mut record)?;
    homeboy_core::controller_runtime::validate(
        record
            .metadata
            .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "controller_runtime",
                    "durable run has no controller runtime pin",
                    Some(record.run_id.clone()),
                    None,
                )
            })?,
    )?;
    Ok(record)
}

/// Resolve the compatible immutable executable for a lifecycle mutation.
/// Legacy pins are migrated atomically before returning a path for re-exec.
pub fn pinned_runtime_for_mutation(run_id: &str) -> Result<Option<std::path::PathBuf>> {
    // One root for the whole read: resolving the run id, reading the record,
    // and migrating its pin are one operation. Three separately resolved homes
    // migrate a pin in a record the caller cannot read back (#7505).
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    let resolved = resolve_run_id_in_store(&lifecycle_store, run_id)?;
    let mut record = lifecycle_store.read_record(&resolved)?;
    migrate_record_controller_runtime_in_store(&lifecycle_store, &mut record)?;
    homeboy_core::controller_runtime::pinned_executable_for_mutation(
        &record.metadata,
        &homeboy_core::build_identity::current().display,
    )
    .map_err(|mut error| {
        // A bad pin must never become a recursive re-exec suggestion. Name the
        // narrow repair operation against the durable record instead.
        error.details["next_actions"] = serde_json::json!([format!(
            "homeboy agent-task runtime-recover {} --artifact <trusted-controller-executable>",
            homeboy_core::engine::shell::quote_arg(&record.run_id)
        )]);
        error
    })
}

/// A v2 controller-runtime pin that belongs to a persisted runner transport,
/// rather than the controller filesystem currently handling a continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerPinnedRuntime {
    pub runner_id: String,
    pub executable: std::path::PathBuf,
}

/// Resolve a runner-owned v2 pin before attempting local migration or
/// validation. Historical Lab records stored the runner's immutable path as the
/// controller pin; re-executing it through the recorded runner lets that host
/// retain the normal digest and identity checks.
pub fn runner_pinned_runtime_for_mutation(run_id: &str) -> Result<Option<RunnerPinnedRuntime>> {
    let record = store::read_record(&resolve_run_id(run_id)?)?;
    let Some(runner_id) = record
        .runner_id()
        .filter(|runner_id| !runner_id.trim().is_empty())
    else {
        return Ok(None);
    };
    let Some(runtime) = record
        .metadata
        .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
    else {
        return Ok(None);
    };
    // Only v2 metadata carries the immutable digest that the runner validates.
    if runtime
        .pointer("/originating/sha256")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Ok(None);
    }
    let Some(executable) = runtime
        .pointer("/originating/pinned_executable")
        .and_then(Value::as_str)
        .filter(|executable| !executable.trim().is_empty())
        .map(std::path::PathBuf::from)
    else {
        return Ok(None);
    };
    // Existing local pins retain the established migration and validation path.
    // A missing v2 path on a runner-backed record is runner-owned provenance.
    if executable.is_file() {
        return Ok(None);
    }
    Ok(Some(RunnerPinnedRuntime {
        runner_id: runner_id.to_string(),
        executable,
    }))
}

/// Seal the currently executing controller into an immutable runtime before a
/// new cook begins its local routing and admission work.  Participates in the
/// FIFO admission queue so concurrent seals wait their turn instead of
/// fast-failing.
pub fn pin_current_controller_runtime(
    request_id: &str,
    cancellation_requested: impl Fn() -> Result<bool>,
) -> Result<std::path::PathBuf> {
    let runtime =
        homeboy_core::controller_runtime::pin_current_queued(request_id, cancellation_requested)?;
    runtime
        .pointer("/originating/pinned_executable")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "new controller runtime pin has no immutable executable",
                None,
                None,
            )
        })
}

/// Prune immutable controller pins through the durable lifecycle ownership
/// boundary so nonterminal records remain authoritative retention roots.
///
/// Retention policy is resolved by core from the operator's configuration; this
/// boundary only forwards the overrides an operator typed.
pub fn prune_controller_runtime_pins(
    apply: bool,
    overrides: homeboy_core::controller_runtime::ControllerRuntimeRetentionOverrides,
) -> Result<homeboy_core::controller_runtime::ControllerRuntimePruneResult> {
    homeboy_core::controller_runtime::prune_pins(apply, overrides)
}

// The ambient `migrate_record_controller_runtime()` shim that used to sit here
// is gone. `pinned_runtime_for_mutation` was its only caller and now migrates
// inside the store it read the record from (#7505).

fn migrate_record_controller_runtime_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
) -> Result<()> {
    let Some(runtime) = record
        .metadata
        .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
    else {
        return Ok(());
    };
    let original = runtime.clone();
    let runtime_root =
        homeboy_core::controller_runtime::runtime_root_in(lifecycle_store.roots().data())?;
    let migrated = homeboy_core::controller_runtime::migrate_legacy_pin_and_persist_in_root(
        &runtime_root,
        &original,
        |migrated| {
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                migrated.clone();
            lifecycle_store.write_record(record)
        },
    )?;
    record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = migrated;
    Ok(())
}

/// Repair only the executable artifact named by durable controller provenance.
pub fn recover_controller_runtime(
    run_id: &str,
    artifact: Option<&std::path::Path>,
    source: Option<&std::path::Path>,
) -> Result<Value> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    recover_controller_runtime_in_store(&lifecycle_store, run_id, artifact, source)
}

/// Repair a run's pinned controller executable against an explicitly rooted
/// store.
///
/// This is the re-entry repair step: a run whose pin no longer resolves cannot
/// be resumed or retried until the provenance it names is restored. Both
/// lifecycle halves follow the injected root — the record the provenance is read
/// from and the record the recovered pin is written back to — because recovering
/// against one home's provenance and persisting into another leaves the run this
/// operator is trying to re-enter still holding the broken pin.
///
/// The immutable controller-runtime store that `recover_pin_and_persist`
/// republishes into is deliberately left process-global, for the same reason
/// `validate_controller_runtime_in_store` leaves it alone: it is a
/// content-addressed executable cache shared across homes, not durable lifecycle
/// state, so it is not one of this store's roots.
pub fn recover_controller_runtime_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    artifact: Option<&std::path::Path>,
    source: Option<&std::path::Path>,
) -> Result<Value> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    let runtime = record
        .metadata
        .get(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "durable run has no controller runtime pin",
                Some(record.run_id.clone()),
                None,
            )
        })?;
    let runtime = runtime.clone();
    let recovered = homeboy_core::controller_runtime::recover_pin_and_persist(
        &runtime,
        artifact,
        source,
        |recovered| {
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] =
                recovered.clone();
            lifecycle_store.write_record(&record)
        },
    )?;
    Ok(recovered)
}

pub fn mark_running(run_id: &str) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    mark_running_in_store(&lifecycle_store, run_id)
}

pub fn mark_running_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let mut record = lifecycle_store.read_record(&run_id)?;
    migrate_record_controller_runtime_in_store(lifecycle_store, &mut record)?;
    homeboy_core::controller_runtime::validate_for_mutation(
        &record.metadata,
        &homeboy_core::build_identity::current().display,
    )?;
    let mut error = None;
    lifecycle_store.mutate_record(&run_id, |record| {
        if record.metadata.get("queue_quarantine").is_some() {
            error = Some(Error::validation_invalid_argument(
                "run_id",
                "agent-task run is quarantined; re-arm its exact run id after repairing durable provenance",
                Some(record.run_id.clone()),
                None,
            ));
            return false;
        }
        if record.state == AgentTaskRunState::Running && record.owner_process_is_running() {
            error = Some(Error::validation_invalid_argument(
                "run_id",
                format!(
                    "agent-task run '{}' is already running under pid {}",
                    record.run_id,
                    record.owner_pid().unwrap_or_default()
                ),
                Some(record.run_id.clone()),
                None,
            ));
            return false;
        }
        if record.state.is_terminal() {
            error = Some(Error::validation_invalid_argument(
                "run_id",
                format!(
                    "agent-task run '{}' is already terminal with state {:?}",
                    record.run_id, record.state
                ),
                Some(record.run_id.clone()),
                None,
            ));
            return false;
        }
        let reclaimed_stale = record.state == AgentTaskRunState::Running;
        record.updated_at = Some(now_timestamp());
        set_run_state(record, AgentTaskRunState::Running);
        update_lifecycle_heartbeat(record);
        for task in &mut record.tasks {
            if task.state == AgentTaskState::Queued {
                task.state = AgentTaskState::Running;
            }
        }
        record.record_runner_metadata(reclaimed_stale);
        true
    })?
    .ok_or_else(|| error.unwrap_or_else(|| Error::internal_unexpected("agent-task run transition was not applied")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderExecutionReservation {
    Acquired,
    AlreadyReserved,
}

/// Durably reserve one provider execution before the scheduler blocks on the
/// backend. A resumed controller must reconcile an existing reservation rather
/// than dispatching the same `(task_id, attempt)` a second time.
pub fn reserve_provider_execution(
    run_id: &str,
    task: &AgentTaskRequest,
    attempt: u32,
) -> Result<ProviderExecutionReservation> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    reserve_provider_execution_in_store(&lifecycle_store, run_id, task, attempt)
}

pub fn reserve_provider_execution_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    task: &AgentTaskRequest,
    attempt: u32,
) -> Result<ProviderExecutionReservation> {
    let run_id = sanitize_run_id(run_id);
    require_record_workspace_owner_in_store(
        &lifecycle_store.workspace_claim_store(),
        &lifecycle_store.read_record(&run_id)?,
    )?;
    let execution_key = format!("{}:{attempt}", task.task_id);
    let mut reservation = ProviderExecutionReservation::AlreadyReserved;
    lifecycle_store.mutate_record(&run_id, |record| {
        let started_at = now_timestamp();
        let consumed = {
            let metadata = record.ensure_metadata_object();
            let executions = metadata
                .entry("provider_executions".to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .expect("provider_executions must be an array");
            if executions
                .iter()
                .any(|execution| execution["key"] == execution_key)
            {
                return false;
            }
            executions.push(json!({
                "key": execution_key,
                "task_id": task.task_id,
                "attempt": attempt,
                "backend": task.executor.backend,
                "model": task.executor.model(),
                "state": "running",
                "started_at": started_at.clone(),
                // This is the process that owns the synchronous local provider
                // boundary, captured before the scheduler can block on it.
                "owner_pid": std::process::id(),
                "owner_linux_starttime_ticks": homeboy_core::process::linux_process_starttime_ticks(std::process::id()).ok().flatten(),
                "owner_identity": format!("{run_id}:{execution_key}"),
            }));
            let consumed = executions.len();
            metadata.insert("provider_executions_consumed".to_string(), json!(consumed));
            consumed
        };
        let _ = consumed;
        // Advance the heartbeat to provider-execution start for a local
        // (in-process) cook. Before this, a local cook left the heartbeat frozen
        // at submission time for the entire provider run, so operators could not
        // distinguish active execution from a hung preflight (#8396). Restrict
        // this to non-runner-backed runs: a runner-backed run's owner PID and
        // heartbeat are owned by the runner, not the controller reserving here.
        if !record.is_runner_backed() {
            record.updated_at = Some(started_at);
            update_lifecycle_heartbeat(record);
        }
        reservation = ProviderExecutionReservation::Acquired;
        true
    })?;
    Ok(reservation)
}

/// Bind a reserved provider execution to the subprocess that actually runs it.
///
/// Fanout workers are threads and therefore share the coordinator PID. The
/// provider subprocess is the first process identity unique to one child run;
/// using it keeps liveness and activity evidence from crossing child records.
pub fn record_provider_execution_process(
    run_id: &str,
    task_id: &str,
    attempt: u32,
    pid: u32,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_provider_execution_process_in_store(&lifecycle_store, run_id, task_id, attempt, pid)
}

/// Bind a reserved provider execution to its subprocess inside an explicitly
/// rooted store.
///
/// The reservation this binding looks for was made in one store, so the lookup
/// has to happen there too. An ambient reach would either find no reservation
/// and reject a legitimate process, or bind this run's provider PID onto
/// another home's identically-keyed execution — and process liveness read back
/// from the wrong record is exactly the evidence this write exists to keep
/// separate across child runs.
pub fn record_provider_execution_process_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    task_id: &str,
    attempt: u32,
    pid: u32,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let key = format!("{task_id}:{attempt}");
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let Some(execution) = record.metadata["provider_executions"]
            .as_array_mut()
            .and_then(|executions| {
                executions
                    .iter_mut()
                    .find(|execution| execution["key"] == key)
            })
        else {
            return false;
        };
        execution["owner_pid"] = json!(pid);
        execution["owner_linux_starttime_ticks"] =
            json!(homeboy_core::process::linux_process_starttime_ticks(pid)
                .ok()
                .flatten());
        true
    })?;
    record.ok_or_else(|| {
        Error::validation_invalid_argument(
            "provider_execution",
            "cannot bind a process to an unreserved provider execution",
            Some(key),
            None,
        )
    })
}

// The ambient `running_owner_pid()` shim that used to sit above this is gone.
// Its last caller was the Cook heartbeat, which samples activity on a thread
// that already held a borrow of the injected lifecycle store for its
// supervision writes — so it now reads the owner PID from the same installation
// it records the sample into (#7505).

/// Return the unambiguous running provider PID for activity sampling, from an
/// explicitly injected durable lifecycle root.
pub fn running_owner_pid_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<u32>> {
    Ok(lifecycle_store
        .read_record(&sanitize_run_id(run_id))?
        .owner_pid())
}

/// Persist the controller-owned Cook phase into an explicitly rooted store.
pub fn record_cook_progress_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    phase: &str,
    attempt: u32,
    detail: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    record_cook_progress_with_activity_in_store(
        lifecycle_store,
        run_id,
        phase,
        attempt,
        detail,
        None,
    )
}

/// Retain a redacted, bounded controller failure independently of continuation
/// claim transitions. Claims describe ownership; they must not replace cause.
pub fn record_cook_controller_failure(
    run_id: &str,
    diagnostic: &Value,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_cook_controller_failure_in_store(&lifecycle_store, run_id, diagnostic)
}

pub fn record_cook_controller_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    diagnostic: &Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let diagnostic = diagnostic.clone();
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        record
            .ensure_metadata_object()
            .insert("cook_controller_failure".to_string(), diagnostic);
        true
    })?;
    record.ok_or_else(|| Error::internal_unexpected("Cook controller failure record was unchanged"))
}

// The ambient `clear_cook_controller_failure()` shim that used to sit above
// this resolved a root and delegated straight here. It had no callers, so it
// was a resolution point that existed for nobody (#7505).

/// Clear a durable controller failure inside an explicitly rooted store.
///
/// A successful explicit rearm starts a new continuation pass. Its prior
/// controller failure remains in the failed continuation artifact, but must not
/// be presented as the cause of a later terminal promotion or finalization.
///
/// This is the erasing half of [`record_cook_controller_failure_in_store`] and
/// has to follow the same root. An ambient reach would leave the injected
/// store's failure standing — so a rearmed continuation would still be
/// presented as the cause of a later promotion — while silently erasing a
/// failure in whatever home the environment pointed at.
pub fn clear_cook_controller_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<()> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        record
            .ensure_metadata_object()
            .remove("cook_controller_failure")
            .is_some()
    })?;
    let _ = record;
    Ok(())
}

pub fn record_cook_progress_with_activity_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    phase: &str,
    attempt: u32,
    detail: Option<&str>,
    activity: Option<Value>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let now = now_timestamp();
        {
            let metadata = record.ensure_metadata_object();
            let previous = metadata.get("cook_progress").cloned();
            let mut progress = json!({
                "phase": phase,
                "attempt": attempt,
                "detail": detail,
                "updated_at": now,
            });
            match activity {
                Some(activity) => {
                    progress["activity"] = activity;
                    progress["activity_observed_at"] = json!(now);
                }
                // A probe that could not read the worktree or the process table
                // says nothing about the provider. Carry the last real sample
                // forward with its own observation time so a reader can see both
                // what was last seen and how stale it is.
                None => {
                    if let Some(previous) = previous.as_ref() {
                        if let Some(retained) = previous
                            .get("activity")
                            .filter(|activity| !activity.is_null())
                        {
                            progress["activity"] = retained.clone();
                            progress["activity_observed_at"] = previous
                                .get("activity_observed_at")
                                .cloned()
                                .unwrap_or(Value::Null);
                        }
                    }
                }
            }
            metadata.insert("cook_progress".to_string(), progress);
        }
        if !record.state.is_terminal() && !record.is_runner_backed() {
            record.updated_at = Some(now_timestamp());
            update_lifecycle_heartbeat(record);
        }
        true
    })?;
    record.ok_or_else(|| Error::internal_unexpected("Cook progress record was unchanged"))
}

pub fn record_cook_observer_event_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    phase: &str,
    diagnostic: Value,
) -> Result<AgentTaskRunRecord> {
    const MAX_EVENTS: usize = 16;

    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let events = metadata
            .entry("cook_observer_events".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let events = events
            .as_array_mut()
            .expect("cook observer events are an array");
        events.push(json!({
            "kind": "delivery_failed",
            "phase": phase,
            "at": now_timestamp(),
            "diagnostic": diagnostic,
        }));
        if events.len() > MAX_EVENTS {
            events.drain(..events.len() - MAX_EVENTS);
        }
        true
    })?;
    record.ok_or_else(|| Error::internal_unexpected("Cook observer event record was unchanged"))
}

/// Entries retained in the durable resource timeline.
///
/// At the fifteen-second heartbeat interval this is a rolling hour. A timeline
/// is only useful if it survives the run that produced it, and an unbounded one
/// would not: a long cook would grow its run record without limit, and the
/// record is rewritten on every heartbeat.
const MAX_COOK_RESOURCE_SAMPLES: usize = 240;

/// Supervision events retained. Decisions are announced on escalation only, so
/// a run reaches a handful of these, not hundreds.
const MAX_COOK_SUPERVISION_EVENTS: usize = 32;

pub fn record_cook_supervision_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    attempt: u32,
    sample: Option<Value>,
    decisions: Vec<Value>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let now = now_timestamp();
        let metadata = record.ensure_metadata_object();
        if let Some(sample) = sample {
            let timeline = metadata
                .entry("cook_resource_timeline".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let timeline = timeline
                .as_array_mut()
                .expect("cook resource timeline is an array");
            timeline.push(json!({
                "at": now,
                "attempt": attempt,
                "sample": sample,
            }));
            if timeline.len() > MAX_COOK_RESOURCE_SAMPLES {
                timeline.drain(..timeline.len() - MAX_COOK_RESOURCE_SAMPLES);
            }
        }
        if !decisions.is_empty() {
            let events = metadata
                .entry("cook_supervision_events".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            let events = events
                .as_array_mut()
                .expect("cook supervision events are an array");
            for decision in &decisions {
                events.push(json!({
                    "kind": "budget_breached",
                    "at": now,
                    "attempt": attempt,
                    "decision": decision,
                }));
            }
            if events.len() > MAX_COOK_SUPERVISION_EVENTS {
                events.drain(..events.len() - MAX_COOK_SUPERVISION_EVENTS);
            }
        }
        true
    })?;
    record.ok_or_else(|| Error::internal_unexpected("Cook supervision record was unchanged"))
}

pub fn record_cook_supervision_stop_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    attempt: u32,
    outcome: Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let events = metadata
            .entry("cook_supervision_events".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let events = events
            .as_array_mut()
            .expect("cook supervision events are an array");
        events.push(json!({
            "kind": "stop_executed",
            "at": now_timestamp(),
            "attempt": attempt,
            "outcome": outcome,
        }));
        if events.len() > MAX_COOK_SUPERVISION_EVENTS {
            events.drain(..events.len() - MAX_COOK_SUPERVISION_EVENTS);
        }
        true
    })?;
    record.ok_or_else(|| Error::internal_unexpected("Cook supervision stop was unchanged"))
}

pub fn record_cook_terminal_result_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    terminal_success: bool,
    exit_code: i32,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let Some(progress) = record
            .ensure_metadata_object()
            .get_mut("cook_progress")
            .and_then(Value::as_object_mut)
        else {
            return false;
        };
        if progress.get("phase").and_then(Value::as_str) != Some("terminal") {
            return false;
        }
        progress.insert("terminal_success".to_string(), json!(terminal_success));
        progress.insert("exit_code".to_string(), json!(exit_code));
        true
    })?;
    record.ok_or_else(|| Error::internal_unexpected("Cook terminal result was unchanged"))
}

/// Record the provider's terminal result before controller-owned patch
/// harvesting. Harvesting can fail or be interrupted independently of the
/// provider execution, so it must not leave this reservation running.
pub fn record_provider_execution_terminal(
    run_id: &str,
    task_id: &str,
    attempt: u32,
    state: &str,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_provider_execution_terminal_in_store(&lifecycle_store, run_id, task_id, attempt, state)
}

pub fn record_provider_execution_terminal_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    task_id: &str,
    attempt: u32,
    state: &str,
) -> Result<AgentTaskRunRecord> {
    if !matches!(
        state,
        "succeeded" | "cancelled" | "timed_out" | "candidate_recoverable" | "failed"
    ) {
        return Err(Error::validation_invalid_argument(
            "state",
            "provider execution terminal state is invalid",
            Some(state.to_string()),
            None,
        ));
    }
    let run_id = sanitize_run_id(run_id);
    let execution_key = format!("{task_id}:{attempt}");
    let mut found = false;
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let cancelled = record.state == AgentTaskRunState::Cancelled;
        let Some(execution) = record
            .ensure_metadata_object()
            .get_mut("provider_executions")
            .and_then(Value::as_array_mut)
            .and_then(|executions| {
                executions
                    .iter_mut()
                    .find(|execution| execution["key"] == execution_key)
            })
        else {
            return false;
        };
        // A confirmed cancellation wins over a provider return that raced with
        // it. Conversely, cancellation observes a terminal provider state
        // before it mutates the run, allowing an already-completed result to be
        // imported instead of being discarded.
        if cancelled {
            found = true;
            return false;
        }
        if execution["state"] != json!("running") {
            found = true;
            return false;
        }
        execution["state"] = json!(state);
        execution["finished_at"] = json!(now_timestamp());
        found = true;
        true
    })?;
    if !found {
        return Err(Error::internal_unexpected(
            "provider execution reached a terminal result without its durable attempt record",
        ));
    }
    // The mutation is intentionally a no-op when cancellation or another
    // terminal writer won the race. That existing record is the durable outcome.
    Ok(record.unwrap_or(lifecycle_store.read_record(&run_id)?))
}

/// Attach stable, bounded provider stream references while the process is still
/// running. Cancellation can win before the scheduler receives an outcome, so
/// these references belong to the durable execution reservation rather than its
/// eventual aggregate.
pub fn record_provider_execution_runtime_evidence_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    task_id: &str,
    attempt: u32,
    stdout_uri: Option<String>,
    stderr_uri: Option<String>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let key = format!("{task_id}:{attempt}");
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let Some(execution) = record.metadata["provider_executions"]
            .as_array_mut()
            .and_then(|executions| {
                executions
                    .iter_mut()
                    .find(|execution| execution["key"] == key)
            })
        else {
            return false;
        };
        execution["runtime_evidence"] = json!({
            "stdout": stdout_uri,
            "stderr": stderr_uri,
            "capture": "bounded_incremental",
        });
        true
    })?;
    record.ok_or_else(|| {
        Error::validation_invalid_argument(
            "provider_execution",
            "cannot attach runtime evidence to an unreserved provider execution",
            Some(key),
            None,
        )
    })
}

pub fn record_provider_execution_runtime_evidence(
    run_id: &str,
    task_id: &str,
    attempt: u32,
    stdout_uri: Option<String>,
    stderr_uri: Option<String>,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_provider_execution_runtime_evidence_in_store(
        &lifecycle_store,
        run_id,
        task_id,
        attempt,
        stdout_uri,
        stderr_uri,
    )
}

// The ambient `has_active_provider_execution()` shim that used to sit here is gone;
// its one scheduler test now asks the store it resolves (#7505).

/// [`has_active_provider_execution`] against explicitly injected durable
/// lifecycle roots.
pub fn has_active_provider_execution_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<bool> {
    let record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    Ok(record.metadata["provider_executions"]
        .as_array()
        .is_some_and(|executions| {
            executions
                .iter()
                .any(|execution| execution["state"] == json!("running"))
        }))
}

/// Record the controller time spent after a provider returned and before its
/// terminal outcome was fully harvested and finalized.
pub fn record_provider_execution_cleanup_elapsed(
    run_id: &str,
    task_id: &str,
    attempt: u32,
    elapsed_ms: u64,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_provider_execution_cleanup_elapsed_in_store(
        &lifecycle_store,
        run_id,
        task_id,
        attempt,
        elapsed_ms,
    )
}

pub fn record_provider_execution_cleanup_elapsed_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    task_id: &str,
    attempt: u32,
    elapsed_ms: u64,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let execution_key = format!("{task_id}:{attempt}");
    let mut found = false;
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let Some(execution) = record
            .ensure_metadata_object()
            .get_mut("provider_executions")
            .and_then(Value::as_array_mut)
            .and_then(|executions| {
                executions
                    .iter_mut()
                    .find(|execution| execution["key"] == execution_key)
            })
        else {
            return false;
        };
        found = true;
        execution["post_provider_cleanup_elapsed_ms"] = json!(elapsed_ms);
        execution["post_provider_cleanup_finished_at"] = json!(now_timestamp());
        true
    })?;
    if !found {
        return Err(Error::internal_unexpected(
            "provider cleanup completed without its durable attempt record",
        ));
    }
    record.ok_or_else(|| Error::internal_unexpected("provider cleanup timing was unchanged"))
}

#[cfg(any(test, feature = "test-support"))]
pub fn rewrite_record_for_test<F>(run_id: &str, rewrite: F) -> Result<AgentTaskRunRecord>
where
    F: FnMut(&mut AgentTaskRunRecord),
{
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, rewrite)
}

/// [`rewrite_record_for_test`] against explicitly injected durable lifecycle
/// roots.
///
/// The read and the write are one read-modify-write, so they cannot be allowed
/// to land in different homes: a rewrite that read the ambient record and
/// committed it into the injected store would overwrite the fixture with a
/// record built from somebody else's state, and every later read from the
/// injected store would still succeed (#7505).
#[cfg(any(test, feature = "test-support"))]
pub fn rewrite_record_for_test_in_store<F>(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    mut rewrite: F,
) -> Result<AgentTaskRunRecord>
where
    F: FnMut(&mut AgentTaskRunRecord),
{
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    rewrite(&mut record);
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

/// Inject raw observation metadata for a corruption-recovery fixture.
///
/// This intentionally bypasses typed record and Lab handoff validation. Normal
/// test rewrites must use `rewrite_record_for_test`.
#[cfg(any(test, feature = "test-support"))]
pub fn inject_raw_record_metadata_for_corruption_test(
    run_id: &str,
    inject: impl FnOnce(&mut Value),
) -> Result<()> {
    store::inject_raw_record_metadata_for_corruption_test(&sanitize_run_id(run_id), inject)
}

/// Reconcile the ownership captured at the local provider boundary. A local
/// provider has no opaque remote handle, so its reserving process is the only
/// durable authority that can prove the reservation is still executing.
fn reconcile_local_provider_ownership(record: &mut AgentTaskRunRecord) -> bool {
    if record.state != AgentTaskRunState::Running || record.is_runner_backed() {
        return false;
    }
    let Some(executions) = record
        .metadata
        .get_mut("provider_executions")
        .and_then(Value::as_array_mut)
    else {
        return false;
    };

    let mut has_reconcilable_execution = false;
    let mut has_live_owner = false;
    let mut has_unverifiable_owner = false;
    let mut has_succeeded = false;
    let mut has_failed = false;
    let mut recovery_identity = Vec::new();
    for execution in executions.iter_mut() {
        match execution["state"].as_str() {
            Some("running") | Some("succeeded") | Some("failed") => {
                has_reconcilable_execution = true;
                has_succeeded |= execution["state"] == json!("succeeded");
                has_failed |= execution["state"] == json!("failed");
                recovery_identity.push(execution["owner_identity"].clone());
                let identity_state = execution
                    .get("owner_pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
                    .map(|pid| {
                        homeboy_core::process::process_identity_state(
                            pid,
                            execution
                                .get("owner_linux_starttime_ticks")
                                .and_then(Value::as_u64),
                        )
                    });
                let live = matches!(
                    identity_state,
                    Some(homeboy_core::process::ProcessIdentityState::Live)
                );
                has_unverifiable_owner |= !matches!(
                    identity_state,
                    Some(
                        homeboy_core::process::ProcessIdentityState::Live
                            | homeboy_core::process::ProcessIdentityState::Dead
                            | homeboy_core::process::ProcessIdentityState::IdentityMismatch
                    )
                );
                execution["owner_state"] = json!(match identity_state {
                    Some(homeboy_core::process::ProcessIdentityState::Live) => "live",
                    Some(homeboy_core::process::ProcessIdentityState::Dead) => "dead",
                    Some(homeboy_core::process::ProcessIdentityState::IdentityMismatch) =>
                        "identity_mismatch",
                    _ => "unverifiable",
                });
                has_live_owner |= live;
            }
            _ => {}
        }
    }
    // Older records predate per-provider ownership, and non-Linux hosts can be
    // unable to verify a persisted identity. Neither alone is proof that the
    // owner died, so retain a joinable run unless its provider already recorded
    // a terminal failure.
    // A terminal provider failure is already conclusive evidence that this
    // execution cannot make progress. It must not retain `Running` solely
    // because the foreground wrapper was interrupted before aggregate harvest.
    if has_live_owner || (has_unverifiable_owner && !has_failed) {
        return true;
    }
    if !has_reconcilable_execution {
        return false;
    }

    let now = now_timestamp();
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "local_provider_ownership".to_string(),
        json!({
            "state": "owner_dead",
            "recovery_identity": recovery_identity,
            "reconciled_at": now,
        }),
    );
    if has_succeeded {
        // The provider reported completion before the foreground owner died,
        // but the aggregate was not yet persisted. Preserve that fact and the
        // workspace as a recoverable candidate instead of erasing it.
        record.updated_at = Some(now);
        set_run_state(record, AgentTaskRunState::CandidateRecoverable);
        for task in &mut record.tasks {
            if task.state == AgentTaskState::Running {
                task.state = AgentTaskState::CandidateRecoverable;
            }
        }
    } else if has_failed {
        record.updated_at = Some(now.clone());
        set_run_state(record, AgentTaskRunState::Failed);
        for task in &mut record.tasks {
            if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
                task.state = AgentTaskState::Failed;
            }
        }
        record.ensure_metadata_object().insert(
            "local_provider_ownership".to_string(),
            json!({
                "state": "provider_failed",
                "recovery_identity": recovery_identity,
                "reconciled_at": now,
            }),
        );
    } else {
        let executions = record
            .ensure_metadata_object()
            .get_mut("provider_executions")
            .and_then(Value::as_array_mut)
            .expect("provider executions were checked above");
        for execution in executions.iter_mut() {
            if execution["state"] == json!("running") {
                execution["state"] = json!("cancelled");
                execution["finished_at"] = json!(now.clone());
            }
        }
        record.updated_at = Some(now.clone());
        set_run_state(record, AgentTaskRunState::Cancelled);
        for task in &mut record.tasks {
            if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
                task.state = AgentTaskState::Cancelled;
            }
        }
        record.ensure_metadata_object().insert(
            "cancel_reason".to_string(),
            json!("local provider owner process is not running"),
        );
    }
    true
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskQueuedRunSkip {
    pub run_id: String,
    pub submitted_at: Option<String>,
    pub age_seconds: Option<i64>,
    pub dispatcher_kind: Option<String>,
    pub category: String,
    pub error_code: String,
    pub summary: String,
    pub provider_id: Option<String>,
    pub required_environment_variables: Vec<String>,
    pub reason: String,
    pub remediation: String,
}

#[derive(Debug, Default)]
pub struct AgentTaskQueuedRunClaim {
    pub record: Option<AgentTaskRunRecord>,
    pub skipped: Vec<AgentTaskQueuedRunSkip>,
    pub inspected: usize,
    pub admission_limit_reached: bool,
}

/// A claim call makes bounded, observable admission progress even when old
/// durable records are malformed. The next invocation resumes after the
/// quarantined records rather than letting stale history monopolize a worker.
pub const MAX_QUEUE_ADMISSION_RECORDS: usize = 64;

/// Claim the oldest executable queued record. Invalid provenance is retained on
/// a nonterminal quarantine record before any running transition can occur.
pub fn claim_next_eligible_queued_run() -> Result<AgentTaskQueuedRunClaim> {
    claim_next_eligible_queued_run_with_preflight(|_, _| Ok(()))
}

/// Claim the oldest executable queued record from an explicitly rooted store.
pub fn claim_next_eligible_queued_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<AgentTaskQueuedRunClaim> {
    claim_next_eligible_queued_run_with_preflight_in_store(lifecycle_store, |_, _| Ok(()))
}

/// Claim the oldest executable queued record after caller-supplied admission
/// checks have validated its durable plan, but before the atomic Running claim.
pub fn claim_next_eligible_queued_run_with_preflight(
    preflight: impl Fn(&AgentTaskRunRecord, &AgentTaskPlan) -> Result<()>,
) -> Result<AgentTaskQueuedRunClaim> {
    claim_next_eligible_queued_run_with_preflight_and_filter(|_| true, preflight)
}

/// Preflighted admission against an explicitly rooted store. The preflight
/// closure is the caller's own, so it is passed through untouched.
pub fn claim_next_eligible_queued_run_with_preflight_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    preflight: impl Fn(&AgentTaskRunRecord, &AgentTaskPlan) -> Result<()>,
) -> Result<AgentTaskQueuedRunClaim> {
    claim_next_eligible_queued_run_with_preflight_and_filter_in_store(
        lifecycle_store,
        |_| true,
        preflight,
    )
}

/// Claim the oldest eligible queued record that matches a caller-owned scope.
/// Records outside the scope are not inspected, quarantined, or allowed to
/// delay admission for targeted work.
pub fn claim_next_eligible_queued_run_with_preflight_and_filter(
    include: impl Fn(&AgentTaskRunRecord) -> bool,
    preflight: impl Fn(&AgentTaskRunRecord, &AgentTaskPlan) -> Result<()>,
) -> Result<AgentTaskQueuedRunClaim> {
    claim_next_eligible_queued_run_with_preflight_and_filter_and_limit(
        include,
        MAX_QUEUE_ADMISSION_RECORDS,
        preflight,
    )
}

/// Scoped, preflighted admission against an explicitly rooted store, using the
/// same default admission budget as the ambient pair.
pub fn claim_next_eligible_queued_run_with_preflight_and_filter_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    include: impl Fn(&AgentTaskRunRecord) -> bool,
    preflight: impl Fn(&AgentTaskRunRecord, &AgentTaskPlan) -> Result<()>,
) -> Result<AgentTaskQueuedRunClaim> {
    claim_next_eligible_queued_run_with_preflight_and_filter_and_limit_in_store(
        lifecycle_store,
        include,
        MAX_QUEUE_ADMISSION_RECORDS,
        preflight,
    )
}

/// Scoped queued admission with an explicit remaining budget shared with other
/// admission phases in the same dispatch invocation.
pub fn claim_next_eligible_queued_run_with_preflight_and_filter_and_limit(
    include: impl Fn(&AgentTaskRunRecord) -> bool,
    limit: usize,
    preflight: impl Fn(&AgentTaskRunRecord, &AgentTaskPlan) -> Result<()>,
) -> Result<AgentTaskQueuedRunClaim> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    claim_next_eligible_queued_run_with_preflight_and_filter_and_limit_in_store(
        &lifecycle_store,
        include,
        limit,
        preflight,
    )
}

/// The whole claim, taken through one explicitly injected store.
///
/// This is the single body of the claim chain; every other `claim_next_*`
/// entry point narrows its arguments and delegates here. Each step is rooted in
/// `lifecycle_store` rather than the process environment:
///
/// * the queue scan reads that store's own observation database, so a claim
///   never inspects one queue and then wins a run in another,
/// * the controller-runtime and durable-plan preflight read that store's own
///   record and run directory,
/// * quarantine of invalid provenance mutates that store's own record, and
/// * the atomic Running transition takes that store's own config lock through
///   [`mark_running_in_store`].
///
/// The `include` and `preflight` closures belong to the caller and are passed
/// through unchanged; neither is given a store it did not already have.
pub fn claim_next_eligible_queued_run_with_preflight_and_filter_and_limit_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    include: impl Fn(&AgentTaskRunRecord) -> bool,
    limit: usize,
    preflight: impl Fn(&AgentTaskRunRecord, &AgentTaskPlan) -> Result<()>,
) -> Result<AgentTaskQueuedRunClaim> {
    let mut queued: Vec<AgentTaskRunRecord> = lifecycle_store
        .read_records()?
        .into_iter()
        .filter(|record| {
            record.state == AgentTaskRunState::Queued
                && !is_transport_proxy(record)
                && record.metadata.get("queue_quarantine").is_none()
                && include(record)
        })
        .collect();
    queued.sort_by(|left, right| {
        left.submitted_at
            .cmp(&right.submitted_at)
            .then_with(|| left.run_id.cmp(&right.run_id))
    });

    let mut skipped = Vec::new();
    let mut inspected = 0;
    for record in queued {
        if inspected == limit {
            return Ok(AgentTaskQueuedRunClaim {
                record: None,
                skipped,
                inspected,
                admission_limit_reached: true,
            });
        }
        inspected += 1;
        let plan = match validate_controller_runtime_in_store(lifecycle_store, &record.run_id)
            .and_then(|_| load_controller_plan_in_store(lifecycle_store, &record.run_id))
        {
            Ok(plan) => plan,
            Err(error) => {
                quarantine_queued_run_in_store(lifecycle_store, &record, None, &error)?;
                skipped.push(queue_skip(&record, None, &error));
                continue;
            }
        };
        if let Err(error) = preflight(&record, &plan) {
            quarantine_queued_run_in_store(lifecycle_store, &record, Some(&plan), &error)?;
            skipped.push(queue_skip(&record, Some(&plan), &error));
            continue;
        }
        match mark_running_in_store(lifecycle_store, &record.run_id) {
            Ok(claimed) => {
                return Ok(AgentTaskQueuedRunClaim {
                    record: Some(claimed),
                    skipped,
                    inspected,
                    admission_limit_reached: false,
                })
            }
            Err(error) if error.code == ErrorCode::ValidationInvalidArgument => {
                quarantine_queued_run_in_store(lifecycle_store, &record, Some(&plan), &error)?;
                skipped.push(queue_skip(&record, Some(&plan), &error));
            }
            Err(error) => return Err(error),
        }
    }

    Ok(AgentTaskQueuedRunClaim {
        record: None,
        skipped,
        inspected,
        admission_limit_reached: false,
    })
}

pub fn claim_next_queued_run() -> Result<Option<AgentTaskRunRecord>> {
    Ok(claim_next_eligible_queued_run()?.record)
}

/// Claim the oldest executable queued record from an explicitly rooted store,
/// discarding the skip diagnostics exactly as the ambient pair does.
pub fn claim_next_queued_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<Option<AgentTaskRunRecord>> {
    Ok(claim_next_eligible_queued_run_in_store(lifecycle_store)?.record)
}

fn queue_skip(
    record: &AgentTaskRunRecord,
    plan: Option<&AgentTaskPlan>,
    error: &Error,
) -> AgentTaskQueuedRunSkip {
    let diagnostic = redacted_queue_diagnostic(plan, error);
    AgentTaskQueuedRunSkip {
        run_id: record.run_id.clone(),
        submitted_at: Some(record.submitted_at.clone()),
        age_seconds: DateTime::parse_from_rfc3339(&record.submitted_at)
            .ok()
            .map(|submitted| {
                (Utc::now() - submitted.with_timezone(&Utc))
                    .num_seconds()
                    .max(0)
            }),
        dispatcher_kind: plan
            .and_then(|plan| plan.metadata.pointer("/attempt_dispatch/kind"))
            .and_then(Value::as_str)
            .and_then(trusted_dispatcher_kind),
        category: diagnostic.category.to_string(),
        error_code: diagnostic.error_code,
        summary: diagnostic.summary.clone(),
        provider_id: diagnostic.provider_id.clone(),
        required_environment_variables: diagnostic.required_environment_variables.clone(),
        reason: diagnostic.summary,
        remediation: queue_quarantine_remediation().to_string(),
    }
}

pub(crate) fn trusted_dispatcher_kind(kind: &str) -> Option<String> {
    matches!(kind, "local" | "lab" | "test-detached").then(|| kind.to_string())
}

fn queue_quarantine_remediation() -> &'static str {
    "inspect retained diagnostics with: homeboy agent-task status <run-id> --exact --full"
}

/// Retain the redacted admission diagnostic on the record inside the store the
/// claim is scanning. A quarantine written into another root would leave the
/// scanned queue permanently re-inspecting the same bad record.
fn quarantine_queued_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    plan: Option<&AgentTaskPlan>,
    error: &Error,
) -> Result<()> {
    let diagnostic = redacted_queue_diagnostic(plan, error);
    let _ = lifecycle_store.mutate_record(&record.run_id, |quarantined| {
        if quarantined.state != AgentTaskRunState::Queued
            || quarantined.metadata.get("queue_quarantine").is_some()
        {
            return false;
        }
        let now = now_timestamp();
        let remediation = queue_quarantine_remediation();
        quarantined.ensure_metadata_object().insert(
            "queue_quarantine".to_string(),
            json!({
                "category": diagnostic.category,
                "error_code": diagnostic.error_code,
                "summary": diagnostic.summary,
                "provider_id": diagnostic.provider_id,
                "required_environment_variables": diagnostic.required_environment_variables,
                "quarantined_at": now,
                "remediation": remediation,
            }),
        );
        true
    })?;
    Ok(())
}

struct RedactedQueueDiagnostic {
    category: &'static str,
    error_code: String,
    summary: String,
    provider_id: Option<String>,
    required_environment_variables: Vec<String>,
}

fn redacted_queue_diagnostic(
    plan: Option<&AgentTaskPlan>,
    error: &Error,
) -> RedactedQueueDiagnostic {
    let provider_id = plan.and_then(trusted_plan_provider_id);
    let required_environment_variables = plan
        .map(trusted_plan_environment_variables)
        .unwrap_or_default();
    let (category, summary) = if required_environment_variables.is_empty() {
        (
            "queue_admission_preflight_failed",
            "queued run failed admission preflight",
        )
    } else {
        (
            "required_environment_preflight_failed",
            "required environment is unavailable for queued execution",
        )
    };
    RedactedQueueDiagnostic {
        category,
        error_code: error.code.as_str().to_string(),
        summary: summary.to_string(),
        provider_id,
        required_environment_variables,
    }
}

fn trusted_plan_provider_id(plan: &AgentTaskPlan) -> Option<String> {
    plan.tasks
        .first()
        .and_then(|task| {
            task.executor
                .selector
                .as_deref()
                .or(Some(&task.executor.backend))
        })
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
        .map(str::to_string)
}

fn trusted_plan_environment_variables(plan: &AgentTaskPlan) -> Vec<String> {
    let mut names = BTreeSet::new();
    for name in plan
        .tasks
        .iter()
        .flat_map(|task| task.executor.secret_env.iter())
        .chain(
            plan.services
                .iter()
                .flat_map(|service| service.secret_env.iter()),
        )
    {
        if name.len() <= 128
            && !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            names.insert(name.clone());
        }
    }
    names.into_iter().take(16).collect()
}

/// Re-arm a quarantined record by exact durable run ID after its provenance is repaired.
pub fn rearm_quarantined_run(run_id: &str) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    rearm_quarantined_run_in_store(&lifecycle_store, run_id)
}

/// Re-arm a quarantined record inside an explicitly rooted store.
///
/// This is the clearing half of the quarantine pair, and it has to read and
/// write the same root the quarantine marker was written into. The eligibility
/// guard — queued, and carrying a `queue_quarantine` marker — is evaluated
/// inside the mutation closure, so an ambient read decides re-armability from
/// another home's copy of the identity: a run quarantined here would be reported
/// as "not quarantined" and refuse to re-arm, while a run quarantined only in
/// the ambient home would report success and leave this store's queue still
/// skipping the record on every claim.
pub fn rearm_quarantined_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = require_literal_run_id(run_id)?;
    lifecycle_store
        .mutate_record(&run_id, |record| {
            if record.state != AgentTaskRunState::Queued
                || record.metadata.get("queue_quarantine").is_none()
            {
                return false;
            }
            record.ensure_metadata_object().remove("queue_quarantine");
            record.updated_at = Some(now_timestamp());
            true
        })?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "only an exact queued quarantined run can be re-armed",
                Some(run_id),
                None,
            )
        })
}

/// Quarantine one exact queued run without changing its lifecycle state or
/// removing any evidence. It can only return through `rearm_quarantined_run`.
pub fn quarantine_queued_run_exact(run_id: &str, reason: &str) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    quarantine_queued_run_exact_in_store(&lifecycle_store, run_id, reason)
}

/// Quarantine one exact queued run inside an explicitly rooted store.
///
/// The operator quarantine and the automatic one that `quarantine_queued_run_in_store`
/// writes during a claim are the same durable marker, and a claim scanning this
/// store must see it. Written ambiently, the operator would be told the run is
/// quarantined while this store's queue kept handing it out, and
/// `rearm_quarantined_run_in_store` would then find nothing to clear.
pub fn quarantine_queued_run_exact_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    reason: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = require_literal_run_id(run_id)?;
    let operator_reason = normalized_operator_quarantine_reason(reason);
    lifecycle_store
        .mutate_record(&run_id, |record| {
            if record.state != AgentTaskRunState::Queued
                || record.metadata.get("queue_quarantine").is_some()
            {
                return false;
            }
            let quarantined_at = now_timestamp();
            let remediation =
                "repair provenance, then re-arm with: homeboy agent-task rearm <run-id>";
            record.updated_at = Some(quarantined_at.clone());
            record.ensure_metadata_object().insert(
                "queue_quarantine".to_string(),
                json!({
                    "category": "operator_quarantine",
                    "error_code": "operator_quarantine",
                    "summary": "operator quarantined this queued run",
                    "operator_reason": operator_reason,
                    "quarantined_at": quarantined_at,
                    "remediation": remediation,
                }),
            );
            true
        })?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "run_id",
                "only an exact queued non-quarantined run can be quarantined",
                Some(run_id),
                None,
            )
        })
}

fn normalized_operator_quarantine_reason(reason: &str) -> String {
    const MAX_BYTES: usize = 240;
    let mut normalized = String::new();
    for character in reason.chars().filter(|character| !character.is_control()) {
        if normalized.len() + character.len_utf8() > MAX_BYTES {
            break;
        }
        normalized.push(character);
    }
    let normalized = normalized.trim();
    if normalized.is_empty() {
        "operator requested quarantine".to_string()
    } else {
        normalized.to_string()
    }
}

fn require_literal_run_id(run_id: &str) -> Result<String> {
    let sanitized = sanitize_run_id(run_id);
    if sanitized != run_id {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "quarantine and re-arm require a literal exact durable run id",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(sanitized)
}

pub fn record_run_aggregate(
    run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_run_aggregate_in_store(&lifecycle_store, run_id, plan, aggregate)
}

pub fn record_run_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    record_aggregate_in_store(lifecycle_store, &mut record, plan, aggregate)
}

/// Reproject terminal artifacts from controller-owned durable state. This is a
/// recovery path for historical runner results whose aggregate was persisted
/// before the controller finalized its artifact-byte projection.
pub fn reconcile_terminal_artifact_projection(run_id: &str) -> Result<bool> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    reconcile_terminal_artifact_projection_in_store(&lifecycle_store, run_id)
}

/// Reproject terminal artifacts inside an explicitly rooted store.
///
/// Every input and every output follows the injected root: the terminal record,
/// the controller-owned plan, the aggregate the byte checks are derived from,
/// and the observation registry the projection is published into. That last one
/// is the reason this sibling exists — `record_terminal_artifact_projection`
/// opens the observation database and resolves the artifact root ambiently, so
/// a reprojection driven from one home's aggregate would have registered its
/// controller-owned bytes under another home's artifact root while every
/// positive assertion still passed. `PathRoots` carries `artifacts` separately
/// from `data`, and `open_observation_initialized` is what binds both to this
/// store.
pub fn reconcile_terminal_artifact_projection_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<bool> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    if !record.state.is_terminal() {
        return Ok(false);
    }

    // Require the controller-owned plan as part of the durable lifecycle
    // contract even though artifact projection derives its byte checks from the
    // aggregate. The runner staging plan is never a recovery input.
    let _plan = lifecycle_store.read_controller_plan(&record.run_id)?;
    let aggregate = lifecycle_store.read_aggregate(&record.run_id)?;
    record_terminal_artifact_projection_in_store(lifecycle_store, &mut record, &aggregate)?;
    update_cook_candidate_after_completion_in_store(lifecycle_store, &record, &aggregate, None)?;
    Ok(true)
}

/// Whether answering for this record can require reaching the runner at all.
///
/// A controller-local run — no runner id, no runner job id, and no Lab handoff —
/// is fully described by durable controller state. Answering for it must never
/// depend on a runner being reachable (#10418): the sharpest symptom of the
/// wedged-Lab outage was `agent-task status <id>` failing to return a *known
/// controller-local* run because the read path unconditionally entered runner
/// reconciliation.
pub fn is_controller_local(record: &AgentTaskRunRecord) -> bool {
    record.runner_id().is_none() && record.runner_job_id().is_none() && record.lab_handoff.is_none()
}

/// Whether a read-side `status()` may reach the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentTaskRunnerProbe {
    /// Reach the runner only when the record is genuinely runner-backed and
    /// still running. This is the historical behavior for Lab runs and the
    /// default.
    #[default]
    WhenRunnerBacked,
    /// Never reach the runner. The answer comes from durable controller state
    /// alone and is labelled as such.
    Never,
}

/// Read-side options for [`status_with_options`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AgentTaskStatusOptions {
    pub runner_probe: AgentTaskRunnerProbe,
}

/// What the read path actually did about the runner, reported alongside the
/// record so a caller can tell a complete answer from a deliberately-local one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AgentTaskRunnerProbePlan {
    /// Whether runner reconciliation was performed for this read.
    pub performed: bool,
    /// Why the runner was not reached, when it was not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<&'static str>,
    /// Whether this record is answerable entirely from controller-local state.
    pub controller_local: bool,
}

/// Bounded runner-owned evidence for a diagnostic read. Unlike status
/// reconciliation, this never changes the durable lifecycle record.
#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskRunnerDiagnosticProbe {
    pub performed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const RUNNER_DIAGNOSTIC_EVENT_LIMIT: usize = 12;

/// Read bounded evidence from the runner that owns an active or abnormal
/// terminal run. Controller-local and healthy successful records remain fully
/// local reads.
pub fn runner_diagnostic_probe(record: &AgentTaskRunRecord) -> AgentTaskRunnerDiagnosticProbe {
    let runner_id = record.runner_id().map(str::to_string);
    let runner_job_id = record.runner_job_id().map(str::to_string);
    let applicable = record.state == AgentTaskRunState::Running
        || (record.state.is_terminal() && record.state != AgentTaskRunState::Succeeded);
    let skipped_reason = if is_controller_local(record) {
        Some(RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL)
    } else if !applicable {
        Some("healthy_terminal_record")
    } else if runner_id.is_none() {
        Some("missing_runner_id")
    } else if runner_job_id.is_none() {
        Some("missing_runner_job_id")
    } else {
        None
    };
    let mut probe = AgentTaskRunnerDiagnosticProbe {
        performed: false,
        skipped_reason,
        runner_id,
        runner_job_id,
        snapshot: None,
        error: None,
    };
    let (Some(runner_id), Some(runner_job_id)) =
        (probe.runner_id.as_deref(), probe.runner_job_id.as_deref())
    else {
        return probe;
    };
    if probe.skipped_reason.is_some() {
        return probe;
    }
    probe.performed = true;
    match super::runner_continuation::with_runner_continuation(|provider| {
        provider.runner_job_log_snapshot(runner_id, runner_job_id)
    }) {
        Ok(snapshot) => {
            let event_count = snapshot.events.len();
            probe.snapshot = Some(json!({
                "job": snapshot.job,
                "events": snapshot.events.into_iter().take(RUNNER_DIAGNOSTIC_EVENT_LIMIT).collect::<Vec<_>>(),
                "events_omitted": event_count.saturating_sub(RUNNER_DIAGNOSTIC_EVENT_LIMIT),
            }));
        }
        Err(error) => probe.error = Some(error.message),
    }
    probe
}

/// The record is fully controller-local; the runner is irrelevant to the answer.
pub const RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL: &str = "controller_local_record";
/// The caller explicitly asked for a local-only answer.
pub const RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT: &str = "caller_opted_out";
/// A terminal / non-running record has no live runner job to reconcile.
pub const RUNNER_PROBE_SKIPPED_NOT_RUNNING: &str = "run_is_not_running";

/// Decide whether this read touches the runner. Pure so the "controller-local
/// answers locally" contract is directly testable.
pub fn runner_probe_plan(
    record: &AgentTaskRunRecord,
    options: AgentTaskStatusOptions,
) -> AgentTaskRunnerProbePlan {
    let controller_local = is_controller_local(record);
    let skipped_reason = if controller_local {
        Some(RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL)
    } else if options.runner_probe == AgentTaskRunnerProbe::Never {
        Some(RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT)
    } else if record.state != AgentTaskRunState::Running {
        Some(RUNNER_PROBE_SKIPPED_NOT_RUNNING)
    } else {
        None
    };
    AgentTaskRunnerProbePlan {
        performed: skipped_reason.is_none(),
        skipped_reason,
        controller_local,
    }
}

/// A durable run record plus the read-side runner-probe decision that produced
/// it.
#[derive(Debug, Clone)]
pub struct AgentTaskStatusOutcome {
    pub record: AgentTaskRunRecord,
    pub runner_probe: AgentTaskRunnerProbePlan,
}

/// Read the durable run record with live reconciliation applied (deferred
/// candidate, runtime admission, and runner/daemon status projection) so callers
/// see the current, joinable controller record.
pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    Ok(status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )?
    .record)
}

/// [`status`] with explicit control over whether the read may reach the runner.
///
/// A controller-local record is always answered locally, regardless of options.
pub fn status_with_options(
    run_id: &str,
    options: AgentTaskStatusOptions,
) -> Result<AgentTaskStatusOutcome> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    status_in_store(&lifecycle_store, run_id, options, false)
}

// The ambient `exact_status()` shim that used to sit here is
// gone; the reconciler was its only caller and now reads exactly through the store it resolved once (#7505).

/// The shared, store-rooted body of [`status`], [`status_with_options`], and
/// [`exact_status`].
///
/// `status` is not a read. It takes two advisory locks and has roughly twenty
/// durable write sites, so every step below has to name the installation the
/// caller injected — a status that decided from one home and committed into
/// another would be silently wrong in exactly the way #7505 exists to stop.
/// The three entry points above are now thin ambient wrappers that resolve
/// `AgentTaskLifecycleStore::from_current_environment()` and delegate here,
/// which is the same store every `store::` shim used to resolve for itself, so
/// their behaviour is unchanged.
///
/// Both advisory locks follow the injected store.
/// `reconcile_deferred_candidate_in_store` and the expiry path's
/// `LabHandoffLock::lock_in_store` (via `expire_unaccepted_lab_handoff_in_store`
/// and `record_detached_lab_run_in_store`) both take their lock on this store's
/// own `run_dir`. A lock resolved from `paths::homeboy_data()` while the
/// mutation it guards followed an injected store would be held where nobody
/// contends for it.
///
/// Two roots that are *not* lifecycle state stay process-global on purpose, and
/// neither is an oversight:
///
/// * `with_runner_continuation` — the runner/broker provider registry, reached
///   through `reconcile_runner_job_state_in_store`. It is configured trust
///   material and a subprocess contract, not durable lifecycle state (#12618).
/// * `homeboy_core::build_identity::current()` — the identity of *this*
///   coordinator process, which is the fact the continuation scheduler is
///   recording.
///
/// # The Cook recipe store is derived, not injected
///
/// The Cook continuation family (`load_recipe`, `load_recipe_for_attempt`,
/// `enqueue_terminal_continuation`) needs a `CookRecipeStore`, which is a
/// different store kind from the lifecycle store this function accepts. That is
/// the cross-kind shape `KNOWN_MIXED_STORE_FUNCTIONS` exists to make someone
/// argue for, so here is the argument.
///
/// This function derives it: `CookRecipeStore::from_data_root(lifecycle_store
/// .data_root())`. It does not take a second store parameter, because a second
/// parameter would be a *hazard*, not a safeguard. `CookRecipeStore` carries
/// exactly one field — a data root — and every path it resolves hangs off it
/// (`<data>/agent-task-cooks`, `<data>/agent-task-cook-continuations`). There is
/// no information in a `CookRecipeStore` that is not already in the lifecycle
/// store's data root, so pairing them can only ever add a way for the two to
/// disagree. A caller that passed a mismatched pair would enqueue a Cook
/// continuation for a run whose lifecycle record lives in another home, and
/// nothing would fail while it happened. Derivation makes that unrepresentable
/// instead of merely discouraged.
///
/// The derivation is exact, not approximate: `CookRecipeStore::from_current_data_root()`
/// resolves `paths::homeboy_data()`, and `AgentTaskLifecycleStore::from_environment()`
/// resolves `PathRoots::from_environment()`, whose `data` is that same
/// `paths::homeboy_data()`. For the ambient wrappers above the derived store is
/// byte-for-byte the store the old code resolved.
///
/// `validate_recipe_attempt_record` is the one member of that family that is
/// *not* recipe-store work: it reads the controller plan, which is lifecycle
/// state. This function calls
/// `validate_recipe_attempt_record_with_controller_plan` with a plan read from
/// the injected lifecycle store, so the recipe half and the plan half cannot
/// come from different homes.
pub fn status_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    options: AgentTaskStatusOptions,
    exact: bool,
) -> Result<AgentTaskStatusOutcome> {
    let recipe_store =
        crate::agent_task_service::CookRecipeStore::from_data_root(lifecycle_store.data_root());
    let requested_run_id = sanitize_run_id(run_id);
    let resolved_run_id = if exact {
        requested_run_id.clone()
    } else {
        resolve_run_id_in_store(lifecycle_store, run_id)?
    };
    let _ = reconcile_deferred_candidate_in_store(lifecycle_store, &resolved_run_id)?;
    let mut record = lifecycle_store.read_record(&resolved_run_id)?;
    // The admission queue is durable lifecycle-adjacent state, so it is read
    // from this store's own controller-runtime root and not from
    // `paths::controller_runtimes_store()`. Reporting this installation's queue
    // position against another installation's owner is the same class of split
    // as writing the record itself into the wrong home. This mirrors the rooted
    // `cancel_admission_at` call in the cancellation spine.
    if let Ok(admission) = homeboy_core::controller_runtime::admission_status_at(
        &lifecycle_store
            .data_root()
            .join(homeboy_core::paths::CONTROLLER_RUNTIMES_STORE),
        &record.run_id,
    ) {
        record.metadata["controller_admission"] = admission;
        lifecycle_store.write_record(&record)?;
    }
    if reconcile_candidate_adoption(&mut record) {
        lifecycle_store.write_record(&record)?;
    }
    if reconcile_pending_runner_submission_intent_in_store(lifecycle_store, &resolved_run_id)? {
        record = lifecycle_store.read_record(&resolved_run_id)?;
    }
    if has_expired_pending_runner_submission_intent(&record, chrono::Utc::now()) {
        let _ = expire_unaccepted_lab_handoff_in_store(lifecycle_store, &resolved_run_id)?;
        record = lifecycle_store.read_record(&resolved_run_id)?;
    }
    // A daemon can evict a completed job from its active store before a restarted
    // controller observes it. The terminal event log already mirrored into this
    // observation record is sufficient to recover the aggregate and artifacts.
    // Consume it before querying the live runner, which is no longer authority
    // once its active entry has been evicted.
    if project_persisted_terminal_runner_events_in_store(lifecycle_store, &mut record)? {
        record = lifecycle_store.read_record(&resolved_run_id)?;
    }
    if super::cancellation::reconcile_controller_job_cancellation_in_store(
        lifecycle_store,
        &mut record,
    )? {
        lifecycle_store.write_record(&record)?;
    }
    if !record.state.is_terminal() {
        let controller_plan = lifecycle_store.read_controller_plan(&record.run_id)?;
        let controller_plan_path = lifecycle_store
            .controller_plan_path(&record.run_id)
            .display()
            .to_string();
        if record.plan_path != controller_plan_path {
            record.plan_path = controller_plan_path;
            lifecycle_store.write_record(&record)?;
        }
        if let Ok(aggregate) = lifecycle_store.read_aggregate(&record.run_id) {
            let aggregate_path = lifecycle_store
                .aggregate_path(&record.run_id)
                .display()
                .to_string();
            let mut reconciled = record.clone();
            let projection_plan = aggregate_projection_plan(&controller_plan, &aggregate);
            apply_aggregate_to_record(
                &mut reconciled,
                &projection_plan,
                &aggregate,
                aggregate_path,
            );

            if reconciled != record {
                if let Err(error) = lifecycle_store.write_record(&reconciled) {
                    reconciled
                        .ensure_metadata_object()
                        .insert("finalization_error".to_string(), json!(error.message));
                }

                record = reconciled;
            }
        }
    }
    if reconcile_local_provider_ownership(&mut record) {
        lifecycle_store.write_record(&record)?;
    }
    // The only genuinely-remote step in this read. Skipping it for a
    // controller-local record is what makes `agent-task status` answerable while
    // the Lab is wedged (#10418).
    let runner_probe = runner_probe_plan(&record, options);
    let before_liveness_reconciliation = record.clone();
    if runner_probe.performed {
        reconcile_runner_job_state_in_store(lifecycle_store, &mut record)?;
    }
    record.annotate_stale_running();
    if record != before_liveness_reconciliation {
        lifecycle_store.write_record(&record)?;
    }
    if record.state.is_terminal() {
        if let Ok(aggregate) = lifecycle_store.read_aggregate(&record.run_id) {
            if reconcile_terminal_provider_models(&mut record, &aggregate) {
                lifecycle_store.write_record(&record)?;
            }
            if !crate::agent_task_lifecycle::terminal_artifact_projection_is_verified_in_store(
                lifecycle_store,
                &record,
                &aggregate,
            )? {
                crate::agent_task_lifecycle::record_terminal_artifact_projection_in_store(
                    lifecycle_store,
                    &mut record,
                    &aggregate,
                )?;
            }
            // Reproject authoritative aggregate model evidence onto a terminal
            // record whose durable lifecycle model is stale-null (#9411). A run
            // that went terminal before the #9404/#9405 model repair keeps
            // `provider_runtime[].metadata.model = null`, which blocks
            // `finalize-pr` even though the aggregate recorded a concrete model.
            if crate::agent_task_lifecycle::terminal_provider_model_reconciliation_needed(
                &record, &aggregate,
            ) {
                let controller_plan = lifecycle_store.read_controller_plan(&record.run_id)?;
                let projection_plan = aggregate_projection_plan(&controller_plan, &aggregate);
                crate::agent_task_lifecycle::reconcile_terminal_provider_model_in_store(
                    lifecycle_store,
                    &mut record,
                    &projection_plan,
                    &aggregate,
                )?;
            }
        }
    }
    // Read-side reconciliation only writes the durable continuation signal.
    // The separate consumer owns execution and cannot inherit a local closure.
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::CandidateRecoverable
            | AgentTaskRunState::PartialRecoverable
    ) {
        let recipe_by_cook_id = record
            .metadata
            .get("cook_id")
            .and_then(Value::as_str)
            .map(|cook_id| recipe_store.load_recipe(cook_id))
            .transpose();
        let recipe = match recipe_by_cook_id {
            Ok(Some(recipe)) => Ok(Some(recipe)),
            Ok(None) => recipe_store.load_recipe_for_attempt(&record.run_id),
            Err(error) => Err(error),
        };
        match recipe {
            Ok(Some(recipe)) => {
                let cook_id = recipe.cook_id.clone();
                // The recipe half comes from the derived recipe store; the
                // controller plan this validates against is lifecycle state and
                // comes from the injected lifecycle store. `validate_recipe_attempt_record`
                // reads that plan ambiently, which is why the
                // `_with_controller_plan` form is used here instead.
                let attempt_validation = load_controller_plan_in_store(
                    lifecycle_store,
                    &record.run_id,
                )
                .and_then(|controller_plan| {
                    crate::agent_task_service::validate_recipe_attempt_record_with_controller_plan(
                        &recipe,
                        &record.run_id,
                        &record,
                        &controller_plan,
                    )
                });
                match attempt_validation {
                    Ok(()) => {
                        if let Some(reason) =
                            crate::agent_task_lifecycle::terminal_artifact_projection_readiness_in_store(
                                lifecycle_store,
                                &record.run_id,
                            )?
                        {
                            let projection_status = record
                                .metadata
                                .pointer("/artifact_projection/status")
                                .and_then(Value::as_str)
                                .unwrap_or("pending")
                                .to_string();
                            let run_id = record.run_id.clone();
                            let repair_command = format!("homeboy agent-task status {run_id}");
                            record.ensure_metadata_object().insert(
                                "cook_continuation_scheduler".to_string(),
                                json!({
                                    "status": format!("artifact_projection_{projection_status}"),
                                    "cook_id": cook_id,
                                    "run_id": run_id,
                                    "phase": "artifact_projection",
                                    "message": reason,
                                    "repair_command": repair_command,
                                }),
                            );
                            lifecycle_store.write_record(&record)?;
                            return Ok(AgentTaskStatusOutcome {
                                record,
                                runner_probe,
                            });
                        }
                        let existing_scheduler_status = record
                            .metadata
                            .get("cook_continuation_scheduler")
                            .and_then(Value::as_object)
                            .and_then(|scheduler| scheduler.get("status"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        match recipe_store.enqueue_terminal_continuation(&cook_id, &record.run_id) {
                            Ok(enqueued) => {
                                let run_id = record.run_id.clone();
                                let coordinator_build_identity =
                                    homeboy_core::build_identity::current().display;
                                let candidate =
                                    record.latest_executor_evidence.as_ref().map(|evidence| {
                                        json!({
                                            "task_id": evidence.task_id,
                                            "provider_run_id": evidence.provider_run_id,
                                            "normalized_output_ref": evidence.normalized_output_ref,
                                        })
                                    });
                                let status = if enqueued {
                                    "queued"
                                } else {
                                    existing_scheduler_status
                                        .as_deref()
                                        .unwrap_or("already_queued_or_completed")
                                };
                                record.ensure_metadata_object().insert(
                                    "cook_continuation_scheduler".to_string(),
                                    json!({
                                        "status": status,
                                        "cook_id": cook_id,
                                        "run_id": run_id,
                                        "coordinator_build_identity": coordinator_build_identity,
                                        "candidate": candidate,
                                    }),
                                );
                                lifecycle_store.write_record(&record)?;
                            }
                            Err(error) => {
                                record.ensure_metadata_object().insert(
                                    "cook_continuation_scheduler".to_string(),
                                    json!({
                                        "status": "failed",
                                        "error_code": error.code.as_str(),
                                        "message": error.message,
                                    }),
                                );
                                lifecycle_store.write_record(&record)?;
                            }
                        }
                    }
                    Err(error) => {
                        record.ensure_metadata_object().insert(
                            "cook_continuation_scheduler".to_string(),
                            json!({
                                "status": "failed",
                                "error_code": error.code.as_str(),
                                "message": error.message,
                            }),
                        );
                        lifecycle_store.write_record(&record)?;
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                record.ensure_metadata_object().insert(
                    "cook_continuation_scheduler".to_string(),
                    json!({
                        "status": "failed",
                        "error_code": error.code.as_str(),
                        "message": error.message,
                    }),
                );
                lifecycle_store.write_record(&record)?;
            }
        }
    }
    if !exact && requested_run_id != record.run_id {
        if let Ok(index) = lifecycle_store.read_cook_index(&requested_run_id) {
            project_cook_alias_adoption_in_store(lifecycle_store, &mut record, &index)?;
            let metadata = record.ensure_metadata_object();
            metadata.insert("cook_alias".to_string(), json!(requested_run_id));
            metadata.insert(
                "cook_index".to_string(),
                serde_json::to_value(index).unwrap_or(Value::Null),
            );
        }
    }
    Ok(AgentTaskStatusOutcome {
        record,
        runner_probe,
    })
}

/// Read the controller-owned lifecycle record without contacting its runner.
///
/// This is deliberately separate from [`status`]: runner reconciliation is
/// authoritative when an operator explicitly asks to refresh liveness, but a
/// wedged runner must never hide state already persisted by the controller.
pub fn persisted_status(run_id: &str) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    persisted_status_in_store(&lifecycle_store, run_id)
}

/// [`persisted_status`] against explicitly injected durable lifecycle roots.
pub fn persisted_status_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let resolved_run_id = resolve_run_id_in_store(lifecycle_store, run_id)?;
    lifecycle_store.read_record_bounded(&resolved_run_id)
}

/// Refresh accepted runner handoffs and expire unbound controller handoffs before
/// a read model (such as activity) projects lifecycle state. A controller wait
/// expiry is not terminal after a runner job is recorded: the runner daemon
/// remains the authority until it reports a terminal job result.
pub fn run_status(run_id: &str, since_cursor: Option<u64>) -> Result<AgentTaskRunStatus> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    run_status_in_store(&lifecycle_store, run_id, since_cursor)
}

/// [`run_status`] against explicitly injected durable lifecycle roots.
///
/// The reconciliation underneath this projection writes — it is
/// [`status_in_store`] — so the aggregate and plan it then reads have to come
/// from the same installation those writes landed in. Projecting a bridge view
/// from one home's aggregate over another home's freshly reconciled record
/// would report progress events for a run that never produced them (#7505).
pub fn run_status_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    since_cursor: Option<u64>,
) -> Result<AgentTaskRunStatus> {
    let record = status_in_store(
        lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )?
    .record;
    let aggregate = lifecycle_store.read_aggregate(&record.run_id).ok();
    let (events, artifact_refs) = match aggregate.as_ref() {
        Some(aggregate) => {
            let refs = artifact_refs_for_outcomes(&aggregate.outcomes);
            (aggregate.events.clone(), refs)
        }
        None => {
            // Surface a local cook's durable running provider execution so the
            // live bridge status advances past "task submitted" too (#8396).
            let mut events = queued_events(&record.tasks);
            events.extend(local_provider_execution_events(&record));
            (events, record.artifact_refs.clone())
        }
    };
    let plan = load_plan_for_execution_in_store(lifecycle_store, &record.run_id).ok();
    let candidate = plan.as_ref().and_then(|plan| {
        (plan.tasks.len() > 1).then(|| {
            let selected = aggregate
                .as_ref()
                .and_then(|value| value.selected_outcome());
            AgentTaskCandidateStatus {
                policy: plan.options.candidate_completion,
                selected_task_id: selected.map(|outcome| outcome.task_id.clone()),
                candidates: aggregate
                    .as_ref()
                    .map(|value| tasks_for_aggregate(&plan, value))
                    .unwrap_or_else(|| record.tasks.clone()),
                deadline_timeout_ms: plan.options.timeout_ms,
                cancellation_supervision: if selected.is_some() {
                    "scheduler_deferred_cleanup".to_string()
                } else {
                    "controller_owned".to_string()
                },
                promotion_action: selected.and_then(|outcome| {
                    outcome.metadata["candidate_selection"]["promotion_action"]
                        .as_str()
                        .map(str::to_string)
                }),
            }
        })
    });
    let action_eligibility = lifecycle_action_eligibility(&record, plan.as_ref());
    let normalized_events = normalize_progress_events(&record.run_id, &events, &artifact_refs);
    let latest_event_cursor = normalized_events
        .last()
        .map(|event| event.sequence)
        .unwrap_or(0);
    let cursor = since_cursor.unwrap_or(0);
    let normalized_events = normalized_events
        .into_iter()
        .filter(|event| event.sequence > cursor)
        .collect();

    Ok(AgentTaskRunStatus {
        schema: schemas::RUN_STATUS.to_string(),
        run_id: record.run_id,
        plan_id: record.plan_id,
        state: record.state,
        submitted_at: record.submitted_at,
        updated_at: record.updated_at,
        totals: record
            .totals
            .unwrap_or_else(|| totals_for_tasks(&record.tasks)),
        latest_event_cursor,
        artifact_refs: record.artifact_refs,
        normalized_events,
        action_eligibility,
        candidate,
    })
}

pub fn list_records() -> Result<Vec<AgentTaskRunRecord>> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    list_records_in_store(&lifecycle_store)
}

/// [`list_records`] against explicitly injected durable lifecycle roots.
///
/// The snapshot enumeration and the per-record refresh must name the same
/// installation: this refreshes through [`status_in_store`], which writes, so
/// enumerating one home's records and reconciling them against another's would
/// terminalize, expire, and reproject runs that do not exist in the home the
/// caller asked about (#7505).
pub fn list_records_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<Vec<AgentTaskRunRecord>> {
    let mut records = Vec::new();
    for record in lifecycle_store.read_records()? {
        if let Ok(record) = status_in_store(
            lifecycle_store,
            &record.run_id,
            AgentTaskStatusOptions::default(),
            false,
        ) {
            records.push(record.record);
            // Discovery health owns malformed-record reporting. A transient
            // status refresh failure must not reintroduce stderr-only state.
        }
    }
    records.sort_by(|left, right| {
        right
            .updated_at
            .as_ref()
            .unwrap_or(&right.submitted_at)
            .cmp(left.updated_at.as_ref().unwrap_or(&left.submitted_at))
            .then_with(|| right.submitted_at.cmp(&left.submitted_at))
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    Ok(records)
}

// The ambient `list_records_with_health()` shim that used to sit here is gone.
// The controller-pin reference provider was its only caller and now lists from
// a store it resolves once (#7505).

/// [`list_records_with_health`] against explicitly injected durable lifecycle
/// roots.
///
/// The health summary and the refreshed records are two views of one
/// installation. Reporting discovery health for one home beside records
/// reconciled in another would attribute malformed-record findings to runs the
/// caller can read back perfectly well (#7505).
pub fn list_records_with_health_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)> {
    let (records, health) = read_records_with_health_in_store(lifecycle_store)?;
    let mut refreshed = Vec::new();
    for record in records {
        if let Ok(record) = status_in_store(
            lifecycle_store,
            &record.run_id,
            AgentTaskStatusOptions::default(),
            false,
        ) {
            refreshed.push(record.record);
        }
    }
    refreshed.sort_by(|left, right| {
        right
            .updated_at
            .as_ref()
            .unwrap_or(&right.submitted_at)
            .cmp(left.updated_at.as_ref().unwrap_or(&left.submitted_at))
            .then_with(|| right.submitted_at.cmp(&left.submitted_at))
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
    Ok((refreshed, health))
}

/// Read the durable registry snapshot without runner reconciliation. Bounded
/// recovery readers use this path so disconnected historical runner mirrors
/// cannot delay access to controller-owned state.
pub fn read_records_with_health() -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)>
{
    read_records_with_health_bounded(1000)
}

/// [`read_records_with_health`] against explicitly injected durable lifecycle
/// roots.
pub fn read_records_with_health_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)> {
    read_records_with_health_bounded_in_store(lifecycle_store, 1000)
}

pub fn read_records_with_health_bounded(
    limit: usize,
) -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    read_records_with_health_bounded_in_store(&lifecycle_store, limit)
}

/// [`read_records_with_health_bounded`] against explicitly injected durable
/// lifecycle roots.
pub fn read_records_with_health_bounded_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    limit: usize,
) -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)> {
    let (mut records, health) = lifecycle_store.read_records_with_health_bounded(limit)?;
    sort_records_newest_first(&mut records);
    Ok((records, health))
}

/// Read every durable registry record without runner reconciliation.
///
/// Exact-match discovery uses this path so filtering happens before a result is
/// selected rather than against the ordinary bounded display snapshot.
pub fn read_all_records_with_health(
) -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    read_all_records_with_health_in_store(&lifecycle_store)
}

/// [`read_all_records_with_health`] against explicitly injected durable
/// lifecycle roots.
pub fn read_all_records_with_health_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<(Vec<AgentTaskRunRecord>, AgentTaskRecordHealthSummary)> {
    let (mut records, health) = lifecycle_store.read_all_records_with_health()?;
    sort_records_newest_first(&mut records);
    Ok((records, health))
}

fn sort_records_newest_first(records: &mut [AgentTaskRunRecord]) {
    records.sort_by(|left, right| {
        right
            .updated_at
            .as_ref()
            .unwrap_or(&right.submitted_at)
            .cmp(left.updated_at.as_ref().unwrap_or(&left.submitted_at))
            .then_with(|| right.submitted_at.cmp(&left.submitted_at))
            .then_with(|| right.run_id.cmp(&left.run_id))
    });
}

/// Resolve an aggregate artifact back to its controller-owned durable run.
/// Aggregate paths are passed to promotion commands after the controller has
/// finished, so the path rather than a transient process-local identifier is
/// the durable source identity.
pub fn run_id_for_aggregate_path(path: &std::path::Path) -> Result<Option<String>> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    run_id_for_aggregate_path_in_store(&lifecycle_store, path)
}

/// [`run_id_for_aggregate_path`] against explicitly injected durable lifecycle
/// roots.
pub fn run_id_for_aggregate_path_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    path: &std::path::Path,
) -> Result<Option<String>> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut matching_run_ids = lifecycle_store
        .read_records()?
        .into_iter()
        .filter_map(|record| {
            let aggregate_path = lifecycle_store.aggregate_path(&record.run_id);
            let aggregate_path = aggregate_path.canonicalize().unwrap_or(aggregate_path);
            (aggregate_path == path).then_some(record.run_id)
        })
        .collect::<Vec<_>>();
    matching_run_ids.sort();
    matching_run_ids.dedup();
    match matching_run_ids.as_slice() {
        [] => Ok(None),
        [run_id] => Ok(Some(run_id.clone())),
        _ => Err(Error::validation_invalid_argument(
            "source",
            "aggregate path is associated with multiple durable agent-task runs",
            Some(path.display().to_string()),
            None,
        )),
    }
}

pub fn run_record_exists(run_id: &str) -> Result<bool> {
    store::record_exists(&sanitize_run_id(run_id))
}

/// [`run_record_exists`] against explicitly injected durable lifecycle roots.
pub fn run_record_exists_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<bool> {
    lifecycle_store.record_exists(&sanitize_run_id(run_id))
}

/// Non-initializing durable existence check for read-only diagnostics.
pub fn run_record_exists_readonly(run_id: &str) -> Result<bool> {
    store::record_exists_readonly(&sanitize_run_id(run_id))
}

/// [`run_record_exists_readonly`] against explicitly injected durable lifecycle
/// roots.
pub fn run_record_exists_readonly_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<bool> {
    lifecycle_store.record_exists_readonly(&sanitize_run_id(run_id))
}

/// Whether a durable run record exists for `run_id` after the same resolution
/// `retry` applies (a cook id resolves to its latest run). The plain
/// `run_record_exists` is an exact-match check, so a resolvable id (e.g. a cook
/// id) reports absent even though `retry` would succeed — which previously made
/// the Lab retry handoff silently fall through and ship an unrunnable
/// `agent-task retry <id>` to a runner with no such record (#8390).
pub fn run_record_exists_resolved(run_id: &str) -> Result<bool> {
    store::record_exists(&resolve_run_id(run_id)?)
}

/// [`run_record_exists_resolved`] against explicitly injected durable lifecycle
/// roots.
pub fn run_record_exists_resolved_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<bool> {
    lifecycle_store.record_exists(&resolve_run_id_in_store(lifecycle_store, run_id)?)
}

// The ambient `mark_resuming()` shim that used to sit here is gone. The resume
// path was its only caller and now marks inside the store it resolved for the
// resume itself (#7505).

/// Stamp the resume request and re-enter Running inside an explicitly rooted
/// store.
///
/// Every part of this follows the injected root, and each for its own reason.
/// The terminal guard is decided from this store's own record: read ambiently,
/// another home's copy of the same identity could refuse a resumable run, or
/// admit a resume into a lifecycle that already finished here. The resume stamp
/// then has to land on the same record the guard just read, or the evidence that
/// a resume was requested ends up in a home that never resumed anything.
///
/// The transition itself goes through `mark_running_in_store` rather than the
/// ambient `mark_running`, and that is the half that matters most: `mark_running`
/// carries the quarantine, live-owner and terminal guards. Evaluated ambiently,
/// a run this store has quarantined would be re-armed to Running anyway — the
/// exact inverse of the quarantine/re-arm pair below — while the state it wrote
/// landed somewhere the resuming controller never reads.
pub(crate) fn mark_resuming_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialRecoverable
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
            | AgentTaskRunState::Cancelled
    ) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id),
            None,
        ));
    }

    let metadata = record.ensure_metadata_object();
    metadata.insert("resume_requested_at".to_string(), json!(now_timestamp()));
    lifecycle_store.write_record(&record)?;
    mark_running_in_store(lifecycle_store, run_id)
}

pub fn retry(run_id: &str, requested_run_id: Option<&str>) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    retry_in_store(&lifecycle_store, run_id, requested_run_id)
}

/// Retry a run inside an explicitly rooted store, without the lineage
/// reservation. See [`retry_with_runtime_admission_in_store`] for what follows
/// the injected root.
pub(crate) fn retry_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    requested_run_id: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    retry_with_force_inner_in_store(lifecycle_store, run_id, requested_run_id, false, false)
}

/// Whether a persisted plan contains enough source identity to offer a retry
/// that can be materialized without consulting the caller's current directory.
pub fn plan_has_retry_materialization_identity(plan: &AgentTaskPlan) -> bool {
    if plan.tasks.iter().any(|task| {
        task.workspace
            .root
            .as_deref()
            .or_else(|| {
                task.executor
                    .config
                    .get("workspace_root")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                task.metadata
                    .get("workspace")
                    .and_then(|workspace| workspace.get("root"))
                    .and_then(Value::as_str)
            })
            .is_some_and(|root| !root.trim().is_empty())
    }) {
        return true;
    }

    let Some(replay) = plan.metadata.get("generic_lab_command_replay") else {
        return false;
    };
    replay.get("schema").and_then(Value::as_str) == Some("homeboy/generic-lab-command-replay/v1")
        && replay
            .get("normalized_args")
            .and_then(Value::as_array)
            .is_some_and(|args| !args.is_empty())
        && replay
            .pointer("/materialization/canonical_root")
            .and_then(Value::as_str)
            .is_some_and(|root| !root.trim().is_empty())
        && replay
            .pointer("/materialization/content_identity")
            .and_then(Value::as_str)
            .is_some_and(|identity| !identity.trim().is_empty())
}

pub(crate) fn record_metadata_value(run_id: &str, key: &str, value: Value) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_metadata_value_in_store(&lifecycle_store, run_id, key, value)
}

pub(crate) fn record_metadata_value_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    key: &str,
    value: Value,
) -> Result<()> {
    lifecycle_store.record_metadata_value(run_id, key, value)
}

// The ambient `retry_with_force()` shim that used to sit above this is gone.
// Its last caller was the Cook retry boundary, which now reserves the successor
// in the store it holds for the whole lineage (#7505).

/// Reserve one successor for the complete retry lineage inside an explicitly
/// rooted store. The advisory lock is taken beside that store's own run
/// directory, so two roots reserve independently instead of contending for the
/// ambient lineage.
pub(crate) fn retry_with_force_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    requested_run_id: Option<&str>,
    force: bool,
) -> Result<AgentTaskRunRecord> {
    retry_with_force_inner_in_store(lifecycle_store, run_id, requested_run_id, force, true)
}

fn retry_with_force_inner_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    requested_run_id: Option<&str>,
    force: bool,
    enforce_lineage_reservation: bool,
) -> Result<AgentTaskRunRecord> {
    let runtime_root =
        homeboy_core::controller_runtime::runtime_root_in(lifecycle_store.roots().data())?;
    retry_with_runtime_admission_in_store(
        lifecycle_store,
        run_id,
        requested_run_id,
        force,
        enforce_lineage_reservation,
        Some(&|run_id| {
            homeboy_core::controller_runtime::admission_status_at(&runtime_root, run_id).ok()
        }),
        |run_id| {
            homeboy_core::controller_runtime::admit_current_for_with_cancellation_check_in_root(
                &runtime_root,
                run_id,
                || Ok(lifecycle_store.read_record(run_id)?.state.is_terminal()),
            )
        },
    )
}

/// Admit one retry successor into an explicitly rooted store.
///
/// This is the whole re-entry decision, and every input to it follows the
/// injected root. The source record and the Cook alias that resolves it come
/// from this store, so a Cook id cannot resolve against another home's index and
/// mint a successor over a live Cook here. The lineage walk that finds the root
/// of the `retry_of` chain reads this store's records, the lineage reservation
/// lock is taken beside this store's own run directory, and the successor scan
/// that decides "is a retry already active?" reads this store's observation
/// database — a scan that answered ambiently would refuse a legitimate retry
/// because another home holds an active successor, or, far worse, admit a second
/// live successor because the active one it should have seen was recorded here.
/// The plan, the acceptance-repair lineage write, and the retry lineage stamped
/// back onto the source and root records all follow the same store.
///
/// The controller-runtime admission and its queue projection are deliberately
/// left as parameters, exactly as `submit_plan_with_runtime_admission_in_store`
/// leaves them. Admission itself is machine-global by design — it writes under
/// `paths::controller_runtimes_store()` and takes a cross-process lock — but the
/// cancellation check inside it reads *lifecycle* state, and that read is rooted
/// by the caller that owns the store.
pub(crate) fn retry_with_runtime_admission_in_store<F, A>(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    requested_run_id: Option<&str>,
    force: bool,
    enforce_lineage_reservation: bool,
    admission_status: Option<&dyn Fn(&str) -> Option<Value>>,
    admit_runtime: F,
) -> Result<AgentTaskRunRecord>
where
    F: FnOnce(&str) -> Result<A>,
    A: RuntimeAdmissionEvidence,
{
    let source = lifecycle_store.read_record(&resolve_run_id_in_store(lifecycle_store, run_id)?)?;
    if source.acceptance.as_ref().is_some_and(|acceptance| {
        acceptance.repair_attempts > 1
            || (acceptance.repair_attempts > 0
                && acceptance.verdict != AgentTaskAcceptanceVerdict::Rejected)
    }) {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "acceptance rejection repair budget is exhausted for this lineage",
            Some(source.run_id.clone()),
            None,
        ));
    }
    let root_run_id = retry_root_run_id_in_store(lifecycle_store, &source)?;
    let _reservation = enforce_lineage_reservation
        .then(|| RetryLineageLock::lock_in_store(lifecycle_store, &root_run_id))
        .transpose()?;
    let mut requested_run_id = requested_run_id;
    if enforce_lineage_reservation {
        let records = lifecycle_store.read_records()?;
        let mut successors = records
            .into_iter()
            .filter(|record| record.run_id != root_run_id)
            .filter(|record| {
                retry_root_run_id_in_store(lifecycle_store, record)
                    .ok()
                    .as_deref()
                    == Some(&root_run_id)
            })
            .collect::<Vec<_>>();
        successors.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        if let Some(active) = successors.iter().find(|record| !record.state.is_terminal()) {
            if !force {
                // A caller can lose the response after the first durable write.
                // Replaying its exact requested successor is an idempotent read,
                // not an attempt to allocate beside the active reservation.
                if requested_run_id == Some(active.run_id.as_str()) {
                    return Ok(active.clone());
                }
                return Err(active_retry_successor_error(active));
            }
            if requested_run_id == Some(active.run_id.as_str()) {
                requested_run_id = None;
            }
        }
        if !successors.is_empty() && !force {
            return Err(Error::validation_invalid_argument(
                "force",
                format!(
                    "retry lineage rooted at '{}' already has terminal successor(s); use --force to create another retry",
                    root_run_id
                ),
                Some(root_run_id),
                None,
            ));
        }
    }
    let mut plan = load_controller_plan_in_store(lifecycle_store, &source.run_id)?;
    // Both restorations below are filesystem work on the plan value rather than
    // lifecycle-store work: they re-point task workspace roots at durable
    // checkouts, and both are no-ops for a plan carrying no Cook candidate
    // evidence. The initial one reads only the workspace it is restoring. The
    // follow-up one materializes a whole checkout under an artifact root, which
    // is why it takes this store's own — see its doc comment for the one reach
    // that remains ambient there.
    super::cook_workspace_restore::restore_initial_cook_candidate_workspace(&mut plan)?;
    super::cook_workspace_restore::restore_follow_up_cook_candidate_workspace_in_root(
        &mut plan,
        &lifecycle_store.artifact_root(),
    )?;
    if source
        .acceptance
        .as_ref()
        .is_some_and(|acceptance| acceptance.verdict == AgentTaskAcceptanceVerdict::Rejected)
    {
        if let Some(feedback) = source.metadata["acceptance_repair"]["feedback"].as_str() {
            for task in &mut plan.tasks {
                task.instructions.push_str(&format!(
                    "\n\nAddress this reviewer remediation feedback, then preserve the Cook's normal verification and review-form contract:\n{feedback}"
                ));
                task.inputs["cook_loop"]["reviewer_remediation"] = json!({
                    "source_run_id": source.run_id,
                    "feedback": feedback,
                    "max_attempts": 1,
                });
            }
        }
    }
    let mut metadata = serde_json::Map::new();
    if let Some(route) =
        homeboy_core::notification_route::NotificationRoute::from_metadata(&source.metadata)
    {
        // Retries are new durable runs, but retain the initiating route. Resume
        // operates on the same record and therefore needs no copy.
        metadata.insert(
            homeboy_core::notification_route::NOTIFICATION_ROUTE_METADATA_KEY.to_string(),
            serde_json::to_value(route).expect("notification route is serializable"),
        );
    }
    let retry_origin = [
        "runner_id",
        "runner_job_id",
        "remote_workspace",
        "remote_command",
        "runner_execution_record",
        "pre_execution_failure",
        "runner_job_events",
    ]
    .into_iter()
    .filter_map(|key| {
        source
            .metadata
            .get(key)
            .map(|value| (key.to_string(), value.clone()))
    })
    .collect::<serde_json::Map<_, _>>();
    if !retry_origin.is_empty() {
        metadata.insert("retry_origin".to_string(), Value::Object(retry_origin));
    }
    metadata.insert("retry_of".to_string(), json!(source.run_id));
    if enforce_lineage_reservation {
        metadata.insert("retried_from".to_string(), json!(source.run_id));
        metadata.insert("retry_root".to_string(), json!(root_run_id));
    }
    metadata.insert("retry_requested_at".to_string(), json!(now_timestamp()));
    let mut record = submit_plan_with_runtime_admission_in_store(
        lifecycle_store,
        &plan,
        requested_run_id,
        execution_runner_id(),
        Some(metadata),
        admission_status,
        admit_runtime,
    )?;
    if let Some(mut acceptance) = source.acceptance.clone() {
        // A repair is a new candidate, but it retains the rejected verdict and
        // evidence as lineage instead of erasing the reviewer decision.
        acceptance.archive();
        acceptance.verdict = AgentTaskAcceptanceVerdict::Pending;
        acceptance.candidate = empty_acceptance_candidate();
        acceptance.base_sha.clear();
        acceptance.actor = None;
        acceptance.recorded_at = None;
        acceptance.provider_ref = None;
        acceptance.verifier = None;
        acceptance.evidence_refs.clear();
        acceptance.repair_attempts = source
            .acceptance
            .as_ref()
            .map(|acceptance| acceptance.repair_attempts)
            .unwrap_or_default();
        if let Some(updated) = lifecycle_store.mutate_record(&record.run_id, |child| {
            child.acceptance = Some(acceptance.clone());
            child.ensure_metadata_object().insert(
                "acceptance_repair_lineage".to_string(),
                json!({ "source_run_id": source.run_id, "count": acceptance.repair_attempts, "max": 1 }),
            );
            true
        })? {
            record = updated;
        }
    }
    if enforce_lineage_reservation {
        persist_retry_lineage_in_store(
            lifecycle_store,
            &source.run_id,
            &root_run_id,
            &record.run_id,
        )?;
    }
    Ok(record)
}

const RETRY_LINEAGE_LIMIT: usize = 16;

struct RetryLineageLock {
    #[allow(dead_code)]
    file: File,
}

impl RetryLineageLock {
    /// Reserve a lineage beside the injected store's own run directory.
    ///
    /// This lock is the whole reservation: it is what makes "does an active
    /// successor already exist?" a decision one process at a time. Taken at
    /// `paths::homeboy_data()` while the scan and the successor write went to an
    /// injected root, two concurrent retries in two roots would serialize
    /// against each other for no reason, and — the direction that actually
    /// loses — two retries in the *same* injected root reached from processes
    /// with different ambient homes would not serialize at all, so both would
    /// scan, both would see no active successor, and both would allocate one.
    fn lock_in_store(lifecycle_store: &AgentTaskLifecycleStore, root_run_id: &str) -> Result<Self> {
        let path = lifecycle_store
            .data_root()
            .join("agent-task-runs")
            .join("retry-lineages")
            .join(format!("{}.lock", sanitize_run_id(root_run_id)));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| Error::internal_io(error.to_string(), None))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
        #[cfg(unix)]
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
            return Err(Error::internal_io(
                std::io::Error::last_os_error().to_string(),
                Some(format!("lock retry lineage {root_run_id}")),
            ));
        }
        Ok(Self { file })
    }
}

/// Walk the `retry_of` chain to the lineage root inside an explicitly rooted
/// store. The walk decides which lineage lock is taken and which successors
/// count as this run's own, so a parent read from another home would reserve and
/// scan a lineage that does not exist here.
fn retry_root_run_id_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
) -> Result<String> {
    let mut current = record.clone();
    for _ in 0..RETRY_LINEAGE_LIMIT {
        let Some(parent) = current.metadata.get("retry_of").and_then(Value::as_str) else {
            return Ok(current.run_id);
        };
        current = lifecycle_store.read_record(&sanitize_run_id(parent))?;
    }
    Err(Error::validation_invalid_argument(
        "retry_of",
        "retry lineage exceeds the supported depth",
        Some(record.run_id.clone()),
        None,
    ))
}

fn active_retry_successor_error(record: &AgentTaskRunRecord) -> Error {
    Error::validation_invalid_argument(
        "run_id",
        format!(
            "active retry successor '{}' is {:?}; inspect it with `homeboy agent-task status {}`",
            record.run_id, record.state, record.run_id
        ),
        Some(record.run_id.clone()),
        Some(vec![format!("homeboy agent-task status {}", record.run_id)]),
    )
}

/// Stamp the successor onto the source and root records inside an explicitly
/// rooted store. This is the durable evidence the next lineage scan reads, so
/// writing it ambiently would leave the injected root's own lineage empty and
/// let the following retry allocate beside a successor it cannot see.
fn persist_retry_lineage_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    source_run_id: &str,
    root_run_id: &str,
    child_run_id: &str,
) -> Result<()> {
    let mut targets = vec![sanitize_run_id(source_run_id)];
    let root_run_id = sanitize_run_id(root_run_id);
    if !targets.contains(&root_run_id) {
        targets.push(root_run_id.clone());
    }
    for run_id in targets {
        lifecycle_store.mutate_record(&run_id, |record| {
            let metadata = record.ensure_metadata_object();
            let lineage = metadata
                .entry("retries".to_string())
                .or_insert_with(|| json!([]));
            if !lineage.is_array() {
                *lineage = json!([]);
            }
            let retries = lineage.as_array_mut().expect("retry lineage is an array");
            if !retries.iter().any(|entry| entry == child_run_id) {
                retries.push(json!(child_run_id));
                if retries.len() > RETRY_LINEAGE_LIMIT {
                    retries.drain(..retries.len() - RETRY_LINEAGE_LIMIT);
                }
            }
            metadata.insert("retry_root".to_string(), json!(root_run_id));
            record.updated_at = Some(now_timestamp());
            true
        })?;
    }
    Ok(())
}

// The ambient `find_unbound_cook_retry_successor()` shim that used to sit
// above this is gone. Its last caller was the Cook retry boundary, which now
// looks the successor up in the store it reserved it in (#7505).

/// Find the one lifecycle-first Cook retry reservation that can be bound to an
/// unbound recipe attempt, inside an explicitly rooted store.
///
/// The `retry_of` lookup is backed by the observation metadata index; the plan
/// and attempt-shaped run id prevent adoption of an unrelated retry from the
/// same source.
///
/// The caller treats `None` as authority to create a reservation, so this read
/// has to come from the store the reservation was made in. Answered ambiently it
/// fails in both directions: a successor reserved here reads back as absent and
/// a second one is minted over it, or an unrelated home's successor is adopted
/// and this Cook attempt is bound to a run that does not exist in its own root.
pub(crate) fn find_unbound_cook_retry_successor_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    source_run_id: &str,
    cook_id: &str,
    attempt: u32,
    plan: &AgentTaskPlan,
) -> Result<Option<AgentTaskRunRecord>> {
    let prefix = format!("{}-attempt-{attempt}-", sanitize_run_id(cook_id));
    let mut matches = lifecycle_store
        .read_retry_successors(&sanitize_run_id(source_run_id))?
        .into_iter()
        .filter(|record| record.run_id.starts_with(&prefix))
        .filter(|record| record.plan_id == plan.plan_id)
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    match matches.as_slice() {
        [] => Ok(None),
        [record] => Ok(Some(record.clone())),
        _ => Err(Error::validation_invalid_argument(
            "cook_recipe.attempts",
            "multiple lifecycle retry reservations match this Cook attempt",
            Some(source_run_id.to_string()),
            None,
        )),
    }
}

/// Rebuild a gate-failed Cook candidate from its controller-owned promotion
/// before retrying. A persisted follow-up plan names a temporary checkout, not
/// authority to reuse whatever happens to exist at that path.
pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    artifacts_in_store(&lifecycle_store, run_id)
}

/// [`artifacts`] against explicitly injected durable lifecycle roots.
pub fn artifacts_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunArtifacts> {
    let snapshot = durable_local_read_in_store(lifecycle_store, run_id)?;
    let record = snapshot.record;
    let run_id = record.run_id.clone();
    let aggregate = snapshot.aggregate;
    let latest_executor_evidence = record.latest_executor_evidence.as_ref();
    let mut evidence_refs = aggregate_evidence_refs(aggregate.as_ref(), latest_executor_evidence);
    if aggregate.is_none()
        && (record.metadata.get("kind").and_then(Value::as_str)
            == Some("lab_offload_pre_dispatch_failure")
            || record
                .tasks
                .iter()
                .any(|task| task.task_id == "agent-task-predispatch"))
    {
        evidence_refs.insert(
            0,
            AgentTaskEvidenceRef {
                kind: "lab-offload-pre-dispatch-failure".to_string(),
                uri: format!("homeboy://agent-task/run/{run_id}/logs"),
                label: Some("Lab offload pre-dispatch failure".to_string()),
            },
        );
        dedup_evidence_refs(&mut evidence_refs);
    }
    Ok(AgentTaskRunArtifacts {
        schema: schemas::RUN_ARTIFACTS.to_string(),
        run_id,
        artifacts: aggregate
            .as_ref()
            .map(crate::agent_task_artifacts::reviewer_facing_aggregate)
            .map(|aggregate| aggregate_artifacts(Some(&aggregate)))
            .unwrap_or_default(),
        evidence_refs,
    })
}

/// A bounded controller-local view used by read-only aggregate consumers.
///
/// It intentionally never calls [`status`], which can reconcile a live runner.
/// The observation-store record read uses its read-only 750ms SQLite busy bound;
/// aggregate failure is represented in `unavailable_sources` so callers can
/// still render the durable identity and phase they did obtain.
#[derive(Debug, Clone)]
pub struct AgentTaskDurableLocalRead {
    pub record: AgentTaskRunRecord,
    pub aggregate: Option<AgentTaskAggregate>,
    pub unavailable_sources: Vec<AgentTaskDurableReadUnavailable>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTaskDurableReadUnavailable {
    pub source: &'static str,
    pub reason_code: &'static str,
    pub detail: String,
}

/// Read the durable controller record and its local aggregate within the
/// read-only store budget, without runner liveness reconciliation.
pub fn durable_local_read(run_id: &str) -> Result<AgentTaskDurableLocalRead> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    durable_local_read_in_store(&lifecycle_store, run_id)
}

/// [`durable_local_read`] against explicitly injected durable lifecycle roots.
pub fn durable_local_read_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskDurableLocalRead> {
    let record = persisted_status_in_store(lifecycle_store, run_id)?;
    durable_local_read_record_in_store(lifecycle_store, record)
}

/// Read one concrete durable record without resolving a Cook ID through its
/// latest attempt, from an explicitly injected durable lifecycle root. This is
/// the inspection counterpart to [`exact_record`].
///
/// The ambient `exact_durable_local_read()` shim that used to sit above this is
/// gone: `status_once` was its only caller and now reads through the store it
/// resolved for the whole status read (#7505).
pub fn exact_durable_local_read_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskDurableLocalRead> {
    let record = lifecycle_store.read_record_bounded(&sanitize_run_id(run_id))?;
    durable_local_read_record_in_store(lifecycle_store, record)
}

fn durable_local_read_record_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: AgentTaskRunRecord,
) -> Result<AgentTaskDurableLocalRead> {
    let aggregate = match lifecycle_store.read_aggregate_bounded(&record.run_id) {
        Ok(aggregate) => Some(aggregate),
        Err(error) => {
            return Ok(AgentTaskDurableLocalRead {
                record,
                aggregate: None,
                unavailable_sources: vec![AgentTaskDurableReadUnavailable {
                    source: "aggregate",
                    reason_code: if error.details["reason_code"] == "durable_read.oversized" {
                        "durable_read.oversized"
                    } else {
                        "durable_read.unavailable"
                    },
                    detail: format!(
                        "The controller-local aggregate was unavailable within the durable read; the record below remains authoritative partial evidence: {}",
                        error.message
                    ),
                }],
            });
        }
    };
    Ok(AgentTaskDurableLocalRead {
        record,
        aggregate,
        unavailable_sources: Vec::new(),
    })
}

/// Read the aggregate after a transport reconciliation completed it without
/// scheduling the controller-side synthetic handoff task.
pub fn read_aggregate(run_id: &str) -> Result<AgentTaskAggregate> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    read_aggregate_in_store(&lifecycle_store, run_id)
}

/// [`read_aggregate`] against explicitly injected durable lifecycle roots.
pub fn read_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskAggregate> {
    let run_id = resolve_run_id_in_store(lifecycle_store, run_id)?;
    lifecycle_store.read_aggregate(&run_id)
}

/// Read an immutable attempt directly; unlike `read_aggregate`, this never
/// treats a Cook ID as an alias for its latest attempt.
pub fn read_attempt_aggregate(run_id: &str) -> Result<AgentTaskAggregate> {
    store::read_aggregate(&sanitize_run_id(run_id))
}

/// [`read_attempt_aggregate`] against explicitly injected durable lifecycle
/// roots.
pub fn read_attempt_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskAggregate> {
    lifecycle_store.read_aggregate(&sanitize_run_id(run_id))
}

pub fn aggregate_source(run_id: &str) -> Result<(String, PathBuf)> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    aggregate_source_in_store(&lifecycle_store, run_id)
}

/// [`aggregate_source`] against explicitly injected durable lifecycle roots.
///
/// This reads like an accessor and is not one. Candidate selection consults the
/// Cook index, and the `status_in_store` below it is the full reconciliation —
/// two advisory locks and roughly twenty durable write sites. Selecting an
/// attempt from one home's Cook index, reconciling it into another, and then
/// serializing a third home's aggregate would answer with bytes no single
/// installation ever held, without failing while it did (#7505).
pub fn aggregate_source_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<(String, PathBuf)> {
    let selected_run_id = match cook_index_in_store(lifecycle_store, run_id)
        .and_then(|_| select_cook_candidate_in_store(lifecycle_store, run_id))
    {
        Ok(selection) if selection.incomplete => {
            return Err(Error::validation_invalid_argument(
                "cook_id",
                "candidate selection is incomplete after its bounded recovery window",
                Some(run_id.to_string()),
                None,
            ));
        }
        Ok(selection) if !selection.run_id.is_empty() => selection.run_id,
        _ => run_id.to_string(),
    };
    let record = status_in_store(
        lifecycle_store,
        &selected_run_id,
        AgentTaskStatusOptions::default(),
        false,
    )?
    .record;
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
    // `record.run_id` is already resolved, so these are the store's own exact
    // reads rather than the alias-resolving `lifecycle_ops` wrappers.
    let aggregate = lifecycle_store.read_aggregate(&record.run_id)?;
    let raw = serde_json::to_string_pretty(&aggregate).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("serialize agent-task aggregate {}", record.run_id)),
        )
    })?;
    let path = lifecycle_store.aggregate_path(&record.run_id);
    Ok((raw, path))
}

pub fn record_cook_attempt(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
) -> Result<AgentTaskCookIndex> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_cook_attempt_in_store(&lifecycle_store, cook_id, attempt, run_id)
}

pub fn record_cook_attempt_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    attempt: u32,
    run_id: &str,
) -> Result<AgentTaskCookIndex> {
    let registration = lifecycle_store.with_config_lock(|| {
        let registration =
            record_cook_attempt_locked_in_store(lifecycle_store, cook_id, attempt, run_id)?;
        // The index is the handoff's ownership proof. Redirect its placeholder
        // while the index writer lock is held so an exit observer cannot fail
        // the parent between index publication and this transition.
        complete_detached_cook_handoff_parent_in_store(lifecycle_store, cook_id, run_id)?;
        Ok(registration)
    })?;
    let index = registration.project_terminal_after_unlock_in_store(lifecycle_store)?;
    Ok(index)
}

// The ambient `record_cook_attempt_locked()` shim that used to sit here is gone;
// its one strict-lock test now registers inside the store it resolves (#7505).

pub(crate) fn record_cook_attempt_locked_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    attempt: u32,
    run_id: &str,
) -> Result<CookAttemptRegistration> {
    let cook_id = sanitize_run_id(cook_id);
    let run_id = sanitize_run_id(run_id);
    if let Ok(parent) = lifecycle_store.read_record(&cook_id) {
        let handoff = &parent.metadata["detached_cook_handoff"];
        let reserved_child = handoff["materializing_attempt_run_id"] == run_id;
        if handoff["cook_id"] == cook_id
            && (((parent.state.is_terminal() && handoff["state"] != "redirected")
                || handoff["cancellation_fence"]["state"] == "cancelled")
                && !reserved_child
                || (handoff["state"] != "pending" && handoff["state"] != "redirected"))
        {
            return Err(Error::validation_invalid_argument(
                "cook_id",
                "detached Cook handoff was cancelled or terminal before its attempt could materialize",
                Some(cook_id),
                None,
            ));
        }
    }
    // Validate both durable ownership projections before changing either one.
    lifecycle_store.validate_cook_index_attempt(&cook_id, attempt, &run_id)?;
    let mut record = lifecycle_store.read_record(&run_id)?;
    let recorded_cook_id = record.metadata.get("cook_id").and_then(Value::as_str);
    let recorded_attempt = record.metadata.get("cook_attempt").and_then(Value::as_u64);
    if recorded_cook_id.is_some_and(|value| value != cook_id)
        || recorded_attempt.is_some_and(|value| value != u64::from(attempt))
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "durable lifecycle run is already owned by a different Cook attempt",
            Some(run_id),
            None,
        ));
    }
    let recorded_at = now_timestamp();
    let metadata = record.ensure_metadata_object();
    metadata.insert("cook_id".to_string(), json!(&cook_id));
    metadata.insert("cook_attempt".to_string(), json!(attempt));
    if let Ok(parent) = lifecycle_store.read_record(&cook_id) {
        if let Some(supervisor) = parent.metadata["detached_cook_handoff"]
            .get("supervisor_job_id")
            .cloned()
        {
            metadata.insert(
                "local_cook_supervisor".to_string(),
                json!({
                    "job_id": supervisor,
                    "reattach_command": format!("homeboy agent-task status {cook_id} --full"),
                }),
            );
        }
    }
    let committed = lifecycle_store.write_record_locked_without_terminal_projection(&record)?;
    // Completion can precede Cook registration during handoff recovery. Re-read
    // its persisted aggregate after the Cook identity is durable, then commit the
    // attempt and substantive pointer in the same index write.
    let candidate = lifecycle_store
        .read_aggregate(&committed.run_id)
        .ok()
        .and_then(|aggregate| {
            substantive_candidate_from_aggregate(&committed.run_id, attempt, &aggregate, None)
        });
    let index = lifecycle_store.write_cook_index_attempt_locked(
        &cook_id,
        attempt,
        &committed.run_id,
        recorded_at,
        candidate,
    )?;
    Ok(CookAttemptRegistration {
        index,
        run_id: committed.run_id,
    })
}

pub(crate) struct CookAttemptRegistration {
    index: AgentTaskCookIndex,
    run_id: String,
}

impl CookAttemptRegistration {
    pub(crate) fn project_terminal_after_unlock(self) -> Result<AgentTaskCookIndex> {
        let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
        self.project_terminal_after_unlock_in_store(&lifecycle_store)
    }

    pub(crate) fn project_terminal_after_unlock_in_store(
        self,
        lifecycle_store: &AgentTaskLifecycleStore,
    ) -> Result<AgentTaskCookIndex> {
        lifecycle_store.project_terminal_record_after_unlock(&self.run_id)?;
        Ok(self.index)
    }
}

// The ambient `record_cook_recovery_checkpoint()` shim that used to sit here is gone;
// the fanout resume path was its only caller and now checkpoints inside the store it resolves (#7505).

pub fn record_cook_recovery_checkpoint_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    phase: &str,
    next_command: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let checkpoint = json!({
        "schema": "homeboy/agent-task-cook-recovery-checkpoint/v1",
        "phase": phase,
        "next_command": next_command,
    });
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        if record.metadata.get("cook_recovery_checkpoint") == Some(&checkpoint) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        record
            .ensure_metadata_object()
            .insert("cook_recovery_checkpoint".to_string(), checkpoint.clone());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => lifecycle_store.read_record(&run_id),
    }
}

/// Re-arm a finalized candidate after a dependency rebase. The original
/// provider output remains authoritative; Cook resumes at the promotion/gate
/// boundary and finalizes a fresh review only after those gates pass again.
pub fn invalidate_cook_finalization_for_dependency(
    run_id: &str,
    dependency_revision: &str,
    next_command: &str,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    invalidate_cook_finalization_for_dependency_in_store(
        &lifecycle_store,
        run_id,
        dependency_revision,
        next_command,
    )
}

/// Re-arm a finalized candidate inside an explicitly rooted store.
///
/// This is the invalidating half of [`record_cook_finalization_in_store`], and
/// like [`clear_cook_moving_base_recovery_in_store`] it has to erase from the
/// same root that recorded. Its idempotence guard reads the checkpoint it is
/// about to write, so an ambient reach would decide "already re-armed" from
/// another home's evidence and leave this store's finalization standing — the
/// exact state that lets a stale review be published as if its gates had been
/// rerun.
///
/// The unchanged-record fallback read follows the injected store too, so the
/// record handed back to the caller is always the one this call operated on.
pub fn invalidate_cook_finalization_for_dependency_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    dependency_revision: &str,
    next_command: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let metadata = record.ensure_metadata_object();
        // Retrying the same invalidation after an interrupted coordinator must
        // preserve the original review evidence and leave its timestamp stable.
        if metadata.get("cook_finalization").is_none()
            && metadata
                .get("cook_recovery_source_checkpoint")
                .is_some_and(|checkpoint| {
                    checkpoint["phase"] == "verification_pending"
                        && checkpoint["dependency_revision"] == dependency_revision
                })
        {
            return false;
        }
        let prior = metadata.remove("cook_finalization");
        let original_prior = metadata
            .get("cook_recovery_source_checkpoint")
            .and_then(|checkpoint| checkpoint.get("prior_finalization"))
            .cloned()
            .unwrap_or_else(|| prior.clone().unwrap_or(Value::Null));
        let Some(mut promotion) = metadata.get("latest_promotion").cloned() else {
            // A terminal child can have no promotion only when it was never a
            // review-ready Cook; retain an inspectable invalidation marker.
            metadata.insert("dependency_rebase".to_string(), json!({ "revision": dependency_revision, "next_command": next_command, "prior_finalization": original_prior }));
            record.updated_at = Some(now_timestamp());
            return true;
        };
        promotion["status"] = json!("verification_pending");
        promotion["dependency_revision"] = json!(dependency_revision);
        metadata.insert("latest_promotion".to_string(), promotion);
        metadata.insert(
            "cook_recovery_source_checkpoint".to_string(),
            json!({
                "schema": "homeboy/agent-task-cook-recovery-checkpoint/v1",
                "phase": "verification_pending",
                "next_command": next_command,
                "dependency_revision": dependency_revision,
                "prior_finalization": original_prior,
            }),
        );
        record.updated_at = Some(now_timestamp());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => lifecycle_store.read_record(&run_id),
    }
}

pub fn cook_index(cook_id: &str) -> Result<AgentTaskCookIndex> {
    store::read_cook_index(&sanitize_run_id(cook_id))
}

/// [`cook_index`] against explicitly injected durable lifecycle roots.
pub fn cook_index_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<AgentTaskCookIndex> {
    lifecycle_store.read_cook_index(&sanitize_run_id(cook_id))
}

pub fn cook_index_exists(cook_id: &str) -> Result<bool> {
    store::cook_index_exists(&sanitize_run_id(cook_id))
}

/// [`cook_index_exists`] against explicitly injected durable lifecycle roots.
pub fn cook_index_exists_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<bool> {
    Ok(lifecycle_store.cook_index_exists(&sanitize_run_id(cook_id)))
}

/// The durable child identity reserved by a detached Cook handoff before its
/// Cook index is published. The parent remains the cancellation and reattach
/// identity; this is read-side authority only while the handoff is pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedCookMaterializingAttempt {
    pub cook_id: String,
    pub run_id: String,
}

// The ambient `resolve_detached_cook_materializing_attempt()` shim that used to sit
// here is gone; the status reader was its only caller and now resolves the attempt in the store it checked the index against (#7505).

/// [`resolve_detached_cook_materializing_attempt`] against explicitly injected
/// durable lifecycle roots.
pub fn resolve_detached_cook_materializing_attempt_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<Option<DetachedCookMaterializingAttempt>> {
    let cook_id = sanitize_run_id(cook_id);
    if lifecycle_store.cook_index_exists(&cook_id) {
        return Ok(None);
    }
    let Ok(parent) = lifecycle_store.read_record(&cook_id) else {
        return Ok(None);
    };
    let handoff = &parent.metadata["detached_cook_handoff"];
    if handoff["cook_id"] != cook_id || handoff["state"] != "pending" {
        return Ok(None);
    }
    let Some(run_id) = handoff["materializing_attempt_run_id"].as_str() else {
        return Ok(None);
    };
    let run_id = sanitize_run_id(run_id);
    if lifecycle_store.read_record(&run_id).is_err() {
        return Ok(None);
    }
    Ok(Some(DetachedCookMaterializingAttempt { cook_id, run_id }))
}

#[cfg(test)]
pub(crate) fn replace_cook_index_for_test(index: &AgentTaskCookIndex) -> Result<()> {
    store::write_cook_index_for_test(index)
}

/// The bounded controller-owned answer to which Cook attempt still owns a
/// candidate. The mutable latest-attempt alias is chronological history, not
/// candidate authority: a later metadata-only attempt must not erase a patch.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookCandidateSelection {
    pub schema: String,
    pub cook_id: String,
    pub run_id: String,
    pub attempt: u32,
    pub latest_attempt_run_id: String,
    pub reason: String,
    #[serde(default)]
    pub incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_newer_attempts: Vec<AgentTaskCookCandidateSkippedAttempt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_newer_run_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTaskCookCandidateSkippedAttempt {
    pub run_id: String,
    pub reason: String,
}

const COOK_CANDIDATE_SELECTION_WINDOW: usize = 64;

/// Select the latest attempt with controller-readable actionable patch bytes.
/// Ties use run ID so duplicate attempt numbers remain deterministic. When no
/// attempt has candidate bytes, retain the legacy latest attempt for old runs.
pub fn select_cook_candidate(cook_id: &str) -> Result<AgentTaskCookCandidateSelection> {
    let index = cook_index(cook_id)?;
    select_cook_candidate_from_index(cook_id, index, None)
}

pub(crate) fn select_cook_candidate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Result<AgentTaskCookCandidateSelection> {
    let index = lifecycle_store.read_cook_index(&sanitize_run_id(cook_id))?;
    select_cook_candidate_from_index(cook_id, index, Some(lifecycle_store))
}

pub fn select_cook_candidate_from_attempts(
    cook_id: &str,
    attempts: Vec<AgentTaskCookIndexAttempt>,
) -> Result<AgentTaskCookCandidateSelection> {
    let latest_run_id = attempts
        .last()
        .map(|attempt| attempt.run_id.clone())
        .unwrap_or_default();
    select_cook_candidate_from_index(
        cook_id,
        AgentTaskCookIndex {
            schema: schemas::COOK_INDEX.to_string(),
            cook_id: cook_id.to_string(),
            latest_run_id,
            latest_substantive_candidate: None,
            attempts,
        },
        None,
    )
}

fn select_cook_candidate_from_index(
    cook_id: &str,
    index: AgentTaskCookIndex,
    lifecycle_store: Option<&AgentTaskLifecycleStore>,
) -> Result<AgentTaskCookCandidateSelection> {
    if let Some(candidate) = index.latest_substantive_candidate.as_ref() {
        if substantive_candidate_in_store(&candidate.run_id, lifecycle_store).as_ref()
            == Some(&(candidate.task_id.clone(), candidate.artifact_id.clone()))
        {
            return Ok(AgentTaskCookCandidateSelection {
                schema: "homeboy/agent-task-cook-candidate-selection/v1".to_string(),
                cook_id: index.cook_id,
                run_id: candidate.run_id.clone(),
                attempt: candidate.attempt,
                latest_attempt_run_id: index.latest_run_id,
                reason: "latest_substantive_candidate_pointer".to_string(),
                incomplete: false,
                selected_task_id: Some(candidate.task_id.clone()),
                selected_artifact_id: Some(candidate.artifact_id.clone()),
                skipped_newer_attempts: Vec::new(),
                skipped_newer_run_ids: Vec::new(),
            });
        }
    }
    // Legacy indexes predate the durable pointer. Their recovery path reads at
    // most this fixed tail window and reports incomplete rather than widening.
    let attempts = index
        .attempts
        .iter()
        .rev()
        .take(COOK_CANDIDATE_SELECTION_WINDOW)
        .collect::<Vec<_>>();
    let latest_attempt_run_id = index.latest_run_id.clone();
    let mut skipped_newer_run_ids = Vec::new();
    let mut skipped_newer_attempts = Vec::new();
    for attempt in attempts.iter().take(COOK_CANDIDATE_SELECTION_WINDOW) {
        if let Some((task_id, artifact_id)) =
            substantive_candidate_in_store(&attempt.run_id, lifecycle_store)
        {
            return Ok(AgentTaskCookCandidateSelection {
                schema: "homeboy/agent-task-cook-candidate-selection/v1".to_string(),
                cook_id: index.cook_id,
                run_id: attempt.run_id.clone(),
                attempt: attempt.attempt,
                latest_attempt_run_id,
                reason: if skipped_newer_run_ids.is_empty() {
                    "latest_attempt_has_substantive_candidate".to_string()
                } else {
                    "latest_substantive_candidate_after_non_substantive_attempts".to_string()
                },
                incomplete: false,
                selected_task_id: Some(task_id),
                selected_artifact_id: Some(artifact_id),
                skipped_newer_attempts,
                skipped_newer_run_ids,
            });
        }
        skipped_newer_run_ids.push(attempt.run_id.clone());
        skipped_newer_attempts.push(AgentTaskCookCandidateSkippedAttempt {
            run_id: attempt.run_id.clone(),
            reason: "no_verified_canonical_promotable_patch".to_string(),
        });
    }
    if index.attempts.len() > COOK_CANDIDATE_SELECTION_WINDOW {
        return Ok(AgentTaskCookCandidateSelection {
            schema: "homeboy/agent-task-cook-candidate-selection/v1".to_string(),
            cook_id: index.cook_id,
            run_id: String::new(),
            attempt: 0,
            latest_attempt_run_id,
            reason: "selection_window_exhausted_without_promotable_candidate".to_string(),
            incomplete: true,
            selected_task_id: None,
            selected_artifact_id: None,
            skipped_newer_attempts,
            skipped_newer_run_ids,
        });
    }
    let latest = attempts.first().ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_id",
            "durable Cook index has no attempts",
            Some(cook_id.to_string()),
            None,
        )
    })?;
    Ok(AgentTaskCookCandidateSelection {
        schema: "homeboy/agent-task-cook-candidate-selection/v1".to_string(),
        cook_id: index.cook_id,
        run_id: latest.run_id.clone(),
        attempt: latest.attempt,
        latest_attempt_run_id,
        reason: "no_substantive_candidate_evidence_preserve_latest_attempt_compatibility"
            .to_string(),
        incomplete: false,
        selected_task_id: None,
        selected_artifact_id: None,
        skipped_newer_attempts,
        skipped_newer_run_ids,
    })
}

pub(crate) fn update_cook_candidate_after_completion(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
    promotion: Option<Value>,
) -> Result<()> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    update_cook_candidate_after_completion_in_store(&lifecycle_store, record, aggregate, promotion)
}

pub(crate) fn update_cook_candidate_after_completion_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
    promotion: Option<Value>,
) -> Result<()> {
    let Some(cook_id) = record.metadata.get("cook_id").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(attempt) = record.metadata.get("cook_attempt").and_then(Value::as_u64) else {
        return Ok(());
    };
    let Some(candidate) =
        substantive_candidate_from_aggregate(&record.run_id, attempt as u32, aggregate, promotion)
    else {
        return Ok(());
    };
    lifecycle_store.update_cook_index(cook_id, |index| {
        replace_latest_substantive_candidate(index, candidate)
    })?;
    Ok(())
}

fn replace_latest_substantive_candidate(
    index: &mut AgentTaskCookIndex,
    candidate: AgentTaskCookLatestSubstantiveCandidate,
) -> bool {
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
    replace
}

fn substantive_candidate_from_aggregate(
    run_id: &str,
    attempt: u32,
    aggregate: &AgentTaskAggregate,
    promotion: Option<Value>,
) -> Option<AgentTaskCookLatestSubstantiveCandidate> {
    let (task_id, artifact_id) = substantive_candidate_in_aggregate(run_id, aggregate)?;
    let outcome = aggregate
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == task_id)?;
    let artifact = outcome
        .artifacts
        .iter()
        .find(|artifact| artifact.id == artifact_id)?;
    let promotion_provenance = promotion
        .as_ref()
        .and_then(|value| value.get("provenance").cloned());
    let destination_provenance = promotion.as_ref().map(|value| {
        json!({
            "to_worktree": value.get("to_worktree"),
            "target": value.get("target"),
        })
    });
    Some(AgentTaskCookLatestSubstantiveCandidate {
        schema: "homeboy/agent-task-cook-latest-substantive-candidate/v1".to_string(),
        run_id: run_id.to_string(),
        attempt,
        task_id,
        artifact_id,
        artifact_kind: artifact.kind.clone(),
        artifact_sha256: artifact.sha256.clone(),
        artifact_size_bytes: artifact.size_bytes,
        integrity: json!({
            "sha256": artifact.sha256,
            "size_bytes": artifact.size_bytes,
            "controller_projection": "verified",
            "canonical_patch": true,
        }),
        promotion_provenance,
        destination_provenance,
        recorded_at: now_timestamp(),
    })
}

fn substantive_candidate_in_store(
    run_id: &str,
    lifecycle_store: Option<&AgentTaskLifecycleStore>,
) -> Option<(String, String)> {
    // Candidate recovery is a bounded scan. Avoid the aggregate reader's
    // reconciliation path when this controller record never projected one.
    let record = match lifecycle_store {
        Some(store) => store.read_record(run_id).ok()?,
        None => exact_record(run_id).ok()?,
    };
    let aggregate_path = record.aggregate_path?;
    if !std::path::Path::new(&aggregate_path).exists() {
        return None;
    }
    let aggregate = match lifecycle_store {
        Some(store) => store.read_aggregate(run_id),
        None => store::read_aggregate(run_id),
    };
    let Ok(aggregate) = aggregate else {
        return None;
    };
    substantive_candidate_in_aggregate(run_id, &aggregate)
}

fn substantive_candidate_in_aggregate(
    run_id: &str,
    aggregate: &AgentTaskAggregate,
) -> Option<(String, String)> {
    let outcome = aggregate.selected_outcome().or_else(|| {
        (aggregate.outcomes.len() == 1)
            .then(|| aggregate.outcomes.first())
            .flatten()
    });
    let outcome = outcome?;
    // Metadata alone (and typed artifact envelopes) cannot authorize recovery.
    // Selection requires controller-readable bytes that pass the same integrity
    // and canonical patch normalization used by promotion.
    outcome.artifacts.iter().find_map(|artifact| {
        if !crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact) {
            return None;
        }
        let path = crate::agent_task_lifecycle::verified_controller_artifact_projection_path(
            run_id,
            &outcome.task_id,
            artifact,
        )
        .ok()
        .flatten()?;
        std::fs::canonicalize(path)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|bytes| {
                (crate::agent_task_promotion::validate_artifact_content(artifact, &bytes).is_ok()
                    && crate::agent_task_promotion::normalize_promotion_patch(
                        &bytes,
                        "candidate-selection",
                    )
                    .is_ok_and(|patch| !patch.content.trim().is_empty()))
                .then(|| (outcome.task_id.clone(), artifact.id.clone()))
            })
    })
}

/// Read one durable attempt without resolving a cook ID through its latest
/// index entry. Recovery must inspect historical source attempts directly.
pub fn exact_record(run_id: &str) -> Result<AgentTaskRunRecord> {
    store::read_record(&sanitize_run_id(run_id))
}

/// [`exact_record`] against explicitly injected durable lifecycle roots.
pub fn exact_record_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    lifecycle_store.read_record(&sanitize_run_id(run_id))
}

// The ambient `reconcile_scope_run_ids()` shim that used to sit here is gone;
// the reconciler moved to the rooted form in a prior slice and the one
// remaining test caller now resolves its own store (#7505).

/// [`reconcile_scope_run_ids`] against explicitly injected durable lifecycle
/// roots.
pub fn reconcile_scope_run_ids_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Vec<String>> {
    let requested = sanitize_run_id(run_id);
    let mut run_ids = vec![requested.clone()];

    if let Ok(parent) = lifecycle_store.read_record(&requested) {
        if let Some(attempt_run_id) = parent
            .metadata
            .pointer("/detached_cook_handoff/attempt_run_id")
            .and_then(Value::as_str)
        {
            let attempt_run_id = sanitize_run_id(attempt_run_id);
            let attempt = lifecycle_store.read_record(&attempt_run_id)?;
            let attempt_cook_id = attempt.metadata.get("cook_id").and_then(Value::as_str);
            if attempt_cook_id != Some(requested.as_str()) {
                return Err(Error::validation_invalid_argument(
                    "detached_cook_handoff.attempt_run_id",
                    "detached Cook handoff attempt does not belong to its parent Cook",
                    Some(attempt_run_id),
                    None,
                ));
            }
            let index = lifecycle_store.read_cook_index(&requested).map_err(|_| {
                Error::validation_invalid_argument(
                    "detached_cook_handoff.attempt_run_id",
                    "detached Cook handoff attempt has no Cook index authority",
                    Some(attempt.run_id.clone()),
                    None,
                )
            })?;
            if !index
                .attempts
                .iter()
                .any(|entry| entry.run_id == attempt.run_id)
            {
                return Err(Error::validation_invalid_argument(
                    "detached_cook_handoff.attempt_run_id",
                    "detached Cook handoff attempt is absent from its Cook index",
                    Some(attempt.run_id),
                    None,
                ));
            }
            // The handoff binds this repair to its accepted child. A later
            // index retry is a separate attempt and remains out of scope.
            run_ids.push(attempt.run_id);
        } else if let Ok(index) = lifecycle_store.read_cook_index(&requested) {
            run_ids.push(index.latest_run_id);
        }
    } else if let Ok(index) = lifecycle_store.read_cook_index(&requested) {
        run_ids[0] = index.latest_run_id;
    }

    run_ids.sort();
    run_ids.dedup();
    Ok(run_ids)
}

/// Resolve a caller-supplied identifier to the durable run it addresses.
///
/// A Cook id is an alias, not a record: attempts are stored under
/// `{cook_id}-attempt-{n}-{suffix}`, so `exact_record(cook_id)` always misses.
/// This maps the alias through the durable Cook index and returns the id
/// unchanged when it is not an alias.
///
/// This is a **pure read**: `store::read_cook_index` is a single
/// `fs::read_to_string` of `agent-task-cooks/<id>/index.json` and writes
/// nothing. Read models that must not mutate persisted state (`activity`,
/// #10308) may use it; `status()` may not, because it reconciles.
pub(crate) fn resolve_run_id(run_id: &str) -> Result<String> {
    let run_id = sanitize_run_id(run_id);
    match store::read_cook_index(&run_id) {
        Ok(index) => Ok(index.latest_run_id),
        Err(_) => Ok(run_id),
    }
}

/// The store-rooted counterpart of [`resolve_run_id`], consulting the injected
/// store's own Cook index.
///
/// This is deliberately a sibling rather than a delegation target for the
/// ambient function. [`resolve_run_id`] swallows every index read failure and
/// therefore cannot fail; routing it through a store constructor would let an
/// unresolvable environment turn a total function into a fallible one.
pub(crate) fn resolve_run_id_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<String> {
    let run_id = sanitize_run_id(run_id);
    match lifecycle_store.read_cook_index(&run_id) {
        Ok(index) => Ok(index.latest_run_id),
        Err(_) => Ok(run_id),
    }
}

fn acceptance_candidate(
    promotion: &Value,
) -> Option<crate::agent_task_promotion::AgentTaskCandidateFingerprint> {
    promotion
        .pointer("/provenance/candidate/fingerprint")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .filter(candidate_is_complete)
}

fn candidate_is_complete(
    candidate: &crate::agent_task_promotion::AgentTaskCandidateFingerprint,
) -> bool {
    !candidate.schema.trim().is_empty()
        && !candidate.target_path.trim().is_empty()
        && !candidate.head.trim().is_empty()
        && !candidate.base.trim().is_empty()
        && !candidate.sha256.trim().is_empty()
        && !candidate.tree.trim().is_empty()
}

fn empty_acceptance_candidate() -> crate::agent_task_promotion::AgentTaskCandidateFingerprint {
    crate::agent_task_promotion::AgentTaskCandidateFingerprint {
        schema: String::new(),
        target_path: String::new(),
        head: String::new(),
        base: String::new(),
        changed_files: Vec::new(),
        sha256: String::new(),
        tree: String::new(),
    }
}

pub fn record_promotion(run_id: &str, promotion: Value) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_promotion_in_store(&lifecycle_store, run_id, promotion)
}

/// Retain a bounded, ordered replay of runner-delivered promotion frames. This
/// is observer evidence only; promotion checkpoints remain the recovery authority.
pub fn record_promotion_progress_frames(
    run_id: &str,
    runner_job_id: &str,
    frames: impl IntoIterator<Item = (u64, Value)>,
) -> Result<()> {
    const MAX_FRAMES: usize = 64;

    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    let run_id = sanitize_run_id(run_id);
    let _ = lifecycle_store.mutate_record(&run_id, |record| {
        let metadata = record.ensure_metadata_object();
        let events = metadata
            .entry("promotion_progress_frames".to_string())
            .or_insert_with(|| json!([]));
        let events = events
            .as_array_mut()
            .expect("promotion progress frames are an array");
        let mut changed = false;
        for (sequence, frame) in frames {
            if events.iter().any(|event| {
                event.get("runner_job_id").and_then(Value::as_str) == Some(runner_job_id)
                    && event.get("sequence").and_then(Value::as_u64) == Some(sequence)
            }) {
                continue;
            }
            let frame = json!({
                "runner_job_id": runner_job_id,
                "sequence": sequence,
                "frame": frame,
                "recorded_at": now_timestamp(),
            });
            events.push(homeboy_core::redaction::redact_json(&frame));
            changed = true;
        }
        if events.len() > MAX_FRAMES {
            let excess = events.len() - MAX_FRAMES;
            events.drain(..excess);
            changed = true;
        }
        changed
    })?;
    Ok(())
}

pub fn record_promotion_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    promotion: Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        if record.metadata.get("latest_promotion") == Some(&promotion) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        let metadata = record.ensure_metadata_object();
        // The first post-apply checkpoint is immutable recovery authority. Later
        // gate/finalization reports advance `latest_promotion` without obscuring
        // the exact candidate that may be resumed after a coordinator loss.
        if promotion.get("status").and_then(Value::as_str) == Some("verification_pending") {
            metadata
                .entry("cook_recovery_source_checkpoint".to_string())
                .or_insert_with(|| promotion.clone());
        }
        let promotions = metadata
            .entry("promotions".to_string())
            .or_insert_with(|| json!([]));
        if !promotions.is_array() {
            *promotions = json!([]);
        }
        promotions
            .as_array_mut()
            .expect("promotions array")
            .push(promotion.clone());
        metadata.insert("latest_promotion".to_string(), promotion.clone());
        if let Some(acceptance) = record.acceptance.as_mut() {
            let candidate = acceptance_candidate(&promotion);
            let base_sha = promotion
                .pointer("/verified_base/sha")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if candidate
                .as_ref()
                .is_none_or(|candidate| !acceptance.matches_candidate(candidate, &base_sha))
            {
                acceptance.archive();
                acceptance.verdict = AgentTaskAcceptanceVerdict::Pending;
                acceptance.candidate = candidate.unwrap_or_else(empty_acceptance_candidate);
                acceptance.base_sha = base_sha;
                acceptance.actor = None;
                acceptance.recorded_at = None;
                acceptance.provider_ref = None;
                acceptance.verifier = None;
                acceptance.attestation = None;
                acceptance.signature = None;
                acceptance.key_id = None;
                acceptance.evidence_refs.clear();
                acceptance.repair_attempts = 0;
            }
        }
        if record.acceptance.is_none()
            && promotion.get("status").and_then(Value::as_str) == Some("applied")
        {
            let requirement = record
                .metadata
                .get("acceptance_requirement")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok());
            let candidate = acceptance_candidate(&promotion);
            let base_sha = promotion
                .pointer("/verified_base/sha")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty());
            if let (Some(requirement), Some(candidate), Some(base_sha)) =
                (requirement, candidate, base_sha)
            {
                record.acceptance = Some(AgentTaskAcceptanceRecord::pending(
                    requirement,
                    candidate,
                    base_sha.to_string(),
                ));
            }
        }
        true
    })?;
    let record = match record {
        Some(record) => record,
        None => lifecycle_store.read_record(&run_id)?,
    };
    if let Ok(aggregate) = lifecycle_store.read_aggregate(&run_id) {
        update_cook_candidate_after_completion_in_store(
            lifecycle_store,
            &record,
            &aggregate,
            Some(promotion),
        )?;
    }
    Ok(record)
}

/// Record a verdict against an explicitly rooted store. The thin delegation to
/// the feedback-bearing sibling mirrors the ambient pair exactly.
pub fn record_acceptance_verdict_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    verdict: AgentTaskAcceptanceVerdict,
    evidence_refs: Vec<String>,
    token: String,
) -> Result<AgentTaskRunRecord> {
    record_acceptance_verdict_with_feedback_in_store(
        lifecycle_store,
        run_id,
        verdict,
        evidence_refs,
        token,
        None,
    )
}

// The ambient `record_acceptance_verdict_with_feedback()` shim that used to sit
// here is gone; the acceptance CLI command was its only caller (#7505).

/// Record an authority verdict inside an explicitly rooted store.
///
/// The pre-verification binding read, the drift comparison made again under the
/// record mutation, and the durable verdict write are one compare-and-swap and
/// all three follow the injected root. Read ambiently, the binding this call
/// sends to the authority would describe another home's applied promotion,
/// while the verdict it produced landed here — a signed attestation bound to a
/// candidate this store never promoted, with the drift guard that exists to
/// catch exactly that comparing the wrong two records.
///
/// The bounded validation guards are repeated here rather than moved behind the
/// ambient entry point: a blank token, an empty evidence list, oversized
/// feedback, and feedback on a non-rejection are refused before any store is
/// touched, exactly as they are today.
///
/// The acceptance verifier registry is deliberately left process-global. It is
/// configured trust material and a subprocess contract, not durable lifecycle
/// state, so it is not one of this store's roots.
pub fn record_acceptance_verdict_with_feedback_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    verdict: AgentTaskAcceptanceVerdict,
    evidence_refs: Vec<String>,
    token: String,
    feedback: Option<String>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    if token.trim().is_empty() || evidence_refs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "acceptance requires an authority token and at least one evidence reference",
            None,
            None,
        ));
    }
    let feedback = feedback
        .map(|feedback| feedback.trim().to_string())
        .filter(|feedback| !feedback.is_empty());
    if feedback
        .as_ref()
        .is_some_and(|feedback| feedback.len() > 2000)
    {
        return Err(Error::validation_invalid_argument(
            "feedback",
            "reviewer remediation feedback must be at most 2000 bytes",
            None,
            None,
        ));
    }
    if feedback.is_some() && verdict != AgentTaskAcceptanceVerdict::Rejected {
        return Err(Error::validation_invalid_argument(
            "feedback",
            "reviewer remediation feedback is only valid with a rejected verdict",
            None,
            None,
        ));
    }
    let existing = lifecycle_store.read_record(&run_id)?;
    let acceptance = existing.acceptance.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "acceptance",
            "acceptance is not required for this run",
            None,
            None,
        )
    })?;
    validate_acceptance_requirement(&acceptance.requirement)?;
    if acceptance.verdict == AgentTaskAcceptanceVerdict::Pending
        && (!candidate_is_complete(&acceptance.candidate) || acceptance.base_sha.trim().is_empty())
    {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "acceptance is unavailable until an applied promotion records the candidate and verified base",
            None,
            None,
        ));
    }
    if existing
        .metadata
        .pointer("/latest_promotion/status")
        .and_then(Value::as_str)
        != Some("applied")
    {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "acceptance requires an applied promotion with green deterministic gates",
            None,
            None,
        ));
    }
    // Keep the pre-verification binding and compare it again under the durable
    // record mutation. An authority result cannot be applied to a promotion
    // that advanced while the authority was deciding.
    let expected_requirement = acceptance.requirement.clone();
    let expected_candidate = acceptance.candidate.clone();
    let expected_base_sha = acceptance.base_sha.clone();
    let request = AgentTaskAcceptanceVerificationRequest {
        requirement: expected_requirement.clone(),
        verdict,
        candidate: expected_candidate.clone(),
        base_sha: expected_base_sha.clone(),
        evidence_refs: evidence_refs.clone(),
        token,
    };
    let (attestation, verifier) = with_acceptance_verifier(|verifier| {
        (verifier.verify_acceptance(&request), verifier.provenance())
    });
    let attestation = attestation?;
    validate_attestation(&acceptance.requirement, &attestation)?;
    if verifier.verifier.trim().is_empty() || verifier.configuration.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "authority verifier has incomplete configured provenance",
            None,
            None,
        ));
    }
    if acceptance.verdict == verdict
        && acceptance.actor.as_deref() == Some(attestation.actor.as_str())
        && acceptance.evidence_refs == evidence_refs
    {
        return Ok(existing);
    }
    let mut promotion_drifted = false;
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let Some(acceptance) = record.acceptance.as_mut() else {
            promotion_drifted = true;
            return false;
        };
        if acceptance.requirement != expected_requirement
            || acceptance.candidate != expected_candidate
            || acceptance.base_sha != expected_base_sha
            || record
                .metadata
                .pointer("/latest_promotion/status")
                .and_then(Value::as_str)
                != Some("applied")
        {
            promotion_drifted = true;
            return false;
        }
        if matches!(verdict, AgentTaskAcceptanceVerdict::Pending) {
            return false;
        }
        if acceptance.verdict == verdict
            && acceptance.actor.as_deref() == Some(attestation.actor.as_str())
            && acceptance.evidence_refs == evidence_refs
        {
            return false;
        }
        acceptance.archive();
        acceptance.verdict = verdict;
        acceptance.actor = Some(attestation.actor.clone());
        acceptance.recorded_at = Some(attestation.issued_at.clone());
        acceptance.provider_ref = Some(attestation.provider_ref.clone());
        acceptance.verifier = Some(verifier.clone());
        acceptance.attestation = Some(attestation.clone());
        acceptance.signature = Some(attestation.signature.clone());
        acceptance.key_id = Some(attestation.key_id.clone());
        acceptance.evidence_refs = evidence_refs.clone();
        let repair = if verdict == AgentTaskAcceptanceVerdict::Rejected {
            acceptance.repair_attempts = acceptance.repair_attempts.saturating_add(1);
            Some(json!({
                "status": if acceptance.repair_attempts == 1 { "repair_available" } else { "repair_exhausted" },
                "candidate": acceptance.candidate,
                "base_sha": acceptance.base_sha,
                "attempts": acceptance.repair_attempts,
                "max_attempts": 1,
                "evidence_refs": acceptance.evidence_refs,
                "feedback": feedback,
            }))
        } else {
            None
        };
        if let Some(repair) = repair {
            record
                .ensure_metadata_object()
                .insert("acceptance_repair".to_string(), repair);
        }
        record.updated_at = Some(now_timestamp());
        true
    })?;
    if promotion_drifted {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "acceptance candidate changed while the authority verdict was being recorded; rerun acceptance for the current applied promotion",
            None,
            None,
        ));
    }
    record.ok_or_else(|| Error::validation_invalid_argument("acceptance", "acceptance is not required for this run, does not match the declared authority/policy, or the identical verdict is already durable", None, None))
}

/// Persist the controller publication result separately from promotion so a
/// resumed cook can prove finalization already completed before it publishes.
pub fn record_cook_finalization(run_id: &str, finalization: Value) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_cook_finalization_in_store(&lifecycle_store, run_id, finalization)
}

pub fn record_cook_finalization_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    finalization: Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        if record.metadata.get("cook_finalization") == Some(&finalization) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        record
            .ensure_metadata_object()
            .insert("cook_finalization".to_string(), finalization.clone());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => lifecycle_store.read_record(&run_id),
    }
}

// The ambient `record_cook_force_with_lease_receipt()` shim that used to sit
// here is gone; the fanout force-with-lease path was its only caller and now writes both receipts into one store (#7505).

pub fn record_cook_force_with_lease_receipt_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    receipt: Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        if record.metadata.get("cook_force_with_lease_receipt") == Some(&receipt) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        record
            .ensure_metadata_object()
            .insert("cook_force_with_lease_receipt".to_string(), receipt.clone());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => lifecycle_store.read_record(&run_id),
    }
}

/// Persist a validated manual publication intent without claiming Cook completion.
pub(crate) fn record_manual_finalization_intent(
    run_id: &str,
    intent: Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let digest = manual_finalization_intent_digest(&intent);
    let record = store::mutate_record(&run_id, |record| {
        if record.metadata.get("manual_finalization_intent") == Some(&intent)
            && record.metadata.get("manual_finalization_intent_digest") == Some(&json!(digest))
        {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        record
            .ensure_metadata_object()
            .insert("manual_finalization_intent".to_string(), intent.clone());
        record.ensure_metadata_object().insert(
            "manual_finalization_intent_digest".to_string(),
            json!(digest),
        );
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => store::read_record(&run_id),
    }
}

/// Stable digest of the exact validated manual dossier persisted for recovery.
pub(crate) fn manual_finalization_intent_digest(intent: &Value) -> String {
    content_hash::sha256_hex(
        &serde_json::to_vec(intent).expect("JSON values always serialize into a manual intent"),
    )
}

/// Persist a completed manual receipt and bind it to the validated intent digest.
pub(crate) fn record_manual_finalization_receipt(
    run_id: &str,
    receipt: Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = store::mutate_record(&run_id, |record| {
        let intent_digest = record.metadata["manual_finalization_intent_digest"].clone();
        record.updated_at = Some(now_timestamp());
        let metadata = record.ensure_metadata_object();
        metadata.insert("cook_finalization".to_string(), receipt.clone());
        metadata.insert(
            "manual_finalization_receipt_digest".to_string(),
            json!(manual_finalization_intent_digest(&receipt)),
        );
        metadata.insert(
            "manual_finalization_receipt_intent_digest".to_string(),
            intent_digest,
        );
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => store::read_record(&run_id),
    }
}

/// Checkpoint controller-owned recovery after a promoted, green candidate loses
/// its publication base. The terminal provider result remains untouched.
pub fn record_cook_moving_base_recovery(
    run_id: &str,
    recovery: Value,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_cook_moving_base_recovery_in_store(&lifecycle_store, run_id, recovery)
}

pub fn record_cook_moving_base_recovery_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    recovery: Value,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        if record.metadata.get("cook_moving_base_recovery") == Some(&recovery) {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        record
            .ensure_metadata_object()
            .insert("cook_moving_base_recovery".to_string(), recovery.clone());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => lifecycle_store.read_record(&run_id),
    }
}

pub(crate) fn clear_cook_moving_base_recovery_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let record = lifecycle_store.mutate_record(&run_id, |record| {
        let Some(metadata) = record.metadata.as_object_mut() else {
            return false;
        };
        if metadata.remove("cook_moving_base_recovery").is_none() {
            return false;
        }
        record.updated_at = Some(now_timestamp());
        true
    })?;
    match record {
        Some(record) => Ok(record),
        None => lifecycle_store.read_record(&run_id),
    }
}
