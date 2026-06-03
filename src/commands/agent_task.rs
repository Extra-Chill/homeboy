use clap::{Args, Subcommand};
use serde_json::Value;

use homeboy::core::agent_task_lifecycle;
use homeboy::core::agent_task_promotion::{promote, AgentTaskPromotionOptions};
use homeboy::core::agent_task_provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_task_scheduler::{
    AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskScheduler,
};
use homeboy::core::config;

use super::{CmdResult, GlobalArgs};

#[derive(Args, Debug)]
pub struct AgentTaskArgs {
    #[command(subcommand)]
    pub command: AgentTaskCommand,
}

#[derive(Subcommand, Debug)]
pub enum AgentTaskCommand {
    /// Run an agent-task plan through extension-declared executor providers.
    RunPlan(RunPlanArgs),
    /// Execute a previously submitted durable agent-task run.
    Run(StatusArgs),
    /// Persist an agent-task plan and return a durable run id without executing it.
    Submit(SubmitArgs),
    /// Read durable agent-task run status.
    Status(StatusArgs),
    /// Read durable agent-task run scheduler events.
    Logs(StatusArgs),
    /// List artifacts and evidence refs recorded for a completed run.
    Artifacts(StatusArgs),
    /// Promote a completed generic patch artifact into a managed worktree.
    Promote(PromoteArgs),
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

pub fn run(args: AgentTaskArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskCommand::RunPlan(run_args) => run_plan(run_args),
        AgentTaskCommand::Run(status_args) => run_submitted(status_args),
        AgentTaskCommand::Submit(submit_args) => submit(submit_args),
        AgentTaskCommand::Status(status_args) => status(status_args),
        AgentTaskCommand::Logs(status_args) => logs(status_args),
        AgentTaskCommand::Artifacts(status_args) => artifacts(status_args),
        AgentTaskCommand::Promote(promote_args) => promote_artifact(promote_args),
        AgentTaskCommand::Providers => providers(),
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
    plan: AgentTaskPlan,
    record_run_id: Option<&str>,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter,
{
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
    let plan = agent_task_lifecycle::load_plan(&args.run_id)?;
    agent_task_lifecycle::mark_running(&args.run_id)?;
    let scheduler = AgentTaskScheduler::new(ExtensionProviderAgentTaskExecutor::discover());
    let aggregate = scheduler.run(plan.clone());
    agent_task_lifecycle::record_run_aggregate(&args.run_id, &plan, &aggregate)?;
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

fn promote_artifact(args: PromoteArgs) -> CmdResult<Value> {
    let (raw, source_path) = read_promotion_source(&args.source)?;
    let report = promote(AgentTaskPromotionOptions {
        source: raw,
        source_path,
        to_worktree: args.to_worktree,
        task_id: args.task_id,
        artifact_id: args.artifact_id,
        dry_run: args.dry_run,
        verify: args.verify,
    })?;

    Ok((serde_json::to_value(report).unwrap_or(Value::Null), 0))
}

fn read_promotion_source(
    spec: &str,
) -> homeboy::core::Result<(String, Option<std::path::PathBuf>)> {
    if let Ok((raw, path)) = agent_task_lifecycle::aggregate_source(spec) {
        return Ok((raw, Some(path)));
    }

    Ok((
        config::read_json_spec_to_string(spec)?,
        source_spec_path(spec),
    ))
}

fn source_spec_path(spec: &str) -> Option<std::path::PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(std::path::PathBuf::from(
        spec.strip_prefix('@').unwrap_or(spec),
    ))
}

fn providers() -> CmdResult<Value> {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    Ok((
        serde_json::to_value(executor.providers()).unwrap_or(Value::Null),
        0,
    ))
}

fn read_plan(spec: &str) -> homeboy::core::Result<AgentTaskPlan> {
    let raw = config::read_json_spec_to_string(spec)?;
    serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some("agent-task plan".to_string()),
            Some(raw.clone()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
        AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_OUTCOME_SCHEMA,
        AGENT_TASK_REQUEST_SCHEMA,
    };
    use homeboy::core::agent_task_lifecycle::{
        status as lifecycle_status, AgentTaskRunRecord, AgentTaskRunState,
    };
    use homeboy::core::agent_task_scheduler::{AgentTaskExecutionContext, AgentTaskState};
    use serde_json::Value;
    use std::sync::{Arc, Mutex, OnceLock};

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

    struct InspectingExecutor {
        run_id: String,
        observed_status: Arc<Mutex<Option<AgentTaskRunRecord>>>,
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
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    fn with_temp_home(run: impl FnOnce()) {
        let lock = test_home_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = tempfile::tempdir().expect("temp home");
        std::env::set_var("HOME", home.path());
        run();
        drop(lock);
    }

    fn test_home_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
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
