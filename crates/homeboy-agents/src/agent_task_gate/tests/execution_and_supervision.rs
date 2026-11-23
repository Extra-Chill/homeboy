use super::*;

#[test]
fn declared_test_executor_reports_its_suite_deadline() {
    let workspace = tempfile::tempdir().expect("workspace");
    let plan =
        homeboy_engine_primitives::test_execution::TestExecutionPlan::declared_homeboy_review_test(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            1,
        )
        .expect("declared plan");
    let supervision = GateSupervision {
        timeout: Duration::from_millis(10),
        no_progress_timeout: Duration::from_secs(1),
        heartbeat_interval: Duration::from_millis(1),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };

    let report = run_gate_argv_with_supervision(
        workspace.path(),
        1,
        vec!["sleep".to_string(), "2".to_string()],
        "homeboy review test",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(&supervision),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
        Some(&plan),
    )
    .expect("timed execution report");

    assert_eq!(report.termination, AgentTaskGateTermination::TimedOut);
    assert_eq!(
        report.test_execution_outcome,
        Some(homeboy_engine_primitives::test_execution::TestExecutionOutcome::TimedOut)
    );
}

/// #18300: a gate that ran and failed is re-run once. Fail-then-pass
/// passes with the first failure kept as flake evidence; a second failure
/// refuses. #18554: a supervision stop is not a candidate verdict: it is
/// re-gated once on the same candidate under the same budgets, a re-gate that
/// passes is green with the stop recorded alongside, and a repeated stop is
/// unavailable verification, never `failed`. A Homeboy setup failure is the
/// gate environment, not the candidate: it is re-gated once and a repeat
/// keeps its environment classification instead of a candidate rerun.
#[test]
fn a_failed_gate_is_rerun_once_and_a_recovered_failure_is_recorded_as_flaky() {
    let timeout = GateSupervision {
        timeout: Duration::from_millis(200),
        no_progress_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_millis(10),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };
    for (script, supervision, status, flaky, regated, expected_runs) in [
        (
            "[ \"$(wc -l < \"$RUNS\")\" -ge 2 ] || { echo first run lost a race; exit 1; }",
            None,
            AgentTaskGateStatus::Succeeded,
            true,
            false,
            2,
        ),
        ("exit 1", None, AgentTaskGateStatus::Failed, false, false, 2),
        (
            "printf '%s' '{\"success\":false,\"diagnostics\":{\"code\":\"validation.invalid_argument\",\"message\":\"no Lab runner to wait for\"}}'; exit 2",
            None,
            AgentTaskGateStatus::Failed,
            false,
            true,
            2,
        ),
        (
            "true",
            None,
            AgentTaskGateStatus::Succeeded,
            false,
            false,
            1,
        ),
        (
            "sleep 5",
            Some(&timeout),
            AgentTaskGateStatus::Unavailable,
            false,
            true,
            2,
        ),
        (
            "[ \"$(wc -l < \"$RUNS\")\" -ge 2 ] || sleep 5",
            Some(&timeout),
            AgentTaskGateStatus::Succeeded,
            false,
            true,
            2,
        ),
    ] {
        let workspace = tempfile::tempdir().expect("workspace");
        let runs = workspace.path().join("runs");
        let command = format!("RUNS='{}'; echo run >> \"$RUNS\"; {script}", runs.display());
        let report = run_gate_command_with_supervision(
            workspace.path(),
            1,
            &command,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            supervision,
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate report");
        assert_eq!(report.status, status, "{script}");
        assert_eq!(report.flaky.is_some(), flaky, "{script}");
        assert_eq!(report.supervision_regate.is_some(), regated, "{script}");
        if let Some(first) = &report.supervision_regate {
            let expected = if supervision.is_some() {
                "wall-clock limit of 200 ms"
            } else {
                "no Lab runner to wait for"
            };
            assert!(first.summary.contains(expected), "{first:?}");
        }
        if regated && supervision.is_none() {
            let evidence = report.failure_evidence.as_ref().expect("evidence");
            assert_eq!(
                evidence.classification,
                AgentTaskGateFailureClassification::Environment
            );
            assert!(evidence.summary.contains("no Lab runner to wait for"));
        }
        if let Some(first) = &report.flaky {
            assert_eq!(first.exit_code, 1);
            assert!(
                first.stdout_tail.contains("first run lost a race"),
                "{first:?}"
            );
        }
        let ran = std::fs::read_to_string(&runs).unwrap().lines().count();
        assert_eq!(ran, expected_runs, "{script}");
    }
}

#[test]
fn gate_invocation_identity_includes_declared_adapter_argv_and_deadline() {
    let first =
        homeboy_engine_primitives::test_execution::TestExecutionPlan::declared_homeboy_review_test(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            10,
        )
        .unwrap();
    let changed_deadline =
        homeboy_engine_primitives::test_execution::TestExecutionPlan::declared_homeboy_review_test(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            11,
        )
        .unwrap();
    let changed_argv =
        homeboy_engine_primitives::test_execution::TestExecutionPlan::declared_homeboy_review_test(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
                "component".to_string(),
            ],
            10,
        )
        .unwrap();
    let first = AgentTaskGateInvocation::DeclaredTest { plan: first };
    assert_ne!(
        first.identity_digest().unwrap(),
        AgentTaskGateInvocation::DeclaredTest {
            plan: changed_deadline
        }
        .identity_digest()
        .unwrap()
    );
    assert_ne!(
        first.identity_digest().unwrap(),
        AgentTaskGateInvocation::DeclaredTest { plan: changed_argv }
            .identity_digest()
            .unwrap()
    );
}

#[test]
fn legacy_shell_invocation_digest_is_the_noninteractive_argv_and_shell_is_required() {
    let command = "cargo test --locked".to_string();
    let fresh = recorded_legacy_gate_invocation(&legacy_gate_argv(&command)).unwrap();
    assert_eq!(
        fresh.identity_digest().unwrap(),
        homeboy_engine_primitives::content_hash::nul_separated_digest([
            "sh",
            "-c",
            command.as_str(),
        ])
    );
    assert_eq!(
        serde_json::to_value(&fresh).unwrap()["shell"],
        serde_json::json!("non_interactive")
    );
    // An invocation without an explicit shell is an older record shape.
    assert!(
        serde_json::from_value::<AgentTaskGateInvocation>(serde_json::json!({
            "kind": "legacy_shell", "command": command
        }))
        .is_err()
    );
    assert!(recorded_legacy_gate_invocation(&[
        "sh".to_string(),
        "-lc".to_string(),
        command.clone(),
    ])
    .is_none());
}

#[cfg(unix)]
#[test]
fn explicit_login_shell_argv_is_executed_and_recorded_verbatim() {
    let workspace = tempfile::tempdir().unwrap();
    for shell in ["sh", "bash"] {
        let argv = vec![
            shell.to_string(),
            "-lc".to_string(),
            "printf explicitly-declared".to_string(),
        ];
        let report = run_gate_argv_with_supervision(
            workspace.path(),
            1,
            argv.clone(),
            "explicit login shell",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            None,
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(report.command, argv);
        assert_eq!(report.exit_code, 0);
        assert_eq!(report.stdout, "explicitly-declared");
        let recorded: AgentTaskGateReport =
            serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        assert_eq!(recorded.command, argv);
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "native engineering proof requires initialized Bash4+ on PATH"]
fn legacy_gate_preserves_initialized_bash_toolchain_and_bashpid() {
    let initialized = Command::new("bash")
        .args(["-c", "test -n \"$BASHPID\" && printf '%s' \"$BASH\""])
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "native proof requires initialized Bash4+"
    );
    let workspace = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let bin = workspace.path().join("bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(
        String::from_utf8(initialized.stdout).unwrap(),
        bin.join("bash"),
    )
    .unwrap();
    let mut policy = AgentTaskGateEnvironmentPolicy::default();
    policy.isolate_home = true;
    policy.isolate_xdg = true;
    policy.hydrate_rust_cache = false;
    policy.variables.insert(
        "PATH".to_string(),
        format!("{}:/usr/bin:/bin", bin.display()),
    );
    preflight_gate_toolchains(
        workspace.path(),
        &policy,
        &[AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: "bash".to_string(),
            probe_arguments: vec!["-c".to_string(), "test -n \"$BASHPID\"".to_string()],
        }],
        &[],
        Some(runtime.path()),
        Duration::from_secs(5),
    )
    .unwrap();
    fs::write(
        workspace.path().join("bash-proof.sh"),
        "test -n \"$BASHPID\" && printf '%s\\n' \"$BASH_VERSION\"\n",
    )
    .unwrap();
    let command = "set -eu; command -v bash; bash ./bash-proof.sh; sh -c 'command -v bash; bash ./bash-proof.sh'";
    let candidate = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        workspace.path(),
        1,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(runtime.path()),
        &policy,
        &[],
    )
    .unwrap();
    let baseline = run_gate_command_with_timeout(
        workspace.path(),
        1,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        runtime.path(),
        Duration::from_secs(5),
        &policy,
        &[],
    )
    .unwrap();
    for report in [&candidate, &baseline] {
        assert_eq!(report.exit_code, 0, "{}", report.stderr);
        let lines: Vec<_> = report.stdout.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], bin.join("bash").to_str().unwrap());
        assert_eq!(lines[0], lines[2]);
        assert_eq!(lines[1], lines[3]);
        eprintln!("initialized Bash gate version: {}", lines[1]);
    }
    assert_eq!(candidate.stdout, baseline.stdout);
}

#[cfg(unix)]
#[test]
fn legacy_gate_preserves_declared_path_at_readiness_candidate_and_baseline() {
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime = tempfile::tempdir().expect("runtime");
    let bin = workspace.path().join("bin");
    fs::create_dir(&bin).unwrap();
    // Shadow a system tool so login-profile PATH reordering is observable.
    let tool = bin.join("bash");
    homeboy_test_support::write_executable(&tool, "#!/bin/sh\nset -eu\ntest -d \"$HOME\"\ntest -d \"$XDG_CONFIG_HOME\"\ntest \"$HOME\" != \"$OPERATOR_HOME\"\nif test \"${1:-}\" = readiness; then printf '%s\\n' \"$0\" > \"$READINESS_RECORD\"; else printf '%s\\n' \"$0\"; fi\n", 0o755).unwrap();

    let readiness = workspace.path().join("readiness");
    let mut policy = AgentTaskGateEnvironmentPolicy::default();
    policy.isolate_home = true;
    policy.isolate_xdg = true;
    policy.hydrate_rust_cache = false;
    policy.variables.insert(
        "PATH".to_string(),
        format!("{}:/usr/bin:/bin", bin.display()),
    );
    policy.variables.insert(
        "OPERATOR_HOME".to_string(),
        std::env::var("HOME").unwrap_or_default(),
    );
    policy.variables.insert(
        "READINESS_RECORD".to_string(),
        readiness.display().to_string(),
    );
    preflight_gate_toolchains(
        workspace.path(),
        &policy,
        &[AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: "bash".to_string(),
            probe_arguments: vec!["readiness".to_string()],
        }],
        &[],
        Some(runtime.path()),
        Duration::from_secs(5),
    )
    .expect("declared readiness tool");
    let observed = fs::read_to_string(&readiness).unwrap();
    assert_eq!(observed.trim(), tool.to_str().unwrap());
    let command = "set -eu; command -v bash; bash; sh -c 'command -v bash; bash'";
    let candidate = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        workspace.path(),
        1,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(runtime.path()),
        &policy,
        &[],
    )
    .expect("candidate execution");
    let baseline = run_gate_command_with_timeout(
        workspace.path(),
        1,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        runtime.path(),
        Duration::from_secs(5),
        &policy,
        &[],
    )
    .expect("bounded baseline execution");
    for report in [&candidate, &baseline] {
        assert_eq!(report.exit_code, 0, "{}", report.stderr);
        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(report.stdout, observed.repeat(4));
        assert_eq!(report.termination, AgentTaskGateTermination::Completed);
        assert!(report
            .environment
            .sanitized
            .iter()
            .any(|variable| variable.name == "HOME"));
        assert!(report
            .environment
            .sanitized
            .iter()
            .any(|variable| variable.name == "XDG_CONFIG_HOME"));
    }
    assert_eq!(
        candidate.invocation().unwrap().identity_digest().unwrap(),
        baseline.invocation().unwrap().identity_digest().unwrap()
    );
}

/// Runs the external Action's real release-wrapper suite without editing or
/// copying its programs. Source and toolchain are explicit engineering inputs.
#[cfg(unix)]
#[test]
#[ignore = "requires an owned read-only Homeboy Action checkout and declared Bash4+ toolchain"]
fn legacy_gate_runs_unchanged_action_release_wrapper_suite() {
    let action =
        fs::canonicalize(std::env::var("HOMEBOY_TEST_ACTION_REFERENCE").expect("Action checkout"))
            .unwrap();
    let declared_path =
        std::env::var("HOMEBOY_TEST_ACTION_TOOLCHAIN_PATH").expect("declared toolchain PATH");
    let evidence = PathBuf::from(
        std::env::var("HOMEBOY_TEST_ACTION_EVIDENCE_DIR").expect("owned evidence directory"),
    );
    fs::create_dir_all(&evidence).unwrap();
    let source = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&action)
        .output()
        .unwrap();
    assert!(source.status.success());
    let source = String::from_utf8(source.stdout).unwrap().trim().to_string();
    let clean = || {
        let status = Command::new("git")
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .current_dir(&action)
            .output()
            .unwrap();
        assert!(status.status.success());
        assert!(
            status.stdout.is_empty(),
            "Action reference must stay unchanged"
        );
    };
    clean();
    let mut policy = AgentTaskGateEnvironmentPolicy::default();
    policy.mode = AgentTaskGateEnvironmentMode::Replace;
    policy.isolate_home = true;
    policy.isolate_xdg = true;
    policy.hydrate_rust_cache = false;
    policy
        .variables
        .insert("PATH".to_string(), declared_path.clone());
    let candidate_runtime = tempfile::tempdir_in(&evidence).unwrap();
    let baseline_runtime = tempfile::tempdir_in(&evidence).unwrap();
    let timeout = Duration::from_secs(120);
    for runtime in [&candidate_runtime, &baseline_runtime] {
        preflight_gate_toolchains(
            &action,
            &policy,
            &[
                AgentTaskGateToolchainRequirement {
                    resolve_only: false,
                    command: "bash".to_string(),
                    probe_arguments: vec!["-c".to_string(), "test -n \"$BASHPID\"".to_string()],
                },
                AgentTaskGateToolchainRequirement {
                    resolve_only: false,
                    command: "jq".to_string(),
                    probe_arguments: vec!["--version".to_string()],
                },
                AgentTaskGateToolchainRequirement {
                    resolve_only: false,
                    command: "python3".to_string(),
                    probe_arguments: vec!["--version".to_string()],
                },
            ],
            &[],
            Some(runtime.path()),
            Duration::from_secs(10),
        )
        .unwrap();
    }
    let supervision = GateSupervision {
        timeout,
        no_progress_timeout: timeout,
        heartbeat_interval: Duration::from_secs(1),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };
    let versions = run_gate_command_with_supervision(&action, 1,
            "set -eu; command -v bash; bash --version; jq --version; python3 --version; test -d \"$HOME\"; test -d \"$XDG_CONFIG_HOME\"; sh -c 'command -v bash; bash --version'",
            AgentTaskGateVisibility::Visible, AgentTaskGateRevealPolicy::FullEvidence,
            Some(candidate_runtime.path()), Some(&supervision), &policy, &[]).unwrap();
    fs::write(
        evidence.join("toolchain-report.json"),
        serde_json::to_vec_pretty(&versions).unwrap(),
    )
    .unwrap();
    assert_eq!(versions.exit_code, 0, "{}", versions.stderr);
    let command = "bash scripts/release/test-run-release-wrapper.sh";
    let candidate = run_gate_command_with_supervision(
        &action,
        1,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(candidate_runtime.path()),
        Some(&supervision),
        &policy,
        &[],
    )
    .unwrap();
    let baseline = run_gate_command_with_timeout(
        &action,
        1,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        baseline_runtime.path(),
        timeout,
        &policy,
        &[],
    )
    .unwrap();
    fs::write(
        evidence.join("candidate-report.json"),
        serde_json::to_vec_pretty(&candidate).unwrap(),
    )
    .unwrap();
    fs::write(
        evidence.join("baseline-report.json"),
        serde_json::to_vec_pretty(&baseline).unwrap(),
    )
    .unwrap();
    fs::write(evidence.join("source.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "action_sha": source, "declared_path": declared_path, "command": command, "timeout_seconds": timeout.as_secs()
        })).unwrap()).unwrap();
    clean();
    let mut counts = Vec::new();
    for (owner, report) in [("candidate", &candidate), ("baseline", &baseline)] {
        let passed = report
            .stdout
            .lines()
            .filter(|line| line.starts_with("PASS:"))
            .count();
        let failed = report
            .stdout
            .lines()
            .filter(|line| line.starts_with("FAIL:"))
            .count();
        eprintln!("Action {source} {owner}: exit={}, termination={:?}, PASS={passed}, FAIL={failed}\n{}\n{}", report.exit_code, report.termination, report.stdout, report.stderr);
        assert_eq!(report.exit_code, 0, "{owner} wrapper suite failed; retained reports contain bounded failure/timeout evidence");
        assert_eq!(report.termination, AgentTaskGateTermination::Completed);
        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert!(passed > 0);
        assert_eq!(failed, 0);
        assert!(report
            .stdout
            .contains("All run-release wrapper checks passed."));
        assert!(report
            .stdout
            .contains("PASS: release liveness timeout cleans descendants without mutation"));
        counts.push(passed);
    }
    assert_eq!(counts[0], counts[1]);
    assert_eq!(candidate.command, legacy_gate_argv(command));
    assert_eq!(candidate.command, baseline.command);
    assert_eq!(
        candidate.invocation().unwrap().identity_digest().unwrap(),
        baseline.invocation().unwrap().identity_digest().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn declared_baseline_executor_exports_the_plan_deadline() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = workspace.path().join("bin");
    std::fs::create_dir(&bin).expect("bin");
    let homeboy = bin.join("homeboy");
    homeboy_test_support::write_executable(
        &homeboy,
        "#!/bin/sh\nprintf %s \"$HOMEBOY_TEST_TIMEOUT_SECONDS\"\n",
        0o755,
    )
    .expect("adapter");

    let plan =
        homeboy_engine_primitives::test_execution::TestExecutionPlan::declared_homeboy_review_test(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            42,
        )
        .unwrap();
    let mut environment = AgentTaskGateEnvironmentPolicy::default();
    environment
        .variables
        .insert("PATH".to_string(), format!("{}:/bin", bin.display()));
    let runtime = tempfile::tempdir().expect("runtime");

    let report = run_declared_test_with_timeout(
        workspace.path(),
        1,
        &plan,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        runtime.path(),
        &environment,
        &[],
    )
    .expect("baseline execution");

    assert_eq!(report.stdout, "42");
    assert_eq!(
        report.test_execution_outcome,
        Some(homeboy_engine_primitives::test_execution::TestExecutionOutcome::Passed)
    );
}

#[test]
fn env_var_guard_restores_even_when_the_test_body_panics() {
    // The property that matters. An assertion failing between the mutation
    // and a hand-written restore is what turns one red test into a cascade
    // across every test scheduled after it in the same process.
    let _lock = env_mutex();
    let scratch = tempfile::tempdir().expect("scratch");
    std::env::set_var("__HOMEBOY_TEST_ENV_GUARD_PANIC__", "original");

    let panicked = std::panic::catch_unwind(|| {
        let _guard = EnvVarGuard::set(&[("__HOMEBOY_TEST_ENV_GUARD_PANIC__", scratch.path())]);
        panic!("assertion failed mid-test");
    })
    .is_err();

    assert!(panicked);
    assert_eq!(
        std::env::var("__HOMEBOY_TEST_ENV_GUARD_PANIC__")
            .ok()
            .as_deref(),
        Some("original"),
        "a panicking test must not leak its environment mutation"
    );
    std::env::remove_var("__HOMEBOY_TEST_ENV_GUARD_PANIC__");
}

#[test]
fn isolated_gate_test_leaves_home_intact() {
    // Regression: `isolated_gate_does_not_observe_ambient_...` used to end
    // with an unconditional `remove_var("HOME")`, deleting HOME for the
    // rest of the process. Roughly ninety tests in modules that sort after
    // `agent_task_gate` then failed, many with "HOME environment variable
    // not set on Unix-like system".
    let _lock = env_mutex();
    let before = std::env::var_os("HOME");

    {
        let scratch = tempfile::tempdir().expect("scratch");
        let _guard = EnvVarGuard::set(&[("HOME", scratch.path())]);
    }

    assert_eq!(
        std::env::var_os("HOME"),
        before,
        "HOME must survive a gate test that isolates it"
    );
}

/// Gate setup for `gates`, under the default hydration policy.
pub(super) fn hydrate_gate_dependency_roots(
    checkout: &Path,
    workspace: &str,
    gates: &[&str],
) -> Result<Vec<AgentTaskGateSetupEvidence>> {
    let gates = gates
        .iter()
        .map(|gate| gate.to_string())
        .collect::<Vec<_>>();
    hydrate_gate_dependency_roots_for_component(
        checkout,
        true,
        workspace,
        &homeboy_core::deps::DependencyHydrationPolicy::default(),
        None,
        Some(&gates),
    )
}

#[test]
fn dependency_walk_resolves_component_once() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        let root = checkout.path();
        fs::write(
                root.join("homeboy.json"),
                r#"{"id":"fixture-deps","deps":{"provider":"fixture","commands":{"install":{"argv":["sh","-c","printf hydrated >> hydration-count"]}}}}"#,
            )
            .expect("dependency provider");
        for index in 0..40 {
            fs::create_dir_all(root.join(format!("plain-{index}/nested")))
                .expect("plain nested directories");
        }

        let before = homeboy_core::deps::hydration_component_resolution_count();
        let evidence = hydrate_gate_dependency_roots_for_component(
            root,
            true,
            "fixture",
            &homeboy_core::deps::DependencyHydrationPolicy::default(),
            None,
            None,
        )
        .expect("dependency walk");
        assert_eq!(
            homeboy_core::deps::hydration_component_resolution_count() - before,
            1
        );
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].package_root, ".");
        assert_eq!(
            evidence[0].status,
            homeboy_core::deps::DependencyHydrationStatus::Succeeded
        );
        assert_eq!(
            fs::read_to_string(root.join("hydration-count")).unwrap(),
            "hydrated"
        );
    });
}

#[test]
fn root_declared_provider_hydrates_once_and_skips_unrelated_directories() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        let root = checkout.path();
        fs::write(
                root.join("homeboy.json"),
                r#"{"id":"fixture-deps","deps":{"provider":"fixture","commands":{"install":{"argv":["sh","-c","printf hydrated >> hydration-count"]}}}}"#,
            )
            .expect("dependency provider");
        for directory in [".claude", "docs", "packages", "scripts", "tests"] {
            fs::create_dir(root.join(directory)).expect("unrelated directory");
        }

        let evidence = hydrate_gate_dependency_roots(root, "fixture", &["cargo check"])
            .expect("dependency hydration");
        let succeeded = evidence
            .iter()
            .filter(|setup| {
                setup.status == homeboy_core::deps::DependencyHydrationStatus::Succeeded
            })
            .collect::<Vec<_>>();
        assert_eq!(succeeded.len(), 1);
        assert_eq!(succeeded[0].package_root, ".");
        assert_eq!(
            fs::read_to_string(root.join("hydration-count")).unwrap(),
            "hydrated"
        );
        assert_eq!(evidence.len(), 1);
    });
}

/// Monorepo component roots can be more than one level below the checkout.
/// Use a portable provider fixture so this exercises setup and execution
/// without requiring Composer or network access on the test host.
#[test]
fn subdirectory_component_dev_tools_are_hydrated_before_gate_execution() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        let root = checkout.path();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .status()
                .unwrap()
                .success());
        };
        git(&["init", "-q"]);
        fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        let provider = r#"{"id":"fixture-composer","deps":{"provider":"composer","commands":{"install":{"argv":["sh","-c","mkdir -p vendor/bin; printf '#!/bin/sh\\nexit 0\\n' > vendor/bin/phpcs; chmod +x vendor/bin/phpcs"]}}}}"#;
        for component in [
            "php-transformer",
            "plugins/example",
            "packages/example",
            "ignored/deep",
            "submodule/deep",
        ] {
            let path = root.join(component);
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("composer.json"),
                r#"{"require-dev":{"squizlabs/php_codesniffer":"*"}}"#,
            )
            .unwrap();
            fs::write(path.join("homeboy.json"), provider).unwrap();
        }
        fs::write(
            root.join("submodule/.git"),
            "gitdir: ../.git/modules/submodule\n",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("ignored"), root.join("linked")).unwrap();

        let evidence = hydrate_gate_dependency_roots(
            root,
            "destination",
            &[
                "cd php-transformer && composer run lint",
                "cd plugins/example && true",
                "cd packages/example && true",
                "cd ignored/deep && true",
                "cd submodule/deep && true",
            ],
        )
        .unwrap();
        assert_eq!(
            evidence
                .iter()
                .map(|step| step.package_root.as_str())
                .collect::<Vec<_>>(),
            vec!["packages/example", "php-transformer", "plugins/example"]
        );
        for step in &evidence {
            assert_eq!(
                step.status,
                homeboy_core::deps::DependencyHydrationStatus::Succeeded
            );
            let command = format!("cd {} && vendor/bin/phpcs", step.package_root);
            let report = run_gate_command(root, 1, &command).unwrap();
            assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        }
        assert!(!root.join("ignored/deep/vendor").exists());
        assert!(!root.join("submodule/deep/vendor").exists());
    });
}

/// A child directory that is its own repository (a submodule) belongs to
/// another project: the root workspace consumes its packages, and its own
/// lockfile is not this checkout's contract. Gate setup hydrates real
/// package children but never installs inside a nested repository.
#[test]
fn nested_repository_children_are_not_hydrated() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        let root = checkout.path();
        let provider = r#"{"id":"fixture-deps","deps":{"provider":"fixture","commands":{"install":{"argv":["sh","-c","printf hydrated >> hydration-count"]}}}}"#;
        for directory in ["package", "vendor"] {
            fs::create_dir(root.join(directory)).expect("child");
            fs::write(root.join(directory).join("homeboy.json"), provider).expect("child provider");
        }
        fs::write(root.join("vendor/.git"), "gitdir: ../.git/modules/vendor\n")
            .expect("submodule gitfile");

        let evidence = hydrate_gate_dependency_roots(
            root,
            "fixture",
            &["cd package && true", "cd vendor && true"],
        )
        .expect("dependency hydration");
        let roots = evidence
            .iter()
            .map(|setup| setup.package_root.as_str())
            .collect::<Vec<_>>();
        assert_eq!(roots, vec!["package"], "{evidence:?}");
        assert!(root.join("package/hydration-count").is_file());
        assert!(!root.join("vendor/hydration-count").exists());
    });
}

/// A build leaves `dist/package.json` behind (no lockfile). It is ignored
/// output, not a package root: installing there failed every promotion
/// with `npm ci` EUSAGE once the checkout had been built.
#[test]
fn git_ignored_children_are_not_hydrated() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        let root = checkout.path();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        fs::write(root.join(".gitignore"), "dist/\nbuild\n").expect("gitignore");
        let provider = r#"{"id":"fixture-deps","deps":{"provider":"fixture","commands":{"install":{"argv":["sh","-c","printf hydrated >> hydration-count"]}}}}"#;
        for directory in ["package", "dist", "build"] {
            fs::create_dir(root.join(directory)).expect("child");
            fs::write(root.join(directory).join("homeboy.json"), provider).expect("child provider");
        }

        let evidence = hydrate_gate_dependency_roots(
            root,
            "fixture",
            &["cd package && true", "cd dist && true", "cd build && true"],
        )
        .expect("dependency hydration");
        let roots = evidence
            .iter()
            .map(|setup| setup.package_root.as_str())
            .collect::<Vec<_>>();
        assert_eq!(roots, vec!["package"], "{evidence:?}");
        assert!(!root.join("dist/hydration-count").exists());
        assert!(!root.join("build/hydration-count").exists());
    });
}

/// Ignore rules are matched against each child's checkout-relative path
/// (#16857): a nested `.gitignore` prunes the directories beneath it, and a
/// root-anchored pattern does not prune a same-named nested directory.
#[test]
fn nested_gitignore_rules_apply_to_their_own_subtree() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        let root = checkout.path();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "-q"])
            .status()
            .expect("git");
        assert!(status.success());
        fs::write(root.join(".gitignore"), "/build/\n").expect("root gitignore");
        fs::create_dir_all(root.join("extensions")).expect("extensions");
        fs::write(root.join("extensions/.gitignore"), "**/node_modules/\n")
            .expect("nested gitignore");
        let provider = r#"{"id":"fixture-deps","deps":{"provider":"fixture","commands":{"install":{"argv":["sh","-c","printf hydrated >> hydration-count"]}}}}"#;
        for directory in [
            "build",
            "extensions/tool/build",
            "extensions/tool/node_modules/dep",
        ] {
            fs::create_dir_all(root.join(directory)).expect("child");
            fs::write(root.join(directory).join("homeboy.json"), provider).expect("child provider");
        }

        let evidence = hydrate_gate_dependency_roots(
            root,
            "fixture",
            &[
                "cd build && true",
                "cd extensions/tool/build && true",
                "cd extensions/tool/node_modules/dep && true",
            ],
        )
        .expect("dependency hydration");
        let roots = evidence
            .iter()
            .map(|setup| setup.package_root.as_str())
            .collect::<Vec<_>>();
        assert_eq!(roots, vec!["extensions/tool/build"], "{evidence:?}");
        assert!(!root.join("build/hydration-count").exists());
        assert!(!root
            .join("extensions/tool/node_modules/dep/hydration-count")
            .exists());
    });
}

/// Built-in npm defaults apply only at the checkout, not to fixture inputs
/// or workspace members without their own selection files (#16670).
#[test]
fn builtin_adapter_defaults_do_not_declare_descendant_roots() {
    let cases = [
        (
            "fixture",
            Some("package-lock.json"),
            Some("fixtures/websites/46-mdx-design-system"),
            false,
            false,
        ),
        (
            "workspace",
            Some("npm-shrinkwrap.json"),
            Some("packages/cli"),
            false,
            true,
        ),
        ("root default", None, None, false, false),
        (
            "nested lockfile",
            Some("package-lock.json"),
            Some("packages/independent"),
            true,
            false,
        ),
    ];
    for (description, root_lock, descendant, nested_lock, workspace) in cases {
        homeboy_core::test_support::with_isolated_home(|home| {
            let checkout = tempfile::tempdir().expect("checkout");
            let root = checkout.path();
            let manifest = serde_json::json!({
                "name": "fixture",
                "dependencies": {"example": "1.0.0"},
                "workspaces": if workspace { vec!["packages/*"] } else { vec![] },
            });
            fs::write(root.join("package.json"), manifest.to_string()).unwrap();
            if let Some(lock) = root_lock {
                fs::write(root.join(lock), "{}").unwrap();
            }
            if let Some(directory) = descendant {
                let child = root.join(directory);
                fs::create_dir_all(&child).unwrap();
                fs::write(child.join("package.json"), manifest.to_string()).unwrap();
                if nested_lock {
                    fs::write(child.join("package-lock.json"), "{}").unwrap();
                }
            }
            // The built-in Node adapter contract from deps.rs. Stub its
            // install commands, not its selection, so no real npm runs.
            let adapters = home
                .path()
                .join(".config/homeboy/extensions/dependency-adapters");
            fs::create_dir_all(&adapters).unwrap();
            fs::write(adapters.join("index.json"), r#"{"schema":"homeboy-extension/dependency-adapter-index/v1","manifests":[{"id":"nodejs-package-managers","ecosystem":"nodejs","path":"nodejs.json"}]}"#).unwrap();
            let managers = [
                    ("pnpm", "pnpm-lock.yaml", false),
                    ("yarn", "yarn.lock", false),
                    ("npm", "package-lock.json", true),
                ].into_iter().enumerate().map(|(priority, (id, lock, default))| serde_json::json!({
                    "id": id,
                    "selection": {"priority": priority + 1, "files": [lock], "default": default},
                    "commands": {"install": {"command": "mkdir -p node_modules; printf installed > node_modules/installed; printf installed >> hydration-count"}},
                    "package_identity": {"manifest": "package.json", "name": "name", "dependencies": ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"]},
                    "outputs": [{"path": "node_modules", "kind": "directory"}],
                })).collect::<Vec<_>>();
            fs::write(
                adapters.join("nodejs.json"),
                serde_json::json!({
                    "schema": "homeboy-extension/dependency-adapter-manifest/v1",
                    "id": "nodejs-package-managers", "version": 1, "ecosystem": "nodejs",
                    "project_signals": {"root_files": ["package.json"]},
                    "package_managers": managers,
                })
                .to_string(),
            )
            .unwrap();

            let gate = descendant
                .map(|directory| format!("cd {directory} && true"))
                .unwrap_or_else(|| "true".to_string());
            let evidence = hydrate_gate_dependency_roots(root, "fixture", &[&gate]).unwrap();
            let mut expected = vec!["."];
            if nested_lock {
                expected.push(descendant.unwrap());
            }
            assert_eq!(
                evidence
                    .iter()
                    .map(|step| step.package_root.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "{description}: {evidence:?}"
            );
            for step in &evidence {
                assert_eq!(
                    step.status,
                    homeboy_core::deps::DependencyHydrationStatus::Succeeded,
                    "{description}: {step:?}"
                );
                assert_eq!(
                    fs::read_to_string(root.join(&step.package_root).join("hydration-count"))
                        .unwrap(),
                    "installed"
                );
            }
            if let Some(directory) = descendant {
                assert_eq!(
                    root.join(directory).join("hydration-count").exists(),
                    nested_lock,
                    "{description}"
                );
            }
        });
    }
}

/// A dependency adapter whose selection searches upward binds a workspace
/// member to the ancestor root carrying the lockfile, so the member is not
/// a dependency root of its own: hydrating it reran the ancestor's install
/// from inside the member, where the root's setup does not hold (#16692).
/// Each selection root hydrates once at that root; a nested directory
/// carrying its own lockfile is a distinct root.
#[test]
fn upward_selection_hydrates_each_selection_root_once_at_its_own_root() {
    struct Case {
        description: &'static str,
        nested_lockfile: bool,
        expected_roots: Vec<&'static str>,
    }
    let cases = [
        Case {
            description:
                "workspace member with only a project signal installs at the hydrated root",
            nested_lockfile: false,
            expected_roots: vec!["."],
        },
        Case {
            description: "nested directory with its own lockfile is its own selection root",
            nested_lockfile: true,
            expected_roots: vec![".", "tools/generator"],
        },
    ];
    for case in cases {
        homeboy_core::test_support::with_isolated_home(|home| {
            let checkout = tempfile::tempdir().expect("checkout");
            let root = checkout.path();
            fs::write(root.join("project.json"), r#"{"name":"workspace"}"#)
                .expect("root project signal");
            fs::write(root.join("workspace.lock"), "").expect("root lockfile");
            let member = root.join("apps/cli");
            fs::create_dir_all(&member).expect("workspace member");
            fs::write(member.join("project.json"), r#"{"name":"cli"}"#)
                .expect("member project signal");
            if case.nested_lockfile {
                let nested = root.join("tools/generator");
                fs::create_dir_all(&nested).expect("nested root");
                fs::write(nested.join("project.json"), r#"{"name":"generator"}"#)
                    .expect("nested project signal");
                fs::write(nested.join("workspace.lock"), "").expect("nested lockfile");
            }
            let adapters = home
                .path()
                .join(".config/homeboy/extensions/dependency-adapters");
            fs::create_dir_all(&adapters).expect("adapter directory");
            fs::write(
                    adapters.join("index.json"),
                    r#"{"schema":"homeboy-extension/dependency-adapter-index/v1","manifests":[{"id":"fixture","ecosystem":"fixture","path":"fixture.json"}]}"#,
                )
                .expect("adapter index");
            fs::write(
                    adapters.join("fixture.json"),
                    r#"{"schema":"homeboy-extension/dependency-adapter-manifest/v1","id":"fixture","version":1,"ecosystem":"fixture","project_signals":{"root_files":["project.json"]},"package_managers":[{"id":"fixture","selection":{"priority":1,"files":["workspace.lock"],"search":"upward"},"commands":{"install":{"command":"mkdir -p deps && printf installed > deps/installed"}},"outputs":[{"path":"deps","kind":"directory"}]}]}"#,
                )
                .expect("adapter manifest");

            let evidence = hydrate_gate_dependency_roots(
                root,
                "fixture",
                &["cd apps/cli && true", "cd tools/generator && true"],
            )
            .expect("dependency hydration");
            let roots = evidence
                .iter()
                .map(|setup| setup.package_root.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                roots, case.expected_roots,
                "{}: {evidence:?}",
                case.description
            );
            for setup in &evidence {
                assert_eq!(
                    setup.status,
                    homeboy_core::deps::DependencyHydrationStatus::Succeeded,
                    "{}: {setup:?}",
                    case.description
                );
            }
            // The install ran at each hydrated selection root and nowhere else.
            assert!(
                root.join("deps/installed").is_file(),
                "{}",
                case.description
            );
            assert!(
                !member.join("deps/installed").exists(),
                "{}",
                case.description
            );
            assert_eq!(
                root.join("tools/generator/deps/installed").is_file(),
                case.nested_lockfile,
                "{}",
                case.description
            );
        });
    }
}

/// Gate setup hydrates the checkout root and only the descendant roots a
/// gate names (#16857). A gate inside a member of a descendant root reaches
/// that root through the member's upward selection (#16109).
#[test]
fn gate_setup_hydrates_only_descendant_roots_a_gate_names() {
    let cases: [(&str, &[&str]); 3] = [
        ("cargo check", &["."]),
        ("cd tool && npm test", &[".", "tool"]),
        ("cd tool/sub && x", &[".", "tool"]),
    ];
    for (gate, expected_roots) in cases {
        homeboy_core::test_support::with_isolated_home(|home| {
            let checkout = tempfile::tempdir().expect("checkout");
            let root = checkout.path();
            fs::create_dir_all(root.join("tool/sub")).expect("member");
            for (directory, lockfile) in [(".", true), ("tool", true), ("tool/sub", false)] {
                let directory = root.join(directory);
                fs::write(directory.join("project.json"), "{}").expect("project signal");
                if lockfile {
                    fs::write(directory.join("workspace.lock"), "").expect("lockfile");
                }
            }
            let adapters = home
                .path()
                .join(".config/homeboy/extensions/dependency-adapters");
            fs::create_dir_all(&adapters).expect("adapter directory");
            fs::write(
                    adapters.join("index.json"),
                    r#"{"schema":"homeboy-extension/dependency-adapter-index/v1","manifests":[{"id":"fixture","ecosystem":"fixture","path":"fixture.json"}]}"#,
                )
                .expect("adapter index");
            fs::write(
                    adapters.join("fixture.json"),
                    r#"{"schema":"homeboy-extension/dependency-adapter-manifest/v1","id":"fixture","version":1,"ecosystem":"fixture","project_signals":{"root_files":["project.json"]},"package_managers":[{"id":"fixture","selection":{"priority":1,"files":["workspace.lock"],"search":"upward"},"commands":{"install":{"command":"mkdir -p deps && printf installed > deps/installed"}},"outputs":[{"path":"deps","kind":"directory"}]}]}"#,
                )
                .expect("adapter manifest");

            let evidence = hydrate_gate_dependency_roots(root, "fixture", &[gate])
                .expect("dependency hydration");
            let roots = evidence
                .iter()
                .map(|setup| setup.package_root.as_str())
                .collect::<Vec<_>>();
            assert_eq!(roots, expected_roots, "{gate}: {evidence:?}");
            assert_eq!(
                root.join("tool/deps/installed").is_file(),
                expected_roots.contains(&"tool"),
                "{gate}"
            );
            assert!(!root.join("tool/sub/deps").exists(), "{gate}");
        });
    }
}

#[test]
fn gate_dependency_hydration_records_one_phase_for_multiple_candidates() {
    let timing_dir = tempfile::tempdir().unwrap();
    let timing_store = homeboy_core::observation::ObservationStore::open_initialized_at(
        timing_dir.path().join("timing.db"),
    )
    .unwrap();
    let timing_run = timing_store
        .start_run(homeboy_core::observation::NewRunRecord::builder("test").build())
        .unwrap();
    let _timing = homeboy_core::observation::phase_timing::for_run(&timing_store, &timing_run.id);

    homeboy_core::test_support::with_isolated_home(|_| {
        let checkout = tempfile::tempdir().unwrap();
        fs::create_dir_all(checkout.path().join("first/nested")).unwrap();
        fs::create_dir_all(checkout.path().join("second")).unwrap();

        let outcomes = hydrate_gate_dependency_roots_for_component(
            checkout.path(),
            true,
            "fixture",
            &homeboy_core::deps::DependencyHydrationPolicy::default(),
            None,
            None,
        )
        .unwrap();
        assert!(outcomes.is_empty());

        let spans = timing_store.phase_spans(&timing_run.id).unwrap().spans;
        assert_eq!(
            spans
                .iter()
                .filter(|span| span.id == "stage.dependency_hydration")
                .count(),
            1
        );
    });
}

#[test]
fn isolated_gate_setup_maps_declared_download_auth_without_exposing_secrets() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        fs::write(root.join("project.json"), "{}").unwrap();
        let adapters = home
            .path()
            .join(".config/homeboy/extensions/dependency-adapters");
        fs::create_dir_all(&adapters).unwrap();
        fs::write(adapters.join("index.json"), r#"{"schema":"homeboy-extension/dependency-adapter-index/v1","manifests":[{"id":"fixture","ecosystem":"fixture","path":"fixture.json"}]}"#).unwrap();
        let secret = "fixture-download-credential-unique";
        let source = home.path().join("credentials.json");
        // Shaped like a real auth.json: structural lines must not become redaction terms.
        let content =
            format!("{{\n  \"github-oauth\": {{\n    \"github.com\": \"{secret}\"\n  }}\n}}\n");
        fs::write(&source, &content).unwrap();
        for (mode, file, explicit, isolated, succeeds) in [
            ("content", "credentials.json", None, true, true),
            ("path", "credentials.json", None, true, true),
            (
                "content",
                "credentials.json",
                Some("operator-download-credential"),
                true,
                true,
            ),
            ("content", "absent.json", None, true, true),
            ("content", "credentials.json", None, false, true),
            ("content", "credentials.json", None, true, false),
        ] {
            let command = format!(
                r#"printf 'ordinary {{output}}\n'; printf '%s' "${{FIXTURE_DOWNLOAD_AUTH:+present}}" > observed-present; printf '%s' "${{#FIXTURE_DOWNLOAD_AUTH}}" > observed-length; if [ '{mode}' = path ] && [ -n "$FIXTURE_DOWNLOAD_AUTH" ]; then cat "$FIXTURE_DOWNLOAD_AUTH"; else printf '%s' "$FIXTURE_DOWNLOAD_AUTH"; fi; printf '\nHOMEBOY_PROGRESS {{"phase":"install","current":"%s"}}\n' "$FIXTURE_DOWNLOAD_AUTH"; sleep 0.05; exit {}"#,
                if succeeds { 0 } else { 1 }
            );
            let manifest = serde_json::json!({
                "schema":"homeboy-extension/dependency-adapter-manifest/v1", "id":"fixture", "version":1,
                "ecosystem":"fixture", "project_signals":{"root_files":["project.json"]},
                "package_managers":[{"id":"fixture", "download_auth":{"env":"FIXTURE_DOWNLOAD_AUTH","file":file,"as":mode},
                    "selection":{"priority":1,"files":["project.json"],"search":"upward"},
                    "commands":{"install":{"command":command}}}]
            });
            fs::write(adapters.join("fixture.json"), manifest.to_string()).unwrap();
            let mut gate_policy = AgentTaskGateEnvironmentPolicy::default();
            if let Some(value) = explicit {
                gate_policy
                    .variables
                    .insert("FIXTURE_DOWNLOAD_AUTH".into(), value.into());
            }
            let selected = selected_gate_environment(&gate_policy, None).unwrap();
            let mut environment = selected.hydration_environment();
            environment.isolate_download_cache = isolated;
            let progress = Arc::new(Mutex::new(Vec::new()));
            let policy = homeboy_core::deps::DependencyHydrationPolicy {
                environment,
                heartbeat_interval: Duration::from_millis(10),
                on_progress: Arc::new({
                    let progress = Arc::clone(&progress);
                    move |event| progress.lock().unwrap().push(format!("{event:?}"))
                }),
                ..Default::default()
            };
            let outcomes = hydrate_gate_dependency_roots_for_component(
                root, true, "fixture", &policy, None, None,
            )
            .unwrap();
            assert_eq!(outcomes.len(), 1);
            assert_eq!(
                outcomes[0].status,
                if succeeds {
                    homeboy_core::deps::DependencyHydrationStatus::Succeeded
                } else {
                    homeboy_core::deps::DependencyHydrationStatus::Failed
                }
            );
            let expected = if !isolated || file == "absent.json" {
                String::new()
            } else if let Some(value) = explicit {
                value.to_string()
            } else if mode == "path" {
                source.display().to_string()
            } else {
                content.clone()
            };
            assert_eq!(
                fs::read_to_string(root.join("observed-length")).unwrap(),
                expected.len().to_string()
            );
            assert_eq!(
                fs::read_to_string(root.join("observed-present")).unwrap(),
                if expected.is_empty() { "" } else { "present" }
            );
            if isolated {
                assert_eq!(
                    outcomes[0].download_auth["FIXTURE_DOWNLOAD_AUTH"],
                    if file == "absent.json" {
                        "unavailable".to_string()
                    } else if explicit.is_some() {
                        "operator_override".to_string()
                    } else {
                        source.display().to_string()
                    }
                );
            } else {
                assert!(outcomes[0].download_auth.is_empty());
            }
            if !succeeds {
                // Failed outcomes keep output; redaction must leave ordinary text intact.
                assert!(outcomes[0].stdout.contains("ordinary {output}"));
            }
            let evidence = serde_json::to_string(&outcomes).unwrap();
            let progress = progress.lock().unwrap().join("\n");
            for value in [secret, content.as_str(), "operator-download-credential"] {
                assert!(!evidence.contains(value), "secret in outcome");
                assert!(!progress.contains(value), "secret in progress");
            }
            assert!(!Path::new(selected.values.get("HOME").unwrap())
                .join("credentials.json")
                .exists());
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, home.path().join("linked.json")).unwrap();
        for file in [
            "/absolute.json",
            "../credentials.json",
            ".config",
            "linked.json",
        ] {
            if file == "linked.json" && !cfg!(unix) {
                continue;
            }
            let manifest = serde_json::json!({
                "schema":"homeboy-extension/dependency-adapter-manifest/v1", "id":"fixture", "version":1,
                "ecosystem":"fixture", "project_signals":{"root_files":["project.json"]},
                "package_managers":[{"id":"fixture", "download_auth":{"env":"FIXTURE_DOWNLOAD_AUTH","file":file,"as":"content"},
                    "selection":{"priority":1,"files":["project.json"],"search":"upward"},
                    "commands":{"install":{"command":"touch unsafe-install-ran"}}}]
            });
            fs::write(adapters.join("fixture.json"), manifest.to_string()).unwrap();
            let selected =
                selected_gate_environment(&AgentTaskGateEnvironmentPolicy::default(), None)
                    .unwrap();
            let policy = homeboy_core::deps::DependencyHydrationPolicy {
                environment: selected.hydration_environment(),
                ..Default::default()
            };
            let error = hydrate_gate_dependency_roots_for_component(
                root, true, "fixture", &policy, None, None,
            )
            .unwrap_err();
            assert!(!format!("{error:?}").contains(secret));
            assert!(!root.join("unsafe-install-ran").exists());
        }
    });
}

#[test]
fn isolated_gate_setup_shares_declared_download_cache_and_honors_override() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let checkout = tempfile::tempdir().unwrap();
        let root = checkout.path();
        fs::write(root.join("project.json"), "{}").unwrap();
        let adapters = home
            .path()
            .join(".config/homeboy/extensions/dependency-adapters");
        fs::create_dir_all(&adapters).unwrap();
        fs::write(adapters.join("index.json"), r#"{"schema":"homeboy-extension/dependency-adapter-index/v1","manifests":[{"id":"fixture","ecosystem":"fixture","path":"fixture.json"}]}"#).unwrap();
        fs::write(adapters.join("fixture.json"), r#"{"schema":"homeboy-extension/dependency-adapter-manifest/v1","id":"fixture","version":1,"ecosystem":"fixture","project_signals":{"root_files":["project.json"]},"package_managers":[{"id":"fixture","download_cache":{"env":"FIXTURE_DOWNLOAD_CACHE"},"selection":{"priority":1,"files":["project.json"],"search":"upward"},"commands":{"install":{"command":"printf '%s' \"$FIXTURE_DOWNLOAD_CACHE\" > observed-cache; printf '%s' \"$HOME\" > observed-home"}}}]}"#).unwrap();
        let expected = homeboy_core::paths::homeboy_data()
            .unwrap()
            .join("controller-state/dependency-download-cache/fixture");
        for explicit in [None, None, Some("/operator/cache")] {
            let mut gate_policy = AgentTaskGateEnvironmentPolicy::default();
            if let Some(value) = explicit {
                gate_policy
                    .variables
                    .insert("FIXTURE_DOWNLOAD_CACHE".to_string(), value.to_string());
            }
            let selected = selected_gate_environment(&gate_policy, None).unwrap();
            let home = selected.values.get("HOME").unwrap();
            let policy = homeboy_core::deps::DependencyHydrationPolicy {
                environment: selected.hydration_environment(),
                ..Default::default()
            };
            let outcomes = hydrate_gate_dependency_roots_for_component(
                root, true, "fixture", &policy, None, None,
            )
            .unwrap();
            assert_eq!(outcomes.len(), 1);
            assert_eq!(
                outcomes[0].status,
                homeboy_core::deps::DependencyHydrationStatus::Succeeded
            );
            let expected = explicit
                .map(str::to_string)
                .unwrap_or_else(|| expected.display().to_string());
            assert_eq!(
                fs::read_to_string(root.join("observed-cache")).unwrap(),
                expected
            );
            assert_eq!(
                outcomes[0].download_cache["FIXTURE_DOWNLOAD_CACHE"],
                expected
            );
            assert_eq!(
                fs::read_to_string(root.join("observed-home")).unwrap(),
                *home
            );
        }
        assert!(expected.is_dir());
    });
}

#[cfg(unix)]
#[test]
fn failed_durable_spawn_registration_reaps_the_isolated_gate_group() {
    let worktree = tempfile::tempdir().expect("worktree");
    let child_pid = Arc::new(Mutex::new(None));
    let supervision = GateSupervision {
        timeout: Duration::from_secs(3),
        no_progress_timeout: Duration::from_secs(1),
        heartbeat_interval: Duration::from_millis(10),
        on_spawn: Arc::new({
            let child_pid = Arc::clone(&child_pid);
            move |pid, _| {
                *child_pid.lock().expect("child pid") = Some(pid);
                Err(Error::internal_unexpected("durable write failed"))
            }
        }),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };
    assert!(run_gate_command_with_supervision(
        worktree.path(),
        1,
        "sleep 30",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(&supervision),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .is_err());
    let pid = child_pid
        .lock()
        .expect("child pid")
        .expect("durable registration received child pid");
    assert!(!homeboy_core::process::pid_is_running(pid));
}

#[cfg(unix)]
#[test]
fn supervised_gate_heartbeats_with_a_bounded_live_output_tail() {
    let worktree = tempfile::tempdir().expect("worktree");
    let tails = Arc::new(Mutex::new(Vec::new()));
    let supervision = GateSupervision {
        timeout: Duration::from_secs(1),
        no_progress_timeout: Duration::from_secs(1),
        heartbeat_interval: Duration::from_millis(10),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new({
            let tails = Arc::clone(&tails);
            move |status| {
                tails
                    .lock()
                    .expect("tails")
                    .push(status.output_tail.clone());
                Ok(())
            }
        }),
        is_cancelled: Arc::new(|| false),
    };
    run_gate_command_with_supervision(
        worktree.path(),
        1,
        "printf stdout; printf stderr >&2; sleep 0.5",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(&supervision),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .expect("gate");
    assert!(tails
        .lock()
        .expect("tails")
        .iter()
        .any(|tail| tail.contains("stdout") && tail.contains("stderr")));
}

#[cfg(unix)]
#[test]
fn supervised_private_gate_never_persists_live_output() {
    let worktree = tempfile::tempdir().expect("worktree");
    let tails = Arc::new(Mutex::new(Vec::new()));
    let supervision = GateSupervision {
        timeout: Duration::from_secs(1),
        no_progress_timeout: Duration::from_secs(1),
        heartbeat_interval: Duration::from_millis(10),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new({
            let tails = Arc::clone(&tails);
            move |status| {
                tails
                    .lock()
                    .expect("tails")
                    .push(status.output_tail.clone());
                Ok(())
            }
        }),
        is_cancelled: Arc::new(|| false),
    };
    run_gate_command_with_supervision(
        worktree.path(),
        1,
        "printf secret-stdout; printf secret-stderr >&2; sleep 0.5",
        AgentTaskGateVisibility::Private,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(&supervision),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .expect("private gate");
    let tails = tails.lock().expect("tails");
    assert!(!tails.is_empty());
    assert!(tails
        .iter()
        .all(|tail| tail == "private gate output withheld"));
}

#[cfg(unix)]
#[test]
fn gate_failure_evidence_retains_last_phase_and_exit_reason() {
    let worktree = tempfile::tempdir().expect("worktree");
    for command in [
            "yes first-phase | head -c 100000; printf '\\n'; printf '%6000s' x; printf '\\n%s\\n' second-phase-marker; exit 3",
            "kill -TERM $$",
        ] {
            let report = run_gate_command_with_supervision(
                worktree.path(),
                1,
                command,
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                None,
                None,
                &AgentTaskGateEnvironmentPolicy::default(),
                &[],
            )
            .expect("gate");
            let evidence = failure_excerpt::gate_failure_excerpt(&report).expect("failure evidence");
            if command.starts_with("yes") {
                assert!(evidence.contains("second-phase-marker"), "{evidence}");
                assert!(failure_excerpt::failure_output_excerpt(&report.stdout, &report.stderr)
                    .contains("second-phase-marker"));
                assert!(evidence.contains("exit code: 3"), "{evidence}");
                assert_eq!(serde_json::to_value(&report).unwrap()["exit_code"], 3);
            } else {
                assert!(evidence.contains("signal: 15"), "{evidence}");
            }
        }
}

#[test]
fn gate_retains_bounded_tails_with_full_stream_metadata() {
    let worktree = tempfile::tempdir().expect("worktree");
    let report = run_gate_command_with_supervision(
        worktree.path(),
        1,
        "yes x | head -c 100000; yes y | head -c 100000 >&2",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        None,
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .expect("gate");
    assert!(report.capture.stdout.truncated);
    assert!(report.capture.stderr.truncated);
    assert_eq!(report.capture.stdout.bytes_seen, 100_000);
    assert_eq!(report.capture.stdout.bytes_retained, 65_536);
    assert_eq!(report.capture.stdout.bytes_truncated, 34_464);
    assert!(report.capture.stdout.sha256.starts_with("sha256:"));
    assert!(report.stdout.len() <= 65_536);
}

#[cfg(unix)]
#[test]
fn silent_gate_stall_is_not_a_semantic_command_failure() {
    let worktree = tempfile::tempdir().expect("worktree");
    let supervision = GateSupervision {
        timeout: Duration::from_secs(1),
        no_progress_timeout: Duration::from_millis(30),
        heartbeat_interval: Duration::from_millis(10),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };
    let report = run_gate_command_with_supervision(
        worktree.path(),
        1,
        "sleep 1",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(&supervision),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .expect("gate report");
    assert_eq!(report.termination, AgentTaskGateTermination::NoProgress);
    assert_eq!(report.exit_code, 125);
    let evidence = report.failure_evidence.as_ref().expect("failure evidence");
    assert_eq!(
        serde_json::to_value(evidence.classification).unwrap(),
        "execution_budget"
    );
    // The stop was re-gated once under the same budgets; the first run's
    // evidence is kept alongside, and the repeat is unavailable
    // verification rather than a candidate failure (#18554).
    assert!(evidence.summary.contains("no output for 30 ms"));
    assert!(evidence.summary.contains("no_progress"));
    let first = report.supervision_regate.as_ref().expect("first run");
    assert!(first.summary.contains("no output for 30 ms"));
    assert_eq!(report.status, AgentTaskGateStatus::Unavailable);
}

#[cfg(unix)]
#[test]
fn progress_markers_keep_an_active_gate_alive_and_are_persisted() {
    let worktree = tempfile::tempdir().expect("worktree");
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let supervision = GateSupervision {
        timeout: Duration::from_secs(3),
        no_progress_timeout: Duration::from_millis(200),
        heartbeat_interval: Duration::from_millis(10),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new({
            let statuses = Arc::clone(&statuses);
            move |status| {
                statuses.lock().expect("statuses").push(status.clone());
                Ok(())
            }
        }),
        is_cancelled: Arc::new(|| false),
    };
    let report = run_gate_command_with_supervision(
            worktree.path(), 1,
            "for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do printf 'HOMEBOY_PROGRESS {\"phase\":\"test\",\"current\":\"%s\"}\\n' \"$i\"; sleep 0.02; done",
            AgentTaskGateVisibility::Visible, AgentTaskGateRevealPolicy::FullEvidence,
            None, Some(&supervision), &AgentTaskGateEnvironmentPolicy::default(), &[],
        ).expect("gate report");
    assert_eq!(report.termination, AgentTaskGateTermination::Completed);
    assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
    assert!(statuses.lock().expect("statuses").iter().any(|status| {
        status
            .progress
            .as_ref()
            .is_some_and(|progress| progress.phase == "test")
            && status.last_progress_ms_ago.is_some()
    }));
}

#[cfg(unix)]
pub(super) fn chatty_gate_supervision() -> GateSupervision {
    GateSupervision {
        timeout: Duration::from_secs(5),
        no_progress_timeout: Duration::from_millis(250),
        heartbeat_interval: Duration::from_millis(10),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    }
}

/// An uninstrumented gate (any third-party test runner) is alive while it
/// prints, even when it runs far longer than the no-progress window.
#[cfg(unix)]
#[test]
fn uninstrumented_gate_output_keeps_it_alive() {
    let worktree = tempfile::tempdir().expect("worktree");
    let report = run_gate_command_with_supervision(
        worktree.path(),
        1,
        "for i in $(seq 1 40); do echo \"case $i passed\"; sleep 0.02; done",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(&chatty_gate_supervision()),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .expect("gate report");
    assert_eq!(report.termination, AgentTaskGateTermination::Completed);
    assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
}

/// Once a gate emits a marker it is instrumented: ordinary output can no
/// longer mask a stalled test case (#11415).
#[cfg(unix)]
#[test]
fn instrumented_gate_output_cannot_mask_a_missing_marker() {
    let worktree = tempfile::tempdir().expect("worktree");
    let report = run_gate_command_with_supervision(
            worktree.path(),
            1,
            "printf 'HOMEBOY_PROGRESS {\"phase\":\"test\"}\\n'; for i in $(seq 1 100); do echo \"line $i\"; sleep 0.02; done",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&chatty_gate_supervision()),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate report");
    assert_eq!(report.termination, AgentTaskGateTermination::NoProgress);
    let evidence = report.failure_evidence.as_ref().expect("failure evidence");
    assert!(
        evidence
            .summary
            .contains("no HOMEBOY_PROGRESS marker for 250 ms"),
        "{}",
        evidence.summary
    );
    let first = report.supervision_regate.as_ref().expect("first run");
    assert!(
        first
            .summary
            .contains("no HOMEBOY_PROGRESS marker for 250 ms"),
        "{}",
        first.summary
    );
}

/// A marker with `"liveness":"output"` hands a delegated phase (a cargo
/// build, an extension test runner) back to output liveness, so an
/// instrumented command can run an uninstrumented child (#16152).
#[cfg(unix)]
#[test]
fn output_liveness_marker_keeps_a_delegated_phase_alive() {
    let worktree = tempfile::tempdir().expect("worktree");
    let report = run_gate_command_with_supervision(
            worktree.path(),
            1,
            // The exact setup markers `homeboy review` emits.
            concat!(
                "printf '%s\\n' 'HOMEBOY_PROGRESS {\"component\":\"c\",\"current\":\"c\",\"deadline_ms\":1,\"elapsed_ms\":0,\"last_progress_ms_ago\":null,\"liveness\":null,\"phase\":\"dependency_setup\",\"provider\":null,\"status\":\"running\"}'; ",
                "printf '%s\\n' 'HOMEBOY_PROGRESS {\"component\":\"c\",\"current\":\"c\",\"deadline_ms\":1,\"elapsed_ms\":1,\"last_progress_ms_ago\":null,\"liveness\":\"output\",\"phase\":\"dependency_setup\",\"provider\":null,\"status\":\"completed\"}'; ",
                "for i in $(seq 1 40); do echo \"case $i passed\"; sleep 0.02; done",
            ),
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&chatty_gate_supervision()),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate report");
    assert_eq!(report.termination, AgentTaskGateTermination::Completed);
    assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
}

/// The next plain marker restores marker-only liveness after a handoff.
#[cfg(unix)]
#[test]
fn plain_marker_after_output_handoff_restores_marker_only_liveness() {
    let worktree = tempfile::tempdir().expect("worktree");
    let report = run_gate_command_with_supervision(
            worktree.path(),
            1,
            "printf 'HOMEBOY_PROGRESS {\"phase\":\"build\",\"liveness\":\"output\"}\\n'; printf 'HOMEBOY_PROGRESS {\"phase\":\"test\"}\\n'; for i in $(seq 1 100); do echo \"line $i\"; sleep 0.02; done",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&chatty_gate_supervision()),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate report");
    assert_eq!(report.termination, AgentTaskGateTermination::NoProgress);
    let evidence = report.failure_evidence.as_ref().expect("failure evidence");
    assert!(
        evidence
            .summary
            .contains("no HOMEBOY_PROGRESS marker for 250 ms"),
        "{}",
        evidence.summary
    );
    let first = report.supervision_regate.as_ref().expect("first run");
    assert!(
        first
            .summary
            .contains("no HOMEBOY_PROGRESS marker for 250 ms"),
        "{}",
        first.summary
    );
}

#[test]
fn semantic_failure_remains_completed_with_reviewer_resolvable_command() {
    let worktree = tempfile::tempdir().expect("worktree");
    let report = run_gate_command_with_supervision(
        worktree.path(),
        1,
        "printf failure >&2; exit 7",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        None,
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .expect("gate report");
    assert_eq!(report.termination, AgentTaskGateTermination::Completed);
    assert_eq!(report.exit_code, 7);
    assert_eq!(
        report.failure_evidence.as_ref().expect("evidence").command,
        "printf failure >&2; exit 7"
    );
}

#[test]
fn command_owned_budget_exit_codes_remain_candidate_failures() {
    let worktree = tempfile::tempdir().expect("worktree");
    for code in [124, 125] {
        let report = run_gate_command_with_supervision(
            worktree.path(),
            1,
            &format!("exit {code}"),
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            None,
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("command report");
        assert_eq!(report.termination, AgentTaskGateTermination::Completed);
        assert_eq!(
            report.failure_evidence.as_ref().unwrap().classification,
            AgentTaskGateFailureClassification::CandidateCode
        );
    }
}

/// A ptrace exit-stop makes cleanup genuinely incomplete, rather than merely
/// slow: even SIGKILL cannot reap the detached child until the tracer resumes it.
#[cfg(target_os = "linux")]
#[test]
fn completed_gate_keeps_verdict_when_process_tree_cleanup_is_incomplete() {
    for (code, expected) in [
        (0, AgentTaskGateStatus::Succeeded),
        (1, AgentTaskGateStatus::Failed),
    ] {
        let workspace = tempfile::tempdir().expect("workspace");
        let pid_path = workspace.path().join("descendant.pid");
        let ready_path = workspace.path().join("tracer.ready");
        let (release, released) = std::sync::mpsc::channel::<()>();
        let tracer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let pid: libc::pid_t = loop {
                if let Ok(text) = std::fs::read_to_string(&pid_path) {
                    if let Ok(pid) = text.parse() {
                        break pid;
                    }
                }
                assert!(Instant::now() < deadline, "descendant did not start");
                std::thread::sleep(Duration::from_millis(10));
            };
            unsafe {
                assert_eq!(libc::ptrace(libc::PTRACE_ATTACH, pid, 0, 0), 0);
                let mut status = 0;
                assert_eq!(libc::waitpid(pid, &mut status, libc::__WALL), pid);
                assert!(libc::WIFSTOPPED(status));
                assert_eq!(
                    libc::ptrace(libc::PTRACE_SETOPTIONS, pid, 0, libc::PTRACE_O_TRACEEXIT),
                    0
                );
                assert_eq!(libc::ptrace(libc::PTRACE_CONT, pid, 0, 0), 0);
                std::fs::write(ready_path, "ready").expect("release root");
                // Hold the SIGKILL exit event until the report is built.
                loop {
                    assert_eq!(libc::waitpid(pid, &mut status, libc::__WALL), pid);
                    if status >> 16 == libc::PTRACE_EVENT_EXIT {
                        break;
                    }
                    assert!(libc::WIFSTOPPED(status));
                    assert_eq!(
                        libc::ptrace(libc::PTRACE_CONT, pid, 0, libc::WSTOPSIG(status)),
                        0
                    );
                }
                let _ = released.recv_timeout(Duration::from_secs(20));
                assert_eq!(libc::ptrace(libc::PTRACE_CONT, pid, 0, 0), 0);
                assert_eq!(libc::waitpid(pid, &mut status, libc::__WALL), pid);
            }
        });
        let script = format!(
            r#"
import os, signal, time
pid = os.fork()
if pid == 0:
    os.setsid()
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    os.close(1)
    os.close(2)
    with open('descendant.pid', 'w') as f:
        f.write(str(os.getpid()))
    while True:
        time.sleep(1)
while not os.path.exists('tracer.ready'):
    time.sleep(0.01)
print('root output captured', flush=True)
os._exit({code})
"#
        );
        let result = run_gate_argv(
            workspace.path(),
            1,
            vec!["python3".into(), "-c".into(), script],
            "cleanup fixture",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            GateExecution {
                runtime_tmpdir: None,
                timeout: None,
                supervision: None,
                baseline_timeout_diagnostic: false,
                cpus: None,
                deadline: None,
            },
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
            None,
        );
        drop(release);
        tracer.join().expect("tracer cleanup");
        let report = result.expect("completed gate report");
        assert_eq!(report.status, expected);
        assert_eq!(report.exit_code, code);
        assert_eq!(report.termination, AgentTaskGateTermination::Completed);
        assert_eq!(report.stdout.trim(), "root output captured");
        let warning = report.cleanup_warning.as_ref().expect("cleanup warning");
        assert!(warning.process_group.is_some());
        assert_eq!(warning.failed_step, "process_tree_drain_and_reap");
        let encoded = serde_json::to_value(&report).expect("receipt report");
        let decoded: AgentTaskGateReport =
            serde_json::from_value(encoded).expect("receipt roundtrip");
        assert_eq!(decoded.cleanup_warning, report.cleanup_warning);
    }
}
