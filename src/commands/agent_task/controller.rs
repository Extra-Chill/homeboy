//! `agent-task controller` handlers: durable multi-agent loop controller
//! lifecycle, spec defaults, and the CLI dispatch bridge.

use serde_json::Value;

use homeboy::core::agent_task_controller_service::{
    build_run_failure_summary, init_from_spec_for_resume_with_resolution,
    ControllerResumeStateResolution,
};
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
use homeboy::core::agent_tasks::loop_controller::AgentTaskLoopPolicyAction;

use super::super::CmdResult;
use super::args::{
    AgentTaskControllerApplyEventArgs, AgentTaskControllerArgs, AgentTaskControllerCommand,
    AgentTaskControllerFromSpecArgs, AgentTaskControllerMaterializeArgs,
    AgentTaskControllerPlanArgs, AgentTaskControllerRunArgs, AgentTaskControllerRunFromSpecArgs,
    AgentTaskControllerRunNextArgs, AgentTaskControllerValidateProofArgs, AgentTaskLoopArgs,
    AgentTaskLoopCommand, AgentTaskLoopDefineArgs, AgentTaskLoopResumeArgs,
    AgentTaskLoopStatusArgs,
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
        AgentTaskControllerCommand::RunFromSpec(run_args) => controller_run_from_spec(run_args),
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

pub(super) fn loop_command(args: AgentTaskLoopArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskLoopCommand::Define(define_args) => loop_define(define_args),
        AgentTaskLoopCommand::Status(status_args) => loop_status(status_args),
        AgentTaskLoopCommand::Resume(resume_args) => loop_resume(resume_args),
        AgentTaskLoopCommand::Stop(status_args) => loop_stop(status_args),
    }
}

fn loop_define(args: AgentTaskLoopDefineArgs) -> CmdResult<Value> {
    if !args.on && !args.off {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "on/off",
            "agent-task loop define requires an explicit --on or --off state",
            Some(args.spec.clone()),
            None,
        ));
    }
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
    stamp_loop_runtime_metadata(&mut spec.metadata, !args.off, args.revolution_limit, false)?;

    let report = agent_task_controller_service::init_from_spec(ControllerFromSpecRequest { spec })?;
    if !args.resume {
        return Ok((
            command_json_value(serde_json::json!({
                "schema": "homeboy/agent-task-loop-define-result/v1",
                "loop_id": report.loop_id.clone(),
                "on": !args.off,
                "revolution_limit": args.revolution_limit,
                "resume": null,
                "controller": report.controller.clone(),
                "actions": report.actions.clone(),
            }))?,
            0,
        ));
    }
    if args.off {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "resume",
            "agent-task loop define --resume requires --on because off loops do not execute handoffs",
            Some(report.loop_id),
            None,
        ));
    }

    let defaults = ControllerDispatchDefaults::from_loop_define_args(&args);
    let (resume_report, exit_code) = loop_resume_with_executor(
        report.loop_id.clone(),
        args.revolution_limit,
        ExtensionProviderAgentTaskExecutor::discover(),
        defaults,
    )?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-loop-define-result/v1",
            "loop_id": report.loop_id.clone(),
            "on": true,
            "revolution_limit": args.revolution_limit,
            "controller": report.controller.clone(),
            "actions": report.actions.clone(),
            "resume": resume_report,
        }),
        exit_code,
    ))
}

fn loop_status(args: AgentTaskLoopStatusArgs) -> CmdResult<Value> {
    let report =
        homeboy::core::agent_tasks::loop_controller::controller_status_report(&args.loop_id)?;
    Ok((
        command_json_value(serde_json::json!({
            "schema": "homeboy/agent-task-loop-status-result/v1",
            "runtime": loop_runtime_metadata(&report.controller.metadata),
            "status": report,
        }))?,
        0,
    ))
}

fn loop_resume(args: AgentTaskLoopResumeArgs) -> CmdResult<Value> {
    let defaults = ControllerDispatchDefaults::from_loop_resume_args(&args);
    loop_resume_with_executor(
        args.loop_id,
        args.revolution_limit,
        ExtensionProviderAgentTaskExecutor::discover(),
        defaults,
    )
}

fn loop_resume_with_executor<E>(
    loop_id: String,
    revolution_limit: Option<u32>,
    executor: E,
    defaults: ControllerDispatchDefaults,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    let mut record = homeboy::core::agent_tasks::loop_controller::load_controller(&loop_id)?;
    let runtime = loop_runtime_metadata(&record.metadata);
    if !runtime["on"].as_bool().unwrap_or(true) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "loop_id",
            "agent-task loop resume requires an on loop; run `agent-task loop define --on` or update the loop state first",
            Some(loop_id),
            None,
        ));
    }
    let limit = revolution_limit.or_else(|| {
        runtime["revolution_limit"]
            .as_u64()
            .map(|value| value as u32)
    });
    let current = runtime["revolutions"].as_u64().unwrap_or(0) as u32;
    if limit.is_some_and(|limit| current >= limit) {
        return Ok((
            serde_json::json!({
                "schema": "homeboy/agent-task-loop-resume-result/v1",
                "loop_id": record.loop_id,
                "claimed": false,
                "stopped_reason": "revolution_limit_reached",
                "runtime": runtime,
                "controller": record,
            }),
            0,
        ));
    }

    stamp_record_loop_runtime_metadata(&mut record, true, limit, true)?;
    homeboy::core::agent_tasks::loop_controller::write_controller(&record)?;
    let resumed_loop_id = loop_id.clone();
    let (value, exit_code) =
        controller_resume_with_executor_and_defaults(loop_id, executor, defaults)?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-loop-resume-result/v1",
            "runtime": loop_runtime_metadata(&homeboy::core::agent_tasks::loop_controller::load_controller(&resumed_loop_id)?.metadata),
            "resume": value,
        }),
        exit_code,
    ))
}

fn loop_stop(args: AgentTaskLoopStatusArgs) -> CmdResult<Value> {
    let mut record = homeboy::core::agent_tasks::loop_controller::load_controller(&args.loop_id)?;
    let runtime = loop_runtime_metadata(&record.metadata);
    let limit = runtime["revolution_limit"]
        .as_u64()
        .map(|value| value as u32);
    stamp_record_loop_runtime_metadata(&mut record, false, limit, false)?;
    homeboy::core::agent_tasks::loop_controller::write_controller(&record)?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-loop-stop-result/v1",
            "loop_id": record.loop_id,
            "on": false,
            "runtime": loop_runtime_metadata(&record.metadata),
            "controller": record,
        }),
        0,
    ))
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
    let materialized = materialize_controller_spec(
        &args.spec,
        args.inputs.as_deref(),
        &args.policy_results,
        None,
    )?;
    Ok((materialized.value, 0))
}

fn controller_run_from_spec(args: AgentTaskControllerRunFromSpecArgs) -> CmdResult<Value> {
    controller_run_from_spec_with_executor(args, ExtensionProviderAgentTaskExecutor::discover())
}

#[cfg(test)]
pub(super) fn controller_run_from_spec_with_test_executor<E>(
    args: AgentTaskControllerRunFromSpecArgs,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    controller_run_from_spec_with_executor(args, executor)
}

fn controller_run_from_spec_with_executor<E>(
    args: AgentTaskControllerRunFromSpecArgs,
    executor: E,
) -> CmdResult<Value>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    if args.max_actions == 0 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "max-actions",
            "agent-task controller run-from-spec requires --max-actions greater than zero",
            Some(args.spec),
            None,
        ));
    }

    let defaults = ControllerDispatchDefaults::from_run_from_spec_args(&args);
    let materialized = materialize_controller_spec(
        &args.spec,
        args.inputs.as_deref(),
        &args.policy_results,
        Some(&defaults),
    )?;
    let from_spec = init_from_spec_for_resume_with_resolution(
        ControllerFromSpecRequest {
            spec: materialized.spec.clone(),
        },
        resume_state_resolution(
            args.reconcile_stale,
            args.replace,
            args.fork,
            args.resume_existing,
        ),
    )?;
    let dispatch = CliDispatchHook {
        executor: executor.clone(),
        defaults,
    };
    let resume_result = agent_task_controller_service::resume_with_options(
        &from_spec.loop_id,
        executor,
        &dispatch,
        agent_task_controller_service::ControllerResumeOptions {
            max_actions: args.max_actions as usize,
            stop_on_terminal: true,
        },
    )?;
    let exit_code = resume_result.exit_code;
    let stopped_reason = resume_result.value.stopped_reason.clone();
    let results = resume_result.value.results;

    let status =
        homeboy::core::agent_tasks::loop_controller::controller_status_report(&from_spec.loop_id)?;

    // On a terminal failure, normalize the nested provider/runtime failures into
    // a compact root-cause `failure_summary` with durable evidence refs so
    // operators never have to hand-traverse the envelope below it (#6220).
    let failure_summary = if exit_code != 0 {
        let status_value = serde_json::to_value(&status)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
        Some(build_run_failure_summary(
            &from_spec.loop_id,
            &stopped_reason,
            &results,
            &status_value,
        ))
    } else {
        None
    };

    let mut envelope = serde_json::json!({
        "schema": "homeboy/agent-task-loop-controller-run-from-spec-result/v1",
        "loop_id": from_spec.loop_id.clone(),
        "max_actions": args.max_actions,
        "stopped_reason": stopped_reason,
        "materialization": materialized.value,
        "from_spec": from_spec,
        "results": results,
        "status": status,
    });
    if let Some(summary) = failure_summary {
        let summary_value = serde_json::to_value(summary)
            .map_err(|error| homeboy::core::Error::internal_json(error.to_string(), None))?;
        if let Value::Object(map) = &mut envelope {
            map.insert("failure_summary".to_string(), summary_value);
        }
    }
    Ok((envelope, exit_code))
}

fn materialize_controller_spec(
    spec_source: &str,
    inputs_source: Option<&str>,
    policy_result_sources: &[String],
    dispatch_defaults: Option<&ControllerDispatchDefaults>,
) -> homeboy::core::Result<MaterializedControllerSpec> {
    let source = agent_task_controller_service::load_materialize_spec_source(spec_source)?;
    let mut spec = source.spec;
    agent_task_controller_service::apply_spec_dispatch_defaults(&mut spec, spec_source);
    if let Some(defaults) = dispatch_defaults {
        defaults.apply_to_spec(&mut spec);
    }
    let run_inputs = match inputs_source {
        Some(inputs) => {
            serde_json::from_str(&config::read_json_spec_to_string(inputs)?).map_err(|error| {
                homeboy::core::Error::validation_invalid_argument(
                    "inputs",
                    error.to_string(),
                    Some(inputs.to_string()),
                    None,
                )
            })?
        }
        None => Value::Null,
    };
    let policy_results = policy_result_sources
        .iter()
        .map(|policy_result| parse_policy_result(policy_result))
        .collect::<homeboy::core::Result<Vec<_>>>()?;
    let report = materialize_repo_loop_spec(AgentTaskLoopSpecMaterializationRequest {
        spec: &spec,
        run_inputs: &run_inputs,
        policy_results: &policy_results,
    })?;
    let mut value = command_json_value(&report)?;
    if let Some(generator_evidence) = source.generator_evidence {
        if let Value::Object(map) = &mut value {
            map.insert("generator_evidence".to_string(), generator_evidence);
        }
    }
    Ok(MaterializedControllerSpec {
        spec: report.spec,
        value,
    })
}

struct MaterializedControllerSpec {
    spec: AgentTaskRepoLoopSpec,
    value: Value,
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

/// Translate the mutually exclusive `--replace`/`--fork`/`--resume-existing`
/// flags into a controller stale-state resolution. Clap enforces exclusivity,
/// so at most one is set; none selected falls back to the guarded default.
fn resume_state_resolution(
    reconcile_stale: bool,
    replace: bool,
    fork: bool,
    resume_existing: bool,
) -> ControllerResumeStateResolution {
    if reconcile_stale {
        ControllerResumeStateResolution::ReconcileStale
    } else if replace {
        ControllerResumeStateResolution::Replace
    } else if fork {
        ControllerResumeStateResolution::Fork
    } else if resume_existing {
        ControllerResumeStateResolution::ResumeExisting
    } else {
        ControllerResumeStateResolution::Guard
    }
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
    let defaults = ControllerDispatchDefaults::from_from_spec_args(&args);
    defaults.apply_to_spec(&mut spec);
    if args.doctor {
        return controller_from_spec_doctor(spec, &args);
    }
    let report = if args.resume {
        init_from_spec_for_resume_with_resolution(
            ControllerFromSpecRequest { spec },
            resume_state_resolution(false, args.replace, args.fork, args.resume_existing),
        )?
    } else {
        agent_task_controller_service::init_from_spec(ControllerFromSpecRequest { spec })?
    };
    if !args.resume {
        return Ok((command_json_value(report)?, 0));
    }

    let (resume_report, exit_code) = controller_resume_with_executor(
        report.loop_id.clone(),
        ExtensionProviderAgentTaskExecutor::discover(),
        defaults,
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

fn controller_from_spec_doctor(
    spec: AgentTaskRepoLoopSpec,
    args: &AgentTaskControllerFromSpecArgs,
) -> CmdResult<Value> {
    let plan = agent_task_controller_service::plan_from_spec(ControllerPlanRequest { spec })?;
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    let mut checks = vec![doctor_check(
        "controller.spec",
        "ok",
        "Controller spec parsed and compiled without writing state",
        None,
        serde_json::json!({ "loop_id": plan.loop_id, "action_count": plan.actions.len() }),
    )];
    let defaults = ControllerDispatchDefaults::from_from_spec_args(args);

    for action in &plan.actions {
        match &action.action {
            AgentTaskLoopPolicyAction::SpawnTask { request, .. } => {
                checks.extend(doctor_dispatch_request_checks(
                    &action.action_id,
                    request,
                    &defaults,
                    &executor,
                ));
            }
            AgentTaskLoopPolicyAction::FanOut {
                request_template, ..
            } => {
                checks.extend(doctor_dispatch_request_checks(
                    &action.action_id,
                    request_template,
                    &defaults,
                    &executor,
                ));
            }
            _ => {}
        }
    }

    let ok = checks
        .iter()
        .all(|check| check.get("status").and_then(Value::as_str) != Some("error"));
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-loop-controller-doctor-result/v1",
            "ok": ok,
            "loop_id": plan.loop_id,
            "checks": checks,
            "run_command": format!("homeboy agent-task controller from-spec {} --resume", args.spec),
        }),
        if ok { 0 } else { 1 },
    ))
}

fn doctor_dispatch_request_checks(
    action_id: &str,
    request: &Value,
    defaults: &ControllerDispatchDefaults,
    executor: &ExtensionProviderAgentTaskExecutor,
) -> Vec<Value> {
    let mut checks = Vec::new();
    let mode = request
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("dispatch");
    if mode != "dispatch" {
        checks.push(doctor_check(
            format!("controller.action.{action_id}.dispatch"),
            "warning",
            format!("Controller action uses unsupported preflight mode '{mode}'"),
            Some("Only generic dispatch-mode controller actions are preflighted by this doctor"),
            serde_json::json!({ "action_id": action_id, "mode": mode }),
        ));
        return checks;
    }

    let command = match agent_task_controller_service::controller_request_dispatch_command(
        request,
        &defaults.to_overrides(),
    ) {
        Ok(command) => command,
        Err(error) => {
            checks.push(doctor_check(
                format!("controller.action.{action_id}.dispatch.parse"),
                "error",
                format!("Controller dispatch request is invalid: {}", error.message),
                Some("Fix the action's generic dispatch request shape in the controller spec"),
                serde_json::json!({ "action_id": action_id, "details": error.details }),
            ));
            return checks;
        }
    };

    let dispatch_request = match dispatch_service::resolve_dispatch_request(command) {
        Ok(request) => request,
        Err(error) => {
            checks.push(doctor_check(
                format!("controller.action.{action_id}.dispatch.resolve"),
                "error",
                format!("Dispatch prerequisites are incomplete: {}", error.message),
                Some("Declare a generic dispatch backend/default backend, selector, workspace, and prompt inputs before running the controller"),
                serde_json::json!({ "action_id": action_id, "details": error.details }),
            ));
            return checks;
        }
    };

    match dispatch_service::build_dispatch_plan(&dispatch_request) {
        Ok(mut dispatch_plan) => {
            homeboy::core::agent_task_provider::apply_provider_runner_secret_env_contracts(
                &mut dispatch_plan,
            );
            if let Err(error) = dispatch_service::preflight_dispatch_provider_secrets(&dispatch_plan)
            {
                checks.push(doctor_check(
                    format!("controller.action.{action_id}.dispatch.secrets"),
                    "error",
                    format!("Required provider secrets are missing: {}", error.message),
                    Some("Configure the declared secret env values or remove the generic secret requirement from the provider/config"),
                    serde_json::json!({ "action_id": action_id, "details": error.details }),
                ));
            }
            checks.push(provider_availability_check(
                action_id,
                &dispatch_request.backend,
                dispatch_request.selector.as_deref(),
                &dispatch_request.required_capabilities,
                executor,
            ));
        }
        Err(error) => checks.push(doctor_check(
            format!("controller.action.{action_id}.dispatch.plan"),
            "error",
            format!("Dispatch plan could not be built: {}", error.message),
            Some("Fix generic workspace/materialization inputs such as cwd, workspace, repo, provider config, or task prompt inputs"),
            serde_json::json!({ "action_id": action_id, "details": error.details }),
        )),
    }

    checks
}

fn provider_availability_check(
    action_id: &str,
    backend: &str,
    selector: Option<&str>,
    required_capabilities: &[String],
    executor: &ExtensionProviderAgentTaskExecutor,
) -> Value {
    if backend == "fixture" {
        return doctor_check(
            format!("controller.action.{action_id}.provider"),
            "ok",
            "Fixture provider is available for deterministic local execution",
            None,
            serde_json::json!({ "action_id": action_id, "backend": backend }),
        );
    }

    let matches: Vec<_> = executor
        .providers()
        .iter()
        .filter(|provider| {
            provider.backend == backend || provider.extension_id.as_deref() == Some(backend)
        })
        .collect();
    let selected = selector
        .and_then(|selector| {
            matches
                .iter()
                .find(|provider| provider.id == selector)
                .copied()
        })
        .or_else(|| (matches.len() == 1).then(|| matches[0]));

    let Some(provider) = selected else {
        let available_provider_ids: Vec<_> =
            matches.iter().map(|provider| provider.id.clone()).collect();
        let remediation = if matches.is_empty() {
            "Install or enable a generic agent-task provider for this backend, or pass --dispatch-backend with an available backend"
        } else {
            "Pass --dispatch-selector with one of the available provider ids"
        };
        return doctor_check(
            format!("controller.action.{action_id}.provider"),
            "error",
            format!("No selectable provider found for backend '{backend}'"),
            Some(remediation),
            serde_json::json!({
                "action_id": action_id,
                "backend": backend,
                "selector": selector,
                "available_provider_ids": available_provider_ids,
            }),
        );
    };

    let missing_capabilities: Vec<_> = required_capabilities
        .iter()
        .filter(|capability| !provider.capabilities.contains(capability))
        .cloned()
        .collect();
    if !missing_capabilities.is_empty() {
        return doctor_check(
            format!("controller.action.{action_id}.provider.capabilities"),
            "error",
            format!(
                "Provider '{}' is missing required capabilities: {}",
                provider.id,
                missing_capabilities.join(", ")
            ),
            Some("Choose a provider with the declared generic capabilities or adjust the workflow capability declaration"),
            serde_json::json!({ "action_id": action_id, "provider": provider.id, "missing_capabilities": missing_capabilities }),
        );
    }

    doctor_check(
        format!("controller.action.{action_id}.provider"),
        "ok",
        format!("Provider '{}' is selectable", provider.id),
        None,
        serde_json::json!({ "action_id": action_id, "backend": backend, "selector": selector, "provider": provider.id }),
    )
}

fn doctor_check(
    id: impl Into<String>,
    status: &str,
    message: impl Into<String>,
    remediation: Option<&str>,
    details: Value,
) -> Value {
    serde_json::json!({
        "id": id.into(),
        "status": status,
        "message": message.into(),
        "remediation": remediation,
        "details": details,
    })
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
    provider_config: Option<String>,
}

impl ControllerDispatchDefaults {
    fn from_from_spec_args(args: &AgentTaskControllerFromSpecArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
            provider_config: args.dispatch_provider_config.clone(),
        }
    }

    fn from_run_from_spec_args(args: &AgentTaskControllerRunFromSpecArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
            provider_config: args.dispatch_provider_config.clone(),
        }
    }

    fn from_run_next_args(args: &AgentTaskControllerRunNextArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
            provider_config: args.dispatch_provider_config.clone(),
        }
    }

    fn from_run_args(args: &AgentTaskControllerRunArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
            provider_config: args.dispatch_provider_config.clone(),
        }
    }

    fn from_loop_define_args(args: &AgentTaskLoopDefineArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
            provider_config: args.dispatch_provider_config.clone(),
        }
    }

    fn from_loop_resume_args(args: &AgentTaskLoopResumeArgs) -> Self {
        Self {
            backend: args.dispatch_backend.clone(),
            selector: args.dispatch_selector.clone(),
            model: args.dispatch_model.clone(),
            provider_config: args.dispatch_provider_config.clone(),
        }
    }

    fn to_overrides(&self) -> agent_task_controller_service::ControllerDispatchOverrides {
        agent_task_controller_service::ControllerDispatchOverrides {
            backend: self.backend.clone(),
            selector: self.selector.clone(),
            model: self.model.clone(),
            provider_config: self.provider_config.clone(),
        }
    }

    fn apply_to_spec(&self, spec: &mut AgentTaskRepoLoopSpec) {
        if self.is_empty() {
            return;
        }
        self.apply_to_spec_metadata(&mut spec.metadata);
        self.apply_to_actions(&mut spec.actions);
        for phase in &mut spec.phases {
            self.apply_to_actions(&mut phase.actions);
        }
        if let Some(policy) = &mut spec.policy {
            for transition in &mut policy.transitions {
                self.apply_to_actions(&mut transition.actions);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.backend.is_none()
            && self.selector.is_none()
            && self.model.is_none()
            && self.provider_config.is_none()
    }

    fn apply_to_spec_metadata(&self, metadata: &mut Value) {
        let mut object = match std::mem::take(metadata) {
            Value::Object(map) => map,
            Value::Null => serde_json::Map::new(),
            other => {
                let mut map = serde_json::Map::new();
                map.insert("repo_loop_metadata".to_string(), other);
                map
            }
        };
        let defaults = object
            .entry("dispatch_defaults".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Value::Object(defaults) = defaults {
            insert_default_string(defaults, "backend", self.backend.as_deref());
            insert_default_string(defaults, "selector", self.selector.as_deref());
            insert_default_string(defaults, "model", self.model.as_deref());
            insert_default_string(defaults, "provider_config", self.provider_config.as_deref());
        }
        *metadata = Value::Object(object);
    }

    fn apply_to_actions(&self, actions: &mut [AgentTaskLoopPolicyAction]) {
        for action in actions {
            match action {
                AgentTaskLoopPolicyAction::SpawnTask { request, .. }
                | AgentTaskLoopPolicyAction::SpawnController { request, .. }
                | AgentTaskLoopPolicyAction::SpawnSubloop { request, .. } => {
                    self.apply_to_dispatch_request(request);
                }
                AgentTaskLoopPolicyAction::FanOut {
                    request_template, ..
                }
                | AgentTaskLoopPolicyAction::RouteFinding {
                    request_template, ..
                } => {
                    self.apply_to_dispatch_request(request_template);
                }
                _ => {}
            }
        }
    }

    fn apply_to_dispatch_request(&self, request: &mut Value) {
        let Some(dispatch) = dispatch_object_mut(request) else {
            return;
        };
        insert_default_string(dispatch, "backend", self.backend.as_deref());
        insert_default_string(dispatch, "selector", self.selector.as_deref());
        insert_default_string(dispatch, "model", self.model.as_deref());
        insert_default_string(dispatch, "provider_config", self.provider_config.as_deref());
    }
}

fn dispatch_object_mut(value: &mut Value) -> Option<&mut serde_json::Map<String, Value>> {
    let object = value.as_object_mut()?;
    if object.get("mode").and_then(Value::as_str) != Some("dispatch") {
        return None;
    }
    if object.contains_key("dispatch") {
        return object.get_mut("dispatch").and_then(Value::as_object_mut);
    }
    Some(object)
}

fn insert_default_string(map: &mut serde_json::Map<String, Value>, key: &str, value: Option<&str>) {
    if map
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .is_some()
    {
        return;
    }
    if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn stamp_loop_runtime_metadata(
    metadata: &mut Value,
    on: bool,
    revolution_limit: Option<u32>,
    increment_revolution: bool,
) -> homeboy::core::Result<()> {
    if metadata.is_null() {
        *metadata = serde_json::json!({});
    }
    let Some(object) = metadata.as_object_mut() else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "metadata",
            "loop runtime metadata requires object metadata",
            Some(metadata.to_string()),
            None,
        ));
    };
    let runtime = object
        .entry("runtime".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !runtime.is_object() {
        *runtime = serde_json::json!({});
    }
    let runtime = runtime.as_object_mut().expect("runtime object");
    let current = runtime
        .get("revolutions")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    runtime.insert("on".to_string(), Value::Bool(on));
    runtime.insert(
        "state".to_string(),
        Value::String(if on { "on" } else { "off" }.to_string()),
    );
    runtime.insert(
        "revolutions".to_string(),
        Value::Number(serde_json::Number::from(if increment_revolution {
            current + 1
        } else {
            current
        })),
    );
    if let Some(limit) = revolution_limit {
        runtime.insert(
            "revolution_limit".to_string(),
            Value::Number(serde_json::Number::from(limit)),
        );
    }
    runtime.insert(
        "continuation_policy".to_string(),
        serde_json::json!({
            "mode": "until_stopped_or_revolution_limit",
            "resume_command": "homeboy agent-task loop resume <loop-id>",
            "stop_command": "homeboy agent-task loop stop <loop-id>"
        }),
    );
    Ok(())
}

fn stamp_record_loop_runtime_metadata(
    record: &mut homeboy::core::agent_tasks::loop_controller::AgentTaskLoopControllerRecord,
    on: bool,
    revolution_limit: Option<u32>,
    increment_revolution: bool,
) -> homeboy::core::Result<()> {
    stamp_loop_runtime_metadata(
        &mut record.metadata,
        on,
        revolution_limit,
        increment_revolution,
    )
}

fn loop_runtime_metadata(metadata: &Value) -> Value {
    metadata.get("runtime").cloned().unwrap_or_else(|| {
        serde_json::json!({
            "on": true,
            "state": "on",
            "revolutions": 0,
            "continuation_policy": {
                "mode": "until_stopped_or_revolution_limit",
                "resume_command": "homeboy agent-task loop resume <loop-id>",
                "stop_command": "homeboy agent-task loop stop <loop-id>"
            }
        })
    })
}

impl<E> ControllerDispatchHook for CliDispatchHook<E>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    fn dispatch(&self, request: &Value) -> homeboy::core::Result<(Value, i32)> {
        let command = agent_task_controller_service::controller_request_dispatch_command(
            request,
            &self.defaults.to_overrides(),
        )?;
        dispatch_service::run_dispatch_command(command, self.executor.clone())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_dispatch_defaults_apply_provider_config_when_missing() {
        let command = ControllerDispatchDefaults {
            backend: Some("ignored-backend".to_string()),
            selector: None,
            model: Some("gpt-5.5".to_string()),
            provider_config: Some(r#"{"runtime_wordpress_version":"7.0"}"#.to_string()),
        }
        .to_overrides();
        let command = agent_task_controller_service::controller_request_dispatch_command(
            &serde_json::json!({
                "dispatch": {
                    "prompt": "Cook",
                    "tasks": ["Do the work"],
                    "backend": "synthetic-runtime"
                }
            }),
            &command,
        )
        .expect("dispatch command");

        assert_eq!(command.backend.as_deref(), Some("synthetic-runtime"));
        assert_eq!(command.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(
            command.core.provider_config.as_deref(),
            Some(r#"{"runtime_wordpress_version":"7.0"}"#)
        );
    }

    #[test]
    fn controller_dispatch_defaults_preserve_existing_provider_config() {
        let overrides = ControllerDispatchDefaults {
            backend: None,
            selector: None,
            model: None,
            provider_config: Some(r#"{"runtime_wordpress_version":"7.0"}"#.to_string()),
        }
        .to_overrides();
        let command = agent_task_controller_service::controller_request_dispatch_command(
            &serde_json::json!({
                "dispatch": {
                    "prompt": "Cook",
                    "tasks": ["Do the work"],
                    "provider_config": "{\"runtime_wordpress_version\":\"nightly\"}"
                }
            }),
            &overrides,
        )
        .expect("dispatch command");

        assert_eq!(
            command.core.provider_config.as_deref(),
            Some(r#"{"runtime_wordpress_version":"nightly"}"#)
        );
    }

    #[test]
    fn controller_dispatch_defaults_apply_selector_when_missing() {
        let overrides = ControllerDispatchDefaults {
            backend: Some("fixture".to_string()),
            selector: Some("fixture-provider".to_string()),
            model: Some("gpt-test".to_string()),
            provider_config: None,
        }
        .to_overrides();
        let command = agent_task_controller_service::controller_request_dispatch_command(
            &serde_json::json!({
                "mode": "dispatch",
                "dispatch": {
                    "prompt": "Cook"
                }
            }),
            &overrides,
        )
        .expect("dispatch command");

        assert_eq!(command.backend.as_deref(), Some("fixture"));
        assert_eq!(command.selector.as_deref(), Some("fixture-provider"));
        assert_eq!(command.model.as_deref(), Some("gpt-test"));
    }
}
