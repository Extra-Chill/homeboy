use super::*;

#[cfg(unix)]
#[test]
fn timeout_is_distinct_from_no_progress() {
    let worktree = tempfile::tempdir().expect("worktree");
    let supervision = GateSupervision {
        timeout: Duration::from_millis(30),
        no_progress_timeout: Duration::from_secs(1),
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
    assert_eq!(report.termination, AgentTaskGateTermination::TimedOut);
    assert_eq!(report.exit_code, 124);
    let evidence = report.failure_evidence.as_ref().expect("failure evidence");
    assert_eq!(
        serde_json::to_value(evidence.classification).unwrap(),
        "execution_budget"
    );
    assert!(evidence.summary.contains("wall-clock limit of 30 ms"));
    let first = report.supervision_regate.as_ref().expect("first run");
    assert!(first.summary.contains("wall-clock limit of 30 ms"));
}

#[test]
fn legacy_gate_report_deserializes_with_new_defaults() {
    let report: AgentTaskGateReport = serde_json::from_value(serde_json::json!({
        "schema": "homeboy/agent-task-gate-report/v2", "id": "gate-1",
        "status": "succeeded", "command": ["sh", "-lc", "true"], "exit_code": 0
    }))
    .expect("legacy report");
    assert_eq!(report.termination, AgentTaskGateTermination::Completed);
    assert_eq!(report.capture.stdout.bytes_seen, 0);
}

#[test]
fn agent_task_gate_status_bridges_to_homeboy_gate_status() {
    assert_eq!(
        HomeboyGateStatus::from(AgentTaskGateStatus::Succeeded),
        HomeboyGateStatus::Passed
    );
    assert_eq!(
        HomeboyGateStatus::from(AgentTaskGateStatus::Failed),
        HomeboyGateStatus::Failed
    );
    assert_eq!(
        HomeboyGateStatus::from(AgentTaskGateStatus::Skipped),
        HomeboyGateStatus::Skipped
    );
}

#[test]
fn declared_opaque_sidecar_is_normalized_with_durable_evidence_reference() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = include_str!(
        "../../../../../tests/fixtures/agent_task_gate_feedback/opaque-producer-diagnostics.json"
    );
    fs::write(temp.path().join("diagnostics.json"), fixture).expect("write sidecar");
    let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");
    ingest_gate_diagnostic_sidecars(
        temp.path(),
        &[AgentTaskGateDiagnosticSidecarMapping {
            source_schema: "example/producer-diagnostic/v1".to_string(),
            target_schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
            path: "diagnostics.json".to_string(),
            producer: AgentTaskGateDiagnosticProducer {
                id: "opaque-producer".to_string(),
                schema: "example/producer-output/v1".to_string(),
            },
        }],
        &mut report,
        "homeboy://agent-task/run/run-1/promotion/gates/gate-1",
    );
    let diagnostic = &report.failure_evidence.unwrap().diagnostics[0];
    assert_eq!(diagnostic.schema, AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA);
    assert_eq!(diagnostic.identity, "opaque:stable-identity");
    assert_eq!(diagnostic.producer.id, "opaque-producer");
    assert_eq!(
        diagnostic.full_evidence_ref,
        "homeboy://agent-task/run/run-1/promotion/gates/gate-1"
    );
}

#[test]
fn absent_or_malformed_declared_sidecars_preserve_gate_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");
    let mapping = AgentTaskGateDiagnosticSidecarMapping {
        source_schema: "example/producer-diagnostic/v1".to_string(),
        target_schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
        path: "diagnostics.json".to_string(),
        producer: AgentTaskGateDiagnosticProducer {
            id: "opaque-producer".to_string(),
            schema: "example/producer-output/v1".to_string(),
        },
    };
    ingest_gate_diagnostic_sidecars(
        temp.path(),
        std::slice::from_ref(&mapping),
        &mut report,
        "evidence",
    );
    fs::write(temp.path().join("diagnostics.json"), "not json").expect("write malformed sidecar");
    ingest_gate_diagnostic_sidecars(temp.path(), &[mapping], &mut report, "evidence");
    assert!(report.failure_evidence.unwrap().diagnostics.is_empty());
}

#[test]
fn verify_gate_policy_defaults_are_durable_and_backward_compatible() {
    let defaults = VerifyGateOptions::default();
    assert_eq!(defaults.gate_timeout(), Duration::from_secs(30 * 60));
    assert_eq!(defaults.gate_heartbeat_interval(), Duration::from_secs(5));
    assert!(!defaults.rerun_completed_gates);
    assert_eq!(
        defaults.execution_policy,
        AgentTaskGateExecutionPolicy::OrderedFailFast
    );

    let legacy: VerifyGateOptions = serde_json::from_value(serde_json::json!({
        "verify": ["cargo test"],
        "private_verify": [],
        "private_gate_reveal": "summary_only"
    }))
    .expect("deserialize legacy gate policy");
    assert_eq!(
        legacy,
        VerifyGateOptions {
            verify: vec!["cargo test".to_string()],
            ..VerifyGateOptions::default()
        }
    );
}

#[test]
fn gate_command_reports_success_without_failure_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");

    let report = run_gate_command(temp.path(), 1, "printf 'ok'").expect("gate report");

    assert_eq!(report.schema, AGENT_TASK_GATE_REPORT_SCHEMA);
    assert_eq!(report.id, "gate-1");
    assert_eq!(report.visibility, AgentTaskGateVisibility::Visible);
    assert_eq!(
        report.reveal_policy,
        AgentTaskGateRevealPolicy::FullEvidence
    );
    assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
    assert_eq!(report.exit_code, 0);
    assert_eq!(report.stdout, "ok");
    assert!(report.failure_evidence.is_none());
}

#[test]
fn accepted_inherited_failure_rebuilds_the_embedded_plan_step() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");

    report.accept_inherited_failure();

    assert_eq!(report.status, AgentTaskGateStatus::AcceptedInheritedFailure);
    let step = serde_json::to_value(&report.step).expect("serialize step");
    assert_eq!(step["status"], "failed");
    assert_eq!(step["outputs"]["accepted_inherited_failure"], true);
    assert_eq!(step["outputs"]["baseline_red"], true);
    assert_eq!(
        step["outputs"]["gate_result"]["status"],
        "accepted_inherited_failure"
    );
}

#[test]
fn gate_command_uses_the_declared_runtime_scratch_root() {
    let worktree = tempfile::tempdir().expect("worktree");
    let scratch = tempfile::tempdir().expect("scratch");

    let report = run_gate_command_with_policy_and_runtime_tmpdir(
        worktree.path(),
        8,
        "printf '%s:%s:%s' \"$TMPDIR\" \"$TEMP\" \"$TMP\"",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(scratch.path()),
    )
    .expect("gate report");

    let expected = scratch.path().join("tmp").display().to_string();
    assert_eq!(report.stdout, format!("{expected}:{expected}:{expected}"));
}

#[test]
fn gate_command_normalizes_failure_for_agent_feedback() {
    let temp = tempfile::tempdir().expect("tempdir");

    let report = run_gate_command(
        temp.path(),
        2,
        "printf 'line one\nline two\n'; printf 'boom\n' >&2; exit 42",
    )
    .expect("gate report");
    let evidence = report.failure_evidence.as_ref().expect("failure evidence");

    assert_eq!(report.status, AgentTaskGateStatus::Failed);
    assert_eq!(report.exit_code, 42);
    assert_eq!(
        evidence.command,
        "printf 'line one\nline two\n'; printf 'boom\n' >&2; exit 42"
    );
    assert_eq!(evidence.stdout_tail, "line one\nline two");
    assert_eq!(evidence.stderr_tail, "boom");
    assert!(evidence.summary.contains("deterministic gate failed"));
    assert!(evidence.agent_feedback.contains("Fix the code"));
}

#[test]
fn gate_command_records_private_visibility_and_reveal_policy() {
    let temp = tempfile::tempdir().expect("tempdir");

    let report = run_gate_command_with_policy(
        temp.path(),
        3,
        "printf 'hidden failure'; exit 1",
        AgentTaskGateVisibility::Private,
        AgentTaskGateRevealPolicy::SummaryOnly,
    )
    .expect("gate report");

    assert_eq!(report.status, AgentTaskGateStatus::Failed);
    assert_eq!(report.visibility, AgentTaskGateVisibility::Private);
    assert_eq!(report.reveal_policy, AgentTaskGateRevealPolicy::SummaryOnly);
    assert_eq!(report.stdout, "hidden failure");
}

#[test]
fn agent_task_gate_report_normalizes_to_homeboy_gate_result() {
    let temp = tempfile::tempdir().expect("tempdir");

    let report = run_gate_command_with_policy(
        temp.path(),
        4,
        "printf 'hidden failure'; exit 1",
        AgentTaskGateVisibility::Private,
        AgentTaskGateRevealPolicy::SummaryOnly,
    )
    .expect("gate report");
    let result: HomeboyGateResult = report.into();

    assert_eq!(
        result.schema,
        homeboy_core::gate::HOMEBOY_GATE_RESULT_SCHEMA
    );
    assert_eq!(result.id, "gate-4");
    assert_eq!(result.kind, HomeboyGateKind::Command);
    assert_eq!(result.status, HomeboyGateStatus::Failed);
    assert_eq!(result.visibility, HomeboyGateVisibility::Private);
    assert_eq!(result.reveal_policy, HomeboyGateRevealPolicy::SummaryOnly);
    assert_eq!(result.retryable, Some(true));
    assert!(result.summary.contains("detailed evidence is withheld"));
    assert!(result
        .agent_feedback
        .contains("hidden evaluator details are withheld"));
    assert_eq!(result.evidence["exit_code"], 1);
    assert_eq!(result.evidence["withheld"], true);
    assert_eq!(result.evidence.get("stdout"), None);
    assert_eq!(result.evidence.get("stderr"), None);
    assert_eq!(result.provenance["source_type"], "AgentTaskGateReport");
}

#[test]
fn skipped_private_gate_preserves_blocker_evidence_without_command_disclosure() {
    let report = AgentTaskGateReport::skipped(
        "gate-2",
        vec![
            "sh".to_string(),
            "-lc".to_string(),
            "private-command --secret".to_string(),
        ],
        AgentTaskGateVisibility::Private,
        AgentTaskGateRevealPolicy::Redacted,
        "gate-1",
    );

    assert_eq!(report.schema, AGENT_TASK_GATE_REPORT_SCHEMA);
    assert_eq!(report.step.status, PlanStepStatus::Skipped);
    assert_eq!(
        report
            .skip_reason
            .as_ref()
            .map(|reason| reason.blocking_gate_id.as_str()),
        Some("gate-1")
    );

    let result: HomeboyGateResult = report.into();
    assert_eq!(result.status, HomeboyGateStatus::Skipped);
    assert_eq!(result.visibility, HomeboyGateVisibility::Private);
    assert_eq!(result.evidence["skip_reason"]["blocking_gate_id"], "gate-1");
    assert_eq!(result.evidence.get("command"), None);
    assert!(!result.summary.contains("private-command"));
    assert!(result.agent_feedback.is_empty());
}

#[test]
fn private_redacted_agent_task_gate_result_omits_command_and_output_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");

    let report = run_gate_command_with_policy(
        temp.path(),
        6,
        "printf 'secret stdout'; printf 'secret stderr' >&2; exit 1",
        AgentTaskGateVisibility::Private,
        AgentTaskGateRevealPolicy::Redacted,
    )
    .expect("gate report");
    let result: HomeboyGateResult = report.into();

    assert_eq!(result.status, HomeboyGateStatus::Failed);
    assert_eq!(result.reveal_policy, HomeboyGateRevealPolicy::Redacted);
    assert_eq!(result.evidence["redacted"], true);
    assert_eq!(result.evidence.get("command"), None);
    assert_eq!(result.evidence.get("stdout"), None);
    assert_eq!(result.evidence.get("stderr"), None);
    assert!(result.summary.contains("evidence redacted"));
}

#[test]
fn successful_agent_task_gate_report_normalizes_to_passed_gate_result() {
    let temp = tempfile::tempdir().expect("tempdir");

    let report = run_gate_command(temp.path(), 5, "printf 'ok'").expect("gate report");
    let result: HomeboyGateResult = report.into();

    assert_eq!(result.id, "gate-5");
    assert_eq!(result.kind, HomeboyGateKind::Command);
    assert_eq!(result.status, HomeboyGateStatus::Passed);
    assert_eq!(result.retryable, Some(false));
    assert_eq!(result.evidence["exit_code"], 0);
    assert_eq!(result.evidence["stdout"], "ok");
    assert!(result.agent_feedback.is_empty());
    assert!(result.summary.contains("deterministic gate passed"));
}

#[test]
fn isolated_gate_does_not_observe_ambient_durable_recipes_runs_or_runtime_liveness() {
    let _guard = env_mutex();
    let worktree = tempfile::tempdir().expect("worktree");
    let ambient = tempfile::tempdir().expect("ambient state");
    let home = ambient.path().join("home");
    let xdg_state = ambient.path().join("state");
    let xdg_runtime = ambient.path().join("runtime");
    fs::create_dir_all(home.join(".homeboy/recipes")).expect("ambient recipe directory");
    fs::create_dir_all(xdg_state.join("runs/active")).expect("ambient run directory");
    fs::create_dir_all(xdg_runtime.join("homeboy-live")).expect("ambient runtime directory");
    let _env = EnvVarGuard::set(&[
        ("HOME", home.as_path()),
        ("XDG_STATE_HOME", xdg_state.as_path()),
        ("XDG_RUNTIME_DIR", xdg_runtime.as_path()),
    ]);

    let report = run_gate_command(
            worktree.path(),
            7,
            "test ! -e \"$HOME/.homeboy/recipes\" && test ! -e \"$XDG_STATE_HOME/runs/active\" && test ! -e \"$XDG_RUNTIME_DIR/homeboy-live\"",
        )
        .expect("isolated gate report");

    assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
    assert!(report
        .environment
        .sanitized
        .iter()
        .any(|variable| variable.name == "HOME"));
    assert!(report
        .environment
        .sanitized
        .iter()
        .any(|variable| variable.name == "XDG_STATE_HOME"));
    assert!(report
        .environment
        .sanitized
        .iter()
        .any(|variable| variable.name == "XDG_RUNTIME_DIR"));
}

#[test]
fn declared_playwright_browsers_resolves_nodejs_extension_mapping() {
    let _guard = env_mutex();
    let worktree = tempfile::tempdir().unwrap();
    let ambient = tempfile::tempdir().unwrap();
    let extensions = ambient.path().join(".config/homeboy/extensions/nodejs");
    fs::create_dir_all(&extensions).unwrap();
    fs::write(
        extensions.join("nodejs.json"),
        include_str!("../../../../../extensions/nodejs/nodejs.json"),
    )
    .unwrap();
    let cache = ambient.path().join(".cache/ms-playwright");
    fs::create_dir_all(&cache).unwrap();
    let _env = EnvVarGuard::set(&[("HOME", ambient.path())]);
    // Gates (including Homeboy's own Lab gates) can export the runner's
    // real PLAYWRIGHT_BROWSERS_PATH, which outranks the HOME mapping.
    let _playwright = homeboy_core::test_support::EnvVarGuard::unset("PLAYWRIGHT_BROWSERS_PATH");
    let policy = AgentTaskGateEnvironmentPolicy {
        requirements: AgentTaskGateRequirements {
            host_tools: vec!["playwright-browsers".into()],
            secrets: Vec::new(),
        },
        ..Default::default()
    };
    let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        worktree.path(),
        1,
        &format!(
            "test \"$PLAYWRIGHT_BROWSERS_PATH\" = '{}' && test \"$HOME\" != '{}'",
            cache.display(),
            ambient.path().display()
        ),
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(worktree.path()),
        &policy,
        &[],
    )
    .unwrap();
    assert_eq!(report.exit_code, 0, "{report:?}");
}

#[test]
fn isolated_gate_discovers_only_selected_extension_copies_with_replayable_provenance() {
    let _guard = env_mutex();
    let worktree = tempfile::tempdir().expect("worktree");
    let ambient = tempfile::tempdir().expect("ambient home");
    let extensions = ambient.path().join(".config/homeboy/extensions");
    let selected = extensions.join("selected-fixture");
    let unselected = extensions.join("ambient-fixture");
    fs::create_dir_all(&selected).expect("selected extension");
    fs::create_dir_all(&unselected).expect("unselected extension");
    fs::write(
        selected.join("selected-fixture.json"),
        r#"{"id":"selected-fixture","name":"Selected","version":"1.0.0"}"#,
    )
    .expect("selected manifest");
    fs::write(selected.join(".source-revision"), "revision-123\n").expect("selected revision");
    fs::write(
        unselected.join("ambient-fixture.json"),
        r#"{"id":"ambient-fixture","name":"Ambient","version":"1.0.0"}"#,
    )
    .expect("ambient manifest");
    // Stub the runner browser cache so preservation and replay do not
    // depend on whatever cache the host (or an enclosing Lab gate) has.
    let browser_cache = tempfile::tempdir().expect("stub browser cache");
    let _env = EnvVarGuard::set(&[
        ("HOME", ambient.path()),
        ("PLAYWRIGHT_BROWSERS_PATH", browser_cache.path()),
    ]);
    let policy = AgentTaskGateEnvironmentPolicy {
        extension_inputs: vec![AgentTaskGateExtensionInput {
            id: "selected-fixture".to_string(),
            source: selected.display().to_string(),
            identity: None,
        }],
        ..AgentTaskGateEnvironmentPolicy::default()
    };
    let command = "test -f \"$HOME/.config/homeboy/extensions/selected-fixture/selected-fixture.json\" && test ! -e \"$HOME/.config/homeboy/extensions/ambient-fixture\" && printf gate-owned > \"$HOME/.config/homeboy/extensions/selected-fixture/gate-owned\"";

    let candidate = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        worktree.path(),
        1,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(worktree.path()),
        &policy,
        &[],
    )
    .expect("selected extension gate");
    let baseline = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        worktree.path(),
        1,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(worktree.path()),
        &candidate.environment.replay_policy(),
        &[],
    )
    .expect("replayed selected extension gate");

    assert_eq!(candidate.status, AgentTaskGateStatus::Succeeded);
    assert_eq!(candidate.environment.extension_inputs.len(), 1);
    assert_eq!(
        candidate.environment.extension_inputs[0].id,
        "selected-fixture"
    );
    assert!(candidate.environment.extension_inputs[0]
        .identity
        .starts_with("sha256:"));
    assert_eq!(
        candidate.environment.extension_inputs[0]
            .source_revision
            .as_deref(),
        Some("revision-123")
    );
    let copied = worktree
        .path()
        .join("tmp/gate-home/.config/homeboy/extensions/selected-fixture");
    assert!(!fs::symlink_metadata(&copied)
        .expect("copied extension metadata")
        .file_type()
        .is_symlink());
    assert!(copied.join("gate-owned").exists());
    assert!(!selected.join("gate-owned").exists());
    let gate_home = worktree.path().join("tmp/gate-home");
    let gate_config_root = gate_home.join(".config/homeboy");
    assert_eq!(
        homeboy_core::paths::homeboy_root_env_default_under_home(
            homeboy_core::paths::HOMEBOY_CONFIG_ROOT_ENV,
            &gate_home,
        )
        .as_deref(),
        Some(gate_config_root.as_path())
    );
    {
        let _env = EnvVarGuard::set(&[(
            homeboy_core::paths::HOMEBOY_CONFIG_ROOT_ENV,
            gate_config_root.as_path(),
        )]);
        assert_eq!(
            homeboy_core::extension::catalog::available_extension_ids(),
            vec!["selected-fixture"]
        );
    }
    assert_eq!(
        candidate.environment.extension_inputs,
        baseline.environment.extension_inputs
    );
    assert_eq!(
        candidate.environment.preserved["PLAYWRIGHT_BROWSERS_PATH"],
        "PLAYWRIGHT_BROWSERS_PATH"
    );
}

#[cfg(unix)]
#[test]
fn isolated_gate_resolves_a_symlinked_extension_root_with_source_provenance() {
    let _guard = env_mutex();
    let worktree = tempfile::tempdir().expect("worktree");
    let source_root = tempfile::tempdir().expect("extension source root");
    let source = source_root.path().join("selected-fixture");
    let linked_root = tempfile::tempdir().expect("linked extension root");
    let link = linked_root.path().join("selected-fixture");
    fs::create_dir_all(&source).expect("selected extension");
    fs::write(
        source.join("selected-fixture.json"),
        r#"{"id":"selected-fixture","name":"Selected","version":"1.0.0"}"#,
    )
    .expect("selected manifest");
    fs::write(
        linked_root.path().join(".selected-fixture.source-revision"),
        "revision-123\n",
    )
    .expect("linked source revision");
    std::os::unix::fs::symlink(&source, &link).expect("linked extension root");
    let policy = AgentTaskGateEnvironmentPolicy {
        extension_inputs: vec![AgentTaskGateExtensionInput {
            id: "selected-fixture".to_string(),
            source: link.display().to_string(),
            identity: None,
        }],
        ..AgentTaskGateEnvironmentPolicy::default()
    };

    let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        worktree.path(),
        1,
        "test -f \"$HOME/.config/homeboy/extensions/selected-fixture/selected-fixture.json\"",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(worktree.path()),
        &policy,
        &[],
    )
    .expect("symlinked extension gate");

    let input = &report.environment.extension_inputs[0];
    assert_eq!(input.source_revision.as_deref(), Some("revision-123"));
    assert_eq!(
        input.identity,
        extension_tree_identity(&source).expect("source identity")
    );
    let copied = worktree
        .path()
        .join("tmp/gate-home/.config/homeboy/extensions/selected-fixture");
    assert!(!fs::symlink_metadata(&copied)
        .expect("copied extension metadata")
        .file_type()
        .is_symlink());
    assert!(copied.join("selected-fixture.json").is_file());
}

#[test]
fn replay_policy_rejects_extension_content_drift_after_candidate_evidence() {
    let _guard = env_mutex();
    let worktree = tempfile::tempdir().expect("worktree");
    let source = tempfile::tempdir().expect("extension source");
    // Stub the runner HOME and browser cache so preservation and replay
    // are hermetic inside Lab gates and on developer machines alike.
    let host_home = tempfile::tempdir().expect("host home");
    let browser_cache = tempfile::tempdir().expect("stub browser cache");
    let _env = EnvVarGuard::set(&[
        ("HOME", host_home.path()),
        ("PLAYWRIGHT_BROWSERS_PATH", browser_cache.path()),
    ]);
    fs::write(
        source.path().join("selected-fixture.json"),
        "candidate content",
    )
    .expect("candidate content");
    let policy = AgentTaskGateEnvironmentPolicy {
        extension_inputs: vec![AgentTaskGateExtensionInput {
            id: "selected-fixture".to_string(),
            source: source.path().display().to_string(),
            identity: None,
        }],
        ..AgentTaskGateEnvironmentPolicy::default()
    };
    let candidate = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        worktree.path(),
        1,
        "true",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(worktree.path()),
        &policy,
        &[],
    )
    .expect("candidate gate evidence");
    fs::write(
        source.path().join("selected-fixture.json"),
        "drifted content",
    )
    .expect("drifted content");

    let error = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        worktree.path(),
        1,
        "true",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        Some(worktree.path()),
        &candidate.environment.replay_policy(),
        &[],
    )
    .expect_err("replay must reject source content drift");

    assert_eq!(
            error.message,
            "Invalid argument 'gate_environment.extension_inputs': extension input no longer matches the candidate gate identity"
        );
    assert_eq!(error.details["field"], "gate_environment.extension_inputs");
}

#[test]
fn isolated_gate_shared_closure_runs_rust_inventory_and_tests_and_rejects_drift() {
    let _guard = env_mutex();
    let project = tempfile::tempdir().expect("project");
    let root = tempfile::tempdir().expect("extension source root");
    let rust = root.path().join("rust");
    let helpers = root.path().join("scripts/lib");
    fs::create_dir_all(rust.join("scripts")).unwrap();
    fs::create_dir_all(&helpers).unwrap();
    fs::create_dir_all(root.path().join("assets")).unwrap();
    fs::write(
        root.path().join("homeboy-extension-root.json"),
        r#"{"shared_assets":["scripts/lib","assets"]}"#,
    )
    .unwrap();
    fs::write(
        rust.join("rust.json"),
        r#"{"id":"rust","name":"Rust","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(rust.join("scripts/test-runner.sh"),
            "#!/bin/sh\nset -eu\n. \"$(dirname \"$0\")/../../scripts/lib/runner-harness.sh\"\nrun_tests \"$@\"\n").unwrap();
    let helper = helpers.join("runner-harness.sh");
    fs::write(&helper, "run_tests() { \"$@\"; }\n").unwrap();
    fs::write(
        project.path().join("fixture.rs"),
        "#[test] fn copied_runner_reaches_test() { assert_eq!(2 + 2, 4); }\n",
    )
    .unwrap();
    let binary = project.path().join("fixture-test");
    let compiled = Command::new("rustc")
        .arg("--test")
        .arg(project.path().join("fixture.rs"))
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("compile real Rust test harness");
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let input = AgentTaskGateExtensionInput {
        id: "rust".to_string(),
        source: rust.display().to_string(),
        identity: None,
    };
    let runtime = tempfile::tempdir().unwrap();
    let home = runtime.path().join("home");
    let mut candidate = AgentTaskGateEnvironment::default();
    materialize_extension_inputs(&mut candidate, &home, &[input.clone()]).unwrap();
    let copied_runner = home.join(".config/homeboy/extensions/rust/scripts/test-runner.sh");
    for args in [vec!["--list"], vec![]] {
        let output = Command::new("sh")
            .arg(&copied_runner)
            .arg(&binary)
            .args(&args)
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("copied_runner_reaches_test"), "{stdout}");
        if args.is_empty() {
            assert!(stdout.contains("1 passed"), "{stdout}");
        }
    }
    let provenance = &candidate.extension_inputs[0];
    assert_eq!(provenance.shared_assets.len(), 2);
    assert!(home.join(".config/homeboy/extensions/assets").is_dir());
    let original_identity = extension_tree_identity(&rust).unwrap();
    assert_ne!(provenance.identity, original_identity);
    let replay = candidate.replay_policy();
    let baseline_home = runtime.path().join("baseline-home");
    let mut baseline = AgentTaskGateEnvironment::default();
    materialize_extension_inputs(&mut baseline, &baseline_home, &replay.extension_inputs).unwrap();
    assert_eq!(candidate.extension_inputs, baseline.extension_inputs);
    let baseline_output = Command::new("sh")
        .arg(baseline_home.join(".config/homeboy/extensions/rust/scripts/test-runner.sh"))
        .arg(&binary)
        .env("HOME", &baseline_home)
        .output()
        .unwrap();
    assert!(baseline_output.status.success());
    fs::write(
        home.join(".config/homeboy/extensions/scripts/lib/runner-harness.sh"),
        "gate-owned changes\n",
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&helper).unwrap(),
        "run_tests() { \"$@\"; }\n"
    );
    assert_eq!(extension_tree_identity(&rust).unwrap(), original_identity);
    fs::write(&helper, "run_tests() { return 1; }\n").unwrap();
    let error =
        materialize_extension_inputs(&mut baseline, &baseline_home, &replay.extension_inputs)
            .expect_err("shared content drift must fail before execution");
    assert!(error
        .message
        .contains("no longer matches the candidate gate identity"));
    assert_eq!(extension_tree_identity(&rust).unwrap(), original_identity);
    fs::remove_dir_all(&helpers).unwrap();
    assert!(
        materialize_extension_inputs(&mut baseline, &baseline_home, &[input]).is_err(),
        "a missing declared source must not be silently skipped"
    );
}

#[test]
fn isolated_gate_shared_closure_resolves_installed_metadata_and_rejects_unsafe_paths() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("extensions/fixture");
    let shared = root.path().join("extensions/scripts/lib");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&shared).unwrap();
    fs::write(source.join("fixture.json"), "{}").unwrap();
    fs::write(shared.join("settings.sh"), "helper\n").unwrap();
    #[cfg(unix)]
    {
        let linked_source = root.path().join("linked-shared-source");
        fs::rename(&shared, &linked_source).unwrap();
        std::os::unix::fs::symlink(&linked_source, &shared).unwrap();
    }
    let manifest = source.join(".homeboy-extension-root.json");
    fs::write(&manifest, r#"{"shared_assets":["scripts/lib"]}"#).unwrap();
    let home = root.path().join("gate-home");
    let input = AgentTaskGateExtensionInput {
        id: "fixture".to_string(),
        source: source.display().to_string(),
        identity: None,
    };
    let mut report = AgentTaskGateEnvironment::default();
    materialize_extension_inputs(&mut report, &home, &[input.clone()]).unwrap();
    assert!(home
        .join(".config/homeboy/extensions/scripts/lib/settings.sh")
        .is_file());
    assert!(
        !fs::symlink_metadata(home.join(".config/homeboy/extensions/scripts/lib"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::write(
        home.join(".config/homeboy/extensions/scripts/lib/settings.sh"),
        "gate-owned\n",
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(shared.join("settings.sh")).unwrap(),
        "helper\n"
    );
    for path in [
        "../credentials",
        "/host/config",
        "fixture",
        "scripts/lib/../../fixture",
    ] {
        fs::write(
            &manifest,
            serde_json::to_vec(&json!({"shared_assets":[path]})).unwrap(),
        )
        .unwrap();
        assert!(
            materialize_extension_inputs(&mut report, &home, &[input.clone()]).is_err(),
            "{path}"
        );
    }
}

#[test]
fn replacing_gate_environment_preserves_declared_variables_and_reports_policy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let policy = AgentTaskGateEnvironmentPolicy {
        lab_runner: None,
        mode: AgentTaskGateEnvironmentMode::Replace,
        variables: BTreeMap::from([("DECLARED_INPUT".to_string(), "kept".to_string())]),
        preserve: BTreeMap::new(),
        isolate_home: false,
        isolate_xdg: false,
        hydrate_rust_cache: true,
        hydrate_dependencies: true,
        shared_cargo_target: Some(false),
        admitted_component_id: None,
        component_cwd: None,
        extension_inputs: Vec::new(),
        extension_sources: Vec::new(),
        data_inputs: Vec::new(),
        requirements: AgentTaskGateRequirements::default(),
    };
    let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        temp.path(),
        8,
        "test \"$DECLARED_INPUT\" = kept && test -z \"${HOME:-}\"",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        &policy,
        &[],
    )
    .expect("gate report");

    assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
    assert_eq!(
        report.environment.mode,
        AgentTaskGateEnvironmentMode::Replace
    );
    assert_eq!(report.environment.inherited.len(), 1);
    assert_eq!(report.environment.inherited[0].name, "DECLARED_INPUT");
    assert_eq!(report.environment.inherited[0].value, "kept");
}

#[test]
fn playwright_cache_is_mapped_only_for_isolated_non_replace_gate_environments() {
    let home = tempfile::tempdir().expect("host home");
    let cache = home.path().join(PLAYWRIGHT_CACHE_SUBPATH);
    fs::create_dir_all(&cache).expect("browser cache");
    let policy = AgentTaskGateEnvironmentPolicy::default();
    let mut values = BTreeMap::new();
    let mut report = AgentTaskGateEnvironment::default();

    preserve_playwright_cache(&policy, None, Some(home.path()), &mut values, &mut report);

    assert_eq!(
        values["PLAYWRIGHT_BROWSERS_PATH"],
        cache.display().to_string()
    );
    assert_eq!(
        report.preserved["PLAYWRIGHT_BROWSERS_PATH"],
        format!("HOME/{PLAYWRIGHT_CACHE_SUBPATH}")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn isolated_gate_preserves_native_macos_browser_cache_without_host_credentials() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let cache = home.path().join("Library/Caches/ms-playwright");
        fs::create_dir_all(&cache).expect("native browser cache");
        fs::write(cache.join("ready-browser"), "installed resource").expect("browser resource");
        fs::write(home.path().join(".npmrc"), "private host configuration").expect("host config");
        let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            home.path(),
            1,
            "test -f \"$PLAYWRIGHT_BROWSERS_PATH/ready-browser\" && test ! -e \"$HOME/.npmrc\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("isolated gate");
        assert_eq!(
            report.status,
            AgentTaskGateStatus::Succeeded,
            "{}",
            report.stderr
        );
        assert_eq!(
            report.environment.preserved["PLAYWRIGHT_BROWSERS_PATH"],
            "HOME/Library/Caches/ms-playwright"
        );
    });
}

#[test]
fn playwright_cache_mapping_respects_absence_explicit_values_and_replace_mode() {
    // Absent cache: nothing is injected.
    let empty_home = tempfile::tempdir().expect("host home without cache");
    let mut values = BTreeMap::new();
    let mut report = AgentTaskGateEnvironment::default();
    preserve_playwright_cache(
        &AgentTaskGateEnvironmentPolicy::default(),
        None,
        Some(empty_home.path()),
        &mut values,
        &mut report,
    );
    assert!(values.is_empty());
    assert!(report.preserved.is_empty());

    // No host HOME: nothing is injected.
    preserve_playwright_cache(
        &AgentTaskGateEnvironmentPolicy::default(),
        None,
        None,
        &mut values,
        &mut report,
    );
    assert!(values.is_empty());

    let home = tempfile::tempdir().expect("host home");
    fs::create_dir_all(home.path().join(PLAYWRIGHT_CACHE_SUBPATH)).expect("browser cache");

    // Each case declares its expected output and provenance explicitly.
    let explicit = "/explicit/cache".to_string();
    let home_cache = home
        .path()
        .join(PLAYWRIGHT_CACHE_SUBPATH)
        .display()
        .to_string();
    let declared_dir = tempfile::tempdir().expect("declared runner cache");
    let declared_cache = declared_dir.path().to_path_buf();
    let other_home = tempfile::tempdir().expect("runner host home");
    fs::create_dir_all(other_home.path().join(PLAYWRIGHT_CACHE_SUBPATH)).expect("home cache");
    for (policy, declared, host, expected_value, expected_preserved) in [
        (
            AgentTaskGateEnvironmentPolicy {
                variables: BTreeMap::from([(
                    "PLAYWRIGHT_BROWSERS_PATH".to_string(),
                    explicit.clone(),
                )]),
                ..AgentTaskGateEnvironmentPolicy::default()
            },
            None,
            Some(home.path()),
            Some(explicit.clone()),
            None,
        ),
        (
            AgentTaskGateEnvironmentPolicy {
                preserve: BTreeMap::from([(
                    "PLAYWRIGHT_BROWSERS_PATH".to_string(),
                    "HOME/custom".to_string(),
                )]),
                ..AgentTaskGateEnvironmentPolicy::default()
            },
            None,
            Some(home.path()),
            None,
            None,
        ),
        (
            AgentTaskGateEnvironmentPolicy {
                mode: AgentTaskGateEnvironmentMode::Replace,
                ..AgentTaskGateEnvironmentPolicy::default()
            },
            None,
            Some(home.path()),
            Some(home_cache.clone()),
            Some(format!("HOME/{PLAYWRIGHT_CACHE_SUBPATH}")),
        ),
        (
            AgentTaskGateEnvironmentPolicy {
                isolate_home: false,
                isolate_xdg: false,
                ..AgentTaskGateEnvironmentPolicy::default()
            },
            None,
            Some(home.path()),
            None,
            None,
        ),
        (
            AgentTaskGateEnvironmentPolicy::default(),
            Some(declared_cache.as_path()),
            Some(other_home.path()),
            Some(declared_cache.display().to_string()),
            Some("PLAYWRIGHT_BROWSERS_PATH".to_string()),
        ),
    ] {
        values.clear();
        report.preserved.clear();
        if let Some(value) = policy.variables.get("PLAYWRIGHT_BROWSERS_PATH") {
            values.insert("PLAYWRIGHT_BROWSERS_PATH".to_string(), value.clone());
        }
        preserve_playwright_cache(&policy, declared, host, &mut values, &mut report);
        assert_eq!(
            values.get("PLAYWRIGHT_BROWSERS_PATH"),
            expected_value.as_ref()
        );
        assert_eq!(
            report.preserved.get("PLAYWRIGHT_BROWSERS_PATH"),
            expected_preserved.as_ref()
        );
    }
}

#[test]
fn declared_package_artifact_maps_resource_preserves_isolation_and_records_provenance() {
    let workspace = tempfile::tempdir().expect("workspace");
    let artifact = workspace.path().join("fixtures/ready.bin");
    fs::create_dir_all(artifact.parent().expect("artifact parent")).expect("artifact parent");
    fs::write(&artifact, b"neutral fixture").expect("artifact");
    let digest = format!("sha256:{:x}", Sha256::digest(b"neutral fixture"));
    let requirement = AgentTaskGatePackageArtifactRequirement {
        id: "fixture-resource".to_string(),
        environment: AgentTaskGateArtifactEnvironmentMapping {
            name: "FIXTURE_RESOURCE_ROOT".to_string(),
            source: None,
            default: Some("fixtures".to_string()),
        },
        required_paths: vec![AgentTaskGateArtifactPathRequirement {
            path: "fixtures/ready.bin".to_string(),
            sha256: Some(digest),
        }],
        remediation: json!({"action": "refresh_fixture_resource"}),
    };

    let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        workspace.path(),
        1,
        "test \"$FIXTURE_RESOURCE_ROOT\" = fixtures && test \"$HOME\" != fixtures",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        &AgentTaskGateEnvironmentPolicy::default(),
        &[requirement],
    )
    .expect("declared resource is gate-ready");

    assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
    assert_eq!(report.environment.package_artifacts.len(), 1);
    assert_eq!(
        report.environment.package_artifacts[0].id,
        "fixture-resource"
    );
    assert_eq!(
        report.environment.package_artifacts[0].artifacts[0].path,
        "fixtures/ready.bin"
    );
}

#[test]
fn missing_declared_package_artifact_stops_preflight_with_caller_remediation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let requirement = AgentTaskGatePackageArtifactRequirement {
        id: "fixture-resource".to_string(),
        environment: AgentTaskGateArtifactEnvironmentMapping {
            name: "FIXTURE_RESOURCE_ROOT".to_string(),
            source: None,
            default: Some("fixtures".to_string()),
        },
        required_paths: vec![AgentTaskGateArtifactPathRequirement {
            path: "fixtures/missing.bin".to_string(),
            sha256: None,
        }],
        remediation: json!({"action": "refresh_fixture_resource"}),
    };

    let error = preflight_gate_toolchains(
        workspace.path(),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
        &[requirement],
        None,
        Duration::from_secs(1),
    )
    .expect_err("missing resource must stop provider preflight");

    assert_eq!(
        error.details["package_artifact_readiness"]["invalid_paths"],
        json!(["fixtures/missing.bin"])
    );
    assert_eq!(
        error.details["package_artifact_readiness"]["remediation"]["action"],
        "refresh_fixture_resource"
    );
}

/// #18121: a `homeboy review test` probe the Lab transport never ran is
/// not a broken gate declaration; only a probe that ran and failed is.
#[test]
fn gate_target_probe_transport_failure_is_not_a_declaration_error() {
    let probe = AgentTaskGateToolchainRequirement {
        resolve_only: false,
        command: "homeboy".to_string(),
        probe_arguments: vec!["review".to_string(), "test".to_string()],
    };
    let transport = Error::new(
            homeboy_core::ErrorCode::RunnerLabTransportFailure,
            "Lab gate transport did not produce verified passing evidence: runner gate did not return terminal artifact evidence",
            json!({"stage": "terminal_projection", "gate_disposition": "unavailable"}),
        )
        .with_retryable(true);
    let error =
        gate_target_preflight_error("homeboy review test fixture", &probe, transport.clone());
    assert_eq!(error.code, transport.code);
    assert!(placement::is_lab_gate_transport_failure(&error));
    assert_ne!(error.details["failure_classification"], "gate_declaration");

    let ran_and_failed = Error::internal_unexpected("component fixture has no test capability");
    let error = gate_target_preflight_error("homeboy review test fixture", &probe, ran_and_failed);
    assert_eq!(error.details["failure_classification"], "gate_declaration");
    assert_eq!(error.retryable, Some(false));
}

#[cfg(unix)]
#[test]
fn declared_environment_probe_checks_external_browser_cache_before_suite() {
    let _guard = env_mutex();
    let workspace = tempfile::tempdir().expect("workspace");
    let host_home = tempfile::tempdir().expect("host home");
    let browser_cache = host_home.path().join(".cache/browser-fixture");
    fs::create_dir_all(&browser_cache).expect("browser cache");
    fs::write(
        host_home.path().join(".npmrc"),
        "//registry.invalid/:_authToken=secret",
    )
    .expect("host credential fixture");
    let browser = browser_cache.join("chromium");
    homeboy_test_support::write_executable(
        &browser,
        "#!/bin/sh\nprintf 'Chromium 128.4\\n'\n",
        0o755,
    )
    .expect("browser fixture");

    let expected_version = browser_cache.join("expected-version");
    fs::write(&expected_version, "Chromium 128.4\n").expect("expected browser version");
    fs::write(workspace.path().join("suite-ran"), "absent").expect("suite sentinel");
    let candidate_fingerprint =
        Sha256::digest(fs::read(workspace.path().join("suite-ran")).expect("suite sentinel"));
    let _environment = EnvVarGuard::set(&[("HOME", host_home.path())]);
    let policy = AgentTaskGateEnvironmentPolicy {
        preserve: BTreeMap::from([(
            "PLAYWRIGHT_BROWSERS_PATH".to_string(),
            "HOME/.cache/browser-fixture".to_string(),
        )]),
        ..AgentTaskGateEnvironmentPolicy::default()
    };
    let requirement = AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: "sh".to_string(),
            probe_arguments: vec![
                "-c".to_string(),
                r#"test -x "$PLAYWRIGHT_BROWSERS_PATH/chromium" && test "$(cat "$PLAYWRIGHT_BROWSERS_PATH/expected-version")" = "$("$PLAYWRIGHT_BROWSERS_PATH/chromium" --version)" && test ! -e "$HOME/.npmrc""#.to_string(),
            ],
        };

    let missing_cache = tempfile::tempdir().expect("missing cache home");
    let _missing_home = EnvVarGuard::set(&[("HOME", missing_cache.path())]);
    let error = preflight_gate_toolchains(
        workspace.path(),
        &policy,
        std::slice::from_ref(&requirement),
        &[],
        None,
        Duration::from_secs(10),
    )
    .expect_err("missing mapped browser cache must stop before suite execution");
    assert!(error
        .to_string()
        .contains("gate toolchain preflight failed"));
    assert!(error.details["toolchain_preflight"]["remediation"]
        .as_str()
        .expect("structured remediation")
        .contains("--gate-env-from"));
    assert_eq!(
        fs::read_to_string(workspace.path().join("suite-ran")).expect("suite sentinel"),
        "absent"
    );

    // Repoint the declared host source to the installed cache. The child
    // sees the deliberate mapping and isolated HOME, not host credentials.
    let _installed_home = EnvVarGuard::set(&[("HOME", host_home.path())]);
    preflight_gate_toolchains(
        workspace.path(),
        &policy,
        std::slice::from_ref(&requirement),
        &[],
        None,
        Duration::from_secs(10),
    )
    .expect("installed executable with exact expected version passes");

    fs::write(&expected_version, "Chromium 128.5\n").expect("mismatched version");
    let error = preflight_gate_toolchains(
        workspace.path(),
        &policy,
        &[requirement],
        &[],
        None,
        Duration::from_secs(10),
    )
    .expect_err("mismatched browser version must fail preflight");
    assert!(error
        .to_string()
        .contains("gate toolchain preflight failed"));
    assert_eq!(
        fs::read_to_string(workspace.path().join("suite-ran")).expect("suite sentinel"),
        "absent"
    );
    assert_eq!(
        Sha256::digest(fs::read(workspace.path().join("suite-ran")).expect("suite sentinel")),
        candidate_fingerprint
    );
    assert!(!workspace.path().join(".npmrc").exists());
}

/// Toolchain preflight is declared, never inferred. A gate command is a
/// shell program: its first token can be a builtin, a provider-owned alias,
/// or part of a compound expression, so probing it as an executable would
/// change what the gate means. `c3462d472` made preflight opt-in for that
/// reason and this test tracked the removed inference until #10658.
#[test]
fn gate_toolchain_requirements_are_explicit() {
    let options = VerifyGateOptions {
        verify: vec!["cargo test --lib".to_string()],
        private_verify: vec!["npm test".to_string()],
        gate_toolchains: vec![AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: "cargo".to_string(),
            probe_arguments: vec!["metadata".to_string()],
        }],
        ..VerifyGateOptions::default()
    };

    assert_eq!(
        options.required_toolchains(),
        vec![AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: "cargo".to_string(),
            probe_arguments: vec!["metadata".to_string()],
        }]
    );
}

/// A gate command whose first token is a shell builtin or part of a
/// compound expression declares no toolchain at all. `c3462d472` made
/// preflight opt-in precisely because a gate command is a shell program,
/// so its first token is not necessarily an executable to probe.
#[test]
fn shell_gate_commands_contribute_no_inferred_toolchain() {
    let options = VerifyGateOptions {
        verify: vec![
            "test -f Cargo.toml && cargo test --lib".to_string(),
            "[ -d target ]".to_string(),
        ],
        private_verify: vec!["npm test".to_string()],
        ..VerifyGateOptions::default()
    };

    assert!(options.required_toolchains().is_empty());
}

#[cfg(unix)]
#[test]
fn toolchain_preflight_preserves_only_declared_homes_for_cargo_on_path() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(
        module_path!(),
        "toolchain_preflight_preserves_only_declared_homes_for_cargo_on_path",
    ) {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let original_home = tempfile::tempdir().expect("original home");
    let cargo_bin = original_home.path().join(".cargo/bin");
    fs::create_dir_all(&cargo_bin).expect("cargo bin");
    let cargo = cargo_bin.join("cargo");
    homeboy_test_support::write_executable(&cargo, format!(
                "#!/bin/sh\ntest \"$RUSTUP_HOME\" = \"{}/.rustup\" && test \"$CARGO_HOME\" = \"{}/.cargo\"\n",
                original_home.path().display(),
                original_home.path().display(),
            ), 0o755)
        .expect("cargo probe");

    let _env = EnvVarGuard::set(&[
        ("HOME", original_home.path()),
        ("PATH", cargo_bin.as_path()),
    ]);

    let policy = AgentTaskGateEnvironmentPolicy {
        preserve: BTreeMap::from([
            ("RUSTUP_HOME".to_string(), "HOME/.rustup".to_string()),
            ("CARGO_HOME".to_string(), "HOME/.cargo".to_string()),
        ]),
        ..AgentTaskGateEnvironmentPolicy::default()
    };
    let result = preflight_gate_toolchains(
        workspace.path(),
        &policy,
        &[AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: "cargo".to_string(),
            probe_arguments: vec!["--version".to_string()],
        }],
        &[],
        None,
        Duration::from_secs(10),
    );

    result.expect("declared cargo homes initialize the toolchain");
}

/// #15453: the host PATH carries no rustc (rustup lives in ~/.cargo/bin and
/// the daemon started outside a login shell). Toolchain cargo spawns rustc
/// by name during `fetch --locked`, so hydration must put the toolchain bin
/// on PATH the way gate execution does.
#[cfg(unix)]
#[test]
fn rust_gate_cache_hydration_resolves_rustc_from_toolchain_bin() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(
        module_path!(),
        "rust_gate_cache_hydration_resolves_rustc_from_toolchain_bin",
    ) {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let cache = tempfile::tempdir().expect("cache");
    let fixtures = tempfile::tempdir().expect("fixtures");
    let host_bin = tempfile::tempdir().expect("host bin");

    let toolchain_cargo = fixtures.path().join("cargo");
    homeboy_test_support::write_executable(
        &toolchain_cargo,
        "#!/bin/sh\nrustc -vV >/dev/null || exit 101\n",
        0o755,
    )
    .expect("toolchain cargo fixture");
    let toolchain_rustc = fixtures.path().join("rustc");
    homeboy_test_support::write_executable(
        &toolchain_rustc,
        "#!/bin/sh\necho 'rustc 1.95.0-test'\n",
        0o755,
    )
    .expect("toolchain rustc fixture");
    let rustup = host_bin.path().join("rustup");
    homeboy_test_support::write_executable(&rustup, format!(
                "#!/bin/sh\nbin=\"$RUSTUP_HOME/toolchains/1.95.0-test/bin\"\nif test \"$1\" = toolchain; then\n  /bin/mkdir -p \"$bin\"\n  /bin/cp \"{}\" \"$bin/cargo\"\n  /bin/cp \"{}\" \"$bin/rustc\"\nelif test \"$1\" = which; then\n  printf '%s\\n' \"$bin/cargo\"\nfi\n",
                toolchain_cargo.display(),
                toolchain_rustc.display()
            ), 0o755)
        .expect("rustup fixture");

    let _environment = EnvVarGuard::set(&[("PATH", host_bin.path())]);

    let relative = hydrate_rust_cache_with_timeout(
        workspace.path(),
        cache.path(),
        Duration::from_secs(10),
        None,
    )
    .expect("fetch_locked resolves rustc from the toolchain bin");
    assert_eq!(
        relative,
        PathBuf::from("toolchains/1.95.0-test/bin/cargo"),
        "hydration reports the toolchain cargo relative to RUSTUP_HOME"
    );
}

#[cfg(unix)]
#[test]
fn rust_gate_cache_hydrates_once_coordinates_waiters_and_separates_identities() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(
        module_path!(),
        "rust_gate_cache_hydrates_once_coordinates_waiters_and_separates_identities",
    ) {
        return;
    }
    let root = tempfile::tempdir().expect("controller data");
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime = tempfile::tempdir().expect("runtime");
    let bin = tempfile::tempdir().expect("tool bin");
    fs::write(workspace.path().join("Cargo.lock"), "version = 4\n").expect("lockfile");
    fs::write(
        workspace.path().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.95.0\"\n",
    )
    .expect("toolchain");
    let direct_cargo = bin.path().join("direct-cargo");
    homeboy_test_support::write_executable(
        &direct_cargo,
        "#!/bin/sh\nprintf '%s\\n' cargo >> \"$CARGO_HOME/registry/hydration.log\"\n",
        0o755,
    )
    .expect("direct cargo fixture");
    let rustup = bin.path().join("rustup");
    homeboy_test_support::write_executable(&rustup, format!(
                "#!/bin/sh\ntest \"$0\" = \"$CARGO_HOME/bin/rustup\"\nif test \"$1\" = toolchain; then\n  /bin/mkdir -p \"$CARGO_HOME/registry\" \"$RUSTUP_HOME/toolchains/1.95.0-test/bin\"\n  /bin/cp \"{}\" \"$RUSTUP_HOME/toolchains/1.95.0-test/bin/cargo\"\n  printf '%s\\n' rustup >> \"$CARGO_HOME/registry/hydration.log\"\nelif test \"$1\" = which; then\n  printf '%s\\n' \"$RUSTUP_HOME/toolchains/1.95.0-test/bin/cargo\"\nfi\n/bin/sleep 0.1\n",
                direct_cargo.display()
            ), 0o755)
        .expect("rustup fixture");
    let cargo = bin.path().join("cargo");
    homeboy_test_support::write_executable(
        &cargo,
        "#!/bin/sh\necho 'host rustup proxy must not execute' >&2\nexit 70\n",
        0o755,
    )
    .expect("host cargo proxy fixture");

    let _environment = EnvVarGuard::set(&[
        (homeboy_core::paths::HOMEBOY_DATA_DIR_ENV, root.path()),
        ("PATH", bin.path()),
    ]);
    let policy = AgentTaskGateEnvironmentPolicy {
        isolate_home: true,
        isolate_xdg: true,
        ..AgentTaskGateEnvironmentPolicy::default()
    };
    let cache_identity = rust_cache_identity(workspace.path()).expect("cache identity");
    let hydration_log = root
        .path()
        .join("controller-state/gate-rust-cache")
        .join(&cache_identity)
        .join("cargo/registry/hydration.log");

    let states = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let mut environment = selected_gate_environment(&policy, Some(runtime.path()))
                .expect("first environment");
            environment
                .configure_rust_cache(workspace.path())
                .expect("first hydration");
            let evidence = environment.report.rust_cache.expect("first evidence");
            if evidence.state == "hydrated" {
                assert!(evidence.hydrate_ms.is_some());
            }
            evidence.state
        });
        let second = scope.spawn(|| {
            let mut environment = selected_gate_environment(&policy, Some(runtime.path()))
                .expect("second environment");
            environment
                .configure_rust_cache(workspace.path())
                .expect("second hydration");
            environment
                .report
                .rust_cache
                .expect("second evidence")
                .state
        });
        [
            first.join().expect("first thread"),
            second.join().expect("second thread"),
        ]
    });
    assert!(states.contains(&"hydrated".to_string()), "{states:?}");
    assert!(
        states.iter().any(|state| state == "wait" || state == "hit"),
        "{states:?}"
    );
    assert_eq!(
        fs::read_to_string(&hydration_log)
            .expect("hydration log")
            .lines()
            .count(),
        2
    );

    let mut repeated =
        selected_gate_environment(&policy, Some(runtime.path())).expect("repeat environment");
    repeated
        .configure_rust_cache(workspace.path())
        .expect("cache hit");
    for name in ["CARGO_HOME", "RUSTUP_HOME"] {
        let value = repeated.values.get(name).expect("gate overlay path");
        assert!(value.starts_with(repeated.values.get("HOME").expect("isolated HOME")));
        assert!(!value.starts_with(root.path().to_string_lossy().as_ref()));
    }
    let cargo_on_path = std::env::split_paths(&std::ffi::OsString::from(
        repeated.values.get("PATH").expect("gate PATH"),
    ))
    .next()
    .expect("toolchain bin on PATH")
    .join("cargo");
    assert!(cargo_on_path.is_file());
    assert!(cargo_on_path.starts_with(
        repeated
            .values
            .get("RUSTUP_HOME")
            .expect("isolated RUSTUP_HOME")
    ));
    assert_eq!(
        repeated.report.rust_cache.expect("hit evidence").state,
        "hit"
    );
    assert_eq!(
        fs::read_to_string(&hydration_log)
            .expect("hydration log")
            .lines()
            .count(),
        2
    );

    let first_identity = rust_cache_identity(workspace.path()).expect("first identity");
    fs::write(workspace.path().join("Cargo.lock"), "version = 5\n").expect("different lockfile");
    assert_eq!(
        first_identity,
        rust_cache_identity(workspace.path()).expect("second identity")
    );
    let mut incremental =
        selected_gate_environment(&policy, Some(runtime.path())).expect("fetch environment");
    incremental
        .configure_rust_cache(workspace.path())
        .expect("incremental fetch");
    let evidence = incremental.report.rust_cache.expect("fetch evidence");
    assert_eq!(evidence.state, "fetched");
    assert!(evidence.hydrate_ms.is_some());
    assert_eq!(
        fs::read_to_string(&hydration_log).expect("fetch log"),
        "rustup\ncargo\ncargo\n",
        "a lockfile change must only run the fetch phase"
    );
    let mut hit =
        selected_gate_environment(&policy, Some(runtime.path())).expect("hit environment");
    hit.configure_rust_cache(workspace.path())
        .expect("updated cache hit");
    let evidence = hit.report.rust_cache.expect("updated hit evidence");
    assert_eq!(evidence.state, "hit");
    assert_eq!(evidence.hydrate_ms, None);
    assert_eq!(
        fs::read_to_string(&hydration_log).expect("hit log"),
        "rustup\ncargo\ncargo\n"
    );
    fs::write(
        workspace.path().join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.96.0\"\n",
    )
    .expect("different toolchain");
    assert_ne!(
        first_identity,
        rust_cache_identity(workspace.path()).expect("third identity")
    );
    let toolchain_identity = rust_cache_identity(workspace.path()).expect("toolchain identity");
    fs::create_dir_all(workspace.path().join(".cargo")).expect("cargo config directory");
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        "[net]\noffline = true\n",
    )
    .expect("cargo config");
    assert_ne!(
        toolchain_identity,
        rust_cache_identity(workspace.path()).expect("config identity")
    );
}

#[cfg(unix)]
#[test]
fn rust_gate_overlay_shares_installed_toolchain_and_cleans_up() {
    use std::os::unix::fs::{symlink, MetadataExt};

    let runner = tempfile::tempdir().expect("runner");
    let cache = tempfile::tempdir().expect("cache");
    let gate = tempfile::tempdir().expect("gate");
    let toolchain = runner.path().join(".rustup/toolchains/test");
    fs::create_dir_all(toolchain.join("bin")).unwrap();
    fs::write(toolchain.join("bin/rustc"), "shared compiler").unwrap();
    fs::create_dir_all(cache.path().join("toolchains")).unwrap();
    symlink(&toolchain, cache.path().join("toolchains/test")).unwrap();
    fs::write(cache.path().join("settings.toml"), "isolated settings").unwrap();

    let overlay = tempfile::tempdir_in(gate.path()).unwrap();
    hydrate_gate_rustup(cache.path(), overlay.path()).unwrap();
    let compiler = overlay.path().join("toolchains/test/bin/rustc");
    assert_eq!(
        compiler.canonicalize().unwrap(),
        toolchain.join("bin/rustc")
    );
    assert_eq!(
        fs::metadata(&compiler).unwrap().ino(),
        fs::metadata(toolchain.join("bin/rustc")).unwrap().ino()
    );
    fs::write(overlay.path().join("settings.toml"), "gate settings").unwrap();
    assert_eq!(
        fs::read_to_string(cache.path().join("settings.toml")).unwrap(),
        "isolated settings"
    );
    let overlay_path = overlay.path().to_path_buf();
    drop(overlay);
    assert!(!overlay_path.exists());
    assert!(toolchain.join("bin/rustc").is_file());
}

#[cfg(unix)]
#[test]
fn rust_gate_cache_rejects_corrupt_markers_and_unsafe_roots() {
    use std::os::unix::fs::PermissionsExt;

    let marker = tempfile::NamedTempFile::new().expect("marker");
    fs::write(marker.path(), "not json").expect("corrupt marker");
    let cache = tempfile::tempdir().expect("cache");
    assert!(valid_marker_toolchain_cargo(marker.path(), "identity", cache.path()).is_err());

    let root = tempfile::tempdir().expect("cache root");
    let mut permissions = fs::metadata(root.path()).expect("metadata").permissions();
    permissions.set_mode(0o777);
    fs::set_permissions(root.path(), permissions).expect("unsafe permissions");
    assert!(ensure_safe_rust_cache_root(root.path()).is_err());
}

#[cfg(unix)]
#[test]
fn rust_gate_cache_preserves_safe_symlinks_and_rejects_unsafe_content() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().expect("source");
    let overlay = tempfile::tempdir().expect("overlay");
    fs::create_dir_all(source.path().join("bin")).expect("cache bin");
    let rustup = source.path().join("bin/rustup");
    homeboy_test_support::write_executable(&rustup, "#!/bin/sh\nprintf 'rustup\\n'\n", 0o755)
        .expect("rustup proxy");

    symlink("rustup", source.path().join("bin/cargo")).expect("normal cargo proxy");
    symlink(&rustup, source.path().join("absolute-rustup")).expect("absolute safe link");
    copy_tree_preserving_safe_symlinks(source.path(), overlay.path()).expect("copy cache");
    assert_eq!(
        fs::read_link(overlay.path().join("bin/cargo")).expect("relative cargo link"),
        PathBuf::from("rustup")
    );
    assert_eq!(
        fs::read_link(overlay.path().join("absolute-rustup")).expect("absolute rustup link"),
        overlay.path().join("bin/rustup")
    );
    assert_eq!(
        Command::new(overlay.path().join("bin/cargo"))
            .output()
            .expect("execute copied cargo proxy")
            .stdout,
        b"rustup\n"
    );

    let reused_overlay = tempfile::tempdir().expect("reused overlay");
    copy_tree_preserving_safe_symlinks(source.path(), reused_overlay.path()).expect("reuse cache");
    assert_eq!(
        fs::read_link(reused_overlay.path().join("bin/cargo")).expect("reused cargo link"),
        PathBuf::from("rustup")
    );

    for (name, target, expected) in [
        ("escape", PathBuf::from("/tmp"), "escapes its source root"),
        ("dangling", PathBuf::from("missing"), "dangling target"),
        ("cycle-a", PathBuf::from("cycle-b"), "cycle"),
    ] {
        let malicious = tempfile::tempdir().expect("malicious cache");
        symlink(&target, malicious.path().join(name)).expect("malicious symlink");
        if name == "cycle-a" {
            symlink("cycle-a", malicious.path().join("cycle-b")).expect("cycle link");
        }
        let error = copy_tree_preserving_safe_symlinks(
            malicious.path(),
            tempfile::tempdir().expect("malicious overlay").path(),
        )
        .expect_err("reject malicious cache link");
        let diagnostic = error.details["error"].as_str().unwrap_or_default();
        assert!(diagnostic.contains(name), "{diagnostic}");
        assert!(diagnostic.contains(expected), "{diagnostic}");
    }
}

#[cfg(unix)]
#[test]
fn rust_gate_cache_bounds_stalled_hydration() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(module_path!(), "rust_gate_cache_bounds_stalled_hydration") {
        return;
    }
    let bin = tempfile::tempdir().expect("tool bin");
    let workspace = tempfile::tempdir().expect("workspace");
    let cache = tempfile::tempdir().expect("cache");
    for tool in ["rustup", "cargo"] {
        let path = bin.path().join(tool);
        homeboy_test_support::write_executable(&path, "#!/bin/sh\n/bin/sleep 1\n", 0o755)
            .expect("tool fixture");
    }
    let _environment = EnvVarGuard::set(&[("PATH", bin.path())]);
    let started = Instant::now();
    assert!(hydrate_rust_cache_with_timeout(
        workspace.path(),
        cache.path(),
        Duration::from_millis(50),
        None,
    )
    .is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[cfg(unix)]
#[test]
fn toolchain_preflight_reports_generic_initialization_failures_without_code_feedback() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(
        module_path!(),
        "toolchain_preflight_reports_generic_initialization_failures_without_code_feedback",
    ) {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = tempfile::tempdir().expect("bin");
    let tool = bin.path().join("other-tool");
    homeboy_test_support::write_executable(
        &tool,
        "#!/bin/sh\necho unavailable >&2\nexit 7\n",
        0o755,
    )
    .expect("tool");

    let prior_path = std::env::var_os("PATH");
    std::env::set_var("PATH", bin.path());

    let error = preflight_gate_toolchains(
        workspace.path(),
        &AgentTaskGateEnvironmentPolicy::default(),
        &[AgentTaskGateToolchainRequirement {
            resolve_only: false,
            command: "other-tool".to_string(),
            probe_arguments: vec!["initialize".to_string()],
        }],
        &[],
        None,
        Duration::from_secs(10),
    )
    .expect_err("unusable declared toolchain fails preflight");

    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    assert!(error.message.contains("gate toolchain preflight"));
    assert!(!error.message.contains("Fix the code"));
}

#[test]
fn homeboy_review_test_gates_derive_a_preflight_only_probe() {
    let probe = |command: &str| {
        homeboy_review_test_probe(command).map(|probe| {
            assert_eq!(probe.command, "homeboy");
            probe.probe_arguments.join(" ")
        })
    };
    assert_eq!(
        probe("homeboy review test blocks-engine").as_deref(),
        Some("review test blocks-engine --preflight-only")
    );
    // Passthrough runner arguments are not resolution inputs.
    assert_eq!(
        probe("homeboy review test demo --setting mode=fast -- --filter unit").as_deref(),
        Some("review test demo --setting mode=fast --preflight-only")
    );
    for command in [
        "homeboy review lint demo",
        "cd php && composer test",
        "homeboy review test demo && echo ok",
        "homeboy review test $COMPONENT",
        "./homeboy review test demo",
    ] {
        assert_eq!(probe(command), None, "{command}");
    }
}

/// A nested `homeboy review test` gate whose component does not resolve in
/// the gate environment is rejected at admission as a gate declaration,
/// before a provider runs (Extra-Chill/homeboy#16159). A gate that resolves
/// passes the same probe.
#[cfg(unix)]
#[test]
fn gate_preflight_rejects_an_unresolvable_homeboy_gate_as_a_declaration() {
    let _guard = env_mutex();
    if !rerun_alone_to_replace_path(
        module_path!(),
        "gate_preflight_rejects_an_unresolvable_homeboy_gate_as_a_declaration",
    ) {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = tempfile::tempdir().expect("bin");
    let homeboy = bin.path().join("homeboy");
    // Stands in for the real CLI: only `--preflight-only` is allowed, and
    // only `ready-component` resolves.
    homeboy_test_support::write_executable(&homeboy, r#"#!/bin/sh
eval "last=\${$#}"
[ "$last" = "--preflight-only" ] || { echo "probe ran the gate" >&2; exit 9; }
[ "$3" = "ready-component" ] && { echo '{"success":true,"data":{"preflight":"ready"}}'; exit 0; }
echo '{"success":false,"diagnostics":{"code":"component.not_found","message":"Component not found"}}'
exit 2
"#, 0o755)
        .expect("homeboy");

    let prior_path = std::env::var_os("PATH");
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.path().display()));

    let gates = |command: &str| VerifyGateOptions {
        verify: vec![command.to_string()],
        ..VerifyGateOptions::default()
    };
    let ready = preflight_verify_gate_options(
        workspace.path(),
        &gates("homeboy review test ready-component"),
    );
    let rejected = preflight_verify_gate_options(
        workspace.path(),
        &gates("homeboy review test missing-component"),
    );

    match prior_path {
        Some(value) => std::env::set_var("PATH", value),
        None => std::env::remove_var("PATH"),
    }
    ready.expect("a resolvable gate passes admission");
    let error = rejected.expect_err("an unresolvable gate is rejected at admission");
    assert_eq!(error.details["failure_classification"], "gate_declaration");
    assert_eq!(
        error.details["command"],
        "homeboy review test missing-component"
    );
    assert_eq!(
        error.details["probe"]["arguments"],
        serde_json::json!(["review", "test", "missing-component", "--preflight-only"])
    );
    assert_eq!(error.retryable, Some(false));
    assert!(
        error.message.contains("cannot run in the gate environment")
            && !error.message.contains("toolchain"),
        "{}",
        error.message
    );
}

/// #18869: a bench gate's report carries the compare verdict and artifact it
/// announced on stderr, and the base never explains its failure.
#[test]
fn bench_gate_report_carries_compare_evidence_and_is_not_base_explainable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let evidence = homeboy_core::bench_compare::BenchCompareGateEvidence {
        schema: homeboy_core::bench_compare::GATE_EVIDENCE_SCHEMA.to_string(),
        compare_id: "bench-compare-1".to_string(),
        passed: false,
        fail_on: homeboy_core::bench_compare::BenchCompareFailOn::NewBlockers,
        summary: homeboy_core::bench_compare::BenchCompareSummary {
            routes: 1,
            regressed: 1,
            new_blockers: 1,
            ..Default::default()
        },
        violations: vec!["head introduced 1 new evaluator blocker(s)".to_string()],
        artifact_path: "/data/bench-compare/1/bench-compare.json".to_string(),
    };
    std::fs::write(
        temp.path().join("evidence.txt"),
        format!("{}\n", evidence.marker_line()),
    )
    .expect("evidence");
    let command = "homeboy() { cat evidence.txt >&2; return 1; }; homeboy bench compare --cook-gate --base base --head WORKTREE --path .";

    let report = run_gate_command(temp.path(), 1, command).expect("gate report");

    assert_eq!(report.status, AgentTaskGateStatus::Failed);
    assert_eq!(report.bench_compare, Some(evidence));
    assert!(!base_explainable(command, &[]));
    assert!(base_explainable("cargo test", &[]));
    assert!(!base_explainable("cargo test", &["cargo test".to_string()]));

    let plain = run_gate_command(temp.path(), 2, "cat evidence.txt >&2").expect("plain gate");
    assert_eq!(plain.bench_compare, None, "only bench gates carry evidence");
}
