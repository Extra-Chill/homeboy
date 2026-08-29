//! Agent-task run discovery, liveness classification, and list/active/latest
//! reporting. Pure move out of the former `agent_task_service.rs` god-file.

use crate::agent_task::{AgentTaskRequest, AgentTaskSourceRef};
use crate::agent_task_lifecycle::{self, AgentTaskRecordHealthSummary, AgentTaskRunRecord};
use crate::agent_task_scheduler::AgentTaskState;
use homeboy_core::api_jobs::{JobStatus, JobStore};
use std::collections::{BTreeMap, BTreeSet};
// `agent-task active` treats a `Running` record that has gone this long without
// an `updated_at` heartbeat as suspect even when its owner process/runner-job
// liveness cannot be disproven (#5682). Shared with `activity` so the two
// surfaces cannot disagree about what "stale" means.
use homeboy_core::observation::RUNNING_HEARTBEAT_STALE_MINUTES;
use homeboy_core::{Error, Result};
use homeboy_upgrade::upgrade::{
    register_controller_upgrade_admission_provider as register_upgrade_admission_provider,
    ControllerUpgradeAdmission, ControllerUpgradeAdmissionProvider, ControllerUpgradeBlocker,
};
use serde_json::Value;

/// The cross-location surface that federates controller-local records with
/// records resident on connected Lab runners.
pub const FEDERATED_DISCOVERY_COMMAND: &str =
    "homeboy activity list   # federates controller-local records with runner-resident ones";

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTaskDiscoveryOptions {
    /// Maximum number of runs to return, applied after filtering/sorting.
    /// `None` returns every matching run. Ignored for the `latest` filter,
    /// which is always a single run.
    pub limit: Option<usize>,
    /// Zero-based offset into the filtered, newest-first result set.
    pub cursor: usize,
    pub repo: Option<String>,
    pub workspace: Option<String>,
    pub task_url: Option<String>,
    pub submitted_after: Option<String>,
    pub state: Option<String>,
    pub placement: Option<String>,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskDiscoveryReport {
    pub schema: &'static str,
    pub filter: &'static str,
    /// Whether this surface reconciled while reading.
    ///
    /// Discovery is a pure read over the durable records — always `false`.
    /// `agent_task_lifecycle::status()` is by contrast a reconciling read that
    /// *writes*, so it and this list can legitimately report different states
    /// for the same run at the same instant, and calling that one changes what
    /// this one returns next. Emitting the flag lets a consumer tell which kind
    /// of answer it is holding rather than inferring it from the command name
    /// (#W3-15).
    pub reconciled: bool,
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
    /// Offset to pass as `--cursor` for the next distinct page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<usize>,
    pub runs: Vec<AgentTaskDiscoveryRun>,
    /// Bounded health evidence for malformed, legacy, conflicting, or
    /// quarantined lifecycle rows omitted from the typed run projection.
    pub record_health: agent_task_lifecycle::AgentTaskRecordHealthSummary,
    /// Liveness buckets for the `active` filter so operators can separate
    /// genuinely-active runs from stale/suspect/unreconciled records at a
    /// glance. Only populated for the `active` filter; `None` elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness_summary: Option<AgentTaskLivenessSummary>,
    /// The federated surface that *does* see runner-resident runs.
    ///
    /// This field replaces the `lab_discovery` prose hint, which apologised for
    /// a gap instead of closing it: it told operators that a freshly-offloaded
    /// run's durable record lives on the runner and that they should go run a
    /// second, runner-scoped command themselves. `homeboy activity` now
    /// federates connected Lab runners directly, so the correct answer is a
    /// command that already includes them rather than an explanation of why
    /// this one does not (#W3-15, was #5681).
    pub federated_command: &'static str,
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
    /// Every classification, in report order. Exported so a consumer can render
    /// the full bucket set without hardcoding the four names.
    pub const ALL: [AgentTaskLiveness; 4] = [
        AgentTaskLiveness::Active,
        AgentTaskLiveness::Stale,
        AgentTaskLiveness::Suspect,
        AgentTaskLiveness::Unreconciled,
    ];

    /// The wire label for this classification — the same string the enum
    /// serializes to.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentTaskLiveness::Active => "active",
            AgentTaskLiveness::Stale => "stale",
            AgentTaskLiveness::Suspect => "suspect",
            AgentTaskLiveness::Unreconciled => "unreconciled",
        }
    }

    /// Whether this classification is a candidate for safe reconcile/cancel.
    ///
    /// This is the decision-relevant predicate over the classification, and it
    /// is public — and emitted as `liveness_reconcilable` next to `liveness` —
    /// so a consumer holding `liveness: "suspect"` does not have to hardcode
    /// which of the four values mean "you may safely reconcile this" (#W3-4).
    /// The mapping is exactly: `Active` is not reconcilable; `Stale`,
    /// `Suspect`, and `Unreconciled` are.
    pub fn is_reconcilable(self) -> bool {
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
    /// How many of the above are candidates for safe reconcile/cancel, computed
    /// from [`AgentTaskLiveness::is_reconcilable`] rather than by re-summing the
    /// buckets a consumer would otherwise have to know the meaning of (#W3-4).
    pub reconcilable: usize,
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
    /// Sibling of [`Self::liveness`]: whether that classification is a
    /// candidate for safe reconcile/cancel. Emitted so an orchestrator reading
    /// `liveness: "suspect"` does not have to hardcode which of the four values
    /// mean "you may reconcile this" (#W3-4). Always present exactly when
    /// `liveness` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness_reconcilable: Option<bool>,
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
    let (records, record_health) = agent_task_lifecycle::read_records_with_health()?;
    discovery_report(filter, options, records, record_health)
}

/// Find the newest run matching list filters without treating a bounded display
/// snapshot as the complete search corpus.
pub fn discover_filtered_latest_run(
    options: AgentTaskDiscoveryOptions,
) -> Result<AgentTaskDiscoveryReport> {
    let (records, record_health) = agent_task_lifecycle::read_all_records_with_health()?;
    discovery_report(
        AgentTaskDiscoveryFilter::Latest,
        options,
        records,
        record_health,
    )
}

fn discovery_report(
    filter: AgentTaskDiscoveryFilter,
    options: AgentTaskDiscoveryOptions,
    mut records: Vec<AgentTaskRunRecord>,
    record_health: AgentTaskRecordHealthSummary,
) -> Result<AgentTaskDiscoveryReport> {
    if options.limit == Some(0) {
        return Err(Error::validation_invalid_argument(
            "limit",
            "must be greater than zero",
            Some("0".to_string()),
            None,
        ));
    }
    // Fixture-backed runner rows were once written by an in-process concurrent
    // Cook test. Treat only the durable fixture provenance as non-production;
    // an unknown runner with any real/unknown executor stays visible and blocks.
    records.retain(|record| !is_fixture_runner_record(record));
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
    let submitted_after = options
        .submitted_after
        .as_deref()
        .map(parse_submitted_after)
        .transpose()?;
    records.retain(|record| matches_discovery_options(record, &options, submitted_after.as_ref()));
    if filter == AgentTaskDiscoveryFilter::Latest {
        records.truncate(1);
    }
    let total = records.len();

    let now = chrono::Utc::now();
    let liveness_summary = is_active.then(|| liveness_summary_for_records(&records, now));

    // `latest` is always a single run; only `all`/`active` honor a limit cap.
    let effective_limit = match filter {
        AgentTaskDiscoveryFilter::Latest => None,
        _ => options.limit,
    };
    let cursor = options.cursor.min(total);
    let end = effective_limit
        .map(|limit| cursor.saturating_add(limit).min(total))
        .unwrap_or(total);
    let records = records.drain(cursor..end).collect::<Vec<_>>();

    let runs: Vec<_> = records
        .into_iter()
        .map(|record| discovery_run(record, is_active, now))
        .collect();

    let truncated = end < total;

    Ok(AgentTaskDiscoveryReport {
        schema: "homeboy/agent-task-discovery/v1",
        filter: match filter {
            AgentTaskDiscoveryFilter::All => "all",
            AgentTaskDiscoveryFilter::Active => "active",
            AgentTaskDiscoveryFilter::Latest => "latest",
        },
        // Constant rather than a parameter: there is no discovery path that
        // writes. If one is ever added it must set this, not inherit `false`.
        reconciled: false,
        count: runs.len(),
        total,
        limit: effective_limit,
        truncated,
        next_cursor: truncated.then_some(end),
        runs,
        record_health,
        liveness_summary,
        federated_command: FEDERATED_DISCOVERY_COMMAND,
    })
}

fn matches_discovery_options(
    record: &AgentTaskRunRecord,
    options: &AgentTaskDiscoveryOptions,
    submitted_after: Option<&chrono::DateTime<chrono::Utc>>,
) -> bool {
    if options
        .state
        .as_deref()
        .is_some_and(|state| !format!("{:?}", record.state).eq_ignore_ascii_case(state))
        || submitted_after.is_some_and(|after| {
            chrono::DateTime::parse_from_rfc3339(&record.submitted_at)
                .map(|submitted| submitted.with_timezone(&chrono::Utc) <= *after)
                .unwrap_or(true)
        })
        || options.placement.as_deref().is_some_and(|placement| {
            let source = run_source(record);
            !(matches!(
                (placement, source.as_str()),
                ("local", "local") | ("remote", "remote")
            ) || placement == "runner" && source.starts_with("runner:"))
        })
    {
        return false;
    }

    if options.repo.is_none()
        && options.workspace.is_none()
        && options.task_url.is_none()
        && options.parent_id.is_none()
    {
        return true;
    }

    let plan = agent_task_lifecycle::load_controller_plan(&record.run_id).ok();
    let first_task = plan.as_ref().and_then(|plan| plan.tasks.first());
    let repo = plan
        .as_ref()
        .and_then(|plan| plan.group_key.as_deref())
        .or_else(|| first_task.and_then(|task| task.group_key.as_deref()))
        .or_else(|| first_task.and_then(|task| task.workspace.component_id.as_deref()))
        .or_else(|| first_task.and_then(|task| task.workspace.slug.as_deref()));
    let remote_workspace = metadata_string(&record.metadata, "remote_workspace");
    let workspace = first_task
        .and_then(|task| task.workspace.root.as_deref())
        .or(remote_workspace.as_deref());
    let sourced_task_url = first_task.and_then(task_source_url);
    let task_url = first_task
        .and_then(|task| task.workspace.task_url.as_deref())
        .or(sourced_task_url.as_deref());
    let parent_matches = first_task
        .map(|task| task.parent_plan_id.as_deref() == options.parent_id.as_deref())
        .unwrap_or(false);

    options
        .repo
        .as_deref()
        .is_none_or(|value| repo == Some(value))
        && options
            .workspace
            .as_deref()
            .is_none_or(|value| workspace == Some(value))
        && options
            .task_url
            .as_deref()
            .is_none_or(|value| task_url == Some(value))
        && options.parent_id.as_deref().is_none_or(|_| parent_matches)
}

fn parse_submitted_after(value: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
        .map_err(|_| {
            Error::validation_invalid_argument(
                "submitted-after",
                "must be a valid RFC3339 timestamp",
                Some(value.to_string()),
                Some(vec![
                    "Example: --submitted-after 2026-08-03T10:00:00Z".to_string()
                ]),
            )
        })
}

fn liveness_summary_for_records(
    records: &[AgentTaskRunRecord],
    now: chrono::DateTime<chrono::Utc>,
) -> AgentTaskLivenessSummary {
    let mut summary = AgentTaskLivenessSummary {
        reconcile_command: "homeboy agent-task active --reconcile --dry-run",
        ..Default::default()
    };
    for record in records {
        let liveness =
            classify_liveness(record, age_minutes(record.updated_at.as_deref(), now), now);
        match liveness {
            AgentTaskLiveness::Active => summary.active += 1,
            AgentTaskLiveness::Stale => summary.stale += 1,
            AgentTaskLiveness::Suspect => summary.suspect += 1,
            AgentTaskLiveness::Unreconciled => summary.unreconciled += 1,
        }
        if liveness.is_reconcilable() {
            summary.reconcilable += 1;
        }
    }
    summary
}

/// Read-only controller-upgrade admission derived from the same liveness model
/// as `agent-task active`. A stale local record is retained for audit and later
/// explicit reconciliation, not treated as a live controller owner. A stale
/// runner-backed record remains fail-closed because its remote owner is not
/// disproven by a local read.
pub(crate) fn controller_upgrade_admission_for_records(
    records: &[AgentTaskRunRecord],
    record_health: AgentTaskRecordHealthSummary,
    now: chrono::DateTime<chrono::Utc>,
) -> ControllerUpgradeAdmission {
    let records = records
        .iter()
        .filter(|record| !is_fixture_runner_record(record))
        .cloned()
        .collect::<Vec<_>>();
    let summary = liveness_summary_for_records(&records, now);
    let mut parent_by_attempt = BTreeMap::new();
    let mut invalid_handoff_parents = BTreeSet::new();
    for record in &records {
        if let Some(attempt) = record
            .metadata
            .pointer("/detached_cook_handoff/attempt_run_id")
            .and_then(serde_json::Value::as_str)
        {
            let linked_attempt = records.iter().find(|candidate| candidate.run_id == attempt);
            let belongs_to_parent = linked_attempt
                .and_then(|candidate| candidate.metadata.get("cook_id"))
                .and_then(serde_json::Value::as_str)
                == Some(record.run_id.as_str());
            let indexed = agent_task_lifecycle::cook_index(&record.run_id)
                .ok()
                .is_some_and(|index| index.attempts.iter().any(|entry| entry.run_id == attempt));
            if belongs_to_parent && indexed {
                parent_by_attempt.insert(attempt, &record.run_id);
            } else {
                // Do not prescribe the parent alias here: scoped reconciliation
                // correctly rejects an untrusted handoff link. The record-health
                // primitive is executable and owns repairing malformed authority.
                invalid_handoff_parents.insert(record.run_id.as_str());
            }
        }
    }
    let mut blockers = records
        .iter()
        // Durable terminal state is authoritative even when stale ownership
        // metadata remains from the process that produced it.
        .filter(|record| !record.state.is_terminal())
        .filter_map(|record| {
            let liveness =
                classify_liveness(record, age_minutes(record.updated_at.as_deref(), now), now);
            let runner_unverified = record.runner_job_id().is_some()
                && matches!(
                    liveness,
                    AgentTaskLiveness::Stale
                        | AgentTaskLiveness::Suspect
                        | AgentTaskLiveness::Unreconciled
                );
            (!matches!(liveness, AgentTaskLiveness::Stale) || runner_unverified).then(|| {
                let group_run_id = parent_by_attempt
                    .get(record.run_id.as_str())
                    .copied()
                    .unwrap_or(&record.run_id);
                let recovery_command = if invalid_handoff_parents.contains(record.run_id.as_str()) {
                    "homeboy agent-task reconcile-records --dry-run".to_string()
                } else {
                    match record.runner_id() {
                        Some(runner)
                            if agent_task_lifecycle::runner_authority(runner)
                                != agent_task_lifecycle::RunnerAuthority::Removed =>
                        {
                            format!("homeboy runner reconcile {runner}")
                        }
                        // A live local owner blocks replacement, but cancelling
                        // it would discard work. Inspect its authoritative
                        // status instead of prescribing a destructive action.
                        _ if liveness == AgentTaskLiveness::Active => {
                            format!("homeboy --placement local agent-task status {group_run_id}")
                        }
                        // Process evidence that cannot prove a dead owner stays
                        // fail-closed. The dry run is read-only and reconciles
                        // all durable/provider evidence before any mutation.
                        _ if matches!(
                            liveness,
                            AgentTaskLiveness::Suspect | AgentTaskLiveness::Unreconciled
                        ) => format!(
                            "homeboy --placement local agent-task reconcile {group_run_id} --dry-run"
                        ),
                        _ => format!(
                            "homeboy --placement local agent-task reconcile {group_run_id} --apply"
                        ),
                    }
                };
                let (owner, scope, postcondition) = if recovery_command
                    .starts_with("homeboy runner reconcile ")
                {
                    (
                        "runner_generations",
                        format!("runner `{}` and its persisted daemon generations", record.runner_id().unwrap_or_default()),
                        "the runner accepts jobs with no unresolved generation projection",
                    )
                } else if recovery_command.starts_with("homeboy agent-task reconcile-records") {
                    (
                        "durable_agent_task_record_health",
                        "stored durable agent-task record health".to_string(),
                        "durable record authority is internally consistent",
                    )
                } else {
                    (
                        "durable_agent_tasks",
                        format!("durable run or Cook group `{group_run_id}`"),
                        "the selected durable records are reconciled to authoritative provider state",
                    )
                };
                ControllerUpgradeBlocker {
                    run_id: group_run_id.clone(),
                    owner: owner.to_string(),
                    scope,
                    postcondition: postcondition.to_string(),
                    liveness: liveness.as_str(),
                    reason: if runner_unverified {
                        "runner_job_unverified_after_daemon_restart".to_string()
                    } else {
                        stale_reason_for_record(record)
                    },
                    action: recovery_command.clone(),
                    recovery_command,
                }
            })
        })
        .collect::<Vec<_>>();
    let mut grouped = BTreeMap::<String, Vec<ControllerUpgradeBlocker>>::new();
    for blocker in blockers.drain(..) {
        grouped
            .entry(blocker.run_id.clone())
            .or_default()
            .push(blocker);
    }
    let mut blockers = grouped
        .into_values()
        .map(|mut group| {
            group.sort_by(|left, right| left.action.cmp(&right.action));
            // Runner generation ownership is authoritative for a runner-backed
            // record; one blocker must route to one owning reconciliation plane.
            group
                .iter()
                .find(|blocker| blocker.owner == "runner_generations")
                .cloned()
                .unwrap_or_else(|| group.remove(0))
        })
        .collect::<Vec<_>>();
    blockers.sort_by(|left, right| left.recovery_command.cmp(&right.recovery_command));
    blockers.dedup_by(|left, right| left.recovery_command == right.recovery_command);
    ControllerUpgradeAdmission {
        schema: "homeboy/controller-upgrade-admission/v1",
        active: summary.active,
        stale: summary.stale,
        suspect: summary.suspect,
        unreconciled: summary.unreconciled,
        reconcilable: summary.reconcilable,
        record_health: serde_json::to_value(record_health).unwrap_or(serde_json::Value::Null),
        blockers,
    }
}

struct AgentTaskControllerUpgradeAdmissionProvider;

impl ControllerUpgradeAdmissionProvider for AgentTaskControllerUpgradeAdmissionProvider {
    fn controller_upgrade_admission(&self) -> Result<ControllerUpgradeAdmission> {
        let (records, health) = agent_task_lifecycle::read_records_with_health()?;
        Ok(controller_upgrade_admission_for_records(
            &records,
            health,
            chrono::Utc::now(),
        ))
    }

    fn recover_controller_upgrade_admission_for_verified_target(
        &self,
    ) -> Result<ControllerUpgradeAdmission> {
        agent_task_lifecycle::quarantine_verified_fixture_runner_records()?;
        self.controller_upgrade_admission()
    }
}

pub fn register_controller_upgrade_admission_provider() {
    register_upgrade_admission_provider(Box::new(AgentTaskControllerUpgradeAdmissionProvider));
}

fn classify_liveness(
    record: &AgentTaskRunRecord,
    last_update_age_minutes: Option<i64>,
    now: chrono::DateTime<chrono::Utc>,
) -> AgentTaskLiveness {
    // A detached Cook parent has no executable task or owner until its child
    // and supervisor are durably attached. It is an active admission record,
    // not stale queued work for the daemon reconciliation tick to cancel.
    if agent_task_lifecycle::detached_cook_admission_is_live(record, now) {
        return AgentTaskLiveness::Active;
    }
    if record.lab_handoff_validation_error().is_some() {
        return AgentTaskLiveness::Unreconciled;
    }
    // A bounded local Cook supervisor lease is ownership for both initial
    // submissions and retries until runner identity is published. Honor it
    // regardless of queued/running so discovery cannot cancel a live lease.
    if record.has_live_pending_local_cook_supervisor(now) {
        return AgentTaskLiveness::Active;
    }
    // A local Cook retry owns a queued lifecycle reservation before its child
    // begins provider execution. Its current daemon job is the authoritative
    // owner, so test it before generic queued-record staleness.
    if live_local_cook_retry_supervisor(record) {
        return AgentTaskLiveness::Active;
    }
    // Worktree lookup and materialization are controller-owned even when the
    // eventual provider execution is destined for a runner. They precede the
    // queued-to-running transition, so their fresh Cook heartbeat is the only
    // possible owner during this phase.
    if record.has_fresh_controller_pre_provider_heartbeat() {
        return AgentTaskLiveness::Active;
    }
    if record.state != agent_task_lifecycle::AgentTaskRunState::Running {
        if agent_task_lifecycle::has_expired_pending_runner_submission_intent(record, now) {
            return AgentTaskLiveness::Unreconciled;
        }
        // A queued record has no executing work of its own. Fresh timestamps
        // only prove that it was serialized recently, not that an owner still
        // exists. Retain it as active solely with a live owner or a complete
        // runner submission that is still inside its acceptance deadline.
        let has_live_owner = record.owner_process_is_running();
        let has_live_submission_intent =
            agent_task_lifecycle::has_live_pending_runner_submission_intent(record, now);
        // The materializing proxy write is the state transition into a planned
        // runner execution. It has no local PID by design, but its fresh
        // heartbeat proves the controller is still advancing toward submission.
        let has_fresh_planned_runner_execution =
            record.has_planned_runner_execution() && record.has_fresh_update();
        if !has_live_owner && !has_live_submission_intent && !has_fresh_planned_runner_execution {
            return AgentTaskLiveness::Stale;
        }
        return AgentTaskLiveness::Active;
    }

    if agent_task_lifecycle::has_expired_pending_runner_submission_intent(record, now) {
        return AgentTaskLiveness::Unreconciled;
    }
    // Pending reverse-broker ownership is runner-host authority. A projected
    // runner PID cannot be probed on this controller; acceptance or expiry is
    // the durable boundary for resolving its liveness.
    if agent_task_lifecycle::has_live_pending_runner_submission_intent(record, now) {
        return AgentTaskLiveness::Active;
    }

    match record.local_owner_liveness() {
        // Heartbeats are a projection, while a verified owner is authoritative.
        // Check this before honoring an older stale marker.
        agent_task_lifecycle::LocalOwnerLiveness::Live => return AgentTaskLiveness::Active,
        agent_task_lifecycle::LocalOwnerLiveness::Unverifiable => {
            return AgentTaskLiveness::Unreconciled;
        }
        agent_task_lifecycle::LocalOwnerLiveness::Dead => return AgentTaskLiveness::Stale,
        agent_task_lifecycle::LocalOwnerLiveness::Absent => {}
    }

    if record.is_stale_running() {
        return AgentTaskLiveness::Stale;
    }

    // A record can carry a stale-running condition that has not yet been
    // annotated onto its metadata (annotation happens on the `status` path, not
    // at rest). Detect a dead owner process directly here so discovery agrees
    // with `status` and `reconcile_stale_active_runs` actually terminalizes the
    // ghost `running` record instead of classifying it Active because the
    // owner_pid is merely PRESENT (#9718). A runner-backed job with a fresh
    // heartbeat is authoritative liveness and is deliberately left alone.
    let owner_process_dead = record.owner_pid().is_some()
        && record.local_owner_liveness() == agent_task_lifecycle::LocalOwnerLiveness::Dead;
    let runner_backed_and_fresh = record.runner_job_id().is_some() && record.has_fresh_update();
    if owner_process_dead && !runner_backed_and_fresh {
        return AgentTaskLiveness::Stale;
    }

    let stale_by_age =
        last_update_age_minutes.is_some_and(|age| age >= RUNNING_HEARTBEAT_STALE_MINUTES);

    let has_owner_signal =
        record.metadata.get("runner_pid").is_some() || record.runner_id().is_some();

    match (stale_by_age, has_owner_signal) {
        (true, _) => AgentTaskLiveness::Suspect,
        (false, true) => AgentTaskLiveness::Active,
        // No disproven liveness, no recent heartbeat, no owner signal at all:
        // we genuinely cannot confirm this run either way.
        (false, false) => AgentTaskLiveness::Unreconciled,
    }
}

fn live_local_cook_retry_supervisor(record: &AgentTaskRunRecord) -> bool {
    let supervisor = &record.metadata["local_cook_supervisor"];
    if supervisor["job_type"].as_str() != Some(crate::agent_task_service::AGENT_TASK_COOK_JOB_TYPE)
    {
        return false;
    }
    let Some(job_id) = supervisor["job_id"].as_str() else {
        return false;
    };
    let Ok(job_id) = uuid::Uuid::parse_str(job_id) else {
        return false;
    };
    let Ok(daemon) = homeboy_core::daemon::read_status() else {
        return false;
    };
    let Some(lease) = daemon.state.map(|state| state.lease_id) else {
        return false;
    };
    if !daemon.reachable || !daemon.fresh {
        return false;
    }
    let Ok(path) = homeboy_core::paths::daemon_jobs_file() else {
        return false;
    };
    let Ok(store) = JobStore::open_without_reconciliation(path) else {
        return false;
    };
    let Ok(job) = store.get(job_id) else {
        return false;
    };
    // Controller jobs retain their transport namespace in durable storage.
    // The retry metadata carries the submitted job type, not that stored
    // operation name, so compare against the authoritative controller form.
    job.operation
        == format!(
            "controller.{}",
            crate::agent_task_service::AGENT_TASK_COOK_JOB_TYPE
        )
        && job.daemon_lease_id.as_deref() == Some(lease.as_str())
        && matches!(job.status, JobStatus::Queued | JobStatus::Running)
}

/// Label where a run executes so an operator can trace the runner process.
fn run_source(record: &AgentTaskRunRecord) -> String {
    if let Some(runner_id) = record.runner_id() {
        return format!("runner:{runner_id}");
    }
    if record.metadata.get("remote_run_id").is_some() || record.runner_job_id().is_some() {
        return "remote".to_string();
    }
    "local".to_string()
}

fn is_fixture_runner_record(record: &AgentTaskRunRecord) -> bool {
    agent_task_lifecycle::load_controller_plan(&record.run_id)
        .ok()
        .and_then(|plan| agent_task_lifecycle::fixture_runner_provenance(record, &plan))
        .is_some()
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
    let plan = agent_task_lifecycle::load_controller_plan(&record.run_id).ok();
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
    let liveness = classify.then(|| classify_liveness(&record, last_update_age_minutes, now));
    let stale_reason = metadata_string(
        &record.metadata,
        agent_task_lifecycle::METADATA_KEY_STALE_RUNNING_REASON,
    )
    .or_else(|| {
        (liveness == Some(AgentTaskLiveness::Stale)).then(|| stale_reason_for_record(&record))
    });

    // Runner metadata describes execution placement, not necessarily lifecycle
    // record ownership. Controller handoff projections retain their durable
    // record locally across runner reconnects, so their commands must remain
    // controller-scoped until a runner-local record is independently discovered.
    let runner_id = record.runner_id().map(str::to_string);
    let runner_job_id = record.runner_job_id().map(str::to_string);
    let command_prefix = if metadata_string(&record.metadata, "lifecycle_store_owner").as_deref()
        == Some("controller")
    {
        "homeboy --placement local agent-task".to_string()
    } else {
        match runner_id.as_deref() {
            // Lifecycle records resident on a runner must execute there. The
            // global `--runner` flag is only for portable Lab offload commands.
            Some(runner_id) => format!("homeboy runner exec {runner_id} -- homeboy agent-task"),
            None => "homeboy --placement local agent-task".to_string(),
        }
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
        runner_id,
        runner_job_id,
        remote_run_id: metadata_string(&record.metadata, "remote_run_id"),
        stale: (liveness == Some(AgentTaskLiveness::Stale))
            .then_some(true)
            .or_else(|| {
                metadata_bool(&record.metadata, agent_task_lifecycle::METADATA_KEY_STALE_RUNNING)
            }),
        stale_reason,
        retryable: metadata_bool(&record.metadata, agent_task_lifecycle::METADATA_KEY_RETRYABLE),
        liveness,
        liveness_reconcilable: liveness.map(AgentTaskLiveness::is_reconcilable),
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
            reconcile: format!("{command_prefix} reconcile {run_id} --dry-run"),
        },
    }
}

/// Explain every stale read projection, including a pure discovery read that
/// deliberately leaves the durable record untouched.
fn stale_reason_for_record(record: &AgentTaskRunRecord) -> String {
    if record.owner_pid().is_some() && !record.owner_process_is_running() {
        return "owner_process_not_running".to_string();
    }
    if record.runner_job_id().is_some() {
        return "runner_job_unverified_after_daemon_restart".to_string();
    }
    if record.owner_pid().is_none() {
        return "missing_runner_pid".to_string();
    }
    "stale_running_marked".to_string()
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
            AgentTaskState::CandidateRecoverable => counts.completed += 1,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn queued_record(supervisor: Value) -> AgentTaskRunRecord {
        serde_json::from_value(json!({
            "schema": "homeboy/agent-task-run/v1",
            "run_id": "retry-attempt",
            "plan_id": "plan",
            "state": "queued",
            "submitted_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
            "plan_path": "plan.json",
            "metadata": {
                "cook_id": "cook",
                "local_cook_supervisor": supervisor,
            },
        }))
        .expect("queued record")
    }

    #[test]
    fn queued_retry_honors_only_a_valid_bounded_pending_supervisor_lease() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:01:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc);
        let valid = queued_record(json!({
            "state": "pending",
            "pinned_run_id": "retry-attempt",
            "lease_started_at": "2026-01-01T00:00:45Z",
            "lease_expires_at": "2026-01-01T00:01:15Z",
        }));
        assert_eq!(
            classify_liveness(&valid, Some(10), now),
            AgentTaskLiveness::Active
        );

        let expired = queued_record(json!({
            "state": "pending",
            "pinned_run_id": "retry-attempt",
            "lease_started_at": "2026-01-01T00:00:00Z",
            "lease_expires_at": "2026-01-01T00:00:30Z",
        }));
        assert_eq!(
            classify_liveness(&expired, Some(10), now),
            AgentTaskLiveness::Stale
        );

        let invalid = queued_record(json!({
            "state": "pending",
            "pinned_run_id": "retry-attempt",
            "lease_started_at": "2026-01-01T00:00:00Z",
            "lease_expires_at": "2026-01-01T00:10:00Z",
        }));
        assert_eq!(
            classify_liveness(&invalid, Some(10), now),
            AgentTaskLiveness::Stale
        );
    }
}
