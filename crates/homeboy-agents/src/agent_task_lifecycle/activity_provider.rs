//! Agent-task implementation of the activity hook.
//!
//! Projects durable agent-task lifecycle records into core `ActivityItem`s and
//! supplies the record-health summary, provided to core's activity report
//! through the `ActivityAgentTaskProvider` hook so the activity report does not
//! depend on the agent-task subsystem directly.

use serde_json::Value;

use crate::agent_task_lifecycle::{self, AgentTaskRunRecord};
use homeboy_core::activity::agent_task_provider::{
    register_activity_agent_task_provider, ActivityAgentTaskProvider,
};
use homeboy_core::activity::{
    is_active, is_failure, ActivityCrossRefs, ActivityEvidenceRef, ActivityItem,
    ActivityNextAction, ActivityRunnerRefs, ActivityState,
};
use homeboy_core::run_lifecycle_record::RunExecutionState;
use homeboy_core::Result;

struct AgentTaskActivityProvider;

impl ActivityAgentTaskProvider for AgentTaskActivityProvider {
    /// Resolve one durable record by its primary key (`exact_record`, a single
    /// indexed `get_run`) instead of listing and refreshing every record.
    ///
    /// This read is deliberately not `status()`: `activity` is documented as a
    /// read model that does not mutate persisted state, and `status()` is a
    /// reconciling read that writes. Resolving one id must not enter that path
    /// (#10308).
    ///
    /// An id that is not a durable agent-task record — an observation run id, a
    /// daemon job UUID, a malformed record — is `None`, not an error, so id
    /// resolution falls through to the next provider.
    fn probe_by_id(&self, id: &str) -> Result<Option<ActivityItem>> {
        Ok(agent_task_lifecycle::exact_record(id)
            .ok()
            .map(item_from_agent_task))
    }

    /// One pass over the durable records yields both the projected items and
    /// the health summary for the same read, through the single-pass
    /// `list_records_with_health` (#10308).
    fn agent_task_activity(&self) -> Result<(Vec<ActivityItem>, Value)> {
        let (records, health) = agent_task_lifecycle::list_records_with_health()?;
        let items = records.into_iter().map(item_from_agent_task).collect();
        let health = serde_json::to_value(health).map_err(|error| {
            homeboy_core::Error::internal_json(
                error.to_string(),
                Some("serialize agent-task record health".to_string()),
            )
        })?;
        Ok((items, health))
    }
}

fn metadata_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn action(label: impl Into<String>, command: impl Into<String>) -> ActivityNextAction {
    ActivityNextAction {
        label: label.into(),
        command: command.into(),
    }
}

fn item_from_agent_task(record: AgentTaskRunRecord) -> ActivityItem {
    let runner_id = metadata_string(&record.metadata, &["runner_id"]);
    let job_id = metadata_string(&record.metadata, &["runner_job_id", "job_id"]);
    let remote_run_id = metadata_string(&record.metadata, &["remote_run_id"]);
    let state = ActivityState::from(RunExecutionState::from(record.state));
    ActivityItem {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        source_store: "agent-task.lifecycle".to_string(),
        state,
        created_at: record.submitted_at.clone(),
        updated_at: record.updated_at.clone(),
        finished_at: if is_active(state) {
            None
        } else {
            record.updated_at.clone()
        },
        command: None,
        cwd: None,
        runner: ActivityRunnerRefs {
            runner_id: runner_id.clone(),
            job_id: job_id.clone(),
            transport: remote_run_id,
        },
        refs: ActivityCrossRefs {
            run_id: None,
            agent_task_run_id: Some(record.run_id.clone()),
            runner_job_id: job_id,
        },
        artifacts: record
            .artifact_refs
            .into_iter()
            .map(|artifact| ActivityEvidenceRef {
                id: artifact.task_id,
                kind: artifact.kind,
                uri: artifact.uri,
            })
            .collect(),
        evidence: record
            .latest_executor_evidence
            .into_iter()
            .flat_map(|evidence| evidence.refs())
            .enumerate()
            .map(|(index, evidence)| ActivityEvidenceRef {
                id: evidence
                    .label
                    .unwrap_or_else(|| format!("evidence-{}", index + 1)),
                kind: evidence.kind,
                uri: evidence.uri,
            })
            .collect(),
        source_projections: Vec::new(),
        state_conflicts: Vec::new(),
        next_actions: actions_for_agent_task(&record.run_id, runner_id.as_deref(), state),
    }
}

fn actions_for_agent_task(
    run_id: &str,
    runner_id: Option<&str>,
    state: ActivityState,
) -> Vec<ActivityNextAction> {
    let command_prefix = runner_id
        .map(|runner_id| format!("homeboy runner exec {runner_id} -- homeboy agent-task"))
        .unwrap_or_else(|| "homeboy agent-task".to_string());
    let mut actions = vec![
        action("status", format!("{command_prefix} status {run_id}")),
        action("logs", format!("{command_prefix} logs {run_id}")),
        action("artifacts", format!("{command_prefix} artifacts {run_id}")),
    ];
    if is_active(state) {
        actions.push(action("watch", format!("homeboy activity watch {run_id}")));
    } else if is_failure(state) {
        actions.push(action(
            "retry",
            format!("{command_prefix} retry --run {run_id}"),
        ));
    }
    if matches!(state, ActivityState::Stale) {
        actions.push(action(
            "reconcile",
            format!("{command_prefix} reconcile {run_id} --dry-run"),
        ));
    }
    actions
}

/// Register the agent-task activity provider. Called once at startup so core's
/// activity report includes agent-task records without depending on the
/// agent-task subsystem.
pub fn register() {
    register_activity_agent_task_provider(Box::new(AgentTaskActivityProvider));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_lifecycle::tests::{succeeded_aggregate, test_plan};
    use homeboy_core::activity::ActivityScope;
    use homeboy_core::observation::{NewRunRecord, ObservationStore};
    use homeboy_core::test_support::with_isolated_home;

    fn seed_record(run_id: &str) -> String {
        let plan = test_plan();
        let aggregate = succeeded_aggregate(&plan);
        agent_task_lifecycle::record_completed_run(&plan, &aggregate, Some(run_id))
            .expect("durable agent-task record")
            .run_id
    }

    #[test]
    fn runner_backed_actions_execute_on_the_owning_runner() {
        let actions = actions_for_agent_task("run-1", Some("lab-a"), ActivityState::Stale);

        assert!(actions.iter().any(|action| {
            action.command
                == "homeboy runner exec lab-a -- homeboy agent-task reconcile run-1 --dry-run"
        }));
    }

    #[test]
    fn probe_by_id_resolves_one_record_without_scanning_or_writing() {
        // #10308: resolving a single agent-task id must be an indexed read, not
        // a full-corpus refresh. Preserve the exact durable records to prove
        // that neither the target nor its sibling was scanned or rewritten.
        with_isolated_home(|_| {
            let target = seed_record("run-probe-target");
            let sibling = seed_record("run-probe-sibling");
            let target_before = agent_task_lifecycle::exact_record(&target).expect("target record");
            let sibling_before =
                agent_task_lifecycle::exact_record(&sibling).expect("sibling record");

            let item = AgentTaskActivityProvider
                .probe_by_id(&target)
                .expect("probe")
                .expect("agent-task activity item");

            assert_eq!(item.id, target);
            assert_eq!(item.kind, "agent-task");
            assert_eq!(item.source_store, "agent-task.lifecycle");
            assert_eq!(
                item.refs.agent_task_run_id.as_deref(),
                Some(target.as_str())
            );
            assert_eq!(
                agent_task_lifecycle::exact_record(&target).expect("target remains readable"),
                target_before
            );
            assert_eq!(
                agent_task_lifecycle::exact_record(&sibling).expect("sibling remains readable"),
                sibling_before
            );
        });
    }

    #[test]
    fn the_report_carries_record_health_from_the_same_pass_that_projects_items() {
        // Report shape is unchanged: `list` still attaches the record-health
        // summary — now from the single pass that also projects the items —
        // and `show` carries the field as null rather than paying for a corpus
        // scan to fill it (#10308).
        with_isolated_home(|_| {
            register();
            let run_id = seed_record("run-report-health");

            let report = homeboy_core::activity::activity_report(ActivityScope::All, 50)
                .expect("activity report");

            assert!(report.items.iter().any(|item| item.id == run_id));
            assert_eq!(report.agent_task_record_health["healthy"], 1);

            let shown = homeboy_core::activity::show_activity(&run_id).expect("show activity");

            assert_eq!(shown.schema, report.schema);
            assert!(shown.agent_task_record_health.is_null());
        });
    }

    #[test]
    fn probe_by_id_ignores_ids_that_are_not_durable_agent_task_records() {
        // An observation run id or an unknown id is "not found here", not an
        // error, so core's resolver falls through to the next probe.
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("observation store");
            let run = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("observation run");

            assert!(AgentTaskActivityProvider
                .probe_by_id(&run.id)
                .expect("probe")
                .is_none());
            assert!(AgentTaskActivityProvider
                .probe_by_id("no-such-run")
                .expect("probe")
                .is_none());
        });
    }

    #[test]
    fn show_activity_resolves_an_agent_task_id_through_the_indexed_probe() {
        // End-to-end: `homeboy activity show <agent-task-id>` resolves the
        // authoritative lifecycle projection — the same source that wins the id
        // in `activity list` — without entering the reconciling scan.
        with_isolated_home(|_| {
            register();
            let run_id = seed_record("run-show-probe");
            let before = agent_task_lifecycle::exact_record(&run_id).expect("record before show");

            let report = homeboy_core::activity::show_activity(&run_id).expect("show activity");

            assert_eq!(report.items.len(), 1);
            assert_eq!(report.items[0].id, run_id);
            assert_eq!(report.items[0].source_store, "agent-task.lifecycle");
            assert_eq!(
                agent_task_lifecycle::exact_record(&run_id).expect("record after show"),
                before
            );
        });
    }
}
