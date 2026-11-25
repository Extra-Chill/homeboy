use super::*;

#[test]
fn gate_phase_slug_is_stable_and_bounded() {
    for (label, command, expected) in [
        (Some("Cargo TEST!"), "ignored", "gate.cargo-test"),
        (None, "set -eu\n\ncargo fmt --check", "gate.cargo-fmt-check"),
        (None, "set -e\n!!!", "gate.gate-7"),
        (Some("!!!"), "cargo test", "gate.gate-7"),
        (
            None,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "gate.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    ] {
        assert_eq!(super::super::gate_phase_name(label, command, 7), expected);
    }
}

#[test]
fn review_test_gate_evidence_names_failing_test_despite_stdout_cap() {
    let (stdout, stderr) = huge_review_test_failure_capture();
    let failing = "homeboy-agents::agent_task_gate::tests::needle_gate_regression";
    let evidence =
        gate_failure_evidence("homeboy review test homeboy", 100, &stdout, &stderr, None);
    assert_eq!(
        evidence.classification,
        AgentTaskGateFailureClassification::CandidateCode
    );
    assert_eq!(
        evidence.failed_tests.len(),
        1,
        "{:?}",
        evidence.failed_tests
    );
    assert_eq!(evidence.failed_tests[0].id, failing);
    assert!(evidence.failed_tests[0].message.contains("needle mismatch"));
    assert!(evidence.failed_tests[0]
        .output
        .contains("panicked at crates/homeboy-agents/src/agent_task_gate.rs:42:9"));
    assert!(
        evidence.agent_feedback.contains(failing),
        "{}",
        evidence.agent_feedback
    );
    assert!(evidence.agent_feedback.contains("needle mismatch"));
    assert!(evidence.agent_feedback.len() < 8 * 1024);

    // The persisted gate report carries the structured field, and the
    // shared excerpt (CLI status/diagnose and the gate-fix prompt) shows it.
    let report = AgentTaskGateReport::new(
        "homeboy-review-test",
        legacy_gate_argv("homeboy review test homeboy"),
        100,
        &stdout,
        &stderr,
        Some(evidence),
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        AgentTaskGateEnvironment::default(),
    );
    let value = serde_json::to_value(&report).expect("report json");
    assert_eq!(value["failure_evidence"]["failed_tests"][0]["id"], failing);
    let excerpt = failure_excerpt::gate_failure_excerpt(&report).expect("excerpt");
    assert!(excerpt.contains("failed tests (1):"), "{excerpt}");
    assert!(excerpt.contains(failing), "{excerpt}");
    assert!(excerpt.contains("needle mismatch"), "{excerpt}");
}

#[test]
fn review_test_gate_evidence_falls_back_to_complete_json_failed_ids() {
    let stdout = r#"{"data":{"test_runtime_evidence":{"failed_test_ids":["a::fails"],"status":"complete","tests":[{"id":"a::fails"},{"id":"a::passes"}]}},"success":false}"#;
    let evidence = gate_failure_evidence("homeboy review test demo", 1, stdout, "", None);
    assert_eq!(evidence.failed_tests.len(), 1);
    assert_eq!(evidence.failed_tests[0].id, "a::fails");
    assert!(evidence.agent_feedback.contains("`a::fails`"));
    let plain = gate_failure_evidence("./verify", 1, "boom", "", None);
    assert!(plain.failed_tests.is_empty());
    assert!(plain.agent_feedback.contains("stdout/stderr tails"));
}
#[test]
fn unresolved_component_lint_base_is_unavailable_without_verdict() {
    let workspace = tempfile::tempdir().unwrap();
    let report = super::super::run_gate_command_with_timeout(
        workspace.path(),
        1,
        "HOMEBOY_LINT_BASE=0000000000000000000000000000000000000000 false",
        super::super::AgentTaskGateVisibility::Visible,
        super::super::AgentTaskGateRevealPolicy::FullEvidence,
        workspace.path(),
        std::time::Duration::from_secs(10),
        &super::super::AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
    .unwrap();
    assert_eq!(
        report.status,
        super::super::AgentTaskGateStatus::Unavailable
    );
    assert!(report.failure_evidence.is_none());
    assert_eq!(report.exit_code, 0);
}

/// A bash-only gate program under dash dies on line 1 with exit 2 on the
/// candidate and the baseline alike. That is a broken gate declaration,
/// not inherited infrastructure (Extra-Chill/homeboy#15305).
#[test]
fn dash_rejecting_a_bash_only_gate_is_a_gate_declaration_failure() {
    let evidence = gate_failure_evidence(
        "set -euo pipefail\necho VERIFY OK\n",
        2,
        "",
        "sh: 1: set: Illegal option -o pipefail\n",
        None,
    );
    assert_eq!(
        evidence.classification,
        AgentTaskGateFailureClassification::GateDeclaration
    );
    assert!(
        evidence.summary.contains("Illegal option -o pipefail"),
        "summary must name the interpreter rejection: {}",
        evidence.summary
    );
    assert!(
        evidence.agent_feedback.contains("POSIX `sh -c`"),
        "feedback must explain the gate shell: {}",
        evidence.agent_feedback
    );
}

#[test]
fn shell_syntax_errors_are_gate_declaration_failures() {
    for stderr in [
        "sh: 3: Syntax error: \"(\" unexpected\n",
        "sh: 1: Bad substitution\n",
        "sh: line 4: syntax error near unexpected token `fi'\n",
    ] {
        let evidence = gate_failure_evidence("./verify", 2, "", stderr, None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::GateDeclaration,
            "stderr {stderr:?}"
        );
    }
}

/// A Homeboy setup/infrastructure error (structured envelope, exit 2)
/// never produced a candidate verdict, so it must not ask the provider to
/// rewrite code (Extra-Chill/homeboy#17585). It is the gate environment
/// failing, so it is classified as infrastructure and names the setup error.
#[test]
fn homeboy_setup_errors_are_not_candidate_failures() {
    let stdout = r#"{"success":false,"diagnostics":{"code":"extension.execution_failed","failure_digest":{"summary":"dependency install failed"}}}"#;
    let evidence = gate_failure_evidence("homeboy review lint demo", 2, stdout, "", None);
    assert_eq!(
        evidence.classification,
        AgentTaskGateFailureClassification::Environment
    );
    assert!(evidence.stdout_tail.contains("dependency install failed"));
    assert!(
        evidence
            .summary
            .contains("gate environment unavailable: Homeboy gate setup failed before a candidate verdict: dependency install failed"),
        "{}",
        evidence.summary
    );
    assert!(evidence
        .agent_feedback
        .contains("Do not rewrite candidate code"));
    for (exit_code, output) in [(1, stdout), (2, "lint failed")] {
        let evidence = gate_failure_evidence("verify", exit_code, output, "", None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::CandidateCode
        );
    }
}

/// A Homeboy gate that fails its own capability preflight never checked
/// the candidate, so it must stop the Cook instead of asking the provider
/// to change code (Extra-Chill/homeboy#15381).
#[test]
fn homeboy_gate_target_unresolved_is_a_gate_declaration_failure() {
    let stdout = r#"{
  "success": false,
  "error": {
    "code": "validation.invalid_argument",
    "message": "Invalid argument 'extension': Component 'extrachill' has no linked extensions that provide test support, and 1 manifest(s) could not be read: wordpress",
    "details": {
      "field": "extension",
      "review_capability_preflight": {
        "capability": "test",
        "effective_component": { "id": "extrachill" }
      }
    },
    "hints": [
      { "message": "Review capability preflight failed before resource admission or Lab routing; repair the component capability, then retry the same command." }
    ]
  }
}"#;
    let evidence = gate_failure_evidence("homeboy review test extrachill", 2, stdout, "", None);
    assert_eq!(
        evidence.classification,
        AgentTaskGateFailureClassification::GateDeclaration
    );
    assert!(
        evidence
            .summary
            .contains("manifest(s) could not be read: wordpress"),
        "summary must name the environment failure: {}",
        evidence.summary
    );
    assert!(
        evidence
            .agent_feedback
            .contains("not candidate-code feedback")
            && !evidence.agent_feedback.contains("Fix the code"),
        "feedback must not ask for a code change: {}",
        evidence.agent_feedback
    );
}

/// A nested Homeboy gate whose component or capability does not resolve in
/// the gate environment never checked the candidate. The exact stdout from
/// a Lab cook whose isolated gate XDG hid the operator component registry
/// (Extra-Chill/homeboy#16157) must classify as a broken gate, and an
/// identical baseline replay must not turn it into an inherited failure.
#[test]
fn homeboy_gate_with_unresolved_target_is_a_gate_declaration_failure() {
    let component_not_found = r#"{
  "schema": "homeboy/command-result/v3",
  "command": "review",
  "operation": "test",
  "success": false,
  "exit_code": 2,
  "status": "failed",
  "summary": "Component not found",
  "diagnostics": {
    "code": "component.not_found",
    "message": "Component not found",
    "details": { "id": "blocks-engine" },
    "hints": [ { "message": "Run 'homeboy component list' to see available components" } ],
    "failure_digest": { "summary": "Component not found" }
  }
}
"#;
    let extension_unsupported = r#"{
  "schema": "homeboy/command-result/v3",
  "success": false,
  "exit_code": 2,
  "diagnostics": {
    "code": "extension.unsupported",
    "message": "No extension provider configured for component 'blocks-engine'",
    "details": { "component_id": "blocks-engine" }
  }
}"#;
    for stdout in [component_not_found, extension_unsupported] {
        let evidence =
            gate_failure_evidence("homeboy review test blocks-engine", 2, stdout, "", None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::GateDeclaration,
            "{stdout}"
        );
        assert!(
            evidence.summary.contains(if stdout == component_not_found {
                "component.not_found: Component not found"
            } else {
                "extension.unsupported: No extension provider configured"
            }),
            "{}",
            evidence.summary
        );
        assert!(
            !evidence.agent_feedback.contains("Fix the code"),
            "{}",
            evidence.agent_feedback
        );
    }
}

#[test]
fn homeboy_gate_capability_failure_summary_preserves_resolved_source_path() {
    let stdout = json!({
        "success": false,
        "diagnostics": {
            "code": "extension.unsupported",
            "message": "No extension provider configured for component 'data-machine-events'",
            "details": {"review_capability_preflight": {"effective_component": {
                "source_path": "/lab/worktree/plugins/data-machine-events"
            }}}
        }
    })
    .to_string();
    for (out, err) in [(stdout.as_str(), ""), ("", stdout.as_str())] {
        let evidence =
            gate_failure_evidence("homeboy review test data-machine-events", 2, out, err, None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::GateDeclaration
        );
        assert!(evidence.summary.contains("extension.unsupported: No extension provider configured for component 'data-machine-events'"));
        assert!(evidence
            .summary
            .contains("resolved source_path: /lab/worktree/plugins/data-machine-events"));
        assert!(!evidence.summary.contains("could not resolve"));
    }
}

/// A Homeboy result that resolved its target and then failed is ordinary
/// candidate feedback, as is a successful result.
#[test]
fn homeboy_gate_that_resolved_its_target_stays_candidate_code() {
    for stdout in [
        r#"{"success": false, "diagnostics": {"code": "validation.invalid_argument", "details": {}}}"#,
        r#"{"success": true, "diagnostics": {"code": "component.not_found"}}"#,
        "{\"success\": false}",
        "component.not_found\n",
    ] {
        let evidence = gate_failure_evidence("homeboy review test demo", 1, stdout, "", None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::CandidateCode,
            "{stdout}"
        );
    }
}

/// Mentioning the words in ordinary output is not the structured marker.
#[test]
fn prose_about_capability_preflight_stays_candidate_code() {
    let evidence = gate_failure_evidence(
        "./verify",
        1,
        "test review_capability_preflight_parser ... FAILED\n",
        "",
        None,
    );
    assert_eq!(
        evidence.classification,
        AgentTaskGateFailureClassification::CandidateCode
    );
}

#[test]
fn libtest_environment_names_are_not_diagnostics() {
    for status in ["ok", "ignored", "ignored, needs space", "FAILED"] {
        for name in [
            "staging_enospc_is_retryable",
            "toolchain_not found",
            "workspace staging failed",
            "spawn_enoent",
            "no space left on device",
            "gate_environment_unavailable",
        ] {
            for output in [
                format!("test {name} ... {status}\n"),
                format!("..test {name} has been running for over 60 seconds\n"),
            ] {
                assert_eq!(gate_environment_error(&output, ""), None, "{output}");
                assert_eq!(gate_environment_error("", &output), None, "{output}");
            }
        }
    }
    assert_eq!(
        gate_environment_error("test result: FAILED. enospc", ""),
        None
    );
}

#[test]
fn libtest_failed_tests_outrank_environment_signature_mentions() {
    let stdout = "test workspace::staging_enospc_is_retryable ... ok\ntest refresh_with_active_jobs ... FAILED\ntest result: FAILED. 1 passed; 1 failed\n";
    for stderr in ["", "checking enospc recovery"] {
        let evidence = gate_failure_evidence("cargo test --lib", 101, stdout, stderr, None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::CandidateCode
        );
        assert_eq!(evidence.failed_tests[0].id, "refresh_with_active_jobs");
        assert!(evidence.summary.contains("refresh_with_active_jobs"));
        assert!(!evidence.summary.contains("staging_enospc"));
        assert!(evidence.agent_feedback.contains("These tests failed"));
    }
    for diagnostic in [
        "error: failed to write /tmp/output: No space left on device (os error 28)",
        "Error: spawn cargo ENOENT",
    ] {
        // A bare signature must not hide a later genuine diagnostic either.
        let stderr = format!("checking enospc recovery\n{diagnostic}");
        let evidence = gate_failure_evidence("cargo test --lib", 101, stdout, &stderr, None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::Environment
        );
        assert!(evidence.summary.contains(diagnostic));
    }
}

#[test]
fn infrastructure_output_is_not_a_candidate_verdict() {
    for output in [
        "Error: spawn /gate-home/.studio/php-bin/8.4.27-studio-1/php ENOENT",
        "error: toolchain 'nightly' is not installed",
        "No space left on device",
        "lab_workspace_dependency_hydration failed",
        "runner.lab_transport_failure: connection reset",
        "workspace staging failed: connection closed",
    ] {
        let evidence = gate_failure_evidence("verify", 1, "", output, None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::Environment,
            "{output}"
        );
        assert!(evidence.summary.contains(output));
        assert!(evidence.agent_feedback.contains("Preserve the candidate"));
    }
    let evidence = gate_failure_evidence("verify", 1, "", "assertion failed: left == right", None);
    assert_eq!(
        evidence.classification,
        AgentTaskGateFailureClassification::CandidateCode
    );
}

#[test]
fn missing_gate_executables_are_environment_failures() {
    for (command, code, stdout, stderr, bin) in [
        ("nginx -t", 127, "", "sh: 1: nginx: not found\n", "nginx"),
        (
            "tsc",
            127,
            "",
            "sh: line 1: tsc: command not found\n",
            "tsc",
        ),
        (
            "pnpm exec tsc",
            254,
            "ERR_PNPM_RECURSIVE_EXEC_FIRST_FAIL Command \"tsc\" not found",
            "",
            "tsc",
        ),
        (
            "pnpm exec tsc",
            254,
            "",
            "ERR_PNPM_RECURSIVE_EXEC_FIRST_FAIL Command \"tsc\" not found",
            "tsc",
        ),
        (
            "pnpm exec tsc",
            254,
            "Command \"tsc\" not found",
            "ERR_PNPM_RECURSIVE_EXEC_FIRST_FAIL",
            "tsc",
        ),
        (
            "npx tsc",
            1,
            "",
            "npm error could not determine executable to run",
            "tsc",
        ),
    ] {
        let evidence = gate_failure_evidence(command, code, stdout, stderr, None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::Environment
        );
        assert_eq!(
            evidence.summary,
            format!("gate environment cannot run `{bin}`: executable not found")
        );
        assert!(evidence
            .agent_feedback
            .contains("Environment failure, not candidate-code feedback"));
        assert!(evidence.agent_feedback.contains("preserved candidate"));
        let mut report = AgentTaskGateReport::skipped(
            "gate-1",
            vec![command.to_string()],
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            "fixture",
        );
        report.status = AgentTaskGateStatus::Failed;
        report.failure_evidence = Some(evidence);
        report.accept_inherited_failure();
        assert_eq!(report.status, AgentTaskGateStatus::Failed, "{command}");
    }
}

#[test]
fn composer_missing_dev_tool_cannot_be_accepted_as_inherited() {
    let temp = tempfile::tempdir().unwrap();
    let command = "printf '%s\\n' 'sh: 1: phpcs: not found' 'Script phpcs --standard=phpcs.xml.dist handling the lint:php event returned with error code 127' >&2; exit 1";
    let mut report = run_gate_command(temp.path(), 1, command).unwrap();
    report.accept_inherited_failure();
    assert_eq!(report.status, AgentTaskGateStatus::Failed);
    let evidence = report.failure_evidence.unwrap();
    assert_eq!(
        evidence.classification,
        AgentTaskGateFailureClassification::Environment
    );
    assert!(evidence.summary.contains("`phpcs`"));
    assert!(evidence
        .agent_feedback
        .contains("Composer dev dependencies in subdirectories"));
}

#[test]
fn missing_executable_cannot_be_accepted_as_inherited() {
    let temp = tempfile::tempdir().unwrap();
    let mut report = run_gate_command(temp.path(), 1, "homeboy_nonexistent_gate_16197").unwrap();
    report.accept_inherited_failure();
    assert_eq!(report.status, AgentTaskGateStatus::Failed);
    assert_eq!(
        report.failure_evidence.unwrap().classification,
        AgentTaskGateFailureClassification::Environment
    );
}

/// Ordinary failures stay candidate feedback: a shell message on a
/// non-syntax exit status, or a test's own "syntax error".
#[test]
fn ordinary_gate_failures_stay_candidate_code() {
    for (exit_code, stderr) in [
        (1, "test fixture: not found\n"),
        (127, "sh: 1: ./bin/check: not found\n"),
        (1, "sh: 1: set: Illegal option -o pipefail\n"),
        (2, "VERIFY FAIL: README lacks target\n"),
        (2, "parser test: syntax error in fixture.conf\n"),
    ] {
        let evidence = gate_failure_evidence("./verify", exit_code, "", stderr, None);
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::CandidateCode,
            "exit {exit_code} stderr {stderr:?}"
        );
    }
}

/// A conclusive comparison carries no diagnostic, and the field is omitted
/// from the wire form so persisted reports written before it existed still
/// round-trip.
#[test]
fn baseline_comparison_omits_an_absent_diagnostic() {
    let comparison = AgentTaskGateBaselineComparison {
        base_ref: "abc".to_string(),
        exit_code: 1,
        failure_fingerprint: "fp".to_string(),
        matches_candidate_failure: true,
        result: AgentTaskGateDifferentialResult::BaselineRed,
        diagnostic: None,
        failure_attribution: None,
    };
    let json = serde_json::to_value(&comparison).expect("serialize");
    assert!(
        json.get("diagnostic").is_none(),
        "absent diagnostic must not widen the wire form: {json}"
    );
}

/// A report persisted before the field existed must still deserialize.
#[test]
fn baseline_comparison_reads_reports_written_without_a_diagnostic() {
    let json = serde_json::json!({
        "base_ref": "abc",
        "exit_code": 124,
        "failure_fingerprint": "",
        "matches_candidate_failure": false,
        "result": "inconclusive"
    });
    let comparison: AgentTaskGateBaselineComparison =
        serde_json::from_value(json).expect("deserialize legacy report");
    assert_eq!(comparison.diagnostic, None);
    assert_eq!(
        comparison.result,
        AgentTaskGateDifferentialResult::Inconclusive
    );
}

/// An inconclusive comparison round-trips the reason it could not conclude.
#[test]
fn baseline_comparison_round_trips_its_diagnostic() {
    let comparison = AgentTaskGateBaselineComparison {
        base_ref: "abc".to_string(),
        exit_code: 124,
        failure_fingerprint: String::new(),
        matches_candidate_failure: false,
        result: AgentTaskGateDifferentialResult::Inconclusive,
        diagnostic: Some("baseline replay failed: boom".to_string()),
        failure_attribution: None,
    };
    let json = serde_json::to_value(&comparison).expect("serialize");
    assert_eq!(json["diagnostic"], "baseline replay failed: boom");
    let back: AgentTaskGateBaselineComparison = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, comparison);
}

/// `--gate-no-progress-timeout-seconds` (and the wall-clock budget) must
/// reach a Lab runner even for gate runs that do not thread explicit
/// supervision, such as the verified-base differential replays. Those runs
/// execute inside the declared `with_gate_supervision` scope, and the request
/// builder must carry that scope's budgets instead of hardcoded 300s/1800s
/// defaults (Extra-Chill/homeboy#15856).
#[test]
fn lab_gate_request_budgets_inherit_the_declared_supervision_scope() {
    struct CapturingTransport {
        previous: Arc<dyn placement::LabGateTransport>,
        captured: Arc<Mutex<Vec<placement::LabGateRequest>>>,
    }
    impl placement::LabGateTransport for CapturingTransport {
        fn declared_runner(&self, command: &str) -> Result<Option<String>> {
            self.previous.declared_runner(command)
        }
        fn execute(
            &self,
            cwd: &Path,
            request: &placement::LabGateRequest,
            supervision: Option<&GateSupervision>,
        ) -> Result<placement::LabGateReceipt> {
            if request.runner_id != "budget-capture-runner" {
                return self.previous.execute(cwd, request, supervision);
            }
            self.captured.lock().unwrap().push(request.clone());
            Err(Error::new(
                homeboy_core::ErrorCode::RunnerLabTransportFailure,
                "request captured; execution not required",
                serde_json::json!({"gate_disposition": "unavailable"}),
            ))
        }
    }
    let captured = Arc::new(Mutex::new(Vec::new()));
    placement::register_lab_gate_transport(Arc::new(CapturingTransport {
        previous: placement::active_transport(),
        captured: Arc::clone(&captured),
    }));
    let workspace = tempfile::tempdir().expect("gate workspace");
    let environment = AgentTaskGateEnvironmentPolicy {
        lab_runner: Some("budget-capture-runner".to_string()),
        ..Default::default()
    };
    let declared = VerifyGateOptions {
        gate_timeout_seconds: 2100,
        gate_no_progress_timeout_seconds: 1500,
        ..Default::default()
    };
    // The verified-base differential replay shape: an explicit wall-clock
    // deadline, no explicit supervision, inside the declared scope.
    crate::agent_task_promotion::with_gate_supervision(declared.declared_supervision(), || {
        run_gate_command_with_timeout(
            workspace.path(),
            1,
            "true",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            workspace.path(),
            declared.gate_timeout(),
            &environment,
            &[],
        )
    })
    .expect("a refused transport yields a non-execution gate report");
    let requests = captured.lock().expect("captured requests");
    let request = requests.last().expect("captured Lab gate request");
    assert_eq!(
        (request.timeout_seconds, request.no_progress_timeout_seconds),
        (2100, 1500),
        "the Lab request must carry the declared budgets, not defaults"
    );
}

/// #16262: a verified-base (inherited-failure) replay of a Lab gate
/// rebuilds its request from the candidate report's environment. That
/// replay must still ask the runner to hydrate the baseline workspace, or
/// the baseline would replay without the dependencies the candidate had.
#[test]
fn lab_gate_replay_policy_requests_runner_side_hydration() {
    let mut environment = AgentTaskGateEnvironment::default();
    environment.lab_runner = Some("lab".to_string());
    let policy = environment.replay_policy();
    assert_eq!(policy.lab_runner.as_deref(), Some("lab"));
    assert!(policy.hydrate_dependencies);
}

/// #14731: `homeboy review test` and equivalent portable-Lab-route
/// commands exit `0` and print the runner-wait marker when they
/// could not execute under the resolved placement. Reading exit code
/// alone misread this as a passing gate; this pins the marker-based
/// classification as `Deferred`, distinct from both `Succeeded` and
/// `Failed`.
#[test]
fn a_runner_wait_marker_on_stdout_classifies_as_deferred_regardless_of_exit_code() {
    for exit_code in [0, 1] {
        let report = AgentTaskGateReport::new(
            "gate-1",
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "homeboy review test homeboy".to_string(),
            ],
            exit_code,
            r#"{"schema":"homeboy/runner-wait-step/v1","status":"deferred","job_id":"step-fixture","command":"review test"}"#,
            String::new(),
            None,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            AgentTaskGateEnvironment::default(),
        );
        assert_eq!(
            report.status,
            AgentTaskGateStatus::Deferred,
            "exit_code={exit_code}"
        );
    }
}

/// A gate whose stdout is not the runner-wait marker keeps the
/// ordinary exit-code classification.
#[test]
fn ordinary_stdout_is_not_misclassified_as_deferred() {
    let succeeded = AgentTaskGateReport::new(
        "gate-1",
        vec!["sh".to_string(), "-lc".to_string(), "true".to_string()],
        0,
        "ok",
        String::new(),
        None,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        AgentTaskGateEnvironment::default(),
    );
    assert_eq!(succeeded.status, AgentTaskGateStatus::Succeeded);

    let failed = AgentTaskGateReport::new(
        "gate-1",
        vec!["sh".to_string(), "-lc".to_string(), "false".to_string()],
        1,
        "",
        "boom",
        None,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        AgentTaskGateEnvironment::default(),
    );
    assert_eq!(failed.status, AgentTaskGateStatus::Failed);
}

/// Serializes tests that mutate process-global environment state.
///
/// This must be the lock core hands out, not a module-local one. Rust runs
/// a crate's tests as threads in a single process, so a `Mutex` scoped to
/// this module only orders `agent_task_gate`'s own tests against each
/// other, while every other module in the binary keeps running against the
/// same environment. That is how a `HOME` mutation here went unnoticed.
pub(super) fn env_mutex() -> std::sync::MutexGuard<'static, ()> {
    homeboy_core::test_support::env_lock()
}

/// Replacing `PATH` is process-wide: while it names a fixture bin, every
/// parallel test that spawns `git` or `sh` by name fails with ENOENT, and
/// the env lock cannot prevent it because those spawners never take it
/// (#14832). A test that must replace `PATH` reruns itself alone in a child
/// of this test binary and mutates the environment only there.
///
/// Returns `true` inside that child, where the caller runs its body. Call it
/// while holding [`env_mutex`], so the child inherits the real environment
/// rather than another test's isolated home.
#[cfg(unix)]
pub(super) fn rerun_alone_to_replace_path(module: &str, test: &str) -> bool {
    const CHILD_ENV: &str = "HOMEBOY_TEST_GATE_PATH_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        return true;
    }
    let test_name = homeboy_core::test_support::harness_test_name(module, test);
    let output = homeboy_core::test_support::run_child_test(
        std::process::Command::new(std::env::current_exe().expect("current test executable"))
            .args(["--exact", &test_name, "--nocapture"])
            .env(CHILD_ENV, "1"),
        &test_name,
    );
    assert!(
        output.status.success(),
        "{test_name} failed in its PATH-isolated child:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

#[test]
fn cargo_test_results_preserve_selected_counts_without_rejecting_broad_gates() {
    let summary = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1675 filtered out; finished in 0.00s\n";
    let zero = cargo_test_result(
        "RUSTFLAGS=-Dwarnings timeout 30 cargo --quiet test selected_test -- --exact",
        0,
        summary,
        "",
    )
    .expect("Cargo summary is parsed");
    assert_eq!(zero.total, 0);
    assert_eq!(zero.filtered, 1675);
    assert_eq!(effective_gate_exit_code(0, None), 0);

    let empty = cargo_test_result(
            "cargo test --locked -p empty-crate",
            0,
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
            "",
        )
        .expect("empty Cargo summary is parsed");
    assert_eq!(empty.total, 0);
    assert_eq!(empty.filtered, 0);
    assert_eq!(effective_gate_exit_code(0, None), 0);

    let selected = cargo_test_result(
            "cargo test --locked selected_test",
            0,
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1674 filtered out; finished in 0.00s\n",
            "",
        )
        .expect("Cargo summary is parsed");
    assert_eq!(selected.total, 1);
    assert_eq!(selected.passed, 1);
    assert_eq!(effective_gate_exit_code(0, None), 0);

    assert!(cargo_test_result("echo cargo test", 0, summary, "").is_none());
    assert!(cargo_test_result("timeout 30 echo cargo test", 0, summary, "").is_none());
    assert!(cargo_test_result(
        "cargo test",
        0,
        "test result: ok. 0 passed; 0 failed; arbitrary output",
        "",
    )
    .is_none());
}

#[test]
fn a_quiet_focused_gate_counts_tests_from_cargo_summaries() {
    let command = "cargo test -q -p homeboy-lab-runner --lib workspace";
    for (stdout, exit) in [
            ("running 34 tests\n..................................\ntest result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 2018 filtered out; finished in 10.36s\n", 0),
            ("running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2052 filtered out; finished in 0.00s\n", 1),
        ] {
            let result = cargo_test_result(command, 0, stdout, "");
            let selection = cargo_selection(command, &[], stdout, "", result.as_ref()).unwrap();
            assert_eq!(effective_gate_exit_code(0, Some(&selection)), exit, "{stdout}");
        }

    // A quiet substring filter whose selected tests fail is a candidate
    // failure, not `invalid_focused_cargo_selection`: a FAILED summary, or
    // a harness that died before its summary, still counts the selection.
    let command = "cargo test -q -p homeboy-cli --lib status";
    for stdout in [
        "running 241 tests\n....F.F\nfailures:\n    commands::status::tests::a\n    commands::status::tests::b\n\ntest result: FAILED. 239 passed; 2 failed; 0 ignored; 0 measured; 2683 filtered out; finished in 15.98s\n",
        "running 241 tests\n....\nthread 'main' has overflowed its stack\n",
    ] {
        let result = cargo_test_result(command, 101, stdout, "");
        let selection = cargo_selection(command, &[], stdout, "", result.as_ref()).unwrap();
        assert_eq!(selection.selected_count, 241, "{stdout}");
        assert_eq!(effective_gate_exit_code(101, Some(&selection)), 101);
        let evidence = gate_failure_evidence(command, 101, stdout, "", Some(&selection));
        assert_eq!(
            evidence.classification,
            AgentTaskGateFailureClassification::CandidateCode,
            "{}",
            evidence.summary
        );
    }
}

/// A focused Cargo filter is refused at admission, before any provider
/// runs, only when it cannot be part of any test path in the checkout.
#[test]
fn focused_cargo_filters_are_checked_against_the_checkout_at_admission() {
    let workspace = tempfile::tempdir().unwrap();
    let package = workspace.path().join("crates/demo");
    fs::create_dir_all(package.join("src/status")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/demo\"]\n",
    )
    .unwrap();
    fs::write(package.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(
        package.join("src/lib.rs"),
        "mod status;\n/// Untested: `projection` names no test path.\npub mod projection {}\n",
    )
    .unwrap();
    fs::write(
        package.join("src/status/mod.rs"),
        "#[cfg(test)]\nmod tests {\n    #[test]\n    fn summary_bounds_hung_probe() {\n        let _ = \"#[test] fn projection_fixture\";\n    }\n}\n",
    )
    .unwrap();
    let admit = |command: &str| {
        super::super::cargo_selection_admission::reject_unmatched_focused_cargo_filters(
            workspace.path(),
            [&command.to_string()],
        )
    };

    for accepted in [
        "cargo test -q -p demo --lib status",
        "cargo test -p demo --lib hung_probe",
        "cargo test -p demo --lib status::tests::summary",
        "cargo test --workspace --lib tests::summary_bounds_hung_probe",
        // An exact name may be a test the provider is asked to add.
        "cargo test -p demo --lib projection::tests::renders -- --exact",
        // Unresolvable scope stays a runtime decision.
        "cargo test -p unknown --lib projection",
    ] {
        admit(accepted).unwrap_or_else(|error| panic!("{accepted}: {error}"));
    }

    // Neither a module without tests nor a string fixture is a test path.
    let gates = VerifyGateOptions {
        verify: vec!["cargo test -p demo --lib projection".to_string()],
        ..VerifyGateOptions::default()
    };
    let error = gates.preflight(workspace.path()).unwrap_err();
    assert!(error.to_string().contains("selects no tests"), "{error}");
    assert!(error.to_string().contains("-- --exact"), "{error}");
    assert_eq!(
        error.details["failure_classification"],
        "zero_tests_selected"
    );
    assert_eq!(error.details["filter"], "projection");
    assert_eq!(error.retryable, Some(false));
}

#[test]
fn cargo_selection_admits_exact_or_module_prefix_and_keeps_broad_gates_explicit() {
    let focused = cargo_selection(
        "cargo test selected_test -- --exact",
        &[
            "sh".to_string(),
            "-lc".to_string(),
            "cargo test selected_test -- --exact".to_string(),
        ],
        "selected_test: test\nselected_test_unrelated: test\ntest selected_test ... ok\n",
        "",
        None,
    )
    .expect("Cargo selection");
    assert_eq!(focused.mode, "focused");
    assert_eq!(focused.filter_interpretation, "exact");
    assert_eq!(
        focused.discovered_ids,
        vec!["selected_test", "selected_test_unrelated"]
    );
    assert_eq!(focused.selected_ids, vec!["selected_test"]);
    assert_eq!(focused.selected_count, 1);
    assert_eq!(effective_gate_exit_code(0, Some(&focused)), 0);

    let broad = cargo_selection(
            "cargo test --locked",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test --locked".to_string(),
            ],
            "selected_test: test\nselected_test_unrelated: test\ntest selected_test ... ok\ntest selected_test_unrelated ... ok\n",
            "",
            None,
        )
        .expect("Cargo selection");
    assert_eq!(broad.mode, "broad");
    assert_eq!(broad.filter_interpretation, "broad_explicit");
    assert_eq!(broad.selected_count, 2);
    assert_eq!(effective_gate_exit_code(0, Some(&broad)), 0);

    let ambiguous = cargo_selection(
            "cargo test selected_test",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test selected_test".to_string(),
            ],
            "selected_test: test\nselected_test_unrelated: test\ntest selected_test ... ok\ntest selected_test_unrelated ... ok\n",
            "",
            None,
        )
        .expect("Cargo selection");
    assert_eq!(ambiguous.filter_interpretation, "substring_ambiguous");
    assert_eq!(
        ambiguous.selected_ids,
        vec!["selected_test", "selected_test_unrelated"]
    );
    assert_eq!(ambiguous.selected_count, 2);
    assert_eq!(effective_gate_exit_code(0, Some(&ambiguous)), 0);
    let serialized = serde_json::to_value(&ambiguous).expect("selection evidence serializes");
    assert_eq!(serialized["filter_interpretation"], "substring_ambiguous");
    assert_eq!(serialized["selected_count"], 2);
    let multiple_recovery = ambiguous.recovery.expect("discovery recovery metadata");
    assert_eq!(multiple_recovery.action, "discover_and_choose");
    assert_eq!(multiple_recovery.command, "cargo test -- --list");
    assert_eq!(
        multiple_recovery.candidate_ids,
        vec!["selected_test", "selected_test_unrelated"]
    );

    let non_exact_single = cargo_selection(
        "cargo test selected_test",
        &[
            "sh".to_string(),
            "-lc".to_string(),
            "cargo test selected_test".to_string(),
        ],
        "test selected_test ... ok\n",
        "",
        None,
    )
    .expect("Cargo selection");
    assert_eq!(non_exact_single.selected_count, 1);
    assert_eq!(effective_gate_exit_code(0, Some(&non_exact_single)), 0);
    let serialized_recovery = serde_json::to_value(&non_exact_single)
        .expect("reviewer-facing recovery metadata serializes");
    assert_eq!(
        serialized_recovery["recovery"]["command"],
        "cargo test selected_test -- --exact"
    );
    let one_recovery = non_exact_single.recovery.expect("exact recovery metadata");
    assert_eq!(one_recovery.action, "rerun_exact");
    assert_eq!(
        one_recovery.argv,
        vec!["cargo", "test", "selected_test", "--", "--exact"]
    );
    assert_eq!(one_recovery.command, "cargo test selected_test -- --exact");

    let module = cargo_selection(
        "cargo test session_store",
        &[
            "sh".to_string(),
            "-lc".to_string(),
            "cargo test session_store".to_string(),
        ],
        "test session_store::tests::first ... ok\ntest session_store::tests::second ... ok\n",
        "",
        None,
    )
    .expect("module-prefix Cargo selection");
    assert_eq!(module.filter_interpretation, "module_prefix");
    assert_eq!(module.selected_count, 2);
    assert_eq!(effective_gate_exit_code(0, Some(&module)), 0);

    let prefixed = cargo_selection(
            "RUSTFLAGS=\"-D warnings\" timeout --signal TERM 30 cargo test --locked -p example --test integration --features feature-a selected --lib -- --ignored --nocapture",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "RUSTFLAGS=\"-D warnings\" timeout --signal TERM 30 cargo test --locked -p example --test integration --features feature-a selected --lib -- --ignored --nocapture".to_string(),
            ],
            "test crate::selected ... ok\n",
            "",
            None,
        )
        .expect("prefixed Cargo selection");
    let prefixed_recovery = prefixed.recovery.expect("prefix-preserving exact recovery");
    assert_eq!(prefixed_recovery.action, "rerun_exact");
    assert_eq!(
        prefixed_recovery.argv,
        vec![
            "RUSTFLAGS=-D warnings",
            "timeout",
            "--signal",
            "TERM",
            "30",
            "cargo",
            "test",
            "--locked",
            "-p",
            "example",
            "--test",
            "integration",
            "--features",
            "feature-a",
            "crate::selected",
            "--lib",
            "--",
            "--ignored",
            "--nocapture",
            "--exact",
        ]
    );
    assert_eq!(
            prefixed_recovery.command,
            "RUSTFLAGS='-D warnings' timeout --signal TERM 30 cargo test --locked -p example --test integration --features feature-a crate::selected --lib -- --ignored --nocapture --exact"
        );

    let zero_match = cargo_selection(
        "cargo test --test integration missing_test --lib -- --ignored --exact",
        &[
            "sh".to_string(),
            "-lc".to_string(),
            "cargo test --test integration missing_test --lib -- --ignored --exact".to_string(),
        ],
        "selected_test: test\nselected_test_unrelated: test\n",
        "",
        None,
    )
    .expect("Cargo selection");
    assert_eq!(zero_match.filter_interpretation, "exact");
    assert!(zero_match.selected_ids.is_empty());
    assert_eq!(zero_match.selected_count, 0);
    assert_eq!(effective_gate_exit_code(0, Some(&zero_match)), 1);
    let zero_evidence = gate_failure_evidence(
        "cargo test --test integration missing_test --lib -- --ignored --exact",
        1,
        "",
        "",
        Some(&zero_match),
    );
    assert_eq!(
        zero_evidence.classification,
        AgentTaskGateFailureClassification::ZeroTestsSelected
    );
    assert!(zero_evidence.summary.contains("selected no tests"));
    let zero_recovery = zero_match.recovery.expect("discovery recovery metadata");
    assert_eq!(zero_recovery.action, "discover_and_choose");
    assert_eq!(
        zero_recovery.argv,
        vec![
            "cargo",
            "test",
            "--test",
            "integration",
            "--lib",
            "--",
            "--ignored",
            "--list",
        ]
    );
    assert_eq!(
        zero_recovery.candidate_ids,
        vec!["selected_test", "selected_test_unrelated"]
    );

    let redacted = cargo_selection_recovery(
        &[
            "cargo".to_string(),
            "test".to_string(),
            "unsafe_filter".to_string(),
        ],
        0,
        &["token=secret".to_string()],
        1,
        false,
        false,
    )
    .expect("redacted exact recovery");
    assert!(redacted.command.contains("[REDACTED]"));
    assert!(!redacted.command.contains("secret"));

    let quoted = cargo_selection_recovery(
        &[
            "cargo".to_string(),
            "test".to_string(),
            "unsafe_filter".to_string(),
        ],
        0,
        &["module::id; touch /tmp/pwn".to_string()],
        1,
        false,
        false,
    )
    .expect("shell-quoted exact recovery");
    assert!(quoted.command.contains("'module::id; touch /tmp/pwn'"));

    let shell_control = cargo_selection(
        "cargo test selected_test && printf unexpected",
        &[
            "sh".to_string(),
            "-lc".to_string(),
            "cargo test selected_test && printf unexpected".to_string(),
        ],
        "test selected_test ... ok\n",
        "",
        None,
    )
    .expect("shell-controlled Cargo selection");
    assert_eq!(
        shell_control
            .recovery
            .expect("discovery recovery for shell control")
            .action,
        "discover_and_choose"
    );

    let package_only = cargo_selection(
        "cargo test --locked -p empty-crate",
        &[
            "sh".to_string(),
            "-lc".to_string(),
            "cargo test --locked -p empty-crate".to_string(),
        ],
        "",
        "",
        None,
    )
    .expect("Cargo selection");
    assert_eq!(package_only.mode, "broad");
    assert_eq!(effective_gate_exit_code(0, Some(&package_only)), 0);

    let quiet_exact = cargo_selection(
            "cargo --quiet test selected_test -- --exact",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo --quiet test selected_test -- --exact".to_string(),
            ],
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n",
            "",
            Some(&AgentTaskGateTestResult {
                runner: "cargo".to_string(),
                total: 1,
                passed: 1,
                failed: 0,
                filtered: 1,
                runner_exit_code: 0,
            }),
        )
        .expect("quiet exact Cargo selection");
    assert_eq!(quiet_exact.selected_ids, vec!["selected_test"]);
    assert_eq!(quiet_exact.selected_count, 1);
    assert_eq!(effective_gate_exit_code(0, Some(&quiet_exact)), 0);

    let quiet_zero_match = cargo_selection(
            "cargo --quiet test missing_test -- --exact",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo --quiet test missing_test -- --exact".to_string(),
            ],
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s\n",
            "",
            Some(&AgentTaskGateTestResult {
                runner: "cargo".to_string(),
                total: 0,
                passed: 0,
                failed: 0,
                filtered: 2,
                runner_exit_code: 0,
            }),
        )
        .expect("quiet zero-match Cargo selection");
    assert!(quiet_zero_match.selected_ids.is_empty());
    assert_eq!(quiet_zero_match.selected_count, 0);
    assert_eq!(effective_gate_exit_code(0, Some(&quiet_zero_match)), 1);
}

#[test]
fn failing_substring_selection_fails_with_the_real_cargo_exit_code() {
    let failing = cargo_selection(
            "cargo test selected_test",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test selected_test".to_string(),
            ],
            "selected_test: test\nselected_test_unrelated: test\ntest selected_test ... ok\ntest selected_test_unrelated ... FAILED\n",
            "",
            None,
        )
        .expect("Cargo selection");
    assert_eq!(failing.filter_interpretation, "substring_ambiguous");
    assert_eq!(failing.selected_count, 2);
    assert_eq!(effective_gate_exit_code(101, Some(&failing)), 101);
    let evidence = gate_failure_evidence("cargo test selected_test", 101, "", "", Some(&failing));
    assert_eq!(
        evidence.classification,
        AgentTaskGateFailureClassification::CandidateCode
    );
    assert_eq!(evidence.exit_code, 101);
    assert!(evidence
        .summary
        .contains("deterministic gate failed with exit code 101"));
    assert!(!evidence.summary.contains("invalid_focused_cargo_selection"));
}

#[cfg(unix)]
#[test]
fn cargo_gate_rejects_zero_match_and_accepts_one_match_through_supported_declarations() {
    let _guard = env_mutex();
    let temp = tempfile::tempdir().expect("temporary Cargo fixture");
    std::fs::create_dir(temp.path().join("src")).expect("fixture source directory");
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("fixture manifest");
    std::fs::write(
            temp.path().join("src/lib.rs"),
            "#[cfg(test)]\nmod tests { #[test] fn selected_test() {} #[test] fn other_test() { panic!(\"intentional substring failure\"); } }\n",
        )
        .expect("fixture test");
    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).expect("wrapper directory");
    let timeout = bin.join("timeout");
    homeboy_test_support::write_executable(&timeout, "#!/bin/sh\nwhile [ \"$1\" = -s ] || [ \"$1\" = --signal ]; do shift; shift; done\nshift\nshift\nHOME=\"$HOMEBOY_ORIGINAL_HOME\"\nPATH=\"$HOMEBOY_ORIGINAL_PATH\"\nexport HOME PATH\nexec cargo \"$@\"\n", 0o755)
        .expect("timeout wrapper");

    let target = temp.path().join("target");
    let command = |timeout_options: &str, cargo_invocation: &str| {
        format!(
                "RUSTFLAGS=\"-D warnings\" HOMEBOY_ORIGINAL_HOME='{}' HOMEBOY_ORIGINAL_PATH='{}' CARGO_TARGET_DIR='{}' PATH='{}' timeout {timeout_options} 30 cargo {cargo_invocation}",
                std::env::var("HOME").expect("host HOME"),
                std::env::var("PATH").expect("host PATH"),
                target.display(),
                bin.display(),
            )
    };

    let zero = run_gate_command(
        temp.path(),
        1,
        &command("-s TERM", "--quiet test tests::missing_test -- --exact"),
    )
    .expect("zero-match Cargo gate report");
    assert_eq!(zero.status, AgentTaskGateStatus::Failed);
    assert_eq!(
        zero.failure_evidence
            .as_ref()
            .expect("zero-match failure evidence")
            .classification,
        AgentTaskGateFailureClassification::ZeroTestsSelected
    );
    let zero_counts = zero.test_result.as_ref().expect("zero-match test counts");
    assert_eq!(zero_counts.total, 0);
    assert_eq!(zero_counts.passed, 0);
    assert_eq!(zero_counts.failed, 0);
    assert!(
        zero_counts.filtered > 0,
        "zero match retained filter evidence"
    );

    let selected = run_gate_command(
        temp.path(),
        2,
        &command(
            "--signal TERM",
            "--quiet test tests::selected_test -- --exact",
        ),
    )
    .expect("one-match Cargo gate report");
    assert_eq!(
        selected.status,
        AgentTaskGateStatus::Succeeded,
        "{:#?}",
        selected.failure_evidence
    );
    let selected_counts = selected
        .test_result
        .as_ref()
        .expect("one-match test counts");
    assert_eq!(selected_counts.total, 1);
    assert_eq!(selected_counts.passed, 1);
    assert_eq!(selected_counts.failed, 0);

    // A substring selection that ran passing tests is a gate pass: cargo's
    // real exit code stands, while the broad selection stays visible in
    // the recorded evidence.
    let substring = run_gate_command(temp.path(), 3, &command("--signal TERM", "test selected"))
        .expect("passing substring Cargo gate report");
    assert_eq!(
        substring.status,
        AgentTaskGateStatus::Succeeded,
        "{:#?}",
        substring.failure_evidence
    );
    let substring_selection = substring
        .cargo_selection
        .expect("substring selection evidence");
    assert_eq!(
        substring_selection.filter_interpretation,
        "substring_ambiguous"
    );
    assert_eq!(substring_selection.selected_count, 1);
    assert_eq!(
        substring_selection.selected_ids,
        vec!["tests::selected_test"]
    );

    // A substring selection with a failing test fails on the candidate,
    // not on the gate declaration, and carries cargo's real exit code.
    let failing = run_gate_command(temp.path(), 4, &command("--signal TERM", "test other"))
        .expect("failing substring Cargo gate report");
    assert_eq!(failing.status, AgentTaskGateStatus::Failed);
    let failing_evidence = failing
        .failure_evidence
        .expect("failing substring failure evidence");
    assert_eq!(
        failing_evidence.classification,
        AgentTaskGateFailureClassification::CandidateCode
    );
    assert_eq!(failing_evidence.exit_code, 101);
    assert!(failing_evidence
        .summary
        .contains("deterministic gate failed with exit code 101"));
}

// Allocation fixtures must not depend on the runner's spare bytes/inodes.
pub(super) fn configure_fixture_cargo_capacity() {
    let mut config = homeboy_core::defaults::HomeboyConfig::default();
    config.retention.shared_store_reserve_bytes = 0;
    config.retention.shared_store_reserve_inodes = 0;
    homeboy_core::defaults::save_config(&config).expect("fixture capacity policy");
}

#[test]
fn declared_shared_cargo_target_is_reported_without_parsing_gate_shell_text() {
    homeboy_core::test_support::with_isolated_home(|_| {
        configure_fixture_cargo_capacity();
        let temp = tempfile::tempdir().expect("gate fixture");
        let mut policy = AgentTaskGateEnvironmentPolicy::default();
        policy.shared_cargo_target = Some(true);
        // The test runner's required Cargo target is an ambient override. An
        // explicit empty declaration clears it so this exercises the contract.
        policy
            .variables
            .insert("CARGO_TARGET_DIR".to_string(), String::new());

        let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            temp.path(),
            1,
            "test -n \"$CARGO_TARGET_DIR\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &policy,
            &[],
        )
        .expect("declared managed gate");

        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        let target = report.environment.cargo_target.expect("target evidence");
        assert_eq!(target.resolution, "isolated");
        assert!(target.path.contains("cargo-target"));
        assert!(target.owner.starts_with("agent-task-gate"));
    });
}

#[test]
fn gate_inherits_shared_cargo_target_from_component_metadata() {
    homeboy_core::test_support::with_isolated_home(|_| {
        configure_fixture_cargo_capacity();
        let temp = tempfile::tempdir().expect("gate fixture");
        std::fs::write(
            temp.path().join("homeboy.json"),
            r#"{"id":"cargo-fixture","managed_execution":{"shared_cargo_target":true}}"#,
        )
        .expect("component manifest");
        let mut policy = AgentTaskGateEnvironmentPolicy::default();
        policy
            .variables
            .insert("CARGO_TARGET_DIR".to_string(), String::new());

        let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            temp.path(),
            1,
            "test -n \"$CARGO_TARGET_DIR\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &policy,
            &[],
        )
        .expect("metadata-managed gate");

        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(
            report
                .environment
                .cargo_target
                .expect("target evidence")
                .resolution,
            "isolated"
        );
    });
}

#[test]
fn isolated_cargo_targets_report_warm_cache_hits_and_distinct_allocations() {
    homeboy_core::test_support::with_isolated_home(|_| {
        configure_fixture_cargo_capacity();
        let temp = tempfile::tempdir().expect("gate fixture");
        let store = homeboy_core::observation::ObservationStore::open_initialized().unwrap();
        let run_id = store
            .start_run(homeboy_core::observation::NewRunRecord::builder("agent-task").build())
            .unwrap()
            .id;
        let _timing = homeboy_core::observation::phase_context::for_run(&store, &run_id);
        let mut policy = AgentTaskGateEnvironmentPolicy::default();
        policy.shared_cargo_target = Some(true);
        policy
            .variables
            .insert("CARGO_TARGET_DIR".to_string(), String::new());

        let first = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            temp.path(),
            1,
            "printf artifact > \"$CARGO_TARGET_DIR/artifact\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &policy,
            &[],
        )
        .expect("first managed gate");
        let first = first
            .environment
            .cargo_target
            .expect("first target evidence");
        assert_eq!(first.state.as_deref(), Some("empty"));
        assert!(first.acquire_ms.is_some());
        assert!(first.seed_ms.is_some());
        assert!(first.growth_bytes.is_some());
        assert_eq!(first.bytes_before, Some(0));
        assert!(first.bytes_after.expect("target bytes") > 0);

        let second = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            temp.path(),
            2,
            "printf artifact > \"$CARGO_TARGET_DIR/artifact\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &policy,
            &[],
        )
        .expect("reused managed gate");
        let second = second
            .environment
            .cargo_target
            .expect("second target evidence");
        assert_eq!(second.state.as_deref(), Some("seeded"));
        assert!(second.seed_bytes.unwrap() > 0);
        assert_ne!(first.identity, second.identity);
        assert_ne!(first.path, second.path);
        let spans = store.phase_spans(&run_id).unwrap();
        for (phase, count) in [
            ("gate.printf-artifact-cargo-target-dir-artifac", 2),
            ("publish.gate_cargo_target", 2),
        ] {
            assert_eq!(
                spans.spans.iter().filter(|span| span.id == phase).count(),
                count
            );
        }
    });
}

#[test]
fn agent_task_gate_candidate_target_is_fresh_and_cross_candidate_rebuilds() {
    homeboy_core::test_support::with_isolated_home(|_| {
        configure_fixture_cargo_capacity();
        let temp = tempfile::tempdir().unwrap();
        let modified = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let mut reports = Vec::new();
        for index in 1..=3 {
            // B overwrites the same physical source path used by gate 2.
            let cwd = temp.path().join(format!("gate-{}", index.min(2)));
            fs::create_dir_all(cwd.join("src")).unwrap();
            for (name, content) in [
                    ("Cargo.toml", "[package]\nname = \"candidate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
                    ("src/lib.rs", "pub fn candidate() {}\n"),
                ] {
                    let path = cwd.join(name);
                    fs::write(&path, content).unwrap();
                    fs::File::open(path).unwrap().set_modified(modified).unwrap();
                }
            // Fresh Git roots retain the same repository cache identity.
            std::process::Command::new("git")
                .arg("init")
                .arg(&cwd)
                .output()
                .unwrap();
            std::process::Command::new("git")
                .args([
                    "-C",
                    cwd.to_str().unwrap(),
                    "remote",
                    "add",
                    "origin",
                    "https://example.invalid/candidate",
                ])
                .output()
                .unwrap();
            fs::write(cwd.join(".gitignore"), "target/\n").unwrap();
            fs::write(cwd.join("Cargo.lock"), "# fixture lock\nversion = 4\n\n[[package]]\nname = \"candidate-fixture\"\nversion = \"0.1.0\"\n").unwrap();
            for args in [
                vec!["add", "."],
                vec![
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "-m",
                    "fixture",
                ],
            ] {
                if index == 3 {
                    continue;
                }
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&cwd)
                    .output()
                    .unwrap()
                    .status
                    .success());
            }
            if index == 3 {
                let source = cwd.join("src/lib.rs");
                fs::write(&source, "compile_error!(\"candidate B must compile\");\n").unwrap();
                fs::File::open(source)
                    .unwrap()
                    .set_modified(modified - Duration::from_secs(86400))
                    .unwrap();
            }
            if index == 2 {
                // The pre-reuse behavior: seed a fresh target and invalidate
                // workspace sources. Compare the exact same Cargo workload.
                let original_lock = fs::read(cwd.join("Cargo.lock")).unwrap();
                let baseline = homeboy_core::cleanup::acquire_managed_cargo_target(
                    "agent-task-gate",
                    &cwd,
                    None,
                )
                .unwrap();
                baseline.stamp_isolated_sources(&cwd).unwrap();
                let before = baseline.size_bytes().unwrap();
                let mut policy = AgentTaskGateEnvironmentPolicy::default();
                policy.shared_cargo_target = Some(false);
                policy.variables.insert(
                    "CARGO_TARGET_DIR".into(),
                    baseline.target_dir().display().to_string(),
                );
                let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
                    &cwd,
                    20,
                    "cargo check --verbose",
                    AgentTaskGateVisibility::Visible,
                    AgentTaskGateRevealPolicy::FullEvidence,
                    None,
                    &policy,
                    &[],
                )
                .unwrap();
                assert_eq!(report.exit_code, 0);
                assert!(report.stderr.contains("Checking candidate-fixture"));
                fs::write(cwd.join("Cargo.lock"), original_lock).unwrap();
                eprintln!(
                    "isolated second gate: growth_bytes={}, {:?}",
                    baseline.size_bytes().unwrap().saturating_sub(before),
                    report.environment.resources
                );
            }
            let mut policy = AgentTaskGateEnvironmentPolicy::default();
            policy.shared_cargo_target = Some(true);
            let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
                &cwd,
                index,
                "cargo check --verbose",
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                None,
                &policy,
                &[],
            )
            .unwrap();
            assert_eq!(
                report.exit_code,
                if index == 3 { 101 } else { 0 },
                "{}",
                report.stderr
            );
            eprintln!(
                "candidate gate {index}: {:?}, {:?}",
                report.environment.resources, report.environment.cargo_target
            );
            reports.push(report);
        }
        assert!(reports[0].stderr.contains("Checking candidate-fixture"));
        assert!(reports[1].stderr.contains("Fresh candidate-fixture"));
        let first = reports[0].environment.cargo_target.as_ref().unwrap();
        let second = reports[1].environment.cargo_target.as_ref().unwrap();
        assert_eq!(first.path, second.path);
        assert!(second.growth_bytes.unwrap() < 4096);
        let third = reports[2].environment.cargo_target.as_ref().unwrap();
        assert_ne!(second.path, third.path);
        assert!(reports[2].stderr.contains("candidate B must compile"));
    });
}

#[test]
fn concurrent_cargo_build_gates_with_one_declared_target_get_isolated_stores() {
    homeboy_core::test_support::with_isolated_home(|_| {
        configure_fixture_cargo_capacity();
        let temp = tempfile::tempdir().expect("gate fixture root");
        let first_worktree = temp.path().join("first");
        let second_worktree = temp.path().join("second");
        for worktree in [&first_worktree, &second_worktree] {
            std::fs::create_dir_all(worktree.join("src")).expect("fixture source directory");
            std::fs::write(
                    worktree.join("Cargo.toml"),
                    "[package]\nname = \"parallel-gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                )
                .expect("fixture manifest");
            std::fs::write(worktree.join("src/lib.rs"), "pub fn fixture() {}\n")
                .expect("fixture source");
        }
        let barrier = std::sync::Barrier::new(2);
        let reports = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                run_shared_target_fixture_gate(&first_worktree, 1, temp.path())
            });
            let second = scope.spawn(|| {
                barrier.wait();
                run_shared_target_fixture_gate(&second_worktree, 2, temp.path())
            });
            [
                first.join().expect("first gate thread"),
                second.join().expect("second gate thread"),
            ]
        });

        let first = reports[0]
            .as_ref()
            .expect("first managed gate")
            .environment
            .cargo_target
            .as_ref()
            .expect("first target evidence");
        let second = reports[1]
            .as_ref()
            .expect("second managed gate")
            .environment
            .cargo_target
            .as_ref()
            .expect("second target evidence");
        assert_ne!(first.path, second.path);
        assert_ne!(first.identity, second.identity);
        assert!(std::path::Path::new(&first.path).is_dir());
        assert!(std::path::Path::new(&first.path).join("debug").is_dir());
        assert!(std::path::Path::new(&second.path).join("debug").is_dir());
    });
}

pub(super) fn run_shared_target_fixture_gate(
    cwd: &Path,
    index: usize,
    declared_target: &Path,
) -> Result<AgentTaskGateReport> {
    let mut policy = AgentTaskGateEnvironmentPolicy::default();
    policy.shared_cargo_target = Some(true);
    policy.variables.insert(
        "CARGO_TARGET_DIR".to_string(),
        declared_target
            .join("declared-shared-target")
            .display()
            .to_string(),
    );
    run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        cwd,
        index,
        "cargo build --quiet",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        &policy,
        &[],
    )
}

/// Restores process-global environment variables when dropped.
///
/// Save/restore has to be panic-safe. A failing assertion between the
/// mutation and a manual restore would leave the variable altered for every
/// test that runs afterwards, turning one red test into a cascade.
pub(super) struct EnvVarGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvVarGuard {
    pub(super) fn set(names_and_values: &[(&'static str, &std::path::Path)]) -> Self {
        let saved = names_and_values
            .iter()
            .map(|(name, value)| {
                let prior = std::env::var_os(name);
                std::env::set_var(name, value);
                (*name, prior)
            })
            .collect();
        Self { saved }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        for (name, prior) in self.saved.drain(..) {
            match prior {
                Some(value) => std::env::set_var(name, value),
                // Only remove what was genuinely absent before.
                None => std::env::remove_var(name),
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn declared_adapter_fixture(
    exit_code: i32,
) -> (
    tempfile::TempDir,
    homeboy_engine_primitives::test_execution::TestExecutionPlan,
    AgentTaskGateEnvironmentPolicy,
) {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = workspace.path().join("bin");
    std::fs::create_dir(&bin).expect("bin");
    let homeboy = bin.join("homeboy");
    homeboy_test_support::write_executable(&homeboy, format!(
                "#!/bin/sh\ntest \"$1\" = review && test \"$2\" = test && test \"$3\" = '$HOME' || exit 97\nif [ \"$HOMEBOY_FAKE_GATE_BLOCK\" = 1 ]; then while :; do sleep 1; done; fi\n[ -z \"$HOMEBOY_FAKE_GATE_SLEEP\" ] || sleep \"$HOMEBOY_FAKE_GATE_SLEEP\"\nprintf '%s|%s|%s|%s' \"$1\" \"$2\" \"$3\" \"$HOMEBOY_TEST_TIMEOUT_SECONDS\"\nexit {exit_code}\n"
            ), 0o755)
        .expect("adapter");

    let plan =
        homeboy_engine_primitives::test_execution::TestExecutionPlan::declared_homeboy_review_test(
            vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
                "$HOME".to_string(),
            ],
            42,
        )
        .expect("declared plan");
    let mut environment = AgentTaskGateEnvironmentPolicy::default();
    environment
        .variables
        .insert("PATH".to_string(), format!("{}:/bin", bin.display()));
    (workspace, plan, environment)
}

#[cfg(unix)]
pub(super) fn declared_supervision(
    cancelled: bool,
    no_progress_timeout: Duration,
) -> GateSupervision {
    GateSupervision {
        timeout: Duration::from_millis(10),
        no_progress_timeout,
        heartbeat_interval: Duration::from_millis(5),
        on_spawn: Arc::new(|_, _| Ok(())),
        on_heartbeat: Arc::new(|_| Ok(())),
        is_cancelled: Arc::new(move || cancelled),
    }
}

#[cfg(unix)]
#[test]
fn declared_candidate_and_baseline_replay_identical_argv_deadline_and_outcomes() {
    for (exit_code, outcome) in [
        (
            0,
            homeboy_engine_primitives::test_execution::TestExecutionOutcome::Passed,
        ),
        (
            7,
            homeboy_engine_primitives::test_execution::TestExecutionOutcome::Failed,
        ),
    ] {
        let (workspace, plan, environment) = declared_adapter_fixture(exit_code);
        let runtime = tempfile::tempdir().expect("runtime");
        let supervision = declared_supervision(false, Duration::from_secs(1));
        let candidate = run_declared_test_with_supervision(
            workspace.path(),
            1,
            &plan,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            Some(runtime.path()),
            Some(&supervision),
            &environment,
            &[],
        )
        .expect("candidate execution");
        let baseline = run_declared_test_with_timeout(
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

        assert_eq!(candidate.command, plan.declared_command().unwrap());
        assert_eq!(candidate.command, baseline.command);
        assert_eq!(candidate.stdout, "review|test|$HOME|42");
        assert_eq!(candidate.stdout, baseline.stdout);
        assert_eq!(candidate.test_execution_outcome, Some(outcome));
        assert_eq!(
            candidate.test_execution_outcome,
            baseline.test_execution_outcome
        );
    }
}

#[cfg(unix)]
#[test]
fn declared_supervised_cancellation_retains_its_typed_outcome() {
    let (workspace, plan, mut environment) = declared_adapter_fixture(0);
    environment
        .variables
        .insert("HOMEBOY_FAKE_GATE_BLOCK".to_string(), "1".to_string());
    let supervision = declared_supervision(true, Duration::from_secs(1));
    let report = run_declared_test_with_supervision(
        workspace.path(),
        1,
        &plan,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(&supervision),
        &environment,
        &[],
    )
    .expect("cancelled declared execution");

    assert_eq!(report.termination, AgentTaskGateTermination::Cancelled);
    assert_eq!(
        report.test_execution_outcome,
        Some(homeboy_engine_primitives::test_execution::TestExecutionOutcome::Cancelled)
    );
}

#[cfg(unix)]
#[test]
fn declared_supervised_no_progress_retains_timeout_outcome() {
    let (workspace, plan, mut environment) = declared_adapter_fixture(0);
    environment
        .variables
        .insert("HOMEBOY_FAKE_GATE_SLEEP".to_string(), "2".to_string());
    let supervision = declared_supervision(false, Duration::from_millis(20));
    let report = run_declared_test_with_supervision(
        workspace.path(),
        1,
        &plan,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        Some(&supervision),
        &environment,
        &[],
    )
    .expect("stalled declared execution");

    assert_eq!(report.termination, AgentTaskGateTermination::NoProgress);
    assert_eq!(
        report.test_execution_outcome,
        Some(homeboy_engine_primitives::test_execution::TestExecutionOutcome::TimedOut)
    );
}

#[test]
fn env_var_guard_restores_a_prior_value() {
    let _lock = env_mutex();
    let scratch = tempfile::tempdir().expect("scratch");
    std::env::set_var("__HOMEBOY_TEST_ENV_GUARD__", "original");

    {
        let _guard = EnvVarGuard::set(&[("__HOMEBOY_TEST_ENV_GUARD__", scratch.path())]);
        assert_eq!(
            std::env::var_os("__HOMEBOY_TEST_ENV_GUARD__"),
            Some(scratch.path().as_os_str().to_owned())
        );
    }

    assert_eq!(
        std::env::var("__HOMEBOY_TEST_ENV_GUARD__").ok().as_deref(),
        Some("original"),
        "a variable that existed before must be put back, not deleted"
    );
    std::env::remove_var("__HOMEBOY_TEST_ENV_GUARD__");
}

#[test]
fn env_var_guard_removes_a_variable_that_was_absent() {
    let _lock = env_mutex();
    let scratch = tempfile::tempdir().expect("scratch");
    std::env::remove_var("__HOMEBOY_TEST_ENV_GUARD_ABSENT__");

    {
        let _guard = EnvVarGuard::set(&[("__HOMEBOY_TEST_ENV_GUARD_ABSENT__", scratch.path())]);
        assert!(std::env::var_os("__HOMEBOY_TEST_ENV_GUARD_ABSENT__").is_some());
    }

    assert!(std::env::var_os("__HOMEBOY_TEST_ENV_GUARD_ABSENT__").is_none());
}

#[test]
fn declared_test_executor_uses_direct_argv_and_reports_a_typed_outcome() {
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

    let report = run_gate_argv_with_supervision(
        workspace.path(),
        1,
        vec!["printf".to_string(), "%s".to_string(), "$HOME".to_string()],
        "homeboy review test",
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
        None,
        None,
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
        Some(&plan),
    )
    .expect("direct argv execution");

    assert_eq!(report.command, ["printf", "%s", "$HOME"]);
    assert_eq!(report.stdout, "$HOME", "a shell would expand this argument");
    assert_eq!(
        report.test_execution_outcome,
        Some(homeboy_engine_primitives::test_execution::TestExecutionOutcome::Passed)
    );
    assert!(
        report.test_result.is_none(),
        "declared outcomes are not parsed"
    );
}
