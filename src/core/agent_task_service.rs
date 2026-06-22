use serde_json::Value;
use std::path::PathBuf;

use crate::core::agent_task::{AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkspaceMode};
use crate::core::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
};
use crate::core::agent_task_finalization::{
    finalize_pr, AgentTaskPrEvidence, AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails,
    AgentTaskPrSourceRelationship, AgentTaskPrVerification,
};
use crate::core::agent_task_gate::VerifyGateOptions;
use crate::core::agent_task_lifecycle::{
    self, AgentTaskRunArtifacts, AgentTaskRunLog, AgentTaskRunRecord,
};
use crate::core::agent_task_promotion::{
    promote, AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use crate::core::agent_task_provider::{
    apply_provider_runner_secret_env_contracts, provider_secret_sources_for_plan,
};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskScheduler, AgentTaskState,
};
use crate::core::agent_task_secrets::validate_secret_env_with_fallbacks;
use crate::core::secret_env_plan::SecretEnvPlan;
use crate::core::{config, worktree, Error, Result};

#[derive(Debug, Clone)]
pub struct AgentTaskRunResult<T> {
    pub value: T,
    pub exit_code: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTaskDiscoveryFilter {
    All,
    Active,
    Latest,
}

/// Discovery options layered on top of an [`AgentTaskDiscoveryFilter`]. Today
/// this carries the operator-facing `--limit` cap shared by the `list`/`active`
/// list surfaces so a large run history stays scannable, matching the
/// pagination affordance other list commands expose (#5681).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentTaskDiscoveryOptions {
    /// Maximum number of runs to return, applied after filtering/sorting.
    /// `None` returns every matching run. Ignored for the `latest` filter,
    /// which is always a single run.
    pub limit: Option<usize>,
}

/// Number of minutes a `Running` record may go without an `updated_at`
/// heartbeat before `agent-task active` treats it as suspect even when its
/// owner process/runner-job liveness cannot be disproven. Lab/offloaded runs
/// whose runner process died silently surface here so operators can reconcile
/// them instead of trusting a frozen `running` record indefinitely (#5682).
const STALE_UPDATE_THRESHOLD_MINUTES: i64 = 30;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskDiscoveryReport {
    pub schema: &'static str,
    pub filter: &'static str,
    pub count: usize,
    /// Total matching runs before any `--limit` cap was applied. Equals `count`
    /// when no limit truncated the list; larger when results were capped so an
    /// operator knows more runs exist (#5681).
    pub total: usize,
    /// The applied `--limit`, echoed back so consumers can tell a capped list
    /// from a complete one. `None` when every matching run was returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// `true` when `total > count` because the `--limit` cap truncated results.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    pub runs: Vec<AgentTaskDiscoveryRun>,
    /// Liveness buckets for the `active` filter so operators can separate
    /// genuinely-active runs from stale/suspect/unreconciled records at a
    /// glance. Only populated for the `active` filter; `None` elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness_summary: Option<AgentTaskLivenessSummary>,
    /// Operator guidance explaining how to find Lab/offloaded runs that this
    /// local discovery pass may not see. A freshly offloaded Lab cook's durable
    /// record lives on the runner, so a local `agent-task list/active/latest`
    /// can miss it; this note documents the correct runner-scoped command and
    /// the `homeboy runs list` fallback (#5681).
    pub lab_discovery: AgentTaskLabDiscoveryHint,
}

/// Guidance describing where Lab/offloaded agent-task runs are discoverable.
/// Local discovery (`agent-task list/active/latest` without `--runner`) only
/// sees runs whose durable records live on this controller; a run offloaded to
/// a Lab runner is recorded on that runner until it reports back. This hint
/// gives operators the exact runner-scoped command plus the cross-location
/// fallback so a freshly-offloaded run is never "lost" (#5681).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskLabDiscoveryHint {
    pub note: &'static str,
    pub runner_scoped_command: &'static str,
    pub fallback_command: &'static str,
}

impl Default for AgentTaskLabDiscoveryHint {
    fn default() -> Self {
        Self {
            note: "This list covers runs whose durable records live on this controller. A run offloaded to a Lab runner is recorded on that runner until it reports back, so a freshly-offloaded run may not appear here yet.",
            runner_scoped_command:
                "homeboy --runner <runner-id> agent-task list   # discover runs resident on a specific Lab runner",
            fallback_command:
                "homeboy runs list   # cross-location fallback that includes offloaded runs",
        }
    }
}

/// Coarse liveness classification for an active (queued/running) run. The
/// `active` filter separates runs into these buckets so a stale/orphaned
/// `running` record — especially a Lab/offloaded run whose runner process died
/// — is never silently treated as genuinely-active (#5682).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskLiveness {
    /// Queued, or running with a verifiable live owner/runner and a fresh heartbeat.
    Active,
    /// Running but the lifecycle layer already flagged the record stale
    /// (owner process gone, runner job unverified, missing runner pid).
    Stale,
    /// Running with no disproven liveness, but the last heartbeat is older than
    /// the staleness threshold — likely orphaned, worth reconciling.
    Suspect,
    /// Running with no owner/runner liveness signal at all and no recent
    /// heartbeat — cannot be confirmed either way without reconciliation.
    Unreconciled,
}

impl AgentTaskLiveness {
    fn as_str(self) -> &'static str {
        match self {
            AgentTaskLiveness::Active => "active",
            AgentTaskLiveness::Stale => "stale",
            AgentTaskLiveness::Suspect => "suspect",
            AgentTaskLiveness::Unreconciled => "unreconciled",
        }
    }

    /// Whether this classification is a candidate for safe reconcile/cancel.
    fn is_reconcilable(self) -> bool {
        matches!(
            self,
            AgentTaskLiveness::Stale | AgentTaskLiveness::Suspect | AgentTaskLiveness::Unreconciled
        )
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AgentTaskLivenessSummary {
    pub active: usize,
    pub stale: usize,
    pub suspect: usize,
    pub unreconciled: usize,
    /// Convenience hint: the safe command path to reconcile stale-running
    /// records without manual state edits.
    pub reconcile_command: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskDiscoveryRun {
    pub run_id: String,
    pub state: agent_task_lifecycle::AgentTaskRunState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    pub counts: AgentTaskDiscoveryCounts,
    pub submitted_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner_job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
    /// Liveness classification of this run (active/stale/suspect/unreconciled).
    /// Populated for the `active` filter; `None` for `all`/`latest` lists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<AgentTaskLiveness>,
    /// Where this run executes: `local`, `remote`, or `runner:<id>`. Lets an
    /// operator trace the runner process for Lab/offloaded runs.
    pub source: String,
    /// Last heartbeat/update timestamp, surfaced so operators can judge
    /// staleness without opening the full record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update: Option<String>,
    /// Age of `last_update` in minutes at report time, when computable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update_age_minutes: Option<i64>,
    pub commands: AgentTaskDiscoveryCommands,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AgentTaskDiscoveryCounts {
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskDiscoveryCommands {
    pub status: String,
    pub logs: String,
    pub artifacts: String,
    pub review: String,
    pub retry: String,
    pub run_plan: String,
    pub promote: String,
    /// Safe per-run reconcile/cancel for a stale-running record. Uses the
    /// lifecycle cancel path (terminates a live owner tree only if present,
    /// otherwise just marks the orphaned record cancelled) — never a manual
    /// state edit (#5682).
    pub reconcile: String,
}

#[derive(Debug, Clone)]
pub struct AgentTaskLoopServiceOptions {
    pub loop_id: String,
    pub initial_run_id: String,
    pub to_worktree: String,
    pub provider_command: Option<String>,
    /// Shared deterministic verification gate fields, factored out of the
    /// per-field duplication that previously spanned the loop/promote types.
    pub gates: VerifyGateOptions,
    pub max_attempts: u32,
    pub no_finalize: bool,
    pub base: String,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub source_refs: Vec<String>,
    pub protected_branches: Vec<String>,
    pub ai_tool: String,
    pub ai_model: Option<String>,
    pub ai_used_for: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskLoopReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub status: String,
    pub attempts: Vec<AgentTaskLoopAttemptReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalization: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskLoopAttemptReport {
    pub attempt: u32,
    pub run_id: String,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<AgentTaskPromotionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<AgentTaskCookLoopReport>,
}

pub fn read_plan(spec: &str) -> Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(spec)?;
    let mut plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })?;
    normalize_plan_workspaces(&mut plan)?;
    Ok(plan)
}

pub fn discover_runs(filter: AgentTaskDiscoveryFilter) -> Result<AgentTaskDiscoveryReport> {
    discover_runs_with_options(filter, AgentTaskDiscoveryOptions::default())
}

/// Discovery with operator options (currently `--limit`). The `latest` filter
/// is inherently a single run, so a limit is a no-op there; `all`/`active`
/// truncate to the requested cap after filtering and sorting, and report the
/// pre-cap `total` so consumers know more runs exist (#5681).
pub fn discover_runs_with_options(
    filter: AgentTaskDiscoveryFilter,
    options: AgentTaskDiscoveryOptions,
) -> Result<AgentTaskDiscoveryReport> {
    let mut records = agent_task_lifecycle::list_records()?;
    let is_active = filter == AgentTaskDiscoveryFilter::Active;
    if is_active {
        records.retain(|record| {
            matches!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Queued
                    | agent_task_lifecycle::AgentTaskRunState::Running
            )
        });
    }
    if filter == AgentTaskDiscoveryFilter::Latest {
        records.truncate(1);
    }

    let total = records.len();

    // `latest` is always a single run; only `all`/`active` honor a limit cap.
    let effective_limit = match filter {
        AgentTaskDiscoveryFilter::Latest => None,
        _ => options.limit,
    };
    if let Some(limit) = effective_limit {
        records.truncate(limit);
    }

    let now = chrono::Utc::now();
    let runs: Vec<_> = records
        .into_iter()
        .map(|record| discovery_run(record, is_active, now))
        .collect();

    let liveness_summary = is_active.then(|| liveness_summary(&runs));
    let truncated = runs.len() < total;

    Ok(AgentTaskDiscoveryReport {
        schema: "homeboy/agent-task-discovery/v1",
        filter: match filter {
            AgentTaskDiscoveryFilter::All => "all",
            AgentTaskDiscoveryFilter::Active => "active",
            AgentTaskDiscoveryFilter::Latest => "latest",
        },
        count: runs.len(),
        total,
        limit: effective_limit,
        truncated,
        runs,
        liveness_summary,
        lab_discovery: AgentTaskLabDiscoveryHint::default(),
    })
}

fn liveness_summary(runs: &[AgentTaskDiscoveryRun]) -> AgentTaskLivenessSummary {
    let mut summary = AgentTaskLivenessSummary {
        reconcile_command: "homeboy agent-task active --reconcile",
        ..Default::default()
    };
    for run in runs {
        match run.liveness {
            Some(AgentTaskLiveness::Active) | None => summary.active += 1,
            Some(AgentTaskLiveness::Stale) => summary.stale += 1,
            Some(AgentTaskLiveness::Suspect) => summary.suspect += 1,
            Some(AgentTaskLiveness::Unreconciled) => summary.unreconciled += 1,
        }
    }
    summary
}

/// Report returned by [`reconcile_stale_active_runs`]. Lists every active run
/// that was classified non-active, and for the reconcilable ones records the
/// outcome of the safe cancel attempt.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskReconcileReport {
    pub schema: &'static str,
    /// `true` when no records were actually mutated (preview mode).
    pub dry_run: bool,
    pub considered: usize,
    pub reconciled: usize,
    pub failed: usize,
    pub runs: Vec<AgentTaskReconcileRun>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskReconcileRun {
    pub run_id: String,
    pub liveness: AgentTaskLiveness,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    /// `reconciled`, `would-reconcile` (dry run), or `failed`.
    pub action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Safely reconcile stale/suspect/unreconciled active runs without manual state
/// edits (#5682). Each candidate is cancelled through the lifecycle cancel path,
/// which terminates a still-live owner process tree only when one actually
/// exists and otherwise just marks the orphaned `running` record cancelled —
/// the exact safe operation an operator would otherwise be tempted to do by
/// hand-editing run JSON.
///
/// Genuinely-active runs (live owner/runner with a fresh heartbeat, or queued
/// work) are never touched. With `dry_run`, candidates are reported but no
/// record is mutated so an operator can preview the blast radius first.
pub fn reconcile_stale_active_runs(dry_run: bool) -> Result<AgentTaskReconcileReport> {
    let report = discover_runs(AgentTaskDiscoveryFilter::Active)?;

    let mut runs = Vec::new();
    let mut reconciled = 0usize;
    let mut failed = 0usize;

    for run in report.runs {
        let Some(liveness) = run.liveness else {
            continue;
        };
        if !liveness.is_reconcilable() {
            continue;
        }

        if dry_run {
            runs.push(AgentTaskReconcileRun {
                run_id: run.run_id,
                liveness,
                source: run.source,
                stale_reason: run.stale_reason,
                action: "would-reconcile",
                error: None,
            });
            continue;
        }

        let reason = run
            .stale_reason
            .clone()
            .unwrap_or_else(|| format!("reconciled stale-{} run", liveness.as_str()));
        match agent_task_lifecycle::cancel_run(&run.run_id, Some(&reason)) {
            Ok(_) => {
                reconciled += 1;
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    stale_reason: run.stale_reason,
                    action: "reconciled",
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                runs.push(AgentTaskReconcileRun {
                    run_id: run.run_id,
                    liveness,
                    source: run.source,
                    stale_reason: run.stale_reason,
                    action: "failed",
                    error: Some(error.message),
                });
            }
        }
    }

    Ok(AgentTaskReconcileReport {
        schema: "homeboy/agent-task-reconcile/v1",
        dry_run,
        considered: runs.len(),
        reconciled,
        failed,
        runs,
    })
}

/// Classify an active run's liveness from the durable record's already-computed
/// stale flag plus a heartbeat-age fallback. The lifecycle layer
/// (`annotate_stale_running`) is authoritative for disproven liveness (dead
/// owner pid, unverified runner job). This adds a heartbeat-age signal so a
/// Lab/offloaded run whose runner died silently — leaving liveness merely
/// *unverifiable* rather than *disproven* — still surfaces as suspect (#5682).
fn classify_liveness(
    record: &AgentTaskRunRecord,
    last_update_age_minutes: Option<i64>,
) -> AgentTaskLiveness {
    if record.state != agent_task_lifecycle::AgentTaskRunState::Running {
        // Queued runs are genuinely pending work, not stale.
        return AgentTaskLiveness::Active;
    }

    if metadata_bool(&record.metadata, "stale_running") == Some(true) {
        return AgentTaskLiveness::Stale;
    }

    let stale_by_age =
        last_update_age_minutes.is_some_and(|age| age >= STALE_UPDATE_THRESHOLD_MINUTES);

    let has_owner_signal =
        record.metadata.get("runner_pid").is_some() || record.metadata.get("runner_id").is_some();

    match (stale_by_age, has_owner_signal) {
        (true, _) => AgentTaskLiveness::Suspect,
        (false, true) => AgentTaskLiveness::Active,
        // No disproven liveness, no recent heartbeat, no owner signal at all:
        // we genuinely cannot confirm this run either way.
        (false, false) => AgentTaskLiveness::Unreconciled,
    }
}

/// Label where a run executes so an operator can trace the runner process.
fn run_source(record: &AgentTaskRunRecord) -> String {
    if let Some(runner_id) =
        metadata_string(&record.metadata, "runner_id").filter(|id| !id.trim().is_empty())
    {
        return format!("runner:{runner_id}");
    }
    if record.metadata.get("remote_run_id").is_some()
        || record.metadata.get("runner_job_id").is_some()
        || record.metadata.get("job_id").is_some()
    {
        return "remote".to_string();
    }
    "local".to_string()
}

/// Age in whole minutes between `timestamp` (RFC3339) and `now`, clamped to
/// non-negative. `None` when the timestamp is absent or unparseable.
fn age_minutes(timestamp: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Option<i64> {
    let raw = timestamp?;
    let parsed = chrono::DateTime::parse_from_rfc3339(raw).ok()?;
    let minutes = now
        .signed_duration_since(parsed.with_timezone(&chrono::Utc))
        .num_minutes();
    Some(minutes.max(0))
}

fn discovery_run(
    record: AgentTaskRunRecord,
    classify: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> AgentTaskDiscoveryRun {
    let plan = agent_task_lifecycle::load_plan(&record.run_id).ok();
    let first_task = plan.as_ref().and_then(|plan| plan.tasks.first());
    let repo = plan
        .as_ref()
        .and_then(|plan| plan.group_key.clone())
        .or_else(|| first_task.and_then(|task| task.group_key.clone()))
        .or_else(|| first_task.and_then(|task| task.workspace.component_id.clone()))
        .or_else(|| first_task.and_then(|task| task.workspace.slug.clone()));
    let workspace = first_task
        .and_then(|task| task.workspace.root.clone())
        .or_else(|| metadata_string(&record.metadata, "remote_workspace"));
    let task_url = first_task
        .and_then(|task| task.workspace.task_url.clone())
        .or_else(|| first_task.and_then(task_source_url));
    let aggregate_path = record.aggregate_path.clone();
    let run_id = record.run_id.clone();

    let last_update = record.updated_at.clone();
    let last_update_age_minutes = age_minutes(last_update.as_deref(), now);
    let source = run_source(&record);
    let liveness = classify.then(|| classify_liveness(&record, last_update_age_minutes));

    // A runner-backed run's durable record (status/logs/artifacts/review) lives
    // on the runner, so the emitted recovery commands must be runner-scoped to
    // resolve against the run's actual location. Local runs use the bare
    // command. This keeps "commands emitted in run metadata valid for the run
    // location" (#5681).
    let runner_id = metadata_string(&record.metadata, "runner_id")
        .filter(|runner_id| !runner_id.trim().is_empty());
    let command_prefix = match runner_id.as_deref() {
        Some(runner_id) => format!("homeboy --runner {runner_id} agent-task"),
        None => "homeboy agent-task".to_string(),
    };

    AgentTaskDiscoveryRun {
        run_id: run_id.clone(),
        state: record.state,
        repo,
        workspace,
        task_url,
        counts: discovery_counts(&record.tasks),
        submitted_at: record.submitted_at,
        updated_at: record.updated_at,
        runner_id: metadata_string(&record.metadata, "runner_id"),
        runner_job_id: metadata_string(&record.metadata, "runner_job_id")
            .or_else(|| metadata_string(&record.metadata, "job_id")),
        remote_run_id: metadata_string(&record.metadata, "remote_run_id"),
        stale: metadata_bool(&record.metadata, "stale_running"),
        stale_reason: metadata_string(&record.metadata, "stale_running_reason"),
        retryable: metadata_bool(&record.metadata, "retryable"),
        liveness,
        source,
        last_update,
        last_update_age_minutes,
        commands: AgentTaskDiscoveryCommands {
            status: format!("{command_prefix} status {run_id}"),
            logs: format!("{command_prefix} logs {run_id}"),
            artifacts: format!("{command_prefix} artifacts {run_id}"),
            review: format!("{command_prefix} review {run_id}"),
            retry: format!("{command_prefix} retry {run_id} --run"),
            run_plan: format!(
                "homeboy --runner <runner-id> agent-task run-plan --plan @{} --record-run-id <new-run-id>",
                record.plan_path
            ),
            promote: aggregate_path
                .map(|path| format!("homeboy agent-task promote {path} --to-worktree <handle>"))
                .unwrap_or_else(|| format!("{command_prefix} review {run_id}")),
            reconcile: format!("{command_prefix} cancel {run_id} --reason stale-running"),
        },
    }
}

fn discovery_counts(tasks: &[agent_task_lifecycle::AgentTaskRunTask]) -> AgentTaskDiscoveryCounts {
    let mut counts = AgentTaskDiscoveryCounts::default();
    for task in tasks {
        match task.state {
            AgentTaskState::Queued | AgentTaskState::Blocked | AgentTaskState::Skipped => {
                counts.queued += 1;
            }
            AgentTaskState::Running => counts.running += 1,
            AgentTaskState::Succeeded | AgentTaskState::Cancelled => counts.completed += 1,
            AgentTaskState::Failed | AgentTaskState::TimedOut => counts.failed += 1,
        }
    }
    counts
}

fn task_source_url(task: &AgentTaskRequest) -> Option<String> {
    task.source_refs
        .iter()
        .find(|source| source.kind == "task")
        .or_else(|| task.source_refs.first())
        .map(source_uri)
}

fn source_uri(source: &AgentTaskSourceRef) -> String {
    source.uri.clone()
}

fn metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn metadata_bool(metadata: &Value, key: &str) -> Option<bool> {
    metadata.get(key).and_then(Value::as_bool)
}

pub fn run_cook_loop<E>(
    options: AgentTaskLoopServiceOptions,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskLoopReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let max_attempts = options.max_attempts.max(1);
    let mut attempts = Vec::new();
    let mut run_id = options.initial_run_id.clone();
    let loop_id = options.loop_id.clone();

    for attempt in 1..=max_attempts {
        let record = agent_task_lifecycle::status(&run_id)?;
        let plan = agent_task_lifecycle::load_plan(&run_id)?;
        let Some(source_request) = plan.tasks.first().cloned() else {
            return Ok(loop_report(
                loop_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task loop requires a plan with one source task".to_string()),
                1,
            ));
        };
        if plan.tasks.len() != 1 {
            return Ok(loop_report(
                loop_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task loop currently supports one task per cook attempt".to_string()),
                1,
            ));
        }

        if !matches!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        ) {
            attempts.push(AgentTaskLoopAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(loop_report(
                loop_id,
                "provider_failure",
                attempts,
                None,
                Some(format!(
                    "agent-task run {run_id} ended in state {:?}",
                    record.state
                )),
                1,
            ));
        }

        let promotion = match promote_attempt(&options, &run_id) {
            Ok(report) => report,
            Err(error) => {
                attempts.push(AgentTaskLoopAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path,
                    promotion: None,
                    feedback: None,
                });
                return Ok(loop_report(
                    loop_id,
                    "policy_failure",
                    attempts,
                    None,
                    Some(error.to_string()),
                    1,
                ));
            }
        };

        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion.clone(),
            attempt,
            max_attempts,
            source_run_id: Some(run_id.clone()),
            current_diff: String::new(),
            metadata: Value::Null,
        });
        let feedback_status = feedback.status;
        let follow_up_request = feedback.follow_up_request.clone();
        attempts.push(AgentTaskLoopAttemptReport {
            attempt,
            run_id: run_id.clone(),
            run_state: format!("{:?}", record.state),
            aggregate_path: record.aggregate_path,
            promotion: Some(promotion.clone()),
            feedback: Some(feedback.clone()),
        });

        match feedback_status {
            AgentTaskCookLoopStatus::GreenCompleted => {
                if options.no_finalize {
                    return Ok(loop_report(
                        loop_id,
                        "green_no_finalize",
                        attempts,
                        None,
                        Some(
                            "deterministic gates completed green; --no-finalize skipped commit, push, and PR finalization"
                                .to_string(),
                        ),
                        0,
                    ));
                }
                let finalization = finalize_loop_pr(&options, &loop_id, &promotion)?;
                let final_status = finalization["status"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let exit_code = if final_status == "review_ready" { 0 } else { 1 };
                let stop_reason = (final_status == "no_changes").then(|| {
                    "cook completed provider execution and gates, but finalization found no changed files; task likely still requires review or retry".to_string()
                });
                return Ok(loop_report(
                    loop_id,
                    &final_status,
                    attempts,
                    Some(finalization),
                    stop_reason,
                    exit_code,
                ));
            }
            AgentTaskCookLoopStatus::NoChanges => {
                return Ok(loop_report(
                    loop_id,
                    "no_changes",
                    attempts,
                    None,
                    Some(
                        "cook completed provider execution but produced no changed files; task likely still requires review or retry"
                            .to_string(),
                    ),
                    1,
                ));
            }
            AgentTaskCookLoopStatus::RetryRequested => {
                let Some(follow_up_request) = follow_up_request else {
                    return Ok(loop_report(
                        loop_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(
                            "cook-loop feedback requested retry without a follow-up request"
                                .to_string(),
                        ),
                        1,
                    ));
                };
                let next_run_id = format!("{loop_id}-attempt-{}", attempt + 1);
                let follow_up_plan = AgentTaskPlan::new(
                    format!("{loop_id}-cook-loop-attempt-{}", attempt + 1),
                    vec![follow_up_request],
                );
                run_loaded_plan(follow_up_plan, Some(&next_run_id), executor.clone())?;
                run_id = next_run_id;
            }
            AgentTaskCookLoopStatus::RetriesExhausted => {
                return Ok(loop_report(
                    loop_id,
                    "retries_exhausted",
                    attempts,
                    None,
                    Some(
                        "deterministic gates stayed red after the configured attempt budget"
                            .to_string(),
                    ),
                    1,
                ));
            }
        }
    }

    Ok(loop_report(
        loop_id,
        "retries_exhausted",
        attempts,
        None,
        Some("cook-loop attempt budget exhausted".to_string()),
        1,
    ))
}

pub fn promotion_source(spec: &str) -> Result<(String, Option<PathBuf>)> {
    if spec != "-" {
        let path = PathBuf::from(spec.strip_prefix('@').unwrap_or(spec));
        if path.is_file() {
            let raw = std::fs::read_to_string(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "read agent-task promotion source {}",
                        path.display()
                    )),
                )
            })?;
            return Ok((raw, Some(path)));
        }
    }

    if let Ok((raw, path)) = agent_task_lifecycle::aggregate_source(spec) {
        return Ok((raw, Some(path)));
    }

    Ok((
        config::read_json_spec_to_string(spec)?,
        source_spec_path(spec),
    ))
}

pub fn run_loaded_plan<E>(
    mut plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    prepare_plan_for_execution(&mut plan, record_run_id)?;

    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::mark_running(run_id)?;
    }

    let aggregate = run_plan_with_scheduler(plan.clone(), executor);
    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
    }
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: aggregate,
    })
}

pub fn submit_plan_spec(spec: &str, run_id: Option<&str>) -> Result<AgentTaskRunRecord> {
    let plan = read_plan(spec)?;
    agent_task_lifecycle::submit_plan(&plan, run_id)
}

pub fn run_submitted<E>(
    run_id: String,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    let mut plan = agent_task_lifecycle::load_plan(&run_id)?;
    prepare_plan_for_execution(&mut plan, Some(&run_id))?;
    agent_task_lifecycle::mark_running(&run_id)?;
    run_prepared_claimed(run_id, plan, executor)
}

pub fn run_next<E>(executor: E) -> Result<AgentTaskRunResult<Option<AgentTaskAggregate>>>
where
    E: AgentTaskExecutorAdapter,
{
    let Some(record) = agent_task_lifecycle::claim_next_queued_run()? else {
        return Ok(AgentTaskRunResult {
            value: None,
            exit_code: 0,
        });
    };

    let result = run_claimed(record.run_id, executor)?;
    Ok(AgentTaskRunResult {
        value: Some(result.value),
        exit_code: result.exit_code,
    })
}

pub fn resume<E>(run_id: String, executor: E) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_resuming(&run_id)?;
    run_claimed(run_id, executor)
}

pub fn retry(
    run_id: &str,
    new_run_id: Option<&str>,
    run: bool,
) -> Result<AgentTaskRetryServiceResult> {
    let record = agent_task_lifecycle::retry(run_id, new_run_id)?;
    Ok(AgentTaskRetryServiceResult { record, run })
}

#[derive(Debug, Clone)]
pub struct AgentTaskRetryServiceResult {
    pub record: AgentTaskRunRecord,
    pub run: bool,
}

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    agent_task_lifecycle::status(run_id)
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    agent_task_lifecycle::logs(run_id)
}

pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    agent_task_lifecycle::artifacts(run_id)
}

pub fn cancel(run_id: &str, reason: Option<&str>) -> Result<AgentTaskRunRecord> {
    agent_task_lifecycle::cancel_run(run_id, reason)
}

pub fn normalize_plan_workspaces(plan: &mut AgentTaskPlan) -> Result<()> {
    for request in &mut plan.tasks {
        normalize_component_worktree_workspace(request)?;
    }

    Ok(())
}

fn run_claimed<E>(run_id: String, executor: E) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    let mut plan = agent_task_lifecycle::load_plan(&run_id)?;
    prepare_plan_for_execution(&mut plan, Some(&run_id))?;
    run_prepared_claimed(run_id, plan, executor)
}

fn run_prepared_claimed<E>(
    run_id: String,
    plan: AgentTaskPlan,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    let aggregate = run_plan_with_scheduler(plan.clone(), executor);
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: aggregate,
    })
}

fn prepare_plan_for_execution(plan: &mut AgentTaskPlan, run_id: Option<&str>) -> Result<()> {
    prepare_plan_workspaces(plan, run_id)?;
    apply_provider_runner_secret_env_contracts(plan);
    preflight_plan_secret_env(plan)
}

fn prepare_plan_workspaces(plan: &mut AgentTaskPlan, run_id: Option<&str>) -> Result<()> {
    for request in &mut plan.tasks {
        prepare_component_worktree_workspace(request, run_id)?;
    }

    Ok(())
}

fn preflight_plan_secret_env(plan: &AgentTaskPlan) -> Result<()> {
    let secret_env_plan = SecretEnvPlan::from_secret_env_names(
        plan.tasks
            .iter()
            .flat_map(|task| task.executor.secret_env.iter().cloned()),
    );

    validate_secret_env_with_fallbacks(
        &secret_env_plan.secret_env_names(),
        &provider_secret_sources_for_plan(plan),
    )
    .map_err(|error| {
        Error::validation_invalid_argument(
            "secret_env",
            error.message,
            None,
            Some(vec![
                "Agent-task executor provider manifests can declare runner-required secret env contracts; Homeboy validates those contracts before task execution.".to_string(),
                "For local execution, configure provider credentials with `homeboy agent-task auth map-env`, `set-keychain`, or `set-keychain-bundle`.".to_string(),
                "For delegated runner execution, configure the selected runner's secret_env references so the runner receives these names without printing values.".to_string(),
            ]),
        )
    })
}

fn run_plan_with_scheduler<E>(plan: AgentTaskPlan, executor: E) -> AgentTaskAggregate
where
    E: AgentTaskExecutorAdapter,
{
    AgentTaskScheduler::new(executor).run(plan)
}

pub fn aggregate_exit_code(aggregate: &AgentTaskAggregate) -> i32 {
    if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    }
}

fn promote_attempt(
    options: &AgentTaskLoopServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let (source, source_path) = promotion_source(run_id)?;
    promote(AgentTaskPromotionOptions {
        source,
        source_run_id: Some(run_id.to_string()),
        source_path,
        to_worktree: options.to_worktree.clone(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: options.gates.clone(),
        provider_command: options.provider_command.clone(),
    })
}

fn finalize_loop_pr(
    options: &AgentTaskLoopServiceOptions,
    loop_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> Result<Value> {
    if promotion.status != AgentTaskPromotionStatus::Applied {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "agent-task loop finalization requires an applied promotion with green gates",
            None,
            None,
        ));
    }
    let path = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "promotion provider did not report the applied worktree path",
                None,
                None,
            )
        })?
        .to_string();
    let source_refs = options
        .source_refs
        .iter()
        .cloned()
        .chain(std::iter::once(format!(
            "homeboy://agent-task/run/{loop_id}"
        )))
        .collect();
    let artifact_refs = std::iter::once(promotion.patch_artifact.path.clone()).collect();
    let report = finalize_pr(AgentTaskPrFinalizationOptions {
        path,
        run_id: loop_id.to_string(),
        base: options.base.clone(),
        head: options.head.clone(),
        title: options.title.clone(),
        commit_message: options.commit_message.clone(),
        gate_results: Vec::new(),
        normalized_gate_results: promotion.gate_results.clone(),
        changed_files: promotion.changed_files.clone(),
        evidence: AgentTaskPrEvidence {
            source_refs,
            artifact_refs,
            attempt_summary: format!(
                "{} deterministic cook-loop gate attempt(s) completed green",
                promotion.deterministic_gates.len()
            ),
            ai_tool: options.ai_tool.clone(),
            ai_model: options.ai_model.clone(),
            source_relationship: AgentTaskPrSourceRelationship::default(),
            verification: AgentTaskPrVerification {
                targeted_checks_run: options.gates.verify.clone(),
                targeted_checks_unavailable: None,
                ci_expected: vec!["Homeboy CI after push".to_string()],
                manual_reviewer_check: None,
            },
            runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
            lifecycle: crate::core::agent_task_lifecycle::status(loop_id)
                .ok()
                .map(|record| record.lifecycle),
        },
        ai_used_for: options.ai_used_for.clone(),
        protected_branches: options.protected_branches.clone(),
    })?;
    Ok(serde_json::to_value(report).unwrap_or(Value::Null))
}

fn loop_report(
    loop_id: String,
    status: &str,
    attempts: Vec<AgentTaskLoopAttemptReport>,
    finalization: Option<Value>,
    stop_reason: Option<String>,
    exit_code: i32,
) -> AgentTaskRunResult<AgentTaskLoopReport> {
    AgentTaskRunResult {
        value: AgentTaskLoopReport {
            schema: "homeboy/agent-task-loop/v1",
            loop_id,
            status: status.to_string(),
            attempts,
            finalization,
            stop_reason,
        },
        exit_code,
    }
}

fn source_spec_path(spec: &str) -> Option<PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(PathBuf::from(spec.strip_prefix('@').unwrap_or(spec)))
}

fn normalize_component_worktree_workspace(request: &mut AgentTaskRequest) -> Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }

    let Some(component_id) = request.workspace.component_id.clone() else {
        return Err(Error::validation_invalid_argument(
            "workspace.component_id",
            format!(
                "agent-task task '{}' component-worktree workspace requires component_id",
                request.task_id
            ),
            None,
            None,
        ));
    };

    let resolved_root = request
        .workspace
        .root
        .clone()
        .or_else(|| materialization_string(&request.workspace.materialization, "root"))
        .or_else(|| materialization_string(&request.workspace.materialization, "resolved_root"));

    let Some(root) = resolved_root else {
        return Ok(());
    };

    request.workspace.kind = None;
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
    request.workspace.root = Some(root);
    request.workspace.slug = Some(component_id);
    request.workspace.component_id = None;
    request.workspace.branch = None;
    request.workspace.base_ref = None;
    request.workspace.task_url = None;
    request.workspace.cleanup = None;
    request.workspace.materialization = Value::Null;

    Ok(())
}

fn prepare_component_worktree_workspace(
    request: &mut AgentTaskRequest,
    run_id: Option<&str>,
) -> Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }
    if request.workspace.root.is_some()
        || materialization_string(&request.workspace.materialization, "root").is_some()
        || materialization_string(&request.workspace.materialization, "resolved_root").is_some()
    {
        return normalize_component_worktree_workspace(request);
    }

    let component_id = request.workspace.component_id.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace.component_id",
            format!(
                "agent-task task '{}' component-worktree workspace requires component_id",
                request.task_id
            ),
            None,
            None,
        )
    })?;
    let branch = request.workspace.branch.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace.branch",
            format!(
                "agent-task task '{}' component-worktree workspace for component '{}' requires branch",
                request.task_id, component_id
            ),
            None,
            None,
        )
    })?;
    let cleanup_policy = cleanup_policy_for_workspace(request.workspace.cleanup.as_deref());
    let created = worktree::create(worktree::WorktreeCreateOptions {
        component_id: component_id.clone(),
        branch,
        from: request.workspace.base_ref.clone(),
        task_url: request.workspace.task_url.clone().or_else(|| {
            request
                .source_refs
                .iter()
                .find(|source| source.kind == "task")
                .or_else(|| request.source_refs.first())
                .map(source_uri)
        }),
        run_id: run_id.map(str::to_string),
        cleanup_policy,
    })?;
    let record = created.record;
    let cleanup = cleanup_lifecycle_policy(&record.cleanup_policy);
    request.workspace.kind = None;
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
    request.workspace.root = Some(record.worktree_path.clone());
    request.workspace.slug = Some(component_id);
    request.workspace.component_id = None;
    request.workspace.branch = None;
    request.workspace.base_ref = None;
    request.workspace.task_url = None;
    request.workspace.cleanup = Some(cleanup.to_string());
    request.workspace.materialization = serde_json::json!({
        "kind": "homeboy-worktree",
        "id": record.id,
        "component_id": record.component_id,
        "branch": record.branch,
        "base_ref": record.base_ref,
        "root": record.worktree_path,
        "source_checkout": record.source_checkout,
        "task_url": record.task_url,
        "run_id": record.run_id,
        "cleanup_policy": cleanup,
    });

    Ok(())
}

fn cleanup_policy_for_workspace(value: Option<&str>) -> Option<worktree::CleanupPolicy> {
    match value {
        Some("remove_when_safe") | Some("remove-when-safe") | Some("cleanup") => {
            Some(worktree::CleanupPolicy::RemoveWhenSafe)
        }
        Some("preserve") | Some("preserve_on_failure") | Some("preserve-on-failure") => {
            Some(worktree::CleanupPolicy::PreserveOnFailure)
        }
        _ => None,
    }
}

fn cleanup_lifecycle_policy(policy: &worktree::CleanupPolicy) -> &'static str {
    match policy {
        worktree::CleanupPolicy::RemoveWhenSafe => "remove_when_safe",
        worktree::CleanupPolicy::PreserveOnFailure => "preserve",
    }
}

fn materialization_string(materialization: &Value, key: &str) -> Option<String> {
    materialization
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskSourceRef, AgentTaskWorkspace,
        AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_lifecycle::{status as lifecycle_status, AgentTaskRunState};
    use crate::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskState};
    use crate::test_support::with_isolated_home;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    #[test]
    fn service_run_loaded_plan_persists_durable_lifecycle() {
        with_isolated_home(|_| {
            let result = run_loaded_plan(test_plan(), Some("service-run"), SucceedingExecutor)
                .expect("service run completed");
            let record = lifecycle_status("service-run").expect("status persisted");

            assert_eq!(result.exit_code, 0);
            assert_eq!(record.state, AgentTaskRunState::Succeeded);
            assert_eq!(record.tasks[0].state, AgentTaskState::Succeeded);
            assert!(record.aggregate_path.is_some());
        });
    }

    #[test]
    fn service_normalizes_resolved_component_worktree_plan() {
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("homeboy".to_string());
        plan.tasks[0].workspace.materialization = serde_json::json!({
            "resolved_root": "/tmp/homeboy@service"
        });

        normalize_plan_workspaces(&mut plan).expect("workspace normalized");

        assert!(plan.tasks[0].workspace.kind.is_none());
        assert_eq!(plan.tasks[0].workspace.slug.as_deref(), Some("homeboy"));
        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some("/tmp/homeboy@service")
        );
        assert_eq!(
            plan.tasks[0].workspace.mode,
            AgentTaskWorkspaceMode::Existing
        );
        assert!(plan.tasks[0].workspace.materialization.is_null());
    }

    #[test]
    fn service_materializes_component_worktree_before_provider_dispatch() {
        with_isolated_home(|home| {
            let repo = home.path().join("fixture");
            create_git_repo(&repo);
            write_component_registration(home.path(), "fixture", &repo);
            let observed_request = Arc::new(Mutex::new(None));
            let mut plan = test_plan();
            plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
            plan.tasks[0].workspace.component_id = Some("fixture".to_string());
            plan.tasks[0].workspace.branch = Some("fix/service-task".to_string());
            plan.tasks[0].workspace.base_ref = Some("HEAD".to_string());
            plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
            plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
                kind: "task".to_string(),
                uri: "https://example.com/tasks/123".to_string(),
                revision: None,
            }];

            let result = run_loaded_plan(
                plan,
                Some("service-materialized-worktree"),
                CapturingExecutor {
                    observed_request: Arc::clone(&observed_request),
                },
            )
            .expect("run-plan completed");
            let observed = observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("provider saw request");
            let record = worktree::resolve("fixture@fix-service-task").expect("worktree record");

            assert_eq!(result.exit_code, 0);
            assert_eq!(
                record.run_id.as_deref(),
                Some("service-materialized-worktree")
            );
            assert_eq!(
                record.task_url.as_deref(),
                Some("https://example.com/tasks/123")
            );
            assert_eq!(
                record.cleanup_policy,
                worktree::CleanupPolicy::PreserveOnFailure
            );
            assert_eq!(observed.workspace.mode, AgentTaskWorkspaceMode::Existing);
            assert_eq!(
                observed.workspace.root.as_deref(),
                Some(record.worktree_path.as_str())
            );
            assert_eq!(observed.workspace.slug.as_deref(), Some("fixture"));
            assert!(observed.workspace.kind.is_none());
            assert!(observed.workspace.component_id.is_none());
            assert_eq!(observed.workspace.cleanup.as_deref(), Some("preserve"));
            assert_eq!(
                observed.workspace.materialization["id"].as_str(),
                Some("fixture@fix-service-task")
            );
            assert!(Path::new(&record.worktree_path).is_dir());
        });
    }

    #[test]
    fn discovery_lists_durable_runs_with_operator_commands() {
        with_isolated_home(|_| {
            let plan = discovery_plan();
            agent_task_lifecycle::submit_plan(&plan, Some("run-discovery-list"))
                .expect("submitted");

            let report = discover_runs(AgentTaskDiscoveryFilter::All).expect("listed");
            let run = report.runs.first().expect("run");

            assert_eq!(report.schema, "homeboy/agent-task-discovery/v1");
            assert_eq!(report.filter, "all");
            assert_eq!(report.count, 1);
            assert_eq!(report.total, 1);
            assert!(!report.truncated);
            assert!(report.limit.is_none());
            assert!(report
                .lab_discovery
                .runner_scoped_command
                .contains("--runner"));
            assert!(report.lab_discovery.fallback_command.contains("runs list"));
            assert_eq!(run.run_id, "run-discovery-list");
            assert_eq!(run.state, AgentTaskRunState::Queued);
            assert_eq!(run.repo.as_deref(), Some("homeboy"));
            assert_eq!(run.workspace.as_deref(), Some("/tmp/homeboy"));
            assert_eq!(
                run.task_url.as_deref(),
                Some("https://github.com/Extra-Chill/homeboy/issues/4386")
            );
            assert_eq!(run.counts.queued, 1);
            assert_eq!(
                run.commands.status,
                "homeboy agent-task status run-discovery-list"
            );
            assert_eq!(
                run.commands.logs,
                "homeboy agent-task logs run-discovery-list"
            );
            assert_eq!(
                run.commands.artifacts,
                "homeboy agent-task artifacts run-discovery-list"
            );
            assert_eq!(
                run.commands.review,
                "homeboy agent-task review run-discovery-list"
            );
            assert_eq!(
                run.commands.retry,
                "homeboy agent-task retry run-discovery-list --run"
            );
            assert!(run
                .commands
                .run_plan
                .contains("homeboy --runner <runner-id> agent-task run-plan --plan @"));
            assert!(run
                .commands
                .run_plan
                .contains("/agent-task-runs/run-discovery-list/plan.json"));
        });
    }

    #[test]
    fn discovery_active_filters_to_queued_and_running_runs() {
        with_isolated_home(|_| {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-active-queued"))
                .expect("queued submitted");
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-active-running"))
                .expect("running submitted");
            agent_task_lifecycle::mark_running("run-active-running").expect("marked running");
            run_loaded_plan(
                discovery_plan(),
                Some("run-active-complete"),
                SucceedingExecutor,
            )
            .expect("completed");

            let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");
            let run_ids: Vec<_> = report.runs.iter().map(|run| run.run_id.as_str()).collect();

            assert_eq!(report.filter, "active");
            assert_eq!(report.count, 2);
            assert!(run_ids.contains(&"run-active-queued"));
            assert!(run_ids.contains(&"run-active-running"));
            assert!(!run_ids.contains(&"run-active-complete"));
        });
    }

    #[test]
    fn discovery_active_marks_runner_backed_running_run_as_stale_retryable() {
        with_isolated_home(|_| {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-runner-stale"))
                .expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test("run-runner-stale", |record| {
                record.state = AgentTaskRunState::Running;
                record.tasks[0].state = AgentTaskState::Running;
                record.metadata = serde_json::json!({
                    "runner_id": "homeboy-lab",
                    "runner_job_id": "job-123",
                });
            })
            .expect("running runner-backed record stored");

            let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");
            let run = report
                .runs
                .iter()
                .find(|run| run.run_id == "run-runner-stale")
                .expect("runner-backed run listed");

            assert_eq!(run.runner_id.as_deref(), Some("homeboy-lab"));
            assert_eq!(run.runner_job_id.as_deref(), Some("job-123"));
            assert_eq!(run.stale, Some(true));
            assert_eq!(
                run.stale_reason.as_deref(),
                Some("runner_job_unverified_after_daemon_restart")
            );
            assert_eq!(run.retryable, Some(true));
        });
    }

    #[test]
    fn discovery_active_classifies_liveness_and_source() {
        with_isolated_home(|_| {
            // Queued run: always classified active.
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-live-queued"))
                .expect("queued submitted");

            // Stale runner-backed run: lifecycle flags it stale -> Stale.
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-live-stale"))
                .expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test("run-live-stale", |record| {
                record.state = AgentTaskRunState::Running;
                record.tasks[0].state = AgentTaskState::Running;
                record.metadata = serde_json::json!({
                    "runner_id": "homeboy-lab",
                    "runner_job_id": "job-xyz",
                });
            })
            .expect("stale runner-backed record stored");

            let report = discover_runs(AgentTaskDiscoveryFilter::Active).expect("active listed");

            let queued = report
                .runs
                .iter()
                .find(|run| run.run_id == "run-live-queued")
                .expect("queued listed");
            assert_eq!(queued.liveness, Some(AgentTaskLiveness::Active));
            assert_eq!(queued.source, "local");

            let stale = report
                .runs
                .iter()
                .find(|run| run.run_id == "run-live-stale")
                .expect("stale listed");
            assert_eq!(stale.liveness, Some(AgentTaskLiveness::Stale));
            assert_eq!(stale.source, "runner:homeboy-lab");

            let summary = report.liveness_summary.expect("active summary present");
            assert!(summary.active >= 1);
            assert_eq!(summary.stale, 1);
            assert_eq!(
                summary.reconcile_command,
                "homeboy agent-task active --reconcile"
            );
        });
    }

    #[test]
    fn reconcile_dry_run_reports_but_does_not_cancel_stale_runs() {
        with_isolated_home(|_| {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-reconcile-dry"))
                .expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test("run-reconcile-dry", |record| {
                record.state = AgentTaskRunState::Running;
                record.tasks[0].state = AgentTaskState::Running;
                record.metadata = serde_json::json!({
                    "runner_id": "homeboy-lab",
                    "runner_job_id": "job-dry",
                });
            })
            .expect("stale record stored");

            let report = reconcile_stale_active_runs(true).expect("dry run reconciled");
            assert!(report.dry_run);
            assert_eq!(report.reconciled, 0);
            assert_eq!(report.considered, 1);
            assert_eq!(report.runs[0].action, "would-reconcile");

            // Record must remain running after a dry run.
            let record = lifecycle_status("run-reconcile-dry").expect("status");
            assert_eq!(record.state, AgentTaskRunState::Running);
        });
    }

    #[test]
    fn reconcile_cancels_stale_running_record_without_manual_edit() {
        with_isolated_home(|_| {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-reconcile-live"))
                .expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test("run-reconcile-live", |record| {
                record.state = AgentTaskRunState::Running;
                record.tasks[0].state = AgentTaskState::Running;
                record.metadata = serde_json::json!({
                    "runner_id": "homeboy-lab",
                    "runner_job_id": "job-live",
                });
            })
            .expect("stale record stored");

            let report = reconcile_stale_active_runs(false).expect("reconciled");
            assert!(!report.dry_run);
            assert_eq!(report.reconciled, 1);
            assert_eq!(report.failed, 0);
            assert_eq!(report.runs[0].action, "reconciled");

            let record = lifecycle_status("run-reconcile-live").expect("status");
            assert_eq!(record.state, AgentTaskRunState::Cancelled);

            // A genuinely-active run reconcile pass leaves nothing to do.
            let empty = reconcile_stale_active_runs(false).expect("nothing to reconcile");
            assert_eq!(empty.considered, 0);
            assert_eq!(empty.reconciled, 0);
        });
    }

    #[test]
    fn discovery_latest_returns_only_newest_run() {
        with_isolated_home(|_| {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-a"))
                .expect("first submitted");
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-z"))
                .expect("second submitted");

            let report = discover_runs(AgentTaskDiscoveryFilter::Latest).expect("latest listed");

            assert_eq!(report.filter, "latest");
            assert_eq!(report.count, 1);
            assert_eq!(report.runs[0].run_id, "run-latest-z");
        });
    }

    #[test]
    fn discovery_limit_caps_list_and_reports_total() {
        with_isolated_home(|_| {
            for run_id in ["run-cap-a", "run-cap-b", "run-cap-c"] {
                agent_task_lifecycle::submit_plan(&discovery_plan(), Some(run_id))
                    .expect("submitted");
            }

            let report = discover_runs_with_options(
                AgentTaskDiscoveryFilter::All,
                AgentTaskDiscoveryOptions { limit: Some(2) },
            )
            .expect("listed with limit");

            assert_eq!(report.count, 2);
            assert_eq!(report.total, 3);
            assert_eq!(report.limit, Some(2));
            assert!(report.truncated);
            assert_eq!(report.runs.len(), 2);
        });
    }

    #[test]
    fn discovery_latest_ignores_limit() {
        with_isolated_home(|_| {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-limit-a"))
                .expect("submitted");
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-latest-limit-z"))
                .expect("submitted");

            let report = discover_runs_with_options(
                AgentTaskDiscoveryFilter::Latest,
                AgentTaskDiscoveryOptions { limit: Some(5) },
            )
            .expect("latest listed");

            // `latest` is always a single run; a limit is a no-op and not echoed.
            assert_eq!(report.count, 1);
            assert!(report.limit.is_none());
            assert!(!report.truncated);
        });
    }

    #[test]
    fn discovery_runner_backed_run_emits_runner_scoped_commands() {
        with_isolated_home(|_| {
            agent_task_lifecycle::submit_plan(&discovery_plan(), Some("run-runner-commands"))
                .expect("submitted");
            agent_task_lifecycle::rewrite_record_for_test("run-runner-commands", |record| {
                record.state = AgentTaskRunState::Running;
                record.tasks[0].state = AgentTaskState::Running;
                record.metadata = serde_json::json!({
                    "runner_id": "homeboy-lab",
                });
            })
            .expect("runner-backed record stored");

            let report = discover_runs(AgentTaskDiscoveryFilter::All).expect("listed");
            let run = report
                .runs
                .iter()
                .find(|run| run.run_id == "run-runner-commands")
                .expect("runner-backed run listed");

            // Commands must be valid for the run's location: runner-scoped.
            assert_eq!(
                run.commands.status,
                "homeboy --runner homeboy-lab agent-task status run-runner-commands"
            );
            assert_eq!(
                run.commands.logs,
                "homeboy --runner homeboy-lab agent-task logs run-runner-commands"
            );
            assert_eq!(
                run.commands.review,
                "homeboy --runner homeboy-lab agent-task review run-runner-commands"
            );
            assert_eq!(
                run.commands.reconcile,
                "homeboy --runner homeboy-lab agent-task cancel run-runner-commands --reason stale-running"
            );
        });
    }

    struct SucceedingExecutor;

    impl AgentTaskExecutorAdapter for SucceedingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    struct CapturingExecutor {
        observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
    }

    impl AgentTaskExecutorAdapter for CapturingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            *self
                .observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    fn create_git_repo(path: &Path) {
        std::fs::create_dir_all(path).expect("repo dir");
        run_git(path, &["init", "-q"]);
        run_git(path, &["config", "user.email", "homeboy@example.com"]);
        run_git(path, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(path.join("README.md"), "initial\n").expect("readme");
        run_git(path, &["add", "."]);
        run_git(path, &["commit", "-q", "-m", "initial"]);
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
        let dir = home.join(".config/homeboy/components");
        std::fs::create_dir_all(&dir).expect("components dir");
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::json!({
                "local_path": local_path,
                "remote_path": format!("wp-content/plugins/{id}")
            })
            .to_string(),
        )
        .expect("component registration");
    }

    fn test_plan() -> AgentTaskPlan {
        AgentTaskPlan::new(
            "service-plan",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "service-task".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: Some("service".to_string()),
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "run".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }

    fn discovery_plan() -> AgentTaskPlan {
        let mut plan = test_plan();
        plan.group_key = Some("homeboy".to_string());
        plan.tasks[0].group_key = Some("homeboy".to_string());
        plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
            kind: "task".to_string(),
            uri: "https://github.com/Extra-Chill/homeboy/issues/4386".to_string(),
            revision: None,
        }];
        plan.tasks[0].workspace.root = Some("/tmp/homeboy".to_string());
        plan.tasks[0].workspace.slug = Some("homeboy".to_string());
        plan
    }
}
