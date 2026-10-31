use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static SCRIPT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Wall-clock budget for provider subprocess tests. Generous on purpose: these
/// tests spawn real `node` processes, and the suite runs at default parallelism
/// on shared machines where a tight window turns a correct terminal status into
/// a spurious `Timeout` (#7739).
pub(super) const PROVIDER_TEST_TIMEOUT_MS: u64 = 60_000;

/// Isolated home for a test that executes a provider. Execution materializes
/// its artifact root below the ambient home; without a home of its own the test
/// writes into whichever home a parallel test isolates, and loses the root when
/// that test exits (#14832).
pub(super) fn artifact_home() -> homeboy_core::test_support::HomeGuard {
    homeboy_core::test_support::HomeGuard::new()
}

pub(super) fn script(body: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "homeboy-agent-task-provider-{}-{}.js",
        std::process::id(),
        SCRIPT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&path, format!("#!/usr/bin/env node\nif(process.argv.includes('--version')) {{ console.log('test-pi'); process.exit(0); }}\nif(process.argv.includes('--list-models')) {{ console.log('test model'); process.exit(0); }}\n{body}")).expect("script written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("executable fake Pi");
    }
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
        immediate_failure_patterns: Vec::new(),
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
    let mut command_parts = command.split_whitespace();
    let mut program = command_parts.next().unwrap_or("pi");
    if program == "node" {
        program = command_parts.next().expect("fake Pi script");
    }
    let args: Vec<_> = command_parts.collect();
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
            config: json!({"command": program, "command_args": args}),
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
        runtime_tools: Vec::new(),
        metadata: Value::Null,
    };
    (request, provider)
}
