//! Observation-store handlers for the `runs` subcommands.
//!
//! These functions own the local observation-store reads and the mirrored
//! runner-job composition that back `runs list/show/resume-plan/artifacts` and
//! the `runs artifact` retrieval/cleanup subcommands.

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::Value;

use homeboy::core::activity::{self, ActivityFilter, ActivityOptions, ActivityScope};
use homeboy::core::api_jobs;
use homeboy::core::artifact_address::ArtifactAddress;
use homeboy::core::observation::evidence_report::directory_publication_guidance;
use homeboy::core::observation::runs_service;
use homeboy::core::observation::{
    merge_metadata, ArtifactListFilter, FindingListFilter, ObservationStore, RunCursor,
    RunListFilter, RunRecord, RunStatus, MAX_RUN_PAGE_LIMIT,
};
use homeboy::core::resource_lifecycle_index::resource_lifecycle_index_from_artifacts;
use homeboy::core::validation_progress::ValidationProgressLedger;
use homeboy::core::Error;
use homeboy::runner::readonly_probe;
use homeboy::runner::runners as runner;

use super::bench::run_contains_scenario;
use super::common::{run_summaries_with_artifact_indexes, RunSummary};
use super::types::{
    actionable_for_run_detail, actionable_for_run_list, RunDetail, RunsArtifactArgs,
    RunsArtifactCommand, RunsArtifactCommandHint, RunsArtifactGetArgs, RunsArtifactGetHandleArgs,
    RunsArtifactGetOutput, RunsArtifactPage, RunsArtifactPathGuide, RunsArtifactPreviewHandleArgs,
    RunsArtifactPullEntry, RunsArtifactPullSummary, RunsArtifactsArgs, RunsArtifactsOutput,
    RunsCancelOutput, RunsDirectoryArtifactPublicationGuidance, RunsEnvKeyOutput, RunsEnvOutput,
    RunsEnvSourceLayerOutput, RunsEnvSummary, RunsFieldSelectionOutput, RunsListArgs,
    RunsListOutput, RunsOutput, RunsResumePlanOutput, RunsSelectedField, RunsShowOutput,
    RunsStaleRunSummary,
};
use super::{reconcile, remote, remote_artifact, CmdResult};

#[cfg(test)]
/// The observation store the enclosing isolated home installs.
///
/// At file scope so every `#[cfg(test)]` module here reaches it the same way.
/// A test is the entry point for its own unit of work, so opening once here is
/// a boundary open, not an ambient one inside production code (#7505).
fn test_store() -> homeboy::core::observation::ObservationStore {
    homeboy::core::observation::ObservationStore::open_initialized().expect("observation store")
}

pub fn list_runs(
    store: &ObservationStore,
    args: RunsListArgs,
    command: &'static str,
) -> CmdResult<RunsOutput> {
    if args.active {
        return list_active_runs(args);
    }
    validate_list_search_args(&args)?;
    if let Some(runner_id) = args.runner.clone() {
        return remote::list_runner_runs(&runner_id, args, command);
    }

    // `--running` is shorthand for `--status running`; the two are mutually
    // exclusive at the CLI layer so this never overrides an explicit status.
    let status = if args.running {
        Some("running".to_string())
    } else {
        args.status.clone()
    };
    let status_filter = status.clone();

    // Resolve time-window bounds up front so a bad `--since/--until` fails
    // before any store read.
    let since = args.since.as_deref().map(resolve_time_bound).transpose()?;
    let until = args.until.as_deref().map(resolve_time_bound).transpose()?;

    let (run_records, search) =
        list_runs_for_discovery(&store, &args, status, since.as_deref(), until.as_deref())?;

    // Collapse runner-execution mirrors into one canonical row per logical
    // execution unless the caller explicitly wants every underlying row.
    let (run_records, hidden_mirrors) = if args.include_mirrors {
        (run_records, 0)
    } else {
        let deduped = runs_service::dedupe_runner_execution_mirrors(run_records);
        (deduped.canonical, deduped.hidden_mirrors)
    };

    // Apply the caller's limit to the canonical, post-filter set.
    let limit = args.limit.max(0) as usize;
    let run_records = run_records.into_iter().take(limit).collect::<Vec<_>>();

    let active_runner_jobs = if args.include_active_runner_jobs {
        active_runner_job_summaries(status_filter.as_deref())
    } else {
        ActiveRunnerJobEnrichment::default()
    };
    let stale_runs = stale_run_summary(
        &run_records,
        &active_runner_jobs.durable_run_ids,
        active_runner_jobs.complete,
    );

    let mut runs = run_summaries_with_artifact_indexes(&store, run_records)?;
    runs.extend(active_runner_jobs.runs);
    let runner_enrichment = active_runner_jobs.state;

    let matched_runs = runs.len();
    let actionable = actionable_for_run_list(&runs, stale_runs.as_ref());
    Ok((
        RunsOutput::List(RunsListOutput {
            command,
            runs,
            matched_runs,
            hidden_mirrors,
            // Carry the reason for any bounded probe that did not answer, so an
            // empty/short active-job list is never mistaken for an idle Lab.
            probe_degradations: readonly_probe::take_degradations(),
            runner_enrichment,
            stale_runs,
            search,
            actionable,
        }),
        0,
    ))
}

fn list_active_runs(args: RunsListArgs) -> CmdResult<RunsOutput> {
    let filter = ActivityFilter {
        task_url: args.task_url,
        repository: args.repo,
        worktree: args.workspace,
    };
    let report = activity::activity_report_filtered(
        ActivityScope::ActiveRecent,
        args.limit.max(1) as usize,
        ActivityOptions::default(),
        &filter,
        "runs.list_active",
    )?;
    Ok((RunsOutput::Active(Box::new(report)), 0))
}

pub fn cancel_run(store: &ObservationStore, run_id: &str) -> CmdResult<RunsOutput> {
    let run = runs_service::require_run(&store, run_id)?;
    if run.status != RunStatus::Running.as_str() {
        return Err(Error::validation_invalid_argument(
            "run-id",
            "only running observation runs can be cancelled",
            Some(run_id.to_string()),
            None,
        ));
    }
    let metadata = merge_metadata(
        run.metadata_json,
        serde_json::json!({
            "cancellation": { "requested": true, "requested_at": chrono::Utc::now().to_rfc3339() }
        }),
    );
    let cancelled = store
        .finish_running_run(run_id, RunStatus::Skipped, Some(metadata))?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "run-id",
                "run completed before cancellation could be recorded",
                Some(run_id.to_string()),
                None,
            )
        })?;
    let actionable =
        super::types::actionable_for_run_summary(&super::run_summary(cancelled.clone()));
    Ok((
        RunsOutput::Cancel(RunsCancelOutput {
            command: "runs.cancel",
            run_id: cancelled.id.clone(),
            status: cancelled.status,
            cancellation: "cooperative; the foreground owner stops before its next stage",
            actionable,
        }),
        0,
    ))
}

/// Hard cap for non-indexed provenance discovery. A zero result at this boundary
/// is explicitly labelled partial rather than incorrectly reported as absence.
pub(super) const DISCOVERY_SCAN_ROW_LIMIT: usize = 5_000;

fn list_runs_for_discovery(
    store: &ObservationStore,
    args: &RunsListArgs,
    status: Option<String>,
    since: Option<&str>,
    until: Option<&str>,
) -> homeboy::core::Result<(Vec<RunRecord>, super::types::RunsListSearch)> {
    let mut runs = Vec::new();
    let mut cursor: Option<RunCursor> = None;
    let mut scanned_rows = 0;
    let needs_post_filter = args.scenario_id.is_some()
        || args.since.is_some()
        || args.until.is_some()
        || args.id.is_some()
        || args.command_contains.is_some()
        || args.workspace.is_some()
        || args.correlation.is_some();
    let page_size = if needs_post_filter {
        MAX_RUN_PAGE_LIMIT
    } else {
        args.limit.clamp(1, MAX_RUN_PAGE_LIMIT)
    };

    loop {
        let remaining = DISCOVERY_SCAN_ROW_LIMIT.saturating_sub(scanned_rows);
        if remaining == 0 {
            break;
        }
        let page = store.list_runs_page(RunListFilter {
            kind: args.kind.clone(),
            component_id: args.component_id.clone(),
            status: status.clone(),
            rig_id: args.rig.clone(),
            limit: Some((remaining as i64).min(page_size)),
            after: cursor,
            ..RunListFilter::default()
        })?;
        scanned_rows += page.runs.len();
        runs.extend(page.runs.into_iter().filter(|run| {
            args.scenario_id
                .as_deref()
                .is_none_or(|scenario| run_contains_scenario(run, scenario))
                && run_matches_list_filters(run, args, since, until)
        }));
        if !needs_post_filter || !page.truncated {
            return Ok((runs, super::types::RunsListSearch::complete(scanned_rows)));
        }
        cursor = page.next_cursor;
    }

    Ok((runs, super::types::RunsListSearch::bounded(scanned_rows)))
}

pub(super) fn validate_list_search_args(args: &RunsListArgs) -> homeboy::core::Result<()> {
    for (name, value) in [
        ("id", args.id.as_deref()),
        ("command-contains", args.command_contains.as_deref()),
        ("workspace", args.workspace.as_deref()),
        ("correlation", args.correlation.as_deref()),
    ] {
        if value.is_some_and(|value| value.trim().is_empty()) {
            return Err(Error::validation_invalid_argument(
                name,
                "search fragments must contain at least one non-whitespace character",
                value.map(str::to_string),
                None,
            ));
        }
    }
    Ok(())
}

/// Resolve a `--since`/`--until` bound into an RFC-3339 timestamp. Accepts an
/// absolute RFC-3339 timestamp (used verbatim) or a relative age like `2d` /
/// `6h` / `30m` (resolved to `now - age`).
pub(super) fn resolve_time_bound(raw: &str) -> homeboy::core::Result<String> {
    let trimmed = raw.trim();
    if chrono::DateTime::parse_from_rfc3339(trimmed).is_ok() {
        return Ok(trimmed.to_string());
    }
    super::common::since_threshold(trimmed)
}

/// Post-store filters that operate on fields not indexed by `RunListFilter`.
/// Agent-task lifecycle state is already copied into the canonical observation
/// record; search its durable provenance rather than requiring a second store.
fn run_matches_list_filters(
    run: &RunRecord,
    args: &RunsListArgs,
    since: Option<&str>,
    until: Option<&str>,
) -> bool {
    // Time window: string comparison is correct for RFC-3339 timestamps in the
    // same offset; the store persists UTC `started_at`.
    if let Some(since) = since {
        if run.started_at.as_str() < since {
            return false;
        }
    }
    if let Some(until) = until {
        if run.started_at.as_str() > until {
            return false;
        }
    }
    if let Some(fragment) = args.id.as_deref() {
        if !run_id_or_label_contains(run, fragment) {
            return false;
        }
    }
    if let Some(needle) = args.command_contains.as_deref() {
        if !run_command_contains(run, needle) {
            return false;
        }
    }
    if let Some(workspace) = args.workspace.as_deref() {
        if !run_workspace_contains(run, workspace) {
            return false;
        }
    }
    if let Some(correlation) = args.correlation.as_deref() {
        if !run_correlates_with(run, correlation) {
            return false;
        }
    }
    true
}

/// True when the run's persisted id or run-label contains `fragment`.
fn run_id_or_label_contains(run: &RunRecord, fragment: &str) -> bool {
    if run.id.contains(fragment) {
        return true;
    }
    run.command
        .as_deref()
        .and_then(runs_service::command_run_id_label)
        .is_some_and(|label| label.contains(fragment))
        || metadata_string_values(
            run,
            &[
                "/agent_task_run/run_id",
                "/agent_task_run/plan_id",
                "/agent_task_aggregate/plan_id",
                "/agent_task_run/metadata/runner_id",
                "/agent_task_run/metadata/runner_job_id",
            ],
        )
        .iter()
        .any(|value| value.contains(fragment))
}

fn run_command_contains(run: &RunRecord, needle: &str) -> bool {
    run.command
        .as_deref()
        .is_some_and(|command| command.contains(needle))
        || metadata_string_values(
            run,
            &[
                "/agent_task_run/metadata/remote_command",
                "/agent_task_run/metadata/local_command",
                "/agent_task_run/metadata/command",
                "/remote_command",
            ],
        )
        .iter()
        .any(|value| value.contains(needle))
}

fn run_workspace_contains(run: &RunRecord, needle: &str) -> bool {
    run.cwd
        .as_deref()
        .is_some_and(|cwd| workspace_matches(cwd, needle))
        || metadata_string_values(
            run,
            &[
                "/agent_task_run/workspace_identity/locator",
                "/agent_task_run/workspace_claim/workspace/locator",
                "/agent_task_run/workspace_owner_lease/workspace/locator",
                "/agent_task_run/metadata/remote_workspace",
                "/remote_workspace",
                "/lab/remote_workspace",
            ],
        )
        .iter()
        .any(|value| workspace_matches(value, needle))
}

/// Read only explicitly named scalar provenance values. Agent-task metadata can
/// retain diagnostics and secret-redacted environment records; recursively
/// flattening it turns `runs list` into both a false-match source and a secret
/// presence oracle.
fn metadata_string_values(run: &RunRecord, pointers: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    for pointer in pointers {
        if let Some(value) = run.metadata_json.pointer(pointer) {
            collect_metadata_string_or_array(value, &mut values);
        }
    }
    values
}

fn collect_metadata_string_or_array(value: &Value, values: &mut Vec<String>) {
    match value {
        Value::String(value) => values.push(value.clone()),
        Value::Array(items) => {
            for item in items {
                if let Value::String(value) = item {
                    values.push(value.clone());
                }
            }
        }
        _ => {}
    }
}

/// True when the run shares the `correlation` fragment across its id, run-label,
/// or durable Lab lineage (runner id or job id) — so a controller run, its
/// runner job, and mirrored observation rows resolve together.
fn run_correlates_with(run: &RunRecord, correlation: &str) -> bool {
    if run_id_or_label_contains(run, correlation) {
        return true;
    }
    if let Some((runner_id, job_id)) = runs_service::lab_run_lineage(run) {
        if runner_id.contains(correlation) || job_id.contains(correlation) {
            return true;
        }
    }
    metadata_string_values(
        run,
        &[
            "/agent_task_run/plan_id",
            "/agent_task_aggregate/plan_id",
            "/agent_task_run/metadata/runner_id",
            "/agent_task_run/metadata/runner_job_id",
        ],
    )
    .iter()
    .any(|value| value.contains(correlation))
        || metadata_array_field_values(
            run,
            "/agent_task_run/provider_handles",
            &["provider_run_id", "session_id"],
        )
        .iter()
        .any(|value| value.contains(correlation))
}

fn metadata_array_field_values(run: &RunRecord, pointer: &str, fields: &[&str]) -> Vec<String> {
    run.metadata_json
        .pointer(pointer)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .flat_map(|item| {
            fields
                .iter()
                .filter_map(move |field| item.get(*field).and_then(Value::as_str))
        })
        .map(str::to_string)
        .collect()
}

pub(super) fn workspace_matches(candidate: &str, query: &str) -> bool {
    let candidate = normalize_workspace_locator(candidate);
    let query = normalize_workspace_locator(query);
    candidate == query
        || candidate
            .strip_suffix(&query)
            .is_some_and(|prefix| prefix.ends_with('/'))
}

fn normalize_workspace_locator(value: &str) -> String {
    let absolute = value.starts_with(['/', '\\']);
    let mut parts = Vec::new();
    let normalized_separators = value.replace('\\', "/");
    for part in normalized_separators.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|part| *part != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push(part);
                }
            }
            part => parts.push(part),
        }
    }
    format!("{}{}", if absolute { "/" } else { "" }, parts.join("/"))
}

/// Active runner jobs for `runs list --include-active-runner-jobs`.
///
/// Deliberately uses the latency-bounded indexed snapshot rather than the full
/// `statuses()` report (#10418). `statuses()` reconciles the daemon generation
/// ledger and issues unbounded remote identity probes — expensive
/// *reconciliation* that a read-only listing must never block on. The indexed
/// snapshot answers the only question this listing asks ("what is running right
/// now") with one bounded `/jobs` query per runner.
#[derive(Default)]
struct ActiveRunnerJobEnrichment {
    runs: Vec<RunSummary>,
    state: Option<super::types::RunsRunnerEnrichment>,
    durable_run_ids: HashSet<String>,
    complete: bool,
}

fn active_runner_job_summaries(status: Option<&str>) -> ActiveRunnerJobEnrichment {
    // This view deliberately avoids `runner::status()`: that path reconciles
    // every draining generation and can block local run discovery behind a
    // wedged runner. The indexed snapshot makes one bounded current-session
    // request per runner instead of scanning its historical generations.
    active_runner_job_summaries_from_snapshots(runner::statuses_indexed(), status)
}

fn active_runner_job_summaries_from_snapshots(
    snapshots: homeboy::core::Result<Vec<runner::RunnerActiveJobsSnapshot>>,
    status: Option<&str>,
) -> ActiveRunnerJobEnrichment {
    let snapshots = match snapshots {
        Ok(snapshots) => snapshots,
        Err(error) => {
            return ActiveRunnerJobEnrichment {
                runs: Vec::new(),
                state: Some(super::types::RunsRunnerEnrichment {
                    status: "partial",
                    partial: true,
                    runner_unavailable: vec![super::types::RunsRunnerUnavailable {
                        runner_id: "controller".to_string(),
                        code: error.code.as_str().to_string(),
                        message: error.message,
                    }],
                }),
                durable_run_ids: HashSet::new(),
                complete: false,
            };
        }
    };
    let unavailable = snapshots
        .iter()
        .filter_map(|snapshot| {
            snapshot
                .active_job_error
                .as_ref()
                .map(|error| super::types::RunsRunnerUnavailable {
                    runner_id: snapshot.runner_id.clone(),
                    code: error.code.clone(),
                    message: error.message.clone(),
                })
        })
        .collect::<Vec<_>>();
    let complete = unavailable.is_empty();
    let active_jobs = snapshots
        .into_iter()
        .filter(|snapshot| snapshot.connected)
        .flat_map(|snapshot| snapshot.active_jobs)
        .filter(|job| match status {
            Some(status) => status == job.status.run_status_label(),
            None => true,
        })
        .collect::<Vec<_>>();
    let durable_run_ids = active_jobs
        .iter()
        .filter_map(|job| job.durable_run_id.clone())
        .collect();
    let runs = active_jobs
        .into_iter()
        .filter_map(active_runner_job_run_summary_if_durable)
        .collect();
    let state = Some(super::types::RunsRunnerEnrichment {
        status: if unavailable.is_empty() {
            "complete"
        } else {
            "partial"
        },
        partial: !complete,
        runner_unavailable: unavailable,
    });
    ActiveRunnerJobEnrichment {
        runs,
        state,
        durable_run_ids,
        complete,
    }
}

const STALE_RUN_SAMPLE_LIMIT: usize = 10;

fn stale_run_summary(
    runs: &[RunRecord],
    active_durable_run_ids: &HashSet<String>,
    active_runner_jobs_complete: bool,
) -> Option<RunsStaleRunSummary> {
    let stale_ids = runs
        .iter()
        .filter(|run| {
            run.status == RunStatus::Running.as_str()
                && reconcile::stale_running_reason(run, &homeboy::core::process::pid_is_running)
                    .is_some()
                // A direct daemon snapshot is authoritative for runner-backed
                // rows. Never call one stale while that job is live or unknown —
                // but "unknown" cannot mean "forever". Past the reconciliation
                // exemption ceiling, an absent snapshot stops being evidence of
                // a live job and becomes evidence of a lost one (#11107).
                && !active_durable_run_ids.contains(&run.id)
                && (!reconcile::runner_backed_run(run)
                    || active_runner_jobs_complete
                    || reconcile::runner_backed_exemption_expired(run))
        })
        .map(|run| run.id.clone())
        .collect::<Vec<_>>();
    (!stale_ids.is_empty()).then(|| {
        let run_ids = stale_ids
            .iter()
            .take(STALE_RUN_SAMPLE_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        RunsStaleRunSummary {
            count: stale_ids.len(),
            omitted_run_count: stale_ids.len().saturating_sub(run_ids.len()),
            run_ids,
            action: super::types::stale_runs_reconcile_action(),
        }
    })
}

#[cfg(test)]
mod runner_enrichment_tests {

    use super::*;

    #[test]
    fn total_indexed_snapshot_failure_is_typed_partial_output() {
        let enrichment = active_runner_job_summaries_from_snapshots(
            Err(Error::internal_unexpected("wedged runner probe")),
            None,
        );
        let state = enrichment.state.expect("partial state");

        assert!(enrichment.runs.is_empty());
        assert_eq!(state.status, "partial");
        assert!(state.partial);
        assert_eq!(state.runner_unavailable.len(), 1);
        assert_eq!(state.runner_unavailable[0].runner_id, "controller");
        assert!(state.runner_unavailable[0]
            .message
            .contains("wedged runner probe"));
    }

    fn dead_owned_run(id: &str, runner_backed: bool) -> RunRecord {
        let mut metadata = serde_json::json!({ "homeboy_run_owner": { "pid": u32::MAX } });
        if runner_backed {
            metadata["runner_job_id"] = serde_json::json!("job-1");
        }
        RunRecord {
            id: id.to_string(),
            kind: "bench".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: None,
            status: RunStatus::Running.as_str().to_string(),
            command: Some("homeboy bench".to_string()),
            cwd: None,
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: metadata,
        }
    }

    #[test]
    fn stale_summary_exposes_one_non_mutating_grouped_action() {
        let summary = stale_run_summary(
            &[dead_owned_run("stale-run", false)],
            &HashSet::new(),
            false,
        )
        .expect("stale summary");

        assert_eq!(summary.count, 1);
        assert_eq!(summary.run_ids, vec!["stale-run"]);
        assert_eq!(summary.action.command, "homeboy runs reconcile --dry-run");
        assert!(summary.action.kind.is_some());
    }

    #[test]
    fn stale_summary_preserves_current_authoritative_runner_job() {
        let mut active = HashSet::new();
        active.insert("current-run".to_string());

        assert!(
            stale_run_summary(&[dead_owned_run("current-run", true)], &active, true,).is_none()
        );
    }

    #[test]
    fn stale_summary_bounds_large_displayed_sets() {
        let runs = (0..100)
            .map(|index| dead_owned_run(&format!("stale-{index}"), false))
            .collect::<Vec<_>>();
        let summary = stale_run_summary(&runs, &HashSet::new(), false).expect("stale summary");

        assert_eq!(summary.count, 100);
        assert_eq!(summary.run_ids.len(), STALE_RUN_SAMPLE_LIMIT);
        assert_eq!(summary.omitted_run_count, 90);
    }
}

fn active_runner_job_run_summary_if_durable(
    job: api_jobs::ActiveRunnerJobSummary,
) -> Option<RunSummary> {
    let summary = api_jobs::active_runner_job_run_summary_if_durable(job)?;
    Some(RunSummary {
        id: summary.id,
        kind: summary.kind,
        status: summary.status,
        started_at: summary.started_at,
        finished_at: None,
        component_id: None,
        rig_id: None,
        git_sha: None,
        command: Some(summary.command),
        cwd: summary.cwd,
        status_note: Some(summary.status_note),
        artifact_index: None,
    })
}

pub fn show_run(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_readonly()?;
    let run = run_detail(&store, run_id)?;
    let actionable = actionable_for_run_detail(&run);
    Ok((
        RunsOutput::Show(RunsShowOutput {
            command: "runs.show",
            run,
            actionable,
        }),
        0,
    ))
}

pub(crate) fn resume_plan(store: &ObservationStore, run_id: &str) -> CmdResult<RunsOutput> {
    reconcile::reconcile_owned_stale_running_runs(store, 1000)?;
    let run = runs_service::require_run(store, run_id)?;
    let Some(ledger) = validation_progress_ledger_for_run(&run) else {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run `{run_id}` does not contain validation progress metadata"),
            Some(run_id.to_string()),
            Some(vec![
                "Run `homeboy runs show <run-id>` to inspect available metadata.".to_string(),
                "Validation progress is recorded for Homeboy-managed validation command sets with a run directory.".to_string(),
            ]),
        ));
    };

    Ok((
        RunsOutput::ResumePlan(RunsResumePlanOutput {
            command: "runs.resume-plan",
            run_id: run_id.to_string(),
            status: ledger.status.clone(),
            completed_count: ledger.completed_count,
            command_count: ledger.command_count,
            failed_count: ledger.failed_count,
            last_completed_command: ledger.last_completed_command.clone(),
            active_command: ledger.active_command.clone(),
            next_command: ledger.next_command.clone(),
            hints: ledger.resume_hints(),
        }),
        0,
    ))
}

fn validation_progress_ledger_for_run(run: &RunRecord) -> Option<ValidationProgressLedger> {
    ValidationProgressLedger::from_run(run).or_else(|| {
        run.metadata_json
            .get("run_dir")
            .and_then(Value::as_str)
            .and_then(|path| {
                homeboy::core::engine::run_dir::RunDir::from_existing(PathBuf::from(path)).ok()
            })
            .and_then(|run_dir| ValidationProgressLedger::read_from_run_dir(&run_dir))
    })
}

#[cfg(test)]
pub fn artifacts(run_id: &str) -> CmdResult<RunsOutput> {
    artifacts_from_args(
        &test_store(),
        RunsArtifactsArgs {
            run_id: run_id.to_string(),
            runner: None,
            pull: false,
            pull_dir: None,
            token: None,
            kind: None,
            mime: None,
            original_path: None,
            path_suffix: None,
            fixture: None,
            surface: None,
            scenario: None,
            name_glob: None,
            limit: 50,
            offset: 0,
            // Preserve the direct test helper's historical exhaustive projection;
            // the public CLI defaults to the bounded discovery page.
            full: true,
        },
    )
}

pub(crate) fn artifacts_from_args(
    store: &ObservationStore,
    args: RunsArtifactsArgs,
) -> CmdResult<RunsOutput> {
    if let Some(runner_id) = args.runner.as_deref() {
        if args.pull {
            return Err(Error::validation_invalid_argument(
                "pull",
                "`--pull` operates on the local mirrored observation store; drop `--runner` to retrieve runner artifact bytes to the operator-local artifact root",
                Some(runner_id.to_string()),
                Some(vec![
                    format!("Run `homeboy runs artifacts {} --pull` (without --runner) to pull mirrored runner artifacts locally.", args.run_id),
                ]),
            ));
        }
        return remote::runner_artifacts(runner_id, &args);
    }

    let run = runs_service::require_run(store, &args.run_id)?;
    let run_id = run.id;
    let filter = ArtifactListFilter {
        token: args.token.clone(),
        kind: args.kind.clone(),
        mime: args.mime.clone(),
        original_path: args.original_path.clone(),
        path_suffix: args.path_suffix.clone(),
        fixture: args.fixture.clone(),
        surface: args.surface.clone(),
        scenario: args.scenario.clone(),
        name_glob: args.name_glob.clone(),
        limit: args.limit,
        offset: args.offset,
    };
    let page = if args.full {
        None
    } else {
        Some(store.list_artifacts_page(&run_id, &filter)?)
    };
    let artifacts = match page.as_ref() {
        Some(page) => page.artifacts.clone(),
        None => runs_service::list_artifacts_for_run(store, &run_id)?,
    };
    if !args.full {
        let page = page.expect("bounded artifact page is present");
        return Ok((
            RunsOutput::Artifacts(RunsArtifactsOutput {
                command: "runs.artifacts",
                run_id: run_id.clone(),
                runner_id: None,
                path_guide: RunsArtifactPathGuide::for_listing(&run_id, None),
                next_commands: artifact_get_command_hints(&run_id, &artifacts),
                artifacts,
                page: Some(RunsArtifactPage {
                    total: page.total,
                    limit: page.limit,
                    offset: page.offset,
                    next_offset: (page.offset + page.artifacts.len() < page.total)
                        .then_some(page.offset + page.artifacts.len()),
                }),
                resource_lifecycle_index: None,
                directory_publication: Vec::new(),
                preview_entrypoints: Vec::new(),
                matrix_summary: None,
                fuzz_result_envelopes: Vec::new(),
                pull: None,
            }),
            0,
        ));
    }
    let preview_entrypoints = artifacts
        .iter()
        .flat_map(homeboy::core::artifacts::html_preview_entrypoints)
        .collect();
    let findings = store.list_findings(FindingListFilter {
        run_id: Some(run_id.clone()),
        tool: None,
        file: None,
        fingerprint: None,
        limit: Some(10_000),
    })?;
    let matrix_summary =
        homeboy::core::artifacts::summarize_matrix_artifacts(&run_id, &artifacts, &findings);
    let fuzz_result_envelopes = artifacts
        .iter()
        .filter_map(homeboy::fuzz::inspect_fuzz_result_envelope_artifact)
        .collect();
    let directory_publication = directory_publication_guidance_for_artifacts(&artifacts);
    let resource_lifecycle_index = resource_lifecycle_index_from_artifacts(&artifacts)?;
    let pull = if args.pull {
        Some(pull_artifacts_to_local(
            &artifacts,
            args.pull_dir.as_deref(),
        )?)
    } else {
        None
    };
    Ok((
        RunsOutput::Artifacts(RunsArtifactsOutput {
            command: "runs.artifacts",
            run_id: run_id.clone(),
            runner_id: None,
            path_guide: RunsArtifactPathGuide::for_listing(&run_id, None),
            next_commands: artifact_get_command_hints(&run_id, &artifacts),
            artifacts,
            page: None,
            resource_lifecycle_index,
            directory_publication,
            preview_entrypoints,
            matrix_summary,
            fuzz_result_envelopes,
            pull,
        }),
        0,
    ))
}

fn artifact_get_command_hints(
    run_id: &str,
    artifacts: &[homeboy::core::observation::ArtifactRecord],
) -> Vec<RunsArtifactCommandHint> {
    artifacts
        .iter()
        .map(|artifact| {
            let token = preferred_artifact_token(artifact, artifacts);
            RunsArtifactCommandHint {
                artifact_id: artifact.id.clone(),
                kind: artifact.kind.clone(),
                get_command: format!(
                    "homeboy runs artifact get {} {}",
                    shell_arg(run_id),
                    shell_arg(&token)
                ),
                token,
            }
        })
        .collect()
}

fn preferred_artifact_token(
    artifact: &homeboy::core::observation::ArtifactRecord,
    artifacts: &[homeboy::core::observation::ArtifactRecord],
) -> String {
    if let Some(name) = artifact.metadata_json.get("name").and_then(Value::as_str) {
        if token_is_unique(name, artifacts) {
            return name.to_string();
        }
    }
    if token_is_unique(&artifact.kind, artifacts) {
        return artifact.kind.clone();
    }
    artifact.id.clone()
}

fn token_is_unique(token: &str, artifacts: &[homeboy::core::observation::ArtifactRecord]) -> bool {
    artifacts
        .iter()
        .filter(|artifact| {
            artifact.id == token
                || artifact.kind == token
                || artifact.metadata_json.get("name").and_then(Value::as_str) == Some(token)
                || artifact
                    .metadata_json
                    .get("original_manifest_id")
                    .and_then(Value::as_str)
                    == Some(token)
        })
        .count()
        == 1
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn directory_publication_guidance_for_artifacts(
    artifacts: &[homeboy::core::observation::ArtifactRecord],
) -> Vec<RunsDirectoryArtifactPublicationGuidance> {
    artifacts
        .iter()
        .filter_map(|artifact| {
            let address = ArtifactAddress::from_record(artifact);
            directory_publication_guidance(artifact, &address).map(|guidance| {
                RunsDirectoryArtifactPublicationGuidance {
                    artifact_id: artifact.id.clone(),
                    kind: artifact.kind.clone(),
                    guidance,
                }
            })
        })
        .collect()
}

/// Best-effort retrieval of each artifact's bytes to the operator-local
/// artifact root so a completed run is self-contained.
///
/// Local-file (and locally-present directory) artifacts are already operator
/// readable and reported as `already_local`. Remote runner artifacts are
/// downloaded; metadata-only / url artifacts are skipped with a reason. A
/// single artifact's failure never aborts the pass — it is recorded so the
/// operator sees exactly which diagnostics are unreachable and why.
fn pull_artifacts_to_local(
    artifacts: &[homeboy::core::observation::ArtifactRecord],
    pull_dir: Option<&std::path::Path>,
) -> homeboy::core::Result<RunsArtifactPullSummary> {
    let pull_root = match pull_dir {
        Some(dir) => dir.display().to_string(),
        None => homeboy::core::artifact_root()?.display().to_string(),
    };
    let mut entries = Vec::with_capacity(artifacts.len());
    let (mut pulled_count, mut already_local_count, mut skipped_count, mut failed_count) =
        (0, 0, 0, 0);

    for artifact in artifacts {
        let entry = match runs_service::classify_artifact_storage(artifact) {
            runs_service::ArtifactStorage::LocalFile => {
                already_local_count += 1;
                RunsArtifactPullEntry {
                    artifact_id: artifact.id.clone(),
                    storage: "local_file",
                    status: "already_local",
                    output_path: Some(artifact.path.clone()),
                    size_bytes: artifact.size_bytes,
                    content_type: artifact.mime.clone(),
                    sha256: artifact.sha256.clone(),
                    error: None,
                }
            }
            runs_service::ArtifactStorage::Remote => {
                let output = pull_dir.map(|dir| dir.join(sanitize_artifact_filename(&artifact.id)));
                match runs_service::download_remote_artifact(artifact.clone(), output) {
                    Ok(outcome) => {
                        pulled_count += 1;
                        RunsArtifactPullEntry {
                            artifact_id: artifact.id.clone(),
                            storage: "remote",
                            status: "pulled",
                            output_path: Some(outcome.output_path.display().to_string()),
                            size_bytes: outcome.size_bytes,
                            content_type: outcome.content_type,
                            sha256: outcome.sha256,
                            error: None,
                        }
                    }
                    Err(err) => {
                        failed_count += 1;
                        RunsArtifactPullEntry {
                            artifact_id: artifact.id.clone(),
                            storage: "remote",
                            status: "failed",
                            output_path: None,
                            size_bytes: None,
                            content_type: None,
                            sha256: None,
                            error: Some(err.message),
                        }
                    }
                }
            }
            runs_service::ArtifactStorage::PublicUrl => {
                let output = pull_dir.map(|dir| dir.join(sanitize_artifact_filename(&artifact.id)));
                match runs_service::download_public_artifact(artifact.clone(), output) {
                    Ok(outcome) => {
                        pulled_count += 1;
                        RunsArtifactPullEntry {
                            artifact_id: artifact.id.clone(),
                            storage: "public_url",
                            status: "pulled",
                            output_path: Some(outcome.output_path.display().to_string()),
                            size_bytes: outcome.size_bytes,
                            content_type: outcome.content_type,
                            sha256: outcome.sha256,
                            error: None,
                        }
                    }
                    Err(err) => {
                        failed_count += 1;
                        RunsArtifactPullEntry {
                            artifact_id: artifact.id.clone(),
                            storage: "public_url",
                            status: "failed",
                            output_path: None,
                            size_bytes: None,
                            content_type: None,
                            sha256: None,
                            error: Some(err.message),
                        }
                    }
                }
            }
            runs_service::ArtifactStorage::MetadataOnly => {
                skipped_count += 1;
                RunsArtifactPullEntry {
                    artifact_id: artifact.id.clone(),
                    storage: "metadata_only",
                    status: "skipped",
                    output_path: None,
                    size_bytes: None,
                    content_type: None,
                    sha256: None,
                    error: Some(
                        "artifact was imported as metadata only; bytes are not available"
                            .to_string(),
                    ),
                }
            }
            runs_service::ArtifactStorage::Other => {
                // A locally-present directory artifact is already self-contained.
                if artifact.artifact_type == "directory"
                    && std::path::Path::new(&artifact.path).is_dir()
                {
                    already_local_count += 1;
                    RunsArtifactPullEntry {
                        artifact_id: artifact.id.clone(),
                        storage: "other",
                        status: "already_local",
                        output_path: Some(artifact.path.clone()),
                        size_bytes: artifact.size_bytes,
                        content_type: artifact.mime.clone(),
                        sha256: artifact.sha256.clone(),
                        error: None,
                    }
                } else {
                    skipped_count += 1;
                    RunsArtifactPullEntry {
                        artifact_id: artifact.id.clone(),
                        storage: "other",
                        status: "skipped",
                        output_path: None,
                        size_bytes: None,
                        content_type: None,
                        sha256: None,
                        error: Some(format!(
                            "artifact type `{}` is not a pullable file",
                            artifact.artifact_type
                        )),
                    }
                }
            }
        };
        entries.push(entry);
    }

    Ok(RunsArtifactPullSummary {
        pull_root,
        pulled_count,
        already_local_count,
        skipped_count,
        failed_count,
        entries,
    })
}

/// Derive a filesystem-safe filename from an artifact id for `--pull-dir`
/// targets.
///
/// One implementation, shared with the runner download cache writer
/// (#10586): this substitution used to exist only here, on the `--pull-dir`
/// path, while the default cache path joined the remote's name unsanitized.
/// Two copies is how that gap reopens.
fn sanitize_artifact_filename(artifact_id: &str) -> String {
    homeboy::core::runner_download_cache::sanitize_artifact_file_name(artifact_id)
}

pub fn env(store: &ObservationStore, run_id: &str) -> CmdResult<RunsOutput> {
    let run = runs_service::require_run(store, run_id)?;
    let Some(envelope) = run.metadata_json.get("env_resolution").cloned() else {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run `{run_id}` does not contain Lab environment provenance metadata"),
            Some(run_id.to_string()),
            Some(vec![
                "Environment provenance is recorded for Lab-offloaded runs that include `homeboy/env-resolution/v1` metadata.".to_string(),
                "Run `homeboy runs show <run-id> --json` to inspect available metadata keys.".to_string(),
            ]),
        ));
    };

    let schema = envelope
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema != "homeboy/env-resolution/v1" {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run `{run_id}` contains unsupported environment provenance schema `{schema}`"),
            Some(run_id.to_string()),
            Some(vec![
                "Expected `homeboy/env-resolution/v1`.".to_string(),
                "Run `homeboy runs show <run-id> --json` to inspect the raw metadata shape."
                    .to_string(),
            ]),
        ));
    }

    let values_redacted = envelope
        .get("values_redacted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !values_redacted {
        return Err(Error::validation_invalid_argument(
            "run_id",
            format!("run `{run_id}` environment provenance is not marked redacted"),
            Some(run_id.to_string()),
            Some(vec![
                "Homeboy refuses to print unredacted environment provenance.".to_string(),
                "Capture a fresh Lab run with `homeboy/env-resolution/v1` redacted provenance metadata.".to_string(),
            ]),
        ));
    }
    let keys = envelope
        .get("keys")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(env_key_output)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let summary = RunsEnvSummary {
        key_count: keys.len(),
        secret_key_count: keys
            .iter()
            .filter(|entry| entry.classification == "secret")
            .count(),
        public_key_count: keys
            .iter()
            .filter(|entry| entry.classification == "public")
            .count(),
        shadowed_key_count: keys
            .iter()
            .filter(|entry| !entry.shadowed_source_layers.is_empty())
            .count(),
    };

    Ok((
        RunsOutput::Env(RunsEnvOutput {
            command: "runs.env",
            run_id: run_id.to_string(),
            schema: schema.to_string(),
            values_redacted,
            summary,
            keys,
        }),
        0,
    ))
}

fn env_key_output(value: &Value) -> Option<RunsEnvKeyOutput> {
    Some(RunsEnvKeyOutput {
        key: value.get("key")?.as_str()?.to_string(),
        classification: value
            .get("classification")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        value_status: value
            .get("value_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        value_preview: value
            .get("value_preview")
            .and_then(Value::as_str)
            .unwrap_or("<redacted>")
            .to_string(),
        winning_source_layer: value
            .get("winning_source_layer")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        shadowed_source_layers: value
            .get("shadowed_source_layers")
            .and_then(Value::as_array)
            .map(|layers| {
                layers
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        source_layers: value
            .get("source_layers")
            .and_then(Value::as_array)
            .map(|layers| layers.iter().filter_map(env_source_layer_output).collect())
            .unwrap_or_default(),
    })
}

fn env_source_layer_output(value: &Value) -> Option<RunsEnvSourceLayerOutput> {
    Some(RunsEnvSourceLayerOutput {
        source: value.get("source")?.as_str()?.to_string(),
        status: value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        classification: value
            .get("classification")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        value_status: value
            .get("value_status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    })
}

pub fn artifact_command(args: RunsArtifactArgs) -> CmdResult<RunsOutput> {
    match args.command {
        RunsArtifactCommand::Attach(args) => remote_artifact::attach(args),
        RunsArtifactCommand::Get(args) => artifact_get(args),
        RunsArtifactCommand::GetHandle(args) => artifact_get_handle(args),
        RunsArtifactCommand::Preview(args) => remote_artifact::preview(args),
        RunsArtifactCommand::PreviewHandle(args) => artifact_preview_handle(args),
        RunsArtifactCommand::Capture(args) => remote_artifact::capture(args),
        RunsArtifactCommand::CleanupDownloads(args) => remote_artifact::cleanup_downloads(args),
        RunsArtifactCommand::CleanupPersisted(args) => remote_artifact::cleanup_persisted(args),
        RunsArtifactCommand::Postprocess(args) => {
            let (output, exit_code) = crate::commands::artifact_postprocess::run(args)?;
            Ok((RunsOutput::ArtifactPostprocess(output), exit_code))
        }
    }
}

pub(crate) fn artifact_preview_handle(
    args: RunsArtifactPreviewHandleArgs,
) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    let artifact = store
        .get_artifact_for_handle(&args.handle)?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "handle",
                "artifact handle was not found",
                Some(args.handle),
                None,
            )
        })?;
    remote_artifact::preview_artifact(artifact, args.port)
}

pub(crate) fn artifact_get(args: RunsArtifactGetArgs) -> CmdResult<RunsOutput> {
    let fields = args.field.clone();
    let (output, exit_code) = artifact_get_inner(args)?;
    if fields.is_empty() {
        return Ok((output, exit_code));
    }
    apply_field_selection(output, &fields)
}

fn artifact_get_inner(args: RunsArtifactGetArgs) -> CmdResult<RunsOutput> {
    if let Some(runner_id) = args.runner.clone() {
        return remote::runner_artifact_get(&runner_id, args);
    }

    let store = ObservationStore::open_initialized()?;
    let artifact = runs_service::resolve_artifact_for_run(&store, &args.run_id, &args.artifact_id)?;
    artifact_get_resolved(artifact, args.output)
}

/// Handle lookup is deliberately exact: it has no run-id, name, ordinal, or
/// fuzzy token fallback. The unique handle index scopes the resolved artifact
/// to its durable owner before any byte access happens.
fn artifact_get_handle(args: RunsArtifactGetHandleArgs) -> CmdResult<RunsOutput> {
    let fields = args.field.clone();
    let store = ObservationStore::open_initialized()?;
    let artifact = store
        .get_artifact_for_handle(&args.handle)?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "handle",
                "artifact handle was not found",
                Some(args.handle),
                None,
            )
        })?;
    let (output, exit_code) = artifact_get_resolved(artifact, args.output)?;
    if fields.is_empty() {
        Ok((output, exit_code))
    } else {
        apply_field_selection(output, &fields)
    }
}

fn artifact_get_resolved(
    artifact: homeboy::core::observation::ArtifactRecord,
    output: Option<PathBuf>,
) -> CmdResult<RunsOutput> {
    match runs_service::classify_artifact_storage(&artifact) {
        runs_service::ArtifactStorage::LocalFile => {
            let outcome = runs_service::copy_local_file_artifact(artifact, output)?;
            Ok((
                RunsOutput::ArtifactGet(RunsArtifactGetOutput {
                    command: "runs.artifact.get",
                    run_id: outcome.run_id,
                    artifact_id: outcome.artifact_id,
                    runner_id: None,
                    source_content_url: None,
                    output_path: outcome.output_path.display().to_string(),
                    content_type: outcome.content_type,
                    size_bytes: outcome.size_bytes,
                    sha256: outcome.sha256,
                    artifact_ref: None,
                }),
                0,
            ))
        }
        runs_service::ArtifactStorage::Remote => remote_artifact::get(artifact, output),
        runs_service::ArtifactStorage::PublicUrl => {
            let source_content_url = artifact
                .url
                .clone()
                .or_else(|| artifact.public_url.clone());
            let outcome = runs_service::download_public_artifact(artifact, output)?;
            Ok((
                RunsOutput::ArtifactGet(RunsArtifactGetOutput {
                    command: "runs.artifact.get",
                    run_id: outcome.run_id,
                    artifact_id: outcome.artifact_id,
                    runner_id: None,
                    source_content_url,
                    output_path: outcome.output_path.display().to_string(),
                    content_type: outcome.content_type,
                    size_bytes: outcome.size_bytes,
                    sha256: outcome.sha256,
                    artifact_ref: None,
                }),
                0,
            ))
        }
        runs_service::ArtifactStorage::MetadataOnly => Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} was imported as metadata only; artifact bytes are not available in this bundle",
                artifact.id
            ),
            Some(artifact.id),
            None,
        )),
        runs_service::ArtifactStorage::Other => Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "artifact {} is {}, not a downloadable file",
                artifact.id, artifact.artifact_type
            ),
            Some(artifact.id.clone()),
            None,
        )),
    }
}

/// Project `--field`/`-q` selectors over a `show`, `evidence`, or `artifact get` result,
/// returning a compact [`RunsOutput::FieldSelection`] carrying only the
/// requested fields. Show selectors are rooted at the run detail; artifact-get
/// selectors at the artifact-get result. Unsupported variants are returned
/// unchanged so the selector never silently swallows other output.
pub(super) fn apply_field_selection(
    output: RunsOutput,
    fields: &[String],
) -> CmdResult<RunsOutput> {
    let value = serde_json::to_value(&output).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("serialize runs output for field selection".to_string()),
        )
    })?;
    let variant = value
        .get("variant")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = value.get("payload").cloned().unwrap_or(Value::Null);
    let (root, run_id, artifact_id) = match variant {
        "show" => {
            let run = payload.get("run").cloned().unwrap_or(Value::Null);
            let run_id = run
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (run, run_id, None)
        }
        "artifact_get" => {
            let run_id = payload
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let artifact_id = payload
                .get("artifact_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            (payload.clone(), run_id, artifact_id)
        }
        "evidence" | "evidence_summary" => {
            let run_id = payload
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (payload, run_id, None)
        }
        _ => return Ok((output, 0)),
    };

    let selected = super::common::select_fields(&root, fields)?
        .into_iter()
        .map(|(field, value)| RunsSelectedField { field, value })
        .collect();

    Ok((
        RunsOutput::FieldSelection(RunsFieldSelectionOutput {
            command: "runs.field",
            run_id,
            artifact_id,
            fields: selected,
        }),
        0,
    ))
}

pub(super) fn require_run(
    store: &ObservationStore,
    run_id: &str,
) -> homeboy::core::Result<RunRecord> {
    runs_service::require_run(store, run_id)
}

pub(super) fn run_detail(
    store: &ObservationStore,
    run_id: &str,
) -> homeboy::core::Result<RunDetail> {
    let (run, artifacts) = runs_service::load_run_with_artifacts(store, run_id)?;
    Ok(RunDetail {
        summary: run_summary(run.clone()),
        homeboy_version: run.homeboy_version,
        metadata: run.metadata_json,
        artifacts,
    })
}

pub(crate) fn run_summary(run: RunRecord) -> RunSummary {
    let status_note = reconcile::running_status_note(&run);
    RunSummary {
        id: run.id,
        kind: run.kind,
        status: run.status,
        started_at: run.started_at,
        finished_at: run.finished_at,
        component_id: run.component_id,
        rig_id: run.rig_id,
        git_sha: run.git_sha,
        command: run.command,
        cwd: run.cwd,
        status_note,
        artifact_index: None,
    }
}

#[cfg(test)]
mod pull_tests {
    use std::fs;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    use homeboy::core::observation::{ArtifactRecord, NewRunRecord, ObservationStore};
    use homeboy::test_support::with_isolated_home;

    use super::*;

    fn artifact_root_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn metadata_only_record(run_id: &str, id: &str) -> ArtifactRecord {
        ArtifactRecord {
            id: id.to_string(),
            run_id: run_id.to_string(),
            kind: "finding-packets".to_string(),
            artifact_type: "metadata-only".to_string(),
            path: format!("metadata://{id}"),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: None,
            metadata_json: serde_json::json!({}),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn sanitize_artifact_filename_is_path_safe() {
        assert_eq!(
            sanitize_artifact_filename("finding-packets.json"),
            "finding-packets.json"
        );
        assert_eq!(sanitize_artifact_filename("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_artifact_filename("a/b\\c"), "a_b_c");
        assert_eq!(sanitize_artifact_filename("..."), "artifact");
        assert_eq!(sanitize_artifact_filename(""), "artifact");
    }

    #[test]
    fn pull_reports_local_file_as_already_local() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let source = home.path().join("finding-packets.json");
            fs::write(&source, br#"{"findings":[]}"#).expect("source");
            let artifact = store
                .record_artifact(&run.id, "finding-packets", &source)
                .expect("artifact");

            let summary = pull_artifacts_to_local(std::slice::from_ref(&artifact), None)
                .expect("pull summary");

            assert_eq!(summary.already_local_count, 1);
            assert_eq!(summary.pulled_count, 0);
            assert_eq!(summary.failed_count, 0);
            assert_eq!(summary.entries.len(), 1);
            assert_eq!(summary.entries[0].status, "already_local");
            assert_eq!(summary.entries[0].storage, "local_file");
            assert_eq!(
                summary.entries[0].output_path.as_deref(),
                Some(artifact.path.as_str())
            );
        });
    }

    #[test]
    fn pull_skips_metadata_only_artifacts_with_reason() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            let record = metadata_only_record("run-1", "finding-packets");
            let summary = pull_artifacts_to_local(&[record], None).expect("pull summary");

            assert_eq!(summary.skipped_count, 1);
            assert_eq!(summary.entries[0].status, "skipped");
            assert_eq!(summary.entries[0].storage, "metadata_only");
            assert!(summary.entries[0]
                .error
                .as_deref()
                .unwrap()
                .contains("metadata only"));
        });
    }

    #[test]
    fn pull_records_per_artifact_failure_without_aborting() {
        let _guard = artifact_root_test_lock();
        with_isolated_home(|home| {
            let artifact_root = home.path().join("artifacts");
            homeboy::core::set_artifact_root_override(Some(artifact_root));

            // A remote runner-artifact ref for a runner that does not exist must
            // be reported as a failed entry, not panic or abort the pass.
            let remote = ArtifactRecord {
                id: "matrix-json".to_string(),
                run_id: "run-1".to_string(),
                kind: "matrix".to_string(),
                artifact_type: "remote_file".to_string(),
                path: "runner-artifact://does-not-exist/run-1/matrix-json".to_string(),
                url: None,
                public_url: None,
                viewer_url: None,
                viewer_links: Vec::new(),
                sha256: None,
                size_bytes: None,
                mime: None,
                metadata_json: serde_json::json!({}),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            let local_source = home.path().join("summary.json");
            fs::write(&local_source, b"{}").expect("source");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let local = store
                .record_artifact(&run.id, "summary", &local_source)
                .expect("artifact");

            let summary = pull_artifacts_to_local(&[remote, local], None).expect("pull summary");

            assert_eq!(summary.entries.len(), 2);
            assert_eq!(summary.failed_count, 1);
            assert_eq!(summary.already_local_count, 1);
            let remote_entry = summary
                .entries
                .iter()
                .find(|entry| entry.artifact_id == "matrix-json")
                .expect("remote entry");
            assert_eq!(remote_entry.status, "failed");
            assert!(remote_entry.error.is_some());
        });
    }

    #[test]
    fn artifacts_from_args_pull_with_runner_is_rejected() {
        let result = artifacts_from_args(
            &test_store(),
            RunsArtifactsArgs {
                run_id: "run-1".to_string(),
                runner: Some("lab".to_string()),
                pull: true,
                pull_dir: None,
                token: None,
                kind: None,
                mime: None,
                original_path: None,
                path_suffix: None,
                fixture: None,
                surface: None,
                scenario: None,
                name_glob: None,
                limit: 50,
                offset: 0,
                full: false,
            },
        );
        let Err(err) = result else {
            panic!("--pull with --runner should fail");
        };
        assert!(err.to_string().contains("local mirrored observation store"));
    }

    #[test]
    fn cancel_marks_a_running_run_skipped_without_changing_its_identity() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(
                    NewRunRecord::builder("review")
                        .component_id("homeboy")
                        .build(),
                )
                .expect("run");

            let (output, exit_code) =
                cancel_run(&super::test_store(), &run.id).expect("cancel run");

            assert_eq!(exit_code, 0);
            let RunsOutput::Cancel(output) = output else {
                panic!("expected cancel output");
            };
            assert_eq!(output.command, "runs.cancel");
            assert_eq!(output.run_id, run.id);
            assert_eq!(output.status, "skipped");
            let persisted = store
                .get_run(&run.id)
                .expect("get run")
                .expect("run exists");
            assert_eq!(persisted.status, "skipped");
            assert_eq!(persisted.metadata_json["cancellation"]["requested"], true);
        });
    }
}
