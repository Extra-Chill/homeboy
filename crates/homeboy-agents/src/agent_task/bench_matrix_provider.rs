//! Agent-task implementation of the bench agent-task matrix hook.
//!
//! Projects cross-rig bench comparison entries into an agent-task matrix (plan +
//! aggregate) and serializes it to JSON for core's bench comparison report,
//! provided through the `BenchAgentTaskMatrixProvider` hook so bench does not
//! depend on the agent-task subsystem directly.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::agent_task::{
    expand_agent_task_matrix, AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskExecutor,
    AgentTaskLimits, AgentTaskMatrixAggregate, AgentTaskMatrixAxis, AgentTaskMatrixPlan,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
    AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
};
use homeboy_extension::bench::diagnostic::BenchDiagnostic;
use homeboy_extension::bench::report::comparison::agent_task_matrix_provider::{
    register_bench_agent_task_matrix_provider, BenchAgentTaskMatrixProvider,
};
use homeboy_extension::bench::report::comparison::RigBenchEntry;

struct AgentTaskBenchMatrixProvider;

impl BenchAgentTaskMatrixProvider for AgentTaskBenchMatrixProvider {
    fn bench_agent_task_matrix(
        &self,
        component: &str,
        iterations: u64,
        entries: &[RigBenchEntry],
        axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
    ) -> Option<(Value, Value)> {
        let (plan, aggregate) =
            build_agent_task_matrix_summary(component, iterations, entries, axes_by_rig)?;
        Some((
            serde_json::to_value(plan).ok()?,
            serde_json::to_value(aggregate).ok()?,
        ))
    }
}

/// Register the agent-task bench-matrix provider. Called once at startup so
/// core's bench comparison report can include an agent-task matrix without
/// depending on the agent-task subsystem.
pub fn register() {
    register_bench_agent_task_matrix_provider(Box::new(AgentTaskBenchMatrixProvider));
}

fn build_agent_task_matrix_summary(
    component: &str,
    iterations: u64,
    entries: &[RigBenchEntry],
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
) -> Option<(AgentTaskMatrixPlan, AgentTaskMatrixAggregate)> {
    if axes_by_rig.is_empty() {
        return None;
    }

    let axes = agent_task_axes(axes_by_rig);
    if axes.is_empty() {
        return None;
    }

    let plan = expand_agent_task_matrix(
        format!("bench/{component}"),
        axes,
        AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "template".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "homeboy.bench".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: vec!["bench".to_string()],
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "Run the selected bench matrix cell.".to_string(),
            inputs: json!({
                "component": component,
                "iterations": iterations,
            }),
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: vec!["bench-results".to_string()],
            artifact_declarations: Vec::new(),
            metadata: json!({ "source": "bench.comparison" }),
        },
    )
    .ok()?;

    let outcomes = entries
        .iter()
        .filter_map(|entry| agent_task_outcome_for_entry(entry, axes_by_rig, &plan))
        .collect::<Vec<_>>();
    let aggregate = AgentTaskMatrixAggregate::from_outcomes(&plan, &outcomes);

    Some((plan, aggregate))
}

fn agent_task_axes(
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
) -> Vec<AgentTaskMatrixAxis> {
    let mut values_by_axis: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for axes in axes_by_rig.values() {
        for (axis, value) in axes {
            let values = values_by_axis.entry(axis.clone()).or_default();
            if !values.contains(value) {
                values.push(value.clone());
            }
        }
    }

    values_by_axis
        .into_iter()
        .map(|(name, values)| AgentTaskMatrixAxis { name, values })
        .collect()
}

fn agent_task_outcome_for_entry(
    entry: &RigBenchEntry,
    axes_by_rig: &BTreeMap<String, BTreeMap<String, String>>,
    plan: &AgentTaskMatrixPlan,
) -> Option<AgentTaskOutcome> {
    let axes = axes_by_rig.get(&entry.rig_id)?;
    let task_id = plan
        .cells
        .iter()
        .find(|cell| &cell.axes == axes)
        .map(|cell| cell.task.task_id.clone())?;

    Some(AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status: if entry.passed {
            AgentTaskOutcomeStatus::Succeeded
        } else {
            AgentTaskOutcomeStatus::Failed
        },
        summary: Some(entry.status.clone()),
        failure_classification: None,
        outputs: serde_json::Value::Null,
        artifacts: entry
            .artifacts
            .iter()
            .enumerate()
            .map(|(index, artifact)| AgentTaskArtifact {
                id: format!("{}:{}", entry.rig_id, index),
                kind: artifact
                    .kind
                    .clone()
                    .or_else(|| artifact.artifact_type.clone())
                    .unwrap_or_else(|| "bench_artifact".to_string()),
                name: Some(artifact.name.clone()),
                label: artifact.label.clone(),
                role: artifact.role.clone(),
                semantic_key: None,
                path: artifact.path.clone(),
                url: artifact.url.clone(),
                mime: None,
                size_bytes: None,
                sha256: None,
                metadata: json!({
                    "scenario_id": artifact.scenario_id,
                    "run_index": artifact.run_index,
                    "label": artifact.label,
                }),
                schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            })
            .collect(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: entry
            .diagnostics
            .iter()
            .map(agent_task_diagnostic)
            .collect(),
        workflow: None,
        follow_up: None,
        metadata: json!({
            "rig_id": entry.rig_id,
            "status": entry.status,
            "exit_code": entry.exit_code,
        }),
    })
}

fn agent_task_diagnostic(diagnostic: &BenchDiagnostic) -> AgentTaskDiagnostic {
    AgentTaskDiagnostic {
        class: diagnostic.class.clone(),
        message: diagnostic.message.clone().unwrap_or_default(),
        data: json!({
            "source": diagnostic.source,
            "metadata": diagnostic.metadata,
        }),
    }
}
