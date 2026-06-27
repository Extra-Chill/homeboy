use super::*;

pub fn cancel_run(run_id: &str, reason: Option<&str>) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if record.state == AgentTaskRunState::Cancelled {
        return Ok(record);
    }

    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialFailure
            | AgentTaskRunState::Failed
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

    // Classify how live cancellation can be performed for this run BEFORE we
    // mutate the durable record, so we can record either a real termination or
    // deterministic operator recovery instructions (acceptance: never force
    // manual process spelunking; always surface pids + safe commands).
    let cancellation = if record.state == AgentTaskRunState::Running {
        classify_live_cancellation(&record)?
    } else {
        LiveCancellationOutcome::NotRunning
    };

    let cancelled_at = now_timestamp();
    let was_stale_running = record.state == AgentTaskRunState::Running;
    record.updated_at = Some(cancelled_at.clone());
    set_run_state(&mut record, AgentTaskRunState::Cancelled);
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = AgentTaskState::Cancelled;
        }
    }

    let metadata = record.ensure_metadata_object();
    metadata.insert("cancelled_at".to_string(), json!(cancelled_at));
    metadata.insert("cancelled_by_pid".to_string(), json!(std::process::id()));
    metadata.insert(
        "cancel_reason".to_string(),
        json!(reason.unwrap_or("cancel requested")),
    );
    metadata.remove("live_cancellation");
    metadata.remove("live_cancellation_unsupported");
    match cancellation {
        LiveCancellationOutcome::Terminated(termination) => {
            metadata.insert(
                "live_cancellation".to_string(),
                json!({
                    "owner_pid": termination.owner_pid,
                    "descendant_pids": termination.descendant_pids,
                    "signalled_pids": termination.signalled_pids,
                    "signal": termination.signal,
                    "killed_pids": termination.killed_pids,
                    "surviving_pids": termination.surviving_pids,
                    "recovery_commands": termination.recovery_commands,
                }),
            );
        }
        LiveCancellationOutcome::Unsupported(unsupported) => {
            metadata.insert(
                "live_cancellation_unsupported".to_string(),
                json!({
                    "reason": unsupported.reason,
                    "owner_pid": unsupported.owner_pid,
                    "runner_id": unsupported.runner_id,
                    "runner_job_id": unsupported.runner_job_id,
                    "recovery_commands": unsupported.recovery_commands,
                }),
            );
        }
        LiveCancellationOutcome::NotRunning => {}
    }
    if was_stale_running {
        metadata.insert("cancelled_stale_running".to_string(), json!(true));
    }
    metadata.remove("stale_running");
    metadata.remove("stale_running_reason");

    store::write_record(&record)?;
    Ok(record)
}

/// Outcome of attempting live cancellation of a running run's provider process
/// tree. Either Homeboy signalled the tree itself, it can only hand the operator
/// deterministic recovery commands (runner-side / non-Unix host), or the run was
/// not actually running.
enum LiveCancellationOutcome {
    Terminated(crate::core::process::ProcessTreeTermination),
    Unsupported(UnsupportedLiveCancellation),
    NotRunning,
}

/// Recovery payload surfaced when Homeboy cannot itself signal the provider
/// process tree (the owner pid lives on a runner host, or no live process is
/// reachable). Carries the recorded identifiers plus copy-pasteable commands so
/// the operator never has to spelunk for child pids.
struct UnsupportedLiveCancellation {
    reason: String,
    owner_pid: Option<u32>,
    runner_id: Option<String>,
    runner_job_id: Option<String>,
    recovery_commands: Vec<String>,
}

fn classify_live_cancellation(record: &AgentTaskRunRecord) -> Result<LiveCancellationOutcome> {
    let owner_pid = record.owner_pid();

    // Local, live owner process: terminate its tree directly (SIGTERM then
    // SIGKILL escalation handled inside terminate_process_tree).
    if let Some(pid) = owner_pid {
        if record.owner_process_is_running() {
            let termination = crate::core::process::terminate_process_tree(pid)?;
            return Ok(LiveCancellationOutcome::Terminated(termination));
        }
    }

    // Runner-backed run whose provider process tree lives on a different host:
    // we cannot signal it from this controller. Emit deterministic recovery
    // commands keyed on the recorded runner + pid instead of failing.
    if record.is_runner_backed() {
        let runner_id = record.runner_id().map(str::to_string);
        let runner_job_id = record.runner_job_id().map(str::to_string);
        let mut recovery_commands = Vec::new();
        if let Some(runner) = runner_id.as_deref() {
            if let Some(job) = runner_job_id.as_deref() {
                recovery_commands.push(format!(
                    "homeboy runner exec {runner} -- homeboy agent-task cancel {} # cancel on the owning runner",
                    record.run_id
                ));
                let _ = job;
            }
        }
        if let Some(pid) = owner_pid {
            recovery_commands.extend(crate::core::process::process_tree_recovery_commands(pid));
        }
        let reason = if owner_pid.is_some() {
            "provider process tree runs on the owning runner host; signal it there"
        } else {
            "runner-backed run has no controller-local owner pid to signal"
        }
        .to_string();
        return Ok(LiveCancellationOutcome::Unsupported(
            UnsupportedLiveCancellation {
                reason,
                owner_pid,
                runner_id,
                runner_job_id,
                recovery_commands,
            },
        ));
    }

    // No reachable live process (stale running record, or no recorded pid): the
    // record is being reclaimed. If a pid was recorded, still hand back recovery
    // commands so a now-orphaned tree can be cleaned up by hand.
    if let Some(pid) = owner_pid {
        return Ok(LiveCancellationOutcome::Unsupported(
            UnsupportedLiveCancellation {
                reason: "recorded owner pid is not running on this host".to_string(),
                owner_pid: Some(pid),
                runner_id: None,
                runner_job_id: None,
                recovery_commands: crate::core::process::process_tree_recovery_commands(pid),
            },
        ));
    }

    Ok(LiveCancellationOutcome::NotRunning)
}

pub fn cancel(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
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

    record.updated_at = Some(now_timestamp());
    set_run_state(&mut record, AgentTaskRunState::Cancelled);
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = AgentTaskState::Cancelled;
        }
    }
    for handle in &mut record.provider_handles {
        if !matches!(
            handle.state,
            Some(AgentTaskState::Succeeded | AgentTaskState::Failed | AgentTaskState::Cancelled)
        ) {
            handle.state = Some(AgentTaskState::Cancelled);
        }
    }
    let metadata = record.ensure_metadata_object();
    metadata.insert("cancel_requested_at".to_string(), json!(now_timestamp()));
    metadata.insert(
        "cancel_note".to_string(),
        json!("provider-specific cancellation is delegated through opaque provider handles"),
    );
    store::write_record(&record)?;
    Ok(record)
}
