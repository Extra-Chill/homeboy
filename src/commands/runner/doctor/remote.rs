use super::*;
use types::*;

pub fn report(
    runner_id: &str,
    runner: &Runner,
    server: &Server,
    client: &SshClient,
    options: &RunnerDoctorOptions,
) -> RunnerDoctorOutput {
    let workspace_root = runner
        .workspace_root
        .clone()
        .unwrap_or_else(|| ".".to_string());
    let artifact_root = default_artifact_root(client);
    let mut checks = Vec::new();
    let mut tools = BTreeMap::new();

    checks.push(match client.execute("printf ok") {
        output if output.success && output.stdout.trim() == "ok" => checks::ok(
            "ssh.execution",
            format!("SSH runner {} is reachable", runner_id),
            None,
        ),
        output => checks::error(
            "ssh.execution",
            format!("SSH runner {} is not reachable", runner_id),
            Some("Run `homeboy server status <server-id>` and verify host, user, port, identity_file, and network access".to_string()),
            common::detail_map(&[("stderr", output.stderr.trim()), ("stdout", output.stdout.trim())]),
        ),
    });

    let homeboy_command = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    let local_homeboy_version = env!("CARGO_PKG_VERSION");
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

    let system = SystemProbe {
        os: common::remote_line(client, "uname -s").unwrap_or_else(|| "unknown".to_string()),
        arch: common::remote_line(client, "uname -m").unwrap_or_else(|| "unknown".to_string()),
        kernel: common::remote_line(client, "uname -r"),
    };
    checks.push(checks::ok(
        "system",
        format!("{} {} runner detected", system.os, system.arch),
        None,
    ));

    let cpu = CpuProbe {
        count: common::remote_line(client, "getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1),
    };
    checks.push(checks::ok(
        "cpu",
        format!("{} CPU cores detected", cpu.count),
        None,
    ));

    let memory = probes::remote_memory_probe(client);
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

    let disk = probes::remote_disk_probe(client, &workspace_root);
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

    for spec in probes::tool_specs() {
        if spec.id == "homeboy" {
            continue;
        }
        let probe = probes::remote_tool_probe(client, spec.command, spec.version_args);
        checks.push(checks::tool_check(*spec, &probe));
        tools.insert(spec.id.to_string(), probe);
    }

    for command in normalized_required_tools(&options.required_tools) {
        let version_args = probes::required_tool_version_args(&command);
        let probe = probes::remote_tool_probe(client, &command, version_args);
        checks.push(checks::required_tool_check(&command, &probe));
        tools.entry(command).or_insert(probe);
    }

    let playwright = probes::tool_available(&tools, "playwright");
    let browser_ready = probes::remote_browser_ready(client);
    let display_ready = probes::remote_display_ready(client);
    let xvfb_ready = probes::remote_xvfb_ready(client);
    let headed_browser_ready = probes::headed_browser_ready(display_ready, xvfb_ready);
    checks.push(checks::playwright_check(playwright, browser_ready));
    checks.push(checks::headed_browser_check(
        headed_browser_ready,
        display_ready,
        xvfb_ready,
    ));

    let workspace_writable = probes::remote_path_writable(client, &workspace_root);
    checks.push(checks::path_writable_check(
        "workspace.writable",
        workspace_writable,
        Path::new(&workspace_root),
        "Make the remote workspace root writable by the runner user",
    ));

    let artifact_store_available =
        probes::remote_artifact_store_available(client, &artifact_root);
    checks.push(checks::path_writable_check(
        "artifact_store.available",
        artifact_store_available,
        Path::new(&artifact_root),
        "Create the artifact root or configure HOMEBOY_ARTIFACT_ROOT to a writable directory",
    ));

    if options.scope == RunnerDoctorScope::LabOffload {
        checks.extend(probes::lab_homeboy_path_checks(
            client,
            runner_id,
            &server.id,
            homeboy_command,
            local_homeboy_version,
            &homeboy,
        ));
        checks.extend(probes::provider_readiness_checks(
            client,
            &homeboy::core::agent_tasks::provider::provider_runner_readiness_contracts(),
        ));
        checks.extend(probes::managed_runner_source_checks(
            client,
            &homeboy::core::agent_tasks::provider::provider_runner_source_contracts(),
        ));
    }

    checks.extend(probes::connected_daemon_exec_checks(
        runner_id,
        &workspace_root,
    ));

    for extension_id in normalized_extension_ids(&options.extensions) {
        checks.push(extension_parity::remote_check(
            client,
            homeboy_command,
            options.path.as_deref(),
            &extension_id,
        ));
    }

    let capabilities = probes::capabilities_from(
        &tools,
        false,
        true,
        playwright,
        browser_ready,
        xvfb_ready,
        headed_browser_ready,
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
    };

    RunnerDoctorOutput {
        variant: "doctor",
        command: "runner.doctor",
        runner_id: runner_id.to_string(),
        runner: runner_summary("ssh", Some(runner), Some(server)),
        status: checks::overall_status(&checks),
        capabilities,
        resources,
        checks,
        repairs: Vec::new(),
    }
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
