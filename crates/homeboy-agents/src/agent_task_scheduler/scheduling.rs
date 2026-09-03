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
use std::time::{Duration, Instant};

use serde_json::Value;

use super::outcome::{
    artifact_matches_required_artifact, event, evidence_matches_required_artifact,
    invalid_required_typed_artifacts, mark_generated_from_outputs,
    missing_required_typed_artifacts, missing_typed_artifacts_failure,
    nested_failed_executor_status, provider_run_result_is_empty_incomplete, render_template_string,
    runtime_result_is_materializable, typed_artifact_from_artifact, typed_artifact_from_evidence,
    typed_artifact_from_outcome,
};
use super::resources::{
    render_value_templates, resource_capacity_available, resource_is_busy, select_artifact_payload,
    workspace_is_busy,
};
use super::*;

pub(crate) struct AgentTaskScheduleSupport;

/// One rotation entry `skip_capped_rotation_entries` bypassed because Homeboy
/// already knew its provider was over its usage cap. `exhausted` marks the
/// final skip when no further rotation entry remains to try.
#[derive(Debug, Clone)]
pub(crate) struct UsageCapSkip {
    pub(crate) backend: String,
    pub(crate) selector: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) reset_at: chrono::DateTime<chrono::Utc>,
    pub(crate) exhausted: bool,
}

fn deferred_timeout_outcome(task_id: &str, timeout_ms: u64, source: &str) -> AgentTaskOutcome {
    AgentTaskOutcome {
        task_id: task_id.to_string(),
        status: AgentTaskOutcomeStatus::Timeout,
        summary: Some(format!("provider exceeded timeout_ms={timeout_ms}")),
        failure_classification: Some(AgentTaskFailureClassification::Timeout),
        diagnostics: vec![AgentTaskDiagnostic {
            class: source.to_string(),
            message: format!("provider exceeded timeout_ms={timeout_ms}"),
            data: serde_json::json!({ "timeout_ms": timeout_ms }),
        }],
        ..Default::default()
    }
}

impl AgentTaskScheduleSupport {
    #[expect(
        clippy::too_many_arguments,
        reason = "scheduler inputs are independently bounded resource and lifecycle state"
    )]
    pub(super) fn next_dispatchable_index(
        queued: &VecDeque<ScheduledTask>,
        running: &[RunningTask],
        quarantined: &[QuarantinedTask],
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

            // An existing workspace is mutable executor state. Keep one task at
            // a time in that directory so a commit range belongs to one task.
            if workspace_is_busy(task, running, quarantined) {
                return false;
            }

            if resource_is_busy(task, running).is_some() {
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

    #[expect(
        clippy::result_large_err,
        reason = "scheduler returns the complete outcome for durable lifecycle recording"
    )]
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
            if let Some(typed_artifact) = outcome.typed_artifacts.iter().find(|artifact| {
                Self::typed_artifact_matches_artifact_binding(artifact, artifact_binding)
            }) {
                let artifact_value = serde_json::to_value(typed_artifact).unwrap_or(Value::Null);
                if let Some(payload_path) = &artifact_binding.payload_path {
                    if let Some(value) = typed_artifact
                        .payload
                        .pointer(payload_path)
                        .or_else(|| artifact_value.pointer(payload_path))
                    {
                        return Ok(value.clone());
                    }
                    return missing_binding_fallback(format!(
                        "task '{}' skipped because required typed artifact binding '{}' payload was missing at '{}' from task '{}'",
                        request.task_id, name, payload_path, binding.task_id
                    ));
                }

                return Ok(typed_artifact.payload.clone());
            }

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
            task_id: request.task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(summary.clone()),
            failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
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
            metadata: serde_json::json!({ "skipped": true, "skip_reason": "output_dependency_missing" }),
            ..Default::default()
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
                    continue;
                }

                if let Some(typed_artifact) = outcome.typed_artifacts.iter().find(|artifact| {
                    Self::typed_artifact_matches_output_declaration(artifact, declaration)
                }) {
                    let payload = declaration
                        .payload_path
                        .as_ref()
                        .and_then(|payload_path| typed_artifact.payload.pointer(payload_path))
                        .cloned()
                        .unwrap_or_else(|| typed_artifact.payload.clone());

                    lineage.push(AgentTaskArtifactLineage {
                        task_id: outcome.task_id.clone(),
                        name: declaration.name.clone(),
                        kind: typed_artifact
                            .artifact_type
                            .clone()
                            .unwrap_or_else(|| declaration.kind.clone()),
                        schema: declaration
                            .schema
                            .clone()
                            .or_else(|| typed_artifact.artifact_schema.clone()),
                        artifact_id: typed_artifact
                            .artifact
                            .as_ref()
                            .map(|artifact| artifact.id.clone()),
                        path: typed_artifact
                            .artifact
                            .as_ref()
                            .and_then(|artifact| artifact.path.clone()),
                        url: typed_artifact
                            .artifact
                            .as_ref()
                            .and_then(|artifact| artifact.url.clone()),
                        sha256: typed_artifact
                            .artifact
                            .as_ref()
                            .and_then(|artifact| artifact.sha256.clone()),
                        payload,
                    });
                }
            }
        }
        lineage
    }

    fn typed_artifact_matches_artifact_binding(
        artifact: &AgentTaskTypedArtifact,
        binding: &AgentTaskArtifactBinding,
    ) -> bool {
        let kind_matches = artifact.name == binding.kind
            || artifact.artifact_type.as_deref() == Some(binding.kind.as_str())
            || artifact.artifact_schema.as_deref() == Some(binding.kind.as_str());
        if !kind_matches {
            return false;
        }

        if binding.artifact_id.as_ref().map(|artifact_id| {
            artifact
                .artifact
                .as_ref()
                .map(|artifact| artifact.id.as_str())
                == Some(artifact_id.as_str())
                || artifact.name == *artifact_id
        }) == Some(false)
        {
            return false;
        }

        binding
            .schema
            .as_ref()
            .map(|schema| artifact.artifact_schema.as_deref() == Some(schema.as_str()))
            .unwrap_or(true)
    }

    fn typed_artifact_matches_output_declaration(
        artifact: &AgentTaskTypedArtifact,
        declaration: &AgentTaskArtifactOutputDeclaration,
    ) -> bool {
        let name_matches = artifact.name == declaration.name || artifact.name == declaration.kind;
        let kind_matches = artifact.artifact_type.as_deref() == Some(declaration.kind.as_str())
            || artifact.artifact_schema.as_deref() == Some(declaration.kind.as_str());
        let artifact_id_matches = declaration
            .artifact_id
            .as_ref()
            .map(|artifact_id| {
                artifact
                    .artifact
                    .as_ref()
                    .map(|artifact| artifact.id.as_str())
                    == Some(artifact_id.as_str())
                    || artifact.name == *artifact_id
            })
            .unwrap_or(true);
        let schema_matches = declaration
            .schema
            .as_ref()
            .map(|schema| artifact.artifact_schema.as_deref() == Some(schema.as_str()))
            .unwrap_or(true);

        (name_matches || kind_matches) && artifact_id_matches && schema_matches
    }

    pub(super) fn backpressure_kind(
        queued: &VecDeque<ScheduledTask>,
        running: &[RunningTask],
        quarantined: &[QuarantinedTask],
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

        if workspace_is_busy(task, running, quarantined) {
            return "workspace_quarantined";
        }

        if resource_is_busy(task, running).is_some() {
            return "exclusive_resource";
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

    pub(super) fn exclusive_resource_keys(request: &AgentTaskRequest) -> Vec<String> {
        let mut keys = request
            .limits
            .exclusive_resource_keys
            .iter()
            .map(|key| key.trim())
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        keys.sort();
        keys.dedup();
        keys
    }

    pub(super) fn record_resource_wait(
        task: &mut ScheduledTask,
        running: &[RunningTask],
        events: &mut Vec<AgentTaskProgressEvent>,
    ) {
        let Some((key, blocker_task_id)) = resource_is_busy(task, running) else {
            return;
        };
        let should_record = task
            .resource_wait
            .as_ref()
            .is_none_or(|wait| wait.key != key || wait.blocker_task_id != blocker_task_id);
        if should_record {
            task.resource_wait = Some(ResourceWait {
                key: key.clone(),
                blocker_task_id: blocker_task_id.clone(),
                started_at: Instant::now(),
            });
        }
        let wait = task
            .resource_wait
            .as_ref()
            .expect("resource wait is recorded");
        if !should_record {
            return;
        }
        events.push(event(
            &task.request.task_id,
            AgentTaskState::Blocked,
            task.attempt,
            Some(format!(
                "waiting for exclusive resource '{}' held by '{}' ({} ms elapsed)",
                key,
                blocker_task_id,
                wait.started_at.elapsed().as_millis()
            )),
        ));
    }

    pub(super) fn expire_timed_out_tasks(
        running: &mut Vec<RunningTask>,
        quarantined: &mut Vec<QuarantinedTask>,
        outcomes: &mut Vec<AgentTaskOutcome>,
        events: &mut Vec<AgentTaskProgressEvent>,
        executor: &dyn AgentTaskExecutorAdapter,
        lifecycle_store: Option<&crate::agent_task_lifecycle::AgentTaskLifecycleStore>,
    ) {
        let mut index = 0;
        while index < running.len() {
            let Some(timeout_ms) = running[index].timeout_ms else {
                index += 1;
                continue;
            };
            let elapsed = running[index].started_at.elapsed();
            if elapsed <= Duration::from_millis(timeout_ms) {
                index += 1;
                continue;
            }
            if !running[index].timeout_cancel_requested {
                executor.cancel(&running[index].task_id);
                running[index].timeout_cancel_requested = true;
                events.push(event(
                    &running[index].task_id,
                    AgentTaskState::Running,
                    running[index].attempt,
                    Some(
                        "deadline expired; cancellation requested and bounded grace started"
                            .to_string(),
                    ),
                ));
                index += 1;
                continue;
            }
            if elapsed <= timeout_with_grace(timeout_ms) {
                index += 1;
                continue;
            }

            let mut task = running.remove(index);
            let mut outcome =
                deferred_timeout_outcome(&task.task_id, timeout_ms, "scheduler_timeout");
            outcome.diagnostics.push(AgentTaskDiagnostic {
                class: "agent_task.deferred_cleanup".to_string(),
                message: "grace expired; deferred cleanup owns the still-running attempt workspace".to_string(),
                data: serde_json::json!({ "cleanup": "deferred_cleanup_pending", "grace_ms": timeout_with_grace(timeout_ms).as_millis() }),
            });
            let action_path = match super::deferred_cleanup_action_artifact(&task) {
                Ok(action) => {
                    let path = action.path.clone().map(std::path::PathBuf::from);
                    outcome.artifacts.push(action);
                    path
                }
                Err(error) => {
                    outcome.diagnostics.push(AgentTaskDiagnostic {
                        class: "agent_task.deferred_cleanup_action_failed".to_string(),
                        message: "could not persist deferred cleanup action".to_string(),
                        data: serde_json::json!({ "error": format!("{error:?}") }),
                    });
                    None
                }
            };
            outcome.metadata = serde_json::json!({ "deferred_cleanup_pending": true });
            events.push(event(
                &task.task_id,
                Self::state_for_outcome(&outcome),
                task.attempt,
                outcome.summary.clone(),
            ));
            outcomes.push(outcome);
            quarantined.push(QuarantinedTask {
                workspace_key: task.workspace_key.clone(),
            });
            let lifecycle_store = lifecycle_store.cloned();
            if let Some(join_handle) = task.join_handle.take() {
                // Deferred cleanup outlives the scheduler thread, so it must
                // carry the caller's route to keep recovery attributable.
                let notification_route = homeboy_core::notification_route::capture();
                std::thread::spawn(move || {
                    notification_route.bind(|| {
                        let joined = join_handle
                            .join()
                            .map_err(|_| "provider worker panicked".to_string());
                        let mut recovered = deferred_timeout_outcome(
                            &task.task_id,
                            task.timeout_ms.unwrap_or_default(),
                            "deferred_cleanup",
                        );
                        // A completed join proves the provider no longer owns the
                        // checkout, so its committed candidate is safe to harvest.
                        recovered.status = AgentTaskOutcomeStatus::Succeeded;
                        recovered.failure_classification = None;
                        let harvest = joined.and_then(|_| {
                            super::harvest_uncommitted_patch(&mut recovered, &task)
                                .and_then(|_| super::harvest_committed_patch(&mut recovered, &task))
                                .map_err(|error| format!("{error:?}"))
                        });
                        if harvest.is_ok() {
                            super::mark_timeout_workspace_candidates_incomplete(&mut recovered);
                            // The provider has exited, so runtime artifact discovery can no
                            // longer race its writes to the isolated attempt workspace.
                            Self::reconcile_timeout_artifacts(
                                &mut recovered,
                                &task.request,
                                "deferred_cleanup",
                            );
                            super::finalize_candidate_artifacts(&mut recovered, &task);
                        }
                        let cleanup = harvest.and_then(|_| {
                            task._attempt_workspace
                                .as_ref()
                                .map(|workspace| workspace.cleanup())
                                .unwrap_or(Ok(()))
                        });
                        // Publish terminal execution ownership before making the
                        // cleanup receipt observable. A terminal receipt must
                        // never authorize a retry while the durable provider
                        // ledger still says this owner is running.
                        if let (Some(store), Some(run_id)) =
                            (lifecycle_store.as_ref(), task.run_id.as_deref())
                        {
                            let _ = store.record_provider_execution_terminal(
                                run_id,
                                &task.task_id,
                                task.attempt,
                                "timed_out",
                            );
                        }
                        let receipt = action_path.as_deref().map_or(
                            Err("deferred cleanup descriptor was not persisted".to_string()),
                            |action_path| {
                                super::complete_deferred_cleanup_recovery(
                                    action_path,
                                    &recovered,
                                    cleanup,
                                )
                            },
                        );
                        // Keep the scratch lease active until both checkout
                        // cleanup and its durable receipt have reached a
                        // terminal state. A released lease is reclaimable.
                        if receipt.is_ok() {
                            let _ = super::engine::release_scratch(
                                &task.scratch,
                                "scheduler_timeout_completion",
                                &recovered,
                            );
                        }
                    })
                });
            }
        }
    }

    pub(super) fn normalize_outcome(
        mut outcome: AgentTaskOutcome,
        running: Option<&RunningTask>,
    ) -> AgentTaskOutcome {
        if let Some(running) = running {
            Self::normalize_required_typed_artifacts(&mut outcome, &running.request);
            Self::recover_missing_typed_artifacts_wrapper_failure(&mut outcome, &running.request);
            Self::classify_failed_nested_executor_status(&mut outcome);
            Self::classify_incomplete_executor_result(&mut outcome);
            Self::classify_missing_required_typed_artifacts(&mut outcome, &running.request);
            Self::classify_invalid_required_typed_artifacts(&mut outcome, &running.request);
        } else {
            Self::classify_failed_nested_executor_status(&mut outcome);
            Self::classify_incomplete_executor_result(&mut outcome);
        }

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

    pub(super) fn classify_invalid_required_typed_artifacts(
        outcome: &mut AgentTaskOutcome,
        request: &AgentTaskRequest,
    ) {
        if outcome.status != AgentTaskOutcomeStatus::Succeeded {
            return;
        }

        let invalid = invalid_required_typed_artifacts(outcome, request);
        if invalid.is_empty() {
            return;
        }

        let labels = invalid
            .iter()
            .map(|artifact| {
                let location = artifact
                    .path
                    .as_deref()
                    .or(artifact.url.as_deref())
                    .or(artifact.artifact_id.as_deref())
                    .unwrap_or("unknown location");
                format!("{} ({location}: {})", artifact.name, artifact.reason)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!("agent task produced invalid required typed artifacts: {labels}.");
        outcome.status = AgentTaskOutcomeStatus::Failed;
        outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);
        outcome.summary = Some(message.clone());
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.required_typed_artifacts_invalid".to_string(),
            message,
            data: serde_json::json!({
                "invalid": invalid.iter().map(|artifact| serde_json::json!({
                    "task_id": artifact.task_id,
                    "name": artifact.name,
                    "type": artifact.artifact_type,
                    "artifact_id": artifact.artifact_id,
                    "path": artifact.path,
                    "url": artifact.url,
                    "size_bytes": artifact.size_bytes,
                    "reason": artifact.reason,
                })).collect::<Vec<_>>()
            }),
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
                "provider_run_result": outcome.outputs.get("provider_run_result").cloned(),
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

    /// A provider can create a durable patch before failing its completion
    /// contract. Preserve that candidate for a fresh review and gate pass, but
    /// retain the provider failure classification as the terminal diagnosis.
    pub(super) fn preserve_base_bound_patch_after_provider_failure(outcome: &mut AgentTaskOutcome) {
        if matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded
                | AgentTaskOutcomeStatus::NoOp
                | AgentTaskOutcomeStatus::Cancelled
                | AgentTaskOutcomeStatus::CandidateRecoverable
        ) || !outcome
            .artifacts
            .iter()
            .any(|artifact| is_base_bound_patch_candidate(outcome, artifact))
        {
            return;
        }

        let provider_failure = outcome.failure_classification;
        outcome.status = AgentTaskOutcomeStatus::CandidateRecoverable;
        outcome.summary = Some(
            "provider completion failed after producing a base-bound patch candidate; fresh review and deterministic gates are required before promotion"
                .to_string(),
        );
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.provider_failure_recoverable_candidate".to_string(),
            message: "a non-empty base-bound patch was retained without treating it as verified or promotable"
                .to_string(),
            data: serde_json::json!({
                "provider_failure_classification": provider_failure,
                "required_validation": ["fresh_review", "deterministic_gates"],
                "recovery_action": "homeboy agent-task review <run-id>",
            }),
        });
    }

    pub(super) fn cancelled_outcome(task_id: String, summary: String) -> AgentTaskOutcome {
        AgentTaskOutcome {
            task_id,
            status: AgentTaskOutcomeStatus::Cancelled,
            summary: Some(summary),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "scheduler".to_string(),
                uri: "homeboy://agent-task/cancelled".to_string(),
                label: Some("scheduler cancellation".to_string()),
            }],
            ..Default::default()
        }
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
        // Required-artifact validation runs before timeout discovery. Re-run its
        // materialization after merging late evidence so a captured patch cannot
        // coexist with a false missing-artifact diagnosis.
        Self::normalize_required_typed_artifacts(outcome, request);
        if missing_required_typed_artifacts(outcome, request).is_empty() {
            outcome.diagnostics.retain(|diagnostic| {
                diagnostic.class != "agent_task.required_typed_artifacts_missing"
            });
        }

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
            task_id,
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(summary.clone()),
            failure_classification: Some(AgentTaskFailureClassification::PolicyDenied),
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
            ..Default::default()
        }
    }

    /// Effective provider rotation policy for one task: a per-task
    /// `metadata.provider_rotation` object overrides the plan-level
    /// `options.rotation` policy. Returns `None` when no policy with entries is
    /// configured so unconfigured behavior stays byte-for-byte unchanged.
    pub(crate) fn rotation_policy_for_request(
        request: &AgentTaskRequest,
        plan_rotation: Option<&AgentTaskProviderRotationPolicy>,
    ) -> Option<AgentTaskProviderRotationPolicy> {
        request
            .metadata
            .get("provider_rotation")
            .and_then(|value| {
                serde_json::from_value::<AgentTaskProviderRotationPolicy>(value.clone()).ok()
            })
            .or_else(|| plan_rotation.cloned())
            .filter(|policy| !policy.entries.is_empty())
    }

    pub(crate) fn initial_rotation_index(
        request: &AgentTaskRequest,
        policy: &AgentTaskProviderRotationPolicy,
    ) -> usize {
        policy.entries.first().is_some_and(|entry| {
            entry
                .backend
                .as_deref()
                .is_none_or(|backend| backend == request.executor.backend)
                && entry
                    .selector
                    .as_deref()
                    .is_none_or(|selector| request.executor.selector.as_deref() == Some(selector))
                && entry
                    .model
                    .as_deref()
                    .is_none_or(|model| request.executor.model() == Some(model))
                && entry.provider_config.as_object().is_none_or(|overrides| {
                    overrides.iter().all(|(key, value)| {
                        request
                            .executor
                            .config
                            .get(key)
                            .is_some_and(|actual| actual == value)
                    })
                })
        }) as usize
    }

    /// Rotation triggers only on provider capacity failures (`provider`,
    /// `transient`, `timeout`, `stalled`, `rate_limited`, and
    /// `provider_account_blocked` classifications).
    /// Task-level failures (`execution_failed`, `policy_denied`,
    /// `invalid_input`, `capability_missing`, `unknown`) never rotate so a
    /// provider swap cannot mask a real task failure or policy denial (#6978).
    pub(super) fn should_rotate_provider(
        outcome: &AgentTaskOutcome,
        policy: &AgentTaskProviderRotationPolicy,
        rotation_index: usize,
        rotations_used: usize,
        attempt: u32,
        max_provider_executions: u32,
        max_provider_rotations: u32,
    ) -> bool {
        rotation_index < policy.entries.len()
            && rotations_used < max_provider_rotations as usize
            && attempt < max_provider_executions
            && attempt < policy.max_total_attempts()
            && !matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded
                    | AgentTaskOutcomeStatus::NoOp
                    | AgentTaskOutcomeStatus::Cancelled
                    | AgentTaskOutcomeStatus::CandidateRecoverable
            )
            && matches!(
                outcome.failure_classification,
                Some(
                    AgentTaskFailureClassification::Provider
                        | AgentTaskFailureClassification::Transient
                        | AgentTaskFailureClassification::Timeout
                        | AgentTaskFailureClassification::Stalled
                        | AgentTaskFailureClassification::RateLimited
                        | AgentTaskFailureClassification::ProviderAccountBlocked
                        | AgentTaskFailureClassification::ProviderQuotaExhausted
                        | AgentTaskFailureClassification::ProviderBillingBlocked
                        | AgentTaskFailureClassification::ProviderCredentialsExhausted
                )
            )
    }

    /// Apply one rotation entry onto the re-dispatched request's executor.
    /// Unset entry fields inherit the failing attempt's values; the entry's
    /// `provider_config` object is merged over the executor config, mirroring
    /// the dispatch provider-config layering. Also copies the policy-level
    /// liveness limit into the request so the provider runner can enforce it
    /// per attempt.
    /// Backfill the initial attempt's model from the first rotation entry (#9013).
    ///
    /// The first configured rotation entry describes the initial attempt (later
    /// entries are failover). `apply_rotation_entry` only runs when rotating *to*
    /// a new entry after a failure, so a cook that relied on a configured
    /// `rotation.entries` default (no explicit `--model`) executed with no model,
    /// persisted `provider_model: null`, and then failed finalization *after*
    /// publishing a PR.
    ///
    /// This deliberately only backfills the **model** — not backend, selector,
    /// provider_config, or adoption. Those fields carry failover-specific
    /// semantics (e.g. an entry may switch provider or adopt a prior candidate)
    /// that must not reshape the initial attempt; only the missing model
    /// identity is needed for durable provenance. An explicit `--model` on the
    /// request always wins.
    pub(crate) fn apply_initial_rotation_entry_model(
        request: &mut AgentTaskRequest,
        entry: &AgentTaskProviderRotationEntry,
    ) {
        if request.executor.model.is_some() {
            return;
        }
        // Only backfill from an entry that targets the same backend the initial
        // attempt will actually run (or a backend-agnostic entry). A failover
        // entry for a different provider must not lend its model to this backend.
        if let Some(entry_backend) = entry.backend.as_deref() {
            if entry_backend != request.executor.backend {
                return;
            }
        }
        if let Some(model) = &entry.model {
            request.executor.model = Some(model.clone());
            if let Some(selection) = request.executor.runtime_selection.as_mut() {
                if selection.model.is_none() {
                    selection.model = Some(model.clone());
                }
            }
        }
    }

    pub(crate) fn apply_rotation_entry(
        request: &mut AgentTaskRequest,
        entry: &AgentTaskProviderRotationEntry,
        policy: &AgentTaskProviderRotationPolicy,
    ) {
        let route_changed = entry
            .backend
            .as_deref()
            .is_some_and(|backend| backend != request.executor.backend)
            || entry
                .selector
                .as_deref()
                .is_some_and(|selector| request.executor.selector.as_deref() != Some(selector));
        let executor = &mut request.executor;
        if let Some(backend) = &entry.backend {
            executor.backend = backend.clone();
        }
        if let Some(selector) = &entry.selector {
            executor.selector = Some(selector.clone());
        }
        if let Some(model) = &entry.model {
            executor.model = Some(model.clone());
        }
        if route_changed && entry.provider_config.get("provider").is_none() {
            executor
                .config
                .as_object_mut()
                .map(|config| config.remove("provider"));
            if let Some(selection) = executor.runtime_selection.as_mut() {
                selection.ai_provider_id = None;
            }
        }
        if let Some(overrides) = entry.provider_config.as_object() {
            if !overrides.is_empty() {
                if !executor.config.is_object() {
                    executor.config = Value::Object(serde_json::Map::new());
                }
                executor
                    .config
                    .as_object_mut()
                    .expect("executor config object")
                    .extend(overrides.clone());
            }
        }
        if let Some(selection) = executor.runtime_selection.as_mut() {
            if entry.backend.is_some() {
                selection.executor_backend = entry.backend.clone();
            }
            if entry.selector.is_some() {
                selection.executor_provider_id = entry.selector.clone();
            }
            if entry.model.is_some() {
                selection.model = entry.model.clone();
            }
            if let Some(provider) = entry
                .provider_config
                .get("provider")
                .and_then(Value::as_str)
            {
                selection.ai_provider_id = Some(provider.to_string());
            }
        }
        if route_changed {
            request
                .metadata
                .as_object_mut()
                .map(|metadata| metadata.remove("resolved_runtime_identity"));
        }
        Self::apply_rotation_policy_limits(request, policy);
    }

    /// Copy the policy-level liveness limit into the request when the request
    /// does not already set it. Keeps a per-task override authoritative.
    pub(crate) fn apply_rotation_policy_limits(
        request: &mut AgentTaskRequest,
        policy: &AgentTaskProviderRotationPolicy,
    ) {
        if request.limits.liveness_timeout_ms.is_none() {
            request.limits.liveness_timeout_ms = policy.liveness_timeout_ms;
        }
    }

    /// Return provider routes in the exact order the scheduler can reach them.
    /// The transition count is persisted by compile-time admission so readiness
    /// routing and later execution share one rotation budget.
    pub(crate) fn provider_route_candidates(
        request: &AgentTaskRequest,
        plan_rotation: Option<&AgentTaskProviderRotationPolicy>,
    ) -> Vec<(AgentTaskRequest, usize)> {
        let policy = Self::rotation_policy_for_request(request, plan_rotation);
        let mut candidate = request.clone();
        let Some(policy) = policy.as_ref() else {
            return vec![(candidate, 0)];
        };
        Self::apply_rotation_policy_limits(&mut candidate, policy);
        let bound_index = candidate
            .metadata
            .pointer("/provider_readiness_routing/next_rotation_index")
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok())
            .map(|index| index.min(policy.entries.len()));
        if bound_index.is_none() {
            if let Some(entry) = policy.entries.first() {
                Self::apply_initial_rotation_entry_model(&mut candidate, entry);
            }
        }
        let mut index =
            bound_index.unwrap_or_else(|| Self::initial_rotation_index(&candidate, policy));
        let mut candidates = vec![(candidate.clone(), index)];
        while index < policy.entries.len() {
            Self::apply_rotation_entry(&mut candidate, &policy.entries[index], policy);
            index += 1;
            candidates.push((candidate.clone(), index));
        }
        candidates
    }

    /// Advance a scheduled task past any rotation entries whose provider
    /// Homeboy already knows is over its usage cap this run, so a fanout
    /// sibling (or a later task in this same plan) does not spend a provider
    /// execution rediscovering a cap another task already paid to learn
    /// (#13644).
    ///
    /// A task with no rotation policy is returned unchanged: there is nothing
    /// configured to fail over to, so existing single-attempt behavior is
    /// preserved exactly. When every rotation entry reachable from the
    /// task's current position is presently capped, the last skip's
    /// `exhausted` flag is set so the caller can fail the task without
    /// dispatching a provider already known to refuse it.
    pub(super) fn skip_capped_rotation_entries(
        scheduled: &mut ScheduledTask,
        policy: Option<&AgentTaskProviderRotationPolicy>,
        usage_caps: &crate::agent_task_provider::ProviderUsageCapRegistry,
        now: chrono::DateTime<chrono::Utc>,
        capacity_key: &dyn Fn(&AgentTaskRequest) -> String,
    ) -> Vec<UsageCapSkip> {
        let mut skipped = Vec::new();
        let Some(policy) = policy else {
            return skipped;
        };
        loop {
            let key = capacity_key(&scheduled.request);
            let Some(reset_at) = usage_caps.active(&key, now) else {
                break;
            };
            let backend = scheduled.request.executor.backend.clone();
            let selector = scheduled.request.executor.selector.clone();
            let model = scheduled.request.executor.model().map(str::to_string);
            if scheduled.rotation_index >= policy.entries.len() {
                skipped.push(UsageCapSkip {
                    backend,
                    selector,
                    model,
                    reset_at,
                    exhausted: true,
                });
                break;
            }
            skipped.push(UsageCapSkip {
                backend,
                selector,
                model,
                reset_at,
                exhausted: false,
            });
            let entry = &policy.entries[scheduled.rotation_index];
            Self::apply_rotation_entry(&mut scheduled.request, entry, policy);
            scheduled.rotation_index += 1;
        }
        skipped
    }

    /// Evidence record for one dispatch attempt under a rotation policy.
    pub(super) fn rotation_attempt_record(
        request: &AgentTaskRequest,
        outcome: &AgentTaskOutcome,
        attempt: u32,
        rotation_index: usize,
    ) -> AgentTaskProviderRotationAttempt {
        AgentTaskProviderRotationAttempt {
            attempt,
            rotation_index,
            backend: request.executor.backend.clone(),
            selector: request.executor.selector.clone(),
            model: request.executor.model().map(str::to_string),
            requested_model: request.metadata["model_selection"]["requested"]
                .as_str()
                .filter(|model| !model.trim().is_empty())
                .map(str::to_string)
                .or_else(|| request.executor.model().map(str::to_string)),
            attempted_model: request.executor.model().map(str::to_string),
            candidate_producing_model: outcome.metadata["model_identity"]["provider_reported"]
                .as_str()
                .filter(|model| !model.trim().is_empty())
                .map(str::to_string),
            status: outcome.status,
            failure_classification: outcome.failure_classification,
            summary: outcome.summary.clone(),
        }
    }

    /// Attach the ordered attempt sequence to the final outcome under
    /// `metadata.provider_rotation.attempts` so durable run records and
    /// `agent-task status|logs|latest` show what happened per attempt.
    pub(super) fn attach_rotation_evidence(
        outcome: &mut AgentTaskOutcome,
        attempts: &[AgentTaskProviderRotationAttempt],
    ) {
        if attempts.is_empty() {
            return;
        }
        if !outcome.metadata.is_object() {
            outcome.metadata = serde_json::json!({});
        }
        outcome
            .metadata
            .as_object_mut()
            .expect("outcome metadata object")
            .insert(
                "provider_rotation".to_string(),
                serde_json::json!({ "attempts": attempts }),
            );
    }

    pub(super) fn attach_readiness_skip_evidence(
        outcome: &mut AgentTaskOutcome,
        skipped: &[ProviderRouteEvidence],
    ) {
        if skipped.is_empty() {
            return;
        }
        if !outcome.metadata.is_object() {
            outcome.metadata = serde_json::json!({});
        }
        outcome
            .metadata
            .as_object_mut()
            .expect("outcome metadata object")
            .insert(
                "provider_readiness_routing".to_string(),
                serde_json::json!({ "skipped": skipped }),
            );
    }

    /// Make an exhausted rotation actionable without requiring an operator to
    /// decode its metadata: name every attempted route and its rejection.
    pub(super) fn attach_rotation_exhaustion_diagnostic(
        outcome: &mut AgentTaskOutcome,
        attempts: &[AgentTaskProviderRotationAttempt],
    ) {
        if attempts.len() < 2 {
            return;
        }
        let rejections = attempts
            .iter()
            .map(|attempt| {
                let model = attempt.model.as_deref().unwrap_or("default model");
                let reason = attempt
                    .failure_classification
                    .map(|classification| format!("{classification:?}").to_ascii_lowercase())
                    .unwrap_or_else(|| "unclassified failure".to_string());
                format!("{}/{}: {reason}", attempt.backend, model)
            })
            .collect::<Vec<_>>()
            .join("; ");
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.provider_rotation_exhausted".to_string(),
            message: format!("all configured provider routes were rejected: {rejections}"),
            data: serde_json::json!({ "attempts": attempts }),
        });
    }

    /// Record the configured budget and the terminal constraint that stopped
    /// additional provider execution. This makes a timeout distinguishable from
    /// an exhausted same-provider, rotation, or total-execution budget.
    pub(super) fn attach_execution_budget_evidence(
        outcome: &mut AgentTaskOutcome,
        budget: &AgentTaskExecutionBudget,
        executions_used: u32,
        rotations_used: usize,
        same_provider_retries_used: usize,
    ) {
        if !outcome.metadata.is_object() {
            outcome.metadata = serde_json::json!({});
        }
        let terminal_is_failure = !matches!(
            outcome.status,
            AgentTaskOutcomeStatus::Succeeded
                | AgentTaskOutcomeStatus::NoOp
                | AgentTaskOutcomeStatus::Cancelled
        );
        // Only name a budget that was actually reached. This used to fall
        // through to `same_provider_retries` for *any* terminal failure, so a
        // task that failed for unrelated reasons was still decorated with an
        // authoritative "execution budget exhausted" diagnostic — which sends
        // whoever reads it after the wrong cause (#11419).
        //
        // The rotation and retry arms additionally require that the budget was
        // consumed at all. A limit of zero was never available to exhaust, so
        // blaming it explains nothing about why this attempt failed.
        let exhausted = terminal_is_failure
            .then_some({
                if executions_used >= budget.max_provider_executions {
                    Some("total_executions")
                } else if executions_used > 0
                    && rotations_used > 0
                    && rotations_used >= budget.max_provider_rotations as usize
                {
                    Some("provider_rotations")
                } else if same_provider_retries_used > 0
                    && same_provider_retries_used >= budget.max_same_provider_retries as usize
                {
                    Some("same_provider_retries")
                } else {
                    None
                }
            })
            .flatten();
        outcome
            .metadata
            .as_object_mut()
            .expect("outcome metadata object")
            .insert(
                "execution_budget".to_string(),
                serde_json::json!({
                    "max_provider_executions": budget.max_provider_executions,
                    "max_same_provider_retries": budget.max_same_provider_retries,
                    "max_provider_rotations": budget.max_provider_rotations,
                    "executions_used": executions_used,
                    "provider_rotations_used": rotations_used,
                    "same_provider_retries_used": same_provider_retries_used,
                    "remaining_provider_executions": budget.max_provider_executions.saturating_sub(executions_used),
                    "exhausted": exhausted,
                    "terminal_reason": format!("{:?}", outcome.status).to_lowercase(),
                }),
            );
        if let Some(exhausted) = exhausted {
            outcome.diagnostics.push(AgentTaskDiagnostic {
                class: "agent_task.execution_budget_exhausted".to_string(),
                message: "provider execution stopped because its configured execution budget was exhausted"
                    .to_string(),
                data: serde_json::json!({
                    "exhausted_budget": match exhausted {
                        "total_executions" => "max_provider_executions",
                        "provider_rotations" => "max_provider_rotations",
                        _ => "max_same_provider_retries",
                    },
                    "executions_used": executions_used,
                    "provider_rotations_used": rotations_used,
                }),
            });
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "retry policy preserves separately persisted counters and classifications"
    )]
    pub(super) fn should_retry(
        outcome: &AgentTaskOutcome,
        attempt: u32,
        max_same_provider_retries: u32,
        max_provider_executions: u32,
        retry_max_attempts: u32,
        retry_budget_total: Option<u32>,
        retry_budget_used: u32,
        retryable_failure_classifications: &[AgentTaskFailureClassification],
    ) -> bool {
        // A zero legacy limit means no legacy cap. It does not authorize
        // retries for an otherwise unbounded default budget.
        let legacy_retry_permits_attempt = (retry_max_attempts == 0
            && max_provider_executions != u32::MAX)
            || attempt < retry_max_attempts;
        attempt < max_provider_executions
            && legacy_retry_permits_attempt
            && attempt <= max_same_provider_retries
            && retry_budget_total
                .map(|budget| retry_budget_used < budget)
                .unwrap_or(true)
            && outcome.failure_classification != Some(AgentTaskFailureClassification::PolicyDenied)
            && outcome.failure_classification
                != Some(AgentTaskFailureClassification::ProviderAccountBlocked)
            && outcome.failure_classification
                != Some(AgentTaskFailureClassification::ProviderQuotaExhausted)
            && outcome.failure_classification
                != Some(AgentTaskFailureClassification::ProviderBillingBlocked)
            && outcome.failure_classification
                != Some(AgentTaskFailureClassification::ProviderCredentialsExhausted)
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
                    | AgentTaskOutcomeStatus::CandidateRecoverable
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
            AgentTaskOutcomeStatus::CandidateRecoverable => AgentTaskState::CandidateRecoverable,
            AgentTaskOutcomeStatus::Timeout => AgentTaskState::TimedOut,
            AgentTaskOutcomeStatus::Cancelled => AgentTaskState::Cancelled,
            _ => AgentTaskState::Failed,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "queue status projects independently observable scheduler dimensions"
    )]
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

        if outcomes
            .iter()
            .all(|outcome| outcome.status == AgentTaskOutcomeStatus::CandidateRecoverable)
        {
            return AgentTaskAggregateStatus::PartialRecoverable;
        }
        let failed = outcomes.iter().any(|outcome| {
            !matches!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded
                    | AgentTaskOutcomeStatus::NoOp
                    | AgentTaskOutcomeStatus::CandidateRecoverable
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
                AgentTaskOutcomeStatus::CandidateRecoverable => {
                    totals.candidate_recoverable += 1;
                    totals.recoverable_candidates += 1;
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

fn is_base_bound_patch_candidate(outcome: &AgentTaskOutcome, artifact: &AgentTaskArtifact) -> bool {
    is_actionable_patch_artifact(artifact)
        && artifact.size_bytes.is_some_and(|size| size > 0)
        && artifact
            .sha256
            .as_deref()
            .is_some_and(homeboy_engine_primitives::content_hash::is_sha256_hex)
        && artifact.metadata.get("task_id").and_then(Value::as_str) == Some(&outcome.task_id)
        && artifact
            .metadata
            .get("producer_attempt")
            .is_some_and(Value::is_u64)
        && [
            "run_id",
            "base_ref",
            "provider_backend",
            "repository_identity",
            "workspace_identity",
        ]
        .iter()
        .all(|key| {
            artifact
                .metadata
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
        })
}

#[cfg(test)]
mod execution_budget_evidence_tests {
    use super::*;

    fn budget(executions: u32, rotations: u32, retries: u32) -> AgentTaskExecutionBudget {
        AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: executions,
            max_same_provider_retries: retries,
            max_provider_rotations: rotations,
        }
    }

    fn failed_outcome() -> AgentTaskOutcome {
        AgentTaskOutcome {
            task_id: "task-1".to_string(),
            status: AgentTaskOutcomeStatus::Failed,
            ..Default::default()
        }
    }

    fn exhaustion_diagnostic(outcome: &AgentTaskOutcome) -> Option<&AgentTaskDiagnostic> {
        outcome
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.class == "agent_task.execution_budget_exhausted")
    }

    /// A failure that never approached a limit must not be labelled a budget
    /// exhaustion. This previously fell through to `same_provider_retries` for
    /// every terminal failure, which reported an authoritative cause that had
    /// nothing to do with the actual one (#11419).
    #[test]
    fn a_failure_within_budget_is_not_labelled_exhausted() {
        let mut outcome = failed_outcome();
        AgentTaskScheduleSupport::attach_execution_budget_evidence(
            &mut outcome,
            &budget(10, 10, 0),
            2,
            1,
            0,
        );

        assert!(
            exhaustion_diagnostic(&outcome).is_none(),
            "2 of 10 executions and 1 of 10 rotations is not exhaustion: {:#?}",
            outcome.diagnostics
        );
        // The counts stay attached regardless — they are useful on every outcome.
        assert_eq!(outcome.metadata["execution_budget"]["executions_used"], 2);
        assert_eq!(
            outcome.metadata["execution_budget"]["same_provider_retries_used"],
            0
        );
        assert_eq!(
            outcome.metadata["execution_budget"]["exhausted"],
            Value::Null
        );
    }

    #[test]
    fn a_genuinely_exhausted_budget_is_still_reported() {
        for (executions, rotations, retries, used, rotations_used, retries_used, expected) in [
            (
                1u32,
                10u32,
                10u32,
                1u32,
                0usize,
                0usize,
                "max_provider_executions",
            ),
            (10, 1, 10, 2, 1, 0, "max_provider_rotations"),
            (10, 10, 1, 2, 0, 1, "max_same_provider_retries"),
        ] {
            let mut outcome = failed_outcome();
            AgentTaskScheduleSupport::attach_execution_budget_evidence(
                &mut outcome,
                &budget(executions, rotations, retries),
                used,
                rotations_used,
                retries_used,
            );
            let diagnostic =
                exhaustion_diagnostic(&outcome).expect("a reached limit must still be reported");
            assert_eq!(diagnostic.data["exhausted_budget"], expected);
        }
    }

    /// A limit of zero was never available to exhaust, so blaming it explains
    /// nothing about why the attempt failed.
    #[test]
    fn an_unavailable_budget_is_not_blamed() {
        let mut outcome = failed_outcome();
        AgentTaskScheduleSupport::attach_execution_budget_evidence(
            &mut outcome,
            &budget(10, 0, 0),
            1,
            0,
            0,
        );

        assert!(
            exhaustion_diagnostic(&outcome).is_none(),
            "zero-limit budgets that were never consumed must not be blamed: {:#?}",
            outcome.diagnostics
        );
    }

    #[test]
    fn a_successful_outcome_is_never_labelled_exhausted() {
        let mut outcome = AgentTaskOutcome {
            task_id: "task-1".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            ..Default::default()
        };
        AgentTaskScheduleSupport::attach_execution_budget_evidence(
            &mut outcome,
            &budget(1, 0, 0),
            1,
            0,
            0,
        );

        assert!(exhaustion_diagnostic(&outcome).is_none());
    }
}
