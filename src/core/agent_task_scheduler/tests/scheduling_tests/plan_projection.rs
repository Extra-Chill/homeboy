//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::super::fixtures::*;
use super::super::*;
use crate::core::agent_task::{
    expand_agent_task_matrix, AgentTaskArtifact, AgentTaskArtifactDeclaration,
    AgentTaskMatrixAggregate, AgentTaskMatrixAxis, AgentTaskTypedArtifact,
    AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

mod plan_projection_tests {
    use super::*;

    #[test]
    fn runs_matrix_cells_through_generic_scheduler_and_preserves_axes() {
        let mut statuses = HashMap::new();
        statuses.insert(
            "fanout/site-smoke[model=gpt-5.5,prompt=site-b]".to_string(),
            AgentTaskOutcomeStatus::Failed,
        );
        let scheduler =
            AgentTaskScheduler::new(RecordingExecutor::new(statuses, Duration::from_millis(0)));
        let matrix_plan = expand_agent_task_matrix(
            "fanout/site-smoke",
            vec![
                AgentTaskMatrixAxis {
                    name: "model".to_string(),
                    values: vec!["gpt-5.5".to_string(), "claude".to_string()],
                },
                AgentTaskMatrixAxis {
                    name: "prompt".to_string(),
                    values: vec!["site-a".to_string(), "site-b".to_string()],
                },
            ],
            request("template"),
        )
        .expect("matrix expands");
        let mut schedule_plan = AgentTaskPlan::new(
            matrix_plan.plan_id.clone(),
            matrix_plan
                .cells
                .iter()
                .map(|cell| cell.task.clone())
                .collect(),
        );
        schedule_plan.options.max_concurrency = 2;

        let schedule = scheduler.run(schedule_plan);
        let matrix = AgentTaskMatrixAggregate::from_outcomes(&matrix_plan, &schedule.outcomes);

        assert_eq!(schedule.plan_id, "fanout/site-smoke");
        assert_eq!(schedule.totals.succeeded, 3);
        assert_eq!(schedule.totals.failed, 1);
        assert_eq!(matrix.cells.len(), 4);
        assert!(!matrix.passed);
        let failed = matrix
            .cells
            .iter()
            .find(|cell| cell.status == Some(AgentTaskOutcomeStatus::Failed))
            .expect("failed matrix cell");
        assert_eq!(failed.axes["model"], "gpt-5.5");
        assert_eq!(failed.axes["prompt"], "site-b");
        assert_eq!(failed.evidence_refs[0].kind, "log");
    }

    #[test]
    fn static_batch_plans_remain_compatible_without_output_dependencies() {
        let scheduler = AgentTaskScheduler::new(RecordingExecutor::new(
            HashMap::new(),
            Duration::from_millis(0),
        ));
        let plan_json = serde_json::to_string(&plan_with_tasks(2)).expect("plan json");
        let plan: AgentTaskPlan = serde_json::from_str(&plan_json).expect("static plan decodes");

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            crate::core::agent_task_scheduler::AgentTaskAggregateStatus::Succeeded
        );
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(aggregate.totals.skipped, 0);
        assert_eq!(aggregate.queue.max_concurrency, 1);
        assert!(aggregate.queue.adaptive_concurrency.is_none());
    }

    #[test]
    fn plan_level_component_contracts_are_preserved_on_executor_requests() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let raw = serde_json::json!({
            "schema": AGENT_TASK_PLAN_SCHEMA,
            "plan_id": "plan-components",
            "component_contracts": [{
                "slug": "generic-component",
                "path": "/workspace/generic-component",
                "loadAs": "plugin",
                "activate": true,
                "opaque_executor_hint": { "preserve": true }
            }],
            "tasks": [{
                "task_id": "task-components",
                "executor": { "backend": "test" },
                "instructions": "run"
            }]
        });
        let plan: AgentTaskPlan = serde_json::from_value(raw).expect("plan parses");

        let aggregate = scheduler.run(plan);
        let observed = observed.lock().expect("observed requests");
        let request = observed.first().expect("request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(request.component_contracts.len(), 1);
        assert_eq!(
            request.component_contracts[0].slug.as_deref(),
            Some("generic-component")
        );
        assert_eq!(
            request.component_contracts[0].path.as_deref(),
            Some("/workspace/generic-component")
        );
        assert_eq!(request.component_contracts[0].extra["loadAs"], "plugin");
        assert_eq!(request.component_contracts[0].extra["activate"], true);
        assert_eq!(
            request.component_contracts[0].extra["opaque_executor_hint"]["preserve"],
            true
        );
    }

    #[test]
    fn legacy_agent_task_plan_json_round_trips_through_homeboy_plan_projection() {
        let mut plan =
            AgentTaskPlan::new("plan-projection", vec![request("idea"), request("design")]);
        plan.group_key = Some("group-a".to_string());
        plan.options.max_concurrency = 2;
        plan.metadata = json!({ "source": "compat" });
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: vec!["idea".to_string()],
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );
        plan.artifact_outputs.insert(
            "idea".to_string(),
            vec![AgentTaskArtifactOutputDeclaration {
                name: "concept_packet".to_string(),
                kind: "concept_packet".to_string(),
                schema: Some("example/concept-packet/v1".to_string()),
                artifact_id: Some("concept".to_string()),
                payload_path: Some("/title".to_string()),
            }],
        );

        let raw = serde_json::to_string(&plan).expect("serialize legacy contract");
        let value: Value = serde_json::from_str(&raw).expect("serialized json");
        assert_eq!(value["schema"], AGENT_TASK_PLAN_SCHEMA);
        assert!(value.get("homeboy_plan").is_none());

        let decoded: AgentTaskPlan = serde_json::from_str(&raw).expect("legacy plan decodes");
        let projected = AgentTaskPlan::from_homeboy_plan(decoded.homeboy_plan.clone());

        assert_eq!(
            decoded.homeboy_plan.kind,
            crate::core::plan::PlanKind::AgentTask
        );
        assert_eq!(projected.schema, AGENT_TASK_PLAN_SCHEMA);
        assert_eq!(projected.plan_id, plan.plan_id);
        assert_eq!(projected.group_key, plan.group_key);
        assert_eq!(projected.tasks, plan.tasks);
        assert_eq!(projected.output_dependencies, plan.output_dependencies);
        assert_eq!(projected.artifact_outputs, plan.artifact_outputs);
        assert_eq!(projected.options, plan.options);
        assert_eq!(projected.metadata, plan.metadata);
    }

    #[test]
    fn scheduler_executes_from_projected_homeboy_plan() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(OutputTemplateExecutor {
            observed: Arc::clone(&observed),
            include_issue_number: true,
        });
        let mut plan = AgentTaskPlan::new(
            "plan-homeboy-projection",
            vec![request("idea"), request("design")],
        );
        plan.options.max_concurrency = 2;
        plan.tasks[1].instructions = "Build design for issue #{{outputs.issue_number}}".to_string();
        plan.output_dependencies.insert(
            "design".to_string(),
            AgentTaskOutputDependencies {
                depends_on: vec!["idea".to_string()],
                bindings: HashMap::from([(
                    "issue_number".to_string(),
                    AgentTaskOutputBinding {
                        task_id: "idea".to_string(),
                        path: "/outputs/issue_number".to_string(),
                        artifact: None,
                        required: true,
                        default: Value::Null,
                    },
                )]),
            },
        );
        plan.rebuild_homeboy_plan();
        let projected = AgentTaskPlan::from_homeboy_plan(plan.homeboy_plan.clone());

        let aggregate = scheduler.run(projected);
        let observed = observed.lock().expect("observed requests");
        let design = observed
            .iter()
            .find(|request| request.task_id == "design")
            .expect("design request dispatched");

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 2);
        assert_eq!(design.instructions, "Build design for issue #3447");
    }
}
