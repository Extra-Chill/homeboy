use super::*;
use types::*;

pub fn report(
    runner_id: &str,
    runner: &Runner,
    server: &Server,
    client: &SshClient,
    options: &RunnerDoctorOptions,
) -> RunnerDoctorOutput {
    let scoped = options.scope == RunnerDoctorScope::LabOffload;
    let (per_probe, overall) = probe_limits(options.scope);
    let _probe_limits = client.scoped_probe_limits(per_probe, overall, "runner doctor");
    let ssh_execution = client.execute("printf ok");
    if !ssh_execution.success || ssh_execution.stdout.trim() != "ok" {
        return unreachable_report(runner_id, runner, server, ssh_execution);
    }
    let mut ssh_details = common::detail_map(&[]);
    if homeboy::core::server::client::used_clean_ssh_session(&ssh_execution) {
        ssh_details.insert(
            "transport".to_string(),
            "clean_session_fallback".to_string(),
        );
    }

    // Lab admission uses the live status projection, including the daemon's
    // typed freshness report. Doctor repair must make its decision from that
    // same observation rather than treating a reachable endpoint as ready.
    let persisted_status = if options.scope == RunnerDoctorScope::LabOffload {
        runner::status(runner_id).ok()
    } else {
        runner::diagnostic_status(runner_id).ok()
    };
    if options.scope == RunnerDoctorScope::LabOffload {
        match persisted_status.as_ref() {
            Some(status) if !status.connected => {
                return disconnected_report(
                    runner_id,
                    runner,
                    server,
                    status.daemon_freshness.clone(),
                    admission_summary(status),
                );
            }
            None => {
                return disconnected_report(runner_id, runner, server, None, None);
            }
            Some(_) => {}
        }
    }
    let workspace_root = runner
        .workspace_root
        .clone()
        .unwrap_or_else(|| ".".to_string());
    let artifact_root = if scoped {
        "~/.local/share/homeboy/artifacts".to_string()
    } else {
        default_artifact_root(client)
    };
    let mut checks = Vec::new();
    let mut tools = BTreeMap::new();

    checks.push(checks::ok_with_details(
        "ssh.execution",
        format!("SSH runner {} is reachable", runner_id),
        ssh_details,
    ));

    let homeboy_command = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let local_homeboy_identity = homeboy_product_identity::build_identity();
    let local_homeboy_version = local_homeboy_identity.version.as_str();
    let homeboy = HomeboyProbe {
        version: common::remote_line(
            client,
            &format!(
                "{} --version | awk '{{print $2}}'",
                common::shell_word(homeboy_command)
            ),
        )
        .unwrap_or_else(|| "unknown".to_string()),
        path: runner
            .settings
            .homeboy_path
            .clone()
            .or_else(|| common::remote_line(client, "command -v homeboy")),
    };
    if let Some(check) = checks::homeboy_version_skew_check(
        local_homeboy_version,
        &local_homeboy_identity.display,
        &homeboy.version,
        runner_id,
        &server.id,
    ) {
        checks.push(check);
    }
    checks.push(if homeboy.path.is_some() {
        checks::ok(
            "homeboy",
            "Homeboy is available on the remote runner".to_string(),
            None,
        )
    } else {
        checks::warning(
            "homeboy",
            "Homeboy was not found on the remote runner PATH".to_string(),
            Some("Install Homeboy on the remote runner or configure runner.homeboy_path/server.env.PATH".to_string()),
        )
    });

    let system = if scoped {
        SystemProbe::default()
    } else {
        SystemProbe {
            os: common::remote_line(client, "uname -s").unwrap_or_else(|| "unknown".to_string()),
            arch: common::remote_line(client, "uname -m").unwrap_or_else(|| "unknown".to_string()),
            kernel: common::remote_line(client, "uname -r"),
        }
    };
    if !scoped {
        checks.push(checks::ok(
            "system",
            format!("{} {} runner detected", system.os, system.arch),
            None,
        ));
    }

    let cpu = if scoped {
        CpuProbe::default()
    } else {
        CpuProbe {
        count: common::remote_line(client, "getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1),
    }
    };
    if !scoped {
        checks.push(checks::ok(
            "cpu",
            format!("{} CPU cores detected", cpu.count),
            None,
        ));
    }

    let memory = (!scoped)
        .then(|| probes::remote_memory_probe(client))
        .flatten();
    if !scoped {
        checks.push(match &memory {
            Some(memory) => checks::ok(
                "memory",
                format!("{} MB RAM detected", memory.total_mb),
                None,
            ),
            None => checks::warning(
                "memory",
                "RAM totals could not be detected".to_string(),
                Some(
                    "Ensure /proc/meminfo or sysctl is available on the remote runner".to_string(),
                ),
            ),
        });
    }

    let disk = (!scoped)
        .then(|| probes::remote_disk_probe(client, &workspace_root))
        .flatten();
    if !scoped {
        checks.push(match &disk {
            Some(disk) => checks::ok(
                "disk.workspace_root",
                format!("{} MB available at workspace root", disk.available_mb),
                None,
            ),
            None => checks::warning(
                "disk.workspace_root",
                "Workspace disk capacity could not be detected".to_string(),
                Some("Ensure df is available on the remote runner".to_string()),
            ),
        });
    }

    if !scoped {
        for spec in probes::tool_specs(runner) {
            if spec.id == "homeboy" {
                continue;
            }
            let probe = probes::remote_tool_probe(client, &spec.command, &spec.version_args);
            checks.push(checks::tool_check(spec.clone(), &probe));
            tools.insert(spec.id.to_string(), probe);
        }
    }

    for command in normalized_required_tools(&options.required_tools) {
        let version_args = probes::required_tool_version_args(&command);
        let probe = probes::remote_tool_probe(client, &command, &version_args);
        checks.push(checks::required_tool_check(&command, &probe));
        tools.entry(command).or_insert(probe);
    }

    let mut declared_tools = BTreeMap::new();
    if !scoped {
        for (source, specs) in probes::declared_tool_specs_by_source() {
            let mut source_tools = BTreeMap::new();
            for spec in specs {
                let probe = probes::remote_tool_probe(client, &spec.command, &spec.version_args);
                source_tools.insert(spec.id.clone(), probe.clone());
                tools.entry(spec.id.clone()).or_insert(probe);
            }
            declared_tools.insert(source, source_tools);
        }
    }

    let playwright = probes::tool_available(&tools, "playwright");
    let browser_ready = (!scoped)
        .then(|| probes::remote_browser_ready(client))
        .unwrap_or(false);
    let display_ready = (!scoped)
        .then(|| probes::remote_display_ready(client))
        .unwrap_or(false);
    let xvfb_ready = (!scoped)
        .then(|| probes::remote_xvfb_ready(client))
        .unwrap_or(false);
    let headed_browser_ready = probes::headed_browser_ready(display_ready, xvfb_ready);
    if !scoped {
        checks.push(checks::playwright_check(playwright, browser_ready));
        checks.push(checks::headed_browser_check(
            headed_browser_ready,
            display_ready,
            xvfb_ready,
        ));
    }

    let workspace_writable = (!scoped)
        .then(|| probes::remote_path_writable(client, &workspace_root))
        .unwrap_or(false);
    if !scoped {
        checks.push(checks::path_writable_check(
            "workspace.writable",
            workspace_writable,
            Path::new(&workspace_root),
            "Make the remote workspace root writable by the runner user",
        ));
    }

    let artifact_store_available = (!scoped)
        .then(|| probes::remote_artifact_store_available(client, &artifact_root))
        .unwrap_or(false);
    if !scoped {
        checks.push(checks::path_writable_check(
            "artifact_store.available",
            artifact_store_available,
            Path::new(&artifact_root),
            "Create the artifact root or configure HOMEBOY_ARTIFACT_ROOT to a writable directory",
        ));
    }

    if options.scope == RunnerDoctorScope::LabOffload {
        checks.extend(probes::lab_homeboy_path_checks(
            client,
            runner_id,
            &server.id,
            homeboy_command,
            local_homeboy_version,
            &homeboy,
        ));
        let catalog = homeboy::agents::agent_tasks::provider::AgentTaskProviderCatalog::discover();
        checks.extend(probes::remote_provider_executor_resolution_checks(
            client,
            catalog.providers(),
            options.agent_backend.as_deref(),
            options.agent_selector.as_deref(),
        ));
        checks.extend(probes::provider_readiness_checks(
            client,
            &homeboy::agents::agent_tasks::provider::provider_runner_readiness_contracts(),
        ));
        checks.extend(probes::managed_runner_source_checks(
            client,
            &homeboy::agents::agent_tasks::provider::provider_runner_source_contracts(),
        ));
    }

    let daemon_timeout = client
        .remaining_probe_budget()
        .unwrap_or(std::time::Duration::from_secs(5))
        .min(std::time::Duration::from_secs(5));
    let daemon_checks = persisted_status
        .as_ref()
        .and_then(|status| status.session.as_ref())
        .map(|session| {
            probes::connected_daemon_health_checks_with_timeout(runner_id, session, daemon_timeout)
        })
        .unwrap_or_default();
    let daemon_timed_out = daemon_checks.iter().any(|check| {
        matches!(
            check.details.get("reason_code").map(String::as_str),
            Some("runner_doctor.daemon_timeout" | "runner_doctor.overall_timeout")
        )
    });
    checks.extend(daemon_checks);

    for extension_id in normalized_extension_ids(&options.extensions) {
        checks.push(extension_parity::remote_check(
            client,
            homeboy_command,
            options.path.as_deref(),
            &extension_id,
        ));
    }

    let capabilities = probes::capabilities_from(
        false,
        true,
        homeboy.path.is_some(),
        workspace_writable,
        artifact_store_available,
    );
    let resources = RunnerResources {
        homeboy,
        system,
        cpu,
        memory,
        disk,
        workspace_root: workspace_root.clone(),
        artifact_root,
        tools,
        declared_tools,
    };

    let mut timed_out_probes = client
        .timed_out_probes()
        .into_iter()
        .map(|probe| types::RunnerDoctorTimedOutProbe {
            reason_code: probe.reason_code.to_string(),
            command: probe.command.clone(),
            replay_command: format!(
                "homeboy ssh {} -- {}",
                server.id,
                shell::quote_arg(&probe.command)
            ),
        })
        .collect::<Vec<_>>();
    if daemon_timed_out {
        timed_out_probes.push(types::RunnerDoctorTimedOutProbe {
            reason_code: if daemon_timeout.is_zero() {
                "runner_doctor.overall_timeout".to_string()
            } else {
                "runner_doctor.daemon_timeout".to_string()
            },
            command: "daemon.exec".to_string(),
            replay_command: format!(
                "homeboy runner doctor {} --scope lab-offload",
                shell::quote_arg(runner_id)
            ),
        });
    }
    let diagnostics = Some(types::RunnerDoctorDiagnostics {
        status: if timed_out_probes.is_empty() {
            "complete"
        } else {
            "partial"
        },
        completed_checks: checks.len(),
        timed_out_probes,
    });

    RunnerDoctorOutput {
        variant: "doctor",
        command: "runner.doctor",
        runner_id: runner_id.to_string(),
        runner: runner_summary("ssh", Some(runner), Some(server)),
        status: checks::overall_status(&checks),
        capabilities,
        resources,
        checks,
        secret_env_migration: None,
        diagnostics,
        daemon_recovery: persisted_status
            .as_ref()
            .and_then(|status| status.daemon_freshness.clone()),
        admission_summary: persisted_status.as_ref().and_then(admission_summary),
        repairs: Vec::new(),
    }
}

pub(super) fn probe_limits(scope: RunnerDoctorScope) -> (std::time::Duration, std::time::Duration) {
    match scope {
        RunnerDoctorScope::LabOffload => (
            std::time::Duration::from_secs(3),
            std::time::Duration::from_secs(20),
        ),
        RunnerDoctorScope::General | RunnerDoctorScope::SecretEnv => (
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(60),
        ),
    }
}

pub(super) fn unreachable_report(
    runner_id: &str,
    runner: &Runner,
    server: &Server,
    output: homeboy::core::server::CommandOutput,
) -> RunnerDoctorOutput {
    RunnerDoctorOutput {
        variant: "doctor", command: "runner.doctor", runner_id: runner_id.to_string(),
        runner: runner_summary("ssh", Some(runner), Some(server)), status: RunnerDoctorStatus::Error,
        capabilities: RunnerCapabilities::default(), resources: RunnerResources::default(),
        checks: vec![checks::error("ssh.execution", format!("SSH runner {} is not reachable", runner_id), Some("Run `homeboy server status <server-id>` and verify host, user, port, identity_file, and network access".to_string()), common::detail_map(&[("stderr", output.stderr.trim()), ("stdout", output.stdout.trim())]))],
        secret_env_migration: None, diagnostics: Some(types::RunnerDoctorDiagnostics { status: "partial", completed_checks: 1, timed_out_probes: Vec::new() }), daemon_recovery: None, admission_summary: None, repairs: Vec::new(),
    }
}

pub(super) fn disconnected_report(
    runner_id: &str,
    runner: &Runner,
    server: &Server,
    daemon_recovery: Option<homeboy::core::daemon::DaemonFreshnessReport>,
    admission_summary: Option<homeboy::runner::runners::RunnerAdmissionSummary>,
) -> RunnerDoctorOutput {
    let mut details = BTreeMap::new();
    let (message, remediation) = match daemon_recovery.as_ref() {
        Some(recovery) => {
            details.insert("active_jobs".to_string(), recovery.active_jobs.to_string());
            if let Some(lease_id) = &recovery.lease_id {
                details.insert("lease_id".to_string(), lease_id.clone());
            }
            if let Some(pid) = recovery.pid {
                details.insert("pid".to_string(), pid.to_string());
            }
            if let Some(version) = &recovery.daemon_version {
                details.insert("daemon_version".to_string(), version.clone());
            }
            if let Some(identity) = &recovery.daemon_build_identity {
                details.insert("daemon_build_identity".to_string(), identity.clone());
            }
            if let Some(evidence) = &recovery.ownership_evidence {
                details.insert("ownership_evidence".to_string(), evidence.clone());
            }
            if let Some(evidence) = &recovery.termination_evidence {
                details.insert(
                    "termination_evidence".to_string(),
                    serde_json::to_string(evidence)
                        .unwrap_or_else(|_| "unavailable: serialization failed".to_string()),
                );
            }
            (
                "Disconnected runner was checked through bounded remote lease recovery".to_string(),
                recovery.adoption_command.clone(),
            )
        }
        None => (
            "Disconnected runner has no remote daemon recovery evidence".to_string(),
            None,
        ),
    };
    RunnerDoctorOutput {
        variant: "doctor",
        command: "runner.doctor",
        runner_id: runner_id.to_string(),
        runner: runner_summary("ssh", Some(runner), Some(server)),
        status: RunnerDoctorStatus::Error,
        capabilities: RunnerCapabilities::default(),
        resources: RunnerResources::default(),
        checks: vec![checks::error(
            "daemon.recovery",
            message,
            remediation,
            details,
        )],
        secret_env_migration: None,
        diagnostics: None,
        daemon_recovery,
        admission_summary,
        repairs: Vec::new(),
    }
}

fn admission_summary(
    status: &homeboy::runner::runners::RunnerStatusReport,
) -> Option<homeboy::runner::runners::RunnerAdmissionSummary> {
    let generations =
        runner::runner_generation_inventory_for_session(&status.runner_id, status.session.as_ref())
            .ok()?;
    let owners = runner::runner_generation_job_owners_for_session(
        &status.runner_id,
        status.session.as_ref(),
    )
    .ok()?;
    Some(status.admission_summary_with_generations(&generations, &owners, generations.len()))
}

fn default_artifact_root(client: &SshClient) -> String {
    remote_home_dir(client)
        .and_then(|home| default_artifact_root_for_home(&home))
        .unwrap_or_else(|| "~/.local/share/homeboy/artifacts".to_string())
}

fn remote_home_dir(client: &SshClient) -> Option<String> {
    common::remote_line(
        client,
        "home=${HOME:-}; if [ -z \"$home\" ]; then home=$(getent passwd \"$(id -u)\" 2>/dev/null | cut -d: -f6); fi; if [ -z \"$home\" ]; then home=$(cd ~ 2>/dev/null && pwd -P); fi; [ -n \"$home\" ] && printf '%s\n' \"$home\"",
    )
}

pub(super) fn default_artifact_root_for_home(home: &str) -> Option<String> {
    let home = home.trim();
    if home.is_empty() {
        return None;
    }
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return Some("/.local/share/homeboy/artifacts".to_string());
    }
    Some(format!("{home}/.local/share/homeboy/artifacts"))
}
