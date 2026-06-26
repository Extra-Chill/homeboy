//! Command-specific secret hydration for Lab offload.
//!
//! Splits agent-task vs trace secret discovery from the offload orchestrator so
//! that the secret env contract for each command family stays close to its
//! parser. `build_lab_secret_env_handoff_plan` is the single offload boundary:
//! it resolves controller-owned secret values once, records runner-deferred
//! requirements by name, and exposes only redacted diagnostics for metadata.

use std::collections::HashMap;

use crate::core::agent_tasks::provider::{
    provider_runner_secret_env_for_plan_with_providers,
    provider_secret_sources_for_plan_with_providers, provider_secret_sources_for_providers,
    AgentTaskExecutorProvider, ExtensionProviderAgentTaskExecutor,
};
use crate::core::agent_tasks::scheduler::AgentTaskPlan;
use crate::core::agent_tasks::secrets as agent_task_secrets;
use crate::core::agent_tasks::{
    AgentTaskExecutor, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::secret_env_plan::SecretEnvPlan;
use crate::core::{config, Error, Result};

use super::super::Runner;
use super::args_util::{non_empty_arg, subcommand_index};

/// Provenance key used when a missing runner secret_env name is contributed by
/// the plan's provider runner requirements rather than an individual task.
const PLAN_PROVIDER_PROVENANCE: &str = "plan:provider";
const LAB_SECRET_ENV_HANDOFF_SCHEMA: &str = "homeboy/lab-secret-env-handoff/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LabSecretEnvHandoffPlan {
    pub(super) env_delta: HashMap<String, String>,
    pub(super) diagnostics: serde_json::Value,
    pub(super) secret_env_plan: SecretEnvPlan,
    pub(super) secret_env_names: Vec<String>,
    pub(super) runner_deferred_secret_env: Vec<String>,
}

pub(super) fn build_lab_secret_env_handoff_plan(
    args: &[String],
    mut env_delta: HashMap<String, String>,
) -> Result<LabSecretEnvHandoffPlan> {
    let agent_task_secret_env = hydrate_agent_task_secret_env(args, &mut env_delta)?;
    let trace_secret_env = hydrate_trace_secret_env(args, &mut env_delta)?;
    let tunnel_secret_env = hydrate_tunnel_secret_env(args, &mut env_delta)?;
    let mut secret_env_names = declared_agent_task_secret_env_for_handoff(args)?;
    secret_env_names.extend(declared_trace_secret_env(args));
    secret_env_names.extend(declared_tunnel_secret_env(args));
    secret_env_names.sort();
    secret_env_names.dedup();
    let secret_env_plan = SecretEnvPlan::from_secret_env_names(secret_env_names.clone());

    let mut runner_deferred_secret_env = Vec::new();
    runner_deferred_secret_env.extend(runner_deferred_secret_env_names(&agent_task_secret_env));
    runner_deferred_secret_env.extend(runner_deferred_secret_env_names(&tunnel_secret_env));
    runner_deferred_secret_env.sort();
    runner_deferred_secret_env.dedup();

    let diagnostics = serde_json::json!({
        "schema": LAB_SECRET_ENV_HANDOFF_SCHEMA,
        "secret_env_plan": secret_env_plan.redacted(),
        "final_env_delta": redacted_env_delta_metadata(&env_delta),
        "secret_env_names": secret_env_names.clone(),
        "runner_deferred_secret_env": runner_deferred_secret_env
            .iter()
            .map(|name| serde_json::json!({
                "name": name,
                "source": "runner",
                "required": true,
            }))
            .collect::<Vec<_>>(),
        "agent_task_secret_env": agent_task_secret_env,
        "trace_secret_env": trace_secret_env,
        "tunnel_secret_env": tunnel_secret_env,
    });

    Ok(LabSecretEnvHandoffPlan {
        env_delta,
        diagnostics,
        secret_env_plan,
        secret_env_names,
        runner_deferred_secret_env,
    })
}

fn redacted_env_delta_metadata(env_delta: &HashMap<String, String>) -> Vec<serde_json::Value> {
    let mut names = env_delta.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            serde_json::json!({
                "name": name,
                "forwarded_to_runner": true,
                "value_preview": "<redacted>",
                "redacted": true,
            })
        })
        .collect()
}

fn runner_deferred_secret_env_names(metadata: &serde_json::Value) -> Vec<String> {
    metadata
        .get("runner_deferred_secret_env")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect()
}

pub(super) fn hydrate_agent_task_secret_env(
    args: &[String],
    env: &mut HashMap<String, String>,
) -> Result<serde_json::Value> {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    hydrate_agent_task_secret_env_with_providers(args, env, executor.providers())
}

fn hydrate_agent_task_secret_env_with_providers(
    args: &[String],
    env: &mut HashMap<String, String>,
    providers: &[AgentTaskExecutorProvider],
) -> Result<serde_json::Value> {
    let mut names = declared_agent_task_controller_secret_env_with_providers(args, providers)?;
    let mut runner_deferred_names =
        declared_agent_task_run_plan_secret_env_with_providers(args, providers);
    runner_deferred_names.sort();
    runner_deferred_names.dedup();
    if names.is_empty() && runner_deferred_names.is_empty() {
        return Ok(serde_json::json!({
            "schema": "homeboy/lab-agent-task-secret-env/v1",
            "secret_env": [],
            "runner_deferred_secret_env": [],
        }));
    }

    let fallback_sources = subcommand_index(args, "agent-task")
        .map(|agent_task_index| {
            declared_agent_task_controller_secret_sources_with_providers(
                args,
                agent_task_index,
                providers,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let controller_source_run_plan_names = runner_deferred_names
        .iter()
        .filter(|name| fallback_sources.contains_key(name.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !controller_source_run_plan_names.is_empty() {
        names.extend(controller_source_run_plan_names.iter().cloned());
        names.sort();
        names.dedup();
        runner_deferred_names.retain(|name| !controller_source_run_plan_names.contains(name));
    }
    if !names.is_empty() {
        let resolved = agent_task_secrets::resolve_secret_env_with_fallbacks(&names, &fallback_sources).map_err(|error| {
            Error::validation_invalid_argument(
                "secret-env",
                error.message,
                None,
                Some(vec![
                    "Configure provider secrets with Homeboy's global agent-task secret config, for example `homeboy agent-task auth map-env` or `homeboy agent-task auth set-keychain`.".to_string(),
                ]),
            )
        })?;
        for (name, value) in resolved {
            env.insert(name, value);
        }
    }

    Ok(serde_json::json!({
        "schema": "homeboy/lab-agent-task-secret-env/v1",
        "secret_env": agent_task_secrets::secret_env_status_with_fallbacks(&names, &fallback_sources),
        "runner_deferred_secret_env": runner_deferred_names
            .into_iter()
            .map(|name| serde_json::json!({
                "name": name,
                "source": "runner",
            }))
            .collect::<Vec<_>>(),
    }))
}

pub(super) fn hydrate_trace_secret_env(
    args: &[String],
    env: &mut HashMap<String, String>,
) -> Result<serde_json::Value> {
    let names = declared_trace_secret_env(args);
    if names.is_empty() {
        return Ok(crate::core::trace_secrets::empty_status());
    }

    let project_id = trace_project_id_from_args(args);
    let (resolved, statuses) =
        crate::core::trace_secrets::resolve_secret_env(&names, project_id.as_deref())?;
    for (name, value) in resolved {
        env.insert(name, value);
    }

    Ok(crate::core::trace_secrets::status_metadata(statuses))
}

pub(super) fn hydrate_tunnel_secret_env(
    args: &[String],
    _env: &mut HashMap<String, String>,
) -> Result<serde_json::Value> {
    let names = declared_tunnel_secret_env(args);
    Ok(serde_json::json!({
        "schema": "homeboy/lab-tunnel-secret-env/v1",
        "runner_deferred_secret_env": names
            .into_iter()
            .map(|name| serde_json::json!({
                "name": name,
                "source": "runner",
            }))
            .collect::<Vec<_>>(),
    }))
}

pub(crate) fn declared_agent_task_secret_env(args: &[String]) -> Vec<String> {
    declared_agent_task_secret_env_for_handoff(args).unwrap_or_default()
}

fn declared_agent_task_secret_env_for_handoff(args: &[String]) -> Result<Vec<String>> {
    if subcommand_index(args, "agent-task").is_none() {
        return Ok(Vec::new());
    }

    let executor = ExtensionProviderAgentTaskExecutor::discover();
    let mut names =
        declared_agent_task_controller_secret_env_with_providers(args, executor.providers())?;
    names.extend(declared_agent_task_run_plan_secret_env(args));
    names.sort();
    names.dedup();
    Ok(names)
}

pub(super) fn preflight_agent_task_runner_secret_env_plan(
    runner_id: &str,
    runner: &Runner,
    args: &[String],
    env: &HashMap<String, String>,
    secret_env_plan: &SecretEnvPlan,
) -> Result<()> {
    let tasks = declared_agent_task_run_plan_secret_env_by_task(args);
    if tasks.is_empty() {
        return Ok(());
    }
    let required_names = secret_env_plan.secret_env_names();

    // Dedupe missing env names while preserving first-seen order so the
    // operator-facing message lists each name exactly once, even when several
    // plan tasks declare the same requirement.
    let mut missing: Vec<String> = Vec::new();
    let mut required_by_tasks: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for task in &tasks {
        for name in &task.secret_env {
            if !required_names.contains(name) {
                continue;
            }
            if env.contains_key(name)
                || runner.secret_env.contains_key(name)
                || task.secret_sources.contains(name)
            {
                continue;
            }
            if !missing.iter().any(|seen| seen == name) {
                missing.push(name.clone());
            }
            let entry = required_by_tasks
                .entry(name.clone())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if let serde_json::Value::Array(task_ids) = entry {
                let task_id = serde_json::Value::String(task.task_id.clone());
                if !task_ids.contains(&task_id) {
                    task_ids.push(task_id);
                }
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }

    let mut error = Error::validation_invalid_argument(
        "secret-env",
        format!(
            "missing required agent-task runner secret env on runner `{}`: {}",
            runner_id,
            missing.join(", ")
        ),
        Some(runner_id.to_string()),
        Some(vec![
            "Configure the selected runner secret_env references for the declared names before Lab dispatch.".to_string(),
            "Use `homeboy runner env <runner>` to inspect redacted runner secret_env refs without printing values.".to_string(),
        ]),
    );
    if let serde_json::Value::Object(details) = &mut error.details {
        details.insert(
            "required_by_tasks".to_string(),
            serde_json::Value::Object(required_by_tasks),
        );
    }
    Err(error)
}

fn declared_agent_task_controller_secret_env(args: &[String]) -> Vec<String> {
    if subcommand_index(args, "agent-task").is_none() {
        return Vec::new();
    }

    let executor = ExtensionProviderAgentTaskExecutor::discover();
    declared_agent_task_controller_secret_env_with_providers(args, executor.providers())
        .unwrap_or_default()
}

fn declared_agent_task_controller_secret_env_with_providers(
    args: &[String],
    providers: &[AgentTaskExecutorProvider],
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--secret-env" {
            if let Some(name) = args.get(index + 1) {
                names.push(name.clone());
            }
            index += 2;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--secret-env=") {
            names.push(name.to_string());
        }
        if arg == "--provider-config" || arg == "--dispatch-provider-config" {
            if let Some(spec) = args.get(index + 1) {
                names.extend(declared_provider_config_secret_env(spec)?);
            }
            index += 2;
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--provider-config=") {
            names.extend(declared_provider_config_secret_env(spec)?);
        }
        if let Some(spec) = arg.strip_prefix("--dispatch-provider-config=") {
            names.extend(declared_provider_config_secret_env(spec)?);
        }
        index += 1;
    }
    if let Some(plan) = agent_task_dispatch_provider_plan_from_args(args)? {
        names.extend(provider_runner_secret_env_for_plan_with_providers(
            &plan, providers,
        ));
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn declared_agent_task_controller_secret_sources_with_providers(
    args: &[String],
    agent_task_index: usize,
    providers: &[AgentTaskExecutorProvider],
) -> Result<HashMap<String, crate::core::defaults::AgentTaskSecretSource>> {
    if args.get(agent_task_index + 1).map(String::as_str) != Some("providers") {
        if let Some(plan) = agent_task_dispatch_provider_plan_from_args(args)? {
            return Ok(provider_secret_sources_for_plan_with_providers(
                &plan, providers,
            ));
        }
        return Ok(agent_task_run_plan_from_args(args)
            .map(|plan| provider_secret_sources_for_plan_with_providers(&plan, providers))
            .unwrap_or_default());
    }

    Ok(provider_secret_sources_for_providers(providers))
}

fn agent_task_dispatch_provider_plan_from_args(args: &[String]) -> Result<Option<AgentTaskPlan>> {
    let Some(agent_task_index) = subcommand_index(args, "agent-task") else {
        return Ok(None);
    };
    let Some(command) = args.get(agent_task_index + 1).map(String::as_str) else {
        return Ok(None);
    };
    if !matches!(command, "cook" | "dispatch" | "loop" | "controller") {
        return Ok(None);
    }

    let (backend_flag, selector_flag, model_flag, provider_config_flag) = match command {
        "controller" | "loop" => (
            "--dispatch-backend",
            "--dispatch-selector",
            "--dispatch-model",
            "--dispatch-provider-config",
        ),
        _ => ("--backend", "--selector", "--model", "--provider-config"),
    };

    let Some(backend) = option_arg_after(args, agent_task_index + 2, backend_flag) else {
        return Ok(None);
    };
    let selector = option_arg_after(args, agent_task_index + 2, selector_flag);
    let model = option_arg_after(args, agent_task_index + 2, model_flag);
    let provider_config = option_arg_after(args, agent_task_index + 2, provider_config_flag)
        .map(|spec| read_provider_config_value(&spec))
        .transpose()?
        .unwrap_or(serde_json::Value::Null);

    Ok(Some(AgentTaskPlan::new(
        "lab-agent-task-dispatch-secret-discovery".to_string(),
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "lab-secret-discovery".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend,
                selector,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model,
                config: provider_config,
            },
            instructions: String::new(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: Default::default(),
            limits: Default::default(),
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            metadata: serde_json::Value::Null,
        }],
    )))
}

fn option_arg_after(args: &[String], start: usize, flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    let mut index = start;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            break;
        }
        if arg == flag {
            return args.get(index + 1).cloned();
        }
        if let Some(value) = arg.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
        index += 1;
    }
    None
}

pub(crate) fn declared_trace_secret_env(args: &[String]) -> Vec<String> {
    if subcommand_index(args, "trace").is_none() {
        return Vec::new();
    }

    declared_secret_env_args(args)
}

pub(crate) fn declared_tunnel_secret_env(args: &[String]) -> Vec<String> {
    let Some(tunnel_index) = subcommand_index(args, "tunnel") else {
        return Vec::new();
    };

    match (
        args.get(tunnel_index + 1).map(String::as_str),
        args.get(tunnel_index + 2).map(String::as_str),
    ) {
        (Some("preview-client"), Some("start")) => declared_tunnel_preview_client_token_env(args),
        (Some("service"), Some("start")) if tunnel_service_start_uses_preview_client(args) => {
            vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]
        }
        _ => Vec::new(),
    }
}

fn declared_tunnel_preview_client_token_env(args: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--token-env" {
            if let Some(name) = args.get(index + 1) {
                names.push(name.clone());
            }
            index += 2;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--token-env=") {
            names.push(name.to_string());
        }
        index += 1;
    }

    if names.is_empty() {
        names.push("HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string());
    }
    names.sort();
    names.dedup();
    names
}

fn tunnel_service_start_uses_preview_client(args: &[String]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--public-tunnel-command" {
            if args
                .get(index + 1)
                .is_some_and(|command| command.contains("preview-client start"))
            {
                return true;
            }
            index += 2;
            continue;
        }
        if let Some(command) = arg.strip_prefix("--public-tunnel-command=") {
            if command.contains("preview-client start") {
                return true;
            }
        }
        index += 1;
    }
    false
}

pub(super) fn trace_project_id_from_args(args: &[String]) -> Option<String> {
    let trace_index = subcommand_index(args, "trace")?;

    for index in (trace_index + 1)..args.len() {
        let arg = &args[index];
        if let Some(component) = arg.strip_prefix("--component=") {
            return non_empty_arg(component);
        }
        if arg == "--component" {
            return args.get(index + 1).and_then(|value| non_empty_arg(value));
        }
    }

    match args.get(trace_index + 1).map(String::as_str) {
        Some("compare") => args
            .get(trace_index + 2)
            .and_then(|value| non_empty_arg(value)),
        Some(command) if !command.starts_with('-') => Some(command.to_string()),
        _ => None,
    }
}

fn declared_secret_env_args(args: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--secret-env" {
            if let Some(name) = args.get(index + 1) {
                names.push(name.clone());
            }
            index += 2;
            continue;
        }
        if let Some(name) = arg.strip_prefix("--secret-env=") {
            names.push(name.to_string());
        }
        index += 1;
    }
    names.sort();
    names.dedup();
    names
}

fn read_provider_config_value(spec: &str) -> Result<serde_json::Value> {
    let raw = config::read_json_spec_to_string(spec).map_err(|error| {
        provider_config_error(spec, format!("failed to read provider config: {error}"))
    })?;

    serde_json::from_str(&raw).map_err(|error| {
        provider_config_error(
            spec,
            format!("failed to parse provider config JSON: {error}"),
        )
    })
}

fn provider_config_error(spec: &str, problem: String) -> Error {
    Error::validation_invalid_argument(
        "provider-config",
        problem,
        Some(spec.to_string()),
        Some(vec![
            "Pass --provider-config as inline JSON or @/path/to/provider-config.json.".to_string(),
        ]),
    )
}

fn declared_provider_config_secret_env(spec: &str) -> Result<Vec<String>> {
    let value = read_provider_config_value(spec)?;
    let Some(config) = value.as_object() else {
        return Err(provider_config_error(
            spec,
            "provider config must be a JSON object".to_string(),
        ));
    };

    let mut names = Vec::new();
    for key in ["secret_env", "secretEnv"] {
        match config.get(key) {
            Some(serde_json::Value::Array(items)) => {
                names.extend(
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string),
                );
            }
            Some(serde_json::Value::String(name)) => names.push(name.clone()),
            _ => {}
        }
    }
    Ok(names)
}

fn declared_agent_task_run_plan_secret_env(args: &[String]) -> Vec<String> {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    declared_agent_task_run_plan_secret_env_with_providers(args, executor.providers())
}

fn declared_agent_task_run_plan_secret_env_with_providers(
    args: &[String],
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
    declared_agent_task_run_plan_secret_env_by_task_with_providers(args, providers)
        .into_iter()
        .flat_map(|task| task.secret_env)
        .collect()
}

/// A single plan task's declared runner secret_env names, retaining the task
/// identity so preflight can report per-task provenance without repeating the
/// same env names across tasks in operator-facing prose.
struct PlanTaskSecretEnv {
    task_id: String,
    secret_env: Vec<String>,
    secret_sources: Vec<String>,
}

fn declared_agent_task_run_plan_secret_env_by_task(args: &[String]) -> Vec<PlanTaskSecretEnv> {
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    declared_agent_task_run_plan_secret_env_by_task_with_providers(args, executor.providers())
}

fn declared_agent_task_run_plan_secret_env_by_task_with_providers(
    args: &[String],
    providers: &[AgentTaskExecutorProvider],
) -> Vec<PlanTaskSecretEnv> {
    let Some(plan) = agent_task_run_plan_from_args(args) else {
        return Vec::new();
    };

    let mut tasks = Vec::new();
    let provider_names = provider_runner_secret_env_for_plan_with_providers(&plan, providers);
    let provider_secret_sources: Vec<String> =
        provider_secret_sources_for_plan_with_providers(&plan, providers)
            .into_keys()
            .collect();
    if !provider_names.is_empty() {
        tasks.push(PlanTaskSecretEnv {
            task_id: PLAN_PROVIDER_PROVENANCE.to_string(),
            secret_env: provider_names,
            secret_sources: provider_secret_sources.clone(),
        });
    }
    for request in plan.tasks {
        let mut names = Vec::new();
        names.extend(request.executor.secret_env);
        names.extend(
            request
                .executor
                .config
                .get("secret_env")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string),
        );
        tasks.push(PlanTaskSecretEnv {
            task_id: request.task_id,
            secret_env: names,
            secret_sources: provider_secret_sources.clone(),
        });
    }
    tasks
}

fn agent_task_run_plan_from_args(args: &[String]) -> Option<AgentTaskPlan> {
    let plan_spec = agent_task_run_plan_plan_spec(args)?;
    if plan_spec == "-" {
        return None;
    }
    let raw = config::read_json_spec_to_string(&plan_spec).ok()?;
    serde_json::from_str(&raw).ok()
}

fn agent_task_run_plan_plan_spec(args: &[String]) -> Option<String> {
    let run_plan_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 1)
    })?;

    let mut iter = args.iter().skip(run_plan_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--plan" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--plan=") {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::command_invocation::CommandInvocation;
    use crate::core::runner::RunnerKind;
    use crate::core::server::{RunnerPolicy, RunnerSecretEnvRef, RunnerSettings};

    #[test]
    fn declared_agent_task_secret_env_parses_repeated_and_equals_args() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--secret-env".to_string(),
            "HOMEBOY_TEST_REFRESH_TOKEN".to_string(),
            "--secret-env=HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
            "--secret-env".to_string(),
            "HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
                "HOMEBOY_TEST_REFRESH_TOKEN".to_string(),
            ]
        );
    }

    #[test]
    fn declared_agent_task_secret_env_allows_global_flags_before_agent_task() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--secret-env=HOMEBOY_TEST_ACCESS_TOKEN".to_string(),
        ]);

        assert_eq!(names, vec!["HOMEBOY_TEST_ACCESS_TOKEN".to_string()]);
    }

    #[test]
    fn declared_agent_task_secret_env_ignores_trace_secret_env() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
        ]);

        assert!(names.is_empty());
    }

    #[test]
    fn declared_trace_secret_env_parses_repeated_and_equals_args() {
        let names = declared_trace_secret_env(&[
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env".to_string(),
            "STRIPE_PUBLISHABLE_KEY".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
            "--secret-env".to_string(),
            "STRIPE_SECRET_KEY".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "STRIPE_PUBLISHABLE_KEY".to_string(),
                "STRIPE_SECRET_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn declared_trace_secret_env_allows_global_flags_before_trace() {
        let names = declared_trace_secret_env(&[
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--force-hot".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=STRIPE_SECRET_KEY".to_string(),
        ]);

        assert_eq!(names, vec!["STRIPE_SECRET_KEY".to_string()]);
    }

    #[test]
    fn trace_project_id_from_args_reads_compare_and_component_forms() {
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "trace".to_string(),
                "compare".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "--runner".to_string(),
                "homeboy-lab".to_string(),
                "--force-hot".to_string(),
                "trace".to_string(),
                "compare".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
        assert_eq!(
            trace_project_id_from_args(&[
                "homeboy".to_string(),
                "trace".to_string(),
                "compare-variant".to_string(),
                "--component".to_string(),
                "woocommerce-gateway-stripe".to_string(),
                "--scenario".to_string(),
                "real-wallet".to_string(),
            ]),
            Some("woocommerce-gateway-stripe".to_string())
        );
    }

    #[test]
    fn hydrate_trace_secret_env_reports_redacted_status_without_values() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=HOMEBOY_TRACE_SECRET_TEST_KEY".to_string(),
        ];
        let mut env = std::collections::HashMap::new();
        std::env::set_var("HOMEBOY_TRACE_SECRET_TEST_KEY", "sk_test_fake_not_real");

        let diagnostics = hydrate_trace_secret_env(&args, &mut env).expect("hydrate trace secret");

        assert_eq!(
            env.get("HOMEBOY_TRACE_SECRET_TEST_KEY").map(String::as_str),
            Some("sk_test_fake_not_real")
        );
        let rendered = diagnostics.to_string();
        assert!(rendered.contains("HOMEBOY_TRACE_SECRET_TEST_KEY"));
        assert!(rendered.contains("env"));
        assert!(!rendered.contains("sk_test_fake_not_real"));

        std::env::remove_var("HOMEBOY_TRACE_SECRET_TEST_KEY");
    }

    #[test]
    fn lab_secret_env_handoff_plan_reports_redacted_env_delta_and_exact_names() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "compare".to_string(),
            "woocommerce-gateway-stripe".to_string(),
            "real-wallet".to_string(),
            "--secret-env=HOMEBOY_TRACE_HANDOFF_SECRET_TEST".to_string(),
        ];
        let mut env_delta = HashMap::new();
        env_delta.insert(
            "HOMEBOY_PUBLIC_CONTEXT".to_string(),
            "still-redacted-in-diagnostics".to_string(),
        );
        std::env::set_var("HOMEBOY_TRACE_HANDOFF_SECRET_TEST", "trace-secret-value");

        let plan = build_lab_secret_env_handoff_plan(&args, env_delta).expect("handoff plan");

        assert_eq!(
            plan.env_delta
                .get("HOMEBOY_TRACE_HANDOFF_SECRET_TEST")
                .map(String::as_str),
            Some("trace-secret-value")
        );
        assert_eq!(
            plan.secret_env_names,
            vec!["HOMEBOY_TRACE_HANDOFF_SECRET_TEST".to_string()]
        );
        assert!(plan.runner_deferred_secret_env.is_empty());
        let rendered = plan.diagnostics.to_string();
        assert!(rendered.contains("homeboy/lab-secret-env-handoff/v1"));
        assert!(rendered.contains("HOMEBOY_TRACE_HANDOFF_SECRET_TEST"));
        assert!(rendered.contains("HOMEBOY_PUBLIC_CONTEXT"));
        assert!(!rendered.contains("trace-secret-value"));
        assert!(!rendered.contains("still-redacted-in-diagnostics"));

        std::env::remove_var("HOMEBOY_TRACE_HANDOFF_SECRET_TEST");
    }

    #[test]
    fn lab_secret_env_handoff_plan_reports_runner_deferred_requirements() {
        let args = vec![
            "homeboy".to_string(),
            "tunnel".to_string(),
            "preview-client".to_string(),
            "start".to_string(),
            "--ingress".to_string(),
            "https://preview-broker.example.test".to_string(),
            "--token-env=HOMEBOY_RUNNER_PREVIEW_TOKEN".to_string(),
        ];

        let plan = build_lab_secret_env_handoff_plan(&args, HashMap::new()).expect("handoff plan");

        assert!(plan.env_delta.is_empty());
        assert_eq!(
            plan.secret_env_names,
            vec!["HOMEBOY_RUNNER_PREVIEW_TOKEN".to_string()]
        );
        assert_eq!(
            plan.runner_deferred_secret_env,
            vec!["HOMEBOY_RUNNER_PREVIEW_TOKEN".to_string()]
        );
        assert_eq!(
            plan.diagnostics["runner_deferred_secret_env"][0]["name"],
            "HOMEBOY_RUNNER_PREVIEW_TOKEN"
        );
        assert_eq!(
            plan.diagnostics["runner_deferred_secret_env"][0]["required"],
            true
        );
    }

    #[test]
    fn lab_secret_env_handoff_plan_carries_canonical_secret_env_plan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "canonical-secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "runner-owned",
                        "executor": {
                            "backend": "example",
                            "secret_env": ["HOMEBOY_CANONICAL_RUNNER_SECRET_TEST"]
                        },
                        "instructions": "Use runner-owned credentials."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let handoff = build_lab_secret_env_handoff_plan(&run_plan_args(&plan_path), HashMap::new())
            .expect("handoff plan");

        assert_eq!(
            handoff.secret_env_plan.secret_env_names(),
            handoff.secret_env_names
        );
        assert_eq!(
            handoff.diagnostics["secret_env_plan"]["schema"].as_str(),
            Some(crate::core::secret_env_plan::SECRET_ENV_PLAN_SCHEMA)
        );
        assert_eq!(
            handoff.runner_deferred_secret_env,
            vec!["HOMEBOY_CANONICAL_RUNNER_SECRET_TEST".to_string()]
        );
        let runner = fixture_runner(HashMap::from([(
            "HOMEBOY_CANONICAL_RUNNER_SECRET_TEST".to_string(),
            RunnerSecretEnvRef {
                env: Some("HOMEBOY_CANONICAL_RUNNER_SECRET_TEST".to_string()),
                file: None,
                secret: None,
            },
        )]));

        preflight_agent_task_runner_secret_env_plan(
            "lab-a",
            &runner,
            &run_plan_args(&plan_path),
            &handoff.env_delta,
            &handoff.secret_env_plan,
        )
        .expect("preflight consumes handoff secret env plan");
    }

    #[test]
    fn declared_tunnel_secret_env_reads_preview_client_default_and_override() {
        assert_eq!(
            declared_tunnel_secret_env(&[
                "homeboy".to_string(),
                "tunnel".to_string(),
                "preview-client".to_string(),
                "start".to_string(),
                "--ingress".to_string(),
                "https://preview-broker.example.test".to_string(),
            ]),
            vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]
        );

        assert_eq!(
            declared_tunnel_secret_env(&[
                "homeboy".to_string(),
                "tunnel".to_string(),
                "preview-client".to_string(),
                "start".to_string(),
                "--token-env=RUNNER_PREVIEW_TOKEN".to_string(),
            ]),
            vec!["RUNNER_PREVIEW_TOKEN".to_string()]
        );
    }

    #[test]
    fn declared_tunnel_secret_env_detects_service_preview_client_backend() {
        let names = declared_tunnel_secret_env(&[
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "tunnel".to_string(),
            "service".to_string(),
            "start".to_string(),
            "runner-preview".to_string(),
            "--command".to_string(),
            "npm run dev".to_string(),
            "--public-tunnel-backend".to_string(),
            "command".to_string(),
            "--public-tunnel-command".to_string(),
            "homeboy tunnel preview-client start --ready-stdout".to_string(),
        ]);

        assert_eq!(names, vec!["HOMEBOY_PREVIEW_TUNNEL_TOKEN".to_string()]);
    }

    #[test]
    fn hydrate_tunnel_secret_env_reports_runner_deferred_names_without_values() {
        let args = vec![
            "homeboy".to_string(),
            "tunnel".to_string(),
            "preview-client".to_string(),
            "start".to_string(),
            "--ingress".to_string(),
            "https://preview-broker.example.test".to_string(),
            "--public-host".to_string(),
            "preview.example.test".to_string(),
            "--local-origin".to_string(),
            "http://127.0.0.1:8888".to_string(),
        ];
        let mut env = std::collections::HashMap::new();

        let diagnostics =
            hydrate_tunnel_secret_env(&args, &mut env).expect("hydrate tunnel secret");

        assert!(env.is_empty());
        let rendered = diagnostics.to_string();
        assert!(rendered.contains("homeboy/lab-tunnel-secret-env/v1"));
        assert!(rendered.contains("HOMEBOY_PREVIEW_TUNNEL_TOKEN"));
        assert!(rendered.contains("runner"));
    }

    #[test]
    fn declared_agent_task_secret_env_includes_provider_config_secrets() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({
                "provider": "example",
                "secret_env": [
                    "HOMEBOY_PROVIDER_ACCESS_TOKEN",
                    "HOMEBOY_PROVIDER_REFRESH_TOKEN"
                ],
                "secretEnv": "HOMEBOY_PROVIDER_ACCOUNT_ID"
            })
            .to_string(),
            "--secret-env=OPENAI_API_KEY".to_string(),
        ]);

        assert_eq!(
            names,
            vec![
                "HOMEBOY_PROVIDER_ACCESS_TOKEN".to_string(),
                "HOMEBOY_PROVIDER_ACCOUNT_ID".to_string(),
                "HOMEBOY_PROVIDER_REFRESH_TOKEN".to_string(),
                "OPENAI_API_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn declared_agent_task_controller_run_from_spec_includes_dispatch_provider_config_secrets() {
        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "controller".to_string(),
            "run-from-spec".to_string(),
            "@controller-spec.json".to_string(),
            "--dispatch-provider-config".to_string(),
            serde_json::json!({
                "provider": "example",
                "secret_env": ["HOMEBOY_CONTROLLER_PROVIDER_TOKEN"]
            })
            .to_string(),
        ]);

        assert_eq!(names, vec!["HOMEBOY_CONTROLLER_PROVIDER_TOKEN".to_string()]);
    }

    #[test]
    fn lab_secret_env_handoff_plan_hydrates_controller_dispatch_provider_config_secret() {
        let _secret = RemovedEnvVar::new("HOMEBOY_CONTROLLER_PROVIDER_TOKEN");
        std::env::set_var(
            "HOMEBOY_CONTROLLER_PROVIDER_TOKEN",
            "controller-secret-value",
        );

        let plan = build_lab_secret_env_handoff_plan(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "controller".to_string(),
                "run-from-spec".to_string(),
                "@controller-spec.json".to_string(),
                "--dispatch-provider-config".to_string(),
                serde_json::json!({
                    "provider": "example",
                    "secret_env": ["HOMEBOY_CONTROLLER_PROVIDER_TOKEN"]
                })
                .to_string(),
            ],
            HashMap::new(),
        )
        .expect("controller dispatch provider config secret should hydrate");

        assert_eq!(
            plan.secret_env_names,
            vec!["HOMEBOY_CONTROLLER_PROVIDER_TOKEN".to_string()]
        );
        assert_eq!(
            plan.env_delta
                .get("HOMEBOY_CONTROLLER_PROVIDER_TOKEN")
                .map(String::as_str),
            Some("controller-secret-value")
        );
        assert!(plan
            .diagnostics
            .to_string()
            .contains("HOMEBOY_CONTROLLER_PROVIDER_TOKEN"));
        assert!(!plan
            .diagnostics
            .to_string()
            .contains("controller-secret-value"));
    }

    #[test]
    fn hydrate_agent_task_secret_env_fails_missing_controller_dispatch_provider_config_secret() {
        let _secret = RemovedEnvVar::new("HOMEBOY_CONTROLLER_MISSING_PROVIDER_TOKEN");
        crate::test_support::with_isolated_home(|_| {
            let mut env = HashMap::new();

            let err = hydrate_agent_task_secret_env(
                &[
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "controller".to_string(),
                    "run-from-spec".to_string(),
                    "@controller-spec.json".to_string(),
                    "--dispatch-provider-config".to_string(),
                    serde_json::json!({
                        "provider": "example",
                        "secret_env": ["HOMEBOY_CONTROLLER_MISSING_PROVIDER_TOKEN"]
                    })
                    .to_string(),
                ],
                &mut env,
            )
            .expect_err("missing controller dispatch provider config secret should fail");

            assert_eq!(err.details["field"].as_str(), Some("secret-env"));
            assert!(err
                .message
                .contains("HOMEBOY_CONTROLLER_MISSING_PROVIDER_TOKEN"));
            assert!(err.message.contains("missing"));
            assert!(err.details.to_string().contains("agent-task auth map-env"));
        });
    }

    #[test]
    fn declared_agent_task_controller_dispatch_provider_defaults_include_controller_sources() {
        let provider = fixture_provider_with_example_defaults();
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "controller".to_string(),
            "run-from-spec".to_string(),
            "@controller-spec.json".to_string(),
            "--dispatch-backend".to_string(),
            "sample-runtime".to_string(),
            "--dispatch-provider-config".to_string(),
            serde_json::json!({ "provider": "example-oauth" }).to_string(),
        ];

        let names = declared_agent_task_controller_secret_env_with_providers(
            &args,
            std::slice::from_ref(&provider),
        )
        .expect("dispatch provider config should parse");
        let sources = declared_agent_task_controller_secret_sources_with_providers(
            &args,
            1,
            std::slice::from_ref(&provider),
        )
        .expect("dispatch provider config sources should parse");

        assert!(names.contains(&"EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string()));
        assert_eq!(
            sources
                .get("EXAMPLE_PROVIDER_ACCESS_TOKEN")
                .and_then(|source| source.path.as_deref()),
            Some("~/.example-provider/auth.json")
        );
    }

    #[test]
    fn hydrate_agent_task_secret_env_fails_missing_provider_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing_path = temp.path().join("missing-provider-config.json");
        let mut env = HashMap::new();

        let err = hydrate_agent_task_secret_env(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--provider-config".to_string(),
                format!("@{}", missing_path.display()),
            ],
            &mut env,
        )
        .expect_err("missing provider config should fail secret discovery");

        assert_eq!(err.details["field"].as_str(), Some("provider-config"));
        assert_eq!(
            err.details["id"].as_str(),
            Some(format!("@{}", missing_path.display()).as_str())
        );
        assert!(err.message.contains("failed to read provider config"));
    }

    #[test]
    fn hydrate_agent_task_secret_env_fails_invalid_provider_config_json() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("provider-config.json");
        std::fs::write(&config_path, "{not-json").expect("write invalid config");
        let mut env = HashMap::new();

        let err = hydrate_agent_task_secret_env(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--provider-config".to_string(),
                format!("@{}", config_path.display()),
            ],
            &mut env,
        )
        .expect_err("invalid provider config JSON should fail secret discovery");

        assert_eq!(err.details["field"].as_str(), Some("provider-config"));
        assert_eq!(
            err.details["id"].as_str(),
            Some(format!("@{}", config_path.display()).as_str())
        );
        assert!(err.message.contains("failed to parse provider config JSON"));
    }

    #[test]
    fn hydrate_agent_task_secret_env_fails_non_object_provider_config() {
        let mut env = HashMap::new();

        let err = hydrate_agent_task_secret_env(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--provider-config".to_string(),
                "[]".to_string(),
            ],
            &mut env,
        )
        .expect_err("non-object provider config should fail secret discovery");

        assert_eq!(err.details["field"].as_str(), Some("provider-config"));
        assert!(err
            .message
            .contains("provider config must be a JSON object"));
    }

    #[test]
    fn declared_agent_task_cook_provider_defaults_include_controller_sources() {
        let provider = fixture_provider_with_example_defaults();
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--backend".to_string(),
            "sample-runtime".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({ "provider": "example-oauth" }).to_string(),
        ];

        let names = declared_agent_task_controller_secret_env_with_providers(
            &args,
            std::slice::from_ref(&provider),
        )
        .expect("provider config should parse");
        let sources = declared_agent_task_controller_secret_sources_with_providers(
            &args,
            1,
            std::slice::from_ref(&provider),
        )
        .expect("provider config sources should parse");

        assert!(names.contains(&"EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string()));
        let source = sources
            .get("EXAMPLE_PROVIDER_ACCESS_TOKEN")
            .expect("example provider default source discovered for cook");
        assert_eq!(source.source, "json-file");
        assert_eq!(
            source.path.as_deref(),
            Some("~/.example-provider/auth.json")
        );
        assert_eq!(source.field.as_deref(), Some("tokens.access_token"));
    }

    #[test]
    fn declared_agent_task_providers_still_include_provider_default_sources() {
        let provider = fixture_provider_with_example_defaults();
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "providers".to_string(),
            "--secret-env".to_string(),
            "EXAMPLE_PROVIDER_ACCESS_TOKEN".to_string(),
        ];

        let sources = declared_agent_task_controller_secret_sources_with_providers(
            &args,
            1,
            std::slice::from_ref(&provider),
        )
        .expect("provider default sources should parse");

        assert!(sources.contains_key("EXAMPLE_PROVIDER_ACCESS_TOKEN"));
    }

    #[test]
    fn declared_agent_task_secret_env_includes_run_plan_task_secrets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "idea",
                        "executor": {
                            "backend": "sample-runtime",
                            "secret_env": ["OPENAI_API_KEY"],
                            "config": {
                                "secret_env": ["GITHUB_TOKEN"]
                            }
                        },
                        "instructions": "Generate an idea."
                    },
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "design",
                        "executor": {
                            "backend": "sample-runtime",
                            "secret_env": ["OPENAI_API_KEY"]
                        },
                        "instructions": "Design the idea."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            format!("@{}", plan_path.display()),
            "--record-run-id".to_string(),
            "site-generation-loop".to_string(),
        ]);

        assert_eq!(
            names,
            vec!["GITHUB_TOKEN".to_string(), "OPENAI_API_KEY".to_string()]
        );
    }

    #[test]
    fn declared_agent_task_secret_env_dedupes_cli_and_run_plan_task_secrets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "idea",
                        "executor": {
                            "backend": "sample-runtime",
                            "secret_env": ["GITHUB_TOKEN"]
                        },
                        "instructions": "Generate an idea."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let names = declared_agent_task_secret_env(&[
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--secret-env".to_string(),
            "GITHUB_TOKEN".to_string(),
            "--plan".to_string(),
            format!("@{}", plan_path.display()),
            "--record-run-id".to_string(),
            "site-generation-loop".to_string(),
        ]);

        assert_eq!(names, vec!["GITHUB_TOKEN".to_string()]);
    }

    #[test]
    fn hydrate_agent_task_secret_env_defers_run_plan_secrets_to_runner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "runner-owned-secret-env-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "runner-owned",
                        "executor": {
                            "backend": "example",
                            "secret_env": ["HOMEBOY_RUNNER_ONLY_SECRET_ENV_TEST"]
                        },
                        "instructions": "Use runner-owned credentials."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let mut env = HashMap::new();
        let diagnostics = hydrate_agent_task_secret_env(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "run-plan".to_string(),
                "--plan".to_string(),
                format!("@{}", plan_path.display()),
            ],
            &mut env,
        )
        .expect("plan-declared secrets should not require controller auth");

        assert!(env.is_empty());
        assert_eq!(diagnostics["secret_env"].as_array().unwrap().len(), 0);
        assert_eq!(
            diagnostics["runner_deferred_secret_env"],
            serde_json::json!([
                {
                    "name": "HOMEBOY_RUNNER_ONLY_SECRET_ENV_TEST",
                    "source": "runner"
                }
            ])
        );
    }

    #[test]
    fn hydrate_agent_task_secret_env_resolves_provider_default_run_plan_secrets_on_controller() {
        let _access_token_env = RemovedEnvVar::new("EXAMPLE_PROVIDER_ACCESS_TOKEN");
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let auth_path = temp.path().join("provider-auth.json");
            std::fs::write(
                &auth_path,
                serde_json::json!({
                    "tokens": {
                        "access_token": "controller-access-token"
                    }
                })
                .to_string(),
            )
            .expect("write auth json");
            let plan_path = temp.path().join("plan.json");
            std::fs::write(
                &plan_path,
                serde_json::json!({
                    "schema": "homeboy/agent-task-plan/v1",
                    "plan_id": "provider-default-controller-secret-plan",
                    "tasks": [
                        {
                            "schema": "homeboy/agent-task-request/v1",
                            "task_id": "provider-task",
                            "executor": {
                                "backend": "sample-runtime",
                                "config": {
                                    "provider": "example-oauth"
                                }
                            },
                            "instructions": "Use provider credentials."
                        }
                    ]
                })
                .to_string(),
            )
            .expect("write plan");
            let provider =
                fixture_provider_with_example_defaults_at(&auth_path.display().to_string());
            let mut env = HashMap::new();

            let diagnostics = hydrate_agent_task_secret_env_with_providers(
                &run_plan_args(&plan_path),
                &mut env,
                std::slice::from_ref(&provider),
            )
            .expect("provider default source should resolve on controller");

            assert!(matches!(
                env.get("EXAMPLE_PROVIDER_ACCESS_TOKEN").map(String::as_str),
                Some("controller-access-token")
            ));
            assert_eq!(
                diagnostics["runner_deferred_secret_env"],
                serde_json::json!([])
            );
            assert_eq!(
                diagnostics["secret_env"],
                serde_json::json!([
                    {
                        "name": "EXAMPLE_PROVIDER_ACCESS_TOKEN",
                        "configured": true,
                        "source": "json-file"
                    }
                ])
            );
            assert!(!diagnostics.to_string().contains("controller-access-token"));
        });
    }

    #[test]
    fn preflight_agent_task_runner_secret_env_fails_missing_runner_ref() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "runner-secret-preflight-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "runner-owned",
                        "executor": {
                            "backend": "example",
                            "secret_env": ["HOMEBOY_RUNNER_MISSING_SECRET_TEST"]
                        },
                        "instructions": "Use runner-owned credentials."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let runner = fixture_runner(HashMap::new());
        let args = run_plan_args(&plan_path);
        let secret_env_plan = secret_env_plan_from_args(&args);
        let err = preflight_agent_task_runner_secret_env_plan(
            "lab-a",
            &runner,
            &args,
            &HashMap::new(),
            &secret_env_plan,
        )
        .expect_err("missing runner secret refs should fail before dispatch");

        assert_eq!(err.details["field"].as_str(), Some("secret-env"));
        assert!(err.message.contains("HOMEBOY_RUNNER_MISSING_SECRET_TEST"));
    }

    #[test]
    fn preflight_agent_task_runner_secret_env_accepts_runner_ref() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "runner-secret-preflight-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "runner-owned",
                        "executor": {
                            "backend": "example",
                            "secret_env": ["HOMEBOY_RUNNER_CONFIGURED_SECRET_TEST"]
                        },
                        "instructions": "Use runner-owned credentials."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let runner = fixture_runner(HashMap::from([(
            "HOMEBOY_RUNNER_CONFIGURED_SECRET_TEST".to_string(),
            RunnerSecretEnvRef {
                env: Some("HOMEBOY_RUNNER_CONFIGURED_SECRET_TEST".to_string()),
                file: None,
                secret: None,
            },
        )]));

        let args = run_plan_args(&plan_path);
        let secret_env_plan = secret_env_plan_from_args(&args);
        preflight_agent_task_runner_secret_env_plan(
            "lab-a",
            &runner,
            &args,
            &HashMap::new(),
            &secret_env_plan,
        )
        .expect("configured runner secret refs should pass");
    }

    #[test]
    fn preflight_agent_task_runner_secret_env_accepts_request_env() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "runner-secret-preflight-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "runner-owned",
                        "executor": {
                            "backend": "example",
                            "secret_env": ["HOMEBOY_RUNNER_INJECTED_SECRET_TEST"]
                        },
                        "instructions": "Use request-injected credentials."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let runner = fixture_runner(HashMap::new());
        let env = HashMap::from([(
            "HOMEBOY_RUNNER_INJECTED_SECRET_TEST".to_string(),
            "redacted-test-value".to_string(),
        )]);

        let args = run_plan_args(&plan_path);
        let secret_env_plan = secret_env_plan_from_args(&args);
        preflight_agent_task_runner_secret_env_plan(
            "lab-a",
            &runner,
            &args,
            &env,
            &secret_env_plan,
        )
        .expect("request-injected secret env should pass");
    }

    #[test]
    fn preflight_agent_task_runner_secret_env_dedupes_shared_missing_names_across_tasks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let plan_path = temp.path().join("plan.json");
        std::fs::write(
            &plan_path,
            serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "shared-missing-secret-plan",
                "tasks": [
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "idea",
                        "executor": {
                            "backend": "example",
                            "secret_env": [
                                "HOMEBOY_SHARED_MISSING_SECRET_TEST",
                                "HOMEBOY_OTHER_MISSING_SECRET_TEST"
                            ]
                        },
                        "instructions": "Generate an idea."
                    },
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "design",
                        "executor": {
                            "backend": "example",
                            "secret_env": [
                                "HOMEBOY_SHARED_MISSING_SECRET_TEST",
                                "HOMEBOY_OTHER_MISSING_SECRET_TEST"
                            ]
                        },
                        "instructions": "Design the idea."
                    },
                    {
                        "schema": "homeboy/agent-task-request/v1",
                        "task_id": "build",
                        "executor": {
                            "backend": "example",
                            "secret_env": [
                                "HOMEBOY_SHARED_MISSING_SECRET_TEST",
                                "HOMEBOY_OTHER_MISSING_SECRET_TEST"
                            ]
                        },
                        "instructions": "Build the idea."
                    }
                ]
            })
            .to_string(),
        )
        .expect("write plan");

        let runner = fixture_runner(HashMap::new());
        let args = run_plan_args(&plan_path);
        let secret_env_plan = secret_env_plan_from_args(&args);
        let err = preflight_agent_task_runner_secret_env_plan(
            "lab-a",
            &runner,
            &args,
            &HashMap::new(),
            &secret_env_plan,
        )
        .expect_err("missing runner secret refs should fail before dispatch");

        // The operator-facing message must list each missing name exactly once,
        // preserving first-seen order, even though three tasks declare them.
        let problem = err.details["problem"]
            .as_str()
            .expect("problem detail string");
        assert!(err.message.ends_with(
            "missing required agent-task runner secret env on runner `lab-a`: \
HOMEBOY_SHARED_MISSING_SECRET_TEST, HOMEBOY_OTHER_MISSING_SECRET_TEST"
        ));
        assert_eq!(
            problem,
            "missing required agent-task runner secret env on runner `lab-a`: \
HOMEBOY_SHARED_MISSING_SECRET_TEST, HOMEBOY_OTHER_MISSING_SECRET_TEST"
        );
        // No repeats in either prose field.
        assert_eq!(
            err.message
                .matches("HOMEBOY_SHARED_MISSING_SECRET_TEST")
                .count(),
            1
        );
        assert_eq!(
            problem
                .matches("HOMEBOY_SHARED_MISSING_SECRET_TEST")
                .count(),
            1
        );

        // Per-task provenance is reported in a separate structured field.
        let required_by_tasks = err.details["required_by_tasks"]
            .as_object()
            .expect("required_by_tasks object");
        let shared_tasks = required_by_tasks["HOMEBOY_SHARED_MISSING_SECRET_TEST"]
            .as_array()
            .expect("shared name task list");
        let shared_task_ids: Vec<&str> = shared_tasks.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(shared_task_ids, vec!["idea", "design", "build"]);
        let other_task_ids: Vec<&str> = required_by_tasks["HOMEBOY_OTHER_MISSING_SECRET_TEST"]
            .as_array()
            .expect("other name task list")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(other_task_ids, vec!["idea", "design", "build"]);
    }

    fn run_plan_args(plan_path: &std::path::Path) -> Vec<String> {
        vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            format!("@{}", plan_path.display()),
        ]
    }

    fn secret_env_plan_from_args(args: &[String]) -> SecretEnvPlan {
        SecretEnvPlan::from_secret_env_names(declared_agent_task_run_plan_secret_env(args))
    }

    fn fixture_runner(secret_env: HashMap<String, RunnerSecretEnvRef>) -> Runner {
        Runner {
            id: "lab-a".to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some("server-a".to_string()),
            workspace_root: Some("/tmp/lab-a".to_string()),
            settings: RunnerSettings::default(),
            env: HashMap::new(),
            secret_env,
            resources: HashMap::new(),
            policy: RunnerPolicy::default(),
        }
    }

    struct RemovedEnvVar {
        name: &'static str,
        value: Option<String>,
    }

    impl RemovedEnvVar {
        fn new(name: &'static str) -> Self {
            let value = std::env::var(name).ok();
            std::env::remove_var(name);
            Self { name, value }
        }
    }

    impl Drop for RemovedEnvVar {
        fn drop(&mut self) {
            if let Some(value) = &self.value {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    fn fixture_provider_with_example_defaults() -> AgentTaskExecutorProvider {
        fixture_provider_with_example_defaults_at("~/.example-provider/auth.json")
    }

    fn fixture_provider_with_example_defaults_at(path: &str) -> AgentTaskExecutorProvider {
        let mut provider_defaults = std::collections::BTreeMap::new();
        provider_defaults.insert(
            "example-oauth".to_string(),
            serde_json::json!({
                "secret_env": ["EXAMPLE_PROVIDER_ACCESS_TOKEN"],
                "secret_env_sources": {
                    "EXAMPLE_PROVIDER_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": path,
                        "field": "tokens.access_token"
                    }
                }
            }),
        );

        AgentTaskExecutorProvider {
            schema: "homeboy/agent-task-executor-provider/v1".to_string(),
            id: "sample-runtime.example-oauth".to_string(),
            label: None,
            backend: "sample-runtime".to_string(),
            default_backend: true,
            command: "sample-runtime agent".to_string(),
            command_argv: Vec::new(),
            invocation: CommandInvocation::default(),
            request_schema: "homeboy/agent-task-request/v1".to_string(),
            outcome_schema: "homeboy/agent-task-outcome/v1".to_string(),
            capabilities: Vec::new(),
            secret_requirements: Vec::new(),
            secret_env_requirements: Vec::new(),
            workspace_materialization: None,
            provider_defaults,
            runner_readiness: Vec::new(),
            runner_sources: Vec::new(),
            dependency_failure_patterns: Vec::new(),
            lab_runtime_components: Vec::new(),
            timeout_artifact_discovery: Default::default(),
            role_aliases: Default::default(),
            runtime_contract: Default::default(),
            extension_id: None,
            extension_path: None,
            runtime_id: None,
            runtime_path: None,
            extra: std::collections::BTreeMap::new(),
        }
    }
}
