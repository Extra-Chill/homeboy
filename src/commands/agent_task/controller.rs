//! `agent-task controller` handlers: durable multi-agent loop controller
//! lifecycle, spec defaults, and the CLI dispatch bridge.

use serde_json::Value;

use homeboy::core::agent_task_loop_definition::{
    materialize_repo_loop_spec, AgentTaskLoopPolicyResultMaterialization,
    AgentTaskLoopSpecMaterializationRequest,
};
use homeboy::core::agent_tasks::controller_service as agent_task_controller_service;
use homeboy::core::agent_tasks::controller_service::{
    AgentTaskRepoLoopSpec, ControllerApplyEventRequest, ControllerDispatchHook,
    ControllerFromSpecRequest, ControllerInitRequest, ControllerMarkHumanReadyRequest,
    ControllerPlanRequest,
};
use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::scheduler::AgentTaskExecutorAdapter;
use homeboy::core::config;
use homeboy::core::proof::validate_proof_value;

use homeboy::core::agent_tasks::dispatch_service;

use super::super::agent_task_dispatch::{DispatchArgs, DispatchCoreArgs};
use super::super::CmdResult;
use super::args::{
    AgentTaskControllerApplyEventArgs, AgentTaskControllerArgs, AgentTaskControllerCommand,
    AgentTaskControllerFromSpecArgs, AgentTaskControllerMaterializeArgs,
    AgentTaskControllerPlanArgs, AgentTaskControllerRunArgs, AgentTaskControllerRunNextArgs,
    AgentTaskControllerValidateProofArgs,
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
        AgentTaskControllerCommand::Materialize(materialize_args) => {
            controller_materialize(materialize_args)
        }
        AgentTaskControllerCommand::ValidateProof(validate_args) => {
            controller_validate_proof(validate_args)
        }
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
        AgentTaskControllerCommand::Events(event_args) => apply_controller_event(event_args),
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

fn controller_validate_proof(args: AgentTaskControllerValidateProofArgs) -> CmdResult<Value> {
    let raw = config::read_json_spec_to_string(&args.input)?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "input",
            error.to_string(),
            Some(args.input.clone()),
            None,
        )
    })?;
    let report = validate_proof_value(value);
    let exit_code = if report.valid { 0 } else { 1 };
    Ok((command_json_value(report)?, exit_code))
}

pub(super) fn controller_materialize(args: AgentTaskControllerMaterializeArgs) -> CmdResult<Value> {
    let raw = config::read_json_spec_to_string(&args.spec)?;
    let mut spec: AgentTaskRepoLoopSpec = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "spec",
            error.to_string(),
            Some(args.spec.clone()),
            None,
        )
    })?;
    agent_task_controller_service::apply_spec_dispatch_defaults(&mut spec, &args.spec);
    let run_inputs = match args.inputs {
        Some(inputs) => {
            serde_json::from_str(&config::read_json_spec_to_string(&inputs)?).map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "inputs",
                    error.to_string(),
                    Some(inputs),
                    None,
                )
            })?
        }
        None => Value::Null,
    };
    let policy_results = args
        .policy_results
        .iter()
        .map(|policy_result| parse_policy_result(policy_result))
        .collect::<homeboy::core::Result<Vec<_>>>()?;
    let report = materialize_repo_loop_spec(AgentTaskLoopSpecMaterializationRequest {
        spec: &spec,
        run_inputs: &run_inputs,
        policy_results: &policy_results,
    })?;
    Ok((command_json_value(report)?, 0))
}

fn parse_policy_result(
    source: &str,
) -> homeboy::core::Result<AgentTaskLoopPolicyResultMaterialization> {
    let raw = config::read_json_spec_to_string(source)?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "policy-result",
            error.to_string(),
            Some(source.to_string()),
            None,
        )
    })?;
    AgentTaskLoopPolicyResultMaterialization::from_value(value, source)
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
    agent_task_controller_service::apply_spec_dispatch_defaults(&mut spec, &args.spec);
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
    agent_task_controller_service::apply_spec_dispatch_defaults(&mut spec, &args.spec);
    let report = agent_task_controller_service::init_from_spec(ControllerFromSpecRequest { spec })?;
    if !args.resume {
        return Ok((command_json_value(report)?, 0));
    }

    let (resume_report, exit_code) = controller_resume_with_executor(
        report.loop_id.clone(),
        ExtensionProviderAgentTaskExecutor::discover(),
        ControllerDispatchDefaults::from_from_spec_args(&args),
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

/// Bridge controller spawn-task `"dispatch"` requests into the CLI dispatch path.
#[derive(Clone)]
struct CliDispatchHook<E> {
    executor: E,
    defaults: ControllerDispatchDefaults,
}

#[derive(Clone, Default)]
struct ControllerDispatchDefaults {
    backend: Option<String>,
    selector: Option<String>,
    model: Option<String>,
}

impl ControllerDispatchDefaults {
    fn from_from_spec_args(args: &AgentTaskControllerFromSpecArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
        }
    }

    fn from_run_next_args(args: &AgentTaskControllerRunNextArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
        }
    }

    fn from_run_args(args: &AgentTaskControllerRunArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
        }
    }

    fn apply(&self, args: &mut DispatchArgs) {
        if args.backend.is_none() {
            args.backend = self.backend.clone();
        }
        if args.selector.is_none() {
            args.selector = self.selector.clone();
        }
        if args.model.is_none() {
            args.model = self.model.clone();
        }
    }
}

impl<E> ControllerDispatchHook for CliDispatchHook<E>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    fn dispatch(&self, request: &Value) -> homeboy::core::Result<(Value, i32)> {
        let mut dispatch_args = dispatch_args_from_controller_request(request)?;
        self.defaults.apply(&mut dispatch_args);
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
        concurrency: optional_usize(dispatch, "concurrency")?.unwrap_or(1),
        run_id: optional_string(dispatch, "run_id"),
        core: DispatchCoreArgs {
            tasks_json: optional_string(dispatch, "tasks_json"),
            provider_config: optional_string(dispatch, "provider_config"),
            client_context: optional_string(dispatch, "client_context"),
            attempts: optional_u32(dispatch, "attempts")?.unwrap_or(1),
            queue_only: optional_bool(dispatch, "queue_only").unwrap_or(false),
        },
    })
}

pub(super) fn apply_controller_event(args: AgentTaskControllerApplyEventArgs) -> CmdResult<Value> {
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
    let defaults = ControllerDispatchDefaults::from_run_next_args(&args);
    controller_run_next_with_executor_and_defaults(
        args.loop_id,
        ExtensionProviderAgentTaskExecutor::discover(),
        defaults,
    )
}

fn controller_run_action(args: AgentTaskControllerRunArgs) -> CmdResult<Value> {
    let defaults = ControllerDispatchDefaults::from_run_args(&args);
    controller_run_action_with_executor_and_defaults(
        args.loop_id,
        args.action_id,
        ExtensionProviderAgentTaskExecutor::discover(),
        defaults,
    )
}

fn controller_resume(args: AgentTaskControllerRunNextArgs) -> CmdResult<Value> {
    let defaults = ControllerDispatchDefaults::from_run_next_args(&args);
    controller_resume_with_executor_and_defaults(
        args.loop_id,
        ExtensionProviderAgentTaskExecutor::discover(),
        defaults,
    )
}

#[cfg(test)]
pub(super) fn controller_run_next_with_executor<E>(loop_id: String, executor: E) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    controller_run_next_with_executor_and_defaults(
        loop_id,
        executor,
        ControllerDispatchDefaults::default(),
    )
}

fn controller_run_next_with_executor_and_defaults<E>(
    loop_id: String,
    executor: E,
    defaults: ControllerDispatchDefaults,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
        defaults,
    };
    let result = agent_task_controller_service::run_next(&loop_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

#[cfg(test)]
pub(super) fn controller_run_action_with_executor<E>(
    loop_id: String,
    action_id: String,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    controller_run_action_with_executor_and_defaults(
        loop_id,
        action_id,
        executor,
        ControllerDispatchDefaults::default(),
    )
}

fn controller_run_action_with_executor_and_defaults<E>(
    loop_id: String,
    action_id: String,
    executor: E,
    defaults: ControllerDispatchDefaults,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
        defaults,
    };
    let result =
        agent_task_controller_service::run_action(&loop_id, &action_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}

fn controller_resume_with_executor<E>(
    loop_id: String,
    executor: E,
    defaults: ControllerDispatchDefaults,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    controller_resume_with_executor_and_defaults(loop_id, executor, defaults)
}

fn controller_resume_with_executor_and_defaults<E>(
    loop_id: String,
    executor: E,
    defaults: ControllerDispatchDefaults,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
        defaults,
    };
    let result = agent_task_controller_service::resume(&loop_id, executor, &dispatch)?;
    Ok((command_json_value(result.value)?, result.exit_code))
}
