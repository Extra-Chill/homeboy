//! Dispatch, concurrency, and dependency-resolution engine for the agent-task
//! scheduler.
//!
//! `AgentTaskScheduleSupport` houses the pure scheduling decisions (next
//! dispatchable task, per-executor/per-model concurrency limits, resource
//! budgeting, dependency binding, and totals aggregation) kept separate from
//! the executor-driving loop in the parent module so the scheduling seams stay
//! cohesive and independently testable. Helpers below are scheduling-private
//! (`pub(super)`) so the parent module and tests can reach them without
//! widening the crate-public surface.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

use super::outcome::{
    artifact_matches_required_artifact, event, evidence_matches_required_artifact,
    mark_generated_from_outputs, missing_required_typed_artifacts, missing_typed_artifacts_failure,
    nested_failed_executor_status, provider_run_result_is_empty_incomplete, render_template_string,
    render_template_value, runtime_result_is_materializable, typed_artifact_from_artifact,
    typed_artifact_from_evidence, typed_artifact_from_outcome,
};
use super::*;

pub(super) struct AgentTaskScheduleSupport;

impl AgentTaskScheduleSupport {
    pub(super) fn next_dispatchable_index(
        queued: &VecDeque<ScheduledTask>,
        running: &[RunningTask],
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
        per_executor_concurrency: &HashMap<String, usize>,
        per_model_concurrency: &HashMap<String, usize>,
        resource_budget: &AgentTaskResourceBudget,
    ) -> Option<usize> {
        queued.iter().position(|task| {
            if !Self::dependencies_satisfied(&task.request, completed_by_task, output_dependencies)
            {
                return false;
            }

            let executor_key = executor_key(&task.request);
            let limit = per_executor_concurrency
                .get(&executor_key)
                .copied()
                .unwrap_or(usize::MAX)
                .max(1);
            let running_for_executor = running
                .iter()
                .filter(|running| running.executor_key == executor_key)
                .count();

            if running_for_executor >= limit {
                return false;
            }

            if let Some(model_key) = model_key(&task.request) {
                let model_limit = per_model_concurrency
                    .get(&model_key)
                    .copied()
                    .unwrap_or(usize::MAX)
                    .max(1);
                let running_for_model = running
                    .iter()
                    .filter(|running| running.model_key.as_ref() == Some(&model_key))
                    .count();
                if running_for_model >= model_limit {
                    return false;
                }
            }

            resource_capacity_available(&task.request, running, resource_budget)
        })
    }

    pub(super) fn dependencies_satisfied(
        request: &AgentTaskRequest,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> bool {
        Self::dependency_task_ids(request, output_dependencies)
            .iter()
            .all(|task_id| completed_by_task.contains_key(task_id))
    }

    pub(super) fn waiting_for_dependencies(
        request: &AgentTaskRequest,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> Option<String> {
        let missing: Vec<String> = Self::dependency_task_ids(request, output_dependencies)
            .into_iter()
            .filter(|task_id| !completed_by_task.contains_key(task_id))
            .collect();

        (!missing.is_empty()).then(|| {
            format!(
                "task blocked waiting for output dependencies: {}",
                missing.join(", ")
            )
        })
    }

    pub(super) fn waiting_for_task_dependencies(
        task: &ScheduledTask,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> Option<String> {
        Self::waiting_for_dependencies(&task.request, completed_by_task, output_dependencies)
    }

    pub(super) fn block_scheduled_task(
        task: &ScheduledTask,
        kind: &str,
        message: String,
        backpressure: &mut Vec<AgentTaskBackpressureStatus>,
        events: &mut Vec<AgentTaskProgressEvent>,
    ) -> AgentTaskOutcome {
        backpressure.push(AgentTaskBackpressureStatus {
            kind: kind.to_string(),
            message: message.clone(),
            task_id: Some(task.request.task_id.clone()),
        });
        events.push(event(
            &task.request.task_id,
            AgentTaskState::Blocked,
            task.attempt,
            Some(message.clone()),
        ));
        Self::blocked_outcome(task.request.task_id.clone(), message)
    }

    /// Block a scheduled task, record its blocked outcome, and bump the blocked
    /// counter. Shared by the adaptive-concurrency and resource-budget dispatch
    /// paths so both emit identical bookkeeping (#5091).
    pub(super) fn block_and_record_scheduled_task(
        task: &ScheduledTask,
        kind: &str,
        message: String,
        backpressure: &mut Vec<AgentTaskBackpressureStatus>,
        events: &mut Vec<AgentTaskProgressEvent>,
        outcomes: &mut Vec<AgentTaskOutcome>,
        blocked_count: &mut usize,
    ) {
        outcomes.push(Self::block_scheduled_task(
            task,
            kind,
            message,
            backpressure,
            events,
        ));
        *blocked_count += 1;
    }

    pub(super) fn dependency_task_ids(
        request: &AgentTaskRequest,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> Vec<String> {
        let Some(dependencies) = output_dependencies.get(&request.task_id) else {
            return Vec::new();
        };
        let mut task_ids = dependencies.depends_on.clone();
        for binding in dependencies.bindings.values() {
            if !task_ids.contains(&binding.task_id) {
                task_ids.push(binding.task_id.clone());
            }
        }
        task_ids
    }

    pub(super) fn render_output_dependencies(
        request: &mut AgentTaskRequest,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
        output_dependencies: &HashMap<String, AgentTaskOutputDependencies>,
    ) -> Result<(), AgentTaskOutcome> {
        let Some(dependencies) = output_dependencies.get(&request.task_id) else {
            return Ok(());
        };
        let bindings = match Self::resolve_output_bindings(request, dependencies, completed_by_task)
        {
            Ok(bindings) => bindings,
            Err(message) => return Err(Self::skipped_output_dependency_outcome(request, message)),
        };

        request.instructions = render_template_string(&request.instructions, &bindings);
        render_value_templates(&mut request.inputs, &bindings);
        render_value_templates(&mut request.executor.config, &bindings);
        render_value_templates(&mut request.workspace.materialization, &bindings);
        render_value_templates(&mut request.metadata, &bindings);
        for artifact in &mut request.expected_artifacts {
            *artifact = render_template_string(artifact, &bindings);
        }
        mark_generated_from_outputs(request, dependencies, &bindings);
        Ok(())
    }

    pub(super) fn resolve_output_bindings(
        request: &AgentTaskRequest,
        dependencies: &AgentTaskOutputDependencies,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
    ) -> Result<HashMap<String, Value>, String> {
        let mut bindings = HashMap::new();
        for (name, binding) in &dependencies.bindings {
            let value = Self::select_bound_output(request, name, binding, completed_by_task)?;
            bindings.insert(name.clone(), value);
        }
        Ok(bindings)
    }

    pub(super) fn select_bound_output(
        request: &AgentTaskRequest,
        name: &str,
        binding: &AgentTaskOutputBinding,
        completed_by_task: &HashMap<String, AgentTaskOutcome>,
    ) -> Result<Value, String> {
        let Some(outcome) = completed_by_task.get(&binding.task_id) else {
            return Err(format!(
                "task '{}' skipped because output binding '{}' waited for missing task '{}'",
                request.task_id, name, binding.task_id
            ));
        };

        // Resolve the fallback for a missing binding value: default if set,
        // a required-error if the binding is required, else an empty string.
        let missing_binding_fallback = |required_error: String| -> Result<Value, String> {
            if !binding.default.is_null() {
                return Ok(binding.default.clone());
            }
            if binding.required {
                return Err(required_error);
            }
            Ok(Value::String(String::new()))
        };

        if let Some(artifact_binding) = &binding.artifact {
            let Some(artifact) = outcome.artifacts.iter().find(|artifact| {
                artifact.kind == artifact_binding.kind
                    && artifact_binding
                        .artifact_id
                        .as_ref()
                        .map(|artifact_id| artifact.id == *artifact_id)
                        .unwrap_or(true)
                    && artifact_binding
                        .schema
                        .as_ref()
                        .map(|schema| {
                            artifact
                                .metadata
                                .get("payload_schema")
                                .and_then(Value::as_str)
                                == Some(schema.as_str())
                        })
                        .unwrap_or(true)
            }) else {
                return missing_binding_fallback(format!(
                    "task '{}' skipped because required artifact binding '{}' with kind '{}' was missing from task '{}'",
                    request.task_id, name, artifact_binding.kind, binding.task_id
                ));
            };

            let artifact_value = serde_json::to_value(artifact).unwrap_or(Value::Null);
            if let Some(payload_path) = &artifact_binding.payload_path {
                if let Some(value) = artifact
                    .metadata
                    .get("payload")
                    .and_then(|payload| payload.pointer(payload_path))
                    .or_else(|| artifact_value.pointer(payload_path))
                {
                    return Ok(value.clone());
                }
                return missing_binding_fallback(format!(
                    "task '{}' skipped because required artifact binding '{}' payload was missing at '{}' from task '{}'",
                    request.task_id, name, payload_path, binding.task_id
                ));
            }

            return Ok(artifact_value);
        }

        let outcome_value = serde_json::to_value(outcome).unwrap_or(Value::Null);
        if let Some(value) = outcome_value.pointer(&binding.path) {
            return Ok(value.clone());
        }
        missing_binding_fallback(format!(
            "task '{}' skipped because required output binding '{}' was missing at '{}' from task '{}'",
            request.task_id, name, binding.path, binding.task_id
        ))
    }

    pub(super) fn skipped_output_dependency_outcome(
        request: &AgentTaskRequest,
        summary: String,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(summary.clone()),
            failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/output-dependency-skipped".to_string(),
                label: Some("scheduler output dependency skip".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "output_dependency_missing".to_string(),
                message: summary,
                data: Value::Null,
            }],
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: serde_json::json!({ "skipped": true, "skip_reason": "output_dependency_missing" }),
        }
    }

    pub(super) fn artifact_lineage(
        outcomes: &[AgentTaskOutcome],
        declarations_by_task: &HashMap<String, Vec<AgentTaskArtifactOutputDeclaration>>,
    ) -> Vec<AgentTaskArtifactLineage> {
        let mut lineage = Vec::new();
        for outcome in outcomes {
            let Some(declarations) = declarations_by_task.get(&outcome.task_id) else {
                continue;
            };
            for declaration in declarations {
                if let Some(artifact) = outcome.artifacts.iter().find(|artifact| {
                    artifact.kind == declaration.kind
                        && declaration
                            .artifact_id
                            .as_ref()
                            .map(|artifact_id| artifact.id == *artifact_id)
                            .unwrap_or(true)
                }) {
                    let payload = declaration
                        .payload_path
                        .as_ref()
                        .and_then(|payload_path| select_artifact_payload(artifact, payload_path))
                        .unwrap_or(Value::Null);

                    lineage.push(AgentTaskArtifactLineage {
                        task_id: outcome.task_id.clone(),
                        name: declaration.name.clone(),
                        kind: artifact.kind.clone(),
                        schema: declaration.schema.clone().or_else(|| {
                            artifact
                                .metadata
                                .get("payload_schema")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        }),
                        artifact_id: Some(artifact.id.clone()),
                        path: artifact.path.clone(),
                        url: artifact.url.clone(),
                        sha256: artifact.sha256.clone(),
                        payload,
                    });
                }
            }
        }
        lineage
    }

    pub(super) fn backpressure_kind(
        queued: &VecDeque<ScheduledTask>,
        running: &[RunningTask],
        per_executor_concurrency: &HashMap<String, usize>,
        per_model_concurrency: &HashMap<String, usize>,
        resource_budget: &AgentTaskResourceBudget,
    ) -> &'static str {
        let Some(task) = queued.front() else {
            return "scheduler_capacity";
        };
        let executor_key = executor_key(&task.request);
        let executor_limit = per_executor_concurrency
            .get(&executor_key)
            .copied()
            .unwrap_or(usize::MAX)
            .max(1);
        let running_for_executor = running
            .iter()
            .filter(|running| running.executor_key == executor_key)
            .count();
        if running_for_executor >= executor_limit {
            return "per_executor_concurrency";
        }

        if let Some(model_key) = model_key(&task.request) {
            let model_limit = per_model_concurrency
                .get(&model_key)
                .copied()
                .unwrap_or(usize::MAX)
                .max(1);
            let running_for_model = running
                .iter()
                .filter(|running| running.model_key.as_ref() == Some(&model_key))
                .count();
            if running_for_model >= model_limit {
                return "per_model_concurrency";
            }
        }

        if !resource_capacity_available(&task.request, running, resource_budget) {
            return "resource_budget";
        }

        "scheduler_capacity"
    }

    pub(super) fn cancel_queued(
        queued: &mut VecDeque<ScheduledTask>,
        outcomes: &mut Vec<AgentTaskOutcome>,
        events: &mut Vec<AgentTaskProgressEvent>,
    ) {
        while let Some(task) = queued.pop_front() {
            events.push(event(
                &task.request.task_id,
                AgentTaskState::Cancelled,
                task.attempt,
                Some("cancelled before execution".to_string()),
            ));
            outcomes.push(Self::cancelled_outcome(
                task.request.task_id,
                "cancelled before execution".to_string(),
            ));
        }
    }

    pub(super) fn expire_timed_out_tasks<E>(
        running: &mut Vec<RunningTask>,
        outcomes: &mut Vec<AgentTaskOutcome>,
        events: &mut Vec<AgentTaskProgressEvent>,
        executor: &E,
    ) where
        E: AgentTaskExecutorAdapter,
    {
        let mut index = 0;
        while index < running.len() {
            let timed_out = running[index]
                .timeout_ms
                .map(|timeout_ms| {
                    running[index].started_at.elapsed() > timeout_with_grace(timeout_ms)
                })
                .unwrap_or(false);

            if !timed_out {
                index += 1;
                continue;
            }

            let task = running.remove(index);
            executor.cancel(&task.task_id);
            let outcome = Self::timeout_outcome(
                task.task_id.clone(),
                task.timeout_ms.unwrap_or_default(),
                Some(&task.request),
                "scheduler_timeout",
            );
            events.push(event(
                &task.task_id,
                Self::state_for_outcome(&outcome),
                task.attempt,
                outcome.summary.clone(),
            ));
            outcomes.push(outcome);
        }
    }

    pub(super) fn normalize_outcome(
        mut outcome: AgentTaskOutcome,
        running: Option<&RunningTask>,
    ) -> AgentTaskOutcome {
        if let Some(running) = running {
            Self::normalize_required_typed_artifacts(&mut outcome, &running.request);
            Self::recover_missing_typed_artifacts_wrapper_failure(&mut outcome, &running.request);
            Self::classify_missing_required_typed_artifacts(&mut outcome, &running.request);
        }

        Self::classify_failed_nested_executor_status(&mut outcome);
        Self::classify_incomplete_executor_result(&mut outcome);

        if let Some(running) = running {
            if let Some(timeout_ms) = running.timeout_ms {
                if running.started_at.elapsed() > Duration::from_millis(timeout_ms) {
                    outcome.status = AgentTaskOutcomeStatus::Timeout;
                    outcome.failure_classification = Some(AgentTaskFailureClassification::Timeout);
                    outcome.diagnostics.push(AgentTaskDiagnostic {
                        class: "timeout".to_string(),
                        message: format!("task exceeded timeout_ms={timeout_ms}"),
                        data: Value::Null,
                    });
                }
            }

            if outcome.status == AgentTaskOutcomeStatus::Timeout {
                Self::reconcile_timeout_artifacts(
                    &mut outcome,
                    &running.request,
                    "provider_timeout",
                );
            }
        }
        outcome
    }

    pub(super) fn normalize_required_typed_artifacts(
        outcome: &mut AgentTaskOutcome,
        request: &AgentTaskRequest,
    ) {
        let required = request
            .canonical_artifact_declarations()
            .into_iter()
            .filter(|declaration| declaration.required)
            .map(|declaration| declaration.name)
            .collect::<Vec<_>>();

        for name in required {
            if outcome
                .typed_artifacts
                .iter()
                .any(|artifact| artifact.name == name)
            {
                continue;
            }

            if let Some(artifact) = outcome
                .artifacts
                .iter()
                .find(|artifact| artifact_matches_required_artifact(&name, artifact))
                .cloned()
            {
                outcome.typed_artifacts.push(typed_artifact_from_artifact(
                    &name,
                    artifact,
                    "runtime_artifact",
                ));
                continue;
            }

            if let Some(evidence) = outcome
                .evidence_refs
                .iter()
                .find(|evidence| evidence_matches_required_artifact(&name, evidence))
            {
                outcome.typed_artifacts.push(typed_artifact_from_evidence(
                    &name,
                    evidence,
                    "runtime_evidence",
                ));
                continue;
            }

            if name == "agent_result" && runtime_result_is_materializable(outcome) {
                let typed_artifact = typed_artifact_from_outcome(outcome);
                outcome.typed_artifacts.push(typed_artifact);
            }
        }
    }

    pub(super) fn recover_missing_typed_artifacts_wrapper_failure(
        outcome: &mut AgentTaskOutcome,
        request: &AgentTaskRequest,
    ) {
        if outcome.status == AgentTaskOutcomeStatus::Succeeded
            || !missing_typed_artifacts_failure(outcome)
        {
            return;
        }

        let missing = missing_required_typed_artifacts(outcome, request);
        if !missing.is_empty() {
            return;
        }

        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        outcome.failure_classification = None;
        outcome.summary = Some(
            outcome
                .summary
                .clone()
                .unwrap_or_else(|| "runtime artifacts normalized successfully".to_string()),
        );
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.required_typed_artifacts_normalized".to_string(),
            message: "required typed artifacts were materialized from runtime artifacts"
                .to_string(),
            data: serde_json::json!({
                "typed_artifacts": outcome
                    .typed_artifacts
                    .iter()
                    .map(|artifact| artifact.name.clone())
                    .collect::<Vec<_>>(),
            }),
        });
    }

    pub(super) fn classify_missing_required_typed_artifacts(
        outcome: &mut AgentTaskOutcome,
        request: &AgentTaskRequest,
    ) {
        if outcome.status != AgentTaskOutcomeStatus::Succeeded {
            return;
        }

        let missing = missing_required_typed_artifacts(outcome, request);
        if missing.is_empty() {
            return;
        }

        let message = format!(
            "agent task did not produce required typed artifacts: {}.",
            missing.join(", ")
        );
        outcome.status = AgentTaskOutcomeStatus::Failed;
        outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);
        outcome.summary = Some(message.clone());
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.required_typed_artifacts_missing".to_string(),
            message,
            data: serde_json::json!({ "missing": missing }),
        });
    }

    pub(super) fn classify_failed_nested_executor_status(outcome: &mut AgentTaskOutcome) {
        if outcome.status != AgentTaskOutcomeStatus::Succeeded {
            return;
        }
        let Some(failed_status) = nested_failed_executor_status(outcome) else {
            return;
        };

        let message = format!(
            "nested executor reported failed status: {}={}",
            failed_status.path, failed_status.value
        );
        outcome.status = AgentTaskOutcomeStatus::Failed;
        outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);
        outcome.summary = Some(message.clone());
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.nested_executor_failed_status".to_string(),
            message,
            data: serde_json::json!({
                "path": failed_status.path,
                "key": failed_status.key,
                "value": failed_status.value,
            }),
        });
    }

    pub(super) fn classify_incomplete_executor_result(outcome: &mut AgentTaskOutcome) {
        if outcome.status != AgentTaskOutcomeStatus::Succeeded {
            return;
        }
        let Some(result) = outcome.outputs.get("provider_run_result") else {
            return;
        };
        if !provider_run_result_is_empty_incomplete(result) {
            return;
        }
        let result = result.clone();

        let message = "executor completed without a usable agent result: completed=false, empty reply, no assistant message, and no tool calls"
            .to_string();
        outcome.status = AgentTaskOutcomeStatus::ProviderError;
        outcome.failure_classification = Some(AgentTaskFailureClassification::Provider);
        outcome.summary = Some(message.clone());
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.executor_incomplete_empty_result".to_string(),
            message,
            data: serde_json::json!({
                "provider_run_result": result,
            }),
        });
    }

    pub(super) fn cancelled_outcome(task_id: String, summary: String) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id,
            status: AgentTaskOutcomeStatus::Cancelled,
            summary: Some(summary),
            failure_classification: None,
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/cancelled".to_string(),
                label: Some("scheduler cancellation".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }

    pub(super) fn timeout_outcome(
        task_id: String,
        timeout_ms: u64,
        request: Option<&AgentTaskRequest>,
        timeout_kind: &str,
    ) -> AgentTaskOutcome {
        let mut outcome = AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id,
            status: AgentTaskOutcomeStatus::Timeout,
            summary: Some(format!("task exceeded timeout_ms={timeout_ms}")),
            failure_classification: Some(AgentTaskFailureClassification::Timeout),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/timeout".to_string(),
                label: Some("scheduler timeout".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "timeout".to_string(),
                message: format!("task exceeded timeout_ms={timeout_ms}"),
                data: Value::Null,
            }],
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        };

        if let Some(request) = request {
            Self::reconcile_timeout_artifacts(&mut outcome, request, timeout_kind);
        }

        outcome
    }

    pub(super) fn reconcile_timeout_artifacts(
        outcome: &mut AgentTaskOutcome,
        request: &AgentTaskRequest,
        timeout_kind: &str,
    ) {
        let discovery = TimeoutArtifactDiscovery::discover(request);
        let has_runtime_evidence = discovery.has_runtime_evidence();
        outcome.diagnostics.extend(discovery.diagnostics);
        if !has_runtime_evidence {
            append_unique_artifacts(&mut outcome.artifacts, discovery.artifacts);
            append_unique_evidence_refs(&mut outcome.evidence_refs, discovery.evidence_refs);
            outcome.diagnostics.push(AgentTaskDiagnostic {
                class: timeout_kind.to_string(),
                message:
                    "no completed runtime artifacts were discovered before timeout finalization"
                        .to_string(),
                data: Value::Null,
            });
            return;
        }

        if let Some(discovered) = discovery.outcome {
            merge_timeout_outcome(outcome, discovered);
        }

        append_unique_artifacts(&mut outcome.artifacts, discovery.artifacts);
        append_unique_evidence_refs(&mut outcome.evidence_refs, discovery.evidence_refs);

        let actionable_patch = outcome.metadata.get("actionable").and_then(Value::as_bool)
            != Some(false)
            && outcome.artifacts.iter().any(is_actionable_patch_artifact);
        if actionable_patch {
            outcome.status = AgentTaskOutcomeStatus::Succeeded;
            outcome.failure_classification = None;
            outcome.summary = Some(
                "runtime completed with an actionable artifact before timeout finalization"
                    .to_string(),
            );
        } else if outcome.status == AgentTaskOutcomeStatus::Succeeded
            && outcome.artifacts.iter().any(is_empty_patch_artifact)
        {
            outcome.status = AgentTaskOutcomeStatus::NoOp;
            outcome.failure_classification = None;
            outcome.summary = Some(
                "runtime completed with an empty patch artifact before timeout finalization"
                    .to_string(),
            );
        }

        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "completed_runtime_late_provider_race".to_string(),
            message: if actionable_patch {
                format!(
                    "{timeout_kind} observed after runtime artifacts were already available; preserving actionable artifacts"
                )
            } else {
                format!(
                    "{timeout_kind} observed after runtime artifacts were already available; preserving discovered artifacts"
                )
            },
            data: serde_json::json!({
                "timeout_kind": timeout_kind,
                "artifact_count": outcome.artifacts.len(),
                "evidence_ref_count": outcome.evidence_refs.len(),
                "actionable_patch": actionable_patch,
            }),
        });
    }

    pub(super) fn blocked_outcome(task_id: String, summary: String) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id,
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(summary.clone()),
            failure_classification: Some(AgentTaskFailureClassification::PolicyDenied),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/backpressure".to_string(),
                label: Some("scheduler backpressure".to_string()),
            }],
            diagnostics: vec![AgentTaskDiagnostic {
                class: "backpressure".to_string(),
                message: summary,
                data: Value::Null,
            }],
            outputs: Value::Null,
            follow_up: None,
            metadata: Value::Null,
            workflow: None,
        }
    }

    pub(super) fn should_retry(
        outcome: &AgentTaskOutcome,
        attempt: u32,
        max_attempts: u32,
        retry_budget_total: Option<u32>,
        retry_budget_used: u32,
        retryable_failure_classifications: &[AgentTaskFailureClassification],
    ) -> bool {
        attempt < max_attempts
            && retry_budget_total
                .map(|budget| retry_budget_used < budget)
                .unwrap_or(true)
            && (retryable_failure_classifications.is_empty()
                || outcome
                    .failure_classification
                    .map(|classification| {
                        retryable_failure_classifications.contains(&classification)
                    })
                    .unwrap_or(false))
            && !matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded
                    | AgentTaskOutcomeStatus::NoOp
                    | AgentTaskOutcomeStatus::Cancelled
                    | AgentTaskOutcomeStatus::Timeout
            )
    }

    pub(super) fn remove_running(
        running: &mut Vec<RunningTask>,
        task_id: &str,
    ) -> Option<RunningTask> {
        let index = running.iter().position(|task| task.task_id == task_id)?;
        Some(running.remove(index))
    }

    pub(super) fn state_for_outcome(outcome: &AgentTaskOutcome) -> AgentTaskState {
        match outcome.status {
            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
                AgentTaskState::Succeeded
            }
            AgentTaskOutcomeStatus::Timeout => AgentTaskState::TimedOut,
            AgentTaskOutcomeStatus::Cancelled => AgentTaskState::Cancelled,
            _ => AgentTaskState::Failed,
        }
    }

    pub(super) fn queue_status(
        max_concurrency: usize,
        max_tasks: Option<usize>,
        max_queue_depth: Option<usize>,
        blocked_count: usize,
        outcomes: &[AgentTaskOutcome],
        per_executor_concurrency: &HashMap<String, usize>,
        per_model_concurrency: &HashMap<String, usize>,
        resource_budget: &AgentTaskResourceBudget,
        adaptive_policy: Option<&AgentTaskAdaptiveConcurrencyPolicy>,
        adaptive_decisions: &[AgentTaskAdaptiveConcurrencyDecision],
        backpressure: &[AgentTaskBackpressureStatus],
        retry_budget_remaining: Option<u32>,
    ) -> AgentTaskQueueStatus {
        let per_executor_concurrency = per_executor_concurrency
            .iter()
            .map(|(executor, max_concurrency)| (executor.clone(), (*max_concurrency).max(1)))
            .collect();
        let per_model_concurrency = per_model_concurrency
            .iter()
            .map(|(model, max_concurrency)| (model.clone(), (*max_concurrency).max(1)))
            .collect();

        AgentTaskQueueStatus {
            max_concurrency,
            adaptive_concurrency: adaptive_policy.map(|policy| {
                let max_adaptive_concurrency = policy
                    .max_concurrency
                    .unwrap_or(max_concurrency)
                    .max(policy.min_concurrency.max(1));
                AgentTaskAdaptiveConcurrencyStatus {
                    configured_max_concurrency: max_concurrency,
                    effective_concurrency: adaptive_decisions
                        .last()
                        .map(|decision| decision.effective_concurrency)
                        .unwrap_or(max_concurrency.min(max_adaptive_concurrency)),
                    min_concurrency: policy.min_concurrency.max(1),
                    max_concurrency: max_adaptive_concurrency,
                    decisions: adaptive_decisions.to_vec(),
                }
            }),
            max_tasks,
            max_queue_depth,
            queued: 0,
            running: 0,
            blocked: blocked_count,
            completed: outcomes.len(),
            per_executor_concurrency,
            per_model_concurrency,
            resource_budget: AgentTaskResourceBudgetStatus {
                max_active_units: resource_budget.max_active_units,
                default_task_units: resource_budget.default_task_units.max(1),
                active_units: 0,
                per_executor_task_units: resource_budget.per_executor_task_units.clone(),
                per_model_task_units: resource_budget.per_model_task_units.clone(),
            },
            backpressure: backpressure.to_vec(),
            retry_budget_remaining,
        }
    }

    pub(super) fn aggregate_status(outcomes: &[AgentTaskOutcome]) -> AgentTaskAggregateStatus {
        if outcomes
            .iter()
            .any(|outcome| outcome.status == AgentTaskOutcomeStatus::Cancelled)
        {
            return AgentTaskAggregateStatus::Cancelled;
        }

        let failed = outcomes.iter().any(|outcome| {
            !matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
            )
        });
        let succeeded = outcomes.iter().any(|outcome| {
            matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
            )
        });

        match (succeeded, failed) {
            (true, false) => AgentTaskAggregateStatus::Succeeded,
            (true, true) => AgentTaskAggregateStatus::PartialFailure,
            _ => AgentTaskAggregateStatus::Failed,
        }
    }

    pub(super) fn totals(
        total_tasks: usize,
        outcomes: &[AgentTaskOutcome],
    ) -> AgentTaskAggregateTotals {
        let mut totals = AgentTaskAggregateTotals {
            queued: total_tasks.saturating_sub(outcomes.len()),
            ..AgentTaskAggregateTotals::default()
        };

        for outcome in outcomes {
            if outcome
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.class == "output_dependency_missing")
            {
                totals.skipped += 1;
                continue;
            }

            match outcome.status {
                AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
                    totals.succeeded += 1
                }
                AgentTaskOutcomeStatus::Timeout => totals.timed_out += 1,
                AgentTaskOutcomeStatus::Cancelled => totals.cancelled += 1,
                AgentTaskOutcomeStatus::Failed
                    if outcome.failure_classification
                        == Some(AgentTaskFailureClassification::PolicyDenied)
                        && outcome
                            .diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic.class == "backpressure") =>
                {
                    totals.blocked += 1
                }
                _ => totals.failed += 1,
            }
        }

        totals
    }
}

pub(super) fn select_artifact_payload(
    artifact: &AgentTaskArtifact,
    payload_path: &str,
) -> Option<Value> {
    artifact
        .metadata
        .get("payload")
        .and_then(|payload| payload.pointer(payload_path))
        .cloned()
        .or_else(|| {
            serde_json::to_value(artifact)
                .ok()
                .and_then(|artifact_value| artifact_value.pointer(payload_path).cloned())
        })
}

pub(super) fn executor_key(request: &AgentTaskRequest) -> String {
    match &request.executor.selector {
        Some(selector) => format!("{}:{selector}", request.executor.backend),
        None => request.executor.backend.clone(),
    }
}

pub(super) fn model_key(request: &AgentTaskRequest) -> Option<String> {
    request
        .executor
        .model
        .as_ref()
        .map(|model| match &request.executor.selector {
            Some(selector) => format!("{}:{selector}:{model}", request.executor.backend),
            None => format!("{}:{model}", request.executor.backend),
        })
}

pub(super) fn task_resource_units(
    request: &AgentTaskRequest,
    budget: &AgentTaskResourceBudget,
) -> u32 {
    model_key(request)
        .and_then(|key| budget.per_model_task_units.get(&key).copied())
        .or_else(|| {
            budget
                .per_executor_task_units
                .get(&executor_key(request))
                .copied()
        })
        .unwrap_or_else(|| budget.default_task_units.max(1))
        .max(1)
}

pub(super) fn active_resource_units(running: &[RunningTask]) -> u32 {
    running
        .iter()
        .map(|task| task.resource_units)
        .fold(0, u32::saturating_add)
}

pub(super) fn resource_capacity_available(
    request: &AgentTaskRequest,
    running: &[RunningTask],
    budget: &AgentTaskResourceBudget,
) -> bool {
    let Some(max_active_units) = budget.max_active_units else {
        return true;
    };
    active_resource_units(running).saturating_add(task_resource_units(request, budget))
        <= max_active_units
}

pub(super) fn adaptive_concurrency_decision(
    policy: Option<&AgentTaskAdaptiveConcurrencyPolicy>,
    configured_max_concurrency: usize,
    queued: usize,
    running: usize,
    resource_budget: &AgentTaskResourceBudget,
    active_units: u32,
    previous_effective_concurrency: Option<usize>,
) -> Option<AgentTaskAdaptiveConcurrencyDecision> {
    let policy = policy?;
    let configured_max_concurrency = configured_max_concurrency.max(1);
    let min_concurrency = policy.min_concurrency.max(1);
    let policy_max_concurrency = policy
        .max_concurrency
        .unwrap_or(configured_max_concurrency)
        .max(min_concurrency);
    let mut effective_concurrency = policy_max_concurrency;
    let mut reason =
        format!("adaptive concurrency held at configured ceiling {policy_max_concurrency}");

    if let Some(runner_capacity) = policy.runner_capacity {
        let available_runner_slots = runner_capacity.saturating_sub(policy.active_leases);
        if available_runner_slots == 0 {
            effective_concurrency = 0;
            reason = format!(
                "paused because active_leases={} consume runner_capacity={runner_capacity}",
                policy.active_leases
            );
        } else if available_runner_slots < effective_concurrency {
            effective_concurrency = available_runner_slots;
            reason = format!(
                "scaled down to available runner slots {available_runner_slots} from runner_capacity={runner_capacity} active_leases={}",
                policy.active_leases
            );
        } else if available_runner_slots > configured_max_concurrency
            && policy_max_concurrency > configured_max_concurrency
        {
            reason = format!(
                "scaled up because runner slots are available: runner_capacity={runner_capacity} active_leases={}",
                policy.active_leases
            );
        }
    }

    if let Some(max_active_units) = resource_budget.max_active_units {
        let default_task_units = resource_budget.default_task_units.max(1);
        let available_units = max_active_units.saturating_sub(active_units);
        let resource_slots = (available_units / default_task_units) as usize;
        if resource_slots == 0 {
            effective_concurrency = 0;
            reason = format!(
                "paused because active_units={active_units} consume max_active_units={max_active_units}"
            );
        } else if resource_slots < effective_concurrency {
            effective_concurrency = resource_slots;
            reason = format!(
                "scaled down to resource slots {resource_slots} from max_active_units={max_active_units} active_units={active_units} default_task_units={default_task_units}"
            );
        }
    }

    if policy
        .pause_on_pressure
        .zip(policy.resource_pressure)
        .map(|(pause_on, pressure)| pressure >= pause_on)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because resource_pressure={:?} reached pause_on_pressure={:?}",
            policy.resource_pressure.expect("pressure checked"),
            policy.pause_on_pressure.expect("pause threshold checked")
        );
    }

    if policy
        .pause_after_recent_failures
        .map(|threshold| threshold > 0 && policy.recent_failures >= threshold)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because recent_failures={} reached pause_after_recent_failures={}",
            policy.recent_failures,
            policy.pause_after_recent_failures.unwrap_or_default()
        );
    }

    if policy
        .pause_after_recent_timeouts
        .map(|threshold| threshold > 0 && policy.recent_timeouts >= threshold)
        .unwrap_or(false)
    {
        effective_concurrency = 0;
        reason = format!(
            "paused because recent_timeouts={} reached pause_after_recent_timeouts={}",
            policy.recent_timeouts,
            policy.pause_after_recent_timeouts.unwrap_or_default()
        );
    }

    if effective_concurrency > 0 {
        effective_concurrency = effective_concurrency
            .max(min_concurrency)
            .min(policy_max_concurrency);
    }
    if queued == 0 && running == 0 && effective_concurrency > configured_max_concurrency {
        reason = format!(
            "held because no queued or running tasks need fan-out above configured max {configured_max_concurrency}"
        );
        effective_concurrency = configured_max_concurrency;
    }

    let action = match (previous_effective_concurrency, effective_concurrency) {
        (_, 0) => AgentTaskAdaptiveConcurrencyAction::Paused,
        (Some(previous), current) if current > previous => {
            AgentTaskAdaptiveConcurrencyAction::Increased
        }
        (Some(previous), current) if current < previous => {
            AgentTaskAdaptiveConcurrencyAction::Decreased
        }
        (None, current) if current > configured_max_concurrency => {
            AgentTaskAdaptiveConcurrencyAction::Increased
        }
        (None, current) if current < configured_max_concurrency => {
            AgentTaskAdaptiveConcurrencyAction::Decreased
        }
        _ => AgentTaskAdaptiveConcurrencyAction::Held,
    };

    Some(AgentTaskAdaptiveConcurrencyDecision {
        action,
        effective_concurrency,
        previous_effective_concurrency,
        reason,
        inputs: AgentTaskAdaptiveConcurrencyInputs {
            queued,
            running,
            configured_max_concurrency,
            runner_capacity: policy.runner_capacity,
            active_leases: policy.active_leases,
            queue_depth: policy.queue_depth,
            resource_pressure: policy.resource_pressure,
            max_active_units: resource_budget.max_active_units,
            active_units,
            default_task_units: resource_budget.default_task_units.max(1),
            recent_failures: policy.recent_failures,
            recent_timeouts: policy.recent_timeouts,
        },
    })
}

pub(super) fn render_value_templates(value: &mut Value, bindings: &HashMap<String, Value>) {
    match value {
        Value::String(raw) => {
            if let Some(rendered) = render_template_value(raw, bindings) {
                *value = rendered;
            } else {
                *raw = render_template_string(raw, bindings);
            }
        }
        Value::Array(items) => {
            for item in items {
                render_value_templates(item, bindings);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                render_value_templates(value, bindings);
            }
        }
        _ => {}
    }
}
