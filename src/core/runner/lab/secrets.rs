//! Command-specific secret hydration for Lab offload.
//!
//! Splits agent-task vs trace secret discovery from the offload orchestrator so
//! that the secret env contract for each command family stays close to its
//! parser. Both entry points (`hydrate_agent_task_secret_env`,
//! `hydrate_trace_secret_env`) mutate the runner exec environment in place and
//! return diagnostic metadata that is embedded into the Lab offload envelope.

use std::collections::HashMap;

use crate::core::agent_tasks::provider::{
    provider_runner_secret_env_for_plan, provider_runner_secret_env_for_plan_with_providers,
    provider_secret_sources_for_plan, provider_secret_sources_for_plan_with_providers,
    provider_secret_sources_for_providers, AgentTaskExecutorProvider,
    ExtensionProviderAgentTaskExecutor,
};
use crate::core::agent_tasks::scheduler::AgentTaskPlan;
use crate::core::agent_tasks::secrets as agent_task_secrets;
use crate::core::agent_tasks::{
    AgentTaskExecutor, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
};
use crate::core::{config, Error, Result};

use super::super::Runner;
use super::args_util::{non_empty_arg, subcommand_index};

/// Provenance key used when a missing runner secret_env name is contributed by
/// the plan's provider runner requirements rather than an individual task.
const PLAN_PROVIDER_PROVENANCE: &str = "plan:provider";

pub(super) fn hydrate_agent_task_secret_env(
    args: &[String],
    env: &mut HashMap<String, String>,
) -> Result<serde_json::Value> {
    let names = declared_agent_task_controller_secret_env(args);
    let mut runner_deferred_names = declared_agent_task_run_plan_secret_env(args);
    runner_deferred_names.sort();
    runner_deferred_names.dedup();
    if names.is_empty() && runner_deferred_names.is_empty() {
        return Ok(serde_json::json!({
            "schema": "homeboy/lab-agent-task-secret-env/v1",
            "secret_env": [],
            "runner_deferred_secret_env": [],
        }));
    }

    let fallback_sources = declared_agent_task_controller_secret_sources(args);
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
    let mut names = declared_agent_task_controller_secret_env(args);
    names.extend(declared_agent_task_run_plan_secret_env(args));
    names.sort();
    names.dedup();
    names
}

pub(super) fn preflight_agent_task_runner_secret_env(
    runner_id: &str,
    runner: &Runner,
    args: &[String],
    env: &HashMap<String, String>,
) -> Result<()> {
    let tasks = declared_agent_task_run_plan_secret_env_by_task(args);
    if tasks.is_empty() {
        return Ok(());
    }

    // Dedupe missing env names while preserving first-seen order so the
    // operator-facing message lists each name exactly once, even when several
    // plan tasks declare the same requirement.
    let mut missing: Vec<String> = Vec::new();
    let mut required_by_tasks: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for task in &tasks {
        for name in &task.secret_env {
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
}

fn declared_agent_task_controller_secret_env_with_providers(
    args: &[String],
    providers: &[AgentTaskExecutorProvider],
) -> Vec<String> {
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
        if arg == "--provider-config" {
            if let Some(spec) = args.get(index + 1) {
                names.extend(declared_provider_config_secret_env(spec));
            }
            index += 2;
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--provider-config=") {
            names.extend(declared_provider_config_secret_env(spec));
        }
        index += 1;
    }
    if let Some(plan) = agent_task_dispatch_provider_plan_from_args(args) {
        names.extend(provider_runner_secret_env_for_plan_with_providers(
            &plan, providers,
        ));
    }
    names.sort();
    names.dedup();
    names
}

fn declared_agent_task_controller_secret_sources(
    args: &[String],
) -> HashMap<String, crate::core::defaults::AgentTaskSecretSource> {
    let Some(agent_task_index) = subcommand_index(args, "agent-task") else {
        return HashMap::new();
    };
    let executor = ExtensionProviderAgentTaskExecutor::discover();
    declared_agent_task_controller_secret_sources_with_providers(
        args,
        agent_task_index,
        executor.providers(),
    )
}

fn declared_agent_task_controller_secret_sources_with_providers(
    args: &[String],
    agent_task_index: usize,
    providers: &[AgentTaskExecutorProvider],
) -> HashMap<String, crate::core::defaults::AgentTaskSecretSource> {
    if args.get(agent_task_index + 1).map(String::as_str) != Some("providers") {
        return agent_task_dispatch_provider_plan_from_args(args)
            .map(|plan| provider_secret_sources_for_plan_with_providers(&plan, providers))
            .unwrap_or_default();
    }

    provider_secret_sources_for_providers(providers)
}

fn agent_task_dispatch_provider_plan_from_args(args: &[String]) -> Option<AgentTaskPlan> {
    let agent_task_index = subcommand_index(args, "agent-task")?;
    let command = args.get(agent_task_index + 1)?.as_str();
    if !matches!(command, "cook" | "dispatch" | "loop") {
        return None;
    }

    let backend = option_arg_after(args, agent_task_index + 2, "--backend")?;
    let selector = option_arg_after(args, agent_task_index + 2, "--selector");
    let model = option_arg_after(args, agent_task_index + 2, "--model");
    let provider_config = option_arg_after(args, agent_task_index + 2, "--provider-config")
        .and_then(|spec| config::read_json_spec_to_string(&spec).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(serde_json::Value::Null);

    Some(AgentTaskPlan::new(
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
    ))
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

fn declared_provider_config_secret_env(spec: &str) -> Vec<String> {
    let raw = match config::read_json_spec_to_string(spec) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let Some(config) = value.as_object() else {
        return Vec::new();
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
    names
}

fn declared_agent_task_run_plan_secret_env(args: &[String]) -> Vec<String> {
    declared_agent_task_run_plan_secret_env_by_task(args)
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
    let Some(plan_spec) = agent_task_run_plan_plan_spec(args) else {
        return Vec::new();
    };
    if plan_spec == "-" {
        return Vec::new();
    }
    let raw = match config::read_json_spec_to_string(&plan_spec) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let plan: AgentTaskPlan = match serde_json::from_str(&raw) {
        Ok(plan) => plan,
        Err(_) => return Vec::new(),
    };

    let mut tasks = Vec::new();
    let provider_names = provider_runner_secret_env_for_plan(&plan);
    let provider_secret_sources: Vec<String> = provider_secret_sources_for_plan(&plan)
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
    fn declared_agent_task_cook_provider_defaults_include_controller_sources() {
        let provider = fixture_provider_with_codex_defaults();
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--backend".to_string(),
            "codebox".to_string(),
            "--provider-config".to_string(),
            serde_json::json!({ "provider": "codex" }).to_string(),
        ];

        let names = declared_agent_task_controller_secret_env_with_providers(
            &args,
            std::slice::from_ref(&provider),
        );
        let sources = declared_agent_task_controller_secret_sources_with_providers(
            &args,
            1,
            std::slice::from_ref(&provider),
        );

        assert!(names.contains(&"AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string()));
        let source = sources
            .get("AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN")
            .expect("codex provider default source discovered for cook");
        assert_eq!(source.source, "json-file");
        assert_eq!(source.path.as_deref(), Some("~/.codex/auth.json"));
        assert_eq!(source.field.as_deref(), Some("tokens.access_token"));
    }

    #[test]
    fn declared_agent_task_providers_still_include_provider_default_sources() {
        let provider = fixture_provider_with_codex_defaults();
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "providers".to_string(),
            "--secret-env".to_string(),
            "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN".to_string(),
        ];

        let sources = declared_agent_task_controller_secret_sources_with_providers(
            &args,
            1,
            std::slice::from_ref(&provider),
        );

        assert!(sources.contains_key("AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN"));
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
                            "backend": "codebox",
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
                            "backend": "codebox",
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
                            "backend": "codebox",
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
        let err = preflight_agent_task_runner_secret_env(
            "lab-a",
            &runner,
            &run_plan_args(&plan_path),
            &HashMap::new(),
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

        preflight_agent_task_runner_secret_env(
            "lab-a",
            &runner,
            &run_plan_args(&plan_path),
            &HashMap::new(),
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

        preflight_agent_task_runner_secret_env("lab-a", &runner, &run_plan_args(&plan_path), &env)
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
        let err = preflight_agent_task_runner_secret_env(
            "lab-a",
            &runner,
            &run_plan_args(&plan_path),
            &HashMap::new(),
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

    fn fixture_provider_with_codex_defaults() -> AgentTaskExecutorProvider {
        let mut provider_defaults = std::collections::BTreeMap::new();
        provider_defaults.insert(
            "codex".to_string(),
            serde_json::json!({
                "secret_env": ["AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN"],
                "secret_env_sources": {
                    "AI_PROVIDER_OPENAI_CODEX_ACCESS_TOKEN": {
                        "source": "json-file",
                        "path": "~/.codex/auth.json",
                        "field": "tokens.access_token"
                    }
                }
            }),
        );

        AgentTaskExecutorProvider {
            schema: "homeboy/agent-task-executor-provider/v1".to_string(),
            id: "wp-codebox.codex".to_string(),
            label: None,
            backend: "codebox".to_string(),
            default_backend: true,
            command: "wp-codebox agent".to_string(),
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
