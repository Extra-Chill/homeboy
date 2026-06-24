use super::*;

pub(super) fn script(body: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "homeboy-agent-task-provider-{}-{}.js",
        std::process::id(),
        body.len()
    ));
    fs::write(&path, body).expect("script written");
    path.to_string_lossy().to_string()
}

pub(super) fn request(
    task_id: &str,
    command: String,
) -> (AgentTaskRequest, AgentTaskExecutorProvider) {
    let provider = AgentTaskExecutorProvider {
        schema: AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA.to_string(),
        id: "test.provider".to_string(),
        label: None,
        backend: "test".to_string(),
        default_backend: false,
        command,
        command_argv: Vec::new(),
        invocation: CommandInvocation::default(),
        request_schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        capabilities: vec!["structured_outcome".to_string()],
        secret_requirements: Vec::new(),
        secret_env_requirements: Vec::new(),
        workspace_materialization: None,
        provider_defaults: BTreeMap::new(),
        runner_readiness: Vec::new(),
        runner_sources: Vec::new(),
        dependency_failure_patterns: Vec::new(),
        lab_runtime_components: Vec::new(),
        timeout_artifact_discovery: AgentTaskProviderTimeoutArtifactDiscovery::default(),
        role_aliases: AgentTaskProviderRoleAliases::default(),
        runtime_contract: AgentTaskRuntimeContract::default(),
        extension_id: None,
        extension_path: None,
        runtime_id: None,
        runtime_path: None,
        extra: BTreeMap::new(),
    };
    let request = AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: task_id.to_string(),
        group_key: None,
        parent_plan_id: None,
        executor: AgentTaskExecutor {
            backend: "test".to_string(),
            selector: None,
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: None,
            config: Value::Null,
        },
        instructions: "run".to_string(),
        inputs: Value::Null,
        source_refs: Vec::new(),
        workspace: AgentTaskWorkspace::default(),
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        metadata: Value::Null,
    };
    (request, provider)
}
