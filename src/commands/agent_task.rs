use clap::{Args, Subcommand};
use serde_json::Value;

use homeboy::core::agent_task::AgentTaskRequest;
use homeboy::core::agent_task_lifecycle;
use homeboy::core::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_task_scheduler::{
    AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskScheduler,
};
use homeboy::core::config;

use super::agent_task_dispatch::{run as dispatch, DispatchArgs};
use super::{CmdResult, GlobalArgs};

pub mod review;

#[derive(Args, Debug)]
pub struct AgentTaskArgs {
    #[command(subcommand)]
    pub command: AgentTaskCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskCommand {
    /// Build and dispatch common repo-cooking agent tasks without hand-authored provider JSON.
    Dispatch(DispatchArgs),
    /// Run an agent-task plan through extension-declared executor providers.
    RunPlan(RunPlanArgs),
    /// Execute a previously submitted durable agent-task run.
    Run(StatusArgs),
    /// Claim and execute the oldest queued durable agent-task run.
    RunNext,
    /// Persist an agent-task plan and return a durable run id without executing it.
    Submit(SubmitArgs),
    /// Read durable agent-task run status.
    Status(StatusArgs),
    /// Read durable agent-task run scheduler events.
    Logs(StatusArgs),
    /// List artifacts and evidence refs recorded for a completed run.
    Artifacts(StatusArgs),
    /// Mark a queued or stale-running durable agent-task run as cancelled.
    Cancel(CancelArgs),
    /// Resume a queued or stale-running durable run.
    Resume(StatusArgs),
    /// Submit a fresh durable run from an existing run's plan.
    Retry(RetryArgs),
    /// Build a durable aggregate review envelope from run state, logs, artifacts, and promotion hints.
    Review(ReviewArgs),
    /// Promote a completed generic patch artifact into a managed worktree.
    Promote(PromoteArgs),
    /// Finalize a green cook run into a review-ready pull request.
    FinalizePr(FinalizePrArgs),
    /// List extension-declared agent-task executor providers.
    Providers,
}

#[derive(Args, Debug)]
pub struct RunPlanArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    /// Also persist the completed run lifecycle record under this id.
    #[arg(long, value_name = "ID")]
    pub record_run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct SubmitArgs {
    /// AgentTaskPlan JSON file, @file, or - for stdin.
    #[arg(long, value_name = "PATH")]
    pub plan: String,
    /// Optional durable run id. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub run_id: Option<String>,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,
}

#[derive(Args, Debug)]
pub struct RetryArgs {
    /// Existing durable run id whose plan should be retried.
    pub run_id: String,

    /// Optional durable run id for the retry. Generated when omitted.
    #[arg(long, value_name = "ID")]
    pub new_run_id: Option<String>,

    /// Execute the newly queued retry immediately.
    #[arg(long)]
    pub run: bool,
}

#[derive(Args, Debug)]
pub struct CancelArgs {
    /// Durable run id returned by `agent-task submit` or `agent-task run-plan --record-run-id`.
    pub run_id: String,

    /// Operator-visible reason stored on the durable run record.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}

#[derive(Args, Debug)]
pub struct ReviewArgs {
    /// Durable run id returned by `agent-task submit`, `dispatch`, or `run-plan --record-run-id`.
    pub run_id: String,

    /// Managed DMC worktree handle to include in generated promotion commands.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: Option<String>,
}

#[derive(Args, Debug)]
pub struct PromoteArgs {
    /// AgentTaskOutcome or AgentTaskAggregate JSON file, @file, or - for stdin.
    #[arg(value_name = "SOURCE")]
    pub source: String,

    /// Managed DMC worktree handle to apply into, e.g. repo@branch-slug.
    #[arg(long, value_name = "HANDLE")]
    pub to_worktree: String,

    /// Outcome task id to select when SOURCE is an aggregate.
    #[arg(long, value_name = "TASK_ID")]
    pub task_id: Option<String>,

    /// Patch artifact id to select when the outcome contains multiple patches.
    #[arg(long, value_name = "ARTIFACT_ID")]
    pub artifact_id: Option<String>,

    /// Validate and report the selected promotion without creating/applying.
    #[arg(long)]
    pub dry_run: bool,

    /// Verification command to run in the promoted worktree after apply.
    #[arg(long = "verify", value_name = "COMMAND")]
    pub verify: Vec<String>,
}

#[derive(Args, Debug)]
pub struct FinalizePrArgs {
    /// Durable cook/agent-task run id to link in the PR body.
    #[arg(long, value_name = "ID")]
    pub run_id: String,

    /// Verified candidate worktree path.
    #[arg(long, value_name = "PATH")]
    pub path: String,

    /// PR base branch.
    #[arg(long, default_value = "main", value_name = "BRANCH")]
    pub base: String,

    /// PR head branch. Defaults to the current branch in --path.
    #[arg(long, value_name = "BRANCH")]
    pub head: Option<String>,

    /// PR title.
    #[arg(long, value_name = "TEXT")]
    pub title: String,

    /// Commit message for the verified candidate changes.
    #[arg(long, value_name = "TEXT")]
    pub commit_message: String,

    #[command(flatten)]
    pub evidence: review::FinalizePrEvidenceArgs,

    /// Green gate result as name=status or name=status:detail. Repeatable.
    #[arg(long = "gate-result", value_name = "NAME=STATUS[:DETAIL]")]
    pub gate_results: Vec<String>,

    /// Changed file summary to include in output/PR body. Defaults to git status discovery.
    #[arg(long = "changed-file", value_name = "PATH")]
    pub changed_files: Vec<String>,

    /// Protected branch that may not be finalized directly. Repeatable.
    #[arg(long = "protected-branch", default_values_t = review::default_protected_branches(), value_name = "BRANCH")]
    pub protected_branches: Vec<String>,

    /// AI assistance scope for the PR body.
    #[arg(
        long,
        default_value = "Drafted implementation and tests; Chris reviews and owns the change.",
        value_name = "TEXT"
    )]
    pub ai_used_for: String,
}

pub fn run(args: AgentTaskArgs, global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::Dispatch(dispatch_args) => dispatch(dispatch_args, global),
        AgentTaskCommand::RunPlan(run_args) => run_plan(run_args),
        AgentTaskCommand::Run(status_args) => run_submitted(status_args),
        AgentTaskCommand::RunNext => run_next(),
        AgentTaskCommand::Submit(submit_args) => submit(submit_args),
        AgentTaskCommand::Status(status_args) => status(status_args),
        AgentTaskCommand::Logs(status_args) => logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => artifacts(status_args),
        AgentTaskCommand::Cancel(cancel_args) => cancel(cancel_args),
        AgentTaskCommand::Resume(status_args) => resume(status_args),
        AgentTaskCommand::Retry(retry_args) => retry(retry_args),
        AgentTaskCommand::Review(review_args) => review::review(review_args),
        AgentTaskCommand::Promote(promote_args) => review::promote_artifact(promote_args),
        AgentTaskCommand::FinalizePr(finalize_args) => review::finalize_pull_request(finalize_args),
        AgentTaskCommand::Providers => review::providers(),
    }
}

fn run_plan(args: RunPlanArgs) -> CmdResult<Value> {
    let plan = read_plan(&args.plan)?;
    run_loaded_plan(
        plan,
        args.record_run_id.as_deref(),
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn run_loaded_plan<E>(
    mut plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    normalize_plan_workspaces(&mut plan)?;

    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
        agent_task_lifecycle::mark_running(run_id)?;
    }

    let scheduler = AgentTaskScheduler::new(executor);
    let aggregate = scheduler.run(plan.clone());
    if let Some(run_id) = record_run_id {
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)?;
    }
    let exit_code = if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    };
    Ok((
        serde_json::to_value(aggregate).unwrap_or(Value::Null),
        exit_code,
    ))
}

fn run_submitted(args: StatusArgs) -> CmdResult<Value> {
    run_submitted_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn run_submitted_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_running(&run_id)?;
    run_claimed(run_id, executor)
}

fn run_next() -> CmdResult<Value> {
    run_next_with_executor(ExtensionProviderAgentTaskExecutor::discover())
}

fn run_next_with_executor<E>(executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let Some(record) = agent_task_lifecycle::claim_next_queued_run()? else {
        return Ok((serde_json::json!({ "claimed": false }), 0));
    };

    run_claimed(record.run_id, executor)
}

fn run_claimed<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    let plan = agent_task_lifecycle::load_plan(&run_id)?;
    let scheduler = AgentTaskScheduler::new(executor);
    let aggregate = scheduler.run(plan.clone());
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    let exit_code = if aggregate.totals.failed == 0
        && aggregate.totals.cancelled == 0
        && aggregate.totals.timed_out == 0
    {
        0
    } else {
        1
    };
    Ok((
        serde_json::to_value(aggregate).unwrap_or(Value::Null),
        exit_code,
    ))
}

fn submit(args: SubmitArgs) -> CmdResult<Value> {
    let plan = read_plan(&args.plan)?;
    let record = agent_task_lifecycle::submit_plan(&plan, args.run_id.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn status(args: StatusArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::status(&args.run_id)?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn logs(args: StatusArgs) -> CmdResult<Value> {
    let log = agent_task_lifecycle::logs(&args.run_id)?;
    Ok((serde_json::to_value(log).unwrap_or(Value::Null), 0))
}

fn artifacts(args: StatusArgs) -> CmdResult<Value> {
    let artifacts = agent_task_lifecycle::artifacts(&args.run_id)?;
    Ok((serde_json::to_value(artifacts).unwrap_or(Value::Null), 0))
}

fn cancel(args: CancelArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::cancel_run(&args.run_id, args.reason.as_deref())?;
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn resume(args: StatusArgs) -> CmdResult<Value> {
    run_resume_with_executor(args.run_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn run_resume_with_executor<E>(run_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
    agent_task_lifecycle::mark_resuming(&run_id)?;
    run_claimed(run_id, executor)
}

fn retry(args: RetryArgs) -> CmdResult<Value> {
    let record = agent_task_lifecycle::retry(&args.run_id, args.new_run_id.as_deref())?;
    if args.run {
        return run_submitted_with_executor(
            record.run_id,
            ExtensionProviderAgentTaskExecutor::discover(),
        );
    }
    Ok((serde_json::to_value(record).unwrap_or(Value::Null), 0))
}

fn read_plan(spec: &str) -> homeboy::core::Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(spec)?;
    let mut plan: AgentTaskPlan = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })?;
    normalize_plan_workspaces(&mut plan)?;
    Ok(plan)
}

fn normalize_plan_workspaces(plan: &mut AgentTaskPlan) -> homeboy::core::Result<()> {
    for request in &mut plan.tasks {
        normalize_component_worktree_workspace(request)?;
    }

    Ok(())
}

fn normalize_component_worktree_workspace(
    request: &mut AgentTaskRequest,
) -> homeboy::core::Result<()> {
    if request.workspace.kind.as_deref() != Some("component-worktree") {
        return Ok(());
    }

    let Some(component_id) = request.workspace.component_id.clone() else {
        return Err(homeboy::core::Error::validation_invalid_argument(
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
        return Err(homeboy::core::Error::validation_invalid_argument(
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
    request.workspace.mode = homeboy::core::agent_task::AgentTaskWorkspaceMode::Existing;
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
    use crate::test_support::with_isolated_home;
    use homeboy::core::agent_task::{
        AgentTaskArtifact, AgentTaskEvidenceRef, AgentTaskExecutor, AgentTaskLimits,
        AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest,
        AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use homeboy::core::agent_task_lifecycle::{
        status as lifecycle_status, AgentTaskRunRecord, AgentTaskRunState,
    };
    use homeboy::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskState};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    #[test]
    fn submit_run_status_reports_terminal_state() {
        with_temp_home(|| {
            let plan = AgentTaskPlan::new(
                "plan-cli-terminal",
                vec![AgentTaskRequest {
                    schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "task-cli-terminal".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: AgentTaskExecutor {
                        backend: "missing-provider-test".to_string(),
                        selector: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: None,
                        config: Value::Null,
                    },
                    instructions: "exercise durable terminal status".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: AgentTaskWorkspace::default(),
                    policy: AgentTaskPolicy::default(),
                    limits: AgentTaskLimits::default(),
                    expected_artifacts: Vec::new(),
                    metadata: Value::Null,
                }],
            );
            let plan_file = tempfile::NamedTempFile::new().expect("plan file");
            std::fs::write(
                plan_file.path(),
                serde_json::to_string(&plan).expect("plan json"),
            )
            .expect("write plan");
            let plan_path = format!("@{}", plan_file.path().display());

            submit(SubmitArgs {
                plan: plan_path,
                run_id: Some("run-cli-terminal".to_string()),
            })
            .expect("submitted");
            let (_, run_exit_code) = run_submitted(StatusArgs {
                run_id: "run-cli-terminal".to_string(),
            })
            .expect("run completed");
            let (status_json, status_exit_code) = status(StatusArgs {
                run_id: "run-cli-terminal".to_string(),
            })
            .expect("status loaded");
            let record: AgentTaskRunRecord = serde_json::from_value(status_json).expect("record");

            assert_eq!(run_exit_code, 1);
            assert_eq!(status_exit_code, 0);
            assert_eq!(record.state, AgentTaskRunState::Failed);
            assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
            assert_eq!(record.totals.expect("totals").failed, 1);
        });
    }

    #[test]
    fn run_plan_record_run_id_persists_running_status_before_executor_runs() {
        with_temp_home(|| {
            let run_id = "run-plan-durable";
            let observed_status = Arc::new(Mutex::new(None));
            let executor = InspectingExecutor {
                run_id: run_id.to_string(),
                observed_status: Arc::clone(&observed_status),
            };

            let (_value, exit_code) =
                run_loaded_plan(test_plan(), Some(run_id), executor).expect("run-plan completed");

            let observed = observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("executor observed durable status");
            assert_eq!(exit_code, 0);
            assert_eq!(observed.state, AgentTaskRunState::Running);
            assert_eq!(observed.tasks[0].state, AgentTaskState::Running);
            assert_eq!(observed.metadata["runner_pid"], std::process::id());
            assert!(observed.aggregate_path.is_none());

            let completed = lifecycle_status(run_id).expect("completed status loaded");
            assert_eq!(completed.state, AgentTaskRunState::Succeeded);
            assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
            assert!(completed.aggregate_path.is_some());
        });
    }

    #[test]
    fn run_next_claims_oldest_queued_run_and_leaves_later_runs_queued() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-a"))
                .expect("first submitted");
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-b"))
                .expect("second submitted");
            let observed_status = Arc::new(Mutex::new(None));

            let (_value, exit_code) = run_next_with_executor(InspectingExecutor {
                run_id: "run-next-a".to_string(),
                observed_status: Arc::clone(&observed_status),
            })
            .expect("claimed run completed");

            let observed = observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("executor observed claimed status");
            let first = lifecycle_status("run-next-a").expect("first status");
            let second = lifecycle_status("run-next-b").expect("second status");

            assert_eq!(exit_code, 0);
            assert_eq!(observed.state, AgentTaskRunState::Running);
            assert_eq!(first.state, AgentTaskRunState::Succeeded);
            assert_eq!(second.state, AgentTaskRunState::Queued);
        });
    }

    #[test]
    fn run_next_returns_unclaimed_when_no_queued_runs_exist() {
        with_temp_home(|| {
            let (value, exit_code) = run_next_with_executor(InspectingExecutor {
                run_id: "unused".to_string(),
                observed_status: Arc::new(Mutex::new(None)),
            })
            .expect("run-next checked queue");

            assert_eq!(exit_code, 0);
            assert_eq!(value["claimed"], false);
        });
    }

    #[test]
    fn cancel_command_marks_queued_run_cancelled() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-cli-cancel"))
                .expect("submitted");

            let (value, exit_code) = cancel(CancelArgs {
                run_id: "run-cli-cancel".to_string(),
                reason: Some("not selected".to_string()),
            })
            .expect("cancelled");
            let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

            assert_eq!(exit_code, 0);
            assert_eq!(record.state, AgentTaskRunState::Cancelled);
            assert_eq!(record.tasks[0].state, AgentTaskState::Cancelled);
            assert_eq!(record.metadata["cancel_reason"], json!("not selected"));
        });
    }

    #[test]
    fn retry_command_submits_new_queued_run() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-retry-source"))
                .expect("submitted");

            let (value, exit_code) = retry(RetryArgs {
                run_id: "run-retry-source".to_string(),
                new_run_id: Some("run-retry-cli".to_string()),
                run: false,
            })
            .expect("retry queued");
            let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

            assert_eq!(exit_code, 0);
            assert_eq!(record.run_id, "run-retry-cli");
            assert_eq!(record.state, AgentTaskRunState::Queued);
            assert_eq!(record.metadata["retry_of"], json!("run-retry-source"));
        });
    }

    #[test]
    fn resume_command_executes_existing_run() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-resume-cli"))
                .expect("submitted");
            let observed_status = Arc::new(Mutex::new(None));

            let (_value, exit_code) = run_resume_with_executor(
                "run-resume-cli".to_string(),
                InspectingExecutor {
                    run_id: "run-resume-cli".to_string(),
                    observed_status: Arc::clone(&observed_status),
                },
            )
            .expect("resumed");

            let observed = observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
                .expect("executor observed status");
            let completed = lifecycle_status("run-resume-cli").expect("completed status");

            assert_eq!(exit_code, 0);
            assert!(observed.metadata["resume_requested_at"].is_string());
            assert_eq!(completed.state, AgentTaskRunState::Succeeded);
        });
    }

    #[test]
    fn run_plan_maps_resolved_component_worktree_before_provider_dispatch() {
        let observed_request = Arc::new(Mutex::new(None));
        let executor = CapturingExecutor {
            observed_request: Arc::clone(&observed_request),
        };
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("wp-coding-agents".to_string());
        plan.tasks[0].workspace.branch = Some("fix/179-homeboy-codebox-guidance".to_string());
        plan.tasks[0].workspace.base_ref = Some("origin/main".to_string());
        plan.tasks[0].workspace.task_url =
            Some("https://github.com/Extra-Chill/wp-coding-agents/issues/179".to_string());
        plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
        plan.tasks[0].workspace.materialization = json!({
            "root": "/tmp/homeboy-worktrees/wp-coding-agents@fix-179-homeboy-codebox-guidance"
        });

        let (_value, exit_code) =
            run_loaded_plan(plan, None, executor).expect("run-plan completed");
        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");

        assert_eq!(exit_code, 0);
        assert_eq!(
            observed.workspace.mode,
            homeboy::core::agent_task::AgentTaskWorkspaceMode::Existing
        );
        assert_eq!(
            observed.workspace.root.as_deref(),
            Some("/tmp/homeboy-worktrees/wp-coding-agents@fix-179-homeboy-codebox-guidance")
        );
        assert_eq!(observed.workspace.slug.as_deref(), Some("wp-coding-agents"));
        assert!(observed.workspace.kind.is_none());
        assert!(observed.workspace.component_id.is_none());
        assert!(observed.workspace.branch.is_none());
        assert!(observed.workspace.base_ref.is_none());
        assert!(observed.workspace.task_url.is_none());
        assert!(observed.workspace.cleanup.is_none());
        assert!(observed.workspace.materialization.is_null());
    }

    #[test]
    fn run_plan_rejects_unresolved_component_worktree_until_core_primitive_exists() {
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("wp-coding-agents".to_string());
        plan.tasks[0].workspace.branch = Some("fix/179-homeboy-codebox-guidance".to_string());

        let error = run_loaded_plan(plan, None, CapturingExecutor::default())
            .expect_err("unresolved component worktree rejected");
        let message = error.to_string();

        assert!(message.contains("component-worktree workspace"));
        assert!(message.contains("Extra-Chill/homeboy#3362"));
    }

    #[test]
    fn promotion_source_resolves_completed_run_id() {
        with_temp_home(|| {
            let run_id = "run-promotion-source";

            run_loaded_plan(test_plan(), Some(run_id), InspectingExecutor::noop(run_id))
                .expect("run completed");

            let (raw, path) =
                review::read_promotion_source(run_id).expect("promotion source resolved");

            assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
            assert_eq!(
                path.as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str()),
                Some("aggregate.json")
            );
        });
    }

    #[test]
    fn promotion_source_reads_bare_json_file_path() {
        let file = tempfile::NamedTempFile::new().expect("source file");
        std::fs::write(
            file.path(),
            r#"{"schema":"homeboy/agent-task-aggregate/v1"}"#,
        )
        .expect("write source");

        let (raw, path) = review::read_promotion_source(&file.path().display().to_string())
            .expect("promotion source file resolved");

        assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
        assert_eq!(path.as_deref(), Some(file.path()));
    }

    #[test]
    fn review_reports_queued_run_without_chat_state() {
        with_temp_home(|| {
            agent_task_lifecycle::submit_plan(&test_plan(), Some("run-review-queued"))
                .expect("submitted");

            let (value, exit_code) = review::review(ReviewArgs {
                run_id: "run-review-queued".to_string(),
                to_worktree: None,
            })
            .expect("review loaded");

            assert_eq!(exit_code, 0);
            assert_eq!(value["schema"], "homeboy/agent-task-review/v1");
            assert_eq!(value["run_id"], "run-review-queued");
            assert_eq!(value["state"], "queued");
            assert_eq!(value["transport"]["chat_state_required"], false);
            assert!(value["aggregate_review"].is_null());
            assert_eq!(value["logs"]["events"][0]["state"], "queued");
            assert!(value["next_actions"][0]
                .as_str()
                .expect("next action")
                .contains("run-next"));
        });
    }

    #[test]
    fn review_reports_completed_aggregate_and_promotion_hints() {
        with_temp_home(|| {
            run_loaded_plan(
                test_plan(),
                Some("run-review-completed"),
                ApplyArtifactExecutor,
            )
            .expect("run completed");

            let (value, exit_code) = review::review(ReviewArgs {
                run_id: "run-review-completed".to_string(),
                to_worktree: Some("homeboy@fix-review-flow".to_string()),
            })
            .expect("review loaded");

            assert_eq!(exit_code, 0);
            assert_eq!(value["state"], "succeeded");
            assert_eq!(value["aggregate_review"]["summary"]["apply_candidates"], 1);
            assert_eq!(value["artifacts"]["artifacts"][0]["id"], "patch-a");
            assert_eq!(value["promotion_candidates"][0]["task_id"], "task-a");
            assert_eq!(value["promotion_candidates"][0]["artifact_id"], "patch-a");
            assert_eq!(value["promotion_candidates"][0]["ready"], true);
            assert_eq!(
                value["promotion_candidates"][0]["command"],
                json!([
                    "homeboy",
                    "agent-task",
                    "promote",
                    "run-review-completed",
                    "--task-id",
                    "task-a",
                    "--artifact-id",
                    "patch-a",
                    "--to-worktree",
                    "homeboy@fix-review-flow"
                ])
            );
            assert!(value["next_actions"][0]
                .as_str()
                .expect("next action")
                .contains("promotion_candidates"));
        });
    }

    struct InspectingExecutor {
        run_id: String,
        observed_status: Arc<Mutex<Option<AgentTaskRunRecord>>>,
    }

    impl InspectingExecutor {
        fn noop(run_id: &str) -> Self {
            Self {
                run_id: run_id.to_string(),
                observed_status: Arc::new(Mutex::new(None)),
            }
        }
    }

    impl AgentTaskExecutorAdapter for InspectingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let record =
                lifecycle_status(&self.run_id).expect("status exists before executor runs");
            *self
                .observed_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(record);

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

    #[derive(Default)]
    struct CapturingExecutor {
        observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
    }

    impl AgentTaskExecutorAdapter for CapturingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            *self
                .observed_request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());

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

    struct ApplyArtifactExecutor;

    impl AgentTaskExecutorAdapter for ApplyArtifactExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("produced patch".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch-a".to_string(),
                    kind: "patch".to_string(),
                    name: Some("changes.patch".to_string()),
                    path: Some("target/agent-task-review/changes.patch".to_string()),
                    url: None,
                    mime: Some("text/x-diff".to_string()),
                    size_bytes: Some(42),
                    sha256: Some("abc123".to_string()),
                    metadata: Value::Null,
                }],
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "transcript".to_string(),
                    uri: "target/agent-task-review/transcript.log".to_string(),
                    label: Some("transcript".to_string()),
                }],
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    fn with_temp_home(run: impl FnOnce()) {
        with_isolated_home(|_| run());
    }

    fn test_plan() -> AgentTaskPlan {
        AgentTaskPlan::new(
            "plan-a",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task-a".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: Some("fixture".to_string()),
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
