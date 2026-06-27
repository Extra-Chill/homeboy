use std::collections::BTreeMap;

use homeboy::core::agent_runtime_manifest::{
    discover_agent_runtime_catalog, AgentRuntimeDiagnosticFollowup,
    AgentRuntimeDiagnosticsContract, AgentRuntimeExecutableRequirement,
    AgentRuntimeRuntimeDiagnosticDeclaration, AgentRuntimeSourceConsistencyDiagnostic,
    AgentRuntimeToolDiagnosticDeclaration,
};
use homeboy::core::runners::{
    self as runner, RunnerActiveJobState, RunnerAvailability, RunnerSession, RunnerStatusReport,
    RunnerTunnelMode,
};

use super::super::CmdResult;
use super::types::{
    LabFollowup, LabRunnerHomeboyOutput, LabSelectedRunnerOutput, RunnerArtifactFeatureDiagnostics,
    RunnerConnectionOutput, RunnerExecutableRequirementDiagnostics, RunnerExtra,
    RunnerHomeboyBinaryRole, RunnerOperatorCommand, RunnerOutput, RunnerRuntimeDiagnostics,
    RunnerRuntimePackageDiagnostics, RunnerToolDiagnostics, RunnerWorkflowBinaryGuidance,
    WpCodeboxPackageRuntimeOutput, WpCodeboxProbeValue, WpCodeboxRuntimeDiagnostic,
    WpCodeboxRuntimeOutput,
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
        wp_codebox_runtime: runtime_diagnostics
            .iter()
            .find(|diagnostics| diagnostics.legacy_output.as_deref() == Some("wp_codebox_runtime"))
            .map(wp_codebox_runtime_output_from_generic),
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
    let controller_version = env!("CARGO_PKG_VERSION").to_string();
    let controller_build_identity = homeboy::core::build_identity::current().display;
    let active_daemon_version = status
        .session
        .as_ref()
        .map(|session| session.homeboy_version.clone());
    let version_drift = active_daemon_version
        .as_ref()
        .is_some_and(|version| version != &controller_version);
    let binary_roles = runner_homeboy_binary_roles(
        configured_executable,
        &status.session,
        &active_daemon_version,
    );
    let controller_cli = binary_roles[0].clone();
    let active_daemon = binary_roles[1].clone();
    let configured_job_binary = binary_roles[2].clone();
    LabRunnerHomeboyOutput {
        controller_version,
        controller_build_identity,
        configured_executable: configured_executable.to_string(),
        controller_cli,
        active_daemon,
        configured_job_binary,
        binary_roles,
        workflow_binary_guidance: runner_workflow_binary_guidance(),
        active_daemon_version,
        active_daemon_build_identity: status
            .session
            .as_ref()
            .and_then(|session| session.homeboy_build_identity.clone()),
        stale_daemon: status
            .stale_daemon
            .as_ref()
            .and_then(|warning| serde_json::to_value(warning).ok()),
        version_drift,
        command_availability_checks: lab_command_availability_checks(configured_executable),
        artifact_features: runner_artifact_feature_diagnostics(
            runner_id,
            configured_executable,
            status,
            version_drift,
        ),
        refresh_commands: lab_runner_homeboy_refresh_commands(runner_id),
        upgrade_command: format!(
            "homeboy upgrade --force --upgrade-runner {}",
            shell_arg(runner_id)
        ),
    }
}

pub(crate) fn declared_tool_diagnostics_for_legacy(
    legacy_output: &str,
    runner_id: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Option<RunnerToolDiagnostics> {
    declared_diagnostics_contracts()
        .iter()
        .flat_map(|contract| contract.tools.iter())
        .find(|declaration| declaration.legacy_output.as_deref() == Some(legacy_output))
        .map(|declaration| declared_tool_diagnostics(declaration, runner_id, env))
}

pub(crate) fn declared_runtime_diagnostics_for_legacy(
    legacy_output: &str,
    runner_id: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Option<WpCodeboxRuntimeOutput> {
    declared_diagnostics_contracts()
        .iter()
        .flat_map(|contract| contract.runtimes.iter())
        .find(|declaration| declaration.legacy_output.as_deref() == Some(legacy_output))
        .map(|declaration| declared_runtime_diagnostics(declaration, runner_id, env))
        .map(|diagnostics| wp_codebox_runtime_output_from_generic(&diagnostics))
}

pub(crate) fn declared_tool_diagnostics(
    declaration: &AgentRuntimeToolDiagnosticDeclaration,
    runner_id: Option<&str>,
    env: &BTreeMap<String, String>,
) -> RunnerToolDiagnostics {
    let (configured, configured_binary_source) =
        configured_value(env, &declaration.configured_binary_env);
    let install_dir = install_dir(
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
    let install_dir = install_dir(
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
    let packages = declared_runtime_packages(declaration, env, &install_dir, &managed_cache_source);
    let diagnostics = declared_runtime_source_diagnostics(
        &declaration.source_consistency,
        env,
        configured.as_deref(),
        &install_dir,
        &managed_cache_source,
    );

    RunnerRuntimeDiagnostics {
        runtime: declaration.tool.clone(),
        legacy_output: declaration.legacy_output.clone(),
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

fn wp_codebox_runtime_output_from_generic(
    diagnostics: &RunnerRuntimeDiagnostics,
) -> WpCodeboxRuntimeOutput {
    WpCodeboxRuntimeOutput {
        tool: diagnostics.runtime.clone(),
        configured_binary: diagnostics.configured_binary.clone(),
        configured_binary_source: diagnostics.configured_binary_source.clone(),
        managed_cache_source: diagnostics.managed_cache_source.clone(),
        managed_cache_binary: diagnostics.managed_cache_binary.clone(),
        effective_binary_rule: diagnostics.effective_binary_rule.clone(),
        playground_package: diagnostics
            .packages
            .iter()
            .find(|package| package.field == "playground_package")
            .or_else(|| diagnostics.packages.first())
            .map(wp_codebox_package_output_from_generic)
            .unwrap_or_else(empty_package),
        core_package: diagnostics
            .packages
            .iter()
            .find(|package| package.field == "core_package")
            .or_else(|| diagnostics.packages.get(1))
            .map(wp_codebox_package_output_from_generic)
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

fn wp_codebox_package_output_from_generic(
    package: &RunnerRuntimePackageDiagnostics,
) -> WpCodeboxPackageRuntimeOutput {
    WpCodeboxPackageRuntimeOutput {
        package: package.package.clone(),
        expected_path: package.expected_path.clone(),
        resolution: package.resolution.clone(),
    }
}

pub(crate) fn declared_runtime_source_diagnostics(
    declarations: &[AgentRuntimeSourceConsistencyDiagnostic],
    env: &BTreeMap<String, String>,
    configured_binary: Option<&str>,
    install_dir: &str,
    managed_cache_source: &str,
) -> Vec<WpCodeboxRuntimeDiagnostic> {
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
            diagnostics.push(WpCodeboxRuntimeDiagnostic {
                id: declaration.id.clone(),
                severity: declaration.severity.clone(),
                message: render_path_message(&declaration.message, &path, &root),
                remediation: declaration.remediation.clone(),
            });
        }
    }
    diagnostics
}

fn declared_diagnostics_contracts() -> Vec<AgentRuntimeDiagnosticsContract> {
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
) -> String {
    key.and_then(|key| env.get(key).cloned())
        .or_else(|| default_value.map(str::to_string))
        .unwrap_or_default()
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
    env: &BTreeMap<String, String>,
    install_dir: &str,
    managed_cache_source: &str,
) -> Vec<RunnerRuntimePackageDiagnostics> {
    declaration
        .packages
        .iter()
        .map(|package| RunnerRuntimePackageDiagnostics {
            field: package.field.clone(),
            package: package.package.clone(),
            expected_path: package
                .env_override
                .as_deref()
                .and_then(|key| env.get(key).cloned())
                .unwrap_or_else(|| {
                    render_diagnostic_template(
                        &package.expected_path,
                        install_dir,
                        managed_cache_source,
                    )
                }),
            resolution: WpCodeboxProbeValue {
                value: None,
                source: "runtime_probe_command".to_string(),
            },
        })
        .collect()
}

fn empty_package() -> WpCodeboxPackageRuntimeOutput {
    WpCodeboxPackageRuntimeOutput {
        package: String::new(),
        expected_path: String::new(),
        resolution: WpCodeboxProbeValue {
            value: None,
            source: "runtime_probe_command".to_string(),
        },
    }
}

fn declared_probe_values(
    declaration: &AgentRuntimeRuntimeDiagnosticDeclaration,
) -> BTreeMap<String, WpCodeboxProbeValue> {
    declaration
        .probes
        .iter()
        .map(|probe| {
            (
                probe.field.clone(),
                WpCodeboxProbeValue {
                    value: None,
                    source: probe.source.clone(),
                },
            )
        })
        .collect()
}

fn default_runtime_probe_value() -> WpCodeboxProbeValue {
    WpCodeboxProbeValue {
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

fn runner_homeboy_binary_roles(
    configured_executable: &str,
    session: &Option<RunnerSession>,
    active_daemon_version: &Option<String>,
) -> Vec<RunnerHomeboyBinaryRole> {
    let controller_identity = homeboy::core::build_identity::current();
    vec![
        RunnerHomeboyBinaryRole {
            role: "controller_cli",
            owner: "operator_command",
            path: std::env::current_exe()
                .ok()
                .map(|path| path.display().to_string()),
            version: Some(controller_identity.version),
            build_identity: Some(controller_identity.display),
            purpose: "Renders this status output and submits runner jobs; it does not prove what the runner daemon or job command binary supports.",
        },
        RunnerHomeboyBinaryRole {
            role: "active_daemon",
            owner: "runner_session",
            path: session
                .as_ref()
                .and_then(|session| session.remote_daemon_address.clone()),
            version: active_daemon_version.clone(),
            build_identity: session
                .as_ref()
                .and_then(|session| session.homeboy_build_identity.clone()),
            purpose: "Accepts connected daemon jobs until the runner is disconnected/reconnected; it can lag behind the configured job binary after refresh-homeboy.",
        },
        RunnerHomeboyBinaryRole {
            role: "configured_job_binary",
            owner: "runner_config.settings.homeboy_path",
            path: Some(configured_executable.to_string()),
            version: None,
            build_identity: None,
            purpose: "Binary path selected for runner-side Homeboy subcommands and capability checks; use command_availability_checks to verify required subcommands on the runner.",
        },
    ]
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
        format!("homeboy runner refresh-homeboy {runner_arg} --ref main --reconnect"),
        format!("homeboy runner disconnect {runner_arg}"),
        format!("homeboy runner connect {runner_arg}"),
    ]
}

pub(super) fn runner_followups(runner_id: Option<&str>) -> Vec<LabFollowup> {
    let mut followups = declared_run_followups_for_legacy("managed_followups", None, runner_id);
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
            command: format!("homeboy runner refresh-homeboy {runner_arg} --ref main --reconnect"),
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
    ]);
    followups.extend(declared_followups_for_legacy(
        "managed_followups",
        None,
        Some(runner_id),
    ));
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

pub(crate) fn declared_run_followups_for_legacy(
    legacy_output: &str,
    run_kind: Option<&str>,
    _runner_id: Option<&str>,
) -> Vec<LabFollowup> {
    default_run_followup_declarations()
        .iter()
        .filter(|followup| followup_matches(followup, legacy_output, run_kind))
        .map(declared_run_followup)
        .collect()
}

fn declared_followups_for_legacy(
    legacy_output: &str,
    run_kind: Option<&str>,
    runner_id: Option<&str>,
) -> Vec<LabFollowup> {
    declared_diagnostics_contracts()
        .iter()
        .flat_map(|contract| contract.followups.iter())
        .filter(|followup| followup_matches(followup, legacy_output, run_kind))
        .map(|followup| declared_followup(followup, runner_id))
        .collect()
}

fn followup_matches(
    followup: &AgentRuntimeDiagnosticFollowup,
    legacy_output: &str,
    run_kind: Option<&str>,
) -> bool {
    if followup.legacy_output.as_deref() != Some(legacy_output) {
        return false;
    }
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
        legacy_output: Some("managed_followups".to_string()),
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
            "Runner `{}` has {} stale runner job(s) that are no longer active. Inspect stale_runner_jobs before retrying affected durable runs.",
            report.runner_id, report.stale_runner_job_count
        ));
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

    commands
}
