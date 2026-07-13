use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::agent_task::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskFailureClassification,
    AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest, AgentTaskTypedArtifact,
    AGENT_TASK_OUTCOME_SCHEMA,
};
pub use crate::core::agent_task_schedule::{
    AgentTaskAdaptiveConcurrencyAction, AgentTaskAdaptiveConcurrencyDecision,
    AgentTaskAdaptiveConcurrencyInputs, AgentTaskAdaptiveConcurrencyPolicy,
    AgentTaskAdaptiveConcurrencyStatus, AgentTaskAggregate, AgentTaskAggregateStatus,
    AgentTaskAggregateTotals, AgentTaskArtifactBinding, AgentTaskArtifactLineage,
    AgentTaskArtifactOutputDeclaration, AgentTaskArtifactRunBinding, AgentTaskBackpressureStatus,
    AgentTaskCancellationToken, AgentTaskChildRun, AgentTaskExecutionContext,
    AgentTaskOutputBinding, AgentTaskOutputDependencies, AgentTaskPlan, AgentTaskProgressEvent,
    AgentTaskProviderRotationAttempt, AgentTaskProviderRotationEntry,
    AgentTaskProviderRotationPolicy, AgentTaskQueueStatus, AgentTaskResourceBudget,
    AgentTaskResourceBudgetStatus, AgentTaskRetryPolicy, AgentTaskScheduleOptions, AgentTaskState,
    AGENT_TASK_AGGREGATE_SCHEMA, AGENT_TASK_PLAN_SCHEMA,
};
use crate::core::agent_task_timeout::timeout_with_grace;
use crate::core::agent_task_timeout_artifacts::{
    append_unique_artifacts, append_unique_evidence_refs, is_actionable_patch_artifact,
    is_empty_patch_artifact, merge_timeout_outcome, TimeoutArtifactDiscovery,
};
use crate::core::config::value_type_name;

/// Authoritative execution adapter consumed by the agent-task scheduler.
///
/// Provider lifecycle payloads live with the agent-task schemas, but execution
/// dispatch goes through this single adapter shape so provider selection,
/// outcome normalization, timeouts, and cancellation do not drift across seams.
pub trait AgentTaskExecutorAdapter: Send + Sync + 'static {
    fn execute(
        &self,
        request: AgentTaskRequest,
        context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome;

    fn cancel(&self, _task_id: &str) {}
}

pub struct AgentTaskScheduler<E> {
    executor: Arc<E>,
    run_id: Option<String>,
}

impl<E> AgentTaskScheduler<E>
where
    E: AgentTaskExecutorAdapter,
{
    pub fn new(executor: E) -> Self {
        Self {
            executor: Arc::new(executor),
            run_id: None,
        }
    }

    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    pub fn run(&self, plan: AgentTaskPlan) -> AgentTaskAggregate {
        self.run_with_cancellation(plan, AgentTaskCancellationToken::default())
    }

    pub(crate) fn run_with_cancellation(
        &self,
        plan: AgentTaskPlan,
        cancellation: AgentTaskCancellationToken,
    ) -> AgentTaskAggregate {
        let mut plan = plan.canonicalize();
        let max_concurrency = plan.options.max_concurrency.max(1);
        let total_tasks = plan.tasks.len();
        let max_queue_depth = plan.options.max_queue_depth.or(plan.options.max_tasks);
        let retry_budget_total = plan.options.retry.max_retries_total;
        let output_dependencies = plan.output_dependencies.clone();
        let mut retry_budget_used = 0;
        let mut backpressure = Vec::new();
        let mut blocked_count = 0;
        let accepted_tasks = max_queue_depth
            .map(|max_queue_depth| max_queue_depth.min(plan.tasks.len()))
            .unwrap_or(plan.tasks.len());
        let blocked_tasks = if accepted_tasks < plan.tasks.len() {
            plan.tasks.split_off(accepted_tasks)
        } else {
            Vec::new()
        };
        let plan_rotation = plan.options.rotation.clone();
        let mut queued: VecDeque<ScheduledTask> = plan
            .tasks
            .drain(..)
            .map(|mut request| {
                if let Some(policy) = plan_rotation.as_ref() {
                    AgentTaskScheduleSupport::apply_rotation_policy_limits(&mut request, policy);
                }
                ScheduledTask {
                    workspace_key: AgentTaskScheduleSupport::workspace_key(&request),
                    request,
                    attempt: 1,
                    rotation_index: 0,
                    rotation_attempts: Vec::new(),
                }
            })
            .collect();
        let mut running: Vec<RunningTask> = Vec::new();
        let mut quarantined: Vec<QuarantinedTask> = Vec::new();
        let mut outcomes = Vec::new();
        let mut completed_by_task: HashMap<String, AgentTaskOutcome> = HashMap::new();
        let mut events = Vec::new();
        let mut cancellation_notified = false;
        let max_attempts = plan.options.retry.max_attempts.max(1);
        let (tx, rx) = mpsc::channel();
        let cancellation_tx = tx.clone();
        cancellation.on_cancel(Arc::new(move || {
            let _ = cancellation_tx.send(SchedulerEvent::Cancellation);
        }));
        let mut adaptive_decisions = Vec::new();
        let mut last_effective_concurrency = None;

        for task in &queued {
            events.push(event(
                &task.request.task_id,
                AgentTaskState::Queued,
                1,
                None,
            ));
        }

        for request in blocked_tasks {
            blocked_count += 1;
            let message = format!(
                "task blocked by max_queue_depth={}",
                max_queue_depth.unwrap_or_default()
            );
            backpressure.push(AgentTaskBackpressureStatus {
                kind: "queue_depth".to_string(),
                message: message.clone(),
                task_id: Some(request.task_id.clone()),
            });
            events.push(event(
                &request.task_id,
                AgentTaskState::Blocked,
                1,
                Some(message.clone()),
            ));
            outcomes.push(AgentTaskScheduleSupport::blocked_outcome(
                request.task_id,
                message,
            ));
        }

        while !queued.is_empty() || !running.is_empty() {
            if cancellation.is_cancelled() {
                AgentTaskScheduleSupport::cancel_queued(&mut queued, &mut outcomes, &mut events);
                if !cancellation_notified {
                    for task in &running {
                        self.executor.cancel(&task.task_id);
                    }
                    cancellation_notified = true;
                }
            }

            let adaptive_decision = adaptive_concurrency_decision(
                plan.options.adaptive_concurrency.as_ref(),
                max_concurrency,
                queued.len(),
                running.len(),
                &plan.options.resource_budget,
                active_resource_units(&running),
                last_effective_concurrency,
            );
            let effective_concurrency = adaptive_decision
                .as_ref()
                .map(|decision| decision.effective_concurrency)
                .unwrap_or(max_concurrency);
            if let Some(decision) = adaptive_decision {
                last_effective_concurrency = Some(decision.effective_concurrency);
                if adaptive_decisions
                    .last()
                    .map(|previous: &AgentTaskAdaptiveConcurrencyDecision| {
                        previous.action != decision.action
                            || previous.effective_concurrency != decision.effective_concurrency
                            || previous.reason != decision.reason
                    })
                    .unwrap_or(true)
                {
                    adaptive_decisions.push(decision);
                }
            }

            if effective_concurrency == 0 && running.is_empty() && !queued.is_empty() {
                while let Some(task) = queued.pop_front() {
                    let message = "adaptive concurrency paused dispatch".to_string();
                    AgentTaskScheduleSupport::block_and_record_scheduled_task(
                        &task,
                        "adaptive_concurrency",
                        message,
                        &mut backpressure,
                        &mut events,
                        &mut outcomes,
                        &mut blocked_count,
                    );
                }
                break;
            }

            while running.len() < effective_concurrency
                && !queued.is_empty()
                && !cancellation.is_cancelled()
            {
                let Some(next_index) = AgentTaskScheduleSupport::next_dispatchable_index(
                    &queued,
                    &running,
                    &quarantined,
                    &completed_by_task,
                    &output_dependencies,
                    &plan.options.per_executor_concurrency,
                    &plan.options.per_model_concurrency,
                    &plan.options.resource_budget,
                ) else {
                    if running.is_empty() {
                        if let Some(task) = queued.pop_front() {
                            if AgentTaskScheduleSupport::workspace_is_quarantined(
                                &task,
                                &quarantined,
                            ) {
                                AgentTaskScheduleSupport::block_and_record_scheduled_task(
                                    &task,
                                    "workspace_quarantined",
                                    "task workspace remains quarantined after a timed-out executor"
                                        .to_string(),
                                    &mut backpressure,
                                    &mut events,
                                    &mut outcomes,
                                    &mut blocked_count,
                                );
                                continue;
                            }
                            if let Some(message) =
                                AgentTaskScheduleSupport::waiting_for_task_dependencies(
                                    &task,
                                    &completed_by_task,
                                    &output_dependencies,
                                )
                            {
                                let outcome = AgentTaskScheduleSupport::block_scheduled_task(
                                    &task,
                                    "output_dependency",
                                    message,
                                    &mut backpressure,
                                    &mut events,
                                );
                                record_completed_outcome(
                                    &mut completed_by_task,
                                    &mut outcomes,
                                    outcome,
                                );
                                blocked_count += 1;
                                continue;
                            }

                            let task_units =
                                task_resource_units(&task.request, &plan.options.resource_budget);
                            let max_active_units = plan
                                .options
                                .resource_budget
                                .max_active_units
                                .unwrap_or_default();
                            let message = format!(
                                "task requires resource_units={task_units} over max_active_units={max_active_units}"
                            );
                            AgentTaskScheduleSupport::block_and_record_scheduled_task(
                                &task,
                                "resource_budget",
                                message,
                                &mut backpressure,
                                &mut events,
                                &mut outcomes,
                                &mut blocked_count,
                            );
                            continue;
                        }
                        break;
                    }
                    let dependency_wait = queued.front().and_then(|task| {
                        AgentTaskScheduleSupport::waiting_for_task_dependencies(
                            task,
                            &completed_by_task,
                            &output_dependencies,
                        )
                    });
                    backpressure.push(AgentTaskBackpressureStatus {
                        kind: if dependency_wait.is_some() {
                            "output_dependency".to_string()
                        } else {
                            AgentTaskScheduleSupport::backpressure_kind(
                                &queued,
                                &running,
                                &quarantined,
                                &plan.options.per_executor_concurrency,
                                &plan.options.per_model_concurrency,
                                &plan.options.resource_budget,
                            )
                            .to_string()
                        },
                        message: dependency_wait.unwrap_or_else(|| {
                            "queued tasks are waiting for scheduler capacity".to_string()
                        }),
                        task_id: queued.front().map(|task| task.request.task_id.clone()),
                    });
                    break;
                };
                let scheduled = queued.remove(next_index).expect("queued task");
                let mut request = scheduled.request;
                if let Err(outcome) = AgentTaskScheduleSupport::render_output_dependencies(
                    &mut request,
                    &completed_by_task,
                    &output_dependencies,
                ) {
                    events.push(event(
                        &outcome.task_id,
                        AgentTaskState::Skipped,
                        scheduled.attempt,
                        outcome.summary.clone(),
                    ));
                    record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                    continue;
                }
                let task_id = request.task_id.clone();
                let task_base_sha = match prepare_committed_harvest(&request) {
                    Ok(base) => base,
                    Err(error) => {
                        let outcome = committed_harvest_failure(
                            committed_harvest_preflight_outcome(task_id.clone()),
                            error,
                        );
                        events.push(event(
                            &task_id,
                            AgentTaskState::Failed,
                            scheduled.attempt,
                            outcome.summary.clone(),
                        ));
                        record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                        continue;
                    }
                };
                let executor_key = executor_key(&request);
                let executor = Arc::clone(&self.executor);
                let plan_id = plan.plan_id.clone();
                let task_timeout_ms =
                    crate::core::agent_task_timeout::effective_provider_timeout_ms(
                        request.limits.timeout_ms.or(plan.options.timeout_ms),
                        request.limits.max_runtime_ms,
                    );
                let tx = tx.clone();
                let attempt = scheduled.attempt;
                let context = AgentTaskExecutionContext {
                    plan_id,
                    run_id: self.run_id.clone(),
                    attempt,
                    cancellation: cancellation.clone(),
                };

                events.push(event(&task_id, AgentTaskState::Running, attempt, None));
                running.push(RunningTask {
                    task_id: task_id.clone(),
                    request: request.clone(),
                    workspace_key: scheduled.workspace_key,
                    executor_key,
                    model_key: model_key(&request),
                    resource_units: task_resource_units(&request, &plan.options.resource_budget),
                    attempt,
                    started_at: Instant::now(),
                    timeout_ms: Some(task_timeout_ms),
                    rotation_index: scheduled.rotation_index,
                    rotation_attempts: scheduled.rotation_attempts,
                    task_base_sha,
                });

                thread::spawn(move || {
                    let outcome = executor.execute(request, context);
                    let _ = tx.send(SchedulerEvent::TaskResult(TaskResult {
                        task_id,
                        attempt,
                        outcome,
                    }));
                });
            }

            AgentTaskScheduleSupport::expire_timed_out_tasks(
                &mut running,
                &mut quarantined,
                &mut outcomes,
                &mut events,
                self.executor.as_ref(),
            );

            if running.is_empty() {
                continue;
            }

            let wait_timeout = running
                .iter()
                .filter_map(|task| {
                    task.timeout_ms
                        .map(|ms| timeout_with_grace(ms).saturating_sub(task.started_at.elapsed()))
                })
                .min();
            match wait_timeout.map_or_else(
                || rx.recv().map_err(|_| None),
                |timeout| rx.recv_timeout(timeout).map_err(Some),
            ) {
                Ok(SchedulerEvent::Cancellation) => {
                    continue;
                }
                Ok(SchedulerEvent::TaskResult(result)) => {
                    if cancellation.is_cancelled() {
                        AgentTaskScheduleSupport::cancel_queued(
                            &mut queued,
                            &mut outcomes,
                            &mut events,
                        );
                    }
                    let running_task =
                        AgentTaskScheduleSupport::remove_running(&mut running, &result.task_id);
                    let Some(running_task) = running_task else {
                        continue;
                    };
                    let mut outcome = result.outcome;
                    if let Err(error) = harvest_committed_patch(&mut outcome, &running_task) {
                        outcome = committed_harvest_failure(outcome, error);
                    }
                    let outcome =
                        AgentTaskScheduleSupport::normalize_outcome(outcome, Some(&running_task));
                    let state = AgentTaskScheduleSupport::state_for_outcome(&outcome);
                    events.push(event(
                        &outcome.task_id,
                        state,
                        result.attempt,
                        outcome.summary.clone(),
                    ));
                    if AgentTaskScheduleSupport::should_retry(
                        &outcome,
                        result.attempt,
                        max_attempts,
                        retry_budget_total,
                        retry_budget_used,
                        &plan.options.retry.retryable_failure_classifications,
                    ) {
                        retry_budget_used += 1;
                        let mut request = running_task.request;
                        request.parent_plan_id = Some(plan.plan_id.clone());
                        let next_attempt = result.attempt + 1;
                        events.push(event(
                            &request.task_id,
                            AgentTaskState::Queued,
                            next_attempt,
                            Some("retry queued".to_string()),
                        ));
                        queued.push_back(ScheduledTask {
                            workspace_key: AgentTaskScheduleSupport::workspace_key(&request),
                            request,
                            attempt: next_attempt,
                            rotation_index: running_task.rotation_index,
                            rotation_attempts: running_task.rotation_attempts,
                        });
                        continue;
                    }
                    let rotation_policy = AgentTaskScheduleSupport::rotation_policy_for_request(
                        &running_task.request,
                        plan.options.rotation.as_ref(),
                    );
                    if let Some(policy) = &rotation_policy {
                        if AgentTaskScheduleSupport::should_rotate_provider(
                            &outcome,
                            policy,
                            running_task.rotation_index,
                            result.attempt,
                        ) {
                            let mut rotation_attempts = running_task.rotation_attempts;
                            rotation_attempts.push(
                                AgentTaskScheduleSupport::rotation_attempt_record(
                                    &running_task.request,
                                    &outcome,
                                    result.attempt,
                                    running_task.rotation_index,
                                ),
                            );
                            let entry = &policy.entries[running_task.rotation_index];
                            let mut request = running_task.request;
                            AgentTaskScheduleSupport::apply_rotation_entry(
                                &mut request,
                                entry,
                                policy,
                            );
                            request.parent_plan_id = Some(plan.plan_id.clone());
                            let next_attempt = result.attempt + 1;
                            events.push(event(
                                &request.task_id,
                                AgentTaskState::Queued,
                                next_attempt,
                                Some(format!(
                                    "provider rotation queued: entry {} of {}",
                                    running_task.rotation_index + 1,
                                    policy.entries.len()
                                )),
                            ));
                            queued.push_back(ScheduledTask {
                                workspace_key: AgentTaskScheduleSupport::workspace_key(&request),
                                request,
                                attempt: next_attempt,
                                rotation_index: running_task.rotation_index + 1,
                                rotation_attempts,
                            });
                            continue;
                        }
                    }
                    let mut outcome = outcome;
                    if !running_task.rotation_attempts.is_empty() {
                        let mut rotation_attempts = running_task.rotation_attempts.clone();
                        rotation_attempts.push(AgentTaskScheduleSupport::rotation_attempt_record(
                            &running_task.request,
                            &outcome,
                            result.attempt,
                            running_task.rotation_index,
                        ));
                        AgentTaskScheduleSupport::attach_rotation_evidence(
                            &mut outcome,
                            &rotation_attempts,
                        );
                    }
                    record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                }
                Err(Some(mpsc::RecvTimeoutError::Timeout)) => {}
                Err(Some(mpsc::RecvTimeoutError::Disconnected)) | Err(None) => break,
            }
        }

        let artifact_lineage =
            AgentTaskScheduleSupport::artifact_lineage(&outcomes, &plan.artifact_outputs);
        let child_runs = child_runs_for_outcomes(&outcomes);
        let artifact_bindings = artifact_bindings_for_outcomes(&outcomes);

        AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id,
            status: AgentTaskScheduleSupport::aggregate_status(&outcomes),
            totals: AgentTaskScheduleSupport::totals(total_tasks, &outcomes),
            queue: AgentTaskScheduleSupport::queue_status(
                max_concurrency,
                plan.options.max_tasks,
                plan.options.max_queue_depth,
                blocked_count,
                &outcomes,
                &plan.options.per_executor_concurrency,
                &plan.options.per_model_concurrency,
                &plan.options.resource_budget,
                plan.options.adaptive_concurrency.as_ref(),
                &adaptive_decisions,
                &backpressure,
                retry_budget_total.map(|budget| budget.saturating_sub(retry_budget_used)),
            ),
            outcomes,
            events,
            artifact_lineage,
            child_runs,
            artifact_bindings,
        }
    }
}

#[derive(Debug, Clone)]
struct ScheduledTask {
    request: AgentTaskRequest,
    workspace_key: Option<String>,
    attempt: u32,
    /// Rotation entries already consumed for this task (0 = original executor).
    rotation_index: usize,
    /// Ordered evidence for prior dispatch attempts under a rotation policy.
    rotation_attempts: Vec<AgentTaskProviderRotationAttempt>,
}

#[derive(Debug, Clone)]
struct RunningTask {
    task_id: String,
    request: AgentTaskRequest,
    workspace_key: Option<String>,
    executor_key: String,
    model_key: Option<String>,
    resource_units: u32,
    attempt: u32,
    started_at: Instant,
    timeout_ms: Option<u64>,
    /// Rotation entries already consumed for this task (0 = original executor).
    rotation_index: usize,
    /// Ordered evidence for prior dispatch attempts under a rotation policy.
    rotation_attempts: Vec<AgentTaskProviderRotationAttempt>,
    /// Workspace HEAD captured immediately before the executor runs. It bounds
    /// any committed patch candidate to this dispatch attempt.
    task_base_sha: Option<String>,
}

struct QuarantinedTask {
    workspace_key: Option<String>,
}

struct TaskResult {
    task_id: String,
    attempt: u32,
    outcome: AgentTaskOutcome,
}

enum SchedulerEvent {
    TaskResult(TaskResult),
    Cancellation,
}

fn prepare_committed_harvest(request: &AgentTaskRequest) -> Result<Option<String>, HarvestError> {
    let Some(root) = request.workspace.root.as_deref().map(Path::new) else {
        return Ok(None);
    };
    if !git_is_repository(root)? {
        return Ok(None);
    }
    let status = git_output(root, &["status", "--porcelain", "--untracked-files=all"])?;
    if !status.trim().is_empty() {
        return Err(HarvestError::DirtyWorkspace { status });
    }
    Ok(Some(git_output(root, &["rev-parse", "HEAD"])?))
}

fn harvest_committed_patch(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
) -> Result<(), HarvestError> {
    harvest_committed_patch_with_metadata(outcome, running, committed_change_metadata_for_range)
}

fn harvest_committed_patch_with_metadata(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
    collect_metadata: impl FnOnce(&Path, &str) -> Result<Vec<serde_json::Value>, HarvestError>,
) -> Result<(), HarvestError> {
    if outcome.status != AgentTaskOutcomeStatus::Succeeded
        || outcome.artifacts.iter().any(|artifact| {
            is_actionable_patch_artifact(artifact) || is_empty_patch_artifact(artifact)
        })
    {
        return Ok(());
    }
    let Some(base) = running.task_base_sha.as_deref() else {
        return Ok(());
    };
    let Some(root) = running.request.workspace.root.as_deref().map(Path::new) else {
        return Ok(());
    };
    let head = git_output(root, &["rev-parse", "HEAD"])?;
    if head == base {
        return Ok(());
    }
    if !git_is_ancestor(root, base, "HEAD")? {
        return Err(HarvestError::UnrelatedHead {
            base: base.to_string(),
            head,
        });
    }
    let patch = git_output(
        root,
        &[
            "diff",
            "--binary",
            "--full-index",
            "--find-renames",
            base,
            "HEAD",
        ],
    )?;
    if patch.trim().is_empty() {
        return Ok(());
    }
    let path = committed_patch_path(root, base, &head)?;
    std::fs::write(&path, patch.as_bytes()).map_err(|error| HarvestError::ArtifactWrite {
        path: path.clone(),
        message: error.to_string(),
    })?;
    let range = format!("{base}..HEAD");
    // Retain the patch before collecting optional commit metadata so a later
    // Git failure cannot strand the recoverable artifact.
    outcome.artifacts.push(committed_patch_artifact(
        &path,
        &patch,
        base,
        &range,
        Vec::new(),
    ));
    let commits = collect_metadata(root, &range)?;
    outcome
        .artifacts
        .last_mut()
        .expect("committed patch artifact was attached")
        .metadata = serde_json::json!({
        "change_source": "local_commits",
        "base_ref": base,
        "commit_range": range,
        "commits": commits,
    });
    Ok(())
}

fn committed_change_metadata_for_range(
    cwd: &Path,
    range: &str,
) -> Result<Vec<serde_json::Value>, HarvestError> {
    let output = git_output(
        cwd,
        &["log", "--reverse", "--format=%H%x1f%an%x1f%ae%x1f%s", range],
    )?;
    Ok(committed_change_metadata(&output))
}

fn committed_patch_artifact(
    path: &Path,
    patch: &str,
    base: &str,
    range: &str,
    commits: Vec<serde_json::Value>,
) -> AgentTaskArtifact {
    AgentTaskArtifact {
        schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "committed-changes".to_string(),
        kind: "patch".to_string(),
        name: Some("committed-changes.patch".to_string()),
        label: Some("executor committed changes".to_string()),
        role: Some("patch".to_string()),
        semantic_key: None,
        path: Some(path.display().to_string()),
        url: None,
        mime: Some("text/x-patch".to_string()),
        size_bytes: Some(patch.len() as u64),
        sha256: None,
        metadata: serde_json::json!({
            "change_source": "local_commits",
            "base_ref": base,
            "commit_range": range,
            "commits": commits,
        }),
    }
}

fn committed_patch_path(root: &Path, base: &str, head: &str) -> Result<PathBuf, HarvestError> {
    let git_dir = git_output(root, &["rev-parse", "--git-dir"])?;
    let git_dir = PathBuf::from(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        root.join(git_dir)
    };
    let dir = git_dir.join("homeboy-agent-task");
    std::fs::create_dir_all(&dir).map_err(|error| HarvestError::ArtifactDirectory {
        path: dir.clone(),
        message: error.to_string(),
    })?;
    Ok(dir.join(format!("committed-changes-{base}-{head}.patch")))
}

fn committed_change_metadata(output: &str) -> Vec<serde_json::Value> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\u{1f}');
            Some(serde_json::json!({
                "sha": fields.next()?,
                "author_name": fields.next()?,
                "author_email": fields.next()?,
                "subject": fields.next()?,
            }))
        })
        .collect()
}

fn git_is_ancestor(cwd: &Path, base: &str, head: &str) -> Result<bool, HarvestError> {
    let output = Command::new("git")
        .args(["merge-base", "--is-ancestor", base, head])
        .current_dir(cwd)
        .output()
        .map_err(|error| HarvestError::Git {
            command: format!("git merge-base --is-ancestor {base} {head}"),
            message: error.to_string(),
        })?;
    Ok(output.status.success())
}

fn git_is_repository(cwd: &Path) -> Result<bool, HarvestError> {
    let output = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .map_err(|error| HarvestError::Git {
            command: "git rev-parse --is-inside-work-tree".to_string(),
            message: error.to_string(),
        })?;
    Ok(output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true")
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String, HarvestError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| HarvestError::Git {
            command: format!("git {}", args.join(" ")),
            message: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(HarvestError::Git {
            command: format!("git {}", args.join(" ")),
            message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

enum HarvestError {
    DirtyWorkspace { status: String },
    UnrelatedHead { base: String, head: String },
    Git { command: String, message: String },
    ArtifactDirectory { path: PathBuf, message: String },
    ArtifactWrite { path: PathBuf, message: String },
}

fn committed_harvest_preflight_outcome(task_id: String) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: crate::core::agent_task::AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status: AgentTaskOutcomeStatus::Failed,
        summary: None,
        failure_classification: None,
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: vec![crate::core::agent_task::AgentTaskEvidenceRef {
            kind: "scheduler".to_string(),
            uri: "homeboy://agent-task/committed-harvest-preflight".to_string(),
            label: Some("committed-change harvest preflight".to_string()),
        }],
        diagnostics: Vec::new(),
        outputs: serde_json::Value::Null,
        workflow: None,
        follow_up: None,
        metadata: serde_json::Value::Null,
    }
}

fn committed_harvest_failure(
    mut outcome: AgentTaskOutcome,
    error: HarvestError,
) -> AgentTaskOutcome {
    let (class, message, data) = match error {
        HarvestError::DirtyWorkspace { status } => (
            "agent_task.committed_harvest_dirty_workspace",
            "refusing committed-change harvest from a workspace with pre-existing uncommitted changes"
                .to_string(),
            serde_json::json!({ "status": status }),
        ),
        HarvestError::UnrelatedHead { base, head } => (
            "agent_task.committed_harvest_unrelated_head",
            "workspace HEAD is no longer descended from the task base; refusing to harvest unrelated commits"
                .to_string(),
            serde_json::json!({ "base": base, "head": head }),
        ),
        HarvestError::Git { command, message } => (
            "agent_task.committed_harvest_git_failed",
            format!("committed-change harvest failed while running {command}: {message}"),
            serde_json::json!({ "command": command, "stderr": message }),
        ),
        HarvestError::ArtifactDirectory { path, message } => (
            "agent_task.committed_harvest_artifact_failed",
            format!("committed-change harvest could not create artifact directory {}: {message}", path.display()),
            serde_json::json!({ "path": path, "operation": "create_dir_all", "error": message }),
        ),
        HarvestError::ArtifactWrite { path, message } => (
            "agent_task.committed_harvest_artifact_failed",
            format!("committed-change harvest could not write patch artifact {}: {message}", path.display()),
            serde_json::json!({ "path": path, "operation": "write", "error": message }),
        ),
    };
    outcome.status = AgentTaskOutcomeStatus::Failed;
    outcome.failure_classification = Some(AgentTaskFailureClassification::ExecutionFailed);
    outcome.summary = Some(message.clone());
    outcome.diagnostics.push(AgentTaskDiagnostic {
        class: class.to_string(),
        message,
        data,
    });
    outcome
}

#[cfg(test)]
mod committed_harvest_tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskWorkspace,
        AGENT_TASK_REQUEST_SCHEMA,
    };

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command runs");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn git_metadata_failure_after_patch_creation_preserves_the_patch_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        git(&workspace, &["init", "-b", "main"]);
        git(&workspace, &["config", "user.email", "test@example.com"]);
        git(&workspace, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(workspace.join("file.txt"), "base\n").expect("base file");
        git(&workspace, &["add", "file.txt"]);
        git(&workspace, &["commit", "-m", "base"]);
        let base = git(&workspace, &["rev-parse", "HEAD"]);
        std::fs::write(workspace.join("file.txt"), "committed change\n").expect("change");
        git(&workspace, &["commit", "-am", "agent change"]);
        let request = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-1".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions: String::new(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace {
                root: Some(workspace.display().to_string()),
                ..Default::default()
            },
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: serde_json::Value::Null,
        };
        let running = RunningTask {
            task_id: "task-1".to_string(),
            request,
            workspace_key: None,
            executor_key: "test".to_string(),
            model_key: None,
            resource_units: 1,
            attempt: 1,
            started_at: Instant::now(),
            timeout_ms: None,
            rotation_index: 0,
            rotation_attempts: Vec::new(),
            task_base_sha: Some(base),
        };
        let mut outcome = committed_harvest_preflight_outcome("task-1".to_string());
        outcome.status = AgentTaskOutcomeStatus::Succeeded;
        let error = harvest_committed_patch_with_metadata(&mut outcome, &running, |_, _| {
            Err(HarvestError::Git {
                command: "git log injected metadata failure".to_string(),
                message: "injected failure".to_string(),
            })
        })
        .expect_err("metadata command fails after the real patch is attached");
        let patch_path = outcome.artifacts[0].path.clone().expect("patch path");
        assert!(Path::new(&patch_path).is_file());
        let failed = committed_harvest_failure(outcome, error);

        assert_eq!(failed.status, AgentTaskOutcomeStatus::Failed);
        assert_eq!(failed.artifacts[0].id, "committed-changes");
        assert_eq!(
            failed.artifacts[0].path.as_deref(),
            Some(patch_path.as_str())
        );
        assert!(failed.evidence_refs.iter().any(|evidence| {
            evidence.uri == "homeboy://agent-task/committed-harvest-preflight"
        }));
        assert!(failed.diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.committed_harvest_git_failed"
                && diagnostic.data["command"] == "git log injected metadata failure"
        }));
    }
}

/// Record a finalized outcome in the completed-by-task index and the ordered
/// outcomes list. Shared by the scheduler's dependency-block, dependency-render,
/// and task-completion paths to keep recording behavior identical.
fn record_completed_outcome(
    completed_by_task: &mut HashMap<String, AgentTaskOutcome>,
    outcomes: &mut Vec<AgentTaskOutcome>,
    outcome: AgentTaskOutcome,
) {
    completed_by_task.insert(outcome.task_id.clone(), outcome.clone());
    outcomes.push(outcome);
}

fn child_runs_for_outcomes(outcomes: &[AgentTaskOutcome]) -> Vec<AgentTaskChildRun> {
    outcomes
        .iter()
        .filter_map(|outcome| {
            let run_id = child_run_id(outcome)?;
            Some(AgentTaskChildRun {
                task_id: outcome.task_id.clone(),
                run_id,
                state: AgentTaskScheduleSupport::state_for_outcome(outcome),
                provider: outcome
                    .metadata
                    .get("provider")
                    .or_else(|| outcome.metadata.pointer("/provider_handle/backend"))
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string),
                metadata: child_run_metadata(outcome),
            })
        })
        .collect()
}

fn artifact_bindings_for_outcomes(
    outcomes: &[AgentTaskOutcome],
) -> Vec<AgentTaskArtifactRunBinding> {
    outcomes
        .iter()
        .filter_map(|outcome| child_run_id(outcome).map(|run_id| (outcome, run_id)))
        .flat_map(|(outcome, run_id)| {
            outcome
                .artifacts
                .iter()
                .map(move |artifact| AgentTaskArtifactRunBinding {
                    task_id: outcome.task_id.clone(),
                    run_id: run_id.clone(),
                    artifact_id: artifact.id.clone(),
                    kind: artifact.kind.clone(),
                    name: artifact.name.clone(),
                    path: artifact.path.clone(),
                    url: artifact.url.clone(),
                    sha256: artifact.sha256.clone(),
                })
        })
        .collect()
}

fn child_run_id(outcome: &AgentTaskOutcome) -> Option<String> {
    first_non_empty_json_string([
        outcome.metadata.get("child_run_id"),
        outcome.metadata.get("run_id"),
        outcome.metadata.get("remote_run_id"),
        outcome.metadata.get("provider_run_id"),
        outcome.metadata.pointer("/provider_handle/provider_run_id"),
        outcome.outputs.pointer("/provider_run_result/run_id"),
        outcome.outputs.pointer("/provider_run_result/id"),
    ])
}

fn child_run_metadata(outcome: &AgentTaskOutcome) -> serde_json::Value {
    let mut metadata = serde_json::Map::new();
    for key in ["provider", "provider_handle", "provider_handles"] {
        if let Some(value) = outcome.metadata.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    serde_json::Value::Object(metadata)
}

fn first_non_empty_json_string<'a>(
    values: impl IntoIterator<Item = Option<&'a serde_json::Value>>,
) -> Option<String> {
    values.into_iter().flatten().find_map(|value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

mod outcome;
mod scheduling;
#[cfg(test)]
mod tests;

use outcome::event;
use scheduling::{
    active_resource_units, adaptive_concurrency_decision, executor_key, model_key,
    task_resource_units, AgentTaskScheduleSupport,
};
