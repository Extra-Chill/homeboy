//! Activity reporting — aggregates in-flight and recent work from observation
//! runs, agent-task records, daemon jobs, and runner-resident records into a
//! single deduplicated report, and resolves individual items by id.
//!
//! The data model lives in [`model`], multi-source dedup/reconciliation in
//! [`collector`], shared leaf helpers in [`action_helpers`], and each source
//! adapter in its own submodule (`observation`, `daemon_jobs`,
//! `runner_sessions`, `agent_task_provider`). This root retains only report
//! assembly and id resolution (#9794).
//!
//! ## This is a read model, not a reconciler
//!
//! Every source here is read without writing. That is a deliberate contract,
//! and it is *observable*: `agent-task status <id>` is a reconciling read that
//! writes, and `runs watch` reconciles on purpose. So `activity show <id>` and
//! `agent-task status <id>` can legitimately report different states for the
//! same run at the same instant, and calling one changes what the other
//! returns. [`ActivityReport::reconciled`] is emitted as `false` so a consumer
//! can tell which kind of answer it is holding (#W3-15).

use std::collections::BTreeSet;

use serde_json::Value;

// `crate::observation` is the crate-root module, not this module's own
// `observation` submodule declared below.
use crate::observation::RUNNING_HEARTBEAT_STALE_MINUTES;
use crate::{Error, Result};

pub mod agent_task_provider;

mod action_helpers;
mod collector;
mod model;

mod daemon_jobs;
mod observation;
mod runner_sessions;

pub use model::*;

// Re-exported at module scope so the source-provider submodules can pull the
// shared helpers and collector through `use super::*` (#9794).
pub(crate) use action_helpers::{action, metadata_string, ms_to_rfc3339, parse_ts};
pub(crate) use collector::ActivityCollector;

pub const ACTIVITY_REPORT_SCHEMA: &str = "homeboy/activity-report/v1";

/// Extra records inspected by the default view after its requested result
/// count. This lets current work survive a stale-runner backlog without making
/// the interactive command perform an unbounded historical scan.
const DEFAULT_STALE_PROJECTION_WINDOW: usize = 100;
const DEFAULT_NEXT_ACTION_LIMIT: usize = 20;

/// Environment opt-out for runner federation. Any of `0`/`false`/`no`/`off`
/// disables it; anything else (including absence) leaves it on.
pub const ACTIVITY_FEDERATE_RUNNERS_ENV: &str = "HOMEBOY_ACTIVITY_FEDERATE_RUNNERS";

/// Source-selection policy for one activity report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityOptions {
    /// Consult connected Lab runners for records that are resident on them.
    ///
    /// **Default-on**, deliberately. The alternative — an opt-in flag — means
    /// the default answer to "what is happening right now" silently omits
    /// offloaded work, which is exactly the failure this source exists to fix;
    /// an operator cannot pass a flag for a gap they do not know is there. The
    /// cost is bounded rather than avoided: the source short-circuits entirely
    /// when no runner layer is registered, performs no network at all for a
    /// runner with no connected session, and runs the one query it does make
    /// under the shared read-only probe deadline. See the `runner_sessions`
    /// module for the full bound.
    ///
    /// A latency-sensitive caller opts out with `--no-runners` or
    /// [`ACTIVITY_FEDERATE_RUNNERS_ENV`].
    pub federate_runners: bool,
}

impl Default for ActivityOptions {
    fn default() -> Self {
        Self {
            federate_runners: federate_runners_from(
                std::env::var(ACTIVITY_FEDERATE_RUNNERS_ENV).ok().as_deref(),
            ),
        }
    }
}

/// Resolve the federation switch from a raw override. Split from the
/// environment read so the default-on contract is deterministically testable.
fn federate_runners_from(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
            .unwrap_or_default(),
        "0" | "false" | "no" | "off"
    )
}

pub fn activity_report(scope: ActivityScope, limit: usize) -> Result<ActivityReport> {
    activity_report_with(scope, limit, ActivityOptions::default())
}

pub fn activity_report_with(
    scope: ActivityScope,
    limit: usize,
    options: ActivityOptions,
) -> Result<ActivityReport> {
    let mut collector = ActivityCollector::default();
    observation::collect(&mut collector, limit)?;
    // Items and record health come from one pass over the durable agent-task
    // records. Reading them separately walked the corpus twice (#10308).
    let (agent_task_items, agent_task_record_health) =
        agent_task_provider::agent_task_activity(limit)?;
    for item in agent_task_items {
        collector.insert(item);
    }
    daemon_jobs::collect(&mut collector)?;
    // Runner federation is last on purpose: every controller-local source is
    // already collected before any remote probe is attempted, so the remote
    // bound can only ever add to a complete local answer — it can never delay
    // or lose one.
    let federation = runner_sessions::collect(&mut collector, options.federate_runners);
    let collection_limit = if scope == ActivityScope::ActiveRecent {
        limit.saturating_add(DEFAULT_STALE_PROJECTION_WINDOW)
    } else {
        limit
    };
    let mut report = report_from_items(
        collector.items(ActivityScope::All, collection_limit),
        scope,
        limit,
        "activity",
    );
    report.agent_task_record_health = agent_task_record_health;
    report.partial = federation.partial;
    report.runner_federation = federation;
    Ok(report)
}

/// Resolve a known activity id through targeted, indexed per-provider probes
/// before falling back to a full-corpus scan.
///
/// `activity show`/`watch` are called with a concrete id (an observation run id
/// or a daemon/runner job UUID). Building the entire activity report just to
/// find one item enumerated every observation, agent-task record, daemon job,
/// and record-health probe — which timed out for active Lab
/// jobs (#9762). This probes the cheap indexed lookups first; only when none
/// resolve does it fall back to `resolve_item` over the bounded full report.
///
/// Ownership note: full-corpus aggregation belongs to `activity list`, so the
/// fallback is intentionally the last resort here.
///
/// Probe order mirrors the collector's source precedence. Agent-task lifecycle
/// records are also rows in the observation store, so the untyped observation
/// probe resolves an agent-task id too — but only into the subordinate
/// observation projection. Probing the authoritative lifecycle source first
/// keeps `show`/`watch` agreeing with `list`, where the lifecycle projection
/// wins the same id (#10308).
fn resolve_activity_item(id: &str) -> Result<(Option<ActivityItem>, ActivityRunnerFederation)> {
    resolve_activity_item_with(id, ActivityOptions::default())
}

fn resolve_activity_item_with(
    id: &str,
    options: ActivityOptions,
) -> Result<(Option<ActivityItem>, ActivityRunnerFederation)> {
    // Bounded, indexed probes for the id shapes `show`/`watch` are called with.
    // A failing probe (missing store, etc.) must not abort resolution — treat it
    // as "not found here" and continue so a partial-source outage still resolves
    // the id from another provider.
    //
    // All three are controller-local, so none of them federates and none of them
    // returns a federation record: a hit here costs no remote work at all.
    if let Ok(Some(item)) = agent_task_provider::probe_by_id(id) {
        return Ok((Some(item), ActivityRunnerFederation::default()));
    }
    if let Ok(Some(item)) = observation::probe_by_id(id) {
        return Ok((Some(item), ActivityRunnerFederation::default()));
    }
    if let Ok(Some(item)) = daemon_jobs::probe_by_id(id) {
        return Ok((Some(item), ActivityRunnerFederation::default()));
    }

    // Fallback: aggregate the bounded report and resolve cross-refs (agent-task
    // run ids, runner job ids mirrored onto observation runs, etc.).
    //
    // This is the one path where runner federation genuinely earns its latency:
    // a freshly-offloaded run's durable record lives on the runner, so the three
    // controller-local probes above are *guaranteed* to miss it. Disabling
    // federation here would make `homeboy activity watch <cook-id>` — the exact
    // command the cook notification hands the operator — permanently report
    // "activity item not found" for the window that matters most.
    let report = activity_report_with(ActivityScope::All, 1000, options)?;
    Ok((
        resolve_item(&report.items, id).cloned(),
        report.runner_federation,
    ))
}

fn activity_item_not_found(id: &str) -> Error {
    Error::validation_invalid_argument(
        "id",
        format!("activity item not found: {id}"),
        Some(id.to_string()),
        Some(vec![
            "Run `homeboy activity` to list active and recent work.".to_string(),
        ]),
    )
}

pub fn show_activity(id: &str) -> Result<ActivityReport> {
    show_activity_with(id, ActivityOptions::default())
}

/// `show_activity` with an explicit federation policy.
///
/// The daemon needs this: its HTTP surface runs on a single-threaded accept
/// loop, and the three controller-local probes always miss for a
/// runner-resident id, so the default would fall through to a bounded runner
/// probe and stall every other route for its duration.
pub fn show_activity_with(id: &str, options: ActivityOptions) -> Result<ActivityReport> {
    let (item, federation) = resolve_activity_item_with(id, options)?;
    let Some(item) = item else {
        return Err(activity_item_not_found(id));
    };
    // Record health is a full-corpus diagnostic owned by `activity list`.
    // Attaching it here re-read every durable agent-task record just to answer
    // one id, so it stays null — the report shape carries the field either way
    // (#10308).
    let mut report = report_from_items(vec![item], ActivityScope::All, 1, "activity.show");
    // A `show` that had to fall back through the federation inherits its
    // partiality: an id resolved while a runner was unreachable is answered from
    // an incomplete corpus, and the caller is entitled to know that.
    report.partial = federation.partial;
    report.runner_federation = federation;
    Ok(report)
}

pub fn resolve_activity(id: &str) -> Result<ActivityItem> {
    resolve_activity_item(id)?
        .0
        .ok_or_else(|| activity_item_not_found(id))
}

fn resolve_item<'a>(items: &'a [ActivityItem], id: &str) -> Option<&'a ActivityItem> {
    items.iter().find(|item| {
        item.id == id
            || item.refs.run_id.as_deref() == Some(id)
            || item.refs.agent_task_run_id.as_deref() == Some(id)
            || item.refs.runner_job_id.as_deref() == Some(id)
    })
}

/// Downgrade `Running` rows whose heartbeat is stale to `Stale` so `active`
/// totals reflect only fresh, verifiable work. A row is considered stale when
/// its last heartbeat (`updated_at`) is older than
/// [`RUNNING_HEARTBEAT_STALE_MINUTES`]. A row with a fresh heartbeat — or one
/// with no `updated_at` heartbeat at all, whose liveness cannot be disproven
/// from a timestamp alone — is left untouched. Each downgraded row is annotated
/// with the exact reconcile command so operators can converge or inspect it
/// (#9743).
fn reclassify_stale_running(items: &mut [ActivityItem]) {
    let now = chrono::Utc::now();
    for item in items.iter_mut() {
        if item.state != ActivityState::Running {
            continue;
        }
        let Some(heartbeat) = item.updated_at.as_deref().and_then(parse_ts) else {
            continue;
        };
        let age_minutes = (now - heartbeat).num_minutes();
        if age_minutes < RUNNING_HEARTBEAT_STALE_MINUTES {
            continue;
        }
        item.state = ActivityState::Stale;
        if item.kind != "agent-task" {
            continue;
        }
        let command_prefix = item
            .runner
            .runner_id
            .as_deref()
            .map(|runner_id| format!("homeboy runner exec {runner_id} -- homeboy agent-task"))
            .unwrap_or_else(|| "homeboy agent-task".to_string());
        let reconcile = action(
            "reconcile stale activity",
            format!("{command_prefix} reconcile {} --dry-run", item.id),
        );
        if !item
            .next_actions
            .iter()
            .any(|existing| existing.command == reconcile.command)
        {
            item.next_actions.push(reconcile);
        }
    }
}

fn report_from_items(
    mut items: Vec<ActivityItem>,
    scope: ActivityScope,
    limit: usize,
    command: &'static str,
) -> ActivityReport {
    reclassify_stale_running(&mut items);
    let counts = counts_for_items(&items);
    items.sort_by_key(|item| std::cmp::Reverse(activity_sort_key(item)));
    let collected_items = items.len();
    if scope == ActivityScope::ActiveRecent {
        items.retain(|item| is_active(item.state) || item.finished_at.is_some());
    }
    items.truncate(limit.max(1));
    let stale_items_omitted = counts.stale.saturating_sub(
        items
            .iter()
            .filter(|item| item.state == ActivityState::Stale)
            .count(),
    );
    let all_next_actions = items
        .iter()
        .flat_map(|item| {
            item.next_actions
                .iter()
                .map(|action| action.command.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let next_action_limit = if scope == ActivityScope::ActiveRecent {
        DEFAULT_NEXT_ACTION_LIMIT
    } else {
        usize::MAX
    };
    let next_actions: Vec<_> = all_next_actions
        .iter()
        .take(next_action_limit)
        .cloned()
        .collect();
    let displayed_items = items.len();
    ActivityReport {
        schema: ACTIVITY_REPORT_SCHEMA,
        command,
        counts,
        items,
        // Activity never reconciles. This is a constant rather than a parameter
        // because there is no activity path that writes; if one is ever added
        // it must set this, not silently inherit `false` (#W3-15).
        reconciled: false,
        partial: false,
        runner_federation: ActivityRunnerFederation::default(),
        agent_task_record_health: Value::Null,
        truncation: ActivityTruncation {
            items_omitted: collected_items.saturating_sub(displayed_items),
            stale_items_omitted,
            next_actions_omitted: all_next_actions.len().saturating_sub(next_actions.len()),
        },
        next_actions,
    }
}

fn activity_sort_key(item: &ActivityItem) -> (bool, Option<chrono::DateTime<chrono::Utc>>, String) {
    (
        is_active(item.state),
        item.updated_at
            .as_deref()
            .or(Some(item.created_at.as_str()))
            .and_then(parse_ts),
        item.id.clone(),
    )
}

fn counts_for_items(items: &[ActivityItem]) -> ActivityCounts {
    let mut counts = ActivityCounts {
        total: items.len(),
        ..Default::default()
    };
    for item in items {
        if is_active(item.state) {
            counts.active += 1;
        }
        match item.state {
            ActivityState::Queued => counts.queued += 1,
            ActivityState::Running => counts.running += 1,
            ActivityState::Succeeded => counts.succeeded += 1,
            ActivityState::PartialFailure => counts.partial_failure += 1,
            ActivityState::Failed => counts.failed += 1,
            ActivityState::Cancelled => counts.cancelled += 1,
            ActivityState::TimedOut => counts.timed_out += 1,
            ActivityState::Stale => counts.stale += 1,
            ActivityState::Unknown => counts.unknown += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_jobs::{self, JobStatus};
    use crate::observation::{NewRunRecord, ObservationStore, RunStatus};
    use crate::paths;
    use crate::test_support::with_isolated_home;

    fn item(id: &str, state: ActivityState) -> ActivityItem {
        ActivityItem {
            id: id.to_string(),
            kind: "bench".to_string(),
            source_store: "test".to_string(),
            state,
            created_at: "2026-07-04T00:00:00Z".to_string(),
            updated_at: None,
            finished_at: None,
            command: None,
            cwd: None,
            runner: ActivityRunnerRefs::default(),
            refs: ActivityCrossRefs {
                run_id: Some(id.to_string()),
                agent_task_run_id: None,
                runner_job_id: None,
            },
            artifacts: Vec::new(),
            evidence: Vec::new(),
            source_projections: Vec::new(),
            state_conflicts: Vec::new(),
            next_actions: vec![action("show", format!("homeboy runs show {id}"))],
        }
    }

    #[test]
    fn source_merging_dedupes_by_run_id() {
        let mut collector = ActivityCollector::default();
        collector.insert(item("run-1", ActivityState::Running));
        let mut duplicate = item("job-1", ActivityState::Queued);
        duplicate.refs.run_id = Some("run-1".to_string());
        duplicate.refs.runner_job_id = Some("job-1".to_string());
        duplicate.runner.job_id = Some("job-1".to_string());
        collector.insert(duplicate);

        let items = collector.items(ActivityScope::All, 10);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].refs.run_id.as_deref(), Some("run-1"));
        assert_eq!(items[0].refs.runner_job_id.as_deref(), Some("job-1"));
    }

    #[test]
    fn agent_task_lifecycle_is_authoritative_over_observation_and_runner_projections() {
        let mut collector = ActivityCollector::default();

        let mut observation = item("agent-task-1", ActivityState::Running);
        observation.kind = "agent-task".to_string();
        observation.source_store = "observation.sqlite".to_string();
        collector.insert(observation);

        let mut runner = item("runner-job-1", ActivityState::Running);
        runner.source_store = "runner.session".to_string();
        runner.refs.run_id = Some("agent-task-1".to_string());
        runner.refs.runner_job_id = Some("runner-job-1".to_string());
        collector.insert(runner);

        let mut lifecycle = item("agent-task-1", ActivityState::Queued);
        lifecycle.kind = "agent-task".to_string();
        lifecycle.source_store = "agent-task.lifecycle".to_string();
        lifecycle.refs.run_id = None;
        lifecycle.refs.agent_task_run_id = Some("agent-task-1".to_string());
        collector.insert(lifecycle);

        let items = collector.items(ActivityScope::All, 10);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "agent-task-1");
        assert_eq!(items[0].source_store, "agent-task.lifecycle");
        assert_eq!(items[0].state, ActivityState::Queued);
        assert_eq!(
            items[0]
                .source_projections
                .iter()
                .map(|projection| (projection.source_store.as_str(), projection.state))
                .collect::<Vec<_>>(),
            vec![
                ("agent-task.lifecycle", ActivityState::Queued),
                ("runner.session", ActivityState::Running),
                ("observation.sqlite", ActivityState::Running),
            ]
        );
        assert_eq!(
            items[0]
                .state_conflicts
                .iter()
                .map(|conflict| (
                    conflict.source_store.as_str(),
                    conflict.id.as_str(),
                    conflict.state
                ))
                .collect::<Vec<_>>(),
            vec![
                ("runner.session", "runner-job-1", ActivityState::Running),
                ("observation.sqlite", "agent-task-1", ActivityState::Running),
            ]
        );
    }

    #[test]
    fn source_projection_order_and_conflicts_are_stable_across_collection_order() {
        let mut lifecycle = item("agent-task-1", ActivityState::Queued);
        lifecycle.source_store = "agent-task.lifecycle".to_string();
        lifecycle.refs.run_id = None;
        lifecycle.refs.agent_task_run_id = Some("agent-task-1".to_string());

        let mut observation = item("agent-task-1", ActivityState::Running);
        observation.source_store = "observation.sqlite".to_string();

        let mut runner = item("runner-job-1", ActivityState::Running);
        runner.source_store = "runner.session".to_string();
        runner.refs.run_id = Some("agent-task-1".to_string());

        let collect = |items: Vec<ActivityItem>| {
            let mut collector = ActivityCollector::default();
            for item in items {
                collector.insert(item);
            }
            collector
                .items(ActivityScope::All, 10)
                .into_iter()
                .next()
                .expect("canonical activity item")
        };
        let item = collect(vec![lifecycle.clone(), observation.clone(), runner.clone()]);
        let reverse = collect(vec![runner, observation, lifecycle]);

        assert_eq!(item, reverse);
        assert_eq!(item.source_store, "agent-task.lifecycle");
        assert_eq!(item.state, ActivityState::Queued);
        assert_eq!(
            item.source_projections
                .iter()
                .map(|projection| projection.source_store.as_str())
                .collect::<Vec<_>>(),
            vec![
                "agent-task.lifecycle",
                "runner.session",
                "observation.sqlite"
            ]
        );
        assert_eq!(item.state_conflicts.len(), 2);
    }

    #[test]
    fn id_resolution_checks_all_id_spaces() {
        let mut item = item("run-1", ActivityState::Succeeded);
        item.refs.agent_task_run_id = Some("agent-1".to_string());
        item.refs.runner_job_id = Some("job-1".to_string());
        let items = vec![item];

        assert!(resolve_item(&items, "run-1").is_some());
        assert!(resolve_item(&items, "agent-1").is_some());
        assert!(resolve_item(&items, "job-1").is_some());
        assert!(resolve_item(&items, "missing").is_none());
    }

    #[test]
    fn stale_heartbeat_running_rows_are_reclassified_and_not_counted_active() {
        let now = chrono::Utc::now();
        let fresh_ts = now.to_rfc3339();
        let stale_ts = (now - chrono::Duration::hours(6)).to_rfc3339();

        let mut fresh = item("fresh-running", ActivityState::Running);
        fresh.updated_at = Some(fresh_ts);
        let mut stale = item("stale-running", ActivityState::Running);
        stale.updated_at = Some(stale_ts);
        // No heartbeat at all: liveness cannot be disproven from a timestamp, so
        // it is left as Running (not reclassified).
        let no_heartbeat = item("no-heartbeat-running", ActivityState::Running);

        let report = report_from_items(
            vec![fresh, stale, no_heartbeat],
            ActivityScope::All,
            10,
            "homeboy activity",
        );

        // Fresh + heartbeat-less stay running; the stale-heartbeat row moves to stale.
        assert_eq!(report.counts.running, 2);
        assert_eq!(report.counts.stale, 1);
        assert_eq!(report.counts.active, 2);

        let reclassified = report
            .items
            .iter()
            .find(|item| item.id == "stale-running")
            .expect("stale row present");
        assert_eq!(reclassified.state, ActivityState::Stale);
        assert!(reclassified.next_actions.iter().all(|action| {
            action.command != "homeboy agent-task reconcile stale-running --dry-run"
        }));
    }

    #[test]
    fn stale_agent_task_reconciliation_runs_on_the_owning_runner() {
        let now = chrono::Utc::now();
        let mut stale = item("agent-task-stale", ActivityState::Running);
        stale.kind = "agent-task".to_string();
        stale.updated_at = Some((now - chrono::Duration::hours(6)).to_rfc3339());
        stale.runner.runner_id = Some("lab-a".to_string());

        let report = report_from_items(vec![stale], ActivityScope::All, 10, "homeboy activity");

        assert_eq!(report.items[0].state, ActivityState::Stale);
        assert!(report.items[0].next_actions.iter().any(|action| {
            action.command
                == "homeboy runner exec lab-a -- homeboy agent-task reconcile agent-task-stale --dry-run"
        }));
    }

    #[test]
    fn default_view_prioritizes_active_tasks_and_explicitly_compacts_stale_runner_records() {
        let mut items = (0..100)
            .map(|index| {
                let mut stale = item(&format!("stale-runner-{index:03}"), ActivityState::Stale);
                stale.kind = "runner_execution".to_string();
                stale.runner.runner_id = Some("lab-a".to_string());
                stale
            })
            .collect::<Vec<_>>();
        let mut active_one = item("active-agent-task-one", ActivityState::Running);
        active_one.kind = "agent-task".to_string();
        active_one.next_actions = (0..12)
            .map(|index| action("next", format!("homeboy action active-one-{index}")))
            .collect();
        let mut active_two = item("active-agent-task-two", ActivityState::Queued);
        active_two.kind = "agent-task".to_string();
        active_two.next_actions = (0..12)
            .map(|index| action("next", format!("homeboy action active-two-{index}")))
            .collect();
        items.extend([active_one, active_two]);

        let default = report_from_items(items.clone(), ActivityScope::ActiveRecent, 20, "activity");
        assert_eq!(
            default
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec!["active-agent-task-two", "active-agent-task-one"],
        );
        assert_eq!(default.counts.stale, 100);
        assert_eq!(default.truncation.items_omitted, 100);
        assert_eq!(default.truncation.stale_items_omitted, 100);
        assert_eq!(default.next_actions.len(), DEFAULT_NEXT_ACTION_LIMIT);
        assert_eq!(default.truncation.next_actions_omitted, 4);

        let all = report_from_items(items, ActivityScope::All, 102, "activity");
        assert_eq!(all.items.len(), 102);
        assert_eq!(all.truncation.items_omitted, 0);
        assert_eq!(all.truncation.stale_items_omitted, 0);
        assert_eq!(all.next_actions.len(), 124);
        assert_eq!(all.truncation.next_actions_omitted, 0);
    }

    #[test]
    fn counts_normalize_states() {
        let items = vec![
            item("queued", ActivityState::Queued),
            item("running", ActivityState::Running),
            item("stale", ActivityState::Stale),
        ];
        let counts = counts_for_items(&items);
        assert_eq!(counts.total, 3);
        assert_eq!(counts.active, 2);
        assert_eq!(counts.queued, 1);
        assert_eq!(counts.running, 1);
        assert_eq!(counts.stale, 1);
    }

    #[test]
    fn observation_status_projection_preserves_every_label_and_unknown_values() {
        for case in "running:running,pass:succeeded,skipped:succeeded,fail:failed,error:failed,stale:stale,:unknown,future_status:unknown".split(',') {
            let (status, expected) = case.split_once(':').expect("status case");
            assert_eq!(serde_json::to_value(observation::state_from_run_status(status)).unwrap(), expected, "{status:?}");
        }
    }

    #[test]
    fn activity_policy_preserves_active_and_failure_sets() {
        for case in "unknown:01,queued:10,running:10,succeeded:00,partial_failure:01,failed:01,cancelled:00,timed_out:01,stale:01".split(',') {
            let (label, flags) = case.split_once(':').expect("policy case");
            let state: ActivityState = serde_json::from_value(label.into()).unwrap();
            let (active, failure) = (flags.starts_with('1'), flags.ends_with('1'));
            assert_eq!(is_active(state), active, "{state:?} active");
            assert_eq!(is_failure(state), failure, "{state:?} failure");
        }
    }

    #[test]
    fn next_actions_are_lifted_as_exact_commands() {
        let report = report_from_items(
            vec![item("run-1", ActivityState::Running)],
            ActivityScope::All,
            10,
            "activity",
        );
        assert_eq!(report.next_actions, vec!["homeboy runs show run-1"]);
        assert_eq!(report.items[0].next_actions[0].label, "show");
    }

    #[test]
    fn daemon_job_projects_durable_and_agent_task_run_ids_from_metadata() {
        let store = api_jobs::JobStore::default();
        let job = store.create_with_source_snapshot_and_metadata(
            "runner.exec",
            None,
            Some(serde_json::json!({
                "durable_run_id": "agent-task-run-123",
                "agent_task_run_id": "agent-task-run-123",
            })),
        );

        let item = daemon_jobs::item_from_job(&store, job).expect("activity item");

        assert_eq!(item.refs.run_id.as_deref(), Some("agent-task-run-123"));
        assert_eq!(
            item.refs.agent_task_run_id.as_deref(),
            Some("agent-task-run-123")
        );
    }

    #[test]
    fn daemon_job_activity_collection_does_not_reconcile_running_jobs() {
        with_isolated_home(|_| {
            let path = paths::daemon_jobs_file().expect("jobs path");
            let store =
                api_jobs::JobStore::open_without_reconciliation(&path).expect("open durable store");
            let job = store.create("runner.exec");
            store.start(job.id).expect("start job");

            let mut collector = ActivityCollector::default();
            daemon_jobs::collect(&mut collector).expect("collect activity");

            let reopened = api_jobs::JobStore::open_without_reconciliation(&path)
                .expect("reopen durable store");
            assert_eq!(
                reopened.get(job.id).expect("job remains").status,
                JobStatus::Running
            );
        });
    }

    #[test]
    fn observation_collection_keeps_active_runs_outside_the_recent_source_limit() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let active = store
                .start_run(NewRunRecord::builder("active").build())
                .expect("active run");
            let recent_terminal = store
                .start_run(NewRunRecord::builder("terminal").build())
                .expect("terminal run");
            store
                .finish_run(&recent_terminal.id, RunStatus::Pass, None)
                .expect("finish terminal run");

            let mut collector = ActivityCollector::default();
            observation::collect(&mut collector, 1).expect("collect activity");
            let items = collector.items(ActivityScope::All, 10);

            assert!(items.iter().any(|item| item.id == active.id));
        });
    }

    #[test]
    fn show_activity_resolves_run_id_through_targeted_probe() {
        // #9762: `activity show <run-id>` for a known observation run must
        // resolve via the indexed probe (get_run) rather than a full corpus
        // scan. Seed one active run and assert show() returns exactly it.
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");

            // The targeted probe resolves the run directly.
            let probed = observation::probe_by_id(&run.id).expect("probe");
            assert_eq!(probed.map(|item| item.id), Some(run.id.clone()));

            // show_activity surfaces exactly that item.
            let report = show_activity(&run.id).expect("show");
            assert_eq!(report.items.len(), 1);
            assert_eq!(report.items[0].id, run.id);
        });
    }

    #[test]
    fn observation_probe_returns_none_for_unknown_id() {
        with_isolated_home(|_| {
            ObservationStore::open_initialized().expect("store");
            assert!(observation::probe_by_id("no-such-run")
                .expect("probe")
                .is_none());
        });
    }

    #[test]
    fn agent_task_probe_defaults_to_none_without_a_registered_provider() {
        // The trait default keeps the no-op provider (and any implementor that
        // has no indexed lookup) out of the resolution path entirely, so id
        // resolution falls through to the remaining probes (#10308).
        assert!(agent_task_provider::probe_by_id("agent-task-run-1")
            .expect("probe")
            .is_none());
    }

    #[test]
    fn daemon_probe_ignores_non_uuid_ids() {
        // Run labels / non-UUID ids are never daemon job ids; the probe must
        // short-circuit without touching the job store.
        with_isolated_home(|_| {
            assert!(daemon_jobs::probe_by_id("cook-issue-9762")
                .expect("probe")
                .is_none());
        });
    }

    #[test]
    fn resolve_activity_errors_for_unknown_id() {
        with_isolated_home(|_| {
            ObservationStore::open_initialized().expect("store");
            let error = resolve_activity("missing-id").expect_err("unknown id errors");
            assert!(error.to_string().contains("activity item not found"));
        });
    }

    #[test]
    fn empty_state_output_has_zero_counts() {
        let report = report_from_items(Vec::new(), ActivityScope::All, 10, "activity");
        assert_eq!(report.counts.total, 0);
        assert!(report.items.is_empty());
        assert!(report.next_actions.is_empty());
    }

    /// Runner federation is on unless explicitly disabled. An operator cannot
    /// pass a flag for a gap they do not know exists, so the default answer to
    /// "what is happening right now" must include offloaded work.
    #[test]
    fn runner_federation_is_default_on_and_explicitly_opt_out() {
        assert!(federate_runners_from(None));
        assert!(federate_runners_from(Some("")));
        assert!(federate_runners_from(Some("1")));
        assert!(federate_runners_from(Some("yes")));
        for disabled in ["0", "false", "no", "off", " OFF ", "False"] {
            assert!(
                !federate_runners_from(Some(disabled)),
                "{disabled:?} must disable federation"
            );
        }
    }

    /// `activity` is a read model. The flag is emitted so a consumer can tell
    /// this answer apart from `agent-task status`, which reconciles as it reads
    /// and therefore *changes* what a later `activity show` returns.
    #[test]
    fn activity_reports_are_marked_unreconciled() {
        let report = report_from_items(
            vec![item("run-1", ActivityState::Running)],
            ActivityScope::All,
            10,
            "activity",
        );
        assert!(!report.reconciled);
    }

    /// A report with no runner degradation is complete, not partial — and an
    /// unreachable runner must never turn into a command failure.
    #[test]
    fn a_local_only_report_is_complete_and_names_no_runners() {
        with_isolated_home(|_| {
            ObservationStore::open_initialized().expect("store");
            let report = activity_report_with(
                ActivityScope::All,
                10,
                ActivityOptions {
                    federate_runners: false,
                },
            )
            .expect("activity report");

            assert!(!report.partial);
            assert!(!report.runner_federation.enabled);
            assert!(report.runner_federation.runners.is_empty());
        });
    }
}
