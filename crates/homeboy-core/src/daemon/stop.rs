//! Daemon stop / lease-stop orchestration.
//!
//! Public stop entry points (`stop`, `stop_with_force`, `stop_for_lease`,
//! `force_stop_for_lease`) plus their unlocked/lease-scoped internals and the
//! active-job stop-guard. Extracted from the `daemon` god file. `use super::*`
//! inherits the parent module's state types, lock, and persistence helpers.

use super::*;

pub fn stop() -> Result<DaemonStopResult> {
    stop_with_force(false)
}

/// Stop a daemon only after preserving its active durable work, unless the
/// caller supplied an explicit destructive force request.
pub fn stop_with_force(force: bool) -> Result<DaemonStopResult> {
    let _lock = acquire_daemon_operation_lock()?;
    stop_unlocked_with_force(force)
}

pub fn stop_for_lease(expected_lease_id: &str) -> Result<DaemonStopResult> {
    stop_with_force_for_lease(expected_lease_id, false)
}

/// Terminate a stale or unreachable daemon directly from its persisted lease.
/// This deliberately does not use the daemon HTTP lifecycle endpoint.
pub fn force_stop_for_lease(expected_lease_id: &str) -> Result<DaemonStopResult> {
    let _lock = acquire_daemon_operation_lock()?;
    force_stop_for_lease_unlocked(expected_lease_id)
}

fn force_stop_for_lease_unlocked(expected_lease_id: &str) -> Result<DaemonStopResult> {
    let path = state_path()?;
    let state_path_display = path.display().to_string();
    let validation = validate_lease_file(&path)?;
    if validation.invalid_pid.is_some()
        || (validation.state.is_none() && validation.stale_reason.is_some() && path.exists())
    {
        return Err(corrupt_daemon_lease_error(&path, validation.stale_reason));
    }
    let Some(state) = validation.state else {
        return reconcile_absent_lease_stop(expected_lease_id, state_path_display);
    };
    if state.lease_id != expected_lease_id {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!(
                "forced daemon stop expected lease `{expected_lease_id}` but persisted live lease is `{}`; refusing to signal",
                state.lease_id
            ),
            Some(expected_lease_id.to_string()),
            None,
        ));
    }
    if state.startup_token.is_empty() {
        return Err(Error::validation_invalid_argument(
            "daemon_lease",
            format!(
                "daemon lease at {} does not contain a startup token; refusing to signal pid {}",
                path.display(),
                state.pid
            ),
            Some(path.display().to_string()),
            None,
        ));
    }
    let active_job_ids = active_daemon_job_ids()?;
    if !active_job_ids.is_empty() {
        return Err(active_jobs_block_daemon_stop_error(&state, &active_job_ids));
    }

    let identity = DaemonLeaseIdentity::from_state(&state);
    let current_state = read_lease_if_identity_matches(&path, &identity)?;
    if current_state.pid != state.pid {
        return Err(Error::internal_unexpected(format!(
            "daemon lease changed pid from {} to {}; refusing to signal",
            state.pid, current_state.pid
        )));
    }
    if !pid_is_running(state.pid) {
        remove_lease_if_identity_matches(&path, &identity)?;
        return Ok(DaemonStopResult {
            stopped: false,
            already_absent: true,
            pid: Some(state.pid),
            state_path: state_path_display,
            termination_evidence: None,
        });
    }
    if !pid_has_ownership_token(state.pid, DAEMON_STARTUP_TOKEN_ENV, &state.startup_token)? {
        return Err(Error::validation_invalid_argument(
            "daemon_lease",
            format!(
                "process {} does not own the persisted daemon startup token; refusing to signal a potentially reused PID",
                state.pid
            ),
            Some(state.pid.to_string()),
            None,
        ));
    }

    let _ = read_lease_if_identity_matches(&path, &identity)?;
    terminate_pid_with_sigterm_and_wait(state.pid, FORCE_STOP_WAIT)?;
    let evidence = DaemonTerminationEvidence {
        classification: DaemonTerminationClassification::CleanStop,
        observed_at: chrono::Utc::now().to_rfc3339(),
        lease_id: Some(state.lease_id.clone()),
        pid: Some(state.pid),
        binary_identity: Some(state.build_identity.display.clone()),
        active_jobs: 0,
        resource_evidence: "unavailable: forced stop does not collect OS resource snapshots"
            .to_string(),
        os_evidence: format!(
            "SIGTERM sent to recorded daemon PID and process death verified within {}ms",
            FORCE_STOP_WAIT.as_millis()
        ),
        exit_code: None,
        signal: Some(libc::SIGTERM),
        stdout: None,
        stderr: None,
        stop_requested: true,
    };
    write_termination_evidence(&evidence)?;
    remove_lease_if_identity_matches(&path, &identity)?;
    Ok(DaemonStopResult {
        stopped: true,
        already_absent: false,
        pid: Some(state.pid),
        state_path: state_path_display,
        termination_evidence: Some(evidence),
    })
}

pub(super) fn stop_with_force_for_lease(
    expected_lease_id: &str,
    force: bool,
) -> Result<DaemonStopResult> {
    let _lock = acquire_daemon_operation_lock()?;
    let path = state_path()?;
    let validation = validate_lease_file(&path)?;
    if validation.invalid_pid.is_some()
        || (validation.state.is_none() && validation.stale_reason.is_some() && path.exists())
    {
        return Err(corrupt_daemon_lease_error(&path, validation.stale_reason));
    }
    let Some(state) = validation.state else {
        return reconcile_absent_lease_stop(expected_lease_id, path.display().to_string());
    };
    if state.lease_id != expected_lease_id {
        return Err(Error::validation_invalid_argument(
            "lease_id",
            format!(
                "daemon lifecycle stop expected lease `{expected_lease_id}` but live lease is `{}`; refusing replacement",
                state.lease_id
            ),
            Some(expected_lease_id.to_string()),
            None,
        ));
    }
    // A replacement binary cannot ask a stale daemon to run its own lifecycle
    // endpoint. The persisted lease, token, and zero-job gate below still bind
    // direct termination to this exact daemon owner.
    if !validation.fresh && validation.running {
        return force_stop_for_lease_unlocked(expected_lease_id);
    }
    let mut result = stop_unlocked_with_force(force)?;
    if !result.stopped {
        // A stale-but-live lease is not a successful completion. Re-probe so
        // only disappearance or replacement of the exact requested owner is
        // reported as idempotent absence.
        let status = read_status()?;
        result.already_absent = status
            .state
            .as_ref()
            .is_none_or(|state| state.lease_id != expected_lease_id);
    }
    Ok(result)
}

/// A lease-bound stop can be replayed after a previous stop removed a dead
/// lease. It never signals a process without persisted ownership evidence.
fn reconcile_absent_lease_stop(
    expected_lease_id: &str,
    state_path: String,
) -> Result<DaemonStopResult> {
    let active_job_ids = active_daemon_job_ids()?;
    if !active_job_ids.is_empty() {
        let mut error = Error::validation_invalid_argument(
            "daemon_stop",
            format!(
                "refusing daemon stop for missing lease `{expected_lease_id}` while active durable jobs exist: {}",
                active_job_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(expected_lease_id.to_string()),
            Some(vec![
                "Inspect `homeboy daemon status` and reconcile the active durable jobs before replacing this daemon."
                    .to_string(),
            ]),
        );
        error.details["active_job_ids"] = serde_json::json!(active_job_ids);
        error.details["lifecycle_mutation"] = serde_json::json!("stop");
        return Err(error);
    }
    Ok(DaemonStopResult {
        stopped: false,
        already_absent: true,
        pid: None,
        state_path,
        termination_evidence: None,
    })
}

pub(crate) fn stop_unlocked() -> Result<DaemonStopResult> {
    stop_unlocked_with_force(false)
}

pub(super) fn stop_unlocked_with_force(force: bool) -> Result<DaemonStopResult> {
    let path = state_path()?;
    let state_path_display = path.display().to_string();
    let validation = validate_lease_file(&path)?;
    if validation.invalid_pid.is_some()
        || (validation.state.is_none() && validation.stale_reason.is_some() && path.exists())
    {
        return Err(corrupt_daemon_lease_error(&path, validation.stale_reason));
    }

    let Some(state) = validation.state.as_ref() else {
        return Ok(DaemonStopResult {
            stopped: false,
            already_absent: false,
            pid: None,
            state_path: state_path_display,
            termination_evidence: None,
        });
    };

    if !validation.fresh || !validation.running {
        if !validation.running && path.exists() {
            remove_lease_if_identity_matches(&path, &DaemonLeaseIdentity::from_state(state))?;
        }
        return Ok(DaemonStopResult {
            stopped: false,
            already_absent: false,
            pid: Some(state.pid),
            state_path: state_path_display,
            termination_evidence: None,
        });
    }

    if state.startup_token.is_empty() {
        return Err(Error::validation_invalid_argument(
            "daemon_lease",
            format!(
                "daemon lease at {} does not contain a startup token; refusing to signal pid {}",
                path.display(),
                state.pid
            ),
            Some(path.display().to_string()),
            Some(vec![
                "Start the daemon with `homeboy daemon start` so lifecycle ownership is tokenized"
                    .to_string(),
                format!(
                    "If this lease is stale, remove {} manually after verifying the pid",
                    path.display()
                ),
            ]),
        ));
    }

    let identity = DaemonLeaseIdentity::from_state(state);
    let pid = state.pid;
    let current_state = read_lease_if_identity_matches(&path, &identity)?;
    if current_state.pid != pid {
        return Err(Error::internal_unexpected(format!(
            "daemon lease changed pid from {} to {}; refusing to signal",
            pid, current_state.pid
        )));
    }

    if pid_is_running(pid) {
        let active_job_ids = active_daemon_job_ids()?;
        if !force && !active_job_ids.is_empty() {
            return Err(active_jobs_block_daemon_stop_error(state, &active_job_ids));
        }
        write_termination_evidence(&DaemonTerminationEvidence {
            classification: DaemonTerminationClassification::CleanStop,
            observed_at: chrono::Utc::now().to_rfc3339(),
            lease_id: Some(state.lease_id.clone()),
            pid: Some(pid),
            binary_identity: Some(state.build_identity.display.clone()),
            active_jobs: JobStore::active_count_at_path(paths::daemon_jobs_file()?)?,
            resource_evidence: "unavailable: launcher does not collect OS resource snapshots"
                .to_string(),
            os_evidence:
                "unavailable: no OS termination evidence collected before operator-requested stop"
                    .to_string(),
            exit_code: None,
            signal: None,
            stdout: None,
            stderr: None,
            stop_requested: true,
        })?;
    }
    let stopped = if pid_is_running(pid) {
        terminate_pid(pid)?;
        true
    } else {
        false
    };

    remove_lease_if_identity_matches(&path, &identity)?;

    Ok(DaemonStopResult {
        stopped,
        already_absent: false,
        pid: Some(pid),
        state_path: state_path_display,
        termination_evidence: None,
    })
}

pub(super) fn active_daemon_job_ids() -> Result<Vec<Uuid>> {
    let mut job_ids = JobStore::open_without_reconciliation(paths::daemon_jobs_file()?)?
        .list()
        .into_iter()
        .filter(|job| matches!(job.status, JobStatus::Queued | JobStatus::Running))
        .map(|job| job.id)
        .collect::<Vec<_>>();
    job_ids.sort();
    Ok(job_ids)
}

pub(super) fn active_jobs_block_daemon_stop_error(
    state: &DaemonState,
    active_job_ids: &[Uuid],
) -> Error {
    let current_identity = state.build_identity.display.clone();
    let requested_identity = build_identity::current().display;
    let mut error = Error::validation_invalid_argument(
        "daemon_stop",
        format!(
            "refusing daemon stop for lease `{}` ({current_identity}) while active durable jobs exist: {}. Requested Homeboy identity is `{requested_identity}`; wait for those jobs to finish before stopping the daemon",
            state.lease_id,
            active_job_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
        ),
        Some(state.lease_id.clone()),
        Some(vec!["Inspect `homeboy daemon status` and the listed durable jobs before replacing this daemon.".to_string()]),
    );
    error.details["active_job_ids"] = serde_json::json!(active_job_ids);
    error.details["current_daemon_identity"] = serde_json::json!(current_identity);
    error.details["requested_homeboy_identity"] = serde_json::json!(requested_identity);
    error.details["lifecycle_mutation"] = serde_json::json!("stop");
    error
}
