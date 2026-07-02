//! Read-side handlers: status, logs, artifacts, list/active/latest, and cancel.
//!
//! `status` returns a compact, recovery-first summary by default (#4396):
//! run id, state, totals, a per-task source table (#4392), deduped patch/changed
//! references, and a prominent risk-flag section (#4398). The full verbose
//! payload is available behind `--full`.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use homeboy::core::agent_task_service as agent_task_service_direct;
use homeboy::core::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::core::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskPlan};
use homeboy::core::agent_tasks::service as agent_task_service;
use homeboy::core::agent_tasks::{AgentTaskEvidenceRef, AgentTaskOutcomeStatus};

use super::super::CmdResult;
use super::args::{CancelArgs, DiagnoseArgs, EvidenceArgs, ReplayProviderBoundaryArgs, StatusArgs};

/// Cap the number of detail refs rendered in the compact summary so a noisy
/// aggregate cannot flood recovery output. Overflow is reported as an
/// `omitted` count rather than dropped silently.
const COMPACT_REF_LIMIT: usize = 12;

pub(super) fn status(args: StatusArgs) -> CmdResult<Value> {
    if args.bridge {
        let bridge_status = agent_task_service::run_status(&args.run_id, args.since_cursor)?;
        return Ok((
            serde_json::to_value(bridge_status).unwrap_or(Value::Null),
            0,
        ));
    }

    let record = match agent_task_service::status(&args.run_id) {
        Ok(record) => record,
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
    let mut value = serde_json::to_value(&record).unwrap_or(Value::Null);
    enrich_with_diagnostic_summary(&mut value, &args.run_id)?;
    if args.full {
        return Ok((value, 0));
    }
    let summary = compact_status_summary(&value, &args.run_id);
    Ok((summary, 0))
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
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
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
            json!("run `homeboy agent-task active --reconcile` (or the per-run `commands.reconcile`) to safely cancel stale-running records without manual state edits"),
        );
    }
    Ok((value, 0))
}

/// `agent-task active --reconcile`: safely cancel stale/suspect/unreconciled
/// running records through the lifecycle cancel path. With `dry_run`, the
/// candidates are reported but no record is mutated (#5682).
pub(super) fn reconcile_active(dry_run: bool) -> CmdResult<Value> {
    let report = agent_task_service_direct::reconcile_stale_active_runs(dry_run)?;
    let exit = if report.failed > 0 { 1 } else { 0 };
    Ok((serde_json::to_value(report).unwrap_or(Value::Null), exit))
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

pub(super) fn logs(args: StatusArgs) -> CmdResult<Value> {
    let log = agent_task_service::logs(&args.run_id)?;
    let mut value = serde_json::to_value(log).unwrap_or(Value::Null);
    enrich_with_diagnostic_summary(&mut value, &args.run_id)?;
    Ok((value, 0))
}

pub(super) fn artifacts(args: StatusArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    Ok((serde_json::to_value(artifacts).unwrap_or(Value::Null), 0))
}

pub(super) fn evidence(args: EvidenceArgs) -> CmdResult<Value> {
    let artifacts = agent_task_service::artifacts(&args.run_id)?;
    let aggregate = completed_run_aggregate(&args.run_id).transpose()?;
    let failed_tasks = failed_task_statuses(aggregate.as_ref());
    let plan = agent_task_lifecycle::load_plan(&args.run_id).ok();

    let mut hydrated = Vec::new();
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

        hydrated.push(agent_task_service::hydrate_evidence_ref(
            &args.run_id,
            &evidence_ref,
            task_id.as_deref(),
            plan.as_ref(),
            aggregate.as_ref(),
        ));
    }

    Ok((
        serde_json::to_value(AgentTaskEvidenceReport {
            schema: "homeboy/agent-task-evidence/v1",
            run_id: args.run_id,
            filters: AgentTaskEvidenceFilters {
                kind: args.kind,
                task: args.task,
                failure_only: args.failure_only,
            },
            count: hydrated.len(),
            evidence: hydrated,
        })
        .unwrap_or(Value::Null),
        0,
    ))
}

pub(super) fn diagnose(args: DiagnoseArgs) -> CmdResult<Value> {
    let record = agent_task_service::status(&args.run_id)?;
    let aggregate = completed_run_aggregate(&args.run_id).transpose()?;
    let mut hydrated_evidence = Vec::new();
    let mut nested_reasons = Vec::new();

    if let Some(aggregate) = aggregate.as_ref() {
        for outcome in &aggregate.outcomes {
            for evidence in &outcome.evidence_refs {
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

    Ok((
        json!({
            "schema": "homeboy/agent-task-diagnose/v1",
            "run_id": record.run_id,
            "state": record.state,
            "root_cause": root_cause,
            "causal_chain": causal_chain,
            "missing_artifacts": missing_artifacts,
            "hydrated_evidence": hydrated_evidence,
            "next_commands": next_commands,
        }),
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
    let selected_index = candidates
        .iter()
        .position(|(evidence_ref, _)| !evidence_ref.uri.contains("/plan#"))
        .unwrap_or(0);
    let Some((evidence_ref, task_id)) = candidates.into_iter().nth(selected_index) else {
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

    let hydrated = agent_task_service::hydrate_evidence_ref(
        &args.run_id,
        &evidence_ref,
        task_id.as_deref(),
        plan.as_ref(),
        aggregate.as_ref(),
    );
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
    let executor_config = input
        .get("executor")
        .and_then(|executor| executor.get("config"))
        .cloned()
        .unwrap_or(Value::Null);
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
        "runtime_component_paths": input.pointer("/executor/config/runtime_component_paths").cloned().unwrap_or(Value::Null),
        "runtime_env": input.pointer("/executor/config/runtime_env").cloned().unwrap_or(Value::Null),
        "artifact_declarations": input.get("artifact_declarations").cloned().unwrap_or(Value::Null),
        "package_descriptor": package_descriptor,
    })
}

#[derive(Serialize)]
struct AgentTaskEvidenceReport {
    schema: &'static str,
    run_id: String,
    filters: AgentTaskEvidenceFilters,
    count: usize,
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

    // Prefer failed/errored outcomes, but fall back to scanning every outcome so
    // a root cause attached to a non-failed cell is still surfaced.
    let failed_first = aggregate.outcomes.iter().filter(|outcome| {
        matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Failed
                | AgentTaskOutcomeStatus::ProviderError
                | AgentTaskOutcomeStatus::Timeout
                | AgentTaskOutcomeStatus::UnableToRemediate
        )
    });
    let any_failed = failed_first.clone().next().is_some();

    let scan: Vec<&homeboy::core::agent_tasks::AgentTaskOutcome> = if any_failed {
        failed_first.collect()
    } else {
        aggregate.outcomes.iter().collect()
    };

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
    let plan = agent_task_lifecycle::load_plan(run_id).ok();
    let aggregate = completed_run_aggregate(run_id).and_then(Result::ok);
    let task_table = task_source_table(record, plan.as_ref());
    let ref_inventory = ref_inventory(record, aggregate.as_ref());
    let (refs, refs_omitted) = compact_refs(&ref_inventory);
    let risk_flags = risk_flags(record);
    let work_summary = work_summary(record, aggregate.as_ref(), &ref_inventory);

    let mut summary = json!({
        "schema": "homeboy/agent-task-status-summary/v1",
        "run_id": record.get("run_id").cloned().unwrap_or_else(|| json!(run_id)),
        "state": record.get("state").cloned().unwrap_or(Value::Null),
        "work_summary": work_summary,
        "totals": record.get("totals").cloned().unwrap_or(Value::Null),
        "tasks": task_table,
        "refs": refs,
        "refs_omitted": refs_omitted,
        "risk_flags": risk_flags,
        "execution_location": execution_location(record),
        "queue_visibility": queue_visibility(record),
        "full_command": format!("homeboy agent-task status {run_id} --full"),
    });

    if let Some(diagnostic) = record.get("diagnostic_summary") {
        if !diagnostic.is_null() {
            summary["diagnostic_summary"] = diagnostic.clone();
        }
    }
    if let Some(failure_reasons) = record.get("failure_reasons") {
        if failure_reasons
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty())
        {
            summary["failure_reasons"] = failure_reasons.clone();
        }
    }
    if let Some(aggregate_path) = record.get("aggregate_path") {
        if !aggregate_path.is_null() {
            summary["aggregate_path"] = aggregate_path.clone();
        }
    }
    if let Some(latest_promotion) = record
        .get("metadata")
        .and_then(|metadata| metadata.get("latest_promotion"))
    {
        if !latest_promotion.is_null() {
            summary["latest_promotion"] = latest_promotion.clone();
        }
    }
    summary
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
    let classification = work_classification(
        record,
        promotion_status,
        artifact_ref_count,
        evidence_ref_count,
        committed_changes,
    );

    json!({
        "classification": classification,
        "provider_execution_status": provider_status,
        "promotion_status": promotion_status,
        "artifact_ref_count": artifact_ref_count,
        "evidence_ref_count": evidence_ref_count,
        "committed_changes_detected": committed_changes,
        "artifact_command": record.get("run_id").and_then(Value::as_str).map(|run_id| format!("homeboy agent-task artifacts {run_id}")),
    })
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
) -> &'static str {
    if committed_changes && promotion_status.is_some_and(is_no_change_promotion_status) {
        return "committed_changes_pending_promotion";
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

        assert_eq!(
            summary["latest_promotion"]["patch_artifact_id"],
            "patch.diff"
        );
        assert_eq!(
            summary["latest_promotion"]["operator_notification"]["status"],
            "completed"
        );
        assert_eq!(
            summary["queue_visibility"]["commands"][0],
            "homeboy agent-task list"
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
