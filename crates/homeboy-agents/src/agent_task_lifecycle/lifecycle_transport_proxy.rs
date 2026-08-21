//! Transport-proxy recovery for controller-owned runner handoffs.
//!
//! A controller proxy is a transport handoff, not an agent provider task: the
//! runner owns the mutable workspace and command, and the controller must never
//! reach provider lookup for a proxy run. These operations reconnect, resume, or
//! replay durable runner execution evidence for a proxy without dispatching new
//! provider work. This is the partial-failure recovery surface for Lab handoffs,
//! extracted from `lifecycle_ops` so its invariants stay inspectable in isolation.

use super::*;

/// A controller proxy is a transport handoff, not an agent provider task. Its
/// synthetic plan task identifies the runner so the durable record remains
/// inspectable, but must never reach provider lookup on the controller.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportProxyRecovery {
    Resumed {
        record: AgentTaskRunRecord,
        next_action: String,
    },
    Reconciled {
        record: AgentTaskRunRecord,
        next_action: String,
    },
    ReconnectRequired {
        record: AgentTaskRunRecord,
        next_action: String,
    },
}

impl TransportProxyRecovery {
    pub fn record(&self) -> &AgentTaskRunRecord {
        match self {
            Self::Resumed { record, .. }
            | Self::Reconciled { record, .. }
            | Self::ReconnectRequired { record, .. } => record,
        }
    }

    pub fn next_action(&self) -> &str {
        match self {
            Self::Resumed { next_action, .. }
            | Self::Reconciled { next_action, .. }
            | Self::ReconnectRequired { next_action, .. } => next_action,
        }
    }
}

/// Reconnect a transport-owned proxy through its durable runner execution
/// record. `None` means the run is a normal scheduler-owned plan.
pub fn recover_transport_proxy(run_id: &str) -> Result<Option<TransportProxyRecovery>> {
    recover_transport_proxy_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        run_id,
    )
}

/// The store-rooted counterpart of [`recover_transport_proxy`].
///
/// Recovery is a read-modify-write over one durable proxy record: it reads the
/// record, reconciles a runner snapshot onto it, and commits the result. All
/// three have to land in the same installation. A recovery that read one home's
/// record and committed the reconnect annotation into another would leave the
/// caller's own proxy permanently unrecovered while reporting success — the
/// cross-home write class #7505 exists to remove.
///
/// `with_runner_continuation` is deliberately still process-global: the runner
/// continuation registry is configured trust material and a subprocess
/// contract, not a durable lifecycle root (#12618).
pub fn recover_transport_proxy_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<TransportProxyRecovery>> {
    let mut record = lifecycle_store.read_record(&sanitize_run_id(run_id))?;
    homeboy_core::controller_runtime::validate_for_mutation(
        &record.metadata,
        &homeboy_core::build_identity::current().display,
    )?;
    if !is_transport_proxy(&record) {
        return Ok(None);
    }

    let Some(runner_id) = transport_proxy_runner_id(&record) else {
        return Ok(None);
    };
    let runner_job_id = transport_proxy_runner_job_id(&record);
    let metadata = record.ensure_metadata_object();
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    metadata.insert("transport_recovery".to_string(), json!("required"));
    metadata.insert("runner_id".to_string(), json!(&runner_id));

    let Some(runner_job_id) = runner_job_id else {
        return resume_transport_proxy_on_runner_in_store(lifecycle_store, record, runner_id);
    };

    metadata.insert("runner_job_id".to_string(), json!(&runner_job_id));

    match super::runner_continuation::with_runner_continuation(|p| {
        p.runner_job_log_snapshot(&runner_id, &runner_job_id)
    }) {
        Ok(snapshot) => {
            reconcile_transport_proxy_snapshot_in_store(lifecycle_store, &mut record, &snapshot)?;
            lifecycle_store.write_record(&record)?;
            Ok(Some(TransportProxyRecovery::Reconciled {
                record,
                next_action: format!(
                    "homeboy runner job logs {runner_id} {runner_job_id} --follow"
                ),
            }))
        }
        Err(_) => {
            record.annotate_runner_disconnected();
            lifecycle_store.write_record(&record)?;
            Ok(Some(TransportProxyRecovery::ReconnectRequired {
                record,
                next_action: format!("homeboy runner connect {runner_id}"),
            }))
        }
    }
}

/// Replays already-persisted terminal runner evidence into an incomplete
/// controller projection. It is intentionally limited to an existing terminal
/// job snapshot and cannot dispatch or resume provider work.
///
/// This is deliberately still ambient (#7505). Rooting it is a one-hop edit —
/// every sibling it needs (`project_persisted_terminal_runner_events_in_store`,
/// `reconcile_runner_job_snapshot_in_store`, and the store's own record and
/// aggregate accessors) already exists — but it is *not* a self-contained one.
/// These two calls are the last non-test callers of the ambient
/// `project_persisted_terminal_runner_events` and `reconcile_runner_job_snapshot`
/// wrappers in `lifecycle_runner_projection`. Both are `pub(crate)`, so
/// rerouting them here leaves those wrappers reachable only from `#[cfg(test)]`
/// code, which is a `dead_code` error under `-D warnings`. Closing this slice
/// therefore has to gate or delete those two wrappers in the same commit, in
/// the file that owns them.
pub fn recover_terminal_transport_proxy_evidence(run_id: &str) -> Result<bool> {
    let mut record = store::read_record(&sanitize_run_id(run_id))?;
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
    homeboy_core::controller_runtime::validate(runtime)?;
    if !record.state.is_terminal() || !is_authenticated_transport_recovery_record(&record) {
        return Ok(false);
    }
    let (Some(runner_id), Some(runner_job_id)) = (
        transport_proxy_runner_id(&record),
        transport_proxy_runner_job_id(&record),
    ) else {
        return Ok(false);
    };
    if super::lifecycle_runner_projection::project_persisted_terminal_runner_events(&mut record)? {
        return Ok(true);
    }
    let snapshot = super::runner_continuation::with_runner_continuation(|p| {
        p.runner_job_log_snapshot(&runner_id, &runner_job_id)
    })?;
    if !matches!(
        snapshot.job.status,
        homeboy_core::api_jobs::JobStatus::Succeeded
            | homeboy_core::api_jobs::JobStatus::Failed
            | homeboy_core::api_jobs::JobStatus::Cancelled
    ) {
        return Ok(false);
    }
    reconcile_runner_job_snapshot(&mut record, &snapshot)?;
    store::write_record(&record)?;
    Ok(store::read_aggregate(&record.run_id).is_ok())
}

fn is_authenticated_transport_recovery_record(record: &AgentTaskRunRecord) -> bool {
    record.lab_handoff.as_ref().is_some_and(|handoff| {
        handoff.validation_error().is_none()
            && handoff.state == AgentTaskLabHandoffState::Accepted
            && handoff.authority == AgentTaskLabHandoffAuthority::RunnerDaemon
            && record.runner_id() == Some(handoff.runner_id.as_str())
            && record.runner_job_id() == handoff.runner_job_id.as_deref()
    })
}

/// Resume an unbound proxy where the controller never learned the runner job
/// identity. The persisted command and workspace belong to the runner, so this
/// must execute through the runner rather than through the local scheduler.
///
/// There is deliberately no ambient wrapper: the only caller,
/// [`recover_transport_proxy_in_store`], is rooted and already holds the store
/// whose record it is resuming. Every early-return branch below commits the
/// annotated record, and the post-continuation re-read reloads whatever the
/// runner persisted — all of which must be the caller's own home (#7505).
fn resume_transport_proxy_on_runner_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    mut record: AgentTaskRunRecord,
    runner_id: String,
) -> Result<Option<TransportProxyRecovery>> {
    let Some(remote_workspace) = record
        .metadata
        .get("remote_workspace")
        .and_then(Value::as_str)
        .filter(|path| !path.trim().is_empty())
    else {
        lifecycle_store.write_record(&record)?;
        return Ok(Some(TransportProxyRecovery::ReconnectRequired {
            record,
            next_action: format!("homeboy runner connect {runner_id}"),
        }));
    };
    let Some(remote_command) = record
        .metadata
        .get("remote_command")
        .and_then(Value::as_array)
        .map(|command| {
            command
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|command| !command.is_empty())
    else {
        lifecycle_store.write_record(&record)?;
        return Ok(Some(TransportProxyRecovery::ReconnectRequired {
            record,
            next_action: format!("homeboy runner connect {runner_id}"),
        }));
    };

    if !super::runner_continuation::with_runner_continuation(|p| {
        p.runner_authority(&runner_id).is_configured()
    }) {
        lifecycle_store.write_record(&record)?;
        return Ok(Some(TransportProxyRecovery::ReconnectRequired {
            record,
            next_action: format!("homeboy runner connect {runner_id}"),
        }));
    }

    let remote_workspace = remote_workspace.to_string();
    let run_id = record.run_id.clone();
    let exit_code = super::runner_continuation::with_runner_continuation(|p| {
        p.run_continuation_exec(&runner_id, &remote_workspace, &remote_command, &run_id)
    })?;
    if exit_code != 0 {
        return Err(Error::internal_unexpected(format!(
            "runner continuation for agent-task run '{}' exited with status {exit_code}",
            record.run_id
        )));
    }

    record = lifecycle_store.read_record(&record.run_id)?;
    Ok(Some(TransportProxyRecovery::Resumed {
        next_action: format!("homeboy agent-task status {} --full", record.run_id),
        record,
    }))
}

// The ambient `reconcile_transport_proxy_snapshot()` shim that used to sit here
// is gone. It was already `#[cfg(test)]` — its last production caller had moved
// to the rooted sibling — and the one test still calling it now resolves its
// own store, so the pair collapses to the rooted form alone (#7505).

/// Reconcile a transport-proxy snapshot against an explicitly injected root.
///
/// This is a pure alias for the transport-proxy entry into the one reconcile
/// owner, so the store is passed straight through: the binding write, the
/// aggregate idempotence read, and the terminal commit all belong to
/// `reconcile_runner_job_snapshot_in_store` and are already rooted there.
pub(crate) fn reconcile_transport_proxy_snapshot_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<()> {
    // Binding and the queued -> running transition now live at the top of
    // `reconcile_runner_job_snapshot_in_store` so every reconcile path shares
    // one owner.
    reconcile_runner_job_snapshot_in_store(lifecycle_store, record, snapshot)
}

/// A runner snapshot from the expected Lab is durable acceptance evidence when
/// the controller has not yet recorded its child job ID. Bind that identity
/// before validating the snapshot so the pre-acceptance handoff converges to
/// the same state as an acknowledged daemon response.
///
/// This is the single owner of pre-acceptance handoff binding: it runs at the
/// top of `reconcile_runner_job_snapshot`, so every reconcile path converges on
/// one binding decision instead of each caller binding independently.
///
/// A daemon job is created with `target_runner_id: None` and only gains a runner
/// once claimed, so a snapshot polled before the claim legitimately has no
/// target. Accept an absent target (the expected-Lab handoff is authority) and
/// only reject a target that names a *different* runner.
///
/// Binding is a durable write — it replaces `record` with whatever acceptance
/// persisted — so the installation it commits into has to be the one its caller
/// is reconciling. A binding written to another home would leave the caller's
/// own record permanently unbound while snapshot validation kept rejecting a
/// perfectly valid runner job (#7505).
///
/// There is deliberately no ambient wrapper any more. Both callers —
/// `reconcile_runner_job_snapshot_in_store` and
/// `project_terminal_runner_result_in_store` — are rooted and already hold the
/// store whose record they are binding.
pub(crate) fn bind_pending_lab_handoff_snapshot_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
) -> Result<()> {
    if record.runner_job_id().is_some() {
        return Ok(());
    }
    let runner_id = record
        .lab_handoff
        .as_ref()
        .filter(|handoff| {
            handoff.state == AgentTaskLabHandoffState::Pending
                && handoff.authority == AgentTaskLabHandoffAuthority::Controller
        })
        .map(|handoff| handoff.runner_id.clone())
        .or_else(|| record.runner_id().map(str::to_string))
        // A proxy interrupted *before* Lab acceptance has neither an accepted
        // runner_job_id nor a Pending controller handoff — the controller only
        // recorded its planned execution intent. On recovery the runner accepts
        // a fresh replacement job; bind it from that durable execution record's
        // runner so snapshot validation converges instead of rejecting a valid
        // replacement against an empty controller job id (#9382).
        .or_else(|| planned_execution_record_runner_id(record));
    let Some(runner_id) = runner_id else {
        return Ok(());
    };
    if snapshot
        .job
        .target_runner_id
        .as_deref()
        .is_some_and(|target| target != runner_id)
    {
        return Ok(());
    }
    let remote_workspace = record
        .metadata
        .get("remote_workspace")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let remote_command = record
        .metadata
        .get("remote_command")
        .and_then(Value::as_array)
        .map(|command| {
            command
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if record.state.is_terminal() {
        // The runner can finish and mirror its aggregate before the controller
        // projects the daemon snapshot. Preserve that terminal outcome while
        // attaching the authoritative job identity needed for validation.
        *record = record_runner_job_identity_in_store(
            lifecycle_store,
            &record.run_id,
            &runner_id,
            &snapshot.job.id.to_string(),
        )?;
        return Ok(());
    }
    *record = record_detached_lab_run_in_store(
        lifecycle_store,
        DetachedLabRunRecord {
            run_id: &record.run_id,
            runner_id: &runner_id,
            runner_job_id: &snapshot.job.id.to_string(),
            remote_workspace: &remote_workspace,
            remote_command: &remote_command,
        },
    )?;
    Ok(())
}

/// The runner id recorded on a controller-owned proxy's durable execution
/// record when it has a planned (pre-acceptance) execution intent but no
/// accepted runner job yet.
///
/// This is the recovery-safe fallback for
/// [`bind_pending_lab_handoff_snapshot_in_store`]:
/// it only yields a runner when the execution record is *planned* (not yet
/// bound to a job id) and names this exact run, so binding a replacement job
/// cannot latch onto a terminal or mismatched execution record.
fn planned_execution_record_runner_id(record: &AgentTaskRunRecord) -> Option<String> {
    let execution = record
        .metadata
        .get("runner_execution_record")
        .and_then(|value| {
            serde_json::from_value::<
                    homeboy_core::runner_execution_envelope::RunnerExecutionRecord,
                >(value.clone())
                .ok()
        })?;
    if execution.job_id.is_some() {
        return None;
    }
    if execution
        .agent_task_run_id
        .as_deref()
        .is_some_and(|run_id| run_id != record.run_id)
    {
        return None;
    }
    let runner_id = execution.runner_id.trim();
    (!runner_id.is_empty()).then(|| runner_id.to_string())
}

pub(crate) fn is_transport_proxy(record: &AgentTaskRunRecord) -> bool {
    record
        .metadata
        .get("kind")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.ends_with("_controller_proxy"))
}

fn transport_proxy_runner_id(record: &AgentTaskRunRecord) -> Option<String> {
    record.runner_id().map(str::to_string).or_else(|| {
        record
            .metadata
            .get("runner_execution_record")
            .and_then(|value| {
                serde_json::from_value::<
                    homeboy_core::runner_execution_envelope::RunnerExecutionRecord,
                >(value.clone())
                .ok()
            })
            .map(|execution| execution.runner_id)
            .filter(|runner_id| !runner_id.trim().is_empty())
    })
}

fn transport_proxy_runner_job_id(record: &AgentTaskRunRecord) -> Option<String> {
    record.runner_job_id().map(str::to_string).or_else(|| {
        record
            .metadata
            .get("runner_execution_record")
            .and_then(|value| {
                serde_json::from_value::<
                    homeboy_core::runner_execution_envelope::RunnerExecutionRecord,
                >(value.clone())
                .ok()
            })
            .and_then(|execution| execution.job_id)
            .filter(|job_id| !job_id.trim().is_empty())
    })
}
