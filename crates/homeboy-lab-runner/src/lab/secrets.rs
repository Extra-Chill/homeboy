//! Command-specific secret hydration for Lab offload.
//!
//! Splits agent-task vs trace secret discovery from the offload orchestrator so
//! that the secret env contract for each command family stays close to its
//! parser. `build_lab_secret_env_handoff_plan` is the single offload boundary:
//! it resolves controller-owned secret values once, records runner-deferred
//! requirements by name, and exposes only redacted diagnostics for metadata.

use std::collections::{BTreeMap, HashMap};

use homeboy_agents::agent_tasks::provider::{
    provider_runner_secret_env_for_plan_with_providers,
    provider_secret_sources_for_plan_with_providers, provider_secret_sources_for_providers,
    AgentTaskExecutorProvider, ExtensionProviderAgentTaskExecutor,
};
use homeboy_agents::agent_tasks::scheduler::AgentTaskPlan;
use homeboy_agents::agent_tasks::secrets as agent_task_secrets;
use homeboy_agents::agent_tasks::{
    AgentTaskExecutor, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
};
use homeboy_core::lab_contract::LabSecretEnvSource;
use homeboy_core::secret_env_plan::{
    SecretEnvHandoffEntry, SecretEnvPlan, SecretEnvPlanMaterializeRequest,
};
use homeboy_core::{config, Error, Result};

use super::super::Runner;
use super::args_util::{non_empty_arg, subcommand_index};

/// Provenance key used when a missing runner secret_env name is contributed by
/// the plan's provider runner requirements rather than an individual task.
const PLAN_PROVIDER_PROVENANCE: &str = "plan:provider";
const LAB_SECRET_ENV_HANDOFF_SCHEMA: &str = "homeboy/lab-secret-env-handoff/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LabSecretEnvHandoffPlan {
    pub(crate) env_delta: HashMap<String, String>,
    pub(crate) diagnostics: serde_json::Value,
    pub(crate) entries: Vec<SecretEnvHandoffEntry>,
    pub(crate) secret_env_plan: SecretEnvPlan,
    pub(crate) secret_env_names: Vec<String>,
    pub(crate) runner_deferred_secret_env: Vec<String>,
}

pub(crate) fn build_lab_secret_env_handoff_plan(
    secret_env_sources: &[LabSecretEnvSource],
    args: &[String],
    mut env_delta: HashMap<String, String>,
) -> Result<LabSecretEnvHandoffPlan> {
    let declared_public_env_delta = env_delta.clone();
    let agent_task_secret_env = if secret_env_sources.contains(&LabSecretEnvSource::AgentTask) {
        hydrate_agent_task_secret_env(args, &mut env_delta)?
    } else {
        empty_agent_task_secret_env_metadata()
    };
    let trace_secret_env = if secret_env_sources.contains(&LabSecretEnvSource::Trace) {
        hydrate_trace_secret_env(args, &mut env_delta)?
    } else {
        homeboy_core::trace_secrets::empty_status()
    };
    let tunnel_secret_env = if secret_env_sources.contains(&LabSecretEnvSource::Tunnel) {
        hydrate_tunnel_secret_env(args, &mut env_delta)?
    } else {
        empty_tunnel_secret_env_metadata()
    };
    let mut secret_env_names = Vec::new();
    if secret_env_sources.contains(&LabSecretEnvSource::AgentTask) {
        secret_env_names.extend(declared_agent_task_secret_env_for_handoff(args)?);
    }
    if secret_env_sources.contains(&LabSecretEnvSource::Trace) {
        secret_env_names.extend(declared_trace_secret_env(args));
    }
    if secret_env_sources.contains(&LabSecretEnvSource::Tunnel) {
        secret_env_names.extend(declared_tunnel_secret_env(args));
    }
    secret_env_names.sort();
    secret_env_names.dedup();
    let public_env = declared_public_env_delta
        .into_iter()
        .filter(|(name, _)| !secret_env_names.contains(name))
        .collect::<BTreeMap<_, _>>();
    let secret_env_plan = materialized_lab_secret_env_plan(public_env, secret_env_names.clone())?;
    let resolved_secret_env_names = secret_env_plan.secret_env_names();
    let secret_values = env_delta
        .iter()
        .filter(|(name, _)| resolved_secret_env_names.contains(name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    env_delta = secret_env_plan
        .materialize(secret_values)
        .into_iter()
        .collect::<HashMap<_, _>>();

    let mut runner_deferred_secret_env = Vec::new();
    runner_deferred_secret_env.extend(runner_deferred_secret_env_names(&agent_task_secret_env));
    runner_deferred_secret_env.extend(runner_deferred_secret_env_names(&tunnel_secret_env));
    runner_deferred_secret_env.sort();
    runner_deferred_secret_env.dedup();
    let entries = lab_secret_env_handoff_entries(
        &env_delta,
        &runner_deferred_secret_env,
        &agent_task_secret_env,
        &trace_secret_env,
        &tunnel_secret_env,
    );

    let diagnostics = serde_json::json!({
        "schema": LAB_SECRET_ENV_HANDOFF_SCHEMA,
        "entries": entries,
        "secret_env_plan": redacted_lab_secret_env_plan(&secret_env_plan),
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
        entries,
        secret_env_plan,
        secret_env_names,
        runner_deferred_secret_env,
    })
}

fn empty_agent_task_secret_env_metadata() -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/lab-agent-task-secret-env/v1",
        "secret_env": [],
        "runner_deferred_secret_env": [],
    })
}

fn empty_tunnel_secret_env_metadata() -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/lab-tunnel-secret-env/v1",
        "runner_deferred_secret_env": [],
    })
}

fn materialized_lab_secret_env_plan(
    public_env: BTreeMap<String, String>,
    secret_env_names: Vec<String>,
) -> Result<SecretEnvPlan> {
    SecretEnvPlan::materialize_request(SecretEnvPlanMaterializeRequest {
        public_env,
        secret_env_names,
        diagnostic_source: Some("lab-secret-env-handoff".to_string()),
        ..SecretEnvPlanMaterializeRequest::default()
    })
    .map(|materialization| materialization.plan)
    .map_err(|error| {
        Error::validation_invalid_argument(
            "secret-env-plan",
            error.message,
            None,
            Some(vec![
                "Declare Lab public env and secret env through the secret-env plan contract before dispatch."
                    .to_string(),
            ]),
        )
    })
}

fn redacted_lab_secret_env_plan(plan: &SecretEnvPlan) -> SecretEnvPlan {
    let mut redacted = plan.redacted();
    let replacement = redacted.redaction.replacement.clone();
    for value in redacted.public_env.values_mut() {
        *value = replacement.clone();
    }
    redacted
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

fn lab_secret_env_handoff_entries(
    env_delta: &HashMap<String, String>,
    runner_deferred_secret_env: &[String],
    agent_task_secret_env: &serde_json::Value,
    trace_secret_env: &serde_json::Value,
    tunnel_secret_env: &serde_json::Value,
) -> Vec<SecretEnvHandoffEntry> {
    let mut names = env_delta.keys().cloned().collect::<Vec<_>>();
    names.sort();
    let mut entries = names
        .into_iter()
        .map(|name| SecretEnvHandoffEntry {
            source: secret_env_source_for_name(&name, &[agent_task_secret_env, trace_secret_env])
                .unwrap_or_else(|| "env-delta".to_string()),
            name,
            owner: "controller".to_string(),
            destination: "runner".to_string(),
            status: "forwarded".to_string(),
            remediation: None,
        })
        .collect::<Vec<_>>();

    for name in runner_deferred_secret_env {
        if env_delta.contains_key(name) {
            continue;
        }
        entries.push(SecretEnvHandoffEntry {
            source: secret_env_source_for_name(name, &[agent_task_secret_env, tunnel_secret_env])
                .unwrap_or_else(|| "runner".to_string()),
            name: name.clone(),
            owner: "runner".to_string(),
            destination: "runner".to_string(),
            status: "deferred".to_string(),
            remediation: Some(
                "Configure the selected runner secret_env references for the declared names before Lab dispatch."
                    .to_string(),
            ),
        });
    }

    entries
}

fn secret_env_source_for_name(
    name: &str,
    metadata_values: &[&serde_json::Value],
) -> Option<String> {
    metadata_values.iter().find_map(|metadata| {
        ["secret_env", "runner_deferred_secret_env"]
            .into_iter()
            .find_map(|field| secret_env_source_for_name_in_field(name, metadata, field))
    })
}

fn secret_env_source_for_name_in_field(
    name: &str,
    metadata: &serde_json::Value,
    field: &str,
) -> Option<String> {
    metadata
        .get(field)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| item.get("name").and_then(serde_json::Value::as_str) == Some(name))
        .and_then(|item| item.get("source").and_then(serde_json::Value::as_str))
        .map(str::to_string)
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
        return Ok(empty_agent_task_secret_env_metadata());
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
        return Ok(homeboy_core::trace_secrets::empty_status());
    }

    let project_id = trace_project_id_from_args(args);
    let (resolved, statuses) =
        homeboy_core::trace_secrets::resolve_secret_env(&names, project_id.as_deref())?;
    for (name, value) in resolved {
        env.insert(name, value);
    }

    Ok(homeboy_core::trace_secrets::status_metadata(statuses))
}

pub(super) fn hydrate_tunnel_secret_env(
    args: &[String],
    _env: &mut HashMap<String, String>,
) -> Result<serde_json::Value> {
    let names = declared_tunnel_secret_env(args);
    if names.is_empty() {
        return Ok(empty_tunnel_secret_env_metadata());
    }

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

pub(super) fn preflight_lab_secret_env_handoff(
    runner_id: &str,
    runner: Option<&Runner>,
    env: &HashMap<String, String>,
    handoff: &LabSecretEnvHandoffPlan,
) -> Result<()> {
    let mut missing = Vec::new();
    for entry in &handoff.entries {
        let configured = match entry.owner.as_str() {
            "controller" => env.contains_key(&entry.name),
            "runner" => runner
                .map(|runner| runner.secret_env.contains_key(&entry.name))
                .unwrap_or(true),
            _ => true,
        };
        if configured {
            continue;
        }

        let mut missing_entry = entry.clone();
        missing_entry.status = "missing".to_string();
        missing_entry.remediation = Some(secret_env_handoff_remediation(entry));
        missing.push(missing_entry);
    }

    if missing.is_empty() {
        return Ok(());
    }

    let names = missing
        .iter()
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut error = Error::validation_invalid_argument(
        "secret-env",
        format!(
            "missing required Lab secret env before dispatch on runner `{}`: {}",
            runner_id, names
        ),
        Some(runner_id.to_string()),
        Some(vec![
            "Configure controller-forwarded secrets in the controller environment or Homeboy agent-task secret config before Lab dispatch.".to_string(),
            "Configure runner-deferred secrets with the selected runner secret_env references before Lab dispatch.".to_string(),
            "Use `homeboy runner env <runner>` to inspect redacted runner secret_env refs without printing values.".to_string(),
        ]),
    );
    if let serde_json::Value::Object(details) = &mut error.details {
        details.insert(
            "missing_required_secret_env_handoff".to_string(),
            serde_json::to_value(&missing).unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
        );
        details.insert(
            "secret_env_handoff".to_string(),
            handoff.diagnostics.clone(),
        );
    }
    Err(error)
}

fn secret_env_handoff_remediation(entry: &SecretEnvHandoffEntry) -> String {
    match entry.owner.as_str() {
        "controller" => "Configure this secret in the controller environment or Homeboy agent-task secret config so Lab can forward it without exposing the value.".to_string(),
        "runner" => "Configure this name in the selected runner secret_env references before Lab dispatch.".to_string(),
        _ => "Configure the required secret env name before Lab dispatch.".to_string(),
    }
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
) -> Result<HashMap<String, homeboy_core::defaults::AgentTaskSecretSource>> {
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
mod tests;
