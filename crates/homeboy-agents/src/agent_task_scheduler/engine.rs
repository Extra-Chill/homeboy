use homeboy_engine_primitives::content_hash;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Instant;

use crate::agent_task_service::DerivedCookBaselineCapability;

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

/// The one shape an executor takes once it reaches the scheduler.
///
/// Execution dispatch is a runtime choice -- extension provider, test double,
/// bench harness -- and never a compile-time one, so the executor is carried as
/// an erased shared pointer rather than a type parameter. The trait is
/// object-safe by construction: `execute` and `cancel` both take `&self`,
/// neither is generic, and neither mentions `Self` in return position.
///
/// Sharing is the whole point. A Cook hands the executor to each retry attempt
/// and each parallel branch, and `Arc` makes that a refcount bump onto one
/// underlying adapter instead of a copy -- which matters, because a provider
/// adapter holds real state. `AgentTaskScheduler` has always stored its executor
/// behind an `Arc`, so this is the ownership model that was already in effect,
/// now written into the type.
pub type SharedAgentTaskExecutor = Arc<dyn AgentTaskExecutorAdapter>;

pub struct AgentTaskScheduler {
    executor: SharedAgentTaskExecutor,
    run_id: Option<String>,
    harvest_context: HarvestExecutionContext,
    lifecycle_store: Option<crate::agent_task_lifecycle::AgentTaskLifecycleStore>,
    #[cfg(test)]
    scratch_root: Option<std::path::PathBuf>,
}

impl AgentTaskScheduler {
    pub fn new(executor: SharedAgentTaskExecutor) -> Self {
        Self::new_controller(executor)
    }

    /// Construct a controller-local scheduler that intentionally ignores
    /// ambient Lab transport metadata.
    pub fn new_controller(executor: SharedAgentTaskExecutor) -> Self {
        Self {
            executor,
            run_id: None,
            harvest_context: HarvestExecutionContext::default(),
            lifecycle_store: None,
            #[cfg(test)]
            scratch_root: None,
        }
    }

    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    pub fn with_harvest_context(mut self, context: HarvestExecutionContext) -> Self {
        self.harvest_context = context;
        self
    }

    pub(crate) fn with_lifecycle_store(
        mut self,
        lifecycle_store: crate::agent_task_lifecycle::AgentTaskLifecycleStore,
    ) -> Self {
        self.lifecycle_store = Some(lifecycle_store);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_scratch_root(mut self, data_root: std::path::PathBuf) -> Self {
        self.scratch_root = Some(data_root);
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
        let execution_deadline_unix_ms = plan.options.execution_budget.deadline_unix_ms;
        // A scheduler without a durable run record still needs a private
        // controller-scratch namespace. Plan IDs are semantic labels and can
        // legitimately repeat across independent in-process executions.
        #[cfg(test)]
        let scratch_run_id = self
            .run_id
            .clone()
            .unwrap_or_else(|| format!("ephemeral-{}", uuid::Uuid::new_v4()));
        #[cfg(not(test))]
        let scratch_run_id = self
            .run_id
            .clone()
            .unwrap_or_else(|| format!("ephemeral-{}", uuid::Uuid::new_v4()));
        let max_concurrency = plan.options.max_concurrency.max(1);
        let total_tasks = plan.tasks.len();
        let services = match super::managed_services::ManagedServices::start(
            &plan.services,
            &scratch_run_id,
        ) {
            Ok(services) => Some(services),
            Err(error) => {
                let (evidence_refs, evidence) =
                    super::managed_services::startup_failure_evidence(&scratch_run_id);
                let outcomes = plan
                    .tasks
                    .iter()
                    .map(|request| AgentTaskOutcome {
                        task_id: request.task_id.clone(),
                        status: AgentTaskOutcomeStatus::Failed,
                        summary: Some(error.clone()),
                        failure_classification: Some(
                            AgentTaskFailureClassification::ExecutionFailed,
                        ),
                        evidence_refs: evidence_refs.clone(),
                        diagnostics: vec![AgentTaskDiagnostic {
                            class: "managed_service_startup".to_string(),
                            message: error.clone(),
                            data: evidence.clone(),
                        }],
                        ..Default::default()
                    })
                    .collect::<Vec<_>>();
                return AgentTaskAggregate {
                    schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                    plan_id: plan.plan_id,
                    status: AgentTaskScheduleSupport::aggregate_status(&outcomes),
                    totals: AgentTaskScheduleSupport::totals(total_tasks, &outcomes),
                    outcomes,
                    events: Vec::new(),
                    artifact_lineage: Vec::new(),
                    child_runs: Vec::new(),
                    artifact_bindings: Vec::new(),
                    queue: Default::default(),
                };
            }
        };
        let recovered_outcomes =
            postprocess::recovered_upstream_outcomes(&plan, self.run_id.as_deref());
        let recovered_ids: std::collections::HashSet<_> = recovered_outcomes
            .iter()
            .map(|outcome| outcome.task_id.as_str())
            .collect();
        plan.tasks
            .retain(|task| !recovered_ids.contains(task.task_id.as_str()));
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
                request.limits.execution_deadline_unix_ms = execution_deadline_unix_ms;
                if let Some(policy) = plan_rotation.as_ref() {
                    AgentTaskScheduleSupport::apply_rotation_policy_limits(&mut request, policy);
                    // Backfill the initial attempt's model from the first entry
                    // so a configured model default is persisted before the run
                    // and finalization does not fail after publishing (#9013).
                    if let Some(entry) = policy.entries.first() {
                        AgentTaskScheduleSupport::apply_initial_rotation_entry_model(
                            &mut request,
                            entry,
                        );
                    }
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
        let mut outcomes = recovered_outcomes;
        let mut completed_by_task: HashMap<String, AgentTaskOutcome> = outcomes
            .iter()
            .map(|outcome| (outcome.task_id.clone(), outcome.clone()))
            .collect();
        let mut events = Vec::new();
        let mut cancellation_notified = false;
        let execution_budget = plan.options.execution_budget.clone();
        // Legacy retry policy remains an operator-facing cap; the execution
        // budget adds a shared hard ceiling across retries and rotations.
        let retry_max_attempts = plan.options.retry.max_attempts;
        let (tx, rx) = mpsc::channel();
        let cancellation_tx = tx.clone();
        cancellation.on_cancel(Arc::new(move || {
            let _ = cancellation_tx.send(SchedulerEvent::Cancellation);
        }));
        let mut adaptive_decisions = Vec::new();
        let mut last_effective_concurrency = None;
        let candidate_completion = plan.options.candidate_completion;

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
                if let Some(services) = services.as_ref() {
                    services.bind_into(&mut request.inputs, &mut request.metadata);
                }
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
                if crate::agent_task_timeout::remaining_execution_deadline_ms(
                    execution_deadline_unix_ms,
                ) == Some(0)
                {
                    let outcome = execution_deadline_outcome(
                        task_id.clone(),
                        execution_deadline_unix_ms.expect("checked deadline"),
                        "materialization",
                    );
                    events.push(event(
                        &task_id,
                        AgentTaskState::TimedOut,
                        scheduled.attempt,
                        outcome.summary.clone(),
                    ));
                    record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                    continue;
                }
                let source_identity_valid = request
                    .metadata
                    .get("cook_workspace_identity")
                    .map(|attestation| {
                        request.workspace.root.as_deref().is_some_and(|root| {
                            crate::agent_task_workspace_identity::workspace_matches_attestation(
                                Path::new(root),
                                attestation,
                            )
                        })
                    })
                    .unwrap_or(true);
                if !source_identity_valid {
                    let root = request.workspace.root.as_deref().unwrap_or("<missing>");
                    let outcome = committed_harvest_failure(
                        committed_harvest_preflight_outcome(task_id.clone()),
                        HarvestError::Git {
                            command: "verify Cook source workspace identity".to_string(),
                            cwd: Path::new(root).to_path_buf(),
                            message: "source workspace no longer matches its Cook admission identity attestation".to_string(),
                        },
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
                let harvest_preflight = match prepare_committed_harvest(
                    &request,
                    derived_cook_baseline,
                    &self.harvest_context,
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
                let scratch = if let Some(lifecycle_store) = self.lifecycle_store.as_ref() {
                    crate::controller_scratch::allocate_attempt_at(
                        &lifecycle_store.data_root(),
                        &scratch_run_id,
                        &plan.plan_id,
                        &request.task_id,
                        scheduled.attempt,
                    )
                } else {
                    #[cfg(test)]
                    {
                        match self.scratch_root.as_ref() {
                            Some(data_root) => crate::controller_scratch::allocate_test_attempt_at(
                                data_root,
                                &scratch_run_id,
                                &plan.plan_id,
                                &request.task_id,
                                scheduled.attempt,
                            ),
                            None => crate::controller_scratch::allocate_test_attempt(
                                &scratch_run_id,
                                &plan.plan_id,
                                &request.task_id,
                                scheduled.attempt,
                            ),
                        }
                    }
                    #[cfg(not(test))]
                    {
                        crate::controller_scratch::allocate_attempt(
                            &scratch_run_id,
                            &plan.plan_id,
                            &request.task_id,
                            scheduled.attempt,
                        )
                    }
                };
                let scratch = match scratch {
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
                let attempt_workspace = match prepare_attempt_workspace(
                    &mut request,
                    task_base_sha.as_deref(),
                    harvest_preflight.candidate_baseline.as_ref(),
                    &scratch.path,
                ) {
                    Ok(workspace) => workspace,
                    Err(error) => {
                        let outcome = committed_harvest_failure(
                            committed_harvest_preflight_outcome(task_id.clone()),
                            error,
                        );
                        let _ =
                            release_scratch(&scratch, "attempt_workspace_setup_failed", &outcome);
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
                // Bind the attempt's Git registration to this durable lease.
                // Recorded here, at the producer, so a registration whose
                // directory later disappears can still be pruned by identity
                // (#10568). A failure to record is not fatal: cleanup reads the
                // live worktree's own `.git` pointer first, and only the
                // already-deleted case depends on this row.
                if let Some(workspace) = attempt_workspace.as_ref() {
                    let _ = crate::controller_scratch::record_attempt_git_worktree(
                        &scratch,
                        workspace.root(),
                    );
                }
                let task_base_sha = attempt_workspace
                    .as_ref()
                    .map(|workspace| workspace.base_sha().to_string())
                    .or(task_base_sha);
                if let Some(adoption) = scheduled.adoption.as_ref() {
                    if let Err(mut outcome) = validate_and_apply_candidate_adoption(
                        &request,
                        adoption,
                        task_base_sha.as_deref(),
                    ) {
                        outcome.task_id = task_id.clone();
                        let _ = release_scratch(&scratch, "candidate_adoption_failed", &outcome);
                        record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                        continue;
                    }
                }
                if let Some(root) = request.workspace.root.as_deref() {
                    request.executor.remap_workspace_root(root);
                }
                request
                    .executor
                    .set_runtime_tmpdir(scratch.path.to_string_lossy().as_ref());
                let executor_key = executor_key(&request);
                let executor = Arc::clone(&self.executor);
                let plan_id = plan.plan_id.clone();
                let task_timeout_ms = crate::agent_task_timeout::effective_provider_timeout_ms(
                    request.limits.timeout_ms.or(plan.options.timeout_ms),
                    request.limits.max_runtime_ms,
                );
                let task_timeout_ms = crate::agent_task_timeout::remaining_execution_deadline_ms(
                    execution_deadline_unix_ms,
                )
                .map(|remaining| task_timeout_ms.min(remaining))
                .unwrap_or(task_timeout_ms);
                request.limits.execution_deadline_unix_ms = execution_deadline_unix_ms;
                let tx = tx.clone();
                let attempt = scheduled.attempt;
                let context = AgentTaskExecutionContext {
                    plan_id,
                    run_id: self.run_id.clone(),
                    attempt,
                    cancellation: cancellation.clone(),
                };

                if let Some(run_id) = self.run_id.as_deref() {
                    let reservation = match self.lifecycle_store.as_ref() {
                        Some(store) => store.reserve_provider_execution(run_id, &request, attempt),
                        None => crate::agent_task_lifecycle::reserve_provider_execution(
                            run_id, &request, attempt,
                        ),
                    };
                    match reservation {
                        Ok(crate::agent_task_lifecycle::ProviderExecutionReservation::Acquired) => {}
                        Ok(crate::agent_task_lifecycle::ProviderExecutionReservation::AlreadyReserved) => {
                            let outcome = scratch_allocation_failure(
                                task_id.clone(),
                                "provider execution was already reserved by an interrupted controller; reconcile the durable run instead of redispatching".to_string(),
                            );
                            let _ = release_scratch(&scratch, "provider_execution_already_reserved", &outcome);
                            events.push(event(&task_id, AgentTaskState::Failed, attempt, outcome.summary.clone()));
                            record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                            continue;
                        }
                        Err(error) => {
                            let outcome = scratch_allocation_failure(
                            task_id.clone(),
                            format!(
                                "could not durably record provider execution: {}",
                                error.message
                            ),
                        );
                        let _ = release_scratch(
                            &scratch,
                            "provider_execution_persistence_failed",
                            &outcome,
                        );
                        events.push(event(
                            &task_id,
                            AgentTaskState::Failed,
                            attempt,
                            outcome.summary.clone(),
                        ));
                        record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                        continue;
                        }
                    }
                }

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
                    execution_deadline_unix_ms,
                    timeout_cancel_requested: false,
                    rotation_index: scheduled.rotation_index,
                    rotation_attempts: scheduled.rotation_attempts,
                    candidate_artifacts: scheduled.candidate_artifacts,
                    retry_attempts: scheduled.retry_attempts,
                    source_workspace_root,
                    _attempt_workspace: attempt_workspace.clone(),
                    run_id: self.run_id.clone(),
                    artifact_root: self
                        .lifecycle_store
                        .as_ref()
                        .map(|store| store.artifact_root()),
                    artifact_nonce: uuid::Uuid::new_v4().to_string(),
                    task_base_sha,
                    source_provenance,
                    scratch: scratch.clone(),
                    adoption: scheduled.adoption,
                    join_handle: None,
                });

                // Re-bind the caller's notification route: it is thread-local,
                // so provider work dispatched here would otherwise run unrouted.
                let notification_route = homeboy_core::notification_route::capture();
                let join_handle = thread::spawn(move || {
                    notification_route.bind(|| {
                        let _attempt_workspace = attempt_workspace;
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                executor.execute(request, context)
                            }))
                            .unwrap_or_else(|_| provider_worker_panic(task_id.clone()));
                        let _ = tx.send(SchedulerEvent::TaskResult(Box::new(TaskResult {
                            task_id,
                            attempt,
                            outcome,
                            scratch,
                            completed_at: Instant::now(),
                        })));
                    })
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
                self.lifecycle_store.as_ref(),
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
                    let provider_completed_at = result.completed_at;
                    attach_candidate_adoption_provenance(
                        &mut outcome,
                        running_task.adoption.as_ref(),
                    );
                    persist_resolved_provider_model(&mut outcome, &running_task.request);
                    if let Some(run_id) = running_task.run_id.as_deref() {
                        let terminal_state = match outcome.status {
                            AgentTaskOutcomeStatus::Succeeded | AgentTaskOutcomeStatus::NoOp => {
                                "succeeded"
                            }
                            AgentTaskOutcomeStatus::Cancelled => "cancelled",
                            AgentTaskOutcomeStatus::Timeout => "timed_out",
                            AgentTaskOutcomeStatus::CandidateRecoverable => "candidate_recoverable",
                            _ => "failed",
                        };
                        let terminal = match self.lifecycle_store.as_ref() {
                            Some(store) => store.record_provider_execution_terminal_with_model(
                                run_id,
                                &outcome.task_id,
                                result.attempt,
                                terminal_state,
                                outcome.selected_model(),
                            ),
                            None => {
                                crate::agent_task_lifecycle::record_provider_execution_terminal_with_model(
                                    run_id,
                                    &outcome.task_id,
                                    result.attempt,
                                    terminal_state,
                                    outcome.selected_model(),
                                )
                            }
                        };
                        if let Err(error) = terminal {
                            outcome.status = AgentTaskOutcomeStatus::Failed;
                            outcome.failure_classification =
                                Some(AgentTaskFailureClassification::ExecutionFailed);
                            outcome.summary = Some(format!(
                                    "provider returned successfully but Homeboy could not durably record it: {}",
                                    error.message
                                ));
                            outcome.diagnostics.push(AgentTaskDiagnostic {
                                class: "agent_task.provider_execution_persistence_failed"
                                    .to_string(),
                                message: error.message,
                                data: serde_json::Value::Null,
                            });
                        }
                    }
                    if let Err(error) = harvest_uncommitted_patch(&mut outcome, &running_task)
                        .and_then(|_| harvest_committed_patch(&mut outcome, &running_task))
                    {
                        outcome = committed_harvest_failure(outcome, error);
                    }
                    // Harvest can add Homeboy-generated patch artifacts after the
                    // provider result was normalized. Keep their provenance tied
                    // to the same concrete model as the canonical outcome.
                    persist_resolved_provider_model(&mut outcome, &running_task.request);
                    finalize_candidate_artifacts(&mut outcome, &running_task);
                    let outcome =
                        AgentTaskScheduleSupport::normalize_outcome(outcome, Some(&running_task));
                    let mut outcome = outcome;
                    // Timeout reconciliation can discover a patch written after
                    // the provider returned its incomplete result. Bind those
                    // late artifacts to this exact execution before selecting a
                    // recoverable candidate for promotion.
                    finalize_candidate_artifacts(&mut outcome, &running_task);
                    // A fingerprinted, non-empty patch is a durable candidate.
                    // Let Cook admit and gate it before spending another full
                    // implementation-provider budget. Independent candidate
                    // tasks still follow the plan's candidate-completion policy;
                    // this only prevents sequential rotation of the same task.
                    let candidate_ready_for_convergence = outcome
                        .artifacts
                        .iter()
                        .any(is_fingerprinted_actionable_patch_artifact);
                    let rotation_takes_over = !candidate_ready_for_convergence
                        && AgentTaskScheduleSupport::rotation_policy_for_request(
                            &running_task.request,
                            plan.options.rotation.as_ref(),
                        )
                        .is_some_and(|policy| {
                            let mut eligible = outcome.clone();
                            if running_task.timeout_cancel_requested {
                                eligible.status = AgentTaskOutcomeStatus::Timeout;
                                eligible.failure_classification =
                                    Some(AgentTaskFailureClassification::Timeout);
                            }
                            AgentTaskScheduleSupport::should_rotate_provider(
                                &eligible,
                                &policy,
                                running_task.rotation_index,
                                result.attempt,
                                execution_budget.max_provider_executions,
                                execution_budget.max_provider_rotations,
                            )
                        });
                    if running_task.timeout_cancel_requested {
                        // Cancellation was requested at the deadline and this
                        // result proves the provider no longer owns the checkout.
                        // Harvest above is therefore race-free.
                        let recovered = outcome.artifacts.iter().any(is_actionable_patch_artifact);
                        outcome.status = if recovered && !rotation_takes_over {
                            AgentTaskOutcomeStatus::CandidateRecoverable
                        } else {
                            AgentTaskOutcomeStatus::Timeout
                        };
                        outcome.failure_classification =
                            Some(AgentTaskFailureClassification::Timeout);
                        outcome.diagnostics.push(AgentTaskDiagnostic {
                            class: if running_task
                                .execution_deadline_unix_ms
                                .is_some_and(|deadline| {
                                    crate::agent_task_timeout::now_unix_ms() >= deadline
                                })
                            {
                                "agent_task.execution_deadline_exceeded".to_string()
                            } else {
                                "scheduler_timeout".to_string()
                            },
                            message: if recovered {
                                "provider exited after scheduler cancellation; a recoverable candidate patch was harvested".to_string()
                            } else {
                                "provider exited after scheduler cancellation at its deadline".to_string()
                            },
                            data: serde_json::json!({
                                "timeout_ms": running_task.timeout_ms,
                                "elapsed_ms": running_task.started_at.elapsed().as_millis() as u64,
                                "deadline_unix_ms": running_task.execution_deadline_unix_ms,
                                "remaining_budget_ms": running_task.execution_deadline_unix_ms.map(|deadline| deadline.saturating_sub(crate::agent_task_timeout::now_unix_ms())),
                                "completed_phase": "provider_execution",
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
                    if candidate_ready_for_convergence
                        && outcome.status == AgentTaskOutcomeStatus::Timeout
                    {
                        outcome.status = AgentTaskOutcomeStatus::CandidateRecoverable;
                        outcome.summary = Some(
                            "provider reported a timeout after producing a recoverable candidate; promote the fingerprinted patch through controller gates".to_string(),
                        );
                        outcome.diagnostics.push(AgentTaskDiagnostic {
                            class: "agent_task.provider_timeout_recoverable_candidate".to_string(),
                            message: "a fingerprinted, non-empty patch was retained from a provider-reported timeout".to_string(),
                            data: serde_json::json!({
                                "required_validation": ["fresh_review", "deterministic_gates"],
                            }),
                        });
                    }
                    if !rotation_takes_over {
                        AgentTaskScheduleSupport::preserve_base_bound_patch_after_provider_failure(
                            &mut outcome,
                        );
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
                        execution_budget.max_provider_executions,
                        retry_max_attempts,
                        retry_budget_total,
                        retry_budget_used,
                        &plan.options.retry.retryable_failure_classifications,
                    ) {
                        let timeout_compaction = (outcome.failure_classification
                            == Some(AgentTaskFailureClassification::Timeout))
                        .then_some("timeout");
                        let authorization = cleanup_attempt_workspace(
                            &mut outcome,
                            &running_task,
                            timeout_compaction,
                        );
                        release_and_compact_attempt_workspace(
                            &result.scratch,
                            "retry",
                            &mut outcome,
                            &running_task,
                            authorization,
                        );
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
                    if !candidate_ready_for_convergence {
                        if let Some(policy) = &rotation_policy {
                            if AgentTaskScheduleSupport::should_rotate_provider(
                                &outcome,
                                policy,
                                running_task.rotation_index,
                                result.attempt,
                                execution_budget.max_provider_executions,
                                execution_budget.max_provider_rotations,
                            ) {
                                let authorization = cleanup_attempt_workspace(
                                    &mut outcome,
                                    &running_task,
                                    Some("provider_rotation"),
                                );
                                release_and_compact_attempt_workspace(
                                    &result.scratch,
                                    "provider_rotation",
                                    &mut outcome,
                                    &running_task,
                                    authorization,
                                );
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
                                                    class: "agent_task.candidate_adoption"
                                                        .to_string(),
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
                                if request.metadata["model_selection"]["requested"].is_null() {
                                    if !request.metadata.is_object() {
                                        request.metadata = serde_json::json!({});
                                    }
                                    request.metadata["model_selection"]["requested"] =
                                        serde_json::json!(request.executor.model());
                                }
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
                                    "provider rotation queued: entry {} of {}; backend={}, model={}",
                                    running_task.rotation_index + 1,
                                    policy.entries.len(),
                                    request.executor.backend,
                                    request.executor.model().unwrap_or("not recorded")
                                )),
                            ));
                                queued.push_back(ScheduledTask {
                                    workspace_key: AgentTaskScheduleSupport::workspace_key(
                                        &request,
                                    ),
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
                    }
                    #[expect(
                        clippy::redundant_locals,
                        reason = "the branch deliberately shadows its pre-retry outcome before terminal mutation"
                    )]
                    let mut outcome = outcome;
                    cleanup_attempt_workspace(&mut outcome, &running_task, None);
                    append_unique_artifacts(
                        &mut outcome.artifacts,
                        running_task.candidate_artifacts,
                    );
                    // A prior provider rotation may have captured a base-bound
                    // patch before this terminal provider failure. Decide
                    // recoverability after merging that retained evidence so the
                    // controller can promote the canonical candidate through its
                    // normal gates.
                    AgentTaskScheduleSupport::preserve_base_bound_patch_after_provider_failure(
                        &mut outcome,
                    );
                    // Captured before the loop consumes the vector; this is the
                    // same-provider retry count the budget diagnostic needs.
                    let same_provider_retries_used = running_task.retry_attempts.len();
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
                        same_provider_retries_used,
                    );
                    let _ = release_scratch(
                        &result.scratch,
                        if running_task.timeout_cancel_requested {
                            "scheduler_timeout_completion"
                        } else {
                            terminal_reason(&outcome, cancellation.is_cancelled())
                        },
                        &outcome,
                    );
                    if let Some(run_id) = running_task.run_id.as_deref() {
                        let _ = match self.lifecycle_store.as_ref() {
                            Some(store) => store.record_provider_execution_cleanup_elapsed(
                                run_id,
                                &outcome.task_id,
                                result.attempt,
                                provider_completed_at.elapsed().as_millis() as u64,
                            ),
                            None => crate::agent_task_lifecycle::record_provider_execution_cleanup_elapsed(
                                run_id,
                                &outcome.task_id,
                                result.attempt,
                                provider_completed_at.elapsed().as_millis() as u64,
                            ),
                        };
                    }
                    record_completed_outcome(&mut completed_by_task, &mut outcomes, outcome);
                    if candidate_completion == AgentTaskCandidateCompletionPolicy::FirstGreen
                        && outcomes.last().is_some_and(|outcome| {
                            outcome.status == AgentTaskOutcomeStatus::Succeeded
                        })
                    {
                        let selected_task_id = outcomes
                            .last()
                            .expect("selected candidate outcome")
                            .task_id
                            .clone();
                        if let Some(selected) = outcomes.last_mut() {
                            if !selected.metadata.is_object() {
                                selected.metadata = serde_json::json!({});
                            }
                            selected.metadata["candidate_selection"] = serde_json::json!({
                                "policy": "first_green",
                                "selected_task_id": selected_task_id,
                                "promotion_action": "promote_selected_candidate_only",
                            });
                        }
                        AgentTaskScheduleSupport::cancel_queued(
                            &mut queued,
                            &mut outcomes,
                            &mut events,
                        );
                        // Reuse the scheduler's durable deferred-cleanup owner.
                        // It keeps the join handle until the provider terminates,
                        // preserves late artifacts, and never reopens this result.
                        for task in &mut running {
                            self.executor.cancel(&task.task_id);
                            task.timeout_cancel_requested = true;
                            task.timeout_ms = Some(0);
                            task.started_at = Instant::now() - timeout_with_grace(0);
                        }
                        AgentTaskScheduleSupport::expire_timed_out_tasks(
                            &mut running,
                            &mut quarantined,
                            &mut outcomes,
                            &mut events,
                            self.executor.as_ref(),
                            self.lifecycle_store.as_ref(),
                        );
                        break;
                    }
                }
                Err(Some(mpsc::RecvTimeoutError::Timeout)) => {}
                Err(Some(mpsc::RecvTimeoutError::Disconnected)) | Err(None) => break,
            }
        }

        postprocess::run_postprocess_steps(
            &plan,
            self.run_id.as_deref(),
            &mut outcomes,
            &mut events,
            &cancellation,
        );

        let artifact_lineage =
            AgentTaskScheduleSupport::artifact_lineage(&outcomes, &plan.artifact_outputs);
        let child_runs = child_runs_for_outcomes(&outcomes);
        let artifact_bindings = artifact_bindings_for_outcomes(&outcomes);

        if candidate_completion == AgentTaskCandidateCompletionPolicy::WaitAll {
            if let Some(selected) = outcomes
                .iter_mut()
                .find(|outcome| outcome.status == AgentTaskOutcomeStatus::Succeeded)
            {
                if !selected.metadata.is_object() {
                    selected.metadata = serde_json::json!({});
                }
                selected.metadata["candidate_selection"] = serde_json::json!({
                    "policy": "wait_all",
                    "selected_task_id": selected.task_id,
                    "promotion_action": "promote_selected_candidate_only",
                });
            }
        }

        let services = services
            .map(|services| {
                services.cleanup(if cancellation.is_cancelled() {
                    "cancelled"
                } else {
                    "terminal"
                })
            })
            .unwrap_or_default();
        for outcome in &mut outcomes {
            if !outcome.metadata.is_object() {
                outcome.metadata = serde_json::json!({});
            }
            outcome.metadata["managed_services"] =
                serde_json::to_value(&services).unwrap_or(serde_json::Value::Null);
        }
        AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id,
            status: AgentTaskScheduleSupport::aggregate_status(&outcomes),
            totals: AgentTaskScheduleSupport::totals(
                total_tasks + plan.postprocess_steps.len(),
                &outcomes,
            ),
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

fn is_fingerprinted_actionable_patch_artifact(artifact: &AgentTaskArtifact) -> bool {
    is_actionable_patch_artifact(artifact)
        && artifact
            .sha256
            .as_deref()
            .is_some_and(|fingerprint| !fingerprint.trim().is_empty())
}

pub(crate) fn persist_resolved_provider_model(
    outcome: &mut AgentTaskOutcome,
    request: &AgentTaskRequest,
) {
    // A second normalization pass runs after patch harvest. Once the first pass
    // supplied a configured fallback in `metadata.model`, retain the original
    // runtime-report boundary instead of mistaking that fallback for a report.
    let provider_reported = outcome.metadata["model_identity"]["provider_reported"]
        .as_str()
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            (!outcome.metadata["model_identity"].is_object())
                .then(|| outcome.selected_model().map(str::to_string))
                .flatten()
        });
    let requested = request.metadata["model_selection"]["requested"]
        .as_str()
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string);
    let resolved = request
        .executor
        .model()
        .filter(|model| !model.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            request.metadata["model_selection"]["selected"]
                .as_str()
                .filter(|model| !model.trim().is_empty())
                .map(str::to_string)
        });
    // A runtime report is execution fact. When the runtime omits it, the
    // dispatch-selected model is the only concrete identity available.
    let actual = provider_reported.clone().or_else(|| resolved.clone());
    if !outcome.metadata.is_object() {
        outcome.metadata = serde_json::json!({});
    }
    let metadata = outcome
        .metadata
        .as_object_mut()
        .expect("outcome metadata object");
    // Keep each authority distinct: dispatch intent, selected runtime, and the
    // provider's response must never overwrite one another.
    metadata.insert(
        "model_identity".to_string(),
        serde_json::json!({
            "requested": requested,
            "attempted": resolved,
            "candidate_producing": provider_reported,
            // Retained for consumers of the previous outcome metadata shape.
            "resolved": resolved,
            "provider_reported": provider_reported,
            "actual": actual,
        }),
    );
    if let Some(actual) = actual {
        metadata.insert("model".to_string(), serde_json::json!(actual));
    }
    let actual = outcome.selected_model().map(str::to_string);
    for artifact in &mut outcome.artifacts {
        if artifact.kind != "patch" && artifact.role.as_deref() != Some("patch") {
            continue;
        }
        if !artifact.metadata.is_object() {
            artifact.metadata = serde_json::json!({});
        }
        artifact.metadata["provider_model"] = serde_json::json!(actual);
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
    pub(super) execution_deadline_unix_ms: Option<u64>,
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
    pub(super) artifact_root: Option<std::path::PathBuf>,
    pub(super) artifact_nonce: String,
    /// Workspace HEAD captured immediately before the executor runs. It bounds
    /// any committed patch candidate to this dispatch attempt.
    pub(super) task_base_sha: Option<String>,
    /// Verified source identity for snapshot-backed candidate artifacts.
    pub(super) source_provenance: Option<serde_json::Value>,
    pub(super) scratch: crate::controller_scratch::ControllerScratchAllocation,
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
    scratch: crate::controller_scratch::ControllerScratchAllocation,
    /// Captured by the provider worker before controller-owned artifact work.
    completed_at: Instant,
}

pub(super) fn deferred_cleanup_action_artifact(
    running: &RunningTask,
) -> Result<AgentTaskArtifact, HarvestError> {
    let run_id = running.run_id.as_deref().unwrap_or("unrecorded-run");
    let directory = artifact_root_for_running(running)?
        .join("agent-task")
        .join("deferred-cleanup")
        .join(homeboy_core::paths::sanitize_path_segment(run_id));
    std::fs::create_dir_all(&directory).map_err(|error| HarvestError::ArtifactDirectory {
        path: directory.clone(),
        message: error.to_string(),
    })?;
    let id = format!(
        "{}-attempt-{}-deferred-cleanup",
        homeboy_core::paths::sanitize_path_segment(&running.task_id),
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
    // The aggregate may expose this path as soon as this function returns, so
    // publish only a complete descriptor.
    write_deferred_cleanup_action(&path, &content).map_err(|error| {
        HarvestError::ArtifactWrite {
            path: path.clone(),
            message: error.to_string(),
        }
    })?;
    Ok(AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
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
        sha256: Some(content_hash::sha256_hex(&content)),
        metadata: serde_json::json!({ "run_id": running.run_id, "task_id": running.task_id, "attempt": running.attempt }),
    })
}

pub(super) fn artifact_root_for_running(running: &RunningTask) -> Result<PathBuf, HarvestError> {
    if let Some(root) = running.artifact_root.as_ref() {
        return Ok(root.clone());
    }
    #[cfg(test)]
    {
        Ok(running.scratch.path.clone())
    }
    #[cfg(not(test))]
    {
        homeboy_core::artifacts::root().map_err(|error| HarvestError::ArtifactDirectory {
            path: Path::new("<artifact-root>").to_path_buf(),
            message: error.message,
        })
    }
}

fn write_deferred_cleanup_action(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .expect("cleanup action has a parent directory");
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .expect("cleanup action has a file name")
            .to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&temporary, content)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

pub(super) fn complete_deferred_cleanup_recovery(
    path: &Path,
    outcome: &AgentTaskOutcome,
    cleanup: Result<(), String>,
) -> Result<(), String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read deferred cleanup descriptor: {error}"))?;
    let mut action: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|error| format!("cannot parse deferred cleanup descriptor: {error}"))?;
    let candidates = outcome
        .artifacts
        .iter()
        .filter(|artifact| is_actionable_patch_artifact(artifact))
        .cloned()
        .collect::<Vec<_>>();
    action["completed_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    match (cleanup, candidates.is_empty()) {
        (Err(error), false) => {
            // A committed or dirty attempt is deliberately retained, but its
            // harvested patch is still a completed durable result.
            action["status"] = serde_json::json!("candidate_recovered");
            action["candidate_artifacts"] =
                serde_json::to_value(candidates).unwrap_or(serde_json::Value::Null);
            action["workspace_retention"] =
                serde_json::json!(error.chars().take(512).collect::<String>());
        }
        (Err(error), true) => {
            action["status"] = serde_json::json!("failed");
            action["diagnostic"] = serde_json::json!(error.chars().take(512).collect::<String>());
        }
        (Ok(()), true) => action["status"] = serde_json::json!("completed_no_candidate"),
        (Ok(()), false) => {
            action["status"] = serde_json::json!("candidate_recovered");
            action["candidate_artifacts"] =
                serde_json::to_value(candidates).unwrap_or(serde_json::Value::Null);
        }
    }
    let content = serde_json::to_vec_pretty(&action)
        .map_err(|error| format!("cannot serialize deferred cleanup receipt: {error}"))?;
    write_deferred_cleanup_action(path, &content)
        .map_err(|error| format!("cannot persist deferred cleanup receipt: {error}"))
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
    allocation: &crate::controller_scratch::ControllerScratchAllocation,
    reason: &str,
    outcome: &AgentTaskOutcome,
) -> homeboy_core::Result<()> {
    crate::controller_scratch::release_attempt(
        allocation,
        reason,
        serde_json::json!({
        "task_id": outcome.task_id,
        "status": outcome.status,
        "outcome": outcome,
        }),
    )
}

/// A clean attempt is unregistered from Git before its enclosing scratch lease
/// is released. Dirty, unpushed, and indeterminate checkouts remain registered
/// for lifecycle cleanup instead of being force-removed.
fn cleanup_attempt_workspace(
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
    compaction_reason: Option<&str>,
) -> Option<RecoverablePatchProof> {
    let Some(workspace) = &running._attempt_workspace else {
        return None;
    };
    if let Err(error) = workspace.cleanup() {
        if let Some(reason) = compaction_reason {
            match recoverable_patch_proof(outcome, running, workspace) {
                Ok(proof) => return Some(proof.with_reason(reason)),
                Err(refusal) => outcome.diagnostics.push(AgentTaskDiagnostic {
                    class: "agent_task.attempt_workspace_compaction_refused".to_string(),
                    message: format!("attempt workspace retained: compact recovery proof refused: {refusal}"),
                    data: serde_json::json!({ "outcome": "refused", "reason": reason, "refusal_reason": refusal, "path": running.request.workspace.root }),
                }),
            }
        }
        outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.attempt_workspace_retained".to_string(),
            message: format!("attempt workspace retained for lifecycle cleanup: {error}"),
            data: serde_json::json!({
                "path": running.request.workspace.root,
                "reason": "dirty_unpushed_or_unknown",
            }),
        });
    }
    None
}

struct RecoverablePatchProof {
    id: String,
    sha256: String,
    base_ref: String,
    reason: String,
}

impl RecoverablePatchProof {
    fn with_reason(mut self, reason: &str) -> Self {
        self.reason = reason.to_string();
        self
    }
}

/// Persist terminal evidence before a force removal. A successful release is
/// also the durable authorization a later controller-scratch cleanup rechecks
/// if the process stops between these two operations.
fn release_and_compact_attempt_workspace(
    allocation: &crate::controller_scratch::ControllerScratchAllocation,
    reason: &str,
    outcome: &mut AgentTaskOutcome,
    running: &RunningTask,
    authorization: Option<RecoverablePatchProof>,
) {
    let Some(authorization) = authorization else {
        let _ = release_scratch(allocation, reason, outcome);
        return;
    };
    let evidence = serde_json::json!({
        "task_id": outcome.task_id,
        "status": outcome.status,
        "outcome": outcome,
        "compaction_authorization": {
            "outcome": "authorized",
            "reason": authorization.reason,
            "patch_artifact_id": authorization.id,
            "patch_sha256": authorization.sha256,
            "base_ref": authorization.base_ref,
        },
    });
    match crate::controller_scratch::release_attempt_with_compaction_authorization(
        allocation, reason, evidence,
    ) {
        Ok(()) => {
            let Some(workspace) = &running._attempt_workspace else {
                return;
            };
            match workspace.cleanup_verified_recoverable_patch() {
                Ok(()) => outcome.diagnostics.push(AgentTaskDiagnostic {
                    class: "agent_task.attempt_workspace_compacted".to_string(),
                    message: "attempt checkout was compacted after durable authorization and exact patch verification".to_string(),
                    data: serde_json::json!({
                        "outcome": "compacted",
                        "reason": authorization.reason,
                        "path": running.request.workspace.root,
                        "patch_artifact_id": authorization.id,
                        "patch_sha256": authorization.sha256,
                        "base_ref": authorization.base_ref,
                    }),
                }),
                Err(error) => outcome.diagnostics.push(AgentTaskDiagnostic {
                    class: "agent_task.attempt_workspace_compaction_refused".to_string(),
                    message: format!("attempt workspace retained after durable authorization: {error}"),
                    data: serde_json::json!({ "outcome": "refused", "reason": authorization.reason, "refusal_reason": "worktree_remove_failed", "path": running.request.workspace.root }),
                }),
            }
        }
        Err(error) => outcome.diagnostics.push(AgentTaskDiagnostic {
            class: "agent_task.attempt_workspace_compaction_refused".to_string(),
            message: format!("attempt workspace retained because durable compaction authorization could not be persisted: {}", error.message),
            data: serde_json::json!({ "outcome": "refused", "reason": authorization.reason, "refusal_reason": "authorization_persistence_failed", "path": running.request.workspace.root }),
        }),
    }
}

fn recoverable_patch_proof(
    outcome: &AgentTaskOutcome,
    running: &RunningTask,
    workspace: &AttemptWorkspace,
) -> Result<RecoverablePatchProof, String> {
    let artifact_root = artifact_root_for_running(running).map_err(|error| format!("{error:?}"))?;
    let mut matches = outcome.artifacts.iter().filter(|artifact| {
        artifact.kind == "patch"
            && artifact.metadata["run_id"].as_str() == running.run_id.as_deref()
            && artifact.metadata["task_id"].as_str() == Some(running.task_id.as_str())
            && artifact.metadata["producer_attempt"].as_u64() == Some(running.attempt as u64)
            && artifact.metadata["base_ref"].as_str() == Some(workspace.base_sha())
    });
    let Some(artifact) = matches.next() else {
        return Err("no finalized patch artifact is bound to this attempt base".to_string());
    };
    if matches.next().is_some() {
        return Err(
            "multiple finalized patch artifacts are bound to this attempt base".to_string(),
        );
    }
    let Some(path) = artifact.path.as_deref().map(std::path::PathBuf::from) else {
        return Err("patch artifact has no readable path".to_string());
    };
    if !path.starts_with(&artifact_root) {
        return Err("patch artifact is outside the durable artifact root".to_string());
    }
    let Some(expected_sha256) = artifact.sha256.as_deref() else {
        return Err("patch artifact has no content hash".to_string());
    };
    let patch =
        std::fs::read(&path).map_err(|error| format!("cannot read patch artifact: {error}"))?;
    if homeboy_engine_primitives::content_hash::sha256_hex(&patch) != expected_sha256 {
        return Err("patch artifact content hash does not match its finalized record".to_string());
    }
    if !crate::controller_scratch::workspace_matches_staged_patch(
        workspace.root(),
        workspace.base_sha(),
        &patch,
    ) {
        return Err(
            "checkout does not exactly match the finalized patch against its immutable base"
                .to_string(),
        );
    }
    Ok(RecoverablePatchProof {
        id: artifact.id.clone(),
        sha256: expected_sha256.to_string(),
        base_ref: workspace.base_sha().to_string(),
        reason: String::new(),
    })
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

fn provider_worker_panic(task_id: String) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status: AgentTaskOutcomeStatus::Failed,
        summary: Some("provider worker panicked".to_string()),
        failure_classification: Some(AgentTaskFailureClassification::ExecutionFailed),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_task.provider_worker_panicked".to_string(),
            message: "provider worker panicked before returning an outcome".to_string(),
            data: serde_json::Value::Null,
        }],
        outputs: serde_json::Value::Null,
        workflow: None,
        follow_up: None,
        metadata: serde_json::Value::Null,
    }
}

fn execution_deadline_outcome(
    task_id: String,
    deadline_unix_ms: u64,
    completed_phase: &str,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id,
        status: AgentTaskOutcomeStatus::Timeout,
        summary: Some("agent-task execution deadline expired before provider dispatch".to_string()),
        failure_classification: Some(AgentTaskFailureClassification::Timeout),
        artifacts: Vec::new(),
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: vec![AgentTaskDiagnostic {
            class: "agent_task.execution_deadline_exceeded".to_string(),
            message: format!(
                "the total execution deadline expired during {completed_phase}; no further work will be started"
            ),
            data: serde_json::json!({
                "deadline_unix_ms": deadline_unix_ms,
                "remaining_budget_ms": 0,
                "completed_phase": completed_phase,
            }),
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
    TaskResult(Box<TaskResult>),
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

#[cfg(test)]
mod executor_erasure_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingExecutor(Arc<AtomicUsize>);

    impl AgentTaskExecutorAdapter for CountingExecutor {
        fn execute(
            &self,
            _request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            unreachable!("this fixture exercises the bound and cancel(), never execution");
        }

        fn cancel(&self, _task_id: &str) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// What the real call sites do: take the executor by value and hand a clone
    /// onward, once per retry attempt and once per parallel branch.
    fn dispatch_then_hand_on(executor: SharedAgentTaskExecutor) -> SharedAgentTaskExecutor {
        executor.cancel("task");
        executor.clone()
    }

    #[test]
    fn cloning_a_shared_executor_shares_one_underlying_executor() {
        let calls = Arc::new(AtomicUsize::new(0));
        let executor: SharedAgentTaskExecutor = Arc::new(CountingExecutor(Arc::clone(&calls)));

        let cloned = dispatch_then_hand_on(executor);
        cloned.cancel("task");

        // Cloning must share one executor rather than duplicate it. Retry
        // attempts hand out clones and a provider adapter holds real state --
        // per-clone copies would silently give each attempt its own.
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "both the original and its clone must dispatch to the same executor"
        );
    }

    #[test]
    fn deferred_cleanup_descriptor_replaces_only_complete_content() {
        let directory = tempfile::tempdir().expect("descriptor directory");
        let descriptor = directory.path().join("deferred-cleanup.json");
        std::fs::write(&descriptor, b"old receipt").expect("seed descriptor");

        write_deferred_cleanup_action(&descriptor, br#"{"status":"pending"}"#)
            .expect("atomically publish descriptor");

        assert_eq!(
            std::fs::read(&descriptor).expect("published descriptor"),
            br#"{"status":"pending"}"#
        );
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("descriptor directory entries")
                .count(),
            1,
            "temporary descriptor files must not be exposed"
        );
    }
}
