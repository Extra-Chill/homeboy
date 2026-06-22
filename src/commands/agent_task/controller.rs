//! `agent-task controller` handlers: durable multi-agent loop controller
//! lifecycle, spec defaults, and the CLI dispatch bridge.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

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

const CONTROLLER_SPEC_GENERATOR_SCHEMA: &str = "homeboy/agent-task-loop-spec-generator/v1";

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
    let source = load_materialize_spec_source(&args.spec)?;
    let mut spec = source.spec;
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
    let mut value = command_json_value(report)?;
    if let Some(generator_evidence) = source.generator_evidence {
        if let Value::Object(map) = &mut value {
            map.insert("generator_evidence".to_string(), generator_evidence);
        }
    }
    Ok((value, 0))
}

struct ControllerMaterializeSpecSource {
    spec: AgentTaskRepoLoopSpec,
    generator_evidence: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ControllerSpecGeneratorManifest {
    schema: String,
    command: Vec<String>,
    output_path: String,
    #[serde(default)]
    inputs: Value,
}

fn load_materialize_spec_source(
    source: &str,
) -> homeboy::core::Result<ControllerMaterializeSpecSource> {
    let raw = config::read_json_spec_to_string(source)?;
    match serde_json::from_str::<AgentTaskRepoLoopSpec>(&raw) {
        Ok(spec) => {
            return Ok(ControllerMaterializeSpecSource {
                spec,
                generator_evidence: None,
            })
        }
        Err(spec_error) => {
            let manifest: ControllerSpecGeneratorManifest =
                serde_json::from_str(&raw).map_err(|_| {
                    homeboy::core::Error::validation_invalid_argument(
                        "spec",
                        spec_error.to_string(),
                        Some(source.to_string()),
                        None,
                    )
                })?;
            load_generated_materialize_spec(source, manifest)
        }
    }
}

fn load_generated_materialize_spec(
    source: &str,
    manifest: ControllerSpecGeneratorManifest,
) -> homeboy::core::Result<ControllerMaterializeSpecSource> {
    validate_generator_manifest(source, &manifest)?;
    let manifest_dir = manifest_base_dir(source);
    let output_path = resolve_manifest_path(manifest_dir.as_deref(), &manifest.output_path);
    let command_status = run_spec_generator_command(&manifest, manifest_dir.as_deref())?;
    let generated_raw = std::fs::read_to_string(&output_path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "spec.output_path",
            format!(
                "generator command completed but generated spec was not found at {}; rerun the manifest command or update output_path: {error}",
                output_path.display()
            ),
            Some(source.to_string()),
            Some(vec![format!(
                "{} must write {}",
                manifest.command.join(" "),
                manifest.output_path
            )]),
        )
    })?;
    let spec: AgentTaskRepoLoopSpec = serde_json::from_str(&generated_raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "spec.output_path",
            format!("generated spec JSON is invalid: {error}"),
            Some(output_path.display().to_string()),
            None,
        )
    })?;
    let materialized = materialize_repo_loop_spec(AgentTaskLoopSpecMaterializationRequest {
        spec: &spec,
        run_inputs: &Value::Null,
        policy_results: &[],
    })?;
    let validation_result = validate_proof_value(command_json_value(materialized)?);
    let spec_hash = format!("{:x}", Sha256::digest(generated_raw.as_bytes()));

    Ok(ControllerMaterializeSpecSource {
        spec,
        generator_evidence: Some(serde_json::json!({
            "schema": "homeboy/agent-task-loop-spec-generator-evidence/v1",
            "manifest": source,
            "command": manifest.command,
            "inputs": manifest.inputs,
            "output_path": output_path.display().to_string(),
            "spec_hash": spec_hash,
            "validation_result": validation_result,
            "status": command_status,
        })),
    })
}

fn validate_generator_manifest(
    source: &str,
    manifest: &ControllerSpecGeneratorManifest,
) -> homeboy::core::Result<()> {
    if manifest.schema != CONTROLLER_SPEC_GENERATOR_SCHEMA {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "spec.schema",
            format!(
                "controller materialize generator manifests must use schema {CONTROLLER_SPEC_GENERATOR_SCHEMA}"
            ),
            Some(source.to_string()),
            None,
        ));
    }
    if manifest.command.is_empty() || manifest.command.iter().any(|part| part.trim().is_empty()) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "spec.command",
            "generator command must be a non-empty array of program and arguments",
            Some(source.to_string()),
            None,
        ));
    }
    if manifest.output_path.trim().is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "spec.output_path",
            "generator manifest must declare the spec file path written by the command",
            Some(source.to_string()),
            None,
        ));
    }
    if !manifest.inputs.is_null() && !manifest.inputs.is_object() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "spec.inputs",
            "generator manifest inputs must be a JSON object when present",
            Some(source.to_string()),
            None,
        ));
    }
    Ok(())
}

fn manifest_base_dir(source: &str) -> Option<PathBuf> {
    let file = source.strip_prefix('@')?;
    if file == "-" {
        return None;
    }
    Path::new(file).parent().map(Path::to_path_buf)
}

fn resolve_manifest_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path)
    } else {
        path
    }
}

fn run_spec_generator_command(
    manifest: &ControllerSpecGeneratorManifest,
    manifest_dir: Option<&Path>,
) -> homeboy::core::Result<Value> {
    let mut command = Command::new(&manifest.command[0]);
    command.args(&manifest.command[1..]);
    if let Some(manifest_dir) = manifest_dir {
        command.current_dir(manifest_dir);
    }
    let output = command.output().map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "spec.command",
            format!("failed to execute generator command: {error}"),
            Some(manifest.command.join(" ")),
            None,
        )
    })?;
    let code = output.status.code();
    if !output.status.success() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "spec.command",
            format!(
                "generator command exited with status {}; stderr: {}",
                code.map(|value| value.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string()),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(manifest.command.join(" ")),
            None,
        ));
    }
    Ok(serde_json::json!({
        "exit_code": code,
        "stdout": String::from_utf8_lossy(&output.stdout).trim(),
        "stderr": String::from_utf8_lossy(&output.stderr).trim(),
    }))
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
    if args.doctor {
        return controller_from_spec_doctor(spec, &args);
    }
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

    let mut dispatch_args = match dispatch_args_from_controller_request(request) {
        Ok(args) => args,
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
    defaults.apply(&mut dispatch_args);

    let command: dispatch_service::AgentTaskDispatchCommand = dispatch_args.into();
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
        if args.core.provider_config.is_none() {
            args.core.provider_config = self.provider_config.clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_dispatch_defaults_apply_provider_config_when_missing() {
        let mut args = dispatch_args_from_controller_request(&serde_json::json!({
            "dispatch": {
                "prompt": "Cook",
                "tasks": ["Do the work"],
                "backend": "synthetic-runtime"
            }
        }))
        .expect("dispatch args");

        ControllerDispatchDefaults {
            backend: Some("ignored-backend".to_string()),
            selector: None,
            model: Some("gpt-5.5".to_string()),
            provider_config: Some(r#"{"runtime_wordpress_version":"7.0"}"#.to_string()),
        }
        .apply(&mut args);

        assert_eq!(args.backend.as_deref(), Some("synthetic-runtime"));
        assert_eq!(args.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(
            args.core.provider_config.as_deref(),
            Some(r#"{"runtime_wordpress_version":"7.0"}"#)
        );
    }

    #[test]
    fn controller_dispatch_defaults_preserve_existing_provider_config() {
        let mut args = dispatch_args_from_controller_request(&serde_json::json!({
            "dispatch": {
                "prompt": "Cook",
                "tasks": ["Do the work"],
                "provider_config": "{\"runtime_wordpress_version\":\"nightly\"}"
            }
        }))
        .expect("dispatch args");

        ControllerDispatchDefaults {
            backend: None,
            selector: None,
            model: None,
            provider_config: Some(r#"{"runtime_wordpress_version":"7.0"}"#.to_string()),
        }
        .apply(&mut args);

        assert_eq!(
            args.core.provider_config.as_deref(),
            Some(r#"{"runtime_wordpress_version":"nightly"}"#)
        );
    }
}
