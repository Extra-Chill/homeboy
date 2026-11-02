//! The Pi executor is built in; provider discovery never reads extensions.

use super::*;

pub(crate) fn discover_agent_task_executor_providers() -> Vec<AgentTaskExecutorProvider> {
    vec![AgentTaskExecutorProvider {
        schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.into(),
        id: "pi.agent-task-executor".into(),
        label: Some("Pi agent task executor".into()),
        backend: "pi".into(),
        default_backend: false,
        command: String::new(),
        invocation: CommandInvocation {
            argv: vec!["pi".into()],
            ..Default::default()
        },
        request_schema: AGENT_TASK_REQUEST_SCHEMA.into(),
        outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.into(),
        capabilities: [
            "cli_runtime",
            "workspace_materialization",
            "structured_outcome",
            "provider_owned_auth",
            "provider_owned_session",
        ]
        .map(String::from)
        .into(),
        workspace_materialization: Some(AgentTaskProviderWorkspaceMaterialization {
            cwd: Some("request_workspace".into()),
            requires_git: Some(false),
            write_scope: Some("workspace".into()),
            ..Default::default()
        }),
        provider_defaults: BTreeMap::from([(
            "pi".into(),
            serde_json::json!({"command": "pi", "command_args": ["--mode", "json"]}),
        )]),
        pi_readiness: PiReadinessConfig::default(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_is_available_without_runtime_manifests() {
        let providers = discover_agent_task_executor_providers();
        assert_eq!(providers.len(), 1);
        let provider = &providers[0];
        assert_eq!(provider.id, "pi.agent-task-executor");
        assert_eq!(provider.backend, "pi");
        assert_eq!(provider.invocation.argv, ["pi"]);
        assert_eq!(provider.pi_readiness, PiReadinessConfig::default());
    }
}
