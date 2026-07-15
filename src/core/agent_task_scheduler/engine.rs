use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Instant;

use crate::core::agent_task_service::DerivedCookBaselineCapability;

use super::*;

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
        self.run_with_derived_cook_baseline(plan, None)
    }

    pub(crate) fn run_with_derived_cook_baseline(
        &self,
        plan: AgentTaskPlan,
        derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> AgentTaskAggregate {
        self.run_with_cancellation_and_derived_cook_baseline(
            plan,
            AgentTaskCancellationToken::default(),
            derived_cook_baseline,
        )
    }

    pub(crate) fn run_with_cancellation(
        &self,
        plan: AgentTaskPlan,
        cancellation: AgentTaskCancellationToken,
    ) -> AgentTaskAggregate {
        self.run_with_cancellation_and_derived_cook_baseline(plan, cancellation, None)
    }

    fn run_with_cancellation_and_derived_cook_baseline(
        &self,
        plan: AgentTaskPlan,
        cancellation: AgentTaskCancellationToken,
        derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
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
                    resource_wait: None,
                    attempt: 1,
                    rotation_index: 0,
                    rotation_attempts: Vec::new(),
                    candidate_artifacts: Vec::new(),
                    retry_attempts: Vec::new(),
                    task_base_sha: None,
                    adoption: None,
                }
            })
            .collect();
        let mut running: Vec<RunningTask> = Vec::new();
        let mut quarantined: Vec<QuarantinedTask> = Vec::new();
        let mut outcomes = Vec::new();
        let mut completed_by_task: HashMap<String, AgentTaskOutcome> = HashMap::new();
        let mut events = Vec::new();
        let mut cancellation_notified = false;
        let execution_budget = plan.options.execution_budget.clone();
        let max_attempts = execution_budget.max_provider_executions;
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
                    if dependency_wait.is_none() {
                        if let Some(task) = queued.front_mut() {
                            AgentTaskScheduleSupport::record_resource_wait(
                                task,
                                &running,
                                &mut events,
                            );
                        }
                    }
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
                let harvest_preflight = match prepare_committed_harvest(
                    &request,
                    derived_cook_baseline,
                ) {
                    Ok(preflight) => preflight,
                    Err(error) => {
                        record_harvest_setup_failure(
                            &task_id,
                            scheduled.attempt,
                            error,
                            &mut completed_by_task,
                            &mut outcomes,
                            &mut events,
                        );
                        continue;
                    }
                };
                let task_base_sha = scheduled
                    .task_base_sha
                    .clone()
                    .or(harvest_preflight.base_sha.clone());
                let source_workspace_root = request.workspace.root.clone();
                let source_provenance = harvest_preflight.source_provenance;
                let attempt_workspace =
                    match prepare_attempt_workspace(&mut request, task_base_sha.as_deref()) {
                        Ok(workspace) => workspace,
                        Err(error) => {
                            record_harvest_setup_failure(
                                &task_id,
                                scheduled.attempt,
                                error,
                                &mut completed_by_task,
                                &mut outcomes,
                                &mut events,
                            );
                            continue;
                        }
                    };
                if let Some(adoption) = scheduled.adoption.as_ref() {
                    if let Err(mut outcome) = validate_and_apply_candidate_adoption(
                        &request,
                        adoption,
                        task_base_sha.as_deref(),
                    ) {
                        outcome.task_id = task_id.clone();
                        record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                        continue;
                    }
                }
                if let Some(root) = request.workspace.root.as_deref() {
                    request.executor.remap_workspace_root(root);
                }
                let run_id = self.run_id.as_deref().unwrap_or(&plan.plan_id);
                let scratch = match crate::core::controller_scratch::allocate_attempt(
                    run_id,
                    &plan.plan_id,
                    &request.task_id,
                    scheduled.attempt,
                ) {
                    Ok(scratch) => scratch,
                    Err(error) => {
                        let outcome =
                            scratch_allocation_failure(task_id.clone(), error.to_string());
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
                request
                    .executor
                    .set_runtime_tmpdir(scratch.path.to_string_lossy().as_ref());
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

                let resource_wait_message = scheduled.resource_wait.as_ref().map(|wait| {
                    format!(
                        "acquired exclusive resource '{}' after waiting {} ms; previous holder '{}'",
                        wait.key,
                        wait.started_at.elapsed().as_millis(),
                        wait.blocker_task_id
                    )
                });
                events.push(event(
                    &task_id,
                    AgentTaskState::Running,
                    attempt,
                    resource_wait_message,
                ));
                running.push(RunningTask {
                    task_id: task_id.clone(),
                    request: request.clone(),
                    workspace_key: scheduled.workspace_key,
                    executor_key,
                    model_key: model_key(&request),
                    resource_units: task_resource_units(&request, &plan.options.resource_budget),
                    exclusive_resource_keys: AgentTaskScheduleSupport::exclusive_resource_keys(
                        &request,
                    ),
                    attempt,
                    started_at: Instant::now(),
                    timeout_ms: Some(task_timeout_ms),
                    timeout_cancel_requested: false,
                    rotation_index: scheduled.rotation_index,
                    rotation_attempts: scheduled.rotation_attempts,
                    candidate_artifacts: scheduled.candidate_artifacts,
                    retry_attempts: scheduled.retry_attempts,
                    source_workspace_root,
                    _attempt_workspace: attempt_workspace.clone(),
                    run_id: self.run_id.clone(),
                    artifact_nonce: uuid::Uuid::new_v4().to_string(),
                    task_base_sha,
                    source_provenance,
                    scratch: scratch.clone(),
                    adoption: scheduled.adoption,
                    join_handle: None,
                });

                let join_handle = thread::spawn(move || {
                    let _attempt_workspace = attempt_workspace;
                    let outcome = executor.execute(request, context);
                    let _ = tx.send(SchedulerEvent::TaskResult(TaskResult {
                        task_id,
                        attempt,
                        outcome,
                        scratch,
                    }));
                });
                running
                    .last_mut()
                    .expect("running task inserted")
                    .join_handle = Some(join_handle);
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
                    task.timeout_ms.map(|ms| {
                        let deadline = if task.timeout_cancel_requested {
                            timeout_with_grace(ms)
                        } else {
                            std::time::Duration::from_millis(ms)
                        };
                        deadline.saturating_sub(task.started_at.elapsed())
                    })
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
                    debug_assert_eq!(result.scratch, running_task.scratch);
                    let mut running_task = running_task;
                    if let Some(join_handle) = running_task.join_handle.take() {
                        let _ = join_handle.join();
                    }
                    let mut outcome = result.outcome;
                    attach_candidate_adoption_provenance(
                        &mut outcome,
                        running_task.adoption.as_ref(),
                    );
                    if let Err(error) = harvest_uncommitted_patch(&mut outcome, &running_task)
                        .and_then(|_| harvest_committed_patch(&mut outcome, &running_task))
                    {
                        if let Some(workspace) = &running_task._attempt_workspace {
                            workspace.retain_for_diagnostics();
                        }
                        outcome = committed_harvest_failure(outcome, error);
                    }
                    finalize_candidate_artifacts(&mut outcome, &running_task);
                    let outcome =
                        AgentTaskScheduleSupport::normalize_outcome(outcome, Some(&running_task));
                    let mut outcome = outcome;
                    if running_task.timeout_cancel_requested {
                        // Cancellation was requested at the deadline and this
                        // result proves the provider no longer owns the checkout.
                        // Harvest above is therefore race-free.
                        let recovered = outcome.artifacts.iter().any(is_actionable_patch_artifact);
                        outcome.status = if recovered {
                            AgentTaskOutcomeStatus::CandidateRecoverable
                        } else {
                            AgentTaskOutcomeStatus::Timeout
                        };
                        outcome.failure_classification =
                            Some(AgentTaskFailureClassification::Timeout);
                        outcome.diagnostics.push(AgentTaskDiagnostic {
                            class: "scheduler_timeout".to_string(),
                            message: if recovered {
                                "provider exited after scheduler cancellation; a recoverable candidate patch was harvested".to_string()
                            } else {
                                "provider exited after scheduler cancellation at its deadline".to_string()
                            },
                            data: serde_json::json!({
                                "timeout_ms": running_task.timeout_ms,
                                "elapsed_ms": running_task.started_at.elapsed().as_millis() as u64,
                                "provider_backend": running_task.request.executor.backend,
                                "provider_model": running_task.request.executor.model(),
                                "candidate_recoverable": recovered,
                            }),
                        });
                        if recovered {
                            outcome.summary = Some(
                                "provider timed out after producing a recoverable candidate; promote the fingerprinted patch through controller gates".to_string(),
                            );
                        }
                    }
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
                        execution_budget.max_same_provider_retries,
                        max_attempts,
                        retry_budget_total,
                        retry_budget_used,
                        &plan.options.retry.retryable_failure_classifications,
                    ) {
                        release_scratch(&result.scratch, "retry", &outcome);
                        retry_budget_used += 1;
                        let retry_evidence = retry_attempt_evidence(&outcome, &running_task);
                        let mut retry_attempts = running_task.retry_attempts;
                        retry_attempts.push(retry_evidence);
                        let (mut request, candidate_artifacts) = reset_attempt_request(
                            running_task.request,
                            running_task.source_workspace_root,
                            running_task.candidate_artifacts,
                            &outcome,
                        );
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
                            resource_wait: None,
                            attempt: next_attempt,
                            rotation_index: running_task.rotation_index,
                            rotation_attempts: running_task.rotation_attempts,
                            candidate_artifacts,
                            retry_attempts,
                            task_base_sha: running_task.task_base_sha,
                            adoption: running_task.adoption,
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
                            max_attempts,
                            execution_budget.max_provider_rotations,
                        ) {
                            release_scratch(&result.scratch, "provider_rotation", &outcome);
                            let mut rotation_attempts = running_task.rotation_attempts.clone();
                            rotation_attempts.push(
                                AgentTaskScheduleSupport::rotation_attempt_record(
                                    &running_task.request,
                                    &outcome,
                                    result.attempt,
                                    running_task.rotation_index,
                                ),
                            );
                            let entry = &policy.entries[running_task.rotation_index];
                            let adoption = match entry.adoption.as_ref() {
                                Some(template) => {
                                    let mut candidate_artifacts =
                                        running_task.candidate_artifacts.clone();
                                    append_unique_artifacts(
                                        &mut candidate_artifacts,
                                        outcome
                                            .artifacts
                                            .iter()
                                            .filter(|artifact| {
                                                is_actionable_patch_artifact(artifact)
                                            })
                                            .cloned()
                                            .collect(),
                                    );
                                    match select_candidate_adoption(
                                        template,
                                        &candidate_artifacts,
                                        &running_task,
                                    ) {
                                        Ok(adoption) => Some(adoption),
                                        Err(message) => {
                                            let mut outcome = outcome;
                                            outcome.diagnostics.push(AgentTaskDiagnostic {
                                                class: "agent_task.candidate_adoption".to_string(),
                                                message,
                                                data: serde_json::Value::Null,
                                            });
                                            record_completed_outcome(
                                                &mut completed_by_task,
                                                &mut outcomes,
                                                outcome,
                                            );
                                            continue;
                                        }
                                    }
                                }
                                None => None,
                            };
                            let (mut request, candidate_artifacts) = reset_attempt_request(
                                running_task.request,
                                running_task.source_workspace_root,
                                running_task.candidate_artifacts,
                                &outcome,
                            );
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
                                resource_wait: None,
                                attempt: next_attempt,
                                rotation_index: running_task.rotation_index + 1,
                                rotation_attempts,
                                candidate_artifacts,
                                retry_attempts: running_task.retry_attempts,
                                task_base_sha: running_task.task_base_sha,
                                adoption,
                            });
                            continue;
                        }
                    }
                    let mut outcome = outcome;
                    append_unique_artifacts(
                        &mut outcome.artifacts,
                        running_task.candidate_artifacts,
                    );
                    for retry_attempt in running_task.retry_attempts {
                        outcome.diagnostics.push(AgentTaskDiagnostic {
                            class: "agent_task.retry_attempt".to_string(),
                            message: "previous retry attempt failed; its diagnostics and patch evidence are retained".to_string(),
                            data: retry_attempt,
                        });
                    }
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
                    AgentTaskScheduleSupport::attach_execution_budget_evidence(
                        &mut outcome,
                        &execution_budget,
                        result.attempt,
                        running_task.rotation_index,
                    );
                    release_scratch(
                        &result.scratch,
                        if running_task.timeout_cancel_requested {
                            "scheduler_timeout_completion"
                        } else {
                            terminal_reason(&outcome, cancellation.is_cancelled())
                        },
                        &outcome,
                    );
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
pub(super) struct ScheduledTask {
    pub(super) request: AgentTaskRequest,
    pub(super) workspace_key: Option<String>,
    pub(super) resource_wait: Option<ResourceWait>,
    pub(super) attempt: u32,
    /// Rotation entries already consumed for this task (0 = original executor).
    pub(super) rotation_index: usize,
    /// Ordered evidence for prior dispatch attempts under a rotation policy.
    pub(super) rotation_attempts: Vec<AgentTaskProviderRotationAttempt>,
    /// Patch candidates produced by earlier retry or rotation attempts.
    pub(super) candidate_artifacts: Vec<AgentTaskArtifact>,
    /// Structured diagnostics retained from failed retries before finalization.
    pub(super) retry_attempts: Vec<serde_json::Value>,
    /// Captured before the first provider execution and reused by every sibling.
    pub(super) task_base_sha: Option<String>,
    pub(super) adoption: Option<AgentTaskCandidateAdoption>,
}

#[derive(Debug)]
pub(super) struct RunningTask {
    pub(super) task_id: String,
    pub(super) request: AgentTaskRequest,
    pub(super) workspace_key: Option<String>,
    pub(super) executor_key: String,
    pub(super) model_key: Option<String>,
    pub(super) resource_units: u32,
    pub(super) exclusive_resource_keys: Vec<String>,
    pub(super) attempt: u32,
    pub(super) started_at: Instant,
    pub(super) timeout_ms: Option<u64>,
    /// Deadline cancellation has been sent; harvesting waits for TaskResult.
    pub(super) timeout_cancel_requested: bool,
    /// Rotation entries already consumed for this task (0 = original executor).
    pub(super) rotation_index: usize,
    /// Ordered evidence for prior dispatch attempts under a rotation policy.
    pub(super) rotation_attempts: Vec<AgentTaskProviderRotationAttempt>,
    pub(super) candidate_artifacts: Vec<AgentTaskArtifact>,
    /// Structured diagnostics retained from failed retries before finalization.
    pub(super) retry_attempts: Vec<serde_json::Value>,
    /// The caller-managed workspace used for preflight and as the clean base
    /// for each isolated provider dispatch.
    pub(super) source_workspace_root: Option<String>,
    /// Dropping the final owner removes this detached checkout. The executor
    /// thread holds a clone so timeout finalization cannot race provider I/O.
    pub(super) _attempt_workspace: Option<Arc<AttemptWorkspace>>,
    pub(super) run_id: Option<String>,
    pub(super) artifact_nonce: String,
    /// Workspace HEAD captured immediately before the executor runs. It bounds
    /// any committed patch candidate to this dispatch attempt.
    pub(super) task_base_sha: Option<String>,
    /// Verified source identity for snapshot-backed candidate artifacts.
    pub(super) source_provenance: Option<serde_json::Value>,
    pub(super) scratch: crate::core::controller_scratch::ControllerScratchAllocation,
    pub(super) adoption: Option<AgentTaskCandidateAdoption>,
    pub(super) join_handle: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone)]
pub(super) struct ResourceWait {
    pub(super) key: String,
    pub(super) blocker_task_id: String,
    pub(super) started_at: Instant,
}

pub(super) struct QuarantinedTask {
    pub(super) workspace_key: Option<String>,
}

struct TaskResult {
    task_id: String,
    attempt: u32,
    outcome: AgentTaskOutcome,
    scratch: crate::core::controller_scratch::ControllerScratchAllocation,
}

pub(super) fn deferred_cleanup_action_artifact(
    running: &RunningTask,
) -> Result<AgentTaskArtifact, HarvestError> {
    use sha2::{Digest, Sha256};

    let run_id = running.run_id.as_deref().unwrap_or("unrecorded-run");
    let directory = crate::core::artifacts::root()
        .map_err(|error| HarvestError::ArtifactDirectory {
            path: Path::new("<artifact-root>").to_path_buf(),
            message: error.message,
        })?
        .join("agent-task")
        .join("deferred-cleanup")
        .join(crate::core::paths::sanitize_path_segment(run_id));
    std::fs::create_dir_all(&directory).map_err(|error| HarvestError::ArtifactDirectory {
        path: directory.clone(),
        message: error.to_string(),
    })?;
    let id = format!(
        "{}-attempt-{}-deferred-cleanup",
        crate::core::paths::sanitize_path_segment(&running.task_id),
        running.attempt
    );
    let path = directory.join(format!("{id}.json"));
    let action = serde_json::json!({
        "schema": "homeboy/agent-task-deferred-cleanup/v1",
        "status": "pending",
        "run_id": running.run_id,
        "task_id": running.task_id,
        "attempt": running.attempt,
        "safe_next_action": "Wait for cleanup completion; mutable workspace recovery is intentionally deferred until provider exit.",
    });
    let content = serde_json::to_vec_pretty(&action).expect("cleanup action serializes");
    std::fs::write(&path, &content).map_err(|error| HarvestError::ArtifactWrite {
        path: path.clone(),
        message: error.to_string(),
    })?;
    Ok(AgentTaskArtifact {
        schema: crate::core::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: id.clone(),
        kind: "cleanup_action".to_string(),
        name: Some("deferred-cleanup.json".to_string()),
        label: Some("deferred attempt workspace cleanup".to_string()),
        role: Some("cleanup_action".to_string()),
        semantic_key: None,
        path: Some(path.display().to_string()),
        url: None,
        mime: Some("application/json".to_string()),
        size_bytes: Some(content.len() as u64),
        sha256: Some(format!("{:x}", Sha256::digest(&content))),
        metadata: serde_json::json!({ "run_id": running.run_id, "task_id": running.task_id, "attempt": running.attempt }),
    })
}

pub(super) fn complete_deferred_cleanup_recovery(
    path: &Path,
    outcome: &AgentTaskOutcome,
    cleanup: Result<(), String>,
) {
    let mut action = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let candidates = outcome
        .artifacts
        .iter()
        .filter(|artifact| is_actionable_patch_artifact(artifact))
        .cloned()
        .collect::<Vec<_>>();
    action["completed_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    match cleanup {
        Err(error) => {
            action["status"] = serde_json::json!("failed");
            action["diagnostic"] = serde_json::json!(error.chars().take(512).collect::<String>());
        }
        Ok(()) if candidates.is_empty() => {
            action["status"] = serde_json::json!("completed_no_candidate")
        }
        Ok(()) => {
            action["status"] = serde_json::json!("candidate_recovered");
            action["candidate_artifacts"] =
                serde_json::to_value(candidates).unwrap_or(serde_json::Value::Null);
        }
    }
    let _ = std::fs::write(path, serde_json::to_vec_pretty(&action).unwrap_or_default());
}

fn retry_attempt_evidence(outcome: &AgentTaskOutcome, running: &RunningTask) -> serde_json::Value {
    serde_json::json!({
        "attempt": running.attempt,
        "status": outcome.status,
        "failure_classification": outcome.failure_classification,
        "summary": outcome.summary,
        "diagnostics": outcome.diagnostics,
        "artifacts": outcome.artifacts,
        "evidence_refs": outcome.evidence_refs,
    })
}

pub(super) fn release_scratch(
    allocation: &crate::core::controller_scratch::ControllerScratchAllocation,
    reason: &str,
    outcome: &AgentTaskOutcome,
) {
    let evidence = serde_json::json!({
        "task_id": outcome.task_id,
        "status": outcome.status,
        "outcome": outcome,
    });
    let _ = crate::core::controller_scratch::release_attempt(allocation, reason, evidence);
}

fn terminal_reason(outcome: &AgentTaskOutcome, cancelled: bool) -> &'static str {
    if cancelled || outcome.status == AgentTaskOutcomeStatus::Cancelled {
        "cancelled"
    } else if outcome.status == AgentTaskOutcomeStatus::Timeout {
        "provider_timeout"
    } else if matches!(
        outcome.status,
        AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp
    ) {
        "succeeded"
    } else {
        "provider_failure"
    }
}

fn scratch_allocation_failure(task_id: String, error: String) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status: AgentTaskOutcomeStatus::Failed,
        summary: Some(format!("could not allocate provider scratch root: {error}")),
        failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_task.controller_scratch_allocation_failed".to_string(),
            message: error,
            data: serde_json::Value::Null,
        }],
        outputs: serde_json::Value::Null,
        workflow: None,
        follow_up: None,
        metadata: serde_json::Value::Null,
    }
}

fn reset_attempt_request(
    mut request: AgentTaskRequest,
    source_workspace_root: Option<String>,
    mut candidate_artifacts: Vec<AgentTaskArtifact>,
    outcome: &AgentTaskOutcome,
) -> (AgentTaskRequest, Vec<AgentTaskArtifact>) {
    if let (Some(attempt_root), Some(source_root)) = (
        request.workspace.root.clone(),
        source_workspace_root.clone(),
    ) {
        remap_workspace_config(
            &mut request.executor.config,
            Path::new(&attempt_root),
            Path::new(&source_root),
        );
    }
    request.workspace.root = source_workspace_root;
    append_unique_artifacts(
        &mut candidate_artifacts,
        outcome
            .artifacts
            .iter()
            .filter(|artifact| is_actionable_patch_artifact(artifact))
            .cloned()
            .collect(),
    );
    (request, candidate_artifacts)
}

enum SchedulerEvent {
    TaskResult(TaskResult),
    Cancellation,
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

fn record_harvest_setup_failure(
    task_id: &str,
    attempt: u32,
    error: HarvestError,
    completed_by_task: &mut HashMap<String, AgentTaskOutcome>,
    outcomes: &mut Vec<AgentTaskOutcome>,
    events: &mut Vec<AgentTaskProgressEvent>,
) {
    let outcome =
        committed_harvest_failure(committed_harvest_preflight_outcome(task_id.into()), error);
    events.push(event(
        task_id,
        AgentTaskState::Failed,
        attempt,
        outcome.summary.clone(),
    ));
    record_completed_outcome(completed_by_task, outcomes, outcome);
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
