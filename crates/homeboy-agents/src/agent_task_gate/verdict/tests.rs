use super::*;

fn repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "gate@example.test"],
        vec!["config", "user.name", "Gate"],
        vec!["commit", "--allow-empty", "-m", "base"],
    ] {
        assert!(Command::new("git")
            .args(args)
            .current_dir(directory.path())
            .output()
            .unwrap()
            .status
            .success());
    }
    directory
}

#[test]
fn retry_executes_failed_gate_but_reuses_agent_pass() {
    let _env = homeboy_core::test_support::env_lock();
    let repo = repository();
    let counters = tempfile::tempdir().unwrap();
    let passed_count = counters.path().join("passed");
    let failed_count = counters.path().join("failed");
    let pass = format!("echo run >> '{}'; exit 0", passed_count.display());
    let fail = format!("echo run >> '{}'; exit 1", failed_count.display());
    let policy = AgentTaskGateEnvironmentPolicy {
        mode: AgentTaskGateEnvironmentMode::Replace,
        isolate_home: false,
        isolate_xdg: false,
        hydrate_rust_cache: false,
        hydrate_dependencies: false,
        ..Default::default()
    };
    let run = |command: &str, index| {
        run_gate_command_with_supervision(
            repo.path(),
            index,
            command,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            None,
            &policy,
            &[],
        )
        .unwrap()
    };
    // The agent and promotion call the same recorded executor.
    assert!(!run(&pass, 1).verdict.unwrap().reused);
    assert!(run(&pass, 1).verdict.unwrap().reused);
    let gates = VerifyGateOptions {
        hydrate_dependencies: false,
        gate_environment: policy.clone(),
        ..Default::default()
    };
    // Before any dependency/cache setup, promotion sees the same key when
    // supervision sets its declared suite timeout.
    let supervised = format!("{pass}; true");
    let supervision = GateSupervision {
        timeout: gates.gate_timeout(),
        no_progress_timeout: gates.gate_no_progress_timeout(),
        heartbeat_interval: gates.gate_heartbeat_interval(),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(|| false),
    };
    let runtime = tempfile::tempdir().unwrap();
    let first = run_gate_command_with_supervision(
        repo.path(),
        1,
        &supervised,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(runtime.path()),
        Some(&supervision),
        &policy,
        &[],
    )
    .unwrap();
    let preflight = cached_gate(
        repo.path(),
        1,
        &legacy_gate_argv(&supervised),
        None,
        &gates,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
    )
    .unwrap();
    assert_eq!(first.verdict.unwrap().key, preflight.verdict.unwrap().key);
    fs::remove_file(&passed_count).unwrap();
    // Restore the original counter for the retry assertions below.
    fs::write(&passed_count, "run\n").unwrap();
    assert_eq!(run(&fail, 2).status, AgentTaskGateStatus::Failed);
    // An unchanged retry executes only the failed gate.
    assert!(run(&pass, 1).verdict.unwrap().reused);
    assert!(!run(&fail, 2).verdict.unwrap().reused);
    assert_eq!(
        fs::read_to_string(&passed_count).unwrap().lines().count(),
        1
    );
    assert_eq!(
        fs::read_to_string(&failed_count).unwrap().lines().count(),
        2
    );
    fs::write(repo.path().join("source"), "new tree").unwrap();
    assert!(!run(&pass, 1).verdict.unwrap().reused);
    assert!(!run(&fail, 2).verdict.unwrap().reused);
    assert_eq!(
        fs::read_to_string(&passed_count).unwrap().lines().count(),
        2
    );
    assert_eq!(
        fs::read_to_string(&failed_count).unwrap().lines().count(),
        3
    );
    // Changed invocations and declared environments must execute, not borrow
    // the passing receipt for the current tree.
    assert!(!run(&format!("{pass}; true"), 1).verdict.unwrap().reused);
    let mut changed_policy = policy.clone();
    changed_policy
        .variables
        .insert("INPUT".into(), "changed".into());
    let changed_environment = run_gate_command_with_supervision(
        repo.path(),
        1,
        &pass,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        None,
        &changed_policy,
        &[],
    )
    .unwrap();
    assert!(!changed_environment.verdict.unwrap().reused);
    assert_eq!(
        fs::read_to_string(&passed_count).unwrap().lines().count(),
        4
    );
}

#[test]
fn exact_tree_command_and_environment_reuse_only_passes() {
    let _env = homeboy_core::test_support::env_lock();
    let repo = repository();
    let storage = tempfile::tempdir().unwrap();
    let policy = AgentTaskGateEnvironmentPolicy {
        mode: AgentTaskGateEnvironmentMode::Replace,
        ..Default::default()
    };
    let environment = AgentTaskGateEnvironment::default();
    let mut process = Command::new("sh");
    process.args(["-c", "true"]).env_clear().env("INPUT", "one");
    let prepare = |process: &Command| {
        let mut verdict =
            GateVerdict::prepare(repo.path(), process, &policy, &environment, None, None).unwrap();
        verdict.root = storage.path().to_path_buf();
        verdict
    };
    let visibility = AgentTaskGateVisibility::Visible;
    let reveal = AgentTaskGateRevealPolicy::FullEvidence;
    let original = prepare(&process);
    let key = original.key.clone();
    assert!(original.passed(1, visibility, reveal).is_none());
    let mut report = AgentTaskGateReport::new(
        "gate-1",
        vec!["sh".into(), "-c".into(), "true".into()],
        0,
        "pass",
        String::new(),
        None,
        visibility,
        reveal,
        environment.clone(),
    );
    original.record(repo.path(), &mut report, Duration::from_secs(9));
    let reused = prepare(&process).passed(2, visibility, reveal).unwrap();
    assert_eq!(reused.id, "gate-2");
    let evidence = reused.verdict.unwrap();
    assert!(evidence.reused);
    assert_eq!(evidence.saved_ms, 9000);
    assert!(Path::new(&evidence.original_run).is_file());
    process.env("INPUT", "two");
    assert_ne!(prepare(&process).key, key);
    process.env("INPUT", "one").arg("changed");
    assert_ne!(prepare(&process).key, key);
    let mut process = Command::new("sh");
    process.args(["-c", "true"]).env_clear().env("INPUT", "one");
    fs::write(repo.path().join("source"), "changed").unwrap();
    assert_ne!(prepare(&process).key, key);
    let failed = prepare(&process);
    let failed_key = failed.key.clone();
    report.status = AgentTaskGateStatus::Failed;
    report.exit_code = 1;
    failed.record(repo.path(), &mut report, Duration::from_secs(1));
    assert!(prepare(&process).passed(1, visibility, reveal).is_none());
    assert!(!storage.path().join(failed_key).join("pass.json").exists());
}
