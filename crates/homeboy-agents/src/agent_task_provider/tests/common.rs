use super::*;

/// Wall-clock budget for provider subprocess tests. Generous on purpose: these
/// tests spawn real `node` processes, and the suite runs at default parallelism
/// on shared machines where a tight window turns a correct terminal status into
/// a spurious `Timeout` (#7739).
pub(super) const PROVIDER_TEST_TIMEOUT_MS: u64 = 60_000;

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
        command: String::new(),
        command_argv: command.split_whitespace().map(str::to_string).collect(),
        invocation: CommandInvocation::default(),
        request_schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        outcome_schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        capabilities: vec!["structured_outcome".to_string()],
        secret_requirements: Vec::new(),
        secret_env_requirements: Vec::new(),
        workspace_materialization: None,
        provider_defaults: BTreeMap::new(),
        runner_readiness: Vec::new(),
        readiness_invocation: None,
        runner_sources: Vec::new(),
        dependency_failure_patterns: Vec::new(),
        config_preflights: Vec::new(),
        lab_runtime_components: Vec::new(),
        timeout_artifact_discovery: AgentTaskProviderTimeoutArtifactDiscovery::default(),
        role_aliases: AgentTaskProviderRoleAliases::default(),
        cli: AgentTaskProviderCliMetadata::default(),
        result_contract: AgentTaskProviderResultContract::default(),
        runtime_contract: AgentTaskRuntimeContract::default(),
        extension_id: None,
        extension_path: None,
        runtime_package_source: None,
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
        // Pin an explicit, generous wall-clock timeout instead of inheriting the
        // process-global default. `provider_default_timeout_returns_structured_
        // outcome_without_explicit_timeout` sets the test default to 50ms through
        // a process-wide env var, so any sibling test that relied on the default
        // observed 50ms whenever the two ran concurrently and reported `Timeout`
        // instead of its real terminal status. Being explicit also makes these
        // subprocess-spawning tests tolerant of CPU contention on a loaded box
        // (#7739). Tests that specifically exercise timeout behaviour override
        // `limits.timeout_ms` themselves.
        limits: AgentTaskLimits {
            timeout_ms: Some(PROVIDER_TEST_TIMEOUT_MS),
            ..AgentTaskLimits::default()
        },
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        output_declarations: Vec::new(),
        metadata: Value::Null,
    };
    (request, provider)
}
