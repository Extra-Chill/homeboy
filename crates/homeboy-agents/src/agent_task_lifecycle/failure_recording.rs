use super::*;
use homeboy_engine_primitives::content_hash;
use sha2::Digest;
use std::path::{Path, PathBuf};

pub fn record_pre_execution_failure(
    run_id: &str,
    plan: &AgentTaskPlan,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_pre_execution_failure_in_store(&lifecycle_store, run_id, plan, phase, error)
}

pub(crate) fn record_pre_execution_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
    plan: &AgentTaskPlan,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let run_id = sanitize_run_id(run_id);
    let mut record = lifecycle_store.read_record(&run_id)?;

    // A transport/handoff error that arrives AFTER a provider attempt was
    // already dispatched (or completed) on a runner is not a pre-execution
    // failure. Terminalizing it here would overwrite a succeeded candidate with
    // a `Failed`, zero-artifact, `provider_executions_consumed: 0` record and
    // strand the work — exactly the loss #9377 describes. Preserve the candidate
    // and record the follow-up failure as a non-terminal, recoverable marker so
    // controller reconciliation can adopt the completed candidate without
    // rerunning the provider.
    if record.has_recorded_provider_progress() {
        return record_transport_follow_up_failure_in_store(lifecycle_store, record, phase, error);
    }

    let task_count = plan.tasks.len();
    let failed = task_count;
    let retryable = error.retryable == Some(true);
    let failure_classification = pre_execution_failure_classification(error);
    let candidate_adoption_recovery = candidate_adoption_recovery(phase);
    let outcomes = plan
        .tasks
        .iter()
        .map(|task| build_pre_execution_failure_outcome(&run_id, task, phase, error))
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
    let mut failed_record =
        record_aggregate_in_store(lifecycle_store, &mut record, plan, &aggregate)?;
    let runner_id = failed_record.runner_id().map(str::to_string);
    let metadata = failed_record.ensure_metadata_object();
    if retryable {
        metadata.insert("retryable".to_string(), json!(true));
    }
    metadata.insert(
        "pre_execution_failure".to_string(),
        json!({
            "phase": phase,
            "error_code": error.code.as_str(),
            "failure_classification": failure_classification,
            "retryable": retryable,
            "failure_code": error.details.get("field").cloned().unwrap_or_else(|| json!(error.code.as_str())),
            "message": error.message,
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
            "provider_executions_consumed": 0,
            "candidate_adoption_recovery": candidate_adoption_recovery,
            "controller_identity": homeboy_core::build_identity::current().display,
            "runner_id": runner_id,
            "task_linkage": plan.tasks.iter().map(|task| json!({
                "task_id": task.task_id,
                "workspace": task.workspace,
                "source_refs": task.source_refs,
            })).collect::<Vec<_>>(),
        }),
    );
    lifecycle_store.write_record(&failed_record)?;
    Ok(failed_record)
}

/// Record a post-provider transport/handoff failure without terminalizing a run
/// that already produced a candidate (#9377). The existing durable record — its
/// state, aggregate, artifacts, and provider handles — is preserved verbatim;
/// only a recoverable `transport_follow_up_failure` marker is stamped so
/// controller reconciliation can adopt the completed candidate rather than
/// rerunning the provider. The failure stays `retryable` so recovery is
/// attempted, and never regresses a terminal candidate to `Failed`.
fn record_transport_follow_up_failure(
    record: AgentTaskRunRecord,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_transport_follow_up_failure_in_store(&lifecycle_store, record, phase, error)
}

fn record_transport_follow_up_failure_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    mut record: AgentTaskRunRecord,
    phase: &str,
    error: &Error,
) -> Result<AgentTaskRunRecord> {
    let metadata = record.ensure_metadata_object();
    metadata.insert(
        "transport_follow_up_failure".to_string(),
        json!({
            "schema": super::CANDIDATE_ADOPTION_RECOVERY_SCHEMA,
            "phase": phase,
            "reason": "post_provider_transport_failure",
            "error_code": error.code.as_str(),
            "message": error.message,
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
            "retryable": true,
            "candidate_preserved": true,
            "recorded_at": now_timestamp(),
        }),
    );
    // Preserve the candidate/workspace: a run with recorded provider progress
    // must not be reaped as a clean success when its follow-up failed.
    metadata.insert("candidate_preserved".to_string(), json!(true));
    metadata.insert(METADATA_KEY_RETRYABLE.to_string(), json!(true));
    lifecycle_store.write_record(&record)?;
    Ok(record)
}

pub(crate) fn build_pre_execution_failure_outcome(
    run_id: &str,
    task: &AgentTaskRequest,
    phase: &str,
    error: &Error,
) -> AgentTaskOutcome {
    let retryable = error.retryable == Some(true);
    let failure_classification = pre_execution_failure_classification(error);
    let candidate_adoption_recovery = candidate_adoption_recovery(phase);
    let diagnostic = AgentTaskDiagnostic {
        class: "pre_execution_failure".to_string(),
        message: error.message.clone(),
        data: json!({
            "phase": phase,
            "error_code": error.code.as_str(),
            "retryable": retryable,
            "details": error.details.clone(),
            "hints": error.hints.iter().map(|hint| hint.message.as_str()).collect::<Vec<_>>(),
        }),
    };
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: task.task_id.clone(),
        status: AgentTaskOutcomeStatus::Failed,
        summary: Some(format!(
            "agent-task pre-execution {phase} failed: {}",
            error.message
        )),
        failure_classification: Some(failure_classification),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: vec![AgentTaskEvidenceRef {
            kind: "agent-task-pre-execution-failure".to_string(),
            uri: format!("homeboy://agent-task/run/{run_id}/status"),
            label: Some("Agent-task pre-execution failure".to_string()),
        }],
        diagnostics: vec![diagnostic],
        outputs: json!({
            "schema": "homeboy/agent-task-pre-execution-failure/v1",
            "phase": phase,
            "error_code": error.code.as_str(),
            "retryable": retryable,
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
            "retryable": retryable,
            "provider_executions_consumed": 0,
            "candidate_adoption_recovery": candidate_adoption_recovery,
        }),
    }
}

fn candidate_adoption_recovery(phase: &str) -> Option<serde_json::Value> {
    matches!(
        phase,
        "lab_handoff_preacceptance" | "transport_dispatcher_prepare"
    )
    .then(|| {
        json!({
            "schema": super::CANDIDATE_ADOPTION_RECOVERY_SCHEMA,
            "reason": "pre_provider_transport_failure",
            "provider_executions_consumed": 0,
        })
    })
}

fn pre_execution_failure_classification(error: &Error) -> AgentTaskFailureClassification {
    if error.details["pre_execution_phase"] == "gate_toolchain_preflight" {
        return AgentTaskFailureClassification::CapabilityMissing;
    }
    if error.retryable == Some(true) {
        AgentTaskFailureClassification::Transient
    } else {
        AgentTaskFailureClassification::InvalidInput
    }
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
                kind: Some("lab-offload".to_string()),
                cleanup: Some("preserve".to_string()),
                materialization: metadata.clone(),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
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
                output_declarations: Vec::new(),
                runtime_tools: Vec::new(),
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
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()?;
    record_aggregate_in_store(&lifecycle_store, record, plan, aggregate)
}

pub(crate) fn record_aggregate_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<AgentTaskRunRecord> {
    let aggregate_path = lifecycle_store.aggregate_path(&record.run_id);
    apply_aggregate_to_record(
        record,
        plan,
        aggregate,
        aggregate_path.display().to_string(),
    );
    let mut retained_roots = Vec::new();
    let roots: Vec<PathBuf> = plan
        .tasks
        .iter()
        .filter_map(|task| task.workspace.root.as_ref())
        .filter_map(|root| {
            let path = PathBuf::from(root);
            if path.is_dir() {
                Some(path)
            } else {
                retained_roots.push(serde_json::json!({
                    "path": root,
                    "reason": "workspace root is inaccessible from this controller",
                }));
                None
            }
        })
        .collect();
    record.ensure_metadata_object();
    if !roots.is_empty() {
        record.metadata["automatic_artifact_retention"] =
            match homeboy_core::cleanup::try_run_automatic_artifact_retention(roots) {
                Ok(Some(output)) => serde_json::to_value(output).unwrap_or_else(|error| {
                    serde_json::json!({ "status": "retained", "reason": error.to_string() })
                }),
                Ok(None) => serde_json::json!({
                    "status": "busy",
                    "reason": "automatic artifact retention is already running",
                }),
                Err(error) => serde_json::json!({ "status": "retained", "reason": error.message }),
            };
    }
    if !retained_roots.is_empty() {
        record.metadata["automatic_artifact_retention_inaccessible_roots"] =
            serde_json::Value::Array(retained_roots);
    }
    crate::controller_scratch::register_outcome_resources_at(
        &lifecycle_store.data_root(),
        &record.run_id,
        &aggregate.outcomes,
    )?;
    crate::controller_scratch::finalize_run_at(&lifecycle_store.data_root(), &record.run_id)?;
    lifecycle_store.write_aggregate_and_record(record, aggregate)?;
    record_terminal_artifact_projection_in_store(lifecycle_store, record, aggregate)?;
    // The Cook index this completion updates belongs to the same store the
    // aggregate was just committed into. Resolving it ambiently made this
    // rooted function write its substantive-candidate pointer into whatever
    // home the environment pointed at, which no positive assertion here can
    // see: every record and aggregate read back from the injected store is
    // already correct at that point (#7505).
    update_cook_candidate_after_completion_in_store(lifecycle_store, record, aggregate, None)?;
    Ok(record.clone())
}

/// Register a terminal run's artifacts into its own store's projection root.
///
/// There is no ambient wrapper: the last caller that resolved its own root was
/// `project_terminal_runner_lifecycle_event`, and it now takes a store (#7505).
/// Leaving an unused ambient form behind would only be a new way to reach the
/// process artifact root by accident.
pub(crate) fn record_terminal_artifact_projection_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let recovery_command = format!("homeboy agent-task status {}", record.run_id);
    if record.runner_id().is_none() && aggregate_has_runner_backed_actionable_patch(aggregate) {
        match runner_id_from_artifact_provenance(aggregate) {
            Ok(runner_id) => {
                record
                    .ensure_metadata_object()
                    .insert("runner_id".to_string(), json!(runner_id));
            }
            Err(error) => {
                let status = if error.retryable == Some(true) {
                    "pending"
                } else {
                    "failed"
                };
                record.ensure_metadata_object().insert(
                    "artifact_projection".to_string(),
                    json!({
                        "status": status,
                        "error": error.message,
                        "recovery_action": {
                            "kind": "fetch_and_reconcile",
                            "command": recovery_command,
                        },
                    }),
                );
                return lifecycle_store.write_record(record);
            }
        }
    }
    let observation_store = lifecycle_store.open_observation_initialized()?;
    match project_terminal_artifacts_in_store(&observation_store, record, aggregate) {
        Ok(()) => {
            record.ensure_metadata_object().insert(
                "artifact_projection".to_string(),
                json!({ "status": "complete" }),
            );
        }
        Err(error) => {
            let status = if error.retryable == Some(true) {
                "pending"
            } else {
                "failed"
            };
            record.ensure_metadata_object().insert(
                "artifact_projection".to_string(),
                json!({
                    "status": status,
                    "error": error.message,
                    "recovery_action": {
                        "kind": "fetch_and_reconcile",
                        "command": recovery_command,
                    },
                }),
            );
        }
    }
    lifecycle_store.write_record(record)
}

/// Replace runner-local file references with controller-resolvable aggregate
/// references before persisting a terminal runner result. The original location
/// is deliberately not retained as a durable URI: it is meaningful only on the
/// producing runner and would make a controller attempt local IO against it.
pub(crate) fn project_runner_evidence_refs(
    record: &AgentTaskRunRecord,
    aggregate: &mut AgentTaskAggregate,
) {
    let Some(runner_id) = record
        .runner_id()
        .filter(|runner_id| !runner_id.trim().is_empty())
    else {
        return;
    };
    let runner_job_id = record
        .runner_job_id()
        .filter(|job_id| !job_id.trim().is_empty());
    let encoded_run_id = homeboy_core::execution_contract::encode_uri_component(&record.run_id);

    for outcome in &mut aggregate.outcomes {
        let mut projected = Vec::new();
        for evidence in &mut outcome.evidence_refs {
            if !evidence.uri.starts_with("file://") {
                continue;
            }
            let reference_digest = content_hash::sha256_hex(evidence.uri.as_bytes());
            let encoded_task_id =
                homeboy_core::execution_contract::encode_uri_component(&outcome.task_id);
            evidence.uri = format!(
                "homeboy://agent-task/run/{encoded_run_id}/aggregate#outcome={encoded_task_id}&evidence={reference_digest}"
            );
            projected.push((reference_digest, evidence.kind.clone()));
        }
        if projected.is_empty() {
            continue;
        }
        if !outcome.metadata.is_object() {
            outcome.metadata = json!({});
        }
        if !outcome.metadata["runner_evidence_projection"].is_object() {
            outcome.metadata["runner_evidence_projection"] = json!({});
        }
        let projections = outcome.metadata["runner_evidence_projection"]
            .as_object_mut()
            .expect("runner evidence projection is an object");
        for (reference_digest, kind) in projected {
            projections.insert(
                reference_digest,
                json!({
                    "kind": kind,
                    "source_runner_id": runner_id,
                    "source_runner_job_id": runner_job_id,
                    "retention": "controller_aggregate",
                    "redaction": "producer_redacted",
                }),
            );
        }
    }
}

/// The authoritative model recorded on an aggregate outcome, if any.
///
/// Locates the outcome for `task_id` and reads its concrete model through the
/// canonical [`AgentTaskOutcome::selected_model`] reader, so aggregate → task
/// model reconciliation uses the same present/non-blank definition as every
/// other model-resolution site.
fn aggregate_selected_model(aggregate: &AgentTaskAggregate, task_id: &str) -> Option<String> {
    aggregate
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == task_id)
        .and_then(|outcome| outcome.selected_model())
        .map(str::to_string)
}

/// True when a terminal record's durable lifecycle provider model is missing but
/// the authoritative aggregate outcome recorded a concrete model — the exact
/// stale-terminal state that blocks `finalize-pr` after the #9404/#9405 repair
/// (#9411). The nonterminal `status()` path already normalizes this via
/// `apply_aggregate_to_record`, but a record that went terminal *before* that
/// repair never gets reprojected on read.
pub(crate) fn terminal_provider_model_reconciliation_needed(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> bool {
    record.tasks.iter().any(|task| {
        let durable_missing = task
            .model
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty);
        durable_missing && aggregate_selected_model(aggregate, &task.task_id).is_some()
    })
}

/// Reproject authoritative aggregate model evidence onto a terminal record when
/// the durable lifecycle model is stale-null (#9411).
///
/// Fills only the previously-null provider model — on the run tasks, provider
/// handles, and the rebuilt lifecycle projection — and persists. Terminal state,
/// promotion evidence, gates, totals, and artifact projections are preserved:
/// `set_run_state` re-asserts the existing terminal state and only the
/// model-bearing lifecycle projection is rebuilt from the record's own tasks.
///
/// The persist at the end is the whole point of taking a store: it is the only
/// durable effect, so a reconciliation driven from injected roots must land its
/// repaired model in the same installation the record and aggregate were read
/// from. There is no ambient wrapper — `status_in_store` is the only caller,
/// and the store it hands down is the one its own caller injected.
pub(crate) fn reconcile_terminal_provider_model_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &mut AgentTaskRunRecord,
    plan: &AgentTaskPlan,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let mut changed = false;
    for task in &mut record.tasks {
        if task
            .model
            .as_deref()
            .map(str::trim)
            .is_some_and(|model| !model.is_empty())
        {
            continue;
        }
        if let Some(model) = aggregate_selected_model(aggregate, &task.task_id) {
            task.model = Some(model);
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    persist_provider_handle_models(&mut record.provider_handles, plan);
    update_lifecycle_from_record(record, plan);
    record.updated_at = Some(now_timestamp());
    lifecycle_store.write_record(record)
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
                && artifact.size_bytes.is_some()
                && artifact.sha256.is_some()
        })
        .map(|artifact| {
            artifact_runner_id(artifact)
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

fn artifact_runner_id(artifact: &AgentTaskArtifact) -> Option<&str> {
    artifact
        .metadata
        .pointer("/source_provenance/runner_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|runner_id| !runner_id.is_empty())
}

fn validate_artifact_runner_binding(
    artifact: &AgentTaskArtifact,
    record_runner_id: &str,
) -> Result<()> {
    if artifact_runner_id(artifact).is_some_and(|runner_id| runner_id != record_runner_id) {
        return Err(Error::validation_invalid_argument(
            "artifact.metadata.source_provenance.runner_id",
            format!(
                "artifact '{}' runner provenance conflicts with lifecycle runner binding '{}'",
                artifact.id, record_runner_id
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    Ok(())
}

fn aggregate_has_runner_backed_actionable_patch(aggregate: &AgentTaskAggregate) -> bool {
    aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.artifacts)
        .any(|artifact| {
            crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
                && artifact.path.as_deref().is_none_or(|path| {
                    let path = Path::new(path);
                    path.is_absolute() && !path.is_file()
                })
                && artifact.size_bytes.is_some()
                && artifact.sha256.is_some()
        })
}

pub(crate) fn terminal_artifact_projection_is_verified(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<bool> {
    terminal_artifact_projection_is_verified_with(record, aggregate, || {
        homeboy_core::observation::ObservationStore::open_initialized()
    })
}

/// [`terminal_artifact_projection_is_verified`] against an explicitly injected
/// durable lifecycle root.
///
/// `open_observation_initialized` binds this store's observation database *and*
/// the artifact root it carries, and the second binding is the reason this
/// sibling has to exist. `PathRoots` keeps `artifacts` separate from `data`, and
/// `ObservationStore::open_initialized` resolves both from the process — so a
/// projection verified ambiently looks for controller-owned bytes under
/// whichever artifact root the environment names, and reports an
/// otherwise-complete candidate as unprojected purely because it was asked from
/// the wrong home (#7505, #12618, #12619).
pub(crate) fn terminal_artifact_projection_is_verified_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<bool> {
    terminal_artifact_projection_is_verified_with(record, aggregate, || {
        lifecycle_store.open_observation_initialized()
    })
}

/// The shared body of both forms above.
///
/// `open_observations` is invoked at exactly the point the ambient form used to
/// open its store — lazily, inside the loop, only once an artifact actually
/// requires a durable projection — so neither form initializes an observation
/// database it would not have touched before.
fn terminal_artifact_projection_is_verified_with(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
    mut open_observations: impl FnMut() -> Result<homeboy_core::observation::ObservationStore>,
) -> Result<bool> {
    for outcome in &aggregate.outcomes {
        for artifact in &outcome.artifacts {
            if requires_durable_lab_projection(artifact) {
                if artifact.size_bytes.is_none()
                    || artifact.sha256.is_none()
                    || (artifact.path.is_none() && record.runner_id().is_none())
                {
                    return Ok(false);
                }
                if verified_controller_artifact_projection_path_in_store(
                    &open_observations()?,
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

/// Return a repairable diagnostic when an actionable patch is not yet readable
/// from a verified controller-owned projection. Missing aggregates remain
/// outside this check so historical terminal records keep their existing
/// recovery behavior.
pub fn terminal_artifact_projection_readiness(run_id: &str) -> Result<Option<String>> {
    let record = store::read_record(&super::sanitize_run_id(run_id))?;
    terminal_artifact_projection_readiness_for_record(
        &record,
        store::read_aggregate(&record.run_id),
    )
}

/// [`terminal_artifact_projection_readiness`] against an explicitly injected
/// durable lifecycle root.
///
/// All three reads follow the injected store, and the third is the one that
/// matters: the projection check opens an observation database *and* resolves
/// an artifact root, which `PathRoots` carries separately from `data`. Answered
/// ambiently, an otherwise-complete candidate is reported as unprojected purely
/// because the controller-owned bytes were looked for under the wrong home
/// (#7505, #12618, #12619) — and here that verdict is not merely cosmetic: the
/// caller turns it into a `cook_continuation_scheduler` status that suppresses
/// the terminal continuation enqueue.
///
/// This is the initializing counterpart of
/// [`terminal_artifact_projection_readiness_bounded_in_store`]; it opens the
/// same stores the ambient form opens rather than the read-only ones.
pub fn terminal_artifact_projection_readiness_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<String>> {
    let record = lifecycle_store.read_record(&super::sanitize_run_id(run_id))?;
    terminal_artifact_projection_readiness_for_record_with(
        &record,
        lifecycle_store.read_aggregate(&record.run_id),
        |record, aggregate| {
            terminal_artifact_projection_is_verified_in_store(lifecycle_store, record, aggregate)
        },
    )
}

/// Bounded read-only counterpart used by fanout status while coordinators are
/// writing observations. It intentionally leaves reconciliation to `resume`.
pub fn terminal_artifact_projection_readiness_bounded(run_id: &str) -> Result<Option<String>> {
    let record = store::read_record_bounded(&super::sanitize_run_id(run_id))?;
    terminal_artifact_projection_readiness_for_record(
        &record,
        store::read_aggregate_bounded(&record.run_id),
    )
}

/// [`terminal_artifact_projection_readiness_bounded`] against an explicitly
/// injected durable lifecycle root.
///
/// All three reads follow the injected store. The `store::` shims above are
/// exactly `default_store()?.read_record_bounded` and
/// `read_aggregate_bounded_in_store(&default_store()?, ..)`, and
/// `default_store()` is `AgentTaskLifecycleStore::from_current_environment()`,
/// so an ambient caller reaches byte-identical state through either form. The
/// third is the projection check itself: it opens an observation store and
/// resolves an artifact root, which `PathRoots` carries separately from `data` —
/// see `terminal_artifact_projection_is_verified_in_store`.
pub fn terminal_artifact_projection_readiness_bounded_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<String>> {
    let record = lifecycle_store.read_record_bounded(&super::sanitize_run_id(run_id))?;
    terminal_artifact_projection_readiness_for_record_with(
        &record,
        lifecycle_store.read_aggregate_bounded(&record.run_id),
        |record, aggregate| {
            terminal_artifact_projection_is_verified_in_store(lifecycle_store, record, aggregate)
        },
    )
}

fn terminal_artifact_projection_readiness_for_record(
    record: &AgentTaskRunRecord,
    aggregate: Result<AgentTaskAggregate>,
) -> Result<Option<String>> {
    terminal_artifact_projection_readiness_for_record_with(
        record,
        aggregate,
        terminal_artifact_projection_is_verified,
    )
}

/// The shared body of both readiness forms. `verified` decides which durable
/// roots the projection check itself is answered against; everything else here
/// is derived from the record and aggregate already in hand.
fn terminal_artifact_projection_readiness_for_record_with(
    record: &AgentTaskRunRecord,
    aggregate: Result<AgentTaskAggregate>,
    verified: impl FnOnce(&AgentTaskRunRecord, &AgentTaskAggregate) -> Result<bool>,
) -> Result<Option<String>> {
    let Ok(aggregate) = aggregate else {
        return Ok(None);
    };
    if verified(record, &aggregate)? {
        return Ok(None);
    }
    Ok(Some(
        record
            .metadata
            .get("artifact_projection")
            .and_then(|projection| projection.get("error"))
            .and_then(Value::as_str)
            .unwrap_or(
                "an actionable patch is missing a readable controller projection or required path, size, and SHA-256 integrity metadata",
            )
            .to_string(),
    ))
}

/// Project finalized executor artifacts into the standard observation registry.
/// The lifecycle aggregate remains the source of task semantics; the registry
/// supplies the canonical retrievable-byte index used by `runs artifact get`.
pub(crate) fn project_terminal_artifacts(
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    project_terminal_artifacts_in_store(&store, record, aggregate)
}

fn project_terminal_artifacts_in_store(
    store: &homeboy_core::observation::ObservationStore,
    record: &AgentTaskRunRecord,
    aggregate: &AgentTaskAggregate,
) -> Result<()> {
    let status = match record.state {
        AgentTaskRunState::Succeeded => "pass",
        AgentTaskRunState::CandidateRecoverable => "fail",
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
    store.upsert_imported_run_preserving_terminal(&homeboy_core::observation::RunRecord {
        id: record.run_id.clone(),
        kind: "agent-task".to_string(),
        component_id: None,
        started_at: record.submitted_at.clone(),
        finished_at: record.updated_at.clone(),
        status: status.to_string(),
        command: Some("homeboy agent-task".to_string()),
        cwd: None,
        homeboy_version: Some(homeboy_core::build_identity::current().display),
        git_sha: None,
        rig_id: None,
        metadata_json: existing_metadata,
    })?;

    validate_unique_terminal_artifact_ids(aggregate)?;
    let mut projection_error = None;
    for outcome in &aggregate.outcomes {
        for artifact in &outcome.artifacts {
            let actionable =
                crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact);
            let remote_runner = record.runner_id().filter(|runner_id| {
                super::lifecycle_ops::execution_runner_id().as_deref() != Some(*runner_id)
            });
            // Only artifacts consumed after terminalization need controller
            // bytes. Other declarations remain review metadata and can name a
            // producer-local path that no longer exists.
            if !requires_durable_lab_projection(artifact) {
                continue;
            }
            if actionable
                && (artifact.size_bytes.is_none()
                    || artifact.sha256.is_none()
                    || (artifact.path.is_none() && remote_runner.is_none()))
            {
                return Err(Error::validation_invalid_argument(
                    "artifact_projection",
                    format!(
                        "actionable patch for run '{}', task '{}', and artifact '{}' requires a local path or authenticated runner binding, plus size and SHA-256, before controller projection",
                        record.run_id, outcome.task_id, artifact.id
                    ),
                    Some(artifact.id.clone()),
                    None,
                ));
            }
            if artifact.path.is_none() && remote_runner.is_none() {
                continue;
            }
            if artifact.size_bytes.is_none() || artifact.sha256.is_none() {
                // Unreadable/remote declarations remain visible to review only.
                continue;
            }
            validate_projection_token("artifact.id", &artifact.id)?;
            validate_projection_token("artifact.kind", &artifact.kind)?;
            let base_id = artifact.id.trim();
            let logical_id = base_id;
            // Observation artifact ids are globally unique. Keep the lifecycle
            // logical id as the per-run lookup token exposed by runs artifact.
            let mut id_hash = sha2::Sha256::new();
            sha2::Digest::update(&mut id_hash, record.run_id.as_bytes());
            sha2::Digest::update(&mut id_hash, [0]);
            sha2::Digest::update(&mut id_hash, outcome.task_id.as_bytes());
            sha2::Digest::update(&mut id_hash, [0]);
            sha2::Digest::update(&mut id_hash, logical_id.as_bytes());
            let artifact_id = format!("agent-task-{:x}", id_hash.finalize());
            let mut metadata = json!({
                "name": logical_id,
                "agent_task": {
                    "task_id": outcome.task_id,
                    "logical_artifact_id": logical_id,
                    "runner_provenance": artifact.metadata,
                }
            });
            if remote_runner.is_none() {
                // A local artifact record is copied into the observation store
                // before it can be reused, so tag that retained controller copy
                // before the idempotency check below.
                metadata["agent_task"]["projection"] = json!("controller_local");
            }
            // Prefer pre-existing executor-finalized bytes over a legacy
            // direct import. The import remains evidence, while the finalized
            // projection keeps its established derived controller identity.
            let finalized_path = remote_runner
                .map(|_| controller_finalized_artifact_path(store, artifact))
                .transpose()?
                .flatten();
            if finalized_path.is_some() {
                stamp_legacy_artifact_provenance(
                    store,
                    &record.run_id,
                    &outcome.task_id,
                    artifact,
                    &artifact_id,
                )?;
            }
            if finalized_path.is_none()
                && reusable_terminal_artifact(
                    &store,
                    &record.run_id,
                    &outcome.task_id,
                    artifact,
                    &artifact_id,
                    &metadata,
                )?
            {
                continue;
            }
            if let Some(runner_id) = remote_runner {
                if runner_id.trim().is_empty() {
                    return Err(Error::validation_invalid_argument(
                        "runner_id",
                        "runner id cannot be empty when creating a runner artifact reference",
                        None,
                        None,
                    ));
                }
                validate_artifact_runner_binding(artifact, runner_id)?;
                if let Some(path) = finalized_path {
                    let mut controller_hash = sha2::Sha256::new();
                    sha2::Digest::update(&mut controller_hash, b"controller");
                    sha2::Digest::update(&mut controller_hash, [0]);
                    sha2::Digest::update(&mut controller_hash, artifact_id.as_bytes());
                    let controller_artifact_id =
                        format!("agent-task-{:x}", controller_hash.finalize());
                    let mut metadata = metadata;
                    metadata["agent_task"]["projection"] = json!("controller_finalized");
                    store.record_verified_artifact_with_id(
                        &record.run_id,
                        &artifact.kind,
                        path,
                        &controller_artifact_id,
                        artifact
                            .size_bytes
                            .and_then(|size| i64::try_from(size).ok()),
                        artifact.sha256.as_deref(),
                        metadata,
                    )?;
                } else {
                    let remote_ref = homeboy_core::execution_contract::EXECUTION_CONTRACT
                        .artifacts
                        .runner_artifact_ref(runner_id, &record.run_id, logical_id);
                    let mirror_result = if requires_durable_lab_projection(artifact) {
                        (|| -> Result<()> {
                            let mirror = tempfile::NamedTempFile::new().map_err(|error| {
                                Error::internal_io(
                                    error.to_string(),
                                    Some("create controller artifact mirror".to_string()),
                                )
                            })?;
                            let download =
                                homeboy_core::observation::runs_service::runner_evidence::with_runner_evidence(
                                    |p| {
                                        p.download_remote_artifact(
                                            &remote_ref,
                                            Some(mirror.path().to_path_buf()),
                                        )
                                    },
                                )
                                .map_err(|error| error.with_retryable(true))?;
                            let expected_size = artifact.size_bytes.expect("checked above");
                            let expected_sha256 =
                                artifact.sha256.as_deref().expect("checked above");
                            let actual_size = std::fs::metadata(&download.output_path)
                                .map_err(|error| {
                                    Error::internal_io(
                                        error.to_string(),
                                        Some("inspect controller artifact mirror".to_string()),
                                    )
                                })?
                                .len();
                            let actual_sha256 = homeboy_core::artifact_metadata::sha256_file(
                                &download.output_path,
                            )?;
                            if actual_size != expected_size || actual_sha256 != expected_sha256 {
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
                            sha2::Digest::update(&mut controller_hash, artifact_id.as_bytes());
                            let controller_artifact_id =
                                format!("agent-task-{:x}", controller_hash.finalize());
                            let mut controller_metadata = metadata.clone();
                            controller_metadata["agent_task"]["projection"] =
                                json!("runner_mirrored");
                            store.record_verified_artifact_with_id(
                                &record.run_id,
                                &artifact.kind,
                                &download.output_path,
                                &controller_artifact_id,
                                Some(expected_size as i64),
                                Some(expected_sha256),
                                controller_metadata,
                            )?;
                            Ok(())
                        })()
                    } else {
                        Ok(())
                    };

                    // Preserve the canonical runner retrieval alias even when
                    // the controller also materializes verified bytes.
                    store.import_artifact(&homeboy_core::observation::ArtifactRecord {
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
            } else {
                let path = artifact.path.as_deref().expect("local path checked above");
                let path = controller_projected_local_artifact_path(
                    store,
                    &record.run_id,
                    &outcome.task_id,
                    &artifact.id,
                    path,
                    artifact.size_bytes.expect("checked above"),
                    artifact.sha256.as_deref().expect("checked above"),
                )?;
                store.record_verified_artifact_with_id(
                    &record.run_id,
                    &artifact.kind,
                    &path,
                    &artifact_id,
                    artifact
                        .size_bytes
                        .and_then(|size| i64::try_from(size).ok()),
                    artifact.sha256.as_deref(),
                    metadata,
                )?;
            }
        }
    }
    projection_error.map_or(Ok(()), Err)
}

/// Copy local producer output under controller ownership before publishing its
/// projection. The aggregate path remains provenance only and may disappear
/// when the producer workspace is cleaned up.
fn controller_projected_local_artifact_path(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact_id: &str,
    source: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<std::path::PathBuf> {
    let bytes = std::fs::read(source).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read local artifact {source}")),
        )
    })?;
    if bytes.len() as u64 != expected_size
        || homeboy_engine_primitives::content_hash::sha256_hex(&bytes) != expected_sha256
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            "local artifact does not match its declared SHA-256 and size",
            Some(artifact_id.to_string()),
            None,
        ));
    }
    let path = store
        .artifact_root()?
        .join("controller-projected-agent-task-artifacts")
        .join(homeboy_core::paths::sanitize_path_segment(run_id))
        .join(homeboy_core::paths::sanitize_path_segment(task_id))
        .join(homeboy_core::paths::sanitize_path_segment(artifact_id));
    std::fs::create_dir_all(path.parent().expect("projection path parent")).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller artifact projection".to_string()),
        )
    })?;
    std::fs::write(&path, bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "write controller artifact projection {}",
                path.display()
            )),
        )
    })?;
    Ok(path)
}

fn requires_durable_lab_projection(artifact: &AgentTaskArtifact) -> bool {
    crate::agent_task_timeout_artifacts::is_actionable_patch_artifact(artifact)
        || matches!(
            artifact.kind.as_str(),
            "transcript" | "result" | "agent-result" | "agent_result"
        )
}

/// A direct artifact import can retain the same deterministic lifecycle id
/// before terminal reconciliation. Reuse it only when its controller-local
/// bytes prove it belongs to this artifact projection.
fn reusable_terminal_artifact(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
    artifact_id: &str,
    metadata: &Value,
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
        && homeboy_core::artifact_metadata::sha256_file(Path::new(&existing.path))
            .ok()
            .as_deref()
            == Some(expected_sha256);
    if matches {
        let existing_task_id = existing
            .metadata_json
            .pointer("/agent_task/task_id")
            .and_then(Value::as_str);
        let existing_logical_id = existing
            .metadata_json
            .pointer("/agent_task/logical_artifact_id")
            .and_then(Value::as_str);
        if existing_task_id.is_some_and(|value| value != task_id)
            || existing_logical_id.is_some_and(|value| value != artifact.id)
        {
            return Err(Error::validation_invalid_argument(
                "artifact_id",
                format!(
                    "existing artifact record conflicts with terminal artifact projection: {artifact_id}"
                ),
                Some(artifact_id.to_string()),
                None,
            ));
        }
        if controller_owned_artifact_path(store, Path::new(&existing.path)) {
            // Pre-marker controller projections already have durable storage;
            // retain their record and stamp the current lookup metadata.
            store.update_artifact_metadata(artifact_id, metadata.clone())?;
            return Ok(true);
        }

        // A directly imported observation artifact is durable controller
        // evidence even when its original path was outside the artifact root.
        // Preserve that record and derive a separate controller-local copy
        // before allowing terminal recovery to use its verified bytes.
        let path = controller_projected_local_artifact_path(
            store,
            run_id,
            task_id,
            &artifact.id,
            &existing.path,
            artifact.size_bytes.expect("checked above"),
            expected_sha256,
        )?;
        let controller_artifact_id = controller_projection_artifact_id(artifact_id);
        store.record_verified_artifact_with_id(
            run_id,
            &artifact.kind,
            path,
            &controller_artifact_id,
            expected_size,
            Some(expected_sha256),
            metadata.clone(),
        )?;
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

fn controller_projection_artifact_id(artifact_id: &str) -> String {
    let mut controller_hash = sha2::Sha256::new();
    sha2::Digest::update(&mut controller_hash, b"controller");
    sha2::Digest::update(&mut controller_hash, [0]);
    sha2::Digest::update(&mut controller_hash, artifact_id.as_bytes());
    format!("agent-task-{:x}", controller_hash.finalize())
}

/// Retain legacy imported evidence while making its lifecycle association
/// explicit. This never grants projection authority to its external path.
fn stamp_legacy_artifact_provenance(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
    artifact_id: &str,
) -> Result<()> {
    let Some(existing) = store.get_artifact(artifact_id)? else {
        return Ok(());
    };
    let expected_size = i64::try_from(artifact.size_bytes.expect("checked above")).ok();
    let expected_sha256 = artifact.sha256.as_deref().expect("checked above");
    if existing.artifact_type != "file"
        || existing.run_id != run_id
        || existing.kind != artifact.kind
        || existing.size_bytes != expected_size
        || existing.sha256.as_deref() != Some(expected_sha256)
        || std::fs::metadata(&existing.path)
            .map(|metadata| {
                metadata.is_file() && i64::try_from(metadata.len()).ok() == expected_size
            })
            .unwrap_or(false)
            == false
        || homeboy_core::artifact_metadata::sha256_file(Path::new(&existing.path))
            .ok()
            .as_deref()
            != Some(expected_sha256)
    {
        return Ok(());
    }
    let mut metadata = existing.metadata_json;
    if !metadata.is_object() {
        metadata = json!({});
    }
    let agent_task = metadata["agent_task"].as_object_mut();
    if agent_task.is_none() {
        metadata["agent_task"] = json!({});
    }
    let agent_task = metadata["agent_task"]
        .as_object_mut()
        .expect("agent task metadata object");
    let existing_task_id = agent_task.get("task_id").and_then(Value::as_str);
    let existing_logical_id = agent_task
        .get("logical_artifact_id")
        .and_then(Value::as_str);
    if existing_task_id.is_some_and(|value| value != task_id)
        || existing_logical_id.is_some_and(|value| value != artifact.id)
    {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!("existing artifact record conflicts with terminal artifact projection: {artifact_id}"),
            Some(artifact_id.to_string()),
            None,
        ));
    }
    agent_task.insert("task_id".to_string(), json!(task_id));
    agent_task.insert("logical_artifact_id".to_string(), json!(artifact.id));
    store.update_artifact_metadata(artifact_id, metadata)?;
    Ok(())
}

/// Observation artifacts are controller-owned only when their resolved file is
/// beneath the controller artifact root. A matching digest alone must not turn
/// an ephemeral producer path into durable lifecycle input.
fn controller_owned_artifact_path(
    store: &homeboy_core::observation::ObservationStore,
    path: &Path,
) -> bool {
    let Ok(root) = store.artifact_root().and_then(|root| {
        std::fs::canonicalize(root).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("resolve controller artifact root".to_string()),
            )
        })
    }) else {
        return false;
    };
    controller_owned_artifact_path_at(path, &root)
}

fn controller_owned_artifact_path_at(path: &Path, root: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(root) else {
        return false;
    };
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    path.starts_with(root)
}

/// Find finalized bytes already copied into the controller artifact root. Lab
/// aggregate paths describe runner provenance and are never read after recovery.
fn controller_finalized_artifact_path(
    store: &homeboy_core::observation::ObservationStore,
    artifact: &AgentTaskArtifact,
) -> Result<Option<PathBuf>> {
    let Some(expected_sha256) = artifact.sha256.as_deref() else {
        return Ok(None);
    };
    let Some(expected_size) = artifact.size_bytes else {
        return Ok(None);
    };
    let root = store.artifact_root()?.join("executor-finalized");
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
            && homeboy_core::artifact_metadata::sha256_file(&path)? == expected_sha256
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
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    verified_controller_artifact_projection_path_in_store(&store, run_id, task_id, artifact)
}

pub(crate) fn verified_controller_artifact_projection_path_in_store(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    artifact: &AgentTaskArtifact,
) -> Result<Option<PathBuf>> {
    let artifact_root = store.artifact_root()?;
    let Some(expected_sha256) = artifact.sha256.as_deref() else {
        return Ok(None);
    };
    let Some(expected_size) = artifact
        .size_bytes
        .and_then(|size| i64::try_from(size).ok())
    else {
        return Ok(None);
    };
    let mut candidates: Vec<_> = store
        .list_artifacts(run_id)?
        .into_iter()
        .filter(|candidate| {
            candidate.artifact_type == "file"
                && candidate.kind == artifact.kind
                && (matches!(
                    candidate
                        .metadata_json
                        .pointer("/agent_task/projection")
                        .and_then(serde_json::Value::as_str),
                    Some("controller_local" | "controller_finalized" | "runner_mirrored")
                ) || controller_owned_artifact_path_at(
                    Path::new(&candidate.path),
                    &artifact_root,
                ))
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
        })
        .collect();
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
    let mut candidate = candidates.pop().expect("one candidate checked above");
    let path = PathBuf::from(&candidate.path);
    let actual_size = std::fs::metadata(&path)
        .ok()
        .and_then(|metadata| i64::try_from(metadata.len()).ok());
    let actual_sha256 = homeboy_core::artifact_metadata::sha256_file(&path).ok();
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
    if !controller_owned_artifact_path_at(&path, &artifact_root) {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "controller-side artifact projection for run '{run_id}', task '{task_id}', and artifact '{}' is not stored under controller ownership",
                artifact.id
            ),
            Some(artifact.id.clone()),
            None,
        ));
    }
    if !matches!(
        candidate
            .metadata_json
            .pointer("/agent_task/projection")
            .and_then(serde_json::Value::as_str),
        Some("controller_local" | "controller_finalized" | "runner_mirrored")
    ) {
        candidate.metadata_json["agent_task"]["projection"] = json!("controller_local");
        store.update_artifact_metadata(&candidate.id, candidate.metadata_json)?;
    }
    Ok(Some(path))
}

/// Resolve a controller-retained artifact by its durable logical identity. This
/// is intentionally independent of the runner-reported path: Lab workspaces
/// are disposable after their aggregate has been mirrored.
pub fn verified_controller_artifact_projection(
    run_id: &str,
    task_id: &str,
    logical_artifact_id: &str,
    kind: &str,
    expected_sha256: &str,
    expected_record_id: Option<&str>,
) -> Result<Option<(String, Vec<u8>)>> {
    let store = homeboy_core::observation::ObservationStore::open_initialized()?;
    verified_controller_artifact_projection_in_store(
        &store,
        run_id,
        task_id,
        logical_artifact_id,
        kind,
        expected_sha256,
        expected_record_id,
    )
}

pub(crate) fn verified_controller_artifact_projection_in_store(
    store: &homeboy_core::observation::ObservationStore,
    run_id: &str,
    task_id: &str,
    logical_artifact_id: &str,
    kind: &str,
    expected_sha256: &str,
    expected_record_id: Option<&str>,
) -> Result<Option<(String, Vec<u8>)>> {
    let artifact_root = store.artifact_root()?;
    let candidates: Vec<_> = store
        .list_artifacts(run_id)?
        .into_iter()
        .filter(|candidate| {
            candidate.artifact_type == "file"
                && candidate.kind == kind
                && candidate
                    .metadata_json
                    .pointer("/agent_task/task_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(task_id)
                && candidate
                    .metadata_json
                    .pointer("/agent_task/logical_artifact_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(logical_artifact_id)
        })
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }
    if candidates.len() != 1 {
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "multiple controller-side artifact projections match run '{run_id}', task '{task_id}', and artifact '{logical_artifact_id}'"
            ),
            Some(logical_artifact_id.to_string()),
            None,
        ));
    }
    let candidate = &candidates[0];
    if expected_record_id.is_some_and(|id| id != candidate.id) {
        return Err(Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            format!(
                "controller artifact identity mismatch for run '{run_id}', task '{task_id}', and artifact '{logical_artifact_id}': expected record '{}', found '{}'",
                expected_record_id.unwrap_or_default(), candidate.id
            ),
            Some(logical_artifact_id.to_string()),
            None,
        ));
    }
    let path = PathBuf::from(&candidate.path);
    if !controller_owned_artifact_path_at(&path, &artifact_root) {
        return Err(Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            "controller artifact mirror is outside the owning artifact root",
            Some(path.display().to_string()),
            None,
        ));
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            format!(
                "controller artifact mirror is unavailable for run '{run_id}', task '{task_id}', and artifact '{logical_artifact_id}': {}",
                error
            ),
            Some(logical_artifact_id.to_string()),
            None,
        )
    })?;
    let actual_sha256 = content_hash::sha256_hex(&bytes);
    if candidate.sha256.as_deref() != Some(expected_sha256)
        || candidate.size_bytes != Some(bytes.len() as i64)
        || actual_sha256 != expected_sha256
    {
        return Err(Error::validation_invalid_argument(
            "gate_feedback_candidate_baseline",
            format!(
                "controller artifact mirror hash mismatch for run '{run_id}', task '{task_id}', and artifact '{logical_artifact_id}': expected '{expected_sha256}', record '{:?}', bytes '{actual_sha256}'",
                candidate.sha256
            ),
            Some(logical_artifact_id.to_string()),
            None,
        ));
    }
    Ok(Some((candidate.id.clone(), bytes)))
}

/// Runner artifact retrieval is keyed only by artifact id. Reject duplicates
/// rather than inventing a controller-only alias that cannot be retried safely.
fn validate_unique_terminal_artifact_ids(aggregate: &AgentTaskAggregate) -> Result<()> {
    let mut identities = std::collections::BTreeMap::new();
    for outcome in &aggregate.outcomes {
        for artifact in &outcome.artifacts {
            validate_projection_token("artifact.id", &artifact.id)?;
            let Some(previous_task_id) = identities.insert(&artifact.id, &outcome.task_id) else {
                continue;
            };
            return Err(Error::validation_invalid_argument(
                "artifact.id",
                format!(
                    "terminal aggregate reuses artifact id '{}' for tasks '{}' and '{}'; runner artifact ids must be unique",
                    artifact.id, previous_task_id, outcome.task_id
                ),
                Some(artifact.id.clone()),
                None,
            ));
        }
    }
    Ok(())
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
    fn artifact_runner_provenance_must_match_the_lifecycle_binding() {
        let artifact = artifact("patch", "patch", Some("runner-a"));

        validate_artifact_runner_binding(&artifact, "runner-a").expect("matching runner binding");
        let error = validate_artifact_runner_binding(&artifact, "runner-b")
            .expect_err("conflicting runner identity must fail closed");

        assert!(error
            .message
            .contains("conflicts with lifecycle runner binding"));
    }

    #[test]
    fn reusable_artifact_rejects_conflicting_persisted_logical_identity() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let store = homeboy_core::observation::ObservationStore::open_initialized()
                .expect("observation store");
            let run = store
                .start_run(
                    homeboy_core::observation::NewRunRecord::builder("agent-task")
                        .cwd_path(home.path())
                        .build(),
                )
                .expect("run");
            let path = home.path().join("patch.diff");
            let bytes = b"patch";
            std::fs::write(&path, bytes).expect("patch bytes");
            let mut artifact = artifact("patch", "patch", None);
            artifact.size_bytes = Some(bytes.len() as u64);
            artifact.sha256 = Some(format!("{:x}", sha2::Sha256::digest(bytes)));
            store
                .record_artifact_with_id(
                    &run.id,
                    "patch",
                    &path,
                    "stable-patch",
                    json!({
                        "agent_task": {
                            "task_id": "other-task",
                            "logical_artifact_id": "patch",
                        }
                    }),
                )
                .expect("imported artifact");

            let error = reusable_terminal_artifact(
                &store,
                &run.id,
                "task",
                &artifact,
                "stable-patch",
                &json!({
                    "agent_task": {
                        "task_id": "task",
                        "logical_artifact_id": "patch",
                    }
                }),
            )
            .expect_err("conflicting logical identity must fail closed");

            assert!(error
                .message
                .contains("conflicts with terminal artifact projection"));
        });
    }

    #[test]
    fn controller_local_projection_isolates_identical_ids_across_artifact_roots() {
        let left_context = homeboy_core::test_support::HermeticTestContext::new();
        let right_context = homeboy_core::test_support::HermeticTestContext::new();
        let left_store = AgentTaskLifecycleStore::new(left_context.path_roots())
            .open_observation_initialized()
            .expect("left observation store");
        let right_store = AgentTaskLifecycleStore::new(right_context.path_roots())
            .open_observation_initialized()
            .expect("right observation store");
        let left_source = left_context.root().join("source.patch");
        let right_source = right_context.root().join("source.patch");
        std::fs::write(&left_source, b"left").expect("left source");
        std::fs::write(&right_source, b"right").expect("right source");

        let left = controller_projected_local_artifact_path(
            &left_store,
            "same-run",
            "same-task",
            "same-artifact",
            left_source.to_str().unwrap(),
            4,
            &format!("{:x}", sha2::Sha256::digest(b"left")),
        )
        .expect("left projection");
        let right = controller_projected_local_artifact_path(
            &right_store,
            "same-run",
            "same-task",
            "same-artifact",
            right_source.to_str().unwrap(),
            5,
            &format!("{:x}", sha2::Sha256::digest(b"right")),
        )
        .expect("right projection");

        assert!(left.starts_with(left_store.artifact_root().unwrap()));
        assert!(right.starts_with(right_store.artifact_root().unwrap()));
        assert_ne!(left, right);
        assert_eq!(std::fs::read(left).unwrap(), b"left");
        assert_eq!(std::fs::read(right).unwrap(), b"right");
    }
}
