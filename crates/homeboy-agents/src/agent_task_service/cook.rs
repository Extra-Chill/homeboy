//! Cook orchestration facade: request/report contracts, admission helpers and
//! stable re-exports. The driver, spine, workspace preparation, follow-up
//! dispatch, deadlines, gate setup and projections live in `cook_*` siblings.

pub(super) use homeboy_control_plane_contract::ControlPlaneState;
use homeboy_core::cook_status::GateVerdict;
pub(super) use serde_json::Value;
pub(super) use std::ops::ControlFlow;
pub(super) use std::path::{Path, PathBuf};
#[cfg(test)]
pub(super) use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
pub(super) use std::sync::LazyLock;
#[cfg(test)]
pub(super) use std::sync::Mutex;
pub(super) use std::sync::{mpsc, Arc};
pub(super) use std::time::{Duration, Instant};

pub(super) use crate::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
    AgentTaskIntentionalNoChange, AgentTaskIntentionalNoChangeVerdict,
};

pub(super) use crate::agent_task_dispatch_plan::validate_single_cook_prompt_source;
pub(super) use crate::agent_task_dispatch_service::{self, AgentTaskDispatchCommand};
pub(super) use crate::agent_task_gate::VerifyGateOptions;
pub(super) use crate::agent_task_lifecycle::{self, AgentTaskLifecycleStore};
pub(super) use crate::agent_task_promotion::{
    candidate_fingerprint, AgentTaskPromotionCandidate, AgentTaskPromotionReport,
    AgentTaskPromotionStatus,
};
pub(super) use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutionBudget, AgentTaskPlan, AgentTaskProviderRotationEntry,
    AgentTaskProviderRotationPolicy, AgentTaskScheduleSupport, SharedAgentTaskExecutor,
};
pub(super) use crate::agent_task_timeout::{
    capture_cook_deadline, current_cook_deadline, expired_cook_deadline, CookDeadline,
};
pub(super) use homeboy_core::command_invocation::CommandInvocation;
pub(super) use homeboy_core::cook_status::{CookDisposition, CookStatus};
pub(super) use homeboy_core::error::posix_quote_arg;
pub(super) use homeboy_core::{Error, Result};

pub(super) use super::cook_activity::{CookActivityProbe, CookProviderActivity};
pub(super) use super::cook_baseline::{
    cook_attempt_harvest_context, materialize_follow_up_baseline_in_root,
    materialize_initial_candidate_baseline, re_materialize_follow_up_baseline,
    CookFollowUpBaseline, DerivedCookBaselineCapability,
};
pub(super) use super::cook_budget::{
    budget_remaining, execution_budget_usage, reserve_remediation_budget,
    validate_effective_cook_budget, ExecutionBudgetUsage,
};
pub(super) use super::cook_delivery::CookDeliveryContext;
#[cfg(test)]
pub(super) use super::cook_pre_execution::materialize_initial_cook_attempt_with_stores;
pub(super) use super::cook_pre_execution::{
    materialize_cook_attempt_with_stores, materialize_initial_cook_attempt_with_stores_outcome,
    pre_execution_failure_details, pre_execution_failure_phase, pre_execution_failure_report,
    provider_rotation_attempts, record_pre_execution_failure, retryable_pre_execution_failure,
    terminal_executor_identity, terminal_executor_matches, with_pre_execution_phase,
    CookExecutionPreparation, PreExecutionFailureDetails,
};
pub(super) use super::cook_promotion::{
    attempt_needs_execution_with_store, finalize_or_load_cook_pr_with_stores,
    persisted_promotion_for_attempt_in_store, pre_provider_diagnostic_cause,
    promote_or_load_attempt_in_store, CookReportInput, CookStores,
};
pub(super) use super::cook_recipe::{CookRecipeStore, InitialRecipeMaterialization};
pub(super) use super::cook_recovery::CookRecoveryStep;
pub(super) use super::cook_supervision::{resolve_supervision_policy, CookSupervisor};
#[cfg(test)]
pub(super) use super::execution::run_loaded_plan_with_derived_cook_baseline;
pub(super) use super::execution::{
    run_claimed_loaded_plan_with_derived_cook_baseline_in_store,
    run_loaded_plan_with_derived_cook_baseline_in_store,
};
pub(super) use super::AgentTaskRunResult;

/// Lease window for a cook promotion operation claim. Long enough that a healthy
/// controller finishes promoting and records the result within it; a crashed
/// controller's lease elapses so a resumed pass can reconcile and continue.
pub(super) const PROMOTION_CLAIM_LEASE: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

#[cfg(test)]
pub(super) static WORKSPACE_BASE_CAPTURE_HOOK: LazyLock<
    Mutex<Option<Arc<WorkspaceBaseCaptureHook>>>,
> = LazyLock::new(|| Mutex::new(None));

#[cfg(test)]
pub(super) struct WorkspaceBaseCaptureHook {
    pub(super) workspace: PathBuf,
    pub(super) count: AtomicUsize,
    pub(super) fail_after_recipe_persistence: std::sync::atomic::AtomicBool,
}

/// Render the one public Cook recovery route, `agent-task recover`.
///
/// Cook IDs deliberately preserve normal current-attempt selection, while
/// attempt IDs preserve exact lifecycle lineage for recovery. `executable`
/// permits a controller-pinned runtime to own the recovery without changing
/// the public command shape.
pub fn cook_recover_command(executable: Option<&str>, cook_or_attempt_id: &str) -> String {
    let executable = executable
        .map(posix_quote_arg)
        .unwrap_or_else(|| cook_recovery_command_prefix(cook_or_attempt_id));
    cook_recover_command_with_prefix(&executable, cook_or_attempt_id)
}

/// Render recovery from placement already authenticated by its record.
pub fn cook_recover_command_for_record(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    cook_or_attempt_id: &str,
) -> String {
    cook_recover_command_with_prefix(
        &cook_recovery_command_prefix_for_record(record),
        cook_or_attempt_id,
    )
}

pub(super) fn cook_recover_command_with_prefix(
    executable: &str,
    cook_or_attempt_id: &str,
) -> String {
    cook_recovery_command_with_prefix(executable, &["recover", cook_or_attempt_id])
}

/// Render recovery on the Cook's controller. Provider placement is retained in
/// the durable attempt and consumed by its dispatcher, not by this CLI prefix.
///
/// A local prefix is only recovered from an explicit, durable override. Other
/// controller-local attempts remain subject to current resource policy instead
/// of turning ownership evidence into a new global authorization.
pub fn cook_recovery_command_prefix(run_id: &str) -> String {
    let decision = agent_task_lifecycle::exact_record(run_id)
        .ok()
        .and_then(|record| cook_recovery_placement_decision(&record));
    cook_recovery_command_prefix_for_decision(decision.as_ref())
}

/// Render the recovery prefix from an already-loaded durable attempt record.
/// This keeps callers with an injected lifecycle store from reading ambient
/// placement state while constructing a follow-up command.
pub fn cook_recovery_command_prefix_for_record(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> String {
    let decision = cook_recovery_placement_decision(record);
    cook_recovery_command_prefix_for_decision(decision.as_ref())
}

pub(super) fn cook_recovery_placement_decision(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<homeboy_lab_contract::ExecutionPlacementDecision> {
    serde_json::from_value(record.metadata["execution_placement_decision"].clone()).ok()
}

pub(super) fn cook_recovery_command_prefix_for_decision(
    decision: Option<&homeboy_lab_contract::ExecutionPlacementDecision>,
) -> String {
    match decision {
        Some(decision)
            if decision.requested == homeboy_lab_contract::Placement::Local
                && decision.override_authorization.authorized =>
        {
            "homeboy --placement local".to_string()
        }
        _ => "homeboy".to_string(),
    }
}

/// Build an owner-bound recovery command from the durable placement decision.
pub fn cook_recovery_command(run_id: &str, args: &[&str]) -> String {
    let prefix = cook_recovery_command_prefix(run_id);
    cook_recovery_command_with_prefix(&prefix, args)
}

pub(super) fn cook_recovery_command_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    args: &[&str],
) -> String {
    let prefix = lifecycle_store
        .read_record(run_id)
        .map(|record| cook_recovery_command_prefix_for_record(&record))
        .unwrap_or_else(|_| "homeboy".to_string());
    cook_recovery_command_with_prefix(&prefix, args)
}

/// Build a recovery command from a previously resolved durable prefix.
pub fn cook_recovery_command_with_prefix(prefix: &str, args: &[&str]) -> String {
    let mut command = format!("{prefix} agent-task");
    for arg in args {
        command.push(' ');
        command.push_str(&posix_quote_arg(arg));
    }
    command
}

/// Durable operation key for the promotion of one cook attempt. Promotion is
/// one-per-`run_id`, so the run id is the stable operation identity (#8357).
/// One Cook progress event, as delivered to a foreground observer.
///
/// This used to be three bare `&str`s — `(phase, cook_id, run_id)` — which is
/// why every heartbeat an operator saw was byte-identical and carried nothing
/// about the run it described. `report_cook_progress` already received the
/// attempt and a detail string and recorded them durably; both were thrown away
/// at the observer boundary. Passing a struct means adding a fact is an
/// additive change here rather than a signature change at every call site, so
/// the boundary stops being the place state goes to die (#11482).
#[derive(Clone, Debug)]
pub struct CookProgressEvent<'a> {
    pub phase: &'a str,
    pub cook_id: &'a str,
    pub run_id: &'a str,
    /// Which provider attempt this event belongs to.
    pub attempt: u32,
    /// Controller-owned description of the phase.
    pub detail: Option<&'a str>,
    /// Time since this durable phase began, as observed by its lifecycle owner.
    pub elapsed_ms: u64,
    /// Subsystem that owns the current wait (provider, worktree, queue, etc.).
    pub wait_owner: String,
    /// The terminal Cook result, when this is the terminal progress event.
    ///
    /// This remains separate from `detail`, which is persisted as the Cook's
    /// status string for durable readers.
    pub terminal_success: Option<bool>,
    /// Exact retry command from the durable result's legal actions. A failed
    /// exit alone is not authority to advertise a retry.
    pub terminal_retry_command: Option<&'a str>,
    /// What the provider is doing right now, when it is observable.
    pub activity: Option<&'a CookProviderActivity>,
}

impl CookProgressEvent<'_> {
    /// Bounded one-line rendering of the provider's current activity.
    pub fn activity_summary(&self) -> Option<String> {
        self.activity.and_then(CookProviderActivity::summary_line)
    }
}

pub type CookProgressObserver<'a> = dyn Fn(&CookProgressEvent<'_>) -> Result<()> + Send + Sync + 'a;

pub(super) fn report_cook_progress(
    lifecycle_store: &AgentTaskLifecycleStore,
    observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
    phase: &str,
    attempt: u32,
    detail: Option<&str>,
) -> Result<()> {
    report_cook_progress_with_activity(
        lifecycle_store,
        observer,
        cook_id,
        run_id,
        phase,
        attempt,
        detail,
        None,
        None,
        None,
    )
}

pub(super) fn report_cook_progress_with_activity(
    lifecycle_store: &AgentTaskLifecycleStore,
    observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
    phase: &str,
    attempt: u32,
    detail: Option<&str>,
    activity: Option<&CookProviderActivity>,
    terminal_success: Option<bool>,
    terminal_retry_command: Option<&str>,
) -> Result<()> {
    publish_cook_progress(
        lifecycle_store,
        observer,
        CookProgressEvent {
            phase,
            cook_id,
            run_id,
            attempt,
            detail,
            elapsed_ms: 0,
            wait_owner: String::new(),
            terminal_success,
            terminal_retry_command,
            activity,
        },
        activity.and_then(CookProviderActivity::to_record_value),
    )
}

/// Record a progress phase whose durable activity is controller evidence
/// rather than a provider activity sample (for example a local dispatch
/// waiting for host pressure), then deliver it to the observer.
pub(super) fn report_cook_progress_with_evidence(
    lifecycle_store: &AgentTaskLifecycleStore,
    observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
    phase: &str,
    attempt: u32,
    detail: Option<&str>,
    evidence: Value,
) -> Result<()> {
    publish_cook_progress(
        lifecycle_store,
        observer,
        CookProgressEvent {
            phase,
            cook_id,
            run_id,
            attempt,
            detail,
            elapsed_ms: 0,
            wait_owner: String::new(),
            terminal_success: None,
            terminal_retry_command: None,
            activity: None,
        },
        Some(evidence),
    )
}

/// Persist `event` with its durable activity record, then hand the observer
/// the event with the lifecycle-owned elapsed time and wait owner.
fn publish_cook_progress(
    lifecycle_store: &AgentTaskLifecycleStore,
    observer: Option<&CookProgressObserver<'_>>,
    mut event: CookProgressEvent<'_>,
    activity_record: Option<Value>,
) -> Result<()> {
    let record = lifecycle_store.record_cook_progress_with_activity(
        event.run_id,
        event.phase,
        event.attempt,
        event.detail,
        activity_record,
    )?;
    let progress = &record.metadata["cook_progress"];
    event.elapsed_ms = progress["elapsed_ms"].as_u64().unwrap_or_default();
    event.wait_owner = progress["wait_owner"]
        .as_str()
        .unwrap_or("cook_controller")
        .to_string();
    let run_id = event.run_id;
    let phase = event.phase;
    if let Some(observer) = observer {
        if let Err(error) = observer(&event) {
            // The observer is the submitting client's output channel. Once the
            // progress record exists, a broken client pipe is evidence about
            // observation, never authority to stop promotion or finalization.
            let _ = agent_task_lifecycle::record_cook_observer_event_in_store(
                lifecycle_store,
                run_id,
                phase,
                bounded_error_diagnostic(&error),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn admit_cook_runtime_generation(
    lifecycle_store: &AgentTaskLifecycleStore,
    durable_observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
) -> Result<homeboy_core::runtime_promotion::RuntimeGenerationPinGuard> {
    let pin_run_id = run_id.to_string();
    let pin_cook_id = cook_id.to_string();
    homeboy_core::runtime_promotion::pin_cook_generation_waiting(
        run_id,
        Duration::from_secs(30),
        || {
            lifecycle_store
                .read_record(&pin_run_id)
                .ok()
                .is_some_and(|record| {
                    record.state() == homeboy_lifecycle_contract::RunExecutionState::Cancelled
                })
        },
        |event| {
            let detail = format!(
                "waiting {}ms/{}ms for runtime promotion owner pid {} operation `{}` target `{}` generation `{}`",
                event.waited_ms,
                event.wait_timeout_ms,
                event.owner_pid,
                event.owner_operation,
                event.target,
                event.owner_generation,
            );
            let _ = report_cook_progress(
                lifecycle_store,
                durable_observer,
                &pin_cook_id,
                &pin_run_id,
                "runtime_promotion_wait",
                1,
                Some(&detail),
            );
        },
    )
}

/// Run one controller startup phase while keeping its already-admitted Cook
/// observable. Startup work can block on Git, filesystem capacity, or a shared
/// base-capture lock, so it receives a bounded durable heartbeat before any
/// provider execution starts.
pub(super) fn run_cook_startup_phase<T>(
    lifecycle_store: &AgentTaskLifecycleStore,
    observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
    phase: &str,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    debug_assert!(
        crate::agent_task_lifecycle::CONTROLLER_PRE_PROVIDER_PHASES.contains(&phase),
        "startup phase `{phase}` must be listed in CONTROLLER_PRE_PROVIDER_PHASES"
    );
    let started = Instant::now();
    report_cook_progress(
        lifecycle_store,
        observer,
        cook_id,
        run_id,
        phase,
        1,
        Some("starting; elapsed=0s"),
    )?;
    let (stop, wait) = mpsc::channel();
    let timing_store = lifecycle_store
        .open_observation_initialized()
        .map_err(|error| {
            homeboy_core::log_status!("observe", "phase timing persistence failed: {}", error);
        })
        .ok();
    let _phase = timing_store.as_ref().map(|store| {
        store
            .phase_timer(run_id)
            .into_guard(&format!("stage.{phase}"))
    });
    {
        std::thread::scope(|scope| {
            scope.spawn(homeboy_core::observation::phase_context::inherit(
                move || loop {
                    match wait.recv_timeout(COOK_HEARTBEAT_INTERVAL) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let detail =
                                format!("still running; elapsed={}s", started.elapsed().as_secs());
                            let _ = report_cook_progress(
                                lifecycle_store,
                                observer,
                                cook_id,
                                run_id,
                                phase,
                                1,
                                Some(&detail),
                            );
                        }
                    }
                },
            ));
            let result = operation();
            let _ = stop.send(());
            result
        })
    }
}

/// Durable operation key for finalizing one cook candidate. Keyed by the run id
/// plus the promoted candidate fingerprint (patch SHA), so re-finalizing the
/// same candidate is idempotent while a genuinely different candidate finalizes
/// on its own claim (#8357).
pub(super) fn finalization_operation_key(
    run_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> String {
    match promotion
        .candidate
        .as_ref()
        .map(|candidate| candidate.tree_sha.as_str())
    {
        Some(sha) => format!("finalize:{run_id}:{sha}"),
        None => format!("finalize:{run_id}"),
    }
}

/// Finalize a cook candidate under a durable exactly-once operation claim.
///
/// PR finalization performs its external effects (commit, push, `gh pr create`)
/// and only then records the result. A controller crash after the PR is created
/// but before the result is durable would open a second PR on restart. The claim
/// closes it: reserve `finalize:<run_id>:<sha>` before the effect and complete it
/// with the finalization result after it is durable. A resumed pass revalidates
/// the existing PR idempotently, including its live Git and GitHub identities
/// (#8357).
pub(super) fn finalize_with_operation_claim_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    run_id: &str,
    promotion: &AgentTaskPromotionReport,
    finalize: &mut dyn FnMut(&CookRequest, &str, &AgentTaskPromotionReport) -> Result<Value>,
) -> Result<Value> {
    let observation = lifecycle_store.open_observation_initialized()?;
    let result = observation
        .phase_timer(run_id)
        .time::<_, homeboy_core::Error>("publish", || {
            let operation_key = finalization_operation_key(run_id, promotion);
            match lifecycle_store.claim_cook_operation(
                run_id,
                &operation_key,
                FINALIZATION_CLAIM_LEASE,
            )? {
                // A completed claim only proves an earlier publication. Re-run the
                // idempotent finalizer so a later force-push or PR-head mutation cannot
                // be returned as a still-valid publication.
                agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(_) => {
                    finalize(options, run_id, promotion)
                }
                // A concurrent pass owns a fresh lease. The load-or-finalize path still
                // returns an already-recorded finalization when present; do not mark the
                // claim completed from here — its owner does.
                agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
                    finalize(options, run_id, promotion)
                }
                // This pass owns the operation. Finalize, then record the result as the
                // claim's immutable completion.
                agent_task_lifecycle::ClaimOutcome::Acquired(_lease) => {
                    let finalization = match finalize(options, run_id, promotion) {
                        Ok(finalization) => finalization,
                        Err(error) => {
                            _lease.fail(bounded_error_diagnostic(&error))?;
                            return Err(error);
                        }
                    };
                    _lease.complete(finalization.clone())?;
                    Ok(finalization)
                }
            }
        })?;
    if options.gates.verify_ci {
        if let Some(url) = result["pr_url"].as_str() {
            let store = super::cook_recipe::CookRecipeStore::for_lifecycle_store(lifecycle_store);
            super::cook_ci::save(
                &store,
                &options.identity.cook_id,
                &serde_json::json!({
                    "cook_id": options.identity.cook_id, "pr_url": url, "status": "awaiting_ci"
                }),
            )?;
        }
    }
    Ok(result)
}

/// The PR-finalization closure driven by the Cook runtime. The lifetime is
/// load-bearing because production finalizers borrow their recipe store.
pub(crate) type CookFinalizeFn<'a> = Box<
    dyn FnMut(
            &AgentTaskLifecycleStore,
            &CookRequest,
            &str,
            &AgentTaskPromotionReport,
        ) -> Result<Value>
        + 'a,
>;

#[cfg(test)]
pub(super) type TestCookPromoteFn<'a> = Box<
    dyn FnMut(&AgentTaskLifecycleStore, &CookRequest, &str) -> Result<AgentTaskPromotionReport>
        + 'a,
>;

/// Concrete production side effects. Promotion has one native
/// implementation; only finalization varies because callers bind
/// different durable recipe stores and backends.
pub(crate) struct CookSideEffects<'a> {
    finalize: CookFinalizeFn<'a>,
    #[cfg(test)]
    test_promote: Option<TestCookPromoteFn<'a>>,
}

impl<'a> CookSideEffects<'a> {
    pub(crate) fn new<F>(finalize: F) -> Self
    where
        F: FnMut(
                &AgentTaskLifecycleStore,
                &CookRequest,
                &str,
                &AgentTaskPromotionReport,
            ) -> Result<Value>
            + 'a,
    {
        Self {
            finalize: Box::new(finalize),
            #[cfg(test)]
            test_promote: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test<P, F>(promote: P, finalize: F) -> Self
    where
        P: FnMut(&AgentTaskLifecycleStore, &CookRequest, &str) -> Result<AgentTaskPromotionReport>
            + 'a,
        F: FnMut(
                &AgentTaskLifecycleStore,
                &CookRequest,
                &str,
                &AgentTaskPromotionReport,
            ) -> Result<Value>
            + 'a,
    {
        Self {
            finalize: Box::new(finalize),
            test_promote: Some(Box::new(promote)),
        }
    }

    pub(super) fn promote(
        &mut self,
        lifecycle_store: &AgentTaskLifecycleStore,
        options: &CookRequest,
        run_id: &str,
    ) -> Result<AgentTaskPromotionReport> {
        #[cfg(test)]
        if let Some(promote) = self.test_promote.as_mut() {
            return promote(lifecycle_store, options, run_id);
        }
        promote_with_operation_claim_in_store(lifecycle_store, options, run_id)
    }

    pub(super) fn finalize(
        &mut self,
        lifecycle_store: &AgentTaskLifecycleStore,
        options: &CookRequest,
        run_id: &str,
        promotion: &AgentTaskPromotionReport,
    ) -> Result<Value> {
        finalize_with_operation_claim_in_store(
            lifecycle_store,
            options,
            run_id,
            promotion,
            &mut |options, run_id, promotion| {
                (self.finalize)(lifecycle_store, options, run_id, promotion)
            },
        )
    }
}

/// Bind a terminal Cook outcome to the native destination worktree this Cook
/// provisioned (#18895). A published or nothing-to-deliver Cook finalizes it
/// `succeeded`, so cleanup can reclaim it once safe; every other outcome keeps
/// its evidence preserved. Caller-owned `--cwd` workspaces and destinations
/// another run provisioned stay unfinalized.
pub(super) fn finalize_cook_destination_worktree(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    status: CookStatus,
) -> Result<()> {
    use homeboy_core::worktree_provider::{
        WorktreeCleanupPolicy, WorktreeProvisionLifecycle, WorktreeTerminalDisposition,
    };
    if status.is_in_flight() || super::cook_workspace::cook_uses_explicit_cwd_workspace(options) {
        return Ok(());
    }
    let Some(record) = super::cook_workspace::native_cook_destination_record(options)? else {
        return Ok(());
    };
    let Some(owner) = record
        .run_id
        .clone()
        .filter(|_| record.state == homeboy_core::worktree::TaskWorktreeState::Active)
    else {
        return Ok(());
    };
    let provisioned_by_this_cook = owner == options.identity.initial_run_id
        || lifecycle_store
            .read_cook_index(&options.identity.cook_id)
            .is_ok_and(|index| index.attempts.iter().any(|attempt| attempt.run_id == owner));
    if !provisioned_by_this_cook {
        return Ok(());
    }
    let disposition = match status {
        _ if status.is_published() => WorktreeTerminalDisposition::Succeeded,
        CookStatus::Gate(GateVerdict::NoChanges) | CookStatus::IntentionalNoChange => {
            WorktreeTerminalDisposition::Succeeded
        }
        CookStatus::Cancelled => WorktreeTerminalDisposition::Cancelled,
        CookStatus::TimedOut => WorktreeTerminalDisposition::TimedOut,
        _ => WorktreeTerminalDisposition::Failed,
    };
    let lifecycle = WorktreeProvisionLifecycle {
        purpose: "agent_task_cook".to_string(),
        owner_run_ref: owner,
        cleanup_policy: if disposition == WorktreeTerminalDisposition::Succeeded {
            WorktreeCleanupPolicy::RemoveOnSuccess
        } else {
            WorktreeCleanupPolicy::PreserveOnFailure
        },
    };
    homeboy_core::worktree_provider::finalize_worktree(&record.id, &lifecycle, disposition)?;
    Ok(())
}

/// The promotion checkpoint captures this before gates run, when it is the only
/// complete authorization for reusing the dirty managed destination.
pub(crate) fn gate_feedback_current_diff(promotion: &AgentTaskPromotionReport) -> String {
    promotion
        .provenance
        .pointer("/gate_feedback_baseline/current_diff")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Parse the AI-authored review form off the terminal attempt outcome.
///
/// The agent emits it under `outputs["review_form"]`; the terminal outcome is
/// the last one recorded in the aggregate. Returns `Ok(None)` when the agent
/// emitted no form (the loop treats absence as a gap and nudges a retry). A
/// present-but-malformed form is a hard error so garbage is never rendered.
pub(crate) fn review_form_from_aggregate(
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
) -> Result<Option<crate::agent_task_review_dossier::AiFilledReviewForm>> {
    let Some(outcome) = aggregate.selected_outcome().or_else(|| {
        (aggregate.outcomes.len() == 1)
            .then(|| aggregate.outcomes.first())
            .flatten()
    }) else {
        return Ok(None);
    };
    crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(&outcome.outputs)
}

/// The latest attempt's complete review form exactly as the agent authored
/// it, read from the persisted outcome: `outputs.review_form`, else the same
/// field in the outcome's `json` typed artifact (the runtime result). Never
/// parsed or truncated, so read surfaces return the whole form for terminal
/// and `candidate_recoverable` runs alike (#18930).
pub fn persisted_review_form(
    aggregate: &crate::agent_task_schedule::AgentTaskAggregate,
) -> Option<Value> {
    let outcome = aggregate
        .selected_outcome()
        .or_else(|| aggregate.outcomes.last())?;
    let present = |form: Option<&Value>| form.filter(|form| !form.is_null()).cloned();
    present(outcome.outputs.get("review_form")).or_else(|| {
        outcome
            .typed_artifacts
            .iter()
            .filter(|artifact| artifact.artifact_type.as_deref() == Some("json"))
            .find_map(|artifact| present(artifact.payload.pointer("/outputs/review_form")))
    })
}

/// The agent's own reason, from the review form in `outputs`, for leaving
/// its task incomplete: the "why" of a Cook that settled unfinished.
pub fn agent_incomplete_reason(outputs: &Value) -> Option<String> {
    crate::agent_task_review_dossier::AiFilledReviewForm::from_outcome_outputs(outputs)
        .ok()??
        .incomplete_reason()
        .filter(|reason| !reason.is_empty())
        .map(str::to_string)
}

/// Project the PR-dossier contract before the first finalizing provider attempt
/// is persisted or executed. Standalone and no-finalize requests retain their
/// caller-defined contract.
///
/// The contract is data, not prose (#15818): the `review_form` output
/// declaration carries the schema and the publication metadata records that
/// the controller owns publication. Presenting either to the model is the
/// operator's `agent_task.prompt_templates` concern.
pub(super) fn project_initial_finalizing_review_form_contract(
    options: &mut CookRequest,
) -> Result<()> {
    if options.finalization.no_finalize {
        return Ok(());
    }

    let title_policy = cook_pr_title_policy(options)?;
    for request in &mut options.identity.initial_plan.tasks {
        request.controller_owns_publication();
        request.output_declarations.retain(|declaration| {
            declaration.name != crate::agent_task_review_dossier::AI_REVIEW_FORM_OUTPUT_KEY
        });
        request
            .output_declarations
            .push(crate::agent_task_review_dossier::review_form_output_declaration(title_policy));
        crate::agent_task_prompt_template::merge_homeboy_facts(
            request,
            serde_json::json!({ "publication": { "owner": "controller" } }),
        );
    }
    Ok(())
}

/// The Cook's PR-title rule, from the admitted component's release policy.
pub(super) fn cook_pr_title_policy(
    options: &CookRequest,
) -> Result<crate::agent_task_review_dossier::PrTitlePolicy> {
    let component_id = options
        .gates
        .gate_environment
        .admitted_component_id
        .clone()
        .or_else(|| cook_repository_identity_component_id(&options.identity.initial_plan));
    let component = component_id
        .as_deref()
        .map(cook_registered_component_by_id)
        .transpose()?
        .flatten();
    Ok(crate::agent_task_review_dossier::PrTitlePolicy::for_component(component.as_ref()))
}

/// Record the controller-owned gate population as structured request data
/// before the immutable Cook recipe is persisted. Private gate programs
/// deliberately remain undisclosed; only their count is recorded.
pub(super) fn project_controller_owned_gate_contract(options: &mut CookRequest) {
    let verify = options.gates.verify.clone();
    let private_verify_count = options.gates.private_verify.len();
    if verify.is_empty() && private_verify_count == 0 {
        return;
    }
    for request in &mut options.identity.initial_plan.tasks {
        crate::agent_task_prompt_template::merge_homeboy_facts(
            request,
            serde_json::json!({
                "gates": {
                    "owner": "controller",
                    "verify": verify,
                    "private_verify_count": private_verify_count,
                }
            }),
        );
    }
}

/// Render the operator's attempt template (if any) once every Cook fact has
/// been projected onto the initial tasks. Homeboy appends no prose of its own.
pub(super) fn project_attempt_prompt_template(options: &mut CookRequest) {
    for request in &mut options.identity.initial_plan.tasks {
        crate::agent_task_prompt_template::apply_prompt_template(
            request,
            crate::agent_task_prompt_template::PromptPurpose::Attempt,
        );
    }
}

/// Executes one provider attempt while cook retains ownership of promotion,
/// gates, retries, and finalization.
pub trait AgentTaskCookAttemptDispatcher: Send + Sync + std::fmt::Debug {
    /// Durable, generic transport descriptor used to reconstruct this
    /// dispatcher in a fresh controller process.
    fn durable_recipe(&self) -> Result<Value>;

    /// Establish transport readiness before the cook pins its runtime
    /// generation. A reconnect can promote the runner runtime, which must not
    /// wait on the cook that needs that reconnect.
    fn prepare_for_cook(&self) -> Result<()> {
        Ok(())
    }

    /// External transports identify dispatch failures before a provider can
    /// execute so candidate recovery remains distinct from provider failures.
    fn pre_execution_failure_phase(&self) -> &'static str {
        "cook_pre_execution"
    }

    /// `derived_cook_baseline` is process-local authority for a gate-fix retry.
    /// Implementations must not serialize it into the provider request.
    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()>;
}

/// Durable input compiled before Cook starts. This is the only production
/// request accepted by [`CookService`].
#[derive(Debug, Clone)]
pub struct CookRequest {
    pub identity: CookIdentity,
    pub workspace: CookWorkspace,
    pub provider_transport: CookProviderTransport,
    /// Shared deterministic verification gate fields, factored out of the
    /// per-field duplication that previously spanned the loop/promote types.
    pub gates: VerifyGateOptions,
    pub retry_policy: CookRetryPolicy,
    pub finalization: CookFinalization,
    pub harvest_context: crate::agent_task_scheduler::HarvestExecutionContext,
}

#[derive(Debug, Clone)]
pub struct CookIdentity {
    pub cook_id: String,
    pub initial_run_id: String,
    /// Controller-compiled first attempt. The cook service owns dispatching it
    /// through the same local-or-Lab transport used by gate-feedback retries.
    pub initial_plan: AgentTaskPlan,
}

#[derive(Debug, Clone)]
pub struct CookWorkspace {
    pub to_worktree: String,
    pub source_worktree_path: Option<PathBuf>,
    pub task_base_sha: Option<String>,
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct CookBasePreparation {
    pub schema: &'static str,
    pub declared_base: String,
    pub base_sha: Option<String>,
    pub provenance: &'static str,
    pub remote_freshness: &'static str,
    pub topology: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct CookProviderTransport {
    pub provider_invocation: Option<CommandInvocation>,
    /// The route-selected provider transport. `None` executes locally.
    pub attempt_dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
}

#[derive(Debug, Clone)]
pub struct CookRetryPolicy {
    pub max_attempts: u32,
}

#[derive(Debug, Clone)]
pub struct CookFinalization {
    pub no_finalize: bool,
    /// Publish a newly created PR as a draft while retaining normal finalization.
    pub draft_pr: bool,
    /// Preserve an exhausted, gate-failed candidate as a draft PR.
    pub publish_incomplete_draft: bool,
    /// Leave an unpublished Cook's review form on its task's GitHub issue.
    pub tracker_note: bool,
    pub base: String,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub protected_branches: Vec<String>,
}

pub(super) const COOK_CONTINUE_ROUTE_SCHEMA: &str = "homeboy/agent-task-cook-continue-route/v1";

/// Record that the Cook continuation lifecycle route (`agent-task recover`) selected this exact attempt.
/// This is durable route authority, not caller-controlled plan metadata.
pub(super) fn authorize_cook_continue_route(
    lifecycle_store: &AgentTaskLifecycleStore,
    cook_id: &str,
    run_id: &str,
) -> Result<()> {
    agent_task_lifecycle::exact_record_in_store(lifecycle_store, run_id)?;
    lifecycle_store.record_metadata_value(
        run_id,
        "cook_continue_route",
        serde_json::json!({
            "schema": COOK_CONTINUE_ROUTE_SCHEMA,
            "cook_id": cook_id,
            "run_id": run_id,
        }),
    )
}

/// A Cook outcome.
///
/// `Serialize` is written by hand so the derived completion projection is
/// emitted by every producer without duplicating it across report constructors.
#[derive(Debug, Clone)]
pub struct AgentTaskCookReport {
    pub schema: &'static str,
    pub cook_id: String,
    pub latest_run_id: Option<String>,
    pub history_run_ids: Vec<String>,
    pub invocation_run_ids: Vec<String>,
    pub status: CookStatus,
    /// Whether the Cook will advance on its own, declared by the exit that
    /// produced this report rather than inferred from `status`.
    ///
    /// Consumers waiting on completion should read this instead of
    /// pattern-matching the report status.
    pub disposition: CookDisposition,
    pub attempts: Vec<AgentTaskCookAttemptReport>,
    pub finalization: Option<Value>,
    /// A verified review outcome that intentionally has no candidate to promote
    /// or finalize.
    pub intentional_no_change: Option<AgentTaskCookIntentionalNoChangeReport>,
    /// Candidate authority is separate from `latest_run_id`, which remains the
    /// chronological invocation/index compatibility field.
    pub selected_candidate: Option<Value>,
    pub stop_reason: Option<String>,
    /// Preserves the lifecycle-owned failure boundary when cook stops before
    /// provider dispatch instead of collapsing it into an attempt-budget result.
    pub terminal_phase: Option<String>,
    pub terminal_failure_classification: Option<String>,
    /// Bounded causal facts for an initial provider-command failure. Complete
    /// command evidence remains in the durable run named by `evidence_ref`.
    pub primary_failure: Option<AgentTaskCookPrimaryFailure>,
    /// One bounded, operator-facing line per provider route this run skipped
    /// as unready before dispatch, prefixed once with the route that actually
    /// ran (#15881). A credentials-dead primary is an operator-visible
    /// condition, not just an event-log line.
    pub provider_route_notes: Vec<String>,
    /// Generic durable recovery coordinates for a Cook that stopped after its
    /// recipe was materialized. It may contain a bounded causal provider-command
    /// projection; expanded evidence remains available through `diagnose`.
    pub failure_context: Option<AgentTaskCookFailureContext>,
    /// Store roots that produced this report. Kept out of the wire document so
    /// derived completion evidence remains bound to the execution installation.
    pub(crate) report_stores: Option<(CookRecipeStore, AgentTaskLifecycleStore)>,
}

/// The end-to-end publication state of a Cook candidate.
///
/// Provider success is evidence that a candidate was produced; it is not proof
/// that the requested PR exists. This additive v1 projection leaves the legacy
/// `status` and `finalization` payloads intact for persisted consumers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookCompletion {
    pub schema: &'static str,
    /// The selected provider candidate, when one was durably produced.
    pub candidate_produced: bool,
    /// Whether this Cook requested controller-side PR finalization.
    pub finalization_requested: bool,
    /// `true` only when durable finalization evidence identifies a PR.
    pub pr_finalized: bool,
    /// Closed end-to-end outcome vocabulary for consumers that must not treat
    /// a provider-only success as Cook completion.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<AgentTaskCookRecoveryAction>,
}

/// Compile a Cook with a caller-owned runtime-readiness cache. Batch callers
/// share this cache so identical provider/runtime/model verdicts probe once.
pub fn compile_cook_attempt_with_readiness_cache(
    options: CookRequest,
    dispatch: AgentTaskDispatchCommand,
    readiness_cache: &mut crate::agent_task_provider::ProviderRuntimeReadinessCache,
) -> Result<CookRequest> {
    let catalog = crate::agent_task_provider::AgentTaskProviderCatalog::discover();
    compile_cook_attempt_with_catalog_and_readiness_cache(
        options,
        dispatch,
        &catalog,
        readiness_cache,
    )
}

/// Compile a Cook against one caller-supplied provider catalog. This keeps the
/// preflight and plan construction on the same provider snapshot.
pub fn compile_cook_attempt_with_catalog_and_readiness_cache(
    options: CookRequest,
    dispatch: AgentTaskDispatchCommand,
    catalog: &crate::agent_task_provider::AgentTaskProviderCatalog,
    readiness_cache: &mut crate::agent_task_provider::ProviderRuntimeReadinessCache,
) -> Result<CookRequest> {
    let mut options = compile_cook_attempt_static_with_catalog_and_readiness_cache(
        options,
        dispatch,
        catalog,
        readiness_cache,
    )?;
    // Admission receives the Cook's already-resolved absolute deadline. The
    // provider readiness owner applies it to cache waits and its process tree.
    if let Some(deadline) = current_cook_deadline() {
        let deadline_unix_ms = options
            .identity
            .initial_plan
            .options
            .execution_budget
            .deadline_unix_ms
            .map_or(deadline.deadline_unix_ms(), |existing| {
                existing.min(deadline.deadline_unix_ms())
            });
        options
            .identity
            .initial_plan
            .options
            .execution_budget
            .deadline_unix_ms = Some(deadline_unix_ms);
    }
    // Lab placement proves provider readiness on the runner with the runner's
    // credentials (#15198); only local placement is admitted live here.
    match crate::agent_task_provider::admit_plan_provider_dispatchability(
        &options.identity.initial_plan,
        catalog,
        readiness_cache,
        homeboy_core::parsed_command_preflight::captured_result()
            .as_ref()
            .map(|result| &result.placement),
    ) {
        Ok(selected_plan) => {
            super::execution::preflight_plan_secret_env(&selected_plan)?;
            options.identity.initial_plan = selected_plan;
        }
        Err(error) if error.retryable == Some(true) => {}
        Err(error) => return Err(error),
    }
    Ok(options)
}

/// Compile deterministic Cook inputs without invoking provider-owned live
/// readiness. Preview callers use this inside their static planner before a
/// separate provider-owned admission receives the Cook deadline.
pub fn compile_cook_attempt_static_with_catalog_and_readiness_cache(
    mut options: CookRequest,
    dispatch: AgentTaskDispatchCommand,
    catalog: &crate::agent_task_provider::AgentTaskProviderCatalog,
    readiness_cache: &mut crate::agent_task_provider::ProviderRuntimeReadinessCache,
) -> Result<CookRequest> {
    validate_single_cook_prompt_source(
        dispatch.prompt.as_deref(),
        &dispatch.tasks,
        dispatch.core.tasks_json.as_deref(),
    )?;
    let initial_route =
        agent_task_dispatch_service::resolve_cook_initial_provider_route_with_catalog(
            dispatch.clone(),
            catalog,
        )?;
    let request = agent_task_dispatch_service::resolve_dispatch_request(dispatch)?;
    // Preserve the early static rejection for a single route. Rotation plans
    // are checked as complete effective plans below so an unavailable primary
    // cannot hide a valid fallback. The in-tree test double is not a catalog
    // provider; plan admission selects it through the same `is_fixture_backend`
    // gate [`crate::agent_task_provider::admit_plan_provider_dispatchability`]
    // applies, so the static preflight must not reject it ahead of that gate.
    if initial_route
        .rotation
        .as_ref()
        .is_none_or(|rotation| rotation.entries.is_empty())
        && !crate::agent_task_provider::is_fixture_backend(&initial_route.backend)
    {
        crate::agent_task_provider::preflight_provider_dispatchability(
            catalog,
            &initial_route.backend,
            initial_route.selector.as_deref(),
            initial_route.model.as_deref(),
            &serde_json::Value::Object(Default::default()),
            false,
            readiness_cache,
        )?;
    }
    options.identity.initial_plan =
        crate::agent_task_dispatch_plan::build_dispatch_plan_with_provider_requirements(
            &request,
            |backend, selector| catalog.provider_requires_cwd_git_checkout(backend, selector),
        )?;
    super::execution::preflight_plan_secret_env(&options.identity.initial_plan)?;
    Ok(options)
}

pub(crate) fn apply_resource_guard_stop(
    report: &mut AgentTaskRunResult<AgentTaskCookReport>,
    metadata: &Value,
    run_id: &str,
    runner_id: Option<&str>,
) {
    let Some(stop) = homeboy_runner_contract::find_resource_guard_stop(metadata) else {
        return;
    };
    let violation = &stop.violation;
    let remedy = AgentTaskCookRecoveryAction {
        action: "raise_resource_guard".to_string(),
        command: violation.remedy(),
    };
    let diagnostic = serde_json::json!({
        "code": violation.blocker_code(),
        "message": violation.operator_message(),
        "observed": violation.observed_summary(),
    });
    report.value.terminal_phase = Some("resource_guard".to_string());
    report.value.terminal_failure_classification = Some(violation.blocker_code());
    report.value.stop_reason = Some(violation.operator_message());
    report.value.primary_failure = Some(AgentTaskCookPrimaryFailure {
        schema: "homeboy/agent-task-cook-primary-failure/v1",
        provider_id: runner_id.unwrap_or("runner").to_string(),
        operation: violation.blocker_code(),
        phase: "resource_guard".to_string(),
        exit_code: stop.exit_code.unwrap_or(1),
        stderr_excerpt: violation.observed_summary(),
        evidence_ref: format!("homeboy://agent-task/run/{run_id}/logs"),
        next_action: remedy.clone(),
        diagnostic: Some(diagnostic.clone()),
    });
    if let Some(context) = report.value.failure_context.as_mut() {
        context.phase = "resource_guard".to_string();
        context.reason_code = violation.blocker_code();
        context.recovery_reason = violation.remedy();
        context.next_actions = vec![remedy.clone()];
        context.legal_actions = vec![remedy];
        context.diagnostic = Some(diagnostic);
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookAttemptReport {
    pub attempt: u32,
    pub run_id: String,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<AgentTaskPromotionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<AgentTaskCookLoopReport>,
}

pub fn preflight_cook_continuation_admission_for_observation(
    options: &CookRequest,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Result<Vec<&'static str>> {
    if let Some(error) = cook_continuation_owner_denial(record, &options.identity.cook_id)? {
        return Err(error);
    }
    let mut options = options.clone();
    canonicalize_native_cook_workspace(&mut options)?;
    let verification_pending_continuation =
        super::CookDelivery::from_run_metadata(&record.metadata)
            .is_some_and(|delivery| delivery.awaits_verification());
    // Attempts that can bypass provider execution need durable model evidence
    // before they can reach promotion or finalization. A retryable
    // pre-execution failure has no executed model yet by definition.
    validate_cook_candidate_group(&options.identity.initial_plan)?;
    let mut execution_only = Vec::new();
    if !verification_pending_continuation
        && !cook_workspace_lookup_pending(&options.identity.initial_plan)
        && options.provider_transport.attempt_dispatcher.is_none()
    {
        execution_only.push("provider_workspace_resolution");
    }
    if cook_run_record_needs_execution(record) {
        execution_only.push("provider_execution");
    }
    Ok(execution_only)
}

pub fn live_owner_continuation_denial(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> Option<Error> {
    let child_owner_pid = agent_task_lifecycle::cook_run_state::continuation_child_owner(record);
    if record.state().is_terminal() {
        child_owner_pid?;
    } else if !record.owner_process_is_running() && child_owner_pid.is_none() {
        return None;
    }
    let phase = record
        .metadata
        .get("promotion_progress")
        .and_then(|progress| progress.get("phase"))
        .and_then(Value::as_str)
        .or_else(|| {
            record
                .metadata
                .pointer("/cook_progress/phase")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            record.metadata["provider_executions"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|execution| execution["state"] == "running")
                .map(|_| "provider_execution")
        })
        .unwrap_or("post_provider_work");
    let mut error = Error::validation_invalid_argument(
        "cook_continuation",
        "continuation is blocked while the original local owner is still running",
        Some(record.run_id.clone()),
        None,
    );
    error.details["continuation_admission"] = serde_json::json!({
        "first_authoritative_denial": "live_owner_in_progress",
        "owner_pid": child_owner_pid.or_else(|| record.owner_pid()),
        "phase": phase,
        "status_command": format!("homeboy agent-task status {}", record.run_id),
        "logs_command": format!("homeboy runs logs {}", record.run_id),
    });
    Some(error)
}

/// Refuse continuation while this Cook has a canonical runtime driver. The
/// kernel lock is Cook-scoped, so unlike a lifecycle `runner_pid` it cannot
/// confuse a live batch coordinator driving siblings with this child's owner.
/// Whether a live controller (another process, or another thread of this one)
/// is driving `cook_id` right now. Observers use this to tell a Cook that is
/// between attempt states from one that has stopped.
pub fn cook_driver_is_live(cook_id: &str) -> Result<bool> {
    Ok(CookRecipeStore::from_current_data_root()?
        .foreign_cook_driver(cook_id)?
        .is_some())
}

/// Whether a held Cook driver lock owns `record` as unspawned work: the record
/// names its Cook, claims neither a runner process (`runner_pid`) nor a runner
/// job, and some controller (this process included) holds that Cook's driver
/// lock. Such an attempt is waiting on its driver (admission, worktree or
/// provider lookup), not dead, so it is never `missing_runner_pid` (#19182).
pub(crate) fn cook_driver_owns_unspawned_attempt(
    store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    if record.state().is_terminal()
        || record.owner_pid().is_some()
        || record.runner_job_id().is_some()
    {
        return false;
    }
    let Some(cook_id) = record
        .metadata
        .get("cook_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        return false;
    };
    CookRecipeStore::for_lifecycle_store(store)
        .cook_driver_held(cook_id)
        .unwrap_or(false)
}

pub fn cook_continuation_owner_denial(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
    cook_id: &str,
) -> Result<Option<Error>> {
    if let Some(error) = live_owner_continuation_denial(record) {
        return Ok(Some(error));
    }
    let store = CookRecipeStore::from_current_data_root()?;
    let Some(owner) = store.foreign_cook_driver(cook_id)? else {
        return Ok(None);
    };
    let phase = record
        .metadata
        .pointer("/promotion_progress/phase")
        .and_then(Value::as_str)
        .or_else(|| {
            record
                .metadata
                .pointer("/cook_progress/phase")
                .and_then(Value::as_str)
        })
        .unwrap_or("cook_driver");
    let mut error = Error::validation_invalid_argument(
        "cook_continuation",
        "continuation is blocked while the original local owner is still running",
        Some(record.run_id.clone()),
        None,
    );
    error.details["continuation_admission"] = serde_json::json!({
        "first_authoritative_denial": "live_owner_in_progress",
        "owner_pid": owner.trim().parse::<u32>().ok(),
        "phase": phase,
        "status_command": format!("homeboy agent-task status {}", record.run_id),
        "logs_command": format!("homeboy runs logs {}", record.run_id),
    });
    Ok(Some(error))
}

pub fn cook_continuation_replays_provider(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    cook_run_record_needs_execution(record)
}

pub(super) fn reserve_cook_materialization_capacity(
    lifecycle_store: &AgentTaskLifecycleStore,
    workspace: &std::path::Path,
) -> Result<homeboy_core::capacity::CapacityReservation> {
    let demand = homeboy_core::capacity::demand_for_tree(workspace)?;
    homeboy_core::capacity::reserve_projected_capacity(
        &lifecycle_store.controller_scratch_root(),
        "Cook controller scratch and workspace materialization",
        demand,
        homeboy_core::capacity::CapacityReserve::configured(),
    )
}

/// Surface the same filesystem pressure the materialization capacity reserve
/// would later hit, in orchestration terms, before an unrelated build or write
/// fails on the byte reserve. Advisory while above the reserve; exhausted
/// filesystems refuse admission with a measured, attributed error.
pub fn preflight_cook_materialization_capacity(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<homeboy_core::capacity::CapacityPreflight> {
    Ok(homeboy_core::capacity::preflight_capacity(
        &lifecycle_store.controller_scratch_root(),
        "Cook controller scratch and workspace materialization",
        homeboy_core::capacity::CapacityReserve::configured(),
    ))
}

/// Record an advisory-only disk-pressure observation alongside the orchestration
/// state, naming the reclaim routes an operator can run before the next
/// capacity reserve refuses admission.
pub(super) fn record_cook_disk_pressure_advisory(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    preflight: &homeboy_core::capacity::CapacityPreflight,
) -> Result<()> {
    let advisory = serde_json::json!({
        "schema": "homeboy/cook-disk-pressure-advisory/v1",
        "subject": preflight.subject,
        "status": preflight.status,
        "available_bytes": preflight.available_bytes,
        "available_inodes": preflight.available_inodes,
        "reserve_bytes": preflight.reserve_bytes,
        "reserve_inodes": preflight.reserve_inodes,
        "warning": preflight.warning,
        "reclaim_actions": [
            "homeboy worktree reclaim --apply",
            "homeboy cleanup --apply",
        ],
    });
    lifecycle_store.record_metadata_value(run_id, "cook_workspace_disk_pressure_advisory", advisory)
}

/// Typed execution intent. Continuation authorization is no longer encoded by
/// selecting a different public runner.

/// Reviewers re-run a Cook's visible gates, so at least one `--verify` gate
/// must yield a reviewer-safe command. The rule is static on the gate text:
/// admission and preview call this before any workspace or provider work, and
/// the spine re-checks it for every Cook it runs. The error names each rejected
/// token and the rule it hit.
pub fn validate_reviewer_visible_gates(verify: &[String], cook_id: Option<&str>) -> Result<()> {
    if verify.is_empty()
        || verify.iter().any(|command| {
            crate::agent_task_review_dossier::reviewer_safe_command(command).is_some()
        })
    {
        return Ok(());
    }
    let mut rejected: Vec<String> = Vec::new();
    for command in verify {
        for unsafe_token in crate::agent_task_review_dossier::reviewer_unsafe_tokens(command) {
            let entry = format!("`{}` ({})", unsafe_token.token, unsafe_token.rule);
            if !rejected.contains(&entry) {
                rejected.push(entry);
            }
        }
    }
    let rejected = if rejected.is_empty() {
        "the gate command is empty".to_string()
    } else {
        format!("rejected tokens: {}", rejected.join(", "))
    };
    Err(Error::validation_invalid_argument(
        "verification",
        format!(
            "Cook has no visible gate that can produce a reviewer-safe command; {rejected}. Pass machine-local values as `--gate-env NAME=<path>` and reference \"$NAME\" in --verify, or assign them inline in the gate (`NAME=<path> && bash \"$NAME\"`); move checks that need runtime-computed local values into --private-verify, before provider work"
        ),
        cook_id.map(str::to_string),
        None,
    ))
}

/// Select from durable attempts, including attempts preceding a resumed invocation.
/// An ungated provider result must never displace a gate-failed candidate.
pub(super) fn publish_latest_incomplete_draft(
    options: &CookRequest,
    lifecycle_store: &AgentTaskLifecycleStore,
) -> Result<()> {
    if !options.finalization.publish_incomplete_draft || options.finalization.no_finalize {
        return Ok(());
    }
    let recipes = super::cook_recipe::CookRecipeStore::from_environment()?;
    let recipe = recipes.load_recipe(&options.identity.cook_id)?;
    for attempt in recipe.attempts.iter().rev() {
        let record = lifecycle_store.read_record(&attempt.run_id)?;
        if let Some(promotion) = super::recorded_promotion_report(&record.metadata)? {
            let promotion: AgentTaskPromotionReport =
                serde_json::from_value(promotion).map_err(|error| {
                    Error::git_command_failed(format!("invalid promotion: {error}"))
                })?;
            if promotion.status
                == crate::agent_task_promotion::AgentTaskPromotionStatus::Gate(
                    GateVerdict::GateFailed,
                )
                && !promotion.changed_files.is_empty()
            {
                return super::cook_promotion::finalize_or_load_cook_pr_with_backend_with_stores(
                    &recipes,
                    lifecycle_store,
                    options,
                    &attempt.run_id,
                    &promotion,
                    &mut crate::agent_task_finalization::RealAgentTaskPrFinalizationBackend,
                )
                .map(|_| ());
            }
        }
    }
    Ok(())
}

/// A held draft's title, in precedence order: the operator's `--title`, the
/// agent's specific review-form title, the tracker issue title, then the goal's
/// headline. The title is made conventional when the component's policy needs it.
/// A Cook id or file list is not a title a reviewer can act on (#17919, #18356).
pub(super) fn draft_title(
    operator_title: Option<&str>,
    agent_title: Option<&str>,
    tracker_title: Option<&str>,
    goal: Option<&str>,
    title_policy: crate::agent_task_review_dossier::PrTitlePolicy,
) -> String {
    let title = [operator_title, agent_title, tracker_title]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|title| !title.is_empty())
        .or_else(|| goal.and_then(goal_headline))
        .unwrap_or("Deliver candidate changes");
    match title_policy {
        crate::agent_task_review_dossier::PrTitlePolicy::ConventionalCommit
            if !title_policy.accepts(title) =>
        {
            format!("fix: {title}")
        }
        _ => title.to_string(),
    }
}

/// The goal's first non-empty line, stripped of Markdown heading marks, when it
/// is short enough to read as a title. A `--goal` is often a full prompt.
pub(super) fn goal_headline(goal: &str) -> Option<&str> {
    const MAX_TITLE_CHARS: usize = 100;
    goal.lines()
        .map(|line| line.trim().trim_start_matches('#').trim())
        .find(|line| !line.is_empty())
        .filter(|line| line.chars().count() <= MAX_TITLE_CHARS)
}

#[cfg(test)]
mod publication_title_tests {
    use super::draft_title;
    use crate::agent_task_review_dossier::PrTitlePolicy;

    #[test]
    fn title_fallback_order_and_policy() {
        let prompt = "# Fix adopted titles\n\nLong prompt body...";
        let long = "x".repeat(101);
        // (operator, agent, tracker, goal, plain, conventional)
        let cases = [
            (
                Some("Operator"),
                Some("feat: agent"),
                Some("Issue"),
                Some("Goal"),
                "Operator",
                "fix: Operator",
            ),
            (
                Some(" "),
                Some("feat: agent"),
                Some("Issue"),
                Some("Goal"),
                "feat: agent",
                "feat: agent",
            ),
            (
                None,
                None,
                Some("Issue"),
                Some("Goal"),
                "Issue",
                "fix: Issue",
            ),
            (
                None,
                None,
                None,
                Some(prompt),
                "Fix adopted titles",
                "fix: Fix adopted titles",
            ),
            (
                None,
                None,
                None,
                Some(long.as_str()),
                "Deliver candidate changes",
                "fix: Deliver candidate changes",
            ),
        ];
        for (operator, agent, tracker, goal, plain, conventional) in cases {
            assert_eq!(
                draft_title(operator, agent, tracker, goal, PrTitlePolicy::Plain),
                plain
            );
            assert_eq!(
                draft_title(
                    operator,
                    agent,
                    tracker,
                    goal,
                    PrTitlePolicy::ConventionalCommit
                ),
                conventional
            );
        }
    }
}

/// Stop reason for a Cook whose gate supervision stopped a gate at its
/// wall-clock or no-progress limit, even after the automatic re-gate. Names the
/// gate and the limit (never a private command) and the re-gate step.
pub(super) fn gate_budget_exceeded_stop_reason(
    promotion: &AgentTaskPromotionReport,
    cook_id: &str,
) -> String {
    let stopped = promotion
        .deterministic_gates
        .iter()
        .filter_map(|gate| {
            let limit = match gate.termination {
                crate::agent_task_gate::AgentTaskGateTermination::TimedOut => "wall-clock",
                crate::agent_task_gate::AgentTaskGateTermination::NoProgress => "no-progress",
                _ => return None,
            };
            let regated = if gate.supervision_regate.is_some() {
                ", again after one automatic re-gate"
            } else {
                ""
            };
            Some(format!(
                "gate `{}` stopped at its {limit} limit{regated}",
                gate.id
            ))
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "stop: gate infrastructure timeout; {stopped}. The gate never reached a verdict, so the candidate is preserved and unverified, not failed. Run `homeboy agent-task recover {cook_id}` to re-gate it; if the suite needs longer, re-gate with `homeboy agent-task recover {cook_id} --regate --gate-timeout-seconds <SECONDS> --gate-no-progress-timeout-seconds <SECONDS>` (0 disables the no-progress limit)"
    )
}

/// A deferred gate never executed, so the stop reason names each typed cause
/// and the concrete next step instead of a candidate verdict (#16932).
pub(super) fn gate_environment_unavailable_stop_reason(
    promotion: &AgentTaskPromotionReport,
    cook_id: &str,
) -> String {
    if let Some(failure) = serde_json::to_value(promotion)
        .ok()
        .and_then(|report| super::gate_failure::gate_failure_projection(&report))
        .filter(|failure| failure["error_code"] == "runner.lab_transport_failure")
    {
        if let Some(connect) = failure["next_action"].as_str() {
            return format!("Blocked: {}. Candidate preserved. Next: {connect} && homeboy agent-task recover {} --regate",
                failure["message"].as_str().unwrap_or("runner transport unavailable"),
                homeboy_core::error::posix_quote_arg(cook_id));
        }
    }
    let causes = promotion
        .deterministic_gates
        .iter()
        .filter(|gate| {
            matches!(
                gate.status,
                crate::agent_task_gate::AgentTaskGateStatus::Deferred
                    | crate::agent_task_gate::AgentTaskGateStatus::Unavailable
            )
        })
        .map(|gate| match gate.non_execution_cause() {
            Some(cause) => format!("gate `{}`: {cause}", gate.id),
            None => format!(
                "gate `{}` could not execute under the resolved placement",
                gate.id
            ),
        })
        .collect::<Vec<_>>();
    let causes = if causes.is_empty() {
        "at least one deterministic gate did not execute".to_string()
    } else {
        causes.join("; ")
    };
    format!(
        "{causes}. The candidate patch is promoted and unverified, not rejected. Next: re-run once \
         the required environment (e.g. a ready Lab runner) is available, or re-verify with \
         `homeboy agent-task recover {cook_id}`"
    )
}

#[cfg(test)]
#[path = "cook_tests.rs"]
pub(super) mod tests;

/// Wall-clock budget behaviour at the boundaries the Cook loop checks.
///
/// These cover the decision and the contract it produces. The loop plumbing
/// that calls them is exercised by the integration cases in `cook_tests.rs`.
#[cfg(test)]
mod cook_deadline_tests {
    use super::*;
    use crate::agent_task_timeout::{
        current_cook_deadline, now_unix_ms, with_current_cook_deadline,
    };

    fn expired() -> CookDeadline {
        CookDeadline::from_unix_ms(now_unix_ms().saturating_sub(1))
    }

    /// No budget never expires, a live budget must not stop a Cook that still
    /// has time (firing early kills real work), and an expired one trips.
    #[test]
    fn cook_deadline_boundary_trips_only_after_expiry() {
        assert_eq!(current_cook_deadline(), None);
        assert_eq!(expired_cook_deadline(), None);
        with_current_cook_deadline(
            Some(CookDeadline::from_unix_ms(now_unix_ms() + 3_600_000)),
            || assert_eq!(expired_cook_deadline(), None),
        );
        let deadline = expired();
        with_current_cook_deadline(Some(deadline), || {
            assert_eq!(expired_cook_deadline(), Some(deadline));
        });
    }

    #[test]
    fn dead_owner_does_not_block_persisted_continuation_states() {
        let mut record: agent_task_lifecycle::AgentTaskRunRecord =
            serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-run/v1",
                "run_id": "run-dead-owner",
                "plan_id": "plan",
                "state": "candidate_recoverable",
                "submitted_at": "2026-01-01T00:00:00Z",
                "plan_path": "/plan",
                "metadata": { "runner_pid": u32::MAX }
            }))
            .expect("record");
        super::super::set_cook_delivery_artifact_for_test(
            &mut record.metadata,
            "/promotion",
            serde_json::json!({ "status": "verification_pending" }),
        );
        assert!(live_owner_continuation_denial(&record).is_none());
    }

    #[test]
    fn terminal_child_ignores_live_batch_coordinator_but_fences_its_own_provider() {
        let mut record: agent_task_lifecycle::AgentTaskRunRecord =
            serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-run/v1",
                "run_id": "run-terminal-child",
                "plan_id": "plan",
                "state": "candidate_recoverable",
                "submitted_at": "2026-01-01T00:00:00Z",
                "plan_path": "/plan",
                "metadata": { "runner_pid": std::process::id() }
            }))
            .expect("record");

        assert!(live_owner_continuation_denial(&record).is_none());

        record.metadata["provider_executions"] = serde_json::json!([{
            "state": "running",
            "owner_pid": std::process::id()
        }]);
        let error =
            live_owner_continuation_denial(&record).expect("live child provider remains fenced");
        assert_eq!(
            error.details["continuation_admission"]["first_authoritative_denial"],
            "live_owner_in_progress"
        );
        assert_eq!(
            error.details["continuation_admission"]["phase"],
            "provider_execution"
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_child_gate_fence_reports_gate_owner_not_live_batch_parent() {
        struct SleepChild(std::process::Child);
        impl Drop for SleepChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let coordinator = SleepChild(
            std::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .expect("start live batch coordinator PID"),
        );
        let gate_owner_pid = std::process::id();
        let mut record: agent_task_lifecycle::AgentTaskRunRecord =
            serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-run/v1",
                "run_id": "run-terminal-child-gate-owner",
                "plan_id": "plan",
                "state": "candidate_recoverable",
                "submitted_at": "2026-01-01T00:00:00Z",
                "plan_path": "/plan",
                "metadata": {
                    "runner_pid": coordinator.0.id(),
                    "promotion_progress": {
                        "active": true,
                        "phase": "gate",
                        "owner_pid": gate_owner_pid
                    }
                }
            }))
            .expect("record");

        let error = live_owner_continuation_denial(&record).expect("live gate owner denial");
        assert_eq!(
            error.details["continuation_admission"]["owner_pid"], gate_owner_pid,
            "the batch parent PID is not the gate owner"
        );
        assert_eq!(error.details["continuation_admission"]["phase"], "gate");

        record.metadata["runner_pid"] = serde_json::json!(u32::MAX);
        let error = live_owner_continuation_denial(&record)
            .expect("live exact-child gate owner fences a dead parent");
        assert_eq!(
            error.details["continuation_admission"]["owner_pid"],
            gate_owner_pid
        );
    }

    /// Expiry must terminalize cleanly and inspectably: a known status, a
    /// declared terminal disposition, a non-zero exit, and a stop reason that
    /// names the boundary and the remedy.
    #[test]
    fn deadline_expiry_at_an_attempt_boundary_terminalizes_as_timed_out() {
        let deadline = expired();
        let context = homeboy_core::test_support::HermeticTestContext::new();
        let store = CookRecipeStore::new(context.path_roots());
        let lifecycle_store = store.lifecycle_store();
        let result = cook_deadline_report(
            CookStores {
                recipe: &store,
                lifecycle: &lifecycle_store,
            },
            "cook-1".to_string(),
            Vec::new(),
            "run-1",
            &deadline,
            "attempt 2 could be started",
        );

        assert_eq!(result.exit_code, 1, "expiry is not a success");
        assert_eq!(result.value.status.as_str(), "timed_out");
        assert!(!result.value.status.is_in_flight());
        assert!(!result.value.status.is_success_exit());
        assert_eq!(result.value.disposition, CookDisposition::Terminal);
        assert_eq!(
            result.value.terminal_phase.as_deref(),
            Some("cook_deadline"),
            "the phase must distinguish a budget stop from any other timeout"
        );
        assert_eq!(
            result.value.terminal_failure_classification.as_deref(),
            Some("timed_out")
        );

        let reason = result.value.stop_reason.clone().expect("a stop reason");
        assert!(reason.contains("--max-duration"), "{reason}");
        assert!(reason.contains("attempt 2 could be started"), "{reason}");
    }
}

#[cfg(test)]
mod io_error_attribution_tests {
    use super::*;

    /// #15297: "IO error" alone gave operators nothing to act on. The bounded
    /// cause carries the failing operation and the OS error.
    #[test]
    fn bounded_error_diagnostic_names_the_failing_io_operation() {
        let error = Error::internal_io(
            "Permission denied (os error 13)",
            Some("write promotion checkpoint".to_string()),
        );

        let diagnostic = bounded_error_diagnostic(&error);

        assert_eq!(
            diagnostic["deepest_cause"]["message"],
            "IO error: write promotion checkpoint: Permission denied (os error 13)"
        );
        assert_eq!(
            diagnostic["message"],
            "IO error: write promotion checkpoint: Permission denied (os error 13)"
        );
    }

    #[test]
    fn bounded_error_diagnostic_preserves_refused_path_through_gate_wrappers() {
        let error = Error::new(
            homeboy_core::ErrorCode::ValidationInvalidArgument,
            "Cook gate failed",
            serde_json::json!({"cause": {
                "code": "validation.invalid_argument",
                "message": "workspace content hash refused a symlink cycle at 'tracked/cycle'",
                "details": {"entry_path": "tracked/cycle"}
            }}),
        );
        let diagnostic = bounded_error_diagnostic(&error);
        assert_eq!(diagnostic["deepest_cause"]["entry_path"], "tracked/cycle");
        assert!(diagnostic["deepest_cause"]["message"]
            .as_str()
            .unwrap()
            .contains("tracked/cycle"));
    }

    #[test]
    fn bounded_error_diagnostic_keeps_specific_messages_unchanged() {
        let error = Error::validation_invalid_argument("field", "bad value", None, None);

        let diagnostic = bounded_error_diagnostic(&error);

        assert_eq!(diagnostic["deepest_cause"]["message"], error.message);
    }

    #[test]
    fn bounded_error_diagnostic_surfaces_a_plain_gate_setup_cause() {
        let error = Error::dependency_step_failed(
            "promotion.gate_setup",
            "destination_gate_workspace".to_string(),
            None,
            Vec::new(),
            Vec::new(),
            None,
            Some(serde_json::json!({
                "classification": "destination_gate_toolchain",
                "code": "runner.lab_transport_failure",
                "message": "Lab gate transport did not produce verified passing evidence",
                "retry_action": "retry_dependency_hydration",
            })),
        );

        let diagnostic = bounded_error_diagnostic(&error);

        assert_eq!(
            diagnostic["deepest_cause"]["code"],
            "runner.lab_transport_failure"
        );
        assert_eq!(
            diagnostic["deepest_cause"]["message"],
            "Lab gate transport did not produce verified passing evidence"
        );
    }
}

pub(super) use super::cook_spine::*;

pub use super::cook_workspace::*;

pub(crate) use super::cook_follow_up::*;

pub(super) use super::cook_timeout::*;

pub use super::cook_projections::*;

pub(crate) use super::cook_gate_setup::*;

pub use super::cook_driver::*;

pub(crate) use super::cook_admission::*;

/// The wire contract for the additive lifecycle projection.
///
/// These assert two things that are easy to break by accident: that `status`
/// and `exit_code` semantics are untouched, and that `terminal` continues to
/// come from the declared disposition rather than being re-derived from the
/// status string.
#[cfg(test)]
#[path = "cook_projection_tests.rs"]
mod run_lifecycle_projection_tests;
