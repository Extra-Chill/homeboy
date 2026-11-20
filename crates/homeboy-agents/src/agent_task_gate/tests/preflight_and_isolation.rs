use super::*;

/// #15882: a registry-only component (no portable `homeboy.json`) resolves
/// inside the isolated gate HOME at the gate checkout, so the canonical
/// `homeboy review test <component>` gate passes preflight as it does in the
/// operator's shell.
#[cfg(unix)]
#[test]
fn gate_probe_resolves_a_registry_only_component_under_isolated_home() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(
        module_path!(),
        "gate_probe_resolves_a_registry_only_component_under_isolated_home",
    ) {
        return;
    }
    let host_home = tempfile::tempdir().expect("host home");
    for name in homeboy_core::paths::HOMEBOY_ROOT_ENV_VARS {
        std::env::remove_var(name);
    }
    std::env::set_var("HOME", host_home.path());
    let registry = host_home.path().join(".config/homeboy/components");
    fs::create_dir_all(&registry).expect("host registry");
    fs::write(
        registry.join("registry-only.json"),
        r#"{"local_path":"/host/checkout","remote_url":"https://example.test/registry-only.git"}"#,
    )
    .expect("host registration");
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = tempfile::tempdir().expect("bin");
    // Stands in for the real CLI: the component resolves only when it is
    // registered, at the gate checkout, in the HOME the probe runs under.
    homeboy_test_support::write_executable(
        &bin.path().join("homeboy"),
        r#"#!/bin/sh
record="${HOMEBOY_CONFIG_ROOT:-$HOME/.config/homeboy}/components/$3.json"
grep -q "\"local_path\": \"$PWD\"" "$record" 2>/dev/null \
  && grep -q '"remote_url": "https://example.test/registry-only.git"' "$record" \
  && { echo '{"success":true,"data":{"preflight":"ready"}}'; exit 0; }
echo '{"success":false,"summary":"Component not found"}'
exit 2
"#,
        0o755,
    )
    .expect("homeboy");
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.path().display()));

    let gates = VerifyGateOptions {
        verify: vec!["homeboy review test registry-only".to_string()],
        gate_environment: AgentTaskGateEnvironmentPolicy {
            isolate_home: true,
            admitted_component_id: Some("registry-only".to_string()),
            ..AgentTaskGateEnvironmentPolicy::default()
        },
        ..VerifyGateOptions::default()
    };
    preflight_verify_gate_options(workspace.path(), &gates)
        .expect("the probe resolves the registered component at the gate checkout");
}

/// #16268, #17569: a nested gate probe runs with the same component inputs
/// as the gate itself, including a sub-component's extensions when the
/// admitted root has none. Under HOME isolation the probe must
/// see the component's host-installed extensions, or it rejects a gate the
/// operator's own shell passes; and a failed probe reports what Homeboy
/// said on stdout rather than an empty stderr.
#[cfg(unix)]
#[test]
fn gate_probe_sees_the_components_linked_extensions_under_isolated_home() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(
        module_path!(),
        "gate_probe_sees_the_components_linked_extensions_under_isolated_home",
    ) {
        return;
    }
    // This runs alone in a child process, so it owns HOME and PATH.
    let host_home = tempfile::tempdir().expect("host home");
    for name in homeboy_core::paths::HOMEBOY_ROOT_ENV_VARS {
        std::env::remove_var(name);
    }
    std::env::set_var("HOME", host_home.path());
    let extension = host_home
        .path()
        .join(".config/homeboy/extensions/probe-ext");
    fs::create_dir_all(&extension).expect("host extension");
    fs::write(
        extension.join("probe-ext.json"),
        r#"{"id":"probe-ext","name":"Probe","version":"1.0.0"}"#,
    )
    .expect("extension manifest");

    let workspace = tempfile::tempdir().expect("workspace");
    assert!(Command::new("git")
        .arg("init")
        .current_dir(workspace.path())
        .status()
        .expect("git init")
        .success());
    fs::write(
        workspace.path().join("homeboy.json"),
        r#"{"id":"probe-root"}"#,
    )
    .expect("admitted component without extensions");
    let sibling = workspace.path().join("sub-component");
    fs::create_dir(&sibling).expect("sub-component");
    fs::write(
        sibling.join("homeboy.json"),
        r#"{"id":"probe-component","extensions":{"probe-ext":{}}}"#,
    )
    .expect("portable component");
    let bin = tempfile::tempdir().expect("bin");
    let homeboy = bin.path().join("homeboy");
    // Stands in for the real CLI: the test capability resolves only when
    // the component's extension is installed in the HOME it runs under,
    // and a failure is reported the way Homeboy reports it, on stdout.
    homeboy_test_support::write_executable(
        &homeboy,
        r#"#!/bin/sh
manifest="${HOMEBOY_CONFIG_ROOT:-$HOME/.config/homeboy}/extensions/probe-ext/probe-ext.json"
[ -f "$manifest" ] && { echo '{"success":true,"data":{"preflight":"ready"}}'; exit 0; }
echo '{"success":false,"summary":"no linked extensions provide test support: probe-ext"}'
exit 2
"#,
        0o755,
    )
    .expect("homeboy");

    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.path().display()));

    let gates = VerifyGateOptions {
        verify: vec!["homeboy review test probe-component --path sub-component".to_string()],
        gate_environment: AgentTaskGateEnvironmentPolicy {
            isolate_home: true,
            admitted_component_id: Some("probe-root".to_string()),
            ..AgentTaskGateEnvironmentPolicy::default()
        },
        ..VerifyGateOptions::default()
    };
    preflight_verify_gate_options(workspace.path(), &gates)
        .expect("the probe sees the extension the gate will see");
    let mut selected = selected_gate_environment(&gates.gate_environment, None)
        .expect("isolated gate environment");
    selected
        .configure_linked_extensions(workspace.path())
        .unwrap();
    assert_eq!(selected.report.linked_extensions.len(), 1);
    assert_eq!(selected.report.linked_extensions[0].id, "probe-ext");
    let installed = selected
        .isolated_home
        .as_ref()
        .unwrap()
        .join(".config/homeboy/extensions/probe-ext");
    assert!(installed.is_symlink());
    assert_eq!(
        installed.canonicalize().unwrap(),
        extension.canonicalize().unwrap()
    );

    fs::remove_dir_all(&extension).expect("uninstall host extension");
    let error = preflight_verify_gate_options(workspace.path(), &gates)
        .expect_err("without the extension the gate cannot run");
    assert!(
        error.details["cause"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(
                "the probe exited 2: no linked extensions provide test support: probe-ext"
            )),
        "{}",
        error.details["cause"]
    );
    assert!(
            error.message.contains(
                "missing declared extension(s) `probe-ext` (provision with `homeboy extension install <source> --id probe-ext`)"
            ),
            "{}",
            error.message
        );
}

/// #17769: a declared `homeboy review test` gate for a component whose
/// declared extension is not installed on the gate host resolves preflight
/// ready in an isolated HOME/XDG: the gate environment provisions it from
/// the controller's recorded source through the extension install path.
#[cfg(unix)]
#[test]
fn gate_env_provisions_declared_extension_so_review_test_preflight_is_ready() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(
        module_path!(),
        "gate_env_provisions_declared_extension_so_review_test_preflight_is_ready",
    ) {
        return;
    }
    // This runs alone in a child process, so it owns HOME and PATH.
    let host_home = tempfile::tempdir().expect("host home");
    for name in homeboy_core::paths::HOMEBOY_ROOT_ENV_VARS {
        std::env::remove_var(name);
    }
    std::env::set_var("HOME", host_home.path());
    let controller_source = tempfile::tempdir().expect("controller extension source");
    let source = controller_source.path().join("probe-ext");
    fs::create_dir_all(&source).expect("extension source");
    fs::write(
        source.join("probe-ext.json"),
        r#"{"id":"probe-ext","name":"Probe","version":"1.0.0"}"#,
    )
    .expect("extension manifest");

    let workspace = tempfile::tempdir().expect("workspace");
    assert!(Command::new("git")
        .arg("init")
        .current_dir(workspace.path())
        .status()
        .expect("git init")
        .success());
    fs::write(
        workspace.path().join("homeboy.json"),
        r#"{"id":"probe-component","extensions":{"probe-ext":{}}}"#,
    )
    .expect("portable component");
    let bin = tempfile::tempdir().expect("bin");
    homeboy_test_support::write_executable(
        bin.path().join("homeboy"),
        r#"#!/bin/sh
manifest="${HOMEBOY_CONFIG_ROOT:-$HOME/.config/homeboy}/extensions/probe-ext/probe-ext.json"
[ -f "$manifest" ] && { echo '{"success":true,"data":{"preflight":"ready"}}'; exit 0; }
echo '{"success":false,"summary":"No extension provider configured for component probe-component"}'
exit 2
"#,
        0o755,
    )
    .expect("homeboy");
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.path().display()));

    let gates = VerifyGateOptions {
        verify: vec!["homeboy review test probe-component".to_string()],
        gate_environment: AgentTaskGateEnvironmentPolicy {
            isolate_home: true,
            isolate_xdg: true,
            admitted_component_id: Some("probe-component".to_string()),
            extension_sources: vec![AgentTaskGateExtensionSource {
                id: "probe-ext".to_string(),
                source: source.display().to_string(),
                revision: None,
            }],
            ..AgentTaskGateEnvironmentPolicy::default()
        },
        ..VerifyGateOptions::default()
    };
    preflight_verify_gate_options(workspace.path(), &gates)
        .expect("the gate environment provisions the declared extension");
    assert!(host_home
        .path()
        .join(".config/homeboy/extensions/probe-ext/probe-ext.json")
        .is_file());
}

#[cfg(unix)]
#[test]
fn toolchain_preflight_runs_in_the_invocation_tmpdir() {
    // Deliberately hermetic: the probe is addressed by absolute path so the
    // test never mutates PATH. Sibling tests spawn `sh` without holding
    // ENV_MUTEX, so clobbering PATH here would break them under the test
    // harness's parallelism.
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
    let bin = tempfile::tempdir().expect("bin");
    let gate_tmpdir = runtime_tmpdir.path().join("tmp");

    // Fails unless every temp-dir variable points at the invocation temp
    // dir. An unset variable compares as empty and exits non-zero too.
    let tool = bin.path().join("record-tmpdir");
    homeboy_test_support::write_executable(&tool, format!(
                "#!/bin/sh\n[ \"$TMPDIR\" = '{dir}' ] || exit 3\n[ \"$TEMP\" = '{dir}' ] || exit 4\n[ \"$TMP\" = '{dir}' ] || exit 5\nexit 0\n",
                dir = gate_tmpdir.display()
            ), 0o755)
        .expect("tool");

    preflight_gate_toolchains(
        workspace.path(),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: tool.display().to_string(),
            probe_arguments: vec!["initialize".to_string()],
        }],
        &[],
        Some(runtime_tmpdir.path()),
        Duration::from_secs(10),
    )
    .expect("preflight probes run in the invocation temp dir");
}

#[cfg(unix)]
#[test]
fn toolchain_preflight_times_out_and_reaps_descendants() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = tempfile::tempdir().expect("bin");
    let pid_file = workspace.path().join("descendant.pid");
    let tool = bin.path().join("hung-tool");
    homeboy_test_support::write_executable(
        &tool,
        format!(
            "#!/bin/sh\n/bin/sleep 30 &\necho $! > '{}'\nwait\n",
            pid_file.display()
        ),
        0o755,
    )
    .expect("tool");

    let started = std::time::Instant::now();
    let error = preflight_gate_toolchains(
        workspace.path(),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: tool.display().to_string(),
            probe_arguments: Vec::new(),
        }],
        &[],
        None,
        Duration::from_secs(10),
    )
    .expect_err("hung toolchain probe must time out");

    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(error.retryable, Some(true));
    assert_eq!(
        error.details["toolchain_preflight"]["timed_out"], true,
        "{:#}",
        error.details
    );
    assert_eq!(error.details["toolchain_preflight"]["timeout_ms"], 10_000);
    let descendant_pid = fs::read_to_string(pid_file)
        .expect("descendant pid")
        .trim()
        .parse::<libc::pid_t>()
        .expect("numeric descendant pid");
    assert!(
        !homeboy_engine_primitives::command::process_is_running(descendant_pid as u32),
        "toolchain preflight left descendant {descendant_pid} runnable"
    );
}

#[cfg(unix)]
#[test]
fn toolchain_preflight_honors_a_100ms_limit() {
    let workspace = tempfile::tempdir().expect("workspace");
    let store = homeboy_core::observation::ObservationStore::open_initialized_at(
        workspace.path().join("observe.db"),
    )
    .unwrap();
    let run_id = store
        .start_run(homeboy_core::observation::NewRunRecord::builder("agent-task").build())
        .unwrap()
        .id;
    let _timing = homeboy_core::observation::phase_timing::for_run(&store, &run_id);
    let tool = workspace.path().join("hung-tool");
    homeboy_test_support::write_executable(&tool, "#!/bin/sh\n/bin/sleep 30\n", 0o755)
        .expect("tool");

    let started = std::time::Instant::now();
    let error = preflight_gate_toolchains(
        workspace.path(),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: tool.display().to_string(),
            probe_arguments: Vec::new(),
        }],
        &[],
        None,
        Duration::from_millis(100),
    )
    .expect_err("hung toolchain probe must time out");

    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(error.details["toolchain_preflight"]["timed_out"], true);
    assert_eq!(error.details["toolchain_preflight"]["timeout_ms"], 100);
    let spans = store.phase_spans(&run_id).unwrap();
    assert_eq!(spans.spans[0].id, "admit.gate_toolchains");
    assert_eq!(
        spans.spans[0].status,
        homeboy_engine_primitives::phase_timing::PhaseStatus::Failed
    );
    assert!(spans.spans[0].duration_ms.unwrap() >= 100.0);
}

#[cfg(unix)]
#[test]
fn toolchain_preflight_shares_one_total_deadline_across_probes() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = tempfile::tempdir().expect("bin");
    let first = bin.path().join("first-tool");
    let second = bin.path().join("second-tool");
    homeboy_test_support::write_executable(&first, "#!/bin/sh\n/bin/sleep 0.1\n", 0o755)
        .expect("first tool");
    homeboy_test_support::write_executable(&second, "#!/bin/sh\n/bin/sleep 0.05\nexit 7\n", 0o755)
        .expect("second tool");

    let started = std::time::Instant::now();
    let error = preflight_gate_toolchains(
        workspace.path(),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[
            AgentTaskGateToolchainRequirement {
                resolve_only: false,
                command: first.display().to_string(),
                probe_arguments: Vec::new(),
            },
            AgentTaskGateToolchainRequirement {
                resolve_only: false,
                command: second.display().to_string(),
                probe_arguments: Vec::new(),
            },
        ],
        &[],
        None,
        Duration::from_secs(10),
    )
    .expect_err("second probe must consume only the first probe's remaining deadline");

    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(error.details["toolchain_preflight"]["timed_out"], false);
    assert_eq!(
        error.details["toolchain_preflight"]["command"],
        second.display().to_string()
    );
    assert!(
        error.details["toolchain_preflight"]["probe_timeout_ms"]
            .as_u64()
            .expect("per-probe timeout")
            < 10_000
    );
}

#[cfg(unix)]
#[test]
fn toolchain_preflight_bounds_output_and_reports_capture_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = tempfile::tempdir().expect("bin");
    let tool = bin.path().join("noisy-tool");
    homeboy_test_support::write_executable(
        &tool,
        "#!/bin/sh\nyes stdout | head -c 70000\nyes stderr | head -c 70000 >&2\nexit 7\n",
        0o755,
    )
    .expect("tool");

    let error = preflight_gate_toolchains(
        workspace.path(),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: tool.display().to_string(),
            probe_arguments: Vec::new(),
        }],
        &[],
        None,
        Duration::from_secs(10),
    )
    .expect_err("noisy failed probe must return diagnostics");

    let diagnostics = &error.details["toolchain_preflight"];
    assert_eq!(diagnostics["capture_limit_bytes"], 65_536);
    assert_eq!(diagnostics["stdout"]["bytes_seen"], 70_000);
    assert_eq!(diagnostics["stderr"]["bytes_seen"], 70_000);
    assert_eq!(diagnostics["stdout"]["bytes_retained"], 65_536);
    assert_eq!(diagnostics["stderr"]["bytes_retained"], 65_536);
    assert_eq!(diagnostics["stdout"]["truncated"], true);
    assert_eq!(diagnostics["stderr"]["truncated"], true);
    assert_eq!(
        diagnostics["stdout"]["tail"]
            .as_str()
            .expect("stdout tail")
            .len(),
        65_536
    );
    assert_eq!(
        diagnostics["stderr"]["tail"]
            .as_str()
            .expect("stderr tail")
            .len(),
        65_536
    );
}

#[test]
fn gate_environment_pins_temp_dir_variables_to_the_invocation_tmpdir() {
    let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
    let selected = selected_gate_environment(
        &AgentTaskGateEnvironmentPolicy::default(),
        Some(runtime_tmpdir.path()),
    )
    .expect("gate environment");

    let expected = runtime_tmpdir.path().join("tmp").display().to_string();
    for name in TMPDIR_ENV_VARS {
        assert_eq!(
            selected.values.get(*name),
            Some(&expected),
            "{name} must resolve to the invocation temp dir"
        );
    }
}

#[test]
fn gate_environment_relocates_isolated_config_root_and_data_roots_into_gate_home() {
    let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
    let operator = tempfile::tempdir().expect("operator roots");
    let operator_root = |name: &str| operator.path().join(name).display().to_string();
    // The controller's ambient process environment, plus the roots a Cook
    // step injects into the gate's declared variables.
    let ambient: BTreeMap<String, String> = [
        ("PATH", "/usr/bin".to_string()),
        ("HOME", operator_root("home")),
        ("HOMEBOY_CONFIG_ROOT", operator_root("config")),
        ("HOMEBOY_DATA_DIR", operator_root("data")),
        ("HOMEBOY_ARTIFACT_ROOT", operator_root("artifacts")),
        // The supervising Cook's own identity (set by a bound step child).
        (
            homeboy_core::daemon::step_job::LOCAL_COOK_SUPERVISOR_JOB_ID_ENV,
            "supervisor-job".to_string(),
        ),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect();
    for mode in [
        AgentTaskGateEnvironmentMode::Inherit,
        AgentTaskGateEnvironmentMode::Replace,
    ] {
        let mut policy = AgentTaskGateEnvironmentPolicy {
            mode,
            ..AgentTaskGateEnvironmentPolicy::default()
        };
        assert!(policy.isolate_home);
        for name in ["HOMEBOY_CONFIG_ROOT", "HOMEBOY_DATA_DIR"] {
            policy
                .variables
                .insert(name.to_string(), ambient[name].clone());
        }
        let selected = selected_gate_environment(&policy, Some(runtime_tmpdir.path()))
            .expect("gate environment");
        let gate_home = selected.isolated_home.clone().expect("isolated gate home");

        // Effective child environment: ambient, then the gate overlay.
        let setup = selected.hydration_environment();
        let mut effective = if setup.clear {
            BTreeMap::new()
        } else {
            ambient.clone()
        };
        for name in &setup.remove {
            effective.remove(name);
        }
        effective.extend(setup.set.clone());

        assert_eq!(
            effective.get("HOME").map(PathBuf::from),
            Some(gate_home.clone())
        );
        for name in homeboy_core::paths::HOMEBOY_ROOT_ENV_VARS {
            let value = effective
                .get(*name)
                .unwrap_or_else(|| panic!("{name} must be pinned for {mode:?}"));
            assert!(
                PathBuf::from(value).starts_with(&gate_home),
                "{name}={value} must stay inside the gate home {}",
                gate_home.display()
            );
        }
        for name in COOK_SUPERVISOR_PRIVATE_ENV_VARS {
            assert!(
                !effective.contains_key(*name),
                "{name} is the supervisor's own identity and must not reach a {mode:?} gate"
            );
        }
        for (name, value) in &effective {
            if name.starts_with("HOMEBOY_") {
                assert!(
                    !value.starts_with(operator.path().to_string_lossy().as_ref()),
                    "{name}={value} reaches the controller's roots"
                );
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn gate_command_drops_worker_identity_but_keeps_capabilities_and_policy_inputs() {
    use homeboy_core::runner_job_environment::{
        is_runner_job_identity_env, RUNNER_JOB_IDENTITY_ENV,
    };

    const WORKER: &str = "TEST_GATE_IDENTITY_WORKER";
    if std::env::var_os(WORKER).is_none() {
        let test_name = homeboy_core::test_support::harness_test_name(
            module_path!(),
            "gate_command_drops_worker_identity_but_keeps_capabilities_and_policy_inputs",
        );
        let mut worker = Command::new(std::env::current_exe().expect("test binary"));
        worker.args(["--exact", &test_name, "--nocapture"]);
        worker.env(WORKER, "1");
        let (prefixes, names) = RUNNER_JOB_IDENTITY_ENV;
        for prefix in prefixes {
            worker.env(format!("{prefix}FUTURE_CONTROL"), "worker-identity");
        }
        for name in names.iter().chain(COOK_SUPERVISOR_PRIVATE_ENV_VARS) {
            worker.env(name, "worker-identity");
        }
        worker.env("HOMEBOY_WP_CODEBOX_BIN", "/worker/codebox");
        worker.env("HOMEBOY_NO_UPDATE_CHECK", "1");
        let output = homeboy_core::test_support::run_child_test(&mut worker, &test_name);
        assert!(
            output.status.success(),
            "worker failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    assert_eq!(
        std::env::var("HOMEBOY_RUNNER_JOB_ID").unwrap(),
        "worker-identity"
    );
    let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
    let mut policy = AgentTaskGateEnvironmentPolicy::default();
    policy
        .variables
        .insert("HOMEBOY_GATE_INPUT".into(), "declared".into());
    let selected =
        selected_gate_environment(&policy, Some(runtime_tmpdir.path())).expect("gate environment");
    let mut command = Command::new("/usr/bin/env");
    selected.apply(&mut command);
    let output = command.output().expect("gate command");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("environment output");
    let environment: BTreeMap<_, _> = stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    for name in environment.keys() {
        assert!(!is_runner_job_identity_env(name), "leaked {name}");
        assert!(
            !COOK_SUPERVISOR_PRIVATE_ENV_VARS.contains(name),
            "leaked {name}"
        );
    }
    assert_eq!(
        environment.get("HOMEBOY_WP_CODEBOX_BIN"),
        Some(&"/worker/codebox")
    );
    assert_eq!(environment.get("HOMEBOY_NO_UPDATE_CHECK"), Some(&"1"));
    assert_eq!(environment.get("HOMEBOY_GATE_INPUT"), Some(&"declared"));

    // Even an identity name is allowed when explicitly owned by gate policy.
    policy
        .variables
        .insert("HOMEBOY_RUN_ID".into(), "gate-run".into());
    let selected =
        selected_gate_environment(&policy, Some(runtime_tmpdir.path())).expect("gate environment");
    assert!(!selected
        .hydration_environment()
        .remove
        .contains(&"HOMEBOY_RUN_ID".to_string()));
    let mut command = Command::new("/usr/bin/env");
    selected.apply(&mut command);
    let output = command
        .output()
        .expect("gate command with explicit identity");
    assert!(String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line == "HOMEBOY_RUN_ID=gate-run"));
}

#[test]
fn dependency_setup_environment_matches_the_gate_command_environment() {
    let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
    for mode in [
        AgentTaskGateEnvironmentMode::Inherit,
        AgentTaskGateEnvironmentMode::Replace,
    ] {
        let mut policy = AgentTaskGateEnvironmentPolicy {
            mode,
            ..AgentTaskGateEnvironmentPolicy::default()
        };
        policy
            .variables
            .insert("PATH".to_string(), "/gate-toolchain/bin".to_string());
        let selected = selected_gate_environment(&policy, Some(runtime_tmpdir.path()))
            .expect("gate environment");

        let setup = selected.hydration_environment();
        assert_eq!(setup.clear, mode == AgentTaskGateEnvironmentMode::Replace);
        assert_eq!(setup.set, selected.values);
        assert_eq!(
            setup.set.get("PATH").map(String::as_str),
            Some("/gate-toolchain/bin")
        );
        let sanitized: Vec<String> = selected
            .report
            .sanitized
            .iter()
            .map(|variable| variable.name.clone())
            .chain(
                COOK_SUPERVISOR_PRIVATE_ENV_VARS
                    .iter()
                    .map(|name| name.to_string()),
            )
            .chain(std::env::vars_os().filter_map(|(name, _)| {
                let name = name.to_str()?;
                homeboy_core::runner_job_environment::is_runner_job_identity_env(name)
                    .then(|| name.to_string())
            }))
            .collect();
        assert_eq!(setup.remove, sanitized);
    }
}

#[test]
fn npm_missing_script_is_a_non_retryable_gate_declaration_failure() {
    let workspace = tempfile::tempdir().expect("workspace");
    fs::write(
        workspace.path().join("package.json"),
        r#"{"name":"fixture-package","scripts":{"test":"true"}}"#,
    )
    .expect("manifest");
    let gates = VerifyGateOptions {
        verify: vec!["npm run typecheck".to_string()],
        ..Default::default()
    };

    let error = gates
        .preflight_declarations(workspace.path())
        .expect_err("missing script is a declaration failure");

    assert_eq!(error.details["failure_classification"], "gate_declaration");
    assert_eq!(error.details["package"], "fixture-package");
    assert_eq!(error.details["missing_script"], "typecheck");
    assert!(error.details["remediation"]
        .as_str()
        .expect("remediation")
        .contains("scripts"));
}

/// A symlinked invocation temp alias must yield sandbox paths that are
/// simultaneously **short** and **non-symlink**.
///
/// Resolving the alias away (canonicalizing) would satisfy the non-symlink
/// half while discarding the `sockaddr_un` budget the invocation layer
/// validated on the alias, so this asserts the exported paths stay *under
/// the alias* rather than under its canonical target.
#[cfg(unix)]
#[test]
fn gate_environment_exports_a_real_directory_beneath_the_invocation_tmp_alias() {
    let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
    let alias_root = tempfile::tempdir().expect("alias root");
    let alias = alias_root.path().join("runtime-tmp-alias");
    std::os::unix::fs::symlink(runtime_tmpdir.path(), &alias).expect("runtime temp alias");

    let selected =
        selected_gate_environment(&AgentTaskGateEnvironmentPolicy::default(), Some(&alias))
            .expect("gate environment");
    let expected_root = alias.join("tmp");

    for name in TMPDIR_ENV_VARS {
        assert_eq!(
            selected.values.get(*name).map(PathBuf::from),
            Some(expected_root.clone()),
            "{name} must stay beneath the short invocation temp alias"
        );
    }

    // The exported root is reached *through* the alias, but is itself a
    // real directory — the property security-sensitive child tools check.
    assert!(!fs::symlink_metadata(&expected_root)
        .expect("gate temp root metadata")
        .file_type()
        .is_symlink());
    assert!(expected_root.is_dir());

    for variable in &selected.report.sanitized {
        let path = PathBuf::from(&variable.value);
        assert!(path.starts_with(&expected_root));
        assert!(!fs::symlink_metadata(path)
            .expect("isolated path metadata")
            .file_type()
            .is_symlink());
    }
}

/// Extra-Chill/homeboy#15393: a freshly created cache root must pass the
/// safety check whatever the umask. Run the suite under `umask 002` to
/// exercise the case that failed on the lab runner.
#[cfg(unix)]
#[test]
fn rust_cache_root_is_created_private_and_accepted() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("controller-state/gate-rust-cache");
    super::super::ensure_safe_rust_cache_root(&root).expect("fresh root is accepted");
    let mode = fs::metadata(&root).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    // Idempotent on the second call.
    super::super::ensure_safe_rust_cache_root(&root).expect("existing private root is accepted");
}

#[test]
fn unsafe_rust_cache_root_message_names_path_owner_mode_and_repair() {
    let message = super::super::unsafe_rust_cache_root_message(
        Path::new("/data/controller-state/gate-rust-cache"),
        0,
        0o40775,
        1000,
    );
    for expected in [
        "/data/controller-state/gate-rust-cache",
        "owned by uid 0",
        "mode 775",
        "expected uid 1000",
        "rm -rf",
    ] {
        assert!(
            message.contains(expected),
            "{expected} missing from: {message}"
        );
    }
}

/// Extra-Chill/homeboy#15447: the cache kept one ~2 GB identity per
/// lockfile forever. Eviction keeps the newest ones and the current one,
/// and never removes an identity whose hydration lock is held.
#[test]
fn rust_cache_eviction_keeps_recent_current_and_locked_identities() {
    use std::time::{Duration, SystemTime};
    let root = tempfile::tempdir().expect("cache root");
    let make = |name: &str, age_secs: u64| {
        let dir = root.path().join(name);
        fs::create_dir_all(dir.join("cargo")).expect("identity");
        let marker = dir.join("ready.json");
        fs::write(&marker, "{}").expect("ready marker");
        let mtime = SystemTime::now() - Duration::from_secs(age_secs);
        fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .and_then(|file| file.set_modified(mtime))
            .expect("set marker mtime");
    };
    make("current", 0);
    make("recent", 100);
    make("old", 200);
    make("older", 300);
    make("locked", 400);
    // Unhydrated: no marker, so it sorts oldest and is evicted.
    fs::create_dir_all(root.path().join("abandoned/cargo")).expect("abandoned");
    let held = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(root.path().join("locked/hydrate.lock"))
        .expect("lock file");
    assert!(held.try_lock_exclusive().expect("hold lock"));

    let mut removed = evict_stale_rust_cache_identities(root.path(), "current", 2).expect("evict");
    removed.sort();
    assert_eq!(
        removed,
        vec![
            "abandoned".to_string(),
            "old".to_string(),
            "older".to_string()
        ]
    );
    for kept in ["current", "recent", "locked"] {
        assert!(root.path().join(kept).is_dir(), "{kept} was removed");
    }
}

/// Regression for #15383: the admitted component's linked extensions and
/// their sibling shared assets are linked into the isolated HOME from the
/// executing host, without copying; other installed extensions are not.
#[cfg(unix)]
#[test]
fn linked_runner_extensions_are_exposed_under_isolated_home() {
    let host = tempfile::tempdir().expect("host extensions");
    let ext = host.path();
    std::fs::create_dir_all(ext.join("wordpress/vendor")).expect("wordpress");
    std::fs::write(ext.join("wordpress/wordpress.json"), "{}").expect("manifest");
    std::fs::create_dir_all(ext.join("rust")).expect("rust");
    std::fs::write(ext.join("rust/rust.json"), "{}").expect("rust manifest");
    std::fs::create_dir_all(ext.join("scripts/lib")).expect("shared scripts");
    std::fs::create_dir_all(ext.join("dependency-adapters")).expect("shared adapters");
    let home = tempfile::tempdir().expect("gate home");
    let destination = home.path().join(".config/homeboy/extensions");

    let ids = ["wordpress".to_string(), "missing".to_string()]
        .into_iter()
        .collect();
    let linked = link_runner_extensions(ext, &destination, &ids, &Default::default())
        .expect("link extensions");

    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].id, "wordpress");
    let wordpress = destination.join("wordpress");
    assert!(std::fs::symlink_metadata(&wordpress)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(
        wordpress.join("wordpress.json").is_file(),
        "linked tree is readable"
    );
    assert!(
        destination.join("scripts/lib").is_dir(),
        "shared assets are linked"
    );
    assert!(destination.join("dependency-adapters").is_dir());
    assert!(
        !destination.join("rust").exists(),
        "unrequested extensions stay hidden"
    );
    assert!(!destination.join("missing").exists());
}

#[cfg(unix)]
#[test]
fn explicit_extension_inputs_take_precedence_over_linked_extensions() {
    let host = tempfile::tempdir().expect("host extensions");
    std::fs::create_dir_all(host.path().join("wordpress")).expect("wordpress");
    std::fs::write(host.path().join("wordpress/wordpress.json"), "{}").expect("manifest");
    let home = tempfile::tempdir().expect("gate home");
    let destination = home.path().join(".config/homeboy/extensions");
    let ids = ["wordpress".to_string()].into_iter().collect();
    let skip = ["wordpress".to_string()].into_iter().collect();

    let linked =
        link_runner_extensions(host.path(), &destination, &ids, &skip).expect("link extensions");

    assert!(linked.is_empty());
    assert!(!destination.join("wordpress").exists());
}

/// Regression for #15824: declared test secrets survive `replace` mode
/// and HOME isolation; values never enter the report.
#[test]
fn declared_secret_env_is_carried_into_replaced_gate_environment() {
    let root = tempfile::tempdir().expect("runtime root");
    let policy = AgentTaskGateEnvironmentPolicy {
        mode: AgentTaskGateEnvironmentMode::Replace,
        isolate_home: true,
        isolate_xdg: true,
        ..AgentTaskGateEnvironmentPolicy::default()
    };
    let mut selected =
        selected_gate_environment(&policy, Some(root.path())).expect("gate environment");
    let names = vec![
        "WP_CODEBOX_DB_PASSWORD".to_string(),
        "WP_CODEBOX_DB_HOST".to_string(),
        "UNSET_DECLARED_SECRET".to_string(),
    ];
    selected.carry_secret_env_names(&names, |name| match name {
        "WP_CODEBOX_DB_PASSWORD" => Some("s3cret".to_string()),
        "WP_CODEBOX_DB_HOST" => Some("127.0.0.1".to_string()),
        _ => None,
    });

    assert_eq!(
        selected
            .values
            .get("WP_CODEBOX_DB_PASSWORD")
            .map(String::as_str),
        Some("s3cret")
    );
    assert_eq!(
        selected
            .values
            .get("WP_CODEBOX_DB_HOST")
            .map(String::as_str),
        Some("127.0.0.1")
    );
    assert!(!selected.values.contains_key("UNSET_DECLARED_SECRET"));
    assert_eq!(
        selected.report.declared_secret_env,
        vec![
            "WP_CODEBOX_DB_PASSWORD".to_string(),
            "WP_CODEBOX_DB_HOST".to_string()
        ]
    );
    let report = serde_json::to_string(&selected.report).expect("report json");
    assert!(
        !report.contains("s3cret"),
        "secret values never enter the report"
    );
}

/// Regression for #15836: declared HOME-relative runtime dirs are linked
/// into the isolated HOME; undeclared, missing and invalid paths are not.
#[cfg(unix)]
#[test]
fn declared_extension_home_paths_are_linked_into_isolated_home() {
    let host = tempfile::tempdir().expect("host home");
    std::fs::create_dir_all(host.path().join(".cache/homeboy/wp-codebox/source")).expect("cache");
    std::fs::create_dir_all(host.path().join(".ssh")).expect("undeclared secret dir");
    let gate = tempfile::tempdir().expect("gate home");
    let declared = vec![
        ".cache/homeboy/wp-codebox".to_string(),
        ".cache/homeboy/missing".to_string(),
        "../escape".to_string(),
        "/abs".to_string(),
    ];

    let linked = link_declared_home_paths(host.path(), gate.path(), &declared).expect("link");

    assert_eq!(linked, vec![".cache/homeboy/wp-codebox".to_string()]);
    let dest = gate.path().join(".cache/homeboy/wp-codebox");
    assert!(std::fs::symlink_metadata(&dest)
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(dest.join("source").is_dir());
    assert!(
        !gate.path().join(".ssh").exists(),
        "undeclared HOME state stays hidden"
    );
    assert!(!gate.path().join(".cache/homeboy/missing").exists());

    // Re-linking is a no-op rather than an error.
    assert!(
        link_declared_home_paths(host.path(), gate.path(), &declared)
            .expect("relink")
            .is_empty()
    );
}
