use super::*;

#[cfg(test)]
thread_local! {
    static AFTER_INITIAL_CANCELLATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn install_after_initial_cancellation_for_test(hook: impl FnOnce() + 'static) {
    AFTER_INITIAL_CANCELLATION.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

fn run_after_initial_cancellation_for_test() {
    #[cfg(test)]
    AFTER_INITIAL_CANCELLATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

pub fn cancel_run(run_id: &str, reason: Option<&str>) -> Result<AgentTaskRunRecord> {
    cancel_run_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
        reason,
    )
}

/// The store-rooted counterpart of [`cancel_run`].
///
/// Alias resolution and cancellation are one loop: this re-resolves the Cook
/// alias after each pass and cancels whatever became authoritative. Resolving
/// the alias in one installation and cancelling in another would follow an
/// index that names attempts the cancelled store has never heard of — and would
/// terminate the loop on an equality between two different homes' answers
/// (#7505).
pub fn cancel_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    reason: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let requested_run_id = sanitize_run_id(run_id);
    let mut resolved_run_id = resolve_run_id_in_store(lifecycle_store, &requested_run_id)?;
    // Index publication is independent from parent cancellation. Re-resolve
    // after each mutation until the Cook alias is stable, so every attempt that
    // became authoritative during this operation receives cancellation too.
    for pass in 0..3 {
        // Cancellation by Cook ID is idempotent once its indexed attempt has
        // finished. Keep exact-run cancellation strict: an operator targeting
        // that literal terminal attempt still receives the terminal-state error.
        if resolved_run_id != requested_run_id {
            let resolved_record = lifecycle_store.read_record(&resolved_run_id)?;
            if resolved_record.state.is_terminal() {
                return Ok(resolved_record);
            }
        }
        let record = cancel_resolved_run_in_store(lifecycle_store, &resolved_run_id, reason)?;
        // A first child can have been submitted after reservation but before
        // index publication. Cancel it through the reservation link so it
        // cannot remain an unindexed queued record.
        cancel_reserved_detached_cook_handoff_attempt_if_cancelled_in_store(
            lifecycle_store,
            &requested_run_id,
        )?;
        if pass == 0 {
            run_after_initial_cancellation_for_test();
        }
        let resolved_after_cancellation =
            resolve_run_id_in_store(lifecycle_store, &requested_run_id)?;
        if resolved_after_cancellation == resolved_run_id {
            return Ok(record);
        }
        resolved_run_id = resolved_after_cancellation;
    }
    Err(Error::internal_unexpected(format!(
        "Cook alias '{}' changed repeatedly while cancellation was in progress",
        requested_run_id
    )))
}

// The ambient `cancel_exact_run()` shim that used to sit here is gone; its one
// remaining caller was a cancellation test, which now cancels inside the store
// it resolves (#7505).

/// Cancel one literal run through an explicitly selected lifecycle store.
///
/// Reserved handoff reconciliation needs exact-record semantics, but must not
/// cross into another root merely because two test or controller roots use the
/// same run ID.
pub(crate) fn cancel_exact_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    reason: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let committed = lifecycle_store.with_config_lock(|| {
        let record = lifecycle_store.read_record(&run_id)?;
        ensure_rooted_exact_cancellation_supported(&record)?;
        if successful_provider_execution_is_pending_import(&record) {
            let mut blocker = None;
            return lifecycle_store
                .mutate_record_locked_without_terminal_projection(&run_id, |record| {
                    if record.state.is_terminal() {
                        return false;
                    }
                    if let Err(error) = ensure_rooted_exact_cancellation_supported(record) {
                        blocker = Some(error);
                        return false;
                    }
                    defer_cancellation_for_terminal_provider(record, reason)
                })
                .and_then(|record| match blocker {
                    Some(error) => Err(error),
                    None => Ok(record),
                });
        }
        let service_cleanup = if record.metadata.get("managed_service_supervisor").is_some() {
            let services = crate::agent_task_scheduler::managed_services::reconcile_run_services_at(
                &lifecycle_store.data_root(),
                &record.run_id,
                reason.unwrap_or("cancelled"),
            )
            .map_err(Error::internal_unexpected)?;
            Some(json!({ "transport": "local", "services": services }))
        } else {
            None
        };
        let cancellation = if record.state == AgentTaskRunState::Running
            || (record.state == AgentTaskRunState::Queued
                && record.runner_id().is_some()
                && record.runner_job_id().is_some())
        {
            classify_live_cancellation(&record)?
        } else {
            LiveCancellationOutcome::NotRunning
        };
        if let LiveCancellationOutcome::RunnerJobCancelled { job, .. } = &cancellation {
            if job.status.is_terminal() && job.status != homeboy_core::api_jobs::JobStatus::Cancelled {
                return Err(Error::validation_invalid_argument(
                    "run_id",
                    "runner completed before rooted cancellation could be projected; refusing to overwrite its terminal result",
                    Some(record.run_id),
                    None,
                ));
            }
        }
        let mut blocker = None;
        lifecycle_store.mutate_record_locked_without_terminal_projection(&run_id, |record| {
            if record.state.is_terminal() {
                return false;
            }
            if let Err(error) = ensure_rooted_exact_cancellation_supported(record) {
                blocker = Some(error);
                return false;
            }
            if successful_provider_execution_is_pending_import(record) {
                return defer_cancellation_for_terminal_provider(record, reason);
            }
            let cancelled_at = now_timestamp();
            record.updated_at = Some(cancelled_at.clone());
            set_run_state(record, AgentTaskRunState::Cancelled);
            for task in &mut record.tasks {
                if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
                    task.state = AgentTaskState::Cancelled;
                }
            }
            let runner_id = record.runner_id().map(str::to_string);
            let runner_job_id = record.runner_job_id().map(str::to_string);
            let metadata = record.ensure_metadata_object();
            terminalize_running_provider_executions(metadata, &cancelled_at);
            metadata.insert("cancelled_at".to_string(), json!(cancelled_at));
            metadata.insert("cancelled_by_pid".to_string(), json!(std::process::id()));
            metadata.insert(
                "cancel_reason".to_string(),
                json!(reason.unwrap_or("cancel requested")),
            );
            if let Some(service_cleanup) = service_cleanup.clone() {
                metadata.insert("managed_service_cleanup".to_string(), service_cleanup);
            }
            match &cancellation {
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
                LiveCancellationOutcome::RunnerJobCancelled { job, events } => {
                    metadata.insert(
                        "live_cancellation".to_string(),
                        json!({
                            "runner_id": runner_id,
                            "runner_job_id": runner_job_id,
                            "runner_job_status": job.status,
                            "runner_job_events": events,
                            "cancellation": "runner_job_cancel",
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
            true
        })
        .and_then(|record| match blocker {
            Some(error) => Err(error),
            None => Ok(record),
        })
    })?;
    if let Some(record) = committed.as_ref() {
        lifecycle_store.project_terminal_record_after_unlock(&record.run_id)?;
    }
    let record = committed.unwrap_or(lifecycle_store.read_record(&run_id)?);
    if record.state == AgentTaskRunState::Cancelled {
        crate::controller_scratch::finalize_run_at_explicit_roots(
            &lifecycle_store.data_root(),
            &lifecycle_store.artifact_root(),
            &run_id,
        )?;
        homeboy_core::controller_runtime::cancel_admission_at(
            &lifecycle_store.data_root().join("controller-runtimes"),
            &run_id,
        )?;
    }
    Ok(record)
}

pub(super) fn ensure_rooted_exact_cancellation_supported(
    record: &AgentTaskRunRecord,
) -> Result<()> {
    if record.state.is_terminal() {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!(
                "agent-task run '{}' is already terminal with state {:?}",
                record.run_id, record.state
            ),
            Some(record.run_id.clone()),
            None,
        ));
    }
    // These paths bind or project durable lifecycle records through ambient
    // stores today. Refuse before any lifecycle mutation rather than splitting
    // ownership across roots.
    if record.candidate_adoption.as_ref().is_some_and(|attempt| {
        attempt.is_active()
            || attempt.state == "cancel_requested"
            || attempt.phase == "gate_orphaned"
    }) || record
        .metadata
        .get("lab_staging_controller_job_id")
        .is_some()
        || record
            .metadata
            .pointer("/runner_submission_intent/state")
            .and_then(Value::as_str)
            == Some("pending")
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "rooted exact cancellation cannot reconcile controller-owned or pending runner work without an explicit lifecycle-store transport",
            Some(record.run_id.clone()),
            None,
        ));
    }
    if record
        .metadata
        .get("managed_service_supervisor")
        .and_then(Value::as_object)
        .and_then(|owner| owner.get("runner_id"))
        .is_some()
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "rooted exact cancellation cannot reconcile runner-owned managed services without an explicit lifecycle-store transport",
            Some(record.run_id.clone()),
            None,
        ));
    }
    Ok(())
}

fn defer_cancellation_for_terminal_provider(
    record: &mut AgentTaskRunRecord,
    reason: Option<&str>,
) -> bool {
    record.ensure_metadata_object().insert(
        "cancellation_deferred_for_terminal_provider".to_string(),
        json!({
            "requested_at": now_timestamp(),
            "reason": reason.unwrap_or("cancel requested"),
        }),
    );
    true
}

fn successful_provider_execution_is_pending_import(record: &AgentTaskRunRecord) -> bool {
    record.state == AgentTaskRunState::Running
        && !record.is_runner_backed()
        && record.metadata["provider_executions"]
            .as_array()
            .is_some_and(|executions| {
                executions
                    .iter()
                    .any(|execution| execution["state"] == json!("succeeded"))
            })
}

/// Cancel one already-resolved durable run inside an explicitly rooted store.
///
/// Cancellation is destructive and it is not one write: it terminalizes the
/// record, reaps the run's managed services, cancels a controller staging job,
/// projects a runner-job cancellation, finalizes controller scratch, and
/// releases the controller-runtime admission. Every one of those is keyed on
/// the run and every one of them was ambient. A cancel that read state from one
/// home and wrote its terminal record to another is the worst shape in this
/// bug class — it would leave the *observed* run running with its services and
/// admission intact while reporting success (#7505).
///
/// There is no ambient wrapper: both entry points ([`cancel_run_in_store`] and
/// [`cancel_exact_run`]) resolve exactly one store for the whole operation.
fn cancel_resolved_run_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    reason: Option<&str>,
) -> Result<AgentTaskRunRecord> {
    // Cook IDs are stable aliases. Match status and logs by following an
    // materialized attempt once one exists; before then the handoff parent is
    // the direct record and remains cancellable.
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    let detached_child = record
        .metadata
        .get("detached_cook_handoff")
        .and_then(serde_json::Value::as_object)
        .filter(|handoff| {
            handoff.get("state").and_then(serde_json::Value::as_str) == Some("pending")
        })
        .and_then(|handoff| {
            Some((
                u32::try_from(handoff.get("child_pid")?.as_u64()?).ok()?,
                handoff.get("child_start_identity").cloned(),
            ))
        });
    if let Some((pid, start_identity)) = detached_child {
        let start_identity = start_identity.and_then(|value| serde_json::from_value(value).ok());
        let Some(start_identity) = start_identity else {
            return Err(Error::validation_invalid_argument(
                "detached_cook_handoff.child_start_identity",
                "detached Cook child has no verifiable start identity",
                Some(record.run_id.clone()),
                None,
            ));
        };
        match homeboy_core::process::process_identity_state_with_start_identity(
            pid,
            None,
            Some(&start_identity),
        ) {
            homeboy_core::process::ProcessIdentityState::Live => {
                let termination = homeboy_core::process::terminate_process_tree(pid)?;
                record.ensure_metadata_object().insert(
                    "detached_cook_handoff_cancellation".to_string(),
                    json!({
                        "child_pid": pid,
                        "signal": termination.signal,
                        "signalled_pids": termination.signalled_pids,
                        "killed_pids": termination.killed_pids,
                    }),
                );
                lifecycle_store.write_record(&record)?;
            }
            homeboy_core::process::ProcessIdentityState::Dead => {}
            homeboy_core::process::ProcessIdentityState::IdentityMismatch
            | homeboy_core::process::ProcessIdentityState::Unverifiable => {
                return Err(Error::validation_invalid_argument(
                    "detached_cook_handoff.child_start_identity",
                    "refusing to signal a detached Cook child without an exact process identity match",
                    Some(record.run_id.clone()),
                    None,
                ));
            }
        }
    }
    // Service ownership is independent of the scheduler process. Reconcile the
    // durable ledger before terminalizing so an interrupted controller cannot
    // leave a runner-local preview alive after its run is cancelled.
    let service_cleanup =
        crate::agent_task_scheduler::managed_services::reconcile_run_services_on_owner_at(
            &lifecycle_store.data_root(),
            &record.run_id,
            record.metadata.get("managed_service_supervisor"),
            reason.unwrap_or("cancelled"),
        )
        .map_err(Error::internal_unexpected)?;
    if service_cleanup != serde_json::json!({ "transport": "local", "services": [] }) {
        record
            .ensure_metadata_object()
            .insert("managed_service_cleanup".to_string(), service_cleanup);
        lifecycle_store.write_record(&record)?;
    }
    if record
        .candidate_adoption
        .as_ref()
        .filter(|attempt| {
            attempt.is_active()
                || attempt.state == "cancel_requested"
                || attempt.phase == "gate_orphaned"
        })
        .is_some()
    {
        let process_group = record
            .candidate_adoption
            .as_ref()
            .and_then(|attempt| attempt.gate_process_group);
        let reason = reason.unwrap_or("cancel requested").to_string();
        let now = now_timestamp();
        let attempt = record.candidate_adoption.as_mut().expect("active adoption");
        attempt.state = "cancel_requested".to_string();
        attempt.phase = "cancelling_gate".to_string();
        attempt.terminal_error = Some(reason.clone());
        attempt.updated_at = now.clone();
        attempt.heartbeat_at = now.clone();
        record.ensure_metadata_object().insert(
            "candidate_adoption_cancel_requested_at".to_string(),
            json!(now),
        );
        record.updated_at = Some(now);
        lifecycle_store.write_record(&record)?;

        if let Some(process_group) = process_group {
            if homeboy_core::process::isolated_process_group_is_running(process_group).map_err(
                |error| {
                    Error::internal_unexpected(format!(
                        "inspect adoption gate process group: {error}"
                    ))
                },
            )? {
                homeboy_core::process::terminate_isolated_process_group(process_group)?;
                if !homeboy_core::process::wait_for_isolated_process_group_exit(
                    process_group,
                    std::time::Duration::from_secs(2),
                )
                .map_err(|error| {
                    Error::internal_unexpected(format!(
                        "verify adoption gate process group termination: {error}"
                    ))
                })? {
                    return Err(Error::internal_unexpected(format!(
                        "adoption gate process group {process_group} remains alive after cancellation"
                    )));
                }
            }
        }
        let now = now_timestamp();
        let attempt = record.candidate_adoption.as_mut().expect("active adoption");
        attempt.state = "cancelled".to_string();
        attempt.phase = "terminal".to_string();
        attempt.terminal_error = Some(reason);
        attempt.completed_at = Some(now.clone());
        attempt.updated_at = now.clone();
        attempt.heartbeat_at = now.clone();
        record.updated_at = Some(now);
        lifecycle_store.write_record(&record)?;
        return Ok(record);
    }
    // A pending POST may have been accepted despite a lost response. Resolve
    // its key before cancellation so the original job is cancelled rather than
    // left running on the runner. Preparing intents have no replay request and
    // deliberately do not reach this lookup.
    if matches!(
        record.state,
        AgentTaskRunState::Queued | AgentTaskRunState::Running
    ) && super::lab_handoff_reconciliation::bind_pending_runner_submission_if_accepted_in_store(
        lifecycle_store,
        &record.run_id,
    )? {
        record = lifecycle_store.read_record(&record.run_id)?;
    }
    if record.state == AgentTaskRunState::Cancelled {
        let cancelled_at = record
            .metadata
            .get("cancelled_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| record.updated_at.as_deref().unwrap_or("unknown"))
            .to_string();
        terminalize_running_provider_executions(record.ensure_metadata_object(), &cancelled_at);
        lifecycle_store.write_record(&record)?;
        crate::controller_scratch::finalize_run_at_explicit_roots(
            &lifecycle_store.data_root(),
            &lifecycle_store.artifact_root(),
            &record.run_id,
        )?;
        return Ok(record);
    }

    if matches!(
        record.state,
        AgentTaskRunState::Succeeded
            | AgentTaskRunState::PartialRecoverable
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

    // A provider can return in the narrow window before its scheduler persists
    // the aggregate. Its terminal reservation is durable proof that cancelling
    // now would overwrite completed work; leave the run joinable so the late
    // aggregate import is authoritative.
    if record.state == AgentTaskRunState::Running
        && !record.is_runner_backed()
        && record.metadata["provider_executions"]
            .as_array()
            .is_some_and(|executions| {
                executions
                    .iter()
                    .any(|execution| execution["state"] == json!("succeeded"))
            })
    {
        let now = now_timestamp();
        record.ensure_metadata_object().insert(
            "cancellation_deferred_for_terminal_provider".to_string(),
            json!({ "requested_at": now, "reason": reason.unwrap_or("cancel requested") }),
        );
        lifecycle_store.write_record(&record)?;
        return Ok(record);
    }

    // Staging is controller-local work, not a runner child. Persist the request
    // and its owner before asking the daemon to stop it; provider shutdown can
    // take arbitrarily long after the observing CLI has gone away.
    let controller_cancelled = if let Some(controller_job_id) = record
        .metadata
        .get("lab_staging_controller_job_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_string)
    {
        let cancellation_reason = reason.unwrap_or("agent-task cancellation requested");
        let metadata = record.ensure_metadata_object();
        metadata.insert(
            "controller_job_cancellation".to_string(),
            json!({
                "controller_job_id": controller_job_id,
                "phase": "requesting",
                "reason": cancellation_reason,
                "requested_at": now_timestamp(),
            }),
        );
        lifecycle_store.write_record(&record)?;
        // The controller owns every child admitted during staging, including the
        // final runner job. Never bypass it merely because that child identity
        // was already projected onto the parent.
        let controller_job = homeboy_core::daemon::LocalControllerJobClient::connect()?
            .cancel(&controller_job_id, cancellation_reason)?;
        let metadata = record.ensure_metadata_object();
        metadata.insert(
            "controller_job_cancellation".to_string(),
            json!({
                "controller_job_id": controller_job.id,
                "status": controller_job.status,
                "phase": if controller_job.status == homeboy_core::api_jobs::JobStatus::Cancelled { "cancelled" } else { "requested" },
                "reason": cancellation_reason,
                "requested_at": now_timestamp(),
            }),
        );
        lifecycle_store.write_record(&record)?;
        if controller_job.status != homeboy_core::api_jobs::JobStatus::Cancelled {
            return Ok(record);
        }
        true
    } else {
        false
    };

    // Classify how live cancellation can be performed for this run BEFORE we
    // mutate the durable record, so we can record either a real termination or
    // deterministic operator recovery instructions (acceptance: never force
    // manual process spelunking; always surface pids + safe commands).
    // A runner-backed proxy can have an accepted daemon job while it is still
    // queued before the provider starts. Project cancellation to that job too.
    let cancellation = if controller_cancelled {
        LiveCancellationOutcome::NotRunning
    } else if record.state == AgentTaskRunState::Running
        || (record.state == AgentTaskRunState::Queued
            && record.runner_id().is_some()
            && record.runner_job_id().is_some())
    {
        classify_live_cancellation(&record)?
    } else {
        LiveCancellationOutcome::NotRunning
    };
    // A runner cancellation can race with the daemon publishing its terminal
    // result. The daemon outcome is authoritative: project it rather than
    // overwriting completed work with a controller-only cancellation.
    if let LiveCancellationOutcome::RunnerJobCancelled { job, events } = &cancellation {
        if job.status.is_terminal() && job.status != homeboy_core::api_jobs::JobStatus::Cancelled {
            reconcile_runner_job_snapshot_in_store(
                lifecycle_store,
                &mut record,
                &homeboy_core::api_jobs::RunnerJobLogSnapshot {
                    job: *job.clone(),
                    events: events.clone(),
                },
            )?;
            return Ok(record);
        }
    }
    let runner_id = record.runner_id().map(str::to_string);
    let runner_job_id = record.runner_job_id().map(str::to_string);

    // The provider can publish its terminal reservation after the initial read
    // above. Make the final decision under the record mutation lock so a late
    // success is either observed here or cancellation wins before it can be
    // imported; never overwrite an already durable success.
    let record = lifecycle_store.mutate_record(&record.run_id, |record| {
        // An aggregate or runner projection may have finished after the live
        // cancellation transport returned. Its terminal state is authoritative.
        if record.state.is_terminal() {
            return false;
        }
        if record.state == AgentTaskRunState::Running
            && !record.is_runner_backed()
            && record.metadata["provider_executions"]
                .as_array()
                .is_some_and(|executions| {
                    executions
                        .iter()
                        .any(|execution| execution["state"] == json!("succeeded"))
                })
        {
            record.ensure_metadata_object().insert(
                "cancellation_deferred_for_terminal_provider".to_string(),
                json!({ "requested_at": now_timestamp(), "reason": reason.unwrap_or("cancel requested") }),
            );
            return true;
        }

        let cancelled_at = now_timestamp();
        let was_stale_running = record.state == AgentTaskRunState::Running;
        record.updated_at = Some(cancelled_at.clone());
        set_run_state(record, AgentTaskRunState::Cancelled);
        for task in &mut record.tasks {
            if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
                task.state = AgentTaskState::Cancelled;
            }
        }

        let detached_handoff_parent =
            record.metadata["detached_cook_handoff"]["cook_id"] == record.run_id;
        let metadata = record.ensure_metadata_object();
        terminalize_running_provider_executions(metadata, &cancelled_at);
        metadata.insert("cancelled_at".to_string(), json!(cancelled_at));
        metadata.insert("cancelled_by_pid".to_string(), json!(std::process::id()));
        metadata.insert(
            "cancel_reason".to_string(),
            json!(reason.unwrap_or("cancel requested")),
        );
        if detached_handoff_parent {
            metadata["detached_cook_handoff"]["cancellation_fence"]["state"] =
                json!("cancelled");
        }
        metadata.remove("live_cancellation");
        metadata.remove("live_cancellation_unsupported");
        match &cancellation {
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
            LiveCancellationOutcome::RunnerJobCancelled { job, events } => {
                metadata.insert(
                    "live_cancellation".to_string(),
                    json!({
                        "runner_id": runner_id,
                        "runner_job_id": runner_job_id,
                        "runner_job_status": job.status,
                        "runner_job_events": events,
                        "cancellation": "runner_job_cancel",
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
            metadata.insert(
                METADATA_KEY_CANCELLED_STALE_RUNNING.to_string(),
                json!(true),
            );
        }
        metadata.remove(METADATA_KEY_STALE_RUNNING);
        metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
        true
    })?
    .unwrap_or(lifecycle_store.read_record(&record.run_id)?);
    if record.state == AgentTaskRunState::Cancelled {
        // The same two rooted side effects `cancel_exact_run_in_store` already
        // uses: scratch finalization needs both the data and artifact roots,
        // and admission release needs the controller-runtime store below this
        // store's data root.
        crate::controller_scratch::finalize_run_at_explicit_roots(
            &lifecycle_store.data_root(),
            &lifecycle_store.artifact_root(),
            &record.run_id,
        )?;
        homeboy_core::controller_runtime::cancel_admission_at(
            &lifecycle_store.data_root().join("controller-runtimes"),
            &record.run_id,
        )?;
    }
    Ok(record)
}

/// Reconcile an asynchronously cancelled controller-owned staging job. This is
/// read-side so a CLI that observed only the acknowledgement still converges the
/// durable cook record after the provider exits.
///
/// The two side effects at the end — controller-scratch finalization and
/// admission cancellation — are the reason this takes a store. Both are durable
/// writes keyed on the run, and both had ambient forms that resolve their own
/// roots: `finalize_run` opens `paths::homeboy_data()`'s scratch index and
/// `paths::artifact_root()`, and `cancel_admission` opens the ambient runtime
/// root. A read reconciling a record from injected roots would have marked
/// another home's scratch resources finalized and released another home's
/// admission (#7505). This mirrors `cancel_exact_run_in_store`, which already
/// uses exactly these two rooted forms.
///
/// The caller mutates `record` in place and persists it itself, so no record
/// write happens here. There is no ambient wrapper: `reconcile_status_in_store` is the
/// only caller, and the store it hands down is the one its own caller
/// injected.
pub(super) fn reconcile_controller_job_cancellation_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
) -> Result<bool> {
    let Some(cancellation) = record
        .metadata
        .get("controller_job_cancellation")
        .and_then(serde_json::Value::as_object)
    else {
        return Ok(false);
    };
    if cancellation
        .get("phase")
        .and_then(serde_json::Value::as_str)
        != Some("requested")
    {
        return Ok(false);
    }
    let Some(job_id) = cancellation
        .get("controller_job_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
    else {
        return Ok(false);
    };

    let job = match homeboy_core::daemon::LocalControllerJobClient::connect()
        .and_then(|client| client.status(job_id))
    {
        Ok(job) => job,
        Err(error) => {
            record.ensure_metadata_object().insert(
                "controller_job_cancellation_status_error".to_string(),
                json!({ "checked_at": now_timestamp(), "error": error.message }),
            );
            return Ok(true);
        }
    };
    let metadata = record.ensure_metadata_object();
    metadata.remove("controller_job_cancellation_status_error");
    if job.status != homeboy_core::api_jobs::JobStatus::Cancelled {
        metadata.insert(
            "controller_job_cancellation".to_string(),
            json!({
                "controller_job_id": job.id,
                "status": job.status,
                "phase": "requested",
                "last_checked_at": now_timestamp(),
            }),
        );
        return Ok(true);
    }

    let cancelled_at = now_timestamp();
    set_run_state(record, AgentTaskRunState::Cancelled);
    for task in &mut record.tasks {
        if matches!(task.state, AgentTaskState::Queued | AgentTaskState::Running) {
            task.state = AgentTaskState::Cancelled;
        }
    }
    let metadata = record.ensure_metadata_object();
    terminalize_running_provider_executions(metadata, &cancelled_at);
    metadata.insert("cancelled_at".to_string(), json!(cancelled_at));
    metadata.insert(
        "controller_job_cancellation".to_string(),
        json!({
            "controller_job_id": job.id,
            "status": job.status,
            "phase": "cancelled",
            "confirmed_at": now_timestamp(),
        }),
    );
    metadata.remove(METADATA_KEY_STALE_RUNNING);
    metadata.remove(METADATA_KEY_STALE_RUNNING_REASON);
    record.updated_at = Some(now_timestamp());
    crate::controller_scratch::finalize_run_at_explicit_roots(
        &lifecycle_store.data_root(),
        &lifecycle_store.artifact_root(),
        &record.run_id,
    )?;
    homeboy_core::controller_runtime::cancel_admission_at(
        &lifecycle_store.data_root().join("controller-runtimes"),
        &record.run_id,
    )?;
    Ok(true)
}

/// Outcome of attempting live cancellation of a running run's provider process
/// tree. Either Homeboy signalled the tree itself, it can only hand the operator
/// deterministic recovery commands (runner-side / non-Unix host), or the run was
/// not actually running.
enum LiveCancellationOutcome {
    Terminated(homeboy_core::process::ProcessTreeTermination),
    RunnerJobCancelled {
        job: Box<homeboy_core::api_jobs::Job>,
        events: Vec<homeboy_core::api_jobs::JobEvent>,
    },
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

    // Runner-backed run whose provider process tree lives on a different host:
    // its accepted daemon job is authoritative over any controller-local PID.
    // A PID left by the caller can be stale or reused, so it must never be
    // signalled instead of the owning runner job.
    if record.is_runner_backed() {
        let runner_id = record.runner_id().map(str::to_string);
        let runner_job_id = record.runner_job_id().map(str::to_string);
        let runner_removed = runner_id.as_deref().is_some_and(|runner_id| {
            super::runner_continuation::with_runner_continuation(|provider| {
                provider.runner_authority(runner_id) == RunnerAuthority::Removed
            })
        });
        if runner_removed || record.is_locally_reconcilable_after_runner_idle() {
            // Removed authority or authoritative idle evidence for exact
            // zero-work queued residue means no remote job remains to cancel.
            return Ok(LiveCancellationOutcome::NotRunning);
        }
        if let (Some(runner_id), Some(runner_job_id)) =
            (runner_id.as_deref(), runner_job_id.as_deref())
        {
            match cancel_runner_job(runner_id, runner_job_id, &record.run_id) {
                Ok((job, events)) => {
                    return Ok(LiveCancellationOutcome::RunnerJobCancelled {
                        job: Box::new(job),
                        events,
                    });
                }
                // An accepted Lab handoff has an authoritative remote owner.
                // Leaving it active while only cancelling this projection loses
                // the result, so propagate the failure before mutating the run.
                Err(error) if record.has_accepted_lab_handoff() => return Err(error),
                Err(_) => {}
            }
        }
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
            recovery_commands.extend(homeboy_core::process::process_tree_recovery_commands(pid));
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

    // Local, live owner process: terminate its tree directly (SIGTERM then
    // SIGKILL escalation handled inside terminate_process_tree).
    if let Some(pid) = owner_pid {
        if record.owner_process_is_running() {
            let termination = homeboy_core::process::terminate_process_tree(pid)?;
            return Ok(LiveCancellationOutcome::Terminated(termination));
        }
    }

    // No reachable live process (stale running record, or no recorded pid): the
    // record is being reclaimed. A dead PID is authoritative absence, so do not
    // recommend signals that cannot execute against it.
    if let Some(pid) = owner_pid {
        return Ok(LiveCancellationOutcome::Unsupported(
            UnsupportedLiveCancellation {
                reason: "recorded owner pid is not running on this host".to_string(),
                owner_pid: Some(pid),
                runner_id: None,
                runner_job_id: None,
                recovery_commands: Vec::new(),
            },
        ));
    }

    Ok(LiveCancellationOutcome::NotRunning)
}

fn cancel_runner_job(
    runner_id: &str,
    runner_job_id: &str,
    durable_run_id: &str,
) -> Result<(
    homeboy_core::api_jobs::Job,
    Vec<homeboy_core::api_jobs::JobEvent>,
)> {
    #[cfg(test)]
    if let Some(result) = test_cancel_hook::take(runner_id, runner_job_id, durable_run_id) {
        return result;
    }

    homeboy_core::observation::runs_service::runner_evidence::with_runner_evidence(|p| {
        p.runner_job_cancel_projection(runner_id, runner_job_id, durable_run_id)
    })
}

pub fn cancel(run_id: &str) -> Result<AgentTaskRunRecord> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
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

    let cancelled_at = now_timestamp();
    record.updated_at = Some(cancelled_at.clone());
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
    terminalize_running_provider_executions(metadata, &cancelled_at);
    metadata.insert("cancel_requested_at".to_string(), json!(cancelled_at));
    metadata.insert(
        "cancel_note".to_string(),
        json!("provider-specific cancellation is delegated through opaque provider handles"),
    );
    store::write_record(&record)?;
    crate::controller_scratch::finalize_run(&record.run_id)?;
    Ok(record)
}

fn terminalize_running_provider_executions(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    finished_at: &str,
) {
    let Some(executions) = metadata
        .get_mut("provider_executions")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for execution in executions {
        if execution["state"] == "running" {
            execution["state"] = json!("cancelled");
            execution["finished_at"] = json!(finished_at);
        }
    }
}

#[cfg(test)]
mod tests {

    /// Tests are the entry point for their own unit of work, so the store
    /// resolves once here (#7505).
    fn test_lifecycle_store() -> crate::agent_task_lifecycle::AgentTaskLifecycleStore {
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("lifecycle store")
    }
    use super::*;

    #[test]
    fn cancellation_fence_rejects_a_cook_index_published_after_cancellation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-index-switch-during-cancel";
            record_detached_cook_handoff_parent_in_store(&test_lifecycle_store(), cook_id)
                .expect("persist handoff parent");
            let attempt_id = "cook-index-switch-during-cancel-attempt-1";
            let plan = AgentTaskPlan::new("index-switch-attempt", Vec::new());
            submit_plan(&plan, Some(attempt_id)).expect("persist attempt");
            let cancelled = cancel_run(cook_id, None).expect("cancel detached Cook parent");

            assert_eq!(cancelled.run_id, cook_id);
            assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
            let error =
                record_cook_attempt_in_store(&test_lifecycle_store(), cook_id, 1, attempt_id)
                    .expect_err("cancelled handoff fence rejects late Cook index publication");
            assert_eq!(
                error.code,
                homeboy_core::ErrorCode::ValidationInvalidArgument
            );
            assert!(!cook_index_exists(cook_id).expect("inspect absent Cook index"));
        });
    }

    #[test]
    fn cancellation_between_reservation_and_submission_cancels_the_reachable_child() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-reservation-switch-during-cancel";
            let attempt_id = "cook-reservation-switch-during-cancel-attempt-1";
            record_detached_cook_handoff_parent_in_store(&test_lifecycle_store(), cook_id)
                .expect("persist handoff parent");
            reserve_detached_cook_handoff_materialization_in_store(
                &test_lifecycle_store(),
                cook_id,
                attempt_id,
            )
            .expect("reserve materializing attempt");
            cancel_run(cook_id, None).expect("cancel handoff before child submission");
            let plan = AgentTaskPlan::new("reserved-index-switch-attempt", Vec::new());
            submit_plan(&plan, Some(attempt_id)).expect("persist attempt");

            record_cook_attempt_in_store(&test_lifecycle_store(), cook_id, 1, attempt_id)
                .expect("reserved child remains publishable after cancellation");
            assert!(
                cancel_reserved_detached_cook_handoff_attempt_if_cancelled_in_store(
                    &test_lifecycle_store(),
                    cook_id
                )
                .expect("cancel submitted reserved child"),
                "the reservation reaches the child that was not present during parent cancellation"
            );
            assert!(cook_index_exists(cook_id).expect("inspect durable Cook index"));
            assert_eq!(
                exact_record(attempt_id)
                    .expect("read durable child outcome")
                    .state,
                AgentTaskRunState::Cancelled
            );
            assert!(
                !cancel_reserved_detached_cook_handoff_attempt_if_cancelled_in_store(
                    &test_lifecycle_store(),
                    cook_id
                )
                .expect("replaying queued-child cancellation"),
                "a replayed cancellation is a no-op after the queued child terminalizes"
            );
        });
    }

    #[test]
    fn cancellation_during_index_publication_preserves_a_terminal_child() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-terminal-index-publication-race";
            let attempt_id = "cook-terminal-index-publication-race-attempt-1";
            record_detached_cook_handoff_parent_in_store(&test_lifecycle_store(), cook_id)
                .expect("persist handoff parent");
            reserve_detached_cook_handoff_materialization_in_store(
                &test_lifecycle_store(),
                cook_id,
                attempt_id,
            )
            .expect("reserve materializing attempt");
            submit_plan(
                &AgentTaskPlan::new("terminal-index-publication-race-attempt", Vec::new()),
                Some(attempt_id),
            )
            .expect("persist attempt");
            store::mutate_record(attempt_id, |record| {
                set_run_state(record, AgentTaskRunState::Succeeded);
                true
            })
            .expect("terminalize attempt before index publication");

            install_after_initial_cancellation_for_test(move || {
                record_cook_attempt_in_store(&test_lifecycle_store(), cook_id, 1, attempt_id)
                    .expect("publish terminal reserved child during cancellation");
            });
            let cancelled = cancel_run(cook_id, None)
                .expect("Cook cancellation converges after terminal index publication");

            assert_eq!(cancelled.run_id, attempt_id);
            assert_eq!(cancelled.state, AgentTaskRunState::Succeeded);
            assert!(cook_index_exists(cook_id).expect("inspect durable Cook index"));
        });
    }

    #[test]
    fn repeated_cook_alias_cancellation_preserves_terminal_indexed_attempts() {
        homeboy_core::test_support::with_isolated_home(|_| {
            for (suffix, terminal_state) in [
                ("succeeded", AgentTaskRunState::Succeeded),
                ("failed", AgentTaskRunState::Failed),
            ] {
                let cook_id = format!("cook-terminal-reservation-{suffix}");
                let attempt_id = format!("{cook_id}-attempt-1");
                record_detached_cook_handoff_parent_in_store(&test_lifecycle_store(), &cook_id)
                    .expect("persist handoff parent");
                reserve_detached_cook_handoff_materialization_in_store(
                    &test_lifecycle_store(),
                    &cook_id,
                    &attempt_id,
                )
                .expect("reserve materializing attempt");
                submit_plan(
                    &AgentTaskPlan::new(format!("{suffix}-attempt"), Vec::new()),
                    Some(&attempt_id),
                )
                .expect("persist attempt");
                store::mutate_record(&attempt_id, |record| {
                    set_run_state(record, terminal_state.clone());
                    true
                })
                .expect("terminalize reserved child");
                record_cook_attempt_in_store(&test_lifecycle_store(), &cook_id, 1, &attempt_id)
                    .expect("publish terminal child as the Cook alias");

                for _ in 0..2 {
                    let resolved = cancel_run(&cook_id, None)
                        .expect("cancelling a Cook alias with a terminal child is idempotent");
                    assert_eq!(resolved.run_id, attempt_id);
                    assert_eq!(resolved.state, terminal_state);
                }
                assert_eq!(
                    exact_record(&attempt_id)
                        .expect("read terminal child")
                        .state,
                    terminal_state,
                    "parent cancellation must preserve the terminal child"
                );
                assert!(
                    cancel_exact_run_in_store(
                        &AgentTaskLifecycleStore::from_current_environment()
                            .expect("lifecycle store"),
                        &attempt_id,
                        None
                    )
                    .is_err(),
                    "direct cancellation retains strict terminal-run semantics"
                );
            }
        });
    }

    #[test]
    fn cancellation_never_signals_a_pid_with_a_mismatched_start_identity() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-pid-reuse-safety";
            record_detached_cook_handoff_parent_in_store(&test_lifecycle_store(), cook_id)
                .expect("persist handoff parent");
            let mut child = std::process::Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .expect("spawn child");
            let identity = homeboy_core::process::process_start_identity(child.id())
                .expect("inspect child identity")
                .expect("child identity is present");
            record_detached_cook_handoff_child_in_store(
                &test_lifecycle_store(),
                cook_id,
                child.id(),
                identity,
            )
            .expect("persist child identity");
            store::mutate_record(cook_id, |record| {
                record.metadata["detached_cook_handoff"]["child_start_identity"] =
                    json!({ "platform": "linux", "starttime_ticks": 0 });
                true
            })
            .expect("replace identity with a mismatched fixture");

            assert!(cancel_run(cook_id, None).is_err());
            assert_eq!(
                homeboy_core::process::process_identity_state(child.id(), None),
                homeboy_core::process::ProcessIdentityState::Live,
                "a mismatched persisted identity must not signal the live PID"
            );
            let _ = homeboy_core::process::terminate_process_tree(child.id());
            let _ = child.wait();
        });
    }

    #[test]
    fn cancelled_pre_spawn_fence_blocks_an_unattached_child_before_materialization() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let cook_id = "cook-launcher-died-before-child-attachment";
            record_detached_cook_handoff_parent_in_store(&test_lifecycle_store(), cook_id)
                .expect("persist pre-spawn parent");
            cancel_run(cook_id, None).expect("cancel parent after launcher loss");

            assert!(require_detached_cook_handoff_fence_open_in_store(
                &test_lifecycle_store(),
                cook_id
            )
            .is_err());
            assert!(!cook_index_exists(cook_id).expect("inspect absent Cook index"));
            assert!(
                !run_record_exists(&cook_attempt_run_id(cook_id, 1))
                    .expect("inspect absent materialized attempt"),
                "an unattached child must observe the durable fence before it can materialize"
            );
        });
    }

    #[test]
    fn rooted_exact_cancellation_terminalizes_a_running_reserved_child() {
        let left_context = homeboy_core::test_support::HermeticTestContext::new();
        let right_context = homeboy_core::test_support::HermeticTestContext::new();
        let left = AgentTaskLifecycleStore::new(left_context.path_roots());
        let right = AgentTaskLifecycleStore::new(right_context.path_roots());
        let run_id = "same-running-reserved-child";

        for store in [&left, &right] {
            store
                .submit_plan_with_runtime_admission(
                    &AgentTaskPlan::new("running-reserved-child", Vec::new()),
                    run_id,
                    |_| Ok(json!({})),
                )
                .expect("submit isolated child");
            store
                .mutate_record(run_id, |record| {
                    set_run_state(record, AgentTaskRunState::Running);
                    true
                })
                .expect("mark isolated child running");
        }

        cancel_exact_run_in_store(&left, run_id, Some("parent cancelled"))
            .expect("cancel running child in selected root");

        assert_eq!(
            left.read_record(run_id).expect("read left child").state,
            AgentTaskRunState::Cancelled
        );
        assert_eq!(
            right.read_record(run_id).expect("read right child").state,
            AgentTaskRunState::Running
        );
    }

    #[test]
    fn rooted_exact_cancellation_preserves_success_pending_aggregate_import() {
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = AgentTaskLifecycleStore::new(context.path_roots());
        let run_id = "rooted-success-pending-import";
        store
            .submit_plan_with_runtime_admission(
                &AgentTaskPlan::new("success-pending-import", Vec::new()),
                run_id,
                |_| Ok(json!({})),
            )
            .unwrap();
        store
            .mutate_record(run_id, |record| {
                set_run_state(record, AgentTaskRunState::Running);
                record.metadata["provider_executions"] = json!([{ "state": "succeeded" }]);
                true
            })
            .unwrap();

        let record = cancel_exact_run_in_store(&store, run_id, Some("parent cancelled"))
            .expect("defer cancellation for successful provider");

        assert_eq!(record.state, AgentTaskRunState::Running);
        assert_eq!(
            record.metadata["cancellation_deferred_for_terminal_provider"]["reason"],
            "parent cancelled"
        );
    }
}

#[cfg(test)]
pub(super) mod test_cancel_hook {
    use super::*;
    use std::cell::RefCell;

    type CancelHook = Box<
        dyn FnMut(
            &str,
            &str,
            &str,
        ) -> Result<(
            homeboy_core::api_jobs::Job,
            Vec<homeboy_core::api_jobs::JobEvent>,
        )>,
    >;

    thread_local! {
        static HOOK: RefCell<Option<CancelHook>> = const { RefCell::new(None) };
    }

    pub(in super::super) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|cell| *cell.borrow_mut() = None);
        }
    }

    pub(in super::super) fn install(hook: CancelHook) -> Guard {
        HOOK.with(|cell| *cell.borrow_mut() = Some(hook));
        Guard
    }

    pub(super) fn take(
        runner_id: &str,
        runner_job_id: &str,
        durable_run_id: &str,
    ) -> Option<
        Result<(
            homeboy_core::api_jobs::Job,
            Vec<homeboy_core::api_jobs::JobEvent>,
        )>,
    > {
        HOOK.with(|cell| {
            cell.borrow_mut()
                .as_mut()
                .map(|hook| hook(runner_id, runner_job_id, durable_run_id))
        })
    }
}
