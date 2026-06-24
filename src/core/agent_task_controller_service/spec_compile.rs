//! Split from `agent_task_controller_service` god file (#5208). Structural move only.
#![allow(unused_imports)]
use super::*;
use crate::core::component::{self, TargetSpec};
use crate::core::plan::PlanStepDependencyKind;

pub(super) const REPO_LOOP_SPEC_METADATA_KEY: &str = "repo_loop_spec";
pub(super) const REPO_LOOP_SPEC_WORKFLOW_REASON: &str = "repo loop spec workflow";
pub(super) const REPO_LOOP_SPEC_ACTION_REASON: &str = "repo loop spec action";

#[derive(Debug, Default)]
pub(super) struct RepoLoopSpecReconciliation {
    pub(super) removed_action_count: usize,
    pub(super) removed_dedupe_key_count: usize,
}

pub(super) fn repo_loop_spec_fingerprint(spec: &AgentTaskRepoLoopSpec) -> Result<String> {
    let bytes = serde_json::to_vec(spec).map_err(|error| {
        Error::internal_json(
            format!("repo loop spec fingerprint serialization failed: {error}"),
            Some("agent-task controller from-spec".to_string()),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub(super) fn repo_loop_spec_fingerprint_from_metadata(
    record: &AgentTaskLoopControllerRecord,
) -> Option<String> {
    record
        .metadata
        .get(REPO_LOOP_SPEC_METADATA_KEY)
        .and_then(|value| value.get("fingerprint"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) fn set_repo_loop_spec_metadata(
    record: &mut AgentTaskLoopControllerRecord,
    spec: &AgentTaskRepoLoopSpec,
    fingerprint: &str,
) {
    let mut metadata = match std::mem::take(&mut record.metadata) {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("repo_loop_metadata".to_string(), other);
            map
        }
    };
    metadata.insert(
        REPO_LOOP_SPEC_METADATA_KEY.to_string(),
        serde_json::json!({
            "schema": spec.schema,
            "fingerprint": fingerprint,
            "config_version": spec.config_version,
        }),
    );
    record.metadata = Value::Object(metadata);
}

pub(super) fn controller_spec_homeboy_plan(
    spec: &AgentTaskRepoLoopSpec,
    spec_fingerprint: &str,
    record: &AgentTaskLoopControllerRecord,
    actions: &[AgentTaskLoopPolicyActionRecord],
) -> Result<HomeboyPlan> {
    let mut plan = HomeboyPlan::for_description(
        PlanKind::Controller,
        format!("controller spec {}", record.loop_id),
    );
    plan.id = format!("controller.{}", record.loop_id);
    plan.mode = Some("plan".to_string());
    plan.inputs.insert(
        "schema".to_string(),
        Value::String("homeboy/controller-spec-plan/v1".to_string()),
    );
    plan.inputs
        .insert("loop_id".to_string(), Value::String(record.loop_id.clone()));
    plan.inputs
        .insert("phase".to_string(), Value::String(record.phase.clone()));
    plan.inputs.insert(
        "config_version".to_string(),
        Value::String(record.config_version.clone()),
    );
    plan.inputs.insert(
        "spec_fingerprint".to_string(),
        Value::String(spec_fingerprint.to_string()),
    );
    plan.inputs.insert(
        "controller".to_string(),
        serde_json::to_value(record)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    );
    plan.inputs.insert(
        "declarations".to_string(),
        controller_spec_declarations(spec)?,
    );
    plan.steps = actions
        .iter()
        .map(controller_action_plan_step)
        .collect::<Result<Vec<_>>>()?;
    plan.summary = Some(crate::core::plan::PlanSummary {
        total_steps: plan.steps.len(),
        ready: plan
            .steps
            .iter()
            .filter(|step| step.status == PlanStepStatus::Ready)
            .count(),
        blocked: plan
            .steps
            .iter()
            .filter(|step| step.status == PlanStepStatus::Missing)
            .count(),
        skipped: plan
            .steps
            .iter()
            .filter(|step| matches!(step.status, PlanStepStatus::Skipped | PlanStepStatus::Disabled))
            .count(),
        next_actions: vec!["Run `homeboy agent-task controller from-spec <spec> --resume` to persist and execute this controller spec.".to_string()],
    });
    Ok(plan)
}

pub(super) fn controller_spec_declarations(spec: &AgentTaskRepoLoopSpec) -> Result<Value> {
    Ok(serde_json::json!({
        "agents": spec.agents,
        "tools": spec.tools,
        "abilities": spec.abilities,
        "workflows": spec.workflows,
        "artifacts": spec.artifacts,
        "dependencies": spec.dependencies,
        "gates": spec.gates,
        "metrics": spec.metrics,
        "gate_bundles": spec.gate_bundles,
        "phases": spec.phases,
        "policy": spec.policy,
    }))
}

pub(super) fn controller_action_plan_step(
    action: &AgentTaskLoopPolicyActionRecord,
) -> Result<PlanStep> {
    let action_value = serde_json::to_value(&action.action)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let action_name = action_value
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let status = match action.status {
        AgentTaskLoopActionStatus::Pending => PlanStepStatus::Ready,
        AgentTaskLoopActionStatus::AlreadySatisfied => PlanStepStatus::Skipped,
        AgentTaskLoopActionStatus::BlockedRunnerUnavailable
        | AgentTaskLoopActionStatus::BlockedRemoteMaterialization
        | AgentTaskLoopActionStatus::BlockedLocalFallbackDenied => PlanStepStatus::Missing,
        AgentTaskLoopActionStatus::Running => PlanStepStatus::Running,
        AgentTaskLoopActionStatus::Completed => PlanStepStatus::Success,
        AgentTaskLoopActionStatus::Failed => PlanStepStatus::Failed,
    };
    let mut inputs = HashMap::from([
        ("action".to_string(), action_value),
        ("reason".to_string(), Value::String(action.reason.clone())),
    ]);
    if let Some(dedupe_key) = &action.dedupe_key {
        inputs.insert("dedupe_key".to_string(), Value::String(dedupe_key.clone()));
    }
    let mut outputs = HashMap::new();
    outputs.insert(
        "controller_action".to_string(),
        serde_json::to_value(action)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    );

    Ok(PlanStep {
        id: action.action_id.clone(),
        kind: format!("controller.{action_name}"),
        label: action
            .dedupe_key
            .clone()
            .or_else(|| Some(action_name.to_string())),
        blocking: !matches!(
            action_name.as_str(),
            "wait_for_event" | "wait_for_controller"
        ),
        scope: action
            .dedupe_key
            .as_ref()
            .map(|value| vec![value.clone()])
            .unwrap_or_default(),
        needs: Vec::new(),
        needs_kind: PlanStepDependencyKind::Execution,
        status,
        inputs,
        outputs,
        skip_reason: (action.status == AgentTaskLoopActionStatus::AlreadySatisfied)
            .then(|| "controller action dedupe key is already satisfied".to_string()),
        policy: HashMap::new(),
        missing: action
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.clone())
            .collect(),
    })
}

pub(super) fn reconcile_repo_loop_spec_actions(
    record: &mut AgentTaskLoopControllerRecord,
    previous_fingerprint: Option<&str>,
    current_fingerprint: &str,
) -> Result<RepoLoopSpecReconciliation> {
    if previous_fingerprint == Some(current_fingerprint) {
        return Ok(RepoLoopSpecReconciliation::default());
    }

    let managed_action_ids: Vec<String> = record
        .next_actions
        .iter()
        .filter(|action| is_repo_loop_spec_action(action))
        .map(|action| action.action_id.clone())
        .collect();
    if managed_action_ids.is_empty() {
        return Ok(RepoLoopSpecReconciliation::default());
    }

    if let Some(running) = record.next_actions.iter().find(|action| {
        is_repo_loop_spec_action(action) && action.status == AgentTaskLoopActionStatus::Running
    }) {
        return Err(Error::validation_invalid_argument(
            "spec_fingerprint",
            format!(
                "repo loop spec changed for '{}' while repo-spec action '{}' is running; wait for it to finish before reapplying the spec",
                record.loop_id, running.action_id
            ),
            previous_fingerprint.map(ToString::to_string),
            Some(vec![current_fingerprint.to_string()]),
        ));
    }

    let mut dedupe_keys = record
        .next_actions
        .iter()
        .filter(|action| is_repo_loop_spec_action(action))
        .filter_map(|action| action.dedupe_key.clone())
        .collect::<Vec<_>>();
    dedupe_keys.sort();
    dedupe_keys.dedup();

    let removed_action_count = managed_action_ids.len();
    record
        .next_actions
        .retain(|action| !is_repo_loop_spec_action(action));
    let mut removed_dedupe_key_count = 0;
    for dedupe_key in dedupe_keys {
        if record.dedupe_keys.remove(&dedupe_key).is_some() {
            removed_dedupe_key_count += 1;
        }
    }

    Ok(RepoLoopSpecReconciliation {
        removed_action_count,
        removed_dedupe_key_count,
    })
}

pub(super) fn is_repo_loop_spec_action(action: &AgentTaskLoopPolicyActionRecord) -> bool {
    action.reason == REPO_LOOP_SPEC_WORKFLOW_REASON || action.reason == REPO_LOOP_SPEC_ACTION_REASON
}

pub(crate) fn validate_loop_spec(spec: &AgentTaskRepoLoopSpec) -> Result<()> {
    if spec.loop_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "loop_id",
            "repo loop spec requires a non-empty loop_id",
            None,
            None,
        ));
    }
    for (index, phase) in spec.phases.iter().enumerate() {
        if phase.phase.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                format!("phases[{index}].phase"),
                "repo loop spec phase requires a non-empty phase name",
                None,
                None,
            ));
        }
    }
    for (index, workflow) in spec.workflows.iter().enumerate() {
        if workflow.workflow_id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                format!("workflows[{index}].workflow_id"),
                "repo loop spec workflow requires a non-empty workflow_id",
                None,
                None,
            ));
        }
        if workflow
            .prompt
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
            && workflow.tasks.is_empty()
        {
            return Err(Error::validation_invalid_argument(
                format!("workflows[{index}]"),
                "repo loop spec workflow requires prompt or tasks",
                None,
                None,
            ));
        }
        if let Some(agent_id) = &workflow.agent_id {
            validate_declared_id(
                format!("workflows[{index}].agent_id"),
                agent_id,
                &spec.agents,
                |agent| &agent.agent_id,
            )?;
        }
        validate_declared_ids(
            format!("workflows[{index}].tools"),
            &workflow.tools,
            &spec.tools,
            |tool| &tool.tool_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].abilities"),
            &workflow.abilities,
            &spec.abilities,
            |ability| &ability.ability_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].artifacts"),
            &workflow.artifacts,
            &spec.artifacts,
            |artifact| &artifact.artifact_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].consumes"),
            &workflow.consumes,
            &spec.artifacts,
            |artifact| &artifact.artifact_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].emits"),
            &workflow.emits,
            &spec.artifacts,
            |artifact| &artifact.artifact_id,
        )?;
        validate_workflow_dependencies(
            format!("workflows[{index}].dependencies"),
            &workflow.dependencies,
            spec,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].gates"),
            &workflow.gates,
            &spec.gates,
            |gate| &gate.gate_id,
        )?;
        validate_declared_ids(
            format!("workflows[{index}].metrics"),
            &workflow.metrics,
            &spec.metrics,
            |metric| &metric.metric_id,
        )?;
    }
    for (index, agent) in spec.agents.iter().enumerate() {
        validate_declared_ids(
            format!("agents[{index}].tools"),
            &agent.tools,
            &spec.tools,
            |tool| &tool.tool_id,
        )?;
        validate_declared_ids(
            format!("agents[{index}].abilities"),
            &agent.abilities,
            &spec.abilities,
            |ability| &ability.ability_id,
        )?;
    }
    validate_artifact_graph_edges(spec)?;
    Ok(())
}

pub(super) fn validate_artifact_graph_edges(spec: &AgentTaskRepoLoopSpec) -> Result<()> {
    let mut diagnostics = Vec::new();
    for (index, edge) in spec.artifact_graph.iter().enumerate() {
        if !spec
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == edge.artifact_id)
        {
            diagnostics.push(format!(
                "artifact_graph[{index}].artifact_id references undeclared artifact '{}'",
                edge.artifact_id
            ));
        }
        let producer = spec
            .workflows
            .iter()
            .find(|workflow| workflow.workflow_id == edge.from_workflow_id);
        let consumer = spec
            .workflows
            .iter()
            .find(|workflow| workflow.workflow_id == edge.to_workflow_id);
        if producer.is_none() {
            diagnostics.push(format!(
                "artifact_graph[{index}].from_workflow_id references undeclared workflow '{}'",
                edge.from_workflow_id
            ));
        }
        if consumer.is_none() {
            diagnostics.push(format!(
                "artifact_graph[{index}].to_workflow_id references undeclared workflow '{}'",
                edge.to_workflow_id
            ));
        }
        if let Some(producer) = producer {
            if !producer.artifacts.contains(&edge.artifact_id)
                && !producer.emits.contains(&edge.artifact_id)
            {
                diagnostics.push(format!(
                    "artifact_graph[{index}] producer workflow '{}' does not emit artifact '{}'",
                    edge.from_workflow_id, edge.artifact_id
                ));
            }
        }
        if let Some(consumer) = consumer {
            if !consumer.consumes.contains(&edge.artifact_id)
                && !consumer.dependencies.contains(&edge.artifact_id)
            {
                diagnostics.push(format!(
                    "artifact_graph[{index}] consumer workflow '{}' does not consume artifact '{}'",
                    edge.to_workflow_id, edge.artifact_id
                ));
            }
        }
    }
    if diagnostics.is_empty() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "artifact_graph",
        "repo loop spec artifact_graph edges must reference declared workflow artifact flow",
        Some(spec.loop_id.clone()),
        Some(diagnostics),
    ))
}

/// Schema emitted by [`compile_executable_plan_from_spec`].
///
/// Distinct from the `homeboy/controller-spec-plan/v1` projection produced by
/// [`controller_spec_homeboy_plan`]: the controller-spec plan flattens queued
/// controller actions for dry-run inspection, whereas this schema is the
/// executable agent-task plan whose stages and inter-stage dependencies are
/// derived from the loop controller spec as the single source of truth.
pub(crate) const EXECUTABLE_PLAN_FROM_SPEC_SCHEMA: &str = "homeboy/agent-task-plan-from-spec/v1";

/// Artifact `kind` suffix that marks an artifact as a Homeboy-owned runtime
/// artifact (e.g. `static_validation_run`). Runtime artifacts are synthesized
/// by a Homeboy runtime stage rather than authored/emitted by a repo workflow,
/// so downstream specs can reference them without hard-coding Homeboy/Codebox
/// internals.
pub(crate) const HOMEBOY_RUNTIME_ARTIFACT_KIND_SUFFIX: &str = "_run";

/// Stage `kind` used for the synthetic Homeboy runtime stage that produces a
/// Homeboy-owned runtime artifact in the executable plan.
pub(crate) const HOMEBOY_RUNTIME_STAGE_KIND: &str = "homeboy_runtime_artifact";

/// True when an artifact is a Homeboy-owned runtime artifact: either its `kind`
/// carries the runtime suffix, or no declared workflow produces it while at
/// least one workflow consumes it (so the producer must be a Homeboy runtime,
/// not a repo workflow). Keeping this derivation generic lets repos reference
/// runtime artifacts like `static_validation_run` without WPSG embedding
/// Homeboy-owned producers in its own spec.
pub(crate) fn is_homeboy_runtime_artifact(
    spec: &AgentTaskRepoLoopSpec,
    artifact: &AgentTaskRepoLoopSpecArtifact,
) -> bool {
    if artifact
        .kind
        .ends_with(HOMEBOY_RUNTIME_ARTIFACT_KIND_SUFFIX)
    {
        return true;
    }
    let produced_by_workflow = spec.workflows.iter().any(|workflow| {
        workflow.artifacts.contains(&artifact.artifact_id)
            || workflow.emits.contains(&artifact.artifact_id)
            || spec.artifact_graph.iter().any(|edge| {
                edge.artifact_id == artifact.artifact_id
                    && edge.from_workflow_id == workflow.workflow_id
            })
    });
    if produced_by_workflow {
        return false;
    }
    let consumed_by_workflow = spec.workflows.iter().any(|workflow| {
        workflow.consumes.contains(&artifact.artifact_id)
            || workflow.dependencies.contains(&artifact.artifact_id)
            || spec.artifact_graph.iter().any(|edge| {
                edge.artifact_id == artifact.artifact_id
                    && edge.to_workflow_id == workflow.workflow_id
            })
    });
    consumed_by_workflow
}

/// Collect the Homeboy-owned runtime artifacts a spec depends on, in declared order.
pub(crate) fn homeboy_runtime_artifacts(
    spec: &AgentTaskRepoLoopSpec,
) -> Vec<&AgentTaskRepoLoopSpecArtifact> {
    spec.artifacts
        .iter()
        .filter(|artifact| is_homeboy_runtime_artifact(spec, artifact))
        .collect()
}

/// Validate that the spec's executable task bindings (`consumes`/`emits` and
/// `dependencies` that reference artifacts) are backed by declared
/// `artifact_flow` edges, so the executable plan can consume the controller
/// spec as the single source of truth without inferring undeclared data flow.
///
/// Surfaces every binding violation at once as structured diagnostics rather
/// than failing on the first mismatch, so a repo author can fix the spec in one
/// pass. Homeboy-owned runtime artifacts are exempt from requiring a producer
/// workflow edge because their producer is a Homeboy runtime stage, not a repo
/// workflow.
pub(crate) fn validate_artifact_flow_bindings(spec: &AgentTaskRepoLoopSpec) -> Result<()> {
    let mut diagnostics = Vec::new();
    for workflow in &spec.workflows {
        let mut consumed: Vec<&String> = workflow.consumes.iter().collect();
        for dependency in &workflow.dependencies {
            if spec
                .artifacts
                .iter()
                .any(|artifact| &artifact.artifact_id == dependency)
                && !consumed.contains(&dependency)
            {
                consumed.push(dependency);
            }
        }
        for artifact_id in consumed {
            let runtime_owned = spec
                .artifacts
                .iter()
                .find(|artifact| &artifact.artifact_id == artifact_id)
                .map(|artifact| is_homeboy_runtime_artifact(spec, artifact))
                .unwrap_or(false);
            if runtime_owned {
                // Producer is a Homeboy runtime stage; no repo workflow edge required.
                continue;
            }
            let has_flow_edge = spec.artifact_graph.iter().any(|edge| {
                &edge.artifact_id == artifact_id && edge.to_workflow_id == workflow.workflow_id
            });
            // A consume binding must be backed by an explicit emit or an
            // artifact_flow edge. A producer merely declaring the artifact in
            // its `artifacts` set does not establish a consume flow.
            let has_repo_producer = spec.workflows.iter().any(|producer| {
                producer.workflow_id != workflow.workflow_id
                    && producer.emits.contains(artifact_id)
            });
            if !has_flow_edge && !has_repo_producer {
                diagnostics.push(format!(
                    "workflow '{}' consumes artifact '{}' but no artifact_flow edge or producing workflow declares it; add an artifact_graph edge or declare the artifact as a Homeboy-owned runtime artifact",
                    workflow.workflow_id, artifact_id
                ));
            }
        }
        for artifact_id in &workflow.emits {
            let declared = spec
                .artifacts
                .iter()
                .any(|artifact| &artifact.artifact_id == artifact_id);
            if !declared {
                diagnostics.push(format!(
                    "workflow '{}' emits undeclared artifact '{}'",
                    workflow.workflow_id, artifact_id
                ));
            }
        }
    }
    if diagnostics.is_empty() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "artifact_flow",
        "repo loop spec task bindings must be backed by declared artifact_flow edges or producing workflows",
        Some(spec.loop_id.clone()),
        Some(diagnostics),
    ))
}

/// Compile a loop controller spec into an executable agent-task plan whose
/// stages and inter-stage dependencies are derived from the spec.
///
/// This is the from-spec compiler primitive (#5101): the executable plan builder
/// consumes the controller spec as the single source of truth instead of being
/// authored separately. Each workflow becomes one executable stage; stage
/// dependencies (`needs`) are derived from `artifact_flow` (artifact_graph)
/// edges plus `consumes`/`emits` bindings. Homeboy-owned runtime artifacts (e.g.
/// `static_validation_run`) are represented as synthetic runtime stages that the
/// consuming workflow stages depend on, so the data flow is complete without the
/// repo declaring Homeboy internals.
///
/// Validation (`validate_loop_spec` + `validate_artifact_flow_bindings`) runs
/// first; binding violations are surfaced as structured errors.
pub(crate) fn compile_executable_plan_from_spec(
    spec: &AgentTaskRepoLoopSpec,
) -> Result<HomeboyPlan> {
    validate_loop_spec(spec)?;
    validate_artifact_flow_bindings(spec)?;

    let spec_fingerprint = repo_loop_spec_fingerprint(spec)?;
    let mut plan = HomeboyPlan::for_description(
        PlanKind::AgentTask,
        format!("executable plan from loop spec {}", spec.loop_id),
    );
    plan.id = format!("plan-from-spec.{}", spec.loop_id);
    plan.mode = Some("plan".to_string());
    plan.inputs.insert(
        "schema".to_string(),
        Value::String(EXECUTABLE_PLAN_FROM_SPEC_SCHEMA.to_string()),
    );
    plan.inputs
        .insert("loop_id".to_string(), Value::String(spec.loop_id.clone()));
    plan.inputs.insert(
        "spec_fingerprint".to_string(),
        Value::String(spec_fingerprint),
    );
    plan.inputs.insert(
        "declarations".to_string(),
        controller_spec_declarations(spec)?,
    );

    // Synthetic Homeboy-owned runtime stages: one per runtime artifact, emitting
    // the artifact so consuming workflow stages can declare a dependency on a
    // concrete producer stage instead of hard-coding Homeboy internals.
    let runtime_artifacts = homeboy_runtime_artifacts(spec);
    let runtime_stage_id = |artifact_id: &str| -> String { format!("runtime:{artifact_id}") };
    for artifact in &runtime_artifacts {
        let mut data = HashMap::new();
        data.insert("kind".to_string(), Value::String(artifact.kind.clone()));
        data.insert("required".to_string(), Value::Bool(artifact.required));
        data.insert(
            "owner".to_string(),
            Value::String("homeboy_runtime".to_string()),
        );
        if let Some(description) = &artifact.description {
            data.insert(
                "description".to_string(),
                Value::String(description.clone()),
            );
        }
        plan.artifacts.push(PlanArtifact {
            id: artifact.artifact_id.clone(),
            path: None,
            artifact_type: Some(artifact.kind.clone()),
            data,
        });
        plan.steps.push(PlanStep {
            id: runtime_stage_id(&artifact.artifact_id),
            kind: HOMEBOY_RUNTIME_STAGE_KIND.to_string(),
            label: Some(artifact.artifact_id.clone()),
            blocking: artifact.required,
            scope: Vec::new(),
            needs: Vec::new(),
            needs_kind: PlanStepDependencyKind::Execution,
            status: PlanStepStatus::Ready,
            inputs: HashMap::from([(
                "runtime_artifact".to_string(),
                serde_json::json!({
                    "artifact_id": artifact.artifact_id,
                    "kind": artifact.kind,
                    "required": artifact.required,
                    "owner": "homeboy_runtime",
                }),
            )]),
            outputs: HashMap::from([(
                "emits".to_string(),
                Value::Array(vec![Value::String(artifact.artifact_id.clone())]),
            )]),
            skip_reason: None,
            policy: HashMap::new(),
            missing: Vec::new(),
        });
    }

    // One executable stage per workflow; dependencies derived from artifact flow.
    for workflow in &spec.workflows {
        let needs = executable_stage_dependencies(spec, workflow, &runtime_artifacts);
        plan.steps.push(PlanStep {
            id: format!("stage:{}", workflow.workflow_id),
            kind: "agent_task_dispatch".to_string(),
            label: Some(workflow.workflow_id.clone()),
            blocking: true,
            scope: workflow_fan_out_entity_ids(workflow)?,
            needs,
            needs_kind: PlanStepDependencyKind::Execution,
            status: PlanStepStatus::Ready,
            inputs: HashMap::from([
                (
                    "workflow".to_string(),
                    workflow_declaration_context(spec, workflow)?,
                ),
                (
                    "plan".to_string(),
                    serde_json::to_value(workflow_homeboy_plan(spec, workflow)?)
                        .map_err(|error| Error::internal_json(error.to_string(), None))?,
                ),
            ]),
            outputs: HashMap::from([(
                "emits".to_string(),
                Value::Array(
                    workflow_emitted_artifacts(workflow)
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                ),
            )]),
            skip_reason: None,
            policy: HashMap::new(),
            missing: Vec::new(),
        });
    }

    plan.summary = Some(crate::core::plan::PlanSummary::from_steps(&plan.steps));
    Ok(plan)
}

/// Artifacts a workflow produces (`artifacts` + `emits` + outbound flow edges).
fn workflow_emitted_artifacts(workflow: &AgentTaskRepoLoopSpecWorkflow) -> Vec<String> {
    let mut emitted = workflow.artifacts.clone();
    for artifact_id in &workflow.emits {
        if !emitted.contains(artifact_id) {
            emitted.push(artifact_id.clone());
        }
    }
    emitted
}

/// Derive the executable stage dependencies (`needs`) for a workflow from the
/// artifact flow: every consumed artifact resolves to the stage that produces
/// it — either the producing workflow stage or the synthetic Homeboy runtime
/// stage for a Homeboy-owned runtime artifact. Explicit workflow-id
/// dependencies are preserved as stage dependencies as well.
fn executable_stage_dependencies(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
    runtime_artifacts: &[&AgentTaskRepoLoopSpecArtifact],
) -> Vec<String> {
    let mut needs: Vec<String> = Vec::new();
    fn push_unique(needs: &mut Vec<String>, value: String) {
        if !needs.contains(&value) {
            needs.push(value);
        }
    }

    // Consumed artifacts: map each to its producing stage.
    let mut consumed: Vec<String> = workflow.consumes.clone();
    for dependency in &workflow.dependencies {
        if spec
            .artifacts
            .iter()
            .any(|artifact| &artifact.artifact_id == dependency)
            && !consumed.contains(dependency)
        {
            consumed.push(dependency.clone());
        }
    }
    for edge in spec
        .artifact_graph
        .iter()
        .filter(|edge| edge.to_workflow_id == workflow.workflow_id)
    {
        if !consumed.contains(&edge.artifact_id) {
            consumed.push(edge.artifact_id.clone());
        }
    }

    for artifact_id in &consumed {
        if runtime_artifacts
            .iter()
            .any(|artifact| &artifact.artifact_id == artifact_id)
        {
            push_unique(&mut needs, format!("runtime:{artifact_id}"));
            continue;
        }
        // Prefer the explicit flow edge producer; otherwise any producing workflow.
        let producer = spec
            .artifact_graph
            .iter()
            .find(|edge| {
                &edge.artifact_id == artifact_id && edge.to_workflow_id == workflow.workflow_id
            })
            .map(|edge| edge.from_workflow_id.clone())
            .or_else(|| {
                spec.workflows
                    .iter()
                    .find(|producer| {
                        producer.workflow_id != workflow.workflow_id
                            && (producer.artifacts.contains(artifact_id)
                                || producer.emits.contains(artifact_id))
                    })
                    .map(|producer| producer.workflow_id.clone())
            });
        if let Some(producer) = producer {
            push_unique(&mut needs, format!("stage:{producer}"));
        }
    }

    // Explicit workflow-id dependencies become stage dependencies.
    for dependency in &workflow.dependencies {
        if spec
            .workflows
            .iter()
            .any(|candidate| &candidate.workflow_id == dependency)
        {
            push_unique(&mut needs, format!("stage:{dependency}"));
        }
    }

    needs
}

pub(super) fn validate_declared_ids<T, F>(
    field: String,
    requested: &[String],
    items: &[T],
    id: F,
) -> Result<()>
where
    F: Fn(&T) -> &String + Copy,
{
    for value in requested {
        validate_declared_id(field.clone(), value, items, id)?;
    }
    Ok(())
}

pub(super) fn validate_declared_id<T, F>(
    field: String,
    requested: &str,
    items: &[T],
    id: F,
) -> Result<()>
where
    F: Fn(&T) -> &String,
{
    if items.iter().any(|item| id(item) == requested) {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        field,
        "repo loop spec references an undeclared contract id",
        Some(requested.to_string()),
        None,
    ))
}

pub(super) fn validate_workflow_dependencies(
    field: String,
    requested: &[String],
    spec: &AgentTaskRepoLoopSpec,
) -> Result<()> {
    for value in requested {
        if spec
            .dependencies
            .iter()
            .any(|dependency| dependency.dependency_id == value.as_str())
            || spec
                .artifacts
                .iter()
                .any(|artifact| artifact.artifact_id == value.as_str())
        {
            continue;
        }

        let mut known_ids: Vec<String> = spec
            .dependencies
            .iter()
            .map(|dependency| format!("dependency:{}", dependency.dependency_id))
            .chain(
                spec.artifacts
                    .iter()
                    .map(|artifact| format!("artifact:{}", artifact.artifact_id)),
            )
            .collect();
        known_ids.sort();
        return Err(Error::validation_invalid_argument(
            field,
            "repo loop spec workflow dependency must reference a declared dependency_id or artifact_id",
            Some(value.clone()),
            Some(known_ids),
        ));
    }
    Ok(())
}

pub(super) fn compile_loop_spec_workflows(
    spec: &AgentTaskRepoLoopSpec,
) -> Result<Vec<AgentTaskLoopPolicyAction>> {
    spec.workflows
        .iter()
        .map(|workflow| compile_loop_spec_workflow(spec, workflow))
        .collect()
}

pub(super) fn compile_loop_spec_workflow(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<AgentTaskLoopPolicyAction> {
    let request = workflow_dispatch_request(spec, workflow)?;
    let dedupe_key = format!("workflow:{}", workflow.workflow_id);
    let fan_out_entity_ids = workflow_fan_out_entity_ids(workflow)?;
    if fan_out_entity_ids.is_empty() {
        Ok(AgentTaskLoopPolicyAction::SpawnTask {
            dedupe_key,
            entity_id: None,
            request,
        })
    } else {
        let fan_out = workflow.fan_out.as_ref();
        Ok(AgentTaskLoopPolicyAction::FanOut {
            dedupe_key,
            entity_ids: fan_out_entity_ids,
            max_items: fan_out
                .and_then(|fan_out| fan_out.max_items)
                .unwrap_or(crate::core::agent_task_loop_controller::DEFAULT_FAN_OUT_MAX_ITEMS),
            fail_fast: fan_out
                .and_then(|fan_out| fan_out.fail_fast)
                .unwrap_or(true),
            request_template: request,
        })
    }
}

fn workflow_fan_out_entity_ids(workflow: &AgentTaskRepoLoopSpecWorkflow) -> Result<Vec<String>> {
    if !workflow.entity_ids.is_empty() {
        return Ok(workflow.entity_ids.clone());
    }

    let Some(fan_out) = &workflow.fan_out else {
        return Ok(Vec::new());
    };
    if !fan_out.entity_ids.is_empty() {
        return Ok(fan_out.entity_ids.clone());
    }

    Err(Error::validation_invalid_argument(
        "workflows[].fan_out",
        "repo loop spec workflow fan_out requires concrete entity_ids/items; dynamic fan-out sources need artifact-to-entity expansion in the controller path",
        Some(workflow.workflow_id.clone()),
        fan_out.mode.as_ref().map(|mode| vec![format!("mode:{mode}")]),
    ))
}

pub(super) fn workflow_dispatch_request(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<Value> {
    let mut dispatch = serde_json::Map::new();
    if let Some(prompt) = workflow.prompt.as_ref().filter(|prompt| !prompt.is_empty()) {
        dispatch.insert("prompt".to_string(), Value::String(prompt.clone()));
    }
    if !workflow.tasks.is_empty() {
        dispatch.insert(
            "tasks".to_string(),
            Value::Array(workflow.tasks.iter().cloned().map(Value::String).collect()),
        );
    }
    apply_workflow_dispatch_defaults(spec, &mut dispatch);
    let context = workflow_client_context(spec, workflow)?;
    dispatch.insert(
        "client_context".to_string(),
        Value::String(context.to_string()),
    );
    let required_capabilities = workflow_required_capabilities(spec, workflow);
    if !required_capabilities.is_empty() {
        dispatch.insert(
            "required_capabilities".to_string(),
            Value::Array(
                required_capabilities
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    Ok(serde_json::json!({
        "mode": "dispatch",
        "dispatch": Value::Object(dispatch),
    }))
}

pub(super) fn apply_workflow_dispatch_defaults(
    spec: &AgentTaskRepoLoopSpec,
    dispatch: &mut serde_json::Map<String, Value>,
) {
    let Some(defaults) = spec
        .metadata
        .get("dispatch_defaults")
        .and_then(Value::as_object)
    else {
        return;
    };
    for key in [
        "cwd",
        "workspace",
        "repo",
        "backend",
        "selector",
        "model",
        "provider_config",
    ] {
        if dispatch.contains_key(key) {
            continue;
        }
        if let Some(value) = defaults
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            dispatch.insert(key.to_string(), Value::String(value.to_string()));
        }
    }
}

pub(super) fn workflow_client_context(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<Value> {
    Ok(serde_json::json!({
        "schema": "homeboy/repo-loop-workflow-context/v1",
        "loop_id": spec.loop_id,
        "workflow_id": workflow.workflow_id,
        "plan": workflow_homeboy_plan(spec, workflow)?,
        "agent": workflow.agent_id.as_ref().and_then(|agent_id| {
            spec.agents.iter().find(|agent| &agent.agent_id == agent_id)
        }),
        "tools": select_by_id(&spec.tools, &workflow.tools, |tool| &tool.tool_id),
        "abilities": select_by_id(&spec.abilities, &workflow.abilities, |ability| &ability.ability_id),
        "artifacts": select_by_id(&spec.artifacts, &workflow.artifacts, |artifact| &artifact.artifact_id),
        "artifact_dependencies": workflow_artifact_dependencies(spec, workflow),
        "artifact_graph_edges": workflow_artifact_graph_edges(spec, workflow),
        "dependencies": select_by_id(&spec.dependencies, &workflow.dependencies, |dependency| &dependency.dependency_id),
        "runtime_component_contracts": workflow_runtime_component_contracts(spec, workflow)?,
        "gates": select_by_id(&spec.gates, &workflow.gates, |gate| &gate.gate_id),
        "metrics": select_by_id(&spec.metrics, &workflow.metrics, |metric| &metric.metric_id),
        "inputs": workflow_context_inputs(spec, workflow),
    }))
}

fn workflow_context_inputs(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Value {
    let mut inputs = match workflow.inputs.clone() {
        Value::Object(map) => map,
        Value::Null => serde_json::Map::new(),
        other => {
            let mut map = serde_json::Map::new();
            map.insert("workflow_inputs".to_string(), other);
            map
        }
    };

    if let Some(runtime_task) =
        runtime_task_from_workflow_execution(spec, &workflow.runtime_execution)
    {
        inputs
            .entry("runtime_task".to_string())
            .or_insert(runtime_task);
    }

    if let Some(component_contracts) = inputs
        .get("runtime_config")
        .and_then(|runtime_config| runtime_config.get("component_contracts"))
        .cloned()
    {
        inputs
            .entry("runtime_component_contracts".to_string())
            .or_insert(component_contracts);
    }

    Value::Object(inputs)
}

fn runtime_task_from_workflow_execution(
    spec: &AgentTaskRepoLoopSpec,
    runtime_execution: &Value,
) -> Option<Value> {
    let execution = runtime_execution.as_object()?;
    let ability = execution.get("ability")?.as_str()?.trim();
    if ability.is_empty() {
        return None;
    }

    let mut runtime_task = serde_json::Map::new();
    runtime_task.insert("ability".to_string(), Value::String(ability.to_string()));
    let mut input = execution
        .get("input")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    apply_runtime_task_dispatch_defaults(spec, &mut input);
    runtime_task.insert("input".to_string(), input);
    if let Some(kind) = execution
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
    {
        runtime_task.insert("kind".to_string(), Value::String(kind.to_string()));
    }
    Some(Value::Object(runtime_task))
}

fn apply_runtime_task_dispatch_defaults(spec: &AgentTaskRepoLoopSpec, input: &mut Value) {
    let Some(input) = input.as_object_mut() else {
        return;
    };
    let Some(defaults) = spec
        .metadata
        .get("dispatch_defaults")
        .and_then(Value::as_object)
    else {
        return;
    };

    if let Some(model) = defaults
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
    {
        input
            .entry("model".to_string())
            .or_insert_with(|| Value::String(model.to_string()));
    }

    let Some(provider_config) = defaults
        .get("provider_config")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
    else {
        return;
    };
    let Some(provider_config) = provider_config.as_object() else {
        return;
    };
    for key in ["provider", "model", "options"] {
        if let Some(value) = provider_config.get(key).filter(|value| !value.is_null()) {
            input
                .entry(key.to_string())
                .or_insert_with(|| value.clone());
        }
    }
}

pub(super) fn workflow_homeboy_plan(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<HomeboyPlan> {
    let mut plan = HomeboyPlan::for_description(
        PlanKind::AgentTask,
        format!("repo loop workflow {}", workflow.workflow_id),
    );
    plan.id = format!("{}:{}", spec.loop_id, workflow.workflow_id);
    plan.inputs.insert(
        "schema".to_string(),
        Value::String("homeboy/repo-loop-workflow-plan/v1".to_string()),
    );
    plan.inputs
        .insert("loop_id".to_string(), Value::String(spec.loop_id.clone()));
    plan.inputs.insert(
        "workflow_id".to_string(),
        Value::String(workflow.workflow_id.clone()),
    );
    if let Some(agent_id) = &workflow.agent_id {
        plan.inputs
            .insert("agent_id".to_string(), Value::String(agent_id.clone()));
    }
    plan.inputs.insert(
        "declarations".to_string(),
        workflow_declaration_context(spec, workflow)?,
    );
    let required_capabilities = workflow_required_capabilities(spec, workflow);
    if !required_capabilities.is_empty() {
        plan.policy.insert(
            "required_capabilities".to_string(),
            Value::Array(
                required_capabilities
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    plan.steps.push(PlanStep {
        id: format!("dispatch:{}", workflow.workflow_id),
        kind: "agent_task_dispatch".to_string(),
        label: Some(workflow.workflow_id.clone()),
        blocking: true,
        scope: workflow_fan_out_entity_ids(workflow)?,
        needs: workflow.dependencies.clone(),
        needs_kind: PlanStepDependencyKind::Execution,
        status: PlanStepStatus::Ready,
        inputs: HashMap::from([(
            "workflow".to_string(),
            workflow_declaration_context(spec, workflow)?,
        )]),
        outputs: HashMap::new(),
        skip_reason: None,
        policy: HashMap::new(),
        missing: Vec::new(),
    });
    for artifact in select_by_id(&spec.artifacts, &workflow.artifacts, |artifact| {
        &artifact.artifact_id
    }) {
        let mut data = HashMap::new();
        data.insert("kind".to_string(), Value::String(artifact.kind.clone()));
        data.insert("required".to_string(), Value::Bool(artifact.required));
        if let Some(description) = &artifact.description {
            data.insert(
                "description".to_string(),
                Value::String(description.clone()),
            );
        }
        plan.artifacts.push(PlanArtifact {
            id: artifact.artifact_id.clone(),
            path: None,
            artifact_type: Some(artifact.kind.clone()),
            data,
        });
    }
    Ok(plan)
}

pub(super) fn workflow_declaration_context(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<Value> {
    Ok(serde_json::json!({
        "agent": workflow.agent_id.as_ref().and_then(|agent_id| {
            spec.agents.iter().find(|agent| &agent.agent_id == agent_id)
        }),
        "tools": select_by_id(&spec.tools, &workflow.tools, |tool| &tool.tool_id),
        "abilities": select_by_id(&spec.abilities, &workflow.abilities, |ability| &ability.ability_id),
        "artifacts": select_by_id(&spec.artifacts, &workflow.artifacts, |artifact| &artifact.artifact_id),
        "artifact_dependencies": workflow_artifact_dependencies(spec, workflow),
        "artifact_graph_edges": workflow_artifact_graph_edges(spec, workflow),
        "dependencies": select_by_id(&spec.dependencies, &workflow.dependencies, |dependency| &dependency.dependency_id),
        "runtime_component_contracts": workflow_runtime_component_contracts(spec, workflow)?,
        "gates": select_by_id(&spec.gates, &workflow.gates, |gate| &gate.gate_id),
        "metrics": select_by_id(&spec.metrics, &workflow.metrics, |metric| &metric.metric_id),
        "inputs": workflow.inputs,
    }))
}

fn workflow_runtime_component_contracts(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Result<Vec<Value>> {
    let mut contracts = Vec::new();
    for dependency in select_by_id(&spec.dependencies, &workflow.dependencies, |dependency| {
        &dependency.dependency_id
    }) {
        if !matches!(
            dependency.kind.as_str(),
            "component" | "runtime_component" | "component_contract"
        ) {
            continue;
        }
        let path = match dependency
            .value
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            Some(value) => value.to_string(),
            None => match component::resolve_target(TargetSpec {
                component_id: Some(&dependency.dependency_id),
                path_override: None,
                project: None,
                capability: None,
                allow_synthetic: false,
                accept_bare_directory: false,
                ..TargetSpec::default()
            }) {
                Ok(target) => target.source_path.display().to_string(),
                Err(error) if dependency.required => return Err(error),
                Err(_) => continue,
            },
        };
        contracts.push(serde_json::json!({
            "slug": dependency.dependency_id,
            "path": path,
            "required": dependency.required,
            "source": "repo_loop_spec_dependency",
            "dependency_kind": dependency.kind,
        }));
    }
    Ok(contracts)
}

pub(super) fn workflow_required_capabilities(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<String> {
    let _ = (spec, workflow);
    Vec::new()
}

pub(super) fn select_by_id<'a, T, F>(items: &'a [T], ids: &[String], id: F) -> Vec<&'a T>
where
    F: Fn(&T) -> &String,
{
    if ids.is_empty() {
        return Vec::new();
    }
    ids.iter()
        .filter_map(|requested| items.iter().find(|item| id(item) == requested))
        .collect()
}

pub(super) fn workflow_artifact_dependencies(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<Value> {
    let mut ids = workflow.consumes.clone();
    for dependency in &workflow.dependencies {
        if spec
            .artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == dependency.as_str())
            && !ids.contains(dependency)
        {
            ids.push(dependency.clone());
        }
    }
    for edge in spec
        .artifact_graph
        .iter()
        .filter(|edge| edge.to_workflow_id == workflow.workflow_id)
    {
        if !ids.contains(&edge.artifact_id) {
            ids.push(edge.artifact_id.clone());
        }
    }

    ids.iter()
        .filter_map(|id| {
            let artifact = spec
                .artifacts
                .iter()
                .find(|artifact| artifact.artifact_id == id.as_str())?;
            let mut value = serde_json::to_value(artifact).ok()?;
            if let Some(object) = value.as_object_mut() {
                let producer_workflow_ids: Vec<Value> = spec
                    .workflows
                    .iter()
                    .filter(|producer| producer.workflow_id != workflow.workflow_id)
                    .filter(|producer| {
                        producer.artifacts.contains(id)
                            || producer.emits.contains(id)
                            || spec.artifact_graph.iter().any(|edge| {
                                edge.artifact_id == *id
                                    && edge.from_workflow_id == producer.workflow_id
                                    && edge.to_workflow_id == workflow.workflow_id
                            })
                    })
                    .map(|producer| Value::String(producer.workflow_id.clone()))
                    .collect();
                if !producer_workflow_ids.is_empty() {
                    object.insert(
                        "producer_workflow_ids".to_string(),
                        Value::Array(producer_workflow_ids),
                    );
                }
            }
            Some(value)
        })
        .collect()
}

pub(super) fn workflow_artifact_graph_edges(
    spec: &AgentTaskRepoLoopSpec,
    workflow: &AgentTaskRepoLoopSpecWorkflow,
) -> Vec<Value> {
    spec.artifact_graph
        .iter()
        .filter(|edge| {
            edge.from_workflow_id == workflow.workflow_id
                || edge.to_workflow_id == workflow.workflow_id
        })
        .filter_map(|edge| serde_json::to_value(edge).ok())
        .collect()
}

pub(super) fn compile_loop_spec_policy(
    spec: &AgentTaskRepoLoopSpec,
) -> Option<AgentTaskLoopPolicy> {
    let mut transitions = Vec::new();
    if let Some(policy) = &spec.policy {
        transitions.extend(policy.transitions.clone());
    }
    transitions.extend(spec.phases.iter().enumerate().map(|(index, phase)| {
        AgentTaskLoopTransition {
            transition_id: phase
                .transition_id
                .clone()
                .unwrap_or_else(|| format!("{}-{}", phase.phase, index + 1)),
            from_phase: Some(phase.phase.clone()),
            on_event_type: phase.on_event_type.clone(),
            when_json_path: phase.when_json_path.clone(),
            actions: phase.actions.clone(),
        }
    }));
    if transitions.is_empty() {
        None
    } else {
        Some(AgentTaskLoopPolicy {
            policy_id: spec
                .schema
                .clone()
                .unwrap_or_else(|| "repo-loop-spec".to_string()),
            transitions,
        })
    }
}

pub(super) fn merge_policy_into_event_payload(
    payload: Value,
    policy: AgentTaskLoopPolicy,
) -> Value {
    let policy = serde_json::to_value(policy).unwrap_or(Value::Null);
    match payload {
        Value::Object(mut object) => {
            object.insert("policy".to_string(), policy);
            Value::Object(object)
        }
        Value::Null => serde_json::json!({ "policy": policy }),
        other => serde_json::json!({ "value": other, "policy": policy }),
    }
}
