use serde_json::Value;
use std::path::PathBuf;

use crate::core::agent_task::{AgentTaskRequest, AgentTaskWorkspaceMode};
use crate::core::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
};
use crate::core::agent_task_finalization::{
    finalize_pr, AgentTaskPrEvidence, AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails,
    AgentTaskPrSourceRelationship, AgentTaskPrVerification,
};
use crate::core::agent_task_gate::AgentTaskGateRevealPolicy;
use crate::core::agent_task_lifecycle::{
    self, AgentTaskRunArtifacts, AgentTaskRunLog, AgentTaskRunRecord,
};
use crate::core::agent_task_promotion::{
    promote, AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use crate::core::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskScheduler,
};
use crate::core::{config, Error, Result};

#[derive(Debug, Clone)]
pub struct AgentTaskRunResult<T> {
    pub value: T,
    pub exit_code: i32,
}

#[derive(Debug, Clone)]
pub struct AgentTaskLoopServiceOptions {
    pub loop_id: String,
    pub initial_run_id: String,
    pub to_worktree: String,
    pub provider_command: Option<String>,
    pub verify: Vec<String>,
    pub private_verify: Vec<String>,
    pub private_gate_reveal: AgentTaskGateRevealPolicy,
    pub max_attempts: u32,
    pub no_finalize: bool,
    pub base: String,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub source_refs: Vec<String>,
    pub protected_branches: Vec<String>,
    pub ai_tool: String,
    pub ai_model: Option<String>,
    pub ai_used_for: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskLoopReport {
    pub schema: &'static str,
    pub loop_id: String,
    pub status: String,
    pub attempts: Vec<AgentTaskLoopAttemptReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalization: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskLoopAttemptReport {
    pub attempt: u32,
    pub run_id: String,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<AgentTaskPromotionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<AgentTaskCookLoopReport>,
}

pub fn read_plan(spec: &str) -> Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(spec)?;
    let mut plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })?;
    normalize_plan_workspaces(&mut plan)?;
    Ok(plan)
}

pub fn run_cook_loop<E>(
    options: AgentTaskLoopServiceOptions,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskLoopReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let max_attempts = options.max_attempts.max(1);
    let mut attempts = Vec::new();
    let mut run_id = options.initial_run_id.clone();
    let loop_id = options.loop_id.clone();

    for attempt in 1..=max_attempts {
        let record = agent_task_lifecycle::status(&run_id)?;
        let plan = agent_task_lifecycle::load_plan(&run_id)?;
        let Some(source_request) = plan.tasks.first().cloned() else {
            return Ok(loop_report(
                loop_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task loop requires a plan with one source task".to_string()),
                1,
            ));
        };
        if plan.tasks.len() != 1 {
            return Ok(loop_report(
                loop_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task loop currently supports one task per cook attempt".to_string()),
                1,
            ));
        }

        if !matches!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
        ) {
            attempts.push(AgentTaskLoopAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(loop_report(
                loop_id,
                "provider_failure",
                attempts,
                None,
                Some(format!(
                    "agent-task run {run_id} ended in state {:?}",
                    record.state
                )),
                1,
            ));
        }

        let promotion = match promote_attempt(&options, &run_id) {
            Ok(report) => report,
            Err(error) => {
                attempts.push(AgentTaskLoopAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path,
                    promotion: None,
                    feedback: None,
                });
                return Ok(loop_report(
                    loop_id,
                    "policy_failure",
                    attempts,
                    None,
                    Some(error.to_string()),
                    1,
                ));
            }
        };

        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion.clone(),
            attempt,
            max_attempts,
            source_run_id: Some(run_id.clone()),
            current_diff: String::new(),
            metadata: Value::Null,
        });
        let feedback_status = feedback.status;
        let follow_up_request = feedback.follow_up_request.clone();
        attempts.push(AgentTaskLoopAttemptReport {
            attempt,
            run_id: run_id.clone(),
            run_state: format!("{:?}", record.state),
            aggregate_path: record.aggregate_path,
            promotion: Some(promotion.clone()),
            feedback: Some(feedback.clone()),
        });

        match feedback_status {
            AgentTaskCookLoopStatus::GreenCompleted => {
                if options.no_finalize {
                    return Ok(loop_report(
                        loop_id,
                        "green_no_finalize",
                        attempts,
                        None,
                        Some(
                            "deterministic gates completed green; --no-finalize skipped commit, push, and PR finalization"
                                .to_string(),
                        ),
                        0,
                    ));
                }
                let finalization = finalize_loop_pr(&options, &loop_id, &promotion)?;
                let final_status = finalization["status"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let exit_code = if matches!(final_status.as_str(), "review_ready" | "no_changes") {
                    0
                } else {
                    1
                };
                return Ok(loop_report(
                    loop_id,
                    &final_status,
                    attempts,
                    Some(finalization),
                    None,
                    exit_code,
                ));
            }
            AgentTaskCookLoopStatus::RetryRequested => {
                let Some(follow_up_request) = follow_up_request else {
                    return Ok(loop_report(
                        loop_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(
                            "cook-loop feedback requested retry without a follow-up request"
                                .to_string(),
                        ),
                        1,
                    ));
                };
                let next_run_id = format!("{loop_id}-attempt-{}", attempt + 1);
                let follow_up_plan = AgentTaskPlan::new(
                    format!("{loop_id}-cook-loop-attempt-{}", attempt + 1),
                    vec![follow_up_request],
                );
                run_loaded_plan(follow_up_plan, Some(&next_run_id), executor.clone())?;
                run_id = next_run_id;
            }
            AgentTaskCookLoopStatus::RetriesExhausted => {
                return Ok(loop_report(
                    loop_id,
                    "retries_exhausted",
                    attempts,
                    None,
                    Some(
                        "deterministic gates stayed red after the configured attempt budget"
                            .to_string(),
                    ),
                    1,
                ));
            }
        }
    }

    Ok(loop_report(
        loop_id,
        "retries_exhausted",
        attempts,
        None,
        Some("cook-loop attempt budget exhausted".to_string()),
        1,
    ))
}

pub fn promotion_source(spec: &str) -> Result<(String, Option<PathBuf>)> {
    if spec != "-" {
        let path = PathBuf::from(spec.strip_prefix('@').unwrap_or(spec));
        if path.is_file() {
            let raw = std::fs::read_to_string(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "read agent-task promotion source {}",
                        path.display()
                    )),
                )
            })?;
            return Ok((raw, Some(path)));
        }
    }

    if let Ok((raw, path)) = agent_task_lifecycle::aggregate_source(spec) {
        return Ok((raw, Some(path)));
    }

    Ok((
        config::read_json_spec_to_string(spec)?,
        source_spec_path(spec),
    ))
}

pub fn run_loaded_plan<E>(
    mut plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    normalize_plan_workspaces(&mut plan)?;

    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::mark_running(run_id)?;
    }

    let aggregate = run_plan_with_scheduler(plan.clone(), executor);
    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
    }
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: aggregate,
    })
}

pub fn submit_plan_spec(spec: &str, run_id: Option<&str>) -> Result<AgentTaskRunRecord> {
    let plan = read_plan(spec)?;
    agent_task_lifecycle::submit_plan(&plan, run_id)
}

pub fn run_submitted<E>(
    run_id: String,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_running(&run_id)?;
    run_claimed(run_id, executor)
}

pub fn run_next<E>(executor: E) -> Result<AgentTaskRunResult<Option<AgentTaskAggregate>>>
where
    E: AgentTaskExecutorAdapter,
{
    let Some(record) = agent_task_lifecycle::claim_next_queued_run()? else {
        return Ok(AgentTaskRunResult {
            value: None,
            exit_code: 0,
        });
    };

    let result = run_claimed(record.run_id, executor)?;
    Ok(AgentTaskRunResult {
        value: Some(result.value),
        exit_code: result.exit_code,
    })
}

pub fn resume<E>(run_id: String, executor: E) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_resuming(&run_id)?;
    run_claimed(run_id, executor)
}

pub fn retry(
    run_id: &str,
    new_run_id: Option<&str>,
    run: bool,
) -> Result<AgentTaskRetryServiceResult> {
    let record = agent_task_lifecycle::retry(run_id, new_run_id)?;
    Ok(AgentTaskRetryServiceResult { record, run })
}

#[derive(Debug, Clone)]
pub struct AgentTaskRetryServiceResult {
    pub record: AgentTaskRunRecord,
    pub run: bool,
}

pub fn status(run_id: &str) -> Result<AgentTaskRunRecord> {
    agent_task_lifecycle::status(run_id)
}

pub fn logs(run_id: &str) -> Result<AgentTaskRunLog> {
    agent_task_lifecycle::logs(run_id)
}

pub fn artifacts(run_id: &str) -> Result<AgentTaskRunArtifacts> {
    agent_task_lifecycle::artifacts(run_id)
}

pub fn cancel(run_id: &str, reason: Option<&str>) -> Result<AgentTaskRunRecord> {
    agent_task_lifecycle::cancel_run(run_id, reason)
}

pub fn normalize_plan_workspaces(plan: &mut AgentTaskPlan) -> Result<()> {
    for request in &mut plan.tasks {
        normalize_component_worktree_workspace(request)?;
    }

    Ok(())
}

fn run_claimed<E>(run_id: String, executor: E) -> Result<AgentTaskRunResult<AgentTaskAggregate>>
where
    E: AgentTaskExecutorAdapter,
{
    let plan = agent_task_lifecycle::load_plan(&run_id)?;
    let aggregate = run_plan_with_scheduler(plan.clone(), executor);
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    Ok(AgentTaskRunResult {
        exit_code: aggregate_exit_code(&aggregate),
        value: aggregate,
    })
}

fn run_plan_with_scheduler<E>(plan: AgentTaskPlan, executor: E) -> AgentTaskAggregate
where
    E: AgentTaskExecutorAdapter,
{
    AgentTaskScheduler::new(executor).run(plan)
}

pub fn aggregate_exit_code(aggregate: &AgentTaskAggregate) -> i32 {
    if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    }
}

fn promote_attempt(
    options: &AgentTaskLoopServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let (source, source_path) = promotion_source(run_id)?;
    promote(AgentTaskPromotionOptions {
        source,
        source_path,
        to_worktree: options.to_worktree.clone(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        verify: options.verify.clone(),
        private_verify: options.private_verify.clone(),
        private_gate_reveal: options.private_gate_reveal,
        provider_command: options.provider_command.clone(),
    })
}

fn finalize_loop_pr(
    options: &AgentTaskLoopServiceOptions,
    loop_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> Result<Value> {
    if promotion.status != AgentTaskPromotionStatus::Applied {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "agent-task loop finalization requires an applied promotion with green gates",
            None,
            None,
        ));
    }
    let path = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "promotion provider did not report the applied worktree path",
                None,
                None,
            )
        })?
        .to_string();
    let source_refs = options
        .source_refs
        .iter()
        .cloned()
        .chain(std::iter::once(format!(
            "homeboy://agent-task/run/{loop_id}"
        )))
        .collect();
    let artifact_refs = std::iter::once(promotion.patch_artifact.path.clone()).collect();
    let report = finalize_pr(AgentTaskPrFinalizationOptions {
        path,
        run_id: loop_id.to_string(),
        base: options.base.clone(),
        head: options.head.clone(),
        title: options.title.clone(),
        commit_message: options.commit_message.clone(),
        gate_results: Vec::new(),
        normalized_gate_results: promotion.gate_results.clone(),
        changed_files: promotion.changed_files.clone(),
        evidence: AgentTaskPrEvidence {
            source_refs,
            artifact_refs,
            attempt_summary: format!(
                "{} deterministic cook-loop gate attempt(s) completed green",
                promotion.deterministic_gates.len()
            ),
            ai_tool: options.ai_tool.clone(),
            ai_model: options.ai_model.clone(),
            source_relationship: AgentTaskPrSourceRelationship::default(),
            verification: AgentTaskPrVerification {
                targeted_checks_run: options.verify.clone(),
                targeted_checks_unavailable: None,
                ci_expected: vec!["Homeboy CI after push".to_string()],
                manual_reviewer_check: None,
            },
            runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
        },
        ai_used_for: options.ai_used_for.clone(),
        protected_branches: options.protected_branches.clone(),
    })?;
    Ok(serde_json::to_value(report).unwrap_or(Value::Null))
}

fn loop_report(
    loop_id: String,
    status: &str,
    attempts: Vec<AgentTaskLoopAttemptReport>,
    finalization: Option<Value>,
    stop_reason: Option<String>,
    exit_code: i32,
) -> AgentTaskRunResult<AgentTaskLoopReport> {
    AgentTaskRunResult {
        value: AgentTaskLoopReport {
            schema: "homeboy/agent-task-loop/v1",
            loop_id,
            status: status.to_string(),
            attempts,
            finalization,
            stop_reason,
        },
        exit_code,
    }
}

fn source_spec_path(spec: &str) -> Option<PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(PathBuf::from(spec.strip_prefix('@').unwrap_or(spec)))
}

fn normalize_component_worktree_workspace(request: &mut AgentTaskRequest) -> Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }

    let Some(component_id) = request.workspace.component_id.clone() else {
        return Err(Error::validation_invalid_argument(
            "workspace.component_id",
            format!(
                "agent-task task '{}' component-worktree workspace requires component_id",
                request.task_id
            ),
            None,
            None,
        ));
    };

    let resolved_root = request
        .workspace
        .root
        .clone()
        .or_else(|| materialization_string(&request.workspace.materialization, "root"))
        .or_else(|| materialization_string(&request.workspace.materialization, "resolved_root"));

    let Some(root) = resolved_root else {
        return Err(Error::validation_invalid_argument(
            "workspace.root",
            format!(
                "agent-task task '{}' requested component-worktree workspace for component '{}' but no resolved root was provided; creating component worktrees depends on the generic Homeboy worktree primitive tracked by Extra-Chill/homeboy#3362",
                request.task_id, component_id
            ),
            None,
            None,
        ));
    };

    request.workspace.kind = None;
    request.workspace.mode = AgentTaskWorkspaceMode::Existing;
    request.workspace.root = Some(root);
    request.workspace.slug = Some(component_id);
    request.workspace.component_id = None;
    request.workspace.branch = None;
    request.workspace.base_ref = None;
    request.workspace.task_url = None;
    request.workspace.cleanup = None;
    request.workspace.materialization = Value::Null;

    Ok(())
}

fn materialization_string(materialization: &Value, key: &str) -> Option<String> {
    materialization
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_lifecycle::{status as lifecycle_status, AgentTaskRunState};
    use crate::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskState};
    use crate::test_support::with_isolated_home;

    #[test]
    fn service_run_loaded_plan_persists_durable_lifecycle() {
        with_isolated_home(|_| {
            let result = run_loaded_plan(test_plan(), Some("service-run"), SucceedingExecutor)
                .expect("service run completed");
            let record = lifecycle_status("service-run").expect("status persisted");

            assert_eq!(result.exit_code, 0);
            assert_eq!(record.state, AgentTaskRunState::Succeeded);
            assert_eq!(record.tasks[0].state, AgentTaskState::Succeeded);
            assert!(record.aggregate_path.is_some());
        });
    }

    #[test]
    fn service_normalizes_resolved_component_worktree_plan() {
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("homeboy".to_string());
        plan.tasks[0].workspace.materialization = serde_json::json!({
            "resolved_root": "/tmp/homeboy@service"
        });

        normalize_plan_workspaces(&mut plan).expect("workspace normalized");

        assert!(plan.tasks[0].workspace.kind.is_none());
        assert_eq!(plan.tasks[0].workspace.slug.as_deref(), Some("homeboy"));
        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some("/tmp/homeboy@service")
        );
        assert_eq!(
            plan.tasks[0].workspace.mode,
            AgentTaskWorkspaceMode::Existing
        );
        assert!(plan.tasks[0].workspace.materialization.is_null());
    }

    struct SucceedingExecutor;

    impl AgentTaskExecutorAdapter for SucceedingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                failure_classification: None,
                artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    fn test_plan() -> AgentTaskPlan {
        AgentTaskPlan::new(
            "service-plan",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "service-task".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: Some("service".to_string()),
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "run".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                metadata: Value::Null,
            }],
        )
    }
}
