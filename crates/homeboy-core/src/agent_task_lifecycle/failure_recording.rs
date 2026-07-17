use super::*;
use sha2::Digest;
use std::path::{Path, PathBuf};

pub fn record_pre_execution_failure(
    run_id: &str,
    plan: &AgentTaskPlan,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let mut record = store::read_record(&run_id)?;
    let task_count = plan.tasks.len();
    let failed = task_count;
    let diagnostic = AgentTaskDiagnostic {
        class: "pre_execution_failure".to_string(),
        message: error.message.clone(),
        data: json!({
            "phase": phase,
            "error_code": error.code.as_str(),
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
        }),
    };
    let outcomes = plan
        .tasks
        .iter()
        .map(|task| AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: task.task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(format!(
                "agent-task pre-execution {phase} failed: {}",
                error.message
            )),
            failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "agent-task-pre-execution-failure".to_string(),
                uri: format!("homeboy://agent-task/run/{run_id}/status"),
                label: Some("Agent-task pre-execution failure".to_string()),
            }],
            diagnostics: vec![diagnostic.clone()],
            outputs: json!({
                "schema": "homeboy/agent-task-pre-execution-failure/v1",
                "phase": phase,
                "error_code": error.code.as_str(),
                "message": error.message,
                "details": error.details.clone(),
                "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
            }),
            workflow: None,
            follow_up: None,
            metadata: json!({
                "kind": "pre_execution_failure",
                "phase": phase,
                "error_code": error.code.as_str(),
            }),
        })
        .collect();
    let aggregate = AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Failed,
        totals: AgentTaskAggregateTotals {
            failed,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes,
        events: plan
            .tasks
            .iter()
            .map(|task| AgentTaskProgressEvent {
                task_id: task.task_id.clone(),
                state: AgentTaskState::Failed,
                attempt: 1,
                message: Some(format!(
                    "agent-task pre-execution {phase} failed: {}",
                    error.message
                )),
            })
            .collect(),
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: AgentTaskQueueStatus {
            max_concurrency: plan.options.max_concurrency,
            completed: failed,
            ..AgentTaskQueueStatus::default()
        },
    };
    let mut failed_record = record_aggregate(&mut record, plan, &aggregate)?;
    let runner_id = failed_record.runner_id().map(str::to_string);
    let metadata = failed_record.ensure_metadata_object();
    metadata.insert(
        "pre_execution_failure".to_string(),
        json!({
            "phase": phase,
            "error_code": error.code.as_str(),
            "failure_code": error.details.get("field").cloned().unwrap_or_else(|| json!(error.code.as_str())),
            "message": error.message,
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
            "provider_executions_consumed": 0,
            "controller_identity": crate::build_identity::current().display,
            "runner_id": runner_id,
            "task_linkage": plan.tasks.iter().map(|task| json!({
                "task_id": task.task_id,
                "workspace": task.workspace,
                "source_refs": task.source_refs,
            })).collect::<Vec<_>>(),
        }),
    );
    store::write_record(&failed_record)?;
    Ok(failed_record)
}

/// Shared `(run_id, runner_id)` identity borrowed by the Lab offload dispatch
/// failure/record builders. Embedded as a named field so each builder stops
/// repeating the same two borrows without changing any serialized shape (these
/// builders are internal and not serialized).
#[derive(Debug, Clone, Copy)]
pub struct RunDispatchIdentity<'a> {
    pub run_id: &'a str,
    pub runner_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct AgentTaskPreDispatchFailure<'a> {
    pub identity: RunDispatchIdentity<'a>,
    pub local_command: Vec<String>,
    pub remote_command: Vec<String>,
    pub remote_workspace: &'a str,
    pub failure_message: &'a str,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: i32,
}

pub fn record_pre_dispatch_failure(
    failure: AgentTaskPreDispatchFailure<'_>,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(failure.identity.run_id);
    if let Ok(record) = status(&run_id) {
        return Ok(record);
    }

    let task_id = "agent-task-predispatch".to_string();
    let metadata = json!({
        "kind": "lab_offload_pre_dispatch_failure",
        "runner_id": failure.identity.runner_id,
        "remote_workspace": failure.remote_workspace,
        "local_command": failure.local_command,
        "remote_command": failure.remote_command,
        "exit_code": failure.exit_code,
        "failure_message": failure.failure_message,
    });
    let plan = AgentTaskPlan::new(
        format!("{run_id}.predispatch"),
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: task_id.clone(),
            group_key: Some("lab-offload".to_string()),
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "homeboy-lab".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "Persist Lab offload pre-dispatch validation failure evidence."
                .to_string(),
            inputs: json!({
                "local_command": failure.local_command,
                "remote_command": failure.remote_command,
                "runner_id": failure.identity.runner_id,
                "remote_workspace": failure.remote_workspace,
                "failure": {
                    "message": failure.failure_message,
                    "exit_code": failure.exit_code,
                    "stdout": failure.stdout,
                    "stderr": failure.stderr,
                }
            }),
            source_refs: vec![AgentTaskSourceRef {
                kind: "lab-offload-run".to_string(),
                uri: format!("homeboy://agent-task/run/{run_id}/lab-offload"),
                revision: None,
            }],
            workspace: AgentTaskWorkspace {
                mode: AgentTaskWorkspaceMode::Existing,
                root: Some(failure.remote_workspace.to_string()),
                slug: None,
                kind: Some("lab-offload".to_string()),
                component_id: None,
                branch: None,
                base_ref: None,
                task_url: None,
                cleanup: Some("preserve".to_string()),
                attempt: None,
                materialization: metadata.clone(),
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: metadata.clone(),
        }],
    );
    submit_plan(&plan, Some(&run_id))?;
    let aggregate = AgentTaskAggregate {
        schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
        plan_id: plan.plan_id.clone(),
        status: AgentTaskAggregateStatus::Failed,
        totals: AgentTaskAggregateTotals {
            failed: 1,
            ..AgentTaskAggregateTotals::default()
        },
        outcomes: vec![AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some(failure.failure_message.to_string()),
            failure_classification: Some(AgentTaskFailureClassification::InvalidInput),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "lab-offload-pre-dispatch-failure".to_string(),
                uri: format!("homeboy://agent-task/run/{run_id}/logs"),
                label: Some("Lab offload pre-dispatch failure".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: json!({
                "schema": "homeboy/agent-task-predispatch-failure/v1",
                "runner_id": failure.identity.runner_id,
                "remote_workspace": failure.remote_workspace,
                "local_command": failure.local_command,
                "remote_command": failure.remote_command,
                "exit_code": failure.exit_code,
                "stdout": failure.stdout,
                "stderr": failure.stderr,
            }),
            workflow: None,
            follow_up: None,
            metadata,
        }],
        events: vec![
            AgentTaskProgressEvent {
                task_id: task_id.clone(),
                state: AgentTaskState::Queued,
                attempt: 1,
                message: Some("Lab offload selected and remote command prepared".to_string()),
            },
            AgentTaskProgressEvent {
                task_id,
                state: AgentTaskState::Failed,
                attempt: 1,
                message: Some(failure.failure_message.to_string()),
            },
        ],
        artifact_lineage: Vec::new(),
        child_runs: Vec::new(),
        artifact_bindings: Vec::new(),
        queue: AgentTaskQueueStatus {
            max_concurrency: 1,
            completed: 1,
            ..AgentTaskQueueStatus::default()
        },
    };
    record_run_aggregate(&run_id, &plan, &aggregate)
}

#[derive(Debug, Clone)]
pub struct AgentTaskRemoteDispatchFailure<'a> {
    pub identity: RunDispatchIdentity<'a>,
    pub local_command: Vec<String>,
    pub remote_command: Vec<String>,
    pub remote_workspace: &'a str,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: i32,
}

pub fn record_remote_dispatch_failure(
    failure: AgentTaskRemoteDispatchFailure<'_>,
    envelope: &Value,
) -> Result<Option<AgentTaskRunRecord>> {
    if envelope.get("schema").and_then(Value::as_str) != Some("homeboy/agent-task-dispatch/v1") {
        return Ok(None);
    }

    let Some(aggregate_value) = envelope.get("aggregate") else {
        return Ok(None);
    };

    let run_id = sanitize_run_id(failure.identity.run_id);
    let mut aggregate: AgentTaskAggregate = serde_json::from_value(aggregate_value.clone())
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse offloaded agent-task dispatch aggregate".to_string()),
            )
        })?;
    enrich_remote_dispatch_aggregate(envelope, &mut aggregate);
    if aggregate.events.is_empty() {
        aggregate.events = events_for_outcomes(&aggregate.outcomes);
    }

    let (
        mut record,
        remote_run_id,
        remote_plan_path,
        remote_aggregate_path,
        needs_atomic_terminal_commit,
    ) = if let Some(record_value) = envelope.get("record") {
        let mut record: AgentTaskRunRecord =
            serde_json::from_value(record_value.clone()).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("parse offloaded agent-task dispatch record".to_string()),
                )
            })?;
        let remote_run_id = record.run_id.clone();
        let remote_plan_path = record.plan_path.clone();
        let remote_aggregate_path = record.aggregate_path.clone();
        let plan = if std::path::Path::new(&record.plan_path).is_file() {
            store::read_plan_path(&record.plan_path)?
        } else {
            synthetic_remote_dispatch_plan(&run_id, &failure, envelope, &aggregate)
        };
        record.run_id = run_id.clone();
        record.plan_path = store::write_plan(&run_id, &plan)?.display().to_string();
        apply_aggregate_to_record(
            &mut record,
            &plan,
            &aggregate,
            store::aggregate_path(&run_id)?.display().to_string(),
        );
        (
            record,
            remote_run_id,
            remote_plan_path,
            remote_aggregate_path,
            true,
        )
    } else {
        let remote_run_id = envelope
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or(failure.identity.run_id)
            .to_string();
        let remote_plan_path = envelope
            .get("plan_path")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                envelope
                    .get("plan_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&aggregate.plan_id)
                    .to_string()
            });
        let remote_aggregate_path = envelope
            .get("aggregate_path")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let plan = synthetic_remote_dispatch_plan(&run_id, &failure, envelope, &aggregate);
        let mut record = submit_plan(&plan, Some(&run_id))?;
        record_aggregate(&mut record, &plan, &aggregate)?;
        (
            record,
            remote_run_id,
            remote_plan_path,
            remote_aggregate_path,
            false,
        )
    };

    let provider_run_ids: Vec<String> = record
        .provider_handles
        .iter()
        .map(|handle| handle.provider_run_id.clone())
        .collect();
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "kind".to_string(),
        json!("lab_offload_remote_dispatch_failure"),
    );
    metadata.insert("runner_id".to_string(), json!(failure.identity.runner_id));
    metadata.insert(
        "remote_workspace".to_string(),
        json!(failure.remote_workspace),
    );
    metadata.insert("local_command".to_string(), json!(failure.local_command));
    metadata.insert("remote_command".to_string(), json!(failure.remote_command));
    metadata.insert("exit_code".to_string(), json!(failure.exit_code));
    metadata.insert("stdout".to_string(), json!(failure.stdout));
    metadata.insert("stderr".to_string(), json!(failure.stderr));
    metadata.insert("remote_run_id".to_string(), json!(remote_run_id));
    metadata.insert("remote_plan_path".to_string(), json!(remote_plan_path));
    metadata.insert(
        "remote_aggregate_path".to_string(),
        json!(remote_aggregate_path),
    );
    metadata.insert("provider_run_ids".to_string(), json!(provider_run_ids));

    if needs_atomic_terminal_commit {
        store::write_aggregate_and_record(&record, &aggregate)?;
    } else {
        store::write_record(&record)?;
    }
    Ok(Some(record))
}

fn enrich_remote_dispatch_aggregate(envelope: &Value, aggregate: &mut AgentTaskAggregate) {
    let remote_run_id = envelope.get("run_id").and_then(Value::as_str);
    for outcome in &mut aggregate.outcomes {
        normalize_provider_run_result(outcome);

        if outcome.evidence_refs.is_empty() {
            if let Some(remote_run_id) = remote_run_id {
                outcome.evidence_refs.extend([
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-logs".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/logs"),
                        label: Some("Remote agent-task logs".to_string()),
                    },
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-review".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/review"),
                        label: Some("Remote agent-task review".to_string()),
                    },
                    AgentTaskEvidenceRef {
                        kind: "remote-agent-task-artifacts".to_string(),
                        uri: format!("homeboy://agent-task/run/{remote_run_id}/artifacts"),
                        label: Some("Remote agent-task artifacts".to_string()),
                    },
                ]);
            }
        }
    }
}

fn synthetic_remote_dispatch_plan(
    run_id: &str,
    failure: &AgentTaskRemoteDispatchFailure<'_>,
    envelope: &Value,
    aggregate: &AgentTaskAggregate,
) -> AgentTaskPlan {
    let tasks = aggregate
        .outcomes
        .iter()
        .map(|outcome| {
            let provider = outcome
                .metadata
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("homeboy-lab");
            AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: outcome.task_id.clone(),
                group_key: Some("lab-offload".to_string()),
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: provider.to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: outcome.summary.clone().unwrap_or_else(|| {
                    "Persist remote Lab agent-task dispatch outcome.".to_string()
                }),
                inputs: json!({
                    "remote_dispatch_envelope": envelope,
                    "remote_command": failure.remote_command,
                }),
                source_refs: vec![AgentTaskSourceRef {
                    kind: "lab-offload-remote-dispatch".to_string(),
                    uri: envelope
                        .get("run_id")
                        .and_then(Value::as_str)
                        .map(|remote_run_id| format!("homeboy://agent-task/run/{remote_run_id}"))
                        .unwrap_or_else(|| {
                            format!("homeboy://agent-task/run/{run_id}/lab-offload")
                        }),
                    revision: envelope
                        .get("plan_id")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                }],
                workspace: AgentTaskWorkspace {
                    mode: AgentTaskWorkspaceMode::Existing,
                    root: Some(failure.remote_workspace.to_string()),
                    slug: None,
                    kind: Some("lab-offload".to_string()),
                    component_id: None,
                    branch: None,
                    base_ref: None,
                    task_url: None,
                    cleanup: Some("preserve".to_string()),
                    attempt: None,
                    materialization: json!({
                        "runner_id": failure.identity.runner_id,
                        "remote_workspace": failure.remote_workspace,
                    }),
                },
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                metadata: outcome.metadata.clone(),
            }
        })
        .collect();

    let mut plan = AgentTaskPlan::new(
        envelope
            .get("plan_id")
            .and_then(Value::as_str)
            .unwrap_or(&aggregate.plan_id),
        tasks,
    );
    plan.group_key = Some("lab-offload".to_string());
    plan.metadata = json!({
        "kind": "lab_offload_remote_dispatch_failure",
        "runner_id": failure.identity.runner_id,
        "remote_workspace": failure.remote_workspace,
        "remote_run_id": envelope.get("run_id").and_then(Value::as_str),
    });
    plan
}

pub(crate) fn record_aggregate(
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let aggregate_path = store::aggregate_path(&record.run_id)?;
    apply_aggregate_to_record(
        record,
        plan,
        aggregate,
        aggregate_path.display().to_string(),
    );
    crate::controller_scratch::register_outcome_resources(&record.run_id, &aggregate.outcomes)?;
    crate::controller_scratch::finalize_run(&record.run_id)?;
    store::write_aggregate_and_record(record, aggregate)?;
    record_terminal_artifact_projection(record, aggregate)?;
    Ok(record.clone())
}

pub(crate) fn record_terminal_artifact_projection(
    record: &mut AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    if record.runner_id().is_none() && aggregate_has_unresolved_actionable_patch(aggregate) {
        match runner_id_from_artifact_provenance(aggregate) {
            Ok(runner_id) => {
                record
                    .ensure_metadata_object()
                    .insert("runner_id".to_string(), json!(runner_id));
            }
            Err(error) => {
                record.ensure_metadata_object().insert(
                    "artifact_projection".to_string(),
                    json!({ "status": "pending", "error": error.message }),
                );
                return store::write_record(record);
            }
        }
    }
    match project_terminal_artifacts(record, aggregate) {
        Ok(()) => {
            record.ensure_metadata_object().insert(
                "artifact_projection".to_string(),
                json!({ "status": "complete" }),
            );
        }
        Err(error) => {
            record.ensure_metadata_object().insert(
                "artifact_projection".to_string(),
                json!({ "status": "pending", "error": error.message }),
            );
        }
    }
    store::write_record(record)
}

/// Recover the runner identity for canonical legacy patch artifacts. Diagnostic
/// artifacts can share an aggregate without participating in promotion.
fn runner_id_from_artifact_provenance(aggregate: &AgentTaskAggregate) -> Result<String> {
    let runner_ids = aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.artifacts)
        .filter(|artifact| {
            crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
                && artifact.path.as_deref().is_some_and(|path| Path::new(path).is_absolute())
                && artifact.size_bytes.is_some()
                && artifact.sha256.is_some()
        })
        .map(|artifact| {
            artifact
                .metadata
                .pointer("/source_provenance/runner_id")
                .and_then(Value::as_str)
                .filter(|runner_id| !runner_id.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "artifact.metadata.source_provenance.runner_id",
                        "cannot recover a controller artifact projection without unambiguous runner provenance",
                        Some(artifact.id.clone()),
                        None,
                    )
                })
        })
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    match runner_ids.into_iter().collect::<Vec<_>>().as_slice() {
        [runner_id] => Ok(runner_id.clone()),
        _ => Err(Error::validation_invalid_argument(
            "artifact.metadata.source_provenance.runner_id",
            "cannot recover a controller artifact projection without unambiguous runner provenance",
            None,
            None,
        )),
    }
}

fn aggregate_has_unresolved_actionable_patch(aggregate: &AgentTaskAggregate) -> bool {
    aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.artifacts)
        .any(|artifact| {
            crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
                && artifact
                    .path
                    .as_deref()
                    .is_some_and(|path| Path::new(path).is_absolute())
                && artifact.size_bytes.is_some()
                && artifact.sha256.is_some()
        })
}

pub(crate) fn terminal_artifact_projection_is_verified(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<bool> {
    for outcome in &aggregate.outcomes {
        for artifact in &outcome.artifacts {
            if crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
                && artifact.path.is_some()
                && artifact.size_bytes.is_some()
                && artifact.sha256.is_some()
            {
                if verified_controller_artifact_projection_path(
                    &record.run_id,
                    &outcome.task_id,
                    artifact,
                )?
                .is_none()
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: &str, kind: &str, runner_id: Option<&str>) -> AgentTaskArtifact {
        AgentTaskArtifact {
            schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
            id: id.to_string(),
            kind: kind.to_string(),
            name: None,
            label: None,
            role: None,
            semantic_key: None,
            path: Some("/runner/patch.diff".to_string()),
            url: None,
            mime: None,
            size_bytes: Some(1),
            sha256: Some("a".repeat(64)),
            metadata: runner_id.map_or_else(
                || json!({}),
                |runner_id| json!({ "source_provenance": { "runner_id": runner_id } }),
            ),
        }
    }

    #[test]
    fn legacy_runner_provenance_uses_only_actionable_patch_artifacts() {
        let mut aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: "plan".to_string(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals::default(),
            outcomes: Vec::new(),
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: AgentTaskQueueStatus::default(),
        };
        aggregate.outcomes.push(AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: "task".to_string(),
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: None,
            failure_classification: None,
            artifacts: vec![
                artifact("patch", "patch", Some("runner-a")),
                artifact("transcript", "transcript", None),
                artifact("result", "result", None),
                artifact("runtime-log", "runtime-log", None),
            ],
            typed_artifacts: Vec::new(),
            evidence_refs: Vec::new(),
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        });

        assert_eq!(
            runner_id_from_artifact_provenance(&aggregate).expect("consistent provenance"),
            "runner-a"
        );
        aggregate.outcomes[0]
            .artifacts
            .push(artifact("second-patch", "patch", Some("runner-b")));
        assert!(runner_id_from_artifact_provenance(&aggregate).is_err());
    }

    #[test]
    fn retained_runner_attachment_requires_matching_binding_and_content_identity() {
        crate::test_support::with_isolated_home(|home| {
            let run_id = "retained-runner-attachment";
            let task_id = "task-a";
            let bytes = b"patch bytes";
            let source = home.path().join("patch.diff");
            std::fs::write(&source, bytes).expect("write patch");
            crate::agent_task_lifecycle::record_detached_lab_run(
                crate::agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "homeboy-lab",
                    runner_job_id: "job-123",
                    remote_workspace: "/runner/workspace",
                    remote_command: &["homeboy".to_string(), "agent-task".to_string()],
                },
            )
            .expect("record runner handoff");
            let store = crate::observation::ObservationStore::open_initialized().expect("store");
            let retained = store
                .record_artifact_with_id(
                    run_id,
                    "patch",
                    &source,
                    "attached-patch",
                    json!({
                        "agent_task": {
                            "retained_runner_binding": {
                                "runner_id": "homeboy-lab",
                                "runner_job_id": "job-123"
                            }
                        }
                    }),
                )
                .expect("retain attached bytes");
            let artifact = AgentTaskArtifact {
                schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch".to_string(),
                kind: "patch".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: Some("/runner/workspace/patch.diff".to_string()),
                url: None,
                mime: Some("text/x-patch".to_string()),
                size_bytes: Some(bytes.len() as u64),
                sha256: Some(format!("{:x}", sha2::Sha256::digest(bytes))),
                metadata: json!({}),
            };

            assert_eq!(
                verified_controller_artifact_projection_path(run_id, task_id, &artifact)
                    .expect("matching retained bytes"),
                Some(std::path::PathBuf::from(&retained.path))
            );
            assert_eq!(
                verified_controller_artifact_projection_path(run_id, task_id, &artifact)
                    .expect("idempotent retained lookup"),
                Some(std::path::PathBuf::from(&retained.path))
            );

            let wrong_binding = store
                .record_artifact_with_id(
                    run_id,
                    "patch",
                    &source,
                    "wrong-binding-patch",
                    json!({
                        "agent_task": {
                            "retained_runner_binding": {
                                "runner_id": "other-runner",
                                "runner_job_id": "job-123"
                            }
                        }
                    }),
                )
                .expect("retain wrong binding bytes");
            assert_eq!(
                retained_attachment_binding_matches(
                    &wrong_binding,
                    Some(("homeboy-lab", "job-123"))
                ),
                false
            );
            assert_eq!(
                retained_attachment_binding_matches(
                    &wrong_binding,
                    Some(("other-runner", "job-999"))
                ),
                false
            );
            store
                .delete_artifact_record(&retained.id)
                .expect("remove matching retained record");
            assert_eq!(
                verified_controller_artifact_projection_path(run_id, task_id, &artifact)
                    .expect("wrong runner binding is ignored"),
                None
            );

            let mismatched = home.path().join("mismatched.patch");
            std::fs::write(&mismatched, b"different patch").expect("write mismatch");
            let mismatched = store
                .record_artifact_with_id(
                    run_id,
                    "patch",
                    &mismatched,
                    "mismatched-patch",
                    json!({
                        "agent_task": {
                            "retained_runner_binding": {
                                "runner_id": "homeboy-lab",
                                "runner_job_id": "job-123"
                            }
                        }
                    }),
                )
                .expect("retain mismatched bytes");
            assert!(
                verified_controller_artifact_projection_path(run_id, task_id, &artifact)
                    .expect_err("mismatched retained bytes fail closed")
                    .message
                    .contains("does not match the aggregate SHA-256 and size")
            );
            assert!(std::path::Path::new(&mismatched.path).is_file());

            store
                .record_artifact_with_id(
                    run_id,
                    "patch",
                    &source,
                    "duplicate-patch",
                    json!({
                        "agent_task": {
                            "retained_runner_binding": {
                                "runner_id": "homeboy-lab",
                                "runner_job_id": "job-123"
                            }
                        }
                    }),
                )
                .expect("retain duplicate bytes");
            assert!(
                verified_controller_artifact_projection_path(run_id, task_id, &artifact)
                    .expect_err("ambiguous retained artifact names fail closed")
                    .message
                    .contains("multiple controller-side artifact projections")
            );
        });
    }
}

/// Project finalized executor artifacts into the standard observation registry.
/// The lifecycle aggregate remains the source of task semantics; the registry
/// supplies the canonical retrievable-byte index used by `runs artifact get`.
pub(crate) fn project_terminal_artifacts(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let store = crate::observation::ObservationStore::open_initialized()?;
    let status = match record.state {
        AgentTaskRunState::Succeeded => "pass",
        AgentTaskRunState::PartialRecoverable => "fail",
        AgentTaskRunState::PartialFailure => "fail",
        AgentTaskRunState::Failed => "fail",
        AgentTaskRunState::Cancelled => "fail",
        _ => return Ok(()),
    };
    let mut existing_metadata = store
        .get_run(&record.run_id)?
        .map(|run| run.metadata_json)
        .unwrap_or_else(|| json!({ "agent_task_run": record.run_id }));
    if !existing_metadata.is_object() {
        existing_metadata = json!({});
    }
    existing_metadata
        .as_object_mut()
        .expect("object checked above")
        .insert("agent_task_terminal_state".to_string(), json!(record.state));
    store.upsert_imported_run_preserving_terminal(&crate::observation::RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: None,
        started_at: record.submitted_at.clone(),
        finished_at: record.updated_at.clone(),
        status: status.to_string(),
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version: Some(crate::build_identity::current().display),
        git_sha: None,
        rig_id: None,
        metadata_json: existing_metadata,
    })?;

    let mut used_ids = std::collections::BTreeSet::new();
    let mut projection_error = None;
    for outcome in &aggregate.outcomes {
        for artifact in &outcome.artifacts {
            let Some(path) = artifact.path.as_deref() else {
                continue;
            };
            if artifact.size_bytes.is_none() || artifact.sha256.is_none() {
                // Unreadable/remote declarations remain visible to review only.
                continue;
            }
            validate_projection_token("artifact.id", &artifact.id)?;
            validate_projection_token("artifact.kind", &artifact.kind)?;
            let base_id = artifact.id.trim();
            let logical_id = unique_logical_artifact_id(&mut used_ids, base_id, &outcome.task_id);
            // Observation artifact ids are globally unique. Keep the lifecycle
            // logical id as the per-run lookup token exposed by runs artifact.
            let mut id_hash = sha2::Sha256::new();
            sha2::Digest::update(&mut id_hash, record.run_id.as_bytes());
            sha2::Digest::update(&mut id_hash, [0]);
            sha2::Digest::update(&mut id_hash, outcome.task_id.as_bytes());
            sha2::Digest::update(&mut id_hash, [0]);
            sha2::Digest::update(&mut id_hash, logical_id.as_bytes());
            let artifact_id = format!("agent-task-{:x}", id_hash.finalize());
            let metadata = json!({
                "name": logical_id,
                "agent_task": {
                    "task_id": outcome.task_id,
                    "logical_artifact_id": logical_id,
                    "runner_provenance": artifact.metadata,
                }
            });
            if reusable_terminal_artifact(&store, &record.run_id, artifact, &artifact_id)? {
                continue;
            }
            if let Some(runner_id) = record.runner_id().filter(|runner_id| {
                super::lifecycle_ops::execution_runner_id().as_deref() != Some(*runner_id)
            }) {
                if runner_id.trim().is_empty() {
                    return Err(Error::validation_invalid_argument(
                        "runner_id",
                        "runner id cannot be empty when creating a runner artifact reference",
                        None,
                        None,
                    ));
                }
                match controller_finalized_artifact_path(artifact)? {
                    Some(path) => {
                        let mut controller_hash = sha2::Sha256::new();
                        sha2::Digest::update(&mut controller_hash, b"controller");
                        sha2::Digest::update(&mut controller_hash, [0]);
                        sha2::Digest::update(&mut controller_hash, artifact_id.as_bytes());
                        let controller_artifact_id =
                            format!("agent-task-{:x}", controller_hash.finalize());
                        let mut metadata = metadata;
                        metadata["agent_task"]["projection"] = json!("controller_finalized");
                        store.record_artifact_with_id(
                            &record.run_id,
                            &artifact.kind,
                            path,
                            &controller_artifact_id,
                            metadata,
                        )?;
                    }
                    None => {
                        let remote_ref = crate::execution_contract::EXECUTION_CONTRACT
                            .artifacts
                            .runner_artifact_ref(runner_id, &record.run_id, &logical_id);
                        let mirror_result =
                            if crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(
                                artifact,
                            ) {
                                (|| -> Result<()> {
                                    let mirror =
                                        tempfile::NamedTempFile::new().map_err(|error| {
                                            Error::internal_io(
                                                error.to_string(),
                                                Some(
                                                    "create controller artifact mirror".to_string(),
                                                ),
                                            )
                                        })?;
                                    let download =
                                crate::observation::runs_service::runner_evidence::with_runner_evidence(
                                    |p| {
                                        p.download_remote_artifact(
                                            &remote_ref,
                                            Some(mirror.path().to_path_buf()),
                                        )
                                    },
                                )?;
                                    let expected_size = artifact.size_bytes.expect("checked above");
                                    let expected_sha256 =
                                        artifact.sha256.as_deref().expect("checked above");
                                    let actual_size = std::fs::metadata(&download.output_path)
                                        .map_err(|error| {
                                            Error::internal_io(
                                                error.to_string(),
                                                Some(
                                                    "inspect controller artifact mirror"
                                                        .to_string(),
                                                ),
                                            )
                                        })?
                                        .len();
                                    let actual_sha256 = crate::artifact_metadata::sha256_file(
                                        &download.output_path,
                                    )?;
                                    if actual_size != expected_size
                                        || actual_sha256 != expected_sha256
                                    {
                                        return Err(Error::validation_invalid_argument(
                                    "artifact_id",
                                    format!(
                                        "runner artifact mirror for run '{}', task '{}', and artifact '{}' does not match the aggregate SHA-256 and size",
                                        record.run_id, outcome.task_id, artifact.id
                                    ),
                                    Some(artifact.id.clone()),
                                    None,
                                ));
                                    }
                                    let mut controller_hash = sha2::Sha256::new();
                                    sha2::Digest::update(&mut controller_hash, b"controller");
                                    sha2::Digest::update(&mut controller_hash, [0]);
                                    sha2::Digest::update(
                                        &mut controller_hash,
                                        artifact_id.as_bytes(),
                                    );
                                    let controller_artifact_id =
                                        format!("agent-task-{:x}", controller_hash.finalize());
                                    let mut controller_metadata = metadata.clone();
                                    controller_metadata["agent_task"]["projection"] =
                                        json!("runner_mirrored");
                                    store.record_artifact_with_id(
                                        &record.run_id,
                                        &artifact.kind,
                                        &download.output_path,
                                        &controller_artifact_id,
                                        controller_metadata,
                                    )?;
                                    Ok(())
                                })()
                            } else {
                                Ok(())
                            };

                        // Preserve the canonical runner retrieval alias even when
                        // the controller also materializes verified bytes.
                        store.import_artifact(&crate::observation::ArtifactRecord {
                            id: artifact_id,
                            run_id: record.run_id.clone(),
                            kind: artifact.kind.clone(),
                            artifact_type: "remote_file".to_string(),
                            path: remote_ref,
                            url: None,
                            public_url: None,
                            viewer_url: None,
                            viewer_links: Vec::new(),
                            sha256: artifact.sha256.clone(),
                            size_bytes: artifact
                                .size_bytes
                                .and_then(|value| i64::try_from(value).ok()),
                            mime: artifact.mime.clone(),
                            metadata_json: metadata,
                            created_at: chrono::Utc::now().to_rfc3339(),
                        })?;
                        if let Err(error) = mirror_result {
                            projection_error.get_or_insert(error);
                        }
                    }
                }
            } else {
                store.record_artifact_with_id(
                    &record.run_id,
                    &artifact.kind,
                    path,
                    &artifact_id,
                    metadata,
                )?;
            }
        }
    }
    projection_error.map_or(Ok(()), Err)
}

/// A direct artifact import can retain the same deterministic lifecycle id
/// before terminal reconciliation. Reuse it only when its controller-local
/// bytes prove it belongs to this artifact projection.
fn reusable_terminal_artifact(
    store: &crate::observation::ObservationStore,
    run_id: &str,
    artifact: &AgentTaskArtifact,
    artifact_id: &str,
) -> Result<bool> {
    let Some(existing) = store.get_artifact(artifact_id)? else {
        return Ok(false);
    };
    if existing.artifact_type != "file" {
        return Ok(false);
    }

    let expected_size = i64::try_from(artifact.size_bytes.expect("checked above")).ok();
    let expected_sha256 = artifact.sha256.as_deref().expect("checked above");
    let matches = existing.run_id == run_id
        && existing.size_bytes == expected_size
        && existing.sha256.as_deref() == Some(expected_sha256)
        && std::fs::metadata(&existing.path)
            .map(|metadata| {
                metadata.is_file() && i64::try_from(metadata.len()).ok() == expected_size
            })
            .unwrap_or(false)
        && crate::artifact_metadata::sha256_file(Path::new(&existing.path))
            .ok()
            .as_deref()
            == Some(expected_sha256);
    if matches {
        return Ok(true);
    }

    Err(Error::validation_invalid_argument(
        "artifact_id",
        format!(
            "existing artifact record conflicts with terminal artifact projection: {artifact_id}"
        ),
        Some(artifact_id.to_string()),
        None,
    ))
}

/// Find finalized bytes already copied into the controller artifact root. Lab
/// aggregate paths describe runner provenance and are never read after recovery.
fn controller_finalized_artifact_path(artifact: &AgentTaskArtifact) -> Result<Option<PathBuf>> {
    let Some(expected_sha256) = artifact.sha256.as_deref() else {
        return Ok(None);
    };
    let Some(expected_size) = artifact.size_bytes else {
        return Ok(None);
    };
    let root = crate::paths::artifact_root()?.join("executor-finalized");
    if !root.is_dir() {
        return Ok(None);
    }
    let mut matches = Vec::new();
    collect_matching_finalized_artifacts(&root, expected_sha256, expected_size, &mut matches)?;
    matches.sort();
    Ok(matches.into_iter().next())
}

fn collect_matching_finalized_artifacts(
    directory: &Path,
    expected_sha256: &str,
    expected_size: u64,
    matches: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in std::fs::read_dir(directory).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "read controller finalized artifact directory {}",
                directory.display()
            )),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read controller finalized artifact entry".to_string()),
            )
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!(
                    "inspect controller finalized artifact {}",
                    path.display()
                )),
            )
        })?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_matching_finalized_artifacts(&path, expected_sha256, expected_size, matches)?;
        } else if metadata.is_file()
            && metadata.len() == expected_size
            && crate::artifact_metadata::sha256_file(&path)? == expected_sha256
        {
            matches.push(path);
        }
    }
    Ok(())
}

/// Locate the controller-owned copy of a lifecycle artifact. Aggregate paths
/// describe producer provenance and can point at a runner after reconciliation;
/// promotion must consume the controller projection instead.
pub fn verified_controller_artifact_projection_path(
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
) -> Result<Option<PathBuf>> {
    let Some(expected_sha256) = artifact.sha256.as_deref() else {
        return Ok(None);
    };
    let Some(expected_size) = artifact
        .size_bytes
        .and_then(|size| i64::try_from(size).ok())
    else {
        return Ok(None);
    };
    let store = crate::observation::ObservationStore::open_initialized()?;
    let record = store::read_record(run_id)?;
    let runner_binding = record.runner_id().zip(record.runner_job_id());
    let artifacts = store.list_artifacts(run_id)?;
    let canonical: Vec<_> = artifacts
        .iter()
        .filter(|candidate| canonical_projection_matches(candidate, task_id, artifact))
        .collect();
    // A task-labelled projection is the controller's authoritative byte copy.
    // Retained runner attachments only bootstrap recovery when it is absent.
    let candidates = if canonical.is_empty() {
        artifacts
            .iter()
            .filter(|candidate| {
                candidate.artifact_type == "file"
                    && candidate.kind == artifact.kind
                    && retained_attachment_binding_matches(candidate, runner_binding)
            })
            .collect()
    } else {
        canonical
    };
    if candidates.is_empty() {
        return Ok(None);
    }
    if candidates.len() != 1 {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "multiple controller-side artifact projections match run '{run_id}', task '{task_id}', and artifact '{}'",
                artifact.id
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    let candidate = candidates[0];
    let path = PathBuf::from(&candidate.path);
    let actual_size = std::fs::metadata(&path)
        .ok()
        .and_then(|metadata| i64::try_from(metadata.len()).ok());
    let actual_sha256 = crate::artifact_metadata::sha256_file(&path).ok();
    if candidate.sha256.as_deref() != Some(expected_sha256)
        || candidate.size_bytes != Some(expected_size)
        || actual_size != Some(expected_size)
        || actual_sha256.as_deref() != Some(expected_sha256)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "controller-side artifact projection for run '{run_id}', task '{task_id}', and artifact '{}' does not match the aggregate SHA-256 and size",
                artifact.id
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    Ok(Some(path))
}

fn canonical_projection_matches(
    candidate: &crate::observation::ArtifactRecord,
    task_id: &str,
    artifact: &AgentTaskArtifact,
) -> bool {
    candidate.artifact_type == "file"
        && candidate.kind == artifact.kind
        && candidate
            .metadata_json
            .pointer("/agent_task/task_id")
            .and_then(serde_json::Value::as_str)
            == Some(task_id)
        && candidate
            .metadata_json
            .pointer("/agent_task/logical_artifact_id")
            .and_then(serde_json::Value::as_str)
            == Some(artifact.id.as_str())
}

/// A manually retained runner artifact can become a controller projection only
/// when the owning run's immutable runner/job binding matches. The aggregate's
/// SHA-256 and size are checked by the caller before any bytes are consumed.
fn retained_attachment_binding_matches(
    candidate: &crate::observation::ArtifactRecord,
    runner_binding: Option<(&str, &str)>,
) -> bool {
    let Some((runner_id, runner_job_id)) = runner_binding else {
        return false;
    };
    candidate
        .metadata_json
        .pointer("/agent_task/retained_runner_binding/runner_id")
        .and_then(serde_json::Value::as_str)
        == Some(runner_id)
        && candidate
            .metadata_json
            .pointer("/agent_task/retained_runner_binding/runner_job_id")
            .and_then(serde_json::Value::as_str)
            == Some(runner_job_id)
}

fn unique_logical_artifact_id(
    used_ids: &mut std::collections::BTreeSet<String>,
    base_id: &str,
    task_id: &str,
) -> String {
    if used_ids.insert(base_id.to_string()) {
        return base_id.to_string();
    }
    let prefix = format!("{task_id}-{base_id}");
    for suffix in 1_u64.. {
        let candidate = if suffix == 1 {
            prefix.clone()
        } else {
            format!("{prefix}-{suffix}")
        };
        if used_ids.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("unbounded artifact aliases cannot exhaust u64")
}

fn validate_projection_token(field: &str, value: &str) -> Result<()> {
    crate::agent_task_provider::artifact_finalization::validate_token(field, value)
}

pub(crate) fn apply_aggregate_to_record(
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    aggregate_path: String,
) {
    record.updated_at = Some(now_timestamp());
    set_run_state(record, run_state_for_aggregate(aggregate));
    record.aggregate_path = Some(aggregate_path);
    record.totals = Some(aggregate.totals.clone());
    record.tasks = tasks_for_aggregate(plan, aggregate);
    record.artifact_refs = artifact_refs_for_outcomes(&aggregate.outcomes);
    record.provider_handles = provider_handles_for_outcomes(&aggregate.outcomes);
    persist_provider_handle_models(&mut record.provider_handles, plan);
    record.latest_executor_evidence = latest_executor_evidence(&record.run_id, plan, aggregate);
    update_lifecycle_from_record(record, plan);
    let provider_run_ids: Vec<String> = record
        .provider_handles
        .iter()
        .map(|handle| handle.provider_run_id.clone())
        .collect();
    let latest_executor_evidence_value = record
        .latest_executor_evidence
        .as_ref()
        .map(|evidence| serde_json::to_value(evidence).unwrap_or(Value::Null));
    let metadata = record.ensure_metadata_object();
    metadata.insert("provider_run_ids".to_string(), json!(provider_run_ids));
    if let Some(evidence) = latest_executor_evidence_value {
        metadata.insert("latest_executor_evidence".to_string(), evidence);
    }
}

fn persist_provider_handle_models(
    handles: &mut [AgentTaskRunProviderHandle],
    plan: &AgentTaskPlan,
) {
    for handle in handles {
        if handle
            .metadata
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|model| !model.trim().is_empty())
        {
            continue;
        }
        let Some(model) = plan
            .tasks
            .iter()
            .find(|task| task.task_id == handle.task_id)
            .and_then(|task| task.executor.model())
            .filter(|model| !model.trim().is_empty())
        else {
            continue;
        };
        if !handle.metadata.is_object() {
            handle.metadata = json!({});
        }
        handle
            .metadata
            .as_object_mut()
            .expect("provider handle metadata object")
            .insert("model".to_string(), json!(model));
    }
}
