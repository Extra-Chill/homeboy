use super::super::helpers::version_is_newer;
use super::super::types::InstallMethod;
use crate::core::runner;
use crate::core::runner::Runner;
use crate::core::runner::RunnerCapabilityPreflight;
use crate::core::runner::RunnerExecOptions;
use crate::core::runner::RunnerRequiredTool;
use crate::core::Error;
use crate::core::Result;

pub fn runner_upgrade_command(
    homeboy_path: &str,
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&str>,
) -> Vec<String> {
    let mut command = vec![
        homeboy_path.to_string(),
        "upgrade".to_string(),
        "--no-restart".to_string(),
        "--skip-extensions".to_string(),
        "--skip-runners".to_string(),
    ];

    if force {
        command.push("--force".to_string());
    }

    if let Some(method) = method_override {
        command.push("--method".to_string());
        command.push(method.as_str().to_string());
    }

    if let Some(path) = source_path {
        command.push("--source-path".to_string());
        command.push(path.to_string());
    }

    command
}

pub fn reconnect_runner_daemon(runner_id: &str) -> Result<String> {
    runner::disconnect(runner_id)?;
    let (report, exit_code) = runner::connect(runner_id)?;
    if exit_code == 0 && report.connected {
        return Ok(format!(
            "connected runner daemon restarted after upgrade; session reports {}",
            report
                .homeboy_version
                .as_deref()
                .unwrap_or("the upgraded version")
        ));
    }

    Err(Error::internal_unexpected(format!(
        "runner reconnect exited with {}; {}",
        exit_code,
        report
            .failure_message
            .as_deref()
            .unwrap_or("runner did not report a connected session")
    )))
}

pub fn runner_recovery_commands(
    runner_id: &str,
    homeboy_path: &str,
    path_drift: Option<&String>,
    configured_version: Option<&str>,
    bare_version: Option<&str>,
) -> Vec<String> {
    let mut commands = runner_upgrade_recovery_commands(runner_id);
    if path_drift.is_some() {
        commands.push(runner_inspect_bare_homeboy_command(runner_id));
    }
    if path_drift.is_some()
        && homeboy_path != "homeboy"
        && matches!((configured_version, bare_version), (Some(configured), Some(bare)) if version_is_newer(bare, configured))
    {
        commands.push(runner_set_homeboy_path_command(runner_id, "homeboy"));
    }
    commands
}

pub fn runner_inspect_bare_homeboy_command(runner_id: &str) -> String {
    let script = "type -a homeboy; command -v homeboy; homeboy --version";
    format!(
        "homeboy runner exec {} --ssh -- sh -lc {}",
        shell_arg(runner_id),
        shell_arg(script)
    )
}

pub fn runner_set_homeboy_path_command(runner_id: &str, homeboy_path: &str) -> String {
    format!(
        "homeboy runner set {} --json {}",
        shell_arg(runner_id),
        shell_arg(&serde_json::json!({ "homeboy_path": homeboy_path }).to_string())
    )
}

pub fn runner_upgrade_recovery_commands(runner_id: &str) -> Vec<String> {
    vec![
        format!(
            "homeboy runner exec {} -- homeboy upgrade --no-restart",
            shell_arg(runner_id)
        ),
        format!(
            "homeboy upgrade --force --upgrade-runner {}",
            shell_arg(runner_id)
        ),
    ]
}

pub fn runner_exec_recovery_commands(runner: &Runner, command: &[String]) -> Vec<String> {
    let mut args = vec![
        "homeboy".to_string(),
        "runner".to_string(),
        "exec".to_string(),
        runner.id.clone(),
        "--ssh".to_string(),
        "--".to_string(),
    ];
    args.extend(command.iter().cloned());
    vec![args
        .iter()
        .map(|arg| shell_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")]
}

pub fn shell_arg(arg: &str) -> String {
    if !arg.is_empty()
        && arg.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':' | b'@' | b'=')
        })
    {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\'', "'\\''"))
}

pub fn runner_exec_options(runner: &Runner, command: Vec<String>) -> RunnerExecOptions {
    RunnerExecOptions {
        cwd: None,
        project_id: None,
        allow_diagnostic_ssh: true,
        command,
        env: runner.env.clone(),
        secret_env_names: Vec::new(),
        capture_patch: false,
        raw_exec: false,
        source_snapshot: None,
        capability_preflight: Some(runner_upgrade_capability_plan()),
        required_extensions: Vec::new(),
        require_paths: Vec::new(),
        runner_workload: None,
        detach_after_handoff: false,
    }
}

pub fn runner_upgrade_capability_plan() -> RunnerCapabilityPreflight {
    RunnerCapabilityPreflight {
        command: "homeboy upgrade".to_string(),
        required_tools: vec![RunnerRequiredTool::Homeboy],
        required_commands: Vec::new(),
        required_components: Vec::new(),
        required_env: Vec::new(),
    }
}
