//! Cook retry admission, interruption claims and progress reporting.

use super::cook::*;

pub(super) fn promotion_operation_key(run_id: &str) -> String {
    format!("promote:{run_id}")
}

/// Lease window for a retry-dispatch operation claim. A detached dispatch may
/// take a while to be accepted by the runner; the lease is generous so a healthy
/// controller completes it, while a crashed controller's lease still elapses.
pub(super) const RETRY_DISPATCH_CLAIM_LEASE: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// How long a dispatched retry waits for the runner daemon's acceptance
/// receipt. Detached Lab dispatch returns once controller staging is queued;
/// the daemon writes the receipt only after staging materializes the
/// workspace, hydrates dependencies and hands the job off, which takes
/// minutes. Checking once, immediately, failed every Lab retry even though the
/// runner accepted and ran it (#15364). Bounded by the dispatch claim lease.
pub(super) const RETRY_DISPATCH_ACCEPTANCE_WAIT: std::time::Duration =
    std::time::Duration::from_secs(20 * 60);

/// Poll for the receiver's durable acceptance receipt until `timeout`.
/// Returns early once the attempt reaches a terminal state without one (the
/// handoff failed and recorded why), so a rejected dispatch is not waited out.
pub(super) fn await_dispatch_acceptance_receipt(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    operation_key: &str,
    timeout: std::time::Duration,
) -> Result<Option<Value>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(receipt) = agent_task_lifecycle::dispatch_acceptance_receipt_in_store(
            lifecycle_store,
            run_id,
            operation_key,
        )? {
            return Ok(Some(receipt));
        }
        let terminal = lifecycle_store
            .read_record(run_id)
            .is_ok_and(|record| record.state().is_terminal());
        if terminal || std::time::Instant::now() >= deadline {
            // One last read: the receipt and a terminal projection can land
            // in the same window.
            return agent_task_lifecycle::dispatch_acceptance_receipt_in_store(
                lifecycle_store,
                run_id,
                operation_key,
            );
        }
        std::thread::sleep(dispatch_acceptance_poll_interval());
    }
}

pub(super) fn dispatch_acceptance_poll_interval() -> std::time::Duration {
    #[cfg(test)]
    {
        std::time::Duration::from_millis(10)
    }
    #[cfg(not(test))]
    {
        std::time::Duration::from_secs(5)
    }
}

/// A missing aggregate is a controller interruption, not provider output. Keep
/// its claim separate from retry dispatch because no next run exists yet.
pub(super) const PRE_ARTIFACT_INTERRUPTION_CLAIM_LEASE: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// Transport retries recover controller-to-runner delivery, not semantic work.
/// One replacement gives a transient interruption one fresh connection while a
/// persistent infrastructure defect remains visible for repair.
pub(super) const MAX_TRANSPORT_RETRIES_PER_ATTEMPT: usize = 1;

/// `exempt` replacements were spent on transient substrate contention and do
/// not count against the transport budget (#17801).
pub(super) fn transport_retry_available(
    store: &CookRecipeStore,
    cook_id: &str,
    attempt: u32,
    exempt: usize,
) -> Result<bool> {
    Ok(store
        .load_recipe(cook_id)?
        .attempts
        .iter()
        .filter(|recorded| recorded.attempt == attempt)
        .count()
        <= MAX_TRANSPORT_RETRIES_PER_ATTEMPT + exempt)
}

/// Store locks, runner pressure, build skew and other transient substrate
/// refusals are the shared host's state, not this Cook's: they wait instead
/// of consuming the retry budget (#17801).
pub(super) fn transient_infrastructure_failure(error: &Error) -> bool {
    homeboy_core::substrate_wait::is_transient_error(error) || super::is_store_contention(error)
}

/// What the Cook does with a failure before provider start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreProviderRetry {
    /// Transient substrate refusal: wait it out, off the retry budget.
    Wait,
    /// Lost runner mechanics or transport: one fresh `-transport-retry` run.
    Replace,
    /// Deterministic: the same attempt fails the same way, so stop.
    Stop,
}

/// Durable evidence is stronger than an exit code: only a failed attempt
/// with no provider or remote owner may be automatically replaced.
pub(super) fn durable_transient_pre_execution_failure(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> bool {
    let failure = &record.metadata["pre_execution_failure"];
    record.state() == homeboy_lifecycle_contract::RunExecutionState::Failed
        && failure["retryable"] == true
        && failure["failure_classification"] == "transient"
        && failure["provider_executions_consumed"] == 0
        // Older controllers reported this specific observation skew as invalid
        // input. Other validation failures still require operator correction.
        && failure["error_code"].as_str().is_some_and(|code| {
            !code.starts_with("validation.")
                || failure["details"]["observation_unavailable"] == true
                || (failure["phase"] == "lab_handoff_preacceptance"
                    && failure["message"].as_str().is_some_and(|message| {
                        message.contains("verified daemon reports")
                            && message.contains("but typed /jobs exposed")
                    }))
        })
        && super::cook_recipe::has_unambiguous_zero_execution(record)
        && !record.metadata["lab_pre_execution_circuit"].is_object()
        && homeboy_runner_contract::find_resource_guard_stop(&record.metadata).is_none()
}

/// Replace retryable runner-side and transport failures, not deterministic
/// input or resource-guard refusals (#18454).
pub(super) fn pre_provider_retry(
    error: &Error,
    failure: &PreExecutionFailureDetails,
) -> PreProviderRetry {
    if homeboy_runner_contract::find_resource_guard_stop(&error.details).is_some() {
        PreProviderRetry::Stop
    } else if transient_infrastructure_failure(error) {
        PreProviderRetry::Wait
    } else if failure.retryable && !failure.error_code.starts_with("validation.") {
        PreProviderRetry::Replace
    } else {
        PreProviderRetry::Stop
    }
}

/// `{base}-transport-retry`, then `{base}-transport-retry-2`, `-3`, ...: one
/// bounded suffix however many replacements an attempt needs.
pub(super) fn next_transport_retry_run_id(run_id: &str) -> String {
    let (base, replacements) = agent_task_lifecycle::split_transport_retry_suffix(run_id);
    match replacements {
        0 => format!("{base}-transport-retry"),
        n => format!("{base}-transport-retry-{}", n + 1),
    }
}

/// Sleep one backoff step toward a transient-failure deadline. Returns false
/// when the deadline passed or the Cook was cancelled.
pub(super) fn wait_out_transient_failure(
    lifecycle_store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    cook_id: &str,
    started: std::time::Instant,
    replacements: usize,
    error: &Error,
) -> bool {
    use homeboy_core::substrate_wait::SUBSTRATE_WAIT_DEADLINE;
    let cancelled = || {
        lifecycle_store
            .read_cook_index(cook_id)
            .ok()
            .is_some_and(|index| index.cancellation_fence.is_some())
    };
    let unit = if cfg!(test) {
        std::time::Duration::from_millis(5)
    } else {
        std::time::Duration::from_secs(2)
    };
    let backoff = (unit * (1u32 << replacements.min(4))).min(std::time::Duration::from_secs(30));
    let remaining = SUBSTRATE_WAIT_DEADLINE.saturating_sub(started.elapsed());
    if remaining.is_zero() || cancelled() {
        return false;
    }
    homeboy_core::log_status!(
        "wait",
        "Cook {cook_id}: {} — replacing the run in {}s (waited {}s)",
        error.message,
        backoff.min(remaining).as_secs(),
        started.elapsed().as_secs()
    );
    let wake = std::time::Instant::now() + backoff.min(remaining);
    while std::time::Instant::now() < wake {
        if cancelled() {
            return false;
        }
        std::thread::sleep(
            wake.saturating_duration_since(std::time::Instant::now())
                .min(std::time::Duration::from_secs(1)),
        );
    }
    true
}

/// An authorized placement transition gets one new transport admission after a
/// failed route exhausted its own connection budget. Provider execution and the
/// semantic Cook attempt remain untouched; the replacement run consumes this
/// authority because it does not inherit this record metadata.
pub(super) fn transport_admission_reset_available(
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
) -> bool {
    record.is_some_and(|record| {
        record.metadata["transport_admission_reset"]["kind"] == "placement_transition"
            && record.metadata["transport_admission_reset"]["replacement_decision_id"]
                == record.metadata["execution_placement_decision"]["decision_id"]
            && record.metadata["execution_placement_transition"]["kind"]
                == "explicit_local_continuation"
            && record.metadata["execution_placement_transition"]["replacement_decision_id"]
                == record.metadata["execution_placement_decision"]["decision_id"]
    })
}

/// Durable operation key for the dispatch of one retry attempt. A retry is
/// one-per-generated-`run_id`, so the next run id is the stable identity (#8357).
pub(super) fn retry_dispatch_operation_key(next_run_id: &str) -> String {
    format!("dispatch:{next_run_id}")
}

pub(super) fn pre_artifact_interruption_operation_key(run_id: &str) -> String {
    format!("pre-artifact-interruption:{run_id}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreArtifactInterruptionPhase {
    BeforeProviderStart,
    DuringProviderExecution,
    AfterProviderReturn,
    /// The runner reported the attempt's remote command succeeded, so the
    /// provider ran and published its aggregate, but none could be projected
    /// from the runner result. A new attempt would repeat the paid provider
    /// work and discard the candidate the runner result still holds.
    RunnerResultNotProjected,
}

impl PreArtifactInterruptionPhase {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::BeforeProviderStart => "before_provider_start",
            Self::DuringProviderExecution => "during_provider_execution",
            Self::AfterProviderReturn => "after_provider_return_before_aggregate_persistence",
            Self::RunnerResultNotProjected => "runner_result_not_projected",
        }
    }
}

/// Classify a terminal attempt without an aggregate from the durable provider
/// ledger. A reservation is proof that a provider started; absent reservations
/// consume nothing. A runner-placed attempt keeps its provider ledger on the
/// runner, so its succeeded terminal result is the proof there instead. This
/// deliberately does not manufacture aggregate events.
pub(super) fn pre_artifact_interruption_phase(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> PreArtifactInterruptionPhase {
    if runner_reported_success(record) {
        return PreArtifactInterruptionPhase::RunnerResultNotProjected;
    }
    let executions = record.metadata["provider_executions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if executions.is_empty() {
        PreArtifactInterruptionPhase::BeforeProviderStart
    } else if executions
        .iter()
        .any(|execution| execution["state"] == "running")
    {
        PreArtifactInterruptionPhase::DuringProviderExecution
    } else {
        PreArtifactInterruptionPhase::AfterProviderReturn
    }
}

/// Whether the runner job finished successfully and returned its remote
/// command's terminal result with a zero exit code.
pub(super) fn runner_reported_success(record: &agent_task_lifecycle::AgentTaskRunRecord) -> bool {
    record.metadata["runner_job_status"] == "succeeded"
        && record.metadata["runner_job_events"]
            .as_array()
            .is_some_and(|events| {
                events
                    .iter()
                    .any(|event| event["kind"] == "result" && event["data"]["exit_code"] == 0)
            })
}

pub(crate) fn intentional_no_change_from_aggregate(
    aggregate: &crate::agent_task_scheduler::AgentTaskAggregate,
) -> Option<AgentTaskIntentionalNoChange> {
    let outcome = aggregate.selected_outcome().or_else(|| {
        (aggregate.outcomes.len() == 1)
            .then(|| aggregate.outcomes.first())
            .flatten()
    })?;
    (outcome.status == crate::agent_task::AgentTaskOutcomeStatus::Succeeded).then_some(())?;
    (outcome.metadata["intentional_no_change_verified"] == Value::Bool(true)).then_some(())?;
    let declaration = outcome
        .outputs
        .get("provider_run_result")?
        .get("intentional_no_change")?
        .clone();
    serde_json::from_value::<AgentTaskIntentionalNoChange>(declaration)
        .ok()
        .filter(|declaration| declaration.schema == "homeboy/intentional-no-change/v1")
}

pub(super) fn request_requires_substantive_candidate(
    request: &crate::agent_task::AgentTaskRequest,
) -> bool {
    crate::agent_task_cook_loop::request_requires_substantive_candidate(request)
}

pub(super) fn no_change_requires_substantive_candidate(
    declaration: &AgentTaskIntentionalNoChange,
    request: &crate::agent_task::AgentTaskRequest,
) -> bool {
    match declaration.verdict {
        AgentTaskIntentionalNoChangeVerdict::AlreadySatisfied => false,
        AgentTaskIntentionalNoChangeVerdict::Blocked => true,
        AgentTaskIntentionalNoChangeVerdict::InvestigationOnly => {
            request_requires_substantive_candidate(request)
        }
    }
}

pub(super) fn pre_artifact_execution_count(
    record: &agent_task_lifecycle::AgentTaskRunRecord,
) -> u32 {
    record.metadata["provider_executions"]
        .as_array()
        .map(|executions| executions.len().try_into().unwrap_or(u32::MAX))
        .unwrap_or_default()
}

pub(super) fn pre_artifact_interruption_report(
    stores: CookStores<'_>,
    cook_id: String,
    attempts: Vec<AgentTaskCookAttemptReport>,
    run_id: &str,
    phase: PreArtifactInterruptionPhase,
    reason: String,
    exit_code: i32,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let mut report = stores.report(CookReportInput {
        cook_id,
        status: homeboy_core::cook_status::CookStatus::PreArtifactInterruption,
        disposition: CookDisposition::Terminal,
        attempts,
        finalization: None,
        stop_reason: Some(reason),
        exit_code,
        invocation_latest_run_id: Some(run_id),
    });
    report.value.terminal_phase = Some(phase.name().to_string());
    report.value.terminal_failure_classification = Some("pre_artifact_interruption".to_string());
    report
}

/// Terminalize a Cook that ran out of wall-clock budget.
///
/// Reported as [`CookStatus::TimedOut`] — the existing vocabulary entry for
/// "exceeded its wall-clock budget" — rather than a new status, so the batch
/// totals, exit-code classification, and terminality rules that already
/// understand `timed_out` need no teaching. Exit code is 1: the Cook did not
/// finish the work it was asked to do, and an operator has something to act on.
///
/// [`CookDisposition::Terminal`] is declared explicitly: nothing is left
/// carrying this Cook forward, so the orchestrator's completion is due now.
pub(super) fn cook_deadline_report(
    stores: CookStores<'_>,
    cook_id: String,
    attempts: Vec<AgentTaskCookAttemptReport>,
    run_id: &str,
    deadline: &CookDeadline,
    boundary: &str,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let mut report = stores.report(CookReportInput {
        cook_id,
        // The existing vocabulary entry for "exceeded its wall-clock budget"
        // ([`CookStatus::TimedOut`]). Written as the literal every other exit
        // in this file uses; the linkage is pinned by test.
        status: homeboy_core::cook_status::CookStatus::TimedOut,
        disposition: CookDisposition::Terminal,
        attempts,
        finalization: None,
        stop_reason: Some(deadline.stop_reason(boundary)),
        exit_code: 1,
        invocation_latest_run_id: Some(run_id),
    });
    report.value.terminal_phase = Some("cook_deadline".to_string());
    report.value.terminal_failure_classification = Some("timed_out".to_string());
    report
}

/// Claim exactly one recipe continuation for a terminal run that never
/// persisted an aggregate. The claim is on the interrupted run, so concurrent
/// controllers converge before they can append competing recipe attempts.
#[cfg(test)]
pub(super) fn claim_pre_artifact_interruption_retry(
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    plan: &AgentTaskPlan,
) -> Result<Option<(u32, String)>> {
    let recipe_store = CookRecipeStore::from_current_data_root()?;
    let lifecycle_store = AgentTaskLifecycleStore::from_environment()?;
    claim_pre_artifact_interruption_retry_with_stores(
        (&recipe_store, &lifecycle_store),
        cook_id,
        attempt,
        run_id,
        plan,
        false,
    )
}

pub(super) fn claim_pre_artifact_interruption_retry_with_stores(
    stores: (&CookRecipeStore, &AgentTaskLifecycleStore),
    cook_id: &str,
    attempt: u32,
    run_id: &str,
    plan: &AgentTaskPlan,
    replace_semantic_attempt: bool,
) -> Result<Option<(u32, String)>> {
    let (recipe_store, lifecycle_store) = stores;
    let next_attempt = if replace_semantic_attempt {
        attempt
    } else {
        attempt.checked_add(1).ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_recipe.attempts",
                "durable cook attempt sequence is exhausted",
                Some(cook_id.to_string()),
                None,
            )
        })?
    };
    let operation_key = pre_artifact_interruption_operation_key(run_id);
    let replacement_run_id = replace_semantic_attempt.then(|| next_transport_retry_run_id(run_id));
    let recipe_next_attempt = |required_run_id: Option<&str>, require_plan_match: bool| {
        recipe_store.load_recipe(cook_id).map(|recipe| {
            recipe
                .attempts
                .iter()
                .find(|recorded| {
                    recorded.attempt == next_attempt
                        && replacement_run_id
                            .as_ref()
                            .is_none_or(|id| recorded.run_id == *id)
                        && required_run_id.is_none_or(|id| recorded.run_id == id)
                        && (!require_plan_match || recorded.plan == *plan)
                })
                .map(|recorded| recorded.run_id.clone())
        })
    };

    match lifecycle_store.claim_cook_operation(
        run_id,
        &operation_key,
        PRE_ARTIFACT_INTERRUPTION_CLAIM_LEASE,
    )? {
        agent_task_lifecycle::ClaimOutcome::Acquired(_lease) => {
            let next_run_id = replacement_run_id.clone().unwrap_or_else(|| {
                agent_task_lifecycle::cook_attempt_run_id(cook_id, next_attempt)
            });
            if replace_semantic_attempt {
                recipe_store.record_recipe_attempt_replacement_with_plan(
                    cook_id,
                    run_id,
                    &next_run_id,
                    plan,
                )?;
            } else {
                recipe_store.record_recipe_retry_attempt(
                    cook_id,
                    next_attempt,
                    &next_run_id,
                    plan,
                    run_id,
                )?;
            }
            _lease.complete(serde_json::json!({
                "next_attempt": next_attempt,
                "next_run_id": next_run_id,
            }))?;
            Ok(Some((next_attempt, next_run_id)))
        }
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(result) => {
            let recorded_attempt = result["next_attempt"].as_u64();
            let recorded_run_id = result["next_run_id"].as_str();
            // A completed claim and the immutable recipe attempt are already
            // the authority. Concurrent base capture can enrich the recorded
            // plan after this caller read its copy; comparing those copies
            // would reject a legitimate replay of the same durable receipt.
            if let Some(next_run_id) = recorded_run_id
                .map(|id| recipe_next_attempt(Some(id), false))
                .transpose()?
                .flatten()
            {
                if recorded_attempt == Some(u64::from(next_attempt)) {
                    return Ok(Some((next_attempt, next_run_id)));
                }
            }
            {
                Err(Error::internal_unexpected(
                    "pre-artifact interruption continuation claim conflicts with the durable cook recipe",
                ))
            }
        }
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            // A crash after recipe append but before claim completion is safe to
            // finish: the immutable next attempt is already fully identified.
            if let Some(next_run_id) = recipe_next_attempt(replacement_run_id.as_deref(), true)? {
                lifecycle_store.complete_cook_operation(
                    run_id,
                    &operation_key,
                    serde_json::json!({
                        "next_attempt": next_attempt,
                        "next_run_id": next_run_id,
                    }),
                )?;
                Ok(Some((next_attempt, next_run_id)))
            } else {
                Ok(None)
            }
        }
    }
}

/// Lease window for a finalization operation claim. PR finalization (commit,
/// push, `gh pr create`) can take a while; the lease is generous so a healthy
/// controller completes it, while a crashed controller's lease still elapses.
pub(super) const FINALIZATION_CLAIM_LEASE: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

/// Foreground liveness is deliberately bounded. Provider-native progress still
/// wins when available; this durable heartbeat covers quiet providers.
pub(super) const COOK_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
/// Poll durable provider state between samples so post-provider artifact work
/// does not hold the foreground liveness loop for a full heartbeat interval.
pub(super) const COOK_PROVIDER_TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub(super) fn provider_timeout_heartbeat_detail(
    timeout_ms: u64,
    remaining_ms: u64,
    supervision_detail: Option<&str>,
    activity: &CookProviderActivity,
) -> String {
    let mut detail = format!(
        "provider execution is still running; timeout={timeout_ms}ms; remaining={remaining_ms}ms"
    );
    if let Some(summary) = activity.summary_line() {
        detail.push_str("; ");
        detail.push_str(&summary);
    }
    if let Some(supervision_detail) = supervision_detail {
        detail.push_str("; ");
        detail.push_str(supervision_detail);
    }
    detail.chars().take(512).collect()
}
