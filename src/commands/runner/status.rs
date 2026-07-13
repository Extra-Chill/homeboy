use std::collections::BTreeMap;

use homeboy::core::agent_runtime_manifest::{
    discover_agent_runtime_catalog, AgentRuntimeDiagnosticFollowup,
    AgentRuntimeDiagnosticsContract, AgentRuntimeExecutableRequirement,
    AgentRuntimeRuntimeDiagnosticDeclaration, AgentRuntimeSourceConsistencyDiagnostic,
    AgentRuntimeToolDiagnosticDeclaration,
};
use homeboy::core::runners::{
    self as runner, RunnerActiveJobState, RunnerAvailability, RunnerBinarySource, RunnerSession,
    RunnerStatusReport, RunnerTunnelMode, RuntimeMaterializationStatus,
};

use super::super::CmdResult;
use super::types::{
    LabFollowup, LabRunnerHomeboyOutput, LabSelectedRunnerOutput, RunnerArtifactFeatureDiagnostics,
    RunnerConnectionOutput, RunnerExecutableRequirementDiagnostics, RunnerExtra,
    RunnerHomeboyBinaryRole, RunnerOperatorCommand, RunnerOutput, RunnerRuntimeDiagnostics,
    RunnerRuntimePackageDiagnostics, RunnerToolDiagnostics, RunnerWorkflowBinaryGuidance,
    RuntimeDiagnostic, RuntimePackageOutput, RuntimeProbeValue, SelectedRuntimeOutput,
};

pub(super) fn status(id: Option<&str>) -> CmdResult<RunnerOutput> {
    let preferred_lab_runner = runner::resolve_default_lab_runner()?;
    if let Some(id) = id {
        let report = runner::status(id)?;
        let operator_hints = runner_status_operator_hints(&report);
        let operator_commands = runner_status_operator_commands(&report);
        let selected_lab_runner = selected_lab_runner_status(Some(id), Some(report.clone()))?;
        return Ok((
            RunnerOutput {
                command: "runner.status".to_string(),
                id: Some(id.to_string()),
                extra: RunnerExtra {
                    connection: Some(RunnerConnectionOutput::Status(report)),
                    preferred_lab_runner,
                    selected_lab_runner,
                    managed_followups: runner_followups(Some(id)),
                    operator_hints,
                    operator_commands,
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ));
    }

    let sessions = runner::statuses()?;
    let operator_hints = sessions
        .iter()
        .flat_map(runner_status_operator_hints)
        .collect();
    let operator_commands = sessions
        .iter()
        .flat_map(runner_status_operator_commands)
        .collect();
    let selected_lab_runner = selected_lab_runner_status(preferred_lab_runner.as_deref(), None)?;
    let managed_followups = runner_followups(preferred_lab_runner.as_deref());
    Ok((
        RunnerOutput {
            command: "runner.status".to_string(),
            extra: RunnerExtra {
                sessions,
                preferred_lab_runner,
                selected_lab_runner,
                managed_followups,
                operator_hints,
                operator_commands,
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn selected_lab_runner_status(
    runner_id: Option<&str>,
    status: Option<RunnerStatusReport>,
) -> homeboy::core::Result<Option<LabSelectedRunnerOutput>> {
    let Some(runner_id) = runner_id else {
        return Ok(None);
    };
    let runner_config = runner::load(runner_id)?;
    let status = match status {
        Some(status) => status,
        None => runner::status(runner_id)?,
    };
    let configured_executable = runner_config
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    let effective_env = runner::effective_env(runner_id)?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let runtime_diagnostics =
        declared_runtime_diagnostics_collection(Some(runner_id), &effective_env);
    let executable_requirements = declared_executable_requirement_diagnostics_collection();
    Ok(Some(LabSelectedRunnerOutput {
        runner_id: runner_id.to_string(),
        kind: format!("{:?}", runner_config.kind).to_ascii_lowercase(),
        configured_executable: configured_executable.clone(),
        runner_homeboy: lab_runner_homeboy_output(runner_id, &configured_executable, &status),
        executable_requirements,
        selected_runtime: runtime_diagnostics
            .first()
            .map(selected_runtime_output_from_generic),
        runtime_diagnostics,
        daemon_enabled: runner_config.settings.daemon,
        workspace_root: runner_config.workspace_root.clone(),
        readiness_state: format!("{:?}", status.state).to_ascii_lowercase(),
        connected: status.connected,
        availability: RunnerAvailability::from_status_parts(
            runner_id,
            status.connected,
            status.stale_daemon.is_some(),
            status.active_job_count,
            &status.active_job_state,
            runner_config.settings.concurrency_limit,
        ),
        status,
    }))
}

pub(super) fn declared_executable_requirement_diagnostics_collection(
) -> Vec<RunnerExecutableRequirementDiagnostics> {
    discover_agent_runtime_catalog()
        .manifests
        .into_iter()
        .flat_map(|manifest| {
            let runtime = manifest.id;
            manifest
                .materialization
                .executable_requirements
                .into_iter()
                .map(move |requirement| {
                    declared_executable_requirement_diagnostics(&runtime, requirement)
                })
        })
        .collect()
}

pub(super) fn declared_executable_requirement_diagnostics(
    runtime: &str,
    requirement: AgentRuntimeExecutableRequirement,
) -> RunnerExecutableRequirementDiagnostics {
    RunnerExecutableRequirementDiagnostics {
        runtime: runtime.to_string(),
        id: requirement.id,
        label: requirement.label,
        env: requirement.env,
        candidates: requirement.candidates,
        version_command: requirement.version_command,
        install_hint: requirement.install_hint,
        diagnostic_state: "declared",
    }
}

pub(super) fn lab_runner_homeboy_output(
    runner_id: &str,
    configured_executable: &str,
    status: &RunnerStatusReport,
) -> LabRunnerHomeboyOutput {
    let materialization =
        RuntimeMaterializationStatus::for_homeboy_runner(runner_id, configured_executable, status);
    let stale_daemon = status.stale_daemon.as_ref();
    let binary_roles: Vec<_> = materialization
        .binary_sources
        .iter()
        .map(runner_homeboy_binary_role)
        .collect();
    let controller_cli = binary_roles[0].clone();
    let active_daemon = binary_roles[1].clone();
    let configured_job_binary = binary_roles[2].clone();
    let has_drift = materialization.has_drift();
    LabRunnerHomeboyOutput {
        controller_version: materialization.controller_version,
        controller_build_identity: materialization.controller_build_identity,
        configured_executable: materialization.configured_executable,
        controller_cli,
        active_daemon,
        configured_job_binary,
        binary_roles,
        workflow_binary_guidance: runner_workflow_binary_guidance(),
        active_daemon_version: materialization.active_daemon_version,
        active_daemon_build_identity: materialization.active_daemon_build_identity,
        job_command_binary_version: materialization.job_command_binary_version,
        job_command_binary_build_identity: materialization.job_command_binary_build_identity,
        stale_daemon_severity: materialization.stale_daemon_severity.map(str::to_string),
        stale_daemon_refresh_command: materialization.stale_daemon_refresh_command,
        stale_daemon: stale_daemon.and_then(|warning| serde_json::to_value(warning).ok()),
        version_drift: materialization.version_drift,
        command_availability_checks: lab_command_availability_checks(configured_executable),
        artifact_features: runner_artifact_feature_diagnostics(
            runner_id,
            configured_executable,
            status,
            has_drift,
        ),
        refresh_commands: lab_runner_homeboy_refresh_commands(runner_id),
        upgrade_command: format!(
            "homeboy upgrade --force --upgrade-runner {}",
            shell_arg(runner_id)
        ),
        dev_sync: runner::load(runner_id)
            .ok()
            .and_then(|runner| runner.resources.get("dev_sync").cloned()),
    }
}

pub(crate) fn declared_tool_diagnostics(
    declaration: &AgentRuntimeToolDiagnosticDeclaration,
    runner_id: Option<&str>,
    env: &BTreeMap<String, String>,
) -> RunnerToolDiagnostics {
    let (configured, configured_binary_source) =
        configured_value(env, &declaration.configured_binary_env);
    let (install_dir, _, _) = install_dir(
        env,
        declaration.install_dir_env.as_deref(),
        declaration.default_install_dir.as_deref(),
    );
    let managed_cache_source =
        render_diagnostic_template(&declaration.managed_cache_source, &install_dir, "");
    let managed_cache_binary = render_diagnostic_template(
        &declaration.managed_cache_binary,
        &install_dir,
        &managed_cache_source,
    );
    RunnerToolDiagnostics {
        tool: declaration.tool.clone(),
        configured_binary: configured,
        configured_binary_source,
        managed_cache_source,
        managed_cache_binary,
        effective_binary_rule: declaration.effective_binary_rule.clone(),
        diagnostic_command: diagnostic_command(runner_id, &declaration.diagnostic_script),
    }
}

pub(crate) fn declared_runtime_diagnostics(
    declaration: &AgentRuntimeRuntimeDiagnosticDeclaration,
    runner_id: Option<&str>,
    env: &BTreeMap<String, String>,
) -> RunnerRuntimeDiagnostics {
    let (configured, configured_binary_source) =
        configured_value(env, &declaration.configured_binary_env);
    let (install_dir, install_dir_source, default_install_dir) = install_dir(
        env,
        declaration.install_dir_env.as_deref(),
        declaration.default_install_dir.as_deref(),
    );
    let managed_cache_source =
        render_diagnostic_template(&declaration.managed_cache_source, &install_dir, "");
    let managed_cache_binary = render_diagnostic_template(
        &declaration.managed_cache_binary,
        &install_dir,
        &managed_cache_source,
    );
    let default_managed_cache_source =
        render_diagnostic_template(&declaration.managed_cache_source, &default_install_dir, "");
    let default_managed_cache_binary = render_diagnostic_template(
        &declaration.managed_cache_binary,
        &default_install_dir,
        &default_managed_cache_source,
    );
    let packages = declared_runtime_packages(
        declaration,
        runner_id,
        env,
        &install_dir,
        &install_dir_source,
        &managed_cache_source,
        &default_install_dir,
        &default_managed_cache_source,
    );
    let mut diagnostics = declared_runtime_source_diagnostics(
        &declaration.source_consistency,
        env,
        configured.as_deref(),
        &install_dir,
        &managed_cache_source,
    );
    diagnostics.extend(configured_binary_override_diagnostics(
        declaration,
        runner_id,
        configured.as_deref(),
        &configured_binary_source,
        &default_managed_cache_binary,
    ));
    diagnostics.extend(package_override_diagnostics(
        declaration,
        runner_id,
        &packages,
    ));

    RunnerRuntimeDiagnostics {
        runtime: declaration.tool.clone(),
        configured_binary: configured,
        configured_binary_source,
        managed_cache_source: managed_cache_source.clone(),
        managed_cache_binary,
        effective_binary_rule: declaration.effective_binary_rule.clone(),
        packages,
        probes: declared_probe_values(declaration),
        runtime_probe_command: diagnostic_command(runner_id, &declaration.runtime_probe_script),
        diagnostics,
    }
}

pub(crate) fn declared_runtime_diagnostics_collection(
    runner_id: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Vec<RunnerRuntimeDiagnostics> {
    declared_diagnostics_contracts()
        .iter()
        .flat_map(|contract| contract.runtimes.iter())
        .map(|declaration| declared_runtime_diagnostics(declaration, runner_id, env))
        .collect()
}

fn selected_runtime_output_from_generic(
    diagnostics: &RunnerRuntimeDiagnostics,
) -> SelectedRuntimeOutput {
    SelectedRuntimeOutput {
        tool: diagnostics.runtime.clone(),
        configured_binary: diagnostics.configured_binary.clone(),
        configured_binary_source: diagnostics.configured_binary_source.clone(),
        managed_cache_source: diagnostics.managed_cache_source.clone(),
        managed_cache_binary: diagnostics.managed_cache_binary.clone(),
        effective_binary_rule: diagnostics.effective_binary_rule.clone(),
        primary_package: diagnostics
            .packages
            .iter()
            .find(|package| package.field == "primary_package")
            .or_else(|| diagnostics.packages.first())
            .map(runtime_package_output_from_generic)
            .unwrap_or_else(empty_package),
        secondary_package: diagnostics
            .packages
            .iter()
            .find(|package| package.field == "secondary_package")
            .or_else(|| diagnostics.packages.get(1))
            .map(runtime_package_output_from_generic)
            .unwrap_or_else(empty_package),
        source_git_sha: diagnostics
            .probes
            .get("source_git_sha")
            .cloned()
            .unwrap_or_else(default_runtime_probe_value),
        dist_build_freshness: diagnostics
            .probes
            .get("dist_build_freshness")
            .cloned()
            .unwrap_or_else(default_runtime_probe_value),
        runtime_probe_command: diagnostics.runtime_probe_command.clone(),
        diagnostics: diagnostics.diagnostics.clone(),
    }
}

fn runtime_package_output_from_generic(
    package: &RunnerRuntimePackageDiagnostics,
) -> RuntimePackageOutput {
    RuntimePackageOutput {
        package: package.package.clone(),
        expected_path: package.expected_path.clone(),
        default_path: package.default_path.clone(),
        selection_source: package.selection_source.clone(),
        env_override: package.env_override.clone(),
        remediation_command: package.remediation_command.clone(),
        resolution: package.resolution.clone(),
    }
}

pub(crate) fn declared_runtime_source_diagnostics(
    declarations: &[AgentRuntimeSourceConsistencyDiagnostic],
    env: &BTreeMap<String, String>,
    configured_binary: Option<&str>,
    install_dir: &str,
    managed_cache_source: &str,
) -> Vec<RuntimeDiagnostic> {
    let mut diagnostics = Vec::new();
    for declaration in declarations {
        let path = match declaration.path.as_str() {
            "configured_binary" => configured_binary.map(str::to_string),
            other => env.get(other).cloned(),
        }
        .unwrap_or_else(|| {
            render_diagnostic_template(&declaration.path, install_dir, managed_cache_source)
        });
        let root = render_diagnostic_template(&declaration.root, install_dir, managed_cache_source);
        if !path.starts_with(&root) {
            diagnostics.push(RuntimeDiagnostic {
                id: declaration.id.clone(),
                severity: declaration.severity.clone(),
                message: render_path_message(&declaration.message, &path, &root),
                remediation: declaration.remediation.clone(),
            });
        }
    }
    diagnostics
}

pub(crate) fn declared_diagnostics_contracts() -> Vec<AgentRuntimeDiagnosticsContract> {
    discover_agent_runtime_catalog()
        .manifests
        .into_iter()
        .map(|manifest| manifest.materialization.diagnostics)
        .filter(|contract| !contract.is_empty())
        .collect()
}

fn configured_value(env: &BTreeMap<String, String>, keys: &[String]) -> (Option<String>, String) {
    for key in keys {
        if let Some(value) = env.get(key) {
            return (Some(value.clone()), key.clone());
        }
    }
    (None, "unset".to_string())
}

fn install_dir(
    env: &BTreeMap<String, String>,
    key: Option<&str>,
    default_value: Option<&str>,
) -> (String, String, String) {
    let default_value = default_value.unwrap_or_default().to_string();
    if let Some((key, value)) = key.and_then(|key| env.get(key).map(|value| (key, value.clone()))) {
        return (value, format!("env:{key}"), default_value);
    }
    (
        default_value.clone(),
        "installed_default".to_string(),
        default_value,
    )
}

fn render_diagnostic_template(
    template: &str,
    install_dir: &str,
    managed_cache_source: &str,
) -> String {
    template
        .replace("${install_dir}", install_dir.trim_end_matches('/'))
        .replace(
            "${managed_cache_source}",
            managed_cache_source.trim_end_matches('/'),
        )
}

fn render_path_message(template: &str, path: &str, root: &str) -> String {
    template.replace("${path}", path).replace("${root}", root)
}

fn declared_runtime_packages(
    declaration: &AgentRuntimeRuntimeDiagnosticDeclaration,
    runner_id: Option<&str>,
    env: &BTreeMap<String, String>,
    install_dir: &str,
    install_dir_source: &str,
    managed_cache_source: &str,
    default_install_dir: &str,
    default_managed_cache_source: &str,
) -> Vec<RunnerRuntimePackageDiagnostics> {
    declaration
        .packages
        .iter()
        .map(|package| {
            let effective_path = render_diagnostic_template(
                &package.expected_path,
                install_dir,
                managed_cache_source,
            );
            let default_path = render_diagnostic_template(
                &package.expected_path,
                default_install_dir,
                default_managed_cache_source,
            );
            let override_value = package
                .env_override
                .as_deref()
                .and_then(|key| env.get(key).map(|value| (key, value.clone())));
            let (expected_path, selection_source, env_override, remediation_command) =
                if let Some((key, value)) = override_value {
                    let remediation_command = if value != default_path {
                        Some(runner_env_update_command(runner_id, key, &default_path))
                    } else {
                        None
                    };
                    (
                        value,
                        format!("env:{key}"),
                        Some(key.to_string()),
                        remediation_command,
                    )
                } else {
                    (effective_path, install_dir_source.to_string(), None, None)
                };
            RunnerRuntimePackageDiagnostics {
                field: package.field.clone(),
                package: package.package.clone(),
                expected_path,
                default_path,
                selection_source,
                env_override,
                remediation_command,
                resolution: RuntimeProbeValue {
                    value: None,
                    source: "runtime_probe_command".to_string(),
                },
            }
        })
        .collect()
}

fn configured_binary_override_diagnostics(
    declaration: &AgentRuntimeRuntimeDiagnosticDeclaration,
    runner_id: Option<&str>,
    configured_binary: Option<&str>,
    configured_binary_source: &str,
    managed_cache_binary: &str,
) -> Vec<RuntimeDiagnostic> {
    let Some(configured_binary) = configured_binary else {
        return Vec::new();
    };
    if configured_binary_source == "unset" || configured_binary == managed_cache_binary {
        return Vec::new();
    }
    vec![RuntimeDiagnostic {
        id: format!("{}.configured_binary_env_override", declaration.tool),
        severity: "warning".to_string(),
        message: format!(
            "Configured runtime binary `{configured_binary}` from `{configured_binary_source}` differs from installed default `{managed_cache_binary}`; runner jobs will use the declaration's effective binary rule before workload execution."
        ),
        remediation: runner_env_update_command(runner_id, configured_binary_source, managed_cache_binary),
    }]
}

fn package_override_diagnostics(
    declaration: &AgentRuntimeRuntimeDiagnosticDeclaration,
    runner_id: Option<&str>,
    packages: &[RunnerRuntimePackageDiagnostics],
) -> Vec<RuntimeDiagnostic> {
    packages
        .iter()
        .filter(|package| package.env_override.is_some() && package.expected_path != package.default_path)
        .map(|package| RuntimeDiagnostic {
            id: format!("{}.{}.env_override", declaration.tool, package.field),
            severity: "warning".to_string(),
            message: format!(
                "Configured runtime package `{}` path `{}` from `{}` differs from installed default `{}`.",
                package.package, package.expected_path, package.selection_source, package.default_path
            ),
            remediation: runner_env_update_command(
                runner_id,
                package.env_override.as_deref().unwrap_or_default(),
                &package.default_path,
            ),
        })
        .collect()
}

fn runner_env_update_command(runner_id: Option<&str>, key: &str, value: &str) -> String {
    let json = serde_json::json!({ "env": { key: value } }).to_string();
    match runner_id {
        Some(runner_id) => format!(
            "homeboy runner set {} --json {}",
            shell_arg(runner_id),
            shell_arg(&json)
        ),
        None => format!("homeboy runner set <runner-id> --json {}", shell_arg(&json)),
    }
}

fn empty_package() -> RuntimePackageOutput {
    RuntimePackageOutput {
        package: String::new(),
        expected_path: String::new(),
        default_path: String::new(),
        selection_source: "runtime_probe_command".to_string(),
        env_override: None,
        remediation_command: None,
        resolution: RuntimeProbeValue {
            value: None,
            source: "runtime_probe_command".to_string(),
        },
    }
}

fn declared_probe_values(
    declaration: &AgentRuntimeRuntimeDiagnosticDeclaration,
) -> BTreeMap<String, RuntimeProbeValue> {
    declaration
        .probes
        .iter()
        .map(|probe| {
            (
                probe.field.clone(),
                RuntimeProbeValue {
                    value: None,
                    source: probe.source.clone(),
                },
            )
        })
        .collect()
}

fn default_runtime_probe_value() -> RuntimeProbeValue {
    RuntimeProbeValue {
        value: None,
        source: "runtime_probe_command".to_string(),
    }
}

fn diagnostic_command(runner_id: Option<&str>, script: &str) -> String {
    match runner_id {
        Some(runner_id) => format!(
            "homeboy runner exec {} --raw -- bash -lc {}",
            shell_arg(runner_id),
            shell_arg(script)
        ),
        None => format!("bash -lc {}", shell_arg(script)),
    }
}

fn lab_command_availability_checks(homeboy_path: &str) -> Vec<String> {
    let binary = shell_arg(homeboy_path);
    vec![
        format!("{binary} --version"),
        format!("{binary} runner exec --help"),
        format!("{binary} runs artifact --help"),
        format!("{binary} tunnel artifact-origin dom-boxes --help"),
        format!("{binary} fuzz --help"),
        format!("{binary} runs evidence --help"),
        format!("{binary} extension list"),
    ]
}

fn runner_homeboy_binary_role(source: &RunnerBinarySource) -> RunnerHomeboyBinaryRole {
    RunnerHomeboyBinaryRole {
        role: source.role,
        owner: source.owner,
        path: source.path.clone(),
        version: source.version.clone(),
        build_identity: source.build_identity.clone(),
        purpose: source.purpose,
    }
}

fn runner_workflow_binary_guidance() -> RunnerWorkflowBinaryGuidance {
    RunnerWorkflowBinaryGuidance {
        recent_workflows: "Recent or already-queued runner workflows may still be owned by the active_daemon session shown here until the runner reconnects.",
        explicit_workflows: "Explicit runner workflow commands and capability checks use configured_job_binary unless the workflow overrides the command binary itself.",
        capability_checks: "Use command_availability_checks or artifact_features.runner_command_checks to verify the configured_job_binary on the runner before assuming controller_cli features are available remotely.",
    }
}

pub(super) fn runner_artifact_feature_diagnostics(
    runner_id: &str,
    homeboy_path: &str,
    status: &RunnerStatusReport,
    version_drift: bool,
) -> RunnerArtifactFeatureDiagnostics {
    let binary = shell_arg(homeboy_path);
    let runner_arg = shell_arg(runner_id);
    let mut hints = Vec::new();
    if version_drift || status.stale_daemon.is_some() {
        hints.push(format!(
            "Runner `{runner_id}` reports Homeboy version/build drift. If artifact commands are missing on runner jobs, restart the active daemon with `homeboy runner disconnect {runner_arg}` then `homeboy runner connect {runner_arg}`."
        ));
    }
    if status.connected
        && status
            .session
            .as_ref()
            .and_then(|session| session.local_url.as_ref())
            .is_none()
    {
        hints.push(format!(
            "Runner `{runner_id}` has no direct daemon URL in the active session; verify artifact command support through managed exec instead of assuming the controller binary matches the runner binary."
        ));
    }

    RunnerArtifactFeatureDiagnostics {
        required_features: vec!["runner_exec_artifact_output", "runs_artifact_attach"],
        controller_commands: vec![
            "homeboy runner exec <runner-id> --run-id <run-id> --artifact <path> -- <command>"
                .to_string(),
            "homeboy runs artifact attach <run-id> --runner <runner-id> --path <path> --name <name>"
                .to_string(),
        ],
        runner_command_checks: vec![
            format!("{binary} runner exec --help"),
            format!("{binary} runs artifact attach --help"),
            format!("homeboy runner exec {runner_arg} -- {binary} runner exec --help"),
            format!("homeboy runner exec {runner_arg} -- {binary} runs artifact attach --help"),
        ],
        hints,
    }
}

fn lab_runner_homeboy_refresh_commands(runner_id: &str) -> Vec<String> {
    let runner_arg = shell_arg(runner_id);
    vec![
        format!(
            "homeboy runner refresh-homeboy {runner_arg} --ref v{} --reconnect",
            env!("CARGO_PKG_VERSION")
        ),
        format!("homeboy runner disconnect {runner_arg}"),
        format!("homeboy runner connect {runner_arg}"),
    ]
}

pub(super) fn runner_followups(runner_id: Option<&str>) -> Vec<LabFollowup> {
    let mut followups = declared_run_followups(None, runner_id);
    let Some(runner_id) = runner_id else {
        return followups;
    };
    let runner_arg = shell_arg(runner_id);
    followups.extend([
        LabFollowup {
            label: "doctor".to_string(),
            command: format!("homeboy runner doctor {runner_arg} --scope lab-offload"),
            purpose: "Probe runner tools, workspace writability, artifact storage, and Lab offload readiness.".to_string(),
        },
        LabFollowup {
            label: "refresh_homeboy".to_string(),
            command: format!(
                "homeboy runner refresh-homeboy {runner_arg} --ref v{} --reconnect",
                env!("CARGO_PKG_VERSION")
            ),
            purpose: "Materialize a clean runner-side Homeboy binary, select it for Lab jobs, and refresh the daemon session.".to_string(),
        },
        LabFollowup {
            label: "env".to_string(),
            command: format!("homeboy runner env {runner_arg}"),
            purpose: "Show the redacted environment Homeboy injects into runner jobs.".to_string(),
        },
        LabFollowup {
            label: "homeboy_binary_refresh".to_string(),
            command: format!(
                "homeboy runner disconnect {runner_arg} && homeboy runner connect {runner_arg}"
            ),
            purpose: "Restart the runner daemon so offload uses the currently configured Homeboy binary.".to_string(),
        },
        LabFollowup {
            label: "homeboy_binary_upgrade".to_string(),
            command: format!("homeboy upgrade --force --upgrade-runner {runner_arg}"),
            purpose: "Upgrade the Homeboy binary configured for this runner before reconnecting stale runs.".to_string(),
        },
        LabFollowup {
            label: "exec".to_string(),
            command: format!("homeboy runner exec {runner_arg} -- <command>"),
            purpose: "Run a managed follow-up command through Homeboy instead of opening an ad-hoc shell.".to_string(),
        },
        LabFollowup {
            label: "workspace_prune_preview".to_string(),
            command: format!("homeboy runner workspace prune {runner_arg}"),
            purpose: "Report safe orphaned Lab workspace counts and bytes before deleting anything.".to_string(),
        },
        LabFollowup {
            label: "workspace_prune_drain".to_string(),
            command: format!("homeboy runner workspace prune {runner_arg} --apply --passes 10"),
            purpose: "Reclaim safe orphaned Lab workspaces in bounded passes when the runner workspace filesystem is under disk pressure.".to_string(),
        },
    ]);
    followups.extend(declared_followups(None, Some(runner_id)));
    if let Ok(path) = std::env::current_dir() {
        followups.push(LabFollowup {
            label: "workspace_sync".to_string(),
            command: format!(
                "homeboy runner workspace sync {runner_arg} --path {} --mode snapshot",
                shell_arg(&path.display().to_string())
            ),
            purpose: "Materialize the current checkout into the runner workspace before a replay or follow-up run.".to_string(),
        });
    }
    followups
}

pub(crate) fn declared_run_followups(
    run_kind: Option<&str>,
    _runner_id: Option<&str>,
) -> Vec<LabFollowup> {
    default_run_followup_declarations()
        .iter()
        .filter(|followup| followup_matches(followup, run_kind))
        .map(declared_run_followup)
        .collect()
}

fn declared_followups(run_kind: Option<&str>, runner_id: Option<&str>) -> Vec<LabFollowup> {
    declared_diagnostics_contracts()
        .iter()
        .flat_map(|contract| contract.followups.iter())
        .filter(|followup| followup_matches(followup, run_kind))
        .map(|followup| declared_followup(followup, runner_id))
        .collect()
}

fn followup_matches(followup: &AgentRuntimeDiagnosticFollowup, run_kind: Option<&str>) -> bool {
    let declared_kind = followup
        .run_kind
        .as_deref()
        .or(followup.workload.as_deref());
    match (run_kind, declared_kind) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(run_kind), Some(declared_kind)) => run_kind == declared_kind,
    }
}

fn default_run_followup_declarations() -> Vec<AgentRuntimeDiagnosticFollowup> {
    vec![
        run_followup(
            "recent_runs",
            None,
            "homeboy runs list --limit 5",
            "Find recent persisted run records before digging into runner state.",
        ),
        run_followup(
            "latest_bench_run",
            Some("bench"),
            "homeboy runs latest-run --kind bench",
            "Resolve the latest benchmark run id for evidence inspection.",
        ),
        run_followup(
            "latest_fuzz_run",
            Some("fuzz"),
            "homeboy runs latest-run --kind fuzz",
            "Resolve the latest fuzz run id for evidence inspection.",
        ),
        run_followup(
            "run_artifacts",
            None,
            "homeboy runs artifacts <run-id>",
            "List recorded run artifacts through Homeboy.",
        ),
        run_followup(
            "run_evidence",
            None,
            "homeboy runs evidence <run-id>",
            "Show stable evidence summary and reviewer-facing commands for one run.",
        ),
        run_followup(
            "run_refs",
            Some("bench"),
            "homeboy runs refs --kind bench --limit 10",
            "List recent benchmark run and artifact refs.",
        ),
        run_followup(
            "fuzz_run_refs",
            Some("fuzz"),
            "homeboy runs refs --kind fuzz --limit 10",
            "List recent fuzz run and artifact refs.",
        ),
    ]
}

fn run_followup(
    label: &str,
    run_kind: Option<&str>,
    command_script: &str,
    purpose: &str,
) -> AgentRuntimeDiagnosticFollowup {
    AgentRuntimeDiagnosticFollowup {
        label: label.to_string(),
        run_kind: run_kind.map(str::to_string),
        workload: None,
        command_script: command_script.to_string(),
        purpose: purpose.to_string(),
    }
}

fn declared_followup(
    declaration: &AgentRuntimeDiagnosticFollowup,
    runner_id: Option<&str>,
) -> LabFollowup {
    LabFollowup {
        label: declaration.label.clone(),
        command: diagnostic_command(runner_id, &declaration.command_script),
        purpose: declaration.purpose.clone(),
    }
}

fn declared_run_followup(declaration: &AgentRuntimeDiagnosticFollowup) -> LabFollowup {
    LabFollowup {
        label: declaration.label.clone(),
        command: declaration.command_script.clone(),
        purpose: declaration.purpose.clone(),
    }
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn runner_status_operator_hints(report: &RunnerStatusReport) -> Vec<String> {
    let Some(session) = report.session.as_ref().filter(|_| report.connected) else {
        return Vec::new();
    };
    let mut hints = Vec::new();
    if report.active_job_state == RunnerActiveJobState::Unavailable {
        let reason = report
            .active_job_error
            .as_ref()
            .map(|err| err.message.as_str())
            .unwrap_or("active-job status endpoint was unavailable");
        hints.push(format!(
            "Active-job status for `{}` is unavailable: {reason}. Treat active_job_count=0 as unknown, not idle.",
            report.runner_id
        ));
    }
    if report.stale_runner_job_count > 0 {
        hints.push(format!(
            "Runner `{}` has {} stale runner job(s) that are no longer active. Inspect each durable run with its listed agent-task status command, then retry only recoverable work.",
            report.runner_id, report.stale_runner_job_count
        ));
    }
    if report.stale_daemon.is_some() {
        hints.extend(
            RuntimeMaterializationStatus::for_homeboy_runner(&report.runner_id, "homeboy", report)
                .stale_daemon_hint(),
        );
    }
    match session.mode {
        RunnerTunnelMode::DirectSsh => {
            if report.active_job_count > 0 {
                hints.push(format!(
                    "Active daemon jobs for `{}` are listed from the direct daemon; inspect with `homeboy runner job logs {} <job-id> --follow` and cancel known jobs with `homeboy runner job cancel {} <job-id>`.",
                    report.runner_id, report.runner_id, report.runner_id
                ));
            }
        }
        RunnerTunnelMode::Reverse => reverse_runner_status_hints(report, session, &mut hints),
    }
    hints
}

fn reverse_runner_status_hints(
    report: &RunnerStatusReport,
    session: &RunnerSession,
    hints: &mut Vec<String>,
) {
    if session.broker_url.is_none() {
        hints.push(format!(
            "Reverse runner `{}` has no broker URL; active-job listing, logs, and cancel require reconnecting with `homeboy runner connect <controller-id> --reverse --reverse-runner {} --broker-url <url>`.",
            report.runner_id, report.runner_id
        ));
        return;
    }
    hints.push(format!(
        "Reverse runner `{}` active jobs are listed through the broker; inspect with `homeboy runner job logs {} <job-id> --follow`.",
        report.runner_id, report.runner_id
    ));
    if report.active_job_count > 0 {
        hints.push(format!(
            "Cancel known reverse broker jobs with `homeboy runner job cancel {} <job-id>`; if a claim lease expires, reconcile broker state with `homeboy runner job reconcile {}` instead of mutating the job store manually.",
            report.runner_id, report.runner_id
        ));
    }
}

pub(super) fn runner_status_operator_commands(
    report: &RunnerStatusReport,
) -> Vec<RunnerOperatorCommand> {
    let Some(session) = report.session.as_ref().filter(|_| report.connected) else {
        return Vec::new();
    };

    let mut commands = Vec::new();
    for job in report
        .active_runner_jobs
        .iter()
        .chain(report.stale_runner_jobs.iter())
    {
        if job.lifecycle_state.as_deref() == Some("recoverable_orphan") {
            if let Some(run_id) = job.durable_run_id.as_deref() {
                commands.push(RunnerOperatorCommand {
                    scope: "agent_task_status",
                    runner_id: report.runner_id.clone(),
                    job_id: None,
                    command: format!("homeboy agent-task status {run_id} --full"),
                    description:
                        "Inspect the durable orphaned agent-task run and its preserved evidence."
                            .to_string(),
                });
                commands.push(RunnerOperatorCommand {
                    scope: "agent_task_retry",
                    runner_id: report.runner_id.clone(),
                    job_id: None,
                    command: format!("homeboy agent-task retry {run_id}"),
                    description: "Create a fresh durable attempt after reviewing the orphaned run."
                        .to_string(),
                });
            }
            continue;
        }
        commands.push(RunnerOperatorCommand {
            scope: "job_logs",
            runner_id: report.runner_id.clone(),
            job_id: Some(job.job_id.clone()),
            command: format!(
                "homeboy runner job logs {} {} --follow",
                report.runner_id, job.job_id
            ),
            description: "Follow the active runner job event stream.".to_string(),
        });
        if matches!(job.lifecycle_state.as_deref(), None | Some("active")) {
            commands.push(RunnerOperatorCommand {
                scope: "job_cancel",
                runner_id: report.runner_id.clone(),
                job_id: Some(job.job_id.clone()),
                command: format!(
                    "homeboy runner job cancel {} {}",
                    report.runner_id, job.job_id
                ),
                description: "Request cancellation for a queued or running runner job.".to_string(),
            });
        }
        if let Some(run_id) = job.durable_run_id.as_deref() {
            commands.push(RunnerOperatorCommand {
                scope: "artifact_get",
                runner_id: report.runner_id.clone(),
                job_id: Some(job.job_id.clone()),
                command: format!("homeboy runs artifact get {run_id} <artifact-id> -o <path>"),
                description: "Fetch a mirrored observation artifact after the run records one."
                    .to_string(),
            });
        }
    }

    if session.mode == RunnerTunnelMode::Reverse {
        if session.broker_url.is_some() {
            commands.push(RunnerOperatorCommand {
                scope: "broker_reconcile",
                runner_id: report.runner_id.clone(),
                job_id: None,
                command: format!(
                    "homeboy runner job reconcile {}",
                    shell_arg(&report.runner_id)
                ),
                description:
                    "Fail expired reverse-runner claims through the broker-owned lifecycle path."
                        .to_string(),
            });
            for job in &report.active_runner_jobs {
                commands.push(RunnerOperatorCommand {
                    scope: "broker_artifact_lookup",
                    runner_id: report.runner_id.clone(),
                    job_id: Some(job.job_id.clone()),
                    command: format!(
                        "homeboy runner job artifacts {} {} <artifact-id>",
                        shell_arg(&report.runner_id),
                        shell_arg(&job.job_id)
                    ),
                    description: "Inspect broker-held reverse-runner artifact metadata."
                        .to_string(),
                });
            }
        }
    }

    if let Some(warning) = report.stale_daemon.as_ref() {
        commands.push(RunnerOperatorCommand {
            scope: "daemon_refresh",
            runner_id: report.runner_id.clone(),
            job_id: None,
            command: warning.refresh_command.clone(),
            description: "Restart the active runner daemon so the control plane uses the configured job command binary.".to_string(),
        });
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::runner::{
        RunnerActiveJobState, RunnerSessionRole, RunnerStaleDaemonWarning,
    };
    use homeboy::core::runners::{RunnerSessionState, RunnerTunnelMode};

    #[test]
    fn stale_daemon_status_hint_labels_control_plane_and_job_binary() {
        let report = stale_daemon_report();

        let hints = runner_status_operator_hints(&report);
        let commands = runner_status_operator_commands(&report);

        let hint = hints
            .iter()
            .find(|hint| hint.contains("stale daemon"))
            .expect("stale daemon hint");
        assert!(hint.contains("severity=warning"));
        assert!(hint.contains("active daemon control plane"));
        assert!(hint.contains("homeboy 0.259.0+daemon"));
        assert!(hint.contains("job command binary"));
        assert!(hint.contains("homeboy 0.262.0+binary"));
        let job_binary_refresh = format!(
            "homeboy runner refresh-homeboy homeboy-lab --ref v{} --reconnect",
            env!("CARGO_PKG_VERSION")
        );
        assert!(hint.contains(&job_binary_refresh));
        assert!(hint.contains(
            "homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab"
        ));

        let refresh = commands
            .iter()
            .find(|command| command.scope == "daemon_refresh")
            .expect("daemon refresh command");
        assert!(refresh.command.starts_with(&job_binary_refresh));
        assert!(refresh.command.ends_with(
            "homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab"
        ));
        assert!(refresh
            .description
            .contains("configured job command binary"));
    }

    fn stale_daemon_report() -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(RunnerSession {
                runner_id: "homeboy-lab".to_string(),
                mode: RunnerTunnelMode::DirectSsh,
                role: RunnerSessionRole::Controller,
                server_id: Some("lab-server".to_string()),
                controller_id: None,
                broker_url: None,
                remote_daemon_address: Some("127.0.0.1:7331".to_string()),
                local_port: Some(7331),
                local_url: Some("http://127.0.0.1:7331".to_string()),
                tunnel_pid: Some(12345),
                remote_daemon_pid: Some(23456),
                remote_daemon_lease_id: Some("lease-23456".to_string()),
                homeboy_version: "homeboy 0.259.0".to_string(),
                homeboy_build_identity: Some("homeboy 0.259.0+daemon".to_string()),
                connected_at: "2026-06-26T00:00:00Z".to_string(),
                worker_identity: None,
                worker_pid: None,
                last_seen_at: None,
            }),
            stale_daemon: Some(RunnerStaleDaemonWarning::new(
                "homeboy-lab",
                "homeboy 0.259.0".to_string(),
                "homeboy 0.262.0".to_string(),
                Some("homeboy 0.259.0+daemon".to_string()),
                Some("homeboy 0.262.0+binary".to_string()),
            )),
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            session_path: "/tmp/homeboy-lab.json".to_string(),
        }
    }
}
