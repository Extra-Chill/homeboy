//! Cook identity, route, and policy projections.

use super::run::*;

pub(super) fn run_cook_explicit(
    request: agent_task_service::CookRequest,
    executor: SharedAgentTaskExecutor,
) -> homeboy::core::Result<
    agent_task_service::AgentTaskRunResult<agent_task_service::AgentTaskCookReport>,
> {
    let recipe_store = agent_task_service::CookRecipeStore::from_current_data_root()?;
    let lifecycle_store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    agent_task_service::CookService::run(
        request,
        agent_task_service::CookRuntime::production(executor, &recipe_store, &lifecycle_store),
    )
}

/// Operator-facing durable identity block for a Cook that has just become
/// addressable.
///
/// A Cook's run id is the only handle an interrupted caller has. Everything
/// after durable submission — gate toolchain preflight, transport preparation,
/// Lab materialization, provider execution — can outlive a client timeout, so
/// the handle and its follow-up commands are assembled here and printed the
/// first time the controller reports identity (#10419, #9163).
pub(crate) fn durable_cook_identity_lines(cook_id: Option<&str>, run_id: &str) -> Vec<String> {
    let cook_suffix = cook_id
        .filter(|cook_id| *cook_id != run_id)
        .map(|cook_id| format!(" (cook `{cook_id}`)"))
        .unwrap_or_default();
    vec![
        format!("cook: durable run id `{run_id}`{cook_suffix} — persisted before materialization."),
        format!("cook: status -> homeboy --placement local agent-task status {run_id}"),
        format!("cook: logs   -> homeboy --placement local runs logs {run_id}"),
        format!("cook: cancel -> homeboy --placement local agent-task cancel {run_id}"),
    ]
}

/// Print the durable identity block unconditionally.
///
/// Deliberately not TTY-gated. The clients that most need this — agent/Discord
/// bridges, CI, and anything invoking Homeboy as a tool call — are exactly the
/// non-TTY callers that a TTY-gated status line silently skips, which is how an
/// interrupted Lab Cook ended up with no reported task identity at all (#10419).
pub(crate) fn announce_durable_cook_identity(cook_id: Option<&str>, run_id: &str) {
    for line in durable_cook_identity_lines(cook_id, run_id) {
        eprintln!("{line}");
    }
}

/// Render the resolved execution placement this process already computed
/// before dispatch, the same way preview does (#14729).
///
/// `None` when no preflight decision was captured for this process (e.g. a
/// unit test that never ran the CLI's normal preflight path).
pub(super) fn resolved_execution_placement_line() -> Option<String> {
    let directive = homeboy::core::parsed_command_preflight::captured_result()?.placement;
    Some(format_resolved_execution_placement_line(&directive))
}

/// Pure formatter kept separate from its process-global-reading caller so it
/// can be tested deterministically against a directly constructed directive,
/// rather than through the shared `parsed_command_preflight` capture slot
/// (which is process-wide and not safe to mutate from parallel tests).
pub(super) fn format_resolved_execution_placement_line(
    directive: &homeboy::core::parsed_command_preflight::PlacementDirective,
) -> String {
    let requested = cli_placement_str(directive.requested);
    let selected = match directive.selected {
        homeboy_lab_contract::EffectiveExecutionPlacement::Local => "local",
        homeboy_lab_contract::EffectiveExecutionPlacement::Lab => "lab",
    };
    if selected == requested {
        format!("cook: placement: {selected}")
    } else {
        let reason = directive
            .fallback
            .reason
            .as_deref()
            .map(|reason| format!(" — {reason}"))
            .unwrap_or_default();
        format!("cook: placement: {selected}  (requested: {requested}{reason})")
    }
}

pub(super) fn cli_placement_str(placement: homeboy_lab_contract::Placement) -> &'static str {
    match placement {
        homeboy_lab_contract::Placement::Auto => "auto",
        homeboy_lab_contract::Placement::Local => "local",
        homeboy_lab_contract::Placement::Lab => "lab",
        homeboy_lab_contract::Placement::LabOrLocal => "lab-or-local",
    }
}

/// Report the resolved execution placement once identity is known.
///
/// Non-preview dispatch previously never reported this at all: an operator
/// backgrounding a Cook (the common case, since durable identity is reported
/// before materialization can even start) never saw preview's output and had
/// no other line telling them a requested Lab placement had silently degraded
/// to local execution (#14729).
pub(crate) fn announce_resolved_execution_placement() {
    if let Some(line) = resolved_execution_placement_line() {
        eprintln!("{line}");
    }
}

/// Operator-facing statement of the provider rotation a Cook will actually
/// perform.
///
/// A configured `agent_task.rotation` is only reachable if the execution budget
/// funds it, and nothing surfaced that mismatch — which is how a Cook could
/// carry a multi-provider rotation policy and still die terminally on the first
/// provider's recoverable failure while every other provider sat available
/// (#11082). State the effective behavior on every submission, including when
/// that behavior is "no rotation at all".
pub(crate) fn cook_rotation_disclosure(plan: &AgentTaskPlan) -> String {
    let budget = &plan.options.execution_budget;
    let executions = budget.max_provider_executions;
    let entries = plan
        .options
        .rotation
        .as_ref()
        .map_or(0, |rotation| rotation.entries.len());
    let entries = u32::try_from(entries).unwrap_or(u32::MAX);
    // Only rotations that are both configured AND affordable will ever fire.
    let funded = entries
        .min(budget.max_provider_rotations)
        .min(executions.saturating_sub(1));
    let route_disclosure = plan.metadata["cook_retry_policy"]["route"]["fallback"]["mode"]
        .as_str()
        .map(|mode| format!("cook: route: {mode}; "))
        .unwrap_or_default();
    if funded == 0 {
        format!("{route_disclosure}cook: rotation: disabled ({executions} provider execution(s))")
    } else {
        format!(
            "{route_disclosure}cook: rotation: {funded} fallback provider(s), up to {executions} provider execution(s)"
        )
    }
}

pub(super) fn cook_effective_plan_disclosure(plan: &AgentTaskPlan) -> String {
    let budget = &plan.options.execution_budget;
    let initial_route = plan.tasks.first().map(|task| {
        let backend = task.executor.backend.as_str();
        let model = task.executor.model.as_deref();
        model.map_or_else(|| backend.to_string(), |model| format!("{backend}/{model}"))
    });
    let mut routes = Vec::new();
    if let Some(rotation) = &plan.options.rotation {
        if initial_route.is_none() {
            routes.extend(rotation.entries.first().map(rotation_entry_label));
        }
        routes.extend(
            rotation
                .entries
                .iter()
                .skip(1)
                .take(
                    budget
                        .max_provider_rotations
                        .min(budget.max_provider_executions.saturating_sub(1))
                        as usize,
                )
                .map(rotation_entry_label),
        );
    }
    if let Some(initial_route) = initial_route {
        routes.insert(0, initial_route);
    }
    let rotation = if routes.len() > 1 {
        routes.join(" → ")
    } else {
        routes
            .first()
            .cloned()
            .unwrap_or_else(|| "configured provider".to_string())
    };
    let retry_classes = if budget.max_provider_rotations > 0 {
        "provider/transient/timeout/stalled/rate-limited/account-quota-billing-credentials"
    } else {
        "none"
    };
    format!(
        "cook: effective plan: up to {} executions; rotation order: {}; rotates on {}; {} gate retries (same provider; rotations do not fund gates)",
        budget.max_provider_executions,
        rotation,
        retry_classes,
        budget.max_same_provider_retries,
    )
}

pub(super) fn rotation_entry_label(
    entry: &homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationEntry,
) -> String {
    match (entry.backend.as_deref(), entry.model.as_deref()) {
        (Some(backend), Some(model)) => format!("{backend}/{model}"),
        (Some(backend), None) => backend.to_string(),
        (None, Some(model)) => model.to_string(),
        (None, None) => "configured provider".to_string(),
    }
}

/// What provider readiness actually bound for this Cook, projected for
/// `--preview` (#15854).
///
/// Admission selects the first configured route its readiness probe trusts and
/// records every skipped route and its reason on the bound task
/// (`provider_readiness_routing.skipped[]`). Preview reports that selection
/// instead of the configured primary. Lab placement binds on the runner with
/// the runner's own credentials, so there this states that the runner decides
/// instead of naming a model it cannot prove.
pub(super) fn cook_provider_readiness_projection(
    configured: &AgentTaskPlan,
    plan: &AgentTaskPlan,
    binding_runner_owned: bool,
) -> Value {
    let task = plan.tasks.first();
    let route_order = configured
        .tasks
        .first()
        .map(|task| match task.executor.model.as_deref() {
            Some(model) => format!("{}/{model}", task.executor.backend),
            None => task.executor.backend.clone(),
        })
        .into_iter()
        .chain(
            configured
                .options
                .rotation
                .iter()
                .flat_map(|rotation| rotation.entries.iter().map(rotation_entry_label)),
        )
        .collect::<Vec<_>>();
    // The runner binds a Lab-placed route; the controller has no selection.
    let selected_route = task.filter(|_| !binding_runner_owned).map(|task| {
        serde_json::json!({
            "backend": task.executor.backend,
            "selector": task.executor.selector,
            "model": task.executor.model,
        })
    });
    let skipped = task
        .and_then(|task| {
            task.metadata
                .pointer("/provider_readiness_routing/skipped")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|entry| match &entry {
            Value::Object(object) => {
                let mut pruned = serde_json::Map::new();
                for field in [
                    "backend",
                    "selector",
                    "model",
                    "state",
                    "reason",
                    "classification",
                    "retryable",
                    "remediation",
                ] {
                    if let Some(value) = object.get(field) {
                        pruned.insert(field.to_string(), value.clone());
                    }
                }
                Value::Object(pruned)
            }
            other => other.clone(),
        })
        .collect::<Vec<_>>();
    let mut projection = serde_json::json!({
        "schema": "homeboy/agent-task-provider-readiness-preview/v1",
        "binding_owner": if binding_runner_owned { "runner" } else { "controller" },
        "route_order": route_order,
        "skipped": skipped,
        "selected_route": selected_route,
    });
    if binding_runner_owned {
        projection["statement"] = serde_json::json!(
            "provider binding is decided on the lab runner during execution with the runner's own credentials; this preview does not claim any single model is ready"
        );
        if let Some(runner) = preview_lab_runner_id() {
            projection["runner_readiness_check"] = serde_json::json!(format!(
                "homeboy agent-task providers --runner {runner} --validate-readiness"
            ));
            projection["runner"] = serde_json::json!(runner);
        }
    }
    projection
}

/// The Lab runner this preview's placement selected, if any.
pub(super) fn preview_lab_runner_id() -> Option<String> {
    homeboy::core::parsed_command_preflight::captured_result()
        .and_then(|result| result.placement.runner)
        .map(|runner| runner.runner_id)
}

/// stderr disclosure for the readiness projection in `--preview` output.
/// Quiet when nothing needs correcting: an unpinned controller route whose
/// primary passed readiness binds entry 0 anyway.
pub(super) fn cook_provider_readiness_disclosure(
    plan: &AgentTaskPlan,
    binding_runner_owned: bool,
) -> Option<String> {
    let skipped = plan
        .tasks
        .first()
        .map(|task| {
            task.metadata
                .pointer("/provider_readiness_routing/skipped")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .unwrap_or_default();
    if binding_runner_owned {
        let runner = preview_lab_runner_id();
        return Some(match runner.as_deref() {
            Some(runner) => format!(
                "cook: provider binding is decided on lab runner `{runner}` during execution with the runner's credentials; the provider model below is the configured primary, not a readiness result. Check the runner: homeboy agent-task providers --runner {runner} --validate-readiness"
            ),
            None => "cook: provider binding is decided on the lab runner during execution with the runner's credentials; the provider model below is the configured primary, not a readiness result".to_string(),
        });
    }
    if skipped.is_empty() {
        return None;
    }
    let skipped_summary = skipped
        .iter()
        .map(|entry| {
            let route = entry
                .get("model")
                .and_then(Value::as_str)
                .or(entry.get("backend").and_then(Value::as_str))
                .unwrap_or("configured provider");
            format!(
                "{route} ({}: {})",
                entry
                    .get("classification")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                entry
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason reported"),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let selected = plan.tasks.first().and_then(|task| {
        let backend = task.executor.backend.as_str();
        let model = task.executor.model.as_deref();
        model.map(|model| format!("{backend}/{model}"))
    });
    Some(format!(
        "cook: provider readiness bound route: {}; skipped: {}; binding owner: controller",
        selected.unwrap_or_else(|| "configured provider".to_string()),
        skipped_summary,
    ))
}

/// Warn before a detached Cook becomes observable only through durable status.
pub(crate) fn detached_cook_route_less_warning(
    resolution: &homeboy::core::notification_route::NotificationRouteResolution,
) -> Option<String> {
    if resolution.classification != "route_less" {
        return None;
    }
    let resolver = resolution
        .resolver_transport
        .as_deref()
        .map(|transport| format!(" resolver transport `{transport}` was consulted"))
        .unwrap_or_else(|| " no resolver supplied a route".to_string());
    let missing = (!resolution.missing_context.is_empty())
        .then(|| {
            format!(
                "; resolver-reported caller context missing: {}",
                resolution.missing_context.join(", ")
            )
        })
        .unwrap_or_default();
    Some(format!(
        "cook: detached notification route is route-less;{resolver}{missing}. The Homeboy process can resolve only caller context available to its installed resolver; an active Discord thread is not inferred from extension readiness. To return terminal updates, invoke Cook with `--notification-transport <transport> --notification-route <route>` (or set `HOMEBOY_NOTIFICATION_TRANSPORT` and `HOMEBOY_NOTIFICATION_ROUTE` in the Homeboy process environment); inspect this Cook with `homeboy agent-task status <cook-id>`",
    ))
}

pub(super) fn cook_resolved_policy_disclosure(max_attempts: u32, plan: &AgentTaskPlan) -> String {
    let budget = &plan.options.execution_budget;
    format!(
        "cook: retry policy: 1 initial execution, {} same-provider remediation retry(ies), {} rotation(s), {} provider execution(s) maximum",
        budget.max_same_provider_retries,
        budget.max_provider_rotations,
        budget.max_provider_executions.max(max_attempts),
    )
}

/// Operator-facing statement of the wall-clock budget one provider execution
/// gets before it is cancelled at its deadline.
///
/// The budget was invisible: `--timeout-ms` documented only that it defaulted to
/// "Homeboy's provider timeout", so the only way to learn the number was to
/// spend a run discovering it — the deadline is named for the first time in a
/// `failure_classification: "timeout"` report, long after the sizing decision
/// that needed it (#12568). State it beside the retry budget, where the rest of
/// the execution budget is already disclosed.
pub(super) fn cook_provider_timeout_disclosure(plan: &AgentTaskPlan) -> String {
    let limits = plan.tasks.first().map(|task| &task.limits);
    // Mirror the resolution the provider runner performs, including the
    // task-level value `AgentTaskPlan::canonicalize` would otherwise copy down
    // from plan options later.
    let timeout_ms = effective_provider_timeout_ms(
        limits
            .and_then(|limits| limits.timeout_ms)
            .or(plan.options.timeout_ms),
        limits.and_then(|limits| limits.max_runtime_ms),
    );
    format!(
        "cook: provider timeout: {}s per provider execution (override with --timeout-ms)",
        timeout_ms / 1_000
    )
}

/// Serialize a completed run aggregate and, when the run did not fully succeed,
/// surface a prominent top-level `failure_reasons` summary so the operator sees
/// the root cause (recipe validation, PHP fatal, provider registration, missing
/// path) without hand-digging the nested outcome JSON (#3806). The full nested
/// payload is preserved unchanged; this only ADDS the surfaced summary.
pub(super) fn aggregate_value_with_failure_reasons(aggregate: &AgentTaskAggregate) -> Value {
    let aggregate = homeboy::agents::agent_task_artifacts::reviewer_facing_aggregate(aggregate);
    let mut value = serde_json::to_value(&aggregate).unwrap_or(Value::Null);
    let failure_reasons = super::status::failure_reasons_from_aggregate(&aggregate);
    if !failure_reasons.is_empty() {
        if let Value::Object(map) = &mut value {
            map.insert("failure_reasons".to_string(), Value::Array(failure_reasons));
        }
    }
    value
}
