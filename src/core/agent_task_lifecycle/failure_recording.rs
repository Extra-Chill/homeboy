use super::*;

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
    let metadata = failed_record.ensure_metadata_object();
    metadata.insert(
        "pre_execution_failure".to_string(),
        json!({
            "phase": phase,
            "error_code": error.code.as_str(),
            "message": error.message,
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
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

    let (mut record, remote_run_id, remote_plan_path, remote_aggregate_path) =
        if let Some(record_value) = envelope.get("record") {
            let mut record: AgentTaskRunRecord = serde_json::from_value(record_value.clone())
                .map_err(|error| {
                    Error::internal_json(
                        error.to_string(),
                        Some("parse offloaded agent-task dispatch record".to_string()),
                    )
                })?;
            let remote_run_id = record.run_id.clone();
            let remote_plan_path = record.plan_path.clone();
            let remote_aggregate_path = record.aggregate_path.clone();
            let plan = store::read_plan_path(&record.plan_path).unwrap_or_else(|_| {
                synthetic_remote_dispatch_plan(&run_id, &failure, envelope, &aggregate)
            });
            record.run_id = run_id.clone();
            record.plan_path = store::write_plan(&run_id, &plan)?.display().to_string();
            apply_aggregate_to_record(
                &mut record,
                &plan,
                &aggregate,
                store::write_aggregate(&run_id, &aggregate)?
                    .display()
                    .to_string(),
            );
            (
                record,
                remote_run_id,
                remote_plan_path,
                remote_aggregate_path,
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

    store::write_record(&record)?;
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
    let aggregate_path = store::write_aggregate(&record.run_id, aggregate)?;
    apply_aggregate_to_record(
        record,
        plan,
        aggregate,
        aggregate_path.display().to_string(),
    );
    store::write_record(record)?;
    Ok(record.clone())
}

pub(crate) fn apply_aggregate_to_record(
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
    aggregate_path: String,
) {
    record.state = run_state_for_aggregate(aggregate);
    record.updated_at = Some(now_timestamp());
    record.aggregate_path = Some(aggregate_path);
    record.totals = Some(aggregate.totals.clone());
    record.tasks = tasks_for_aggregate(plan, aggregate);
    record.artifact_refs = artifact_refs_for_outcomes(&aggregate.outcomes);
    record.provider_handles = provider_handles_for_outcomes(&aggregate.outcomes);
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
