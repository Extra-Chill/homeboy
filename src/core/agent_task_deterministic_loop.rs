use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskExecutorAdapter, AgentTaskPlan,
    AgentTaskScheduler,
};
use crate::core::deterministic_loop::{
    DeterministicEvidenceRef, DeterministicLoopHooks, DeterministicLoopReconcileResult,
    DeterministicLoopState, DeterministicLoopStatus,
};
use crate::core::{Error, Result};

use super::agent_task_loop_definition::{compile_loop_definition, AgentTaskLoopDefinition};

pub struct AgentTaskDeterministicLoop<E> {
    definition: AgentTaskLoopDefinition,
    scheduler: AgentTaskScheduler<E>,
}

impl<E> AgentTaskDeterministicLoop<E>
where
    E: AgentTaskExecutorAdapter,
{
    pub fn new(definition: AgentTaskLoopDefinition, executor: E) -> Self {
        Self {
            definition,
            scheduler: AgentTaskScheduler::new(executor),
        }
    }
}

impl<E> DeterministicLoopHooks for AgentTaskDeterministicLoop<E>
where
    E: AgentTaskExecutorAdapter,
{
    type IterationPlan = AgentTaskPlan;
    type IterationOutput = AgentTaskAggregate;

    fn materialize_iteration(
        &mut self,
        state: &DeterministicLoopState,
    ) -> Result<Self::IterationPlan> {
        let mut plan = compile_loop_definition(self.definition.clone())?;
        plan.plan_id = format!("{}-iteration-{}", plan.plan_id, state.iteration);
        plan.metadata = merge_agent_task_loop_metadata(
            plan.metadata,
            serde_json::json!({
                "deterministic_loop": {
                    "loop_id": state.identity.loop_id,
                    "run_id": state.identity.run_id,
                    "iteration": state.iteration,
                }
            }),
        );
        plan.rebuild_homeboy_plan();
        Ok(plan)
    }

    fn execute_iteration(
        &mut self,
        _state: &DeterministicLoopState,
        plan: Self::IterationPlan,
    ) -> Result<Self::IterationOutput> {
        Ok(self.scheduler.run(plan))
    }

    fn reconcile_iteration(
        &mut self,
        _state: &DeterministicLoopState,
        output: Self::IterationOutput,
    ) -> Result<DeterministicLoopReconcileResult> {
        let status = match output.status {
            AgentTaskAggregateStatus::Succeeded => DeterministicLoopStatus::Succeeded,
            AgentTaskAggregateStatus::PartialFailure | AgentTaskAggregateStatus::Failed => {
                DeterministicLoopStatus::Failed
            }
            AgentTaskAggregateStatus::Cancelled => DeterministicLoopStatus::Canceled,
        };
        let mut result = DeterministicLoopReconcileResult::new(status);
        result.artifact_refs = output
            .artifact_lineage
            .iter()
            .filter_map(|artifact| {
                artifact.url.as_ref().or(artifact.path.as_ref()).map(|uri| {
                    DeterministicEvidenceRef {
                        uri: uri.clone(),
                        kind: Some(artifact.kind.clone()),
                        label: Some(artifact.name.clone()),
                    }
                })
            })
            .collect();
        for outcome in &output.outcomes {
            result
                .artifact_refs
                .extend(outcome.artifacts.iter().filter_map(|artifact| {
                    artifact.url.as_ref().or(artifact.path.as_ref()).map(|uri| {
                        DeterministicEvidenceRef {
                            uri: uri.clone(),
                            kind: Some(artifact.kind.clone()),
                            label: artifact.name.clone(),
                        }
                    })
                }));
            result
                .artifact_refs
                .extend(
                    outcome
                        .evidence_refs
                        .iter()
                        .map(|evidence| DeterministicEvidenceRef {
                            uri: evidence.uri.clone(),
                            kind: Some(evidence.kind.clone()),
                            label: evidence.label.clone(),
                        }),
                );
        }
        result.metadata = serde_json::json!({
            "agent_task": {
                "plan_id": output.plan_id,
                "status": output.status,
                "totals": output.totals,
                "events": output.events,
            }
        });
        Ok(result)
    }
}

fn merge_agent_task_loop_metadata(
    current: serde_json::Value,
    next: serde_json::Value,
) -> serde_json::Value {
    match (current, next) {
        (serde_json::Value::Object(mut current), serde_json::Value::Object(next)) => {
            current.extend(next);
            serde_json::Value::Object(current)
        }
        (serde_json::Value::Null, next) => next,
        (current, next) => serde_json::json!({
            "definition_metadata": current,
            "agent_task_loop": next,
        }),
    }
}

pub fn agent_task_loop_spec(
    definition: &AgentTaskLoopDefinition,
    max_iterations: u32,
) -> Result<crate::core::deterministic_loop::DeterministicLoopSpec> {
    if max_iterations == 0 {
        return Err(Error::validation_invalid_argument(
            "max_iterations",
            "agent-task deterministic loops require max_iterations greater than zero",
            Some(definition.loop_id.clone()),
            None,
        ));
    }
    Ok(crate::core::deterministic_loop::DeterministicLoopSpec {
        loop_id: definition.loop_id.clone(),
        max_iterations,
        metadata: serde_json::json!({
            "source_schema": definition.schema,
            "loop_id": definition.loop_id,
            "plan_id": definition.plan_id,
        }),
        ..crate::core::deterministic_loop::DeterministicLoopSpec::default()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use serde_json::{json, Value};

    use super::*;
    use crate::core::agent_task::{
        AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskFailureClassification, AgentTaskLimits,
        AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
        AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_loop_definition::AGENT_TASK_LOOP_DEFINITION_SCHEMA;
    use crate::core::agent_task_schedule::AgentTaskExecutionContext;
    use crate::core::deterministic_loop::{run_deterministic_loop, DeterministicLoopRunIdentity};

    #[derive(Clone)]
    struct RecordingExecutor {
        calls: Arc<AtomicUsize>,
        status: AgentTaskOutcomeStatus,
    }

    impl AgentTaskExecutorAdapter for RecordingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut artifacts = Vec::new();
            if self.status == AgentTaskOutcomeStatus::Succeeded {
                artifacts.push(AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: format!("{}-artifact", request.task_id),
                    kind: "agent_result".to_string(),
                    name: Some("agent_result".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(format!("artifact://{}", request.task_id)),
                    url: None,
                    mime: None,
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                });
            }
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: self.status,
                summary: None,
                failure_classification: (self.status == AgentTaskOutcomeStatus::Failed)
                    .then_some(AgentTaskFailureClassification::ExecutionFailed),
                artifacts,
                typed_artifacts: Vec::new(),
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "log".to_string(),
                    uri: "artifact://log".to_string(),
                    label: Some("log".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    #[test]
    fn agent_task_loop_runs_through_core_deterministic_primitive() {
        let definition = loop_definition();
        let calls = Arc::new(AtomicUsize::new(0));
        let executor = RecordingExecutor {
            calls: Arc::clone(&calls),
            status: AgentTaskOutcomeStatus::Succeeded,
        };
        let spec = agent_task_loop_spec(&definition, 2).expect("spec materializes");
        let identity = DeterministicLoopRunIdentity::new("example/loop", "run-a");
        let mut hooks = AgentTaskDeterministicLoop::new(definition, executor);

        let state = run_deterministic_loop(spec, identity, &mut hooks).expect("loop runs");

        assert_eq!(state.status, DeterministicLoopStatus::Succeeded);
        assert_eq!(state.iteration, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(state
            .artifact_lineage
            .iter()
            .any(|artifact| artifact.uri == "artifact://design"));
    }

    #[test]
    fn agent_task_loop_reconciles_failed_aggregate_to_failed_loop() {
        let definition = loop_definition();
        let executor = RecordingExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
            status: AgentTaskOutcomeStatus::Failed,
        };
        let spec = agent_task_loop_spec(&definition, 2).expect("spec materializes");
        let identity = DeterministicLoopRunIdentity::new("example/loop", "run-a");
        let mut hooks = AgentTaskDeterministicLoop::new(definition, executor);

        let state = run_deterministic_loop(spec, identity, &mut hooks).expect("loop runs");

        assert_eq!(state.status, DeterministicLoopStatus::Failed);
        assert_eq!(state.iteration, 1);
        assert_eq!(
            state.metadata["agent_task"]["status"],
            json!(AgentTaskAggregateStatus::Failed)
        );
    }

    fn loop_definition() -> AgentTaskLoopDefinition {
        serde_json::from_value(json!({
            "schema": AGENT_TASK_LOOP_DEFINITION_SCHEMA,
            "loop_id": "example/loop",
            "plan_id": "example-plan",
            "tasks": [
                { "task_id": "design", "request": request("design") },
                { "task_id": "verify", "request": request("verify"), "depends_on": ["design"] }
            ]
        }))
        .expect("definition parses")
    }

    fn request(task_id: &str) -> Value {
        json!({
            "schema": AGENT_TASK_REQUEST_SCHEMA,
            "task_id": task_id,
            "executor": { "backend": "in-memory", "config": {} },
            "instructions": format!("Run {task_id}"),
            "workspace": AgentTaskWorkspace::default(),
            "policy": AgentTaskPolicy::default(),
            "limits": AgentTaskLimits::default(),
            "inputs": null
        })
    }
}
