#![cfg(test)]

mod part_a;
mod part_b;

use super::*;
use crate::RunnerKind;
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
