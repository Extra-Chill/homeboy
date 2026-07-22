#![cfg(test)]

mod part_a;
mod part_b;

use super::*;
use crate::RunnerKind;
use homeboy_core::command_invocation::CommandInvocation;
use homeboy_core::server::{RunnerPolicy, RunnerSecretEnvRef, RunnerSettings};

pub(super) fn run_plan_args(plan_path: &std::path::Path) -> Vec<String> {
    vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--plan".to_string(),
        format!("@{}", plan_path.display()),
    ]
}

pub(super) fn secret_env_plan_from_args(args: &[String]) -> SecretEnvPlan {
    SecretEnvPlan::from_secret_env_names(declared_agent_task_run_plan_secret_env(args))
}

pub(super) fn fixture_runner(secret_env: HashMap<String, RunnerSecretEnvRef>) -> Runner {
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

pub(super) struct RemovedEnvVar {
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

pub(super) fn fixture_provider_with_example_defaults() -> AgentTaskExecutorProvider {
    fixture_provider_with_example_defaults_at("~/.example-provider/auth.json")
}

pub(super) fn fixture_provider_with_example_defaults_at(path: &str) -> AgentTaskExecutorProvider {
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
        readiness_invocation: None,
        runner_sources: Vec::new(),
        dependency_failure_patterns: Vec::new(),
        config_preflights: Vec::new(),
        lab_runtime_components: Vec::new(),
        timeout_artifact_discovery: Default::default(),
        role_aliases: Default::default(),
        cli: Default::default(),
        result_contract: Default::default(),
        runtime_contract: Default::default(),
        extension_id: None,
        extension_path: None,
        runtime_package_source: None,
        runtime_id: None,
        runtime_path: None,
        extra: std::collections::BTreeMap::new(),
    }
}
