//! `agent-task controller` handlers: durable multi-agent loop controller
//! lifecycle, spec defaults, and the CLI dispatch bridge.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use homeboy::core::agent_tasks::controller_service as agent_task_controller_service;
use homeboy::core::agent_tasks::controller_service::{
    AgentTaskRepoLoopSpec, ControllerApplyEventRequest, ControllerDispatchHook,
    ControllerFromSpecRequest, ControllerInitRequest, ControllerMarkHumanReadyRequest,
    ControllerPlanRequest,
};
use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::scheduler::AgentTaskExecutorAdapter;
use homeboy::core::config;

use homeboy::core::agent_tasks::dispatch_service;

use super::super::agent_task_dispatch::DispatchArgs;
use super::super::CmdResult;
use super::args::{
    AgentTaskControllerApplyEventArgs, AgentTaskControllerArgs, AgentTaskControllerCommand,
    AgentTaskControllerFromSpecArgs, AgentTaskControllerPlanArgs, AgentTaskControllerRunArgs,
    AgentTaskControllerRunNextArgs,
};
use super::command_json_value;

pub(super) fn controller(args: AgentTaskControllerArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskControllerCommand::Init(init_args) => {
            let record = agent_task_controller_service::init(ControllerInitRequest {
                loop_id: init_args.loop_id,
                phase: init_args.phase,
                config_version: init_args.config_version,
            })?;
            Ok((command_json_value(record)?, 0))
        }
        AgentTaskControllerCommand::FromSpec(spec_args) => controller_from_spec(spec_args),
        AgentTaskControllerCommand::Plan(plan_args) => controller_plan(plan_args),
        AgentTaskControllerCommand::Status(status_args) => {
            let report = homeboy::core::agent_tasks::loop_controller::controller_status_report(
                &status_args.loop_id,
            )?;
            Ok((command_json_value(report)?, 0))
        }
        AgentTaskControllerCommand::List => {
            let report = agent_task_controller_service::list()?;
            Ok((command_json_value(report)?, 0))
        }
        AgentTaskControllerCommand::ApplyEvent(event_args) => apply_controller_event(event_args),
        AgentTaskControllerCommand::RunNext(run_args) => controller_run_next(run_args),
        AgentTaskControllerCommand::Run(run_args) => controller_run_action(run_args),
        AgentTaskControllerCommand::Resume(run_args) => controller_resume(run_args),
        AgentTaskControllerCommand::MarkHumanReady(ready_args) => {
            let record =
                agent_task_controller_service::mark_human_ready(ControllerMarkHumanReadyRequest {
                    loop_id: ready_args.loop_id,
                    entity_id: ready_args.entity_id,
                    reason: ready_args.reason,
                })?;
            Ok((command_json_value(record)?, 0))
        }
    }
}

fn controller_plan(args: AgentTaskControllerPlanArgs) -> CmdResult<Value> {
    let raw = config::read_json_spec_to_string(&args.spec)?;
    let mut spec: AgentTaskRepoLoopSpec = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "spec",
            error.to_string(),
            Some(args.spec.clone()),
            None,
        )
    })?;
    apply_from_spec_dispatch_defaults(&mut spec, &args.spec);
    let report = agent_task_controller_service::plan_from_spec(ControllerPlanRequest { spec })?;
    Ok((command_json_value(report)?, 0))
}

pub(super) fn controller_from_spec(args: AgentTaskControllerFromSpecArgs) -> CmdResult<Value> {
    let raw = config::read_json_spec_to_string(&args.spec)?;
    let mut spec: AgentTaskRepoLoopSpec = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "spec",
            error.to_string(),
            Some(args.spec.clone()),
            None,
        )
    })?;
    apply_from_spec_dispatch_defaults(&mut spec, &args.spec);
    let report = agent_task_controller_service::init_from_spec(ControllerFromSpecRequest { spec })?;
    if !args.resume {
        return Ok((command_json_value(report)?, 0));
    }

    let (resume_report, exit_code) = controller_resume_with_executor(
        report.loop_id.clone(),
        ExtensionProviderAgentTaskExecutor::discover(),
    )?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-loop-controller-from-spec-and-resume-result/v1",
            "from_spec": report,
            "resume": resume_report,
        }),
        exit_code,
    ))
}

pub(super) fn apply_from_spec_dispatch_defaults(spec: &mut AgentTaskRepoLoopSpec, spec_arg: &str) {
    apply_from_spec_dispatch_defaults_with_cwd(spec, spec_arg, || std::env::current_dir().ok());
}

pub(super) fn apply_from_spec_dispatch_defaults_with_cwd(
    spec: &mut AgentTaskRepoLoopSpec,
    spec_arg: &str,
    current_dir: impl FnOnce() -> Option<PathBuf>,
) {
    let Some(root) = dispatch_root_for_spec(spec_arg, current_dir) else {
        return;
    };
    apply_dispatch_defaults_for_root(spec, root);
}

fn dispatch_root_for_spec(
    spec_arg: &str,
    current_dir: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    let Some(spec_path) = spec_file_path(spec_arg) else {
        return current_dir().and_then(|path| git_root_for_path(&path));
    };
    if let Some(root) = git_root_for_spec(&spec_path) {
        return Some(root);
    }
    current_dir().and_then(|path| git_root_for_path(&path))
}

fn apply_dispatch_defaults_for_root(spec: &mut AgentTaskRepoLoopSpec, root: PathBuf) {
    let repo = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    if repo.is_empty() {
        return;
    }

    let metadata = match &mut spec.metadata {
        Value::Object(metadata) => metadata,
        _ => {
            spec.metadata = serde_json::json!({});
            spec.metadata.as_object_mut().expect("metadata object")
        }
    };
    let defaults = metadata
        .entry("dispatch_defaults".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Value::Object(defaults) = defaults else {
        return;
    };
    defaults
        .entry("cwd".to_string())
        .or_insert_with(|| Value::String(root.display().to_string()));
    defaults
        .entry("repo".to_string())
        .or_insert_with(|| Value::String(repo));
}

fn spec_file_path(spec_arg: &str) -> Option<PathBuf> {
    if spec_arg == "-" || spec_arg.trim_start().starts_with('{') {
        return None;
    }
    let path = spec_arg.strip_prefix('@').unwrap_or(spec_arg);
    let path = PathBuf::from(path);
    path.is_file().then_some(path)
}

fn git_root_for_spec(spec_path: &Path) -> Option<PathBuf> {
    let dir = spec_path.parent()?;
    git_root_for_path(dir)
}

fn git_root_for_path(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

/// Bridge controller spawn-task `"dispatch"` requests into the CLI dispatch path.
#[derive(Clone)]
struct CliDispatchHook<E> {
    executor: E,
}

impl<E> ControllerDispatchHook for CliDispatchHook<E>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    fn dispatch(&self, request: &Value) -> homeboy::core::Result<(Value, i32)> {
        let dispatch_args = dispatch_args_from_controller_request(request)?;
        dispatch_service::run_dispatch_command(dispatch_args.into(), self.executor.clone())
    }
}

pub(super) fn dispatch_args_from_controller_request(
    request: &Value,
) -> homeboy::core::Result<DispatchArgs> {
    use agent_task_controller_service::{
        optional_bool, optional_string, optional_string_array, optional_u32, optional_usize,
    };
    let dispatch = request.get("dispatch").unwrap_or(request);
    Ok(DispatchArgs {
        prompt: optional_string(dispatch, "prompt"),
        tasks: optional_string_array(dispatch, "tasks")?,
        tasks_json: optional_string(dispatch, "tasks_json"),
        cwd: optional_string(dispatch, "cwd")
            .or_else(|| optional_string(request, "cwd"))
            .or_else(|| optional_string(request, "workspace_root")),
        workspace: optional_string(dispatch, "workspace")
            .or_else(|| optional_string(request, "workspace")),
        repo: optional_string(dispatch, "repo").or_else(|| optional_string(request, "repo")),
        task_url: optional_string(dispatch, "task_url"),
        backend: optional_string(dispatch, "backend"),
        selector: optional_string(dispatch, "selector"),
        model: optional_string(dispatch, "model"),
        required_capabilities: optional_string_array(dispatch, "required_capabilities")?,
        secret_env: optional_string_array(dispatch, "secret_env")?,
        provider_config: optional_string(dispatch, "provider_config"),
        client_context: optional_string(dispatch, "client_context"),
        concurrency: optional_usize(dispatch, "concurrency")?.unwrap_or(1),
        attempts: optional_u32(dispatch, "attempts")?.unwrap_or(1),
        run_id: optional_string(dispatch, "run_id"),
        queue_only: optional_bool(dispatch, "queue_only").unwrap_or(false),
    })
}

fn apply_controller_event(args: AgentTaskControllerApplyEventArgs) -> CmdResult<Value> {
    let payload = match args.payload {
        Some(spec) => {
            serde_json::from_str(&config::read_json_spec_to_string(&spec)?).map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "payload",
                    error.to_string(),
                    Some(spec),
                    None,
                )
            })?
        }
        None => Value::Null,
    };
    let report = agent_task_controller_service::apply_event(ControllerApplyEventRequest {
        loop_id: args.loop_id,
        event_type: args.event_type,
        event_id: args.event_id,
        event_key: args.event_key,
        entity_id: args.entity_id,
        payload,
    })?;
    Ok((command_json_value(report)?, 0))
}

fn controller_run_next(args: AgentTaskControllerRunNextArgs) -> CmdResult<Value> {
    controller_run_next_with_executor(args.loop_id, ExtensionProviderAgentTaskExecutor::discover())
}

fn controller_run_action(args: AgentTaskControllerRunArgs) -> CmdResult<Value> {
    controller_run_action_with_executor(
        args.loop_id,
        args.action_id,
        ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn controller_resume(args: AgentTaskControllerRunNextArgs) -> CmdResult<Value> {
    controller_resume_with_executor(args.loop_id, ExtensionProviderAgentTaskExecutor::discover())
}

pub(super) fn controller_run_next_with_executor<E>(loop_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
    };
    let result = agent_task_controller_service::run_next(&loop_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

pub(super) fn controller_run_action_with_executor<E>(
    loop_id: String,
    action_id: String,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
    };
    let result =
        agent_task_controller_service::run_action(&loop_id, &action_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

pub(super) fn controller_resume_with_executor<E>(loop_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
    };
    let result = agent_task_controller_service::resume(&loop_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}
