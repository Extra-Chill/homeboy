use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::{AgentTaskExecutorProvider, AgentTaskRequest};
use crate::agent_task_scheduler::AgentTaskExecutionContext;
use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_core::{Error, Result};

pub const AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_SCHEMA: &str =
    "homeboy/agent-task-provider-launch-context/v1";
pub const AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_JSON_ENV: &str =
    "HOMEBOY_AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_JSON";

// These are process mechanics, not credentials. Provider-specific additions
// must be declared by the provider invocation instead of widening this list.
const BASE_INHERITED_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "TERM",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "RUSTUP_HOME",
    "CARGO_HOME",
    "NVM_DIR",
    "VOLTA_HOME",
    "PNPM_HOME",
    "BUN_INSTALL",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskProviderLaunchContext {
    pub schema: String,
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub task_id: String,
    pub attempt: u32,
    pub provider_id: String,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited_env_names: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub public_env: BTreeMap<String, String>,
    pub secret_env_plan: SecretEnvPlan,
    pub portable: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub portability_blockers: Vec<String>,
}

impl AgentTaskProviderLaunchContext {
    pub fn materialize(
        request: &AgentTaskRequest,
        provider: &AgentTaskExecutorProvider,
        execution: &AgentTaskExecutionContext,
        cwd: Option<&Path>,
        declared_env: &[(String, String)],
    ) -> Result<Self> {
        let mut inherited_env_names = BASE_INHERITED_ENV
            .iter()
            .filter(|name| std::env::var_os(name).is_some())
            .map(|name| (*name).to_string())
            .collect::<BTreeSet<_>>();
        let secret_env_plan =
            super::secrets::provider_secret_env_plan_with_status(provider, request);
        let secret_env_names = secret_env_plan
            .secret_env_names()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let mut public_env = BTreeMap::new();

        for env_ref in &provider.invocation.env {
            if let Some(value) = env_ref.value.as_ref() {
                if env_ref.redacted.unwrap_or(false) || secret_env_names.contains(&env_ref.name) {
                    return Err(Error::validation_invalid_argument(
                        "provider.invocation.env",
                        "provider launch context rejects inline secret environment values",
                        Some(env_ref.name.clone()),
                        None,
                    ));
                }
                public_env.insert(env_ref.name.clone(), value.clone());
                continue;
            }
            if secret_env_names.contains(&env_ref.name) {
                continue;
            }
            if matches!(env_ref.source.as_deref(), Some("env" | "inherit")) {
                if std::env::var_os(&env_ref.name).is_none() && env_ref.required.unwrap_or(false) {
                    return Err(Error::validation_invalid_argument(
                        "provider.invocation.env",
                        "provider launch context is missing a required inherited environment variable",
                        Some(env_ref.name.clone()),
                        None,
                    ));
                }
                if std::env::var_os(&env_ref.name).is_some() {
                    inherited_env_names.insert(env_ref.name.clone());
                }
            }
        }

        for (name, value) in declared_env {
            if !secret_env_names.contains(name) {
                public_env.insert(name.clone(), value.clone());
            }
        }
        let ambient_secret_env_names = secret_env_plan
            .status
            .iter()
            .filter(|status| status.configured && status.source == "env")
            .map(|status| status.name.clone())
            .collect::<Vec<_>>();
        let portability_blockers = (!ambient_secret_env_names.is_empty())
            .then(|| format!("ambient_secret_env:{}", ambient_secret_env_names.join(",")))
            .into_iter()
            .collect::<Vec<_>>();

        Ok(Self {
            schema: AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_SCHEMA.to_string(),
            plan_id: execution.plan_id.clone(),
            run_id: execution.run_id.clone(),
            task_id: request.task_id.clone(),
            attempt: execution.attempt,
            provider_id: provider.id.clone(),
            backend: provider.backend.clone(),
            runtime_id: provider.runtime_id.clone(),
            workspace_root: request.workspace.root.clone(),
            execution_cwd: cwd.map(|path| path.display().to_string()),
            inherited_env_names: inherited_env_names.into_iter().collect(),
            public_env,
            secret_env_plan,
            portable: portability_blockers.is_empty(),
            portability_blockers,
        })
    }

    pub fn apply_declared_environment(&self, command: &mut Command) -> Result<()> {
        if self.schema != AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "provider_launch_context.schema",
                "provider launch context has an unsupported schema",
                Some(self.schema.clone()),
                None,
            ));
        }

        command.env_clear();
        for name in &self.inherited_env_names {
            let value = std::env::var_os(name).ok_or_else(|| {
                Error::validation_invalid_argument(
                    "provider_launch_context.inherited_env_names",
                    "declared inherited environment changed before provider spawn",
                    Some(name.clone()),
                    None,
                )
            })?;
            command.env(name, value);
        }
        command.envs(&self.public_env);
        command.env(
            AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_JSON_ENV,
            serde_json::to_string(self).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize provider launch context".to_string()),
                )
            })?,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_scheduler::{AgentTaskPlan, AgentTaskScheduler};
    use serde_json::json;
    use std::sync::Arc;

    fn request(task_id: &str, secret_name: &str) -> AgentTaskRequest {
        serde_json::from_value(json!({
            "task_id": task_id,
            "executor": {
                "backend": "test",
                "secret_env": [secret_name]
            },
            "instructions": "run"
        }))
        .expect("request")
    }

    fn provider(secret_name: &str) -> AgentTaskExecutorProvider {
        serde_json::from_value(json!({
            "id": "test.provider",
            "backend": "test",
            "runtime_id": "test-runtime",
            "invocation": {
                "argv": ["true"],
                "env": [
                    { "name": secret_name, "source": "secret_env", "redacted": true },
                    { "name": "PROVIDER_MODE", "value": "test" }
                ]
            }
        }))
        .expect("provider")
    }

    #[test]
    fn launch_context_is_redacted_and_durably_bound_to_the_provider_reservation() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let secret_name = format!("HOMEBOY_TEST_LAUNCH_SECRET_{}", std::process::id());
        let secret_value = format!("launch-secret-value-{}", std::process::id());
        let _secret = homeboy_core::test_support::EnvVarGuard::set(&secret_name, &secret_value);
        let request = request("launch-context-task", &secret_name);
        let provider = provider(&secret_name);
        let run_id = "launch-context-run";
        let plan = AgentTaskPlan::new("launch-context-plan", vec![request.clone()]);
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("durable run");
        let store =
            crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
                .expect("lifecycle store");
        AgentTaskScheduler::new(Arc::new(
            super::super::ExtensionProviderAgentTaskExecutor::with_providers(vec![provider]),
        ))
        .with_run_id(run_id)
        .with_lifecycle_store(store.clone())
        .run(plan);

        let record = store.read_record(run_id).expect("durable record");
        let context = &record.metadata["provider_executions"][0]["launch_context"];
        assert_eq!(context["schema"], AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_SCHEMA);
        assert_eq!(context["provider_id"], "test.provider");
        assert_eq!(context["public_env"]["PROVIDER_MODE"], "test");
        assert_eq!(
            context["public_env"]["HOMEBOY_AGENT_TASK_PROVIDER_ID"],
            "test.provider"
        );
        assert_eq!(context["portable"], false);
        assert!(context["portability_blockers"][0]
            .as_str()
            .expect("portability blocker")
            .contains(&secret_name));
        assert!(context["public_env"].get(&secret_name).is_none());
        assert!(!context["inherited_env_names"]
            .as_array()
            .expect("inherited environment names")
            .iter()
            .any(|name| name == &secret_name));
        assert!(!record.metadata.to_string().contains(&secret_value));
    }
}
