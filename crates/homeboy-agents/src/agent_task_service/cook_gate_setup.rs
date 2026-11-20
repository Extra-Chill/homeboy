//! Cook gate setup retries and bounded diagnostics.

use super::cook::*;

/// Automatic promotion retries after a gate setup failure (#15364).
pub(super) const GATE_SETUP_PROMOTION_RETRIES: u32 = 2;

/// Whether a promotion error is dependency setup for the gates (install,
/// submodules, toolchain materialization) rather than a verdict on the
/// candidate. These are usually environmental (network, registry, a tool
/// installed after the failure), and retrying promotion is idempotent: the
/// candidate is durable and the operation claim is reacquirable after failure.
///
/// A setup failure whose cause is deterministic configuration (for example a
/// Lab gate rejected in preflight for a missing runner `secret_env` ref) is
/// excluded: retrying cannot change it and only multiplies failed Lab runs
/// (#16121).
pub(super) fn is_gate_setup_failure(error: &Error) -> bool {
    error.code == homeboy_core::ErrorCode::DependencyStepFailed
        && error.details["step_id"] == "promotion.gate_setup"
        && error.retryable != Some(false)
        && error.details["cause"]["retry_action"] != "fix_configuration"
}

/// Run promotion, retrying a bounded number of times with backoff when gate
/// setup fails, so an environmental setup failure no longer strands a finished
/// candidate until an operator runs `agent-task recover`. Any other error, and the
/// final setup failure, are returned unchanged.
/// A gate readiness preflight the Lab transport never ran (promisor
/// hydration, a lost terminal receipt, a runner that stayed under pressure or
/// disconnected) proves nothing about the gates. Failing the Cook on it
/// discarded the run before the provider worked, or a continuation's
/// candidate after it did. Let the Cook continue instead: each gate
/// re-observes the runner, and a gate the transport still cannot run defers
/// with the candidate preserved for `agent-task recover` (#18121). A runner
/// that answered "not ready" and every configuration error still fail here.
pub(super) fn defer_gate_preflight_transport_failure(preflight: Result<()>) -> Result<()> {
    match preflight {
        Err(error) if crate::agent_task_gate::placement::is_lab_gate_transport_failure(&error) => {
            homeboy_core::log_status!(
                "warn",
                "gate readiness preflight did not execute on the Lab runner ({}); continuing so the gates re-observe the runner and a non-executed gate defers with the candidate preserved",
                error.message
            );
            Ok(())
        }
        result => result,
    }
}

pub(super) fn promote_with_gate_setup_retries(
    side_effects: &mut CookSideEffects<'_>,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    run_id: &str,
    mut on_retry: impl FnMut(u32, std::time::Duration, &Error),
) -> Result<AgentTaskPromotionReport> {
    let mut retry = 0;
    loop {
        match side_effects.promote(lifecycle_store, options, run_id) {
            Err(error) if is_gate_setup_failure(&error) && retry < GATE_SETUP_PROMOTION_RETRIES => {
                retry += 1;
                let delay = gate_setup_retry_delay(retry);
                on_retry(retry, delay, &error);
                std::thread::sleep(delay);
            }
            result => return result,
        }
    }
}

/// Repair only the historical, unverified fallback. Explicit bases and transport
/// failures retain their original promotion contract and are never rewritten.
pub(super) fn recover_compatibility_cook_base(
    store: &CookRecipeStore,
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &mut CookRequest,
) -> Result<()> {
    if options.identity.initial_plan.metadata["cook_base_resolution"]["source"]
        != "compatibility_fallback"
    {
        return Ok(());
    }
    let target_path = if Path::new(&options.workspace.to_worktree).is_dir() {
        PathBuf::from(&options.workspace.to_worktree)
    } else {
        let Some(target) =
            homeboy_core::worktree_provider::resolve_native_worktree_mutation_target(
                &options.workspace.to_worktree,
            )?
        else {
            return Ok(());
        };
        target.path
    };
    match crate::agent_task_promotion::capture_declared_base(
        &target_path,
        Some(&options.finalization.base),
    ) {
        Ok(_) => return Ok(()),
        Err(error)
            if error.details["field"] == "base_ref"
                && error
                    .message
                    .contains("was not found on origin before promotion gates") => {}
        Err(error) => return Err(error),
    }
    let Some(remote_ref) = homeboy_core::git::default_remote_branch_until(
        &target_path,
        "origin",
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )?
    else {
        return Err(Error::validation_invalid_argument(
            "base",
            "Cook requires a remote default branch; specify --base",
            None,
            None,
        ));
    };
    let base = remote_ref.strip_prefix("origin/").unwrap_or(&remote_ref);
    let verified = crate::agent_task_promotion::capture_declared_base(&target_path, Some(base))?;
    let resolution = serde_json::json!({
        "base": base,
        "from": remote_ref,
        "source": "remote_head",
        "sha": verified.map(|base| base.sha),
        "evidence_path": target_path,
    });
    let mut recipe = store.load_recipe(&options.identity.cook_id)?;
    recipe.finalization["base"] = serde_json::json!(base);
    for attempt in &mut recipe.attempts {
        if attempt.plan.metadata["cook_base_resolution"]["source"] == "compatibility_fallback" {
            attempt.plan.metadata["cook_base_resolution"] = resolution.clone();
        }
    }
    // The recipe is reconstruction authority. Publish it before the derived
    // controller plan so an interruption cannot restore the guessed base.
    store.persist_recipe(&recipe)?;
    options.finalization.base = base.to_string();
    options.identity.initial_plan.metadata["cook_base_resolution"] = resolution;
    if lifecycle_store.record_exists(&options.identity.initial_run_id)? {
        lifecycle_store.write_controller_plan(
            &options.identity.initial_run_id,
            &options.identity.initial_plan,
        )?;
    }
    Ok(())
}

pub(super) fn gate_setup_retry_delay(retry: u32) -> std::time::Duration {
    #[cfg(test)]
    {
        let _ = retry;
        std::time::Duration::ZERO
    }
    #[cfg(not(test))]
    {
        std::time::Duration::from_secs(30 * u64::from(retry))
    }
}

/// Whether a promotion error is another live owner's exactly-once promotion
/// claim (`operation_in_progress`) rather than a verdict on the candidate.
///
/// A pass that observes `LeaseHeld` did not attempt promotion; the claim's
/// owner is still running the gates and will record its own outcome. That is
/// progress, not a durable failure, so it must not terminalize the Cook or
/// announce "needs attention" (#15472). The decision reuses the claim
/// ledger's own liveness predicate, so a dead or expired owner still falls
/// through to the existing failure path (#10236). A claim owned by this very
/// process cannot be progressing behind us, so it is never treated as live.
pub(super) fn promotion_claim_held_by_live_owner(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    error: &Error,
) -> bool {
    if error.details["field"] != "promotion_operation"
        || error.details["problem"] != "operation_in_progress"
    {
        return false;
    }
    let operation_key = promotion_operation_key(run_id);
    agent_task_lifecycle::operation_lease_is_active_in_store(
        lifecycle_store,
        run_id,
        &operation_key,
    )
    .unwrap_or(false)
}

pub(super) fn promote_with_operation_claim_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    options: &CookRequest,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let operation_key = promotion_operation_key(run_id);
    match lifecycle_store.claim_cook_operation(run_id, &operation_key, PROMOTION_CLAIM_LEASE)? {
        // A prior pass already promoted and recorded the result. Load the durable
        // promotion rather than repeating the external effect.
        agent_task_lifecycle::ClaimOutcome::AlreadyCompleted(_) => {
            promote_or_load_attempt_in_store(lifecycle_store, options, run_id)
        }
        // Another pass holds a still-fresh lease. The persisted-promotion read in
        // `promote_or_load_attempt_in_store` still resolves an already-produced promotion;
        // if none exists yet, promotion proceeds (content-addressed and idempotent
        // on disk). Do not mark the claim completed from here — its owner does.
        agent_task_lifecycle::ClaimOutcome::LeaseHeld => {
            let claim = lifecycle_store.operation_claim(run_id, &operation_key)?;
            let mut error = Error::validation_invalid_argument(
                "promotion_operation",
                "operation_in_progress",
                Some(operation_key),
                Some(vec![cook_recovery_command_in_store(
                    lifecycle_store,
                    run_id,
                    &["recover", run_id],
                )]),
            );
            error.details["claim"] = serde_json::to_value(claim).unwrap_or(Value::Null);
            Err(error)
        }
        // This pass owns the operation. Promote, then record the result as the
        // claim's immutable completion.
        agent_task_lifecycle::ClaimOutcome::Acquired(_lease) => {
            let promotion = match promote_or_load_attempt_in_store(lifecycle_store, options, run_id)
            {
                Ok(promotion) => promotion,
                Err(error) => {
                    _lease.fail(bounded_error_diagnostic(&error))?;
                    return Err(error);
                }
            };
            _lease.complete(
                serde_json::to_value(&promotion)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?,
            )?;
            Ok(promotion)
        }
    }
}

pub(crate) fn bounded_error_diagnostic(error: &Error) -> Value {
    let mut details = homeboy_core::redaction::redact_json(&error.details);
    bound_diagnostic_value(&mut details, 0);
    // Select the cause before bounding nested details, which may replace the
    // path with a depth-limit marker in transport-wrapped failures.
    let deepest_cause = deepest_typed_error(&homeboy_core::redaction::redact_json(&error.details))
        .unwrap_or_else(|| {
            serde_json::json!({
                "code": error.code.as_str(),
                "field": details.get("field"),
                "entry_path": details.get("entry_path"),
                "message": truncate_diagnostic_text(&homeboy_core::redaction::redact_string(
                    &error.attributed_message(),
                )),
            })
        });
    serde_json::json!({
        "status": "failed",
        "code": format!("{:?}", error.code),
        "message": truncate_diagnostic_text(&homeboy_core::redaction::redact_string(
            &error.attributed_message(),
        )),
        "details": details,
        "deepest_cause": deepest_cause,
    })
}

/// A failed nested Homeboy command result is more specific than the transport
/// error that carried it. Keep only its typed routing fields in the projection;
/// the enclosing bounded command evidence remains available to full readers.
pub(super) fn deepest_typed_error(value: &Value) -> Option<Value> {
    causal_typed_error(value)
}

pub(super) fn typed_error_at(value: &Value) -> Option<Value> {
    let fields = value.as_object()?;
    (fields.get("schema").and_then(Value::as_str) == Some("homeboy/command-result/v3")
        && fields.get("success").and_then(Value::as_bool) == Some(false))
    .then_some(())?;
    let error = fields.get("error")?.as_object()?;
    let code = error.get("code")?.as_str()?;
    let message = error.get("message")?.as_str()?;
    let mut diagnostic = serde_json::json!({
        "code": code,
        "field": error.get("details").and_then(|details| details.get("field")),
        "message": truncate_diagnostic_text(message),
    });
    if let Some(path) = error
        .get("details")
        .and_then(|details| details.get("entry_path"))
    {
        diagnostic["entry_path"] = path.clone();
    }
    Some(diagnostic)
}

/// Only an explicitly named `cause` is causal. Provider transcripts and sibling
/// response envelopes are evidence, not a nested failure chain.
pub(super) fn causal_typed_error(value: &Value) -> Option<Value> {
    let fields = value.as_object()?;
    let cause = if typed_error_at(value).is_some() {
        fields
            .get("error")
            .and_then(|error| error.get("details"))
            .and_then(|details| details.get("cause"))
    } else {
        fields.get("cause")
    };
    cause
        .and_then(|cause| causal_typed_error(cause).or_else(|| plain_cause_error(cause)))
        .or_else(|| typed_error_at(value))
}

/// A named `cause` recorded directly as `{code, message}` (for example a gate
/// setup failure's underlying hydration or Lab gate error) is the real failure;
/// without this the projection stops at the generic wrapper message.
pub(super) fn plain_cause_error(value: &Value) -> Option<Value> {
    let fields = value.as_object()?;
    let code = fields.get("code")?.as_str()?;
    let message = fields.get("message")?.as_str()?;
    let mut diagnostic = serde_json::json!({
        "code": code,
        "field": fields.get("details").and_then(|details| details.get("field")),
        "message": truncate_diagnostic_text(&homeboy_core::redaction::redact_string(message)),
    });
    if let Some(path) = fields
        .get("details")
        .and_then(|details| details.get("entry_path"))
    {
        diagnostic["entry_path"] = path.clone();
    }
    Some(diagnostic)
}

pub(super) fn bound_diagnostic_value(value: &mut Value, depth: usize) {
    if depth >= 4 {
        *value = Value::String("[omitted: diagnostic depth limit]".to_string());
        return;
    }
    match value {
        Value::String(text) => *text = truncate_diagnostic_text(text),
        Value::Array(items) => {
            items.truncate(8);
            for item in items {
                bound_diagnostic_value(item, depth + 1);
            }
        }
        Value::Object(entries) => {
            for item in entries.values_mut() {
                bound_diagnostic_value(item, depth + 1);
            }
        }
        _ => {}
    }
}

pub(super) fn truncate_diagnostic_text(text: &str) -> String {
    const LIMIT: usize = 2048;
    if text.len() <= LIMIT {
        text.to_string()
    } else {
        let prefix: String = text.chars().take(LIMIT).collect();
        format!("{prefix}...[truncated]")
    }
}

/// A preceding `key: ` can make the prose redactor consume a following
/// `token=value` as ordinary text. Redact each whitespace-delimited fragment a
/// second time so provider-produced diagnostic prose cannot bypass it.
pub(super) fn redact_diagnostic_text(text: &str) -> String {
    let redacted = homeboy_core::redaction::redact_string(text);
    redacted
        .split_inclusive(char::is_whitespace)
        .map(homeboy_core::redaction::redact_string)
        .collect()
}
