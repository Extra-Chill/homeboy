//! Read-side handlers: status, logs, artifacts, list/active/latest, and cancel.
//!
//! `status` returns a compact, recovery-first summary by default (#4396):
//! run id, state, totals, a per-task source table (#4392), deduped patch/changed
//! references, and a prominent risk-flag section (#4398). The full verbose
//! payload is available behind `--full`.

use homeboy_engine_primitives::content_hash;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use homeboy::agents::agent_task_service as agent_task_service_direct;
use homeboy::agents::agent_tasks::lifecycle::{self as agent_task_lifecycle, AgentTaskRunRecord};
use homeboy::agents::agent_tasks::promotion::{AgentTaskPromotionReport, AgentTaskPromotionStatus};
use homeboy::agents::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy::agents::agent_tasks::service as agent_task_service;
use homeboy::agents::agent_tasks::{
    AgentTaskEvidenceRef, AgentTaskFailureClassification, AgentTaskOutcomeStatus,
};
use homeboy::core::engine::shell::quote_arg;
use homeboy::core::output::{budget_json_values, OutputBudget};
use homeboy::runner::runners::{self as runner, RunnerKind};

use super::super::CmdResult;
use super::args::{
    CancelArgs, DiagnoseArgs, EvidenceArgs, LogsArgs, ReplayProviderBoundaryArgs,
    RuntimeRecoverArgs, RuntimeValidateArgs, StatusArgs,
};
use super::candidate::{canonical_candidate_projection, classify_candidates, CandidateState};
use crate::commands::utils::response::{
    CommandActionableMetadata, CommandAgentTaskRef, CommandArtifactRef, CommandNextAction,
    CommandNextActionKind, CommandResultRefs, CommandRunRef, ACTIONABLE_METADATA_KEY,
};

/// Cap the number of detail refs rendered in the compact summary so a noisy
/// aggregate cannot flood recovery output. Overflow is reported as an
/// `omitted` count rather than dropped silently.
const COMPACT_REF_LIMIT: usize = 12;
const COMPACT_TASK_LIMIT: usize = 12;
const COMPACT_TEXT_LIMIT: usize = 512;
const FULL_TEXT_LIMIT: usize = 4 * 1024;

pub(super) fn status(args: StatusArgs) -> CmdResult<Value> {
    if args.bridge {
        let bridge_status = agent_task_service::run_status(&args.run_id, args.since_cursor)?;
        return Ok((
            serde_json::to_value(bridge_status).unwrap_or(Value::Null),
            0,
        ));
    }

    let options = agent_task_lifecycle::AgentTaskStatusOptions {
        runner_probe: if args.no_runner_probe {
            agent_task_lifecycle::AgentTaskRunnerProbe::Never
        } else {
            agent_task_lifecycle::AgentTaskRunnerProbe::WhenRunnerBacked
        },
    };
    let outcome = match agent_task_service_direct::status_with_options(&args.run_id, options) {
        Ok(outcome) => outcome,
        Err(error) if is_missing_agent_task_run_metadata_error(&error) => {
            if let Some(remediation) =
                agent_task_service::offloaded_status_remediation(&args.run_id)?
            {
                return Ok((remediation, 1));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    let runner_probe = runner_probe_projection(&outcome.runner_probe);
    let record = outcome.record;
    // A future durable budget is incompatible, not an absent optional preview.
    if let Err(error) = agent_task_lifecycle::load_plan(&args.run_id) {
        if error
            .message
            .contains("unsupported agent-task execution budget version")
        {
            return Err(error);
        }
    }
    let mut value = serde_json::to_value(&record).unwrap_or(Value::Null);
    enrich_with_diagnostic_summary(&mut value, &args.run_id)?;
    attach_transport_proxy_recovery_guidance(&mut value, &args.run_id);
    if args.full {
        let aggregate = completed_run_aggregate(&args.run_id).and_then(Result::ok);
        attach_full_status_candidate(&mut value, aggregate.as_ref(), &args.run_id);
        bound_full_reader_payload(&mut value);
        attach_runner_probe(&mut value, &runner_probe);
        attach_agent_task_status_actionable(&mut value, &args.run_id);
        return Ok((value, 0));
    }
    let summary = compact_status_summary(&value, &args.run_id);
    let mut summary = summary;
    attach_runner_probe(&mut summary, &runner_probe);
    attach_agent_task_status_actionable(&mut summary, &args.run_id);
    Ok((summary, 0))
}

fn attach_full_status_candidate(
    value: &mut Value,
    aggregate: Option<&AgentTaskAggregate>,
    run_id: &str,
) {
    if let Some(aggregate) = aggregate {
        if let Value::Object(fields) = value {
            fields.insert(
                "aggregate".to_string(),
                serde_json::to_value(aggregate).unwrap_or(Value::Null),
            );
        }
    }
    let canonical = classify_candidates(value);
    let liveness = liveness_summary(value, run_id, canonical.state());
    if let Value::Object(fields) = value {
        fields.insert(
            "canonical_candidate".to_string(),
            canonical_candidate_projection(canonical),
        );
        fields.insert("liveness".to_string(), liveness);
    }
}

/// Project the read-side runner-probe decision into the status payload.
///
/// This is the operator's answer to "is this a complete status, or a local one
/// because the runner could not be consulted?" (#10418). It is always present
/// so the distinction never has to be inferred from a missing field.
pub(crate) fn runner_probe_projection(
    plan: &agent_task_lifecycle::AgentTaskRunnerProbePlan,
) -> Value {
    let mut projection = serde_json::to_value(plan).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut projection {
        map.insert(
            "note".to_string(),
            Value::String(runner_probe_note(plan.skipped_reason)),
        );
    }
    projection
}

fn runner_probe_note(skipped_reason: Option<&str>) -> String {
    let Some(reason) = skipped_reason else {
        return "runner reconciliation was performed for this read".to_string();
    };
    if reason == agent_task_lifecycle::RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL {
        "run is controller-local; answered from durable controller state without contacting any runner".to_string()
    } else if reason == agent_task_lifecycle::RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT {
        "--no-runner-probe was requested; this is a partial, controller-local answer and runner-side job state may be stale".to_string()
    } else if reason == agent_task_lifecycle::RUNNER_PROBE_SKIPPED_NOT_RUNNING {
        "run is not running; there is no live runner job to reconcile".to_string()
    } else {
        format!("runner reconciliation was skipped: {reason}")
    }
}

fn attach_runner_probe(value: &mut Value, runner_probe: &Value) {
    if let Value::Object(map) = value {
        map.insert("runner_probe".to_string(), runner_probe.clone());
    }
}

#[cfg(test)]
mod runner_probe_tests {
    use super::*;

    #[test]
    fn a_controller_local_answer_says_it_never_contacted_a_runner() {
        let projection = runner_probe_projection(&agent_task_lifecycle::AgentTaskRunnerProbePlan {
            performed: false,
            skipped_reason: Some(agent_task_lifecycle::RUNNER_PROBE_SKIPPED_CONTROLLER_LOCAL),
            controller_local: true,
        });

        assert_eq!(projection["performed"], false);
        assert_eq!(projection["controller_local"], true);
        assert_eq!(projection["skipped_reason"], "controller_local_record");
        assert!(projection["note"]
            .as_str()
            .expect("note")
            .contains("without contacting any runner"));
    }

    #[test]
    fn an_opted_out_answer_is_labelled_partial() {
        let projection = runner_probe_projection(&agent_task_lifecycle::AgentTaskRunnerProbePlan {
            performed: false,
            skipped_reason: Some(agent_task_lifecycle::RUNNER_PROBE_SKIPPED_CALLER_OPTED_OUT),
            controller_local: false,
        });

        assert_eq!(projection["skipped_reason"], "caller_opted_out");
        assert!(projection["note"]
            .as_str()
            .expect("note")
            .contains("partial"));
    }

    #[test]
    fn a_completed_probe_reports_no_skip_reason() {
        let projection = runner_probe_projection(&agent_task_lifecycle::AgentTaskRunnerProbePlan {
            performed: true,
            skipped_reason: None,
            controller_local: false,
        });

        assert_eq!(projection["performed"], true);
        assert!(projection.get("skipped_reason").is_none());
        assert!(projection["note"]
            .as_str()
            .expect("note")
            .contains("was performed"));
    }
}

/// Full recovery output remains a local reader: retain a stable digest for a
/// large value instead of repeating multi-attempt patches in every projection.
fn bound_full_reader_payload(value: &mut Value) {
    match value {
        Value::String(text) if text.len() > FULL_TEXT_LIMIT => {
            let digest = content_hash::sha256_hex(text.as_bytes());
            *text = format!("[omitted {} bytes; sha256={digest}]", text.len());
        }
        Value::Array(items) => {
            for item in items {
                bound_full_reader_payload(item);
            }
        }
        Value::Object(fields) => {
            for item in fields.values_mut() {
                bound_full_reader_payload(item);
            }
        }
        _ => {}
    }
}

fn is_missing_agent_task_run_metadata_error(error: &homeboy::core::Error) -> bool {
    error.code == homeboy::core::ErrorCode::InternalJsonError
        && error.message.contains("is missing agent_task_run metadata")
}

pub(super) fn list_runs(
    filter: agent_task_service::AgentTaskDiscoveryFilter,
    options: agent_task_service_direct::AgentTaskDiscoveryOptions,
) -> CmdResult<Value> {
    let report = agent_task_service_direct::discover_runs_with_options(filter, options)?;
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    attach_collection_budget(
        &mut value,
        "runs",
        "homeboy agent-task list --full",
        "homeboy agent-task list --full --output <path>",
    );
    attach_agent_task_discovery_actionable(&mut value);
    Ok((value, 0))
}

/// `agent-task active`: list queued + running runs, but SEPARATE them into
/// active / stale / suspect / unreconciled buckets so a stale or orphaned
/// `running` record (especially a Lab/offloaded run whose runner process died)
/// is never silently treated as genuinely-active (#5682).
///
/// The base discovery report (with per-run liveness, source, last-update age,
/// and a per-run safe reconcile command) is preserved under `report`, and a
/// `buckets` view groups run ids by classification for an at-a-glance triage.
pub(super) fn list_active(
    options: agent_task_service_direct::AgentTaskDiscoveryOptions,
) -> CmdResult<Value> {
    let report = agent_task_service_direct::discover_runs_with_options(
        agent_task_service::AgentTaskDiscoveryFilter::Active,
        options,
    )?;
    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);

    let buckets = active_liveness_buckets(&report);
    if let Value::Object(map) = &mut value {
        map.insert("buckets".to_string(), buckets);
        map.insert(
            "reconcile_hint".to_string(),
            json!("run the per-run `commands.reconcile` preview, then repeat it with `--apply` after reviewing authoritative provider state"),
        );
    }
    attach_collection_budget(
        &mut value,
        "runs",
        "homeboy agent-task active --full",
        "homeboy agent-task active --full --output <path>",
    );
    attach_agent_task_discovery_actionable(&mut value);
    Ok((value, 0))
}

/// `agent-task active --reconcile`: preview stale/suspect/unreconciled records
/// across the fleet. `--apply` is required to mutate the previewed set (#10001).
pub(super) fn reconcile_active(dry_run: bool) -> CmdResult<Value> {
    let report = agent_task_service_direct::reconcile_stale_active_runs(dry_run)?;
    let exit = if report.failed > 0 { 1 } else { 0 };
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), exit))
}

/// `agent-task reconcile <run-id>` always addresses exactly one durable run.
/// It previews by default; `--apply` is the explicit operator authorization.
pub(super) fn reconcile_run(run_id: &str, dry_run: bool) -> CmdResult<Value> {
    let report = agent_task_service_direct::reconcile_run(run_id, dry_run)?;
    let exit = if report.failed > 0 { 1 } else { 0 };
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), exit))
}

pub(super) fn reconcile_records(dry_run: bool) -> CmdResult<Value> {
    let report = agent_task_lifecycle::reconcile_record_health(dry_run)?;
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

/// Group active-run ids by liveness classification for a scannable triage view.
fn active_liveness_buckets(report: &agent_task_service::AgentTaskDiscoveryReport) -> Value {
    use agent_task_service_direct::AgentTaskLiveness;

    let mut active = Vec::new();
    let mut stale = Vec::new();
    let mut suspect = Vec::new();
    let mut unreconciled = Vec::new();

    for run in &report.runs {
        let bucket = match run.liveness {
            Some(AgentTaskLiveness::Active) | None => &mut active,
            Some(AgentTaskLiveness::Stale) => &mut stale,
            Some(AgentTaskLiveness::Suspect) => &mut suspect,
            Some(AgentTaskLiveness::Unreconciled) => &mut unreconciled,
        };
        bucket.push(json!({
            "run_id": run.run_id,
            "state": run.state,
            "source": run.source,
            "last_update": run.last_update,
            "last_update_age_minutes": run.last_update_age_minutes,
            "stale_reason": run.stale_reason,
        }));
    }

    json!({
        "active": active,
        "stale": stale,
        "suspect": suspect,
        "unreconciled": unreconciled,
    })
}

fn attach_agent_task_status_actionable(value: &mut Value, run_id: &str) {
    let mut metadata = CommandActionableMetadata {
        refs: CommandResultRefs {
            agent_tasks: vec![agent_task_ref(run_id)],
            ..Default::default()
        },
        next_actions: vec![
            CommandNextAction::new(
                "show status",
                format!("homeboy agent-task status {run_id} --full"),
            )
            .with_kind(CommandNextActionKind::Show),
            CommandNextAction::new("show logs", format!("homeboy agent-task logs {run_id}"))
                .with_kind(CommandNextActionKind::Show),
            CommandNextAction::new(
                "list artifacts",
                format!("homeboy agent-task artifacts {run_id}"),
            )
            .with_kind(CommandNextActionKind::Artifacts),
        ],
        ..Default::default()
    };

    if classify_candidates(value).state().is_available() {
        let review_command = format!("homeboy agent-task review {run_id}");
        metadata.next_actions.push(
            CommandNextAction::new("review run", review_command)
                .with_kind(CommandNextActionKind::Show),
        );
    }

    if let Some(command) = transport_proxy_recovery_command(value) {
        metadata.next_actions.push(
            CommandNextAction::new("recover runner transport", command)
                .with_kind(CommandNextActionKind::Repair),
        );
    }

    if value
        .pointer("/metadata/stale_running")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        metadata.next_actions.push(
            CommandNextAction::new(
                "reconcile stale run",
                format!(
                    "homeboy agent-task reconcile {} --dry-run",
                    quote_arg(run_id)
                ),
            )
            .with_kind(CommandNextActionKind::Repair),
        );
    }

    attach_actionable_metadata(value, metadata);
}

fn transport_proxy_recovery_command(value: &Value) -> Option<String> {
    value
        .get("transport_recovery")
        .and_then(|recovery| recovery.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn attach_transport_proxy_recovery_guidance(value: &mut Value, run_id: &str) {
    let Some(kind) = value.pointer("/metadata/kind").and_then(Value::as_str) else {
        return;
    };
    if !kind.ends_with("_controller_proxy") {
        return;
    }
    let runner_id = value.pointer("/metadata/runner_id").and_then(Value::as_str);
    let job_id = value
        .pointer("/metadata/runner_job_id")
        .or_else(|| value.pointer("/metadata/runner_execution_record/job_id"))
        .and_then(Value::as_str);
    let guidance = transport_proxy_recovery_guidance(run_id, runner_id, job_id);
    if let Value::Object(map) = value {
        map.insert("transport_recovery".to_string(), guidance);
    }
}

fn transport_proxy_recovery_guidance(
    run_id: &str,
    runner_id: Option<&str>,
    job_id: Option<&str>,
) -> Value {
    let Some(runner_id) = runner_id.filter(|runner_id| !runner_id.trim().is_empty()) else {
        return json!({
            "condition": "controller_proxy_without_runner",
            "runner_id": Value::Null,
            "runner_job_id": job_id,
            "command": format!("homeboy agent-task run {}", quote_arg(run_id)),
        });
    };
    let local_runner = runner::load(runner_id).is_ok_and(|runner| runner.kind == RunnerKind::Local);
    let runner_command_id = quote_arg(runner_id);
    let (condition, command) = if local_runner {
        (
            "local_runner_resume_required",
            format!("homeboy agent-task run {}", quote_arg(run_id)),
        )
    } else if job_id.is_some() {
        (
            "runner_status_refresh_required",
            format!("homeboy runner status {runner_command_id}"),
        )
    } else {
        (
            "controller_proxy_without_runner_job",
            format!("homeboy runner status {runner_command_id}"),
        )
    };
    json!({ "condition": condition, "runner_id": runner_id, "runner_job_id": job_id, "command": command })
}

#[cfg(test)]
mod transport_proxy_tests {
    use super::*;

    #[test]
    fn transport_proxy_guidance_requires_an_explicit_authoritative_status_refresh() {
        let recorded_job = transport_proxy_recovery_guidance(
            "agent-task-proxy-42",
            Some("runner-transport-42"),
            Some("job-42"),
        );
        assert_eq!(recorded_job["condition"], "runner_status_refresh_required");
        assert_eq!(
            recorded_job["command"],
            "homeboy runner status runner-transport-42"
        );
    }

    #[test]
    fn transport_proxy_guidance_reports_missing_runner_and_quotes_commands() {
        let missing_runner = transport_proxy_recovery_guidance("run with spaces", None, None);
        assert_eq!(
            missing_runner["condition"],
            "controller_proxy_without_runner"
        );
        assert_eq!(
            missing_runner["command"],
            "homeboy agent-task run 'run with spaces'"
        );

        let unknown_runner = transport_proxy_recovery_guidance(
            "agent-task-proxy-42",
            Some("runner with spaces"),
            None,
        );
        assert_eq!(
            unknown_runner["condition"],
            "controller_proxy_without_runner_job"
        );
        assert_eq!(
            unknown_runner["command"],
            "homeboy runner status 'runner with spaces'"
        );
    }

    #[test]
    fn transport_proxy_guidance_resumes_local_runner_without_a_status_probe() {
        let guidance = transport_proxy_recovery_guidance("agent-task-local", Some("local"), None);

        assert_eq!(guidance["condition"], "local_runner_resume_required");
        assert_eq!(
            guidance["command"],
            "homeboy agent-task run agent-task-local"
        );
    }
}

fn attach_agent_task_discovery_actionable(value: &mut Value) {
    let runs = value
        .get("runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut metadata = CommandActionableMetadata::default();

    for run in runs.iter().take(20) {
        let Some(run_id) = run.get("run_id").and_then(Value::as_str) else {
            continue;
        };
        metadata.refs.agent_tasks.push(agent_task_ref(run_id));
        if let Some(commands) = run.get("commands") {
            push_command_action(
                &mut metadata.next_actions,
                "show status",
                commands.get("status"),
                CommandNextActionKind::Show,
            );
            push_command_action(
                &mut metadata.next_actions,
                "show logs",
                commands.get("logs"),
                CommandNextActionKind::Show,
            );
            push_command_action(
                &mut metadata.next_actions,
                "list artifacts",
                commands.get("artifacts"),
                CommandNextActionKind::Artifacts,
            );
            if run
                .get("liveness")
                .and_then(Value::as_str)
                .is_some_and(|liveness| liveness != "active")
            {
                push_command_action(
                    &mut metadata.next_actions,
                    "reconcile stale run",
                    commands.get("reconcile"),
                    CommandNextActionKind::Repair,
                );
            }
        }
    }

    attach_actionable_metadata(value, metadata);
}

fn agent_task_ref(run_id: &str) -> CommandAgentTaskRef {
    CommandAgentTaskRef {
        id: run_id.to_string(),
        source: "homeboy-agent-task-lifecycle".to_string(),
        status_command: format!("homeboy agent-task status {run_id} --full"),
        logs_command: format!("homeboy agent-task logs {run_id}"),
        review_command: Some(format!("homeboy agent-task review {run_id}")),
    }
}

fn push_command_action(
    actions: &mut Vec<CommandNextAction>,
    label: &str,
    command: Option<&Value>,
    kind: CommandNextActionKind,
) {
    let Some(command) = command.and_then(Value::as_str) else {
        return;
    };
    actions.push(CommandNextAction::new(label, command).with_kind(kind));
}

fn attach_actionable_metadata(value: &mut Value, metadata: CommandActionableMetadata) {
    if metadata.is_empty() {
        return;
    }
    let Value::Object(map) = value else {
        return;
    };
    if let Ok(metadata) = serde_json::to_value(metadata) {
        map.insert(ACTIONABLE_METADATA_KEY.to_string(), metadata);
    }
}

pub(super) fn logs(args: LogsArgs) -> CmdResult<Value> {
    let log = if args.raw {
        agent_task_service_direct::logs_with_raw(&args.run_id)?
    } else {
        agent_task_service_direct::logs(&args.run_id)?
    };
    let mut value = serde_json::to_value(log).unwrap_or(Value::Null);
    enrich_with_diagnostic_summary(&mut value, &args.run_id)?;
    Ok((value, 0))
}

pub(super) fn artifacts(args: StatusArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    let mut value = serde_json::to_value(artifacts).unwrap_or(Value::Null);
    if !args.full {
        attach_collection_budget(
            &mut value,
            "artifacts",
            &format!(
                "homeboy agent-task artifacts {} --full",
                quote_arg(&args.run_id)
            ),
            &format!(
                "homeboy agent-task artifacts {} --full --output <path>",
                quote_arg(&args.run_id)
            ),
        );
    }
    Ok((value, 0))
}

pub(super) fn evidence(args: EvidenceArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    let aggregate = completed_run_aggregate(&args.run_id).transpose()?;
    let failed_tasks = failed_task_statuses(aggregate.as_ref());
    let plan = agent_task_lifecycle::load_plan(&args.run_id).ok();

    let mut hydrated = Vec::new();
    let mut total = 0;
    for (evidence_ref, task_id) in
        evidence_refs_with_tasks(&artifacts.evidence_refs, aggregate.as_ref())
    {
        if args
            .kind
            .as_deref()
            .is_some_and(|kind| evidence_ref.kind != kind)
        {
            continue;
        }
        if args
            .task
            .as_deref()
            .is_some_and(|task| task_id.as_deref() != Some(task))
        {
            continue;
        }
        if args.failure_only
            && !task_id
                .as_deref()
                .is_some_and(|task| failed_tasks.contains_key(task))
        {
            continue;
        }

        total += 1;
        // Count filtered refs without hydrating their payload once the shared
        // collection budget is full.
        if !args.full && hydrated.len() >= OutputBudget::COLLECTION.max_items {
            continue;
        }
        hydrated.push(agent_task_service::hydrate_evidence_ref(
            &args.run_id,
            &evidence_ref,
            task_id.as_deref(),
            plan.as_ref(),
            aggregate.as_ref(),
        ));
    }

    let mut value = serde_json::to_value(AgentTaskEvidenceReport {
        schema: "homeboy/agent-task-evidence/v1",
        run_id: args.run_id.clone(),
        filters: AgentTaskEvidenceFilters {
            kind: args.kind,
            task: args.task,
            failure_only: args.failure_only,
        },
        count: total,
        evidence_total: total,
        evidence: hydrated,
    })
    .unwrap_or(Value::Null);
    if !args.full {
        attach_collection_budget(
            &mut value,
            "evidence",
            &format!(
                "homeboy agent-task evidence {} --full",
                quote_arg(&args.run_id)
            ),
            &format!(
                "homeboy agent-task evidence {} --full --output <path>",
                quote_arg(&args.run_id)
            ),
        );
    }
    Ok((value, 0))
}

pub(super) fn diagnose(args: DiagnoseArgs) -> CmdResult<Value> {
    let record = agent_task_service::status(&args.run_id)?;
    let aggregate = completed_run_aggregate(&args.run_id).transpose()?;
    let mut hydrated_evidence = Vec::new();
    let mut total_hydrated_evidence = 0;
    let mut nested_reasons = Vec::new();

    if let Some(aggregate) = aggregate.as_ref() {
        for outcome in &aggregate.outcomes {
            for evidence in &outcome.evidence_refs {
                total_hydrated_evidence += 1;
                if !args.full && hydrated_evidence.len() >= OutputBudget::COLLECTION.max_items {
                    continue;
                }
                if let Some(summary) =
                    agent_task_service::hydrate_evidence_summary(&outcome.task_id, evidence)
                {
                    collect_nested_diagnostics(
                        &outcome.task_id,
                        summary.get("summary").unwrap_or(&Value::Null),
                        "hydrated_evidence",
                        &mut nested_reasons,
                    );
                    hydrated_evidence.push(summary);
                }
            }
        }
    }

    let root_cause = ranked_diagnostics(nested_reasons)
        .into_iter()
        .map(collected_diagnostic_value)
        .next()
        .or_else(|| {
            aggregate
                .as_ref()
                .and_then(|aggregate| failure_reasons_from_aggregate(aggregate).into_iter().next())
        });

    let missing_artifacts = aggregate
        .as_ref()
        .map(missing_artifact_summaries)
        .unwrap_or_default();
    let causal_chain = aggregate
        .as_ref()
        .map(causal_chain_from_aggregate)
        .unwrap_or_default();
    let next_commands = diagnose_next_commands(&args.run_id);

    let mut value = json!({
        "schema": "homeboy/agent-task-diagnose/v1",
        "run_id": record.run_id.clone(),
        "state": record.state,
        "root_cause": root_cause,
        "causal_chain": causal_chain,
        "missing_artifacts": missing_artifacts.clone(),
        "hydrated_evidence": hydrated_evidence,
        "hydrated_evidence_total": total_hydrated_evidence,
        "next_commands": next_commands,
    });
    if !args.full {
        attach_collection_budget(
            &mut value,
            "hydrated_evidence",
            &format!(
                "homeboy agent-task diagnose {} --full",
                quote_arg(&args.run_id)
            ),
            &format!(
                "homeboy agent-task diagnose {} --full --output <path>",
                quote_arg(&args.run_id)
            ),
        );
    }
    // The diagnosis is only useful if it leaves the caller with the exact next
    // command for THIS failure. Everything above is already computed; this
    // projects it into the shared actionable envelope instead of discarding it.
    attach_diagnose_actionable(
        &mut value,
        &record,
        aggregate.as_ref(),
        &missing_artifacts,
        record.runner_id(),
    );
    Ok((value, 0))
}

/// Basis marker for `next_actions`: the diagnosis mapped a typed failure
/// classification (or a concrete missing-artifact set) to specific commands.
const DIAGNOSE_ACTION_BASIS_DIAGNOSIS: &str = "diagnosis";
/// Basis marker for `next_actions`: nothing in the diagnosis was specific
/// enough to act on, so the generic recovery set is emitted as an explicit
/// fallback rather than as the only behavior.
const DIAGNOSE_ACTION_BASIS_FALLBACK: &str = "generic_fallback";
/// Basis marker for a recoverable canonical candidate that takes precedence
/// over a later failed provider attempt.
const DIAGNOSE_ACTION_BASIS_CANDIDATE: &str = "canonical_candidate";

/// One failed task and how its executor classified the failure. This is the
/// typed input to the classification→action table: it is read from durable
/// outcome state, never from provider prose, so an emitted action can always be
/// substantiated.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnosedFailure {
    task_id: String,
    classification: AgentTaskFailureClassification,
}

/// Collect the distinct failure classifications a run actually recorded, first
/// implicated task wins. Successful and no-op outcomes are never a failure
/// signal even if a stale classification survived on them.
fn diagnosed_failures(aggregate: &AgentTaskAggregate) -> Vec<DiagnosedFailure> {
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
        });
    }
    failures
}

fn attach_diagnose_actionable(
    value: &mut Value,
    record: &AgentTaskRunRecord,
    aggregate: Option<&AgentTaskAggregate>,
    missing_artifacts: &[Value],
    runner_id: Option<&str>,
) {
    let run_id = record.run_id.as_str();
    let candidate_payload = candidate_result_payload(
        &serde_json::to_value(record).unwrap_or(Value::Null),
        aggregate,
    );
    let candidate_recoverable = classify_candidates(&candidate_payload)
        .state()
        .is_available();
    let failures = aggregate.map(diagnosed_failures).unwrap_or_default();
    let (next_actions, basis) = if candidate_recoverable {
        (
            vec![CommandNextAction::new(
                "review the canonical candidate",
                format!("homeboy agent-task review {}", quote_arg(run_id)),
            )
            .with_kind(CommandNextActionKind::Show)],
            DIAGNOSE_ACTION_BASIS_CANDIDATE,
        )
    } else {
        diagnose_next_actions(run_id, &failures, missing_artifacts, runner_id)
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
fn diagnose_next_actions(
    run_id: &str,
    failures: &[DiagnosedFailure],
    missing_artifacts: &[Value],
    runner_id: Option<&str>,
) -> (Vec<CommandNextAction>, &'static str) {
    let mut actions: Vec<CommandNextAction> = Vec::new();
    for failure in failures {
        for action in classification_next_actions(run_id, failure, runner_id) {
            push_unique_next_action(&mut actions, action);
        }
    }
    for action in missing_artifact_next_actions(run_id, missing_artifacts) {
        push_unique_next_action(&mut actions, action);
    }
    if actions.is_empty() {
        return (
            generic_diagnose_next_actions(run_id),
            DIAGNOSE_ACTION_BASIS_FALLBACK,
        );
    }
    (actions, DIAGNOSE_ACTION_BASIS_DIAGNOSIS)
}

fn push_unique_next_action(actions: &mut Vec<CommandNextAction>, action: CommandNextAction) {
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
fn classification_next_actions(
    run_id: &str,
    failure: &DiagnosedFailure,
    runner_id: Option<&str>,
) -> Vec<CommandNextAction> {
    let run = quote_arg(run_id);
    let task = quote_arg(&failure.task_id);
    let failure_evidence = CommandNextAction::new(
        format!("show failure evidence for {}", failure.task_id),
        format!("homeboy agent-task evidence {run} --task {task} --failure-only"),
    )
    .with_kind(CommandNextActionKind::Show);
    let retry = CommandNextAction::new(
        "retry the run from its plan",
        format!("homeboy agent-task retry {run} --run"),
    )
    .with_kind(CommandNextActionKind::Repair);
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

    match failure.classification {
        // The provider itself errored or was not resolvable: prove which
        // provider was asked for, and whether it is registered and ready.
        AgentTaskFailureClassification::Provider => {
            let mut actions = vec![failure_evidence, list_providers];
            actions.extend(provider_readiness_actions(runner_id));
            actions.push(retry);
            actions
        }
        // Documented as safe to retry with bounded backoff: lead with the retry.
        AgentTaskFailureClassification::Transient => vec![retry, failure_evidence],
        // A wall-clock timeout can still have left a complete candidate patch,
        // so review comes before spending another attempt.
        AgentTaskFailureClassification::Timeout => vec![failure_evidence, review, retry],
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
            actions.push(retry);
            actions
        }
        // Throttled: the evidence carries the retry-after hint, and another
        // registered provider may be able to take the work now.
        AgentTaskFailureClassification::RateLimited => {
            vec![
                failure_evidence,
                CommandNextAction::new(
                    "list registered providers to rotate to",
                    "homeboy agent-task providers".to_string(),
                )
                .with_kind(CommandNextActionKind::Show),
                retry,
            ]
        }
        // Policy refused this request. Retrying an identical request is denied
        // identically, so no retry action is emitted.
        AgentTaskFailureClassification::PolicyDenied => vec![
            failure_evidence,
            CommandNextAction::new(
                "show the full run record including the policy that denied it",
                format!("homeboy agent-task status {run} --full"),
            )
            .with_kind(CommandNextActionKind::Show),
        ],
        // A required capability/tool was not resolvable: the readiness chain is
        // the repair, not another attempt.
        AgentTaskFailureClassification::CapabilityMissing => {
            let mut actions = vec![
                failure_evidence,
                CommandNextAction::new(
                    "show provider declarations and discovery diagnostics",
                    "homeboy agent-task providers --full".to_string(),
                )
                .with_kind(CommandNextActionKind::Show),
            ];
            actions.extend(provider_readiness_actions(runner_id));
            actions
        }
        // The request the provider received was malformed. Replaying the
        // boundary shows the exact rejected input; a retry would resend it.
        AgentTaskFailureClassification::InvalidInput => vec![
            failure_evidence,
            CommandNextAction::new(
                format!("replay the provider boundary for {}", failure.task_id),
                format!("homeboy agent-task replay-provider-boundary {run} --task {task}"),
            )
            .with_kind(CommandNextActionKind::Show),
        ],
        // The work ran and failed (gate/verify failure, harvest failure,
        // required typed artifacts missing): show what the failing step
        // recorded and what it produced before deciding to retry.
        AgentTaskFailureClassification::ExecutionFailed => vec![
            failure_evidence,
            review,
            CommandNextAction::new(
                "list the artifacts the run produced",
                format!("homeboy agent-task artifacts {run} --full"),
            )
            .with_kind(CommandNextActionKind::Artifacts),
            retry,
        ],
        // Deliberately unmapped: an unclassified failure has no substantiable
        // specific step, so the caller gets the explicit generic fallback.
        AgentTaskFailureClassification::Unknown => Vec::new(),
    }
}

/// Provider/runner readiness chain for the runner that owns the run. Emitted
/// only when a runner id is known, because `agent-task doctor` requires one.
fn provider_readiness_actions(runner_id: Option<&str>) -> Vec<CommandNextAction> {
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
fn lost_runner_actions(runner_id: Option<&str>) -> Vec<CommandNextAction> {
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
            format!("homeboy runner doctor {runner} --repair"),
        )
        .with_kind(CommandNextActionKind::Repair),
    ]
}

/// Actions for the specific artifacts a task declared and did not produce.
fn missing_artifact_next_actions(
    run_id: &str,
    missing_artifacts: &[Value],
) -> Vec<CommandNextAction> {
    if missing_artifacts.is_empty() {
        return Vec::new();
    }
    let run = quote_arg(run_id);
    let mut actions = vec![CommandNextAction::new(
        "list the artifacts the run actually produced",
        format!("homeboy agent-task artifacts {run} --full"),
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
                format!("show the declarations {task_id} was given for {names}"),
                format!("homeboy agent-task replay-provider-boundary {run} --task {task}"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
        actions.push(
            CommandNextAction::new(
                format!("show failure evidence for {task_id}"),
                format!("homeboy agent-task evidence {run} --task {task} --failure-only"),
            )
            .with_kind(CommandNextActionKind::Show),
        );
    }
    actions
}

/// The pre-existing static set, retained as the explicit fallback for a failure
/// the diagnosis could not classify.
fn generic_diagnose_next_actions(run_id: &str) -> Vec<CommandNextAction> {
    let run = quote_arg(run_id);
    vec![
        CommandNextAction::new(
            "show the full run record",
            format!("homeboy agent-task status {run} --full"),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            "list the artifacts the run produced",
            format!("homeboy agent-task artifacts {run}"),
        )
        .with_kind(CommandNextActionKind::Artifacts),
        CommandNextAction::new("review run", format!("homeboy agent-task review {run}"))
            .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            "retry the run from its plan",
            format!("homeboy agent-task retry {run} --run"),
        )
        .with_kind(CommandNextActionKind::Repair),
    ]
}

fn diagnose_run_ref(record: &AgentTaskRunRecord, runner_id: Option<&str>) -> CommandRunRef {
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
        status_command: format!("homeboy agent-task status {run} --full"),
        watch_command: format!("homeboy agent-task logs {run}"),
    }
}

fn diagnose_artifact_refs(aggregate: &AgentTaskAggregate) -> Vec<CommandArtifactRef> {
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

fn diagnose_evidence_refs(aggregate: &AgentTaskAggregate) -> Vec<CommandArtifactRef> {
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

#[cfg(test)]
mod diagnose_actionable_tests {
    use super::*;

    fn failure(classification: AgentTaskFailureClassification) -> DiagnosedFailure {
        DiagnosedFailure {
            task_id: "task-a".to_string(),
            classification,
        }
    }

    fn commands(actions: &[CommandNextAction]) -> Vec<&str> {
        actions
            .iter()
            .map(|action| action.command.as_str())
            .collect()
    }

    fn repair_commands(actions: &[CommandNextAction]) -> Vec<&str> {
        actions
            .iter()
            .filter(|action| matches!(action.kind, Some(CommandNextActionKind::Repair)))
            .map(|action| action.command.as_str())
            .collect()
    }

    fn actions_for(
        classification: AgentTaskFailureClassification,
        runner_id: Option<&str>,
    ) -> Vec<CommandNextAction> {
        let (actions, basis) =
            diagnose_next_actions("run-1", &[failure(classification)], &[], runner_id);
        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        actions
    }

    #[test]
    fn a_provider_failure_asks_which_provider_was_registered_before_retrying() {
        let actions = actions_for(AgentTaskFailureClassification::Provider, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task providers",
                "homeboy agent-task retry run-1 --run",
            ]
        );
        assert_eq!(
            repair_commands(&actions),
            vec!["homeboy agent-task retry run-1 --run"]
        );
    }

    #[test]
    fn a_transient_failure_leads_with_the_retry_it_is_documented_to_survive() {
        let actions = actions_for(AgentTaskFailureClassification::Transient, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task retry run-1 --run",
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
            ]
        );
    }

    #[test]
    fn a_timeout_reviews_the_candidate_before_spending_another_attempt() {
        let actions = actions_for(AgentTaskFailureClassification::Timeout, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task review run-1",
                "homeboy agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn a_stalled_run_reconciles_and_repairs_the_runner_that_went_quiet() {
        let actions = actions_for(AgentTaskFailureClassification::Stalled, Some("homeboy-lab"));

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task reconcile run-1 --dry-run",
                "homeboy runner status homeboy-lab",
                "homeboy runner doctor homeboy-lab --repair",
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task retry run-1 --run",
            ]
        );
        assert_eq!(
            repair_commands(&actions),
            vec![
                "homeboy agent-task reconcile run-1 --dry-run",
                "homeboy runner doctor homeboy-lab --repair",
                "homeboy agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn a_stalled_run_without_a_known_runner_never_names_one() {
        let actions = actions_for(AgentTaskFailureClassification::Stalled, None);

        assert!(!commands(&actions)
            .iter()
            .any(|command| command.starts_with("homeboy runner ")));
    }

    #[test]
    fn a_rate_limited_failure_offers_provider_rotation() {
        let actions = actions_for(AgentTaskFailureClassification::RateLimited, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task providers",
                "homeboy agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn a_policy_denial_never_suggests_replaying_the_denied_request() {
        let actions = actions_for(AgentTaskFailureClassification::PolicyDenied, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task status run-1 --full",
            ]
        );
        assert!(repair_commands(&actions).is_empty());
    }

    #[test]
    fn a_missing_capability_runs_the_readiness_repair_chain_not_another_attempt() {
        let actions = actions_for(
            AgentTaskFailureClassification::CapabilityMissing,
            Some("homeboy-lab"),
        );

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task providers --full",
                "homeboy agent-task doctor --runner homeboy-lab",
                "homeboy agent-task doctor --runner homeboy-lab --repair",
            ]
        );
        assert_eq!(
            repair_commands(&actions),
            vec!["homeboy agent-task doctor --runner homeboy-lab --repair"]
        );
    }

    #[test]
    fn invalid_input_replays_the_rejected_boundary_instead_of_resending_it() {
        let actions = actions_for(AgentTaskFailureClassification::InvalidInput, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task replay-provider-boundary run-1 --task task-a",
            ]
        );
        assert!(repair_commands(&actions).is_empty());
    }

    #[test]
    fn an_execution_failure_shows_the_failing_step_and_what_it_produced() {
        let actions = actions_for(AgentTaskFailureClassification::ExecutionFailed, None);

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence run-1 --task task-a --failure-only",
                "homeboy agent-task review run-1",
                "homeboy agent-task artifacts run-1 --full",
                "homeboy agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn an_unclassifiable_failure_falls_back_to_the_generic_set() {
        let (actions, basis) = diagnose_next_actions(
            "run-1",
            &[failure(AgentTaskFailureClassification::Unknown)],
            &[],
            None,
        );

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_FALLBACK);
        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task status run-1 --full",
                "homeboy agent-task artifacts run-1",
                "homeboy agent-task review run-1",
                "homeboy agent-task retry run-1 --run",
            ]
        );
    }

    #[test]
    fn a_run_with_no_diagnosis_at_all_falls_back_to_the_generic_set() {
        let (actions, basis) = diagnose_next_actions("run-1", &[], &[], None);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_FALLBACK);
        assert_eq!(actions.len(), 4);
    }

    #[test]
    fn missing_artifacts_name_the_task_and_the_artifacts_that_were_not_produced() {
        let missing = vec![json!({
            "task_id": "task-b",
            "missing": ["concept_packet", "design_packet"],
        })];

        let (actions, basis) = diagnose_next_actions("run-1", &[], &missing, None);

        assert_eq!(basis, DIAGNOSE_ACTION_BASIS_DIAGNOSIS);
        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task artifacts run-1 --full",
                "homeboy agent-task replay-provider-boundary run-1 --task task-b",
                "homeboy agent-task evidence run-1 --task task-b --failure-only",
            ]
        );
        assert!(actions[1].label.contains("concept_packet, design_packet"));
    }

    #[test]
    fn classification_and_missing_artifact_actions_are_merged_without_duplicates() {
        let missing = vec![json!({ "task_id": "task-a", "missing": ["patch"] })];

        let (actions, _) = diagnose_next_actions(
            "run-1",
            &[failure(AgentTaskFailureClassification::ExecutionFailed)],
            &missing,
            None,
        );

        let commands = commands(&actions);
        assert_eq!(
            commands
                .iter()
                .filter(|command| **command
                    == "homeboy agent-task evidence run-1 --task task-a --failure-only")
                .count(),
            1
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| **command == "homeboy agent-task artifacts run-1 --full")
                .count(),
            1
        );
    }

    #[test]
    fn run_and_task_ids_are_shell_quoted_in_every_emitted_command() {
        let (actions, _) = diagnose_next_actions(
            "run with spaces",
            &[DiagnosedFailure {
                task_id: "task with spaces".to_string(),
                classification: AgentTaskFailureClassification::InvalidInput,
            }],
            &[],
            None,
        );

        assert_eq!(
            commands(&actions),
            vec![
                "homeboy agent-task evidence 'run with spaces' --task 'task with spaces' --failure-only",
                "homeboy agent-task replay-provider-boundary 'run with spaces' --task 'task with spaces'",
            ]
        );
    }
}

/// Apply the shared output primitive to a JSON collection while retaining every
/// command's established field names and schema.
fn attach_collection_budget(
    value: &mut Value,
    field: &str,
    continue_command: &str,
    export_command: &str,
) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    let values = map
        .get_mut(field)
        .and_then(Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    let total = map
        .get(&format!("{field}_total"))
        .or_else(|| map.get("total"))
        .and_then(Value::as_u64)
        .map(|count| count as usize)
        .unwrap_or(values.len());
    let (bounded, metadata) = budget_json_values(
        values,
        total,
        OutputBudget::COLLECTION,
        continue_command,
        export_command,
    );
    map.insert(field.to_string(), Value::Array(bounded));
    map.insert(
        "output_budget".to_string(),
        serde_json::to_value(metadata).unwrap_or(Value::Null),
    );
}

pub(super) fn recover_runtime(args: RuntimeRecoverArgs) -> CmdResult<Value> {
    let recovered = homeboy::agents::agent_task_lifecycle::recover_controller_runtime(
        &args.run_id,
        args.artifact.as_deref().map(std::path::Path::new),
        args.source.as_deref().map(std::path::Path::new),
    )?;
    Ok((
        json!({ "schema": "homeboy/controller-runtime-recovery/v1", "run_id": args.run_id, "recovered": recovered }),
        0,
    ))
}

pub(super) fn validate_runtime(args: RuntimeValidateArgs) -> CmdResult<Value> {
    let record = homeboy::agents::agent_task_lifecycle::validate_controller_runtime(&args.run_id)?;
    Ok((
        json!({ "schema": "homeboy/controller-runtime-validation/v1", "run_id": record.run_id, "state": record.state, "executable": true }),
        0,
    ))
}

pub(super) fn replay_provider_boundary(args: ReplayProviderBoundaryArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    let aggregate = completed_run_aggregate(&args.run_id).transpose()?;
    let plan = agent_task_lifecycle::load_plan(&args.run_id).ok();
    let evidence_entries = evidence_refs_with_tasks(&artifacts.evidence_refs, aggregate.as_ref());
    let candidates = evidence_entries
        .into_iter()
        .filter(|(evidence_ref, task_id)| {
            evidence_ref.kind == "executor-input"
                && args
                    .task
                    .as_deref()
                    .is_none_or(|requested| task_id.as_deref() == Some(requested))
        })
        .collect::<Vec<_>>();

    let candidate_count = candidates.len();
    let hydrate = |evidence_ref: &AgentTaskEvidenceRef, task_id: &Option<String>| {
        agent_task_service::hydrate_evidence_ref(
            &args.run_id,
            evidence_ref,
            task_id.as_deref(),
            plan.as_ref(),
            aggregate.as_ref(),
        )
    };
    let Some((evidence_ref, task_id, hydrated)) = candidates
        .iter()
        .filter(|(evidence_ref, _)| !evidence_ref.uri.contains("/plan#"))
        .find_map(|(evidence_ref, task_id)| {
            let hydrated = hydrate(evidence_ref, task_id);
            (hydrated.status == "ok"
                && !hydrated.truncated
                && hydrated.content.get("format").and_then(Value::as_str) == Some("json"))
            .then(|| (evidence_ref.clone(), task_id.clone(), hydrated))
        })
        .or_else(|| {
            candidates.first().map(|(evidence_ref, task_id)| {
                let hydrated = hydrate(evidence_ref, task_id);
                (evidence_ref.clone(), task_id.clone(), hydrated)
            })
        })
    else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "agent-task replay-provider-boundary",
            "no executor-input evidence was found for this run",
            Some(args.run_id),
            Some(vec![
                "Run the task through an executor that records latest raw executor input evidence.".to_string(),
                "Pass --task when inspecting a multi-task run and only one task should be selected.".to_string(),
            ]),
        ));
    };

    let input = hydrated_executor_input_value(&hydrated)?;
    let boundary = provider_boundary_projection(&input);
    let report = json!({
        "schema": "homeboy/agent-task-provider-boundary-replay/v1",
        "run_id": args.run_id,
        "task_id": task_id,
        "selected_evidence": {
            "kind": evidence_ref.kind,
            "label": evidence_ref.label,
            "uri": evidence_ref.uri,
        },
        "selection": {
            "matching_executor_input_count": candidate_count,
            "rule": "prefer concrete executor-input evidence over plan-level input refs; otherwise first matching ref wins",
        },
        "normalized_provider_boundary": boundary,
    });
    let evidence_uri = agent_task_service::persist_provider_boundary_replay_evidence(&report);
    let report = match evidence_uri {
        Some(uri) => {
            let mut report = report;
            report["typed_evidence"] = json!({
                "kind": "provider-boundary-replay",
                "uri": uri,
                "label": "provider boundary replay inspection",
            });
            report
        }
        None => report,
    };

    Ok((report, 0))
}

pub(super) fn cancel(args: CancelArgs) -> CmdResult<Value> {
    let record = agent_task_service::cancel(&args.run_id, args.reason.as_deref())?;
    let mut value = serde_json::to_value(record).unwrap_or(Value::Null);
    surface_cancellation_recovery(&mut value);
    Ok((value, 0))
}

fn hydrated_executor_input_value(
    result: &agent_task_service::AgentTaskHydratedEvidence,
) -> homeboy::core::Result<Value> {
    if result.status != "ok" {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "executor-input evidence",
            "selected executor-input evidence could not be hydrated",
            result.error.clone(),
            None,
        ));
    }
    if result.truncated {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "executor-input evidence",
            "selected executor-input evidence is truncated and cannot be replayed deterministically",
            Some(result.uri.clone()),
            None,
        ));
    }
    match result.content.get("format").and_then(Value::as_str) {
        Some("json") => Ok(result.content.get("value").cloned().unwrap_or(Value::Null)),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "executor-input evidence",
            "selected executor-input evidence is not JSON",
            Some(result.uri.clone()),
            None,
        )),
    }
}

fn provider_boundary_projection(input: &Value) -> Value {
    let mut executor_config = input
        .get("executor")
        .and_then(|executor| executor.get("config"))
        .cloned()
        .unwrap_or(Value::Null);
    normalize_runtime_env_path_aliases_for_replay(&mut executor_config);
    let runtime_task = input
        .get("inputs")
        .and_then(|inputs| inputs.get("runtime_task"))
        .cloned()
        .unwrap_or(Value::Null);
    let package_descriptor = runtime_task
        .pointer("/input/package")
        .or_else(|| input.pointer("/inputs/package_descriptor"))
        .or_else(|| input.pointer("/metadata/package_descriptor"))
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "runtime_task": runtime_task,
        "provider_config": executor_config,
        "runtime_component_paths": executor_config.get("runtime_component_paths").cloned().unwrap_or(Value::Null),
        "runtime_env": executor_config.get("runtime_env").cloned().unwrap_or(Value::Null),
        "artifact_declarations": input.get("artifact_declarations").cloned().unwrap_or(Value::Null),
        "package_descriptor": package_descriptor,
    })
}

fn normalize_runtime_env_path_aliases_for_replay(config: &mut Value) {
    let aliases = runtime_env_path_aliases_for_replay(config);
    if aliases.is_empty() {
        return;
    }

    let Some(root) = config.as_object_mut() else {
        return;
    };
    let component_paths = root
        .get("runtime_component_paths")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if component_paths.is_empty() {
        return;
    }

    if !root.get("runtime_env").is_some_and(Value::is_object) {
        root.insert(
            "runtime_env".to_string(),
            Value::Object(serde_json::Map::new()),
        );
    }

    let Some(runtime_env) = root.get_mut("runtime_env").and_then(Value::as_object_mut) else {
        return;
    };
    for (component_key, env_name) in aliases {
        if let Some(selected_path) = component_paths
            .get(&component_key)
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            runtime_env.insert(env_name, Value::String(selected_path));
        }
    }
}

fn runtime_env_path_aliases_for_replay(config: &Value) -> Vec<(String, String)> {
    let mut aliases = Vec::new();
    if let Some(map) = config
        .get("runtime_env_path_aliases")
        .and_then(Value::as_object)
    {
        for (component_key, env_names) in map {
            collect_runtime_env_aliases_for_replay(component_key, env_names, &mut aliases);
        }
    }
    if let Some(map) = config.get("runtime_env_aliases").and_then(Value::as_object) {
        for (env_name, component_key) in map {
            if let Some(component_key) = component_key.as_str() {
                aliases.push((component_key.to_string(), env_name.to_string()));
            }
        }
    }
    aliases
}

fn collect_runtime_env_aliases_for_replay(
    component_key: &str,
    value: &Value,
    aliases: &mut Vec<(String, String)>,
) {
    match value {
        Value::String(env_name) => aliases.push((component_key.to_string(), env_name.to_string())),
        Value::Array(items) => {
            for item in items {
                if let Some(env_name) = item.as_str() {
                    aliases.push((component_key.to_string(), env_name.to_string()));
                }
            }
        }
        _ => {}
    }
}

#[derive(Serialize)]
struct AgentTaskEvidenceReport {
    schema: &'static str,
    run_id: String,
    filters: AgentTaskEvidenceFilters,
    count: usize,
    evidence_total: usize,
    evidence: Vec<agent_task_service::AgentTaskHydratedEvidence>,
}

#[derive(Serialize)]
struct AgentTaskEvidenceFilters {
    kind: Option<String>,
    task: Option<String>,
    failure_only: bool,
}

fn failed_task_statuses(
    aggregate: Option<&AgentTaskAggregate>,
) -> HashMap<String, AgentTaskOutcomeStatus> {
    aggregate
        .into_iter()
        .flat_map(|aggregate| aggregate.outcomes.iter())
        .filter(|outcome| {
            matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Failed
                    | AgentTaskOutcomeStatus::ProviderError
                    | AgentTaskOutcomeStatus::Timeout
                    | AgentTaskOutcomeStatus::UnableToRemediate
            )
        })
        .map(|outcome| (outcome.task_id.clone(), outcome.status.clone()))
        .collect()
}

fn evidence_refs_with_tasks(
    refs: &[AgentTaskEvidenceRef],
    aggregate: Option<&AgentTaskAggregate>,
) -> Vec<(AgentTaskEvidenceRef, Option<String>)> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    if let Some(aggregate) = aggregate {
        for outcome in &aggregate.outcomes {
            for evidence_ref in &outcome.evidence_refs {
                if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
                    entries.push((evidence_ref.clone(), Some(outcome.task_id.clone())));
                }
            }
            if let Some(workflow) = &outcome.workflow {
                for step in &workflow.steps {
                    for evidence_ref in &step.artifact_refs {
                        if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
                            entries.push((evidence_ref.clone(), Some(outcome.task_id.clone())));
                        }
                    }
                }
            }
        }
    }
    for evidence_ref in refs {
        if seen.insert((evidence_ref.kind.clone(), evidence_ref.uri.clone())) {
            entries.push((
                evidence_ref.clone(),
                agent_task_service::evidence_ref_task_id(evidence_ref),
            ));
        }
    }
    entries
}

/// Hoist live-cancellation recovery details to the top level of the cancel
/// response so an operator sees the exact safe commands + process identifiers
/// without digging through `metadata` (#5680 acceptance: never force manual
/// process spelunking).
fn surface_cancellation_recovery(value: &mut Value) {
    let metadata = value.get("metadata").cloned().unwrap_or(Value::Null);

    if let Some(live) = metadata.get("live_cancellation").cloned() {
        value["live_cancellation"] = live;
    }

    if let Some(unsupported) = metadata.get("live_cancellation_unsupported").cloned() {
        let recovery_commands = unsupported
            .get("recovery_commands")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let reason = unsupported
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("live cancellation is not available for this provider on this host");
        value["live_cancellation_unsupported"] = unsupported.clone();
        value["recovery"] = json!({
            "message": format!(
                "Live cancellation could not signal the provider process tree directly: {reason}. Run the commands below to terminate it safely.",
            ),
            "owner_pid": unsupported.get("owner_pid").cloned().unwrap_or(Value::Null),
            "runner_id": unsupported.get("runner_id").cloned().unwrap_or(Value::Null),
            "runner_job_id": unsupported.get("runner_job_id").cloned().unwrap_or(Value::Null),
            "recovery_commands": recovery_commands,
        });
    }
}

fn enrich_with_diagnostic_summary(value: &mut Value, run_id: &str) -> homeboy::core::Result<()> {
    let Some(aggregate) = completed_run_aggregate(run_id).transpose()? else {
        return Ok(());
    };
    if let Some(summary) = diagnostic_summary_from_aggregate(&aggregate) {
        value["diagnostic_summary"] = summary;
    }
    let failure_reasons = failure_reasons_from_aggregate(&aggregate);
    if !failure_reasons.is_empty() {
        value["failure_reasons"] = Value::Array(failure_reasons);
    }
    value["execution_states"] = execution_states_from_aggregate(&aggregate, value);
    value["aggregate"] = serde_json::to_value(&aggregate).unwrap_or(Value::Null);
    Ok(())
}

pub(crate) fn completed_run_aggregate(
    run_id: &str,
) -> Option<homeboy::core::Result<AgentTaskAggregate>> {
    match agent_task_lifecycle::aggregate_source(run_id) {
        Ok((raw, _path)) => Some(serde_json::from_str(&raw).map_err(|error| {
            homeboy::core::Error::validation_invalid_json(
                error,
                Some("agent-task aggregate".to_string()),
                Some(raw),
            )
        })),
        Err(error) if error.code == homeboy::core::ErrorCode::ValidationInvalidArgument => None,
        Err(error) => Some(Err(error)),
    }
}

pub(crate) fn diagnostic_summary_from_aggregate(aggregate: &AgentTaskAggregate) -> Option<Value> {
    failure_reasons_from_aggregate(aggregate).into_iter().next()
}

/// Project terminal execution facts into stable machine-readable states. These
/// fields deliberately derive from typed outcome and lifecycle values, never
/// provider summary prose or diagnostic messages.
pub(crate) fn execution_states_from_aggregate(
    aggregate: &AgentTaskAggregate,
    record: &Value,
) -> Value {
    let review =
        homeboy::agents::agent_tasks::AgentTaskAggregateReport::from(aggregate.outcomes.clone());
    let canonical = classify_candidates(&candidate_result_payload(record, Some(aggregate)));
    let candidate_state = canonical.state();
    let provider = aggregate
        .outcomes
        .iter()
        .map(|outcome| {
            let state = if matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Failed
                    | AgentTaskOutcomeStatus::ProviderError
                    | AgentTaskOutcomeStatus::Timeout
                    | AgentTaskOutcomeStatus::UnableToRemediate
                    | AgentTaskOutcomeStatus::Cancelled
            ) {
                "failed"
            } else {
                "succeeded"
            };
            json!({
                "task_id": outcome.task_id,
                "state": state,
                "outcome_status": outcome.status,
                "failure_classification": outcome.failure_classification,
            })
        })
        .collect::<Vec<_>>();
    let candidates = review
        .tasks
        .iter()
        .map(|task| {
            let reason_code = if task.status == AgentTaskOutcomeStatus::NoOp {
                "no_changes_produced"
            } else if task.decision
                == homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::ApplyCandidate
                && candidate_state != CandidateState::PatchAvailable
            {
                candidate_state.as_str()
            } else {
                match task.decision {
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::NoOp => {
                    "no_changes_produced"
                }
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::ApplyCandidate => {
                    "patch_available"
                }
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::RetryCandidate => {
                    "provider_retry_required"
                }
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::IssueReportCandidate => {
                    "issue_report_required"
                }
                homeboy::agents::agent_tasks::AgentTaskReconciliationDecision::ReviewCandidate => {
                    "review_required"
                }
                }
            };
            json!({
                "task_id": task.task_id,
                "state": task.decision,
                "reason_code": reason_code,
            })
        })
        .collect::<Vec<_>>();
    let promotion_status = record
        .pointer("/metadata/latest_promotion")
        .cloned()
        .and_then(|value| serde_json::from_value::<AgentTaskPromotionReport>(value).ok())
        .map(|promotion| match promotion.gate_outcome().status {
            AgentTaskPromotionStatus::DryRun => "dry_run",
            AgentTaskPromotionStatus::VerificationPending => "verification_pending",
            AgentTaskPromotionStatus::Applied => "applied",
            AgentTaskPromotionStatus::GateFailed => "gate_failed",
            AgentTaskPromotionStatus::VerifiedNoChanges => "verified_no_changes",
            AgentTaskPromotionStatus::NoChangesGateFailed => "no_changes_gate_failed",
            AgentTaskPromotionStatus::NoChanges => "no_changes",
        })
        .unwrap_or_else(|| {
            record
                .pointer("/metadata/latest_promotion/status")
                .and_then(Value::as_str)
                .unwrap_or("not_attempted")
        });
    let finalization_status = record
        .pointer("/metadata/cook_finalization/status")
        .and_then(Value::as_str)
        .unwrap_or("not_attempted");
    let patch_promoted = matches!(
        promotion_status,
        "verification_pending" | "applied" | "gate_failed"
    );

    json!({
        "schema": "homeboy/agent-task-execution-states/v1",
        "provider": provider,
        "candidate": {
            "state": candidate_state.as_str(),
            "tasks": candidates,
            "scan": {
                "attempts_omitted": canonical.attempts_omitted,
                "outcomes_omitted": canonical.outcomes_omitted,
                "artifacts_omitted": canonical.artifacts_omitted,
                "degraded": canonical.is_degraded(),
            },
        },
        "gate": {
            "state": match promotion_status {
                "applied" => "passed",
                "gate_failed" => "failed",
                "verification_pending" => "pending",
                _ => "not_run",
            },
        },
        "promotion": {
            "state": promotion_status,
            "patch_promoted": patch_promoted,
        },
        "finalization": { "state": finalization_status },
    })
}

/// Cap the number of surfaced failure reasons so a pathological run with
/// hundreds of nested diagnostics cannot flood the failure summary. Overflow is
/// still available in the full nested payload (`--full` / aggregate file).
const FAILURE_REASON_LIMIT: usize = 8;

/// Build a prominent, top-level "failure reasons" summary for a failed run
/// (#3806). The actual root cause of an agent-task failure (recipe validation
/// issue, PHP fatal, provider registration error, missing path) is otherwise
/// buried deep in nested outcome JSON — both in the typed
/// `outcomes[].diagnostics[]` and in provider-specific nested structures.
///
/// This collects diagnostics from BOTH the typed field and any nested
/// `diagnostics[]` arrays found anywhere in each outcome's `outputs`/`metadata`,
/// dedupes by `(class, message)`, and orders them so the most actionable
/// root-cause classes (validation / fatal / registration / missing-path) appear
/// first. The full nested JSON is left untouched; this only ADDS a surfaced
/// summary so operators see WHY a run failed without hand-digging.
pub(crate) fn failure_reasons_from_aggregate(aggregate: &AgentTaskAggregate) -> Vec<Value> {
    let mut collected: Vec<CollectedDiagnostic> = Vec::new();

    // Failure reasons only describe failed outcomes. Scanning successful
    // outcomes incorrectly promoted provider success diagnostics (including
    // exit status zero) into failure summaries.
    let failed_first = aggregate.outcomes.iter().filter(|outcome| {
        matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Failed
                | AgentTaskOutcomeStatus::ProviderError
                | AgentTaskOutcomeStatus::Timeout
                | AgentTaskOutcomeStatus::UnableToRemediate
        )
    });
    let scan: Vec<&homeboy::agents::agent_tasks::AgentTaskOutcome> = failed_first.collect();

    for outcome in scan {
        for diagnostic in &outcome.diagnostics {
            collected.push(CollectedDiagnostic {
                task_id: outcome.task_id.clone(),
                class: diagnostic.class.clone(),
                message: diagnostic.message.clone(),
                source: "diagnostics".to_string(),
            });
        }
        collect_nested_diagnostics(
            &outcome.task_id,
            &outcome.outputs,
            "outputs",
            &mut collected,
        );
        collect_nested_diagnostics(
            &outcome.task_id,
            &outcome.metadata,
            "metadata",
            &mut collected,
        );
    }

    ranked_diagnostics(collected)
        .into_iter()
        .take(FAILURE_REASON_LIMIT)
        .map(|item| {
            json!({
                "task_id": item.task_id,
                "class": item.class,
                "message": item.message,
                "source": item.source,
            })
        })
        .collect()
}

fn ranked_diagnostics(collected: Vec<CollectedDiagnostic>) -> Vec<CollectedDiagnostic> {
    // Dedupe by (class, message) keeping the first occurrence, then order the
    // most actionable root-cause diagnostics first.
    let mut seen = std::collections::HashSet::new();
    let mut deduped: Vec<CollectedDiagnostic> = Vec::new();
    for item in collected {
        let trimmed = item.message.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = (item.class.to_ascii_lowercase(), trimmed.to_string());
        if !seen.insert(key) {
            continue;
        }
        deduped.push(item);
    }

    deduped.sort_by_key(|item| diagnostic_priority(&item.class, &item.message));
    deduped
}

struct CollectedDiagnostic {
    task_id: String,
    class: String,
    message: String,
    source: String,
}

/// Lower number = higher priority. Actionable root-cause classes
/// (validation/fatal/registration/missing-path) are surfaced before generic or
/// transient noise so the first reason an operator sees is the one worth acting
/// on.
fn diagnostic_priority(class: &str, message: &str) -> u8 {
    let text = format!("{} {}", class, message).to_ascii_lowercase();
    if text.contains("typed_artifacts_missing")
        || text.contains("required_typed_artifacts_missing")
        || text.contains("required typed artifacts")
        || text.contains("declared artifact result envelope")
    {
        8
    } else if text.contains("valid") || text.contains("recipe") || text.contains("schema") {
        0
    } else if text.contains("fatal") || text.contains("error") || text.contains("exception") {
        1
    } else if text.contains("registr")
        || text.contains("provider")
        || text.contains("discovery")
        || text.contains("capability")
    {
        2
    } else if text.contains("missing")
        || text.contains("not_found")
        || text.contains("path")
        || text.contains("io")
    {
        3
    } else {
        9
    }
}

/// Recursively walk a provider-specific JSON value looking for `diagnostics`
/// arrays of objects carrying a `message` (and optional `class`). This is how
/// provider-owned runtime diagnostics get surfaced without the renderer needing
/// to know the exact provider path.
fn collect_nested_diagnostics(
    task_id: &str,
    value: &Value,
    source: &str,
    out: &mut Vec<CollectedDiagnostic>,
) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(items)) = map.get("diagnostics") {
                for item in items {
                    if let Some(message) = item
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                    {
                        let class = item
                            .get("class")
                            .or_else(|| item.get("kind"))
                            .or_else(|| item.get("level"))
                            .and_then(Value::as_str)
                            .unwrap_or("nested")
                            .to_string();
                        out.push(CollectedDiagnostic {
                            task_id: task_id.to_string(),
                            class,
                            message,
                            source: source.to_string(),
                        });
                    }
                }
            }
            for nested in map.values() {
                collect_nested_diagnostics(task_id, nested, source, out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_nested_diagnostics(task_id, nested, source, out);
            }
        }
        _ => {}
    }
}

/// Build the compact, recovery-first `status` summary. Source data is the full
/// run-record `Value` (already enriched with `diagnostic_summary`); the plan is
/// loaded best-effort to map task ids back to issue URLs and prompt titles.
fn compact_status_summary(record: &Value, run_id: &str) -> Value {
    let aggregate = completed_run_aggregate(run_id).and_then(Result::ok);
    compact_status_summary_with_aggregate(record, run_id, aggregate.as_ref())
}

fn compact_status_summary_with_aggregate(
    record: &Value,
    run_id: &str,
    aggregate: Option<&AgentTaskAggregate>,
) -> Value {
    let plan = agent_task_lifecycle::load_plan(run_id).ok();
    let task_table = task_source_table(record, plan.as_ref());
    let tasks_omitted = record
        .get("tasks")
        .and_then(Value::as_array)
        .map_or(0, |tasks| tasks.len().saturating_sub(COMPACT_TASK_LIMIT));
    let ref_inventory = ref_inventory(record, aggregate);
    let (refs, refs_omitted) = compact_refs(&ref_inventory);
    let risk_flags = risk_flags(record);
    let work_summary = work_summary(record, aggregate, &ref_inventory);
    let canonical_candidate = classify_candidates(&candidate_result_payload(record, aggregate));

    let mut summary = json!({
        "schema": "homeboy/agent-task-status-summary/v1",
        "run_id": record.get("run_id").cloned().unwrap_or_else(|| json!(run_id)),
        "state": record.get("state").cloned().unwrap_or(Value::Null),
        "timestamps": compact_fields(record, &["created_at", "updated_at", "started_at", "completed_at"]),
        "work_summary": work_summary,
        "canonical_candidate": canonical_candidate_projection(canonical_candidate),
        "artifact_refs": refs.clone(),
        "artifact_refs_omitted": refs_omitted,
        "totals": record.get("totals").cloned().unwrap_or(Value::Null),
        "tasks": task_table,
        "tasks_omitted": tasks_omitted,
        "refs": refs,
        "refs_omitted": refs_omitted,
        "risk_flags": risk_flags,
        "execution_location": execution_location(record),
        "queue_visibility": queue_visibility(record),
        "execution_budget": plan.as_ref().map(|plan| &plan.options.execution_budget),
        "liveness": liveness_summary(record, run_id, canonical_candidate.state()),
        "full_command": format!("homeboy agent-task status {run_id} --full"),
    });

    if let Some(diagnostic) = record.get("diagnostic_summary") {
        if !diagnostic.is_null() {
            summary["diagnostic_summary"] = diagnostic.clone();
        }
    }
    if let Some(recovery) = record.get("transport_recovery") {
        summary["transport_recovery"] = recovery.clone();
    }
    if let Some(failure_reasons) = record.get("failure_reasons") {
        if failure_reasons
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty())
        {
            summary["failure_reasons"] = failure_reasons.clone();
        }
    }
    if let Some(execution_states) = record.get("execution_states") {
        summary["execution_states"] = execution_states.clone();
    }
    if let Some(aggregate_path) = record.get("aggregate_path") {
        if !aggregate_path.is_null() {
            summary["aggregate_path"] = aggregate_path.clone();
        }
    }
    if let Some(plan) = plan {
        summary["execution_budget"] =
            serde_json::to_value(&plan.options.execution_budget).unwrap_or(Value::Null);
    }
    if let Some(latest_promotion) = record
        .get("metadata")
        .and_then(|metadata| metadata.get("latest_promotion"))
    {
        if !latest_promotion.is_null() {
            summary["latest_promotion"] = compact_fields(
                latest_promotion,
                &[
                    "schema",
                    "status",
                    "run_id",
                    "task_id",
                    "artifact_id",
                    "patch_artifact_id",
                    "patch_artifact",
                    "patch",
                    "updated_at",
                    "created_at",
                    "command",
                ],
            );
        }
    }
    if let Some(cook_finalization) = record
        .get("metadata")
        .and_then(|metadata| metadata.get("cook_finalization"))
    {
        if !cook_finalization.is_null() {
            summary["cook_finalization"] = compact_fields(
                cook_finalization,
                &[
                    "schema",
                    "status",
                    "pr_number",
                    "pr_url",
                    "pull_request_url",
                    "updated_at",
                    "created_at",
                ],
            );
        }
    }
    summary
}

/// Project completion evidence for machine consumers. The durable aggregate and
/// `agent-task status <run-id> --full` remain the explicit lossless paths.
pub(crate) fn compact_aggregate_summary(
    aggregate: &AgentTaskAggregate,
    run_id: Option<&str>,
) -> Value {
    let full = serde_json::to_value(
        homeboy::agents::agent_task_artifacts::reviewer_facing_aggregate(aggregate),
    )
    .unwrap_or(Value::Null);
    let outcomes = full
        .get("outcomes")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let tasks = outcomes.iter().take(COMPACT_TASK_LIMIT).map(|outcome| json!({
        "task_id": outcome.get("task_id"),
        "status": outcome.get("status"),
        "summary": bounded_value(outcome.get("summary").unwrap_or(&Value::Null)),
        "failure_classification": outcome.get("failure_classification"),
        "timestamps": compact_fields(outcome, &["created_at", "updated_at", "timestamp", "finished_at"]),
        "artifacts": compact_items(outcome.get("artifacts"), &["schema", "id", "kind", "name", "path", "url", "sha256", "size_bytes"]),
        "evidence_refs": compact_items(outcome.get("evidence_refs"), &["schema", "kind", "uri", "created_at", "timestamp"]),
    })).collect::<Vec<_>>();
    let mut summary = json!({
        "schema": full.get("schema"),
        "view": "summary",
        "plan_id": full.get("plan_id"),
        "status": full.get("status"),
        "totals": full.get("totals"),
        "tasks": tasks,
        "tasks_omitted": outcomes.len().saturating_sub(COMPACT_TASK_LIMIT),
        "failure_reasons": bounded_failure_reasons(&Value::Array(failure_reasons_from_aggregate(aggregate))),
    });
    if let Some(run_id) = run_id {
        summary["run_id"] = json!(run_id);
        summary["full_command"] = json!(format!("homeboy agent-task status {run_id} --full"));
        summary["evidence_command"] = json!(format!("homeboy agent-task evidence {run_id}"));
    }
    summary
}

pub(crate) fn compact_cook_report(value: Value, full: bool) -> Value {
    if full {
        return value;
    }
    let attempts = value
        .get("attempts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let latest_run_id = value.get("latest_run_id").and_then(Value::as_str);
    let mut summary = json!({
        "schema": value.get("schema"),
        "view": "summary",
        "cook_id": value.get("cook_id"),
        "latest_run_id": value.get("latest_run_id"),
        "status": value.get("status"),
        "stop_reason": bounded_value(value.get("stop_reason").unwrap_or(&Value::Null)),
        "terminal_phase": value.get("terminal_phase"),
        "terminal_failure_classification": value.get("terminal_failure_classification"),
        "attempts": attempts.iter().take(COMPACT_TASK_LIMIT).map(|attempt| compact_fields(attempt, &["attempt", "run_id", "run_state", "aggregate_path"])).collect::<Vec<_>>(),
        "attempts_omitted": attempts.len().saturating_sub(COMPACT_TASK_LIMIT),
        "finalization": value.get("finalization").map(|finalization| compact_fields(finalization, &["schema", "status", "pr_number", "pr_url", "updated_at", "created_at"])),
    });
    if let Some(run_id) = latest_run_id {
        summary["full_command"] = json!(format!("homeboy agent-task status {run_id} --full"));
        summary["evidence_command"] = json!(format!("homeboy agent-task evidence {run_id}"));
    }
    for field in ["provider", "remaining_phases", "continuation_command"] {
        if let Some(value) = value.get(field) {
            summary[field] = bounded_value(value);
        }
    }
    summary
}

fn compact_items(value: Option<&Value>, fields: &[&str]) -> Value {
    Value::Array(
        value
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(COMPACT_REF_LIMIT)
                    .map(|item| compact_fields(item, fields))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn compact_fields(value: &Value, fields: &[&str]) -> Value {
    let mut object = serde_json::Map::new();
    for field in fields {
        if let Some(value) = value.get(*field) {
            object.insert((*field).to_string(), bounded_value(value));
        }
    }
    Value::Object(object)
}

fn bounded_failure_reasons(value: &Value) -> Value {
    Value::Array(
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .take(FAILURE_REASON_LIMIT)
                    .map(|item| compact_fields(item, &["task_id", "class", "message", "source"]))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn bounded_value(value: &Value) -> Value {
    match value {
        Value::String(text) if text.chars().count() > COMPACT_TEXT_LIMIT => Value::String(format!(
            "{}...",
            text.chars().take(COMPACT_TEXT_LIMIT).collect::<String>()
        )),
        _ => value.clone(),
    }
}

fn liveness_summary(record: &Value, run_id: &str, candidate_state: CandidateState) -> Value {
    let metadata = record.get("metadata").unwrap_or(&Value::Null);
    let provider_handle_count = record
        .get("provider_handles")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let stale = metadata
        .get("stale_running")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let runner_job_id = metadata
        .get("runner_job_id")
        .and_then(Value::as_str)
        .filter(|job_id| !job_id.trim().is_empty());
    let terminal = record
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|state| matches!(state, "succeeded" | "failed" | "cancelled"));
    let waiting_for_capacity = metadata
        .get("runner_queue")
        .and_then(|queue| queue.get("state"))
        .and_then(Value::as_str)
        == Some("waiting_for_capacity");
    let candidate_recoverable = candidate_state.is_recoverable();

    // A local cook runs the provider in an in-process thread, so it has neither
    // provider handles nor a runner job id. `reserve_provider_execution` still
    // durably records the running attempt (backend, model, started_at) before
    // the scheduler blocks on the backend, so surface that as authoritative
    // in-flight provider evidence instead of reporting the boundary absent
    // while the provider is actively working (#8396).
    let active_provider_executions = running_provider_executions(metadata);
    let has_active_provider_execution = !active_provider_executions.is_empty();
    let provider_boundary_recorded =
        provider_handle_count > 0 || runner_job_id.is_some() || has_active_provider_execution;
    let local_provider_ownership = metadata.get("local_provider_ownership");

    json!({
        "status": if terminal { "terminal" } else if stale { "stale" } else if waiting_for_capacity { "waiting_for_capacity" } else { "active" },
        "heartbeat_last_seen_at": record.pointer("/lifecycle/heartbeat/last_seen_at"),
        "runner_job_status": metadata.get("runner_job_status"),
        "runner_job_last_seen_at": metadata.get("runner_job_last_seen_at"),
        "provider_boundary": {
            "status": if provider_boundary_recorded { "recorded" } else { "absent" },
            "provider_handle_count": provider_handle_count,
            "runner_job_id": runner_job_id,
            "active_execution_count": active_provider_executions.len(),
            "active_executions": active_provider_executions,
            "local_owner": local_provider_ownership,
        },
        "stale_reason": metadata.get("stale_running_reason"),
        "runner_queue": metadata.get("runner_queue"),
        "next_action": if terminal && candidate_recoverable {
            format!("homeboy agent-task review {run_id}")
        } else if stale {
            format!("homeboy agent-task reconcile {} --dry-run", quote_arg(run_id))
        } else if waiting_for_capacity {
            "await runner completion or reconnect; the runner will claim this FIFO queue entry under its capacity lease".to_string()
        } else {
            "homeboy agent-task status <run-id> --full".to_string()
        },
    })
}

/// Project the durable `provider_executions` metadata into a compact list of the
/// attempts that are still `running`. These are written by
/// `reserve_provider_execution` before the scheduler blocks on the backend and
/// cleared to a terminal state by `record_provider_execution_terminal`, so a
/// running entry is authoritative evidence that a provider is executing right
/// now — the only in-flight signal a local (in-process) cook has (#8396).
fn running_provider_executions(metadata: &Value) -> Vec<Value> {
    metadata
        .get("provider_executions")
        .and_then(Value::as_array)
        .map(|executions| {
            executions
                .iter()
                .filter(|execution| {
                    execution.get("state").and_then(Value::as_str) == Some("running")
                })
                .map(|execution| {
                    json!({
                        "task_id": execution.get("task_id"),
                        "attempt": execution.get("attempt"),
                        "backend": execution.get("backend"),
                        "model": execution.get("model"),
                        "started_at": execution.get("started_at"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn execution_location(record: &Value) -> Value {
    let runner_id = record
        .get("metadata")
        .and_then(|metadata| metadata.get("runner_id"))
        .and_then(Value::as_str)
        .filter(|runner_id| !runner_id.trim().is_empty());
    match runner_id {
        Some(runner_id) => json!(format!("runner:{runner_id}")),
        None => json!("local"),
    }
}

fn queue_visibility(record: &Value) -> Value {
    json!({
        "state": record.get("state").cloned().unwrap_or(Value::Null),
        "totals": record.get("totals").cloned().unwrap_or(Value::Null),
        "commands": [
            "homeboy agent-task list",
            "homeboy agent-task active",
            "homeboy agent-task run-next",
        ],
        "concurrency_note": "Cook/controller concurrency is declared by the queued plan; use `homeboy agent-task status <run-id> --full` to inspect the materialized dispatch settings.",
    })
}

/// Map each run-record task to a source label: task id + issue URL (from the
/// plan source refs) + the first sentence/title of the prompt + a brief
/// artifact summary (#4392).
fn task_source_table(record: &Value, plan: Option<&AgentTaskPlan>) -> Value {
    let Some(tasks) = record.get("tasks").and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };

    let rows: Vec<Value> = tasks
        .iter()
        .take(COMPACT_TASK_LIMIT)
        .map(|task| {
            let task_id = task.get("task_id").and_then(Value::as_str).unwrap_or("");
            let state = task.get("state").cloned().unwrap_or(Value::Null);
            let (issue_url, prompt_title) = plan
                .and_then(|plan| plan_task_source(plan, task_id))
                .unwrap_or((None, None));
            let artifact_summary = task_artifact_summary(record, task_id);

            json!({
                "task_id": task_id,
                "state": state,
                "issue_url": issue_url,
                "prompt": prompt_title,
                "artifacts": artifact_summary,
            })
        })
        .collect();

    Value::Array(rows)
}

/// Resolve a task's issue URL and prompt title from the loaded plan.
fn plan_task_source(
    plan: &AgentTaskPlan,
    task_id: &str,
) -> Option<(Option<String>, Option<String>)> {
    let request = plan.tasks.iter().find(|task| task.task_id == task_id)?;
    let issue_url = request
        .source_refs
        .iter()
        .find(|source| is_issue_uri(&source.uri))
        .or_else(|| request.source_refs.first())
        .map(|source| source.uri.clone());
    let prompt_title = first_sentence(&request.instructions);
    Some((issue_url, prompt_title))
}

fn is_issue_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    lower.contains("/issues/") || lower.contains("/pull/") || lower.contains("github.com")
}

/// First sentence (or first line) of a prompt, trimmed to a recovery-friendly
/// length so the summary stays scannable.
fn first_sentence(prompt: &str) -> Option<String> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return None;
    }
    let end = trimmed
        .find(['.', '\n'])
        .map(|index| index + 1)
        .unwrap_or(trimmed.len());
    let sentence = trimmed[..end].trim().trim_end_matches('.').trim();
    const MAX_CHARS: usize = 140;
    let title = if sentence.chars().count() > MAX_CHARS {
        let truncated: String = sentence.chars().take(MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        sentence.to_string()
    };
    (!title.is_empty()).then_some(title)
}

/// Brief per-task artifact summary derived from the run record's deduped
/// `artifact_refs`.
fn task_artifact_summary(record: &Value, task_id: &str) -> Value {
    let refs = record
        .get("artifact_refs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let task_refs: Vec<&Value> = refs
        .iter()
        .filter(|item| item.get("task_id").and_then(Value::as_str) == Some(task_id))
        .collect();
    let mut kinds: Vec<String> = task_refs
        .iter()
        .filter_map(|item| item.get("kind").and_then(Value::as_str).map(str::to_string))
        .collect();
    kinds.sort();
    kinds.dedup();
    json!({
        "count": task_refs.len(),
        "kinds": kinds,
    })
}

#[derive(Clone)]
struct CompactRef {
    task_id: Value,
    kind: Value,
    uri: String,
    is_evidence: bool,
}

fn ref_inventory(record: &Value, aggregate: Option<&AgentTaskAggregate>) -> Vec<CompactRef> {
    let mut refs: Vec<CompactRef> = record
        .get("artifact_refs")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let uri = item.get("uri").and_then(Value::as_str)?.trim();
                    (!uri.is_empty()).then(|| CompactRef {
                        task_id: item.get("task_id").cloned().unwrap_or(Value::Null),
                        kind: item.get("kind").cloned().unwrap_or(Value::Null),
                        uri: uri.to_string(),
                        is_evidence: false,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(aggregate) = aggregate {
        for outcome in &aggregate.outcomes {
            for artifact in &outcome.artifacts {
                let uri = artifact
                    .url
                    .as_deref()
                    .or(artifact.path.as_deref())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("artifact:{}:{}", outcome.task_id, artifact.id));
                refs.push(CompactRef {
                    task_id: json!(outcome.task_id),
                    kind: json!(artifact.kind),
                    uri,
                    is_evidence: false,
                });
            }
            for evidence in &outcome.evidence_refs {
                let uri = evidence.uri.trim();
                if uri.is_empty() {
                    continue;
                }
                refs.push(CompactRef {
                    task_id: json!(outcome.task_id),
                    kind: json!(evidence.kind),
                    uri: uri.to_string(),
                    is_evidence: true,
                });
            }
        }
    }

    refs
}

/// Deduped, empty-uri-filtered artifact/evidence refs, capped to keep the
/// recovery summary scannable. The full list remains available via `--full`.
fn compact_refs(refs: &[CompactRef]) -> (Value, usize) {
    let mut seen = std::collections::HashSet::new();
    let mut rendered: Vec<Value> = Vec::new();
    let mut total_valid = 0usize;

    for item in refs {
        if item.uri.trim().is_empty() {
            continue;
        }
        if !seen.insert(item.uri.clone()) {
            continue;
        }
        total_valid += 1;
        if rendered.len() < COMPACT_REF_LIMIT {
            rendered.push(json!({
                "task_id": item.task_id.clone(),
                "kind": item.kind.clone(),
                "uri": item.uri.clone(),
            }));
        }
    }

    let omitted = total_valid.saturating_sub(rendered.len());
    (Value::Array(rendered), omitted)
}

fn work_summary(
    record: &Value,
    aggregate: Option<&AgentTaskAggregate>,
    refs: &[CompactRef],
) -> Value {
    let latest_promotion = record
        .get("metadata")
        .and_then(|metadata| metadata.get("latest_promotion"));
    let promotion_status = latest_promotion
        .and_then(|promotion| promotion.get("status"))
        .and_then(Value::as_str);
    let artifact_ref_count = deduped_ref_count(refs.iter().filter(|item| !item.is_evidence));
    let evidence_ref_count = deduped_ref_count(refs.iter().filter(|item| item.is_evidence));
    let provider_status = provider_execution_status(record, aggregate);
    let committed_changes = provider_committed_changes(record)
        || aggregate.is_some_and(aggregate_has_committed_changes)
        || latest_promotion.is_some_and(promotion_reports_committed_changes);
    let canonical = classify_candidates(&candidate_result_payload(record, aggregate));
    let classification = work_classification(
        record,
        promotion_status,
        artifact_ref_count,
        evidence_ref_count,
        committed_changes,
        canonical.state(),
    );

    json!({
        "classification": classification,
        "candidate_state": canonical.state().as_str(),
        "candidate_counts": {
            "patch_available": canonical.available,
            "empty": canonical.empty,
            "missing": canonical.missing,
            "unreadable": canonical.unreadable,
            "conflicting": canonical.conflicting,
            "retained_only": canonical.retained_only,
            "unknown": canonical.unknown,
        },
        "candidate_scan": {
            "attempts_omitted": canonical.attempts_omitted,
            "outcomes_omitted": canonical.outcomes_omitted,
            "artifacts_omitted": canonical.artifacts_omitted,
            "degraded": canonical.is_degraded(),
        },
        "provider_execution_status": provider_status,
        "promotion_status": promotion_status,
        "artifact_ref_count": artifact_ref_count,
        "evidence_ref_count": evidence_ref_count,
        "committed_changes_detected": committed_changes,
        "artifact_command": record.get("run_id").and_then(Value::as_str).map(|run_id| format!("homeboy agent-task artifacts {run_id}")),
    })
}

/// Combine the current record with the immutable aggregate before projecting a
/// candidate result. Promotion, finalization, and adoption facts live on the
/// record while patch facts live in the aggregate; neither may erase the other.
fn candidate_result_payload(record: &Value, aggregate: Option<&AgentTaskAggregate>) -> Value {
    let mut payload = record.clone();
    if let Some(aggregate) = aggregate {
        payload["aggregate"] = serde_json::to_value(aggregate).unwrap_or(Value::Null);
    }
    payload
}

fn deduped_ref_count<'a>(refs: impl Iterator<Item = &'a CompactRef>) -> usize {
    refs.filter(|item| !item.uri.trim().is_empty())
        .map(|item| item.uri.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

fn provider_execution_status(record: &Value, aggregate: Option<&AgentTaskAggregate>) -> Value {
    if let Some(aggregate) = aggregate {
        let mut statuses: Vec<String> = aggregate
            .outcomes
            .iter()
            .map(|outcome| format!("{:?}", outcome.status).to_ascii_lowercase())
            .collect();
        statuses.sort();
        statuses.dedup();
        if statuses.len() == 1 {
            return json!(statuses[0]);
        }
        if !statuses.is_empty() {
            return json!(statuses);
        }
    }
    record.get("state").cloned().unwrap_or(Value::Null)
}

fn work_classification(
    record: &Value,
    promotion_status: Option<&str>,
    artifact_ref_count: usize,
    evidence_ref_count: usize,
    committed_changes: bool,
    candidate_state: CandidateState,
) -> &'static str {
    if committed_changes && promotion_status.is_some_and(is_no_change_promotion_status) {
        return "committed_changes_pending_promotion";
    }
    match candidate_state {
        CandidateState::Finalized => return "pull_request_finalized",
        CandidateState::Promoted => return "promoted_changes",
        CandidateState::PatchAvailable => return "provider_completed_patch_available",
        _ => {}
    }
    if promotion_status.is_some_and(is_no_change_promotion_status) {
        if artifact_ref_count == 0 && evidence_ref_count == 0 {
            return "no_changes";
        }
        return "provider_completed_artifacts_pending_review";
    }
    match promotion_status {
        Some("applied") => return "promoted_changes",
        Some("gate_failed") => return "promoted_changes_gate_failed",
        Some("dry_run") => return "promotion_dry_run",
        _ => {}
    }
    if artifact_ref_count > 0 || evidence_ref_count > 0 {
        return "provider_completed_artifacts_available";
    }
    if record.get("state").and_then(Value::as_str) == Some("succeeded") {
        return "no_changes";
    }
    "unknown"
}

fn is_no_change_promotion_status(status: &str) -> bool {
    matches!(status, "no_changes" | "no_patch_produced")
}

fn provider_committed_changes(record: &Value) -> bool {
    value_reports_committed_changes(record)
}

fn aggregate_has_committed_changes(aggregate: &AgentTaskAggregate) -> bool {
    aggregate.outcomes.iter().any(|outcome| {
        value_reports_committed_changes(&outcome.outputs)
            || value_reports_committed_changes(&outcome.metadata)
            || outcome.artifacts.iter().any(|artifact| {
                value_reports_committed_changes(&artifact.metadata)
                    || artifact
                        .metadata
                        .get("changed_files")
                        .and_then(Value::as_array)
                        .is_some_and(|files| !files.is_empty())
            })
    })
}

fn promotion_reports_committed_changes(promotion: &Value) -> bool {
    value_reports_committed_changes(promotion)
        || promotion
            .get("changed_files")
            .and_then(Value::as_array)
            .is_some_and(|files| !files.is_empty())
}

fn value_reports_committed_changes(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("committed_changes").and_then(Value::as_bool) == Some(true)
                || map
                    .get("commits")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty())
                || map
                    .get("commit_shas")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty())
                || map
                    .get("provider_commits")
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty())
                || map.values().any(value_reports_committed_changes)
        }
        Value::Array(items) => items.iter().any(value_reports_committed_changes),
        _ => false,
    }
}

/// Surface artifact RISK FLAGS prominently (#4398). Flags are derived from the
/// run record's artifact refs and the completed aggregate's artifact metadata,
/// so reviewers see them before promotion/apply instead of digging through
/// buried payloads.
fn risk_flags(record: &Value) -> Value {
    let mut flags: Vec<Value> = Vec::new();

    let run_id = record.get("run_id").and_then(Value::as_str);
    let aggregate = run_id
        .and_then(completed_run_aggregate)
        .and_then(Result::ok);

    let mut has_patch = false;
    let mut has_test_evidence = false;

    if let Some(aggregate) = aggregate.as_ref() {
        for outcome in &aggregate.outcomes {
            for artifact in &outcome.artifacts {
                if artifact.kind == "patch" {
                    has_patch = true;
                    if artifact_is_full_file_rewrite(&artifact.metadata) {
                        flags.push(json!({
                            "flag": "suspicious-full-file-rewrite",
                            "task_id": outcome.task_id,
                            "artifact_id": artifact.id,
                            "detail": "patch artifact metadata marks a full-file rewrite; review the diff scope before applying",
                        }));
                    }
                }
                if value_mentions_redaction(&artifact.metadata) {
                    flags.push(json!({
                        "flag": "secrets-redacted",
                        "task_id": outcome.task_id,
                        "artifact_id": artifact.id,
                        "detail": "artifact metadata contains redacted values; verify no secret leaked into the patch/output",
                    }));
                }
            }
            for evidence in &outcome.evidence_refs {
                if evidence_is_test(&evidence.kind, &evidence.uri) {
                    has_test_evidence = true;
                }
            }
        }
    }

    if has_patch && !has_test_evidence {
        flags.push(json!({
            "flag": "missing-test-evidence",
            "detail": "a patch was produced but no test/transcript evidence ref was recorded; confirm verification before promotion",
        }));
    }

    Value::Array(flags)
}

fn artifact_is_full_file_rewrite(metadata: &Value) -> bool {
    metadata
        .get("full_file_rewrite")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || metadata
            .get("suspicious_full_file_rewrite")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn value_mentions_redaction(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            lower.contains("[redacted]") || lower.contains("redacted")
        }
        Value::Array(items) => items.iter().any(value_mentions_redaction),
        Value::Object(map) => map.values().any(value_mentions_redaction),
        _ => false,
    }
}

fn evidence_is_test(kind: &str, uri: &str) -> bool {
    let kind = kind.to_ascii_lowercase();
    let uri = uri.to_ascii_lowercase();
    kind.contains("test")
        || kind.contains("transcript")
        || kind.contains("gate")
        || uri.contains("test")
        || uri.contains("transcript")
}

fn collected_diagnostic_value(item: CollectedDiagnostic) -> Value {
    json!({
        "task_id": item.task_id,
        "class": item.class,
        "message": item.message,
        "source": item.source,
    })
}

fn missing_artifact_summaries(aggregate: &AgentTaskAggregate) -> Vec<Value> {
    aggregate
        .outcomes
        .iter()
        .filter_map(|outcome| {
            let expected: Vec<String> = outcome
                .metadata
                .get("expected_artifacts")
                .or_else(|| outcome.outputs.get("expected_artifacts"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let produced: std::collections::HashSet<String> = outcome
                .typed_artifacts
                .iter()
                .map(|artifact| artifact.name.clone())
                .collect();
            let missing: Vec<String> = expected
                .into_iter()
                .filter(|name| !produced.contains(name))
                .collect();
            (!missing.is_empty()).then(|| {
                json!({
                    "task_id": outcome.task_id,
                    "missing": missing,
                })
            })
        })
        .collect()
}

fn causal_chain_from_aggregate(aggregate: &AgentTaskAggregate) -> Vec<Value> {
    aggregate
        .outcomes
        .iter()
        .map(|outcome| {
            json!({
                "task_id": outcome.task_id,
                "surface": "agent-task",
                "status": outcome.status,
                "failure_classification": outcome.failure_classification,
                "provider_summary": outcome.summary,
                "evidence_kinds": outcome.evidence_refs.iter().map(|evidence| evidence.kind.clone()).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn diagnose_next_commands(run_id: &str) -> Vec<String> {
    vec![
        format!("homeboy agent-task status {run_id} --full"),
        format!("homeboy agent-task artifacts {run_id}"),
        format!("homeboy agent-task review {run_id}"),
        format!("homeboy agent-task retry {run_id} --run"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_status_surfaces_latest_promotion() {
        let record = json!({
            "run_id": "agent-task-run-1",
            "state": "succeeded",
            "tasks": [],
            "metadata": {
                "latest_promotion": {
                    "schema": "homeboy/agent-task-promotion-status/v1",
                    "status": "applied",
                    "source_run_id": "agent-task-run-1",
                    "patch_artifact_id": "patch.diff",
                    "to_worktree": "homeboy@fix-5055",
                    "operator_notification": {
                        "status": "completed",
                        "message": "patch promoted"
                    }
                }
            }
        });

        let summary = compact_status_summary(&record, "agent-task-run-1");

        assert_eq!(summary["latest_promotion"]["status"], "applied");
        assert!(summary["latest_promotion"]
            .get("operator_notification")
            .is_none());
        assert_eq!(
            summary["queue_visibility"]["commands"][0],
            "homeboy agent-task list"
        );
        assert_eq!(summary["liveness"]["status"], "terminal");
        assert_eq!(
            summary["liveness"]["next_action"],
            "homeboy agent-task status <run-id> --full"
        );
        assert!(summary["queue_visibility"]["concurrency_note"]
            .as_str()
            .unwrap()
            .contains("concurrency"));
        assert_eq!(summary["execution_location"], "local");

        let remote = compact_status_summary(
            &json!({
                "run_id": "agent-task-run-2",
                "state": "running",
                "tasks": [],
                "metadata": { "runner_id": "homeboy-lab" }
            }),
            "agent-task-run-2",
        );
        assert_eq!(remote["execution_location"], "runner:homeboy-lab");

        let stale = compact_status_summary(
            &json!({
                "run_id": "agent-task-ghost",
                "state": "running",
                "tasks": [],
                "provider_handles": [],
                "lifecycle": { "heartbeat": { "last_seen_at": "2026-07-12T23:28:28Z" } },
                "metadata": {
                    "stale_running": true,
                    "stale_running_reason": "runner_job_unverified_after_daemon_restart"
                }
            }),
            "agent-task-ghost",
        );
        assert_eq!(stale["liveness"]["status"], "stale");
        assert_eq!(stale["liveness"]["provider_boundary"]["status"], "absent");
        assert_eq!(
            stale["liveness"]["next_action"],
            "homeboy agent-task reconcile agent-task-ghost --dry-run"
        );

        let accepted_handoff = compact_status_summary(
            &json!({
                "run_id": "agent-task-run-bound",
                "state": "running",
                "tasks": [],
                "provider_handles": [],
                "metadata": {
                    "runner_id": "homeboy-lab",
                    "runner_job_id": "accepted-daemon-job"
                }
            }),
            "agent-task-run-bound",
        );
        assert_eq!(
            accepted_handoff["liveness"]["provider_boundary"]["status"],
            "recorded"
        );
        assert_eq!(
            accepted_handoff["liveness"]["provider_boundary"]["runner_job_id"],
            "accepted-daemon-job"
        );
    }

    #[test]
    fn compact_status_envelope_preserves_candidate_lifecycle_parity() {
        let promoted_record = json!({
            "run_id": "agent-task-promoted",
            "state": "succeeded",
            "tasks": [],
            "metadata": { "latest_promotion": {
                "status": "applied", "patch_artifact": { "id": "patch" }
            }}
        });
        let promoted = compact_status_summary(&promoted_record, "agent-task-promoted");
        assert_eq!(promoted["latest_promotion"]["status"], "applied");
        assert_eq!(
            classify_candidates(&promoted).state(),
            CandidateState::Promoted
        );

        let finalized_record = json!({
            "run_id": "agent-task-finalized",
            "state": "succeeded",
            "tasks": [],
            "metadata": { "cook_finalization": {
                "status": "review_ready", "pr_url": "https://example.test/pull/1"
            }}
        });
        let finalized = compact_status_summary(&finalized_record, "agent-task-finalized");
        assert_eq!(finalized["cook_finalization"]["status"], "review_ready");
        assert_eq!(
            classify_candidates(&finalized).state(),
            CandidateState::Finalized
        );
    }

    #[test]
    fn default_status_projects_durable_patch_candidate_for_rendering_and_actions() {
        let record = json!({
            "run_id": "agent-task-durable-patch",
            "state": "succeeded",
            "tasks": [],
        });
        let aggregate: AgentTaskAggregate = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": "plan-durable-patch",
            "status": "succeeded",
            "totals": {
                "queued": 0, "running": 0, "blocked": 0, "skipped": 0,
                "succeeded": 1, "failed": 0, "cancelled": 0, "timed_out": 0
            },
            "outcomes": [{
                "schema": "homeboy/agent-task-outcome/v1",
                "task_id": "cook",
                "status": "succeeded",
                "summary": "patch ready",
                "failure_classification": null,
                "artifacts": [{
                    "schema": "homeboy/agent-task-artifact/v1",
                    "id": "durable-patch",
                    "kind": "patch",
                    "size_bytes": 32_318,
                    "metadata": { "changed_file_count": 7 }
                }],
                "evidence_refs": [],
                "metadata": {},
                "outputs": {}
            }]
        }))
        .expect("durable aggregate");

        let mut compact = compact_status_summary_with_aggregate(
            &record,
            "agent-task-durable-patch",
            Some(&aggregate),
        );
        assert_eq!(
            compact["canonical_candidate"]["schema"],
            "homeboy/agent-task-candidate/v1"
        );
        assert_eq!(compact["canonical_candidate"]["state"], "patch_available");
        assert_eq!(compact["canonical_candidate"]["diff_bytes"], 32_318);
        assert_eq!(compact["canonical_candidate"]["scan"]["degraded"], false);
        assert!(compact.get("aggregate").is_none());

        attach_agent_task_status_actionable(&mut compact, "agent-task-durable-patch");
        let rendered = crate::commands::agent_task_summary::render_agent_task_summary(
            crate::commands::agent_task_summary::AgentTaskSummaryKind::Status,
            &compact,
        )
        .expect("status summary");

        assert!(rendered.contains("Candidate state: patch_available"));
        assert!(rendered.contains("Patch candidates: 1 non-empty / 0 empty"));
        assert!(!rendered.contains("Patch candidates: 1 non-empty / 0 empty /"));
        assert!(rendered.contains("Diff bytes: 32318"));
        assert!(rendered.contains("Changed files: unknown"));
        assert!(rendered.contains("Next: homeboy agent-task review agent-task-durable-patch"));
        assert!(compact[ACTIONABLE_METADATA_KEY]["next_actions"]
            .as_array()
            .expect("next actions")
            .iter()
            .any(
                |action| action["command"] == "homeboy agent-task review agent-task-durable-patch"
            ));

        let mut full = record.clone();
        attach_full_status_candidate(&mut full, Some(&aggregate), "agent-task-durable-patch");
        attach_agent_task_status_actionable(&mut full, "agent-task-durable-patch");
        let full_rendered = crate::commands::agent_task_summary::render_agent_task_summary(
            crate::commands::agent_task_summary::AgentTaskSummaryKind::Status,
            &full,
        )
        .expect("full status summary");

        assert_eq!(
            full["aggregate"]["outcomes"][0]["artifacts"][0]["size_bytes"],
            32_318
        );
        assert_eq!(full["canonical_candidate"], compact["canonical_candidate"]);
        assert_eq!(
            compact["liveness"]["next_action"],
            "homeboy agent-task review agent-task-durable-patch"
        );
        assert_eq!(full["liveness"], compact["liveness"]);
        assert!(full_rendered.contains("Status: succeeded"));
        assert!(full_rendered.contains("Patch candidates: 1 non-empty / 0 empty"));
        assert!(full_rendered.contains("Changed files: 7"));
        assert!(!full_rendered.contains("no_patch_produced"));
        assert!(full_rendered.contains("Next: homeboy agent-task review agent-task-durable-patch"));
        assert!(full[ACTIONABLE_METADATA_KEY]["next_actions"]
            .as_array()
            .expect("full next actions")
            .iter()
            .any(
                |action| action["command"] == "homeboy agent-task review agent-task-durable-patch"
            ));
    }

    #[test]
    fn local_cook_surfaces_in_flight_provider_execution() {
        // A local cook has no provider handles and no runner job id, but
        // `reserve_provider_execution` durably records the running attempt
        // before the backend blocks. The liveness projection must report the
        // provider boundary as recorded (not absent) and expose the running
        // execution's backend, model, and start time so operators can tell an
        // active provider from a hung preflight (#8396).
        let local_running = compact_status_summary(
            &json!({
                "run_id": "agent-task-local-1",
                "state": "running",
                "tasks": [],
                "provider_handles": [],
                "lifecycle": { "heartbeat": { "last_seen_at": "2026-07-16T00:00:00Z" } },
                "metadata": {
                    "provider_executions_consumed": 1,
                    "provider_executions": [{
                        "key": "cook:1",
                        "task_id": "cook",
                        "attempt": 1,
                        "backend": "opencode",
                        "model": "openai/gpt-5.6-sol",
                        "state": "running",
                        "started_at": "2026-07-16T00:00:00Z"
                    }]
                }
            }),
            "agent-task-local-1",
        );
        let boundary = &local_running["liveness"]["provider_boundary"];
        assert_eq!(boundary["status"], "recorded");
        assert_eq!(boundary["provider_handle_count"], 0);
        assert!(boundary["runner_job_id"].is_null());
        assert_eq!(boundary["active_execution_count"], 1);
        assert_eq!(boundary["active_executions"][0]["backend"], "opencode");
        assert_eq!(
            boundary["active_executions"][0]["model"],
            "openai/gpt-5.6-sol"
        );
        assert_eq!(boundary["active_executions"][0]["task_id"], "cook");
        assert_eq!(
            boundary["active_executions"][0]["started_at"],
            "2026-07-16T00:00:00Z"
        );
        assert_eq!(local_running["execution_location"], "local");
    }

    #[test]
    fn terminal_provider_execution_is_not_surfaced_as_active() {
        // Once the provider execution reaches a terminal state, it must no
        // longer count as in-flight. A local run with only completed executions
        // and no other liveness evidence reports the boundary as absent (#8396).
        let completed = compact_status_summary(
            &json!({
                "run_id": "agent-task-local-2",
                "state": "running",
                "tasks": [],
                "provider_handles": [],
                "metadata": {
                    "provider_executions": [{
                        "key": "cook:1",
                        "task_id": "cook",
                        "attempt": 1,
                        "backend": "opencode",
                        "state": "succeeded",
                        "started_at": "2026-07-16T00:00:00Z",
                        "finished_at": "2026-07-16T00:03:00Z"
                    }]
                }
            }),
            "agent-task-local-2",
        );
        let boundary = &completed["liveness"]["provider_boundary"];
        assert_eq!(boundary["status"], "absent");
        assert_eq!(boundary["active_execution_count"], 0);
        assert!(boundary["active_executions"].as_array().unwrap().is_empty());
    }

    #[test]
    fn compact_status_retains_recovered_patch_refs_for_summary_rendering() {
        let record = json!({
            "run_id": "recovered-lab-run",
            "state": "succeeded",
            "tasks": [{ "task_id": "cook", "state": "succeeded" }],
            "artifact_refs": [{
                "task_id": "cook",
                "kind": "patch",
                "uri": "homeboy://agent-task/run/recovered-lab-run/artifacts#task=cook&artifact=patch",
                "size_bytes": 7926
            }]
        });

        let summary = compact_status_summary(&record, "recovered-lab-run");

        assert_eq!(summary["artifact_refs"][0]["kind"], "patch");
        assert!(summary["artifact_refs"][0].get("size_bytes").is_none());
    }

    #[test]
    fn compact_cook_report_is_bounded_and_full_remains_retrievable() {
        let attempts = (0..(COMPACT_TASK_LIMIT + 3))
            .map(|attempt| {
                json!({
                    "attempt": attempt,
                    "run_id": format!("run-{attempt}"),
                    "run_state": "failed",
                    "promotion": { "nested_evidence": "x".repeat(COMPACT_TEXT_LIMIT + 1) }
                })
            })
            .collect::<Vec<_>>();
        let report = json!({
            "schema": "homeboy/agent-task-cook/v1",
            "cook_id": "cook-1",
            "latest_run_id": "run-14",
            "status": "failed",
            "stop_reason": "x".repeat(COMPACT_TEXT_LIMIT + 1),
            "attempts": attempts,
            "finalization": { "status": "blocked", "nested_evidence": { "large": "x".repeat(COMPACT_TEXT_LIMIT + 1) } }
        });

        let compact = compact_cook_report(report.clone(), false);
        assert_eq!(compact["schema"], report["schema"]);
        assert_eq!(compact["cook_id"], "cook-1");
        assert_eq!(
            compact["attempts"].as_array().unwrap().len(),
            COMPACT_TASK_LIMIT
        );
        assert_eq!(compact["attempts_omitted"], 3);
        assert!(compact["attempts"][0].get("promotion").is_none());
        assert_eq!(
            compact["full_command"],
            "homeboy agent-task status run-14 --full"
        );
        assert_eq!(compact_cook_report(report.clone(), true), report);
    }

    #[test]
    fn compact_work_summary_separates_no_patch_promotion_from_provider_artifacts() {
        let record = json!({
            "run_id": "agent-task-run-artifacts",
            "state": "succeeded",
            "tasks": [],
            "artifact_refs": [{
                "task_id": "task-a",
                "kind": "provider-transcript",
                "uri": "file:///tmp/provider-transcript.json"
            }],
            "metadata": {
                "latest_promotion": {
                    "status": "no_changes",
                    "operator_notification": { "status": "completed" }
                }
            }
        });

        let summary = compact_status_summary(&record, "agent-task-run-artifacts");

        assert_eq!(
            summary["work_summary"]["provider_execution_status"],
            "succeeded"
        );
        assert_eq!(summary["work_summary"]["promotion_status"], "no_changes");
        assert_eq!(
            summary["work_summary"]["classification"],
            "provider_completed_artifacts_pending_review"
        );
        assert_eq!(summary["work_summary"]["artifact_ref_count"], 1);
        assert_eq!(
            summary["refs"][0]["uri"],
            "file:///tmp/provider-transcript.json"
        );
    }

    #[test]
    fn compact_work_summary_classifies_provider_commits_pending_promotion() {
        let record = json!({
            "run_id": "agent-task-run-commits",
            "state": "succeeded",
            "tasks": [],
            "metadata": {
                "latest_promotion": {
                    "status": "no_patch_produced",
                    "changed_files": ["src/lib.rs"],
                    "operator_notification": { "status": "completed" }
                }
            }
        });

        let summary = compact_status_summary(&record, "agent-task-run-commits");

        assert_eq!(
            summary["work_summary"]["classification"],
            "committed_changes_pending_promotion"
        );
        assert_eq!(summary["work_summary"]["committed_changes_detected"], true);
    }

    #[test]
    fn compact_work_summary_preserves_true_no_changes() {
        let record = json!({
            "run_id": "agent-task-run-empty",
            "state": "succeeded",
            "tasks": [],
            "metadata": {
                "latest_promotion": {
                    "status": "no_changes",
                    "operator_notification": { "status": "completed" }
                }
            }
        });

        let summary = compact_status_summary(&record, "agent-task-run-empty");

        assert_eq!(summary["work_summary"]["classification"], "no_changes");
        assert_eq!(summary["work_summary"]["artifact_ref_count"], 0);
        assert_eq!(summary["work_summary"]["evidence_ref_count"], 0);
    }

    #[test]
    fn work_summary_distinguishes_available_promoted_and_finalized_candidates() {
        let available = work_summary(&json!({ "state": "succeeded" }), None, &[]);
        assert_eq!(available["classification"], "no_changes");

        let promoted = work_summary(
            &json!({
                "state": "succeeded",
                "metadata": { "latest_promotion": {
                    "status": "applied", "patch_artifact": { "id": "patch" }
                }}
            }),
            None,
            &[],
        );
        assert_eq!(promoted["candidate_state"], "promoted");
        assert_eq!(promoted["classification"], "promoted_changes");

        let finalized = work_summary(
            &json!({
                "state": "succeeded",
                "metadata": { "cook_finalization": {
                    "status": "review_ready", "pr_url": "https://example.test/pull/1"
                }}
            }),
            None,
            &[],
        );
        assert_eq!(finalized["candidate_state"], "finalized");
        assert_eq!(finalized["classification"], "pull_request_finalized");
    }

    #[test]
    fn aggregate_artifacts_are_counted_when_record_refs_are_empty() {
        let record = json!({
            "run_id": "agent-task-run-aggregate-refs",
            "state": "succeeded",
            "tasks": []
        });
        let aggregate: AgentTaskAggregate = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": "plan-a",
            "status": "succeeded",
            "totals": {
                "queued": 0,
                "running": 0,
                "blocked": 0,
                "skipped": 0,
                "succeeded": 1,
                "failed": 0,
                "cancelled": 0,
                "timed_out": 0
            },
            "outcomes": [{
                "schema": "homeboy/agent-task-outcome/v1",
                "task_id": "task-a",
                "status": "succeeded",
                "summary": "ok",
                "failure_classification": null,
                "artifacts": [{
                    "id": "patch.diff",
                    "kind": "patch",
                    "path": "/tmp/patch.diff",
                    "metadata": null
                }],
                "typed_artifacts": [],
                "evidence_refs": [{
                    "kind": "executor-result",
                    "uri": "file:///tmp/executor-result.json"
                }],
                "diagnostics": [],
                "outputs": null,
                "workflow": null,
                "follow_up": null,
                "metadata": null
            }]
        }))
        .expect("aggregate");
        let refs = ref_inventory(&record, Some(&aggregate));
        let summary = work_summary(&record, Some(&aggregate), &refs);
        let (compact_refs, _) = compact_refs(&refs);

        assert_eq!(summary["artifact_ref_count"], 1);
        assert_eq!(summary["evidence_ref_count"], 1);
        assert_eq!(
            summary["classification"],
            "provider_completed_artifacts_available"
        );
        assert_eq!(compact_refs.as_array().expect("refs").len(), 2);
    }
}
