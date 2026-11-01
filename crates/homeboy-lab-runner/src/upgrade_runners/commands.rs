use crate::Runner;
use crate::RunnerExecOptions;
use homeboy_runner_contract::RunnerCapabilityPreflight;
use homeboy_runner_contract::RunnerRequiredTool;

/// Re-run the verified release install pinned to `target`'s exact tag.
pub(crate) fn runner_binary_refresh_recovery_commands(
    runner_id: &str,
    target: &homeboy_product_identity::BuildIdentity,
) -> Vec<String> {
    vec![format!(
        "homeboy runner refresh-homeboy {} --ref {} --reconnect",
        shell_arg(runner_id),
        shell_arg(&crate::homeboy_refresh::release_ref(&target.version))
    )]
}

pub fn runner_exec_recovery_commands(runner: &Runner, command: &[String]) -> Vec<String> {
    let mut args = vec![
        "homeboy".to_string(),
        "runner".to_string(),
        "exec".to_string(),
        "--ssh".to_string(),
        runner.id.clone(),
        "--".to_string(),
    ];
    args.extend(command.iter().cloned());
    vec![args
        .iter()
        .map(|arg| shell_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")]
}

pub use homeboy_engine_primitives::shell::shell_arg;

pub fn runner_exec_options(runner: &Runner, command: Vec<String>) -> RunnerExecOptions {
    RunnerExecOptions {
        execution_context:
            homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::local("homeboy"),
        cwd: None,
        project_id: None,
        allow_diagnostic_ssh: true,
        diagnostic_ssh_timeout: None,
        command,
        env: runner.env.clone(),
        secret_env_names: Vec::new(),
        secret_env_plan: None,
        env_materialization: None,
        capture_patch: false,
        raw_exec: false,
        source_snapshot: None,
        path_materialization_plan: None,
        capability_preflight: Some(runner_upgrade_capability_plan()),
        required_extensions: Vec::new(),
        extension_env_providers: Vec::new(),
        accepted_extension_settings: Vec::new(),
        require_paths: Vec::new(),
        lab_runner_workload: None,
        run_id: None,
        run_id_owns_generic_exec: false,
        detach_after_handoff: false,
        mirror_evidence: true,
        print_handoff: true,
        read_only_artifact_access: false,
    }
}

pub fn runner_upgrade_capability_plan() -> RunnerCapabilityPreflight {
    RunnerCapabilityPreflight {
        required_toolchain_probes: Vec::new(),
        command: "homeboy upgrade".to_string(),
        required_tools: vec![RunnerRequiredTool::homeboy()],
        required_commands: Vec::new(),
        required_tool_capabilities: Vec::new(),
        required_components: Vec::new(),
        required_env: Vec::new(),
        timeout: None,
    }
}
