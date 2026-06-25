use super::*;
use types::*;

pub fn report(
    runner_id: &str,
    runner: Option<&Runner>,
    options: &RunnerDoctorOptions,
) -> RunnerDoctorOutput {
    let workspace_root = runner
        .and_then(|runner| runner.workspace_root.as_ref())
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let artifact_root = crate::core::paths::artifact_root()
        .unwrap_or_else(|_| workspace_root.join(".homeboy-artifacts"));
    let mut checks = Vec::new();
    let mut tools = BTreeMap::new();

    let homeboy = HomeboyProbe {
        version: env!("CARGO_PKG_VERSION").to_string(),
        path: runner
            .and_then(|runner| runner.settings.homeboy_path.clone())
            .or_else(|| env::current_exe().ok().map(common::display_path)),
    };
    checks.push(checks::ok(
        "homeboy",
        format!("Homeboy {} is running", homeboy.version),
        None,
    ));

    let system = SystemProbe {
        os: env::consts::OS.to_string(),
        arch: env::consts::ARCH.to_string(),
        kernel: common::local_command_line("uname", &["-r"]),
    };
    checks.push(checks::ok(
        "system",
        format!("{} {} runner detected", system.os, system.arch),
        None,
    ));

    let cpu = CpuProbe {
        count: std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1),
    };
    checks.push(checks::ok(
        "cpu",
        format!("{} CPU cores detected", cpu.count),
        None,
    ));

    let memory = probes::local_memory_probe();
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
                "Install platform tools such as sysctl/vm_stat or run on Linux with /proc/meminfo"
                    .to_string(),
            ),
        ),
    });

    let disk = probes::local_disk_probe(&workspace_root);
    checks.push(match &disk {
        Some(disk) => checks::ok(
            "disk.workspace_root",
            format!("{} MB available at workspace root", disk.available_mb),
            None,
        ),
        None => checks::warning(
            "disk.workspace_root",
            "Workspace disk capacity could not be detected".to_string(),
            Some("Ensure df/statvfs is available for the workspace filesystem".to_string()),
        ),
    });

    for spec in probes::tool_specs() {
        if spec.id == "homeboy" {
            continue;
        }
        let probe = probes::local_tool_probe(spec.command, spec.version_args);
        checks.push(checks::tool_check(*spec, &probe));
        tools.insert(spec.id.to_string(), probe);
    }

    for command in normalized_required_tools(&options.required_tools) {
        let version_args = probes::required_tool_version_args(&command);
        let probe = probes::local_tool_probe(&command, version_args);
        checks.push(checks::required_tool_check(&command, &probe));
        tools.entry(command).or_insert(probe);
    }

    let playwright = probes::tool_available(&tools, "playwright");
    let browser_ready = probes::local_browser_ready();
    let display_ready = probes::local_display_ready();
    let xvfb_ready = probes::local_xvfb_ready();
    let headed_browser_ready = probes::headed_browser_ready(display_ready, xvfb_ready);
    checks.push(checks::playwright_check(playwright, browser_ready));
    checks.push(checks::headed_browser_check(
        headed_browser_ready,
        display_ready,
        xvfb_ready,
    ));

    let workspace_writable = probes::local_path_writable(&workspace_root);
    checks.push(checks::path_writable_check(
        "workspace.writable",
        workspace_writable,
        &workspace_root,
        "Make the workspace root writable by the runner user or choose a writable checkout path",
    ));

    let artifact_store_available = probes::local_path_or_parent_writable(&artifact_root);
    checks.push(checks::path_writable_check(
        "artifact_store.available",
        artifact_store_available,
        &artifact_root,
        "Create the artifact root or configure HOMEBOY_ARTIFACT_ROOT to a writable directory",
    ));

    let homeboy_command = homeboy.path.as_deref().unwrap_or("homeboy");
    for extension_id in normalized_extension_ids(&options.extensions) {
        checks.push(extension_parity::local_check(
            homeboy_command,
            options.path.as_deref(),
            &extension_id,
        ));
    }

    let capabilities = probes::capabilities_from(
        &tools,
        true,
        false,
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
        workspace_root: common::display_path(workspace_root),
        artifact_root: common::display_path(artifact_root),
        tools,
    };

    RunnerDoctorOutput {
        variant: "doctor",
        command: "runner.doctor",
        runner_id: runner_id.to_string(),
        runner: runner_summary("local", runner, None),
        status: checks::overall_status(&checks),
        capabilities,
        resources,
        checks,
        repairs: Vec::new(),
    }
}
