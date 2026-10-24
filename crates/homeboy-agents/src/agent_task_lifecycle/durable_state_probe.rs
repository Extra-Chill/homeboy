//! Test-support probe for the "reads never mutate" contract (#13697, #15718).
//!
//! [`durable_state_for_test`] captures everything a lifecycle read could
//! change: every row of the run, event, work-intent, resource and action-claim
//! tables, and the bytes of every file under the data and artifact roots apart
//! from the SQLite database files. Two equal captures mean nothing durable
//! changed in between, including record bytes, the store-owned revision, the
//! event log, and the work-intent outbox.

use super::AgentTaskLifecycleStore;
use std::collections::BTreeMap;
use std::path::Path;

const TABLES: &[(&str, &str)] = &[
    ("runs", "id"),
    ("control_plane_event_appends", "run_id, sequence"),
    ("control_plane_work_intents", "id"),
    ("control_plane_resources", "rowid"),
    ("control_plane_resource_aliases", "rowid"),
    ("control_plane_action_claims", "rowid"),
    ("artifacts", "rowid"),
];

/// Snapshot every durable fact below `lifecycle_store`'s roots.
pub fn durable_state_for_test(
    lifecycle_store: &AgentTaskLifecycleStore,
) -> BTreeMap<String, String> {
    let db_path = lifecycle_store.observation_db_path();
    let mut state = homeboy_core::test_support::observation_table_snapshot(&db_path, TABLES);
    let db_name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    for root in [lifecycle_store.data_root(), lifecycle_store.artifact_root()] {
        collect_files(&root, &db_name, &mut state);
    }
    state
}

fn collect_files(dir: &Path, db_name: &str, state: &mut BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, db_name, state);
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // The database is compared row by row above; its page, WAL and
        // shared-memory files change on a plain read connection.
        if !db_name.is_empty() && name.starts_with(db_name) {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap_or_default();
        state.insert(
            format!("file:{}", path.display()),
            String::from_utf8_lossy(&bytes).into_owned(),
        );
    }
}

/// One durable run in a state the status read must leave untouched.
#[derive(Debug, Clone)]
pub struct StatusPurityFixture {
    pub label: &'static str,
    pub run_id: String,
}

/// Seed, in the ambient installation, one run per lifecycle shape the status
/// read must not mutate: queued, running with a dead owner, every terminal
/// state, a Lab run with a pending (unprojected) terminal runner snapshot, and
/// terminal Cook attempts whose continuation is not yet enqueued.
///
/// Every "pending" fixture is something [`super::reconcile_status`] would act on,
/// so a status read that converged would be caught by
/// [`durable_state_for_test`].
pub fn seed_status_purity_fixtures_for_test() -> super::Result<Vec<StatusPurityFixture>> {
    use super::*;
    let store = AgentTaskLifecycleStore::from_current_environment()?;
    let plan = purity_plan();
    let mut fixtures = Vec::new();
    let mut push = |label: &'static str, run_id: &str| {
        fixtures.push(StatusPurityFixture {
            label,
            run_id: run_id.to_string(),
        })
    };

    submit_plan(&plan, Some("purity-queued"))?;
    push("queued", "purity-queued");

    submit_plan(&plan, Some("purity-running-dead-owner"))?;
    rewrite_record_for_test("purity-running-dead-owner", |record| {
        set_run_state(record, AgentTaskRunState::Running);
        record.metadata["runner_pid"] = serde_json::json!(u32::MAX);
        record.submitted_at = "2000-01-01T00:00:00+00:00".to_string();
        record.updated_at = None;
    })?;
    push("running_dead_owner", "purity-running-dead-owner");

    record_completed_run(&plan, &purity_aggregate(&plan), Some("purity-succeeded"))?;
    push("succeeded", "purity-succeeded");

    for (label, run_id, outcome) in [
        ("failed", "purity-failed", AgentTaskTerminalOutcome::Failed),
        (
            "partial_failure",
            "purity-partial-failure",
            AgentTaskTerminalOutcome::PartialFailure,
        ),
        (
            "partial_recoverable",
            "purity-partial-recoverable",
            AgentTaskTerminalOutcome::PartialRecoverable,
        ),
        (
            "candidate_recoverable",
            "purity-candidate-recoverable",
            AgentTaskTerminalOutcome::CandidateRecoverable,
        ),
    ] {
        submit_plan(&plan, Some(run_id))?;
        let current = store.read_record(run_id)?;
        store
            .transition(
                run_id,
                current.revision,
                AgentTaskTransition::Terminate {
                    outcome,
                    metadata: serde_json::Map::new(),
                },
            )
            .map_err(Error::from)?;
        push(label, run_id);
    }

    submit_plan(&plan, Some("purity-cancelled"))?;
    cancel_run("purity-cancelled", Some("status purity fixture"))?;
    push("cancelled", "purity-cancelled");

    let lab_run = "purity-pending-runner-snapshot";
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    record_detached_lab_run(DetachedLabRunRecord {
        run_id: lab_run,
        runner_id: "homeboy-lab",
        runner_job_id: PURITY_RUNNER_JOB_ID,
        remote_workspace: "/runner/workspace/homeboy",
        remote_command: &command,
    })?;
    let (status, events) = terminal_runner_observation(lab_run, &purity_aggregate(&plan));
    rewrite_record_for_test(lab_run, |record| {
        record.metadata["runner_job_status"] = status.clone();
        record.metadata["runner_job_events"] = events.clone();
    })?;
    push("pending_runner_snapshot", lab_run);

    for (label, cook_id, run_id, outcome) in [
        (
            "succeeded_cook_pending_continuation",
            "purity-cook-succeeded",
            "purity-cook-succeeded-attempt",
            None,
        ),
        (
            "candidate_recoverable_cook_pending_continuation",
            "purity-cook-recoverable",
            "purity-cook-recoverable-attempt",
            Some(AgentTaskTerminalOutcome::CandidateRecoverable),
        ),
    ] {
        seed_terminal_cook_attempt(&store, &plan, cook_id, run_id, outcome)?;
        push(label, run_id);
    }
    Ok(fixtures)
}

const PURITY_RUNNER_JOB_ID: &str = "00000000-0000-0000-0000-000000015718";

fn seed_terminal_cook_attempt(
    store: &AgentTaskLifecycleStore,
    plan: &crate::agent_task_scheduler::AgentTaskPlan,
    cook_id: &str,
    run_id: &str,
    outcome: Option<super::AgentTaskTerminalOutcome>,
) -> super::Result<()> {
    use crate::agent_task_service::{
        AgentTaskCookRecipe, AgentTaskCookRecipeAttempt, CookRecipeStore, COOK_RECIPE_SCHEMA,
    };
    CookRecipeStore::new(store.roots().clone()).persist_recipe(&AgentTaskCookRecipe {
        schema: COOK_RECIPE_SCHEMA.to_string(),
        cook_id: cook_id.to_string(),
        attempts: vec![AgentTaskCookRecipeAttempt {
            lineage: None,
            attempt: 1,
            run_id: run_id.to_string(),
            plan: plan.clone(),
        }],
        promotion_transport: serde_json::json!({"provider_invocation": null, "attempt_dispatch": { "kind": "local" }}),
        gate_policy: serde_json::json!({"verify": [], "private_verify": [], "private_gate_reveal": "summary_only"}),
        retry_budget: serde_json::json!({"max_attempts": 1, "execution_budget": plan.options.execution_budget}),
        finalization: serde_json::json!({"no_finalize": true, "base": "main", "head": null, "title": "title", "commit_message": "message", "protected_branches": [], "ai_tool": "test", "ai_model": null, "ai_used_for": "test", "to_worktree": "target", "source_worktree_path": null, "task_base_sha": null}),
        source_refs: vec!["issue".to_string()],
        runtime_generation: homeboy_core::build_identity::current().display,
        sensitive_mappings: Vec::new(),
        harvest_context: Default::default(),
    })?;
    store.submit_plan_with_runtime_admission(plan, run_id, |_| Ok(serde_json::json!({})))?;
    super::record_cook_attempt_in_store(store, cook_id, 1, run_id)?;
    super::record_run_aggregate_in_store(store, run_id, plan, &purity_aggregate(plan))?;
    if let Some(outcome) = outcome {
        let current = store.read_record(run_id)?;
        store
            .transition(
                run_id,
                current.revision,
                super::AgentTaskTransition::Terminate {
                    outcome,
                    metadata: serde_json::Map::new(),
                },
            )
            .map_err(super::Error::from)?;
    }
    Ok(())
}

fn purity_plan() -> crate::agent_task_scheduler::AgentTaskPlan {
    use crate::agent_task::*;
    crate::agent_task_scheduler::AgentTaskPlan::new(
        "plan-a",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("fixture".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions: "run".to_string(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: serde_json::Value::Null,
        }],
    )
}

fn purity_aggregate(
    plan: &crate::agent_task_scheduler::AgentTaskPlan,
) -> crate::agent_task_scheduler::AgentTaskAggregate {
    use crate::agent_task_scheduler::*;
    let outcomes = vec![crate::agent_task::AgentTaskOutcome {
        task_id: "task-a".to_string(),
        status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
        summary: Some("ok".to_string()),
        ..Default::default()
    }];
    AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        totals: AgentTaskAggregateTotals::from_outcomes(plan.tasks.len(), &outcomes),
        outcomes,
        events: vec![AgentTaskProgressEvent {
            task_id: "task-a".to_string(),
            state: AgentTaskState::Succeeded,
            attempt: 1,
            message: Some("ok".to_string()),
        }],
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: Default::default(),
    }
}

/// A terminal runner job observation persisted on the record but not yet
/// projected into it: the shape a controller sees after the daemon evicted the
/// completed job (`project_persisted_terminal_runner_events_in_store`).
fn terminal_runner_observation(
    run_id: &str,
    aggregate: &crate::agent_task_scheduler::AgentTaskAggregate,
) -> (serde_json::Value, serde_json::Value) {
    (
        serde_json::json!("succeeded"),
        serde_json::json!([{
            "sequence": 1,
            "job_id": PURITY_RUNNER_JOB_ID,
            "kind": "progress",
            "timestamp_ms": 2,
            "message": "agent-task lifecycle event",
            "data": {
                "schema": "homeboy/agent-task-run-plan-lifecycle-event/v1",
                "identity": {
                    "runner_id": "homeboy-lab",
                    "runner_job_id": PURITY_RUNNER_JOB_ID,
                    "persisted_run_id": run_id,
                    "run_id": run_id,
                },
                "aggregate": aggregate,
            },
        }]),
    )
}
