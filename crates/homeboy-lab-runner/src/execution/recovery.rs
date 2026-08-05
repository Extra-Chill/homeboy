//! Recovery of generic runner-exec evidence after a controller interruption.

use super::*;
use fs4::fs_std::FileExt;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
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
const RECOVERY_CHILD_KIND: &str = "runner_exec_recovery_child";
const RECOVERY_OWNER_LEASE: Duration = Duration::from_secs(30);
const RECOVERY_LEASE_HEARTBEAT: Duration = Duration::from_secs(1);
#[derive(Clone)]
struct RecoveryWorker {
    id: String,
    token: String,
    deadline: Instant,
}

impl RecoveryWorker {
    fn renew(&self, store: &ObservationStore) -> Result<()> {
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
        Ok(())
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

/// A child has one stable durable identity per source run. Reclaiming that
/// identity after a terminal error records a new attempt without ever allowing
/// two active workers for the same source record.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunnerExecRecoveryChildSchedule {
    pub child_id: String,
    pub child_token: String,
    pub source_run_id: String,
    pub inspection_action: String,
}

#[derive(Debug, Clone)]
pub struct RunnerExecRecoveryOwnerWork {
    pub children: Vec<RunnerExecRecoveryChildSchedule>,
    pub deferred_count: usize,
}

/// Reserve a durable, independently inspectable owner before a background
/// recovery worker is spawned. Scheduling only reads local evidence; remote
/// reconciliation belongs to the owner, never to the mutating caller.
pub fn schedule_terminal_runner_exec_recovery() -> Result<Option<RunnerExecRecoverySchedule>> {
    let reader = ObservationStore::open_scheduler_reader()?;
    let candidates = recovery_candidates(&reader)?;
    drop(reader);
    if candidates.is_empty() {
        return Ok(None);
    }
    let store = ObservationStore::open_scheduler_writer()?;
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
        let existing = store.get_run(RECOVERY_OWNER_ID)?;
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

fn reconcile_terminal_runner_exec_runs_with_owner(
    worker: &RecoveryWorker,
    source_run_id: &str,
) -> Result<(usize, usize)> {
    let store = ObservationStore::open_initialized()?;
    let mut reconciled = 0;
    let mut deferred = 0;
    let mut unavailable_endpoints = BTreeSet::new();
    let deadline = worker.deadline;
    let mut sessions = BTreeMap::new();
    let candidates = recovery_candidates(&store)?;
    for (index, run) in candidates.iter().enumerate() {
        if source_run_id != run.id {
            continue;
        }
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
        if worker.deadline <= Instant::now() {
            deferred += candidates.len() - index;
            break;
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
                worker.renew(&store)?;
                record_evicted_evidence_loss(&store, run, &error, &worker.token, job_id)?;
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
        worker.renew(&store)?;
        homeboy_agents::agent_task_lifecycle::record_runner_exec_terminal_checkpoint(
            &run.id, &snapshot,
        )?;
        let declarations = run
            .metadata_json
            .get("runner_exec_artifact_declarations")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let strings = |key: &str| -> Vec<String> {
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
        let output = recovered_output(run, &snapshot, cwd);
        let mut artifacts = Vec::new();
        for declaration in strings("artifacts") {
            if homeboy_agents::agent_task_lifecycle::runner_exec_declaration_is_promoted(
                run,
                "artifact",
                &declaration,
            ) {
                continue;
            }
            worker.renew(&store)?;
            let promoted = promote_runner_exec_artifacts(
                &run.id,
                &output,
                std::slice::from_ref(&declaration),
            )?;
            homeboy_agents::agent_task_lifecycle::record_runner_exec_declaration_promotion(
                &run.id,
                "artifact",
                &declaration,
                &promoted,
            )?;
            artifacts.extend(promoted);
        }
        let mut directories = Vec::new();
        for declaration in strings("artifact_dirs") {
            if homeboy_agents::agent_task_lifecycle::runner_exec_declaration_is_promoted(
                run,
                "artifact_dir",
                &declaration,
            ) {
                continue;
            }
            worker.renew(&store)?;
            let promoted = promote_runner_exec_artifact_dirs(
                &run.id,
                &output,
                std::slice::from_ref(&declaration),
            )?;
            homeboy_agents::agent_task_lifecycle::record_runner_exec_declaration_promotion(
                &run.id,
                "artifact_dir",
                &declaration,
                &promoted,
            )?;
            directories.extend(promoted);
        }
        let mut summaries = Vec::new();
        for declaration in strings("summaries") {
            if homeboy_agents::agent_task_lifecycle::runner_exec_declaration_is_promoted(
                run,
                "summary",
                &declaration,
            ) {
                continue;
            }
            worker.renew(&store)?;
            let promoted = promote_runner_exec_summaries(
                &run.id,
                &output,
                std::slice::from_ref(&declaration),
            )?;
            homeboy_agents::agent_task_lifecycle::record_runner_exec_declaration_promotion(
                &run.id,
                "summary",
                &declaration,
                &promoted,
            )?;
            summaries.extend(promoted);
        }
        let retained = artifacts
            .iter()
            .chain(directories.iter())
            .chain(summaries.iter())
            .cloned()
            .collect::<Vec<_>>();
        worker.renew(&store)?;
        homeboy_agents::agent_task_lifecycle::record_runner_exec_artifact_refs(&run.id, &retained)?;
        worker.renew(&store)?;
        homeboy_agents::agent_task_lifecycle::project_terminal_runner_result(&run.id, &snapshot)?;
        reconciled += 1;
    }
    Ok((reconciled, deferred))
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

/// The five-second owner only admits children. It never contacts a runner or
/// mutates source evidence: each source record gets an independently leased
/// worker that can safely outlive this startup budget.
pub fn run_scheduled_terminal_runner_exec_recovery(
    owner_id: &str,
    owner_token: &str,
) -> Result<Option<RunnerExecRecoveryOwnerWork>> {
    let store = ObservationStore::open_scheduler_writer()?;
    let Some(owner) = store.get_run(owner_id)? else {
        return Ok(None);
    };
    if owner.kind != RECOVERY_KIND || owner.status != RunStatus::Running.as_str() {
        return Ok(None);
    }
    if owner.metadata_json["owner_token"].as_str() != Some(owner_token) {
        return Ok(None);
    }
    if !store.renew_running_run_lease(
        owner_id,
        owner_token,
        chrono::Utc::now().timestamp_millis() + RECOVERY_OWNER_LEASE.as_millis() as i64,
    )? {
        return Ok(None);
    }
    let owner_context = RecoveryWorker {
        id: owner_id.to_string(),
        token: owner_token.to_string(),
        deadline: Instant::now() + STARTUP_RUNNER_EXEC_RECOVERY_BUDGET,
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
            let Ok(store) = ObservationStore::open_scheduler_writer() else {
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
    let result = schedule_recovery_children(&store, &owner_context);
    stop_heartbeat.store(true, Ordering::Release);
    let _ = heartbeat_done.send(());
    let _ = heartbeat.join();
    let (children, deferred_count) = match result {
        Ok(result) => result,
        Err(error) => {
            if error.details["ownership_lost"].as_bool() == Some(true) {
                return Ok(None);
            }
            let Some(owner) = store.get_run(owner_id)? else {
                return Ok(None);
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
            return Ok(None);
        }
    };
    Ok(Some(RunnerExecRecoveryOwnerWork {
        children,
        deferred_count,
    }))
}

/// Complete the owner only after the CLI has accounted for every child spawn.
pub fn finish_scheduled_terminal_runner_exec_recovery(
    owner_id: &str,
    owner_token: &str,
    scheduled_count: usize,
    spawn_failed_count: usize,
    deferred_count: usize,
) -> Result<()> {
    let store = ObservationStore::open_scheduler_writer()?;
    let Some(owner) = store.get_run(owner_id)? else {
        return Ok(());
    };
    let mut metadata = owner.metadata_json;
    metadata["phase"] = json!(if deferred_count + spawn_failed_count == 0 {
        "completed"
    } else {
        "deferred"
    });
    metadata["scheduled_count"] = json!(scheduled_count);
    metadata["spawn_failed_count"] = json!(spawn_failed_count);
    metadata["deferred_count"] = json!(deferred_count);
    metadata["budget_ms"] = json!(STARTUP_RUNNER_EXEC_RECOVERY_BUDGET.as_millis() as u64);
    metadata["inspection_action"] = json!(format!("homeboy runs show {owner_id}"));
    store.finish_running_run_with_owner_token(owner_id, owner_token, RunStatus::Pass, metadata)?;
    Ok(())
}

pub fn record_scheduled_terminal_runner_exec_recovery_child_spawn_failure(
    child: &RunnerExecRecoveryChildSchedule,
    error: &std::io::Error,
) -> Result<()> {
    let store = ObservationStore::open_scheduler_writer()?;
    let Some(record) = store.get_run(&child.child_id)? else {
        return Ok(());
    };
    let mut metadata = record.metadata_json;
    metadata["phase"] = json!("spawn_failed");
    metadata["failure"] = json!({
        "schema": "homeboy/runner-exec-recovery-child-spawn-failure/v1",
        "message": error.to_string(),
        "retryable": true,
        "action": child.inspection_action,
    });
    store.finish_running_run_with_owner_token(
        &child.child_id,
        &child.child_token,
        RunStatus::Error,
        metadata,
    )?;
    Ok(())
}

fn schedule_recovery_children(
    store: &ObservationStore,
    owner: &RecoveryWorker,
) -> Result<(Vec<RunnerExecRecoveryChildSchedule>, usize)> {
    let candidates = recovery_candidates(store)?;
    let mut children = Vec::new();
    let mut deferred = 0;
    for run in candidates {
        if owner.deadline <= Instant::now() {
            deferred += 1;
            continue;
        }
        owner.renew(store)?;
        let Some(runner_job_id) = run.metadata_json["runner_job_id"].as_str() else {
            deferred += 1;
            continue;
        };
        let child_id = format!(
            "runner-exec-recovery-child:{}",
            Uuid::new_v5(&Uuid::NAMESPACE_OID, run.id.as_bytes())
        );
        let child_token = Uuid::new_v4().to_string();
        let lease_expires_at_ms =
            chrono::Utc::now().timestamp_millis() + RECOVERY_OWNER_LEASE.as_millis() as i64;
        let claimed = store.claim_expiring_singleton_run(
            NewRunRecord::builder(RECOVERY_CHILD_KIND)
                .metadata(json!({
                    "phase": "scheduled",
                    "source_run_id": run.id,
                    "parent_owner_id": owner.id,
                    "retryable": true,
                    "inspection_action": format!("homeboy runs show {child_id}"),
                }))
                .build(),
            child_id.clone(),
            &child_token,
            lease_expires_at_ms,
        )?;
        if claimed.is_some() {
            if !store.claim_running_runner_exec_recovery_source(
                &run.id,
                &child_id,
                &child_token,
                runner_job_id,
            )? {
                let child = store.get_run(&child_id)?.expect("claimed child exists");
                let mut metadata = child.metadata_json;
                metadata["phase"] = json!("deferred_source_claim_lost");
                metadata["inspection_action"] = json!(format!("homeboy runs show {child_id}"));
                store.finish_running_run_with_owner_token(
                    &child_id,
                    &child_token,
                    RunStatus::Pass,
                    metadata,
                )?;
                deferred += 1;
                continue;
            }
            children.push(RunnerExecRecoveryChildSchedule {
                child_id: child_id.clone(),
                child_token,
                source_run_id: run.id,
                inspection_action: format!("homeboy runs show {child_id}"),
            });
        } else {
            deferred += 1;
        }
    }
    Ok((children, deferred))
}

/// Execute one durable child. The filesystem lock is deliberately held for the
/// complete side-effecting phase: an expired SQLite lease permits a replacement
/// to *try* takeover, but never to overlap a still-live local process.
pub fn run_scheduled_terminal_runner_exec_recovery_child(
    child_id: &str,
    child_token: &str,
) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let Some(child) = store.get_run(child_id)? else {
        return Ok(());
    };
    if child.kind != RECOVERY_CHILD_KIND
        || child.status != RunStatus::Running.as_str()
        || child.metadata_json["owner_token"].as_str() != Some(child_token)
    {
        return Ok(());
    }
    let source_run_id = match child.metadata_json["source_run_id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            return terminalize_child_error(
                &store,
                child_id,
                child_token,
                &child,
                "missing source run identity",
            )
        }
    };
    let Some(_lock) = try_acquire_child_lock(child_id)? else {
        let mut metadata = child.metadata_json;
        metadata["phase"] = json!("deferred_live_worker");
        metadata["inspection_action"] = json!(format!("homeboy runs show {child_id}"));
        store.finish_running_run_with_owner_token(
            child_id,
            child_token,
            RunStatus::Pass,
            metadata,
        )?;
        return Ok(());
    };
    let worker = RecoveryWorker {
        id: child_id.to_string(),
        token: child_token.to_string(),
        // Child work is not governed by the startup owner's five-second budget.
        deadline: Instant::now() + Duration::from_secs(24 * 60 * 60),
    };
    worker.renew(&store)?;
    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat_stop = Arc::clone(&stop);
    let heartbeat_worker = worker.clone();
    let heartbeat = thread::spawn(move || {
        while !heartbeat_stop.load(Ordering::Acquire) {
            thread::sleep(RECOVERY_LEASE_HEARTBEAT);
            if heartbeat_stop.load(Ordering::Acquire) {
                break;
            }
            let Ok(store) = ObservationStore::open_initialized() else {
                break;
            };
            if heartbeat_worker.renew(&store).is_err() {
                break;
            }
        }
    });
    let result = reconcile_terminal_runner_exec_runs_with_owner(&worker, &source_run_id);
    stop.store(true, Ordering::Release);
    let _ = heartbeat.join();
    match result {
        Ok((reconciled, deferred)) => {
            let Some(child) = store.get_run(child_id)? else {
                return Ok(());
            };
            let mut metadata = child.metadata_json;
            metadata["phase"] = json!(if deferred == 0 {
                "completed"
            } else {
                "deferred"
            });
            metadata["reconciled_count"] = json!(reconciled);
            metadata["deferred_count"] = json!(deferred);
            metadata["inspection_action"] = json!(format!("homeboy runs show {child_id}"));
            store.finish_running_run_with_owner_token(
                child_id,
                child_token,
                RunStatus::Pass,
                metadata,
            )?;
            Ok(())
        }
        Err(error) => {
            terminalize_child_error(&store, child_id, child_token, &child, &error.message)
        }
    }
}

fn terminalize_child_error(
    store: &ObservationStore,
    child_id: &str,
    child_token: &str,
    child: &homeboy_core::observation::RunRecord,
    message: &str,
) -> Result<()> {
    let mut metadata = child.metadata_json.clone();
    metadata["phase"] = json!("error");
    metadata["failure"] = json!({ "message": message, "retryable": true });
    metadata["inspection_action"] = json!(format!("homeboy runs show {child_id}"));
    store.finish_running_run_with_owner_token(child_id, child_token, RunStatus::Error, metadata)?;
    Ok(())
}

fn try_acquire_child_lock(child_id: &str) -> Result<Option<std::fs::File>> {
    let root = homeboy_core::paths::homeboy()?.join("runner-exec-recovery");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", root.display())),
        )
    })?;
    let path = root.join(format!("{child_id}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("open {}", path.display())))
        })?;
    match file.try_lock_exclusive() {
        Ok(true) => Ok(Some(file)),
        Ok(false) => Ok(None),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(format!("lock {}", path.display())),
        )),
    }
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
                && run
                    .metadata_json
                    .pointer("/runner_exec_source_lease/expires_at_ms")
                    .and_then(Value::as_i64)
                    .is_none_or(|expires_at_ms| {
                        expires_at_ms < chrono::Utc::now().timestamp_millis()
                    })
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
    child_token: &str,
    runner_job_id: &str,
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
    store.fail_running_runner_exec_recovery_source(
        &run.id,
        child_token,
        runner_job_id,
        metadata,
    )?;
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

            let reader = ObservationStore::open_scheduler_reader().expect("reader");
            assert!(recovery_candidates(&reader).expect("candidates").is_empty());
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

            let reader = ObservationStore::open_scheduler_reader().expect("reader");
            assert!(
                recovery_candidates(&reader).expect("candidates").len()
                    <= STARTUP_RUNNER_EXEC_RECOVERY_LIMIT as usize
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
    fn active_source_lease_skips_recovery_and_expired_lease_allows_takeover() {
        with_isolated_home(|_| {
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                "foreground",
                "runner",
                "job",
                "/workspace",
                &[],
            )
            .expect("source");
            let store = ObservationStore::open_initialized().expect("store");
            assert!(store
                .claim_running_runner_exec_recovery_source(
                    "foreground",
                    "foreground-runner-exec",
                    "foreground-token",
                    "job",
                )
                .expect("foreground lease"));
            let reader = ObservationStore::open_scheduler_reader().expect("reader");
            assert!(recovery_candidates(&reader).expect("candidates").is_empty());
            drop(reader);
            let mut source = store.get_run("foreground").expect("read").expect("source");
            source.metadata_json["runner_exec_source_lease"]["expires_at_ms"] = json!(0);
            store
                .update_run_metadata("foreground", source.metadata_json)
                .expect("expire foreground lease");
            let reader = ObservationStore::open_scheduler_reader().expect("reader");
            assert_eq!(recovery_candidates(&reader).expect("candidates").len(), 1);
            drop(reader);
            assert!(store
                .claim_running_runner_exec_recovery_source(
                    "foreground",
                    "recovery-child",
                    "recovery-token",
                    "job",
                )
                .expect("expired takeover"));
        });
    }

    #[test]
    fn owner_schedules_one_durable_child_per_source_within_its_budget() {
        with_isolated_home(|_| {
            for index in 0..STARTUP_RUNNER_EXEC_RECOVERY_LIMIT {
                homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                    &format!("source-{index}"),
                    "unavailable-runner",
                    &format!("job-{index}"),
                    "/workspace",
                    &[],
                )
                .expect("source");
            }
            let owner = schedule_terminal_runner_exec_recovery()
                .expect("schedule")
                .expect("owner");
            let started = Instant::now();
            let work =
                run_scheduled_terminal_runner_exec_recovery(&owner.owner_id, &owner.owner_token)
                    .expect("owner schedules children")
                    .expect("owner work");
            let children = work.children;
            assert!(started.elapsed() <= STARTUP_RUNNER_EXEC_RECOVERY_BUDGET);
            assert_eq!(children.len(), STARTUP_RUNNER_EXEC_RECOVERY_LIMIT as usize);
            assert_eq!(
                children
                    .iter()
                    .map(|child| &child.child_id)
                    .collect::<BTreeSet<_>>()
                    .len(),
                STARTUP_RUNNER_EXEC_RECOVERY_LIMIT as usize
            );
            let store = ObservationStore::open_initialized().expect("store");
            for child in children {
                let child = store
                    .get_run(&child.child_id)
                    .expect("read")
                    .expect("child");
                assert_eq!(child.status, RunStatus::Running.as_str());
                assert!(child.metadata_json["source_run_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("source-")));
            }
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
    fn child_failure_is_terminal_and_retains_retryable_evidence() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let child_id = "runner-exec-recovery-child:broken";
            let token = "child-token";
            store
                .claim_expiring_singleton_run(
                    NewRunRecord::builder(RECOVERY_CHILD_KIND)
                        .metadata(json!({ "phase": "scheduled" }))
                        .build(),
                    child_id.to_string(),
                    token,
                    chrono::Utc::now().timestamp_millis() + 60_000,
                )
                .expect("claim child")
                .expect("new child");
            run_scheduled_terminal_runner_exec_recovery_child(child_id, token)
                .expect("terminalize malformed child");
            let child = store.get_run(child_id).expect("read").expect("child");
            assert_eq!(child.status, RunStatus::Error.as_str());
            assert_eq!(child.metadata_json["failure"]["retryable"], true);
        });
    }

    #[test]
    fn child_spawn_failure_is_durable_and_excluded_from_successful_schedule() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let child = RunnerExecRecoveryChildSchedule {
                child_id: "runner-exec-recovery-child:spawn-failure".to_string(),
                child_token: "child-token".to_string(),
                source_run_id: "source".to_string(),
                inspection_action: "homeboy runs show runner-exec-recovery-child:spawn-failure"
                    .to_string(),
            };
            store
                .claim_expiring_singleton_run(
                    NewRunRecord::builder(RECOVERY_CHILD_KIND)
                        .metadata(json!({ "source_run_id": child.source_run_id }))
                        .build(),
                    child.child_id.clone(),
                    &child.child_token,
                    chrono::Utc::now().timestamp_millis() + 60_000,
                )
                .expect("claim child");
            record_scheduled_terminal_runner_exec_recovery_child_spawn_failure(
                &child,
                &std::io::Error::new(std::io::ErrorKind::NotFound, "fixture executable missing"),
            )
            .expect("record spawn failure");
            let child = store
                .get_run(&child.child_id)
                .expect("read")
                .expect("child");
            assert_eq!(child.status, RunStatus::Error.as_str());
            assert_eq!(child.metadata_json["phase"], "spawn_failed");
            assert_eq!(child.metadata_json["failure"]["retryable"], true);
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
            store
                .claim_running_runner_exec_recovery_source(run_id, "child", "token", "job")
                .expect("source claim");
            let error = Error {
                code: ErrorCode::ValidationInvalidArgument,
                message: "daemon job evicted".to_string(),
                details: json!({ "http_status": 404 }),
                hints: Vec::new(),
                retryable: None,
                source: None,
            };
            record_evicted_evidence_loss(&store, &run, &error, "token", "job")
                .expect("eviction recorded");
            let terminal = store.get_run(run_id).expect("read").expect("terminal run");
            assert_eq!(terminal.status, "fail");
            assert_eq!(
                terminal.metadata_json["runner_terminal_projection"]["classification"],
                "daemon_evicted_before_terminal_projection"
            );
        });
    }

    #[test]
    fn evicted_evidence_cannot_overwrite_a_concurrently_settled_source() {
        with_isolated_home(|_| {
            let run_id = "evicted-concurrent-source";
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                run_id,
                "runner",
                "job",
                "/workspace",
                &[],
            )
            .expect("run");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(run_id).expect("read").expect("run");
            store
                .claim_running_runner_exec_recovery_source(run_id, "child", "token", "job")
                .expect("source claim");
            store
                .finish_run(run_id, RunStatus::Pass, Some(run.metadata_json.clone()))
                .expect("concurrent terminal result");
            let error = Error {
                code: ErrorCode::ValidationInvalidArgument,
                message: "daemon job evicted".to_string(),
                details: json!({ "http_status": 404 }),
                hints: Vec::new(),
                retryable: None,
                source: None,
            };
            record_evicted_evidence_loss(&store, &run, &error, "token", "job")
                .expect("stale child does not overwrite source");
            assert_eq!(
                store.get_run(run_id).expect("read").expect("source").status,
                RunStatus::Pass.as_str()
            );
        });
    }
}
