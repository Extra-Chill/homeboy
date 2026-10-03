//! Real controller/receiver execution. The native verifier provisions an owned
//! SSH daemon and candidate Homeboy runner before invoking this test explicitly.
use super::*;
use homeboy_agents::agent_tasks::gate::*;
use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn save(evidence: &Path, name: &str, value: &impl serde::Serialize) {
    fs::write(
        evidence.join(name),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}
fn controls(
    heartbeat: Arc<dyn Fn(&AgentTaskGateLiveStatus) -> Result<()> + Send + Sync>,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
) -> GateSupervision {
    GateSupervision {
        timeout: Duration::from_secs(120),
        no_progress_timeout: Duration::from_secs(120),
        heartbeat_interval: Duration::from_millis(100),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: heartbeat,
        is_cancelled: cancelled,
    }
}
fn gate(
    root: &Path,
    command: &str,
    environment: &AgentTaskGateEnvironmentPolicy,
    supervision: &GateSupervision,
    private: bool,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_supervision(
        root,
        1,
        command,
        if private {
            AgentTaskGateVisibility::Private
        } else {
            AgentTaskGateVisibility::Visible
        },
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(supervision),
        environment,
        &[],
    )
}

#[test]
#[ignore = "requires the native verifier's owned SSH runner and candidate daemon"]
fn full_native_transport() {
    let fixture = std::path::PathBuf::from(
        std::env::var("HOMEBOY_NATIVE_GATE_FIXTURE").expect("native fixture"),
    );
    let evidence = std::path::PathBuf::from(std::env::var("HOMEBOY_NATIVE_GATE_EVIDENCE").unwrap());
    let binary = std::env::var("HOMEBOY_NATIVE_GATE_BINARY").unwrap();
    let runner = "native-gate-lab";
    crate::register_runner_workspace_root_provider();
    crate::register_workspace_snapshot_provider();
    crate::register_runner_evidence_provider();
    crate::register_runner_job_preparation_provider();
    crate::enable_production_lab_staging();
    homeboy_agents::orchestration::register();
    super::register();
    let candidate = fixture.join("controller/candidate");
    fs::create_dir_all(candidate.join("src")).unwrap();
    fs::write(candidate.join("candidate.txt"), "original candidate\n").unwrap();
    fs::write(
        candidate.join("Cargo.toml"),
        "[package]\nname='native-gate-fixture'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(candidate.join("src/lib.rs"), "#[test]\nfn first() {\n    assert_eq!(2 + 2, 4);\n}\n\n#[test]\nfn second() {\n    assert_eq!(3 + 3, 6);\n}\n").unwrap();
    assert!(Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&candidate)
        .output()
        .unwrap()
        .status
        .success());
    fs::write(candidate.join(".gitignore"), "target/\n").unwrap();
    fs::write(candidate.join("homeboy.json"), r#"{"id":"native-gate-fixture","extensions":{"rust":{"settings":{"rust_test_runner":"cargo","rust_cargo_test_threads":1}}}}"#).unwrap();
    git(&candidate, &["init", "-q"]);
    git(&candidate, &["config", "user.name", "Native Fixture"]);
    git(
        &candidate,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(&candidate, &["add", "."]);
    git(&candidate, &["commit", "-qm", "candidate"]);
    let assets = fixture.join("controller/declared-assets");
    fs::create_dir_all(assets.join("fixture")).unwrap();
    fs::create_dir_all(assets.join("shared")).unwrap();
    fs::write(assets.join("fixture/fixture.json"), r#"{"id":"fixture"}"#).unwrap();
    fs::write(assets.join("shared/declared.txt"), "declared").unwrap();
    fs::write(
        assets.join("homeboy-extension-root.json"),
        r#"{"shared_assets":["shared"]}"#,
    )
    .unwrap();
    let mut policy = AgentTaskGateEnvironmentPolicy {
        lab_runner: Some(runner.to_string()),
        hydrate_rust_cache: false,
        shared_cargo_target: Some(false),
        extension_inputs: vec![AgentTaskGateExtensionInput {
            id: "fixture".to_string(),
            source: assets.join("fixture").display().to_string(),
            identity: None,
        }],
        ..Default::default()
    };
    let heartbeats = Arc::new(AtomicUsize::new(0));
    let seen = heartbeats.clone();
    let supervision = controls(
        Arc::new(move |heartbeat| {
            assert!(heartbeat.output_tail.len() < 128);
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }),
        Arc::new(|| false),
    );
    policy
        .preserve
        .insert("PATH".to_string(), "PATH".to_string());
    preflight_gate_toolchains(
        &candidate,
        &policy,
        &[AgentTaskGateToolchainRequirement {
            command: "native-gate-tool".to_string(),
            probe_arguments: vec!["--version".to_string()],
        }],
        &[],
        None,
        Duration::from_secs(60),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(std::env::var("HOMEBOY_NATIVE_GATE_PROBE").unwrap()).unwrap(),
        "probed\n"
    );
    let mut package: AgentTaskGatePackageArtifactRequirement = serde_json::from_value(serde_json::json!({
        "id":"candidate-package", "environment":{"name":"FIXTURE_PACKAGE", "default":"."},
        "required_paths":[{"path":"candidate.txt", "sha256":format!("sha256:{}", homeboy_engine_primitives::content_hash::sha256_hex(b"original candidate\n"))}],
        "remediation":{"fixture":"restore the declared package"}
    })).unwrap();
    preflight_gate_toolchains(
        &candidate,
        &policy,
        &[],
        &[package.clone()],
        None,
        Duration::from_secs(30),
    )
    .unwrap();
    package.required_paths[0].sha256 = Some(format!("sha256:{}", "0".repeat(64)));
    let package_error = preflight_gate_toolchains(
        &candidate,
        &policy,
        &[],
        &[package],
        None,
        Duration::from_secs(30),
    )
    .unwrap_err();
    assert_eq!(package_error.details["gate_disposition"], "unavailable");
    save(
        &evidence,
        "package-readiness-rejected.json",
        &package_error.details,
    );
    let missing = preflight_gate_toolchains(
        &candidate,
        &policy,
        &[AgentTaskGateToolchainRequirement {
            command: "no-15359-such-tool".to_string(),
            probe_arguments: vec!["--version".to_string()],
        }],
        &[],
        None,
        Duration::from_secs(30),
    )
    .unwrap_err();
    assert_eq!(missing.details["gate_disposition"], "unavailable");
    save(&evidence, "missing-tool.json", &missing.details);
    let private_marker = "15359-PRIVATE-COMMAND-AND-DATA-MARKER";
    policy
        .variables
        .insert("PRIVATE_DATA".to_string(), private_marker.to_string());
    let command = format!("test -f candidate.txt && test \"$(cat \"$HOME/.config/homeboy/extensions/shared/declared.txt\")\" = declared && sleep 1 && printf '%s' '{private_marker}' && printf '%s' \"$PRIVATE_DATA\"");
    let passing = gate(&candidate, &command, &policy, &supervision, true).unwrap();
    save(&evidence, "private-passing.json", &passing);
    assert_eq!(
        passing.status,
        AgentTaskGateStatus::Succeeded,
        "{passing:?}"
    );
    assert!(passing.stdout.contains(private_marker));
    assert_eq!(
        passing.step.status,
        homeboy_core::plan::PlanStepStatus::Success
    );
    assert!(heartbeats.load(Ordering::SeqCst) > 0);
    assert_eq!(
        passing.environment.extension_inputs[0].source,
        policy.extension_inputs[0].source
    );
    assert_eq!(
        passing.environment.extension_inputs[0].shared_assets[0].source,
        assets.join("shared").display().to_string()
    );
    let replay = passing.environment.replay_policy();
    let baseline = gate(&candidate, &command, &replay, &supervision, true).unwrap();
    save(&evidence, "baseline-replay.json", &baseline);
    assert_eq!(
        baseline.status,
        AgentTaskGateStatus::Succeeded,
        "{baseline:?}"
    );
    assert_eq!(
        passing.environment.extension_inputs[0].identity,
        baseline.environment.extension_inputs[0].identity
    );
    let red = gate(&candidate, "exit 23", &policy, &supervision, false).unwrap();
    save(&evidence, "executed-red.json", &red);
    assert_eq!(red.status, AgentTaskGateStatus::Failed);
    assert_eq!(red.exit_code, 23);
    let deferred = gate(&candidate, "printf '%s' '{\"schema\":\"homeboy/deferred-workload-result/v1\",\"status\":\"deferred\"}'", &policy, &supervision, false).unwrap();
    assert_eq!(deferred.status, AgentTaskGateStatus::Deferred);
    save(&evidence, "deferred.json", &deferred);
    let large = gate(
        &candidate,
        "python3 -c 'print(\"x\" * 100000)'",
        &policy,
        &supervision,
        false,
    )
    .unwrap();
    save(&evidence, "bounded-result.json", &large);
    assert_eq!(large.status, AgentTaskGateStatus::Succeeded);
    assert!(large.capture.stdout.truncated);
    let changed = Arc::new(AtomicBool::new(false));
    let changed_flag = changed.clone();
    let tracked = candidate.join("candidate.txt");
    let mutation = controls(
        Arc::new(move |_| {
            if !changed_flag.swap(true, Ordering::SeqCst) {
                fs::write(&tracked, "changed while running\n").unwrap();
            }
            Ok(())
        }),
        Arc::new(|| false),
    );
    assert!(gate(
        &candidate,
        "sleep 2; cat candidate.txt",
        &policy,
        &mutation,
        false
    )
    .is_err());
    assert!(changed.load(Ordering::SeqCst));
    fs::write(candidate.join("candidate.txt"), "original candidate\n").unwrap();
    let extension_before = fs::read(assets.join("fixture/fixture.json")).unwrap();
    let shared_before = fs::read(assets.join("shared/declared.txt")).unwrap();
    let shared_changed = Arc::new(AtomicBool::new(false));
    let shared_flag = shared_changed.clone();
    let shared_file = assets.join("shared/declared.txt");
    let shared_mutation = controls(
        Arc::new(move |_| {
            if !shared_flag.swap(true, Ordering::SeqCst) {
                fs::write(&shared_file, "changed only on controller during execution").unwrap();
            }
            Ok(())
        }),
        Arc::new(|| false),
    );
    let during_transport = gate(&candidate,
        "sleep 2; test \"$(cat \"$HOME/.config/homeboy/extensions/shared/declared.txt\")\" = declared && printf runner-used-sealed-shared",
        &policy, &shared_mutation, false);
    let extension_after = fs::read(assets.join("fixture/fixture.json")).unwrap();
    let shared_after = fs::read(assets.join("shared/declared.txt")).unwrap();
    fs::write(assets.join("shared/declared.txt"), &shared_before).unwrap();
    assert!(
        shared_changed.load(Ordering::SeqCst),
        "mutation must happen during real admitted remote waiting"
    );
    assert_eq!(
        extension_before, extension_after,
        "extension directory is unchanged"
    );
    assert_eq!(fs::read_dir(assets.join("fixture")).unwrap().count(), 1);
    assert_ne!(
        shared_before, shared_after,
        "only the separate shared tree changed"
    );
    let rejected = during_transport.unwrap();
    save(
        &evidence,
        "shared-mutation-during-transport.json",
        &rejected,
    );
    assert_eq!(rejected.status, AgentTaskGateStatus::Failed);
    assert_eq!(
        rejected.lab_receipt.as_ref().unwrap()["stage"],
        "controller_closure_revalidation"
    );
    assert!(rejected.lab_receipt.as_ref().unwrap()["artifact_ref"]
        .as_str()
        .is_some());
    save(
        &evidence,
        "shared-mutation-source-identities.json",
        &serde_json::json!({
            "extension_before":homeboy_engine_primitives::content_hash::sha256_hex(&extension_before),
            "extension_after":homeboy_engine_primitives::content_hash::sha256_hex(&extension_after),
            "shared_before":homeboy_engine_primitives::content_hash::sha256_hex(&shared_before),
            "shared_after":homeboy_engine_primitives::content_hash::sha256_hex(&shared_after), "restored":true
        }),
    );
    fs::write(assets.join("shared/declared.txt"), "changed closure").unwrap();
    let drift = gate(&candidate, &command, &replay, &supervision, true).unwrap();
    save(&evidence, "closure-drift.json", &drift);
    assert_ne!(drift.status, AgentTaskGateStatus::Succeeded);
    fs::write(assets.join("shared/declared.txt"), "declared").unwrap();
    let pid_file = fixture.join("receiver/gate-child.pid");
    let mut cancel_policy = policy.clone();
    cancel_policy
        .variables
        .insert("CHILD_PID_FILE".to_string(), pid_file.display().to_string());
    let child_started = std::sync::Mutex::new(None);
    let cancel_pid = pid_file.clone();
    let cancellation = controls(
        Arc::new(|_| Ok(())),
        Arc::new(move || {
            if !cancel_pid.exists() {
                return false;
            }
            let mut started = child_started.lock().unwrap();
            started.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(1)
        }),
    );
    let cancelled = gate(
        &candidate,
        "sleep 60 & echo $! > \"$CHILD_PID_FILE\"; wait",
        &cancel_policy,
        &cancellation,
        false,
    )
    .unwrap();
    save(&evidence, "cancelled.json", &cancelled);
    assert_eq!(cancelled.termination, AgentTaskGateTermination::Cancelled);
    let pid = fs::read_to_string(&pid_file)
        .expect("actual runner child PID")
        .trim()
        .to_string();
    let reap_started = Instant::now();
    while Path::new(&format!("/proc/{pid}")).exists()
        && reap_started.elapsed() < Duration::from_secs(10)
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "cancelled gate child survived native reaping"
    );
    let public_jobs = crate::daemon_api_get(runner, "/jobs").unwrap();
    save(&evidence, "public-job-projections.json", &public_jobs);
    let projection = public_jobs.to_string();
    assert!(
        !projection.contains(private_marker),
        "private gate command/data leaked into public job projections"
    );
    // Execute the actual installed Rust extension, not a fixture adapter. Its
    // own declared shared closure is selected by the same production owners.
    let mut rust_policy = policy.clone();
    rust_policy.variables.remove("PRIVATE_DATA");
    rust_policy.extension_inputs = vec![AgentTaskGateExtensionInput {
        id: "rust".to_string(),
        source: std::env::var("HOMEBOY_NATIVE_GATE_RUST_SOURCE").unwrap(),
        identity: None,
    }];
    let cargo = Command::new("rustup")
        .args(["which", "cargo"])
        .output()
        .unwrap();
    let cargo = String::from_utf8(cargo.stdout).unwrap().trim().to_string();
    let rustc = Command::new("rustup")
        .args(["which", "rustc"])
        .output()
        .unwrap();
    let rustc = String::from_utf8(rustc.stdout).unwrap().trim().to_string();
    rust_policy.preserve.clear();
    rust_policy.variables.insert(
        "PATH".to_string(),
        format!(
            "{}:{}:/usr/bin:/bin",
            Path::new(&binary).parent().unwrap().display(),
            Path::new(&cargo).parent().unwrap().display()
        ),
    );
    rust_policy.variables.insert("RUSTC".to_string(), rustc);
    let rust = gate(
        &candidate,
        "homeboy review test --path .",
        &rust_policy,
        &supervision,
        false,
    )
    .unwrap();
    save(&evidence, "installed-rust-extension.json", &rust);
    assert_eq!(
        rust.status,
        AgentTaskGateStatus::Succeeded,
        "actual installed Rust extension failed: {rust:?}"
    );
    #[derive(serde::Deserialize)]
    struct TestCommandReceipt {
        schema: String,
        command: String,
        operation: String,
        data: serde_json::Value,
    }
    let test_receipt: TestCommandReceipt =
        crate::connection::parse_json_from_mixed_stdout(&rust.stdout)
            .expect("actual Rust extension command-result receipt");
    assert_eq!(test_receipt.schema, "homeboy/command-result/v3");
    assert_eq!(test_receipt.command, "review");
    assert_eq!(test_receipt.operation, "test");
    assert_ne!(
        test_receipt
            .data
            .pointer("/test_runtime_evidence/status")
            .and_then(serde_json::Value::as_str),
        Some("invalid_evidence"),
        "{test_receipt_data:?}",
        test_receipt_data = test_receipt.data
    );
    let count = test_receipt
        .data
        .pointer("/summary/passed")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert_eq!(
        count, 2,
        "actual installed Rust extension must supply nonzero typed counts: {rust:?}"
    );
    let stale = fixture.join("stale-homeboy");
    let receiver_binary =
        std::path::PathBuf::from(std::env::var("HOMEBOY_NATIVE_GATE_RECEIVER_BINARY").unwrap());
    fs::copy(&receiver_binary, &stale).unwrap();
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&stale)
        .unwrap()
        .write_all(b"stale-fixture-identity")
        .unwrap();
    // Replace only this fixture's loaded image path atomically. A different
    // compatible job binary is not a stale daemon; its actual lease hash must
    // now disagree with the configured image at the same owned path.
    fs::rename(&stale, &receiver_binary).unwrap();
    let unavailable = gate(
        &candidate,
        "printf should-not-run",
        &policy,
        &supervision,
        false,
    )
    .unwrap();
    save(&evidence, "stale-admission.json", &unavailable);
    assert_eq!(
        unavailable.status,
        AgentTaskGateStatus::Unavailable,
        "{unavailable:?}"
    );
    assert!(unavailable.stdout.is_empty());
    let restored = fixture.join("restored-homeboy");
    fs::copy(&binary, &restored).unwrap();
    fs::rename(&restored, &receiver_binary).unwrap();
    let mut absent_policy = policy.clone();
    absent_policy.lab_runner = Some("no-configured-native-runner".to_string());
    let absent = gate(
        &candidate,
        "printf should-not-run",
        &absent_policy,
        &supervision,
        false,
    )
    .unwrap();
    save(&evidence, "unavailable-admission.json", &absent);
    assert_eq!(absent.status, AgentTaskGateStatus::Unavailable);
    assert!(absent.stdout.is_empty());
    save(
        &evidence,
        "verification-summary.json",
        &serde_json::json!({"private_transport":true,
        "baseline_replay":true,"remote_tool_probe":true,"remote_package_readiness":true,"package_digest_rejected":true,"unavailable_tool_rejected":true,
        "candidate_mutation_rejected":true,"closure_drift_rejected":true,"shared_mutation_during_transport_rejected":true,"cancelled_child_reaped":true,
        "public_projection_private_text_absent":true,"bounded_result":true,"deferred_not_passed":true,
        "installed_rust_tests_passed":count,"stale_admission_rejected":true,"unavailable_admission_rejected":true,"heartbeats":heartbeats.load(Ordering::SeqCst)}),
    );
}
