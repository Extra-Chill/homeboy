//! Recovery of generic runner-exec evidence after a controller interruption.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const STARTUP_RUNNER_EXEC_RECOVERY_LIMIT: i64 = 100;
pub const STARTUP_RUNNER_EXEC_RECOVERY_BUDGET: Duration = Duration::from_secs(5);
const RECOVERY_KIND: &str = "runner_exec_recovery";
const RECOVERY_OWNER_ID: &str = "runner-exec-recovery";
const RECOVERY_OWNER_LEASE: Duration = Duration::from_secs(30);
const RECOVERY_LEASE_HEARTBEAT: Duration = Duration::from_secs(1);
const RECOVERY_CHILD_REQUEST_ENV: &str = "HOMEBOY_RUNNER_EXEC_RECOVERY_RECORD_REQUEST";
const RECOVERY_CLEANUP_BUDGET: Duration = Duration::from_millis(250);

#[derive(serde::Serialize, serde::Deserialize)]
struct RecoveryRecordRequest {
    run_id: String,
    snapshot: homeboy_core::api_jobs::RunnerJobLogSnapshot,
}
#[derive(Clone)]
struct RecoveryOwner {
    id: String,
    token: String,
    deadline: Instant,
}

impl RecoveryOwner {
    fn remaining(&self) -> Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "recovery_budget",
                    "runner-exec recovery owner deadline expired",
                    None,
                    None,
                )
            })
    }

    /// Returns false when the caller must defer the current source record.
    /// Ownership loss remains an error because another owner may be writing it.
    fn before_side_effect(&self, store: &ObservationStore) -> Result<bool> {
        // Do not invent a second reserve for synchronous durable writes. The
        // owner deadline is the only recovery budget; callers defer before
        // beginning a record once it is exhausted.
        if self.remaining().is_err() {
            return Ok(false);
        }
        if !store.renew_running_run_lease(
            &self.id,
            &self.token,
            chrono::Utc::now().timestamp_millis() + RECOVERY_OWNER_LEASE.as_millis() as i64,
        )? {
            let mut error = Error::validation_invalid_argument(
                "recovery_owner",
                "runner-exec recovery ownership was taken over",
                Some(self.id.clone()),
                None,
            );
            error.details["ownership_lost"] = json!(true);
            return Err(error);
        }
        Ok(true)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunnerExecRecoverySchedule {
    pub owner_id: String,
    pub owner_token: String,
    pub deferred_count: usize,
    pub budget_ms: u64,
    pub inspection_action: String,
    pub is_new_owner: bool,
}

/// Reserve a durable, independently inspectable owner before a background
/// recovery worker is spawned. Scheduling only reads local evidence; remote
/// reconciliation belongs to the owner, never to the mutating caller.
pub fn schedule_terminal_runner_exec_recovery() -> Result<Option<RunnerExecRecoverySchedule>> {
    let store = ObservationStore::open_initialized()?;
    let candidates = recovery_candidates(&store)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    // The store's expiring claim is keyed by run ID. A stable ID is therefore
    // the singleton identity; a fresh UUID here would only make each scheduler
    // claim itself and permit overlapping recovery workers.
    let owner_id = RECOVERY_OWNER_ID.to_string();
    let owner_token = Uuid::new_v4().to_string();
    let lease_expires_at_ms =
        chrono::Utc::now().timestamp_millis() + RECOVERY_OWNER_LEASE.as_millis() as i64;
    let owner = store.claim_expiring_singleton_run(
        NewRunRecord::builder(RECOVERY_KIND)
            .metadata(json!({
                "phase": "accepted",
                "deferred_count": candidates.len(),
                "budget_ms": STARTUP_RUNNER_EXEC_RECOVERY_BUDGET.as_millis() as u64,
                "inspection_action": format!("homeboy runs show {owner_id}"),
            }))
            .build(),
        owner_id,
        &owner_token,
        lease_expires_at_ms,
    );
    let Some(owner) = owner? else {
        let existing = store
            .list_runs(RunListFilter {
                kind: Some(RECOVERY_KIND.to_string()),
                status: Some(RunStatus::Running.as_str().to_string()),
                limit: Some(1),
                ..RunListFilter::default()
            })?
            .into_iter()
            .next();
        if let Some(existing) = existing {
            let deferred_count = existing
                .metadata_json
                .get("deferred_count")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            return Ok(Some(recovery_schedule(
                &existing.id,
                deferred_count,
                String::new(),
                false,
            )));
        }
        return Ok(None);
    };
    Ok(Some(recovery_schedule(
        &owner.id,
        candidates.len(),
        owner_token,
        true,
    )))
}

fn recovery_schedule(
    owner_id: &str,
    deferred_count: usize,
    owner_token: String,
    is_new_owner: bool,
) -> RunnerExecRecoverySchedule {
    RunnerExecRecoverySchedule {
        owner_id: owner_id.to_string(),
        owner_token,
        deferred_count,
        budget_ms: STARTUP_RUNNER_EXEC_RECOVERY_BUDGET.as_millis() as u64,
        inspection_action: format!("homeboy runs show {owner_id}"),
        is_new_owner,
    }
}

/// Scan durable generic runner-exec runs, query their exact accepted runner-job
/// binding, then retain declared evidence before terminal projection. This runs
/// before command dispatch so a new daemon operation cannot evict a completed
/// job while its controller checkpoint is still pending.
pub fn reconcile_terminal_runner_exec_runs() -> Result<usize> {
    reconcile_terminal_runner_exec_runs_with_budget(STARTUP_RUNNER_EXEC_RECOVERY_BUDGET)
        .map(|(reconciled, _)| reconciled)
}

/// Reconcile under one owner-wide wall-clock budget. A failed endpoint is not
/// retried for each historical job: all later records on that runner are
/// deferred with their evidence intact for a later owner.
pub fn reconcile_terminal_runner_exec_runs_with_budget(budget: Duration) -> Result<(usize, usize)> {
    reconcile_terminal_runner_exec_runs_with_owner(budget, None)
}

fn reconcile_terminal_runner_exec_runs_with_owner(
    budget: Duration,
    owner: Option<&RecoveryOwner>,
) -> Result<(usize, usize)> {
    let deadline = owner
        .map(|owner| owner.deadline)
        .unwrap_or_else(|| Instant::now() + budget);
    let store = ObservationStore::open_initialized_until(deadline)?;
    let mut reconciled = 0;
    let mut deferred = 0;
    let mut unavailable_endpoints = BTreeSet::new();
    // A scheduled owner owns one absolute budget from creation through every
    // remote read and durable projection. The standalone API creates that
    // deadline here for its own caller-owned budget.
    let mut sessions = BTreeMap::new();
    let candidates = recovery_candidates(&store)?;
    'candidates: for (index, run) in candidates.iter().enumerate() {
        if Instant::now() >= deadline {
            deferred += candidates.len() - index;
            break;
        }
        let (Some(runner_id), Some(job_id), Some(cwd)) = (
            run.metadata_json.get("runner_id").and_then(Value::as_str),
            run.metadata_json
                .get("runner_job_id")
                .and_then(Value::as_str),
            run.cwd.as_deref(),
        ) else {
            continue;
        };
        if let Some(owner) = owner {
            if owner.remaining().is_err() {
                deferred += candidates.len() - index;
                break;
            }
        }
        // Resolve a persisted session under the owner deadline, then cache by
        // its daemon identity. Runner aliases can name the same endpoint.
        let session = {
            if Instant::now() >= deadline {
                Err(Error::validation_invalid_argument(
                    "recovery_budget",
                    "runner-exec recovery owner deadline expired before session resolution",
                    None,
                    None,
                ))
            } else {
                crate::persisted_status_until(runner_id, deadline).and_then(|status| {
                    status.session.ok_or_else(|| {
                        Error::validation_invalid_argument(
                            "runner",
                            "runner has no persisted daemon session for recovery",
                            Some(runner_id.to_string()),
                            None,
                        )
                    })
                })
            }
        };
        let endpoint = session
            .as_ref()
            .map(endpoint_identity)
            .unwrap_or_else(|_| runner_id.to_string());
        if unavailable_endpoints.contains(&endpoint) {
            deferred += 1;
            continue;
        }
        // Cache both healthy sessions and their failures by resolved endpoint,
        // never by an arbitrary runner alias.
        let session = sessions.entry(endpoint.clone()).or_insert(session);
        let snapshot = match session.as_ref().map_err(Clone::clone).and_then(|session| {
            crate::evidence::runner_job_log_snapshot_for_session_until(session, job_id, deadline)
        }) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Some(owner) = owner {
                    if !owner.before_side_effect(&store)? {
                        deferred += candidates.len() - index;
                        break 'candidates;
                    }
                }
                record_evicted_evidence_loss(&store, &run, &error)?;
                // A 404 is a durable per-job result. Other failures describe the
                // endpoint, so avoid amplifying one unavailable daemon into N probes.
                if error.details.get("http_status").and_then(Value::as_u64) != Some(404) {
                    unavailable_endpoints.insert(endpoint);
                }
                deferred += 1;
                continue;
            }
        };
        if !snapshot.job.status.is_terminal() {
            continue;
        }
        if let Some(owner) = owner {
            if !owner.before_side_effect(&store)? {
                deferred += candidates.len() - index;
                break;
            }
            if !run_recovery_record_child(&store, run, &snapshot, owner.deadline)? {
                deferred += candidates.len() - index;
                break;
            }
        } else {
            reconcile_terminal_snapshot(run, &snapshot, cwd)?;
        }
        reconciled += 1;
    }
    Ok((reconciled, deferred))
}

/// Execute the non-cancellable durable sequence from an atomic request. This is
/// entered only by the contained recovery child, never by the owner process.
pub fn run_terminal_runner_exec_recovery_record(request_path: &Path) -> Result<()> {
    let request: RecoveryRecordRequest =
        serde_json::from_slice(&fs::read(request_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read recovery request {}", request_path.display())),
            )
        })?)
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse recovery record request".to_string()),
            )
        })?;
    let store = ObservationStore::open_initialized()?;
    let run = store.get_run(&request.run_id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            "recovery source record disappeared",
            Some(request.run_id.clone()),
            None,
        )
    })?;
    let cwd = run.cwd.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "run_id",
            "recovery source record has no cwd",
            Some(run.id.clone()),
            None,
        )
    })?;
    reconcile_terminal_snapshot(&run, &request.snapshot, &cwd)?;
    let _ = fs::remove_file(request_path);
    Ok(())
}

fn run_recovery_record_child(
    store: &ObservationStore,
    run: &homeboy_core::observation::RunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
    deadline: Instant,
) -> Result<bool> {
    let request_path = write_recovery_record_request(run, snapshot)?;
    let before_artifacts = store.list_artifacts(&run.id)?;
    let executable = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve recovery child executable".to_string()),
        )
    })?;
    let mut command = Command::new(executable);
    command
        .env(RECOVERY_CHILD_REQUEST_ENV, &request_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut containment = homeboy_core::process::ProcessContainment::prepare(&mut command)?;
    let mut child = command.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("spawn recovery record child".to_string()),
        )
    })?;
    containment.attach(&child)?;
    loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("poll recovery record child".to_string()),
            )
        })? {
            let _ = containment.cleanup_after_leader_exit_bounded(RECOVERY_CLEANUP_BUDGET);
            let _ = fs::remove_file(&request_path);
            if status.success() {
                return Ok(true);
            }
            rollback_recovery_record(run, &before_artifacts, deadline + RECOVERY_CLEANUP_BUDGET)?;
            return Ok(false);
        }
        if Instant::now() >= deadline {
            let _ = containment.terminate_on_failure_bounded(RECOVERY_CLEANUP_BUDGET, false);
            let _ = child.wait();
            let _ = fs::remove_file(&request_path);
            rollback_recovery_record(run, &before_artifacts, deadline + RECOVERY_CLEANUP_BUDGET)?;
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn write_recovery_record_request(
    run: &homeboy_core::observation::RunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<PathBuf> {
    let root = std::env::temp_dir().join("homeboy-runner-exec-recovery");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", root.display())),
        )
    })?;
    let path = root.join(format!("{}.json", Uuid::new_v4()));
    let staging = path.with_extension("json.staging");
    let body = serde_json::to_vec(&RecoveryRecordRequest {
        run_id: run.id.clone(),
        snapshot: snapshot.clone(),
    })
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize recovery record request".to_string()),
        )
    })?;
    fs::write(&staging, body).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", staging.display())),
        )
    })?;
    fs::rename(&staging, &path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("publish {}", path.display())),
        )
    })?;
    Ok(path)
}

fn rollback_recovery_record(
    original: &homeboy_core::observation::RunRecord,
    before_artifacts: &[homeboy_core::observation::ArtifactRecord],
    cleanup_deadline: Instant,
) -> Result<()> {
    let store = ObservationStore::open_initialized_until(cleanup_deadline)?;
    let before_ids = before_artifacts
        .iter()
        .map(|artifact| &artifact.id)
        .collect::<BTreeSet<_>>();
    for artifact in store.list_artifacts(&original.id)? {
        if !before_ids.contains(&artifact.id) {
            store.delete_artifact_record(&artifact.id)?;
        }
    }
    // The child may have terminalized the source immediately before the owner
    // killed it. Recovery owns this record exclusively, so restoring the exact
    // pre-child row is safe and prevents a partial terminal projection.
    store.upsert_imported_run(original)
}

fn reconcile_terminal_snapshot(
    run: &homeboy_core::observation::RunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
    cwd: &str,
) -> Result<()> {
    homeboy_agents::agent_task_lifecycle::record_runner_exec_terminal_checkpoint(
        &run.id, snapshot,
    )?;
    let declarations = run
        .metadata_json
        .get("runner_exec_artifact_declarations")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let strings = |key: &str| {
        declarations
            .get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let output = recovered_output(run, snapshot, cwd);
    let mut retained = Vec::new();
    for (role, declarations) in [
        ("artifact", strings("artifacts")),
        ("artifact_dir", strings("artifact_dirs")),
        ("summary", strings("summaries")),
    ] {
        for declaration in declarations {
            if homeboy_agents::agent_task_lifecycle::runner_exec_declaration_is_promoted(
                run,
                role,
                &declaration,
            ) {
                continue;
            }
            let promoted = match role {
                "artifact" => promote_runner_exec_artifacts(
                    &run.id,
                    &output,
                    std::slice::from_ref(&declaration),
                )?,
                "artifact_dir" => promote_runner_exec_artifact_dirs(
                    &run.id,
                    &output,
                    std::slice::from_ref(&declaration),
                )?,
                _ => promote_runner_exec_summaries(
                    &run.id,
                    &output,
                    std::slice::from_ref(&declaration),
                )?,
            };
            homeboy_agents::agent_task_lifecycle::record_runner_exec_declaration_promotion(
                &run.id,
                role,
                &declaration,
                &promoted,
            )?;
            retained.extend(promoted);
        }
    }
    homeboy_agents::agent_task_lifecycle::record_runner_exec_artifact_refs(&run.id, &retained)?;
    homeboy_agents::agent_task_lifecycle::project_terminal_runner_result(&run.id, snapshot)?;
    Ok(())
}

fn endpoint_identity(session: &crate::RunnerSession) -> String {
    session
        .remote_daemon_address
        .clone()
        .or_else(|| session.broker_url.clone())
        .unwrap_or_else(|| {
            format!(
                "{:?}:{}",
                session.mode,
                session
                    .remote_daemon_lease_id
                    .as_deref()
                    .unwrap_or_default()
            )
        })
}

/// Complete one accepted recovery owner and leave deferred source records
/// running, preserving their remote-evidence recovery opportunity.
pub fn run_scheduled_terminal_runner_exec_recovery(
    owner_id: &str,
    owner_token: &str,
) -> Result<()> {
    // The owner budget starts before its first durable-store operation. A
    // contended SQLite writer therefore cannot consume an unaccounted window.
    let deadline = Instant::now() + STARTUP_RUNNER_EXEC_RECOVERY_BUDGET;
    let store = ObservationStore::open_initialized_until(deadline)?;
    let Some(owner) = store.get_run(owner_id)? else {
        return Ok(());
    };
    if owner.kind != RECOVERY_KIND || owner.status != RunStatus::Running.as_str() {
        return Ok(());
    }
    if owner.metadata_json["owner_token"].as_str() != Some(owner_token) {
        return Ok(());
    }
    if !store.renew_running_run_lease(
        owner_id,
        owner_token,
        chrono::Utc::now().timestamp_millis() + RECOVERY_OWNER_LEASE.as_millis() as i64,
    )? {
        return Ok(());
    }
    let owner_context = RecoveryOwner {
        id: owner_id.to_string(),
        token: owner_token.to_string(),
        deadline,
    };
    let stop_heartbeat = Arc::new(AtomicBool::new(false));
    let heartbeat_stop = Arc::clone(&stop_heartbeat);
    let heartbeat_owner = owner_context.clone();
    let (heartbeat_done, heartbeat_stop_signal) = mpsc::channel();
    let heartbeat = thread::spawn(move || {
        while !heartbeat_stop.load(Ordering::Acquire) {
            if heartbeat_stop_signal
                .recv_timeout(RECOVERY_LEASE_HEARTBEAT)
                .is_ok()
            {
                break;
            }
            if heartbeat_stop.load(Ordering::Acquire) {
                break;
            }
            let Ok(store) = ObservationStore::open_initialized() else {
                break;
            };
            let Ok(owned) = store.renew_running_run_lease(
                &heartbeat_owner.id,
                &heartbeat_owner.token,
                chrono::Utc::now().timestamp_millis() + RECOVERY_OWNER_LEASE.as_millis() as i64,
            ) else {
                break;
            };
            if !owned {
                break;
            }
        }
    });
    let result = reconcile_terminal_runner_exec_runs_with_owner(
        STARTUP_RUNNER_EXEC_RECOVERY_BUDGET,
        Some(&owner_context),
    );
    stop_heartbeat.store(true, Ordering::Release);
    let _ = heartbeat_done.send(());
    let _ = heartbeat.join();
    let (reconciled, deferred_count) = match result {
        Ok(result) => result,
        Err(error) => {
            if error.details["ownership_lost"].as_bool() == Some(true) {
                return Ok(());
            }
            let Some(owner) = store.get_run(owner_id)? else {
                return Ok(());
            };
            let mut metadata = owner.metadata_json;
            metadata["phase"] = json!("failed");
            metadata["failure"] = json!({
                "schema": "homeboy/runner-exec-recovery-failure/v1",
                "code": format!("{:?}", error.code),
                "message": error.message,
                "details": error.details,
                "retryable": true,
                "next_actions": [format!("homeboy runs show {owner_id}"), "retry the original mutation to schedule a new recovery owner"],
            });
            store.finish_running_run_with_owner_token(
                owner_id,
                owner_token,
                RunStatus::Error,
                metadata,
            )?;
            return Ok(());
        }
    };
    let mut metadata = owner.metadata_json;
    metadata["phase"] = json!(if deferred_count == 0 {
        "completed"
    } else {
        "deferred"
    });
    metadata["reconciled_count"] = json!(reconciled);
    metadata["deferred_count"] = json!(deferred_count);
    metadata["budget_ms"] = json!(STARTUP_RUNNER_EXEC_RECOVERY_BUDGET.as_millis() as u64);
    metadata["inspection_action"] = json!(format!("homeboy runs show {owner_id}"));
    store.finish_running_run_with_owner_token(owner_id, owner_token, RunStatus::Pass, metadata)?;
    Ok(())
}

pub fn record_scheduled_terminal_runner_exec_recovery_spawn_failure(
    owner_id: &str,
    owner_token: &str,
    error: &std::io::Error,
) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let Some(owner) = store.get_run(owner_id)? else {
        return Ok(());
    };
    let mut metadata = owner.metadata_json;
    metadata["phase"] = json!("spawn_failed");
    metadata["spawn_error"] = json!(error.to_string());
    metadata["inspection_action"] = json!(format!("homeboy runs show {owner_id}"));
    store.finish_running_run_with_owner_token(owner_id, owner_token, RunStatus::Error, metadata)?;
    Ok(())
}

fn recovery_candidates(
    store: &ObservationStore,
) -> Result<Vec<homeboy_core::observation::RunRecord>> {
    Ok(store
        .list_runs(RunListFilter {
            kind: Some("runner_execution".to_string()),
            status: Some(RunStatus::Running.as_str().to_string()),
            limit: Some(STARTUP_RUNNER_EXEC_RECOVERY_LIMIT),
            ..RunListFilter::default()
        })?
        .into_iter()
        .filter(|run| {
            run.metadata_json.get("kind").and_then(Value::as_str) == Some("runner_exec")
                && run
                    .metadata_json
                    .get("runner_id")
                    .and_then(Value::as_str)
                    .is_some()
                && run
                    .metadata_json
                    .get("runner_job_id")
                    .and_then(Value::as_str)
                    .is_some()
                && run.cwd.is_some()
        })
        .collect())
}

fn recovered_output(
    run: &homeboy_core::observation::RunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
    cwd: &str,
) -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: snapshot.job.target_runner_id.clone().unwrap_or_default(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: run
            .metadata_json
            .get("remote_command")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        remote_cwd: cwd.to_string(),
        exit_code: if snapshot.job.status == JobStatus::Succeeded {
            0
        } else {
            1
        },
        stdout: String::new(),
        stderr: String::new(),
        source_snapshot: None,
        job: Some(snapshot.job.clone()),
        runner_job: None,
        job_id: Some(snapshot.job.id.to_string()),
        job_events: Some(snapshot.events.clone()),
        mirror_run_id: None,
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        promoted_outputs: Vec::new(),
        structured_summaries: Vec::new(),
        metrics: None,
        capture: None,
        execution_record: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    }
}

fn record_evicted_evidence_loss(
    store: &ObservationStore,
    run: &homeboy_core::observation::RunRecord,
    error: &Error,
) -> Result<()> {
    let checkpointed = run
        .metadata_json
        .pointer("/runner_terminal_projection/state")
        .and_then(Value::as_str)
        == Some("terminal_checkpointed");
    if error.details.get("http_status").and_then(Value::as_u64) != Some(404) {
        return Ok(());
    }
    let has_declarations = run
        .metadata_json
        .get("runner_exec_artifact_declarations")
        .and_then(Value::as_object)
        .is_some_and(|declarations| {
            declarations
                .values()
                .any(|value| value.as_array().is_some_and(|values| !values.is_empty()))
        });
    if !checkpointed && !has_declarations {
        return Ok(());
    }
    let mut metadata = run.metadata_json.clone();
    metadata["runner_terminal_projection"] = json!({
        "state": "unrecoverable_evidence_loss",
        "classification": if checkpointed { "daemon_evicted_missing_declared_artifacts" } else { "daemon_evicted_before_terminal_projection" },
        "runner_id": run.metadata_json["runner_id"],
        "runner_job_id": run.metadata_json["runner_job_id"],
    });
    store.finish_run(&run.id, RunStatus::Fail, Some(metadata))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::test_support::with_isolated_home;
    use homeboy_core::{Error, ErrorCode};
    use std::sync::{Arc, Barrier};

    #[test]
    fn startup_recovery_does_not_materialize_unrelated_active_payloads() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let unrelated = store
                .start_run(NewRunRecord::builder("unrelated").build())
                .expect("unrelated run");
            drop(store);

            let path = homeboy_core::observation::store::database_path().expect("database path");
            let connection = rusqlite::Connection::open(path).expect("open database");
            connection
                .execute("DROP INDEX idx_runs_metadata_retry_of", [])
                .expect("drop metadata expression index");
            connection
                .execute(
                    "UPDATE runs SET metadata_json = 'invalid-json' WHERE id = ?1",
                    [&unrelated.id],
                )
                .expect("corrupt unrelated payload");
            drop(connection);

            assert_eq!(
                reconcile_terminal_runner_exec_runs().expect("bounded recovery"),
                0
            );
        });
    }

    #[test]
    fn startup_recovery_bounds_runner_exec_candidates() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            drop(store);
            let path = homeboy_core::observation::store::database_path().expect("database path");
            let connection = rusqlite::Connection::open(path).expect("open database");
            connection
                .execute("DROP INDEX idx_runs_metadata_retry_of", [])
                .expect("drop metadata expression index");
            for index in 0..STARTUP_RUNNER_EXEC_RECOVERY_LIMIT {
                connection
                    .execute(
                        "INSERT INTO runs(id, kind, started_at, status, metadata_json) VALUES (?1, 'runner_execution', ?2, 'running', '{\"kind\":\"runner_exec\"}')",
                        rusqlite::params![
                            format!("candidate-{index}"),
                            format!("2026-07-30T{:02}:{:02}:00Z", 18 + index / 60, index % 60)
                        ],
                    )
                    .expect("insert candidate");
            }
            connection
                .execute(
                    "INSERT INTO runs(id, kind, started_at, status, metadata_json) VALUES ('outside-budget', 'runner_execution', '2026-01-01T00:00:00Z', 'running', '{\"kind\":\"runner_exec\"}')",
                    [],
                )
                .expect("insert outside-budget candidate");
            drop(connection);

            assert_eq!(
                reconcile_terminal_runner_exec_runs().expect("bounded recovery"),
                0
            );
        });
    }

    #[test]
    fn recovery_owner_accepts_a_hundred_historical_jobs_without_remote_reconciliation() {
        with_isolated_home(|_| {
            for index in 0..STARTUP_RUNNER_EXEC_RECOVERY_LIMIT {
                homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                    &format!("stale-{index}"),
                    "unavailable-runner",
                    &format!("job-{index}"),
                    "/workspace",
                    &["true".to_string()],
                )
                .expect("historical runner job");
            }

            let schedule = schedule_terminal_runner_exec_recovery()
                .expect("schedule recovery")
                .expect("historical jobs need an owner");
            assert_eq!(
                schedule.deferred_count,
                STARTUP_RUNNER_EXEC_RECOVERY_LIMIT as usize
            );
            assert_eq!(
                schedule.budget_ms,
                STARTUP_RUNNER_EXEC_RECOVERY_BUDGET.as_millis() as u64
            );
            assert_eq!(
                schedule.inspection_action,
                format!("homeboy runs show {}", schedule.owner_id)
            );

            // A zero owner budget is a deterministic interruption boundary: no
            // daemon endpoint is contacted and every source record remains
            // recoverable for the next owner.
            assert_eq!(
                reconcile_terminal_runner_exec_runs_with_budget(Duration::ZERO)
                    .expect("bounded recovery"),
                (0, STARTUP_RUNNER_EXEC_RECOVERY_LIMIT as usize)
            );
            let store = ObservationStore::open_initialized().expect("store");
            assert!(store
                .get_run("stale-0")
                .expect("read")
                .expect("historical record")
                .status
                .eq("running"));
        });
    }

    #[test]
    fn expired_owner_deadline_defers_without_starting_a_second_budget() {
        with_isolated_home(|_| {
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                "stale",
                "unavailable-runner",
                "job",
                "/workspace",
                &[],
            )
            .expect("historical runner job");
            let owner = RecoveryOwner {
                id: RECOVERY_OWNER_ID.to_string(),
                token: "test-owner".to_string(),
                deadline: Instant::now() + Duration::from_millis(20),
            };
            thread::sleep(Duration::from_millis(30));

            let started = Instant::now();
            assert_eq!(
                reconcile_terminal_runner_exec_runs_with_owner(
                    Duration::from_secs(5),
                    Some(&owner),
                )
                .expect("expired owner defers source evidence"),
                (0, 1)
            );
            assert!(
                started.elapsed() < Duration::from_millis(100),
                "an expired owner must not recreate a five-second recovery budget"
            );
            let store = ObservationStore::open_initialized().expect("store");
            assert_eq!(
                store
                    .get_run("stale")
                    .expect("read")
                    .expect("source record")
                    .status,
                RunStatus::Running.as_str(),
                "deferred source evidence remains recoverable"
            );
        });
    }

    #[test]
    fn rollback_restores_source_evidence_and_removes_partial_artifact_records() {
        with_isolated_home(|_| {
            let run_id = "rollback-source";
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                run_id,
                "runner",
                "job",
                "/workspace",
                &[],
            )
            .expect("source record");
            let store = ObservationStore::open_initialized().expect("store");
            let original = store.get_run(run_id).expect("read").expect("source");
            let file = tempfile::NamedTempFile::new().expect("artifact file");
            fs::write(file.path(), "partial").expect("write artifact");
            store
                .record_artifact_with_id(
                    run_id,
                    "artifact",
                    file.path(),
                    "partial-artifact",
                    json!({}),
                )
                .expect("record partial artifact");
            let mut partial = store.get_run(run_id).expect("read").expect("source");
            partial.metadata_json["runner_terminal_projection"] = json!({ "state": "projected" });
            store
                .upsert_imported_run(&partial)
                .expect("write partial projection");

            rollback_recovery_record(&original, &[], Instant::now() + Duration::from_secs(1))
                .expect("rollback");

            let restored = store.get_run(run_id).expect("read").expect("source");
            assert_eq!(restored.metadata_json, original.metadata_json);
            assert!(store.list_artifacts(run_id).expect("artifacts").is_empty());
        });
    }

    #[test]
    fn concurrent_schedulers_grant_one_owner_and_one_spawn_entitlement() {
        with_isolated_home(|_| {
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                "stale",
                "unavailable-runner",
                "job",
                "/workspace",
                &[],
            )
            .expect("historical runner job");
            let barrier = Arc::new(Barrier::new(3));
            let mut workers = Vec::new();
            for _ in 0..2 {
                let barrier = Arc::clone(&barrier);
                workers.push(std::thread::spawn(move || {
                    barrier.wait();
                    schedule_terminal_runner_exec_recovery().expect("schedule")
                }));
            }
            barrier.wait();
            let schedules = workers
                .into_iter()
                .map(|worker| worker.join().expect("scheduler thread").expect("owner"))
                .collect::<Vec<_>>();
            assert_eq!(
                schedules
                    .iter()
                    .filter(|schedule| schedule.is_new_owner)
                    .count(),
                1
            );
            assert_eq!(
                schedules
                    .iter()
                    .map(|schedule| &schedule.owner_id)
                    .collect::<BTreeSet<_>>()
                    .len(),
                1
            );
        });
    }

    #[test]
    fn expired_owner_is_taken_over_with_a_new_token() {
        with_isolated_home(|_| {
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                "stale",
                "unavailable-runner",
                "job",
                "/workspace",
                &[],
            )
            .expect("historical runner job");
            let first = schedule_terminal_runner_exec_recovery()
                .expect("schedule")
                .expect("owner");
            let store = ObservationStore::open_initialized().expect("store");
            let mut owner = store
                .get_run(&first.owner_id)
                .expect("read")
                .expect("owner");
            owner.metadata_json["lease_expires_at_ms"] = json!(0);
            store
                .update_run_metadata(&owner.id, owner.metadata_json)
                .expect("expire lease");
            let second = schedule_terminal_runner_exec_recovery()
                .expect("schedule")
                .expect("replacement owner");
            assert!(second.is_new_owner);
            assert_eq!(second.owner_id, first.owner_id);
            assert_ne!(second.owner_token, first.owner_token);
        });
    }

    #[test]
    fn daemon_eviction_preserves_literal_declaration_paths_in_loss_detection() {
        with_isolated_home(|_| {
            let run_id = "evicted-escaped-declaration";
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                run_id,
                "runner",
                "job",
                "/workspace",
                &["true".to_string()],
            )
            .expect("run");
            homeboy_agents::agent_task_lifecycle::record_runner_exec_artifact_declarations(
                run_id,
                &["artifacts/result.json".to_string(), "a~b/c".to_string()],
                &[],
                &[],
            )
            .expect("declarations");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(run_id).expect("read").expect("run");
            let error = Error {
                code: ErrorCode::ValidationInvalidArgument,
                message: "daemon job evicted".to_string(),
                details: json!({ "http_status": 404 }),
                hints: Vec::new(),
                retryable: None,
                source: None,
            };
            record_evicted_evidence_loss(&store, &run, &error).expect("eviction recorded");
            let terminal = store.get_run(run_id).expect("read").expect("terminal run");
            assert_eq!(terminal.status, "fail");
            assert_eq!(
                terminal.metadata_json["runner_terminal_projection"]["classification"],
                "daemon_evicted_before_terminal_projection"
            );
        });
    }
}
