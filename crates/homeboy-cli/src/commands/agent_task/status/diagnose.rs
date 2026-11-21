//! Diagnosis handler, actionable recovery guidance, and stable run/evidence refs.

use super::*;

// Optional observations must never hold recorded failure evidence hostage. A
// timed-out worker owns only read-only inputs and cannot mutate the run.
fn bounded_diagnostic_probe<T: Send + 'static>(
    timeout: std::time::Duration,
    probe: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(probe());
    });
    receiver.recv_timeout(timeout).ok()
}

const DIAGNOSTIC_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn stalled_observation_returns_within_its_budget() {
        let (release, stalled) = std::sync::mpsc::channel();
        let start = std::time::Instant::now();
        let result = bounded_diagnostic_probe(std::time::Duration::from_millis(20), move || {
            stalled.recv().unwrap();
        });
        assert!(result.is_none());
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        release.send(()).unwrap();
    }

    #[test]
    fn completed_observation_keeps_its_result() {
        assert_eq!(
            bounded_diagnostic_probe(DIAGNOSTIC_PROBE_TIMEOUT, || 42),
            Some(42)
        );
    }
}

pub(in crate::commands::agent_task) fn diagnose(args: DiagnoseArgs) -> CmdResult<Value> {
    if crate::commands::agent_task::cook_batch::is_workflow_instance(&args.run_id) {
        return crate::commands::agent_task::cook_batch::batch_rollup_from_daemon(
            &args.run_id,
            "diagnose",
        );
    }
    let target = resolve_cook_reader_target(&args.run_id, false)?;
    let run_id = &target.run_id;
    // Diagnosis reads recorded evidence, never live runner state.
    let durable_read = agent_task_lifecycle::durable_local_read(run_id)?;
    let record = durable_read.record;
    let aggregate = durable_read.aggregate;
    let runner_diagnostic_probe = agent_task_lifecycle::runner_diagnostic_probe_recorded(&record);
    let admission = agent_task_service_direct::queued_admission(&record);
    let queued_runner_ownership = admission
        .as_ref()
        .filter(|wait| !wait.live_driver && wait.liveness.is_reconcilable())
        .and_then(|wait| {
            queued_runner_ownership_diagnostic(&record, wait.liveness, &runner_diagnostic_probe)
        });
    let mut hydrated_evidence = Vec::new();
    let mut total_hydrated_evidence = 0;
    // The current promotion lifecycle denial is the active blocker. Older
    // controller failures remain in the diagnostic chain as causal history.
    let current_lifecycle_diagnostic = current_lifecycle_diagnostic(&record);
    let mut nested_reasons = current_lifecycle_diagnostic
        .clone()
        .into_iter()
        .chain(lab_staging_root_cause(&record))
        .chain(queued_runner_ownership.clone())
        .chain(persisted_cook_failure_diagnostic(&record))
        .collect::<Vec<_>>();
    let runner_cancellation = runner_cancellation_diagnostic(&record);
    let causal_phase = queued_runner_ownership
        .as_ref()
        .or(runner_cancellation.as_ref())
        .and_then(|diagnostic| diagnostic.data["causal_phase"].as_str())
        .map(str::to_string);
    nested_reasons.extend(runner_cancellation.clone());
    nested_reasons.extend(
        aggregate
            .as_ref()
            .map(aggregate_failure_diagnostics)
            .unwrap_or_default(),
    );
    let mut diagnostic_truncations = Vec::new();
    let mut diagnostic_budget = DiagnosticCollectionBudget::default();

    if let Some(aggregate) = aggregate.as_ref() {
        for outcome in &aggregate.outcomes {
            for evidence in &outcome.evidence_refs {
                total_hydrated_evidence += 1;
                if let Some(truncation) = collect_hydrated_evidence_diagnostics(
                    &outcome.task_id,
                    evidence,
                    &mut nested_reasons,
                    &mut diagnostic_budget,
                ) {
                    diagnostic_truncations.push(truncation);
                }
                if args.full || hydrated_evidence.len() < OutputBudget::COLLECTION.max_items {
                    if let Some(summary) =
                        agent_task_service::hydrate_evidence_summary(&outcome.task_id, evidence)
                    {
                        hydrated_evidence.push(summary);
                    }
                }
            }
        }
    }

    // A controller/runner build skew can only survive as text behind a generic
    // continuation exit. Name it as the root cause (#18206).
    let build_skew =
        provenance_build_skew_root_cause(&record, &nested_reasons, &runner_diagnostic_probe);
    if let Some(diagnostic) = build_skew.clone() {
        nested_reasons.insert(0, diagnostic);
    }
    let gate_failure = agent_task_service_direct::recorded_promotion_report(&record.metadata)
        .ok()
        .flatten()
        .and_then(|report| agent_task_service_direct::gate_failure_projection(&report));
    nested_reasons.extend(gate_failure.as_ref().map(gate_failure_diagnostic));
    let mut ranked_reasons = ranked_diagnostics(nested_reasons);
    let recovery_attempt = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
        .ok()
        .and_then(|store| agent_task_service_direct::recovery_step_projection(&store, &record));
    // A failed regate may never have reached a gate. Its step job's failure
    // is newer evidence than the candidate's original gate output (#18657).
    if let Some(message) = recovery_attempt
        .as_ref()
        .filter(|step| step["state"] == "failed")
        .and_then(|step| step["failure"]["message"].as_str())
    {
        ranked_reasons.insert(
            0,
            CollectedDiagnostic {
                task_id: record.run_id.clone(),
                class: "agent_task.recovery_step_failed".into(),
                message: message.to_string(),
                source: "recovery_step".into(),
                data: recovery_attempt.clone().unwrap(),
            },
        );
    }
    // The canonical Cook verdict, read exactly as `agent-task status --exact`
    // reads it (#15885). The raw lifecycle record can still say `succeeded`
    // for a Cook whose candidate was never verified or published; diagnose
    // must report the state every other surface reports.
    let verdict = homeboy::agents::orchestration::run_exact_from_current_environment(run_id)
        .ok()
        .map(|run| crate::commands::cook_verdict::cook_verdict(&run));
    let record_state = serde_json::to_value(record.state()).unwrap_or(Value::Null);
    let state = verdict
        .as_ref()
        .map(|verdict| json!(verdict.state))
        .unwrap_or_else(|| record_state.clone());
    let state_label = state.as_str().unwrap_or("unknown").to_string();
    // `diagnose` is the printed recovery verb for a failed run (#14679): a
    // human-readable summary must exist whenever it runs, and it must say
    // whether a root cause was actually found rather than leaving `succeeded`
    // with an empty summary ambiguous between "nothing wrong" and "nothing
    // reported". A Cook stopped short of publication without a recorded
    // failure is explained by its verdict, not reported as "no root cause".
    let verdict_reason = verdict.as_ref().and_then(|verdict| verdict.reason.clone());
    let (diagnosis_outcome, summary) = match (ranked_reasons.first(), verdict_reason) {
        (Some(diagnostic), _) => (
            "root_cause_identified",
            format!("{}: {}", diagnostic.class, diagnostic.message),
        ),
        (None, Some(reason)) => ("cook_state_explained", format!("{state_label}: {reason}")),
        (None, None) => (
            "no_root_cause_found",
            format!(
                "no root cause diagnostic was found for run {} (state: {state_label})",
                record.run_id
            ),
        ),
    };
    let root_cause = ranked_reasons
        .first()
        .cloned()
        .map(|item| collected_diagnostic_value_with_details(item, args.full));
    let diagnostic_chain = ranked_reasons
        .into_iter()
        .take(FAILURE_REASON_LIMIT)
        .map(|item| collected_diagnostic_value_with_details(item, args.full))
        .collect::<Vec<_>>();

    let missing_artifacts = aggregate
        .as_ref()
        .map(missing_artifact_summaries)
        .unwrap_or_default();
    let mut causal_chain = aggregate
        .as_ref()
        .map(causal_chain_from_aggregate)
        .unwrap_or_default();
    if causal_chain.is_empty() {
        if let Some(diagnostic) = queued_runner_ownership
            .as_ref()
            .or(runner_cancellation.as_ref())
        {
            causal_chain.push(json!({
                "task_id": diagnostic.task_id,
                "surface": "runner",
                "phase": diagnostic.data["causal_phase"],
                "status": record.state(),
                "failure_classification": "runner_cancellation",
            }));
        }
    }
    let replay_record = record.clone();
    let retry_probe = bounded_diagnostic_probe(DIAGNOSTIC_PROBE_TIMEOUT, move || {
        retry_replay_action(&replay_record)
    });
    let replay_probe_timed_out = retry_probe.is_none();
    let retry = retry_probe.unwrap_or_else(|| {
        RetryReplayAction::unavailable(
            json!({ "runner_id": record.runner_id() }),
            "probe_timed_out: replay workspace admission",
        )
    });
    let lab_snapshot_failure = lab_snapshot_recovery_projection(&record, &retry);
    let next_commands = if gate_failure.is_some() {
        Vec::new() // The shared classification table below owns gate actions.
    } else {
        diagnose_next_commands(
            &record,
            retry.action.as_ref(),
            retry.continuation.as_ref(),
            current_lifecycle_diagnostic.is_some(),
            queued_runner_ownership.is_some(),
        )
    };
    let next_commands = if let Some(skew) = build_skew.as_ref() {
        provenance_build_skew_next_commands(&record, skew)
    } else if lab_staging_root_cause(&record).is_some() {
        vec![format!(
            "homeboy agent-task retry {}",
            quote_arg(record.metadata["cook_id"].as_str().unwrap_or(run_id))
        )]
    } else {
        next_commands
    };

    let mut value = json!({
        "schema": "homeboy/agent-task-diagnose/v1",
        "failure": null,
        "run_id": record.run_id.clone(),
        "state": state,
        "record_state": record_state,
        "summary": summary,
        "diagnosis_outcome": diagnosis_outcome,
        "cook_verdict": verdict,
        "root_cause": root_cause,
        "diagnostic_chain": diagnostic_chain,
        "causal_chain": causal_chain,
        "missing_artifacts": missing_artifacts.clone(),
        "hydrated_evidence": hydrated_evidence,
        "hydrated_evidence_total": total_hydrated_evidence,
        "diagnostic_collection": diagnostic_collection_projection(&diagnostic_truncations),
        "causal_phase": causal_phase,
        "runner_diagnostic_probe": runner_diagnostic_probe,
        "probe_timed_out": (replay_probe_timed_out.then_some("replay_workspace_admission").into_iter().collect::<Vec<_>>()),
        "continuation_admission": record.metadata.get("cook_continuation_admission"),
        "retry_replay": retry.projection(),
        "lab_snapshot_failure": lab_snapshot_failure,
        "next_commands": next_commands,
    });
    attach_gate_cleanup_warnings(&mut value, &record);
    if let Ok(Some(promotion)) =
        agent_task_service_direct::recorded_promotion_report(&record.metadata)
    {
        let outputs: Vec<_> = promotion
            .get("deterministic_gates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(homeboy::agents::agent_task_service::gate_failure_excerpt_from_value)
            .collect();
        if !outputs.is_empty() {
            value["gate_failure_outputs"] = json!(outputs);
        }
    }
    if let Some(projection) = lab_transport_failure_projection(&record, run_id) {
        value["lab_transport_failure"] = projection;
    }
    attach_durable_read_availability(&mut value, &durable_read.unavailable_sources);
    attach_cook_completion(&mut value, &record);
    attach_lab_admission_wait(&mut value, admission.as_ref());
    if let Some(selection) = target.selection {
        value["candidate_selection"] = json!(selection);
    }
    if !args.full {
        attach_collection_budget(
            &mut value,
            "hydrated_evidence",
            &format!("homeboy agent-task diagnose {} --full", quote_arg(run_id)),
            &format!(
                "homeboy agent-task diagnose {} --full --output <path>",
                quote_arg(run_id)
            ),
        );
    }
    // Gate classifications own their recovery guidance. Other diagnoses use
    // the Cook recovery planner, including its durable no-progress marker.
    let recovery_plan = gate_failure
        .is_none()
        .then(|| {
            agent_task_service::load_recipe_for_attempt(run_id)
                .ok()
                .flatten()
                .and_then(|_| agent_task_service_direct::plan_cook_recovery_recorded(run_id).ok())
        })
        .flatten();
    let recovery_prefix =
        agent_task_service_direct::cook_recovery_command_prefix_for_record(&record);
    if gate_failure.is_some() {
        project_gate_failure(&mut value, gate_failure, run_id);
    } else if let Some(plan) = recovery_plan {
        let action = cook_diagnose_next_action(&plan.step, &recovery_prefix, &plan.cook_id);
        value["next_commands"] = json!([action.command]);
        value["recovery_step"] = json!(plan.step);
        value["next_action_basis"] = json!(DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        attach_actionable_metadata(
            &mut value,
            CommandActionableMetadata {
                run: Some(diagnose_run_ref(&record, record.runner_id())),
                next_actions: vec![action],
                ..Default::default()
            },
        );
    } else {
        attach_diagnose_actionable(
            &mut value,
            &record,
            aggregate.as_ref(),
            &missing_artifacts,
            record.runner_id(),
            retry.action.as_ref(),
            retry.continuation.as_ref(),
            runner_cancellation.is_some(),
            queued_runner_ownership.is_some(),
            current_lifecycle_diagnostic.is_some(),
        );
    }
    if let Some(step) = recovery_attempt {
        if step["state"] == "failed" {
            value["summary"] = json!(format!(
                "Recovery step {} failed: {}",
                step["step"].as_str().unwrap_or("recovery"),
                step["failure"]["message"]
                    .as_str()
                    .unwrap_or("unknown failure")
            ));
            value["blocker"] = step["failure"].clone();
            value["failure"] = step["failure"].clone();
        }
        // Keep the planner's `recovery_step` vocabulary separate from the
        // actual admitted execution and its recorded outcome.
        value["recovery_attempt"] = step;
    }
    preserve_controller_owner_placement_with_prefix(&mut value, run_id, &recovery_prefix);
    // Placement is an observation: an unreadable control-plane projection
    // reports `pending` rather than failing the diagnosis.
    value["placement"] = crate::commands::run_placement::placement(
        &agent_task_service::control_plane_run(&record.run_id)
            .ok()
            .and_then(|run| serde_json::to_value(run).ok())
            .unwrap_or(Value::Null),
    );
    if let Some(cause) = persisted_cook_failure_diagnostic(&record)
        .filter(|cause| cause.class == "runner.capacity_busy")
    {
        if let Some(runner) = cause.data["runner_id"].as_str() {
            value["placement"] = json!({"kind": "lab", "runner_id": runner});
        }
    }
    Ok((value, 0))
}

/// Status and diagnose use the same retained-report projection and action table.
pub(super) fn attach_gate_failure(value: &mut Value, record: &AgentTaskRunRecord) {
    let failure = agent_task_service_direct::recorded_promotion_report(&record.metadata)
        .ok()
        .flatten()
        .and_then(|report| agent_task_service_direct::gate_failure_projection(&report));
    project_gate_failure(value, failure, &record.run_id);
}

pub(super) fn project_gate_failure(value: &mut Value, failure: Option<Value>, run: &str) {
    value["failure"] = failure.clone().unwrap_or(Value::Null);
    let Some(failure) = failure else { return };
    let (command, description) = agent_task_service_direct::gate_failure_next_action(
        failure["reason"].as_str().unwrap_or("gate_failed"),
        &quote_arg(run),
    );
    let action = CommandNextAction::new(description.clone(), command.clone());
    value["next_action"] = json!({"command": command, "description": description});
    if value["blocker"].is_object() {
        value["blocker"]["reason"] = json!(command);
    }
    value["next_commands"] = json!([command]);
    value["next_action_basis"] = json!(DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
    attach_actionable_metadata(
        value,
        CommandActionableMetadata {
            next_actions: vec![action],
            ..Default::default()
        },
    );
    if value.get("summary").is_some() {
        value["summary"] = json!(format!(
            "gate {}: {} (exit {}); artifact: {}",
            failure["gate"].as_str().unwrap_or("unknown"),
            failure["termination"].as_str().unwrap_or("unavailable"),
            failure["exit_code"],
            failure["artifact"].as_str().unwrap_or("unavailable")
        ));
    }
}

/// Diagnose's Cook next action is exactly the shared recovery renderer's.
pub(super) fn cook_diagnose_next_action(
    step: &CookRecoveryStep,
    prefix: &str,
    id: &str,
) -> CommandNextAction {
    let next = step.next_action(prefix, id);
    let action = CommandNextAction::new(next.label, next.command);
    if next.show {
        return action.with_kind(CommandNextActionKind::Show);
    }
    action
}

/// These fields are deliberately separate from secondary compact tables. They
/// survive the byte-budget trim and give both JSON and human renderers the same
/// terminal causal answer and exactly one immediately executable action.
pub(super) fn attach_cook_completion(value: &mut Value, record: &AgentTaskRunRecord) {
    let Ok(Some(recipe)) = agent_task_service::load_recipe_for_attempt(&record.run_id) else {
        return;
    };
    let selected_candidate = record
        .metadata
        .get("cook_id")
        .and_then(Value::as_str)
        .and_then(|cook_id| agent_task_service_direct::select_cook_candidate(cook_id).ok())
        .and_then(|selection| serde_json::to_value(selection).ok());
    let finalization_requested = recipe.finalization["no_finalize"] != true;
    if let Some(completion) = agent_task_service_direct::cook_completion(
        selected_candidate.as_ref(),
        finalization_requested,
        // `cook_completion` resolves this receipt from the selected candidate
        // whenever selection exists. This fallback only serves pre-index reports.
        agent_task_service_direct::recorded_finalization_receipt(&record.metadata),
        Some(&record.run_id),
    ) {
        let mut completion = serde_json::to_value(completion).expect("completion serializes");
        // This legacy top-level field remains for existing consumers, but its
        // candidate fact belongs to the Cook rather than this queried attempt.
        completion["scope"] = json!("cook");
        completion["context"] = json!("selected_cook_candidate");
        value["cook_completion"] = completion;
    }
    // The completion projection answers whether a PR exists; it does not carry
    // the PR's identity. Project that beside it so `status` can answer "is there
    // a PR, and where" without a second command (#12571).
    if let Some(pr_url) = CookDelivery::from_run_metadata(&record.metadata)
        .as_ref()
        .and_then(CookDelivery::pr_url)
    {
        value["pr_url"] = Value::String(pr_url.to_string());
    }
}

/// Versioned semantic boundary between one queried lifecycle attempt and the
/// Cook-wide candidate that can survive later empty or cancelled retries.
/// Legacy top-level fields intentionally retain their historical projection.
/// Basis marker for `next_actions`: the diagnosis mapped a typed failure
/// classification (or a concrete missing-artifact set) to specific commands.
pub(super) const DIAGNOSE_ACTION_BASIS_DIAGNOSIS: &str = "diagnosis";
/// Basis marker for `next_actions`: nothing in the diagnosis was specific
/// enough to act on, so the generic recovery set is emitted as an explicit
/// fallback rather than as the only behavior.
pub(super) const DIAGNOSE_ACTION_BASIS_FALLBACK: &str = "generic_fallback";
/// Basis marker for a recoverable canonical candidate that takes precedence
/// over a later failed provider attempt.
pub(super) const DIAGNOSE_ACTION_BASIS_CANDIDATE: &str = "canonical_candidate";

/// One failed task and how its executor classified the failure. This is the
/// typed input to the classification→action table: it is read from durable
/// outcome state, never from provider prose, so an emitted action can always be
/// substantiated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiagnosedFailure {
    pub(super) task_id: String,
    pub(super) classification: AgentTaskFailureClassification,
    pub(super) phase: Option<String>,
    pub(super) provider_budget_consumed: bool,
}

/// Collect the distinct failure classifications a run actually recorded, first
/// implicated task wins. Successful and no-op outcomes are never a failure
/// signal even if a stale classification survived on them.
pub(super) fn diagnosed_failures(aggregate: &AgentTaskAggregate) -> Vec<DiagnosedFailure> {
    let mut failures: Vec<DiagnosedFailure> = Vec::new();
    for outcome in &aggregate.outcomes {
        if matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
        ) {
            continue;
        }
        let Some(classification) = outcome.failure_classification else {
            continue;
        };
        if failures
            .iter()
            .any(|failure| failure.classification == classification)
        {
            continue;
        }
        failures.push(DiagnosedFailure {
            task_id: outcome.task_id.clone(),
            classification,
            phase: outcome
                .metadata
                .get("phase")
                .and_then(Value::as_str)
                .map(str::to_string),
            provider_budget_consumed: outcome.metadata["provider_budget_consumed"] == true,
        });
    }
    failures
}

pub(super) fn attach_diagnose_actionable(
    value: &mut Value,
    record: &AgentTaskRunRecord,
    aggregate: Option<&AgentTaskAggregate>,
    missing_artifacts: &[Value],
    runner_id: Option<&str>,
    retry_action: Option<&CommandNextAction>,
    continuation_action: Option<&CommandNextAction>,
    runner_cancellation: bool,
    queued_runner_ownership: bool,
    current_lifecycle_denial: bool,
) {
    let run_id = record.run_id.as_str();
    if let Some(cause) = persisted_cook_failure_diagnostic(record)
        .filter(|cause| cause.class == "runner.capacity_busy")
    {
        let runner = cause.data["runner_id"].as_str();
        let command = runner
            .map(|runner| format!("homeboy runner status {} --full", quote_arg(runner)))
            .unwrap_or_else(|| "homeboy runner list".to_string());
        value["next_action_basis"] = json!(DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        attach_actionable_metadata(
            value,
            CommandActionableMetadata {
                run: Some(diagnose_run_ref(record, runner)),
                next_actions: vec![CommandNextAction::new(
                    format!(
                        "wait until {} then inspect Lab capacity",
                        cause.data["retry_at"]
                            .as_str()
                            .unwrap_or("the next admission check")
                    ),
                    command,
                )
                .with_kind(CommandNextActionKind::Show)],
                ..Default::default()
            },
        );
        return;
    }
    if let Some(action) = lab_transport_repair_action(record) {
        if let Value::Object(map) = value {
            map.insert(
                "next_action_basis".to_string(),
                Value::String(DIAGNOSE_ACTION_BASIS_DIAGNOSIS.to_string()),
            );
        }
        attach_actionable_metadata(
            value,
            CommandActionableMetadata {
                run: Some(diagnose_run_ref(record, runner_id)),
                refs: CommandResultRefs {
                    agent_tasks: vec![agent_task_ref(run_id)],
                    ..Default::default()
                },
                next_actions: vec![action],
                ..Default::default()
            },
        );
        return;
    }
    let recoverable = homeboy::agents::agent_task_lifecycle::CookRunState::of(record).recoverable;
    let failures = aggregate
        .map(|aggregate| diagnosed_failures(aggregate))
        .unwrap_or_default();
    let (next_actions, basis) = if current_lifecycle_denial {
        (
            current_lifecycle_next_actions(record),
            DIAGNOSE_ACTION_BASIS_DIAGNOSIS,
        )
    } else if let Some(continuation) = continuation_action {
        (vec![continuation.clone()], DIAGNOSE_ACTION_BASIS_CANDIDATE)
    } else if recoverable {
        (
            vec![CommandNextAction::new(
                "review the canonical candidate",
                format!("homeboy agent-task review {}", quote_arg(run_id)),
            )
            .with_kind(CommandNextActionKind::Show)],
            DIAGNOSE_ACTION_BASIS_CANDIDATE,
        )
    } else {
        diagnose_next_actions(
            run_id,
            &failures,
            missing_artifacts,
            runner_id,
            retry_action,
            runner_cancellation,
            queued_runner_ownership,
        )
    };
    if let Value::Object(map) = value {
        map.insert(
            "next_action_basis".to_string(),
            Value::String(basis.to_string()),
        );
    }
    let metadata = CommandActionableMetadata {
        run: Some(diagnose_run_ref(record, runner_id)),
        refs: CommandResultRefs {
            agent_tasks: vec![agent_task_ref(run_id)],
            ..Default::default()
        },
        next_actions,
        artifacts: aggregate.map(diagnose_artifact_refs).unwrap_or_default(),
        evidence: aggregate.map(diagnose_evidence_refs).unwrap_or_default(),
    };
    attach_actionable_metadata(value, metadata);
}

/// Derive the recovery commands for this exact failure. Returns the generic set
/// ONLY when neither a classification nor a missing-artifact set produced a
/// specific step, and labels which of the two happened.
pub(super) fn diagnose_next_actions(
    run_id: &str,
    failures: &[DiagnosedFailure],
    missing_artifacts: &[Value],
    runner_id: Option<&str>,
    retry_action: Option<&CommandNextAction>,
    runner_cancellation: bool,
    queued_runner_ownership: bool,
) -> (Vec<CommandNextAction>, &'static str) {
    let mut actions: Vec<CommandNextAction> = Vec::new();
    for failure in failures {
        for action in classification_next_actions(run_id, failure, runner_id, retry_action) {
            push_unique_next_action(&mut actions, action);
        }
    }
    for action in missing_artifact_next_actions(run_id, missing_artifacts) {
        push_unique_next_action(&mut actions, action);
    }
    if runner_cancellation {
        for action in runner_cancellation_next_actions(run_id, runner_id, retry_action) {
            push_unique_next_action(&mut actions, action);
        }
    }
    if queued_runner_ownership {
        push_unique_next_action(
            &mut actions,
            CommandNextAction::new(
                "inspect the queued runner-owned execution",
                format!(
                    "homeboy --runner {} agent-task diagnose {} --full",
                    quote_arg(runner_id.unwrap_or("<runner>")),
                    quote_arg(run_id)
                ),
            )
            .with_kind(CommandNextActionKind::Show),
        );
    }
    if actions.is_empty() {
        return (
            generic_diagnose_next_actions(run_id, retry_action),
            DIAGNOSE_ACTION_BASIS_FALLBACK,
        );
    }
    (actions, DIAGNOSE_ACTION_BASIS_DIAGNOSIS)
}

/// A pre-provider runner cancellation is not an unclassified provider failure.
/// Inspect the owning runner first; retry is included only when the persisted
/// replay admission already proved it safe.
pub(super) fn runner_cancellation_next_actions(
    run_id: &str,
    runner_id: Option<&str>,
    retry_action: Option<&CommandNextAction>,
) -> Vec<CommandNextAction> {
    let run = quote_arg(run_id);
    let mut actions = vec![CommandNextAction::new(
        "show the durable runner cancellation evidence",
        format!("homeboy agent-task status {run}"),
    )
    .with_kind(CommandNextActionKind::Show)];
    actions.extend(lost_runner_actions(runner_id));
    actions.extend(retry_action.cloned());
    actions
}

pub(super) fn push_unique_next_action(
    actions: &mut Vec<CommandNextAction>,
    action: CommandNextAction,
) {
    if actions
        .iter()
        .any(|existing| existing.command == action.command)
    {
        return;
    }
    actions.push(action);
}

/// The classification→action table. Each arm answers "what do I run next for
/// THIS kind of failure?" with commands that exist in the CLI surface and are
/// scoped to the run and task the diagnosis implicates.
pub(super) fn classification_next_actions(
    run_id: &str,
    failure: &DiagnosedFailure,
    runner_id: Option<&str>,
    retry_action: Option<&CommandNextAction>,
) -> Vec<CommandNextAction> {
    let run = quote_arg(run_id);
    let task = quote_arg(&failure.task_id);
    let failure_evidence = CommandNextAction::new(
        format!("show failure evidence for {}", failure.task_id),
        format!("homeboy runs evidence {run} --task {task} --failure-only"),
    )
    .with_kind(CommandNextActionKind::Show);
    let retry = retry_action.cloned();
    let review = CommandNextAction::new(
        "review the candidate this attempt left behind",
        format!("homeboy agent-task review {run}"),
    )
    .with_kind(CommandNextActionKind::Show);
    let list_providers = CommandNextAction::new(
        "list registered providers",
        "homeboy agent-task providers".to_string(),
    )
    .with_kind(CommandNextActionKind::Show);

    if failure.phase.as_deref() == Some("controller_admission") {
        let mut actions = vec![failure_evidence];
        actions.push(
            CommandNextAction::new(
                "show the controller admission record and runtime pin",
                format!("homeboy agent-task status {run}"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
        return actions;
    }

    if failure.phase.as_deref() == Some("interrupted_owner") {
        let budget_label = if failure.provider_budget_consumed {
            "provider budget was consumed and in-flight work may be duplicated"
        } else {
            "provider budget was not consumed"
        };
        let mut actions = vec![
            failure_evidence,
            CommandNextAction::new(
                format!(
                    "show the interrupted-owner stop reason and candidate harvest evidence; {budget_label}"
                ),
                format!("homeboy agent-task status {run}"),
            )
            .with_kind(CommandNextActionKind::Show),
        ];
        if let Some(retry) = retry {
            actions.push(
                CommandNextAction::new(
                    format!("retry this Cook; {budget_label}"),
                    retry.command.clone(),
                )
                .with_kind(CommandNextActionKind::Repair),
            );
        }
        return actions;
    }

    match failure.classification {
        // The provider itself errored or was not resolvable: prove which
        // provider was asked for, and whether it is registered and ready.
        AgentTaskFailureClassification::Provider => {
            let mut actions = vec![failure_evidence, list_providers];
            actions.extend(provider_readiness_actions(runner_id));
            actions.extend(retry);
            actions
        }
        // Documented as safe to retry with bounded backoff: lead with the retry.
        AgentTaskFailureClassification::Transient => {
            retry.into_iter().chain([failure_evidence]).collect()
        }
        // A wall-clock timeout can still have left a complete candidate patch,
        // so review comes before spending another attempt.
        AgentTaskFailureClassification::Timeout
        | AgentTaskFailureClassification::ProviderTimeoutNoProgress => {
            let mut actions = vec![failure_evidence, review];
            actions.extend(retry);
            actions
        }
        // A silent hang: the durable record is likely still `running` and the
        // owning runner is the thing to interrogate.
        AgentTaskFailureClassification::Stalled => {
            let mut actions = vec![CommandNextAction::new(
                "reconcile the stalled run against authoritative state",
                format!("homeboy agent-task reconcile {run} --dry-run"),
            )
            .with_kind(CommandNextActionKind::Repair)];
            actions.extend(lost_runner_actions(runner_id));
            actions.push(failure_evidence);
            actions.extend(retry);
            actions
        }
        // Throttled: the evidence carries the retry-after hint, and another
        // registered provider may be able to take the work now.
        AgentTaskFailureClassification::RateLimited => {
            let mut actions = vec![
                failure_evidence,
                CommandNextAction::new(
                    "list registered providers to rotate to",
                    "homeboy agent-task providers".to_string(),
                )
                .with_kind(CommandNextActionKind::Show),
            ];
            actions.extend(retry);
            actions
        }
        // This account cannot satisfy another request until its quota, billing,
        // or credentials are repaired. Show alternative providers rather than
        // offering a same-provider retry.
        AgentTaskFailureClassification::ProviderAccountBlocked
        | AgentTaskFailureClassification::ProviderQuotaExhausted
        | AgentTaskFailureClassification::ProviderBillingBlocked
        | AgentTaskFailureClassification::ProviderCredentialsExhausted
        | AgentTaskFailureClassification::ProviderRequestRejected => vec![
            failure_evidence,
            CommandNextAction::new(
                "list registered providers to rotate to",
                "homeboy agent-task providers".to_string(),
            )
            .with_kind(CommandNextActionKind::Show),
        ],
        // Policy refused this request. Retrying an identical request is denied
        // identically, so no retry action is emitted.
        AgentTaskFailureClassification::PolicyDenied => vec![
            failure_evidence,
            CommandNextAction::new(
                "show the full run record including the policy that denied it",
                format!("homeboy agent-task status {run}"),
            )
            .with_kind(CommandNextActionKind::Show),
        ],
        // A required capability/tool was not resolvable: the readiness chain is
        // the repair, not another attempt.
        AgentTaskFailureClassification::CapabilityMissing => {
            let mut actions = vec![
                failure_evidence,
                CommandNextAction::new(
                    "show provider declarations",
                    "homeboy agent-task providers --full".to_string(),
                )
                .with_kind(CommandNextActionKind::Show),
            ];
            actions.extend(provider_readiness_actions(runner_id));
            actions
        }
        // Capacity admission records its scoped cleanup plan in the failure
        // evidence. Do not offer a retry until an operator has inspected it.
        AgentTaskFailureClassification::Capacity => vec![failure_evidence],
        // The request the provider received was malformed. The failure
        // evidence shows the rejected input; a retry would resend it.
        AgentTaskFailureClassification::InvalidInput => vec![failure_evidence],
        // The work ran and failed (gate/verify failure, harvest failure,
        // required typed artifacts missing): show what the failing step
        // recorded and what it produced before deciding to retry.
        AgentTaskFailureClassification::ExecutionFailed => {
            let mut actions = vec![
                failure_evidence,
                review,
                CommandNextAction::new(
                    "list the artifacts the run produced",
                    format!("homeboy runs artifacts {run} --full"),
                )
                .with_kind(CommandNextActionKind::Artifacts),
            ];
            actions.extend(retry);
            actions
        }
        // Deliberately unmapped: an unclassified failure has no substantiable
        // specific step, so the caller gets the explicit generic fallback.
        AgentTaskFailureClassification::Unknown => Vec::new(),
    }
}

/// Provider/runner readiness chain for the runner that owns the run. Emitted
/// only when a runner id is known, because `agent-task doctor` requires one.
pub(super) fn provider_readiness_actions(runner_id: Option<&str>) -> Vec<CommandNextAction> {
    let Some(runner_id) = runner_id else {
        return Vec::new();
    };
    let runner = quote_arg(runner_id);
    vec![
        CommandNextAction::new(
            format!("check provider and runner readiness on {runner_id}"),
            format!("homeboy agent-task doctor --runner {runner}"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            format!("repair provider and runner readiness on {runner_id}"),
            format!("homeboy agent-task doctor --runner {runner} --repair"),
        )
        .with_kind(CommandNextActionKind::Repair),
    ]
}

/// Runner session inspection and repair for a run whose runner went quiet.
pub(super) fn lost_runner_actions(runner_id: Option<&str>) -> Vec<CommandNextAction> {
    let Some(runner_id) = runner_id else {
        return Vec::new();
    };
    let runner = quote_arg(runner_id);
    vec![
        CommandNextAction::new(
            format!("show runner session state for {runner_id}"),
            format!("homeboy runner status {runner}"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            format!("diagnose and repair runner {runner_id}"),
            format!("homeboy runner doctor {runner} --scope lab-offload --repair"),
        )
        .with_kind(CommandNextActionKind::Repair),
    ]
}

/// Actions for the specific artifacts a task declared and did not produce.
pub(super) fn missing_artifact_next_actions(
    run_id: &str,
    missing_artifacts: &[Value],
) -> Vec<CommandNextAction> {
    if missing_artifacts.is_empty() {
        return Vec::new();
    }
    let run = quote_arg(run_id);
    let mut actions = vec![CommandNextAction::new(
        "list the artifacts the run actually produced",
        format!("homeboy runs artifacts {run} --full"),
    )
    .with_kind(CommandNextActionKind::Artifacts)];
    for entry in missing_artifacts.iter().take(COMPACT_REF_LIMIT) {
        let Some(task_id) = entry.get("task_id").and_then(Value::as_str) else {
            continue;
        };
        let names = entry
            .get("missing")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if names.is_empty() {
            continue;
        }
        let task = quote_arg(task_id);
        actions.push(
            CommandNextAction::new(
                format!("show failure evidence for {task_id} (missing {names})"),
                format!("homeboy runs evidence {run} --task {task} --failure-only"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
    }
    actions
}

/// The pre-existing static set, retained as the explicit fallback for a failure
/// the diagnosis could not classify.
pub(super) fn generic_diagnose_next_actions(
    run_id: &str,
    retry_action: Option<&CommandNextAction>,
) -> Vec<CommandNextAction> {
    let run = quote_arg(run_id);
    let mut actions = vec![
        CommandNextAction::new(
            "show the full run record",
            format!("homeboy agent-task status {run}"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            "list the artifacts the run produced",
            format!("homeboy runs artifacts {run}"),
        )
        .with_kind(CommandNextActionKind::Artifacts),
        CommandNextAction::new("review run", format!("homeboy agent-task review {run}"))
            .with_kind(CommandNextActionKind::Show),
    ];
    actions.extend(retry_action.cloned());
    actions
}

/// Read-only handoff commands remain legal for every persisted promotion, gate,
/// or finalization denial. A retry of an earlier provider attempt is not current
/// recovery guidance after a later lifecycle denial.
pub(super) fn current_lifecycle_next_actions(
    record: &AgentTaskRunRecord,
) -> Vec<CommandNextAction> {
    let run_id = &record.run_id;
    let run = quote_arg(run_id);
    let mut actions = vec![
        CommandNextAction::new(
            "show the current promotion, gate, or finalization denial",
            format!("homeboy agent-task status {run}"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            "review the promoted candidate and its gate proof",
            format!("homeboy agent-task review {run}"),
        )
        .with_kind(CommandNextActionKind::Show),
    ];
    let Some(cook_id) = record.metadata.get("cook_id").and_then(Value::as_str) else {
        return actions;
    };
    let Some(status) = current_lifecycle_status(record) else {
        return actions;
    };
    let Some(recovery) =
        agent_task_service_direct::cook_failure_context(cook_id, Some(run_id), status.as_str())
    else {
        return actions;
    };
    for action in recovery.next_actions {
        if action.action == "status" || action.action == "diagnose" {
            continue;
        }
        push_unique_next_action(
            &mut actions,
            CommandNextAction::new(
                format!("{} through the Cook recovery handoff", action.action),
                action.command,
            )
            .with_kind(CommandNextActionKind::Repair),
        );
    }
    actions
}

pub(super) fn diagnose_run_ref(
    record: &AgentTaskRunRecord,
    runner_id: Option<&str>,
) -> CommandRunRef {
    let run = quote_arg(&record.run_id);
    CommandRunRef {
        id: record.run_id.clone(),
        kind: "agent_task".to_string(),
        source: "homeboy-agent-task-lifecycle".to_string(),
        location: Some(
            runner_id
                .map(|runner_id| format!("runner:{runner_id}"))
                .unwrap_or_else(|| "local".to_string()),
        ),
        started_at: Some(record.submitted_at.clone()),
        updated_at: record.updated_at.clone(),
        finished_at: None,
        status_command: format!("homeboy agent-task status {run}"),
        watch_command: format!("homeboy agent-task status {run} --watch"),
    }
}

pub(super) fn diagnose_artifact_refs(aggregate: &AgentTaskAggregate) -> Vec<CommandArtifactRef> {
    aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| outcome.artifacts.iter())
        .take(COMPACT_REF_LIMIT)
        .map(|artifact| CommandArtifactRef {
            id: artifact.id.clone(),
            kind: artifact.kind.clone(),
            uri: artifact
                .url
                .clone()
                .or_else(|| artifact.path.clone())
                .unwrap_or_default(),
            semantic_key: artifact.semantic_key.clone(),
        })
        .collect()
}

pub(super) fn diagnose_evidence_refs(aggregate: &AgentTaskAggregate) -> Vec<CommandArtifactRef> {
    aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| {
            outcome
                .evidence_refs
                .iter()
                .map(move |evidence| (outcome.task_id.as_str(), evidence))
        })
        .take(COMPACT_REF_LIMIT)
        .map(|(task_id, evidence)| CommandArtifactRef {
            id: format!("{task_id}:{}", evidence.kind),
            kind: evidence.kind.clone(),
            uri: evidence.uri.clone(),
            semantic_key: None,
        })
        .collect()
}

pub(super) struct RetryReplayAction {
    pub(super) owner: Value,
    pub(super) readiness: &'static str,
    pub(super) reason: Option<String>,
    pub(super) admission: Value,
    pub(super) action: Option<CommandNextAction>,
    pub(super) continuation: Option<CommandNextAction>,
}

impl RetryReplayAction {
    pub(super) fn unavailable(owner: Value, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            owner,
            readiness: "unavailable",
            admission: json!({ "admitted": false, "reason": reason }),
            reason: Some(reason),
            action: None,
            continuation: None,
        }
    }

    pub(super) fn projection(&self) -> Value {
        json!({
            "owner": self.owner,
            "readiness": self.readiness,
            "reason": self.reason,
            "admission": self.admission,
            "action": self.action.as_ref().and_then(|action| action.action.clone()),
        })
    }
}

/// Render retry only from the durable owner and replay contract. The command and
/// typed action share this one admission decision, so a human hint cannot claim
/// a retry exists when route materialization will reject it.
pub(super) fn retry_replay_action(record: &AgentTaskRunRecord) -> RetryReplayAction {
    // Candidate readers may be observing an older substantive attempt. Retry
    // eligibility and the advertised action must describe the mutation target.
    if let Ok(latest) = agent_task_service_direct::resolve_retry_run_id(&record.run_id) {
        if latest != record.run_id {
            if let Ok(store) = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment() {
                if let Ok(latest_record) = store.read_record_bounded(&latest) {
                    return retry_replay_action(&latest_record);
                }
            }
        }
    }
    let runner_id = record.runner_id().filter(|id| !id.trim().is_empty());
    let local_owner = match runner_id {
        Some(runner_id) => json!({ "placement": "runner", "runner_id": runner_id }),
        None => json!({ "placement": "local" }),
    };
    let plan = match agent_task_lifecycle::load_plan(&record.run_id) {
        Ok(plan) => plan,
        Err(error) => {
            return RetryReplayAction::unavailable(
                local_owner,
                format!("persisted replay plan is unavailable: {}", error.message),
            );
        }
    };
    let has_generic_lab_replay = plan.metadata.get("generic_lab_command_replay").is_some();
    // Cook-owned retries stay on the controller lifecycle. Lab routing makes
    // the same Cook-first decision before it considers generic replay.
    if record.metadata["cook_id"].is_string() && has_generic_lab_replay {
        return RetryReplayAction::unavailable(
            local_owner,
            "Cook-owned generic Lab replay must continue through the controller Cook lifecycle",
        );
    }
    let generic_lab_replay = has_generic_lab_replay;
    let admission = if generic_lab_replay {
        // Status advertises the exact persisted generic replay, rather than a
        // Cook-derived retry plan that may no longer carry its workspace proof.
        crate::commands::infra::route::validate_generic_lab_command_replay_workspace(&plan)
            .and_then(|()| {
                agent_task_service_direct::retry_admission_with_preflight(
                    &record.run_id,
                    |_| Ok(()),
                )
            })
    } else {
        agent_task_service_direct::retry_admission(&record.run_id)
    };
    if let Err(error) = admission {
        return RetryReplayAction::unavailable(
            local_owner,
            format!("retry admission is unavailable: {}", error.message),
        );
    }
    if !plan_has_retry_materialization_identity(&plan) {
        return RetryReplayAction::unavailable(
            local_owner,
            "persisted replay plan has no materialization identity",
        );
    }
    let (owner, action) = if generic_lab_replay {
        let owner = json!({ "placement": "lab" });
        let action = lab_replay_retry_action(&record.run_id, owner.clone());
        (owner, action)
    } else {
        let owner = local_owner;
        let action = owner_bound_retry_action(&record.run_id, runner_id, owner.clone());
        (owner, action)
    };
    RetryReplayAction {
        owner,
        readiness: "ready",
        reason: None,
        admission: json!({ "admitted": true }),
        action: Some(action),
        continuation: None,
    }
}

pub(super) fn lab_replay_retry_action(run_id: &str, owner: Value) -> CommandNextAction {
    let args = vec![
        "--placement".to_string(),
        "lab".to_string(),
        "agent-task".to_string(),
        "retry".to_string(),
        run_id.to_string(),
        "--run".to_string(),
    ];
    CommandNextAction::from_action(
        ExecutableAction::new(
            "agent-task.retry.lab-replay.v1",
            "retry the Lab replay from its persisted workspace",
            "homeboy",
            args,
            ActionSafety::Mutating,
        )
        .with_evidence(json!({
            "schema": "homeboy/agent-task-retry-replay/v1",
            "owner": owner,
            "replay_ready": true,
            "materialization_identity": true,
        })),
    )
}

pub(super) fn owner_bound_retry_action(
    run_id: &str,
    runner_id: Option<&str>,
    owner: Value,
) -> CommandNextAction {
    let mut args = Vec::new();
    if let Some(runner_id) = runner_id {
        args.extend(["--runner".to_string(), runner_id.to_string()]);
    } else {
        args.extend(["--placement".to_string(), "local".to_string()]);
    }
    args.extend([
        "agent-task".to_string(),
        "retry".to_string(),
        run_id.to_string(),
        "--run".to_string(),
    ]);
    let action = ExecutableAction::new(
        "agent-task.retry.owner-bound.v1",
        "retry the run from its persisted plan",
        "homeboy",
        args,
        ActionSafety::Mutating,
    )
    .with_evidence(json!({
        "schema": "homeboy/agent-task-retry-replay/v1",
        "owner": owner,
        "replay_ready": true,
        "materialization_identity": true,
    }));
    CommandNextAction::from_action(action)
}

pub(super) fn plan_has_retry_materialization_identity(plan: &AgentTaskPlan) -> bool {
    plan.tasks.iter().any(|task| {
        task.workspace
            .root
            .as_deref()
            .or_else(|| {
                task.executor
                    .config
                    .get("workspace_root")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                task.metadata
                    .get("workspace")
                    .and_then(|workspace| workspace.get("root"))
                    .and_then(Value::as_str)
            })
            .is_some_and(|root| !root.trim().is_empty())
    }) || plan
        .metadata
        .get("generic_lab_command_replay")
        .is_some_and(|replay| {
            replay.get("schema").and_then(Value::as_str)
                == Some("homeboy/generic-lab-command-replay/v1")
                && replay
                    .get("normalized_args")
                    .and_then(Value::as_array)
                    .is_some_and(|args| !args.is_empty())
                && replay
                    .pointer("/materialization/canonical_root")
                    .and_then(Value::as_str)
                    .is_some_and(|root| !root.trim().is_empty())
                && replay
                    .pointer("/materialization/content_identity")
                    .and_then(Value::as_str)
                    .is_some_and(|identity| !identity.trim().is_empty())
        })
}

pub(super) fn diagnose_next_commands(
    record: &AgentTaskRunRecord,
    retry_action: Option<&CommandNextAction>,
    continuation_action: Option<&CommandNextAction>,
    current_lifecycle_denial: bool,
    queued_runner_ownership: bool,
) -> Vec<String> {
    let owner = record
        .runner_id()
        .map(|runner| format!("--runner {runner}"))
        .unwrap_or_else(|| "--placement local".to_string());
    let run_id = &record.run_id;
    if current_lifecycle_denial {
        return current_lifecycle_next_actions(record)
            .into_iter()
            .map(|action| action.command)
            .collect();
    }
    if queued_runner_ownership {
        return vec![format!(
            "homeboy {owner} agent-task diagnose {run_id} --full"
        )];
    }
    let mut commands = vec![
        format!("homeboy {owner} agent-task diagnose {run_id} --full"),
        format!("homeboy {owner} runs artifacts {run_id}"),
        format!("homeboy {owner} agent-task review {run_id}"),
    ];
    commands.extend(
        continuation_action
            .or(retry_action)
            .map(|action| action.command.clone()),
    );
    commands
}
