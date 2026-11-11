use super::*;

#[test]
fn admitted_gate_reads_snapshot_and_replays_after_source_mutation() {
    let _lock = homeboy_core::test_support::env_lock();
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("marker"), "admitted bytes").unwrap();
    let root = store_root().unwrap();
    let mut policy = AgentTaskGateEnvironmentPolicy {
        hydrate_rust_cache: false,
        hydrate_dependencies: false,
        data_inputs: vec![GateInput {
            id: "capture".into(),
            source: source.display().to_string(),
            env: "CAPTURE".into(),
            digest: String::new(),
            size_bytes: 0,
            directory: false,
        }],
        ..Default::default()
    };
    admit(&mut policy, &root).unwrap();
    let input = &policy.data_inputs[0];
    assert!(input.source.is_empty());
    assert_eq!(input.size_bytes, 14);
    assert!(input.digest.starts_with("sha256:"));
    fs::write(source.join("marker"), "changed").unwrap();
    assert!(input.materialize(&source, &root).unwrap());
    let report = run_gate_command_with_supervision(
        temp.path(),
        1,
        "test \"$(cat \"$CAPTURE/marker\")\" = 'admitted bytes'",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        None,
        &policy,
        &[],
    )
    .unwrap();
    assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
    assert_eq!(report.environment.data_inputs, policy.data_inputs);
    let replay = report.environment.replay_policy();
    assert_eq!(replay.data_inputs, policy.data_inputs);
}

#[cfg(unix)]
#[test]
fn admission_refusal_names_the_symlinked_input() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("capture-target");
    fs::create_dir(&target).unwrap();
    let link = temp.path().join("capture");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut policy = AgentTaskGateEnvironmentPolicy {
        data_inputs: vec![GateInput {
            id: "capture".into(),
            source: link.display().to_string(),
            env: "CAPTURE".into(),
            digest: String::new(),
            size_bytes: 0,
            directory: false,
        }],
        ..Default::default()
    };
    let error = admit(&mut policy, temp.path()).unwrap_err();
    assert_eq!(
        error.details["problem"],
        format!(
            "input `capture` ({}): input is a symbolic link; declare its resolved target instead",
            link.display()
        )
    );
}

/// Concurrent Cooks admitting the same input race to publish its one
/// content-addressed snapshot. Every racer gets the verified snapshot; the
/// losers' rename onto the published directory is not an admission failure.
#[test]
fn concurrent_admissions_of_one_input_share_its_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    for index in 0..200 {
        fs::write(source.join(format!("file-{index}")), "bytes").unwrap();
    }
    let root = temp.path().join("store");
    let (digest, size_bytes, directory) = inspect(&source).unwrap();
    let input = GateInput {
        id: "capture".into(),
        source: String::new(),
        env: "CAPTURE".into(),
        digest,
        size_bytes,
        directory,
    };
    let barrier = std::sync::Barrier::new(8);
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    input.materialize(&source, &root)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    for result in &results {
        assert!(result.is_ok(), "{result:?}");
    }
    input.verify(&input.path_in(&root).unwrap()).unwrap();
    // Snapshots are sealed read-only; unseal so the temp dir can be removed.
    std::process::Command::new("chmod")
        .args(["-R", "u+w"])
        .arg(&root)
        .status()
        .unwrap();
}
