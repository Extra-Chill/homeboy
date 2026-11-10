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
