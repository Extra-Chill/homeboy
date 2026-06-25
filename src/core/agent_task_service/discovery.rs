//! Agent-task run discovery, liveness classification, and list/active/latest
//! reporting. Pure move out of the former `agent_task_service.rs` god-file.

use crate::core::agent_task::{AgentTaskRequest, AgentTaskSourceRef};
use crate::core::agent_task_lifecycle::{self, AgentTaskRunRecord};
use crate::core::agent_task_scheduler::AgentTaskState;
use crate::core::Result;
use serde_json::Value;

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
    pub(super) fn as_str(self) -> &'static str {
        match self {
            AgentTaskLiveness::Active => "active",
            AgentTaskLiveness::Stale => "stale",
            AgentTaskLiveness::Suspect => "suspect",
            AgentTaskLiveness::Unreconciled => "unreconciled",
        }
    }

    /// Whether this classification is a candidate for safe reconcile/cancel.
    pub(super) fn is_reconcilable(self) -> bool {
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

pub(super) fn source_uri(source: &AgentTaskSourceRef) -> String {
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
